//! Durable dictation orchestration, persistence, networking, and IPC.

pub use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, JobId, JobStage,
    ReplacementRule, ServerMessage, ServerMessageKind, Settings, TranscriptionProvider, Workflow,
    WorkflowPhase, WorkflowSignal,
};

mod ipc;
pub use ipc::{IpcClient, IpcError, IpcHandler, IpcServer};
mod external_dictation;
pub use external_dictation::{
    ExternalDictationImportOutcome, ExternalDictationReceipt, ExternalDictationSource,
};
mod fs;
pub use fs::write_atomic;
mod history;
pub use history::{HistoryCursor, HistoryEntry, HistoryMatch, HistoryPage, HistoryQuery};
mod history_search;
mod maintenance_priority;
pub use maintenance_priority::{HistoryIndexMaintenance, RecordingPriorityGuard};
mod ports;
pub use ports::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryStatus, ExternalError,
    HeadlessDeliveryGate, Recorder, RecordingJob, RecordingRequest, RuntimeError, RuntimeEvent,
    Transcriber, Transcript,
};
mod pricing;
mod recovery;
pub use recovery::RecoveryEntry;
mod runtime;
pub use runtime::Runtime;
mod schema;
pub(crate) use schema::{parse_timestamp, timestamp};
mod settings_store;
pub use settings_store::{load_settings, save_settings};
mod usage;
pub use usage::{UsageAggregate, UsageMetric, UsagePoint, UsageSummary, UsageWeek};
