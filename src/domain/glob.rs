//! Glob matching with bash `extglob` support.
//!
//! The standard glob crates do not implement extended globbing
//! (`@(a|b)`, `?(a|b)`, `*(a|b)`, `+(a|b)`, `!(a|b)`), which is exactly the
//! syntax that makes allow/deny rules compact and readable. Patterns are
//! translated once into an anchored, full-match regular expression and then
//! reused for every fragment evaluated in a process.
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
use fancy_regex::Regex;

/// A compiled command matcher. Glob and regex kinds become a full-match
/// [`Regex`]; the literal kind keeps the raw string for exact comparison.
#[derive(Debug, Clone)]
pub(crate) enum Matcher {
    Regex(Regex),
    Literal(String),
}

impl Matcher {
    pub(crate) fn is_match(&self, value: &str) -> bool {
        match self {
            // A backtracking error (e.g. catastrophic input) is treated as "no
            // match" rather than propagated: the engine must never panic.
            Matcher::Regex(re) => re.is_match(value).unwrap_or(false),
            Matcher::Literal(lit) => lit == value,
        }
    }
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
}
