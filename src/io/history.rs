//! Usage-history recording and aggregation (an I/O boundary).
//!
//! The hot path ([`record`]) runs inside every harness hook, so it must be cheap
//! and fail-open: it appends one JSON line per evaluation and never lets an error
//! reach the decision. Two design constraints shape the storage:
//!
//! - **Bounded files.** Raw events accumulate in `events.jsonl` until it crosses
//!   [`SEGMENT_CAP`], at which point they are *folded* into a durable, cumulative
//!   `summary.json` and the raw log is cleared. The summary's size is bounded by
//!   the number of distinct commands/subcommands (capped, with an overflow
//!   bucket), not by how many commands ever ran — so disk use stays bounded while
//!   full-history counts survive forever.
//! - **Performant history.** The summary *is* the precomputed full history, so
//!   reporting reads it directly rather than scanning every event ever recorded.
//!   `events.jsonl` is a bounded recent-detail window on top of it.
//!
//! Concurrency: hooks run as independent processes (one per tool call), possibly
//! in parallel. Appends use `O_APPEND` (atomic per line). The fold — the only
//! read-modify-write — is serialized by an exclusive-create lock file, so two
//! concurrent folds can never double-count. A crashed holder's lock goes stale
//! after [`LOCK_TTL_SECS`] and is reclaimed.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::domain::{DecisionResult, ToolCall, Verdict};
use crate::io::configfs::{self, Env};

/// The active raw-event log (append-only, folded and cleared when it grows past
/// [`SEGMENT_CAP`]).
const EVENTS: &str = "events.jsonl";
/// The durable cumulative aggregate — the full-history source of truth.
const SUMMARY: &str = "summary.json";
/// The fold mutual-exclusion lock.
const LOCK: &str = "compact.lock";
/// The key distinct commands/subcommands collapse into once a map is full, so
/// totals stay exact even though the long tail loses its per-key breakdown.
const OVERFLOW: &str = "(other)";

/// Fold `events.jsonl` into the summary once it exceeds this many bytes (~1 MiB).
const SEGMENT_CAP: u64 = 1_000_000;
/// Cap on distinct keys in the `commands` and `fragments` maps.
const MAX_KEYS: usize = 5_000;
/// Cap on distinct keys in the low-cardinality `projects`/`harnesses` maps.
const MAX_DIM_KEYS: usize = 1_000;
/// Cap on distinct rule names tracked per subcommand.
const MAX_RULES: usize = 16;
/// Cap on fragments recorded per event (defends against pathological input).
const MAX_FRAGMENTS: usize = 64;
/// Cap on the character length of a stored command/subcommand/project string,
/// keeping each event line small enough for atomic appends.
const MAX_STR: usize = 1_000;
/// A lock older than this (by mtime) is treated as abandoned and reclaimed.
const LOCK_TTL_SECS: u64 = 120;

/// What was evaluated, for [`record`]: a shell command line or a tool call.
pub enum Subject<'a> {
    /// A shell command line; per-subcommand detail comes from the decision's
    /// fragments.
    Shell(&'a str),
    /// A non-shell tool call (recorded as a single tool-named subcommand).
    Tool(&'a ToolCall),
}

/// One recorded evaluation. Stored as a single JSON line in `events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unix seconds when recorded.
    pub ts: u64,
    /// The harness that produced the call (`claude-code`, `cursor`, …).
    pub harness: String,
    /// The project/cwd the call ran in (the per-event tag).
    pub project: String,
    /// Whether this was a shell command or a tool call.
    pub kind: EventKind,
    /// The full command line (shell) or tool name (tool).
    pub command: String,
    /// The overall verdict for the whole call.
    pub verdict: String,
    /// Per-parsed-subcommand decisions (empty for unparseable/empty commands).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragments: Vec<FragmentRecord>,
}

/// Whether an [`Event`] came from the shell engine or the tool engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A shell command line decomposed into role-tagged fragments.
    Shell,
    /// A non-shell tool call.
    Tool,
}

/// One parsed subcommand within an [`Event`], with the decision it drew.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentRecord {
    /// The subcommand (fragment argv joined by spaces, or the tool name).
    pub cmd: String,
    /// The structural role (`standalone`, `pipe_filter`, …, or `tool`).
    pub role: String,
    /// This fragment's verdict.
    pub verdict: String,
    /// The rule that decided it, when one matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

/// Per-verdict tallies plus the latest timestamp seen, for any aggregation key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    /// Times allowed.
    #[serde(default)]
    pub allow: u64,
    /// Times denied.
    #[serde(default)]
    pub deny: u64,
    /// Times surfaced for approval.
    #[serde(default)]
    pub ask: u64,
    /// Times deferred to the harness's own flow.
    #[serde(default)]
    pub defer: u64,
    /// Latest Unix-seconds timestamp folded into this key.
    #[serde(default)]
    pub last_ts: u64,
}

impl Counts {
    fn bump(&mut self, verdict: &str, ts: u64) {
        match verdict {
            "allow" => self.allow += 1,
            "deny" => self.deny += 1,
            "ask" => self.ask += 1,
            // Any unrecognized string is treated as a defer: the engine only ever
            // emits the four canonical verdicts, so this is purely defensive.
            _ => self.defer += 1,
        }
        if ts > self.last_ts {
            self.last_ts = ts;
        }
    }

    fn merge(&mut self, other: &Counts) {
        self.allow += other.allow;
        self.deny += other.deny;
        self.ask += other.ask;
        self.defer += other.defer;
        if other.last_ts > self.last_ts {
            self.last_ts = other.last_ts;
        }
    }

    /// Total evaluations across all four verdicts.
    pub fn total(&self) -> u64 {
        self.allow + self.deny + self.ask + self.defer
    }

    /// The count for one verdict.
    pub fn get(&self, verdict: Verdict) -> u64 {
        match verdict {
            Verdict::Allow => self.allow,
            Verdict::Deny => self.deny,
            Verdict::Ask => self.ask,
            Verdict::Defer => self.defer,
        }
    }
}

/// A subcommand's tallies plus a histogram of the rules that decided it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FragCounts {
    /// Per-verdict tallies for this subcommand.
    #[serde(default)]
    pub counts: Counts,
    /// How often each named rule decided this subcommand.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rules: BTreeMap<String, u64>,
}

/// The durable cumulative aggregate: the full history, precomputed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    /// Schema version, for forward migration.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Total events ever folded in.
    #[serde(default)]
    pub events_total: u64,
    /// Earliest event timestamp (Unix seconds), or 0 if none.
    #[serde(default)]
    pub first_ts: u64,
    /// Latest event timestamp (Unix seconds), or 0 if none.
    #[serde(default)]
    pub last_ts: u64,
    /// Verdict tallies across every call.
    #[serde(default)]
    pub overall: Counts,
    /// Per-project verdict tallies.
    #[serde(default)]
    pub projects: BTreeMap<String, Counts>,
    /// Per-harness verdict tallies.
    #[serde(default)]
    pub harnesses: BTreeMap<String, Counts>,
    /// Per-whole-command-line verdict tallies.
    #[serde(default)]
    pub commands: BTreeMap<String, Counts>,
    /// Per-parsed-subcommand verdict tallies (with rule attribution).
    #[serde(default)]
    pub fragments: BTreeMap<String, FragCounts>,
    /// Whether the `commands` map hit its cap and dropped detail into overflow.
    #[serde(default)]
    pub commands_truncated: bool,
    /// Whether the `fragments` map hit its cap and dropped detail into overflow.
    #[serde(default)]
    pub fragments_truncated: bool,
}

fn default_version() -> u32 {
    1
}

impl Default for Summary {
    fn default() -> Self {
        Summary {
            version: default_version(),
            events_total: 0,
            first_ts: 0,
            last_ts: 0,
            overall: Counts::default(),
            projects: BTreeMap::new(),
            harnesses: BTreeMap::new(),
            commands: BTreeMap::new(),
            fragments: BTreeMap::new(),
            commands_truncated: false,
            fragments_truncated: false,
        }
    }
}

impl Summary {
    /// Fold one event into the cumulative tallies.
    pub fn record(&mut self, event: &Event) {
        self.events_total += 1;
        if event.ts != 0 && (self.first_ts == 0 || event.ts < self.first_ts) {
            self.first_ts = event.ts;
        }
        if event.ts > self.last_ts {
            self.last_ts = event.ts;
        }
        self.overall.bump(&event.verdict, event.ts);
        dim_entry(&mut self.projects, &event.project).bump(&event.verdict, event.ts);
        dim_entry(&mut self.harnesses, &event.harness).bump(&event.verdict, event.ts);
        counts_entry(
            &mut self.commands,
            &event.command,
            MAX_KEYS,
            &mut self.commands_truncated,
        )
        .bump(&event.verdict, event.ts);
        for fragment in &event.fragments {
            let entry = frag_entry(
                &mut self.fragments,
                &fragment.cmd,
                MAX_KEYS,
                &mut self.fragments_truncated,
            );
            entry.counts.bump(&fragment.verdict, event.ts);
            if let Some(rule) = &fragment.rule {
                bump_rule(&mut entry.rules, rule);
            }
        }
    }
}

/// Pick the live entry for `key`, collapsing into [`OVERFLOW`] once the map is at
/// `cap` and `key` is new — so the map size is bounded while totals stay exact.
fn counts_entry<'a>(
    map: &'a mut BTreeMap<String, Counts>,
    key: &str,
    cap: usize,
    truncated: &mut bool,
) -> &'a mut Counts {
    if !map.contains_key(key) && map.len() >= cap {
        *truncated = true;
        map.entry(OVERFLOW.to_string()).or_default()
    } else {
        map.entry(key.to_string()).or_default()
    }
}

fn frag_entry<'a>(
    map: &'a mut BTreeMap<String, FragCounts>,
    key: &str,
    cap: usize,
    truncated: &mut bool,
) -> &'a mut FragCounts {
    if !map.contains_key(key) && map.len() >= cap {
        *truncated = true;
        map.entry(OVERFLOW.to_string()).or_default()
    } else {
        map.entry(key.to_string()).or_default()
    }
}

/// Low-cardinality dimension entry (projects/harnesses): same overflow rule, no
/// `truncated` flag since these rarely fill.
fn dim_entry<'a>(map: &'a mut BTreeMap<String, Counts>, key: &str) -> &'a mut Counts {
    let key = if map.contains_key(key) || map.len() < MAX_DIM_KEYS {
        key.to_string()
    } else {
        OVERFLOW.to_string()
    };
    map.entry(key).or_default()
}

fn bump_rule(map: &mut BTreeMap<String, u64>, rule: &str) {
    if map.contains_key(rule) || map.len() < MAX_RULES {
        *map.entry(rule.to_string()).or_default() += 1;
    } else {
        *map.entry(OVERFLOW.to_string()).or_default() += 1;
    }
}

/// Record one evaluation. Best-effort and fail-open: gated off by default,
/// returns silently when disabled or when no config home is resolvable, and
/// swallows every I/O error so the calling hook's decision is never affected.
pub fn record(
    enabled_in_config: bool,
    harness: &str,
    project: &str,
    subject: Subject,
    result: &DecisionResult,
) {
    if !resolve_enabled(enabled_in_config) {
        return;
    }
    let Some(dir) = configfs::default_history_dir(&Env::from_process()) else {
        return;
    };
    let event = build_event(harness, project, subject, result, now_secs());
    let _ = append_event(&dir, &event);
}

/// The `ALLOWLISTER_HISTORY` env var overrides the config toggle: `1`/`true`/
/// `on`/`yes` force recording on, anything else (e.g. `0`/`false`) forces it off.
/// Absent, the config value decides.
fn resolve_enabled(config_enabled: bool) -> bool {
    match std::env::var("ALLOWLISTER_HISTORY") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes" | "enable" | "enabled"
        ),
        Err(_) => config_enabled,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build an [`Event`] from a decision result. Pure, so it is unit-tested directly.
fn build_event(
    harness: &str,
    project: &str,
    subject: Subject,
    result: &DecisionResult,
    ts: u64,
) -> Event {
    let verdict = result.verdict.as_str().to_string();
    let (kind, command, fragments) = match subject {
        Subject::Shell(command) => {
            let fragments = result
                .fragments
                .iter()
                .take(MAX_FRAGMENTS)
                .map(|decision| FragmentRecord {
                    cmd: truncate(&decision.fragment.cmd_string()),
                    role: decision.fragment.role.as_str().to_string(),
                    verdict: decision.verdict.as_str().to_string(),
                    rule: decision.rule_name.clone(),
                })
                .collect();
            (EventKind::Shell, truncate(command), fragments)
        }
        Subject::Tool(call) => {
            // A tool call has no shell structure, so it is its own one subcommand.
            let fragment = FragmentRecord {
                cmd: truncate(&call.tool_name),
                role: "tool".to_string(),
                verdict: verdict.clone(),
                rule: None,
            };
            (EventKind::Tool, truncate(&call.tool_name), vec![fragment])
        }
    };
    Event {
        ts,
        harness: harness.to_string(),
        project: truncate(project),
        kind,
        command,
        verdict,
        fragments,
    }
}

/// Test-only re-export of [`build_event`] so sibling command tests can seed a
/// store the same way the hot path would, without making the builder public.
#[cfg(test)]
pub fn build_event_for_test(
    harness: &str,
    project: &str,
    subject: Subject,
    result: &DecisionResult,
    ts: u64,
) -> Event {
    build_event(harness, project, subject, result, ts)
}

fn truncate(value: &str) -> String {
    if value.chars().count() <= MAX_STR {
        value.to_string()
    } else {
        let mut out: String = value.chars().take(MAX_STR).collect();
        out.push('…');
        out
    }
}

/// Append one event line, then fold if the log has grown past the cap. Public so
/// tests can drive the on-disk format directly.
pub fn append_event(dir: &Path, event: &Event) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let line = match serde_json::to_string(event) {
        Ok(line) => line,
        // A non-serializable event is dropped rather than failing the hook; the
        // fixed shape above always serializes, so this is purely defensive.
        Err(_) => return Ok(()),
    };
    let path = dir.join(EVENTS);
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    drop(file);
    if len >= SEGMENT_CAP {
        let _ = fold(dir);
    }
    Ok(())
}

/// An exclusive lock held for the duration of a fold; removed on drop.
struct Lock {
    path: PathBuf,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Acquire the fold lock, reclaiming it if a previous holder left it stale.
/// `None` means another process holds a fresh lock — the caller simply skips the
/// fold this time (the log folds on a later call instead).
fn try_lock(dir: &Path) -> Option<Lock> {
    let path = dir.join(LOCK);
    if create_lock(&path) {
        return Some(Lock { path });
    }
    if lock_is_stale(&path) {
        let _ = fs::remove_file(&path);
        if create_lock(&path) {
            return Some(Lock { path });
        }
    }
    None
}

fn create_lock(path: &Path) -> bool {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .is_ok()
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age.as_secs() > LOCK_TTL_SECS)
        .unwrap_or(false)
}

/// Fold the raw event log into the durable summary and clear it. Serialized by
/// the lock so concurrent folds never double-count; a no-op when nothing is
/// pending or another fold holds the lock.
fn fold(dir: &Path) -> io::Result<()> {
    let _lock = match try_lock(dir) {
        Some(lock) => lock,
        None => return Ok(()),
    };
    // Move the active log aside atomically so new appends start a fresh file
    // while we consume the snapshot.
    let folding = dir.join(format!("events.{}.folding", std::process::id()));
    match fs::rename(dir.join(EVENTS), &folding) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    }
    let mut summary = load_summary(dir);
    fold_lines(
        &mut summary,
        &fs::read_to_string(&folding).unwrap_or_default(),
    );
    write_summary_atomic(dir, &summary)?;
    let _ = fs::remove_file(&folding);
    Ok(())
}

fn fold_lines(summary: &mut Summary, text: &str) {
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Event>(line) {
            summary.record(&event);
        }
    }
}

fn load_summary(dir: &Path) -> Summary {
    match fs::read_to_string(dir.join(SUMMARY)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Summary::default(),
    }
}

fn write_summary_atomic(dir: &Path, summary: &Summary) -> io::Result<()> {
    let tmp = dir.join(format!("summary.json.tmp.{}", std::process::id()));
    let json = serde_json::to_string(summary).map_err(io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, dir.join(SUMMARY))?;
    Ok(())
}

/// The full history as one summary: the durable aggregate plus the not-yet-folded
/// events, computed without touching disk. This is what reporting reads.
pub fn aggregate(dir: &Path) -> Summary {
    let mut summary = load_summary(dir);
    fold_lines(
        &mut summary,
        &fs::read_to_string(dir.join(EVENTS)).unwrap_or_default(),
    );
    summary
}

/// The recent raw events (the bounded detail window; folded events are gone from
/// here but live on in the summary's counts), oldest first.
pub fn read_events(dir: &Path) -> Vec<Event> {
    fs::read_to_string(dir.join(EVENTS))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Event>(line).ok())
        .collect()
}

/// Fold the recent log into the summary now (the `history compact` verb).
pub fn compact(dir: &Path) -> io::Result<()> {
    fold(dir)
}

/// Delete every history artifact under `dir` (events, summary, lock, temp files),
/// leaving any unrelated files in place.
pub fn clear(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == SUMMARY
            || name == LOCK
            || name.starts_with("events")
            || name.starts_with("summary.json.tmp.")
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// One row of a frequency report: an aggregation key with its tallies and the
/// rules that decided it.
#[derive(Debug, Clone)]
pub struct Row {
    /// The command line, subcommand, or program this row aggregates.
    pub key: String,
    /// Verdict tallies for the key.
    pub counts: Counts,
    /// Rule-attribution histogram (empty for whole-command rows).
    pub rules: BTreeMap<String, u64>,
}

/// Per-subcommand frequency rows. With `program`, subcommands are collapsed to
/// their leading program token (`git push …` and `git status` both count under
/// `git`); otherwise the full subcommand is the key. `verdict` filters to rows
/// with that verdict and sorts by it; otherwise rows sort by total. `top` caps
/// the result.
pub fn fragment_rows(
    summary: &Summary,
    program: bool,
    verdict: Option<Verdict>,
    top: usize,
) -> Vec<Row> {
    let mut grouped: BTreeMap<String, (Counts, BTreeMap<String, u64>)> = BTreeMap::new();
    for (key, frag) in &summary.fragments {
        let key = if program {
            program_of(key)
        } else {
            key.clone()
        };
        let entry = grouped.entry(key).or_default();
        entry.0.merge(&frag.counts);
        for (rule, count) in &frag.rules {
            *entry.1.entry(rule.clone()).or_default() += count;
        }
    }
    let mut rows: Vec<Row> = grouped
        .into_iter()
        .map(|(key, (counts, rules))| Row { key, counts, rules })
        .collect();
    sort_and_take(&mut rows, verdict, top);
    rows
}

/// Per-whole-command-line frequency rows (the "overall full command" view).
pub fn command_rows(summary: &Summary, verdict: Option<Verdict>, top: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = summary
        .commands
        .iter()
        .map(|(key, counts)| Row {
            key: key.clone(),
            counts: counts.clone(),
            rules: BTreeMap::new(),
        })
        .collect();
    sort_and_take(&mut rows, verdict, top);
    rows
}

fn sort_and_take(rows: &mut Vec<Row>, verdict: Option<Verdict>, top: usize) {
    match verdict {
        Some(v) => {
            rows.retain(|row| row.counts.get(v) > 0);
            rows.sort_by(|a, b| {
                b.counts
                    .get(v)
                    .cmp(&a.counts.get(v))
                    .then_with(|| a.key.cmp(&b.key))
            });
        }
        None => rows.sort_by(|a, b| {
            b.counts
                .total()
                .cmp(&a.counts.total())
                .then_with(|| a.key.cmp(&b.key))
        }),
    }
    if rows.len() > top {
        rows.truncate(top);
    }
}

fn program_of(subcommand: &str) -> String {
    subcommand
        .split_whitespace()
        .next()
        .unwrap_or(subcommand)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{self, evaluate, NormalizedParams, ToolCall};
    use serde_json::json;
    use tempfile::TempDir;

    fn shell_event(command: &str, project: &str, ts: u64) -> Event {
        // Use a tiny ruleset so fragments carry real verdicts/roles.
        let cfg = crate::config::compile_str(
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
            "test",
        );
        let result = evaluate(command, &cfg.rules);
        build_event("claude-code", project, Subject::Shell(command), &result, ts)
    }

    #[test]
    fn build_event_shell_carries_fragments() {
        let cfg = crate::config::compile_str(
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
            "t",
        );
        let result = evaluate("ls -la | grep x", &cfg.rules);
        let event = build_event(
            "claude-code",
            "/repo",
            Subject::Shell("ls -la | grep x"),
            &result,
            5,
        );
        assert_eq!(event.kind, EventKind::Shell);
        assert_eq!(event.command, "ls -la | grep x");
        assert_eq!(event.fragments.len(), 2);
        assert_eq!(event.fragments[0].cmd, "ls -la");
        assert_eq!(event.fragments[0].verdict, "allow");
        assert_eq!(event.fragments[0].rule.as_deref(), Some("ls"));
        // The grep filter has no rule here, so it defers and cites no rule.
        assert_eq!(event.fragments[1].cmd, "grep x");
        assert_eq!(event.fragments[1].verdict, "defer");
        assert!(event.fragments[1].rule.is_none());
    }

    #[test]
    fn build_event_tool_is_one_subcommand() {
        let mut params = NormalizedParams::new();
        params.insert(domain::ParamKey::Path, "/repo/x".to_string());
        let call = ToolCall::new(
            domain::Capability::Read,
            "Read".to_string(),
            params,
            json!({}),
        );
        let result = DecisionResult {
            verdict: Verdict::Deny,
            reason: "x".to_string(),
            fragments: Vec::new(),
            warnings: Vec::new(),
            unsupported: Vec::new(),
        };
        let event = build_event("cursor", "/repo", Subject::Tool(&call), &result, 9);
        assert_eq!(event.kind, EventKind::Tool);
        assert_eq!(event.command, "Read");
        assert_eq!(event.fragments.len(), 1);
        assert_eq!(event.fragments[0].cmd, "Read");
        assert_eq!(event.fragments[0].role, "tool");
        assert_eq!(event.fragments[0].verdict, "deny");
    }

    #[test]
    fn truncate_caps_length_and_keeps_utf8() {
        let long = "é".repeat(MAX_STR + 50);
        let out = truncate(&long);
        // MAX_STR chars plus the ellipsis marker.
        assert_eq!(out.chars().count(), MAX_STR + 1);
        assert!(out.ends_with('…'));
        assert_eq!(truncate("short"), "short");
    }

    #[test]
    fn summary_records_overall_command_and_fragment_counts() {
        let mut summary = Summary::default();
        summary.record(&shell_event("ls -la", "/a", 10));
        summary.record(&shell_event("ls -la", "/a", 20));
        summary.record(&shell_event("ls foo | grep x", "/b", 30));
        assert_eq!(summary.events_total, 3);
        assert_eq!(summary.first_ts, 10);
        assert_eq!(summary.last_ts, 30);
        assert_eq!(summary.overall.allow, 2); // two pure-ls commands allowed
        assert_eq!(summary.overall.defer, 1); // the grep pipeline defers
        assert_eq!(summary.commands["ls -la"].allow, 2);
        assert_eq!(summary.fragments["ls -la"].counts.allow, 2);
        assert_eq!(summary.fragments["ls -la"].rules["ls"], 2);
        assert_eq!(summary.fragments["grep x"].counts.defer, 1);
        assert_eq!(summary.projects["/a"].allow, 2);
        assert_eq!(summary.harnesses["claude-code"].total(), 3);
    }

    #[test]
    fn maps_overflow_into_a_single_bucket_when_full() {
        let mut map = BTreeMap::new();
        let mut truncated = false;
        for i in 0..(MAX_KEYS + 5) {
            counts_entry(&mut map, &format!("cmd{i}"), MAX_KEYS, &mut truncated).bump("allow", 1);
        }
        assert!(truncated);
        assert_eq!(map.len(), MAX_KEYS + 1); // capped keys + the overflow bucket
        assert_eq!(map[OVERFLOW].allow, 5);
    }

    #[test]
    fn rules_histogram_overflows_too() {
        let mut rules = BTreeMap::new();
        for i in 0..(MAX_RULES + 3) {
            bump_rule(&mut rules, &format!("rule{i}"));
        }
        assert_eq!(rules.len(), MAX_RULES + 1);
        assert_eq!(rules[OVERFLOW], 3);
    }

    #[test]
    fn append_then_aggregate_round_trips() {
        let dir = TempDir::new().unwrap();
        append_event(dir.path(), &shell_event("ls -la", "/a", 1)).unwrap();
        append_event(dir.path(), &shell_event("ls bar | grep z", "/a", 2)).unwrap();
        let summary = aggregate(dir.path());
        assert_eq!(summary.events_total, 2);
        assert_eq!(summary.fragments["ls -la"].counts.allow, 1);
        assert_eq!(read_events(dir.path()).len(), 2);
    }

    #[test]
    fn compact_folds_events_into_summary_and_clears_the_log() {
        let dir = TempDir::new().unwrap();
        append_event(dir.path(), &shell_event("ls -la", "/a", 1)).unwrap();
        append_event(dir.path(), &shell_event("ls -la", "/a", 2)).unwrap();
        compact(dir.path()).unwrap();
        // Raw events folded away; counts preserved in the durable summary.
        assert!(read_events(dir.path()).is_empty());
        let summary = aggregate(dir.path());
        assert_eq!(summary.events_total, 2);
        assert_eq!(summary.commands["ls -la"].allow, 2);
        // A second compact with nothing pending is a harmless no-op.
        compact(dir.path()).unwrap();
        assert_eq!(aggregate(dir.path()).events_total, 2);
    }

    #[test]
    fn compact_accumulates_across_rounds() {
        let dir = TempDir::new().unwrap();
        append_event(dir.path(), &shell_event("ls a", "/a", 1)).unwrap();
        compact(dir.path()).unwrap();
        append_event(dir.path(), &shell_event("ls a", "/a", 2)).unwrap();
        compact(dir.path()).unwrap();
        assert_eq!(aggregate(dir.path()).commands["ls a"].allow, 2);
    }

    #[test]
    fn clear_removes_all_artifacts() {
        let dir = TempDir::new().unwrap();
        append_event(dir.path(), &shell_event("ls -la", "/a", 1)).unwrap();
        compact(dir.path()).unwrap();
        append_event(dir.path(), &shell_event("ls -la", "/a", 2)).unwrap();
        clear(dir.path()).unwrap();
        assert_eq!(aggregate(dir.path()).events_total, 0);
        // Clearing a never-used dir is fine.
        let empty = TempDir::new().unwrap();
        clear(empty.path()).unwrap();
        clear(&empty.path().join("missing")).unwrap();
    }

    #[test]
    fn fragment_rows_sort_by_total_then_filter_by_verdict() {
        let mut summary = Summary::default();
        for _ in 0..3 {
            summary.record(&shell_event("ls -la", "/a", 1));
        }
        summary.record(&shell_event("ls x | grep y", "/a", 1));
        let rows = fragment_rows(&summary, false, None, 10);
        assert_eq!(rows[0].key, "ls -la");
        assert_eq!(rows[0].counts.allow, 3);
        // Filtering to defer drops the always-allowed ls row.
        let deferred = fragment_rows(&summary, false, Some(Verdict::Defer), 10);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].key, "grep y");
    }

    #[test]
    fn fragment_rows_group_by_program() {
        let mut summary = Summary::default();
        summary.record(&shell_event("ls -la", "/a", 1));
        summary.record(&shell_event("ls foo", "/a", 1));
        let rows = fragment_rows(&summary, true, None, 10);
        // Both ls subcommands collapse under the `ls` program.
        let ls = rows.iter().find(|r| r.key == "ls").unwrap();
        assert_eq!(ls.counts.allow, 2);
        assert!(ls.rules.contains_key("ls"));
    }

    #[test]
    fn command_rows_respect_top_and_verdict() {
        let mut summary = Summary::default();
        summary.record(&shell_event("ls -la", "/a", 1));
        summary.record(&shell_event("ls foo | grep y", "/a", 1));
        let rows = command_rows(&summary, None, 1);
        assert_eq!(rows.len(), 1);
        let deferred = command_rows(&summary, Some(Verdict::Defer), 10);
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].key, "ls foo | grep y");
    }

    #[test]
    fn resolve_enabled_env_overrides_config() {
        // Each nextest test runs in its own process, so mutating the env is safe.
        std::env::remove_var("ALLOWLISTER_HISTORY");
        assert!(resolve_enabled(true));
        assert!(!resolve_enabled(false));
        std::env::set_var("ALLOWLISTER_HISTORY", "1");
        assert!(resolve_enabled(false));
        std::env::set_var("ALLOWLISTER_HISTORY", "0");
        assert!(!resolve_enabled(true));
        std::env::remove_var("ALLOWLISTER_HISTORY");
    }

    #[test]
    fn record_writes_when_forced_on_via_env() {
        let dir = TempDir::new().unwrap();
        std::env::set_var("ALLOWLISTER_HISTORY", "1");
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let cfg = crate::config::compile_str(
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
            "t",
        );
        let result = evaluate("ls -la", &cfg.rules);
        record(
            false,
            "claude-code",
            "/repo",
            Subject::Shell("ls -la"),
            &result,
        );
        let history = dir.path().join("allowlister").join("history");
        let summary = aggregate(&history);
        assert_eq!(summary.events_total, 1);
        assert_eq!(summary.commands["ls -la"].allow, 1);
        std::env::remove_var("ALLOWLISTER_HISTORY");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn record_is_a_noop_when_disabled() {
        let dir = TempDir::new().unwrap();
        std::env::remove_var("ALLOWLISTER_HISTORY");
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        let result = evaluate("ls -la", &[]);
        record(
            false,
            "claude-code",
            "/repo",
            Subject::Shell("ls -la"),
            &result,
        );
        assert!(!dir.path().join("allowlister").join("history").exists());
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn malformed_summary_falls_back_to_default() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(dir.path().join(SUMMARY), "{not json").unwrap();
        // A corrupt summary must not crash reads; it resets to empty.
        assert_eq!(aggregate(dir.path()).events_total, 0);
    }

    #[test]
    fn fold_skips_when_lock_is_held_and_reclaims_when_stale() {
        let dir = TempDir::new().unwrap();
        append_event(dir.path(), &shell_event("ls -la", "/a", 1)).unwrap();
        // A fresh foreign lock blocks the fold (events stay pending).
        let lock = dir.path().join(LOCK);
        fs::write(&lock, "").unwrap();
        compact(dir.path()).unwrap();
        assert_eq!(
            read_events(dir.path()).len(),
            1,
            "held lock blocks the fold"
        );
        // try_lock cannot acquire a fresh foreign lock.
        assert!(try_lock(dir.path()).is_none());
        // Remove it; now the fold proceeds.
        fs::remove_file(&lock).unwrap();
        compact(dir.path()).unwrap();
        assert!(read_events(dir.path()).is_empty());
        assert_eq!(aggregate(dir.path()).events_total, 1);
    }
}
