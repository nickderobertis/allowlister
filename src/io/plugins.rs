//! External dynamic approval plugins.
//!
//! A plugin runs for both subjects: a shell command (carrying its role-tagged
//! `fragments`) and a non-shell tool call (carrying a `tool` object). The request
//! body is a tagged union discriminated by `subject` — `command`/`fragments` are
//! present only for shell, `tool` only for a tool call — so a plugin keys off
//! `subject` and reads only the fields its subject defines. Composition is
//! identical for both: a static deny is final (plugins are skipped), then any
//! plugin deny blocks, any plugin ask surfaces, and a plugin allow may upgrade
//! only a static defer.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::VariantArray;

use crate::config::PluginConfig;
use crate::domain::{DecisionResult, ParamKey, ToolCall, Verdict};

/// Protocol v2 adds `fragments` (shell) and the `tool` object (tool calls); the
/// v1 fields are unchanged so older plugins that ignore the additions keep
/// working.
const PROTOCOL_VERSION: u8 = 2;

/// The request handed to a plugin on stdin. A tagged union on `subject`: shell
/// requests carry `command` + `fragments`; tool requests carry `tool`. The
/// irrelevant arm is omitted rather than nulled, so the shape matches the
/// subject.
#[derive(Debug, Serialize)]
struct PluginRequest<'a> {
    protocol_version: u8,
    subject: &'a str,
    harness: &'a str,
    cwd: &'a str,
    current_verdict: &'a str,
    current_reason: &'a str,
    /// The shell command line. Present only for `subject: "shell"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a str>,
    /// Every parsed fragment in source order, each with its own verdict — the
    /// structured form of the decomposition that `current_reason` only narrates
    /// for the tripping fragments. Present only for `subject: "shell"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    fragments: Option<Vec<PluginFragment<'a>>>,
    /// The normalized tool call. Present only for `subject: "tool"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<PluginTool<'a>>,
}

/// One role-tagged fragment with its individual decision, mirroring a row of
/// `explain`'s decision table.
#[derive(Debug, Serialize)]
struct PluginFragment<'a> {
    /// The fragment as shown — argv joined by single spaces.
    display: String,
    /// Tokenized argv from the AST, so a plugin need not re-tokenize.
    argv: &'a [String],
    /// Structural role: one of `standalone`, `pipe_source`, `pipe_filter`,
    /// `subshell`, `substitution`.
    role: &'a str,
    /// Per-fragment verdict: `allow`, `ask`, `deny`, or `defer`.
    verdict: &'a str,
    /// Name of the matching rule, or null when no rule matched (a defer).
    rule: Option<&'a str>,
    /// Per-fragment explanation, the text `explain` prints after `<-`.
    reason: &'a str,
}

/// A normalized non-shell tool call, the tool-subject counterpart of
/// `fragments`. Mirrors what the tool-rule engine matches on.
#[derive(Debug, Serialize)]
struct PluginTool<'a> {
    /// The harness's own tool name, e.g. `Read`, `mcp__github__create_issue`.
    name: &'a str,
    /// The portable capability the call maps to (`read`, `write`, `mcp`, …).
    capability: &'a str,
    /// Canonical scalar parameters the adapter mapped (path/url/query/…), keyed
    /// by canonical name. Server-defined parameters live in `raw`.
    params: BTreeMap<&'a str, &'a str>,
    /// The original tool-input object, verbatim, so a plugin can inspect any
    /// server-defined parameter the canonical set does not cover.
    raw: &'a Value,
}

#[derive(Debug, Deserialize)]
struct PluginResponse {
    verdict: String,
    #[serde(default)]
    reason: String,
}

/// Run every plugin against a shell command and compose the result with the
/// static decision.
pub(crate) fn evaluate_shell(
    plugins: &[PluginConfig],
    harness: &str,
    cwd: &str,
    command: &str,
    base: DecisionResult,
) -> DecisionResult {
    if plugins.is_empty() || base.verdict == Verdict::Deny {
        return base;
    }
    // Serialize the request once, before `base` is moved into `compose`: the body
    // is identical for every plugin and borrowing `base` here keeps `compose`
    // free to mutate it.
    let body = {
        let fragments: Vec<PluginFragment> = base
            .fragments
            .iter()
            .map(|decision| PluginFragment {
                display: decision.fragment.cmd_string(),
                argv: &decision.fragment.argv,
                role: decision.fragment.role.as_str(),
                verdict: decision.verdict.as_str(),
                rule: decision.rule_name.as_deref(),
                reason: &decision.reason,
            })
            .collect();
        let request = PluginRequest {
            protocol_version: PROTOCOL_VERSION,
            subject: "shell",
            harness,
            cwd,
            current_verdict: base.verdict.as_str(),
            current_reason: &base.reason,
            command: Some(command),
            fragments: Some(fragments),
            tool: None,
        };
        match serde_json::to_vec(&request) {
            Ok(body) => body,
            // Our own request shape always serializes; treat the impossible error
            // as "no plugin input" and leave the static decision untouched.
            Err(_) => return base,
        }
    };
    compose(plugins, &body, base)
}

/// Run every plugin against a non-shell tool call and compose the result with the
/// static decision, exactly as [`evaluate_shell`] does for shell commands.
pub(crate) fn evaluate_tool(
    plugins: &[PluginConfig],
    harness: &str,
    cwd: &str,
    call: &ToolCall,
    base: DecisionResult,
) -> DecisionResult {
    if plugins.is_empty() || base.verdict == Verdict::Deny {
        return base;
    }
    let body = {
        let mut params: BTreeMap<&str, &str> = BTreeMap::new();
        for key in ParamKey::VARIANTS {
            if let Some(value) = call.params.get(*key) {
                params.insert(key.as_str(), value);
            }
        }
        let request = PluginRequest {
            protocol_version: PROTOCOL_VERSION,
            subject: "tool",
            harness,
            cwd,
            current_verdict: base.verdict.as_str(),
            current_reason: &base.reason,
            command: None,
            fragments: None,
            tool: Some(PluginTool {
                name: &call.tool_name,
                capability: call.capability.as_str(),
                params,
                raw: &call.raw,
            }),
        };
        match serde_json::to_vec(&request) {
            Ok(body) => body,
            Err(_) => return base,
        }
    };
    compose(plugins, &body, base)
}

/// Send the same request body to each plugin and fold the verdicts into `base`.
/// The composition is conservative and subject-independent: deny is final, ask
/// outranks a plugin allow, and a plugin allow lifts only a static defer.
fn compose(plugins: &[PluginConfig], body: &[u8], mut base: DecisionResult) -> DecisionResult {
    let mut saw_allow: Option<String> = None;
    let mut saw_ask: Option<String> = None;
    for plugin in plugins {
        match dispatch(plugin, body) {
            Ok(Some((Verdict::Deny, reason))) => {
                base.verdict = Verdict::Deny;
                base.reason = format!("plugin '{}': {reason}", plugin.name);
                return base;
            }
            Ok(Some((Verdict::Ask, reason))) => {
                if saw_ask.is_none() {
                    saw_ask = Some(format!("plugin '{}': {reason}", plugin.name));
                }
            }
            Ok(Some((Verdict::Allow, reason))) => {
                if saw_allow.is_none() {
                    saw_allow = Some(format!("plugin '{}': {reason}", plugin.name));
                }
            }
            Ok(Some((Verdict::Defer, _))) | Ok(None) => {}
            Err(err) => base
                .warnings
                .push(format!("plugin '{}': {err}", plugin.name)),
        }
    }

    if let Some(reason) = saw_ask {
        base.verdict = Verdict::Ask;
        base.reason = reason;
    } else if base.verdict == Verdict::Defer {
        if let Some(reason) = saw_allow {
            base.verdict = Verdict::Allow;
            base.reason = reason;
        }
    }
    base
}

/// Spawn one plugin, write the request body to its stdin, and read back a single
/// verdict — enforcing the configured timeout. Returns the parsed verdict, or an
/// error describing why this plugin produced no usable decision.
fn dispatch(plugin: &PluginConfig, body: &[u8]) -> Result<Option<(Verdict, String)>, String> {
    let mut child = Command::new(&plugin.command[0])
        .args(&plugin.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body)
            .map_err(|err| format!("failed to write request: {err}"))?;
    }
    let deadline = Instant::now() + Duration::from_millis(plugin.timeout_ms);
    loop {
        match child
            .try_wait()
            .map_err(|err| format!("failed to poll plugin: {err}"))?
        {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}ms", plugin.timeout_ms));
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("failed to read response: {err}"))?;
    if !output.status.success() {
        return Err(format!("exited with {}", output.status));
    }
    let response: PluginResponse = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("invalid JSON response: {err}"))?;
    let verdict = match response.verdict.as_str() {
        "allow" => Verdict::Allow,
        "deny" => Verdict::Deny,
        "ask" => Verdict::Ask,
        "defer" => Verdict::Defer,
        other => return Err(format!("unknown verdict '{other}'")),
    };
    let reason = if response.reason.is_empty() {
        response.verdict
    } else {
        response.reason
    };
    Ok(Some((verdict, reason)))
}
