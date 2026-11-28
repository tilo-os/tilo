//! Where this device's member workspaces stand with the devices serving them, published for its
//! person's shell: what each app window in one shows as it opens ("Connecting to Lounge…"), and
//! when the workspace's first sync has completed.
//!
//! The copies and their links are the daemon's: it follows each member workspace's link to its host
//! ([`Workspaces::member_up`]) and publishes the state the link is in. A workspace this device
//! hosts has no link of its own and no entry here; a workspace that linked before and does not
//! now says so, while its first link has not been made yet it is still connecting.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::sync::{RESOURCE, Status, Workspace};

use super::reading::{Changes, Read, Served};

/// One member workspace this daemon follows, as its link to its host last was.
struct Tracked {
    /// Whether its link was up at any time: a copy that synced and lost its host is unreachable,
    /// one that never linked yet is connecting.
    linked_before: bool,
    status: Status,
}

/// The member workspaces' states, read and followed.
pub(crate) struct WorkspaceSync {
    states: Mutex<BTreeMap<String, Tracked>>,
    /// The member copies a watcher already follows.
    watched: Mutex<BTreeSet<String>>,
    /// Moves with every change of the reading: what a follower reads again.
    moved: tokio::sync::watch::Sender<u64>,
}

impl WorkspaceSync {
    /// Publish where this device's member workspaces stand, following each link from now on.
    #[must_use]
    pub(crate) fn new(copies: &Workspaces) -> Arc<Self> {
        let (moved, _) = tokio::sync::watch::channel(0);
        let sync = Arc::new(Self {
            states: Mutex::default(),
            watched: Mutex::default(),
            moved,
        });
        sync.follow(copies);
        sync
    }

    /// Follow each member workspace this device keeps now, and each taken in later.
    fn follow(self: &Arc<Self>, copies: &Workspaces) {
        self.watch_kept(copies);
        let copies = copies.clone();
        let sync = Arc::downgrade(self);
        let mut kept = copies.holds_changed();
        tilo_rt::app().spawn(async move {
            while kept.changed().await.is_ok() {
                let Some(sync) = sync.upgrade() else {
                    return;
                };
                sync.watch_kept(&copies);
            }
        });
    }

    /// Follow the link of every member workspace in `copies` that no watcher follows yet.
    fn watch_kept(self: &Arc<Self>, copies: &Workspaces) {
        for workspace in copies.held_workspaces() {
            let Some(id) = WorkspaceId::parse(&workspace) else {
                continue;
            };
            // A workspace this device hosts is no member workspace and has no link: its apps'
            // states are their own.
            let Ok(mut up) = copies.member_up(id) else {
                continue;
            };
            if !self.watched.lock_recover().insert(workspace.clone()) {
                continue;
            }
            self.note(&workspace, *up.borrow());
            let sync = Arc::downgrade(self);
            let named = workspace.clone();
            tilo_rt::app().spawn(async move {
                while up.changed().await.is_ok() {
                    let Some(sync) = sync.upgrade() else {
                        return;
                    };
                    sync.note(&named, *up.borrow_and_update());
                }
            });
        }
    }

    /// Say whether `workspace`'s link is `up`, and publish the reading if it moved.
    fn note(&self, workspace: &str, up: bool) {
        let mut states = self.states.lock_recover();
        let tracked = states.entry(workspace.to_owned()).or_insert(Tracked {
            linked_before: up,
            status: Status::Connecting,
        });
        tracked.linked_before |= up;
        let now = if up {
            Status::Synced
        } else if tracked.linked_before {
            Status::Unreachable
        } else {
            Status::Connecting
        };
        if tracked.status == now {
            return;
        }
        tracked.status = now;
        drop(states);
        self.moved
            .send_modify(|moved| *moved = moved.wrapping_add(1));
    }

    /// The reading the person's shell follows over the shell link.
    pub(crate) fn published(self: &Arc<Self>) -> Served {
        let reading = Arc::clone(self);
        let changing = Arc::clone(self);
        let read: Arc<Read> =
            Arc::new(move |_caller: &tilo_app_host::services::Caller| reading.reading());
        let changes: Arc<Changes> = Arc::new(move || changing.moved.subscribe());
        Served::new(RESOURCE, read, changes)
    }

    fn reading(&self) -> Value {
        let workspaces: BTreeMap<String, Workspace> = self
            .states
            .lock_recover()
            .iter()
            .map(|(workspace, tracked)| {
                (
                    workspace.clone(),
                    Workspace {
                        status: tracked.status,
                        unsynced_writes: 0,
                    },
                )
            })
            .collect();
        serde_json::json!({ "workspaces": workspaces })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy reports connecting until its first link, synced while linked, unreachable after a
    /// link it had, and synced again when the host returns.
    #[test]
    fn a_copy_is_connecting_synced_and_unreachable_as_its_link_moves() {
        let (moved, _) = tokio::sync::watch::channel(0);
        let sync = WorkspaceSync {
            states: Mutex::default(),
            watched: Mutex::default(),
            moved,
        };
        let workspace = "5eed0001-0000-4000-8000-000000000001";
        let status = || {
            sync.states
                .lock_recover()
                .get(workspace)
                .map(|tracked| tracked.status)
        };
        sync.note(workspace, false);
        assert_eq!(status(), Some(Status::Connecting));
        sync.note(workspace, true);
        assert_eq!(status(), Some(Status::Synced));
        sync.note(workspace, false);
        assert_eq!(status(), Some(Status::Unreachable));
        sync.note(workspace, true);
        assert_eq!(status(), Some(Status::Synced));
    }
}
