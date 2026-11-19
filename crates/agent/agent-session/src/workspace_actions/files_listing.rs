//! A workspace's files as its listing offers them: the folder, and each file in it by its path,
//! with Files' `read-text` on it.
//!
//! What a workspace holds is listed by one catalogue, its apps' documents and its own rows, and
//! an agent finds its way around the workspace by it. The folder is Files, one of the apps the
//! workspace's own rows offer (`app.so.tilo.file-explorer`), with its `list` beside its `open`,
//! and the files are the rows beneath it, each the resource [`fs_protocol::tree::file_resource`] names by its path. Reading one
//! is the read of the tree by that path ([`super::tree_put`]), answered by whoever holds the
//! tree: the file's content, the one place a file of the workspace is kept.

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{FILES_APP, LIST, READ_TEXT, file_resource, live_files};
use tilo_actions::remote::{
    APPLICATION_RESOURCE_KIND, APPLICATION_RESOURCE_PREFIX, Caller, CatalogueQuery, CatalogueRow,
    FOUND_FILE, workspace_provider,
};
use tilo_actions::{ActionParam, ParamType, api::ActionAddress, api::ActionDescriptor};
use tilo_server_protocol::WorkspaceId;

use super::Worker;

/// The resource the workspace's folder is listed as: the Files app's, which its verbs and reads
/// by a path are asked of.
fn folder_resource() -> String {
    format!("{APPLICATION_RESOURCE_PREFIX}{FILES_APP}")
}

/// A read of the workspace's tree, as a row offers it: `action` on `resource`.
fn read(resource: String, kind: &str, label: &str, action: &str) -> ActionDescriptor {
    let (action_label, description, params) = if action == LIST {
        (
            "List",
            "What a folder of the workspace holds, by its path; the workspace's own when none \
             is named.",
            vec![
                ActionParam::optional("folder", ParamType::Text, "The folder's path"),
                ActionParam::optional("from", ParamType::Integer, "Where the page starts"),
            ],
        )
    } else {
        (
            "Read text",
            "What the file says, a page of text at a time.",
            vec![ActionParam::optional(
                "from",
                ParamType::Integer,
                "The character the page starts at",
            )],
        )
    };
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress {
            resource,
            action: action.to_owned(),
        },
        resource_kind: kind.to_owned(),
        resource_label: label.to_owned(),
        action_label: action_label.to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: Some(description.to_owned()),
        locus: tilo_actions::ExecutionLocus::ServerPreferred,
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            kind: ui::permissions::policy::ActionKind::Read,
            confirmation: tilo_actions::ConfirmationPolicy::None,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
            ..tilo_actions::ActionContract::default()
        },
        params,
        input: tilo_actions::InputSpec::None,
        view: None,
    }
}

/// Whether `query` asks for what the workspace's folder holds, or for one of its files by its
/// action: a listing that reads the tree, which the query then waits for as a read of the
/// tree's documents waits ([`super::keys::catalogue_keys`]).
pub(super) fn wants_files(query: &CatalogueQuery) -> bool {
    query.parent_resource.as_deref() == Some(folder_resource().as_str())
        || query
            .target
            .as_ref()
            .is_some_and(|target| fs_protocol::tree::file_of_resource(&target.resource).is_some())
}

impl Worker {
    /// The rows of `workspace_id`'s folder `query` asks for, for `caller`: the folder at the top
    /// level, and its files beneath it, by their paths. None for a caller who may not read what
    /// Files keeps in the workspace. The files are read only for a query that asks for what the
    /// folder holds, or for one of them by its action, so a listing of the top level costs one
    /// row however many files there are.
    pub(super) fn file_rows(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        cluster_id: &str,
        query: &CatalogueQuery,
    ) -> Vec<CatalogueRow> {
        if !self.may_on_tree(workspace_id, caller, &[]) {
            return Vec::new();
        }
        let provider = workspace_provider(cluster_id);
        let folder = folder_resource();
        if !wants_files(query) {
            let mut row = CatalogueRow::for_action(
                provider,
                FILES_APP,
                read(folder, APPLICATION_RESOURCE_KIND, "Files", LIST),
            );
            row.has_children = true;
            return vec![row];
        }
        let Some(tree) = self.workspace_tree(workspace_id, cluster_id) else {
            return Vec::new();
        };
        live_files(&tree)
            .into_iter()
            .filter(|file| file.node.kind == FileNodeKind::File)
            .map(|file| {
                let mut row = CatalogueRow::for_action(
                    provider.clone(),
                    FILES_APP,
                    read(file_resource(&file.path), FOUND_FILE, &file.path, READ_TEXT),
                );
                row.parent_resource.clone_from(&folder);
                row
            })
            .collect()
    }
}
