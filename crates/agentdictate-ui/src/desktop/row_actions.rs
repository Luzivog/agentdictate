use gpui::{Context, SharedString, prelude::*};
use gpui_component::{Sizable, button::Button};

use crate::{WorkspaceAction, action::action_button};

use super::SettingsShell;

/// A transcript's Copy button. It reads "Copied ✓" while `copied`, and its
/// selector gains a `copied-` prefix so rendered tests can see that state.
pub(super) fn copy_button(id: i64, copied: bool, cx: &mut Context<SettingsShell>) -> Button {
    let action = WorkspaceAction::CopyTranscript { id };
    let selector = action.selector();
    let rendered_selector = if copied {
        format!("copied-{selector}")
    } else {
        selector.clone()
    };
    action_button(SharedString::from(selector))
        .debug_selector(move || rendered_selector)
        .small()
        .label(if copied { "Copied ✓" } else { "Copy" })
        .on_click(cx.listener(move |shell, _, _, cx| {
            shell.emit_workspace_action(action.clone(), cx);
            cx.notify();
        }))
}
