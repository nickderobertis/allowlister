//! The shared hook chokepoint: evaluate a call through the pure engine and, as a
//! side effect, record it to usage history.
//!
//! Every harness adapter routes its decision through here instead of calling the
//! engine directly, so recording lives in exactly one place. It cannot live in
//! [`crate::domain`] (which is pure — no I/O), and there is no single function
//! every adapter already shares, since each one first translates its own wire
//! format. This is that one composition point, at the I/O boundary. The only
//! per-adapter inputs are the harness name and the values it already parsed
//! (command/cwd or the normalized call) — the recording logic itself is not
//! repeated. `check`/`explain` call the engine directly and intentionally do not
//! record: only real harness traffic is usage history.

use crate::config::LoadedConfig;
use crate::domain::{self, DecisionResult, ToolCall};
use crate::io::history::{self, Subject};
use crate::io::plugins;

/// Evaluate a shell command against the loaded rules and record the evaluation.
/// `harness` names the calling adapter and `project` is the cwd it ran in;
/// recording resolves that to a repository identity so clones aggregate (see
/// [`crate::io::project`]).
pub(crate) fn evaluate_shell(
    config: &LoadedConfig,
    harness: &str,
    project: &str,
    command: &str,
) -> DecisionResult {
    let result = domain::evaluate(command, &config.rules);
    let result = plugins::evaluate_shell(&config.plugins, harness, project, command, result);
    history::record(
        config.history.enabled,
        harness,
        project,
        Subject::Shell(command),
        &result,
    );
    result
}

/// Evaluate a normalized non-shell tool call against the loaded tool rules and
/// record the evaluation.
pub(crate) fn evaluate_tool(
    config: &LoadedConfig,
    harness: &str,
    project: &str,
    call: &ToolCall,
) -> DecisionResult {
    let result = domain::evaluate_tool_call(call, &config.tool_rules);
    let result = plugins::evaluate_tool(&config.plugins, harness, project, call, result);
    history::record(
        config.history.enabled,
        harness,
        project,
        Subject::Tool(call),
        &result,
    );
    result
}
