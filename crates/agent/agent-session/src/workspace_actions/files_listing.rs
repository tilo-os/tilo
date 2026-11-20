//! A workspace's files as its listing offers them: the folder, and each file and folder in it by
//! its path, with Files' `read-text` on a file and its `list` on a folder.
//!
//! What a workspace holds is listed by one catalogue, its apps' documents and its own rows, and
//! an agent finds its way around the workspace by it. The folder is Files, one of the apps the
//! workspace's own rows offer (`app.so.tilo.file-explorer`), with its `list` beside its `open`,
//! and the files are the rows beneath it, each the resource [`fs_protocol::tree::file_resource`]
//! names by its path, and the folders the resources
//! [`fs_protocol::tree::folder_resource`] names. Reading a file is the read of the tree by that
//! path ([`super::tree_put`]), answered by whoever holds the tree: the file's content, the one
//! place a file of the workspace is kept. A folder's row holds what is in it, so a listing goes
//! into a folder the way a person does.

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{
    FILES_APP, FOLDER_KIND, LIST, READ_TEXT, file_resource, folder_of_resource, folder_resource,
    live_files,
};
use tilo_actions::remote::{
    APPLICATION_RESOURCE_KIND, APPLICATION_RESOURCE_PREFIX, Caller, CatalogueQuery, CatalogueRow,
    FOUND_FILE, workspace_provider,
};
use tilo_actions::{ActionParam, ParamType, api::ActionAddress, api::ActionDescriptor};
use tilo_server_protocol::WorkspaceId;

use super::Worker;

/// The resource the workspace's folder is listed as: the Files app's, which its verbs and reads
/// by a path are asked of.
fn folder_resource_root() -> String {
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
            // A folder's entries and a file's text are what the workspace's own document
            // holds: a caller's `tilo_read` of the row takes this as its read, and its words
            // may come from outside.
            tags: vec![tilo_actions::DOCUMENT_READ_TAG.to_owned()],
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

/// Whether `query` asks for what a folder of the workspace's tree holds, or for one of its
/// files by its action: a listing that reads the tree, which the query then waits for as a read
/// of the tree's documents waits ([`super::keys::catalogue_keys`]).
pub(super) fn wants_files(query: &CatalogueQuery) -> bool {
    query.parent_resource.as_deref().is_some_and(|parent| {
        parent == folder_resource_root().as_str() || folder_of_resource(parent).is_some()
    }) || query
        .target
        .as_ref()
        .is_some_and(|target| fs_protocol::tree::file_of_resource(&target.resource).is_some())
}

/// The folder part of a file's path in the workspace (`Plans/Launch` of
/// `Plans/Launch/notes.md`), empty for one at the root.
fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

impl Worker {
    /// The rows of the folder `query` asks for, for `caller`: the workspace's own folder at the
    /// top level, and beneath it each file and folder by its path — the workspace's whole tree
    /// for its own folder, and a folder's own entries for a folder inside it. None for a caller
    /// who may not read what Files keeps in the workspace. The tree is read only for a query
    /// that asks for what a folder holds, or for one of its files by its action, so a listing of
    /// the top level costs one row however many files there are.
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
        // A file holds nothing that is listed: what a query under it asks for is nothing.
        if query
            .parent_resource
            .as_deref()
            .is_some_and(|parent| fs_protocol::tree::file_of_resource(parent).is_some())
        {
            return Vec::new();
        }
        let provider = workspace_provider(cluster_id);
        if !wants_files(query) {
            let mut row = CatalogueRow::for_action(
                provider,
                FILES_APP,
                read(
                    folder_resource_root(),
                    APPLICATION_RESOURCE_KIND,
                    "Files",
                    LIST,
                ),
            );
            row.has_children = true;
            return vec![row];
        }
        let Some(tree) = self.workspace_tree(workspace_id, cluster_id) else {
            return Vec::new();
        };
        // The folder this query lists: the workspace's own for its own resource, and a folder
        // inside it by its path, whose entries are the ones it holds directly.
        let listed_as = query
            .parent_resource
            .clone()
            .unwrap_or_else(folder_resource_root);
        let inside = folder_of_resource(&listed_as).map(str::to_owned);
        live_files(&tree)
            .into_iter()
            .filter(|file| {
                inside
                    .as_deref()
                    .is_none_or(|dir| parent_path(&file.path) == dir)
            })
            .map(|file| {
                // The workspace's whole tree is listed by paths, and a folder it goes into by
                // the names that folder shows.
                let label = match (&inside, file.path.rsplit_once('/')) {
                    (Some(_), Some((_, leaf))) => leaf,
                    _ => file.path.as_str(),
                };
                let mut row = if file.node.kind == FileNodeKind::Directory {
                    let mut row = CatalogueRow::for_action(
                        provider.clone(),
                        FILES_APP,
                        read(folder_resource(&file.path), FOLDER_KIND, label, LIST),
                    );
                    row.has_children = true;
                    row
                } else {
                    CatalogueRow::for_action(
                        provider.clone(),
                        FILES_APP,
                        read(file_resource(&file.path), FOUND_FILE, label, READ_TEXT),
                    )
                };
                row.parent_resource.clone_from(&listed_as);
                row
            })
            .collect()
    }
}
