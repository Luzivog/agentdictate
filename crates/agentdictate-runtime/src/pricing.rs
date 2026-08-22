use std::collections::BTreeSet;

use agentdictate_core::{Settings, TranscriptionProvider, estimate_session_cost};
use chrono::{DateTime, Utc};
use rusqlite::params;

use crate::{Runtime, RuntimeError, history::recompute_daily_stats, parse_timestamp, timestamp};

struct StoredSession {
    id: i64,
    started_at: DateTime<Utc>,
    duration_seconds: f64,
    transcription_model: String,
    transcription_provider: TranscriptionProvider,
    cleanup_enabled: bool,
    cleanup_model: Option<String>,
    raw_transcript: String,
    cleaned_transcript: Option<String>,
}

impl Runtime {
    /// Synchronizes the Python-compatible pricing table and reprices all
    /// retained history in one transaction.
    pub fn sync_pricing(&mut self, settings: &Settings) -> Result<(), RuntimeError> {
        let sessions = self.stored_sessions()?;
        let transaction = self.connection.transaction()?;
        let now = timestamp(Utc::now());
        for (model, price) in &settings.transcription_prices {
            transaction.execute(
                r#"
                INSERT INTO pricing_settings (
                    model_name, model_type, price_per_audio_minute, currency,
                    updated_at
                ) VALUES (?1, 'transcription', ?2, ?3, ?4)
                ON CONFLICT(model_name, model_type) DO UPDATE SET
                    price_per_audio_minute = excluded.price_per_audio_minute,
                    currency = excluded.currency,
                    updated_at = excluded.updated_at
                "#,
                params![model, price.price_per_audio_minute, price.currency, now,],
            )?;
        }
        for (model, price) in &settings.cleanup_prices {
            transaction.execute(
                r#"
                INSERT INTO pricing_settings (
                    model_name, model_type, input_price_per_1m_tokens,
                    output_price_per_1m_tokens, currency, updated_at
                ) VALUES (?1, 'cleanup', ?2, ?3, ?4, ?5)
                ON CONFLICT(model_name, model_type) DO UPDATE SET
                    input_price_per_1m_tokens = excluded.input_price_per_1m_tokens,
                    output_price_per_1m_tokens = excluded.output_price_per_1m_tokens,
                    currency = excluded.currency,
                    updated_at = excluded.updated_at
                "#,
                params![
                    model,
                    price.input_price_per_1m_tokens,
                    price.output_price_per_1m_tokens,
                    price.currency,
                    now,
                ],
            )?;
        }

        let mut changed_days = BTreeSet::new();
        for session in sessions {
            let api_transcription_price = settings
                .transcription_prices
                .get(&session.transcription_model)
                .map_or(0.0, |price| price.price_per_audio_minute);
            let transcription_price = session
                .transcription_provider
                .marginal_price_per_audio_minute(api_transcription_price);
            let cleanup_price = session
                .cleanup_model
                .as_ref()
                .and_then(|model| settings.cleanup_prices.get(model));
            let cleanup_enabled = session.cleanup_enabled && session.cleaned_transcript.is_some();
            let cost = estimate_session_cost(
                session.duration_seconds,
                &session.raw_transcript,
                session.cleaned_transcript.as_deref(),
                cleanup_enabled,
                transcription_price,
                cleanup_price.map_or(0.0, |price| price.input_price_per_1m_tokens),
                cleanup_price.map_or(0.0, |price| price.output_price_per_1m_tokens),
            );
            transaction.execute(
                r#"
                UPDATE dictation_sessions
                SET estimated_transcription_cost = ?1,
                    estimated_cleanup_cost = ?2,
                    estimated_total_cost = ?3
                WHERE id = ?4
                "#,
                params![
                    cost.transcription_cost,
                    cost.cleanup_cost,
                    cost.total_cost,
                    session.id,
                ],
            )?;
            changed_days.insert(session.started_at.date_naive());
        }
        for day in changed_days {
            recompute_daily_stats(&transaction, day)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn stored_sessions(&self) -> Result<Vec<StoredSession>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT s.id, s.started_at, s.duration_seconds,
                   s.transcription_model, s.transcription_provider,
                   s.cleanup_enabled, s.cleanup_model,
                   h.raw_transcript, h.cleaned_transcript
            FROM dictation_sessions s
            LEFT JOIN transcript_history h ON h.session_id = s.id
            "#,
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredSession {
                    id: row.0,
                    started_at: parse_timestamp(&row.1)?,
                    duration_seconds: row.2,
                    transcription_model: row.3,
                    transcription_provider: row.4.parse::<TranscriptionProvider>().map_err(
                        |error| RuntimeError::InvalidTranscriptionProvider(error.to_string()),
                    )?,
                    cleanup_enabled: row.5,
                    cleanup_model: row.6,
                    raw_transcript: row.7,
                    cleaned_transcript: row.8,
                })
            })
            .collect()
    }
}
