//! Platform-independent AgentDictate domain types and workflow state.

mod costs;
mod dictation;
mod protocol;
mod replacements;
mod settings;
mod snapshots;
mod textfmt;
mod workflow;

pub use costs::*;
pub use dictation::*;
pub use protocol::*;
pub use replacements::*;
pub use settings::*;
pub use snapshots::*;
pub use textfmt::*;
pub use workflow::*;
