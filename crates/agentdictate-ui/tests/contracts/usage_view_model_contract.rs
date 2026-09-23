//! Usage presentation contracts.

use agentdictate_ui::{UsagePeriod, UsageTotals, UsageViewModel};

#[test]
fn selected_period_totals_expose_weighted_average_words_per_minute() {
    let usage = UsageViewModel::new(
        UsagePeriod::Last7Days,
        UsageTotals {
            dictations: 3,
            words: 240,
            audio_seconds: 120,
            estimated_cost_usd: 0.08,
        },
        Vec::new(),
    );

    assert_eq!(usage.average_wpm_value(), "120");

    let month = UsageViewModel::new(
        UsagePeriod::Last30Days,
        UsageTotals {
            dictations: 8,
            words: 900,
            audio_seconds: 300,
            estimated_cost_usd: 0.24,
        },
        Vec::new(),
    );
    assert_eq!(month.average_wpm_value(), "180");

    let no_audio = UsageViewModel::new(
        UsagePeriod::AllTime,
        UsageTotals {
            words: 240,
            audio_seconds: 0,
            ..UsageTotals::default()
        },
        Vec::new(),
    );
    assert_eq!(no_audio.average_wpm_value(), "0");
}
