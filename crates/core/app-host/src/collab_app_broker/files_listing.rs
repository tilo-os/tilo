//! A workspace's folder as its listing offers it: the folder itself, and each file and folder in
//! it by its path, with Files' `read-text` on a file and its `list` on a folder.
//!
//! What a workspace holds is listed by one catalogue, and an agent or a key finds its way around
//! the workspace by it. The folder is Files (`app.so.tilo.file-explorer`), offered beside its
//! `open` with its `list`, and the rows beneath it are its files ([`file_resource`]) and folders
//! ([`folder_resource`]), read from the tree a `files#open` puts files into
//! ([`crate::services::FilesTreePuts::entries`]). The workspace's own folder lists its
//! whole tree by paths; a folder inside it lists what it holds directly, so a listing goes into a
//! folder the way a person does.
//!
//! What a row offers is answered here too: its `list` and `read-text` read the tree this device
//! holds ([`crate::services::FilesTreePuts::read`]), the folder or file named by the row's
//! own resource, so a caller going through the listing calls what a row offers as it is.

use std::sync::Arc;

use serde_json::{Map, Value};

use fs_protocol::tree::{
    FILES_APP, FOLDER_KIND, LIST, READ_TEXT, file_of_resource, file_resource, folder_of_resource,
    folder_resource,
};
use tilo_action_policy::{ActionKind, Answer, TreePath};
use tilo_actions::remote::{
    APPLICATION_RESOURCE_KIND, APPLICATION_RESOURCE_PREFIX, ActionCallError, Caller, CataloguePage,
    CatalogueQuery, CatalogueRow, InvokeArgument, InvokeRequest, Pending, pending,
    provider_workspace,
};
use tilo_actions::{
    ActionAddress, ActionDescriptor, ActionFailure, ActionOutput, ActionParam, InvocationUpdate,
    ParamType,
};
use tilo_desktop_model::Workspace;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_records::search::FOUND_FILE;

use super::action_provider::refusal;
use super::host_action_admission::HostActionAdmission;
use super::{AppBrokerState, BrokerActor, InvocationCaller};

/// The resource the workspace's folder is listed as: the Files app's.
fn folder_root() -> String {
    format!("{APPLICATION_RESOURCE_PREFIX}{FILES_APP}")
}

/// Whether `actor` may read what Files keeps in `workspace` now: the tree's own records.
pub(super) fn may_read_tree(workspace: &Workspace, actor: &BrokerActor, now: u64) -> bool {
    let records = collab_types::document_node(
        &workspace.id().to_string(),
        FILES_APP,
        &fs_protocol::tree::document_id(),
    );
    actor.answer_in(workspace, &records, ActionKind::Read, now) == Answer::Allow
}

/// Whether `actor` may read the content `workspace` keeps by its address now: what a read of
/// the whole workspace lets it, as the content a message names is no file of the folder.
fn may_read_content(workspace: &Workspace, actor: &BrokerActor, now: u64) -> bool {
    actor.answer_in(workspace, &TreePath::root(), ActionKind::Read, now) == Answer::Allow
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
            kind: tilo_actions::ActionKind::Read,
            confirmation: tilo_actions::ConfirmationPolicy::None,
            // A folder's entries and a file's text are what the workspace's own records hold:
            // a caller's read of the row takes this as its read, and its words may come from
            // outside.
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

/// The row the workspace's folder is listed by at the top of the workspace, offered as
/// `provider`: Files with its `list`, holding the files beneath it.
pub(super) fn folder_row(provider: &str) -> CatalogueRow {
    let mut row = CatalogueRow::for_action(
        provider.to_owned(),
        FILES_APP,
        read(folder_root(), APPLICATION_RESOURCE_KIND, "Files", LIST),
    );
    row.has_children = true;
    row
}

/// The folder of the workspace's tree `query` asks what it holds: `""` for the workspace's own
/// folder, a folder's path for one inside it, and whether every entry beneath is listed.
fn listed_folder(query: &CatalogueQuery) -> Option<(String, bool)> {
    let parent = query.parent_resource.as_deref()?;
    if parent == folder_root() {
        return Some((String::new(), true));
    }
    folder_of_resource(parent).map(|folder| (folder.to_owned(), false))
}

/// The rows of `entries`, the folder `folder` lists, offered as `provider` beneath `parent`.
fn entry_rows(
    provider: &str,
    parent: &str,
    folder: &str,
    entries: Vec<crate::services::FilesEntry>,
) -> Vec<CatalogueRow> {
    entries
        .into_iter()
        .map(|entry| {
            // The workspace's whole tree is listed by paths, and a folder it goes into by the
            // names that folder shows.
            let label = if folder.is_empty() {
                entry.path.as_str()
            } else {
                entry
                    .path
                    .rsplit_once('/')
                    .map_or(entry.path.as_str(), |(_, leaf)| leaf)
            };
            let mut row = if entry.folder {
                let mut row = CatalogueRow::for_action(
                    provider.to_owned(),
                    FILES_APP,
                    read(folder_resource(&entry.path), FOLDER_KIND, label, LIST),
                );
                row.has_children = true;
                row
            } else {
                CatalogueRow::for_action(
                    provider.to_owned(),
                    FILES_APP,
                    read(file_resource(&entry.path), FOUND_FILE, label, READ_TEXT),
                )
            };
            parent.clone_into(&mut row.parent_resource);
            row
        })
        .collect()
}

/// The revision a page of `rows` is cut from: what the folder holds, so a cursor read before a
/// file came or went starts again.
fn revision_of(rows: &[CatalogueRow]) -> String {
    let mut hasher = blake3::Hasher::new();
    for row in rows {
        hasher.update(row.resource.id.as_bytes());
        hasher.update(b"\0");
    }
    hasher.finalize().to_hex().to_string()
}

impl AppBrokerState {
    /// The page of the workspace's folder `query` asks for, for `caller`: what a folder of the
    /// workspace's tree holds. `None` for a query that asks for no folder of the tree; an empty
    /// page for a file, which holds nothing listed, and for a caller who may not read what Files
    /// keeps in the workspace.
    pub(super) fn files_page(
        &self,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Option<Pending<Result<CataloguePage, ActionCallError>>> {
        let provider = query.provider.as_deref()?;
        let workspace = provider_workspace(provider)?;
        let parent = query.parent_resource.as_deref()?;
        if file_of_resource(parent).is_some() {
            return Some(Pending::ready(Ok(CataloguePage::from_rows(
                String::new(),
                Vec::new(),
                query,
            ))));
        }
        let (folder, below) = listed_folder(query)?;
        let Some(puts) = self.services.files_open.clone() else {
            return Some(Pending::ready(Err(ActionCallError::Unavailable {
                reason: "the workspace's folder is not served on this host".into(),
            })));
        };
        if !self
            .decide_for(caller, workspace, may_read_tree)
            .unwrap_or(false)
        {
            return Some(Pending::ready(Ok(CataloguePage::from_rows(
                String::new(),
                Vec::new(),
                query,
            ))));
        }
        let Some(id) = WorkspaceId::parse(workspace) else {
            return Some(Pending::ready(Err(ActionCallError::UnknownAction)));
        };
        let (completer, page) = pending();
        let (provider, parent, query) = (provider.to_owned(), parent.to_owned(), query.clone());
        // The tree is read on a thread of its own, as a put changes it: its read waits for the
        // workspace's records, which this device's runtime serves meanwhile.
        std::thread::spawn(move || {
            let answer = puts
                .entries(id, &folder, below)
                .map(|entries| {
                    let rows = entry_rows(&provider, &parent, &folder, entries);
                    CataloguePage::from_rows(revision_of(&rows), rows, &query)
                })
                .map_err(|reason| ActionCallError::Unavailable { reason });
            completer.complete(answer);
        });
        Some(page)
    }
}

/// The rows a broker that keeps `puts` adds to the top of `workspace`'s listing for `actor`.
pub(super) fn top_rows(
    puts: Option<&Arc<dyn crate::services::FilesTreePuts>>,
    (workspace, actor, now): (&Workspace, &BrokerActor, u64),
    provider: &str,
) -> Option<CatalogueRow> {
    (puts.is_some() && may_read_tree(workspace, actor, now)).then(|| folder_row(provider))
}

/// The read of the workspace's folder `action` on `resource` is, as a row offers it: the
/// descriptor it is admitted under, and the path the resource names, which a file's `read-text`
/// reads and a folder's `list` lists. `None` for a resource that is no folder or file of it.
fn read_target(resource: &str, action: &str) -> Option<(ActionDescriptor, Option<String>)> {
    if resource == folder_root() {
        let descriptor = read(folder_root(), APPLICATION_RESOURCE_KIND, "Files", action);
        return Some((descriptor, None));
    }
    let (kind, path) = match (file_of_resource(resource), folder_of_resource(resource)) {
        (Some(path), _) => (FOUND_FILE, path),
        (None, Some(path)) => (FOLDER_KIND, path),
        (None, None) => return None,
    };
    let label = path.rsplit_once('/').map_or(path, |(_, leaf)| leaf);
    let descriptor = read(resource.to_owned(), kind, label, action);
    Some((descriptor, Some(path.to_owned())))
}

/// The arguments `request` carries, with the path its resource names under `name`: what a row
/// offers names its folder or file itself, and its call needs no path besides.
fn read_arguments(
    request: &InvokeRequest,
    named: Option<String>,
    name: &str,
) -> Map<String, Value> {
    let mut arguments = match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        InvokeArgument::None | InvokeArgument::Text(_) => Map::new(),
    };
    if let Some(path) = named {
        arguments.insert(name.to_owned(), Value::String(path));
    }
    arguments
}

impl AppBrokerState {
    /// Files' `list` or `read-text` of the workspace's folder, as its listing offers them: read
    /// from the tree this device holds and answered as the call's output. `None` when the target
    /// is another action, or no folder or file of the workspace.
    ///
    /// # Errors
    /// A caller naming no workspace, one the workspace does not let read its files (or, for
    /// content named by its address, the workspace), an admission that fails, or a host that
    /// serves no folder.
    pub(super) fn start_files_read(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&InvocationCaller>,
        request: &InvokeRequest,
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
    ) -> Result<Option<u64>, ActionCallError> {
        let address = &request.target.address;
        let action = address.action.as_str();
        if action != LIST && action != READ_TEXT {
            return Ok(None);
        }
        let Some((descriptor, named)) = read_target(&address.resource, action) else {
            return Ok(None);
        };
        let workspace_id = workspace.id().to_string();
        if caller.workspace() != Some(workspace_id.as_str()) {
            tracing::warn!(
                "reading a workspace's files was refused: the caller names no such workspace"
            );
            return Err(ActionCallError::UnknownAction);
        }
        // The workspace's folder answers no revision of its own: a row is what its tree holds
        // now, and a listing carries the revision of the page it answered. A caller naming one
        // is naming a directory that has moved on, and is refused as a stale revision, as any
        // host refuses one.
        if request.revision.is_some() {
            return Err(ActionCallError::StaleRevision);
        }
        let arguments = read_arguments(
            request,
            named,
            if action == LIST { "folder" } else { "file" },
        );
        // Content a message names is read by its address, by whoever may read the workspace;
        // a file or folder of the tree by whoever may read what Files keeps in it.
        let allowed = if action == READ_TEXT && arguments.contains_key("content") {
            may_read_content(workspace, policy, now)
        } else {
            may_read_tree(workspace, policy, now)
        };
        if !allowed {
            return Err(ActionCallError::NotPermitted);
        }
        let puts =
            self.services.files_open.clone().ok_or_else(|| {
                refusal("the workspace's folder is not served on this host".into())
            })?;
        let Some(id) = WorkspaceId::parse(&workspace_id) else {
            return Err(ActionCallError::UnknownAction);
        };
        let admitted = self.admit_host_action(
            caller,
            origin,
            HostActionAdmission {
                workspace: &workspace_id,
                app_id: FILES_APP,
                descriptor: &descriptor,
                request,
            },
        )?;
        let call = admitted.id;
        let state = Arc::clone(self);
        let action = action.to_owned();
        // The tree is read on a thread of its own, as a listing of it is: its read waits for the
        // workspace's records, which this device's runtime serves meanwhile.
        std::thread::spawn(move || {
            let update = match puts.read(id, &action, &arguments) {
                Ok(value) => InvocationUpdate::Succeeded {
                    invocation: admitted.invocation,
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
                // What the folder answered instead is the call's own answer, in words its caller
                // reads.
                Err(reason) => InvocationUpdate::Failed {
                    invocation: admitted.invocation,
                    error: ActionFailure {
                        code: "files_read_failed".into(),
                        message: reason,
                        details: None,
                        retryable: false,
                    },
                },
            };
            if let Err(error) = state.complete_host_action(&admitted, update) {
                tracing::warn!(?error, "a read of the workspace's folder was not answered");
            }
        });
        Ok(Some(call))
    }
}

#[cfg(test)]
#[path = "files_listing_tests.rs"]
mod tests;
