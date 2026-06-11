//! `allowlister history` — inspect recorded usage so allowlists can be refined
//! from real behavior.
//!
//! The default view is a per-parsed-subcommand frequency table broken down by
//! verdict (allowed / asked / denied / deferred-to-harness), with a count of how
//! many projects each subcommand ran in; `--view commands` shows whole command
//! lines instead, and `--view programs` collapses each subcommand to its leading
//! program. `--by-project` expands every subcommand/program row into a per-project
//! verdict breakdown. The `recent`, `compact`, `clear`, and `path` subcommands
//! manage the store. Recording is opt-in (see `init`); this command only reads
//! (and, for `compact`/`clear`, maintains) what was recorded.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::Serialize;

use crate::domain::Verdict;
use crate::errors::Result;
use crate::io::configfs::{self, Env};
use crate::io::history::{self, Counts, Event, Recency, Row, Summary};

/// Which frequency table the default view shows.
pub enum View {
    /// Per parsed subcommand, keyed by the full subcommand (e.g. `git push`).
    Fragments,
    /// Per program, collapsing a subcommand to its leading token (e.g. `git`).
    Programs,
    /// Per whole command line (the "overall full command" view).
    Commands,
}

/// Options for the default frequency report.
pub struct ShowArgs {
    /// Which table to show.
    pub view: View,
    /// Restrict to (and sort by) one verdict.
    pub verdict: Option<Verdict>,
    /// Show at most this many rows.
    pub top: usize,
    /// Expand each row into a per-project verdict breakdown (subcommand/program
    /// views only; the whole-command view is not split by project).
    pub by_project: bool,
    /// Emit machine-readable JSON.
    pub json: bool,
}

/// Options for the `recent` event listing.
pub struct RecentArgs {
    /// Show at most this many events.
    pub top: usize,
    /// Keep only events whose project tag contains this substring.
    pub project: Option<String>,
    /// Keep only events from this harness.
    pub harness: Option<String>,
    /// Keep only events with this verdict.
    pub verdict: Option<Verdict>,
    /// Emit machine-readable JSON.
    pub json: bool,
}

/// A maintenance subcommand, or the default report when `None`.
pub enum Action {
    /// List the recent raw events (a bounded, time-ordered window).
    Recent(RecentArgs),
    /// Fold the recent-events log into the durable summary now.
    Compact,
    /// Delete all recorded history.
    Clear {
        /// Skip the confirmation prompt.
        yes: bool,
    },
    /// Print where history is stored.
    Path,
}

const EMPTY_HINT: &str = "No usage recorded yet. Enable recording with `allowlister init` \
(or set \"history\": { \"enabled\": true } in your config), and history will \
accumulate as your agent runs commands.";

/// Run the requested history action against the user-global store.
pub fn run(action: Option<Action>, show: ShowArgs) -> Result<i32> {
    let Some(dir) = configfs::default_history_dir(&Env::from_process()) else {
        println!("No config/home directory found, so there is no history store.");
        return Ok(0);
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let now = history::now_secs();
    match action {
        None => show_in(&dir, &show, now, &mut out),
        Some(Action::Recent(args)) => recent_in(&dir, &args, now, &mut out),
        Some(Action::Compact) => compact_in(&dir, &mut out),
        Some(Action::Clear { yes }) => {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            clear_in(&dir, yes, &mut input, &mut out)
        }
        Some(Action::Path) => {
            let _ = writeln!(out, "{}", dir.display());
            Ok(0)
        }
    }
}

/// Render the frequency report. Separated from [`run`] so it is testable with an
/// explicit directory, report time, and writer.
fn show_in<W: Write>(dir: &Path, args: &ShowArgs, now: u64, out: &mut W) -> Result<i32> {
    let summary = history::aggregate(dir);
    let rows = match args.view {
        View::Fragments => history::fragment_rows(&summary, false, args.verdict, args.top),
        View::Programs => history::fragment_rows(&summary, true, args.verdict, args.top),
        View::Commands => history::command_rows(&summary, args.verdict, args.top),
    };

    if args.json {
        print_json(out, &summary, args, &rows, now);
        return Ok(0);
    }

    if summary.events_total == 0 {
        let _ = writeln!(out, "{EMPTY_HINT}");
        return Ok(0);
    }

    let _ = writeln!(
        out,
        "allowlister usage history — {} event(s) recorded",
        summary.events_total
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "  {}", verdict_line(&summary.overall));
    let _ = writeln!(out);
    let _ = writeln!(out, "{}:", title(args));
    let _ = writeln!(out);
    print_table(out, &rows, &args.view, args.by_project, args.verdict, now);

    let truncated = match args.view {
        View::Commands => summary.commands_truncated,
        _ => summary.fragments_truncated,
    };
    if truncated {
        let _ = writeln!(
            out,
            "\nNote: the long tail of rare entries was collapsed into '(other)'."
        );
    }
    if args.verdict.is_none() {
        let _ = writeln!(
            out,
            "\nTip: `allowlister history --verdict defer` lists what fell through to the \
             harness's own prompt — the best candidates for a new allow rule."
        );
    }
    Ok(0)
}

/// The "allow X ask Y deny Z defer W" summary line.
fn verdict_line(counts: &Counts) -> String {
    format!(
        "allow {}   ask {}   deny {}   defer {}",
        counts.allow, counts.ask, counts.deny, counts.defer
    )
}

fn title(args: &ShowArgs) -> String {
    let base = match args.view {
        View::Fragments => "Most-evaluated subcommands",
        View::Programs => "Most-evaluated programs",
        View::Commands => "Most-evaluated commands",
    };
    match args.verdict {
        Some(v) => format!("{base} ({} only)", v.as_str()),
        None => base.to_string(),
    }
}

/// Print the frequency table with right-aligned counts and a left-aligned key.
/// Beyond the verdict tallies, `RECENT` is the recency-weighted activity (the
/// filtered verdict's when one is set, else the total) decayed to `now`, and
/// `LAST` is the age of the latest use — together they separate live keys from
/// heavy-but-stale ones. For subcommand/program views trailing columns count the
/// projects the key ran in and name the dominant rule; with `by_project`, each
/// row is followed by an indented per-project verdict breakdown.
fn print_table<W: Write>(
    out: &mut W,
    rows: &[Row],
    view: &View,
    by_project: bool,
    verdict: Option<Verdict>,
    now: u64,
) {
    // The whole-command view is not broken down by project, so it carries
    // neither the rule nor the projects columns.
    let show_detail = !matches!(view, View::Commands);
    let key_header = match view {
        View::Commands => "COMMAND",
        View::Programs => "PROGRAM",
        View::Fragments => "SUBCOMMAND",
    };

    // Right-aligned columns: header plus every row's value decides the width.
    let nums = ["TOTAL", "ALLOW", "ASK", "DENY", "DEFER", "RECENT", "LAST"];
    let mut widths: Vec<usize> = nums.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, value) in cells(&row.counts, verdict, now).iter().enumerate() {
            widths[i] = widths[i].max(value.len());
        }
    }
    let key_width = rows
        .iter()
        .map(|r| r.key.len())
        .max()
        .unwrap_or(0)
        .max(key_header.len());
    // The projects column holds a distinct-project count (e.g. `4`, or `64+`
    // once the per-fragment cap overflows).
    let proj_header = "PROJECTS";
    let proj_width = rows
        .iter()
        .map(|r| project_count_label(&r.projects).len())
        .max()
        .unwrap_or(0)
        .max(proj_header.len());

    let mut header = String::new();
    for (i, name) in nums.iter().enumerate() {
        header.push_str(&format!("{:>w$}  ", name, w = widths[i]));
    }
    header.push_str(&format!("{:<kw$}", key_header, kw = key_width));
    if show_detail {
        header.push_str(&format!("  {:>pw$}  RULE", proj_header, pw = proj_width));
    }
    let _ = writeln!(out, "  {}", header.trim_end());

    for row in rows {
        let mut line = String::new();
        for (i, value) in cells(&row.counts, verdict, now).iter().enumerate() {
            line.push_str(&format!("{:>w$}  ", value, w = widths[i]));
        }
        line.push_str(&format!("{:<kw$}", row.key, kw = key_width));
        if show_detail {
            line.push_str(&format!(
                "  {:>pw$}",
                project_count_label(&row.projects),
                pw = proj_width
            ));
            if let Some(rule) = dominant_rule(&row.rules) {
                line.push_str(&format!("  {rule}"));
            }
        }
        let _ = writeln!(out, "  {}", line.trim_end());
        if by_project && show_detail {
            print_project_breakdown(out, &row.projects);
        }
    }
}

/// `N`, or `N+` when the per-fragment project map overflowed its cap (the
/// synthetic overflow bucket means "and more").
fn project_count_label(projects: &BTreeMap<String, Counts>) -> String {
    if projects.contains_key(history::OVERFLOW) {
        // Drop the synthetic bucket from the count and flag the remainder.
        format!("{}+", projects.len().saturating_sub(1))
    } else {
        projects.len().to_string()
    }
}

/// Indented per-project verdict lines for one row, busiest project first.
fn print_project_breakdown<W: Write>(out: &mut W, projects: &BTreeMap<String, Counts>) {
    let mut entries: Vec<(&String, &Counts)> = projects.iter().collect();
    entries.sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(b.0)));
    let key_width = entries.iter().map(|(p, _)| p.len()).max().unwrap_or(0);
    for (project, counts) in entries {
        let _ = writeln!(
            out,
            "        {:<kw$}  {}",
            project,
            verdict_line(counts),
            kw = key_width
        );
    }
}

fn cells(counts: &Counts, verdict: Option<Verdict>, now: u64) -> [String; 7] {
    let recent = counts.recent_at(now);
    let recent_value = match verdict {
        Some(v) => recent.get(v),
        None => recent.total(),
    };
    [
        counts.total().to_string(),
        counts.allow.to_string(),
        counts.ask.to_string(),
        counts.deny.to_string(),
        counts.defer.to_string(),
        format!("{recent_value:.1}"),
        age_label(counts.last_ts, now),
    ]
}

/// Compact age of the latest use (`<1h`, `3h`, `5d`, `2w`, `6mo`, `1y`), or `-`
/// when no timestamp was recorded.
fn age_label(ts: u64, now: u64) -> String {
    const HOUR: u64 = 3_600;
    const DAY: u64 = 86_400;
    if ts == 0 {
        return "-".to_string();
    }
    let secs = now.saturating_sub(ts);
    if secs < HOUR {
        "<1h".to_string()
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else if secs < 7 * DAY {
        format!("{}d", secs / DAY)
    } else if secs < 30 * DAY {
        format!("{}w", secs / (7 * DAY))
    } else if secs < 365 * DAY {
        format!("{}mo", secs / (30 * DAY))
    } else {
        format!("{}y", secs / (365 * DAY))
    }
}

/// The most-cited rule for a subcommand, annotated with `(+N)` when others also
/// fired, so the table shows what is covering it (and blank means uncovered).
fn dominant_rule(rules: &BTreeMap<String, u64>) -> Option<String> {
    let top = rules
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))?;
    if rules.len() > 1 {
        Some(format!("{} (+{})", top.0, rules.len() - 1))
    } else {
        Some(top.0.clone())
    }
}

#[derive(Serialize)]
struct RowJson<'a> {
    key: &'a str,
    total: u64,
    allow: u64,
    ask: u64,
    deny: u64,
    defer: u64,
    /// Unix seconds of this key's earliest recorded use (0 when unknown).
    first_ts: u64,
    /// Unix seconds of this key's latest recorded use.
    last_ts: u64,
    /// Recency-weighted per-verdict activity decayed to `as_of` (30-day
    /// half-life): compare against the raw counts to spot heavy-but-stale keys.
    #[serde(skip_serializing_if = "Recency::is_empty")]
    recent: Recency,
    recent_total: f64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    rules: &'a BTreeMap<String, u64>,
    /// Distinct projects this key ran in (the overflow bucket, if present, counts
    /// as one).
    project_count: usize,
    /// The full per-project verdict breakdown, included only under `--by-project`.
    #[serde(skip_serializing_if = "Option::is_none")]
    projects: Option<&'a BTreeMap<String, Counts>>,
}

#[derive(Serialize)]
struct ShowJson<'a> {
    events_total: u64,
    first_ts: u64,
    last_ts: u64,
    /// The report time (Unix seconds) every `recent` weight was decayed to; ages
    /// are `as_of - last_ts`.
    as_of: u64,
    overall: &'a Counts,
    view: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<&'a str>,
    truncated: bool,
    rows: Vec<RowJson<'a>>,
}

/// Round a decayed weight for output: two decimals carry all the signal a
/// refine pass needs and keep the JSON stable to assert on.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn rounded(recent: &Recency) -> Recency {
    Recency {
        allow: round2(recent.allow),
        deny: round2(recent.deny),
        ask: round2(recent.ask),
        defer: round2(recent.defer),
    }
}

fn print_json<W: Write>(out: &mut W, summary: &Summary, args: &ShowArgs, rows: &[Row], now: u64) {
    let payload = ShowJson {
        events_total: summary.events_total,
        first_ts: summary.first_ts,
        last_ts: summary.last_ts,
        as_of: now,
        overall: &summary.overall,
        view: match args.view {
            View::Fragments => "fragments",
            View::Programs => "programs",
            View::Commands => "commands",
        },
        verdict: args.verdict.map(Verdict::as_str),
        truncated: match args.view {
            View::Commands => summary.commands_truncated,
            _ => summary.fragments_truncated,
        },
        rows: rows
            .iter()
            .map(|row| {
                let recent = rounded(&row.counts.recent_at(now));
                RowJson {
                    key: &row.key,
                    total: row.counts.total(),
                    allow: row.counts.allow,
                    ask: row.counts.ask,
                    deny: row.counts.deny,
                    defer: row.counts.defer,
                    first_ts: row.counts.first_ts,
                    last_ts: row.counts.last_ts,
                    recent,
                    recent_total: round2(recent.total()),
                    rules: &row.rules,
                    project_count: row.projects.len(),
                    projects: args.by_project.then_some(&row.projects),
                }
            })
            .collect(),
    };
    if let Ok(line) = serde_json::to_string(&payload) {
        let _ = writeln!(out, "{line}");
    }
}

/// List the recent raw events (newest first), filtered, each with its age.
fn recent_in<W: Write>(dir: &Path, args: &RecentArgs, now: u64, out: &mut W) -> Result<i32> {
    let mut events = history::read_events(dir);
    events.reverse();
    let selected: Vec<&Event> = events
        .iter()
        .filter(|event| {
            args.project
                .as_deref()
                .is_none_or(|p| event.project.contains(p))
                && args.harness.as_deref().is_none_or(|h| event.harness == h)
                && args.verdict.is_none_or(|v| event.verdict == v.as_str())
        })
        .take(args.top)
        .collect();

    if args.json {
        let line = serde_json::to_string(&selected).unwrap_or_else(|_| "[]".to_string());
        let _ = writeln!(out, "{line}");
        return Ok(0);
    }
    if selected.is_empty() {
        let _ = writeln!(
            out,
            "No recent events. (Older events are folded into the summary and shown by \
             `allowlister history`, not here.)"
        );
        return Ok(0);
    }
    for event in selected {
        let _ = writeln!(
            out,
            "{:<6} {:<11} {:>4}  {}   [{}]",
            event.verdict.to_uppercase(),
            event.harness,
            age_label(event.ts, now),
            event.command,
            event.project
        );
    }
    Ok(0)
}

fn compact_in<W: Write>(dir: &Path, out: &mut W) -> Result<i32> {
    history::compact(dir)?;
    let summary = history::aggregate(dir);
    let _ = writeln!(
        out,
        "Compacted. {} event(s) in the durable summary.",
        summary.events_total
    );
    Ok(0)
}

fn clear_in<R: BufRead, W: Write>(
    dir: &Path,
    yes: bool,
    input: &mut R,
    out: &mut W,
) -> Result<i32> {
    if !yes {
        let _ = write!(
            out,
            "Delete all recorded history under {}? [y/N] ",
            dir.display()
        );
        let _ = out.flush();
        let mut line = String::new();
        input.read_line(&mut line)?;
        if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            let _ = writeln!(out, "Aborted; nothing was deleted.");
            return Ok(0);
        }
    }
    history::clear(dir)?;
    let _ = writeln!(out, "Cleared all recorded history.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::domain::evaluate;
    use crate::io::history::{append_event, Subject};
    use std::io::Cursor;
    use tempfile::TempDir;

    /// The fixed report time most tests use; seeding at the same instant keeps
    /// the recency weights equal to the raw counts.
    const NOW: u64 = 7;

    fn seed(dir: &Path) {
        seed_at(dir, "ls -la", "/repo", NOW);
        seed_at(dir, "ls -la", "/repo", NOW);
        seed_at(dir, "ls foo | grep bar", "/repo", NOW);
    }

    /// Append one shell event with an explicit project and timestamp.
    fn seed_at(dir: &Path, command: &str, project: &str, ts: u64) {
        let cfg = config::compile_str(
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
            "t",
        );
        let result = evaluate(command, &cfg.rules);
        // Build via the public record path by appending a constructed event.
        let event = crate::io::history::build_event_for_test(
            "claude-code",
            project,
            Subject::Shell(command),
            &result,
            ts,
        );
        append_event(dir, &event).unwrap();
    }

    /// Append one shell event under an explicit project, for project-dimension
    /// tests.
    fn seed_in_project(dir: &Path, command: &str, project: &str) {
        seed_at(dir, command, project, NOW);
    }

    fn out_string(buf: Vec<u8>) -> String {
        String::from_utf8(buf).unwrap()
    }

    fn show(view: View, verdict: Option<Verdict>) -> ShowArgs {
        ShowArgs {
            view,
            verdict,
            top: 20,
            by_project: false,
            json: false,
        }
    }

    #[test]
    fn empty_store_prints_enable_hint() {
        let dir = TempDir::new().unwrap();
        let mut out = Vec::new();
        show_in(dir.path(), &show(View::Fragments, None), NOW, &mut out).unwrap();
        assert!(out_string(out).contains("No usage recorded yet"));
    }

    #[test]
    fn fragment_report_lists_subcommands_with_counts_and_rule() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let mut out = Vec::new();
        show_in(dir.path(), &show(View::Fragments, None), NOW, &mut out).unwrap();
        let text = out_string(out);
        assert!(text.contains("3 event(s) recorded"));
        assert!(text.contains("ls -la"));
        assert!(text.contains("SUBCOMMAND"));
        // The covering rule is named; the uncovered grep filter shows none.
        assert!(text.contains("ls")); // rule column
        assert!(text.contains("grep bar"));
        assert!(text.contains("Tip:"));
    }

    #[test]
    fn fragment_report_shows_project_count_by_default() {
        let dir = TempDir::new().unwrap();
        seed_in_project(dir.path(), "ls -la", "/repo-a");
        seed_in_project(dir.path(), "ls -la", "/repo-b");
        let mut out = Vec::new();
        show_in(dir.path(), &show(View::Fragments, None), NOW, &mut out).unwrap();
        let text = out_string(out);
        // The summary column appears with the distinct-project count, but no
        // per-project breakdown without the flag.
        assert!(text.contains("PROJECTS"));
        let ls_row = text.lines().find(|l| l.contains("ls -la")).unwrap();
        assert!(
            ls_row.contains('2'),
            "expected project count 2 in: {ls_row}"
        );
        assert!(!text.contains("/repo-a"));
    }

    #[test]
    fn by_project_expands_per_project_breakdown() {
        let dir = TempDir::new().unwrap();
        seed_in_project(dir.path(), "ls -la", "/repo-a");
        seed_in_project(dir.path(), "ls -la", "/repo-b");
        seed_in_project(dir.path(), "ls -la", "/repo-b");
        let args = ShowArgs {
            view: View::Fragments,
            verdict: None,
            top: 20,
            by_project: true,
            json: false,
        };
        let mut out = Vec::new();
        show_in(dir.path(), &args, NOW, &mut out).unwrap();
        let text = out_string(out);
        // Both projects are listed with their own verdict tallies.
        assert!(text.contains("/repo-a"));
        assert!(text.contains("/repo-b"));
        assert!(text.contains("allow 2")); // /repo-b ran ls twice
    }

    #[test]
    fn by_project_json_includes_per_project_map() {
        let dir = TempDir::new().unwrap();
        seed_in_project(dir.path(), "ls -la", "/repo-a");
        seed_in_project(dir.path(), "ls -la", "/repo-b");
        let args = ShowArgs {
            view: View::Fragments,
            verdict: None,
            top: 20,
            by_project: true,
            json: true,
        };
        let mut out = Vec::new();
        show_in(dir.path(), &args, NOW, &mut out).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out_string(out)).unwrap();
        let row = value["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "ls -la")
            .unwrap();
        assert_eq!(row["project_count"], 2);
        assert_eq!(row["projects"]["/repo-a"]["allow"], 1);
        assert_eq!(row["projects"]["/repo-b"]["allow"], 1);
    }

    #[test]
    fn verdict_filter_narrows_to_deferred() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let mut out = Vec::new();
        show_in(
            dir.path(),
            &show(View::Fragments, Some(Verdict::Defer)),
            NOW,
            &mut out,
        )
        .unwrap();
        let text = out_string(out);
        assert!(text.contains("grep bar"));
        assert!(text.contains("defer only"));
        // The always-allowed ls row is filtered out.
        assert!(!text.contains("ls -la"));
        // No tip when a verdict filter is active.
        assert!(!text.contains("Tip:"));
    }

    #[test]
    fn program_and_command_views_render() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let mut programs = Vec::new();
        show_in(dir.path(), &show(View::Programs, None), NOW, &mut programs).unwrap();
        assert!(out_string(programs).contains("PROGRAM"));
        let mut commands = Vec::new();
        show_in(dir.path(), &show(View::Commands, None), NOW, &mut commands).unwrap();
        let text = out_string(commands);
        assert!(text.contains("COMMAND"));
        assert!(text.contains("ls foo | grep bar"));
    }

    #[test]
    fn json_report_is_machine_readable() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let mut out = Vec::new();
        show_in(
            dir.path(),
            &ShowArgs {
                view: View::Fragments,
                verdict: None,
                top: 20,
                by_project: false,
                json: true,
            },
            NOW,
            &mut out,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&out_string(out)).unwrap();
        assert_eq!(value["events_total"], 3);
        assert_eq!(value["overall"]["allow"], 2);
        assert_eq!(value["view"], "fragments");
        let rows = value["rows"].as_array().unwrap();
        assert!(rows.iter().any(|r| r["key"] == "ls -la" && r["allow"] == 2));
    }

    #[test]
    fn table_carries_recent_and_last_columns() {
        const DAY: u64 = 86_400;
        let dir = TempDir::new().unwrap();
        let now = 100 * DAY;
        seed_at(dir.path(), "ls -la", "/repo", now - 3 * DAY);
        let mut out = Vec::new();
        show_in(dir.path(), &show(View::Fragments, None), now, &mut out).unwrap();
        let text = out_string(out);
        assert!(text.contains("RECENT"), "{text}");
        assert!(text.contains("LAST"), "{text}");
        let row = text.lines().find(|l| l.contains("ls -la")).unwrap();
        // Last used three days before the report time.
        assert!(row.contains("3d"), "expected age 3d in: {row}");
        // One event, slightly decayed over those three days: 0.9-ish.
        assert!(row.contains("0.9"), "expected decayed weight in: {row}");
    }

    #[test]
    fn json_separates_fresh_from_heavy_but_stale_keys() {
        let half_life = crate::io::history::RECENT_HALF_LIFE_SECS;
        let dir = TempDir::new().unwrap();
        let now = 100 * half_life;
        // A burst of use long ago vs a single use now.
        for _ in 0..5 {
            seed_at(
                dir.path(),
                "ls stale | grep old",
                "/repo",
                now - 20 * half_life,
            );
        }
        seed_at(dir.path(), "ls fresh | grep new", "/repo", now);
        let args = ShowArgs {
            view: View::Fragments,
            verdict: Some(Verdict::Defer),
            top: 20,
            by_project: false,
            json: true,
        };
        let mut out = Vec::new();
        show_in(dir.path(), &args, now, &mut out).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out_string(out)).unwrap();
        assert_eq!(value["as_of"], now);
        let rows = value["rows"].as_array().unwrap();
        let row = |key: &str| rows.iter().find(|r| r["key"] == key).unwrap();
        let stale = row("grep old");
        // Five raw defers, but the recency weight has fully decayed away.
        assert_eq!(stale["defer"], 5);
        assert_eq!(stale["recent_total"], 0.0);
        assert!(stale.get("recent").is_none(), "decayed weights are omitted");
        assert_eq!(stale["last_ts"], now - 20 * half_life);
        let fresh = row("grep new");
        assert_eq!(fresh["defer"], 1);
        assert_eq!(fresh["recent_total"], 1.0);
        assert_eq!(fresh["recent"]["defer"], 1.0);
        assert_eq!(fresh["first_ts"], now);
    }

    #[test]
    fn age_labels_cover_the_unit_ladder() {
        const DAY: u64 = 86_400;
        let now = 10 * 365 * DAY;
        assert_eq!(age_label(0, now), "-");
        assert_eq!(age_label(now - 30, now), "<1h");
        assert_eq!(age_label(now - 5 * 3_600, now), "5h");
        assert_eq!(age_label(now - 2 * DAY, now), "2d");
        assert_eq!(age_label(now - 20 * DAY, now), "2w");
        assert_eq!(age_label(now - 200 * DAY, now), "6mo");
        assert_eq!(age_label(now - 800 * DAY, now), "2y");
    }

    #[test]
    fn recent_lists_newest_first_and_filters() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let args = RecentArgs {
            top: 10,
            project: None,
            harness: Some("claude-code".to_string()),
            verdict: Some(Verdict::Defer),
            json: false,
        };
        let mut out = Vec::new();
        recent_in(dir.path(), &args, NOW, &mut out).unwrap();
        let text = out_string(out);
        assert!(text.contains("DEFER"));
        assert!(text.contains("ls foo | grep bar"));
        assert!(!text.contains("ALLOW"));
    }

    #[test]
    fn recent_json_and_empty_message() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let json_args = RecentArgs {
            top: 10,
            project: Some("nope".to_string()),
            harness: None,
            verdict: None,
            json: true,
        };
        let mut out = Vec::new();
        recent_in(dir.path(), &json_args, NOW, &mut out).unwrap();
        assert_eq!(out_string(out).trim(), "[]");

        let text_args = RecentArgs {
            top: 10,
            project: Some("nope".to_string()),
            harness: None,
            verdict: None,
            json: false,
        };
        let mut out = Vec::new();
        recent_in(dir.path(), &text_args, NOW, &mut out).unwrap();
        assert!(out_string(out).contains("No recent events"));
    }

    #[test]
    fn compact_then_show_reads_from_summary() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        let mut out = Vec::new();
        compact_in(dir.path(), &mut out).unwrap();
        assert!(out_string(out).contains("3 event(s)"));
        // After folding, the report still sees everything via the summary.
        let mut out = Vec::new();
        show_in(dir.path(), &show(View::Fragments, None), NOW, &mut out).unwrap();
        assert!(out_string(out).contains("ls -la"));
    }

    #[test]
    fn clear_confirms_aborts_and_deletes() {
        let dir = TempDir::new().unwrap();
        seed(dir.path());
        // "n" aborts.
        let mut input = Cursor::new(b"n\n");
        let mut out = Vec::new();
        clear_in(dir.path(), false, &mut input, &mut out).unwrap();
        assert!(out_string(out).contains("Aborted"));
        assert_eq!(history::aggregate(dir.path()).events_total, 3);
        // "y" deletes.
        let mut input = Cursor::new(b"y\n");
        let mut out = Vec::new();
        clear_in(dir.path(), false, &mut input, &mut out).unwrap();
        assert!(out_string(out).contains("Cleared"));
        assert_eq!(history::aggregate(dir.path()).events_total, 0);
        // --yes skips the prompt.
        seed(dir.path());
        let mut input = Cursor::new(b"");
        let mut out = Vec::new();
        clear_in(dir.path(), true, &mut input, &mut out).unwrap();
        assert_eq!(history::aggregate(dir.path()).events_total, 0);
    }
}
