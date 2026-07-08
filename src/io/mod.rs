//! I/O boundaries: filesystem config discovery and harness stdin/stdout
//! adapters. Pure decision logic lives in [`crate::domain`]; this layer is
//! where the program touches the outside world.

pub mod claude_settings;
pub mod configfs;
pub mod harness;
pub mod history;
pub mod hooks;
pub mod plugins;
pub mod project;
pub mod toolpath;
