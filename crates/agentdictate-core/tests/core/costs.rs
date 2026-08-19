use agentdictate_core::estimate_session_cost;

#[test]
fn session_cost_matches_the_existing_python_estimate() {
    let estimate = estimate_session_cost(
        90.0,
        "abcdabcdabcdabcdabcdabcdabcdabcdabcdabcd",
        Some("abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd"),
        true,
        0.006,
        1.0,
        2.0,
    );

    assert!((estimate.transcription_cost - 0.009).abs() < 1e-12);
    assert_eq!(estimate.cleanup_input_tokens, 10);
    assert_eq!(estimate.cleanup_output_tokens, 20);
    assert!((estimate.cleanup_cost - 0.00005).abs() < 1e-12);
    assert!((estimate.total_cost - 0.00905).abs() < 1e-12);
}
