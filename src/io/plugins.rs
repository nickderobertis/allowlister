//! External dynamic approval plugins.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::PluginConfig;
use crate::domain::{DecisionResult, Verdict};

#[derive(Debug, Serialize)]
struct PluginRequest<'a> {
    protocol_version: u8,
    subject: &'a str,
    harness: &'a str,
    cwd: &'a str,
    command: &'a str,
    current_verdict: &'a str,
    current_reason: &'a str,
}

#[derive(Debug, Deserialize)]
struct PluginResponse {
    verdict: String,
    #[serde(default)]
    reason: String,
}

pub(crate) fn evaluate_shell(
    plugins: &[PluginConfig],
    harness: &str,
    cwd: &str,
    command: &str,
    mut base: DecisionResult,
) -> DecisionResult {
    if plugins.is_empty() || base.verdict == Verdict::Deny {
        return base;
    }

    let mut saw_allow: Option<String> = None;
    let mut saw_ask: Option<String> = None;
    for plugin in plugins {
        match run_plugin(plugin, harness, cwd, command, &base) {
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

fn run_plugin(
    plugin: &PluginConfig,
    harness: &str,
    cwd: &str,
    command: &str,
    current: &DecisionResult,
) -> Result<Option<(Verdict, String)>, String> {
    let request = PluginRequest {
        protocol_version: 1,
        subject: "shell",
        harness,
        cwd,
        command,
        current_verdict: current.verdict.as_str(),
        current_reason: &current.reason,
    };
    let body = serde_json::to_vec(&request).map_err(|err| err.to_string())?;
    let mut child = Command::new(&plugin.command[0])
        .args(&plugin.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&body)
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
