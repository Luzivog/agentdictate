//! Usage presentation contracts.

use agentdictate_ui::{UsagePeriod, UsageTotals, UsageViewModel};

#[test]
fn usage_costs_follow_the_configured_currency_without_losing_usd_compatibility() {
    let totals = UsageTotals {
        estimated_cost_usd: 1.25,
        ..UsageTotals::default()
    };

    assert_eq!(
        UsageViewModel::new(UsagePeriod::Last30Days, totals, Vec::new()).cost_value(),
        "$1.25",
    );
    let euros =
        UsageViewModel::new(UsagePeriod::Last30Days, totals, Vec::new()).with_currency("eur");
    assert_eq!(euros.currency, "EUR");
    assert_eq!(euros.cost_value(), "€1.25");
    assert_eq!(euros.format_cost(0.4), "€0.40");

    let canadian =
        UsageViewModel::new(UsagePeriod::Last30Days, totals, Vec::new()).with_currency("CAD");
    assert_eq!(canadian.cost_value(), "CAD 1.25");
}

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
