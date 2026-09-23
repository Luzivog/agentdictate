//! Platform-independent AgentDictate domain types and workflow state.

mod costs;
mod dictation;
mod hotkey;
mod protocol;
mod settings;
mod snapshots;
mod textfmt;
mod workflow;

pub use costs::*;
pub use dictation::*;
pub use hotkey::*;
pub use protocol::*;
pub use settings::*;
pub use snapshots::*;
pub use textfmt::*;
pub use workflow::*;
