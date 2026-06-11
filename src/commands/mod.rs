//! Subcommand implementations. Each module orchestrates the domain engine and
//! I/O boundaries for one CLI verb and returns a process exit code.

pub mod check;
pub mod config;
pub mod explain;
pub mod history;
pub mod hook;
pub mod init;
pub mod install;
pub mod profile;

use std::path::{Path, PathBuf};

/// Resolve an optional `--cwd` to a concrete directory, defaulting to the
/// process's current directory so project-config discovery can walk upward.
fn resolve_cwd(cwd: Option<&Path>) -> PathBuf {
    match cwd {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}
