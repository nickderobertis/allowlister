//! Structural allow/deny/defer engine for AI coding-agent shell commands.
//!
//! Instead of classifying commands as safe or unsafe in the abstract,
//! allowlister classifies each command by the structural *role* it plays in the
//! shell expression it appears in (standalone, pipe source/filter, subshell,
//! substitution). The bash AST is walked once into a flat list of role-tagged
//! fragments; the rule engine then evaluates each `(argv, role, redirections)`
//! tuple independently. Composition (pipes, `&&`, substitutions) falls out for
//! free, and a rule can say, e.g., "`head` is fine as a pipe filter but not as a
//! standalone file reader."
//!
//! # Layout
//! - [`domain`] — the pure engine: [`domain::analyze`], [`domain::Rule`],
//!   [`domain::decide`]. No I/O.
//! - [`config`] — the JSON rule schema, validation, and user/project merge.
//! - `io` (crate-private) — config discovery and harness stdin/stdout adapters.
//! - `commands` / `cli` (crate-private) — the CLI surface.

pub mod config;
pub mod domain;
pub mod errors;

mod cli;
mod commands;
mod io;

/// Parse process arguments and run the CLI, returning the process exit code.
///
/// `clap` handles `--help`/`--version` and argument errors by printing and
/// exiting directly, so those paths do not return here.
pub fn run() -> errors::Result<i32> {
    use clap::Parser;
    cli::Cli::parse().dispatch()
}
