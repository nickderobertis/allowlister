//! Pure decision engine: no filesystem, process, or terminal I/O.
//!
//! - [`analyzer`] turns a command string into role-tagged fragments.
//! - [`rule`] holds compiled rules and matchers.
//! - [`decision`] composes per-fragment verdicts into one overall verdict.
//!
//! Keep this layer free of I/O so it stays trivially testable and reusable.

mod glob;

pub mod analyzer;
pub mod decision;
pub mod rule;
pub mod toolcall;

pub use analyzer::{analyze, Analysis, Fragment, RedirClass, Redirection, Role};
pub use decision::{
    decide, evaluate, evaluate_tool_call, DecisionResult, FragmentDecision, Verdict,
};
pub use rule::{Action, Grant, MatchKind, RedirPolicy, Rule, ToolRule};
pub use toolcall::{Capability, NormalizedParams, ParamKey, ToolCall};
