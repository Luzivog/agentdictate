use agentdictate_core::{
    AppSnapshot, HotkeyReadiness, ProcessingStage, WorkflowPhase, WorkflowSnapshot,
};

use crate::{HistoryViewModel, Route, WorkspaceViewModel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    Neutral,
    Starting,
    Recording,
    Processing,
    Success,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusViewModel {
    pub label: &'static str,
    pub detail: &'static str,
    pub tone: StatusTone,
    pub is_busy: bool,
}

impl From<WorkflowSnapshot> for StatusViewModel {
    fn from(snapshot: WorkflowSnapshot) -> Self {
        match snapshot.phase {
            WorkflowPhase::Ready => Self {
                label: "Ready",
                detail: "Press Ctrl + Space to start dictating",
                tone: StatusTone::Neutral,
                is_busy: false,
            },
            WorkflowPhase::Starting { .. } => Self {
                label: "Starting",
                detail: "Preparing your microphone",
                tone: StatusTone::Starting,
                is_busy: true,
            },
            WorkflowPhase::Recording { .. } => Self {
                label: "Recording",
                detail: "Listening to your microphone",
                tone: StatusTone::Recording,
                is_busy: true,
            },
            WorkflowPhase::Stopping { .. } => Self {
                label: "Finishing",
                detail: "Securing your recording",
                tone: StatusTone::Processing,
                is_busy: true,
            },
            WorkflowPhase::Processing { stage, .. } => match stage {
                ProcessingStage::Transcribing => Self {
                    label: "Transcribing",
                    detail: "Turning speech into text",
                    tone: StatusTone::Processing,
                    is_busy: true,
                },
                ProcessingStage::Cleaning => Self {
                    label: "Cleaning up",
                    detail: "Polishing your transcript",
                    tone: StatusTone::Processing,
                    is_busy: true,
                },
                ProcessingStage::ReadyToDeliver => Self {
                    label: "Ready to paste",
                    detail: "Your transcript is safe",
                    tone: StatusTone::Success,
                    is_busy: true,
                },
                ProcessingStage::Delivering => Self {
                    label: "Pasting",
                    detail: "Sending text to your active app",
                    tone: StatusTone::Success,
                    is_busy: true,
                },
            },
            WorkflowPhase::NeedsAttention { .. } => Self {
                label: "Needs attention",
                detail: "Your recording is safe and can be recovered",
                tone: StatusTone::Danger,
                is_busy: false,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyViewModel {
    pub label: &'static str,
    pub detail: String,
    pub tone: StatusTone,
    pub is_ready: bool,
}

impl From<HotkeyReadiness> for HotkeyViewModel {
    fn from(readiness: HotkeyReadiness) -> Self {
        match readiness {
            HotkeyReadiness::Starting => Self {
                label: "Checking shortcut",
                detail: "Waiting for the global shortcut listener".to_owned(),
                tone: StatusTone::Starting,
                is_ready: false,
            },
            HotkeyReadiness::Ready => Self {
                label: "Shortcut ready",
                detail: "Ctrl + Space is available".to_owned(),
                tone: StatusTone::Success,
                is_ready: true,
            },
            HotkeyReadiness::Unavailable { message } => Self {
                label: "Shortcut unavailable",
                detail: message,
                tone: StatusTone::Danger,
                is_ready: false,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationItemViewModel {
    pub route: Route,
    pub label: &'static str,
    pub is_active: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellViewModel {
    pub active_route: Route,
    pub navigation: [NavigationItemViewModel; 4],
    pub status: StatusViewModel,
    pub hotkey: HotkeyViewModel,
    pub workspace: WorkspaceViewModel,
    pub snapshot_sequence: Option<u64>,
    pub last_transcript: Option<String>,
}

impl ShellViewModel {
    pub fn from_snapshot(active_route: Route, snapshot: WorkflowSnapshot) -> Self {
        Self {
            active_route,
            navigation: Route::ALL.map(|route| NavigationItemViewModel {
                route,
                label: route.title(),
                is_active: route == active_route,
            }),
            status: snapshot.into(),
            hotkey: HotkeyReadiness::Starting.into(),
            workspace: WorkspaceViewModel::default(),
            snapshot_sequence: None,
            last_transcript: None,
        }
    }

    pub fn from_app_snapshot(active_route: Route, snapshot: AppSnapshot) -> Self {
        let AppSnapshot {
            sequence,
            workflow,
            hotkey,
            recoverable_count,
            last_transcript,
        } = snapshot;
        let mut model = Self::from_snapshot(active_route, workflow);
        model.hotkey = hotkey.into();
        model.workspace.history =
            HistoryViewModel::new(0, u64::try_from(recoverable_count).unwrap_or(u64::MAX));
        model.snapshot_sequence = Some(sequence);
        model.last_transcript = last_transcript;
        model
    }

    pub fn with_history(mut self, history: HistoryViewModel) -> Self {
        self.workspace.history = history;
        self
    }

    pub fn with_workspace(mut self, workspace: WorkspaceViewModel) -> Self {
        self.workspace = workspace;
        self
    }

    pub fn select_route(&mut self, route: Route) {
        self.active_route = route;
        for item in &mut self.navigation {
            item.is_active = item.route == route;
        }
    }
}
