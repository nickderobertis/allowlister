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
//!   full-history counts survive forever. Time survives folding the same way:
//!   each key keeps first/last timestamps and fixed-size decayed [`Recency`]
//!   weights, never a per-event timeline.
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
pub(crate) const OVERFLOW: &str = "(other)";

/// Fold `events.jsonl` into the summary once it exceeds this many bytes (~1 MiB).
const SEGMENT_CAP: u64 = 1_000_000;
/// Cap on distinct keys in the `commands` and `fragments` maps.
const MAX_KEYS: usize = 5_000;
/// Cap on distinct keys in the low-cardinality `projects`/`harnesses` maps.
const MAX_DIM_KEYS: usize = 1_000;
/// Cap on distinct rule names tracked per subcommand.
const MAX_RULES: usize = 16;
/// Cap on distinct projects tracked per subcommand (with an overflow bucket), so
/// the per-fragment project breakdown stays bounded regardless of how many
/// repositories a fragment is ever run in.
const MAX_FRAG_PROJECTS: usize = 64;
/// Cap on fragments recorded per event (defends against pathological input).
const MAX_FRAGMENTS: usize = 64;
/// Cap on the character length of a stored command/subcommand/project string,
/// keeping each event line small enough for atomic appends.
const MAX_STR: usize = 1_000;
/// A lock older than this (by mtime) is treated as abandoned and reclaimed.
const LOCK_TTL_SECS: u64 = 120;
/// Half-life of the recency weights: each verdict's [`Recency`] value is the sum
/// of `0.5^(age / 30 days)` over that verdict's events, so month-old activity
/// counts half and a burst of use long ago decays toward zero. This keeps "is it
/// still relevant?" answerable from a few numbers per key instead of a timeline.
pub(crate) const RECENT_HALF_LIFE_SECS: u64 = 30 * 86_400;
/// Weights below this are clamped to zero so fully-decayed keys drop their
/// `recent` field from the stored JSON instead of carrying dust forever.
const RECENT_EPSILON: f64 = 1e-9;

/// The multiplicative decay a weight undergoes over `age_secs`.
fn decay_factor(age_secs: u64) -> f64 {
    (-(age_secs as f64) * std::f64::consts::LN_2 / RECENT_HALF_LIFE_SECS as f64).exp()
}

fn f64_is_zero(value: &f64) -> bool {
    *value == 0.0
}

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
    /// The project the call ran in (the per-event tag): the repository identity
    /// when the cwd is inside a git repo, else the cwd itself. See
    /// [`crate::io::project`].
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

/// Recency-weighted per-verdict activity: each field is the decayed sum of that
/// verdict's events ([`RECENT_HALF_LIFE_SECS`]), anchored at the owning
/// [`Counts`]'s `last_ts`. Fixed-size by construction — recency costs a few
/// numbers per key, never a growing timeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Recency {
    /// Decayed weight of allow events.
    #[serde(default, skip_serializing_if = "f64_is_zero")]
    pub allow: f64,
    /// Decayed weight of deny events.
    #[serde(default, skip_serializing_if = "f64_is_zero")]
    pub deny: f64,
    /// Decayed weight of ask events.
    #[serde(default, skip_serializing_if = "f64_is_zero")]
    pub ask: f64,
    /// Decayed weight of defer events.
    #[serde(default, skip_serializing_if = "f64_is_zero")]
    pub defer: f64,
}

impl Recency {
    /// True when every weight has fully decayed (or nothing was ever recorded),
    /// so the field can be omitted from stored and reported JSON.
    pub fn is_empty(&self) -> bool {
        self.allow == 0.0 && self.deny == 0.0 && self.ask == 0.0 && self.defer == 0.0
    }

    fn scale(&mut self, factor: f64) {
        for value in [
            &mut self.allow,
            &mut self.deny,
            &mut self.ask,
            &mut self.defer,
        ] {
            *value *= factor;
            if *value < RECENT_EPSILON {
                *value = 0.0;
            }
        }
    }

    fn add(&mut self, other: &Recency) {
        self.allow += other.allow;
        self.deny += other.deny;
        self.ask += other.ask;
        self.defer += other.defer;
    }

    fn bump(&mut self, verdict: &str, weight: f64) {
        match verdict {
            "allow" => self.allow += weight,
            "deny" => self.deny += weight,
            "ask" => self.ask += weight,
            _ => self.defer += weight,
        }
    }

    /// Total decayed weight across all four verdicts.
    pub fn total(&self) -> f64 {
        self.allow + self.deny + self.ask + self.defer
    }

    /// The decayed weight for one verdict.
    pub fn get(&self, verdict: Verdict) -> f64 {
        match verdict {
            Verdict::Allow => self.allow,
            Verdict::Deny => self.deny,
            Verdict::Ask => self.ask,
            Verdict::Defer => self.defer,
        }
    }
}

/// Per-verdict tallies plus the time shape of the key's use: first/latest
/// timestamps and the decayed [`Recency`] weights.
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
    /// Earliest Unix-seconds timestamp folded into this key (0 when unknown).
    #[serde(default)]
    pub first_ts: u64,
    /// Latest Unix-seconds timestamp folded into this key.
    #[serde(default)]
    pub last_ts: u64,
    /// Recency-weighted per-verdict activity, anchored at `last_ts`.
    #[serde(default, skip_serializing_if = "Recency::is_empty")]
    pub recent: Recency,
}

impl Counts {
    fn bump(&mut self, verdict: &str, ts: u64) {
        // Re-anchor the weights at the later of the stored anchor and this
        // event: decay the stored weights forward, or — when concurrent hooks
        // fold events out of order — decay this event's unit weight backward.
        // Either way the sum stays exact regardless of arrival order.
        let weight = if ts >= self.last_ts {
            self.recent.scale(decay_factor(ts - self.last_ts));
            1.0
        } else {
            decay_factor(self.last_ts - ts)
        };
        self.recent.bump(verdict, weight);
        match verdict {
            "allow" => self.allow += 1,
            "deny" => self.deny += 1,
            "ask" => self.ask += 1,
            // Any unrecognized string is treated as a defer: the engine only ever
            // emits the four canonical verdicts, so this is purely defensive.
            _ => self.defer += 1,
        }
        if ts != 0 && (self.first_ts == 0 || ts < self.first_ts) {
            self.first_ts = ts;
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
        // Re-anchor both weight sets at the later timestamp before adding.
        if other.last_ts >= self.last_ts {
            self.recent
                .scale(decay_factor(other.last_ts - self.last_ts));
            self.recent.add(&other.recent);
        } else {
            let mut incoming = other.recent;
            incoming.scale(decay_factor(self.last_ts - other.last_ts));
            self.recent.add(&incoming);
        }
        if other.first_ts != 0 && (self.first_ts == 0 || other.first_ts < self.first_ts) {
            self.first_ts = other.first_ts;
        }
        if other.last_ts > self.last_ts {
            self.last_ts = other.last_ts;
        }
    }

    /// The recency weights decayed from their `last_ts` anchor to `now`, so
    /// reports compare every key at the same moment.
    pub fn recent_at(&self, now: u64) -> Recency {
        let mut recent = self.recent;
        recent.scale(decay_factor(now.saturating_sub(self.last_ts)));
        recent
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

/// A subcommand's tallies plus a histogram of the rules that decided it and the
/// projects it ran in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FragCounts {
    /// Per-verdict tallies for this subcommand.
    #[serde(default)]
    pub counts: Counts,
    /// How often each named rule decided this subcommand.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rules: BTreeMap<String, u64>,
    /// Per-verdict tallies split by the project/cwd the subcommand ran in. Bounded
    /// by [`MAX_FRAG_PROJECTS`], with the long tail collapsed into [`OVERFLOW`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, Counts>,
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
        capped_counts_entry(&mut self.projects, &event.project, MAX_DIM_KEYS)
            .bump(&event.verdict, event.ts);
        capped_counts_entry(&mut self.harnesses, &event.harness, MAX_DIM_KEYS)
            .bump(&event.verdict, event.ts);
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
            capped_counts_entry(&mut entry.projects, &event.project, MAX_FRAG_PROJECTS)
                .bump(&fragment.verdict, event.ts);
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

/// A `Counts` dimension entry that collapses into [`OVERFLOW`] once the map holds
/// `cap` distinct keys — same overflow rule as [`counts_entry`] but without a
/// `truncated` flag, for the low-to-moderate-cardinality dimensions
/// (projects/harnesses, and per-fragment projects) that rarely fill.
fn capped_counts_entry<'a>(
    map: &'a mut BTreeMap<String, Counts>,
    key: &str,
    cap: usize,
) -> &'a mut Counts {
    let key = if map.contains_key(key) || map.len() < cap {
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
///
/// `project` is the working directory the call ran in; it is resolved to a
/// durable repository identity ([`crate::io::project::identify`]) before tagging,
/// so the same repo's clones and subdirectories aggregate. The repo lookup is
/// done here, after the enabled check, so a disabled store costs nothing.
pub fn record(
    enabled_in_config: bool,
    harness: &str,
    project: &str,
    subject: Subject,
    result: &DecisionResult,
) {
    record_with(
        &Env::from_process(),
        enabled_in_config,
        harness,
        project,
        subject,
        result,
    );
}

/// [`record`] with the environment injected, so tests exercise the real
/// recording path (toggle resolution, store-dir discovery, append) without
/// mutating process-global env vars.
fn record_with(
    env: &Env,
    enabled_in_config: bool,
    harness: &str,
    project: &str,
    subject: Subject,
    result: &DecisionResult,
) {
    if !resolve_enabled(env, enabled_in_config) {
        return;
    }
    let Some(dir) = configfs::default_history_dir(env) else {
        return;
    };
    let project = crate::io::project::identify(project);
    let event = build_event(harness, &project, subject, result, now_secs());
    let _ = append_event(&dir, &event);
}

/// The `ALLOWLISTER_HISTORY` override (carried on [`Env`]) beats the config
/// toggle: `1`/`true`/`on`/`yes` force recording on, anything else (e.g.
/// `0`/`false`) forces it off. Absent, the config value decides.
fn resolve_enabled(env: &Env, config_enabled: bool) -> bool {
    match &env.history_override {
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes" | "enable" | "enabled"
        ),
        None => config_enabled,
    }
}

/// Unix seconds now, or 0 when the clock is unreadable (recency then degrades
/// gracefully: such events count but carry no usable anchor).
pub(crate) fn now_secs() -> u64 {
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

/// One row of a frequency report: an aggregation key with its tallies, the
/// rules that decided it, and the projects it ran in.
#[derive(Debug, Clone)]
pub struct Row {
    /// The command line, subcommand, or program this row aggregates.
    pub key: String,
    /// Verdict tallies for the key.
    pub counts: Counts,
    /// Rule-attribution histogram (empty for whole-command rows).
    pub rules: BTreeMap<String, u64>,
    /// Per-project verdict tallies (empty for whole-command rows, which the store
    /// does not break down by project).
    pub projects: BTreeMap<String, Counts>,
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
    type Grouped = (Counts, BTreeMap<String, u64>, BTreeMap<String, Counts>);
    let mut grouped: BTreeMap<String, Grouped> = BTreeMap::new();
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
        for (project, counts) in &frag.projects {
            entry.2.entry(project.clone()).or_default().merge(counts);
        }
    }
    let mut rows: Vec<Row> = grouped
        .into_iter()
        .map(|(key, (counts, rules, projects))| Row {
            key,
            counts,
            rules,
            projects,
        })
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
            projects: BTreeMap::new(),
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
        // Each fragment is also broken down by the project it ran in.
        assert_eq!(summary.fragments["ls -la"].projects["/a"].allow, 2);
        assert_eq!(summary.fragments["ls foo"].projects["/b"].allow, 1);
        assert_eq!(summary.fragments["grep x"].projects["/b"].defer, 1);
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected ≈{expected}, got {actual}"
        );
    }

    #[test]
    fn recency_halves_per_half_life_and_first_last_track() {
        let mut counts = Counts::default();
        counts.bump("defer", 1_000);
        assert_close(counts.recent.defer, 1.0);
        // One half-life later the old weight is worth 0.5, plus the new event.
        counts.bump("defer", 1_000 + RECENT_HALF_LIFE_SECS);
        assert_close(counts.recent.defer, 1.5);
        assert_eq!(counts.first_ts, 1_000);
        assert_eq!(counts.last_ts, 1_000 + RECENT_HALF_LIFE_SECS);
        assert_eq!(counts.defer, 2);
    }

    #[test]
    fn recency_is_independent_of_event_order() {
        let (early, late) = (5_000, 5_000 + RECENT_HALF_LIFE_SECS);
        let mut forward = Counts::default();
        forward.bump("allow", early);
        forward.bump("allow", late);
        let mut backward = Counts::default();
        backward.bump("allow", late);
        backward.bump("allow", early);
        assert_close(forward.recent.allow, backward.recent.allow);
        assert_eq!(forward.first_ts, backward.first_ts);
        assert_eq!(forward.last_ts, backward.last_ts);
    }

    #[test]
    fn merge_re_anchors_recency_at_the_later_timestamp() {
        let mut older = Counts::default();
        older.bump("ask", 1_000);
        let mut newer = Counts::default();
        newer.bump("ask", 1_000 + RECENT_HALF_LIFE_SECS);
        let mut merged_into_newer = newer.clone();
        merged_into_newer.merge(&older);
        let mut merged_into_older = older.clone();
        merged_into_older.merge(&newer);
        // Merging in either direction yields the same anchored weight: the old
        // side decayed one half-life plus the new side at full weight.
        assert_close(merged_into_newer.recent.ask, 1.5);
        assert_close(merged_into_older.recent.ask, 1.5);
        assert_eq!(merged_into_older.first_ts, 1_000);
        assert_eq!(merged_into_older.last_ts, 1_000 + RECENT_HALF_LIFE_SECS);
    }

    #[test]
    fn recent_at_decays_to_the_report_time_and_clamps_dust() {
        let mut counts = Counts::default();
        counts.bump("defer", 1_000);
        assert_close(counts.recent_at(1_000).defer, 1.0);
        assert_close(counts.recent_at(1_000 + RECENT_HALF_LIFE_SECS).defer, 0.5);
        // A burst far in the past fully decays: heavy old use is not relevant.
        let long_dead = counts.recent_at(1_000 + 200 * RECENT_HALF_LIFE_SECS);
        assert!(long_dead.is_empty());
        assert_close(long_dead.total(), 0.0);
        // Clock skew (now before the anchor) must not inflate the weight.
        assert_close(counts.recent_at(0).defer, 1.0);
    }

    #[test]
    fn counts_json_skips_recency_when_empty_and_round_trips_when_set() {
        let empty = serde_json::to_value(Counts::default()).unwrap();
        assert!(empty.get("recent").is_none());
        let mut counts = Counts::default();
        counts.bump("allow", 42);
        let value = serde_json::to_value(&counts).unwrap();
        assert_eq!(value["recent"]["allow"], 1.0);
        assert!(
            value["recent"].get("deny").is_none(),
            "zero weights skipped"
        );
        assert_eq!(value["first_ts"], 42);
        let back: Counts = serde_json::from_value(value).unwrap();
        assert_close(back.recent.allow, 1.0);
        // A pre-recency summary (no recent/first_ts fields) still deserializes.
        let legacy: Counts = serde_json::from_str(r#"{"allow":3,"last_ts":9}"#).unwrap();
        assert_eq!(legacy.allow, 3);
        assert!(legacy.recent.is_empty());
        assert_eq!(legacy.first_ts, 0);
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
    fn legacy_folder_keyed_summary_survives_new_repo_keyed_events() {
        // Upgrade safety: a summary written before repo-identity tagging keys its
        // projects by folder path. After the upgrade, new events key by repo
        // identity instead. The old folder counts must persist untouched (no
        // migration, no re-keying) while the new identity accumulates alongside.
        let dir = TempDir::new().unwrap();
        let mut legacy = Summary::default();
        legacy.record(&shell_event("ls -la", "/home/user/myrepo", 1));
        legacy.record(&shell_event("ls -la", "/home/user/myrepo", 2));
        fs::write(
            dir.path().join(SUMMARY),
            serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();

        // A new event tagged by repo identity, the post-upgrade shape.
        append_event(
            dir.path(),
            &shell_event("ls -la", "github.com/octocat/Hello-World", 3),
        )
        .unwrap();

        let summary = aggregate(dir.path());
        let frag = &summary.fragments["ls -la"];
        // The pre-upgrade folder key is exactly as it was.
        assert_eq!(frag.projects["/home/user/myrepo"].allow, 2);
        // The repo identity is a separate, new key — both coexist, nothing merged.
        assert_eq!(frag.projects["github.com/octocat/Hello-World"].allow, 1);
        assert_eq!(summary.events_total, 3);
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
    fn fragment_rows_carry_and_merge_projects() {
        let mut summary = Summary::default();
        summary.record(&shell_event("ls -la", "/a", 1));
        summary.record(&shell_event("ls -la", "/b", 1));
        summary.record(&shell_event("ls foo", "/a", 1));
        // Subcommand view keeps each fragment's own project split.
        let rows = fragment_rows(&summary, false, None, 10);
        let la = rows.iter().find(|r| r.key == "ls -la").unwrap();
        assert_eq!(la.projects.len(), 2);
        assert_eq!(la.projects["/a"].allow, 1);
        assert_eq!(la.projects["/b"].allow, 1);
        // Program view merges the project splits of every collapsed subcommand.
        let programs = fragment_rows(&summary, true, None, 10);
        let ls = programs.iter().find(|r| r.key == "ls").unwrap();
        assert_eq!(ls.projects["/a"].allow, 2); // ls -la + ls foo, both in /a
        assert_eq!(ls.projects["/b"].allow, 1);
    }

    #[test]
    fn per_fragment_projects_overflow_into_a_single_bucket() {
        let mut summary = Summary::default();
        for i in 0..(MAX_FRAG_PROJECTS + 3) {
            summary.record(&shell_event("ls -la", &format!("/p{i}"), 1));
        }
        let projects = &summary.fragments["ls -la"].projects;
        assert_eq!(projects.len(), MAX_FRAG_PROJECTS + 1); // capped keys + overflow
        assert_eq!(projects[OVERFLOW].allow, 3);
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

    /// An [`Env`] whose only configured input is the history override, with the
    /// store rooted under `xdg`. Avoids mutating process-global env vars, so these
    /// tests are race-free under any test runner.
    fn env_with(xdg: &Path, history_override: Option<&str>) -> Env {
        Env {
            home: None,
            xdg_config_home: Some(xdg.to_path_buf()),
            history_override: history_override.map(str::to_string),
        }
    }

    #[test]
    fn resolve_enabled_env_overrides_config() {
        let absent = Env::default();
        assert!(resolve_enabled(&absent, true));
        assert!(!resolve_enabled(&absent, false));

        let forced_on = Env {
            history_override: Some("1".to_string()),
            ..Env::default()
        };
        assert!(resolve_enabled(&forced_on, false));

        let forced_off = Env {
            history_override: Some("0".to_string()),
            ..Env::default()
        };
        assert!(!resolve_enabled(&forced_off, true));
    }

    #[test]
    fn record_writes_when_forced_on_via_env() {
        let dir = TempDir::new().unwrap();
        let env = env_with(dir.path(), Some("1"));
        let cfg = crate::config::compile_str(
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
            "t",
        );
        let result = evaluate("ls -la", &cfg.rules);
        // Config disables recording; the env override forces it on.
        record_with(
            &env,
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
    }

    #[test]
    fn record_is_a_noop_when_disabled() {
        let dir = TempDir::new().unwrap();
        let env = env_with(dir.path(), None);
        let result = evaluate("ls -la", &[]);
        // No override and config disabled: nothing is written.
        record_with(
            &env,
            false,
            "claude-code",
            "/repo",
            Subject::Shell("ls -la"),
            &result,
        );
        assert!(!dir.path().join("allowlister").join("history").exists());
    }

    #[test]
    fn record_with_env_override_off_beats_enabled_config() {
        // The mirror of the forced-on case: config enables recording but the env
        // override turns it off, so no store is written.
        let dir = TempDir::new().unwrap();
        let env = env_with(dir.path(), Some("0"));
        let result = evaluate("ls -la", &[]);
        record_with(
            &env,
            true,
            "claude-code",
            "/repo",
            Subject::Shell("ls -la"),
            &result,
        );
        assert!(!dir.path().join("allowlister").join("history").exists());
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
