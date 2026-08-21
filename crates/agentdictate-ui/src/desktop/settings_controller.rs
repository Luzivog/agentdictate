use gpui::{Context, Entity, SharedString, Window, prelude::*};
use gpui_component::{
    IndexPath,
    input::InputState,
    select::{SearchableVec, SelectItem, SelectState},
};
use std::sync::Arc;

use crate::{ModelCatalogViewModel, Route, SettingsDraft, WorkspaceAction, WorkspaceViewModel};

use super::{
    SettingsShell,
    settings_page::{
        cleanup_style_options, language_options, paste_shortcut_options, recording_mode_options,
    },
    workspace_with_currency,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SettingOption {
    label: SharedString,
    value: String,
}

impl SettingOption {
    pub(super) fn new(label: impl Into<SharedString>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl SelectItem for SettingOption {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

pub(super) type SettingSelectState = SelectState<SearchableVec<SettingOption>>;

#[derive(Clone)]
pub(super) struct SettingsEditorState {
    pub(super) transcription_model: Entity<SettingSelectState>,
    pub(super) custom_transcription_model: Entity<InputState>,
    pub(super) language: Entity<SettingSelectState>,
    pub(super) transcription_prompt: Entity<InputState>,
    pub(super) cleanup_model: Entity<SettingSelectState>,
    pub(super) custom_cleanup_model: Entity<InputState>,
    pub(super) cleanup_reasoning_effort: Entity<SettingSelectState>,
    pub(super) cleanup_style: Entity<SettingSelectState>,
    pub(super) cleanup_prompt: Entity<InputState>,
    pub(super) recording_mode: Entity<SettingSelectState>,
    pub(super) max_recording_seconds: Entity<InputState>,
    pub(super) audio_ducking_volume_percent: Entity<InputState>,
    pub(super) paste_shortcut: Entity<SettingSelectState>,
}

impl SettingsEditorState {
    pub(super) fn new(
        settings: &agentdictate_core::Settings,
        model_catalog: &ModelCatalogViewModel,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) -> Self {
        let draft = SettingsDraft::from(settings);
        let cleanup_model = settings.active_cleanup_model();
        let cleanup_reasoning_effort = model_catalog
            .normalized_reasoning_effort(cleanup_model, &draft.cleanup_reasoning_effort);
        Self {
            transcription_model: settings_select(
                transcription_model_options(model_catalog),
                &draft.transcription_model,
                true,
                window,
                cx,
            ),
            custom_transcription_model: settings_input(
                draft.custom_transcription_model,
                "Custom OpenAI model",
                window,
                cx,
            ),
            language: settings_select(language_options(), &draft.language, true, window, cx),
            transcription_prompt: settings_text_area(
                draft.transcription_prompt,
                "Names and technical context",
                2,
                5,
                window,
                cx,
            ),
            cleanup_model: settings_select(
                cleanup_model_options(model_catalog),
                &draft.cleanup_model,
                true,
                window,
                cx,
            ),
            custom_cleanup_model: settings_input(
                draft.custom_cleanup_model,
                "Custom cleanup model",
                window,
                cx,
            ),
            cleanup_reasoning_effort: settings_select(
                reasoning_effort_options(model_catalog, cleanup_model),
                &cleanup_reasoning_effort,
                false,
                window,
                cx,
            ),
            cleanup_style: settings_select(
                cleanup_style_options(),
                &draft.cleanup_style,
                false,
                window,
                cx,
            ),
            cleanup_prompt: settings_text_area(
                draft.cleanup_prompt,
                "Cleanup instructions",
                3,
                6,
                window,
                cx,
            ),
            recording_mode: settings_select(
                recording_mode_options(),
                &draft.recording_mode,
                false,
                window,
                cx,
            ),
            max_recording_seconds: settings_number_input(
                draft.max_recording_seconds,
                "300",
                u64::from(u32::MAX),
                window,
                cx,
            ),
            audio_ducking_volume_percent: settings_number_input(
                draft.audio_ducking_volume_percent,
                "15",
                100,
                window,
                cx,
            ),
            paste_shortcut: settings_select(
                paste_shortcut_options(),
                &draft.paste_shortcut,
                false,
                window,
                cx,
            ),
        }
    }

    fn draft(
        &self,
        current: &agentdictate_core::Settings,
        cx: &Context<SettingsShell>,
    ) -> SettingsDraft {
        SettingsDraft {
            transcription_model: selected_setting(
                &self.transcription_model,
                &current.transcription_model,
                cx,
            ),
            custom_transcription_model: self
                .custom_transcription_model
                .read(cx)
                .value()
                .to_string(),
            language: selected_setting(&self.language, &current.language, cx),
            transcription_prompt: self.transcription_prompt.read(cx).value().to_string(),
            cleanup_enabled: current.cleanup_enabled,
            cleanup_model: selected_setting(&self.cleanup_model, &current.cleanup_model, cx),
            custom_cleanup_model: self.custom_cleanup_model.read(cx).value().to_string(),
            cleanup_reasoning_effort: selected_setting(
                &self.cleanup_reasoning_effort,
                &current.cleanup_reasoning_effort,
                cx,
            ),
            cleanup_style: selected_setting(&self.cleanup_style, &current.cleanup_style, cx),
            cleanup_prompt: self.cleanup_prompt.read(cx).value().to_string(),
            hotkey: current.hotkey.clone(),
            recording_mode: selected_setting(&self.recording_mode, &current.recording_mode, cx),
            max_recording_seconds: self.max_recording_seconds.read(cx).value().to_string(),
            audio_ducking_enabled: current.audio_ducking_enabled,
            audio_ducking_volume_percent: self
                .audio_ducking_volume_percent
                .read(cx)
                .value()
                .to_string(),
            paste_shortcut: selected_setting(&self.paste_shortcut, &current.paste_shortcut, cx),
            start_on_login: current.start_on_login,
            save_history: current.save_history,
            preserve_temp_audio: current.preserve_temp_audio,
        }
    }

    pub(super) fn inputs(&self) -> Vec<Entity<InputState>> {
        vec![
            self.custom_transcription_model.clone(),
            self.transcription_prompt.clone(),
            self.custom_cleanup_model.clone(),
            self.cleanup_prompt.clone(),
            self.max_recording_seconds.clone(),
            self.audio_ducking_volume_percent.clone(),
        ]
    }

    pub(super) fn selects(&self) -> Vec<Entity<SettingSelectState>> {
        vec![
            self.transcription_model.clone(),
            self.language.clone(),
            self.cleanup_model.clone(),
            self.cleanup_reasoning_effort.clone(),
            self.cleanup_style.clone(),
            self.recording_mode.clone(),
            self.paste_shortcut.clone(),
        ]
    }

    fn reset(
        &self,
        settings: &agentdictate_core::Settings,
        catalog: &ModelCatalogViewModel,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) {
        let draft = SettingsDraft::from(settings);
        let input_values = [
            (
                self.custom_transcription_model.clone(),
                draft.custom_transcription_model,
            ),
            (
                self.transcription_prompt.clone(),
                draft.transcription_prompt,
            ),
            (
                self.custom_cleanup_model.clone(),
                draft.custom_cleanup_model,
            ),
            (self.cleanup_prompt.clone(), draft.cleanup_prompt),
            (
                self.max_recording_seconds.clone(),
                draft.max_recording_seconds,
            ),
            (
                self.audio_ducking_volume_percent.clone(),
                draft.audio_ducking_volume_percent,
            ),
        ];
        for (input, value) in input_values {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let select_values = [
            (self.transcription_model.clone(), draft.transcription_model),
            (self.language.clone(), draft.language),
            (self.cleanup_model.clone(), draft.cleanup_model),
            (self.cleanup_style.clone(), draft.cleanup_style),
            (self.recording_mode.clone(), draft.recording_mode),
            (self.paste_shortcut.clone(), draft.paste_shortcut),
        ];
        for (select, value) in select_values {
            select.update(cx, |select, cx| {
                select.set_selected_value(&value, window, cx);
            });
        }
        let cleanup_model = settings.active_cleanup_model();
        let reasoning_effort =
            catalog.normalized_reasoning_effort(cleanup_model, &draft.cleanup_reasoning_effort);
        replace_select_options(
            &self.cleanup_reasoning_effort,
            reasoning_effort_options(catalog, cleanup_model),
            &reasoning_effort,
            window,
            cx,
        );
    }

    fn sync_model_catalog(
        &self,
        catalog: &ModelCatalogViewModel,
        settings: &agentdictate_core::Settings,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) {
        let transcription_model =
            selected_setting(&self.transcription_model, &settings.transcription_model, cx);
        let cleanup_model = selected_setting(&self.cleanup_model, &settings.cleanup_model, cx);
        let reasoning_effort = selected_setting(
            &self.cleanup_reasoning_effort,
            &settings.cleanup_reasoning_effort,
            cx,
        );

        replace_select_options(
            &self.transcription_model,
            transcription_model_options(catalog),
            &transcription_model,
            window,
            cx,
        );
        replace_select_options(
            &self.cleanup_model,
            cleanup_model_options(catalog),
            &cleanup_model,
            window,
            cx,
        );
        self.sync_reasoning_options(catalog, settings, &reasoning_effort, window, cx);
    }

    fn sync_reasoning_options(
        &self,
        catalog: &ModelCatalogViewModel,
        settings: &agentdictate_core::Settings,
        selected_reasoning: &str,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) {
        let selected_cleanup = selected_setting(&self.cleanup_model, &settings.cleanup_model, cx);
        let cleanup_model_id = if selected_cleanup == "Custom" {
            self.custom_cleanup_model.read(cx).value().to_string()
        } else {
            selected_cleanup
        };
        let selected_reasoning =
            catalog.normalized_reasoning_effort(&cleanup_model_id, selected_reasoning);
        replace_select_options(
            &self.cleanup_reasoning_effort,
            reasoning_effort_options(catalog, &cleanup_model_id),
            &selected_reasoning,
            window,
            cx,
        );
    }
}

fn settings_input(
    value: String,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
    })
}

fn settings_text_area(
    value: String,
    placeholder: &'static str,
    min_rows: usize,
    max_rows: usize,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
            .auto_grow(min_rows, max_rows)
    })
}

fn settings_number_input(
    value: String,
    placeholder: &'static str,
    maximum: u64,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
            .validate(move |text, _| {
                text.is_empty()
                    || (text.bytes().all(|byte| byte.is_ascii_digit())
                        && text.parse::<u64>().is_ok_and(|value| value <= maximum))
            })
    })
}

fn settings_select(
    mut options: Vec<SettingOption>,
    selected: &str,
    searchable: bool,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<SettingSelectState> {
    let selected_index = options.iter().position(|option| option.value == selected);
    let selected_index = selected_index.unwrap_or_else(|| {
        options.push(SettingOption::new(selected.to_owned(), selected.to_owned()));
        options.len() - 1
    });
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(options),
            Some(IndexPath::default().row(selected_index)),
            window,
            cx,
        )
        .searchable(searchable)
    })
}

fn replace_select_options(
    state: &Entity<SettingSelectState>,
    mut options: Vec<SettingOption>,
    selected: &str,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) {
    if !options.iter().any(|option| option.value == selected) {
        options.push(SettingOption::new(
            format!("{selected} — current value"),
            selected.to_owned(),
        ));
    }
    state.update(cx, |state, cx| {
        state.set_items(SearchableVec::new(options), window, cx);
        state.set_selected_value(&selected.to_owned(), window, cx);
    });
}

pub(super) fn selected_setting(
    state: &Entity<SettingSelectState>,
    fallback: &str,
    cx: &Context<SettingsShell>,
) -> String {
    state
        .read(cx)
        .selected_value()
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

fn captured_shortcut(keystroke: &gpui::Keystroke) -> Result<String, String> {
    if keystroke.modifiers.function {
        return Err("The Function modifier is not supported".to_owned());
    }

    let normalized = keystroke.key.to_ascii_lowercase();
    let key = match normalized.as_str() {
        "space" => "Space".to_owned(),
        "tab" => "Tab".to_owned(),
        "enter" | "return" => "Enter".to_owned(),
        key if matches!(
            key,
            "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12"
        ) =>
        {
            key.to_ascii_uppercase()
        }
        key if key.len() == 1 && key.bytes().all(|byte| byte.is_ascii_alphanumeric()) => {
            key.to_ascii_uppercase()
        }
        _ => return Err("Use a letter, number, Space, Tab, Enter, or F1–F12".to_owned()),
    };

    let is_function_key = normalized.starts_with('f')
        && normalized[1..]
            .parse::<u8>()
            .is_ok_and(|number| (1..=12).contains(&number));
    if !keystroke.modifiers.modified() && !is_function_key {
        return Err("Add Ctrl, Alt, Shift, or Super to this key".to_owned());
    }

    let mut parts = Vec::with_capacity(5);
    if keystroke.modifiers.control {
        parts.push("Ctrl".to_owned());
    }
    if keystroke.modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if keystroke.modifiers.shift {
        parts.push("Shift".to_owned());
    }
    if keystroke.modifiers.platform {
        parts.push("Super".to_owned());
    }
    parts.push(key);
    Ok(parts.join("+"))
}

fn transcription_model_options(catalog: &ModelCatalogViewModel) -> Vec<SettingOption> {
    catalog
        .transcription_models
        .iter()
        .map(|model| SettingOption::new(model.label.clone(), model.id.clone()))
        .chain(std::iter::once(SettingOption::new("Custom…", "Custom")))
        .collect()
}

fn cleanup_model_options(catalog: &ModelCatalogViewModel) -> Vec<SettingOption> {
    catalog
        .cleanup_models
        .iter()
        .map(|model| SettingOption::new(model.label.clone(), model.id.clone()))
        .chain(std::iter::once(SettingOption::new("Custom…", "Custom")))
        .collect()
}

fn reasoning_effort_options(
    catalog: &ModelCatalogViewModel,
    cleanup_model: &str,
) -> Vec<SettingOption> {
    catalog
        .reasoning_options_for(cleanup_model)
        .into_iter()
        .map(|effort| SettingOption::new(effort.label, effort.value))
        .collect()
}

impl SettingsShell {
    /// Atomically replaces the workspace projection received from the daemon.
    pub fn apply_workspace_update(
        &mut self,
        workspace: WorkspaceViewModel,
        cx: &mut Context<Self>,
    ) {
        self.model.workspace = workspace_with_currency(workspace, &self.settings.currency);
        cx.notify();
    }

    pub(super) fn sync_model_catalog_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let catalog = self.model.workspace.model_catalog.clone();
        if catalog == self.applied_model_catalog {
            return;
        }
        if let Some(editor) = self.settings_editor.clone() {
            editor.sync_model_catalog(&catalog, &self.settings, window, cx);
        }
        self.applied_model_catalog = catalog;
    }

    pub(super) fn cleanup_model_selection_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.settings_editor.clone() else {
            return;
        };
        let selected_reasoning = selected_setting(
            &editor.cleanup_reasoning_effort,
            &self.settings.cleanup_reasoning_effort,
            cx,
        );
        editor.sync_reasoning_options(
            &self.model.workspace.model_catalog,
            &self.settings,
            &selected_reasoning,
            window,
            cx,
        );
        self.recompute_settings_dirty(cx);
        cx.notify();
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn select_cleanup_model_for_test(
        &mut self,
        model_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.settings_editor.clone() else {
            return;
        };
        editor.cleanup_model.update(cx, |state, cx| {
            state.set_selected_value(&model_id.to_owned(), window, cx);
        });
        self.cleanup_model_selection_changed(window, cx);
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn selected_cleanup_reasoning_for_test(&self, cx: &gpui::App) -> String {
        self.settings_editor.as_ref().map_or_else(
            || self.settings.cleanup_reasoning_effort.clone(),
            |editor| {
                editor
                    .cleanup_reasoning_effort
                    .read(cx)
                    .selected_value()
                    .cloned()
                    .unwrap_or_else(|| self.settings.cleanup_reasoning_effort.clone())
            },
        )
    }

    pub(super) fn request_model_catalog_refresh(&mut self) {
        if !self.has_api_key {
            return;
        }
        let Some(sink) = &self.command_sink else {
            return;
        };
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        if let Err(error) = sink(agentdictate_core::ClientCommand::refresh_model_catalog(
            request_id,
        )) {
            self.set_route_feedback_for(
                Route::Settings,
                format!("Could not refresh models: {error}"),
            );
        }
    }

    pub(super) fn save_settings_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_editor.clone() else {
            return;
        };
        match editor.draft(&self.settings, cx).apply_to(&self.settings) {
            Ok(settings) => {
                let Some(sink) = &self.command_sink else {
                    self.settings = settings.clone();
                    self.settings_baseline = settings;
                    self.settings_dirty = false;
                    self.set_route_feedback("Saved");
                    return;
                };
                let request_id = self.next_request_id;
                self.next_request_id += 1;
                match sink(agentdictate_core::ClientCommand::update_settings(
                    request_id, &settings,
                )) {
                    Ok(()) => {
                        self.settings = settings.clone();
                        self.settings_baseline = settings;
                        self.settings_dirty = false;
                        self.set_route_feedback("Saved");
                    }
                    Err(error) => {
                        self.set_route_feedback(format!("Could not save: {error}"));
                    }
                }
            }
            Err(error) => self.set_route_feedback(error.to_string()),
        }
    }

    pub(super) const fn settings_is_dirty(&self) -> bool {
        self.settings_dirty
    }

    pub(super) fn recompute_settings_dirty(&mut self, cx: &Context<Self>) {
        self.settings_dirty = self.settings_editor.as_ref().is_some_and(|editor| {
            editor
                .draft(&self.settings, cx)
                .is_dirty_against(&self.settings_baseline)
        });
    }

    pub(super) fn update_settings_draft(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut SettingsDraft),
    ) {
        let Some(editor) = self.settings_editor.as_ref() else {
            return;
        };
        let mut draft = editor.draft(&self.settings, cx);
        update(&mut draft);
        self.settings.cleanup_enabled = draft.cleanup_enabled;
        self.settings.audio_ducking_enabled = draft.audio_ducking_enabled;
        self.settings.start_on_login = draft.start_on_login;
        self.settings.save_history = draft.save_history;
        self.settings.preserve_temp_audio = draft.preserve_temp_audio;
        self.recompute_settings_dirty(cx);
        self.clear_route_feedback();
        cx.notify();
    }

    pub(super) fn discard_settings_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = self.settings_baseline.clone();
        if let Some(editor) = self.settings_editor.clone() {
            editor.reset(
                &self.settings_baseline,
                &self.model.workspace.model_catalog,
                window,
                cx,
            );
        }
        self.settings_dirty = false;
        self.shortcut_capture_active = false;
        self.shortcut_capture_error = None;
        self.clear_route_feedback();
        cx.notify();
    }

    pub(super) fn begin_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        self.shortcut_capture_active = true;
        self.shortcut_capture_error = None;
        cx.notify();
    }

    pub(super) fn cancel_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        self.shortcut_capture_active = false;
        self.shortcut_capture_error = None;
        cx.notify();
    }

    pub(super) fn capture_shortcut(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        if keystroke.key.eq_ignore_ascii_case("escape")
            && keystroke.modifiers == gpui::Modifiers::none()
        {
            self.cancel_shortcut_capture(cx);
            return;
        }
        match captured_shortcut(keystroke) {
            Ok(shortcut) => {
                self.settings.hotkey = shortcut;
                self.shortcut_capture_active = false;
                self.shortcut_capture_error = None;
                self.recompute_settings_dirty(cx);
            }
            Err(error) => self.shortcut_capture_error = Some(error),
        }
        cx.notify();
    }

    pub(super) fn save_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.api_key_input.clone() else {
            return;
        };
        let api_key = input.read(cx).value().trim().to_owned();
        if api_key.is_empty() {
            self.api_key_feedback = Some("Paste an API key first".to_owned());
            return;
        }
        let Some(sink) = &self.command_sink else {
            self.api_key_feedback = Some("API key saving is not connected".to_owned());
            return;
        };
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.api_key_feedback = Some(
            match sink(agentdictate_core::ClientCommand::set_api_key(
                request_id, api_key,
            )) {
                Ok(()) => {
                    self.has_api_key = true;
                    input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
                    "API key saved".to_owned()
                }
                Err(error) => format!("Could not save: {error}"),
            },
        );
    }

    pub(super) fn emit_workspace_action(
        &mut self,
        action: WorkspaceAction,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            action,
            WorkspaceAction::SearchHistory { .. } | WorkspaceAction::LoadMoreHistory
        ) {
            self.emit_history_action(action, cx);
            return;
        }
        if self.workspace_action_in_flight {
            self.set_route_feedback("Another action is still running");
            return;
        }
        let Some(sink) = &self.workspace_action_sink else {
            self.set_route_feedback("This action is not connected yet");
            return;
        };
        let feedback_route = self.model.active_route;
        let sink = Arc::clone(sink);
        let closes_editor = matches!(
            action,
            WorkspaceAction::CreateReplacement { .. } | WorkspaceAction::UpdateReplacement { .. }
        );
        self.pending_destructive_action = None;
        self.workspace_action_in_flight = true;
        self.clear_route_feedback_for(feedback_route);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell
                    .update(cx, |shell, cx| {
                        shell.workspace_action_in_flight = false;
                        match result {
                            Ok(workspace) => {
                                shell.model.workspace =
                                    workspace_with_currency(workspace, &shell.settings.currency);
                                shell.clear_route_feedback_for(feedback_route);
                                if closes_editor {
                                    shell.replacement_editor = None;
                                }
                            }
                            Err(error) => {
                                shell.set_route_feedback_for(
                                    feedback_route,
                                    format!("Could not complete action: {error}"),
                                );
                            }
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// History reads have their own latest-wins lane. A slow search must not
    /// block Copy, replacement edits, or other workspace mutations, and an
    /// obsolete response must never replace a newer query.
    fn emit_history_action(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        if !self.history_action_lane.schedule(&action) {
            return;
        }
        let Some(sink) = &self.workspace_action_sink else {
            self.set_route_feedback_for(Route::History, "History search is not connected yet");
            return;
        };
        let sink = Arc::clone(sink);
        self.clear_route_feedback_for(Route::History);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell
                    .update(cx, |shell, cx| {
                        let completion = shell.history_action_lane.complete();
                        if completion.apply_result {
                            match result {
                                Ok(workspace) => {
                                    shell.model.workspace.history = workspace.history;
                                    shell.clear_route_feedback_for(Route::History);
                                }
                                Err(error) => shell.set_route_feedback_for(
                                    Route::History,
                                    format!("Could not search history: {error}"),
                                ),
                            }
                        }
                        if let Some(query) = completion.next_search {
                            shell.emit_history_action(WorkspaceAction::SearchHistory { query }, cx);
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    pub(super) fn submit_history_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.emit_history_action(WorkspaceAction::SearchHistory { query }, cx);
    }

    pub(super) fn request_destructive_action(
        &mut self,
        action: WorkspaceAction,
        cx: &mut Context<Self>,
    ) {
        if self.pending_destructive_action.as_ref() == Some(&action) {
            self.pending_destructive_action = None;
            self.emit_workspace_action(action, cx);
        } else {
            self.pending_destructive_action = Some(action);
            self.set_route_feedback(
                "Click Confirm delete to permanently remove this item, or continue elsewhere to cancel."
                    .to_owned(),
            );
        }
    }
}
