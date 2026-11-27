//! Test-only collaboration host.
//!
//! This is deliberately a transport harness, not a second policy broker: it
//! speaks the collaboration wire, keeps each channel's record as the device's
//! store does (an endpoint reads the changes past what it holds and writes its
//! own), tells the other subscribers on the channel when the record changed,
//! and lets integration tests exercise the real UI SDK shovel without a shell
//! process or process-global service-fd variables. Presence is said on the
//! connection an app's launcher hands it, never on this wire.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{IntoRawFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use ::collaboration::CollaborationManager;
use collab_types::store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply as Store};
use runtime_collab::wire::{
    Call, ContextUpdate, Event, FRAMING, HostFrame, Operation, Reply, Service, Topic,
};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;
use uuid::Uuid;

const TEST_APP_ID: &str = "ui-integration-test";

struct Subscription<T> {
    peer: u64,
    tx: mpsc::UnboundedSender<T>,
}

#[derive(Default)]
struct Channel {
    /// The channel's record, as the device's store keeps it.
    record: automerge::Automerge,
    /// Each connection's runs of a write whose last run has not arrived.
    incoming: HashMap<u64, Vec<u8>>,
    /// Each connection's context feed: told when the record changed, and what the others say.
    notices: Vec<Subscription<ContextUpdate>>,
}

#[derive(Default)]
struct Hub {
    next_peer: AtomicU64,
    channels: Mutex<HashMap<String, Channel>>,
}

impl Hub {
    /// The page from `from` of the changes of `channel`'s record past `after`, and the heads
    /// they bring a reader to.
    fn changes(&self, channel: &str, after: &[String], from: u64) -> Result<Store, String> {
        let after = after
            .iter()
            .map(|head| head.parse().map_err(|error| format!("{error:?}")))
            .collect::<Result<Vec<automerge::ChangeHash>, _>>()?;
        let (changes, heads) = Self::record_changes(
            self.channels
                .lock()
                .unwrap()
                .entry(channel.to_owned())
                .or_default(),
            &after,
        );
        let from = usize::try_from(from).map_err(|error| error.to_string())?;
        let end = changes.len().min(from.saturating_add(RECORD_PAGE_BYTES));
        Ok(Store::RecordChanges {
            changes: Bytes(changes.get(from..end).ok_or("no such page")?.to_vec()),
            heads,
            next: (end < changes.len()).then(|| u64::try_from(end).unwrap()),
            pending: false,
        })
    }

    /// The changes of `channel`'s record past `after`, as bytes, and the heads they bring a
    /// reader to.
    fn record_changes(
        channel: &Channel,
        after: &[automerge::ChangeHash],
    ) -> (Vec<u8>, Vec<String>) {
        let record = &channel.record;
        (
            record
                .get_changes(after)
                .iter()
                .flat_map(|change| change.raw_bytes().iter().copied())
                .collect(),
            record.get_heads().iter().map(ToString::to_string).collect(),
        )
    }

    /// Take `source`'s own run of changes into `channel`'s record once its last run arrives, and
    /// tell every other connection on it that the record changed.
    fn write(&self, channel: &str, source: u64, run: Vec<u8>, more: bool) -> Result<Store, String> {
        let mut channels = self.channels.lock().unwrap();
        let state = channels.entry(channel.to_owned()).or_default();
        let incoming = state.incoming.entry(source).or_default();
        incoming.extend(run);
        if more {
            return Ok(Store::Written);
        }
        let changes = std::mem::take(incoming);
        state.incoming.remove(&source);
        state
            .record
            .load_incremental(&changes)
            .map_err(|error| format!("not a change: {error}"))?;
        state
            .notices
            .retain(|sub| sub.peer == source || sub.tx.send(ContextUpdate::RecordChanged).is_ok());
        drop(channels);
        Ok(Store::Written)
    }
}

/// A shared typed fan-out service. Each call to [`connect`](Self::connect)
/// opens a fresh connection with explicit attribution.
#[derive(Clone, Default)]
pub struct TypedBroker {
    hub: Arc<Hub>,
}

impl TypedBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn connect(&self, user_id: &str) -> RawFd {
        self.connect_peer(user_id).1
    }

    fn connect_peer(&self, _user_id: &str) -> (u64, RawFd) {
        let peer = self.hub.next_peer.fetch_add(1, Ordering::Relaxed);
        let connection = Connection {
            hub: self.hub.clone(),
            peer,
            subscriptions: RefCell::default(),
        };
        let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        client.set_nonblocking(true).unwrap();
        thread::Builder::new()
            .name(format!("ui-typed-collab-{peer}"))
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                tokio::task::LocalSet::new().block_on(&runtime, async move {
                    let stream = tokio::net::UnixStream::from_std(server).unwrap();
                    connection.serve(stream).await;
                });
            })
            .unwrap();
        (peer, client.into_raw_fd())
    }

    /// Connect a production `CollaborationManager` to the shared test workspace.
    ///
    /// Returns once the manager's route is admitted and this service holds
    /// its record and presence subscriptions, so traffic sent from then on
    /// reaches it without any settling time.
    pub fn manager(&self, user_id: &str) -> CollaborationManager {
        let (peer, fd) = self.connect_peer(user_id);
        let manager = CollaborationManager::for_route_fd(
            fd,
            TEST_APP_ID,
            Uuid::nil(),
            None,
            Some(user_id.to_owned()),
        )
        .expect("typed test endpoint connects");
        assert!(
            manager.is_connected(),
            "{user_id} must connect to the typed broker"
        );
        self.wait_subscribed(TEST_APP_ID, peer, user_id);
        manager
    }

    /// Wait until `peer` holds its context subscription on the `app_id` workspace.
    fn wait_subscribed(&self, app_id: &str, peer: u64, user_id: &str) {
        let channel = ::collaboration::channel_key::format(app_id, &Uuid::nil().to_string(), "");
        tilo_test_env::wait_until(
            format!("{user_id}'s context subscription on {channel}"),
            || {
                self.hub
                    .channels
                    .lock()
                    .unwrap()
                    .get(&channel)
                    .is_some_and(|workspace| workspace.notices.iter().any(|sub| sub.peer == peer))
            },
        );
    }

    /// Connect a production `CollaborationManager` to its own `app_id` workspace,
    /// returning once this service holds its subscriptions there.
    pub fn route_manager(&self, user_id: &str, app_id: &str) -> CollaborationManager {
        let (peer, fd) = self.connect_peer(user_id);
        let manager = CollaborationManager::for_route_fd(
            fd,
            app_id,
            Uuid::nil(),
            None,
            Some(user_id.to_owned()),
        )
        .expect("typed test endpoint connects");
        self.wait_subscribed(app_id, peer, user_id);
        manager
    }
}

/// One endpoint's connection to the shared workspace.
struct Connection {
    hub: Arc<Hub>,
    peer: u64,
    subscriptions: RefCell<HashMap<u64, tokio::task::AbortHandle>>,
}

impl Connection {
    /// Answer the endpoint's calls on `stream` until it closes.
    #[expect(
        clippy::future_not_send,
        reason = "each connection is served on its own thread's `LocalSet`"
    )]
    async fn serve(self, stream: tokio::net::UnixStream) {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (frames, mut outgoing) = mpsc::unbounded_channel::<HostFrame>();
        let write = async {
            while let Some(frame) = outgoing.recv().await {
                if FRAMING
                    .write_value_async(&mut writer, &frame)
                    .await
                    .is_err()
                    || writer.flush().await.is_err()
                {
                    break;
                }
            }
        };
        let read = async {
            while let Ok(Some(Call { id, request })) =
                FRAMING.read_value_async::<_, Call>(&mut reader).await
            {
                self.accept(id, request, &frames);
            }
        };
        tokio::select! {
            () = write => {}
            () = read => {}
        }
        for (_, task) in self.subscriptions.borrow_mut().drain() {
            task.abort();
        }
    }

    fn accept(&self, id: u64, request: Operation, frames: &mpsc::UnboundedSender<HostFrame>) {
        let result = match request {
            Operation::Subscribe { record, topic } => {
                match self.subscribe(id, &record.key, topic, frames) {
                    Ok(task) => {
                        self.subscriptions.borrow_mut().insert(id, task);
                        Ok(Reply::Subscribed)
                    }
                    Err(error) => Err(error),
                }
            }
            Operation::Unsubscribe { subscription } => {
                if let Some(task) = self.subscriptions.borrow_mut().remove(&subscription) {
                    task.abort();
                }
                return;
            }
            Operation::Close { .. } | Operation::Leave { .. } | Operation::Answer { .. } => return,
            request => self.answer(request),
        };
        let _ = frames.send(HostFrame::Reply { id, result });
    }

    fn answer(&self, request: Operation) -> Result<Reply, String> {
        match request {
            // Presence is said on the connection an app's launcher hands it, never here.
            Operation::RetiredSay { .. } => Err("presence is not said on this wire".into()),
            Operation::Command {
                record,
                service: Service::Resources,
                body,
                ..
            } => {
                let channel = record.key;
                let reply = match serde_json::from_slice(&body).map_err(|e| e.to_string())? {
                    Command::RecordChanges { after, from, .. } => {
                        self.hub.changes(&channel, &after, from)?
                    }
                    Command::RecordChange { changes, more, .. } => {
                        self.hub.write(&channel, self.peer, changes.0, more)?
                    }
                    other => {
                        return Err(format!("the typed test host's store answers no {other:?}"));
                    }
                };
                serde_json::to_vec(&reply)
                    .map(Reply::Answer)
                    .map_err(|error| error.to_string())
            }
            // This harness stores no committed history and seals nothing: answer every open
            // with the explicit legacy mode so the route follows its record.
            Operation::Command {
                service: Service::CommittedDocument,
                body,
                ..
            } => {
                use runtime_collab::committed_document::{Reply as Read, Request as Ask};
                match Ask::decode(&body).map_err(std::string::ToString::to_string)? {
                    Ask::Writes => Read::Writes { sealed: false },
                    _ => Read::Legacy,
                }
                .encode()
                .map(Reply::Answer)
                .map_err(Into::into)
            }
            other => Err(format!("the typed test host does not answer {other:?}")),
        }
    }

    /// Start delivering `topic` of `channel` as the subscription `id`.
    fn subscribe(
        &self,
        id: u64,
        channel: &str,
        topic: Topic,
        frames: &mpsc::UnboundedSender<HostFrame>,
    ) -> Result<tokio::task::AbortHandle, String> {
        let frames = frames.clone();
        let deliver = move |event| {
            let _ = frames.send(HostFrame::Event {
                subscription: id,
                event,
            });
        };
        let mut channels = self.hub.channels.lock().unwrap();
        let state = channels.entry(channel.to_owned()).or_default();
        let task = match topic {
            // The record's change notices and what the others say: this harness has no roster
            // or permission snapshots to publish.
            Topic::Context => {
                let (tx, mut rx) = mpsc::unbounded_channel();
                state.notices.push(Subscription {
                    peer: self.peer,
                    tx,
                });
                tokio::task::spawn_local(async move {
                    while let Some(update) = rx.recv().await {
                        deliver(Event::Context(update));
                    }
                })
            }
            other => return Err(format!("the typed test host serves no {other:?} topic")),
        };
        drop(channels);
        Ok(task.abort_handle())
    }
}

/// A real change of a record that sets its `body` to `bytes`: what an app hands its record's
/// store.
pub fn change_carrying(bytes: Vec<u8>) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    doc.put(
        automerge::ROOT,
        "body",
        automerge::ScalarValue::Bytes(bytes),
    )
    .unwrap();
    doc.commit();
    doc.save_after(&[])
}

/// Each change `bytes` carries, as the store takes and hands them on.
pub fn changes_of(bytes: &[u8]) -> Vec<Vec<u8>> {
    automerge::Automerge::parse_changes(bytes)
        .unwrap()
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}
