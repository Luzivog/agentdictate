/// Estimated OpenAI price of one minute of audio for `model`, in USD. A
/// dictation is priced once, when it is recorded. Models without a known
/// price, such as a config.json override, are estimated at gpt-transcribe's.
#[must_use]
pub fn transcription_price_per_minute(model: &str) -> f64 {
    match model {
        "gpt-live-transcribe" => 0.017,
        _ => 0.0045,
    }
}
