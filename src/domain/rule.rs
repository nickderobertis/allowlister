//! Rules and how they match a fragment.
//!
//! A rule matches a fragment by argv (joined-string or per-element pattern)
//! and, optionally, by role. Allow rules additionally carry a redirection
//! policy. Patterns are compiled once when the rule is built; matching is a
//! cheap regex/string test thereafter.

use fancy_regex::Regex;

use super::analyzer::{Fragment, RedirClass, Redirection, Role};
use super::glob::{compile_glob, compile_regex, Matcher};

/// Whether a rule grants or blocks a matching fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Allow,
    Deny,
}

/// How a pattern string is interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    Glob,
    Regex,
    Literal,
}

impl MatchKind {
    /// Parse the `kind` config field; defaults to glob.
    pub fn parse(value: Option<&str>) -> Result<MatchKind, String> {
        match value {
            None | Some("glob") => Ok(MatchKind::Glob),
            Some("regex") => Ok(MatchKind::Regex),
            Some("literal") => Ok(MatchKind::Literal),
            Some(other) => Err(format!("unknown match kind '{other}'")),
        }
    }
}

/// How a rule matches a fragment's argv.
#[derive(Debug, Clone)]
enum ArgvMatcher {
    /// Match against `argv.join(" ")`.
    Joined(Matcher),
    /// Match per element. `tail` is true when a trailing `**` allows any number
    /// of additional trailing arguments (including zero).
    PerElement { head: Vec<Matcher>, tail: bool },
}

/// Allowed redirection targets for an allow rule.
#[derive(Debug, Clone, Default)]
pub struct RedirPolicy {
    /// Forbid every redirection regardless of target.
    deny: bool,
    /// Permitted targets for write redirections; `None` denies all writes.
    write_glob: Option<Vec<Regex>>,
    /// Permitted targets for read redirections; `None` allows any read.
    read_glob: Option<Vec<Regex>>,
}

impl RedirPolicy {
    /// Build a redirection policy from compiled globs.
    pub fn new(deny: bool, write_glob: Option<Vec<Regex>>, read_glob: Option<Vec<Regex>>) -> Self {
        RedirPolicy {
            deny,
            write_glob,
            read_glob,
        }
    }

    /// Build a redirection policy from raw glob strings, compiling each.
    pub fn from_globs(
        deny: bool,
        write: Option<&[String]>,
        read: Option<&[String]>,
    ) -> Result<RedirPolicy, String> {
        let compile = |patterns: &[String]| -> Result<Vec<Regex>, String> {
            patterns
                .iter()
                .map(|p| {
                    compile_glob(p).map_err(|e| format!("invalid redirection glob '{p}': {e}"))
                })
                .collect()
        };
        let write_glob = write.map(compile).transpose()?;
        let read_glob = read.map(compile).transpose()?;
        Ok(RedirPolicy::new(deny, write_glob, read_glob))
    }

    fn allows_write(&self, target: &str) -> bool {
        if self.deny {
            return false;
        }
        match &self.write_glob {
            None => false, // writes are denied unless explicitly permitted.
            Some(globs) => globs.iter().any(|g| g.is_match(target).unwrap_or(false)),
        }
    }

    fn allows_read(&self, target: &str) -> bool {
        if self.deny {
            return false;
        }
        match &self.read_glob {
            None => true, // reads of named files are allowed by default.
            Some(globs) => globs.iter().any(|g| g.is_match(target).unwrap_or(false)),
        }
    }

    /// Whether this policy permits a single redirection.
    pub fn permits(&self, redirection: &Redirection) -> bool {
        match (redirection.class, &redirection.target) {
            (RedirClass::Neutral, _) => !self.deny,
            (RedirClass::Read, None) => !self.deny,
            (RedirClass::Read, Some(target)) => self.allows_read(target),
            (RedirClass::Write, None) => !self.deny,
            (RedirClass::Write, Some(target)) => self.allows_write(target),
        }
    }
}

/// A single allow/deny rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Human-readable identifier shown in decision reasons.
    pub name: String,
    pub action: Action,
    matcher: ArgvMatcher,
    /// Roles this rule applies to; `None` means every role.
    roles: Option<Vec<Role>>,
    pub redirections: RedirPolicy,
    pub description: String,
    /// Config file the rule came from, for diagnostics.
    pub source: String,
}

impl Rule {
    /// Build a rule that matches a joined-argv string pattern.
    // The fields of a rule are inherently many; a params struct would only move
    // the same data behind another type for these two internal constructors.
    #[allow(clippy::too_many_arguments)]
    pub fn from_match(
        name: String,
        action: Action,
        pattern: &str,
        kind: MatchKind,
        roles: Option<Vec<Role>>,
        redirections: RedirPolicy,
        description: String,
        source: String,
    ) -> Result<Rule, String> {
        Ok(Rule {
            name,
            action,
            matcher: ArgvMatcher::Joined(build_matcher(pattern, kind)?),
            roles,
            redirections,
            description,
            source,
        })
    }

    /// Build a rule that matches argv element-by-element.
    #[allow(clippy::too_many_arguments)]
    pub fn from_argv(
        name: String,
        action: Action,
        pattern: &[String],
        kind: MatchKind,
        roles: Option<Vec<Role>>,
        redirections: RedirPolicy,
        description: String,
        source: String,
    ) -> Result<Rule, String> {
        let tail = pattern.last().map(String::as_str) == Some("**");
        let head_slice = if tail {
            &pattern[..pattern.len() - 1]
        } else {
            pattern
        };
        let mut head = Vec::with_capacity(head_slice.len());
        for element in head_slice {
            head.push(build_matcher(element, kind)?);
        }
        Ok(Rule {
            name,
            action,
            matcher: ArgvMatcher::PerElement { head, tail },
            roles,
            redirections,
            description,
            source,
        })
    }

    /// Whether this rule applies to a fragment's role.
    pub fn matches_role(&self, role: Role) -> bool {
        self.roles
            .as_ref()
            .is_none_or(|roles| roles.contains(&role))
    }

    /// Whether this rule's argv pattern matches a fragment's argv.
    pub fn matches_argv(&self, argv: &[String]) -> bool {
        if argv.is_empty() {
            return false;
        }
        match &self.matcher {
            ArgvMatcher::Joined(matcher) => matcher.is_match(&argv.join(" ")),
            ArgvMatcher::PerElement { head, tail } => {
                if *tail {
                    if argv.len() < head.len() {
                        return false;
                    }
                } else if argv.len() != head.len() {
                    return false;
                }
                head.iter().zip(argv).all(|(m, a)| m.is_match(a))
            }
        }
    }

    /// Whether this rule matches the fragment in both argv and role.
    pub fn matches(&self, fragment: &Fragment) -> bool {
        self.matches_role(fragment.role) && self.matches_argv(&fragment.argv)
    }
}

fn build_matcher(pattern: &str, kind: MatchKind) -> Result<Matcher, String> {
    match kind {
        MatchKind::Glob => compile_glob(pattern)
            .map(Matcher::Regex)
            .map_err(|e| format!("invalid glob '{pattern}': {e}")),
        MatchKind::Regex => compile_regex(pattern)
            .map(Matcher::Regex)
            .map_err(|e| format!("invalid regex '{pattern}': {e}")),
        MatchKind::Literal => Ok(Matcher::Literal(pattern.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::analyzer::analyze;

    fn fragment(source: &str) -> Fragment {
        analyze(source).fragments.into_iter().next().unwrap()
    }

    fn allow_match(pattern: &str) -> Rule {
        Rule::from_match(
            "test".into(),
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

    #[test]
    fn joined_glob_matches() {
        let rule = allow_match("git @(status|diff)*");
        assert!(rule.matches(&fragment("git status")));
        assert!(!rule.matches(&fragment("git push")));
    }

    #[test]
    fn argv_tail_double_star() {
        let rule = Rule::from_argv(
            "gh".into(),
            Action::Allow,
            &["gh".into(), "@(pr|issue)".into(), "**".into()],
            MatchKind::Glob,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap();
        assert!(rule.matches(&fragment("gh pr list")));
        assert!(rule.matches(&fragment("gh issue view 1 --json title")));
        assert!(!rule.matches(&fragment("gh repo clone")));
    }

    #[test]
    fn argv_without_tail_requires_exact_length() {
        let rule = Rule::from_argv(
            "exact".into(),
            Action::Allow,
            &["gh".into(), "api".into(), "repos/myorg/**".into()],
            MatchKind::Glob,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap();
        assert!(rule.matches(&fragment("gh api repos/myorg/foo/pulls")));
        assert!(!rule.matches(&fragment("gh api repos/other/foo")));
        assert!(!rule.matches(&fragment("gh api repos/myorg/foo extra")));
    }

    #[test]
    fn role_restriction() {
        let rule = Rule::from_match(
            "filter".into(),
            Action::Allow,
            "head *",
            MatchKind::Glob,
            Some(vec![Role::PipeFilter]),
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap();
        // pipe_filter matches, standalone does not.
        let piped = analyze("cat x | head -5").fragments.pop().unwrap();
        assert!(rule.matches(&piped));
        assert!(!rule.matches(&fragment("head /etc/passwd")));
    }

    #[test]
    fn write_policy_defaults_to_deny() {
        let policy = RedirPolicy::default();
        let redir = analyze("echo hi > /tmp/x")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(!policy.permits(&redir));
    }

    #[test]
    fn write_policy_with_glob() {
        let policy = RedirPolicy::new(false, Some(vec![compile_glob("/tmp/*").unwrap()]), None);
        let redir = analyze("echo hi > /tmp/x")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(policy.permits(&redir));
        let bad = analyze("echo hi > /etc/passwd")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(!policy.permits(&bad));
    }

    #[test]
    fn regex_and_literal_kinds() {
        let regex_rule = Rule::from_match(
            "re".into(),
            Action::Allow,
            "git (status|log)",
            MatchKind::Regex,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap();
        assert!(regex_rule.matches(&fragment("git status")));
        assert!(!regex_rule.matches(&fragment("git push")));

        let literal_rule = Rule::from_match(
            "lit".into(),
            Action::Allow,
            "git status",
            MatchKind::Literal,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap();
        assert!(literal_rule.matches(&fragment("git status")));
        assert!(!literal_rule.matches(&fragment("git status -s")));
    }

    #[test]
    fn from_globs_read_policy_restricts_reads() {
        let policy =
            RedirPolicy::from_globs(false, None, Some(&["/etc/hosts".to_string()])).unwrap();
        let allowed = analyze("cat < /etc/hosts")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(policy.permits(&allowed));
        let denied = analyze("cat < /etc/passwd")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(!policy.permits(&denied));
    }

    #[test]
    fn deny_policy_blocks_every_redirection() {
        let policy = RedirPolicy::from_globs(true, Some(&["/tmp/*".to_string()]), None).unwrap();
        let write = analyze("echo x > /tmp/ok")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(!policy.permits(&write));
    }

    #[test]
    fn match_kind_parse_rejects_unknown() {
        assert!(MatchKind::parse(Some("nope")).is_err());
        assert_eq!(MatchKind::parse(None).unwrap(), MatchKind::Glob);
    }
}
