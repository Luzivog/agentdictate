use gpui::{Context, Entity, SharedString, Subscription, Window, prelude::*};
use gpui_component::{
    IndexPath,
    input::{InputEvent, InputState, TextareaState},
    select::{SearchableVec, SelectEvent, SelectItem, SelectState},
};

use agentdictate_core::TranscriptionProvider;

use crate::{SettingsDraft, settings::settings_fields};

use super::SettingsShell;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SettingOption {
    label: SharedString,
    value: String,
}

impl SettingOption {
    fn new(label: impl Into<SharedString>, value: impl Into<String>) -> Self {
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

macro_rules! read_select {
    (provider, $state:expr, $fallback:expr, $cx:ident) => {
        selected_transcription_provider($state, $fallback, $cx)
    };
    (string, $state:expr, $fallback:expr, $cx:ident) => {
        selected_setting($state, &$fallback, $cx)
    };
}

macro_rules! define_settings_form {
    (
        select {
            $(
                $select_field:ident: $select_type:ty {
                    from: $select_from:ident,
                    apply: $select_apply_kind:ident($select_apply:ident),
                    options: plain($select_options:ident),
                    searchable: $select_searchable:literal,
                    read: $select_read:ident,
                },
            )*
        }
        text_area {
            $(
                $text_area_field:ident: $text_area_type:ty {
                    from: $text_area_from:ident,
                    apply: $text_area_apply_kind:ident($text_area_apply:ident),
                    placeholder: $text_area_placeholder:literal,
                    rows: $text_area_min_rows:literal..=$text_area_max_rows:literal,
                },
            )*
        }
        number {
            $(
                $number_field:ident: $number_type:ty {
                    from: $number_from:ident,
                    apply: $number_apply_kind:ident($number_apply:ident),
                    placeholder: $number_placeholder:literal,
                    maximum: $number_maximum:expr,
                },
            )*
        }
        draft_only {
            $(
                $draft_only_field:ident: $draft_only_type:ty {
                    from: $draft_only_from:ident,
                    apply: $draft_only_apply_kind:ident($draft_only_apply:ident),
                },
            )*
        }
        shortcut {
            $(
                $shortcut_field:ident: $shortcut_type:ty {
                    from: $shortcut_from:ident,
                    apply: $shortcut_apply_kind:ident($shortcut_apply:ident),
                },
            )*
        }
    ) => {
        #[derive(Clone)]
        pub(super) struct SettingsFormState {
            pub(super) draft: SettingsDraft,
            $(pub(super) $select_field: Entity<SettingSelectState>,)*
            $(pub(super) $text_area_field: Entity<TextareaState>,)*
            $(pub(super) $number_field: Entity<InputState>,)*
        }

        impl SettingsFormState {
            pub(super) fn new(
                settings: &agentdictate_core::Settings,
                window: &mut Window,
                cx: &mut Context<SettingsShell>,
            ) -> Self {
                let draft = SettingsDraft::from(settings);
                Self {
                    $(
                        $select_field: settings_select(
                            $select_options(),
                            draft.$select_field.as_str(),
                            $select_searchable,
                            window,
                            cx,
                        ),
                    )*
                    $(
                        $text_area_field: settings_text_area(
                            draft.$text_area_field.clone(),
                            $text_area_placeholder,
                            $text_area_min_rows,
                            $text_area_max_rows,
                            window,
                            cx,
                        ),
                    )*
                    $(
                        $number_field: settings_number_input(
                            draft.$number_field.clone(),
                            $number_placeholder,
                            $number_maximum,
                            window,
                            cx,
                        ),
                    )*
                    draft,
                }
            }

            pub(super) fn snapshot(&self, cx: &gpui::App) -> SettingsDraft {
                let mut draft = self.draft.clone();
                $(
                    draft.$select_field = read_select!(
                        $select_read,
                        &self.$select_field,
                        draft.$select_field,
                        cx
                    );
                )*
                $(draft.$text_area_field = self.$text_area_field.read(cx).value().to_string();)*
                $(draft.$number_field = self.$number_field.read(cx).value().to_string();)*
                draft
            }

            pub(super) fn inputs(&self) -> Vec<Entity<InputState>> {
                vec![$(self.$number_field.clone(),)*]
            }

            pub(super) fn text_areas(&self) -> Vec<Entity<TextareaState>> {
                vec![$(self.$text_area_field.clone(),)*]
            }

            pub(super) fn selects(&self) -> Vec<Entity<SettingSelectState>> {
                vec![$(self.$select_field.clone(),)*]
            }

            pub(super) fn reset(
                &mut self,
                settings: &agentdictate_core::Settings,
                window: &mut Window,
                cx: &mut Context<SettingsShell>,
            ) {
                self.draft = SettingsDraft::from(settings);
                let draft = self.draft.clone();
                $(
                    self.$text_area_field.update(cx, |input, cx| {
                        input.set_value(draft.$text_area_field.clone(), window, cx);
                    });
                )*
                $(
                    self.$number_field.update(cx, |input, cx| {
                        input.set_value(draft.$number_field.clone(), window, cx);
                    });
                )*
                $(
                    self.$select_field.update(cx, |select, cx| {
                        select.set_selected_value(
                            &draft.$select_field.as_str().to_owned(),
                            window,
                            cx,
                        );
                    });
                )*
            }

            pub(super) fn subscriptions(
                &self,
                cx: &mut Context<SettingsShell>,
            ) -> Vec<Subscription> {
                fn on_input_change(
                    shell: &mut SettingsShell,
                    event: &InputEvent,
                    cx: &mut Context<SettingsShell>,
                ) {
                    if matches!(event, InputEvent::Change) {
                        shell.recompute_settings_dirty(cx);
                        cx.notify();
                    }
                }
                let mut subscriptions: Vec<Subscription> = self
                    .inputs()
                    .into_iter()
                    .map(|input| {
                        cx.subscribe(&input, |shell, _, event, cx| on_input_change(shell, event, cx))
                    })
                    .collect();
                subscriptions.extend(self.text_areas().into_iter().map(|text_area| {
                    cx.subscribe(&text_area, |shell, _, event, cx| on_input_change(shell, event, cx))
                }));
                subscriptions.extend(self.selects().into_iter().map(|select| {
                    cx.subscribe(
                        &select,
                        |shell, _, event: &SelectEvent<SearchableVec<SettingOption>>, cx| {
                            if matches!(event, SelectEvent::Confirm(Some(_))) {
                                shell.recompute_settings_dirty(cx);
                                shell.clear_route_feedback();
                                cx.notify();
                            }
                        },
                    )
                }));
                $(
                    let _ = stringify!($shortcut_field);
                    subscriptions.push(cx.observe_keystrokes(|shell, event, _window, cx| {
                        if shell.settings.shortcut_capture_active {
                            shell.capture_shortcut(&event.keystroke, cx);
                            cx.stop_propagation();
                        }
                    }));
                )*
                subscriptions
            }
        }
    };
}

settings_fields!(define_settings_form);

fn settings_text_area(
    value: String,
    placeholder: &'static str,
    min_rows: usize,
    max_rows: usize,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
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
    // With a step and range set, NumberInput steps internally and emits
    // InputEvent::Change, so stepping marks the draft dirty like typing.
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
            .step(1.0)
            .min(0.0)
            .max(maximum as f64)
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

pub(super) fn selected_setting(
    state: &Entity<SettingSelectState>,
    fallback: &str,
    cx: &gpui::App,
) -> String {
    state
        .read(cx)
        .selected_value()
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

pub(super) fn selected_transcription_provider(
    state: &Entity<SettingSelectState>,
    fallback: TranscriptionProvider,
    cx: &gpui::App,
) -> TranscriptionProvider {
    selected_setting(state, fallback.as_str(), cx)
        .parse()
        .unwrap_or(fallback)
}

fn transcription_provider_options() -> Vec<SettingOption> {
    vec![
        SettingOption::new(
            "ChatGPT subscription",
            TranscriptionProvider::ChatGptSubscription.as_str(),
        ),
        SettingOption::new("OpenAI API", TranscriptionProvider::OpenAiApi.as_str()),
    ]
}

fn setting_options(options: &[(&str, &str)]) -> Vec<SettingOption> {
    options
        .iter()
        .map(|(label, value)| SettingOption::new((*label).to_owned(), (*value).to_owned()))
        .collect()
}

fn language_options() -> Vec<SettingOption> {
    setting_options(&[
        ("Auto-detect", ""),
        ("English (en)", "en"),
        ("English and French", "en,fr"),
        ("French (fr)", "fr"),
        ("Spanish (es)", "es"),
        ("German (de)", "de"),
        ("Portuguese (pt)", "pt"),
        ("Italian (it)", "it"),
        ("Dutch (nl)", "nl"),
        ("Polish (pl)", "pl"),
        ("Arabic (ar)", "ar"),
        ("Chinese (zh)", "zh"),
        ("Japanese (ja)", "ja"),
        ("Korean (ko)", "ko"),
        ("Hindi (hi)", "hi"),
    ])
}

fn recording_mode_options() -> Vec<SettingOption> {
    setting_options(&[("Toggle", "toggle"), ("Hold", "hold")])
}

fn paste_shortcut_options() -> Vec<SettingOption> {
    setting_options(&[
        ("Automatic", "Automatic"),
        ("Standard (Ctrl+V)", "Standard (Ctrl+V)"),
        ("Terminal (Ctrl+Shift+V)", "Terminal (Ctrl+Shift+V)"),
    ])
}

fn dictation_mode_options() -> Vec<SettingOption> {
    setting_options(&[("Dictate", "Dictate"), ("Literal", "Literal")])
}
