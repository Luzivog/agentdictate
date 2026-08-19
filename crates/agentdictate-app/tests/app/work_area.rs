use agentdictate_app::parse_x11_work_area;
use agentdictate_ui::LogicalRect;

#[test]
fn x11_work_area_selects_the_current_desktop_instead_of_raw_monitor_bounds() {
    let output = r#"
_NET_CURRENT_DESKTOP(CARDINAL) = 1
_NET_WORKAREA(CARDINAL) = 0, 32, 1920, 1000, 1920, 32, 1920, 1000
"#;

    assert_eq!(
        parse_x11_work_area(output),
        Some(LogicalRect::new(1920, 32, 1920, 1000))
    );
}

#[test]
fn malformed_work_area_is_never_treated_as_geometry() {
    assert_eq!(parse_x11_work_area("_NET_WORKAREA = unavailable"), None);
}
