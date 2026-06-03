//! Filesystem discovery of configuration files.
//!
//! User config (first existing wins):
//!   1. `$XDG_CONFIG_HOME/allowlister/config.json`
//!   2. `~/.config/allowlister/config.json`
//!   3. `~/.allowlister.json`
//!
//! Project config: starting at `cwd`, walk up to the filesystem root or the
//! first directory containing `.git`. At each level collect `.allowlister.json`
//! and `.allowlister/config.json`. Results are returned outermost-first so that
//! more local rules are appended after broader ones.

use std::path::{Path, PathBuf};

/// Environment inputs for discovery. Injected so tests are hermetic.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
}

impl Env {
    /// Read the relevant environment variables from the process.
    pub fn from_process() -> Env {
        Env {
            home: std::env::var_os("HOME").map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        }
    }
}

/// Existing config files in merge order: user config first, then project
/// configs from the outermost ancestor down to `cwd`.
pub fn discover(cwd: &Path, env: &Env) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(user) = user_config_path(env) {
        paths.push(user);
    }
    paths.extend(project_config_paths(cwd));
    paths
}

/// The first existing user-level config path, if any.
pub fn user_config_path(env: &Env) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = &env.xdg_config_home {
        candidates.push(xdg.join("allowlister").join("config.json"));
    }
    if let Some(home) = &env.home {
        candidates.push(home.join(".config").join("allowlister").join("config.json"));
        candidates.push(home.join(".allowlister.json"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// The path a user-level config is *written* to (XDG first, else `~/.config`),
/// whether or not it already exists. `None` when neither `XDG_CONFIG_HOME` nor
/// `HOME` is set. Unlike [`user_config_path`], this does not require the file to
/// exist — it is the destination for `init` and `install`.
pub(crate) fn default_user_config_path(env: &Env) -> Option<PathBuf> {
    if let Some(xdg) = &env.xdg_config_home {
        return Some(xdg.join("allowlister").join("config.json"));
    }
    env.home
        .as_ref()
        .map(|home| home.join(".config").join("allowlister").join("config.json"))
}

/// The project-level config path under `dir` (`<dir>/.allowlister.json`).
pub(crate) fn local_config_path(dir: &Path) -> PathBuf {
    dir.join(".allowlister.json")
}

/// Project configs from `cwd` up to a `.git` boundary or the filesystem root,
/// returned outermost-first.
pub fn project_config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut current = cwd;
    loop {
        for name in [".allowlister.json", ".allowlister/config.json"] {
            let candidate = current.join(name);
            if candidate.is_file() {
                found.push(candidate);
            }
        }
        if current.join(".git").exists() {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    found.reverse();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn user_path_prefers_xdg() {
        let dir = TempDir::new().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(xdg.join("allowlister")).unwrap();
        fs::write(xdg.join("allowlister/config.json"), "{}").unwrap();
        let env = Env {
            home: Some(dir.path().join("home")),
            xdg_config_home: Some(xdg.clone()),
        };
        assert_eq!(
            user_config_path(&env),
            Some(xdg.join("allowlister/config.json"))
        );
    }

    #[test]
    fn project_walk_stops_at_git() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let nested = root.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".allowlister.json"), "{}").unwrap();
        fs::write(nested.join(".allowlister.json"), "{}").unwrap();
        // A config above the git root must not be picked up.
        fs::write(dir.path().join("..").join("ignore.json"), "{}").ok();

        let paths = project_config_paths(&nested);
        assert_eq!(paths.len(), 2);
        // Outermost first.
        assert_eq!(paths[0], root.join(".allowlister.json"));
        assert_eq!(paths[1], nested.join(".allowlister.json"));
    }
}
