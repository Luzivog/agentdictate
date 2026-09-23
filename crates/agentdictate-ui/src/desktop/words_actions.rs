use gpui::{AppContext, Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

use agentdictate_core::SettingChange;

use crate::{SettingsRequest, WordsEdit};

use super::{SettingsShell, settings_shell::Confirmed};

/// The Words screen's inputs and its one open row editor.
pub(super) struct WordsUiState {
    pub(super) filter: Entity<InputState>,
    pub(super) new_spelling: Entity<InputState>,
    pub(super) new_sounds_like: Entity<InputState>,
    pub(super) editor: Option<WordEditor>,
    /// Why the last add or delete was refused, shown under the add row.
    pub(super) error: Option<String>,
}

/// A Words row open for editing.
pub(super) struct WordEditor {
    pub(super) form: WordForm,
    _submit_on_enter: [Subscription; 2],
}

#[derive(Clone)]
pub(super) struct WordForm {
    /// The word's position in the vocabulary.
    pub(super) index: usize,
    pub(super) spelling: Entity<InputState>,
    pub(super) sounds_like: Entity<InputState>,
    /// Why the last Done was refused.
    pub(super) error: Option<String>,
}

/// History's "Fix a word" editor under one expanded transcript.
pub(super) struct FixWordEditor {
    pub(super) form: FixWordForm,
    _submit_on_enter: [Subscription; 2],
}

#[derive(Clone)]
pub(super) struct FixWordForm {
    pub(super) transcript_id: i64,
    pub(super) heard: Entity<InputState>,
    pub(super) spelling: Entity<InputState>,
    pub(super) error: Option<String>,
}

impl WordsUiState {
    /// Creates the screen's inputs. Typing in the filter re-renders the list,
    /// and Enter in the add row adds the word.
    pub(super) fn new(
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) -> (Self, Vec<Subscription>) {
        let filter = text_input(String::new(), "Filter words", window, cx);
        let new_spelling = text_input(String::new(), "New word, e.g. Siobhan", window, cx);
        let new_sounds_like = text_input(
            String::new(),
            "Sounds like (optional), e.g. shiv on",
            window,
            cx,
        );
        let subscriptions = vec![
            cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            submit_on_enter(&new_spelling, window, cx, SettingsShell::add_word),
            submit_on_enter(&new_sounds_like, window, cx, SettingsShell::add_word),
        ];
        let state = Self {
            filter,
            new_spelling,
            new_sounds_like,
            editor: None,
            error: None,
        };
        (state, subscriptions)
    }

    /// Closes the row editor and clears errors, as when leaving the screen.
    pub(super) fn reset(&mut self) {
        self.editor = None;
        self.error = None;
    }
}

fn text_input(
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

fn submit_on_enter(
    input: &Entity<InputState>,
    window: &Window,
    cx: &mut Context<SettingsShell>,
    submit: fn(&mut SettingsShell, &mut Window, &mut Context<SettingsShell>),
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        move |shell, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                submit(shell, window, cx);
            }
        },
    )
}

fn value(input: &Entity<InputState>, cx: &Context<SettingsShell>) -> String {
    input.read(cx).value().to_string()
}

impl SettingsShell {
    /// Applies `edit` to the vocabulary and saves it right away, then runs
    /// `done` with the outcome: `Err` says why the edit or save was refused.
    fn save_words(
        &mut self,
        edit: WordsEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        match edit.apply(&self.settings.shown().vocabulary) {
            Ok(vocabulary) => self.send_settings_request(
                SettingsRequest::Change(SettingChange::Vocabulary(vocabulary)),
                window,
                cx,
                |shell, result, window, cx| {
                    let result = result.map_err(|error| format!("Could not save: {error}"));
                    done(shell, result, window, cx);
                },
            ),
            Err(error) => done(self, Err(error.to_string()), window, cx),
        }
        cx.notify();
    }

    pub(super) fn add_word(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let words = &self.routes.words;
        let (new_spelling, new_sounds_like) =
            (words.new_spelling.clone(), words.new_sounds_like.clone());
        let edit = WordsEdit::Add {
            spelling: value(&new_spelling, cx),
            sounds_like: value(&new_sounds_like, cx),
        };
        self.save_words(edit, window, cx, |shell, result, window, cx| match result {
            Ok(()) => {
                for input in [new_spelling, new_sounds_like] {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
                shell.routes.words.error = None;
                shell.confirm(Confirmed::WordsSaved, cx);
            }
            Err(message) => shell.routes.words.error = Some(message),
        });
    }

    pub(super) fn open_word_editor(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(word) = self.settings.shown().vocabulary.get(index).cloned() else {
            return;
        };
        let (spelling, sounds_like) = (word.spelling, word.aliases.join(", "));
        let spelling = text_input(spelling, "Spelling", window, cx);
        let sounds_like = text_input(sounds_like, "Sounds like (optional)", window, cx);
        spelling.update(cx, |input, cx| input.focus(window, cx));
        self.routes.words.editor = Some(WordEditor {
            _submit_on_enter: [
                submit_on_enter(&spelling, window, cx, Self::commit_word_editor),
                submit_on_enter(&sounds_like, window, cx, Self::commit_word_editor),
            ],
            form: WordForm {
                index,
                spelling,
                sounds_like,
                error: None,
            },
        });
        cx.notify();
    }

    pub(super) fn commit_word_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.routes.words.editor else {
            return;
        };
        let index = editor.form.index;
        let edit = WordsEdit::Update {
            index,
            spelling: value(&editor.form.spelling, cx),
            sounds_like: value(&editor.form.sounds_like, cx),
        };
        self.save_words(edit, window, cx, move |shell, result, _, cx| {
            let editor = &mut shell.routes.words.editor;
            match result {
                Ok(()) => {
                    if editor
                        .as_ref()
                        .is_some_and(|editor| editor.form.index == index)
                    {
                        *editor = None;
                    }
                    shell.confirm(Confirmed::WordsSaved, cx);
                }
                Err(message) => {
                    if let Some(editor) = editor {
                        editor.form.error = Some(message);
                    }
                }
            }
        });
    }

    pub(super) fn close_word_editor(&mut self, cx: &mut Context<Self>) {
        self.routes.words.editor = None;
        cx.notify();
    }

    pub(super) fn delete_word(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Deleting shifts later words, so an open editor would edit another.
        self.routes.words.reset();
        self.save_words(
            WordsEdit::Delete { index },
            window,
            cx,
            |shell, result, _, cx| match result {
                Ok(()) => shell.confirm(Confirmed::WordsSaved, cx),
                Err(message) => shell.routes.words.error = Some(message),
            },
        );
    }

    pub(super) fn open_fix_word(
        &mut self,
        transcript_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let heard = text_input(String::new(), "e.g. shiv on", window, cx);
        let spelling = text_input(String::new(), "e.g. Siobhan", window, cx);
        heard.update(cx, |input, cx| input.focus(window, cx));
        self.routes.fix_word = Some(FixWordEditor {
            _submit_on_enter: [
                submit_on_enter(&heard, window, cx, Self::save_fix_word),
                submit_on_enter(&spelling, window, cx, Self::save_fix_word),
            ],
            form: FixWordForm {
                transcript_id,
                heard,
                spelling,
                error: None,
            },
        });
        cx.notify();
    }

    pub(super) fn save_fix_word(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.routes.fix_word else {
            return;
        };
        let transcript_id = editor.form.transcript_id;
        let edit = WordsEdit::FixWord {
            heard: value(&editor.form.heard, cx),
            spelling: value(&editor.form.spelling, cx),
        };
        self.save_words(edit, window, cx, move |shell, result, _, cx| {
            let editor = &mut shell.routes.fix_word;
            match result {
                Ok(()) => {
                    if editor
                        .as_ref()
                        .is_some_and(|editor| editor.form.transcript_id == transcript_id)
                    {
                        *editor = None;
                    }
                    shell.confirm(Confirmed::AddedToWords(transcript_id), cx);
                }
                Err(message) => {
                    if let Some(editor) = editor {
                        editor.form.error = Some(message);
                    }
                }
            }
        });
    }

    pub(super) fn close_fix_word(&mut self, cx: &mut Context<Self>) {
        self.routes.fix_word = None;
        cx.notify();
    }
}
