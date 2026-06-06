//! Rules and how they match a fragment.
//!
//! A rule matches a fragment by argv (joined-string or per-element pattern)
//! and, optionally, by role. Allow rules additionally carry a redirection
//! policy. Patterns are compiled once when the rule is built; matching is a
//! cheap regex/string test thereafter.

use fancy_regex::Regex;
use serde_json::Value;

use super::analyzer::{Fragment, RedirClass, Redirection, Role};
use super::glob::{compile_glob, compile_glob_matcher, compile_regex, Matcher};
use super::toolcall::{Capability, ParamKey, ToolCall};

/// Whether a rule grants, blocks, or surfaces a matching fragment for approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Allow,
    Deny,
    /// Surface the command for human approval. Sits between deny and allow: a
    /// matching `Ask` (absent any deny) out-prioritizes a broad `Allow`, so it
    /// carves a "confirm first" hole in an otherwise-permissive rule without the
    /// hard wall of a deny.
    Ask,
}

/// What an allow rule grants the commands it matches.
///
/// `Command` (the default) authorizes the command itself. `Redirections` grants
/// only this rule's redirection targets to a command **another** rule already
/// authorized — it never authorizes a command on its own. This keeps the engine
/// invariant that a redirection can never be what grants execution permission,
/// while letting a profile widen scratch-write targets (e.g. `/tmp`) for every
/// already-allowed command without repeating the policy on each rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Grant {
    #[default]
    Command,
    Redirections,
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
        // A `..` component lets a write escape the directory its glob pins it to:
        // `/tmp/../etc/x` matches `/tmp/*` yet resolves outside `/tmp/`. The engine
        // does no filesystem I/O (so it cannot resolve symlinks), but it can reject
        // textual parent-directory traversal outright before glob matching.
        if has_parent_traversal(target) {
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
    /// Whether this rule authorizes the command or only its redirections.
    pub grant: Grant,
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
            grant: Grant::Command,
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
            grant: Grant::Command,
            description,
            source,
        })
    }

    /// Set what this rule grants, consuming and returning the rule. Keeps the
    /// `grant` slot out of the already-long constructors; `Grant::Command` is the
    /// default a freshly built rule carries.
    pub fn with_grant(mut self, grant: Grant) -> Self {
        self.grant = grant;
        self
    }

    /// Whether this rule only grants redirection targets and never authorizes the
    /// command itself.
    pub fn is_redirection_only(&self) -> bool {
        self.grant == Grant::Redirections
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

    /// Like [`matches`](Rule::matches), but also treats each file-target **read**
    /// redirection as if its target were a trailing argument.
    ///
    /// A sensitive-path deny rule (`cat ~/.ssh/id_rsa`) is written against argv,
    /// but `cat < ~/.ssh/id_rsa` hides the path in a redirection that an
    /// argv-only match never inspects — so the secret read slips through. Applying
    /// the same pattern to read-redirection targets closes that gap. Only the
    /// guardrail actions (deny and ask) use this; allow matching stays argv-only
    /// so a redirection can never *grant* permission it otherwise would not.
    pub fn matches_including_read_redirections(&self, fragment: &Fragment) -> bool {
        if self.matches(fragment) {
            return true;
        }
        if !self.matches_role(fragment.role) {
            return false;
        }
        fragment.redirections.iter().any(|redir| {
            redir.class == RedirClass::Read
                && redir.target.as_deref().is_some_and(|target| {
                    let mut argv = fragment.argv.clone();
                    argv.push(target.to_string());
                    self.matches_argv(&argv)
                })
        })
    }
}

/// Whether a path contains a `..` component (parent-directory traversal). Split
/// on both separators so a redirection target is judged the same on any platform.
fn has_parent_traversal(path: &str) -> bool {
    path.split(['/', '\\']).any(|component| component == "..")
}

fn build_matcher(pattern: &str, kind: MatchKind) -> Result<Matcher, String> {
    match kind {
        MatchKind::Glob => {
            compile_glob_matcher(pattern).map_err(|e| format!("invalid glob '{pattern}': {e}"))
        }
        MatchKind::Regex => compile_regex(pattern)
            .map(Matcher::Regex)
            .map_err(|e| format!("invalid regex '{pattern}': {e}")),
        MatchKind::Literal => Ok(Matcher::Literal(pattern.to_string())),
    }
}

/// Which tool a [`ToolRule`] selects: a canonical capability, or a glob over the
/// raw harness tool name (the escape hatch for MCP tools, e.g. `mcp__github__*`,
/// and any tool not yet given a canonical capability).
#[derive(Debug, Clone)]
enum ToolSelector {
    Capability(Capability),
    RawName(Matcher),
}

/// Where a [`ParamConstraint`] reads its value(s) from a [`ToolCall`].
#[derive(Debug, Clone)]
enum ParamSelector {
    /// A canonical scalar parameter the adapter normalized.
    Canonical(ParamKey),
    /// A path into the raw tool-input JSON, for server-defined parameters.
    JsonPath(Vec<PathSeg>),
}

/// One step of a JSON path: an object key or an array index.
#[derive(Debug, Clone)]
enum PathSeg {
    Key(String),
    Index(usize),
}

/// One AND-ed parameter constraint on a tool rule.
#[derive(Debug, Clone)]
struct ParamConstraint {
    selector: ParamSelector,
    /// Any-of globs (mirrors [`RedirPolicy`]'s `globs.iter().any`).
    globs: Vec<Matcher>,
    /// Reject a value containing `..` before glob matching (path params only).
    reject_traversal: bool,
}

/// A rule that matches a normalized [`ToolCall`] — the non-shell sibling of
/// [`Rule`]. It reuses the same [`Action`], the same `Matcher` engine, and
/// (via [`decision`](super::decision)) the same deny-supreme / first-allow /
/// else-defer composition. The structural shell [`Rule`] is left untouched, so a
/// tool call and a bash command never cross paths.
#[derive(Debug, Clone)]
pub struct ToolRule {
    pub name: String,
    pub action: Action,
    selector: ToolSelector,
    /// AND-ed parameter constraints; an empty list matches the capability/name
    /// regardless of parameters (a capability-only rule).
    params: Vec<ParamConstraint>,
    pub description: String,
    pub source: String,
}

impl ToolRule {
    /// Compile a tool rule from its config pieces. `tool` is a capability word
    /// (`read`/`write`/…/`mcp`) or, failing that, a glob over the raw tool name.
    /// Canonical `params` and raw `jsonpath` constraints both compile their globs
    /// with the rule's `kind`.
    #[allow(clippy::too_many_arguments)]
    pub fn compile(
        name: String,
        action: Action,
        tool: &str,
        kind: MatchKind,
        params: &[(ParamKey, Vec<String>)],
        jsonpath: &[(String, Vec<String>)],
        description: String,
        source: String,
    ) -> Result<ToolRule, String> {
        let selector = match Capability::parse(tool) {
            Some(cap) => ToolSelector::Capability(cap),
            None => ToolSelector::RawName(build_matcher(tool, kind)?),
        };

        let mut constraints = Vec::with_capacity(params.len() + jsonpath.len());
        for (key, globs) in params {
            constraints.push(ParamConstraint {
                selector: ParamSelector::Canonical(*key),
                globs: compile_matchers(globs, kind)?,
                reject_traversal: key.is_path_like(),
            });
        }
        for (path, globs) in jsonpath {
            let segs = parse_json_path(path);
            if segs.is_empty() {
                return Err(format!("invalid jsonpath '{path}': empty path"));
            }
            constraints.push(ParamConstraint {
                selector: ParamSelector::JsonPath(segs),
                globs: compile_matchers(globs, kind)?,
                reject_traversal: false,
            });
        }

        Ok(ToolRule {
            name,
            action,
            selector,
            params: constraints,
            description,
            source,
        })
    }

    /// Whether this rule matches a tool call: the selector matches **and** every
    /// parameter constraint holds.
    pub fn matches(&self, call: &ToolCall) -> bool {
        let selected = match &self.selector {
            ToolSelector::Capability(cap) => call.capability == *cap,
            ToolSelector::RawName(matcher) => matcher.is_match(&call.tool_name),
        };
        selected && self.params.iter().all(|c| c.matches(call, self.action))
    }
}

impl ParamConstraint {
    /// Whether this constraint is satisfied. The allow/deny asymmetry mirrors the
    /// shell engine: an **allow** requires *every* resolved value to be permitted
    /// (and a `..` escape is never permitted), while a **deny** fires if *any*
    /// value is dangerous. A constraint whose parameter is absent never matches —
    /// absence is undecided, so it defers rather than allowing or denying.
    fn matches(&self, call: &ToolCall, action: Action) -> bool {
        let values = self.resolve(call);
        if values.is_empty() {
            return false;
        }
        let hit = |value: &str| self.globs.iter().any(|g| g.is_match(value));
        match action {
            Action::Allow => values
                .iter()
                .all(|v| !(self.reject_traversal && has_parent_traversal(v)) && hit(v)),
            // Ask is a guardrail like deny: it fires if *any* value is dangerous,
            // so a "confirm first" constraint cannot be sidestepped by burying a
            // matching value among innocuous ones.
            Action::Deny | Action::Ask => values.iter().any(|v| hit(v)),
        }
    }

    /// The string value(s) this constraint should test. A canonical parameter is
    /// at most one value; a JSON path may resolve to several (an array yields one
    /// per element).
    fn resolve(&self, call: &ToolCall) -> Vec<String> {
        match &self.selector {
            ParamSelector::Canonical(key) => call
                .params
                .get(*key)
                .map(str::to_string)
                .into_iter()
                .collect(),
            ParamSelector::JsonPath(path) => resolve_json_path(&call.raw, path),
        }
    }
}

/// Compile a list of glob patterns into matchers under one match kind.
fn compile_matchers(patterns: &[String], kind: MatchKind) -> Result<Vec<Matcher>, String> {
    patterns.iter().map(|p| build_matcher(p, kind)).collect()
}

/// Parse a dotted JSON path with optional `[index]` array steps, e.g.
/// `args.files[0].path` → `[Key("args"), Key("files"), Index(0), Key("path")]`.
fn parse_json_path(path: &str) -> Vec<PathSeg> {
    let mut segs = Vec::new();
    for part in path.split('.') {
        let (name, rest) = match part.find('[') {
            Some(open) => (&part[..open], &part[open..]),
            None => (part, ""),
        };
        if !name.is_empty() {
            segs.push(PathSeg::Key(name.to_string()));
        }
        let mut chars = rest;
        while let Some(open) = chars.find('[') {
            let Some(close_rel) = chars[open..].find(']') else {
                break;
            };
            let close = open + close_rel;
            if let Ok(n) = chars[open + 1..close].parse::<usize>() {
                segs.push(PathSeg::Index(n));
            }
            chars = &chars[close + 1..];
        }
    }
    segs
}

/// Resolve a JSON path against a value, returning the string form of the
/// leaf(s). A string/number/bool stringifies; an array yields one string per
/// element; an object leaf or an unresolved path yields nothing.
fn resolve_json_path(root: &Value, path: &[PathSeg]) -> Vec<String> {
    let mut current = root;
    for seg in path {
        match seg {
            PathSeg::Key(key) => match current.get(key.as_str()) {
                Some(next) => current = next,
                None => return Vec::new(),
            },
            PathSeg::Index(idx) => match current.get(*idx) {
                Some(next) => current = next,
                None => return Vec::new(),
            },
        }
    }
    value_to_strings(current)
}

fn value_to_strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Number(n) => vec![n.to_string()],
        Value::Bool(b) => vec![b.to_string()],
        Value::Array(items) => items.iter().flat_map(value_to_strings).collect(),
        // An object leaf has no scalar value (path deeper); null is absent.
        Value::Object(_) | Value::Null => Vec::new(),
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

    #[test]
    fn read_redirection_target_is_matched_for_deny() {
        // A deny rule written against argv (`cat ~/.ssh/id_rsa`) must also fire on
        // the redirection form (`cat < ~/.ssh/id_rsa`), where the secret path lives
        // in a redirection rather than argv.
        let rule = Rule::from_match(
            "secret".into(),
            Action::Deny,
            "@(cat|base64) *@(id_rsa|*/.ssh/*)*",
            MatchKind::Glob,
            None,
            RedirPolicy::default(),
            String::new(),
            String::new(),
        )
        .unwrap();

        let redirected = fragment("cat < ~/.ssh/id_rsa");
        assert!(
            !rule.matches(&redirected),
            "argv alone does not name the path"
        );
        assert!(rule.matches_including_read_redirections(&redirected));

        // A write redirection is not a read and must not be folded into argv here.
        let written = fragment("cat foo > ~/.ssh/id_rsa");
        assert!(!rule.matches_including_read_redirections(&written));

        // An innocent read target still does not match.
        let benign = fragment("cat < /etc/hosts");
        assert!(!rule.matches_including_read_redirections(&benign));
    }

    #[test]
    fn parent_traversal_write_target_is_rejected() {
        let policy = RedirPolicy::new(false, Some(vec![compile_glob("/tmp/**").unwrap()]), None);
        let ok = analyze("echo x > /tmp/scratch")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(policy.permits(&ok));
        // `/tmp/../escape` glob-matches `/tmp/**` but escapes the scratch dir.
        let escape = analyze("echo x > /tmp/../escape")
            .fragments
            .pop()
            .unwrap()
            .redirections
            .pop()
            .unwrap();
        assert!(!policy.permits(&escape));
    }

    #[test]
    fn has_parent_traversal_detects_dotdot_components_only() {
        assert!(has_parent_traversal("/tmp/../x"));
        assert!(has_parent_traversal(".."));
        assert!(has_parent_traversal("a/b/../c"));
        assert!(has_parent_traversal("a\\..\\b"));
        // A `..` that is only part of a name is not a traversal component.
        assert!(!has_parent_traversal("/tmp/scratch"));
        assert!(!has_parent_traversal("..foo"));
        assert!(!has_parent_traversal("foo..bar"));
    }

    #[test]
    fn json_path_resolves_scalars_arrays_and_misses() {
        let raw = serde_json::json!({
            "a": { "b": ["x", 7, true] },
            "obj": { "k": "v" },
            "n": 3
        });
        let at = |p: &str| resolve_json_path(&raw, &parse_json_path(p));
        assert_eq!(at("a.b[0]"), vec!["x".to_string()]);
        // Numbers and bools stringify so they can be globbed.
        assert_eq!(at("a.b[1]"), vec!["7".to_string()]);
        assert_eq!(at("a.b[2]"), vec!["true".to_string()]);
        // A whole array yields one string per element.
        assert_eq!(
            at("a.b"),
            vec!["x".to_string(), "7".to_string(), "true".to_string()]
        );
        assert_eq!(at("n"), vec!["3".to_string()]);
        // Object leaf, missing key, out-of-range index, and indexing a non-array
        // all resolve to nothing (the constraint then fails rather than panics).
        assert!(at("obj").is_empty());
        assert!(at("missing").is_empty());
        assert!(at("a.b[9]").is_empty());
        assert!(at("n[0]").is_empty());
    }

    #[test]
    fn parse_json_path_tolerates_garbage_without_panic() {
        // Dotted keys plus chained indices.
        assert_eq!(parse_json_path("a.b[0][1].c").len(), 5);
        // A non-numeric index contributes no Index step; an unterminated bracket
        // simply stops — neither panics.
        assert_eq!(parse_json_path("a[x]").len(), 1);
        assert_eq!(parse_json_path("a[0").len(), 1);
    }

    #[test]
    fn tool_rule_rejects_empty_jsonpath_key() {
        let result = ToolRule::compile(
            "r".into(),
            Action::Allow,
            "mcp",
            MatchKind::Glob,
            &[],
            &[(String::new(), vec!["x".into()])],
            String::new(),
            String::new(),
        );
        assert!(result.is_err());
    }
}
