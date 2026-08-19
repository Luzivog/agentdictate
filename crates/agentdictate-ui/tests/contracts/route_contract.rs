//! Route contracts.

use std::str::FromStr;

use agentdictate_ui::Route;

#[test]
fn routes_have_stable_navigation_order_and_deep_link_slugs() {
    assert_eq!(
        Route::ALL,
        [
            Route::Overview,
            Route::History,
            Route::Replacements,
            Route::Settings,
        ]
    );
    assert_eq!(Route::Overview.title(), "Overview");
    assert_eq!(Route::History.slug(), "history");
    assert_eq!(Route::from_str("replacements"), Ok(Route::Replacements));
    assert!(Route::from_str("unknown").is_err());
}

#[test]
fn navigation_destinations_expose_stable_ids_and_accessible_labels() {
    assert_eq!(Route::Overview.navigation_id(), "nav-overview");
    assert_eq!(
        Route::Replacements.accessibility_label(),
        "Open Replacements"
    );
}
