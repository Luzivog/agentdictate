//! Usage presentation contracts.

use agentdictate_ui::{UsageDayViewModel, UsagePeriod, UsageTotals, UsageViewModel};

#[test]
fn thirty_day_activity_uses_sparse_readable_axis_labels() {
    let activity = (0..30)
        .map(|day| UsageDayViewModel::new(format!("Aug {}", day + 1), 1, 10, 5, 0.01))
        .collect();
    let usage = UsageViewModel::new(UsagePeriod::Last30Days, UsageTotals::default(), activity);

    let visible = (0..30)
        .filter(|index| usage.axis_label_visible(*index))
        .collect::<Vec<_>>();

    assert_eq!(visible, vec![0, 6, 12, 18, 24, 29]);
}

#[test]
fn seven_day_activity_labels_every_day() {
    let activity = (0..7)
        .map(|day| UsageDayViewModel::new(format!("Aug {}", day + 1), 1, 10, 5, 0.01))
        .collect();
    let usage = UsageViewModel::new(UsagePeriod::Last7Days, UsageTotals::default(), activity);

    assert!((0..7).all(|index| usage.axis_label_visible(index)));
}

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
