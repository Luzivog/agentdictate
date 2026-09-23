//! Durable dictation orchestration, persistence, networking, and IPC.

pub use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, JobId, JobStage,
    ReplacementRule, ServerMessage, ServerMessageKind, Settings, Workflow, WorkflowPhase,
    WorkflowSignal,
};

mod ipc;
pub use ipc::{IpcClient, IpcError, IpcHandler, IpcServer};
mod fs;
pub use fs::write_atomic;
mod history;
mod history_search;
mod maintenance_priority;
pub use maintenance_priority::{HistoryIndexMaintenance, RecordingPriorityGuard};
mod ports;
pub use ports::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryMethod,
    DeliveryStatus, ExternalError, HeadlessDeliveryGate, Recorder, RecordingJob, RecordingRequest,
    RuntimeError, Transcriber, Transcript,
};
mod recovery;
mod runtime;
pub use runtime::Runtime;
mod schema;
pub(crate) use schema::{parse_timestamp, timestamp};
mod settings_store;
pub use settings_store::{load_settings, save_settings};
mod startup_cleanup;
pub use startup_cleanup::FinishedJobCleanup;
mod usage;
