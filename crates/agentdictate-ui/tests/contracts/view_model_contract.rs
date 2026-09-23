//! Shell view-model contracts.

use agentdictate_core::{
    AppSnapshot, DesktopReadiness, ExposedInput, HotkeyReadiness, MissingTool, Readiness,
    WorkflowPhase, WorkflowSnapshot,
};
use agentdictate_ui::{HomeStatus, Route, ShellViewModel, needs_setup};

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
    assert!(fix.opens_setup);

    let HomeStatus::Fix(fix) = HomeStatus::new(
        &readiness(|readiness| {
            readiness.desktop.exposed_input = Some(ExposedInput {
                rule: Some("/etc/udev/rules.d/99-open-input.rules".into()),
            });
            readiness.desktop.missing_tools = vec![MissingTool::Ffmpeg];
        }),
        "Ctrl+Space",
    ) else {
        panic!("world-readable keyboards need a fix");
    };
    assert_eq!(fix.title, "Other apps can read your keyboard");
    assert!(
        fix.detail
            .starts_with("/etc/udev/rules.d/99-open-input.rules lets every app")
    );
    assert_eq!(
        fix.command.as_deref(),
        Some("sudo rm /etc/udev/rules.d/99-open-input.rules")
    );
    // Granting access can't override another app's rule.
    assert!(!fix.opens_setup);
}

/// The rule another package installed wins over AgentDictate's, so the
/// card asks to delete it first, with a command that is safe to paste.
#[test]
fn the_exposed_input_fix_deletes_the_open_rule_before_setup_access() {
    let fix = |rule: Option<&str>| {
        let HomeStatus::Fix(fix) = HomeStatus::new(
            &readiness(|readiness| {
                readiness.desktop.exposed_input = Some(ExposedInput {
                    rule: rule.map(Into::into),
                });
            }),
            "Ctrl+Space",
        ) else {
            panic!("world-readable keyboards need a fix");
        };
        fix
    };

    let unusual = fix(Some("/etc/udev/rules.d/my input's.rules"));
    assert_eq!(
        unusual.command.as_deref(),
        Some(r"sudo rm '/etc/udev/rules.d/my input'\''s.rules'")
    );
    assert!(
        unusual
            .detail
            .ends_with("then run agentdictate setup-access.")
    );

    // Without a rule to delete, granting access in Setup applies the
    // private mode again.
    let unknown = fix(None);
    assert_eq!(unknown.command, None);
    assert!(unknown.opens_setup);
}

#[test]
fn setup_opens_until_the_key_the_shortcut_and_pasting_all_work() {
    assert!(!needs_setup(&readiness(|_| {})));
    // A shortcut still starting, a slower upload or a world-readable
    // keyboard don't stop dictation.
    assert!(!needs_setup(&readiness(|readiness| {
        readiness.shortcut = HotkeyReadiness::Starting;
        readiness.desktop.missing_tools = vec![MissingTool::Ffmpeg];
        readiness.desktop.exposed_input = Some(ExposedInput { rule: None });
    })));
    for broken in [
        readiness(|readiness| readiness.transcription_key = false),
        readiness(|readiness| {
            readiness.shortcut = HotkeyReadiness::Unavailable {
                message: "no readable keyboard".to_owned(),
            };
        }),
        readiness(|readiness| readiness.desktop.paste_access = false),
    ] {
        assert!(needs_setup(&broken), "{broken:?}");
    }
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

    let model = ShellViewModel::from_app_snapshot(snapshot.clone());

    // Without a key, the window opens on Setup.
    assert_eq!(model.active_route, Route::Setup);
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
