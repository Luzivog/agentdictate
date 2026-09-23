use std::sync::Arc;

use agentdictate_core::HotkeyCaptureOutcome;
use gpui::{AppContext, Context, Window};

use crate::{SettingsDraft, UiActionError};

use super::{SettingsShell, settings_shell::ShortcutCapture};

impl SettingsShell {
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn settings_draft_for_test(&self, cx: &gpui::App) -> SettingsDraft {
        self.settings.form.snapshot(cx)
    }

    /// Sends the whole of `settings` to the daemon, which validates, saves and
    /// applies them.
    pub(super) fn send_settings(
        &mut self,
        settings: &agentdictate_core::Settings,
    ) -> Result<(), UiActionError> {
        let request_id = self.settings_commands.next_request_id;
        self.settings_commands.next_request_id += 1;
        (self.settings_commands.command_sink)(agentdictate_core::ClientCommand::update_settings(
            request_id, settings,
        ))
    }

    pub(super) fn save_settings_editor(&mut self, cx: &mut Context<Self>) {
        let draft = self.settings.form.snapshot(cx);
        match draft.apply_to(&self.settings.baseline) {
            Ok(settings) => match self.send_settings(&settings) {
                Ok(()) => {
                    self.accept_saved_settings(settings);
                    self.set_route_feedback("Saved");
                }
                Err(error) => {
                    self.set_route_feedback(format!("Could not save: {error}"));
                }
            },
            Err(error) => self.set_route_feedback(error.to_string()),
        }
    }

    fn accept_saved_settings(&mut self, settings: agentdictate_core::Settings) {
        self.settings.form.draft = SettingsDraft::from(&settings);
        self.settings.current = settings.clone();
        self.settings.baseline = settings;
        self.settings.dirty = false;
    }

    pub(super) fn recompute_settings_dirty(&mut self, cx: &Context<Self>) {
        self.settings.dirty = self
            .settings
            .form
            .snapshot(cx)
            .is_dirty_against(&self.settings.baseline);
    }

    pub(super) fn update_settings_draft(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut SettingsDraft),
    ) {
        update(&mut self.settings.form.draft);
        self.recompute_settings_dirty(cx);
        self.clear_route_feedback();
        cx.notify();
    }

    pub(super) fn discard_settings_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let baseline = self.settings.baseline.clone();
        self.settings.current = baseline.clone();
        self.settings.form.reset(&baseline, window, cx);
        self.settings.dirty = false;
        self.cancel_shortcut_capture(cx);
        self.clear_route_feedback();
        cx.notify();
    }

    /// Asks the daemon for the next shortcut pressed on any keyboard. The
    /// window drops keyboard focus so the chord cannot also edit a field.
    pub(super) fn begin_shortcut_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.blur(cx);
        let capture = Arc::clone(&self.settings_commands.hotkey_capture);
        let answer = cx.background_spawn(async move { capture() });
        let answer = cx.spawn(async move |shell, cx| {
            let result = answer.await;
            let _ = shell.update(cx, |shell, cx| shell.finish_shortcut_capture(result, cx));
        });
        self.settings.shortcut_capture = ShortcutCapture::Listening { _answer: answer };
        cx.notify();
    }

    /// Stops a capture in progress and clears any capture message.
    pub(super) fn cancel_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        let previous =
            std::mem::replace(&mut self.settings.shortcut_capture, ShortcutCapture::Idle);
        if previous.is_listening() {
            let request_id = self.settings_commands.next_request_id;
            self.settings_commands.next_request_id += 1;
            let command = agentdictate_core::ClientCommand::cancel_hotkey_capture(request_id);
            if let Err(error) = (self.settings_commands.command_sink)(command) {
                self.settings.shortcut_capture =
                    ShortcutCapture::Failed(format!("Could not stop listening: {error}"));
            }
        }
        cx.notify();
    }

    fn finish_shortcut_capture(
        &mut self,
        result: Result<HotkeyCaptureOutcome, UiActionError>,
        cx: &mut Context<Self>,
    ) {
        self.settings.shortcut_capture = match result {
            Ok(HotkeyCaptureOutcome::Captured { hotkey }) => {
                self.settings.form.draft.hotkey = hotkey;
                self.recompute_settings_dirty(cx);
                ShortcutCapture::Idle
            }
            Ok(HotkeyCaptureOutcome::Cancelled) => ShortcutCapture::Idle,
            Ok(HotkeyCaptureOutcome::TimedOut) => {
                ShortcutCapture::Failed("No shortcut was pressed. Try again.".to_owned())
            }
            Err(error) => ShortcutCapture::Failed(format!("Could not capture: {error}")),
        };
        cx.notify();
    }

    pub(super) fn save_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.settings_commands.api_key_input.clone();
        let api_key = input.read(cx).value().trim().to_owned();
        if api_key.is_empty() {
            self.settings_commands.api_key_feedback = Some("Paste an API key first".to_owned());
            return;
        }
        let request_id = self.settings_commands.next_request_id;
        self.settings_commands.next_request_id += 1;
        self.settings_commands.api_key_feedback = Some(
            match (self.settings_commands.command_sink)(
                agentdictate_core::ClientCommand::set_api_key(request_id, api_key),
            ) {
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
