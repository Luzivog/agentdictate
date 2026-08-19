from __future__ import annotations

from datetime import datetime, timezone

SCHEMA = """
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS dictation_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL,
    cleanup_enabled INTEGER NOT NULL DEFAULT 0,
    cleanup_model TEXT,
    cleanup_style TEXT,
    raw_word_count INTEGER NOT NULL DEFAULT 0,
    final_word_count INTEGER NOT NULL DEFAULT 0,
    final_character_count INTEGER NOT NULL DEFAULT 0,
    estimated_transcription_cost REAL NOT NULL DEFAULT 0,
    estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 1,
    error_message TEXT
);

CREATE TABLE IF NOT EXISTS transcript_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES dictation_sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    raw_transcript TEXT NOT NULL DEFAULT '',
    cleaned_transcript TEXT,
    final_text TEXT NOT NULL DEFAULT '',
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    cleanup_error TEXT
);

CREATE TABLE IF NOT EXISTS replacement_mappings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_phrase TEXT NOT NULL,
    replacement_phrase TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    case_sensitive INTEGER NOT NULL DEFAULT 0,
    whole_word_only INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_stats (
    date TEXT PRIMARY KEY,
    total_sessions INTEGER NOT NULL DEFAULT 0,
    total_words INTEGER NOT NULL DEFAULT 0,
    total_audio_seconds REAL NOT NULL DEFAULT 0,
    average_wpm REAL NOT NULL DEFAULT 0,
    estimated_transcription_cost REAL NOT NULL DEFAULT 0,
    estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS pricing_settings (
    model_name TEXT NOT NULL,
    model_type TEXT NOT NULL,
    input_price_per_1m_tokens REAL NOT NULL DEFAULT 0,
    output_price_per_1m_tokens REAL NOT NULL DEFAULT 0,
    price_per_audio_minute REAL NOT NULL DEFAULT 0,
    currency TEXT NOT NULL DEFAULT 'USD',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (model_name, model_type)
);

CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON dictation_sessions(started_at);
CREATE INDEX IF NOT EXISTS idx_history_created_at ON transcript_history(created_at);
CREATE INDEX IF NOT EXISTS idx_history_session_id ON transcript_history(session_id);

CREATE TABLE IF NOT EXISTS dictation_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    state TEXT NOT NULL,
    stage TEXT NOT NULL,
    audio_path TEXT NOT NULL UNIQUE,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL DEFAULT '',
    raw_transcript TEXT NOT NULL DEFAULT '',
    final_text TEXT NOT NULL DEFAULT '',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_dictation_jobs_state ON dictation_jobs(state);
CREATE INDEX IF NOT EXISTS idx_dictation_jobs_updated_at ON dictation_jobs(updated_at);
"""


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()
