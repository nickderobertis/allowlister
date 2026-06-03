//! Command-line surface (clap derive). All argument parsing, subcommands,
//! defaults, and value validation live here so the CLI is discoverable in one
//! place. `main` only forwards to [`Cli::dispatch`].

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::commands;
use crate::errors::Result;

/// Decide whether to allow, deny, or defer each shell command an AI coding
/// agent wants to run — by parsing the command, decomposing it into role-tagged
/// fragments, and matching each fragment against your rules.
#[derive(Debug, Parser)]
#[command(name = "allowlister", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run as a harness hook: read the hook JSON on stdin, write a decision on
    /// stdout. Only `claude-code` is implemented.
    Hook {
        /// The coding harness whose hook protocol to speak.
        #[arg(value_enum)]
        harness: Harness,
    },

    /// Evaluate a single command and print its verdict. Exit 0 for
    /// allow/defer, 2 for deny.
    Check {
        /// The shell command to evaluate.
        command: String,
        /// Directory used for project-config discovery (defaults to the current
        /// directory).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Emit a machine-readable JSON object instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Explain how a command is evaluated: config sources, fragments, and the
    /// per-fragment decision. The primary debugging tool.
    Explain {
        /// The shell command to explain.
        command: String,
        /// Directory used for project-config discovery (defaults to the current
        /// directory).
        #[arg(long)]
        cwd: Option<PathBuf>,
    },

    /// Write a starter config and print the settings snippet to register the
    /// hook.
    Init {
        /// Write the user-level config (the default).
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Write a project-level `.allowlister.json` in the current directory.
        #[arg(long)]
        local: bool,
    },
}

/// Supported coding harnesses.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Harness {
    /// Anthropic Claude Code (`PreToolUse` hook).
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Cursor (stub).
    Cursor,
    /// GitHub Copilot (stub).
    Copilot,
}

impl Cli {
    /// Run the parsed command, returning the process exit code.
    pub fn dispatch(self) -> Result<i32> {
        match self.command {
            Command::Hook { harness } => commands::hook::run(harness),
            Command::Check { command, cwd, json } => {
                commands::check::run(&command, cwd.as_deref(), json)
            }
            Command::Explain { command, cwd } => commands::explain::run(&command, cwd.as_deref()),
            Command::Init { global, local } => commands::init::run(global, local),
        }
    }
}
