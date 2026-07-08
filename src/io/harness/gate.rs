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

use std::path::Path;

use crate::config::LoadedConfig;
use crate::domain::{self, DecisionResult, ToolCall};
use crate::io::history::{self, Subject};
use crate::io::plugins;
use crate::io::toolpath;

/// Evaluate a shell command against the loaded rules and record the evaluation.
/// `harness` names the calling adapter and `project` is the cwd it ran in;
/// recording resolves that to a repository identity so clones aggregate (see
/// [`crate::io::project`]). `session_id` is the harness's own session identifier
/// (when it sends one); it is passed to plugins but not recorded, so the usage
/// store stays bounded by distinct commands rather than by session count.
pub(crate) fn evaluate_shell(
    config: &LoadedConfig,
    harness: &str,
    project: &str,
    session_id: Option<&str>,
    command: &str,
) -> DecisionResult {
    let result = domain::evaluate(command, &config.rules);
    let result = plugins::evaluate_shell(
        &config.plugins,
        harness,
        project,
        session_id,
        command,
        result,
    );
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
    session_id: Option<&str>,
    call: &ToolCall,
) -> DecisionResult {
    // Scope the call's file path to the working directory *for engine matching
    // only*, so a portable `./**` profile rule matches the same whether the
    // harness sent an absolute or a relative path (see [`toolpath`]). Plugins and
    // history keep the original call — the path exactly as the harness sent it —
    // so scoping is purely an internal matching detail, not a rewrite the rest of
    // the boundary observes.
    let scoped = toolpath::scope_to_base(call, Path::new(project));
    let result = domain::evaluate_tool_call(&scoped, &config.tool_rules);
    let result =
        plugins::evaluate_tool(&config.plugins, harness, project, session_id, call, result);
    history::record(
        config.history.enabled,
        harness,
        project,
        Subject::Tool(call),
        &result,
    );
    result
}
