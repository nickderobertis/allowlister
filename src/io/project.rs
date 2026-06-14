//! Resolve the durable "project" tag a usage-history event is attributed to.
//!
//! History is one user-global store with each event tagged by the project it ran
//! in. Tagging by the raw working directory splits the same logical repository
//! across every clone (and every subdirectory) it is ever checked out into, so
//! the counts never add up across machines. Instead, a directory inside a git
//! repository is tagged by *repository identity*:
//!
//! - its `origin` remote URL (any remote when there is no `origin`), normalized
//!   so the same repo aggregates whether cloned over HTTPS or SSH — this is what
//!   makes history add up across clones, the whole point of the git method; or
//! - the repository root path, when the repo has no remote to key on (clones
//!   still cannot merge, but every subdirectory of one checkout does).
//!
//! A directory that is not in a git repository falls back to the working
//! directory itself, the original folder-based tag.
//!
//! This is best-effort and fail-open like the rest of recording: any failure to
//! read the repo just degrades to the next fallback, never an error.

use std::fs;
use std::path::Path;

/// The project tag for a working directory: repository identity when `cwd` is
/// inside a git repo, else the directory itself (unchanged folder tag).
pub(crate) fn identify(cwd: &str) -> String {
    git_identity(cwd).unwrap_or_else(|| cwd.to_string())
}

/// Repository identity for `cwd`, or `None` when it is not inside a git repo.
fn git_identity(cwd: &str) -> Option<String> {
    // Walk the path as given (like project-config discovery, which also does not
    // canonicalize): a remote keys clones together regardless of checkout path,
    // and for the no-remote fallback the root is reported in the same spelling the
    // caller used — so the tag matches what users and `--project` filters see,
    // not a platform-canonicalized form (e.g. Windows `\\?\` verbatim paths).
    let root = find_repo_root(Path::new(cwd))?;
    // Without a remote the root path is the best we can do (it still merges a
    // checkout's subdirectories).
    Some(remote_identity(root).unwrap_or_else(|| root.to_string_lossy().into_owned()))
}

/// Walk up from `start` to the nearest ancestor containing a `.git` entry — the
/// same boundary project-config discovery stops at.
fn find_repo_root(start: &Path) -> Option<&Path> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        current = dir.parent();
    }
    None
}

/// The normalized remote URL recorded in `<root>/.git/config`, preferring
/// `origin`. `None` when `.git` is not a readable directory with a config (e.g. a
/// worktree's `.git` file) or the repo has no remote.
fn remote_identity(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join(".git").join("config")).ok()?;
    normalize_remote(&remote_url(&text)?)
}

/// The `url` of the `origin` remote in a git config, or the first remote's url
/// when there is no `origin`.
fn remote_url(config: &str) -> Option<String> {
    let mut current_remote: Option<String> = None;
    let mut origin: Option<String> = None;
    let mut first: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Entering a new section: track it only when it is a remote.
            current_remote = section_remote(line);
            continue;
        }
        let Some(remote) = &current_remote else {
            continue;
        };
        if let Some(url) = config_value(line, "url") {
            if first.is_none() {
                first = Some(url.clone());
            }
            if remote == "origin" {
                origin = Some(url);
            }
        }
    }
    origin.or(first)
}

/// The remote name of a `[remote "name"]` section header, or `None` for any other
/// section.
fn section_remote(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?.trim();
    let name = inner.strip_prefix("remote")?.trim();
    Some(name.strip_prefix('"')?.strip_suffix('"')?.to_string())
}

/// The value of `key = value` on a git-config line, unquoted. `None` when the
/// line sets a different key (matching is whole-key, so `url` never matches
/// `urlfoo`).
fn config_value(line: &str, key: &str) -> Option<String> {
    let rest = line
        .trim_start()
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix('=')?
        .trim();
    let value = rest
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(rest);
    (!value.is_empty()).then(|| value.to_string())
}

/// Reduce a remote URL to a `host/path` identity so the same repository collapses
/// to one tag regardless of transport: `https://github.com/o/r.git`,
/// `git@github.com:o/r.git`, and `ssh://git@github.com/o/r` all become
/// `github.com/o/r`. The host is lowercased (case-insensitive); the path is left
/// as-is.
fn normalize_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let has_scheme = url.contains("://");
    // Drop the scheme.
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    // Drop any `user[:pass]@` credentials (the first `@` is the separator; a
    // later one would belong to the path).
    let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
    // scp-like `host:path` (no scheme) uses a colon where a URL uses a slash.
    let mut out = match (has_scheme, rest.split_once(':')) {
        (false, Some((host, path))) => format!("{host}/{}", path.trim_start_matches('/')),
        _ => rest.to_string(),
    };
    // Drop a trailing `.git` and slashes so spellings converge.
    let trimmed = out.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    out = trimmed.trim_end_matches('/').to_string();
    if out.is_empty() {
        return None;
    }
    // Lowercase only the host (up to the first slash); paths can be
    // case-sensitive.
    let split = out.find('/').unwrap_or(out.len());
    if let Some(host) = out.get_mut(..split) {
        host.make_ascii_lowercase();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Make `dir` a git repo whose `.git/config` declares `remote` named `origin`
    /// pointing at `url` (no remote when `url` is empty).
    fn init_repo(dir: &Path, url: &str) {
        let git = dir.join(".git");
        fs::create_dir_all(&git).unwrap();
        let body = if url.is_empty() {
            "[core]\n\trepositoryformatversion = 0\n".to_string()
        } else {
            format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n")
        };
        fs::write(git.join("config"), body).unwrap();
    }

    #[test]
    fn non_git_dir_falls_back_to_the_folder() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        assert_eq!(identify(&cwd), cwd);
    }

    #[test]
    fn git_repo_is_tagged_by_its_remote() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path(), "https://github.com/octocat/Hello-World.git");
        assert_eq!(
            identify(&dir.path().to_string_lossy()),
            "github.com/octocat/Hello-World"
        );
    }

    #[test]
    fn subdirectory_of_a_repo_resolves_to_the_repo_identity() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path(), "git@github.com:octocat/Hello-World.git");
        let nested = dir.path().join("src/deep");
        fs::create_dir_all(&nested).unwrap();
        // A subdirectory still tags as the one repository.
        assert_eq!(
            identify(&nested.to_string_lossy()),
            "github.com/octocat/Hello-World"
        );
    }

    #[test]
    fn https_and_ssh_clones_aggregate_to_one_tag() {
        let https = TempDir::new().unwrap();
        init_repo(https.path(), "https://github.com/octocat/Hello-World.git");
        let ssh = TempDir::new().unwrap();
        init_repo(ssh.path(), "ssh://git@github.com/octocat/Hello-World.git");
        let scp = TempDir::new().unwrap();
        init_repo(scp.path(), "git@github.com:octocat/Hello-World.git");
        // Three clones over three transports collapse to the same identity.
        let a = identify(&https.path().to_string_lossy());
        let b = identify(&ssh.path().to_string_lossy());
        let c = identify(&scp.path().to_string_lossy());
        assert_eq!(a, "github.com/octocat/Hello-World");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn repo_without_a_remote_falls_back_to_the_root_path() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path(), "");
        // No remote to key on: the repo root path, in the spelling the caller
        // passed (not canonicalized), is the tag.
        assert_eq!(
            identify(&dir.path().to_string_lossy()),
            dir.path().to_string_lossy()
        );
    }

    #[test]
    fn git_file_worktree_falls_back_to_the_root_path() {
        // A linked worktree or submodule has a `.git` *file* (`gitdir: …`), not a
        // directory, so there is no `<root>/.git/config` to read. The lookup must
        // still treat the directory as a repo root and fall back to its path.
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".git"),
            "gitdir: /elsewhere/.git/worktrees/wt\n",
        )
        .unwrap();
        assert_eq!(
            identify(&dir.path().to_string_lossy()),
            dir.path().to_string_lossy()
        );
    }

    #[test]
    fn origin_is_preferred_over_other_remotes() {
        let config = "[remote \"upstream\"]\n\turl = https://github.com/up/stream.git\n\
                      [remote \"origin\"]\n\turl = https://github.com/me/fork.git\n";
        assert_eq!(
            remote_url(config).as_deref(),
            Some("https://github.com/me/fork.git")
        );
    }

    #[test]
    fn first_remote_is_used_when_there_is_no_origin() {
        let config = "[remote \"upstream\"]\n\turl = https://github.com/up/stream.git\n";
        assert_eq!(
            remote_url(config).as_deref(),
            Some("https://github.com/up/stream.git")
        );
    }

    #[test]
    fn url_outside_a_remote_section_is_ignored() {
        // A `url` under some non-remote section must not be mistaken for a remote.
        let config = "[branch \"main\"]\n\turl = not-a-remote\n";
        assert_eq!(remote_url(config), None);
    }

    #[test]
    fn config_value_matches_whole_keys_only() {
        assert_eq!(config_value("url = x", "url").as_deref(), Some("x"));
        assert_eq!(config_value("\turl=y", "url").as_deref(), Some("y"));
        assert_eq!(config_value("urlfoo = z", "url"), None);
        assert_eq!(config_value("fetch = +refs", "url"), None);
    }

    #[test]
    fn normalize_strips_scheme_credentials_and_dot_git() {
        assert_eq!(
            normalize_remote("https://user:token@GitHub.com/Org/Repo.git").as_deref(),
            Some("github.com/Org/Repo")
        );
        assert_eq!(
            normalize_remote("git@gitlab.com:group/sub/proj.git").as_deref(),
            Some("gitlab.com/group/sub/proj")
        );
        assert_eq!(normalize_remote("").as_deref(), None);
    }
}
