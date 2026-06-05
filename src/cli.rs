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
    /// stdout. `claude-code`, `cursor`, and `copilot` are implemented.
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

    /// Set allowlister up: write a config from a chosen ruleset and register the
    /// hook in the selected harness's settings. Runs an interactive flow on a
    /// terminal, or non-interactively from these flags.
    Init {
        /// Write the user-level config (the default).
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Write a project-level `.allowlister.json` in the current directory.
        #[arg(long)]
        local: bool,
        /// Starting ruleset: a built-in (`starter`, `read-only`, `repo-write`)
        /// or a path to an allowlist JSON file. Defaults to `starter`.
        #[arg(long, value_name = "SOURCE")]
        profile: Option<String>,
        /// Which coding harness to wire the hook into (`claude-code`, `cursor`,
        /// or `copilot`). Defaults to `claude-code`. Run `init` again per harness
        /// to set up more than one.
        #[arg(long, value_enum, default_value = "claude-code")]
        harness: Harness,
        /// Register the hook in the selected harness's settings (the default).
        #[arg(long, overrides_with = "no_hooks")]
        hooks: bool,
        /// Do not touch the harness settings; just print the snippet to wire by
        /// hand.
        #[arg(long = "no-hooks")]
        no_hooks: bool,
        /// Walk through the setup step by step, reading answers from stdin.
        /// Engaged automatically when stdin is a terminal.
        #[arg(long, short)]
        interactive: bool,
        /// Accept the defaults without prompting (the scriptable path).
        #[arg(long, short = 'y', conflicts_with = "interactive")]
        yes: bool,
        /// Overwrite an existing config instead of refusing.
        #[arg(long)]
        force: bool,
    },

    /// Merge an allowlist into your config, creating it if absent. Re-running
    /// never duplicates rules, so it is safe to layer profiles.
    Install {
        /// A built-in profile name (`read-only` or `repo-write`) or a path to an
        /// allowlist JSON file.
        source: String,
        /// Merge into the user-level config (the default).
        #[arg(long, conflicts_with_all = ["local", "output"])]
        global: bool,
        /// Merge into a project `.allowlister.json` in the current directory.
        #[arg(long, conflicts_with = "output")]
        local: bool,
        /// Merge into an explicit file path instead of a discovered config.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
}

/// Supported coding harnesses.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Harness {
    /// Anthropic Claude Code (`PreToolUse` hook).
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Cursor (`beforeShellExecution` hook).
    Cursor,
    /// GitHub Copilot CLI (`preToolUse` hook).
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
            Command::Init {
                global,
                local,
                profile,
                harness,
                hooks,
                no_hooks,
                interactive,
                yes,
                force,
            } => {
                // `--no-hooks` and `--hooks` override each other (last wins); if
                // neither is given, leave the choice unset so the interactive
                // flow can ask and the non-interactive default (on) applies.
                let hooks = if no_hooks {
                    Some(false)
                } else if hooks {
                    Some(true)
                } else {
                    None
                };
                commands::init::run(commands::init::InitArgs {
                    global,
                    local,
                    profile,
                    harness,
                    hooks,
                    interactive,
                    yes,
                    force,
                })
            }
            Command::Install {
                source,
                global,
                local,
                output,
            } => commands::install::run(&source, global, local, output.as_deref()),
        }
    }
}
