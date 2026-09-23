use agentdictate_core::{DictationNotice, FailureKind};

/// How the app words a failed dictation: a short headline, and advice that
/// says what happened and what to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FailureWording {
    pub headline: &'static str,
    pub advice: &'static str,
}

#[must_use]
pub const fn failure_wording(kind: FailureKind) -> FailureWording {
    let (headline, advice) = match kind {
        FailureKind::Offline => (
            "Couldn't reach OpenAI",
            "Check your internet connection, then transcribe it again.",
        ),
        FailureKind::CredentialMissing => (
            "No OpenAI API key",
            "Add your API key in Settings, then transcribe it again.",
        ),
        FailureKind::CredentialRejected => (
            "API key refused",
            "Check your API key in Settings, then transcribe it again.",
        ),
        FailureKind::RateLimited => (
            "OpenAI limit reached",
            "OpenAI is limiting requests or your quota ran out. Wait a minute, then transcribe it again.",
        ),
        FailureKind::ProviderError => (
            "Couldn't transcribe",
            "OpenAI couldn't transcribe this recording. Try again in a moment.",
        ),
        FailureKind::NoSpeech => (
            "Didn't hear anything",
            "No words were recognized. Check that the right microphone is on.",
        ),
        FailureKind::MicrophoneUnavailable => (
            "Microphone unavailable",
            "Check that your microphone is connected and not in use by another app.",
        ),
        FailureKind::MicrophoneStalled => (
            "Microphone stopped",
            "The microphone stopped sending audio. What was recorded is saved.",
        ),
        FailureKind::PasteNotConfirmed => (
            "Couldn't paste",
            "The text may not have reached your app. It's saved, so you can paste it again.",
        ),
        FailureKind::Unexpected => (
            "Dictation interrupted",
            "AgentDictate stopped before this dictation finished. The recording is saved.",
        ),
    };
    FailureWording { headline, advice }
}

/// What a notice says: a title, a detail when there is more to say, and
/// the sentence a desktop notification adds below the title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoticeWording {
    pub title: &'static str,
    pub detail: Option<&'static str>,
    pub body: &'static str,
}

#[must_use]
pub const fn notice_wording(notice: DictationNotice) -> NoticeWording {
    match notice {
        DictationNotice::Copied => NoticeWording {
            title: "Copied — press Ctrl+V",
            detail: None,
            body: "The text wasn't pasted, so it's on the clipboard. Press Ctrl+V where you want it.",
        },
        DictationNotice::NothingHeard => NoticeWording {
            title: "Didn't hear anything",
            detail: Some("Check your microphone"),
            body: "The recording was silent. Check that the right microphone is on and not muted.",
        },
        DictationNotice::PasteUnavailable => NoticeWording {
            title: "Can't paste it again",
            detail: None,
            body: "Only the last dictation, or one waiting in Recovery, can be pasted again. Copy older ones from History.",
        },
        DictationNotice::Failed { failure } => {
            let wording = failure_wording(failure);
            NoticeWording {
                title: wording.headline,
                detail: Some(match failure {
                    // Nothing was recorded, so there is nothing to recover.
                    FailureKind::MicrophoneUnavailable => "Check your microphone",
                    _ => "Saved to Recovery",
                }),
                body: wording.advice,
            }
        }
    }
}

/// The reason a Recovery item shows: its failure's wording, or for an item
/// without one (a note, or an item from before failures were typed) its
/// stored message.
#[must_use]
pub fn recovery_reason(failure: Option<FailureKind>, stored_message: Option<&str>) -> String {
    match failure {
        Some(kind) => {
            let wording = failure_wording(kind);
            format!("{} · {}", wording.headline, wording.advice)
        }
        None => stored_message
            .filter(|message| !message.trim().is_empty())
            .unwrap_or("Recording saved safely")
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_failure_is_worded_instead_of_its_raw_error_and_a_note_shows_as_stored() {
        let raw = "Could not reach OpenAI: error sending request for url (https://api.openai.com/v1/audio/transcriptions)";

        let reason = recovery_reason(Some(FailureKind::Offline), Some(raw));

        assert!(reason.starts_with("Couldn't reach OpenAI · "), "{reason}");
        assert!(!reason.contains("error sending request"), "{reason}");
        assert_eq!(
            recovery_reason(None, Some("Cancelled before paste")),
            "Cancelled before paste"
        );
    }
}
