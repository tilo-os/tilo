#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::services::Caller;
use crate::services::HostServices;
use runtime_collab::wire::{Event, Receipt};
use tokio::sync::{mpsc, oneshot};

use super::super::*;
use super::*;

/// The session's services as a test publishes them: one `sound` resource,
/// with the watches it was asked for and those it let go of.
#[derive(Default)]
struct Provider {
    watched: Mutex<Vec<ResourceWatch>>,
    unwatched: Mutex<Vec<ResourceWatch>>,
    /// Kept until unwatched, as the session keeps a watch's sink.
    sinks: Mutex<Vec<(ResourceWatch, mpsc::UnboundedSender<Event>)>>,
    next: AtomicU64,
}

impl PublishedResources for Provider {
    fn snapshot(&self, _caller: &Caller, resource: &str) -> oneshot::Receiver<Reading> {
        let (reply, answered) = oneshot::channel();
        let reading = (resource == "sound").then(|| br#"{"output":40,"volume":0.4}"#.to_vec());
        let _ = reply.send((7, reading));
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        _resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> ResourceWatch {
        let watch = ResourceWatch::new(self.next.fetch_add(1, Ordering::Relaxed));
        let _ = sink.send(Event::Reading {
            resource: "sound".into(),
            revision: 8,
            reading: Some(br#"{"output":40,"volume":0.4}"#.to_vec()),
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
    let (state, bridge_rx) = AppBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        None,
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
        "so.tilo.settings".into(),
        "the-workspace".into(),
        native_media::PeerProcess::from_socket(&host).unwrap(),
        crate::services::AuthenticatedApplicationLaunch {
            creation: None,
            continuation_instance: None,
            launch_ticket: None,
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
