//! Copilot adapter — not yet implemented.
//!
//! Like the Cursor adapter, only the request/response envelope differs; the
//! decision pipeline is shared. Implementing this means translating Copilot's
//! tool-permission protocol and delegating to [`crate::domain::evaluate`].

use crate::errors::{Error, Result};

/// Placeholder entry point. Returns an unimplemented error until the Copilot
/// protocol is wired up.
pub fn run() -> Result<i32> {
    Err(Error::HarnessUnimplemented("copilot".to_string()))
}
