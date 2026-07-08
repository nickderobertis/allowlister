//! Scope a tool call's file path to the config directory.
//!
//! A shipped profile cannot name a machine-specific absolute path, so it says
//! "inside the project" with a relative glob like `./**`. But a harness may send
//! the same file as an absolute path (`/home/u/repo/src/x`) or a relative one
//! (`src/x`), and harnesses disagree on which. This rewrites the canonical
//! `path` parameter into one consistent form before the engine matches it: a
//! path that resolves inside `base` becomes `./`-relative (forward slashes,
//! `.`/`..` resolved); a path that resolves outside is left as written. So one
//! `./**` rule fires the same for every harness and path style, and a path
//! outside the project matches no such rule and defers.
//!
//! Purely lexical: no filesystem access (it works for paths that do not exist)
//! and no symlink resolution, matching the rest of the engine. Resolving `..`
//! textually also means `<base>/../etc/passwd` normalizes to an outside path, so
//! traversal cannot disguise an external file as in-project.

use std::path::{Component, Path, PathBuf};

use crate::domain::{Capability, ParamKey, ToolCall};

/// Return a copy of `call` whose canonical `path` parameter is scoped to `base`
/// (see module docs). A call without a `path` parameter — a web fetch, an MCP
/// tool, a `read` that carried no path — is returned unchanged, except for
/// `glob`/`grep`: those search the working directory when no path is given, so an
/// absent path is really "the project root". Scoping it like an explicit `.` lets
/// an in-project `./**` rule fire instead of deferring — which would strand a
/// headless agent (a bare `Glob`/`Grep` with no approval channel) on a defer.
pub(crate) fn scope_to_base(call: &ToolCall, base: &Path) -> ToolCall {
    let path = match call.params.get(ParamKey::Path) {
        Some(path) => path.to_string(),
        None if matches!(call.capability, Capability::Glob | Capability::Grep) => ".".to_string(),
        None => return call.clone(),
    };
    let scoped = scope_path(&path, base);
    let mut params = call.params.clone();
    params.insert(ParamKey::Path, scoped);
    ToolCall::new(
        call.capability,
        call.tool_name.clone(),
        params,
        call.raw.clone(),
    )
}

/// The path-string rewrite behind [`scope_to_base`].
fn scope_path(path: &str, base: &Path) -> String {
    // Nothing to anchor against, or nothing to anchor: leave the value verbatim
    // so behavior degrades to raw string matching rather than a wrong rewrite.
    if path.is_empty() || !base.is_absolute() {
        return path.to_string();
    }
    // A `~`-relative path needs $HOME to expand (an env read this pure layer
    // avoids) and points outside the project anyway; leave it so the
    // secret-read deny still matches it by substring.
    if path == "~" || path.starts_with("~/") || path.starts_with("~\\") {
        return path.to_string();
    }

    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        base.join(raw)
    };
    let abs = lexical_normalize(&joined);
    let base = lexical_normalize(base);

    match abs.strip_prefix(&base) {
        // Inside the project: emit a `./`-relative, forward-slash path so a
        // single `./**` glob matches it regardless of the source path style.
        Ok(relative) => {
            let relative = to_forward_slashes(relative);
            if relative.is_empty() {
                "./".to_string()
            } else {
                format!("./{relative}")
            }
        }
        // Outside the project: emit the resolved absolute path with forward
        // slashes. Inside-vs-outside was decided from the normalized `abs`, so
        // traversal cannot disguise an external file as in-project; forward slashes
        // (rather than the platform separator) let a config's secret-path deny —
        // written with `/`, e.g. `**/.ssh/**` — match on Windows too.
        Err(_) => os_path_to_slash(&abs),
    }
}

/// The path's string form with `/` separators. On Windows the backslash is the
/// path separator, so normalize it to `/` for matching against forward-slash
/// config globs; on Unix a backslash is a legal filename byte, so leave it be.
fn os_path_to_slash(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        text.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        text
    }
}

/// Resolve `.` and `..` components textually, without touching the filesystem.
/// A `..` pops a preceding normal component; with nothing to pop (a relative
/// path ascending past its start) the `..` is kept, which keeps the path outside
/// any base rather than silently collapsing it.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !pop_normal(&mut out) {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Pop the last component only when it is a normal path segment, so a `..` never
/// ascends past a root/prefix (`/`, `C:\`) or collapses an earlier, un-poppable
/// `..` — it just cannot resolve, and the path stays outside any base.
fn pop_normal(path: &mut PathBuf) -> bool {
    matches!(path.components().next_back(), Some(Component::Normal(_))) && path.pop()
}

/// Join a relative path's normal segments with `/`, the separator the profile
/// globs are written in, so matching is identical on every platform.
fn to_forward_slashes(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Capability, NormalizedParams};
    use serde_json::json;

    // Used only by the `#[cfg(unix)]` cases below (they assert on Unix absolute
    // paths), so it is Unix-gated too — otherwise it is dead code on Windows and
    // trips the `-D warnings` lint.
    #[cfg(unix)]
    fn scoped(path: &str, base: &str) -> String {
        scope_path(path, Path::new(base))
    }

    #[cfg(unix)]
    #[test]
    fn absolute_and_relative_inside_normalize_identically() {
        // The whole point: an absolute in-project path and a relative one to the
        // same file collapse to one `./`-relative form.
        assert_eq!(scoped("/repo/src/main.rs", "/repo"), "./src/main.rs");
        assert_eq!(scoped("src/main.rs", "/repo"), "./src/main.rs");
        assert_eq!(scoped("./src/main.rs", "/repo"), "./src/main.rs");
        // A nested subdirectory still resolves under the project.
        assert_eq!(scoped("/repo/a/b/c.txt", "/repo"), "./a/b/c.txt");
        // The config directory itself collapses to `./` (no trailing segment).
        assert_eq!(scoped("/repo", "/repo"), "./");
        assert_eq!(scoped(".", "/repo"), "./");
    }

    #[cfg(unix)]
    #[test]
    fn paths_outside_the_project_resolve_to_an_absolute_form() {
        // An outside path resolves to an absolute (never `./`) form, so an
        // in-project `./**` allow can't match it, while an absolute secret-path
        // deny still can.
        for (path, base, expected) in [
            ("/etc/hosts", "/repo", "/etc/hosts"),
            // A relative path escaping the base resolves to the outside absolute.
            ("../other/x", "/repo", "/other/x"),
            // Traversal that escapes the base is judged outside from the resolved
            // path, so it can never masquerade as in-project.
            ("/repo/../etc/passwd", "/repo", "/etc/passwd"),
            ("subdir/../../etc/x", "/repo", "/etc/x"),
            ("/repo-evil/x", "/repo", "/repo-evil/x"),
            ("/repo/../repo-evil/x", "/repo", "/repo-evil/x"),
            // More `..` than depth: the surplus cannot pop past root, so it never
            // resolves back inside the base.
            ("/a/../../etc/x", "/repo", "/../etc/x"),
        ] {
            let out = scoped(path, base);
            assert_eq!(out, expected, "input {path:?}");
            assert!(
                !out.starts_with("./"),
                "outside path must not look in-project"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_normalize_separators_for_forward_slash_globs() {
        // A real Windows harness sends backslash paths, but matching is done
        // against config globs written with `/`, so the scoped form must use `/` —
        // inside the project and out. Only exercised on Windows.
        let base = Path::new(r"C:\repo");
        assert_eq!(scope_path(r"C:\repo\src\main.rs", base), "./src/main.rs");
        assert_eq!(
            scope_path(r"C:\Users\me\.ssh\id_rsa", base),
            "C:/Users/me/.ssh/id_rsa"
        );
    }

    #[cfg(unix)]
    #[test]
    fn home_relative_and_unanchored_paths_are_left_verbatim() {
        // `~` needs $HOME to expand; leave it so the secret deny matches it.
        assert_eq!(scoped("~/.ssh/id_rsa", "/repo"), "~/.ssh/id_rsa");
        // A non-absolute base cannot anchor anything, so nothing is rewritten.
        assert_eq!(scoped("/repo/src/x", "."), "/repo/src/x");
        assert_eq!(scoped("", "/repo"), "");
    }

    #[test]
    fn only_the_path_param_is_rewritten() {
        // Build a read call with an in-project path via the platform's own base,
        // so the assertion holds on every OS.
        let base = std::env::current_dir().unwrap();
        let file = base.join("src").join("x.rs");
        let mut params = NormalizedParams::new();
        params.insert(ParamKey::Path, file.to_string_lossy().into_owned());
        let call = ToolCall::new(
            Capability::Read,
            "Read".to_string(),
            params,
            json!({ "file_path": file.to_string_lossy() }),
        );
        let out = scope_to_base(&call, &base);
        assert_eq!(out.params.get(ParamKey::Path), Some("./src/x.rs"));
        // The raw input is untouched, so jsonpath rules still see the original.
        assert_eq!(out.raw, call.raw);
    }

    #[test]
    fn a_call_without_a_path_is_unchanged() {
        let mut params = NormalizedParams::new();
        params.insert(ParamKey::Url, "https://github.com/x".to_string());
        let call = ToolCall::new(
            Capability::WebFetch,
            "WebFetch".to_string(),
            params,
            json!({ "url": "https://github.com/x" }),
        );
        let out = scope_to_base(&call, Path::new("/repo"));
        assert_eq!(out.params.get(ParamKey::Url), Some("https://github.com/x"));
        // A pathless read (Codex apply_patch, a read carrying no path) has nothing
        // to scope, so no synthetic in-project path is invented for it.
        let read = ToolCall::new(
            Capability::Read,
            "read".to_string(),
            NormalizedParams::new(),
            json!({}),
        );
        assert_eq!(
            scope_to_base(&read, Path::new("/repo"))
                .params
                .get(ParamKey::Path),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn glob_and_grep_without_a_path_scope_to_the_project_root() {
        // glob/grep default to searching the working directory, so an absent path
        // must scope to the project root (`./`) — which an in-project `./**` rule
        // matches — rather than staying pathless and deferring (the halt in #119).
        for capability in [Capability::Glob, Capability::Grep] {
            let call = ToolCall::new(
                capability,
                "test".to_string(),
                NormalizedParams::new(),
                json!({ "pattern": "**/*.rs" }),
            );
            let out = scope_to_base(&call, Path::new("/repo"));
            assert_eq!(
                out.params.get(ParamKey::Path),
                Some("./"),
                "{capability:?} with no path should target the project root"
            );
        }
        // An explicit in-project glob path still normalizes the usual way.
        let mut params = NormalizedParams::new();
        params.insert(ParamKey::Path, "/repo/src".to_string());
        let call = ToolCall::new(Capability::Glob, "test".to_string(), params, json!({}));
        assert_eq!(
            scope_to_base(&call, Path::new("/repo"))
                .params
                .get(ParamKey::Path),
            Some("./src")
        );
    }
}
