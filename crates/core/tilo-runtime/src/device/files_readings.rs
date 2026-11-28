//! A folder of a workspace's tree, as this device publishes it to the views that follow it:
//! `files/folder~<path>`, and `files/folder~` for the workspace's own folder.
//!
//! The reading is what Files' own `list` answers — the folder, what it holds, and where the next
//! page starts — read where this device keeps the tree
//! ([`FilesTreePuts::read`](tilo_app_host::services::FilesTreePuts::read)) and made again
//! whenever the workspace takes a write of the tree, whichever device wrote it. A view renders
//! it, so what a person sees is the workspace's folder and never a copy of it.
//!
//! Who reads it is decided by the workspace's own grants on the tree's records, as the listing
//! the broker serves is decided ([`may_read`]): a caller the workspace does not let read its
//! files is answered nothing, and a watching view is asked again when the tree changes, so a
//! grant taken back is a folder that stops reading.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fs_protocol::tree::{self, FILES_APP};
use serde_json::{Map, Value};
use tilo_action_policy::Answer;
use tilo_action_types::ActionKind;
use tilo_app_host::services::Caller;
use tilo_app_host::services::{PublishedResources, Reading, ResourceWatch};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::workspaces::ResourceWrite;
use tokio::sync::{mpsc, oneshot};

use super::local_files::Folders;

/// The folders of the workspaces this device keeps, as it publishes them.
pub(crate) struct Readings {
    folders: Arc<dyn Folders>,
    /// Counts each reading published, so a follower can tell one from another.
    edits: Arc<AtomicU64>,
    /// The watches this provider minted, by id, each with the task that ends when it does.
    watches: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_watch: AtomicU64,
}

/// The folders of the workspaces `folders` keeps, as this device publishes them.
pub(crate) fn published(folders: &Arc<dyn Folders>) -> Arc<dyn PublishedResources> {
    Arc::new(Readings {
        folders: Arc::clone(folders),
        edits: Arc::new(AtomicU64::new(0)),
        watches: Mutex::default(),
        next_watch: AtomicU64::new(0),
    })
}

/// Whether `caller` may read what Files keeps of `workspace` now: the workspace's decision on
/// the tree's own records, as the broker's listing decides it. Who is asking is the caller the
/// instance's view was opened for, when the dispatch admitted one (an agent reading the app's
/// window, a page showing it); a person at their own window reads as the device's person.
fn may_read(folders: &dyn Folders, caller: &Caller, workspace: &str) -> bool {
    let Some(held) = folders.workspace(workspace) else {
        return false;
    };
    let actor = match &caller.opened_for {
        Some(opened) => tilo_broker_core::BrokerActor::for_caller(opened),
        None => match tilo_member_types::PersonId::parse(&folders.person()) {
            Ok(person) => tilo_broker_core::BrokerActor::person(person),
            Err(_) => return false,
        },
    };
    let records = collab_types::document_node(workspace, FILES_APP, &tree::document_id());
    actor.answer_in(&held, &records, ActionKind::Read, folders.now()) == Answer::Allow
}

/// What the folder `resource` names holds, as the workspace's tree has it now.
fn reading(folders: &dyn Folders, caller: &Caller, resource: &str) -> Result<Value, String> {
    let path = tree::folder_of_reading(resource)
        .ok_or_else(|| format!("`{resource}` is no folder of a workspace's tree"))?;
    let workspace = caller.workspace.as_str();
    if !may_read(folders, caller, workspace) {
        return Err("You can't read this workspace's files: it doesn't let you.".to_owned());
    }
    let id = WorkspaceId::parse(workspace).ok_or("that workspace has no id this device reads")?;
    let puts = folders
        .puts()
        .ok_or("The workspace's folder is not served on this computer.")?;
    let arguments = Map::from_iter([("folder".to_owned(), Value::String(path.to_owned()))]);
    let listing = puts.read(id, tree::LIST, &arguments)?;
    Ok(with_changed_as_times(listing))
}

/// `listing` with each entry's `changed` read as a time: the tree names it in milliseconds since
/// the epoch, and a view reads a `Time` as the instant it is, in RFC 3339 text.
fn with_changed_as_times(mut listing: Value) -> Value {
    let Some(entries) = listing.get_mut("entries").and_then(Value::as_array_mut) else {
        return listing;
    };
    for entry in entries {
        let Some(ms) = entry.get("changed").and_then(Value::as_i64) else {
            continue;
        };
        if let Some(at) = jiff::Timestamp::from_millisecond(ms)
            .ok()
            .map(|at| Value::String(at.to_string()))
        {
            if let Some(map) = entry.as_object_mut() {
                map.insert("changed".to_owned(), at);
            }
        }
    }
    listing
}

/// The bytes a reading travels as: what the folder holds, or why it does not read.
fn encoded(reading: Result<Value, String>, folder: &str) -> Option<Vec<u8>> {
    let value = match reading {
        Ok(value) => value,
        // A folder that does not read says why in its own field: a view draws that reason,
        // where a reading that has not arrived yet reads as the folder it has not filled.
        Err(why) => serde_json::json!({ "folder": folder, "error": why }),
    };
    match serde_json::to_vec(&value) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            tracing::warn!(%error, "a folder reading does not encode");
            None
        }
    }
}

/// Whether `write` changed the tree of `workspace`: a write of one of its records, the tree's
/// own or a chunk of it.
fn wrote_tree(write: &ResourceWrite, workspace: &str) -> bool {
    write.workspace == workspace
        && write.app == FILES_APP
        && tree::chunk_of_record(&write.record).is_some()
}

/// Wait until `workspace`'s tree changed: `Some` to read the folder again, and `None` when the
/// workspace's writes ended and no reading comes after.
async fn tree_wrote(
    writes: &mut tokio::sync::broadcast::Receiver<ResourceWrite>,
    workspace: &str,
) -> Option<()> {
    loop {
        match writes.recv().await {
            Ok(write) if !wrote_tree(&write, workspace) => {}
            // A write of the tree, or some that went by unheard: one of them may be its.
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return Some(()),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
        }
    }
}

impl PublishedResources for Readings {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        if tree::folder_of_reading(resource).is_none() {
            let _ = answer.send(Ok((0, None)));
            return answered;
        }
        let folders = Arc::clone(&self.folders);
        let caller = caller.clone();
        let folder = tree::folder_of_reading(resource)
            .unwrap_or_default()
            .to_owned();
        let resource = resource.to_owned();
        let revision = self.edits.load(Ordering::Relaxed);
        // The tree is read on a thread of its own, as the broker's listing reads it: its read
        // waits for the workspace's records, which this device's runtime serves meanwhile.
        std::thread::spawn(move || {
            let reading = encoded(reading(&*folders, &caller, &resource), &folder);
            let _ = answer.send(Ok((revision, reading)));
        });
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if tree::folder_of_reading(resource).is_none() {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        let workspace = caller.workspace.clone();
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        let folders = Arc::clone(&self.folders);
        let edits = Arc::clone(&self.edits);
        let caller = caller.clone();
        let folder = tree::folder_of_reading(resource)
            .unwrap_or_default()
            .to_owned();
        let resource: Arc<str> = Arc::from(resource);
        let task = tilo_rt::app().handle().spawn(async move {
            // Follow the workspace's writes before reading it, so a change between the two is
            // not missed: each write of the tree makes the reading again.
            let mut writes = folders.writes();
            loop {
                // The tree is read on a thread of its own, as the broker's listing reads it:
                // its read waits for the workspace's records, which the runtime serves meanwhile.
                let (read, read_now) = oneshot::channel();
                let (keeper, asked, at, named) = (
                    Arc::clone(&folders),
                    caller.clone(),
                    resource.clone(),
                    folder.clone(),
                );
                let spawned = std::thread::Builder::new()
                    .name("tilo-folder-reading".to_owned())
                    .spawn(move || {
                        let _ = read.send(encoded(reading(&*keeper, &asked, &at), &named));
                    });
                let held = match spawned {
                    Ok(_) => read_now.await.ok().flatten(),
                    Err(error) => {
                        tracing::warn!(%error, "a folder reading is not read");
                        None
                    }
                };
                if sink
                    .send(runtime_collab::wire::Event::Reading {
                        resource: resource.to_string(),
                        revision: edits.fetch_add(1, Ordering::Relaxed),
                        reading: held,
                    })
                    .is_err()
                {
                    return;
                }
                if tree_wrote(&mut writes, &workspace).await.is_none() {
                    return;
                }
            }
        });
        self.watches.lock_recover().insert(id, task.abort_handle());
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        if let Some(task) = self.watches.lock_recover().remove(&watch.id()) {
            task.abort();
        }
    }

    fn invoke(
        &self,
        _caller: &Caller,
        resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: format!("`{resource}` answers no `{action}`: a folder is read, and changed through Files' verbs"),
        });
        answered
    }
}
