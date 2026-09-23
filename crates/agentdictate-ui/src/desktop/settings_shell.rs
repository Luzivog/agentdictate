use std::{collections::HashSet, time::Duration};

use gpui::{AppContext, Context, Entity, ScrollHandle, Task, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::{Route, ShellViewModel, ThemeTokens, WorkspaceAction, WorkspaceActionSink};

use super::{
    HotkeyCaptureSink, SettingsShell, SettingsSink,
    history_action_lane::HistoryActionLane,
    settings_form::{SettingRow, SettingsForm},
    words_actions::{FixWordEditor, WordsUiState},
};

/// The Dictation shortcut control. The daemon captures the shortcut from the
/// keyboards themselves, so it keeps the physical key on any layout.
pub(super) enum ShortcutCapture {
    Idle,
    /// Waiting for the daemon's answer; dropping the task ignores it.
    Listening {
        _answer: Task<()>,
    },
    /// Why the last capture ended without a shortcut.
    Failed(String),
}

impl ShortcutCapture {
    pub(super) const fn is_listening(&self) -> bool {
        matches!(self, Self::Listening { .. })
    }

    pub(super) fn failure(&self) -> Option<String> {
        match self {
            Self::Failed(reason) => Some(reason.clone()),
            Self::Idle | Self::Listening { .. } => None,
        }
    }
}

pub(super) struct WorkspaceActionState {
    pub(super) sink: WorkspaceActionSink,
    pub(super) in_flight: bool,
    pub(super) history_lane: HistoryActionLane,
}

#[derive(Clone, Debug, Default)]
pub(super) struct RouteUiEntry {
    pub(super) feedback: Option<String>,
    pub(super) scroll: ScrollHandle,
}

pub(super) struct RouteUiState {
    pub(super) entries: [RouteUiEntry; Route::ALL.len()],
    pub(super) history_search_input: Entity<InputState>,
    pub(super) pending_destructive_action: Option<WorkspaceAction>,
    pub(super) overview_recent_expanded: bool,
    /// History rows showing their whole transcript.
    pub(super) expanded_transcripts: HashSet<i64>,
    /// History's "Fix a word" editor, open under one expanded transcript.
    pub(super) fix_word: Option<FixWordEditor>,
    pub(super) words: WordsUiState,
    pub(super) confirmation: Option<Confirmation>,
}

/// How long a "✓" confirmation shows after its action succeeds.
pub(super) const CONFIRMATION_DURATION: Duration = Duration::from_millis(1_500);

/// What a short "✓" confirmation is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Confirmed {
    /// Transcript `id` was copied: its Copy buttons read "Copied ✓".
    Copied(i64),
    /// The Words screen saved a change and shows "Saved ✓".
    WordsSaved,
    /// "Fix a word" on transcript `id` added to Words.
    AddedToWords(i64),
    /// A Settings text row saved its value and shows "Saved ✓".
    SettingSaved(SettingRow),
}

/// The confirmation on screen, with the timer that clears it. Replacing it
/// drops, and so cancels, the previous timer.
pub(super) struct Confirmation {
    subject: Confirmed,
    _reset: Task<()>,
}

impl RouteUiState {
    pub(super) fn entry(&self, route: Route) -> &RouteUiEntry {
        &self.entries[route_index(route)]
    }

    pub(super) fn entry_mut(&mut self, route: Route) -> &mut RouteUiEntry {
        &mut self.entries[route_index(route)]
    }
}

pub(super) const fn route_index(route: Route) -> usize {
    match route {
        Route::Home => 0,
        Route::History => 1,
        Route::Words => 2,
        Route::Settings => 3,
    }
}

impl SettingsShell {
    /// Builds the settings window's shell around the daemon's settings and the
    /// sinks that send settings, shortcut captures and workspace actions back
    /// to it.
    pub fn new(
        model: ShellViewModel,
        settings: agentdictate_core::SettingsSnapshot,
        settings_sink: SettingsSink,
        hotkey_capture: HotkeyCaptureSink,
        workspace_action_sink: WorkspaceActionSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_history_search = model.workspace.history.search.clone();
        let history_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search every transcript")
                .default_value(initial_history_search)
        });
        let (settings, mut subscriptions) =
            SettingsForm::new(settings, settings_sink, hotkey_capture, window, cx);
        subscriptions.push(cx.subscribe(
            &history_search_input,
            |shell, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    shell.submit_history_search(query, cx);
                }
            },
        ));

        let (words, words_subscriptions) = WordsUiState::new(window, cx);
        subscriptions.extend(words_subscriptions);

        let routes = RouteUiState {
            entries: std::array::from_fn(|_| RouteUiEntry::default()),
            history_search_input,
            pending_destructive_action: None,
            overview_recent_expanded: false,
            expanded_transcripts: HashSet::new(),
            fix_word: None,
            words,
            confirmation: None,
        };

        Self {
            model,
            theme: ThemeTokens::default(),
            settings,
            workspace_actions: WorkspaceActionState {
                sink: workspace_action_sink,
                in_flight: false,
                history_lane: HistoryActionLane::default(),
            },
            routes,
            _subscriptions: subscriptions,
        }
    }

    pub const fn active_route(&self) -> Route {
        self.model.active_route
    }

    pub const fn view_model(&self) -> &ShellViewModel {
        &self.model
    }

    /// Expands a collapsed History row to its whole transcript, or collapses
    /// it along with its "Fix a word" editor.
    pub(super) fn toggle_transcript(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.routes.expanded_transcripts.remove(&id) {
            if self
                .routes
                .fix_word
                .as_ref()
                .is_some_and(|editor| editor.form.transcript_id == id)
            {
                self.routes.fix_word = None;
            }
        } else {
            self.routes.expanded_transcripts.insert(id);
        }
        cx.notify();
    }

    /// Shows the confirmation for `subject` for [`CONFIRMATION_DURATION`].
    pub(super) fn confirm(&mut self, subject: Confirmed, cx: &mut Context<Self>) {
        let reset = cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(CONFIRMATION_DURATION).await;
            let _ = shell.update(cx, |shell, cx| {
                shell.routes.confirmation = None;
                cx.notify();
            });
        });
        self.routes.confirmation = Some(Confirmation {
            subject,
            _reset: reset,
        });
    }

    /// What the confirmation on screen is about, if one is showing.
    pub(super) fn confirmed(&self) -> Option<Confirmed> {
        self.routes
            .confirmation
            .as_ref()
            .map(|confirmation| confirmation.subject)
    }

    /// The transcript whose Copy buttons read "Copied ✓", if any.
    pub(super) fn copied_transcript(&self) -> Option<i64> {
        match self.confirmed()? {
            Confirmed::Copied(id) => Some(id),
            Confirmed::WordsSaved | Confirmed::AddedToWords(_) | Confirmed::SettingSaved(_) => None,
        }
    }

    pub(super) fn select_route(&mut self, route: Route, cx: &mut Context<Self>) {
        let previous_route = self.model.active_route;
        self.model.select_route(route);
        self.routes.pending_destructive_action = None;
        self.routes.words.reset();
        self.clear_route_feedback_for(previous_route);
        self.settings.error = None;
        cx.notify();
    }
}
