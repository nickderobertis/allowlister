//! Cursor adapter — not yet implemented.
//!
//! Cursor's permission protocol differs from Claude Code's only in I/O shape;
//! the decision pipeline (`analyze` + `decide`) is shared. Implementing this
//! adapter means parsing Cursor's request envelope and emitting its decision
//! envelope, then delegating to [`crate::domain::evaluate`].

use crate::errors::{Error, Result};

/// Placeholder entry point. Returns an unimplemented error until the Cursor
/// protocol is wired up.
pub fn run() -> Result<i32> {
    Err(Error::HarnessUnimplemented("cursor".to_string()))
}
