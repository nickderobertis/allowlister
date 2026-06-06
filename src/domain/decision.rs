//! Evaluate fragments against rules and compose a single verdict.
//!
//! Invariants:
//! - **Deny is supreme.** Any matching deny rule denies the fragment,
//!   regardless of how many allow or ask rules also match.
//! - **Ask outranks allow.** Absent any deny, a matching ask rule surfaces the
//!   fragment for approval even when an allow rule also matches. This lets an
//!   ask rule carve a "confirm first" hole in a broad allow without the hard
//!   wall of a deny — the precedence is deny > ask > allow.
//! - **Allow is any-match.** The first matching allow rule (whose redirection
//!   policy permits the fragment's redirections) allows it. A redirection-only
//!   allow rule contributes redirection targets but never authorizes a command,
//!   so it can only widen targets for a command another rule already permitted.
//! - **Defer means "undecided."** It is never a synonym for allow, deny, or ask;
//!   the harness's own permission flow takes over.
//! - Rule order never changes the verdict, only which rule is cited.

use super::analyzer::{Analysis, Fragment};
use super::rule::{Action, Rule, ToolRule};
use super::toolcall::ToolCall;

/// The outcome for a command or a single fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    /// Surface the command for the harness's approval prompt. Emitted by a
    /// matching `ask` rule that outranks any allow but yields to a deny.
    Ask,
    /// Nothing matched — fall through to the harness's normal flow.
    Defer,
}

impl Verdict {
    /// Lowercase wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Allow => "allow",
            Verdict::Deny => "deny",
            Verdict::Defer => "defer",
            Verdict::Ask => "ask",
        }
    }
}

/// The decision for one fragment, with the rule responsible (if any).
#[derive(Clone, Debug)]
pub struct FragmentDecision {
    pub fragment: Fragment,
    pub verdict: Verdict,
    pub rule_name: Option<String>,
    pub reason: String,
}

/// The overall decision plus per-fragment detail for `explain`.
#[derive(Clone, Debug)]
pub struct DecisionResult {
    pub verdict: Verdict,
    pub reason: String,
    pub fragments: Vec<FragmentDecision>,
    pub warnings: Vec<String>,
    pub unsupported: Vec<String>,
}

/// Apply `rules` to every fragment in `analysis` and compose a verdict.
pub fn decide(analysis: &Analysis, rules: &[Rule]) -> DecisionResult {
    // Unsupported constructs (function definitions, etc.) downgrade to defer
    // with a visible reason rather than guessing.
    if !analysis.unsupported.is_empty() {
        return DecisionResult {
            verdict: Verdict::Defer,
            reason: format!("unsupported construct: {}", analysis.unsupported.join("; ")),
            fragments: Vec::new(),
            warnings: analysis.warnings.clone(),
            unsupported: analysis.unsupported.clone(),
        };
    }

    if analysis.fragments.is_empty() {
        let (verdict, reason) = if analysis.warnings.is_empty() {
            (Verdict::Allow, "empty command".to_string())
        } else {
            (
                Verdict::Defer,
                "could not parse command; see warnings".to_string(),
            )
        };
        return DecisionResult {
            verdict,
            reason,
            fragments: Vec::new(),
            warnings: analysis.warnings.clone(),
            unsupported: Vec::new(),
        };
    }

    let decisions: Vec<FragmentDecision> = analysis
        .fragments
        .iter()
        .map(|fragment| decide_fragment(fragment, rules))
        .collect();

    // Any deny → overall deny (report the first denied fragment).
    if let Some(denied) = decisions.iter().find(|d| d.verdict == Verdict::Deny) {
        let reason = format!(
            "`{}` ({}): {}",
            denied.fragment.cmd_string(),
            denied.fragment.role.as_str(),
            denied.reason
        );
        return result(Verdict::Deny, reason, decisions, analysis);
    }

    // No deny: any ask → overall ask (report the first asked fragment). Ask
    // outranks allow, so one fragment needing confirmation holds the whole
    // command for approval.
    if let Some(asked) = decisions.iter().find(|d| d.verdict == Verdict::Ask) {
        let reason = format!(
            "`{}` ({}): {}",
            asked.fragment.cmd_string(),
            asked.fragment.role.as_str(),
            asked.reason
        );
        return result(Verdict::Ask, reason, decisions, analysis);
    }

    // All allow → overall allow.
    if decisions.iter().all(|d| d.verdict == Verdict::Allow) {
        let reason = format!("all {} command(s) matched allow rules", decisions.len());
        return result(Verdict::Allow, reason, decisions, analysis);
    }

    // Otherwise some fragment is undecided → defer the whole command.
    let deferred = decisions
        .iter()
        .find(|d| d.verdict == Verdict::Defer)
        .expect("a non-allow, non-deny, non-ask decision must be a defer");
    let reason = format!(
        "no rule matched `{}` ({})",
        deferred.fragment.cmd_string(),
        deferred.fragment.role.as_str()
    );
    result(Verdict::Defer, reason, decisions, analysis)
}

fn result(
    verdict: Verdict,
    reason: String,
    fragments: Vec<FragmentDecision>,
    analysis: &Analysis,
) -> DecisionResult {
    DecisionResult {
        verdict,
        reason,
        fragments,
        warnings: analysis.warnings.clone(),
        unsupported: Vec::new(),
    }
}

fn decide_fragment(fragment: &Fragment, rules: &[Rule]) -> FragmentDecision {
    // Allow rules split by what they grant: `authorizers` permit the command
    // itself; `redirection_grants` only widen redirection targets for a command
    // some authorizer already permitted. A redirection-only rule never appears in
    // `authorizers`, so it can never be what grants execution permission.
    let mut authorizers: Vec<&Rule> = Vec::new();
    let mut redirection_grants: Vec<&Rule> = Vec::new();
    // The first matching ask rule, held until the loop finishes: a deny found
    // later must still win, so ask cannot short-circuit the way deny does.
    let mut ask: Option<&Rule> = None;

    for rule in rules {
        match rule.action {
            // Deny rules also see read-redirection targets, so `cat < secret` is
            // denied exactly like `cat secret`. Allow rules match argv only — a
            // redirection must never be what grants permission.
            Action::Deny => {
                if rule.matches_including_read_redirections(fragment) {
                    return FragmentDecision {
                        fragment: fragment.clone(),
                        verdict: Verdict::Deny,
                        rule_name: Some(rule.name.clone()),
                        reason: format!("denied by rule '{}'", rule.name),
                    };
                }
            }
            // Ask is a guardrail like deny (it sees read-redirection targets too),
            // but it never short-circuits: keep scanning for a deny that outranks
            // it. Record only the first match — rule order picks who is cited.
            Action::Ask => {
                if ask.is_none() && rule.matches_including_read_redirections(fragment) {
                    ask = Some(rule);
                }
            }
            Action::Allow => {
                if rule.matches(fragment) {
                    if rule.is_redirection_only() {
                        redirection_grants.push(rule);
                    } else {
                        authorizers.push(rule);
                    }
                }
            }
        }
    }

    // No deny fired. An ask outranks any allow, so surface for approval before
    // considering authorizers or redirection grants.
    if let Some(rule) = ask {
        return FragmentDecision {
            fragment: fragment.clone(),
            verdict: Verdict::Ask,
            rule_name: Some(rule.name.clone()),
            reason: format!("needs approval per rule '{}'", rule.name),
        };
    }

    // No rule authorizes the command — a redirection-only match is not enough.
    if authorizers.is_empty() {
        return FragmentDecision {
            fragment: fragment.clone(),
            verdict: Verdict::Defer,
            rule_name: None,
            reason: "no matching rule".to_string(),
        };
    }

    if fragment.redirections.is_empty() {
        let rule = authorizers[0];
        return FragmentDecision {
            fragment: fragment.clone(),
            verdict: Verdict::Allow,
            rule_name: Some(rule.name.clone()),
            reason: format!("allowed by '{}'", rule.name),
        };
    }

    // With redirections, accept the first rule — authorizer or redirection-only
    // grant — that permits them all. A redirection-only grant only widens targets
    // for the command an authorizer already permitted above.
    let first = authorizers[0];
    let mut candidates = authorizers;
    candidates.extend(redirection_grants);
    for rule in &candidates {
        if fragment
            .redirections
            .iter()
            .all(|redir| rule.redirections.permits(redir))
        {
            return FragmentDecision {
                fragment: fragment.clone(),
                verdict: Verdict::Allow,
                rule_name: Some(rule.name.clone()),
                reason: format!("allowed by '{}' (including redirections)", rule.name),
            };
        }
    }

    // No rule permits every redirection — deny, citing a target that no candidate
    // (authorizer or redirection-only grant) permits.
    let offender = fragment
        .redirections
        .iter()
        .find(|redir| {
            !candidates
                .iter()
                .any(|rule| rule.redirections.permits(redir))
        })
        .unwrap_or(&fragment.redirections[0]);
    FragmentDecision {
        fragment: fragment.clone(),
        verdict: Verdict::Deny,
        rule_name: Some(first.name.clone()),
        reason: format!(
            "redirection `{}` not permitted by any matching allow rule (checked {})",
            offender.display,
            candidates.len()
        ),
    }
}

/// Convenience for callers that have a command string and compiled rules.
pub fn evaluate(command: &str, rules: &[Rule]) -> DecisionResult {
    let analysis = super::analyzer::analyze(command);
    decide(&analysis, rules)
}

/// Evaluate a single normalized tool call against the tool rules, composing a
/// verdict the same way the shell engine does: any matching **deny** denies
/// (supreme), else any matching **ask** surfaces for approval, else the first
/// matching **allow** allows, else **defer**. A tool call has no fragments or
/// redirections, so the result carries only a verdict and reason; rule order
/// never changes the verdict, only which rule is cited.
pub fn evaluate_tool_call(call: &ToolCall, rules: &[ToolRule]) -> DecisionResult {
    for rule in rules {
        if rule.action == Action::Deny && rule.matches(call) {
            return tool_result(
                Verdict::Deny,
                format!("tool `{}` denied by rule '{}'", call.tool_name, rule.name),
            );
        }
    }
    for rule in rules {
        if rule.action == Action::Ask && rule.matches(call) {
            return tool_result(
                Verdict::Ask,
                format!(
                    "tool `{}` needs approval per rule '{}'",
                    call.tool_name, rule.name
                ),
            );
        }
    }
    for rule in rules {
        if rule.action == Action::Allow && rule.matches(call) {
            return tool_result(
                Verdict::Allow,
                format!("tool `{}` allowed by rule '{}'", call.tool_name, rule.name),
            );
        }
    }
    tool_result(
        Verdict::Defer,
        format!("no rule matched tool `{}`", call.tool_name),
    )
}

fn tool_result(verdict: Verdict, reason: String) -> DecisionResult {
    DecisionResult {
        verdict,
        reason,
        fragments: Vec::new(),
        warnings: Vec::new(),
        unsupported: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::analyzer::analyze;
    use crate::domain::rule::{Action, Grant, MatchKind, RedirPolicy, Rule};

    fn allow(name: &str, pattern: &str) -> Rule {
        Rule::from_match(
            name.into(),
            Action::Allow,
            pattern,
            MatchKind::Glob,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap()
    }

    fn deny(name: &str, pattern: &str) -> Rule {
        Rule::from_match(
            name.into(),
            Action::Deny,
            pattern,
            MatchKind::Glob,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap()
    }

    fn ask(name: &str, pattern: &str) -> Rule {
        Rule::from_match(
            name.into(),
            Action::Ask,
            pattern,
            MatchKind::Glob,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap()
    }

    #[test]
    fn ask_rule_surfaces_for_approval() {
        let rules = vec![ask("publish", "npm publish*")];
        assert_eq!(evaluate("npm publish", &rules).verdict, Verdict::Ask);
    }

    #[test]
    fn ask_outranks_a_broad_allow_it_is_carved_out_of() {
        // The motivating shape: a broad allow plus an ask that punches a
        // "confirm first" hole in it, with the allow left untouched.
        let rules = vec![
            allow("git push", "git push*"),
            ask("force push", "git push*--force*"),
        ];
        assert_eq!(
            evaluate("git push -u origin feat", &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate("git push --force origin main", &rules).verdict,
            Verdict::Ask
        );
    }

    #[test]
    fn deny_is_supreme_over_ask() {
        // Rule order must not matter: deny wins whether it precedes or follows
        // the ask that also matches.
        let ask_first = vec![ask("a", "rm *"), deny("d", "rm -rf*")];
        let deny_first = vec![deny("d", "rm -rf*"), ask("a", "rm *")];
        assert_eq!(evaluate("rm -rf /", &ask_first).verdict, Verdict::Deny);
        assert_eq!(evaluate("rm -rf /", &deny_first).verdict, Verdict::Deny);
    }

    #[test]
    fn ask_sees_read_redirection_targets_like_deny() {
        // An ask guardrail folds in read-redirection targets, so the hidden-path
        // form trips it just as the argv form does.
        let rules = vec![ask("secretish", "cat *id_rsa*")];
        assert_eq!(evaluate("cat ~/.ssh/id_rsa", &rules).verdict, Verdict::Ask);
        assert_eq!(
            evaluate("cat < ~/.ssh/id_rsa", &rules).verdict,
            Verdict::Ask
        );
    }

    #[test]
    fn one_asked_fragment_holds_the_whole_command() {
        // Across a pipeline, an ask in any fragment surfaces the whole command,
        // but a deny in another fragment still wins.
        let rules = vec![
            allow("git log", "git log*"),
            allow("grep", "grep *"),
            ask("rm", "rm *"),
            deny("nuke", "rm -rf*"),
        ];
        assert_eq!(evaluate("git log | grep x", &rules).verdict, Verdict::Allow);
        assert_eq!(evaluate("git log | rm foo", &rules).verdict, Verdict::Ask);
        assert_eq!(
            evaluate("git log | rm -rf foo", &rules).verdict,
            Verdict::Deny
        );
    }

    /// A redirection-only allow rule: matches `pattern`, grants `writes`, and
    /// never authorizes a command.
    fn redir_grant(name: &str, pattern: &str, writes: &[&str]) -> Rule {
        let globs: Vec<String> = writes.iter().map(|w| w.to_string()).collect();
        Rule::from_match(
            name.into(),
            Action::Allow,
            pattern,
            MatchKind::Glob,
            None,
            RedirPolicy::from_globs(false, Some(&globs), None).unwrap(),
            String::new(),
            String::new(),
        )
        .unwrap()
        .with_grant(Grant::Redirections)
    }

    #[test]
    fn redirection_only_rule_does_not_authorize_a_command() {
        // A redirection-only `**` rule matches argv but must not allow the bare
        // command — only a real authorizer can.
        let rules = vec![redir_grant("scratch", "**", &["/tmp/**"])];
        assert_eq!(evaluate("just dev", &rules).verdict, Verdict::Defer);
    }

    #[test]
    fn redirection_only_rule_widens_targets_for_authorized_command() {
        let rules = vec![
            allow("just", "just *"),
            redir_grant("scratch", "**", &["/tmp/*", "/tmp/**"]),
        ];
        assert_eq!(
            evaluate("just dev > /tmp/dev-server.log", &rules).verdict,
            Verdict::Allow
        );
        // The stderr dup rides along; the file write is the only gated redirection.
        assert_eq!(
            evaluate("just dev > /tmp/dev-server.log 2>&1", &rules).verdict,
            Verdict::Allow
        );
    }

    #[test]
    fn redirection_only_grant_does_not_widen_beyond_its_globs() {
        let rules = vec![
            allow("just", "just *"),
            redir_grant("scratch", "**", &["/tmp/**"]),
        ];
        // `./out.log` is outside the scratch glob and `just` grants no writes.
        assert_eq!(
            evaluate("just dev > ./out.log", &rules).verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn deny_still_beats_a_redirection_only_grant() {
        let rules = vec![
            allow("just", "just *"),
            deny("rm", "rm -rf*"),
            redir_grant("scratch", "**", &["/tmp/**"]),
        ];
        assert_eq!(evaluate("rm -rf / > /tmp/x", &rules).verdict, Verdict::Deny);
    }

    #[test]
    fn redirect_denied_without_a_grant() {
        // Baseline: with no redirection-only rule, an authorized command carrying
        // a file redirect is denied exactly as before.
        let rules = vec![allow("just", "just *")];
        assert_eq!(
            evaluate("just dev > /tmp/dev-server.log", &rules).verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn deny_beats_allow() {
        let rules = vec![allow("a", "rm *"), deny("d", "rm -rf*")];
        assert_eq!(evaluate("rm -rf /", &rules).verdict, Verdict::Deny);
    }

    #[test]
    fn all_allow_is_allow() {
        let rules = vec![allow("git", "git @(status|diff|log)*")];
        assert_eq!(
            evaluate("git status && git log", &rules).verdict,
            Verdict::Allow
        );
    }

    #[test]
    fn unmatched_defers() {
        let rules = vec![allow("git", "git status")];
        assert_eq!(
            evaluate("git status && make", &rules).verdict,
            Verdict::Defer
        );
    }

    #[test]
    fn order_does_not_change_verdict() {
        let forward = vec![allow("a", "rm *"), deny("d", "rm -rf*")];
        let reversed = vec![deny("d", "rm -rf*"), allow("a", "rm *")];
        assert_eq!(
            evaluate("rm -rf /", &forward).verdict,
            evaluate("rm -rf /", &reversed).verdict
        );
    }

    #[test]
    fn unsupported_construct_defers() {
        let rules = vec![deny("d", "rm -rf*")];
        let analysis = analyze("f() { rm -rf /; }; f");
        assert_eq!(decide(&analysis, &rules).verdict, Verdict::Defer);
    }

    #[test]
    fn empty_command_allows() {
        let rules: Vec<Rule> = Vec::new();
        let result = evaluate("", &rules);
        assert_eq!(result.verdict, Verdict::Allow);
        assert!(result.reason.contains("empty"));
    }

    #[test]
    fn unparseable_command_defers() {
        let rules: Vec<Rule> = Vec::new();
        let result = evaluate("for do done (", &rules);
        assert_eq!(result.verdict, Verdict::Defer);
        assert!(result.reason.contains("warnings"));
    }

    #[test]
    fn allowed_command_with_forbidden_redirect_denies() {
        // The default redirection policy denies writes, so an otherwise-allowed
        // command that redirects output is denied.
        let rules = vec![allow("echo", "echo *")];
        assert_eq!(evaluate("echo hi > /tmp/x", &rules).verdict, Verdict::Deny);
    }

    #[test]
    fn secret_read_via_input_redirection_is_denied() {
        // The allow rule grants `cat` and its default policy permits reading any
        // named file, but the deny rule must still fire on the redirected secret.
        let rules = vec![
            allow("read", "cat *"),
            allow("read-bare", "cat"),
            deny("secret", "cat *@(id_rsa|*/.ssh/*)*"),
        ];
        assert_eq!(
            evaluate("cat < ~/.ssh/id_rsa", &rules).verdict,
            Verdict::Deny
        );
        // The plain-argument form was already denied; both paths agree now.
        assert_eq!(evaluate("cat ~/.ssh/id_rsa", &rules).verdict, Verdict::Deny);
    }

    #[test]
    fn allowed_command_with_permitted_redirect_allows() {
        let globs = vec!["/tmp/*".to_string()];
        let rule = Rule::from_match(
            "echo".into(),
            Action::Allow,
            "echo *",
            MatchKind::Glob,
            None,
            RedirPolicy::from_globs(false, Some(globs.as_slice()), None).unwrap(),
            String::new(),
            String::new(),
        )
        .unwrap();
        assert_eq!(
            evaluate("echo hi > /tmp/x", &[rule]).verdict,
            Verdict::Allow
        );
    }
}

#[cfg(test)]
mod tool_tests {
    use super::evaluate_tool_call;
    use crate::domain::rule::{Action, MatchKind, ToolRule};
    use crate::domain::toolcall::{Capability, NormalizedParams, ParamKey, ToolCall};
    use crate::domain::Verdict;
    use serde_json::{json, Value};

    /// Build a tool call with canonical params and a raw object for JSON-path
    /// matching.
    fn call(cap: Capability, name: &str, params: &[(ParamKey, &str)], raw: Value) -> ToolCall {
        let mut np = NormalizedParams::new();
        for (key, value) in params {
            np.insert(*key, (*value).to_string());
        }
        ToolCall::new(cap, name.to_string(), np, raw)
    }

    fn read(path: &str) -> ToolCall {
        call(
            Capability::Read,
            "Read",
            &[(ParamKey::Path, path)],
            json!({ "file_path": path }),
        )
    }

    /// A capability rule with canonical-parameter constraints.
    fn rule(name: &str, action: Action, tool: &str, params: &[(ParamKey, &[&str])]) -> ToolRule {
        let params: Vec<(ParamKey, Vec<String>)> = params
            .iter()
            .map(|(k, globs)| (*k, globs.iter().map(|g| g.to_string()).collect()))
            .collect();
        ToolRule::compile(
            name.into(),
            action,
            tool,
            MatchKind::Glob,
            &params,
            &[],
            String::new(),
            String::new(),
        )
        .unwrap()
    }

    /// A rule with raw JSON-path constraints (for MCP / server-defined params).
    fn rule_jsonpath(
        name: &str,
        action: Action,
        tool: &str,
        jsonpath: &[(&str, &[&str])],
    ) -> ToolRule {
        let jsonpath: Vec<(String, Vec<String>)> = jsonpath
            .iter()
            .map(|(k, globs)| (k.to_string(), globs.iter().map(|g| g.to_string()).collect()))
            .collect();
        ToolRule::compile(
            name.into(),
            action,
            tool,
            MatchKind::Glob,
            &[],
            &jsonpath,
            String::new(),
            String::new(),
        )
        .unwrap()
    }

    #[test]
    fn ask_outranks_allow_but_yields_to_deny_for_tool_calls() {
        // deny > ask > allow holds on the tool engine too: a broad allow, an ask
        // that carves a subset out for approval, and a deny that still wins.
        let rules = vec![
            rule("repo", Action::Allow, "read", &[(ParamKey::Path, &["**"])]),
            rule(
                "confirm-env",
                Action::Ask,
                "read",
                &[(ParamKey::Path, &["**/.env*"])],
            ),
            rule(
                "secrets",
                Action::Deny,
                "read",
                &[(ParamKey::Path, &["**/.ssh/**"])],
            ),
        ];
        assert_eq!(
            evaluate_tool_call(&read("/repo/a.ts"), &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate_tool_call(&read("/repo/.env"), &rules).verdict,
            Verdict::Ask
        );
        assert_eq!(
            evaluate_tool_call(&read("/repo/.ssh/id_rsa"), &rules).verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn read_allow_deny_defer_by_path() {
        let rules = vec![
            rule(
                "repo",
                Action::Allow,
                "read",
                &[(ParamKey::Path, &["/repo/**"])],
            ),
            rule(
                "secrets",
                Action::Deny,
                "read",
                &[(ParamKey::Path, &["**/.ssh/**", "**/*.pem"])],
            ),
        ];
        assert_eq!(
            evaluate_tool_call(&read("/repo/a.ts"), &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate_tool_call(&read("/home/u/.ssh/id_rsa"), &rules).verdict,
            Verdict::Deny
        );
        assert_eq!(
            evaluate_tool_call(&read("/etc/hosts"), &rules).verdict,
            Verdict::Defer
        );
        assert_eq!(
            evaluate_tool_call(&read("/repo/key.pem"), &rules).verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn deny_is_supreme_and_order_independent() {
        let allow = rule(
            "repo",
            Action::Allow,
            "read",
            &[(ParamKey::Path, &["/repo/**"])],
        );
        let deny = rule(
            "secret",
            Action::Deny,
            "read",
            &[(ParamKey::Path, &["**/secret*"])],
        );
        let forward = vec![allow.clone(), deny.clone()];
        let reversed = vec![deny, allow];
        assert_eq!(
            evaluate_tool_call(&read("/repo/secret.txt"), &forward).verdict,
            Verdict::Deny
        );
        assert_eq!(
            evaluate_tool_call(&read("/repo/secret.txt"), &reversed).verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn traversal_cannot_widen_an_allow_but_deny_sees_through() {
        let rules = vec![
            rule(
                "repo",
                Action::Allow,
                "read",
                &[(ParamKey::Path, &["/repo/**"])],
            ),
            rule(
                "ssh",
                Action::Deny,
                "read",
                &[(ParamKey::Path, &["**/.ssh/**"])],
            ),
        ];
        // `/repo/**` would textually match, but `..` is never permitted on allow.
        assert_eq!(
            evaluate_tool_call(&read("/repo/../etc/passwd"), &rules).verdict,
            Verdict::Defer
        );
        // A deny still fires through `..`.
        assert_eq!(
            evaluate_tool_call(&read("/repo/../home/.ssh/id_rsa"), &rules).verdict,
            Verdict::Deny
        );
    }

    #[test]
    fn web_fetch_host_scoping() {
        let rules = vec![rule(
            "github",
            Action::Allow,
            "web_fetch",
            &[(
                ParamKey::Url,
                &["https://github.com/**", "https://*.github.com/**"],
            )],
        )];
        let fetch = |url: &str| {
            call(
                Capability::WebFetch,
                "WebFetch",
                &[(ParamKey::Url, url)],
                json!({ "url": url }),
            )
        };
        assert_eq!(
            evaluate_tool_call(&fetch("https://github.com/foo"), &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate_tool_call(&fetch("https://evil.test/x"), &rules).verdict,
            Verdict::Defer
        );
        assert_eq!(
            evaluate_tool_call(&fetch("https://raw.githubusercontent.com/x"), &rules).verdict,
            Verdict::Defer
        );
    }

    #[test]
    fn capability_only_deny_fires_regardless_of_params() {
        let rules = vec![rule("no-search", Action::Deny, "web_search", &[])];
        let search = call(
            Capability::WebSearch,
            "WebSearch",
            &[(ParamKey::Query, "anything")],
            json!({ "query": "anything" }),
        );
        assert_eq!(evaluate_tool_call(&search, &rules).verdict, Verdict::Deny);
    }

    #[test]
    fn missing_param_does_not_match() {
        let allow = vec![rule(
            "repo",
            Action::Allow,
            "read",
            &[(ParamKey::Path, &["/repo/**"])],
        )];
        let deny = vec![rule(
            "ssh",
            Action::Deny,
            "read",
            &[(ParamKey::Path, &["**/.ssh/**"])],
        )];
        let no_path = call(Capability::Read, "Read", &[], json!({ "offset": 0 }));
        // An allow that names a path it cannot find must not allow.
        assert_eq!(evaluate_tool_call(&no_path, &allow).verdict, Verdict::Defer);
        // A deny that names a path it cannot find must not fire.
        assert_eq!(evaluate_tool_call(&no_path, &deny).verdict, Verdict::Defer);
    }

    #[test]
    fn and_across_params() {
        let rules = vec![rule(
            "todos",
            Action::Allow,
            "grep",
            &[
                (ParamKey::Pattern, &["TODO*"]),
                (ParamKey::Path, &["/repo/**"]),
            ],
        )];
        let grep = |pat: &str, path: &str| {
            call(
                Capability::Grep,
                "Grep",
                &[(ParamKey::Pattern, pat), (ParamKey::Path, path)],
                json!({ "pattern": pat, "path": path }),
            )
        };
        assert_eq!(
            evaluate_tool_call(&grep("TODO", "/repo/x"), &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate_tool_call(&grep("TODO", "/etc"), &rules).verdict,
            Verdict::Defer
        );
        assert_eq!(
            evaluate_tool_call(&grep("FIXME", "/repo/x"), &rules).verdict,
            Verdict::Defer
        );
    }

    #[test]
    fn mcp_raw_name_selector_with_extglob() {
        let rules = vec![rule(
            "linear-ro",
            Action::Allow,
            "mcp__linear__@(list|get)*",
            &[],
        )];
        let mcp = |name: &str| call(Capability::Mcp, name, &[], json!({}));
        assert_eq!(
            evaluate_tool_call(&mcp("mcp__linear__list_issues"), &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate_tool_call(&mcp("mcp__linear__delete_issue"), &rules).verdict,
            Verdict::Defer
        );
    }

    #[test]
    fn mcp_canonical_server_tool_is_portable() {
        let rules = vec![
            rule(
                "linear-ro",
                Action::Allow,
                "mcp",
                &[
                    (ParamKey::McpServer, &["linear"]),
                    (ParamKey::McpTool, &["@(list|get)*"]),
                ],
            ),
            rule(
                "no-destroy",
                Action::Deny,
                "mcp",
                &[(ParamKey::McpTool, &["delete*"])],
            ),
        ];
        let mcp = |server: &str, tool: &str| {
            call(
                Capability::Mcp,
                &format!("mcp__{server}__{tool}"),
                &[(ParamKey::McpServer, server), (ParamKey::McpTool, tool)],
                json!({}),
            )
        };
        assert_eq!(
            evaluate_tool_call(&mcp("linear", "list_issues"), &rules).verdict,
            Verdict::Allow
        );
        assert_eq!(
            evaluate_tool_call(&mcp("linear", "delete_issue"), &rules).verdict,
            Verdict::Deny
        );
        assert_eq!(
            evaluate_tool_call(&mcp("github", "list_repos"), &rules).verdict,
            Verdict::Defer
        );
    }

    #[test]
    fn jsonpath_scalar_and_array_with_allow_all_deny_any() {
        let allow_in_repo = rule_jsonpath(
            "fs-allow",
            Action::Allow,
            "mcp__fs__write",
            &[("paths", &["/repo/**"])],
        );
        let deny_ssh = rule_jsonpath(
            "fs-deny",
            Action::Deny,
            "mcp__fs__write",
            &[("paths", &["**/.ssh/**"])],
        );
        let rules = vec![allow_in_repo, deny_ssh];
        let write = |paths: Value| {
            call(
                Capability::Mcp,
                "mcp__fs__write",
                &[],
                json!({ "paths": paths }),
            )
        };

        // deny=any: one ssh element fires the deny.
        assert_eq!(
            evaluate_tool_call(&write(json!(["/repo/a", "/home/.ssh/b"])), &rules).verdict,
            Verdict::Deny
        );
        // allow=all: every element in repo allows.
        assert_eq!(
            evaluate_tool_call(&write(json!(["/repo/a", "/repo/b"])), &rules).verdict,
            Verdict::Allow
        );
        // allow=all fails when one element is outside repo (and no deny applies).
        assert_eq!(
            evaluate_tool_call(&write(json!(["/repo/a", "/etc/x"])), &rules).verdict,
            Verdict::Defer
        );
    }

    #[test]
    fn jsonpath_scalar_owner_deny() {
        let rules = vec![rule_jsonpath(
            "no-evilcorp",
            Action::Deny,
            "mcp__*",
            &[("owner", &["evilcorp"])],
        )];
        let issue = |owner: &str| {
            call(
                Capability::Mcp,
                "mcp__github__create_issue",
                &[],
                json!({ "owner": owner }),
            )
        };
        assert_eq!(
            evaluate_tool_call(&issue("evilcorp"), &rules).verdict,
            Verdict::Deny
        );
        assert_eq!(
            evaluate_tool_call(&issue("goodcorp"), &rules).verdict,
            Verdict::Defer
        );
    }
}
