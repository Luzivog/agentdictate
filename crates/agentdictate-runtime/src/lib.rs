//! Durable dictation orchestration, persistence, networking, and IPC.

pub use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, JobId, JobStage, ServerMessage,
    ServerMessageKind, Settings, Workflow, WorkflowPhase, WorkflowSignal,
};

mod ipc;
pub use ipc::{IpcClient, IpcError, IpcHandler, IpcServer, SOCKET_FILE_NAME};
mod fs;
pub use fs::write_atomic;
mod history;
pub use history::DeletedTranscript;
mod legacy_replacements;
pub use legacy_replacements::{RetiredReplacement, RetiredReplacementOutcome};
mod migrations;
mod observer;
pub use observer::DatabaseObserver;
mod ports;
pub use ports::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryMethod,
    DeliveryStatus, ExternalError, HeadlessDeliveryGate, JobFailure, ObservedFocus, Recorder,
    RecordingJob, RecordingRequest, RuntimeError, StoredTranscript, Transcript,
    TranscriptionOutcome,
};
mod recovery;
mod retention;
mod runtime;
pub use runtime::Runtime;
mod schema;
pub(crate) use schema::{parse_timestamp, timestamp};
mod settings_store;
pub use settings_store::{load_settings, save_settings};
mod startup_cleanup;
pub use startup_cleanup::FinishedJobCleanup;
mod usage;
