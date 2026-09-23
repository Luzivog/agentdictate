use agentdictate_linux::paste::{
    DeliveryAction, DeliveryFailure, DeliveryObservation, DeliveryResult, FocusTarget,
    PasteDelivery, PasteShortcut, ShortcutMode,
};

#[test]
fn paste_is_not_injected_until_clipboard_readiness_is_observed() {
    let target = FocusTarget::x11(42, "chatgpt Chatgpt");
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);

    assert_eq!(delivery.action(), DeliveryAction::ObserveFocus);
    assert_eq!(
        delivery.advance(DeliveryObservation::Focus(target.clone())),
        DeliveryAction::PublishClipboard
    );
    assert_eq!(
        delivery.advance(DeliveryObservation::ClipboardReady),
        DeliveryAction::ObserveFocus
    );
    assert_eq!(
        delivery.advance(DeliveryObservation::Focus(target.clone())),
        DeliveryAction::InjectPaste {
            target,
            shortcut: PasteShortcut::Standard,
        }
    );
}

#[test]
fn auto_mode_uses_universal_paste_for_every_target() {
    for target in [
        FocusTarget::x11(42, "chatgpt Chatgpt"),
        FocusTarget::x11(84, "kitty kitty"),
        FocusTarget::wayland(),
    ] {
        let mut delivery = PasteDelivery::new(ShortcutMode::Auto);
        delivery.advance(DeliveryObservation::Focus(target.clone()));
        delivery.advance(DeliveryObservation::ClipboardReady);

        assert_eq!(
            delivery.advance(DeliveryObservation::Focus(target.clone())),
            DeliveryAction::InjectPaste {
                target,
                shortcut: PasteShortcut::Universal,
            }
        );
    }
}

#[test]
fn ambiguous_injection_failure_is_final_and_never_retried() {
    let target = FocusTarget::x11(42, "chatgpt Chatgpt");
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(target.clone()));
    delivery.advance(DeliveryObservation::ClipboardReady);
    delivery.advance(DeliveryObservation::Focus(target.clone()));

    let finished = DeliveryAction::Finished(DeliveryResult {
        copied: true,
        paste_triggered: false,
        consumed: false,
        failure: Some(DeliveryFailure::InjectionAmbiguous),
    });
    assert_eq!(
        delivery.advance(DeliveryObservation::InjectionFailed),
        finished
    );
    assert_eq!(
        delivery.advance(DeliveryObservation::Focus(target)),
        finished
    );
}

#[test]
fn deadline_after_injection_begins_is_ambiguous_not_safe_to_retry() {
    let target = FocusTarget::wayland();
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(target.clone()));
    delivery.advance(DeliveryObservation::ClipboardReady);
    delivery.advance(DeliveryObservation::Focus(target));

    assert_eq!(
        delivery.advance(DeliveryObservation::DeadlineReached),
        DeliveryAction::Finished(DeliveryResult {
            copied: true,
            paste_triggered: false,
            consumed: false,
            failure: Some(DeliveryFailure::InjectionAmbiguous),
        })
    );
}

#[test]
fn deadline_with_changing_focus_keeps_the_copy_but_skips_paste() {
    let mut delivery = PasteDelivery::new(ShortcutMode::Auto);
    delivery.advance(DeliveryObservation::Focus(FocusTarget::x11(
        42,
        "chatgpt Chatgpt",
    )));
    delivery.advance(DeliveryObservation::ClipboardReady);
    delivery.advance(DeliveryObservation::Focus(FocusTarget::x11(
        84,
        "kitty kitty",
    )));

    assert_eq!(
        delivery.advance(DeliveryObservation::DeadlineReached),
        DeliveryAction::Finished(DeliveryResult {
            copied: true,
            paste_triggered: false,
            consumed: false,
            failure: Some(DeliveryFailure::FocusUnstable),
        })
    );
}

#[test]
fn clipboard_readiness_deadline_reports_that_nothing_was_copied() {
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(FocusTarget::wayland()));

    assert_eq!(
        delivery.advance(DeliveryObservation::DeadlineReached),
        DeliveryAction::Finished(DeliveryResult {
            copied: false,
            paste_triggered: false,
            consumed: false,
            failure: Some(DeliveryFailure::ClipboardUnavailable),
        })
    );
}

#[test]
fn clipboard_failure_never_attempts_paste() {
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(FocusTarget::wayland()));

    assert_eq!(
        delivery.advance(DeliveryObservation::ClipboardUnavailable),
        DeliveryAction::Finished(DeliveryResult {
            copied: false,
            paste_triggered: false,
            consumed: false,
            failure: Some(DeliveryFailure::ClipboardUnavailable),
        })
    );
}

#[test]
fn x11_focus_identity_is_the_window_id_not_mutable_class_metadata() {
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(FocusTarget::x11(42, "")));
    delivery.advance(DeliveryObservation::ClipboardReady);
    let same_window = FocusTarget::x11(42, "chatgpt Chatgpt");

    assert_eq!(
        delivery.advance(DeliveryObservation::Focus(same_window.clone())),
        DeliveryAction::InjectPaste {
            target: same_window,
            shortcut: PasteShortcut::Standard,
        }
    );
}

/// The published text serves X11 and Wayland windows alike, so a focus
/// change only needs a second look at the new window before the paste.
#[test]
fn a_focus_change_is_confirmed_by_a_second_look_before_pasting() {
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(FocusTarget::x11(
        42,
        "chatgpt Chatgpt",
    )));
    delivery.advance(DeliveryObservation::ClipboardReady);

    assert_eq!(
        delivery.advance(DeliveryObservation::Focus(FocusTarget::wayland())),
        DeliveryAction::ObserveFocus
    );
    assert_eq!(
        delivery.advance(DeliveryObservation::Focus(FocusTarget::wayland())),
        DeliveryAction::InjectPaste {
            target: FocusTarget::wayland(),
            shortcut: PasteShortcut::Standard,
        }
    );
}

#[test]
fn sent_paste_completes_as_copied_triggered_and_acknowledged_by_the_target() {
    let target = FocusTarget::wayland();
    let mut delivery = PasteDelivery::new(ShortcutMode::Standard);
    delivery.advance(DeliveryObservation::Focus(target.clone()));
    delivery.advance(DeliveryObservation::ClipboardReady);
    delivery.advance(DeliveryObservation::Focus(target));

    assert_eq!(
        delivery.advance(DeliveryObservation::PasteSent { consumed: true }),
        DeliveryAction::Finished(DeliveryResult {
            copied: true,
            paste_triggered: true,
            consumed: true,
            failure: None,
        })
    );
}

#[test]
fn explicit_modes_pin_their_shortcut_regardless_of_window_class() {
    let terminal = FocusTarget::x11(84, "kitty kitty");
    let mut standard = PasteDelivery::new(ShortcutMode::Standard);
    standard.advance(DeliveryObservation::Focus(terminal.clone()));
    standard.advance(DeliveryObservation::ClipboardReady);
    assert_eq!(
        standard.advance(DeliveryObservation::Focus(terminal.clone())),
        DeliveryAction::InjectPaste {
            target: terminal.clone(),
            shortcut: PasteShortcut::Standard,
        }
    );

    let mut forced_terminal = PasteDelivery::new(ShortcutMode::Terminal);
    forced_terminal.advance(DeliveryObservation::Focus(terminal.clone()));
    forced_terminal.advance(DeliveryObservation::ClipboardReady);
    assert_eq!(
        forced_terminal.advance(DeliveryObservation::Focus(terminal.clone())),
        DeliveryAction::InjectPaste {
            target: terminal,
            shortcut: PasteShortcut::Terminal,
        }
    );
}
