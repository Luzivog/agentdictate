//! The Settings screen's state: the daemon's settings, the edits on their
//! way to it, and the controls that make them.

use std::{borrow::Cow, collections::VecDeque, sync::Arc};

use agentdictate_core::{
    KeepTranscripts, LANGUAGES, PasteShortcut, SettingChange, Settings, SettingsSnapshot,
};
use gpui::{Context, Entity, SharedString, Subscription, Window, prelude::*};
use gpui_component::{
    IndexPath,
    input::{InputEvent, InputState, TextareaState},
    select::{SearchableVec, SelectEvent, SelectItem, SelectState},
};

use crate::{SettingsRequest, UiActionError};

use super::{
    HotkeyCaptureSink, SettingsShell, SettingsSink,
    settings_shell::{Confirmed, ShortcutCapture},
};

/// A row on the Settings screen: where a change's "Saved ✓" or refusal shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingRow {
    ApiKey,
    Language,
    Shortcut,
    LowerSounds,
    KeepTranscripts,
    StartOnLogin,
    AboutYourWork,
    ExactMode,
    PasteMethod,
    StopAfter,
    HowMuchToLower,
    KeepAudio,
}

/// One choice in a Settings dropdown, holding the setting's own value.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SettingOption<T> {
    label: SharedString,
    value: T,
}

impl<T: Clone + PartialEq> SelectItem for SettingOption<T> {
    type Value = T;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

pub(super) type SettingSelect<T> = SelectState<SearchableVec<SettingOption<T>>>;

/// "Stop recording after" choices, in seconds; 0 never stops.
const STOP_AFTER_CHOICES: [(u32, &str); 4] = [
    (300, "5 minutes"),
    (900, "15 minutes"),
    (3_600, "1 hour"),
    (0, "Never"),
];

/// "How much to lower" choices, as the share of the volume that stays.
const LOWER_BY_CHOICES: [(u8, &str); 3] = [(50, "A little"), (15, "A lot"), (0, "Mute")];

const KEEP_TRANSCRIPTS_CHOICES: [(KeepTranscripts, &str); 3] = [
    (KeepTranscripts::Forever, "Forever"),
    (KeepTranscripts::Days30, "30 days"),
    (KeepTranscripts::Never, "Don't keep"),
];

const PASTE_METHOD_CHOICES: [(PasteShortcut, &str); 3] = [
    (PasteShortcut::Automatic, "Automatic (recommended)"),
    (PasteShortcut::Standard, "Ctrl+V"),
    (PasteShortcut::Terminal, "Ctrl+Shift+V (terminals)"),
];

/// The controls that keep their own state between renders.
#[derive(Clone)]
pub(super) struct SettingsControls {
    pub(super) api_key: Entity<InputState>,
    pub(super) language: Entity<SettingSelect<String>>,
    pub(super) keep_transcripts: Entity<SettingSelect<KeepTranscripts>>,
    pub(super) about_your_work: Entity<TextareaState>,
    pub(super) paste_method: Entity<SettingSelect<PasteShortcut>>,
    pub(super) stop_after: Entity<SettingSelect<u32>>,
    pub(super) how_much_to_lower: Entity<SettingSelect<u8>>,
}

/// Runs with the daemon's answer to one request: `Ok` once it saved it, or
/// why it was refused.
type RequestDone = Box<
    dyn FnOnce(&mut SettingsShell, Result<(), String>, &mut Window, &mut Context<SettingsShell>),
>;

struct PendingRequest {
    request: SettingsRequest,
    done: RequestDone,
}

pub(super) struct SettingsForm {
    /// The daemon's settings as of its last answer.
    pub(super) saved: SettingsSnapshot,
    /// Requests the daemon has not answered, oldest first. Only the first
    /// is in flight, so edits land in the order they were made.
    pending: VecDeque<PendingRequest>,
    sink: SettingsSink,
    pub(super) hotkey_capture: HotkeyCaptureSink,
    pub(super) controls: SettingsControls,
    pub(super) advanced_open: bool,
    /// The saved key's "Replace" was clicked, so the key field shows.
    pub(super) replacing_api_key: bool,
    pub(super) shortcut_capture: ShortcutCapture,
    /// A shorter Keep transcripts choice, waiting for Confirm delete because
    /// it deletes stored transcripts.
    pub(super) shorter_retention: Option<KeepTranscripts>,
    /// Why the last change on a row was refused, shown under that row.
    pub(super) error: Option<(SettingRow, String)>,
}

impl SettingsForm {
    /// Creates the controls from `saved` and subscribes to their edits.
    pub(super) fn new(
        saved: SettingsSnapshot,
        sink: SettingsSink,
        hotkey_capture: HotkeyCaptureSink,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) -> (Self, Vec<Subscription>) {
        let settings = &saved.values;
        let languages = LANGUAGES.map(|(code, name)| (code.to_owned(), name));
        let controls = SettingsControls {
            api_key: cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true)),
            language: setting_select(
                &languages,
                settings.language.clone(),
                String::clone,
                true,
                window,
                cx,
            ),
            keep_transcripts: setting_select(
                &KEEP_TRANSCRIPTS_CHOICES,
                settings.keep_transcripts,
                |keep| keep.as_str().to_owned(),
                false,
                window,
                cx,
            ),
            about_your_work: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("e.g. Product design, Figma, user interviews")
                    .default_value(settings.transcription_prompt.clone())
                    .auto_grow(2, 5)
                    .submit_on_enter(true)
            }),
            paste_method: setting_select(
                &PASTE_METHOD_CHOICES,
                settings.paste_shortcut,
                |shortcut| shortcut.as_str().to_owned(),
                false,
                window,
                cx,
            ),
            stop_after: setting_select(
                &STOP_AFTER_CHOICES,
                settings.max_recording_seconds,
                |seconds| match seconds % 60 {
                    0 => format!("{} minutes", seconds / 60),
                    _ => format!("{seconds} seconds"),
                },
                false,
                window,
                cx,
            ),
            how_much_to_lower: setting_select(
                &LOWER_BY_CHOICES,
                settings.audio_ducking_volume_percent,
                |percent| format!("To {percent}%"),
                false,
                window,
                cx,
            ),
        };
        let subscriptions = vec![
            on_choice(
                &controls.language,
                SettingRow::Language,
                SettingChange::Language,
                window,
                cx,
            ),
            cx.subscribe_in(
                &controls.keep_transcripts,
                window,
                |shell,
                 _,
                 event: &SelectEvent<SearchableVec<SettingOption<KeepTranscripts>>>,
                 window,
                 cx| {
                    if let SelectEvent::Confirm(Some(choice)) = event {
                        shell.choose_keep_transcripts(*choice, window, cx);
                    }
                },
            ),
            on_choice(
                &controls.paste_method,
                SettingRow::PasteMethod,
                SettingChange::PasteShortcut,
                window,
                cx,
            ),
            on_choice(
                &controls.stop_after,
                SettingRow::StopAfter,
                SettingChange::MaxRecordingSeconds,
                window,
                cx,
            ),
            on_choice(
                &controls.how_much_to_lower,
                SettingRow::HowMuchToLower,
                SettingChange::AudioDuckingVolumePercent,
                window,
                cx,
            ),
            cx.subscribe_in(
                &controls.about_your_work,
                window,
                |shell, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        shell.commit_about_your_work(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &controls.api_key,
                window,
                |shell, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        shell.save_api_key(window, cx);
                    }
                },
            ),
        ];
        let form = Self {
            saved,
            pending: VecDeque::new(),
            sink,
            hotkey_capture,
            controls,
            advanced_open: false,
            replacing_api_key: false,
            shortcut_capture: ShortcutCapture::Idle,
            shorter_retention: None,
            error: None,
        };
        (form, subscriptions)
    }

    /// The settings as the screen shows them: the daemon's, with the edits
    /// still on their way applied in order.
    pub(super) fn shown(&self) -> Cow<'_, Settings> {
        if self.pending.is_empty() {
            return Cow::Borrowed(&self.saved.values);
        }
        let mut settings = self.saved.values.clone();
        for pending in &self.pending {
            if let SettingsRequest::Change(change) = &pending.request {
                // A refused change shows its error when the daemon answers.
                let _ = change.clone().apply(&mut settings);
            }
        }
        Cow::Owned(settings)
    }

    /// Points every dropdown back at the shown value, after a refusal or a
    /// cancelled choice.
    pub(super) fn sync_choices(&self, window: &mut Window, cx: &mut Context<SettingsShell>) {
        let shown = self.shown().into_owned();
        let controls = &self.controls;
        select_value(&controls.language, &shown.language, window, cx);
        select_value(
            &controls.keep_transcripts,
            &shown.keep_transcripts,
            window,
            cx,
        );
        select_value(&controls.paste_method, &shown.paste_shortcut, window, cx);
        select_value(
            &controls.stop_after,
            &shown.max_recording_seconds,
            window,
            cx,
        );
        select_value(
            &controls.how_much_to_lower,
            &shown.audio_ducking_volume_percent,
            window,
            cx,
        );
    }
}

impl SettingsShell {
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn shown_settings_for_test(&self) -> Settings {
        self.settings.shown().into_owned()
    }

    /// Picks `choice` in the Keep transcripts dropdown, as a click would.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn choose_keep_transcripts_for_test(
        &self,
        choice: KeepTranscripts,
        cx: &mut Context<Self>,
    ) {
        self.settings
            .controls
            .keep_transcripts
            .update(cx, |_, cx| cx.emit(SelectEvent::Confirm(Some(choice))));
    }

    /// Applies `change`, made on `row`, right away: the screen shows it at
    /// once and the daemon saves it in the background. A refusal shows under
    /// the row, and the row goes back to the saved value.
    pub(super) fn change_setting(
        &mut self,
        row: SettingRow,
        change: SettingChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .settings
            .error
            .as_ref()
            .is_some_and(|(errored, _)| *errored == row)
        {
            self.settings.error = None;
        }
        self.send_settings_request(
            SettingsRequest::Change(change),
            window,
            cx,
            move |shell, result, window, cx| match result {
                // Toggles and choices show their new value; text says so.
                Ok(()) if row == SettingRow::AboutYourWork => {
                    shell.confirm(Confirmed::SettingSaved(row), cx);
                }
                Ok(()) => {}
                Err(error) => {
                    shell.settings.error = Some((row, format!("Not saved: {error}")));
                    shell.settings.sync_choices(window, cx);
                }
            },
        );
    }

    /// Queues `request` for the daemon and runs `done` with its answer.
    pub(super) fn send_settings_request(
        &mut self,
        request: SettingsRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.settings.pending.push_back(PendingRequest {
            request,
            done: Box::new(done),
        });
        if self.settings.pending.len() == 1 {
            self.send_first_settings_request(window, cx);
        }
        cx.notify();
    }

    fn send_first_settings_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = self.settings.pending.front() else {
            return;
        };
        let request = first.request.clone();
        let sink = Arc::clone(&self.settings.sink);
        let answer = cx.background_spawn(async move { sink(request) });
        cx.spawn_in(window, async move |shell, cx| {
            let answer = answer.await;
            // The window may have closed while the daemon answered.
            let _ = shell.update_in(cx, |shell, window, cx| {
                shell.finish_settings_request(answer, window, cx);
            });
        })
        .detach();
    }

    fn finish_settings_request(
        &mut self,
        answer: Result<SettingsSnapshot, UiActionError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(finished) = self.settings.pending.pop_front() else {
            return;
        };
        let result = match answer {
            Ok(saved) => {
                self.settings.saved = saved;
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        };
        (finished.done)(self, result, window, cx);
        self.send_first_settings_request(window, cx);
        cx.notify();
    }

    /// Saves "About your work" when its box loses focus or on Enter.
    fn commit_about_your_work(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.settings.controls.about_your_work.read(cx).value();
        let text = text.trim();
        if text == self.settings.shown().transcription_prompt {
            return;
        }
        let change = SettingChange::TranscriptionPrompt(text.to_owned());
        self.change_setting(SettingRow::AboutYourWork, change, window, cx);
    }
}

/// Applies a dropdown's choice as `change` from `row`.
fn on_choice<T: Clone + PartialEq + 'static>(
    select: &Entity<SettingSelect<T>>,
    row: SettingRow,
    change: fn(T) -> SettingChange,
    window: &Window,
    cx: &mut Context<SettingsShell>,
) -> Subscription {
    cx.subscribe_in(
        select,
        window,
        move |shell, _, event: &SelectEvent<SearchableVec<SettingOption<T>>>, window, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                shell.change_setting(row, change(value.clone()), window, cx);
            }
        },
    )
}

/// A dropdown of `choices` with `selected` chosen. A stored value that is not
/// a choice, such as one edited into config.json, is added with `label`.
fn setting_select<T: Clone + PartialEq + 'static>(
    choices: &[(T, &'static str)],
    selected: T,
    label: fn(&T) -> String,
    searchable: bool,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<SettingSelect<T>> {
    let mut options: Vec<_> = choices
        .iter()
        .map(|(value, label)| SettingOption {
            label: (*label).into(),
            value: value.clone(),
        })
        .collect();
    let index = options
        .iter()
        .position(|option| option.value == selected)
        .unwrap_or_else(|| {
            options.push(SettingOption {
                label: label(&selected).into(),
                value: selected,
            });
            options.len() - 1
        });
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(options),
            Some(IndexPath::default().row(index)),
            window,
            cx,
        )
        .searchable(searchable)
    })
}

fn select_value<T: Clone + PartialEq + 'static>(
    select: &Entity<SettingSelect<T>>,
    value: &T,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) {
    select.update(cx, |select, cx| {
        select.set_selected_value(value, window, cx)
    });
}

/// Whether switching Keep transcripts from `current` to `choice` deletes
/// transcripts that `current` keeps.
pub(super) fn deletes_transcripts(choice: KeepTranscripts, current: KeepTranscripts) -> bool {
    match (choice.limit(), current.limit()) {
        (Some(choice), Some(current)) => choice < current,
        (Some(_), None) => true,
        (None, _) => false,
    }
}
