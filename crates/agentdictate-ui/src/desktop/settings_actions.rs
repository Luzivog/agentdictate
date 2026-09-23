use std::sync::Arc;

use agentdictate_core::{HotkeyCaptureOutcome, KeepTranscripts, SettingChange};
use gpui::{AppContext, Context, Window};

use crate::{SettingsRequest, UiActionError};

use super::{
    SettingsShell,
    settings_form::{SettingRow, deletes_transcripts},
    settings_shell::ShortcutCapture,
};

impl SettingsShell {
    /// Asks the daemon for the next shortcut pressed on any keyboard. The
    /// window drops keyboard focus so the chord cannot also edit a field.
    pub(super) fn begin_shortcut_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.blur(cx);
        let capture = Arc::clone(&self.settings.hotkey_capture);
        let answer = cx.background_spawn(async move { capture() });
        let answer = cx.spawn_in(window, async move |shell, cx| {
            let result = answer.await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                shell.finish_shortcut_capture(result, window, cx);
            });
        });
        self.settings.shortcut_capture = ShortcutCapture::Listening { _answer: answer };
        cx.notify();
    }

    /// Stops a capture in progress and clears any capture message.
    pub(super) fn cancel_shortcut_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous =
            std::mem::replace(&mut self.settings.shortcut_capture, ShortcutCapture::Idle);
        if previous.is_listening() {
            self.send_settings_request(
                SettingsRequest::CancelHotkeyCapture,
                window,
                cx,
                |shell, result, _, _| {
                    if let Err(error) = result {
                        shell.settings.shortcut_capture =
                            ShortcutCapture::Failed(format!("Could not stop listening: {error}"));
                    }
                },
            );
        }
        cx.notify();
    }

    /// Applies the captured shortcut at once, or says why none was captured.
    fn finish_shortcut_capture(
        &mut self,
        result: Result<HotkeyCaptureOutcome, UiActionError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.shortcut_capture = match result {
            Ok(HotkeyCaptureOutcome::Captured { hotkey }) => {
                let change = SettingChange::Hotkey(hotkey);
                self.change_setting(SettingRow::Shortcut, change, window, cx);
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

    /// Saves the key typed in the API key field, then empties the field.
    pub(super) fn save_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let api_key = self
            .settings
            .controls
            .api_key
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if api_key.is_empty() {
            self.settings.error = Some((SettingRow::ApiKey, "Paste an API key first".to_owned()));
            cx.notify();
            return;
        }
        self.settings.error = None;
        self.send_settings_request(
            SettingsRequest::SetApiKey(api_key),
            window,
            cx,
            |shell, result, window, cx| match result {
                Ok(()) => {
                    shell.settings.controls.api_key.update(cx, |input, cx| {
                        input.set_value(String::new(), window, cx);
                    });
                    shell.settings.replacing_api_key = false;
                }
                Err(error) => {
                    shell.settings.error =
                        Some((SettingRow::ApiKey, format!("Not saved: {error}")));
                }
            },
        );
    }

    /// Shows the key field in place of "Key saved ✓", or hides it again.
    pub(super) fn set_replacing_api_key(
        &mut self,
        replacing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.replacing_api_key = replacing;
        let input = self.settings.controls.api_key.clone();
        input.update(cx, |input, cx| {
            input.set_value(String::new(), window, cx);
            if replacing {
                input.focus(window, cx);
            }
        });
        cx.notify();
    }

    /// Applies a Keep transcripts choice that keeps as much or more at once.
    /// A shorter one deletes stored transcripts, so it waits for Confirm delete.
    pub(super) fn choose_keep_transcripts(
        &mut self,
        choice: KeepTranscripts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.settings.shown().keep_transcripts;
        if deletes_transcripts(choice, current) {
            self.settings.shorter_retention = Some(choice);
        } else {
            self.settings.shorter_retention = None;
            if choice != current {
                let change = SettingChange::KeepTranscripts(choice);
                self.change_setting(SettingRow::KeepTranscripts, change, window, cx);
            }
        }
        cx.notify();
    }

    /// Applies the shorter Keep transcripts choice the user confirmed, or
    /// drops it and shows the kept choice again.
    pub(super) fn settle_shorter_retention(
        &mut self,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.settings.shorter_retention.take() {
            Some(choice) if confirmed => {
                let change = SettingChange::KeepTranscripts(choice);
                self.change_setting(SettingRow::KeepTranscripts, change, window, cx);
            }
            _ => self.settings.sync_choices(window, cx),
        }
        cx.notify();
    }

    pub(super) fn toggle_advanced_settings(&mut self, cx: &mut Context<Self>) {
        self.settings.advanced_open = !self.settings.advanced_open;
        cx.notify();
    }
}
