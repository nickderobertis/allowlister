//! `allowlister hook <harness>` — dispatch to the harness adapter.

use crate::cli::Harness;
use crate::errors::Result;
use crate::io::harness::{claude_code, copilot, cursor, goose};

/// Run the hook adapter for the requested harness.
pub fn run(harness: Harness) -> Result<i32> {
    match harness {
        Harness::ClaudeCode => claude_code::run(),
        Harness::Cursor => cursor::run(),
        Harness::Goose => goose::run(),
        Harness::Copilot => copilot::run(),
    }
}
