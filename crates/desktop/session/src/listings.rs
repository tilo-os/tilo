//! The records an app lists rather than runs on this device (a workspace's invocations), followed
//! for the shell's views that list them with a query (`query(invocations.Invocation, status:
//! .waiting, person: me)`).
//!
//! Each listing is published under its app's id, as its listing reads
//! (`{"items": [...]}`): read from the workspace the person is in through the device's one read,
//! each answer waiting for the next change to it, from the first view that asked for it on. A
//! workspace the person left is followed no more.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::remote::ResourceReadRequest;
use tilo_actions::{ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started};
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_session_core::desktop::state::DesktopHandle;

/// How long a follow waits for a change before it asks again.
const FOLLOW_WAIT_MS: u32 = 25_000;
/// How long a follow waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The last reading of one listing, and how many readings moved it.
#[derive(Default)]
struct Latest {
    moves: u64,
    reading: Option<Value>,
}

/// One listing followed in one workspace; the follow ends when it is dropped.
struct Followed {
    workspace: String,
    latest: Arc<Mutex<Latest>>,
    follow: tokio::task::AbortHandle,
}

impl Drop for Followed {
    fn drop(&mut self) {
        self.follow.abort();
    }
}

/// The listings of the workspace in front, as this session publishes them.
pub(crate) struct Listings {
    daemon: DaemonShell,
    /// The layout, where the workspace the person is in is read.
    handle: DesktopHandle,
    /// Wakes the session's loop when a listing moves.
    wake: tilo_service::Wake,
    /// Each listing followed, by its app.
    followed: RefCell<std::collections::BTreeMap<&'static str, Followed>>,
    /// The moves last told, so a move is told once.
    seen: Cell<u64>,
}

impl Listings {
    /// The listings of whichever workspace `handle` says the person is in, waking the session's
    /// loop through `wake` when one moves.
    pub(crate) fn new(
        daemon: DaemonShell,
        handle: DesktopHandle,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            daemon,
            handle,
            wake,
            followed: RefCell::default(),
            seen: Cell::new(0),
        }
    }

    fn workspace(&self) -> Option<String> {
        self.handle
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
    }

    /// The listing of `app`'s `record`, followed in the workspace in front from now on.
    fn follow(&self, app: &'static str, record: &'static str) -> Option<Value> {
        let workspace = self.workspace()?;
        let mut followed = self.followed.borrow_mut();
        if followed
            .get(app)
            .is_some_and(|held| held.workspace != workspace)
        {
            followed.remove(app);
        }
        let held = followed
            .entry(app)
            .or_insert_with(|| self.start(&workspace, app, record));
        held.latest.lock_recover().reading.clone()
    }

    fn start(&self, workspace: &str, app: &'static str, record: &'static str) -> Followed {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let follow = {
            let (daemon, latest, wake) = (
                self.daemon.clone(),
                Arc::clone(&latest),
                Arc::clone(&self.wake),
            );
            let workspace = workspace.to_owned();
            self.daemon.runtime().clone().spawn(async move {
                let mut after: Option<String> = None;
                loop {
                    let read = ResourceReadRequest {
                        workspace: workspace.clone(),
                        app: app.to_owned(),
                        record: record.to_owned(),
                        joined: false,
                        wait_ms: after.is_some().then_some(FOLLOW_WAIT_MS),
                        after: after.clone(),
                        answer: tilo_actions::remote::ReadAnswer::Reading,
                    };
                    match daemon.read(read).await {
                        Ok(readings) => {
                            let revision = readings["revision"].as_str().map(str::to_owned);
                            if revision.is_some() && revision == after {
                                continue;
                            }
                            after = revision;
                            let reading = readings["records"]
                                .as_array()
                                .and_then(|records| records.first())
                                .and_then(|first| first.get("value"))
                                .filter(|value| value.is_object())
                                .cloned();
                            let mut held = latest.lock_recover();
                            held.reading = reading;
                            held.moves = held.moves.wrapping_add(1);
                            drop(held);
                            wake();
                        }
                        Err(error) => {
                            tracing::warn!(%error, %workspace, app, "a listing did not read");
                            tokio::time::sleep(ASK_AGAIN).await;
                        }
                    }
                }
            })
        };
        Followed {
            workspace: workspace.to_owned(),
            latest,
            follow: follow.abort_handle(),
        }
    }
}

impl Service for Listings {
    fn resources(&self) -> Vec<ResourceSpec> {
        Vec::new()
    }

    /// The listing published as `resource` (its app's id), once it was read.
    fn read(&self, resource: &str) -> Option<Value> {
        let (_, app, record) = tilolang_tilo::platform_apps::platform_listings()
            .into_iter()
            .find(|(_, app, _)| *app == resource)?;
        self.follow(app, record)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
            resource, action,
        )))
    }

    /// A listing moved: its reading is published again.
    fn poll(&mut self) -> bool {
        let moves = self
            .followed
            .borrow()
            .values()
            .map(|held| held.latest.lock_recover().moves)
            .fold(0_u64, u64::wrapping_add);
        self.seen.replace(moves) != moves
    }
}
