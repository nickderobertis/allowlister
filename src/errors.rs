//! Typed errors for the application boundary.
//!
//! Domain logic does not surface these; the hook path in particular never
//! propagates an error into a deny. These are the failures `main` maps to a
//! process exit code: unknown CLI inputs, unwritable config locations, and I/O.

use std::path::PathBuf;

/// Errors that can reach `main`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("harness '{0}' is not yet implemented (supported: claude-code, cursor, goose)")]
    HarnessUnimplemented(String),

    #[error("could not locate a home/config directory to write to")]
    NoConfigHome,

    #[error("config already exists at {0}; refusing to overwrite (remove it or edit in place)")]
    ConfigExists(PathBuf),

    #[error("'{0}' is not a file or a built-in profile (try 'read-only' or 'repo-write')")]
    UnknownSource(String),

    // `origin` is a plain field (not `#[source]`) because it is a human label —
    // a file path or a profile name — not an underlying error to chain.
    #[error("{origin}: {message}")]
    InvalidConfig { origin: String, message: String },

    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for the application boundary.
pub type Result<T> = std::result::Result<T, Error>;
