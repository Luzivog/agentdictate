use serde::{Deserialize, Serialize};

/// Why a dictation did not end in a normal paste, in terms the user can act
/// on. The daemon decides it where the job fails, Recovery keeps it with the
/// job, and the UI words it, so raw error text never reaches the user.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The transcription service could not be reached.
    Offline,
    /// No API key is saved.
    CredentialMissing,
    /// The service refused the saved API key.
    CredentialRejected,
    /// The service is limiting requests, or the account is out of quota.
    RateLimited,
    /// The service answered with an error or an unusable response.
    ProviderError,
    /// No words were heard in the recording.
    NoSpeech,
    /// The microphone could not start recording.
    MicrophoneUnavailable,
    /// The microphone stopped delivering audio, or its recorder exited.
    MicrophoneStalled,
    /// The text was ready, but the paste failed or may not have landed.
    PasteNotConfirmed,
    /// Anything else: AgentDictate stopped, or saving the dictation failed.
    Unexpected,
}

impl FailureKind {
    pub const ALL: [Self; 10] = [
        Self::Offline,
        Self::CredentialMissing,
        Self::CredentialRejected,
        Self::RateLimited,
        Self::ProviderError,
        Self::NoSpeech,
        Self::MicrophoneUnavailable,
        Self::MicrophoneStalled,
        Self::PasteNotConfirmed,
        Self::Unexpected,
    ];

    /// The name stored with a Recovery item.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::CredentialMissing => "credential_missing",
            Self::CredentialRejected => "credential_rejected",
            Self::RateLimited => "rate_limited",
            Self::ProviderError => "provider_error",
            Self::NoSpeech => "no_speech",
            Self::MicrophoneUnavailable => "microphone_unavailable",
            Self::MicrophoneStalled => "microphone_stalled",
            Self::PasteNotConfirmed => "paste_not_confirmed",
            Self::Unexpected => "unexpected",
        }
    }

    /// Reads a stored name. A name this version does not know, written by a
    /// newer one, reads as `Unexpected`, so it never stops Recovery loading.
    #[must_use]
    pub fn from_stored(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == name)
            .unwrap_or(Self::Unexpected)
    }
}

/// What the overlay and a desktop notification say when a dictation ends
/// without its text being pasted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "notice", rename_all = "snake_case")]
pub enum DictationNotice {
    /// The text is on the clipboard but was not pasted: press Ctrl+V.
    Copied,
    /// The recording was quiet, so nothing was transcribed or kept.
    NothingHeard,
    /// The dictation failed and waits in Recovery.
    Failed { failure: FailureKind },
    /// "Paste again" was asked for a dictation that is neither the last one
    /// nor waiting in Recovery, so nothing was pasted.
    PasteUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_reads_back_from_its_stored_name_and_unknown_names_are_unexpected() {
        for kind in FailureKind::ALL {
            assert_eq!(FailureKind::from_stored(kind.as_str()), kind);
        }
        assert_eq!(
            FailureKind::from_stored("a_kind_from_a_newer_version"),
            FailureKind::Unexpected
        );
    }
}
