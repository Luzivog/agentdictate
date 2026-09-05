use agentdictate_core::{AppliedReplacement, TranscriptionProvider, count_words_ascii_history};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::history::{recompute_daily_stats, serialize_replacements};
use crate::{Runtime, RuntimeError, timestamp};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalDictationSource {
    ChatGptDesktop,
}

impl ExternalDictationSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatGptDesktop => "chatgpt_desktop",
        }
    }

    const fn transcription_provider(self) -> TranscriptionProvider {
        match self {
            Self::ChatGptDesktop => TranscriptionProvider::ChatGptSubscription,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExternalDictationReceipt {
    pub source: ExternalDictationSource,
    pub source_id: String,
    pub started_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub transcription_model: String,
    pub raw_transcript: String,
    pub final_text: String,
    pub replacements_applied: Vec<AppliedReplacement>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalDictationImportOutcome {
    Imported { session_id: i64, word_count: u64 },
    AlreadyImported,
}

impl Runtime {
    /// Adds a standard history and usage session from another dictation client.
    ///
    /// The source receipt and usage row commit in one immediate transaction.
    /// The receipt ledger survives history deletion so a later scan cannot
    /// restore usage that the user deliberately cleared.
    pub fn import_external_dictation(
        &mut self,
        receipt: &ExternalDictationReceipt,
    ) -> Result<ExternalDictationImportOutcome, RuntimeError> {
        validate_receipt(receipt)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = receipt.source.as_str();
        let exists = transaction
            .query_row(
                r#"
                SELECT 1
                FROM external_dictation_imports
                WHERE source = ?1 AND source_id = ?2
                "#,
                params![source, receipt.source_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            return Ok(ExternalDictationImportOutcome::AlreadyImported);
        }

        let raw_transcript = receipt.raw_transcript.trim();
        let final_text = receipt.final_text.trim();
        let raw_words = count_words_ascii_history(raw_transcript);
        let final_words = count_words_ascii_history(final_text);
        let replacements_applied = serialize_replacements(&receipt.replacements_applied)?;
        let duration_milliseconds = (receipt.duration_seconds * 1_000.0).round() as i64;
        let ended_at = receipt.started_at + Duration::milliseconds(duration_milliseconds);
        transaction.execute(
            r#"
            INSERT INTO dictation_sessions (
                started_at, ended_at, duration_seconds, transcription_model,
                transcription_provider, cleanup_enabled, raw_word_count,
                final_word_count, final_character_count,
                estimated_transcription_cost, estimated_cleanup_cost,
                estimated_total_cost, success, error_message, runtime_job_id
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, 0, 0, 0, 1, NULL, NULL
            )
            "#,
            params![
                timestamp(receipt.started_at),
                timestamp(ended_at),
                receipt.duration_seconds,
                receipt.transcription_model,
                receipt.source.transcription_provider().as_str(),
                raw_words,
                final_words,
                final_text.chars().count() as u64,
            ],
        )?;
        let session_id = transaction.last_insert_rowid();
        transaction.execute(
            r#"
            INSERT INTO transcript_history (
                session_id, created_at, raw_transcript, cleaned_transcript,
                final_text, replacements_applied, copied_to_clipboard,
                paste_triggered, cleanup_error
            ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, 0, 0, NULL)
            "#,
            params![
                session_id,
                timestamp(ended_at),
                raw_transcript,
                final_text,
                replacements_applied,
            ],
        )?;
        transaction.execute(
            r#"
            INSERT INTO external_dictation_imports (
                source, source_id, imported_at, session_id
            ) VALUES (?1, ?2, ?3, ?4)
            "#,
            params![source, receipt.source_id, timestamp(Utc::now()), session_id],
        )?;
        recompute_daily_stats(&transaction, receipt.started_at.date_naive())?;
        transaction.commit()?;
        Ok(ExternalDictationImportOutcome::Imported {
            session_id,
            word_count: final_words,
        })
    }
}

fn validate_receipt(receipt: &ExternalDictationReceipt) -> Result<(), RuntimeError> {
    if receipt.source_id.trim().is_empty() {
        return Err(RuntimeError::InvalidExternalDictation(
            "source id is blank".to_owned(),
        ));
    }
    if receipt.source_id.len() > 512 {
        return Err(RuntimeError::InvalidExternalDictation(
            "source id is too long".to_owned(),
        ));
    }
    if !receipt.duration_seconds.is_finite() || receipt.duration_seconds < 0.0 {
        return Err(RuntimeError::InvalidExternalDictation(
            "duration must be a finite non-negative number".to_owned(),
        ));
    }
    if receipt.transcription_model.trim().is_empty() {
        return Err(RuntimeError::InvalidExternalDictation(
            "transcription model is blank".to_owned(),
        ));
    }
    if receipt.raw_transcript.trim().is_empty() {
        return Err(RuntimeError::InvalidExternalDictation(
            "raw transcript is blank".to_owned(),
        ));
    }
    if receipt.final_text.trim().is_empty() {
        return Err(RuntimeError::InvalidExternalDictation(
            "final text is blank".to_owned(),
        ));
    }
    Ok(())
}
