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
use tilo_action_policy::{Answer, Context};
use tilo_action_types::ActionKind;
use tilo_app_host::services::Caller;
use tilo_app_host::services::{PublishedResources, Reading, ResourceWatch};
use tilo_lock::MutexRecover as _;
use tilo_member_types::Acting;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::files::access;
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
    if actor.answer_in(&held, &records, ActionKind::Read, folders.now()) == Answer::Allow {
        return true;
    }
    // A person let into one item of the tree alone reads that item: the listing shows only
    // theirs, and says the folder is empty of the rest.
    let person = reader_person(folders, caller);
    let acting = Acting::person(person);
    tree_of(folders, workspace).is_some_and(|tree| {
        access::tree_shares(&held.policy, &tree)
            .iter()
            .any(|share| access::grantee_reaches(&share.grantee, &acting))
    })
}

/// The person `caller` reads as, by their key: whose policy decides what the view reads. The
/// device's own person when the window is theirs, and whoever the instance was opened for.
fn reader_person(folders: &dyn Folders, caller: &Caller) -> String {
    caller
        .opened_for
        .as_ref()
        .and_then(|opened| opened.acting().person_id())
        .map_or_else(|| folders.person(), str::to_owned)
}

/// Every node of `workspace`'s tree as `(id, path, folder)`: the items a share can be given on,
/// each by where it stands now.
fn tree_of(folders: &dyn Folders, workspace: &str) -> Option<Vec<(String, String, bool)>> {
    let puts = folders.puts()?;
    let id = WorkspaceId::parse(workspace)?;
    puts.entries(id, "", true).ok().map(|entries| {
        entries
            .into_iter()
            .map(|entry| (entry.id, entry.path, entry.folder))
            .collect()
    })
}

/// `listing` without the entries `caller` may not read: a member whose role is over the whole
/// workspace reads every entry, and one let into items alone reads theirs and the folders above
/// them.
fn only_their_items(
    folders: &dyn Folders,
    caller: &Caller,
    workspace: &str,
    mut listing: Value,
) -> Value {
    let Some(held) = folders.workspace(workspace) else {
        return listing;
    };
    let person = reader_person(folders, caller);
    let context = Context::at_time(
        std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(folders.now()))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
    );
    if access::decide(&held.policy, workspace, &person, &context).is_some() {
        return listing;
    }
    let Some(tree) = tree_of(folders, workspace) else {
        return listing;
    };
    let acting = Acting::person(person.clone());
    let allowed = |at: &str| {
        access::reads_path(
            &held.policy,
            workspace,
            &person,
            &acting,
            at,
            &tree,
            &context,
        )
    };
    if let Some(entries) = listing.get_mut("entries").and_then(Value::as_array_mut) {
        entries.retain(|entry| {
            entry
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(allowed)
        });
    }
    listing
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
    let listing = only_their_items(folders, caller, workspace, listing);
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
            && let Some(map) = entry.as_object_mut()
        {
            map.insert("changed".to_owned(), at);
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
        let task = self.watches.lock_recover().remove(&watch.id());
        if let Some(task) = task {
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use tilo_action_policy::{Grant, WorkspacePolicy};
    use tilo_action_types::KindSet;
    use tilo_app_host::services::{FilesEntry, FilesPutCall, FilesTreePuts};
    use tilo_desktop_model::Workspace;
    use tilo_server_protocol::WorkspaceId;

    const ME: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const OWNER: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    /// The workspace's folder, with the entries it holds and the reading Files' LIST answers.
    struct Tree {
        workspace: Workspace,
        entries: Vec<(String, String, bool)>,
    }

    impl FilesTreePuts for Tree {
        fn put(
            &self,
            _workspace: WorkspaceId,
            _arguments: &serde_json::Map<String, Value>,
            _call: Option<FilesPutCall<'_>>,
            _confirm: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            Err("a put is not this test's".to_owned())
        }

        fn entries(
            &self,
            _workspace: WorkspaceId,
            folder: &str,
            below: bool,
        ) -> Result<Vec<FilesEntry>, String> {
            Ok(self
                .entries
                .iter()
                .filter(|(_, path, _)| {
                    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                    parent == folder || (below && path.starts_with(folder))
                })
                .map(|(id, path, folder)| FilesEntry {
                    path: path.clone(),
                    folder: *folder,
                    id: id.clone(),
                })
                .collect())
        }

        fn read(
            &self,
            _workspace: WorkspaceId,
            action: &str,
            arguments: &serde_json::Map<String, Value>,
        ) -> Result<Value, String> {
            if action != tree::LIST {
                return Err("this test reads a folder".to_owned());
            }
            let folder = arguments
                .get("folder")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let entries: Vec<Value> = self
                .entries
                .iter()
                .filter(|(_, path, _)| {
                    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                    parent == folder
                })
                .map(|(id, path, folder)| {
                    serde_json::json!({
                        "id": id,
                        "name": path.rsplit_once('/').map_or(path.as_str(), |(_, name)| name),
                        "path": path,
                        "kind": if *folder { "folder" } else { "file" },
                        "size": 1,
                        "changed": 0,
                    })
                })
                .collect();
            Ok(
                serde_json::json!({ "folder": folder, "entries": entries, "from": 0, "next": null, "total": entries.len() }),
            )
        }

        fn change(
            &self,
            _workspace: WorkspaceId,
            _verb: &str,
            _arguments: &serde_json::Map<String, Value>,
            _call: Option<FilesPutCall<'_>>,
            _confirm: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            Err("a change is not this test's".to_owned())
        }
    }

    struct Folders {
        tree: Arc<Tree>,
    }

    impl Folders {
        fn new(policy: WorkspacePolicy, entries: Vec<(String, String, bool)>) -> Self {
            let id = policy.workspace.clone();
            let mut workspace = Workspace::new(WorkspaceId::parse(&id).unwrap(), "Home");
            workspace.policy = policy;
            Self {
                tree: Arc::new(Tree { workspace, entries }),
            }
        }
    }

    impl super::Folders for Folders {
        fn workspace(&self, id: &str) -> Option<Workspace> {
            (self.tree.workspace.id().to_string() == id).then(|| self.tree.workspace.clone())
        }

        fn now(&self) -> u64 {
            1
        }

        fn puts(&self) -> Option<Arc<dyn FilesTreePuts>> {
            Some(self.tree.clone() as Arc<dyn FilesTreePuts>)
        }

        fn writes(&self) -> tokio::sync::broadcast::Receiver<ResourceWrite> {
            let (_send, received) = tokio::sync::broadcast::channel(1);
            received
        }

        fn person(&self) -> String {
            ME.to_owned()
        }
    }

    /// A person's read share of `node` in the workspace `policy` is about.
    fn share(policy: &mut WorkspacePolicy, node: &str) {
        policy.grants.push(Grant::new(
            "share:one",
            tilo_action_policy::Grantee::person(ME),
            fs_protocol::file_action::node_scope(&policy.workspace, node),
            KindSet::READ,
            Answer::Allow,
            tilo_action_policy::Grantee::person(OWNER),
        ));
    }

    fn caller(workspace: &str) -> Caller {
        Caller {
            app_id: "so.tilo.file-explorer".to_owned(),
            workspace: workspace.to_owned(),
            opened_for: Some(
                tilo_actions::remote::Caller::local_person(ME).in_workspace(workspace),
            ),
        }
    }

    fn entry_paths(listing: &Value) -> Vec<String> {
        listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A person shared one file of a folder reads that file alone: the listing says the folder
    /// holds the file they were let into, and not the one beside it.
    #[test]
    fn a_member_shared_one_file_reads_only_it() {
        let id = WorkspaceId::new().to_string();
        let mut policy = WorkspacePolicy::new(id.clone(), OWNER);
        share(&mut policy, "record:notes");
        let folders = Folders::new(
            policy.clone(),
            vec![
                ("record:trip".into(), "Trip".into(), true),
                ("record:notes".into(), "Trip/notes.md".into(), false),
                ("record:other".into(), "Trip/other.md".into(), false),
            ],
        );
        let listing = reading(&folders, &caller(&id), "files/folder~Trip")
            .expect("a person let into a file reads the folder that holds it");
        assert_eq!(entry_paths(&listing), ["Trip/notes.md"], "{listing}");
    }

    /// A person the workspace lets read all its files reads every entry, and one with no share
    /// reads no folder at all.
    #[test]
    fn a_role_over_the_workspace_reads_everything_and_a_stranger_nothing() {
        let id = WorkspaceId::new().to_string();
        let mut policy = WorkspacePolicy::new(id.clone(), OWNER);
        policy.grants.push(Grant::new(
            "role:viewer",
            tilo_action_policy::Grantee::person(ME),
            tilo_action_policy::TreePath::workspace(id.clone()),
            KindSet::READ,
            Answer::Allow,
            tilo_action_policy::Grantee::person(OWNER),
        ));
        let entries = vec![
            ("record:notes".into(), "Trip/notes.md".into(), false),
            ("record:other".into(), "Trip/other.md".into(), false),
        ];
        let folders = Folders::new(policy, entries.clone());
        let listing = reading(&folders, &caller(&id), "files/folder~Trip")
            .expect("a viewer reads the folder");
        assert_eq!(entry_paths(&listing), ["Trip/notes.md", "Trip/other.md"]);

        let nothing = Folders::new(WorkspacePolicy::new(id.clone(), OWNER), entries);
        assert!(
            reading(&nothing, &caller(&id), "files/folder~Trip").is_err(),
            "a person with no share reads no folder"
        );
    }
}
