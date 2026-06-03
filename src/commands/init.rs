//! `allowlister init` — write a starter config and print the settings snippet
//! that registers the hook as the source of allow truth.

use std::fs;
use std::path::PathBuf;

use crate::errors::{Error, Result};
use crate::io::configfs::{self, Env};

/// A conservative starter ruleset: read-only inspection commands, common pipe
/// filters scoped to their role, and a couple of nuclear denies.
const STARTER_CONFIG: &str = r#"{
  "rules": [
    { "name": "ls",           "match": "ls*",                                    "action": "allow" },
    { "name": "pwd",          "match": "pwd",                                    "action": "allow" },
    { "name": "echo",         "match": "echo *",                                 "action": "allow" },
    { "name": "cat",          "match": "cat *",                                  "action": "allow" },

    { "name": "git read-only",
      "match": "git @(status|diff|log|show|branch|remote|rev-parse|describe)*",
      "action": "allow" },

    { "name": "pipe filters",
      "match": "@(head|tail|wc|grep|awk|sort|uniq|cut|sed|tr|jq|less|more) *",
      "action": "allow",
      "roles": ["pipe_filter", "substitution"] },

    { "name": "rm -rf — never",   "match": "rm -rf*",  "action": "deny" },

    { "name": "shell as pipe target — never",
      "argv": ["@(sh|bash|zsh|fish|dash|ksh)", "**"],
      "action": "deny",
      "roles": ["pipe_filter"] }
  ]
}
"#;

/// The recommended `~/.claude/settings.json` snippet: register the hook for the
/// `Bash` matcher, keep `permissions.allow` and `permissions.ask` empty (so the
/// hook is the source of allow truth), and add a tiny nuclear-pattern deny.
const SETTINGS_SNIPPET: &str = r#"{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Bash",
      "hooks": [{
        "type": "command",
        "command": "allowlister hook claude-code",
        "timeout": 10
      }]
    }]
  },
  "permissions": {
    "allow": [],
    "ask": [],
    "deny": [
      "Bash(rm -rf /)",
      "Bash(rm -rf ~)",
      "Bash(rm -rf /*)"
    ]
  }
}"#;

/// Write the starter config and print the settings snippet. `local` selects a
/// project `.allowlister.json`; otherwise the user-level config is written.
pub fn run(_global: bool, local: bool) -> Result<i32> {
    let path = if local {
        local_config_path()
    } else {
        global_config_path()?
    };

    if path.exists() {
        return Err(Error::ConfigExists(path));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| Error::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    fs::write(&path, STARTER_CONFIG).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;

    println!("Wrote starter config: {}", path.display());
    println!();
    print_hook_setup();

    Ok(0)
}

/// Print the `~/.claude/settings.json` snippet that registers the hook, plus the
/// one rule that matters: never broaden `permissions.allow`. Shared so that any
/// command which lands a fresh config (`init`, `install`) can hand the user the
/// same wiring instructions.
pub(crate) fn print_hook_setup() {
    println!("Add this to ~/.claude/settings.json (merge with any existing keys):");
    println!();
    println!("{SETTINGS_SNIPPET}");
    println!();
    println!("IMPORTANT: do NOT add \"Bash\" or \"Bash(*)\" to permissions.allow.");
    println!("A broad allow makes Claude Code skip its prompt on its own, which");
    println!("short-circuits the hook's per-fragment allow analysis — the whole");
    println!("point of allowlister. Let the hook be the source of allow truth.");
}

fn global_config_path() -> Result<PathBuf> {
    configfs::default_user_config_path(&Env::from_process()).ok_or(Error::NoConfigHome)
}

fn local_config_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    configfs::local_config_path(&cwd)
}
