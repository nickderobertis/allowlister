//! Glob matching with bash `extglob` support.
//!
//! The standard glob crates do not implement extended globbing
//! (`@(a|b)`, `?(a|b)`, `*(a|b)`, `+(a|b)`, `!(a|b)`), which is exactly the
//! syntax that makes allow/deny rules compact and readable. A pattern is
//! translated into an anchored, full-match regular expression — built at most
//! once per process and reused for every fragment evaluated — but for most
//! glob rules construction is deferred until a candidate value passes the
//! rule's literal-prefix gate, which in a spawn-per-call binary usually means
//! never.
//!
//! Translation rules:
//! - `*` matches any run of characters, `?` matches a single character.
//! - `[abc]` / `[!abc]` map to regex character classes.
//! - `@(p|q)` exactly one, `?(p|q)` zero-or-one, `*(p|q)` any number,
//!   `+(p|q)` one-or-more, `!(p|q)` anything not matching the alternatives.
//! - Every other character is matched literally.

// `fancy_regex` (not the `regex` crate) backs the matcher because extglob
// negation `!(a|b)` compiles to a negative look-ahead, which the `regex` crate
// does not support. `regex::escape` is still used to quote literal characters.
use std::sync::OnceLock;

use fancy_regex::Regex;

/// A compiled command matcher. The regex kind is a full-match [`Regex`]; the
/// literal kind keeps the raw string for exact comparison; the lazy-glob kind
/// defers regex construction until a candidate value first passes its
/// literal-prefix gate.
#[derive(Debug, Clone)]
pub(crate) enum Matcher {
    Regex(Regex),
    Literal(String),
    /// A glob whose regex is built on first use. Regex construction is the
    /// dominant per-spawn cost (the binary recompiles every rule on each
    /// invocation) and most rules never see a value that could match them, so
    /// the gate — a value must start with one of the pattern's necessary
    /// literal prefixes — lets almost every rule skip compilation entirely.
    LazyGlob {
        pattern: String,
        /// Necessary literal prefixes (see [`literal_prefixes`]). An empty
        /// string gates nothing, so the gate only ever skips work, never a
        /// match.
        prefixes: Vec<String>,
        compiled: OnceLock<Option<Regex>>,
    },
}

impl Matcher {
    pub(crate) fn is_match(&self, value: &str) -> bool {
        match self {
            // A backtracking error (e.g. catastrophic input) is treated as "no
            // match" rather than propagated: the engine must never panic.
            Matcher::Regex(re) => re.is_match(value).unwrap_or(false),
            Matcher::Literal(lit) => lit == value,
            Matcher::LazyGlob {
                pattern,
                prefixes,
                compiled,
            } => {
                if !prefixes.iter().any(|p| value.starts_with(p.as_str())) {
                    return false;
                }
                // A compile failure (only reachable through pathological
                // patterns, e.g. nesting past the regex engine's limits) is
                // "never matches", mirroring the backtracking-error policy.
                compiled
                    .get_or_init(|| compile_glob(pattern).ok())
                    .as_ref()
                    .is_some_and(|re| re.is_match(value).unwrap_or(false))
            }
        }
    }
}

/// Compile an extglob pattern into a matcher, avoiding regex construction
/// wherever possible — building a regex is the dominant per-invocation cost and
/// the binary recompiles every rule on each spawn.
///
/// - No glob syntax at all: a metacharacter-free glob's anchored full-match is
///   exactly string equality (this dialect has no escapes; every non-meta char
///   matches itself), so the pattern becomes a [`Matcher::Literal`].
/// - A character class (`[`): compiled eagerly. Class content is copied into
///   the regex verbatim, making it the one construct whose translation can fail
///   to compile, and an invalid pattern must surface at config-load time.
/// - Everything else: a [`Matcher::LazyGlob`]. The translation is valid regex
///   by construction, so deferring compilation behind the literal-prefix gate
///   loses no load-time validation.
pub(crate) fn compile_glob_matcher(pattern: &str) -> Result<Matcher, fancy_regex::Error> {
    if !pattern_has_glob_meta(pattern) {
        return Ok(Matcher::Literal(pattern.to_string()));
    }
    if pattern.contains('[') {
        return Ok(Matcher::Regex(compile_glob(pattern)?));
    }
    Ok(Matcher::LazyGlob {
        pattern: pattern.to_string(),
        prefixes: literal_prefixes(pattern),
        compiled: OnceLock::new(),
    })
}

/// Necessary literal prefixes of any value matching `pattern`: every value the
/// compiled (anchored, full-match) pattern accepts starts with at least one of
/// the returned strings. The scan walks literal characters until the first glob
/// construct. An `@(…|…)`/`+(…|…)` group must match one alternative immediately,
/// so it forks the prefix per alternative (recursively); every other construct —
/// `*`, `?`, `[`, an optional or negated group, an unbalanced group — pins down
/// nothing further, so the scan stops with the literal accumulated so far. The
/// result only ever under-promises: a prefix may be shorter than what the
/// pattern truly requires (the empty string gates nothing), never longer.
fn literal_prefixes(pattern: &str) -> Vec<String> {
    // Past this many forks the gate's linear scan stops being clearly cheaper
    // than compiling; fall back to the unforked base prefix.
    const MAX_PREFIXES: usize = 64;
    let chars: Vec<char> = pattern.chars().collect();
    let mut base = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '@' | '?' | '*' | '+' | '!') && i + 1 < chars.len() && chars[i + 1] == '(' {
            if matches!(c, '@' | '+') {
                if let Some((alts, _)) = parse_extglob(&chars, i + 2) {
                    let mut out = Vec::new();
                    for alt in &alts {
                        for sub in literal_prefixes(alt) {
                            out.push(format!("{base}{sub}"));
                        }
                        if out.len() > MAX_PREFIXES {
                            return vec![base];
                        }
                    }
                    return out;
                }
            }
            // `?(…)`/`*(…)` match zero times and `!(…)` matches the empty
            // string, so nothing after `base` is certain. An unbalanced group
            // is matched literally by `translate`, so `base` remains a true
            // (if shortened) prefix there too.
            return vec![base];
        }
        match c {
            '*' | '?' | '[' => return vec![base],
            other => base.push(other),
        }
        i += 1;
    }
    vec![base]
}

/// Whether a pattern contains any glob syntax that `translate` would turn into
/// regex (rather than an escaped literal). Deliberately *over*-approximates: a
/// `[` or extglob opener that does not actually form a valid construct still
/// reports `true`, so the pattern compiles to a regex that `translate` makes
/// behave literally anyway. The reverse — reporting `false` for a real glob —
/// must never happen, as it would silently change matching to equality.
fn pattern_has_glob_meta(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| match b {
        b'*' | b'?' | b'[' => true,
        // An extglob group opens with `@( ?( *( +( !(`; `?(`/`*(` are already
        // caught by the `?`/`*` arms, the rest are caught here.
        b'(' => i > 0 && matches!(bytes[i - 1], b'@' | b'?' | b'*' | b'+' | b'!'),
        _ => false,
    })
}

/// Compile an extglob pattern into a full-match regex.
pub(crate) fn compile_glob(pattern: &str) -> Result<Regex, fancy_regex::Error> {
    Regex::new(&anchored(&translate(pattern)))
}

/// Compile a user-supplied regex with full-match (anchored) semantics.
pub(crate) fn compile_regex(pattern: &str) -> Result<Regex, fancy_regex::Error> {
    Regex::new(&anchored(pattern))
}

fn anchored(body: &str) -> String {
    // `\A`/`\z` anchor to the whole text; `(?s:...)` makes `.` span newlines so
    // a pattern's `*` behaves like a shell glob across the (rare) multi-line word.
    format!(r"\A(?s:{body})\z")
}

/// Translate an extglob pattern into a regex body (no anchors).
fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n * 2);
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if matches!(c, '@' | '?' | '*' | '+' | '!') && i + 1 < n && chars[i + 1] == '(' {
            if let Some((alts, end)) = parse_extglob(&chars, i + 2) {
                let inner = alts
                    .iter()
                    .map(|a| translate(a))
                    .collect::<Vec<_>>()
                    .join("|");
                match c {
                    '@' => out.push_str(&format!("(?:{inner})")),
                    '?' => out.push_str(&format!("(?:{inner})?")),
                    '*' => out.push_str(&format!("(?:{inner})*")),
                    '+' => out.push_str(&format!("(?:{inner})+")),
                    // Negation has no direct regex form; approximate with a
                    // tempered greedy token. Adequate for path/argument rules.
                    '!' => out.push_str(&format!("(?:(?!(?:{inner})).)*")),
                    _ => unreachable!(),
                }
                i = end + 1;
                continue;
            }
            // Unbalanced extglob — treat the operator char literally.
            out.push_str(&regex::escape(&c.to_string()));
            i += 1;
            continue;
        }
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                if let Some(end) = parse_char_class(&chars, i) {
                    let raw: String = chars[i + 1..end].iter().collect();
                    let cls = match raw.strip_prefix('!') {
                        Some(rest) => format!("^{rest}"),
                        None => raw,
                    };
                    out.push('[');
                    out.push_str(&cls);
                    out.push(']');
                    i = end + 1;
                    continue;
                }
                out.push_str(&regex::escape("["));
            }
            other => out.push_str(&regex::escape(&other.to_string())),
        }
        i += 1;
    }
    out
}

/// Parse the alternatives of an extglob group whose `(` is at `start - 1`.
/// `start` points just past the `(`. Returns the alternatives and the index of
/// the closing `)`.
fn parse_extglob(chars: &[char], start: usize) -> Option<(Vec<String>, usize)> {
    let mut depth = 1usize;
    let mut alts = Vec::new();
    let mut chunk_start = start;
    let mut j = start;
    while j < chars.len() {
        match chars[j] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    alts.push(chars[chunk_start..j].iter().collect());
                    return Some((alts, j));
                }
            }
            '|' if depth == 1 => {
                alts.push(chars[chunk_start..j].iter().collect());
                chunk_start = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Find the closing `]` of a character class beginning at `open`. Handles a
/// leading `!`/`^` negation and a literal `]` as the first class member.
fn parse_char_class(chars: &[char], open: usize) -> Option<usize> {
    let mut j = open + 1;
    if j < chars.len() && (chars[j] == '!' || chars[j] == '^') {
        j += 1;
    }
    if j < chars.len() && chars[j] == ']' {
        j += 1;
    }
    while j < chars.len() && chars[j] != ']' {
        j += 1;
    }
    (j < chars.len()).then_some(j)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str, value: &str) -> bool {
        compile_glob(pattern).unwrap().is_match(value).unwrap()
    }

    #[test]
    fn star_and_question() {
        assert!(glob("ls*", "ls -la"));
        assert!(glob("git ?", "git a"));
        assert!(!glob("git ?", "git ab"));
        assert!(glob("echo *", "echo hi there"));
        assert!(!glob("echo *", "echohi"));
    }

    #[test]
    fn full_match_is_anchored() {
        assert!(glob("rm -rf*", "rm -rf /tmp"));
        assert!(!glob("pwd", "pwd extra"));
        assert!(glob("pwd", "pwd"));
    }

    #[test]
    fn extglob_exactly_one() {
        assert!(glob("@(true|false|:)*", "true && echo"));
        assert!(glob("@(sh|bash|zsh)", "bash"));
        assert!(!glob("@(sh|bash|zsh)", "fish"));
    }

    #[test]
    fn extglob_nested_alternatives() {
        let p = "@(npm|yarn|pnpm) @(list|test|run @(test|build))*";
        assert!(glob(p, "npm test"));
        assert!(glob(p, "npm run build --silent"));
        assert!(glob(p, "yarn list"));
        assert!(!glob(p, "npm publish"));
    }

    #[test]
    fn char_class() {
        assert!(glob("file[0-9]", "file3"));
        assert!(!glob("file[0-9]", "filea"));
        assert!(glob("file[!0-9]", "filea"));
    }

    #[test]
    fn literal_metacharacters_are_escaped() {
        assert!(glob("a.b", "a.b"));
        assert!(!glob("a.b", "axb"));
    }

    #[test]
    fn matcher_literal_is_exact() {
        let m = Matcher::Literal("git status".to_string());
        assert!(m.is_match("git status"));
        assert!(!m.is_match("git status -s"));
    }

    #[test]
    fn extglob_negation() {
        assert!(glob("!(foo)", "bar"));
        assert!(!glob("!(foo)", "foo"));
    }

    #[test]
    fn extglob_zero_or_one_and_one_or_more() {
        assert!(glob("ab?(c)", "ab"));
        assert!(glob("ab?(c)", "abc"));
        assert!(!glob("ab?(c)", "abcc"));
        assert!(glob("a+(b)", "abbb"));
        assert!(!glob("a+(b)", "a"));
    }

    #[test]
    fn unbalanced_extglob_is_literal() {
        // A `@(` with no closing paren is treated as literal text.
        assert!(glob("@(a", "@(a"));
    }

    #[test]
    fn regex_kind_is_full_match() {
        let re = compile_regex("git (status|diff)").unwrap();
        assert!(re.is_match("git status").unwrap());
        assert!(!re.is_match("git status --short").unwrap());
    }

    #[test]
    fn metacharacter_free_globs_compile_to_literals() {
        // Plain words and patterns whose only "special" chars are literal in the
        // glob dialect must skip regex construction.
        for pattern in ["gh", "api", "-X", "git", "a.b", "foo(bar)", "repos/myorg"] {
            assert!(
                matches!(compile_glob_matcher(pattern).unwrap(), Matcher::Literal(_)),
                "expected {pattern:?} to compile to a literal matcher",
            );
        }
    }

    #[test]
    fn char_class_globs_compile_eagerly_others_lazily() {
        // A character class can make translation fail, so it must surface a
        // compile error at build time (an eager regex)…
        for pattern in ["file[0-9]", "a[!x]b", "@(a|b)[0-9]"] {
            assert!(
                matches!(compile_glob_matcher(pattern).unwrap(), Matcher::Regex(_)),
                "expected {pattern:?} to compile to an eager regex matcher",
            );
        }
        // …while every other glob construct translates to valid-by-construction
        // regex and defers compilation behind the prefix gate.
        for pattern in ["ls*", "git ?", "@(a|b)", "?(a)", "*(a)", "+(a)", "!(x)"] {
            assert!(
                matches!(
                    compile_glob_matcher(pattern).unwrap(),
                    Matcher::LazyGlob { .. }
                ),
                "expected {pattern:?} to compile to a lazy glob matcher",
            );
        }
    }

    #[test]
    fn lazy_glob_matches_exactly_like_the_eager_regex() {
        // Laziness must be transparent: for a matrix of patterns and values the
        // lazy matcher agrees with the eager regex it replaced.
        let patterns = [
            "ls*",
            "git @(status|diff)*",
            "@(true|false|:)*",
            "@(head|tail) *",
            "+(ab|c)d",
            "?(re)build",
            "*(x)y",
            "!(foo)",
            "x@(y|z)w",
            "@(a|@(b|c))t",
            "rm -rf*",
        ];
        let values = [
            "ls -la",
            "git status",
            "git diff --stat",
            "git push",
            "true && echo",
            "head -5",
            "tail -n 1",
            "abd",
            "cd",
            "d",
            "build",
            "rebuild",
            "y",
            "xy",
            "foo",
            "bar",
            "xyw",
            "xzw",
            "at",
            "bt",
            "rm -rf /",
            "",
        ];
        for pattern in patterns {
            let lazy = compile_glob_matcher(pattern).unwrap();
            assert!(matches!(lazy, Matcher::LazyGlob { .. }));
            let eager = Matcher::Regex(compile_glob(pattern).unwrap());
            for value in values {
                assert_eq!(
                    lazy.is_match(value),
                    eager.is_match(value),
                    "lazy disagrees with eager for pattern {pattern:?} on {value:?}",
                );
            }
        }
    }

    #[test]
    fn literal_prefixes_fork_on_required_groups_only() {
        let prefixes = |p: &str| literal_prefixes(p);
        // Literal text up to the first wildcard.
        assert_eq!(prefixes("git *"), vec!["git ".to_string()]);
        assert_eq!(prefixes("git ?"), vec!["git ".to_string()]);
        assert_eq!(prefixes("a[0-9]b"), vec!["a".to_string()]);
        // A required group forks per alternative, recursively.
        assert_eq!(prefixes("@(a|b)c"), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            prefixes("x@(y|z)w"),
            vec!["xy".to_string(), "xz".to_string()]
        );
        assert_eq!(
            prefixes("+(ab|c)d"),
            vec!["ab".to_string(), "c".to_string()]
        );
        assert_eq!(
            prefixes("@(a@(b|c)|d)"),
            vec!["ab".to_string(), "ac".to_string(), "d".to_string()]
        );
        // Alternatives that start with a wildcard contribute the base alone.
        assert_eq!(
            prefixes("git @(*x|pull)"),
            vec!["git ".to_string(), "git pull".to_string()]
        );
        // Optional and negated groups pin nothing down.
        assert_eq!(prefixes("?(a)b"), vec![String::new()]);
        assert_eq!(prefixes("*(a)b"), vec![String::new()]);
        assert_eq!(prefixes("!(x)y"), vec![String::new()]);
        // An unbalanced group stops the scan conservatively.
        assert_eq!(prefixes("a@(b"), vec!["a".to_string()]);
    }

    #[test]
    fn oversized_prefix_fork_falls_back_to_base() {
        // More alternatives than the fork cap: the gate degrades to the base
        // prefix (here empty, gating nothing) rather than a huge prefix list.
        let alts = (0..70).map(|i| format!("cmd{i}")).collect::<Vec<_>>();
        let pattern = format!("@({}) *", alts.join("|"));
        assert_eq!(literal_prefixes(&pattern), vec![String::new()]);
        // Matching still works — the gate never rejects, it just stops helping.
        let matcher = compile_glob_matcher(&pattern).unwrap();
        assert!(matcher.is_match("cmd42 --flag"));
        assert!(!matcher.is_match("other --flag"));
    }

    #[test]
    fn gate_rejection_skips_regex_construction() {
        // The perf contract itself: a value that fails the prefix gate must be
        // rejected without ever building the regex.
        let matcher = compile_glob_matcher("git @(status|diff)*").unwrap();
        assert!(!matcher.is_match("npm install"));
        let Matcher::LazyGlob { compiled, .. } = &matcher else {
            panic!("expected a lazy glob matcher");
        };
        assert!(
            compiled.get().is_none(),
            "gate rejection must not compile the regex"
        );
        assert!(matcher.is_match("git status --short"));
        assert!(
            compiled.get().is_some(),
            "a gate pass compiles exactly once"
        );
    }

    #[test]
    fn lazy_compile_failure_is_no_match_not_a_panic() {
        // compile_glob_matcher never builds a lazy matcher whose translation
        // fails, but the engine's never-panic guarantee must hold even if that
        // invariant ever drifts: a failed lazy compile is simply "no match".
        let matcher = Matcher::LazyGlob {
            pattern: "[z-a]".to_string(),
            prefixes: vec![String::new()],
            compiled: OnceLock::new(),
        };
        assert!(!matcher.is_match("anything"));
    }

    #[test]
    fn literal_fast_path_matches_exactly_like_the_regex_path() {
        // The optimization must be transparent: for every literal-detected
        // pattern the fast path agrees with the full regex it replaced, both on
        // the exact text and on near-misses.
        for pattern in ["gh", "-X", "a.b", "foo(bar)", "repos/myorg"] {
            let fast = compile_glob_matcher(pattern).unwrap();
            let regex = Matcher::Regex(compile_glob(pattern).unwrap());
            for value in [pattern, "", &format!("{pattern}x"), &format!("x{pattern}")] {
                assert_eq!(
                    fast.is_match(value),
                    regex.is_match(value),
                    "fast path disagrees with regex for pattern {pattern:?} on {value:?}",
                );
            }
            assert!(fast.is_match(pattern));
        }
    }
}
