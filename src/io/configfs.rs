//! Filesystem discovery of configuration files.
//!
//! Configs are JSONC; `.jsonc` is the preferred extension (editors then accept
//! the comments) and the default for newly created files, while `.json` remains
//! fully supported. Wherever both spellings could exist, `.jsonc` wins and the
//! `.json` twin is ignored.
//!
//! User config (first existing wins):
//!   1. `$XDG_CONFIG_HOME/allowlister/config.jsonc` (or `.json`)
//!   2. `~/.config/allowlister/config.jsonc` (or `.json`)
//!   3. `~/.allowlister.jsonc` (or `.json`)
//!
//! Project config: starting at `cwd`, walk up to the filesystem root or the
//! first directory containing `.git`. At each level collect `.allowlister.jsonc`
//! and `.allowlister/config.jsonc` (or their `.json` twins). Results are
//! returned outermost-first so that more local rules are appended after broader
//! ones.

use std::path::{Path, PathBuf};

/// Environment inputs for discovery. Injected so tests are hermetic.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    /// Raw `ALLOWLISTER_HISTORY` value, if set. The recording toggle's semantics
    /// (which strings mean on) live in `io::history`; this layer only carries the
    /// raw value so the override is injectable in tests.
    pub history_override: Option<String>,
}

impl Env {
    /// Read the relevant environment variables from the process.
    pub fn from_process() -> Env {
        Env {
            home: std::env::var_os("HOME").map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            history_override: std::env::var("ALLOWLISTER_HISTORY").ok(),
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

/// The user-level config filenames, preferred spelling first.
const USER_CONFIG_NAMES: [&str; 2] = ["config.jsonc", "config.json"];
/// The home-dotfile and project config filenames, preferred spelling first.
const DOTFILE_NAMES: [&str; 2] = [".allowlister.jsonc", ".allowlister.json"];

/// The first existing user-level config path, if any. Location order decides
/// first (XDG, then `~/.config`, then the home dotfile); within a location the
/// `.jsonc` spelling beats `.json`.
pub fn user_config_path(env: &Env) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xdg) = &env.xdg_config_home {
        for name in USER_CONFIG_NAMES {
            candidates.push(xdg.join("allowlister").join(name));
        }
    }
    if let Some(home) = &env.home {
        for name in USER_CONFIG_NAMES {
            candidates.push(home.join(".config").join("allowlister").join(name));
        }
        for name in DOTFILE_NAMES {
            candidates.push(home.join(name));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// The path under `dir` an update or a new file should target: an existing file
/// is updated in place (preferred spelling first), and when neither spelling
/// exists the preferred one is the destination for a new file.
fn existing_or_default(dir: &Path, names: [&str; 2]) -> PathBuf {
    names
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| dir.join(names[0]))
}

/// The path a user-level config is *written* to (XDG first, else `~/.config`),
/// whether or not it already exists. An existing config keeps its spelling and
/// is updated in place; a new one is created as `config.jsonc`. `None` when
/// neither `XDG_CONFIG_HOME` nor `HOME` is set. Unlike [`user_config_path`],
/// this does not require the file to exist — it is the destination for `init`
/// and `install`.
pub(crate) fn default_user_config_path(env: &Env) -> Option<PathBuf> {
    let dir = if let Some(xdg) = &env.xdg_config_home {
        xdg.join("allowlister")
    } else {
        env.home.as_ref()?.join(".config").join("allowlister")
    };
    Some(existing_or_default(&dir, USER_CONFIG_NAMES))
}

/// The project-level config path under `dir`: an existing `.allowlister.jsonc`
/// or `.allowlister.json` is updated in place; otherwise a new file goes to
/// `.allowlister.jsonc`.
pub(crate) fn local_config_path(dir: &Path) -> PathBuf {
    existing_or_default(dir, DOTFILE_NAMES)
}

/// The directory usage history is written to: a `history/` folder beside the
/// user-level config (XDG first, else `~/.config`). `None` when neither
/// `XDG_CONFIG_HOME` nor `HOME` is set. History is intentionally user-global —
/// one store spanning every project — with each event tagged by its project, so
/// it survives across repositories and never lands in version control.
pub(crate) fn default_history_dir(env: &Env) -> Option<PathBuf> {
    let config = default_user_config_path(env)?;
    let parent = config.parent().unwrap_or(Path::new("."));
    Some(parent.join("history"))
}

/// Project configs from `cwd` up to a `.git` boundary or the filesystem root,
/// returned outermost-first. At each level the dotfile and the `.allowlister/`
/// directory are separate locations; within each, `.jsonc` wins over `.json`.
pub fn project_config_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut current = cwd;
    loop {
        for names in [
            DOTFILE_NAMES,
            [".allowlister/config.jsonc", ".allowlister/config.json"],
        ] {
            if let Some(candidate) = names
                .into_iter()
                .map(|name| current.join(name))
                .find(|path| path.is_file())
            {
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
            ..Env::default()
        };
        assert_eq!(
            user_config_path(&env),
            Some(xdg.join("allowlister/config.json"))
        );
    }

    #[test]
    fn jsonc_wins_over_json_in_the_same_location() {
        let dir = TempDir::new().unwrap();
        let xdg = dir.path().join("xdg");
        fs::create_dir_all(xdg.join("allowlister")).unwrap();
        fs::write(xdg.join("allowlister/config.json"), "{}").unwrap();
        fs::write(xdg.join("allowlister/config.jsonc"), "{}").unwrap();
        let env = Env {
            home: None,
            xdg_config_home: Some(xdg.clone()),
            ..Env::default()
        };
        assert_eq!(
            user_config_path(&env),
            Some(xdg.join("allowlister/config.jsonc"))
        );
    }

    #[test]
    fn a_jsonc_only_user_config_is_discovered() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".allowlister.jsonc"), "{}").unwrap();
        let env = Env {
            home: Some(home.clone()),
            xdg_config_home: None,
            ..Env::default()
        };
        assert_eq!(
            user_config_path(&env),
            Some(home.join(".allowlister.jsonc"))
        );
    }

    #[test]
    fn write_targets_keep_an_existing_json_but_default_to_jsonc() {
        let dir = TempDir::new().unwrap();
        // No file yet: a new config is created with the .jsonc spelling.
        assert_eq!(
            local_config_path(dir.path()),
            dir.path().join(".allowlister.jsonc")
        );
        // An existing .json config keeps being updated in place.
        fs::write(dir.path().join(".allowlister.json"), "{}").unwrap();
        assert_eq!(
            local_config_path(dir.path()),
            dir.path().join(".allowlister.json")
        );
        // Once a .jsonc twin exists it wins.
        fs::write(dir.path().join(".allowlister.jsonc"), "{}").unwrap();
        assert_eq!(
            local_config_path(dir.path()),
            dir.path().join(".allowlister.jsonc")
        );

        let xdg = dir.path().join("xdg");
        let env = Env {
            home: None,
            xdg_config_home: Some(xdg.clone()),
            ..Env::default()
        };
        assert_eq!(
            default_user_config_path(&env),
            Some(xdg.join("allowlister/config.jsonc"))
        );
        fs::create_dir_all(xdg.join("allowlister")).unwrap();
        fs::write(xdg.join("allowlister/config.json"), "{}").unwrap();
        assert_eq!(
            default_user_config_path(&env),
            Some(xdg.join("allowlister/config.json"))
        );
    }

    #[test]
    fn project_walk_prefers_jsonc_and_ignores_the_json_twin() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join(".allowlister")).unwrap();
        fs::write(root.join(".allowlister.json"), "{}").unwrap();
        fs::write(root.join(".allowlister.jsonc"), "{}").unwrap();
        fs::write(root.join(".allowlister/config.json"), "{}").unwrap();

        let paths = project_config_paths(root);
        // The dotfile pair collapses to its .jsonc spelling; the directory
        // location (json only) is still picked up as a separate config. The
        // walk reverses the collected list, so within a level the directory
        // location precedes the dotfile (order only affects which rule a
        // reason cites, never the verdict).
        assert_eq!(
            paths,
            vec![
                root.join(".allowlister/config.json"),
                root.join(".allowlister.jsonc"),
            ]
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
