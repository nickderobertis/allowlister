//! Evaluate fragments against rules and compose a single verdict.
//!
//! Invariants:
//! - **Deny is supreme.** Any matching deny rule denies the fragment,
//!   regardless of how many allow rules also match.
//! - **Allow is any-match.** The first matching allow rule (whose redirection
//!   policy permits the fragment's redirections) allows it.
//! - **Defer means "undecided."** It is never a synonym for allow or deny; the
//!   harness's own permission flow takes over.
//! - Rule order never changes the verdict, only which rule is cited.

use super::analyzer::{Analysis, Fragment};
use super::rule::{Action, Rule};

/// The outcome for a command or a single fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    /// Nothing matched — fall through to the harness's normal flow.
    Defer,
    /// Reserved escalation; the engine does not currently emit this.
    Ask,
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

    // All allow → overall allow.
    if decisions.iter().all(|d| d.verdict == Verdict::Allow) {
        let reason = format!("all {} command(s) matched allow rules", decisions.len());
        return result(Verdict::Allow, reason, decisions, analysis);
    }

    // Otherwise some fragment is undecided → defer the whole command.
    let deferred = decisions
        .iter()
        .find(|d| d.verdict == Verdict::Defer)
        .expect("a non-allow, non-deny decision must be a defer");
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
    let mut matched_allows: Vec<&Rule> = Vec::new();

    for rule in rules {
        if !rule.matches(fragment) {
            continue;
        }
        match rule.action {
            Action::Deny => {
                return FragmentDecision {
                    fragment: fragment.clone(),
                    verdict: Verdict::Deny,
                    rule_name: Some(rule.name.clone()),
                    reason: format!("denied by rule '{}'", rule.name),
                };
            }
            Action::Allow => matched_allows.push(rule),
        }
    }

    if matched_allows.is_empty() {
        return FragmentDecision {
            fragment: fragment.clone(),
            verdict: Verdict::Defer,
            rule_name: None,
            reason: "no matching rule".to_string(),
        };
    }

    if fragment.redirections.is_empty() {
        let rule = matched_allows[0];
        return FragmentDecision {
            fragment: fragment.clone(),
            verdict: Verdict::Allow,
            rule_name: Some(rule.name.clone()),
            reason: format!("allowed by '{}'", rule.name),
        };
    }

    // With redirections, accept the first allow rule that permits them all.
    for rule in &matched_allows {
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

    // No allow rule permits the redirections — deny, citing the offender.
    let first = matched_allows[0];
    let offender = fragment
        .redirections
        .iter()
        .find(|redir| !first.redirections.permits(redir))
        .unwrap_or(&fragment.redirections[0]);
    FragmentDecision {
        fragment: fragment.clone(),
        verdict: Verdict::Deny,
        rule_name: Some(first.name.clone()),
        reason: format!(
            "redirection `{}` not permitted by any matching allow rule (checked {})",
            offender.display,
            matched_allows.len()
        ),
    }
}

/// Convenience for callers that have a command string and compiled rules.
pub fn evaluate(command: &str, rules: &[Rule]) -> DecisionResult {
    let analysis = super::analyzer::analyze(command);
    decide(&analysis, rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::analyzer::analyze;
    use crate::domain::rule::{Action, MatchKind, RedirPolicy, Rule};

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
}
