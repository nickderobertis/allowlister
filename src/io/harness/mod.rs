//! Harness adapters.
//!
//! Each adapter translates a coding harness's stdin/stdout permission protocol
//! into the shared decision pipeline (`analyze` + `decide`). Only the I/O shape
//! differs between harnesses; the engine is identical. Claude Code, Cursor,
//! Codex, and Copilot are all implemented.

pub mod claude_code;
pub mod codex;
pub mod copilot;
pub mod crush;
pub mod cursor;
