//! Command-line surface (clap derive). All argument parsing, subcommands,
//! defaults, and value validation live here so the CLI is discoverable in one
//! place. `main` only forwards to [`Cli::dispatch`].

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

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
    /// stdout. See `--help` for the supported harnesses.
    Hook {
        /// The coding harness whose hook protocol to speak.
        #[arg(value_enum)]
        harness: Harness,
    },

    /// Evaluate a single command — or, with `--tool`, a non-shell tool call —
    /// and print its verdict. Exit 0 for allow/defer, 2 for deny.
    #[command(group(ArgGroup::new("target").required(true).args(["command", "tool"])))]
    Check {
        /// The shell command to evaluate. Omit when using `--tool`.
        command: Option<String>,
        /// Directory used for project-config discovery (defaults to the current
        /// directory).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Emit a machine-readable JSON object instead of human text.
        #[arg(long)]
        json: bool,
        /// Evaluate a non-shell tool call instead: a capability
        /// (read/write/edit/glob/grep/web_fetch/web_search/mcp) or a raw tool
        /// name such as `mcp__github__create_issue`.
        #[arg(long)]
        tool: Option<String>,
        /// A canonical tool parameter as `key=value` (repeatable), e.g.
        /// `--param path=/repo/src/main.rs`. Requires `--tool`.
        #[arg(long = "param", value_name = "KEY=VALUE", requires = "tool")]
        param: Vec<String>,
        /// Raw tool-input JSON object for server-defined parameters and
        /// `jsonpath` rules, e.g. `--raw '{"owner":"acme"}'`. Requires `--tool`.
        #[arg(long, value_name = "JSON", requires = "tool")]
        raw: Option<String>,
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
        /// Write a project-level `.allowlister.jsonc` in the current directory
        /// (updates an existing `.allowlister.json` in place).
        #[arg(long)]
        local: bool,
        /// Starting ruleset: a built-in (`starter`, `read-only`, `repo-write`)
        /// or a path to an allowlist JSON/JSONC file. Defaults to `starter`.
        #[arg(long, value_name = "SOURCE")]
        profile: Option<String>,
        /// Which coding harness to wire the hook into; see `--help` for the
        /// choices. Defaults to `claude-code`. Run `init` again per harness
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
        /// Record a local history of evaluated commands to help you refine your
        /// allowlist (off by default; you are asked when running interactively).
        #[arg(long, overrides_with = "no_history")]
        history: bool,
        /// Do not record command history (the default).
        #[arg(long = "no-history")]
        no_history: bool,
    },

    /// Inspect recorded usage — how often each command, and each parsed
    /// subcommand, was allowed, asked, denied, or deferred to the harness — so
    /// you can refine your allowlist from real behavior. Recording is opt-in
    /// (turn it on during `init`). With no subcommand, prints the frequency
    /// report.
    History {
        #[command(subcommand)]
        action: Option<HistoryAction>,
        /// Which table to show: per-subcommand `fragments` (the default),
        /// `programs` (collapsed to the leading program), or whole `commands`.
        #[arg(long, value_enum, default_value = "fragments")]
        view: ViewArg,
        /// Show only rows that had this verdict, sorted by it. `defer` surfaces
        /// the commands that fell through to the harness's own prompt.
        #[arg(long, value_enum)]
        verdict: Option<VerdictArg>,
        /// Show at most this many rows.
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Expand each subcommand/program row into a per-project verdict
        /// breakdown (the whole-command view is not split by project).
        #[arg(long = "by-project")]
        by_project: bool,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Merge an allowlist into your config, creating it if absent. Re-running
    /// never duplicates rules, so it is safe to layer profiles.
    Install {
        /// A built-in profile name (`read-only` or `repo-write`) or a path to an
        /// allowlist JSON/JSONC file.
        source: String,
        /// Merge into the user-level config (the default).
        #[arg(long, conflicts_with_all = ["local", "output"])]
        global: bool,
        /// Merge into a project `.allowlister.jsonc` in the current directory
        /// (updates an existing `.allowlister.json` in place).
        #[arg(long, conflicts_with = "output")]
        local: bool,
        /// Merge into an explicit file path instead of a discovered config.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },

    /// Manage your allowlist configuration directly: add or remove a single
    /// rule, or show the effective merged config and the source of each rule.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Run the bundled sample dynamic approval plugin.
    #[command(hide = true)]
    ExamplePlugin,
}

/// Subcommands for `config`.
#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// Add a single rule to a config file (creating it if absent). Rules are
    /// de-duplicated by name, like `install`, so re-adding the same name is a
    /// no-op rather than a duplicate.
    Add(ConfigAddArgs),

    /// Remove the rule with the given `name` from a config file, leaving the
    /// surrounding rules, comments, and formatting intact.
    Remove {
        /// The `name` of the rule to remove.
        name: String,
        /// Edit the user-level config (the default).
        #[arg(long, conflicts_with_all = ["local", "output"])]
        global: bool,
        /// Edit a project `.allowlister.jsonc` in the current directory.
        #[arg(long, conflicts_with = "output")]
        local: bool,
        /// Edit an explicit file path instead of a discovered config.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },

    /// Show the effective configuration — every active rule and the file it came
    /// from. Merges the user and project configs by default; `--global` or
    /// `--local` narrows to a single scope.
    Show {
        /// Directory used for project-config discovery (defaults to the current
        /// directory).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Show only the user-level config.
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Show only the project-level config(s).
        #[arg(long)]
        local: bool,
        /// Emit a machine-readable JSON object instead of human text.
        #[arg(long)]
        json: bool,
    },
}

/// Flags for `config add`. Exactly one of `--match`, `--argv`, or `--tool`
/// selects what the rule matches; the rest refine it.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("matcher").required(true).args(["match_pattern", "argv", "tool"])))]
struct ConfigAddArgs {
    /// The rule's name (shown in decision reasons and used to de-duplicate).
    #[arg(long)]
    name: Option<String>,
    /// What a match does: allow, deny, or ask (surface for approval).
    #[arg(long, value_enum, default_value = "allow")]
    action: ActionArg,
    /// Match the whole joined command against this pattern (a shell rule).
    #[arg(long = "match", value_name = "PATTERN")]
    match_pattern: Option<String>,
    /// Match argv element-by-element; repeat per element (a shell rule). A
    /// trailing `**` element allows any number of further arguments.
    #[arg(long, value_name = "ARG")]
    argv: Vec<String>,
    /// Match a non-shell tool call: a capability
    /// (read/write/edit/glob/grep/web_fetch/web_search/mcp) or a raw tool-name
    /// glob such as `mcp__github__*`.
    #[arg(long)]
    tool: Option<String>,
    /// How patterns are interpreted: glob (the default), regex, or literal.
    #[arg(long, value_enum)]
    kind: Option<KindArg>,
    /// Restrict a shell rule to these roles (repeatable), e.g.
    /// standalone/pipe_filter/pipe_source/substitution.
    #[arg(long = "role", value_name = "ROLE")]
    roles: Vec<String>,
    /// Constrain a canonical tool parameter as `key=glob` (repeatable), e.g.
    /// `--param path=/repo/**`. Requires `--tool`.
    #[arg(long = "param", value_name = "KEY=GLOB")]
    params: Vec<String>,
    /// Constrain a raw tool-input field as `jsonpath=glob` (repeatable), e.g.
    /// `--jsonpath owner=acme`. Requires `--tool`.
    #[arg(long = "jsonpath", value_name = "PATH=GLOB")]
    jsonpath: Vec<String>,
    /// A human description stored alongside the rule.
    #[arg(long)]
    description: Option<String>,
    /// Add to the user-level config (the default).
    #[arg(long, conflicts_with_all = ["local", "output"])]
    global: bool,
    /// Add to a project `.allowlister.jsonc` in the current directory.
    #[arg(long, conflicts_with = "output")]
    local: bool,
    /// Add to an explicit file path instead of a discovered config.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
}

/// A rule action, for `config add`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ActionArg {
    Allow,
    Deny,
    Ask,
}

impl ActionArg {
    fn as_str(self) -> &'static str {
        match self {
            ActionArg::Allow => "allow",
            ActionArg::Deny => "deny",
            ActionArg::Ask => "ask",
        }
    }
}

/// A match kind, for `config add`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum KindArg {
    Glob,
    Regex,
    Literal,
}

impl KindArg {
    fn as_str(self) -> &'static str {
        match self {
            KindArg::Glob => "glob",
            KindArg::Regex => "regex",
            KindArg::Literal => "literal",
        }
    }
}

/// Maintenance subcommands for `history`.
#[derive(Debug, Subcommand)]
enum HistoryAction {
    /// List the recent recorded events (a bounded, time-ordered window).
    Recent {
        /// Show at most this many events.
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// Keep only events whose project tag contains this substring.
        #[arg(long)]
        project: Option<String>,
        /// Keep only events from this harness.
        #[arg(long)]
        harness: Option<String>,
        /// Keep only events with this verdict.
        #[arg(long, value_enum)]
        verdict: Option<VerdictArg>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Fold the recent-events log into the durable summary now.
    Compact,
    /// Delete all recorded history.
    Clear {
        /// Skip the confirmation prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Print where history data is stored.
    Path,
}

/// The frequency table `history` shows by default.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ViewArg {
    /// Per parsed subcommand (the full subcommand string).
    Fragments,
    /// Per program (the subcommand's leading token).
    Programs,
    /// Per whole command line.
    Commands,
}

/// A verdict, for `history` filters.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum VerdictArg {
    Allow,
    Deny,
    Ask,
    Defer,
}

impl HistoryAction {
    /// Map the clap subcommand onto the command layer's action type.
    fn into_command(self) -> commands::history::Action {
        match self {
            HistoryAction::Recent {
                top,
                project,
                harness,
                verdict,
                json,
            } => commands::history::Action::Recent(commands::history::RecentArgs {
                top,
                project,
                harness,
                verdict: verdict.map(Into::into),
                json,
            }),
            HistoryAction::Compact => commands::history::Action::Compact,
            HistoryAction::Clear { yes } => commands::history::Action::Clear { yes },
            HistoryAction::Path => commands::history::Action::Path,
        }
    }
}

impl From<ViewArg> for commands::history::View {
    fn from(arg: ViewArg) -> Self {
        match arg {
            ViewArg::Fragments => commands::history::View::Fragments,
            ViewArg::Programs => commands::history::View::Programs,
            ViewArg::Commands => commands::history::View::Commands,
        }
    }
}

impl From<VerdictArg> for crate::domain::Verdict {
    fn from(arg: VerdictArg) -> Self {
        match arg {
            VerdictArg::Allow => crate::domain::Verdict::Allow,
            VerdictArg::Deny => crate::domain::Verdict::Deny,
            VerdictArg::Ask => crate::domain::Verdict::Ask,
            VerdictArg::Defer => crate::domain::Verdict::Defer,
        }
    }
}

/// Supported coding harnesses.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Harness {
    /// Anthropic Claude Code (`PreToolUse` hook).
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Cursor (`beforeShellExecution` hook).
    Cursor,
    /// OpenAI Codex CLI (`PreToolUse` hook).
    Codex,
    /// Crush (`PreToolUse` hook).
    Crush,
    /// Qwen Code (`PreToolUse` hook).
    Qwen,
    /// Goose (`PreToolUse` hook).
    Goose,
    /// OpenCode (`tool.execute.before` plugin shim).
    #[value(name = "opencode")]
    OpenCode,
    /// GitHub Copilot CLI (`preToolUse` hook).
    Copilot,
}

impl Cli {
    /// Run the parsed command, returning the process exit code.
    pub fn dispatch(self) -> Result<i32> {
        match self.command {
            Command::Hook { harness } => commands::hook::run(harness),
            Command::Check {
                command,
                cwd,
                json,
                tool,
                param,
                raw,
            } => commands::check::run(commands::check::CheckArgs {
                command: command.as_deref(),
                cwd: cwd.as_deref(),
                json,
                tool: tool.as_deref(),
                params: &param,
                raw: raw.as_deref(),
            }),
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
                history,
                no_history,
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
                // History mirrors the same tri-state, but its non-interactive
                // default is off, so an unset choice (`None`) records nothing.
                let history = if no_history {
                    Some(false)
                } else if history {
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
                    history,
                })
            }
            Command::History {
                action,
                view,
                verdict,
                top,
                by_project,
                json,
            } => commands::history::run(
                action.map(HistoryAction::into_command),
                commands::history::ShowArgs {
                    view: view.into(),
                    verdict: verdict.map(Into::into),
                    top,
                    by_project,
                    json,
                },
            ),
            Command::Install {
                source,
                global,
                local,
                output,
            } => commands::install::run(&source, global, local, output.as_deref()),
            Command::Config { action } => match action {
                ConfigAction::Add(args) => commands::config::add(commands::config::AddArgs {
                    name: args.name,
                    action: args.action.as_str().to_string(),
                    match_pattern: args.match_pattern,
                    argv: args.argv,
                    tool: args.tool,
                    kind: args.kind.map(|k| k.as_str().to_string()),
                    roles: args.roles,
                    params: args.params,
                    jsonpath: args.jsonpath,
                    description: args.description,
                    local: args.local,
                    output: args.output,
                }),
                ConfigAction::Remove {
                    name,
                    global: _,
                    local,
                    output,
                } => commands::config::remove(&name, local, output.as_deref()),
                ConfigAction::Show {
                    cwd,
                    global,
                    local,
                    json,
                } => commands::config::show(commands::config::ShowArgs {
                    cwd,
                    global,
                    local,
                    json,
                }),
            },
            Command::ExamplePlugin => commands::example_plugin::run(),
        }
    }
}
