#![cfg(feature = "test-support")]

use agentdictate_ui::AgentDictateAssets;
use gpui::AssetSource;

#[test]
fn desktop_controls_ship_every_icon_they_request() {
    let assets = AgentDictateAssets;

    for path in [
        "icons/chevron-down.svg",
        "icons/eye.svg",
        "icons/inbox.svg",
        "icons/minus.svg",
        "icons/plus.svg",
    ] {
        let bytes = assets
            .load(path)
            .unwrap_or_else(|error| panic!("{path} should load: {error}"))
            .unwrap_or_else(|| panic!("{path} should be embedded"));
        let svg = std::str::from_utf8(&bytes)
            .unwrap_or_else(|error| panic!("{path} should be UTF-8 SVG: {error}"));
        assert!(svg.starts_with("<svg"), "{path} did not contain an SVG");
        assert!(
            svg.contains("currentColor"),
            "{path} must inherit control color"
        );
    }

    let listed = assets.list("icons").expect("icon list should load");
    assert!(listed.iter().any(|path| path == "minus.svg"));
    assert!(listed.iter().any(|path| path == "plus.svg"));
    assert!(listed.iter().any(|path| path == "chevron-down.svg"));
}
