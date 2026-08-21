use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub transcription_cost: f64,
    pub cleanup_cost: f64,
    pub total_cost: f64,
    pub cleanup_input_tokens: u64,
    pub cleanup_output_tokens: u64,
}

#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    ((text.len() as f64 / 4.0).round_ties_even() as u64).max(1)
}

#[must_use]
pub fn estimate_session_cost(
    duration_seconds: f64,
    raw_transcript: &str,
    cleaned_transcript: Option<&str>,
    cleanup_enabled: bool,
    transcription_price_per_minute: f64,
    cleanup_input_price_per_1m_tokens: f64,
    cleanup_output_price_per_1m_tokens: f64,
) -> CostEstimate {
    let transcription_cost =
        (duration_seconds.max(0.0) / 60.0) * transcription_price_per_minute.max(0.0);
    let (cleanup_cost, cleanup_input_tokens, cleanup_output_tokens) = if cleanup_enabled
        && let Some(cleaned) = cleaned_transcript
    {
        let input_tokens = estimate_tokens(raw_transcript);
        let output_tokens = estimate_tokens(cleaned);
        let cost = input_tokens as f64 / 1_000_000.0 * cleanup_input_price_per_1m_tokens.max(0.0)
            + output_tokens as f64 / 1_000_000.0 * cleanup_output_price_per_1m_tokens.max(0.0);
        (cost, input_tokens, output_tokens)
    } else {
        (0.0, 0, 0)
    };
    CostEstimate {
        transcription_cost,
        cleanup_cost,
        total_cost: transcription_cost + cleanup_cost,
        cleanup_input_tokens,
        cleanup_output_tokens,
    }
}
