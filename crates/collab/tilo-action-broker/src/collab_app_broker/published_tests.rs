#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::services::HostServices;
use crate::services::{Caller, Granted};
use runtime_collab::wire::{Event, Receipt};
use tokio::sync::{mpsc, oneshot};

use super::super::*;
use super::*;

/// The session's services as a test publishes them: a `sound` resource
/// everyone reads, a `clipboard` one the workspace's grants decide, and the
/// watches it was asked for and those it let go of.
#[derive(Default)]
struct Provider {
    watched: Mutex<Vec<ResourceWatch>>,
    unwatched: Mutex<Vec<ResourceWatch>>,
    /// Kept until unwatched, as the session keeps a watch's sink.
    sinks: Mutex<Vec<(ResourceWatch, mpsc::UnboundedSender<Event>)>>,
    next: AtomicU64,
}

/// What the session publishes of `resource`, for a test that reads it.
fn reading_of(resource: &str) -> Option<Vec<u8>> {
    match resource {
        "sound" => Some(br#"{"output":40,"volume":0.4}"#.to_vec()),
        "clipboard" => Some(br#"["the copied line"]"#.to_vec()),
        _ => None,
    }
}

impl PublishedResources for Provider {
    /// What the person copied is the session's own: the `session` kind of
    /// the session's app, which only the workspace's grants read.
    fn granted(&self, resource: &str) -> Option<Granted> {
        (resource == "clipboard").then(|| Granted {
            publisher: tilo_action_policy::roles::SESSION_APP.to_owned(),
            kind: tilo_action_policy::roles::SESSION_KIND.to_owned(),
        })
    }

    fn snapshot(&self, _caller: &Caller, resource: &str) -> oneshot::Receiver<Reading> {
        let (reply, answered) = oneshot::channel();
        let _ = reply.send((7, reading_of(resource)));
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> ResourceWatch {
        let watch = ResourceWatch::new(self.next.fetch_add(1, Ordering::Relaxed));
        let _ = sink.send(Event::Reading {
            resource: resource.into(),
            revision: 8,
            reading: reading_of(resource),
        });
        self.watched.lock().unwrap().push(watch);
        self.sinks.lock().unwrap().push((watch, sink));
        watch
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.sinks
            .lock()
            .unwrap()
            .retain(|(held, _)| *held != watch);
        self.unwatched.lock().unwrap().push(watch);
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _resource: &str,
        _action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        let (reply, answered) = oneshot::channel();
        let _ = reply.send(Receipt::Accepted);
        answered
    }
}

/// An app the launcher attested as `so.tilo.settings` in `the-workspace`,
/// reading the services `provider` publishes.
struct App {
    client: runtime_collab::CollabClient,
    _state: Arc<AppBrokerState>,
}

fn app(provider: Arc<Provider>) -> App {
    app_in(provider, "so.tilo.settings", "the-workspace", None)
}

/// An app the launcher attested as `id` in `workspace_id`, reading what `provider`
/// publishes, with what `membership` says the workspace grants.
fn app_in(
    provider: Arc<Provider>,
    id: &str,
    workspace_id: &str,
    membership: Option<WorkspaceTreeHandle>,
) -> App {
    let (state, bridge_rx) = AppBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        membership,
        None,
        None,
        HostServices {
            published: Some(provider),
            ..HostServices::default()
        },
    );
    let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
    let peer_id = state.next_peer_id();
    let (host, endpoint) = tokio::net::UnixStream::pair().unwrap();
    let local = native_media::LocalAppConnection::attested(
        id.into(),
        workspace_id.into(),
        native_media::PeerProcess::from_socket(&host).unwrap(),
        crate::services::AuthenticatedApplicationLaunch {
            continuation_instance: None,
            launch_ticket: None,
            windowless: false,
        },
    )
    .unwrap();
    drop(endpoint);
    let (connection, outgoing) = wire::WireConnection::new(Some(Rc::new(AppConnection {
        worker: None,
        app_worker: None,
        state: state.clone(),
        directories: directories.clone(),
        peer_id,
        lease: Rc::new(ConnectionLease {
            local_media: Some(local),
            state: state.clone(),
            directories,
            peer_id,
        }),
        holds: route_holds::ConnectionHolds::default(),
    })));
    App {
        client: wire_client(connection, outgoing),
        _state: state,
    }
}

/// A workspace `owner` owns, as a desk keeps it, with the session's panel
/// granted to read the session's own readings: what every session's workspace
/// holds.
fn panel_workspace() -> (
    crate::test_support::WorkspaceTreeHandle,
    crate::services::WorkspaceAuthorityHandle,
    String,
) {
    use crate::test_support::{Subject, Workspace, WorkspaceSlot, WorkspaceTree};
    let mut workspace = Workspace::new(WorkspaceSlot(41), "Readings");
    workspace.set_owner("owner");
    let workspace_id = workspace.id().to_string();
    workspace.grant(tilo_action_policy::roles::device_panel(
        &workspace_id,
        Subject::Person("owner".into()),
    ));
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let tree = tree.shared();
    let membership = tree.authority();
    (tree, membership, workspace_id)
}

/// The reading of what the person copied reaches the panel's launch, and no
/// other app: the workspace grants the panel, and nothing an app borrows from the
/// person reads it.
#[tokio::test(flavor = "current_thread")]
async fn a_granted_app_reads_the_session_reading_and_another_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = Arc::new(Provider::default());
            let (_tree, membership, workspace_id) = panel_workspace();
            let panel = app_in(
                Arc::clone(&provider),
                tilo_action_policy::roles::DEVICE_PANEL,
                &workspace_id,
                Some(Arc::clone(&membership)),
            );
            assert_eq!(
                panel.client.services().snapshot("clipboard").await.unwrap(),
                runtime_collab::Snapshot {
                    revision: 7,
                    reading: Some(r#"["the copied line"]"#.into()),
                },
                "the panel reads what the person copied"
            );
            assert_eq!(
                panel.client.services().snapshot("sound").await.unwrap(),
                runtime_collab::Snapshot {
                    revision: 7,
                    reading: Some(r#"{"output":40,"volume":0.4}"#.into()),
                },
                "and what everyone reads stays everyone's"
            );

            let other = app_in(
                Arc::clone(&provider),
                "so.tilo.settings",
                &workspace_id,
                Some(membership),
            );
            let refused = other.client.services().snapshot("clipboard").await;
            let refusal = refused.expect_err("an app the workspace does not grant is refused");
            assert!(
                refusal.to_string().contains("not granted"),
                "refused as a reading nobody granted it: {refusal}"
            );
        })
        .await;
}

/// A watch the workspace does not grant is not opened at all, and one it does is
/// served its reading.
#[tokio::test(flavor = "current_thread")]
async fn only_a_granted_app_watches_the_session_reading() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = Arc::new(Provider::default());
            let (_tree, membership, workspace_id) = panel_workspace();
            let panel = app_in(
                Arc::clone(&provider),
                tilo_action_policy::roles::DEVICE_PANEL,
                &workspace_id,
                Some(Arc::clone(&membership)),
            );
            let readings = panel.client.services().watch("clipboard").await.unwrap();
            assert_eq!(
                readings.read().await.unwrap().reading.as_deref(),
                Some(r#"["the copied line"]"#),
                "the panel is sent the reading it watches"
            );

            let other = app_in(
                Arc::clone(&provider),
                "so.tilo.settings",
                &workspace_id,
                Some(membership),
            );
            let refused = other.client.services().watch("clipboard").await;
            let Err(refusal) = refused else {
                panic!("an app the workspace does not grant watches nothing");
            };
            assert!(
                refusal.to_string().contains("not granted"),
                "refused as a reading nobody granted it: {refusal}"
            );
            assert_eq!(
                provider.watched.lock().unwrap().len(),
                1,
                "only the granted watch was opened"
            );
        })
        .await;
}

/// A watch an app opened ends with the connection that opened it: the
/// session is not left publishing to a connection that is gone.
#[tokio::test(flavor = "current_thread")]
async fn a_watch_ends_with_the_connection_that_opened_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = Arc::new(Provider::default());
            let app = app(provider.clone());
            let readings = app.client.services().watch("sound").await.unwrap();
            assert_eq!(readings.read().await.unwrap().revision, 8);
            assert_eq!(
                provider.watched.lock().unwrap().len(),
                1,
                "the session serves the watch it was asked for"
            );
            assert!(provider.unwatched.lock().unwrap().is_empty());

            // The app goes away: its connection ends, and with it the watch.
            drop(app);
            for _ in 0..1000 {
                if !provider.unwatched.lock().unwrap().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert_eq!(
                provider.unwatched.lock().unwrap().as_slice(),
                provider.watched.lock().unwrap().as_slice(),
                "the watch is let go of when the connection ends"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_reads_and_invokes_the_session_services_on_its_connection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let provider = Arc::new(Provider::default());
            let app = app(provider);
            let services = app.client.services();
            assert_eq!(
                services.snapshot("sound").await.unwrap(),
                runtime_collab::Snapshot {
                    revision: 7,
                    reading: Some(r#"{"output":40,"volume":0.4}"#.into()),
                }
            );
            assert_eq!(
                services.snapshot("weather").await.unwrap(),
                runtime_collab::Snapshot {
                    revision: 7,
                    reading: None,
                },
                "a resource no service publishes reads as nothing"
            );
            assert_eq!(
                services
                    .invoke(
                        "sound",
                        "set-volume",
                        &serde_json::json!({ "level": 0.4 })
                            .as_object()
                            .unwrap()
                            .clone(),
                    )
                    .await
                    .unwrap(),
                Receipt::Accepted
            );
        })
        .await;
}
