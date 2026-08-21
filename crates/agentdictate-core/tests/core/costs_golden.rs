use agentdictate_core::estimate_session_cost;
use serde_json::Value;

/// The fixture is generated from the legacy Python implementation
/// (`src/agentdictate/costs.py`), pinning bit-identical parity of the shared
/// cost math, including round-ties-even token estimation.
#[test]
fn session_costs_match_the_legacy_python_golden_values() {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/core/fixtures/costs_golden.json"
    ))
    .expect("golden fixture is present");
    let document: Value = serde_json::from_str(&fixture).expect("fixture parses");

    for case in document["cases"]
        .as_array()
        .expect("fixture holds a case array")
    {
        let cleaned = case["cleaned"]
            .as_str()
            .filter(|_| !case["cleaned"].is_null());
        let estimate = estimate_session_cost(
            case["duration_seconds"].as_f64().unwrap(),
            case["raw"].as_str().unwrap(),
            cleaned,
            case["cleanup_enabled"].as_bool().unwrap(),
            case["transcription_price"].as_f64().unwrap(),
            case["input_price"].as_f64().unwrap(),
            case["output_price"].as_f64().unwrap(),
        );
        let expected = &case["expected"];
        assert_eq!(
            estimate.transcription_cost,
            expected["transcription_cost"].as_f64().unwrap(),
            "transcription cost diverged for case {case}"
        );
        assert_eq!(
            estimate.cleanup_cost,
            expected["cleanup_cost"].as_f64().unwrap(),
            "cleanup cost diverged for case {case}"
        );
        assert_eq!(
            estimate.total_cost,
            expected["total_cost"].as_f64().unwrap(),
            "total cost diverged for case {case}"
        );
        assert_eq!(
            estimate.cleanup_input_tokens,
            expected["cleanup_input_tokens"].as_u64().unwrap(),
            "input tokens diverged for case {case}"
        );
        assert_eq!(
            estimate.cleanup_output_tokens,
            expected["cleanup_output_tokens"].as_u64().unwrap(),
            "output tokens diverged for case {case}"
        );
    }
}
