//! Harness adapters.
//!
//! Each adapter translates a coding harness's stdin/stdout permission protocol
//! into the shared decision pipeline (`analyze` + `decide`). Only the I/O shape
//! differs between harnesses; the engine is identical. Claude Code is
//! implemented; Cursor and Copilot are stubs that document the extension point.

pub mod claude_code;
pub mod copilot;
pub mod cursor;
