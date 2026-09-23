//! Shell view-model contracts.

use agentdictate_core::{
    AppSnapshot, DesktopReadiness, ExposedInput, HotkeyReadiness, MissingTool, Readiness,
    WorkflowPhase, WorkflowSnapshot,
};
use agentdictate_ui::{HomeStatus, Route, ShellViewModel};

fn readiness(change: impl FnOnce(&mut Readiness)) -> Readiness {
    let mut readiness = Readiness {
        shortcut: HotkeyReadiness::Ready,
        transcription_key: true,
        desktop: DesktopReadiness::default(),
    };
    change(&mut readiness);
    readiness
}

#[test]
fn home_says_ready_with_the_shortcut_or_shows_the_most_important_fix() {
    assert_eq!(
        HomeStatus::new(&readiness(|_| {}), "Ctrl+Alt+D"),
        HomeStatus::Ready {
            shortcut: "Ctrl+Alt+D".to_owned()
        }
    );
    assert_eq!(
        HomeStatus::new(
            &readiness(|readiness| readiness.shortcut = HotkeyReadiness::Starting),
            "Ctrl+Space"
        ),
        HomeStatus::Starting
    );

    // Nothing works without a key, so it outranks a slower upload.
    let HomeStatus::Fix(fix) = HomeStatus::new(
        &readiness(|readiness| {
            readiness.transcription_key = false;
            readiness.desktop.missing_tools = vec![MissingTool::Ffmpeg];
        }),
        "Ctrl+Space",
    ) else {
        panic!("a missing key needs a fix");
    };
    assert_eq!(fix.title, "Add your OpenAI API key");
    assert!(fix.opens_settings);

    let HomeStatus::Fix(fix) = HomeStatus::new(
        &readiness(|readiness| {
            readiness.desktop.exposed_input = Some(ExposedInput {
                rule: Some("/etc/udev/rules.d/99-vibetyper-uinput.rules".into()),
            });
            readiness.desktop.missing_tools = vec![MissingTool::Ffmpeg];
        }),
        "Ctrl+Space",
    ) else {
        panic!("world-readable keyboards need a fix");
    };
    assert_eq!(fix.title, "Other apps can read your keyboard");
    assert_eq!(
        fix.detail,
        "Run agentdictate setup-access or remove the rule from /etc/udev/rules.d/99-vibetyper-uinput.rules."
    );
}

#[test]
fn the_status_snapshot_brings_readiness_and_the_recovery_count() {
    let snapshot = AppSnapshot {
        workflow: WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
        readiness: readiness(|readiness| readiness.transcription_key = false),
        recoverable_count: 3,
        overlay_unavailable: false,
        history_set_aside: None,
    };

    let model = ShellViewModel::from_app_snapshot(Route::Home, snapshot.clone());

    assert_eq!(model.workspace.readiness, snapshot.readiness);
    assert_eq!(model.workspace.history.recovery.item_count, 3);
    assert_eq!(model.workspace.daemon_banner(), None);
}

#[test]
fn selecting_a_route_updates_the_route_and_navigation_atomically() {
    let mut model = ShellViewModel::new(Route::Home);

    model.select_route(Route::Settings);

    assert_eq!(model.active_route, Route::Settings);
    assert!(
        model
            .navigation
            .iter()
            .find(|item| item.route == Route::Settings)
            .expect("settings navigation item")
            .is_active
    );
    assert_eq!(
        model
            .navigation
            .iter()
            .filter(|item| item.is_active)
            .count(),
        1
    );
}
