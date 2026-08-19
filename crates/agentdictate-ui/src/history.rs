#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStage {
    Transcription,
    Delivery,
}

impl RecoveryStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transcription => "Needs transcription",
            Self::Delivery => "Ready to paste",
        }
    }

    pub const fn primary_action_label(self) -> &'static str {
        match self {
            Self::Transcription => "Transcribe again",
            Self::Delivery => "Paste again",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryItemViewModel {
    pub id: String,
    pub stage: RecoveryStage,
    pub captured_at: String,
    pub duration: String,
    pub error: String,
    pub transcript_preview: Option<String>,
}

impl RecoveryItemViewModel {
    pub fn new(
        id: impl Into<String>,
        stage: RecoveryStage,
        captured_at: impl Into<String>,
        duration: impl Into<String>,
        error: impl Into<String>,
        transcript_preview: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            stage,
            captured_at: captured_at.into(),
            duration: duration.into(),
            error: error.into(),
            transcript_preview,
        }
    }

    pub const fn primary_action_label(&self) -> &'static str {
        self.stage.primary_action_label()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptViewModel {
    pub id: i64,
    pub created_at: String,
    pub text: String,
    pub word_count: u64,
    pub duration: String,
}

impl TranscriptViewModel {
    pub fn new(
        id: i64,
        created_at: impl Into<String>,
        text: impl Into<String>,
        word_count: u64,
        duration: impl Into<String>,
    ) -> Self {
        Self {
            id,
            created_at: created_at.into(),
            text: text.into(),
            word_count,
            duration: duration.into(),
        }
    }

    pub fn preview(&self) -> String {
        const MAX_CHARACTERS: usize = 120;
        let mut characters = self.text.chars();
        let preview = characters.by_ref().take(MAX_CHARACTERS).collect::<String>();
        if characters.next().is_some() {
            format!("{preview}…")
        } else {
            preview
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryViewModel {
    pub title: &'static str,
    pub detail: String,
    pub item_count: u64,
    pub items: Vec<RecoveryItemViewModel>,
}

impl RecoveryViewModel {
    pub fn from_item_count(item_count: u64) -> Self {
        let detail = match item_count {
            0 => "No recordings need your attention".to_owned(),
            1 => "1 recording needs your attention".to_owned(),
            count => format!("{count} recordings need your attention"),
        };

        Self {
            title: "Recovery",
            detail,
            item_count,
            items: Vec::new(),
        }
    }

    pub fn from_items(items: Vec<RecoveryItemViewModel>) -> Self {
        let mut model = Self::from_item_count(items.len() as u64);
        model.items = items;
        model
    }

    pub const fn has_items(&self) -> bool {
        self.item_count > 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryViewModel {
    pub transcript_count: u64,
    pub search: String,
    pub has_more: bool,
    pub recovery: RecoveryViewModel,
    pub transcripts: Vec<TranscriptViewModel>,
}

impl HistoryViewModel {
    pub fn new(transcript_count: u64, recoverable_recording_count: u64) -> Self {
        Self {
            transcript_count,
            search: String::new(),
            has_more: false,
            recovery: RecoveryViewModel::from_item_count(recoverable_recording_count),
            transcripts: Vec::new(),
        }
    }

    pub fn from_records(
        recovery_items: Vec<RecoveryItemViewModel>,
        transcripts: Vec<TranscriptViewModel>,
    ) -> Self {
        let transcript_count = transcripts.len() as u64;
        Self::from_page(
            recovery_items,
            transcripts,
            transcript_count,
            String::new(),
            false,
        )
    }

    pub fn from_page(
        recovery_items: Vec<RecoveryItemViewModel>,
        transcripts: Vec<TranscriptViewModel>,
        transcript_count: u64,
        search: String,
        has_more: bool,
    ) -> Self {
        Self {
            transcript_count,
            search,
            has_more,
            recovery: RecoveryViewModel::from_items(recovery_items),
            transcripts,
        }
    }
}

impl Default for HistoryViewModel {
    fn default() -> Self {
        Self::new(0, 0)
    }
}
