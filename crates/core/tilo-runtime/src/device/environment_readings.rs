//! A workspace's machine environments as this device publishes them to the apps it serves: each
//! environment's record, read where the workspace keeps it, as `environments`.
//!
//! The environments are the workspace's own records, kept in the store this device's workspace
//! service holds and syncs (`tilo_environment::Environment`). A view of them holds no copy: the
//! reading is made from the records each time it is asked for, and a watch has it made again
//! whenever the workspace takes in a write of one — whichever device wrote it, and whichever of
//! the records' verbs changed it. An app names a workspace and reads only that one's.
//!
//! The entries carry what a window lists: the record's key, its name, the packages its tools
//! install, the directories it may reach and whether it may write there, and its home and
//! runtime release.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use collab_types::store::wire::{Command, Reply};
use serde_json::{Value, json};
use tilo_action_services::environment_commands::ENVIRONMENTS;
use tilo_app_host::services::{Caller, PublishedResources, Reading, ResourceWatch};
use tilo_environment::{Environment, HomePolicy};
use tilo_lock::MutexRecover as _;
use tilo_resource::Resource as _;
use tilo_workspace_service::workspaces::{ResourceWrite, Workspaces};
use tokio::sync::{mpsc, oneshot};

use super::workspaces::ServiceWorkspaces;

/// The app whose documents the environments are, as the shell runtime keeps them.
const TERMINAL: &str = tilo_app_id::apps::TERMINAL;
/// The kind of Terminal's records an environment is one of.
const ENVIRONMENT: &str = "environment";
/// What every environment record's id starts with, beside Terminal's other documents.
const PREFIX: &str = "environment:";

/// The workspace's environments as this device publishes them.
pub(crate) struct Readings {
    /// The workspaces this device holds, read where it keeps them.
    workspaces: Workspaces,
    /// Counts each reading published, so a follower can tell one from another.
    edits: Arc<AtomicU64>,
    /// The watches this provider minted, by id, each with the task that ends when it does.
    watches: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_watch: AtomicU64,
}

impl Readings {
    /// The environments of the workspaces `workspaces` holds.
    #[must_use]
    pub(crate) fn of(workspaces: Workspaces) -> Self {
        Self {
            workspaces,
            edits: Arc::new(AtomicU64::new(0)),
            watches: Mutex::default(),
            next_watch: AtomicU64::new(0),
        }
    }
}

/// The reading of `workspace`'s environments now: each record Terminal keeps of one, by its key.
async fn reading(workspaces: &Workspaces, workspace: &str) -> Value {
    let mut entries = Vec::new();
    let Ok(Reply::Ids(records)) = workspaces
        .own_kind_command(
            workspace,
            (TERMINAL, Some(ENVIRONMENT)),
            Command::Records,
            Vec::new(),
        )
        .await
    else {
        // A workspace this device does not hold, or one whose store cannot be read, has no
        // environment here: a caller reads an empty list, never another workspace's.
        return json!({ "environments": entries });
    };
    for record in records {
        let Some(key) = record.strip_prefix(PREFIX) else {
            continue;
        };
        let Some(changes) = changes_of(workspaces, workspace, &record).await else {
            continue;
        };
        let Ok(doc) = automerge::AutoCommit::load(&changes) else {
            continue;
        };
        let Ok(environment) = Environment::read(&doc) else {
            continue;
        };
        entries.push(entry(key, &environment));
    }
    json!({ "environments": entries })
}

/// The changes of `record` in `workspace`, joined from every page: `None` while the record is on
/// its way to this device, or one its store could not answer.
async fn changes_of(workspaces: &Workspaces, workspace: &str, record: &str) -> Option<Vec<u8>> {
    let mut joined = Vec::new();
    let mut from = 0;
    loop {
        let reply = workspaces
            .own_kind_command(
                workspace,
                (TERMINAL, Some(ENVIRONMENT)),
                Command::RecordChanges {
                    record: Some(record.to_owned()),
                    after: Vec::new(),
                    from,
                },
                Vec::new(),
            )
            .await
            .ok()?;
        let Reply::RecordChanges {
            changes,
            next,
            pending,
            ..
        } = reply
        else {
            return None;
        };
        if pending {
            return None;
        }
        joined.extend_from_slice(&changes.0);
        match next {
            Some(further) => from = further,
            None => return Some(joined),
        }
    }
}

/// One environment as a window lists it.
fn entry(key: &str, environment: &Environment) -> Value {
    let policy = ush::policy::Policy::new(environment.grants.clone());
    let now = Some(tilo_clock::unix_now());
    let mut grants: Vec<Value> = policy
        .trusted_dirs(now)
        .iter()
        .map(|dir| json!({ "path": dir.display().to_string(), "write": true }))
        .collect();
    grants.extend(
        policy
            .readable_dirs(now)
            .iter()
            .map(|dir| json!({ "path": dir.display().to_string(), "write": false })),
    );
    json!({
        "record": key,
        "name": environment.name.clone(),
        "tools": environment
            .tools
            .iter()
            .flat_map(|tool| tool.packages.iter().cloned())
            .collect::<Vec<_>>(),
        "grants": grants,
        "home": match environment.home {
            HomePolicy::Persistent => "Persistent",
            HomePolicy::Ephemeral => "Ephemeral",
        },
        "runtime": environment.runtime.release.clone(),
    })
}

/// The `environments` resource of the workspaces `workspaces` holds, as this device publishes
/// it.
#[must_use]
pub(crate) fn published(workspaces: &Arc<ServiceWorkspaces>) -> Arc<dyn PublishedResources> {
    Arc::new(Readings::of(workspaces.copies().clone()))
}

/// The bytes a reading travels as, or nothing when it does not encode.
fn encoded(reading: &Value) -> Option<Vec<u8>> {
    match serde_json::to_vec(reading) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            tracing::warn!(%error, "an environments reading does not encode");
            None
        }
    }
}

/// Whether `write` changed an environment of `workspace`: a write names the record it changed,
/// as the record's own id or as the document its content is kept in.
fn written(write: &ResourceWrite, workspace: &str) -> bool {
    write.workspace == workspace
        && write.app == TERMINAL
        && (write.record.starts_with(PREFIX)
            || tilo_resource_store::content_resource(&write.record)
                .is_some_and(|(kind, _)| kind == ENVIRONMENT))
}

impl PublishedResources for Readings {
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        if resource != ENVIRONMENTS {
            let _ = answer.send(Ok((0, None)));
            return answered;
        }
        let workspaces = self.workspaces.clone();
        let caller = caller.clone();
        let revision = self.edits.load(Ordering::Relaxed);
        tilo_rt::app().handle().spawn(async move {
            let value = reading(&workspaces, &caller.workspace).await;
            let _ = answer.send(Ok((revision, encoded(&value))));
        });
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<ResourceWatch, String> {
        if resource != ENVIRONMENTS {
            return Err(format!("this device publishes no `{resource}` resource"));
        }
        let id = self.next_watch.fetch_add(1, Ordering::Relaxed);
        let workspaces = self.workspaces.clone();
        let edits = Arc::clone(&self.edits);
        let caller = caller.clone();
        let task = tilo_rt::app().handle().spawn(async move {
            // Follow the workspace's writes before reading it, so a change between the two is
            // not missed: each matching write makes the reading again.
            let mut writes = workspaces.document_writes();
            let publish = |reading: &Value| {
                sink.send(runtime_collab::wire::Event::Reading {
                    resource: ENVIRONMENTS.to_owned(),
                    revision: edits.fetch_add(1, Ordering::Relaxed),
                    reading: encoded(reading),
                })
                .is_ok()
            };
            if !publish(&reading(&workspaces, &caller.workspace).await) {
                return;
            }
            loop {
                let again = match writes.recv().await {
                    Ok(write) => written(&write, &caller.workspace),
                    // Some writes went by unheard: one of them may have been ours.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                if again && !publish(&reading(&workspaces, &caller.workspace).await) {
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

    /// The environments are read here alone; changing them is the record's verbs, which the
    /// dispatch answers.
    fn invoke(
        &self,
        _caller: &Caller,
        _resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<runtime_collab::wire::Receipt> {
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(runtime_collab::wire::Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: format!("`{action}` is a change of the record, not of its reading"),
        });
        answered
    }
}
