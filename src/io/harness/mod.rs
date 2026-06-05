//! Harness adapters.
//!
//! Each adapter translates a coding harness's stdin/stdout permission protocol
//! into the shared decision pipeline (`analyze` + `decide`). Only the I/O shape
//! differs between harnesses; the engine is identical. Claude Code, Cursor, and
//! Codex are implemented; Copilot is a stub that documents the extension point.

pub mod claude_code;
pub mod codex;
pub mod copilot;
pub mod cursor;
