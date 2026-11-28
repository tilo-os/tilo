//! The records an app lists rather than runs on this device (a workspace's invocations), followed
//! for the shell's views that list them with a query (`query(invocations.Invocation, status:
//! .waiting, person: me)`).
//!
//! Each listing is published under its app's id, as its listing reads
//! (`{"items": [...]}`): read from the workspace the person is in through the device's one read,
//! each answer waiting for the next change to it, from the first view that asked for it on. A
//! workspace the person left is followed no more.
//!
//! Not every wait ends in a reading, and a listing says so rather than waiting on one that may
//! never come: the person is in no workspace, the workspace never took this device in, the
//! workspace names no device that keeps it, or the device answered that it could not read the
//! listing (no provider serves it). Each is published as the listing's reading, carrying why
//! (`{"$unreadable": …}`), which a view reads with `unreadable(r)`; a reading that answers
//! replaces it.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::remote::ResourceReadRequest;
use tilo_actions::{ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started};
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_session_core::desktop::state::DesktopHandle;

/// How long a follow waits before it asks a daemon that did not answer again.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The last reading of one listing, and how many readings moved it.
#[derive(Default)]
struct Latest {
    moves: u64,
    reading: Option<Value>,
    /// Why the device could not read the listing, once it said so: the workspace's host serves no
    /// provider for it, or the device could not reach the host. A listing says that rather than
    /// waiting on a reading no service may ever give; one that answers clears it.
    trouble: Option<String>,
}

/// A listing the device could not read, as the reading a view reads `unreadable(r)` from: the
/// words whatever said it, under the key every unreadable reading carries.
fn unreadable(why: &str) -> Value {
    Value::Object(Map::from_iter([(
        tilolang_tilo::runtime::store::UNREADABLE.to_owned(),
        Value::String(why.to_owned()),
    )]))
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
    /// How the person's workspaces stand, as the daemon says: whether this device was taken into
    /// the one in front, and which device keeps it. A workspace this device is not in, or one no
    /// device keeps, publishes nothing it could read, which the listing says instead of waiting.
    view: Option<Arc<tilo_session_client::workspace_view::WorkspaceView>>,
}

impl Listings {
    /// The listings of whichever workspace `handle` says the person is in, waking the session's
    /// loop through `wake` when one moves.
    pub(crate) fn new(
        daemon: DaemonShell,
        handle: DesktopHandle,
        wake: tilo_service::Wake,
        view: Option<Arc<tilo_session_client::workspace_view::WorkspaceView>>,
    ) -> Self {
        Self {
            daemon,
            handle,
            wake,
            followed: RefCell::default(),
            seen: Cell::new(0),
            view,
        }
    }

    fn workspace(&self) -> Option<String> {
        self.handle
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
    }

    /// The listing of `app`'s `record`, followed in the workspace in front from now on.
    fn follow(&self, app: &'static str, record: &'static str) -> Option<Value> {
        // The person is in no workspace: no workspace lists anything for them, which a view says
        // rather than waiting on a reading no workspace will give.
        let Some(workspace) = self.workspace() else {
            return Some(unreadable("There is no workspace here to read them from."));
        };
        // What the daemon says of the workspace itself: a device it never took in, or a workspace
        // no device keeps, publishes nothing this one could read, however long it waits.
        if let Some(view) = &self.view
            && let Some(hosting) =
                tilo_desktop_model::WorkspaceId::parse(&workspace).and_then(|id| view.hosting(id))
        {
            if !hosting.admitted {
                return Some(unreadable(
                    "This device is not a member of the workspace yet.",
                ));
            }
            // A workspace this device keeps is kept, whatever name its record gives its host.
            if !hosting.here && hosting.host.trim().is_empty() {
                return Some(unreadable("The workspace names no device that keeps it."));
            }
        }
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
        let latest = held.latest.lock_recover();
        match &latest.trouble {
            // The device said why it cannot read it: what it said stands until a reading answers.
            Some(why) => Some(unreadable(why)),
            None => latest.reading.clone(),
        }
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
                        wait_ms: after
                            .is_some()
                            .then_some(tilo_actions::remote::MAX_REMOTE_WAIT_MS),
                        after: after.clone(),
                        answer: tilo_actions::remote::ReadAnswer::Reading,
                    };
                    match daemon.read(read).await {
                        Ok(readings) => {
                            let revision = readings
                                .get("revision")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned);
                            if revision.is_some() && revision == after {
                                continue;
                            }
                            after = revision;
                            let reading = readings
                                .get("records")
                                .and_then(|records| records.get(0))
                                .and_then(|first| first.get("value"))
                                .filter(|value| value.is_object())
                                .cloned();
                            // A reading that answered clears what the device said before it: the
                            // listing reads again. Nothing published yet (a provider that is
                            // still arriving) leaves what stands, its reading or its reason.
                            if let Some(reading) = reading {
                                let mut held = latest.lock_recover();
                                held.reading = Some(reading);
                                held.trouble = None;
                                held.moves = held.moves.wrapping_add(1);
                                drop(held);
                                wake();
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, %workspace, app, "a listing did not read");
                            let said = {
                                let mut held = latest.lock_recover();
                                if held.trouble.as_deref() == Some(error.as_str()) {
                                    false
                                } else {
                                    held.trouble = Some(error);
                                    held.moves = held.moves.wrapping_add(1);
                                    true
                                }
                            };
                            if said {
                                wake();
                            }
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

    /// The listing published as `resource` (its app's id): its reading, or why the device could
    /// not read it.
    fn read(&self, resource: &str) -> Option<Value> {
        let (_, app, record) = tilolang_tilo::platform::platform_listings()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{Layout, Workspace, WorkspaceId};

    /// A desktop in no workspace: the person is signed in to none yet.
    fn no_workspace() -> DesktopHandle {
        DesktopHandle::new(Layout::default(), 7)
    }

    /// A desktop whose workspace in front is `21da5765`.
    fn in_workspace() -> DesktopHandle {
        let id =
            WorkspaceId::parse("21da5765-f361-4bd8-b8b4-ddb8d9296853").expect("a workspace id");
        let mut tree = Layout::default();
        tree.workspaces.push(Workspace::new(id, "Home"));
        tree.active = id;
        DesktopHandle::new(tree, 7)
    }

    fn listings(daemon: DaemonShell, handle: DesktopHandle) -> Listings {
        Listings::new(
            daemon,
            handle,
            tilo_service::LoopWake::default().handle(),
            None,
        )
    }

    /// The asks a workspace makes of the daemon, for a test that only reads its standing.
    struct NoAsks;

    impl tilo_session_client::workspace_view::ShellAsks for NoAsks {
        fn account(
            &self,
            _resource: &str,
            _action: &str,
            _arguments: serde_json::Map<String, Value>,
        ) -> tilo_session_client::engine::ServiceAnswer {
            Box::pin(async { Err("no asks in this test".to_owned()) })
        }

        fn invoke(
            &self,
            _workspace: Option<String>,
            _request: tilo_actions::remote::InvokeRequest,
        ) -> tilo_session_client::engine::ServiceAnswer {
            Box::pin(async { Err("no asks in this test".to_owned()) })
        }
    }

    /// A daemon this desktop cannot reach: every ask it makes answers `why`.
    fn unreachable(runtime: &tokio::runtime::Runtime, why: &str) -> DaemonShell {
        DaemonShell::new(Err(why.to_owned()), runtime.handle().clone())
    }

    /// No workspace lists nothing: a view drawing the runs is told so, rather than waiting on a
    /// reading no workspace will give.
    #[test]
    fn a_listing_in_no_workspace_says_so() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let read = listings(unreachable(&runtime, "no daemon"), no_workspace())
            .read("tilo.invocations")
            .expect("answered");
        assert_eq!(
            read.get(tilolang_tilo::runtime::store::UNREADABLE)
                .and_then(Value::as_str),
            Some("There is no workspace here to read them from."),
            "{read}"
        );
    }

    /// A device the workspace never took in reads none of it, however long it waits: the listing
    /// says why rather than pending forever.
    #[test]
    fn a_listing_of_a_workspace_this_device_is_not_in_says_so() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let id = "21da5765-f361-4bd8-b8b4-ddb8d9296853";
        let view = tilo_session_client::workspace_view::WorkspaceView::new(
            [7; 32],
            [9; 32],
            Arc::new(NoAsks),
        );
        let standing = tilo_peer_state::workspace_view::WorkspaceReading {
            id: id.to_owned(),
            hosting: Some(tilo_session_client::workspace_view::Hosting {
                host: "Their laptop".to_owned(),
                here: false,
                admitted: false,
            }),
            ..Default::default()
        };
        view.apply(
            &tilo_peer_state::workspace_view::resource(id),
            Some(&serde_json::to_string(&standing).expect("a standing")),
        )
        .expect("the standing reads");
        let listings = Listings::new(
            unreachable(&runtime, "no daemon"),
            in_workspace(),
            tilo_service::LoopWake::default().handle(),
            Some(Arc::new(view)),
        );
        let read = listings.read("tilo.invocations").expect("answered");
        assert_eq!(
            read.get(tilolang_tilo::runtime::store::UNREADABLE)
                .and_then(Value::as_str),
            Some("This device is not a member of the workspace yet."),
            "{read}"
        );
    }

    /// A device that could not read the listing says why, rather than waiting on a provider that
    /// may never come: what it said stands until a reading answers.
    #[test]
    fn a_listing_the_device_could_not_read_says_why() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let listings = listings(
            unreachable(
                &runtime,
                "this device publishes no `tilo.invocations` resource",
            ),
            in_workspace(),
        );
        assert!(
            listings.read("tilo.invocations").is_none(),
            "the first ask starts the follow and has no answer yet"
        );
        runtime.block_on(async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        });
        let read = listings
            .read("tilo.invocations")
            .expect("the device said why");
        assert_eq!(
            read.get(tilolang_tilo::runtime::store::UNREADABLE)
                .and_then(Value::as_str),
            Some("this device publishes no `tilo.invocations` resource"),
            "{read}"
        );
    }
}
