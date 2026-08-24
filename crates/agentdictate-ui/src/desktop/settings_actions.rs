use gpui::{Context, Window};

#[cfg(feature = "test-support")]
use agentdictate_core::TranscriptionProvider;

use crate::{Route, SettingsDraft};

use super::SettingsShell;

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

impl SettingsShell {
    pub(super) fn sync_model_catalog_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let catalog = self.model.workspace.model_catalog.clone();
        if catalog == self.settings.applied_model_catalog {
            return;
        }
        if let Some(form) = self.settings.form.clone() {
            form.sync_model_catalog(&catalog, window, cx);
        }
        self.settings.applied_model_catalog = catalog;
    }

    pub(super) fn cleanup_model_selection_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.settings.form.clone() else {
            return;
        };
        form.sync_dependent_options(&self.model.workspace.model_catalog, window, cx);
        self.recompute_settings_dirty(cx);
        cx.notify();
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn select_transcription_provider_for_test(
        &mut self,
        provider: TranscriptionProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.settings.form.clone() else {
            return;
        };
        form.transcription_provider.update(cx, |state, cx| {
            state.set_selected_value(&provider.as_str().to_owned(), window, cx);
        });
        self.recompute_settings_dirty(cx);
        self.clear_route_feedback();
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
        let Some(form) = self.settings.form.clone() else {
            return;
        };
        form.cleanup_model.update(cx, |state, cx| {
            state.set_selected_value(&model_id.to_owned(), window, cx);
        });
        self.cleanup_model_selection_changed(window, cx);
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn selected_cleanup_reasoning_for_test(&self, cx: &gpui::App) -> String {
        self.settings.form.as_ref().map_or_else(
            || self.settings.current.cleanup_reasoning_effort.clone(),
            |form| {
                form.cleanup_reasoning_effort
                    .read(cx)
                    .selected_value()
                    .cloned()
                    .unwrap_or_else(|| form.draft.cleanup_reasoning_effort.clone())
            },
        )
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn settings_draft_for_test(&self, cx: &gpui::App) -> SettingsDraft {
        self.settings.form.as_ref().map_or_else(
            || SettingsDraft::from(&self.settings.current),
            |form| form.snapshot(cx),
        )
    }

    pub(super) fn request_model_catalog_refresh(&mut self) {
        if !self.settings_commands.has_api_key {
            return;
        }
        let Some(sink) = &self.settings_commands.command_sink else {
            return;
        };
        let request_id = self.settings_commands.next_request_id;
        self.settings_commands.next_request_id += 1;
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
        let Some(form) = self.settings.form.as_ref() else {
            return;
        };
        let draft = form.snapshot(cx);
        match draft.apply_to(&self.settings.baseline) {
            Ok(settings) => {
                let Some(sink) = &self.settings_commands.command_sink else {
                    self.accept_saved_settings(settings);
                    self.set_route_feedback("Saved");
                    return;
                };
                let request_id = self.settings_commands.next_request_id;
                self.settings_commands.next_request_id += 1;
                match sink(agentdictate_core::ClientCommand::update_settings(
                    request_id, &settings,
                )) {
                    Ok(()) => {
                        self.accept_saved_settings(settings);
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

    fn accept_saved_settings(&mut self, settings: agentdictate_core::Settings) {
        if let Some(form) = self.settings.form.as_mut() {
            form.draft = SettingsDraft::from(&settings);
        }
        self.settings.current = settings.clone();
        self.settings.baseline = settings;
        self.settings.dirty = false;
    }

    pub(super) fn recompute_settings_dirty(&mut self, cx: &Context<Self>) {
        self.settings.dirty = self
            .settings
            .form
            .as_ref()
            .is_some_and(|form| form.snapshot(cx).is_dirty_against(&self.settings.baseline));
    }

    pub(super) fn update_settings_draft(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut SettingsDraft),
    ) {
        let Some(form) = self.settings.form.as_mut() else {
            return;
        };
        update(&mut form.draft);
        self.recompute_settings_dirty(cx);
        self.clear_route_feedback();
        cx.notify();
    }

    pub(super) fn discard_settings_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let baseline = self.settings.baseline.clone();
        let catalog = self.model.workspace.model_catalog.clone();
        self.settings.current = baseline.clone();
        if let Some(form) = self.settings.form.as_mut() {
            form.reset(&baseline, &catalog, window, cx);
        }
        self.settings.dirty = false;
        self.settings.shortcut_capture_active = false;
        self.settings.shortcut_capture_error = None;
        self.clear_route_feedback();
        cx.notify();
    }

    pub(super) fn begin_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        self.settings.shortcut_capture_active = true;
        self.settings.shortcut_capture_error = None;
        cx.notify();
    }

    pub(super) fn cancel_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        self.settings.shortcut_capture_active = false;
        self.settings.shortcut_capture_error = None;
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
                if let Some(form) = self.settings.form.as_mut() {
                    form.draft.hotkey = shortcut;
                }
                self.settings.shortcut_capture_active = false;
                self.settings.shortcut_capture_error = None;
                self.recompute_settings_dirty(cx);
            }
            Err(error) => self.settings.shortcut_capture_error = Some(error),
        }
        cx.notify();
    }

    pub(super) fn save_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.settings_commands.api_key_input.clone() else {
            return;
        };
        let api_key = input.read(cx).value().trim().to_owned();
        if api_key.is_empty() {
            self.settings_commands.api_key_feedback = Some("Paste an API key first".to_owned());
            return;
        }
        let Some(sink) = &self.settings_commands.command_sink else {
            self.settings_commands.api_key_feedback =
                Some("API key saving is not connected".to_owned());
            return;
        };
        let request_id = self.settings_commands.next_request_id;
        self.settings_commands.next_request_id += 1;
        self.settings_commands.api_key_feedback = Some(
            match sink(agentdictate_core::ClientCommand::set_api_key(
                request_id, api_key,
            )) {
                Ok(()) => {
                    self.settings_commands.has_api_key = true;
                    input.update(cx, |input, cx| {
                        input.set_value(String::new(), window, cx);
                    });
                    "API key saved".to_owned()
                }
                Err(error) => format!("Could not save: {error}"),
            },
        );
    }
}
