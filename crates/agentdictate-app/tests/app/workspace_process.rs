use std::path::Path;

use agentdictate_app::{AgentProcess, AppPaths};
use agentdictate_core::{ClientCommand, ReplacementRule, ServerMessageKind};
use agentdictate_runtime::IpcHandler;
use tempfile::tempdir;

#[test]
fn workspace_replacement_commands_return_the_refreshed_projection() {
    let directory = tempdir().unwrap();
    let mut process = AgentProcess::open(app_paths(directory.path())).unwrap();

    let initial = process.handle(ClientCommand::get_workspace(1));
    let ServerMessageKind::Workspace { workspace, .. } = initial.kind else {
        panic!("workspace query should return workspace data");
    };
    assert!(workspace.replacements.is_empty());

    let created = process.handle(ClientCommand::create_replacement(
        2,
        ReplacementRule {
            id: None,
            source_phrase: "kube cuddle".into(),
            replacement_phrase: "kubectl".into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        },
    ));
    let ServerMessageKind::Workspace { workspace, .. } = created.kind else {
        panic!("successful mutation should refresh workspace data");
    };
    assert_eq!(workspace.replacements.len(), 1);
    assert_eq!(workspace.replacements[0].replacement_phrase, "kubectl");

    let id = workspace.replacements[0].id.unwrap();
    let deleted = process.handle(ClientCommand::delete_replacement(3, id));
    let ServerMessageKind::Workspace { workspace, .. } = deleted.kind else {
        panic!("successful deletion should refresh workspace data");
    };
    assert!(workspace.replacements.is_empty());
}

#[test]
fn blank_replacement_is_rejected_without_mutating_workspace() {
    let directory = tempdir().unwrap();
    let mut process = AgentProcess::open(app_paths(directory.path())).unwrap();

    let response = process.handle(ClientCommand::create_replacement(
        4,
        ReplacementRule {
            id: None,
            source_phrase: "  ".into(),
            replacement_phrase: "ignored".into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        },
    ));

    assert!(matches!(
        response.kind,
        ServerMessageKind::CommandRejected { .. }
    ));
}

#[test]
fn catalog_refresh_returns_a_bundled_workspace_without_waiting_for_the_network() {
    let directory = tempdir().unwrap();
    let mut process = AgentProcess::open(app_paths(directory.path())).unwrap();

    let response = process.handle(ClientCommand::refresh_model_catalog(5));

    let ServerMessageKind::Workspace { workspace, .. } = response.kind else {
        panic!("workspace query should return workspace data");
    };
    assert_eq!(
        workspace.model_catalog.status,
        agentdictate_core::ModelCatalogStatus::Builtin
    );
    assert!(
        workspace
            .model_catalog
            .transcription_models
            .iter()
            .any(|model| model.id == "gpt-transcribe")
    );
    assert!(
        workspace
            .model_catalog
            .cleanup_models
            .iter()
            .any(|model| model.id == "gpt-5.4-nano")
    );
}

fn app_paths(root: &Path) -> AppPaths {
    AppPaths::from_roots(
        root.join("config"),
        root.join("data"),
        root.join("state"),
        root.join("cache"),
        root.join("runtime"),
    )
}
