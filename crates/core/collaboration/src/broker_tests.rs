#![cfg_attr(
    not(test),
    allow(
        dead_code,
        unused_imports,
        reason = "only the fixtures are used outside tests"
    )
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    reason = "the tests and the fixtures they share with the toolkit's tests are test code"
)]
use super::*;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};

use collab_types::presence::PresenceKey;
use runtime_collab::SurfaceDelta;

/// The concrete broker transport's bridge state behind a test manager.
pub(super) fn broker_state(manager: &CollaborationManager) -> BridgeState {
    match &manager.backend {
        Backend::Transport(transport) => transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
            .expect("a broker transport")
            .state
            .clone(),
        Backend::Disabled(_) => panic!("a broker transport"),
    }
}

/// The shovel's channel-key schedule once the key round in flight has
/// been applied: whether its timer is armed, and how many rounds it has
/// started.
#[cfg(test)]
fn key_schedule(manager: &CollaborationManager) -> KeyScheduleSeen {
    let Backend::Transport(transport) = &manager.backend else {
        panic!("a broker transport");
    };
    let transport = transport
        .as_any()
        .and_then(|any| any.downcast_ref::<BrokerTransport>())
        .expect("a broker transport");
    let (reply, seen) = mpsc::sync_channel(1);
    assert!(enqueue_sdk_command(
        &transport.outbound_tx,
        SdkCommand::KeyScheduleProbe(reply),
        &transport.state,
    ));
    tilo_test_env::recv("the shovel to report its key schedule", &seen)
}

/// How often the shovel found a queued document mutation's route not
/// publishable so far: read before the step that queues the mutation.
#[cfg(test)]
fn publication_hold_waits(state: &BridgeState) -> usize {
    state.publication_hold_waits.load(Ordering::Acquire)
}

/// Wait until the shovel has seen a queued document mutation find its
/// route not publishable since `before` ([`publication_hold_waits`]):
/// the mutation is held until the route changes. Read after
/// the mutation was queued, the count may already hold its one wait,
/// which then sleeps until the route changes without counting again.
#[cfg(test)]
fn wait_held_at_publication(state: &BridgeState, before: usize) {
    tilo_test_env::wait_until(
        "the queued mutation to wait at the publication hold",
        || state.publication_hold_waits.load(Ordering::Acquire) > before,
    );
}

#[cfg(unix)]
#[cfg(test)]
include!("committed_read_deadline_tests.rs");
#[cfg(unix)]
#[cfg(test)]
include!("directory_publication_tests.rs");
#[cfg(test)]
include!("committed_absent_tests.rs");
#[cfg(unix)]
#[cfg(test)]
include!("route_join_wait_tests.rs");
#[cfg(test)]
include!("cluster_route_tests.rs");
#[cfg(test)]
include!("committed_changes_tests.rs");
use crate::CollaborationState;

use crate::time::{Duration, Instant};
use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(unix)]
use std::os::fd::IntoRawFd;
use std::rc::Rc;

use runtime_collab::wire::{
    self, Call, ContextUpdate, Event, HostFrame, InvocationCall, LiveActivityCall, Operation,
    Publication, Reply, Service, SettingsCall, Topic,
};
use tokio::io::AsyncWriteExt as _;

/// A real change writing `text`, as a window's direct edit names itself: the bytes an app hands
/// its record's store.
fn change(text: &str) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    let message = tilo_actions::FrameworkChange::Native.change_message();
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "text", text).unwrap();
    doc.commit_with(automerge::transaction::CommitOptions::default().with_message(message));
    doc.save_after(&[])
}

/// Each change `bytes` carries, as the store takes it.
fn changes_of(bytes: &[u8]) -> Vec<Vec<u8>> {
    automerge::Automerge::parse_changes(bytes)
        .unwrap()
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// Changes the fake broker hands a reader holding `from`.
#[derive(Clone)]
pub struct FixtureChanges {
    from: [u8; 32],
    base: Vec<[u8; 32]>,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct Captures {
    /// Hold each next local document report until its hold is released, as a
    /// host waiting on the app's directory does; free ones are answered
    /// at once. Every report is refused: the fixture admits none.
    pub admit_holds: std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>,
    pub admits_asked: usize,
    /// The app and the records whose settings the channel hands out.
    pub settings: Option<(String, Arc<dyn settings_store::SettingsSource>)>,
    /// The app and the board whose live activity the channel hands out.
    pub live_activities: Option<(String, Arc<runtime_collab::LiveActivities>)>,
    pub committed_history_fixture: Option<Vec<u8>>,
    pub committed_revision: Option<[u8; 32]>,
    pub invocation_fixture_enabled: bool,
    pub invocations: std::collections::VecDeque<runtime_collab::Invocation>,
    pub received_invocations: usize,
    pub invocation_execution: Option<Result<bool, String>>,
    pub committed_recheck_hold: Option<tokio::sync::oneshot::Receiver<()>>,
    pub committed_ack_hold: Option<(mpsc::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
    pub committed_private_binding: Option<u64>,
    pub committed_ack_failure: Option<String>,
    pub committed_ack_reply: Option<runtime_collab::committed_document::Reply>,
    /// Refuse every committed Open, as a broker does for a revoked peer.
    pub committed_open_failure: Option<String>,
    /// Refuse committed Opens on these channels only, as the session
    /// host does for a route this connection holds no read ownership of.
    pub committed_open_refused_channels: Vec<String>,
    /// Answer a committed Open on these channels `Absent`, and hold
    /// their `Await`s, as the session host does for an on-demand record
    /// its workspace's hosts have none of.
    pub committed_absent_channels: Vec<String>,
    /// Never answer a committed Open on these channels, as a broker
    /// whose read waits for an answer that never comes.
    pub committed_open_unanswered_channels: Vec<String>,
    /// Hold committed-read `Await`s until this is notified, as the broker
    /// does until the read changes. Without it every `Await` fails, as a
    /// call to a broker can, and the reader re-Opens at its own pace.
    pub committed_awaits: Option<Arc<tokio::sync::Notify>>,
    /// Answer Opens `Busy`, as a broker whose byte budget other reads
    /// hold: the reader's place in its queue. While set, a held `Await`
    /// waits for the turn (cleared, then [`Captures::committed_awaits`]
    /// notified) as the broker's does.
    pub committed_busy: bool,
    /// An Open was answered `Busy`: once the turn comes the held
    /// `Await` answers with the Open's capture, as the broker's does.
    pub committed_queued: bool,
    /// The changes `OpenChanges` answers a reader holding their `from`
    /// revision with. `OpenChanges` is answered as the broker does: with
    /// these changes when they fit, otherwise exactly as the `Open` it
    /// stands for.
    pub committed_changes: Option<FixtureChanges>,
    /// Hold each next join until its hold is released, as a starved broker
    /// does.
    pub join_holds: std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>,
    /// Refuse every free join, as a broker does for a revoked peer.
    pub join_refusal: Option<String>,
    /// The workspace's status as the broker reports it, or why the broker
    /// cannot be asked; without it the host refuses to say.
    pub workspace_status:
        Option<tokio::sync::watch::Receiver<Result<runtime_collab::WorkspaceStatus, String>>>,
    pub committed_reads: Vec<runtime_collab::committed_document::Request>,
    /// Lose the connection on the next record write: the store may or
    /// may not have kept the change, an unknown outcome.
    pub fail_document: bool,
    /// Refuse this many record writes the way a store that will not take
    /// the change answers: a definite error, not a lost call.
    pub refuse_documents: usize,
    pub refused_documents: usize,
    pub document_holds: std::collections::VecDeque<DocumentSendHold>,
    /// Refuse every non-terminal invocation update, and lose the
    /// connection on the first terminal one, whose delivery is then
    /// unknown.
    pub fail_terminal: bool,
    /// Refuse this many invocation updates the way a broker that will not
    /// take them answers: a definite error, not a lost call.
    pub refuse_terminals: usize,
    pub terminal_updates: usize,
    pub invocation_updates: Vec<(u64, tilo_actions::InvocationUpdate)>,
    /// Each admission of a record on the connection, in order: the first call naming it, and
    /// each renewal of its committed read after that.
    pub joined: Vec<String>,
    /// The records whose committed read has been bound to its window once: a later binding is a
    /// renewal's.
    pub committed_bound: std::collections::HashSet<String>,
    /// Each stream subscribed to, by its route, in the order asked.
    pub subscribed: Vec<(String, Topic)>,
    pub joined_resource_types: Vec<String>,
    /// Each write of a record's changes the store took, in order: the changes it carried.
    pub record_writes: Vec<Vec<Vec<u8>>>,
    /// The channel of each read of a record's changes, in order.
    pub record_reads: Vec<String>,
    /// The action each record write named, in the order they came.
    pub record_commits: Vec<Option<collab_types::WorkspaceCommit>>,
    /// Each frame the app said of its person, in order.
    pub said: Vec<collab_types::presence::Presence>,
    pub action_publications: Vec<(String, Vec<(String, String)>)>,
    /// The connection's surface as its deltas built it, and each delta.
    pub surface_table: runtime_collab::SurfaceTable,
    pub surface_deltas: Vec<runtime_collab::SurfaceDelta>,
    /// Each invocation of the connection's own action, in arrival order,
    /// and whether its surface offered the action when it arrived.
    pub own_invocations: Vec<(tilo_actions::ActionAddress, bool)>,
    /// Refuse this many action-surface publications, as a broker whose
    /// admission is momentarily unavailable does.
    pub refuse_action_publications: usize,
    pub refused_action_publications: usize,
    pub action_directories: Vec<String>,
    pub refuse_action_directories: usize,
    pub directory_attempts: Vec<crate::time::Instant>,
    pub directory_ack: Option<Arc<tokio::sync::Notify>>,
    /// The channel service whose changes `Changed { GroupKeys }`
    /// follows; without one the host refuses the wait.
    pub key_changes: Option<Arc<tokio::sync::watch::Sender<u64>>>,
    /// Answers this channel's private service requests, as the session
    /// does on an app's route; without one the call is unimplemented,
    /// as on a broker with no owner behind it.
    #[allow(clippy::type_complexity, reason = "one fixture hook")]
    pub service_sessions: Option<
        Arc<
            dyn Fn(
                    &runtime_collab::BrowserServiceSessionCommand,
                ) -> Result<runtime_collab::BrowserServiceSessionReply, String>
                + Send
                + Sync,
        >,
    >,
}

pub struct DocumentSendHold {
    entered: mpsc::Sender<()>,
    release: tokio::sync::oneshot::Receiver<Result<(), String>>,
}

impl Captures {
    /// Hold the next record write: it says when it is entered, and is
    /// answered with what is sent (dropping the sender answers `Err`).
    pub fn hold_document(
        &mut self,
    ) -> (
        mpsc::Receiver<()>,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    ) {
        let (entered, observed) = mpsc::channel();
        let (release, receiver) = tokio::sync::oneshot::channel();
        self.document_holds.push_back(DocumentSendHold {
            entered,
            release: receiver,
        });
        (observed, release)
    }
}

/// Shared between the fake host's thread and the test.
pub type SharedCaptures = std::sync::Arc<std::sync::Mutex<Captures>>;

#[derive(Clone)]
pub struct SeedPresence {
    member: collab_types::MemberId,
    frame: collab_types::presence::Presence,
}

#[derive(Clone)]
pub struct SeedContext {
    permissions: Vec<String>,
}

/// What a fake host's channels start with, and what it records.
#[derive(Default)]
pub struct FakeBroker {
    pub captures: SharedCaptures,
    /// The changes every channel's record holds when it is first asked for.
    pub record: Vec<Vec<u8>>,
    /// Changes made elsewhere: each is taken into every record open on the
    /// connection, whose context feeds are told the record changed.
    pub peer_changes: Option<tokio_mpsc::UnboundedReceiver<Vec<u8>>>,
    pub presence_backlog: Vec<SeedPresence>,
    pub context_backlog: Vec<SeedContext>,
    pub terminate_context_stream: bool,
}

/// An answer the fake host sends once it is ready.
type FakeAnswer = Pin<Box<dyn Future<Output = Result<Reply, String>>>>;

fn answered(result: Result<Reply, String>) -> FakeAnswer {
    Box::pin(std::future::ready(result))
}

/// The fixture's answer to a committed read `decoded` on `channel_id`, as
/// the broker answers it, kept in [`Captures::committed_reads`].
fn committed_answer(
    captures: &SharedCaptures,
    channel_id: &str,
    decoded: Result<runtime_collab::committed_document::Request, String>,
) -> FakeAnswer {
    use runtime_collab::committed_document::{Reply as Read, Request as Ask};
    let encode = |reply: Read| reply.encode().map(Reply::Answer).map_err(str::to_owned);
    let result = (|| {
        let request = decoded?;
        // How the record is kept, which every route asks as it opens: never sealed here.
        if request == Ask::Writes {
            return Ok((encode(Read::Writes { sealed: false })?, None));
        }
        let mut captures = captures.lock().unwrap();
        // The end of a read, which a renewal starts with: nothing of it is kept here.
        if request == Ask::Leave {
            return Ok((encode(Read::Cancelled)?, None));
        }
        // A renewal binds its read to the window again: the host admits the connection to the
        // record once more, held or refused as the test asks, as a join was.
        let mut renewal = None;
        if request == Ask::PrivateWindowBinding
            && !captures.committed_bound.insert(channel_id.to_owned())
        {
            captures.joined.push(channel_id.to_owned());
            let kind = captures
                .joined
                .iter()
                .zip(captures.joined_resource_types.iter())
                .rev()
                .find(|(key, _)| key.as_str() == channel_id)
                .map(|(_, kind)| kind.clone())
                .unwrap_or_default();
            captures.joined_resource_types.push(kind);
            renewal = captures.join_holds.pop_front();
            if renewal.is_none()
                && let Some(error) = captures.join_refusal.clone()
            {
                return Err(error);
            }
        }
        captures.committed_reads.push(request.clone());
        let changes = captures.committed_changes.clone();
        let opens = matches!(request, Ask::Open { .. } | Ask::OpenChanges { .. });
        if opens {
            if let Some(error) = captures.committed_open_failure.clone() {
                return Err(error);
            }
            if captures
                .committed_open_refused_channels
                .iter()
                .any(|held| held == channel_id)
            {
                return Err("committed read ownership refused for this route".into());
            }
        }
        let busy = opens && captures.committed_busy;
        let current_revision = captures.committed_revision.unwrap_or([7; 32]);
        let absent = captures
            .committed_absent_channels
            .iter()
            .any(|held| held == channel_id)
            && matches!(request, Ask::Open { .. });
        let reply = match captures.committed_history_fixture.as_ref() {
            Some(_) if absent => Read::Absent,
            None => Read::Legacy,
            Some(_) if busy => {
                captures.committed_queued = true;
                Read::Busy
            }
            Some(bytes) => match request {
                Ask::PrivateWindowBinding => Read::PrivateWindowBinding {
                    instance: captures.committed_private_binding,
                },
                Ask::Open {
                    known_revision: Some(revision),
                } if revision == current_revision => Read::Unchanged { revision },
                Ask::OpenChanges { known_revision } => match changes {
                    _ if known_revision == current_revision => Read::Unchanged {
                        revision: known_revision,
                    },
                    Some(changes) if changes.from == known_revision => Read::Changes {
                        capture: 1,
                        revision: current_revision,
                        base: changes.base,
                        total: changes.bytes.len() as u32,
                        bytes: changes.bytes,
                        token: [3; 16],
                    },
                    _ => Read::Chunk {
                        capture: 1,
                        revision: current_revision,
                        total: bytes.len() as u32,
                        offset: 0,
                        bytes: bytes.clone(),
                        token: [3; 16],
                    },
                },
                Ask::Open { .. } => Read::Chunk {
                    capture: 1,
                    revision: current_revision,
                    total: bytes.len() as u32,
                    offset: 0,
                    bytes: bytes.clone(),
                    token: [3; 16],
                },
                Ask::Cancel { capture: 1, token } if token == [3; 16] => {
                    if let Some(error) = captures.committed_ack_failure.take() {
                        return Err(error);
                    }
                    captures
                        .committed_ack_reply
                        .take()
                        .unwrap_or(Read::Cancelled)
                }
                _ => return Err("fixture read continuation refused".into()),
            },
        };
        let hold = if renewal.is_some() {
            renewal
        } else if matches!(reply, Read::Unchanged { .. }) {
            captures.committed_recheck_hold.take()
        } else if matches!(reply, Read::Cancelled) {
            captures.committed_ack_hold.take().map(|(entered, hold)| {
                entered.send(()).unwrap();
                hold
            })
        } else {
            None
        };
        Ok((encode(reply)?, hold))
    })();
    Box::pin(async move {
        let (reply, hold) = result?;
        if let Some(hold) = hold {
            hold.await
                .map_err(|_| "fixture recheck hold dropped".to_owned())?;
        }
        Ok(reply)
    })
}

/// A channel's record as the store keeps it, and the context feeds told
/// when it changes elsewhere.
#[derive(Default)]
struct FakeRecord {
    doc: automerge::Automerge,
    feeds: Vec<Feed>,
    /// The runs of a write whose last run has not arrived.
    incoming: Vec<u8>,
}

impl FakeRecord {
    fn take(&mut self, change: &[u8]) -> Result<(), String> {
        self.doc
            .load_incremental(change)
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// One record a fake host's connection named.
struct FakeChannel {
    channel_id: String,
    record: Rc<RefCell<FakeRecord>>,
    presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
    context_backlog: Rc<RefCell<Vec<SeedContext>>>,
    settings: RefCell<Option<Rc<runtime_collab::settings::host::AppSettingsServer>>>,
    live_activity: RefCell<Option<Rc<runtime_collab::live_activity::LiveActivityServer>>>,
}

impl FakeChannel {
    fn app_id(&self) -> String {
        runtime_collab::route::channel_key::parse(&self.channel_id).0
    }
}

/// Delivers one subscription's events.
#[derive(Clone)]
struct Feed {
    frames: tokio_mpsc::UnboundedSender<HostFrame>,
    subscription: u64,
}

impl Feed {
    fn send(&self, event: Event) {
        let _ = self.frames.send(HostFrame::Event {
            subscription: self.subscription,
            event,
        });
    }
}

/// The collaboration host a test stands up: it answers one connection's
/// calls from `broker`'s seeds and records them in its captures.
struct FakeHost {
    broker: FakeBroker,
    frames: tokio_mpsc::UnboundedSender<HostFrame>,
    /// What the connection holds on each record it named, by record, made when first named.
    channels: RefCell<HashMap<String, Rc<tokio::sync::OnceCell<Rc<FakeChannel>>>>>,
    /// Each channel's record, by channel, and the changes a new one starts with.
    records: RefCell<HashMap<String, Rc<RefCell<FakeRecord>>>>,
    seed: RefCell<Vec<Vec<u8>>>,
    subscriptions: RefCell<HashMap<u64, (String, tokio::task::AbortHandle)>>,
    /// Ends the connection unanswered: a call whose outcome is unknown.
    hang_up: Rc<tokio::sync::Notify>,
}

/// Serve `broker` as the collaboration host on `stream` until either
/// side closes it. Runs on a `LocalSet`.
#[expect(
    clippy::future_not_send,
    reason = "the fake host keeps its channels in `Rc`s on the `LocalSet` it serves on"
)]
pub(super) async fn serve_fake_broker<S>(stream: S, mut broker: FakeBroker)
where
    S: AsyncRead + AsyncWrite + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (frames, mut outgoing) = tokio_mpsc::unbounded_channel();
    let peer_changes = broker.peer_changes.take();
    let seed = RefCell::new(broker.record.clone());
    let host = Rc::new(FakeHost {
        broker,
        frames,
        channels: RefCell::default(),
        records: RefCell::default(),
        seed,
        subscriptions: RefCell::default(),
        hang_up: Rc::new(tokio::sync::Notify::new()),
    });
    let write = async {
        while let Some(frame) = outgoing.recv().await {
            if wire::FRAMING
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
        while let Ok(Some(call)) = wire::FRAMING.read_value_async::<_, Call>(&mut reader).await {
            host.accept(call);
        }
    };
    let peers = async {
        if let Some(mut changes) = peer_changes {
            while let Some(change) = changes.recv().await {
                host.peer_change(change);
            }
        }
        std::future::pending::<()>().await;
    };
    let hang_up = host.hang_up.clone();
    tokio::select! {
        () = write => {}
        () = read => {}
        () = peers => {}
        () = hang_up.notified() => {}
    }
    for (_, (_, task)) in host.subscriptions.borrow_mut().drain() {
        task.abort();
    }
    host.channels.borrow_mut().clear();
}

impl FakeHost {
    fn captures(&self) -> std::sync::MutexGuard<'_, Captures> {
        self.broker.captures.lock().unwrap()
    }

    /// What the connection holds on `record`, opened as the first call naming it opens it.
    #[expect(
        clippy::future_not_send,
        reason = "the fake host keeps its channels in `Rc`s on the `LocalSet` it serves on"
    )]
    async fn channel(self: &Rc<Self>, record: wire::Record) -> Result<Rc<FakeChannel>, String> {
        let opened = Rc::clone(
            self.channels
                .borrow_mut()
                .entry(record.key.clone())
                .or_default(),
        );
        opened
            .get_or_try_init(|| self.open(record))
            .await
            .map(Rc::clone)
    }

    /// `answer` about the record `record` names, once the connection holds it.
    fn on(
        self: &Rc<Self>,
        record: wire::Record,
        answer: impl FnOnce(&Rc<Self>, &FakeChannel) -> FakeAnswer + 'static,
    ) -> FakeAnswer {
        let host = Rc::clone(self);
        Box::pin(async move {
            let channel = host.channel(record).await?;
            answer(&host, &channel).await
        })
    }

    /// The record of `channel_id`, made from the seed when first asked.
    fn record(&self, channel_id: &str) -> Rc<RefCell<FakeRecord>> {
        let mut records = self.records.borrow_mut();
        let record = records.entry(channel_id.to_owned()).or_insert_with(|| {
            let mut record = FakeRecord::default();
            for change in self.seed.borrow().iter() {
                record.take(change).unwrap();
            }
            Rc::new(RefCell::new(record))
        });
        Rc::clone(record)
    }

    /// A change made elsewhere: every record takes it and its feeds hear so.
    fn peer_change(&self, change: Vec<u8>) {
        for record in self.records.borrow().values() {
            let mut record = record.borrow_mut();
            record.take(&change).unwrap();
            for feed in &record.feeds {
                feed.send(Event::Context(ContextUpdate::RecordChanged));
            }
        }
        self.seed.borrow_mut().push(change);
    }

    /// Lose the connection, never answering the call at hand.
    fn hang_up(&self) -> FakeAnswer {
        self.hang_up.notify_one();
        Box::pin(std::future::pending())
    }

    fn accept(self: &Rc<Self>, Call { id, request }: Call) {
        match request {
            Operation::Subscribe { record, topic } => {
                let host = Rc::clone(self);
                tokio::task::spawn_local(async move { host.subscribe(id, record, topic).await });
            }
            Operation::Unsubscribe { subscription } => {
                if let Some((_, task)) = self.subscriptions.borrow_mut().remove(&subscription) {
                    task.abort();
                }
            }
            Operation::Leave { record } => {
                self.channels.borrow_mut().remove(&record);
                self.subscriptions
                    .borrow_mut()
                    .retain(|_, (channel, task)| {
                        let open = *channel != record;
                        if !open {
                            task.abort();
                        }
                        open
                    });
            }
            // Nothing here asks the endpoint anything.
            Operation::Answer { .. } => {}
            request => {
                let answer = self.answer(request);
                let frames = self.frames.clone();
                tokio::task::spawn_local(async move {
                    let result = answer.await;
                    let _ = frames.send(HostFrame::Reply { id, result });
                });
            }
        }
    }

    #[expect(
        clippy::future_not_send,
        reason = "the fake host keeps its channels in `Rc`s on the `LocalSet` it serves on"
    )]
    async fn subscribe(self: Rc<Self>, id: u64, record: wire::Record, topic: Topic) {
        let feed = Feed {
            frames: self.frames.clone(),
            subscription: id,
        };
        let key = record.key.clone();
        let delivery = self
            .channel(record)
            .await
            .and_then(|channel| self.topic(&channel, topic, feed.clone()));
        let delivery = match delivery {
            Ok(delivery) => delivery,
            Err(error) => {
                let _ = self.frames.send(HostFrame::Reply {
                    id,
                    result: Err(error),
                });
                return;
            }
        };
        let _ = self.frames.send(HostFrame::Reply {
            id,
            result: Ok(Reply::Subscribed),
        });
        let task = tokio::task::spawn_local(async move {
            let reason = delivery.await;
            let _ = feed.frames.send(HostFrame::Ended {
                subscription: id,
                reason,
            });
        });
        self.subscriptions
            .borrow_mut()
            .insert(id, (key, task.abort_handle()));
    }

    /// What `topic` delivers on `channel`, ending with why it ended.
    fn topic(
        &self,
        channel: &Rc<FakeChannel>,
        topic: Topic,
        feed: Feed,
    ) -> Result<Pin<Box<dyn Future<Output = String>>>, String> {
        let captures = self.broker.captures.clone();
        captures
            .lock()
            .unwrap()
            .subscribed
            .push((channel.channel_id.clone(), topic.clone()));
        Ok(match topic {
            Topic::Context => {
                channel.record.borrow_mut().feeds.push(feed.clone());
                let backlog = std::mem::take(&mut *channel.context_backlog.borrow_mut());
                let heard = std::mem::take(&mut *channel.presence_backlog.borrow_mut());
                let terminate = self.broker.terminate_context_stream;
                Box::pin(async move {
                    for seed in heard {
                        feed.send(Event::Context(ContextUpdate::Heard {
                            member: seed.member,
                            frame: Box::new(seed.frame),
                        }));
                    }
                    for seed in backlog {
                        feed.send(Event::Context(ContextUpdate::Snapshot {
                            permissions: seed.permissions,
                            apps: Vec::new(),
                        }));
                    }
                    if terminate {
                        return "fixture context source gone".to_owned();
                    }
                    std::future::pending().await
                })
            }
            Topic::Invocations { stable_ids } => {
                if !captures.lock().unwrap().invocation_fixture_enabled {
                    return Err("fixture has no invocation stream".into());
                }
                assert!(stable_ids);
                Box::pin(async move {
                    loop {
                        let invocation = {
                            let mut captures = captures.lock().unwrap();
                            let invocation = captures.invocations.pop_front();
                            if invocation.is_some() {
                                captures.received_invocations += 1;
                            }
                            invocation
                        };
                        match invocation {
                            Some(invocation) => {
                                feed.send(Event::Invocation(wire::Invocation {
                                    id: invocation.id,
                                    invocation: invocation.invocation_uuid,
                                    resource: invocation.resource,
                                    action: invocation.action,
                                    argument: invocation.argument,
                                    actor: invocation.actor,
                                    context: Some(invocation.context_json)
                                        .filter(|context| !context.is_empty()),
                                    typed: invocation.typed,
                                }));
                            }
                            None => tokio::time::sleep(Duration::from_millis(1)).await,
                        }
                    }
                })
            }
            other => return Err(format!("the fixture serves no {other:?} topic")),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per call the fixture answers"
    )]
    fn answer(self: &Rc<Self>, request: Operation) -> FakeAnswer {
        match request {
            Operation::Workspace { seen, .. } => {
                let Some(mut status) = self.captures().workspace_status.clone() else {
                    return answered(Err("the fixture reports no workspace status".into()));
                };
                Box::pin(async move {
                    loop {
                        let now = status.borrow_and_update().clone();
                        match (now, &seen) {
                            (Err(reason), _) => return Err(reason),
                            (Ok(now), None) => return Ok(Reply::Workspace(now)),
                            (Ok(now), Some(seen)) if now != *seen => {
                                return Ok(Reply::Workspace(now));
                            }
                            (Ok(_), Some(_)) => {}
                        }
                        status
                            .changed()
                            .await
                            .map_err(|_| "workspace status ended".to_owned())?;
                    }
                })
            }
            Operation::Publish {
                record,
                publication,
            } => self.on(record, move |host, channel| {
                host.publish(channel, publication)
            }),
            Operation::Say { record, frame } => self.on(record, move |host, _| {
                host.captures().said.push(frame);
                answered(Ok(Reply::Done))
            }),
            Operation::Command {
                record,
                service,
                body,
                ..
            } => self.on(record, move |host, channel| match service {
                Service::CommittedDocument => host.committed_document(channel, &body),
                Service::RemoteAction => answered(host.remote_action(&body)),
                Service::BrowserServiceSession => answered(host.browser_service_session(&body)),
                Service::Resources => host.resource_command(channel, &body),
                other => answered(Err(format!("the fixture serves no {other:?}"))),
            }),
            Operation::Changed {
                of: wire::Changes::GroupKeys,
                after,
                ..
            } => {
                let Some(changes) = self.captures().key_changes.clone() else {
                    return answered(Err("fixture has no channel service".into()));
                };
                let mut changed = changes.subscribe();
                Box::pin(async move {
                    let change = *changed
                        .wait_for(|change| *change != after)
                        .await
                        .map_err(|_| "fixture service gone".to_owned())?;
                    Ok(Reply::Changed(change))
                })
            }
            Operation::Invocation { id, call, .. } => match call {
                InvocationCall::Cancelled => {
                    answered(match self.captures().invocation_execution.as_ref() {
                        Some(Ok(cancelled)) => Ok(Reply::Cancelled(*cancelled)),
                        Some(Err(error)) => Err(error.clone()),
                        None => Err("fixture has no invocation".into()),
                    })
                }
                InvocationCall::Update(update) => self.update_invocation(id, &update),
                InvocationCall::Complete { .. } => {
                    answered(Err("the fixture completes no invocations".into()))
                }
            },
            Operation::Settings { record, call } => self.on(record, move |host, channel| {
                let settings = match host.settings(channel) {
                    Ok(settings) => settings,
                    Err(error) => return answered(Err(error)),
                };
                let view = |(revision, document)| Reply::Settings { revision, document };
                match call {
                    SettingsCall::Read { after } => {
                        Box::pin(async move { settings.read(after).await.map(view) })
                    }
                    SettingsCall::Update {
                        section,
                        values,
                        action,
                    } => answered(
                        settings
                            .update(&section, &values, action.as_deref())
                            .map(view),
                    ),
                    SettingsCall::Display { command, origin } => {
                        answered(settings.display(&command, origin).map(Reply::Answer))
                    }
                }
            }),
            Operation::LiveActivity { record, call } => self.on(record, move |host, channel| {
                let activity = match host.live_activity(channel) {
                    Ok(activity) => activity,
                    Err(error) => return answered(Err(error)),
                };
                match call {
                    LiveActivityCall::Show(shown) => {
                        answered(activity.show(&shown).map(|()| Reply::Done))
                    }
                    LiveActivityCall::Controls { after } => Box::pin(async move {
                        activity
                            .controls(after)
                            .await
                            .map(|(serial, controls)| Reply::Controls { serial, controls })
                    }),
                }
            }),
            Operation::Admit { .. } => {
                let hold = {
                    let mut captures = self.captures();
                    captures.admits_asked += 1;
                    captures.admit_holds.pop_front()
                };
                Box::pin(async move {
                    if let Some(hold) = hold {
                        let _ = hold.await;
                    }
                    Err("the fixture admits no reports".into())
                })
            }
            other => answered(Err(format!("the fixture does not answer {other:?}"))),
        }
    }

    /// What the connection holds on `record`, as the first call naming it opens it: the open is
    /// recorded, and held or refused as the test asks.
    #[expect(
        clippy::future_not_send,
        reason = "the fake host keeps its channels in `Rc`s on the `LocalSet` it serves on"
    )]
    async fn open(&self, record: wire::Record) -> Result<Rc<FakeChannel>, String> {
        let held = {
            let mut captures = self.captures();
            captures.joined.push(record.key.clone());
            captures
                .joined_resource_types
                .push(record.kind.clone().unwrap_or_default());
            let held = captures.join_holds.pop_front();
            if held.is_none()
                && let Some(error) = captures.join_refusal.clone()
            {
                return Err(error);
            }
            held
        };
        if let Some(held) = held {
            // `JoiningProbe::REFUSAL`, word for word; the probe is not built on every target.
            held.await
                .map_err(|_| "fixture join hold dropped".to_owned())?;
        }
        Ok(Rc::new(FakeChannel {
            record: self.record(&record.key),
            channel_id: record.key,
            presence_backlog: Rc::new(RefCell::new(self.broker.presence_backlog.clone())),
            context_backlog: Rc::new(RefCell::new(self.broker.context_backlog.clone())),
            settings: RefCell::default(),
            live_activity: RefCell::default(),
        }))
    }

    fn settings(
        &self,
        channel: &FakeChannel,
    ) -> Result<Rc<runtime_collab::settings::host::AppSettingsServer>, String> {
        if let Some(settings) = channel.settings.borrow().as_ref() {
            return Ok(Rc::clone(settings));
        }
        let (app, source) = self.captures().settings.clone().ok_or("no settings")?;
        let settings = Rc::new(runtime_collab::settings::host::AppSettingsServer::new(
            app,
            source,
            Arc::new(|| Ok(())),
        ));
        *channel.settings.borrow_mut() = Some(Rc::clone(&settings));
        Ok(settings)
    }

    fn live_activity(
        &self,
        channel: &FakeChannel,
    ) -> Result<Rc<runtime_collab::live_activity::LiveActivityServer>, String> {
        if let Some(activity) = channel.live_activity.borrow().as_ref() {
            return Ok(Rc::clone(activity));
        }
        let (app, board) = self
            .captures()
            .live_activities
            .clone()
            .ok_or("no live activities")?;
        let activity = Rc::new(runtime_collab::live_activity::LiveActivityServer::new(
            app,
            board,
            Arc::new(|| Ok(())),
        ));
        *channel.live_activity.borrow_mut() = Some(Rc::clone(&activity));
        Ok(activity)
    }

    fn publish(&self, channel: &FakeChannel, publication: Publication) -> FakeAnswer {
        match publication {
            Publication::Surface(delta) => {
                let delta: runtime_collab::SurfaceDelta = match serde_json::from_str(&delta) {
                    Ok(delta) => delta,
                    Err(error) => return answered(Err(error.to_string())),
                };
                let mut captures = self.captures();
                if captures.refuse_action_publications > 0 {
                    captures.refuse_action_publications -= 1;
                    captures.refused_action_publications += 1;
                    return answered(Err("current host worker authority unavailable".into()));
                }
                if let Err(reason) = captures.surface_table.apply(&delta) {
                    return answered(Err(reason));
                }
                let actions = captures
                    .surface_table
                    .actions()
                    .into_iter()
                    .map(|action| (action.resource_label, action.action))
                    .collect();
                captures.surface_deltas.push(delta);
                captures
                    .action_publications
                    .push((channel.app_id(), actions));
                answered(Ok(Reply::Done))
            }
            Publication::Directory => {
                let mut captures = self.captures();
                captures.directory_attempts.push(crate::time::now());
                let refused = captures.refuse_action_directories > 0;
                if refused {
                    captures.refuse_action_directories -= 1;
                }
                captures.action_directories.push(channel.app_id());
                let ack = captures.directory_ack.clone();
                drop(captures);
                Box::pin(async move {
                    if let Some(ack) = ack {
                        ack.notified().await;
                    }
                    if refused {
                        Err("fixture directory refusal".into())
                    } else {
                        Ok(Reply::Done)
                    }
                })
            }
            Publication::Report(_) => answered(Ok(Reply::Done)),
            other => answered(Err(format!("the fixture takes no {other:?}"))),
        }
    }

    /// A store command on the channel's record, answered as the device's
    /// store does: the changes past what the reader holds, or the writer's
    /// own changes taken.
    fn resource_command(&self, channel: &FakeChannel, body: &[u8]) -> FakeAnswer {
        use collab_types::store::wire::{Command, Reply as Store};
        let command: Command = match serde_json::from_slice(body) {
            Ok(command) => command,
            Err(error) => return answered(Err(error.to_string())),
        };
        match command {
            Command::RecordChanges { after, from, .. } => {
                if from == 0 {
                    self.captures()
                        .record_reads
                        .push(channel.channel_id.clone());
                }
                let after = after
                    .iter()
                    .map(|head| head.parse().unwrap())
                    .collect::<Vec<automerge::ChangeHash>>();
                let record = channel.record.borrow();
                let changes: Vec<u8> = record
                    .doc
                    .get_changes(&after)
                    .iter()
                    .flat_map(|change| change.raw_bytes().iter().copied())
                    .collect();
                let from = usize::try_from(from).unwrap();
                let end = changes
                    .len()
                    .min(from + collab_types::store::wire::RECORD_PAGE_BYTES);
                answered(store_answer(&Store::RecordChanges {
                    changes: collab_types::store::wire::Bytes(changes[from..end].to_vec()),
                    heads: record
                        .doc
                        .get_heads()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    next: (end < changes.len()).then(|| u64::try_from(end).unwrap()),
                    pending: false,
                }))
            }
            Command::RecordChange {
                changes,
                more,
                commit,
                ..
            } => {
                let mut record = channel.record.borrow_mut();
                record.incoming.extend(changes.0);
                if more {
                    return answered(store_answer(&Store::Written));
                }
                let changes = changes_of(&std::mem::take(&mut record.incoming));
                drop(record);
                self.captures().record_commits.push(commit);
                self.write_record(channel, changes)
            }
            other => answered(Err(format!("the fixture's store answers no {other:?}"))),
        }
    }

    fn write_record(&self, channel: &FakeChannel, changes: Vec<Vec<u8>>) -> FakeAnswer {
        let mut captures = self.captures();
        if captures.fail_document {
            // The call is lost on its way back: the store may or may not
            // have kept the change, which is an unknown outcome, not an
            // answer. A refusal below is the opposite: the store read the
            // change and said it will not take this one.
            drop(captures);
            return self.hang_up();
        }
        if captures.refuse_documents > 0 {
            captures.refuse_documents -= 1;
            captures.refused_documents += 1;
            tracing::warn!("collab: fixture refuses one record change");
            return answered(Err("so.tilo.chat named no kind for a new record".into()));
        }
        captures.record_writes.push(changes.clone());
        let hold = captures.document_holds.pop_front();
        drop(captures);
        let record = Rc::clone(&channel.record);
        let written = move || {
            let mut record = record.borrow_mut();
            for change in &changes {
                record.take(change)?;
            }
            store_answer(&collab_types::store::wire::Reply::Written)
        };
        let Some(hold) = hold else {
            return answered(written());
        };
        hold.entered.send(()).unwrap();
        Box::pin(async move {
            hold.release
                .await
                .map_err(|_| "document hold dropped".to_owned())??;
            written()
        })
    }

    fn update_invocation(&self, id: u64, update: &str) -> FakeAnswer {
        let update: tilo_actions::InvocationUpdate = serde_json::from_str(update).unwrap();
        let mut captures = self.captures();
        let terminal = update.is_terminal();
        if captures.invocation_fixture_enabled {
            captures.invocation_updates.push((id, update));
        }
        captures.terminal_updates += 1;
        if captures.fail_terminal {
            if terminal {
                // No answer comes back: the peer may or may not have
                // kept this update, an unknown outcome.
                drop(captures);
                return self.hang_up();
            }
            return answered(Err("this invocation is not accepted here".into()));
        }
        if captures.refuse_terminals > 0 {
            captures.refuse_terminals -= 1;
            tracing::warn!("collab: fixture refuses one invocation update");
            return answered(Err("this invocation update was not accepted".into()));
        }
        answered(Ok(Reply::Done))
    }

    /// Admits an invocation of the connection's own action only when
    /// the surface it published offers it, as the session action host
    /// does: anything else is refused as unknown. Admitted ones are
    /// accepted and never run.
    /// A private service request, answered as the session's owner
    /// would through `service_sessions`; refused without one.
    fn browser_service_session(&self, body: &[u8]) -> Result<Reply, String> {
        let Some(serve) = self.captures().service_sessions.clone() else {
            return Err("fixture has no owner".into());
        };
        let command =
            runtime_collab::BrowserServiceSessionCommand::decode(body).map_err(str::to_owned)?;
        let reply = serve(&command)?;
        reply
            .encode()
            .map(|bytes| Reply::Answer(bytes.to_vec()))
            .map_err(str::to_owned)
    }

    fn remote_action(&self, frame: &[u8]) -> Result<Reply, String> {
        use tilo_actions::remote::{
            ActionCallError, InvocationReceipt, ReceiptStatus, RemoteActionFrame,
            RemoteActionMessage,
        };
        let frame = RemoteActionFrame::decode(frame).map_err(|e| e.to_string())?;
        let RemoteActionMessage::Invoke(request) = &frame.message else {
            return Err("the fixture answers invocations only".into());
        };
        let address = request.target.address.clone();
        let mut captures = self.captures();
        let offered = request.target.provider == tilo_actions::remote::OWN_PROVIDER
            && captures.surface_table.actions().iter().any(|action| {
                action.resource == address.resource && action.action == address.action
            });
        captures.own_invocations.push((address, offered));
        let answer = if offered {
            RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Accepted,
            })
        } else {
            tilo_actions::remote::refuse(&frame.message, ActionCallError::UnknownAction)
                .expect("an invocation has a refusal")
        };
        RemoteActionFrame::new(answer)
            .encode()
            .map(Reply::Answer)
            .map_err(|error| error.to_string())
    }

    fn committed_document(&self, channel: &FakeChannel, body: &[u8]) -> FakeAnswer {
        use runtime_collab::committed_document::Request as Ask;
        let decoded = Ask::decode(body).map_err(|error| error.to_string());
        if let Ok(Ask::Await { known_revision }) = decoded {
            let held = {
                let mut captures = self.captures();
                captures.committed_reads.push(Ask::Await { known_revision });
                captures.committed_awaits.clone()
            };
            let Some(change) = held else {
                return answered(Err("invalid committed read request".into()));
            };
            let captures = self.broker.captures.clone();
            let channel = channel.channel_id.clone();
            return Box::pin(async move {
                loop {
                    let notified = change.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    let absent = captures
                        .lock()
                        .unwrap()
                        .committed_absent_channels
                        .contains(&channel);
                    if absent {
                        notified.await;
                        continue;
                    }
                    // `None`: the wait ends; `Some(current)`: it holds
                    // while `current` is what the reader knows, or while
                    // its turn in a busy broker's queue has not come.
                    let holds = {
                        let mut captures = captures.lock().unwrap();
                        if captures.committed_busy && captures.committed_queued {
                            Some(known_revision)
                        } else if std::mem::take(&mut captures.committed_queued) {
                            None
                        } else {
                            (captures.committed_open_failure.is_none()
                                && captures.committed_history_fixture.is_some())
                            .then(|| captures.committed_revision.unwrap_or([7; 32]))
                            .filter(|current| Some(*current) == known_revision)
                            .map(Some)
                        }
                    };
                    if holds.is_none() {
                        break;
                    }
                    notified.await;
                }
                // The wait answers as the Open it stands for.
                let open = known_revision.map_or(
                    Ask::Open {
                        known_revision: None,
                    },
                    |known_revision| Ask::OpenChanges { known_revision },
                );
                committed_answer(&captures, &channel, Ok(open)).await
            });
        }
        if let Ok(request @ (Ask::Open { .. } | Ask::OpenChanges { .. })) = &decoded {
            let mut captures = self.captures();
            if captures
                .committed_open_unanswered_channels
                .contains(&channel.channel_id)
            {
                captures.committed_reads.push(request.clone());
                return Box::pin(std::future::pending());
            }
        }
        committed_answer(&self.broker.captures, &channel.channel_id, decoded)
    }
}

/// `reply` as the device's store sends it.
fn store_answer(reply: &collab_types::store::wire::Reply) -> Result<Reply, String> {
    serde_json::to_vec(reply)
        .map(Reply::Answer)
        .map_err(|error| error.to_string())
}

#[test]
#[cfg(unix)]
fn inherited_identity_delegates_keys() {
    use std::os::fd::AsFd as _;
    use tilo_identity_client::wire::{FRAMING, Request, Response};

    let cluster_id = Uuid::new_v4();
    let workspace = cluster_id.to_string();
    let (app, mut host) = std::os::unix::net::UnixStream::pair().unwrap();
    let expected_workspace = workspace.clone();
    let worker = thread::spawn(move || {
        assert!(matches!(
            FRAMING.read_value::<_, Request>(&mut host).unwrap(),
            Some(Request::Metadata)
        ));
        FRAMING
            .write_value(
                &mut host,
                &Response::Metadata {
                    display_name: "Ada".into(),
                    avatar_color: [1, 2, 3],
                    group_public_key: [4; 32],
                    signing_public_key: [5; 32],
                    device_certificate: Some(b"fixture".to_vec()),
                    threshold: 1,
                    total_shares: 1,
                    share_index: 1,
                },
            )
            .unwrap();
        assert!(matches!(
            FRAMING.read_value::<_, Request>(&mut host).unwrap(),
            Some(Request::GroupKeys { workspace_id, app_id, document_id, known_generation })
                if workspace_id == expected_workspace && app_id == "notes"
                    && document_id == "page" && known_generation == 3
        ));
        FRAMING
            .write_value(
                &mut host,
                &Response::GroupKeys {
                    access: tilo_identity_client::wire::KeyAccess::Removed,
                    generation: 4,
                    keys: Vec::new(),
                },
            )
            .unwrap();
    });
    let identity = tilo_identity_client::inherited::connect_service(app.as_fd()).unwrap();
    let joined = sdk_join_identity_from(identity);
    assert_eq!(joined.member_kind, "person");
    let SdkDocumentSigner::Certified(signer) = joined.signer else {
        panic!("an inherited identity must retain its broker signer");
    };
    let keys = signer.group_keys(&workspace, "notes", "page", 3).unwrap();
    assert_eq!(keys.access, tilo_identity_client::wire::KeyAccess::Removed);
    assert_eq!(keys.generation, 4);
    worker.join().unwrap();
}

/// Stand up a fake collaboration host on one end of a Unix socket
/// pair; returns the client-side raw fd + captures state for assertions.
#[cfg(unix)]
pub fn stand_up_fake_broker(broker: FakeBroker) -> (RawFd, SharedCaptures, thread::JoinHandle<()>) {
    let captures = broker.captures.clone();
    let (server_side, client_side) = std::os::unix::net::UnixStream::pair().unwrap();
    server_side.set_nonblocking(true).unwrap();
    client_side.set_nonblocking(true).unwrap();
    let client_fd = client_side.into_raw_fd();

    let handle = thread::Builder::new()
        .name("ui-collab-test-broker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            LocalSet::new().block_on(&rt, async move {
                let stream =
                    tokio::net::UnixStream::from_std(server_side).expect("wrap server side");
                serve_fake_broker(stream, broker).await;
            });
        })
        .unwrap();

    (client_fd, captures, handle)
}

#[cfg(unix)]
fn committed_route_over_fake_broker(
    broker: FakeBroker,
) -> (CollaborationManager, SharedCaptures, thread::JoinHandle<()>) {
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let workspace = Uuid::new_v4();
    let manager = CollaborationManager::for_route_from(
        SdkConnectSource::Fd(fd),
        "so.tilo.notes".into(),
        workspace,
        Some("tab:0000000000000003".into()),
        crate::local_identity::own_name(),
        Some(SdkJoinIdentity {
            member_kind: "person",
            signer: harness_document_signer(),
        }),
        "person",
    )
    .expect("the fake broker's committed route opens");
    (manager, captures, thread)
}

#[test]
fn session_owner_commit_sdk_failure_stops_queued_terminal() {
    let broker = FakeBroker::default();
    let (mut manager, captures, thread) = committed_route_over_fake_broker(broker);
    captures.lock().unwrap().fail_document = true;
    manager.send(CollabRequest::Document {
        mutation: collab_types::DocumentMutation::Snapshot(change("committed worker mutation")),
        commit: None,
    });
    manager.send(CollabRequest::UpdateInvocation {
        id: 42,
        update: tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput::default(),
        },
    });
    tilo_test_env::wait_until(
        "the SDK to disconnect after the owner document RPC failed",
        || !manager.is_connected(),
    );
    assert_eq!(
        captures.lock().unwrap().terminal_updates,
        0,
        "failed document publication must not be followed by successful completion"
    );
    drop(manager);
    thread.join().unwrap();
}

/// A route kept beside the window's own whose join the broker refuses
/// (here its committed read) says why on the route, where diagnostics
/// and the window read it, instead of staying unready without a word.
#[test]
#[cfg(unix)]
fn a_refused_route_join_says_why() {
    let workspace = Uuid::new_v4();
    let primary = RouteSpec::document("so.tilo.notes", workspace, "tab:0000000000000001");
    let beside = RouteSpec::document(
        "so.tilo.chat",
        workspace,
        collab_types::workspace_document_id("tilo.workspace.emoji"),
    );
    let (mut manager, _) = CollaborationManager::native_initial_named_invocation_fixture_for_test(
        primary.clone(),
        automerge::AutoCommit::new().save(),
        std::slice::from_ref(&beside),
    );
    assert_eq!(manager.route_join_failure(&beside), None);
    manager.join_route(
        beside.clone(),
        collab_types::document::ResourceType::new("tilo.rich-text"),
    );
    tilo_test_env::wait_until("the refused join to say why", || {
        manager.route_join_failure(&beside).is_some()
    });
    let reason = manager.route_join_failure(&beside).unwrap();
    assert!(
        reason.contains("committed read ownership refused for this route"),
        "{reason}"
    );
    assert_eq!(
        manager.route_join_failure(&primary),
        None,
        "the window's own route joined"
    );
}

/// A write larger than one command goes to the store in runs, each naming the action it is
/// part of, and the store takes it whole as one write once the last run arrives.
#[test]
fn a_large_write_under_an_action_reaches_the_store_whole() {
    use automerge::transaction::Transactable as _;
    let (mut manager, captured, thread) = committed_route_over_fake_broker(FakeBroker::default());
    // Letters no compression shrinks, so the bytes stay larger than one run.
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut doc = automerge::AutoCommit::new();
    for index in 0..6 {
        let value: String = (0..1024 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                char::from(b'a' + u8::try_from(seed % 26).unwrap())
            })
            .collect();
        doc.put(automerge::ROOT, format!("part-{index}"), value)
            .unwrap();
        doc.commit();
    }
    let bytes = doc.save_after(&[]);
    assert!(bytes.len() > collab_types::store::wire::RECORD_PAGE_BYTES);
    let commit = collab_types::WorkspaceCommit {
        version: collab_types::COMMIT_VERSION,
        workspace: Uuid::new_v4(),
        author: collab_types::CommitAuthor {
            actor: None,
            device: [0; 32],
        },
        at_ms: 1_700_000_000_000,
        invocation: None,
        run: None,
        parts: Vec::new(),
    };
    assert!(manager.send(CollabRequest::document_action(
        DocumentMutation::Incremental(bytes.clone()),
        commit.clone(),
    )));
    manager
        .begin_document_publication_barrier()
        .unwrap()
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap();
    let captured_now = captured.lock().unwrap();
    assert_eq!(
        captured_now.record_writes,
        [changes_of(&bytes)],
        "one write, whole"
    );
    assert_eq!(
        captured_now.record_commits,
        [Some(commit)],
        "under its action"
    );
    drop(captured_now);
    drop(manager);
    thread.join().unwrap();
}

/// The store may answer one write with a definite refusal — it read the
/// change and will not take it, here because the record it would create
/// was named without its kind. That is not a lost connection: the bridge
/// stays open, the app stays published and reachable, and the very next
/// change goes through.
#[test]
fn a_refused_document_commit_fails_only_that_change() {
    let broker = FakeBroker::default();
    let (mut manager, captured, thread) = committed_route_over_fake_broker(broker);
    captured.lock().unwrap().refuse_documents = 1;
    assert!(manager.send(CollabRequest::document_incremental(change(
        "refused change"
    ))));
    let refused = manager.begin_document_publication_barrier().unwrap();
    let error = refused
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap_err();
    assert!(
        matches!(&error, DocumentPublicationError::Refused(reason)
            if reason.contains("named no kind for a new record")),
        "the refused change is reported as refused, not as an unknown outcome: {error}"
    );
    assert!(
        manager.is_connected(),
        "a definite refusal is an answer, not a lost connection"
    );
    assert!(broker_state(&manager).is_healthy());
    assert!(
        manager
            .refused_change()
            .is_some_and(|said| said.contains("named no kind")),
        "the connection stays up, so the owner is told which change was refused"
    );
    assert_eq!(
        captured.lock().unwrap().refused_documents,
        1,
        "the broker answered about exactly one change"
    );
    assert!(
        broker_state(&manager).is_healthy(),
        "the refusal must not withdraw the app's publication"
    );
    // The app's publication still works: a later change goes through.
    assert!(manager.send(CollabRequest::document_incremental(change("later change"))));
    let later = manager.begin_document_publication_barrier().unwrap();
    assert_eq!(later.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    assert_eq!(captured.lock().unwrap().record_writes.len(), 1);
    assert!(manager.is_connected());
    drop(manager);
    thread.join().unwrap();
}

/// The outcome-unknown half of the same path: a write that never
/// got an answer still ends collaboration, as it always has.
#[test]
fn a_document_commit_whose_outcome_is_unknown_still_closes_the_bridge() {
    let broker = FakeBroker::default();
    let (mut manager, captured, thread) = committed_route_over_fake_broker(broker);
    captured.lock().unwrap().fail_document = true;
    assert!(manager.send(CollabRequest::document_incremental(change(
        "unknown change"
    ))));
    tilo_test_env::wait_until("`!manager.is_connected()`", || !manager.is_connected());
    assert_eq!(
        manager.refused_change(),
        None,
        "an unknown outcome is not a refusal"
    );
    drop(manager);
    thread.join().unwrap();
}

/// An invocation update the broker refuses is about that invocation: the
/// bridge stays open and the app keeps its place. An update whose
/// delivery is unknown still stops outbound work.
#[test]
fn a_refused_invocation_update_keeps_the_bridge() {
    let broker = FakeBroker::default();
    let (mut manager, captured, thread) = committed_route_over_fake_broker(broker);
    {
        let mut captured = captured.lock().unwrap();
        captured.refuse_terminals = 1;
        captured.invocation_fixture_enabled = true;
    }
    let refused = tilo_actions::InvocationId::new();
    manager.send(CollabRequest::UpdateInvocation {
        id: 41,
        update: tilo_actions::InvocationUpdate::Succeeded {
            invocation: refused,
            output: tilo_actions::ActionOutput::default(),
        },
    });
    // The refused answer is followed by the invocation's failure, so its
    // caller is not left waiting for an answer that will not come.
    tilo_test_env::wait_until("the refused answer's failure to reach the broker", || {
        captured.lock().unwrap().terminal_updates == 2
    });
    assert!(matches!(
        &captured.lock().unwrap().invocation_updates[1],
        (41, tilo_actions::InvocationUpdate::Failed { invocation, error })
            if *invocation == refused && error.code == "answer_refused"
    ));
    // Give the shovel a turn to act on the answer the broker sent.
    thread::sleep(Duration::from_millis(50));
    assert!(
        manager.is_connected(),
        "a refused invocation update is an answer, not a lost connection"
    );
    // Later work still reaches the broker.
    manager.send(CollabRequest::UpdateInvocation {
        id: 42,
        update: tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput::default(),
        },
    });
    tilo_test_env::wait_until("the later update to reach the broker", || {
        captured.lock().unwrap().terminal_updates == 3
    });
    assert!(manager.is_connected());
    drop(manager);
    thread.join().unwrap();
}

/// A broker-owned acceptance rejection is about that update; a later
/// terminal whose delivery is unknown (the receipt barrier failed) still
/// stops subsequent outbound work before the next request goes out.
#[test]
fn session_owner_commit_sdk_terminal_failure_disconnects_before_next_request() {
    let broker = FakeBroker::default();
    let (mut manager, captures, thread) = committed_route_over_fake_broker(broker);
    captures.lock().unwrap().fail_terminal = true;
    let invocation = tilo_actions::InvocationId::new();
    manager.send(CollabRequest::UpdateInvocation {
        id: 42,
        update: tilo_actions::InvocationUpdate::Accepted { invocation },
    });
    for id in [42, 43] {
        manager.send(CollabRequest::UpdateInvocation {
            id,
            update: tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        });
    }
    tilo_test_env::wait_until("`!manager.is_connected()`", || !manager.is_connected());
    assert_eq!(
        captures.lock().unwrap().terminal_updates,
        2,
        "a broker-owned acceptance rejection must not kill a background worker; the subsequent failed terminal must"
    );
    drop(manager);
    thread.join().unwrap();
}

#[test]
#[cfg(unix)]
fn document_publication_receipt_waits_for_primary_and_routed_rpc_completion() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let mut holds = Vec::new();
    for _ in 0..2 {
        let (observed, release) = captures.lock().unwrap().hold_document();
        // Dropping the sender on unwind releases the fake RPC with Err.
        holds.push((observed, release));
    }
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let cluster_id = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", cluster_id);
    let secondary = RouteSpec {
        app_id: "other".into(),
        cluster_id,
        document: Some("second".into()),
    };
    manager.join_route(
        secondary.clone(),
        collab_types::document::ResourceType::new("tilo.rich-text"),
    );
    tilo_test_env::wait_until(
        "`broker_state(&manager).document_reads.publishable(&secondary.channel_id())`",
        || {
            broker_state(&manager)
                .document_reads
                .publishable(&secondary.channel_id())
        },
    );
    assert!(manager.send(CollabRequest::document_incremental(change("first"))));
    let first = manager.begin_document_publication_barrier().unwrap();
    assert!(
        manager
            .try_send_to_route(
                secondary,
                CollabRequest::document_incremental(change("second"))
            )
            .is_ok()
    );
    let second = manager.begin_document_publication_barrier().unwrap();
    let mut holds = holds.into_iter();
    let (entered, release) = holds.next().unwrap();
    tilo_test_env::recv("the held document RPC", &entered);
    assert_eq!(first.try_complete(), Ok(None));
    assert_eq!(second.try_complete(), Ok(None));
    release.send(Ok(())).unwrap();
    let (entered, release) = holds.next().unwrap();
    tilo_test_env::recv("the held document RPC", &entered);
    // The second RPC can only begin after the first marker has resolved.
    assert_eq!(first.wait(Some(Instant::now())), Ok(()));
    assert_eq!(second.try_complete(), Ok(None));
    release.send(Ok(())).unwrap();
    assert_eq!(second.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    assert_eq!(captures.lock().unwrap().record_writes.len(), 2);
    assert!(manager.is_connected());
    drop(manager);
    thread.join().unwrap();
}

/// A burst of edits many times the outbound queue must be absorbed by
/// backpressure: the app is told `Busy` and retries, the channel stays
/// up, and every edit reaches the broker exactly once, in order.
#[test]
fn a_flood_of_edits_is_backpressured_not_fatal() {
    let (fd, captures, thread) = stand_up_fake_broker(FakeBroker::default());
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    let total = SDK_COMMAND_QUEUE * 3;
    let mut busy = 0usize;
    for n in 0..total {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match manager.try_send(CollabRequest::document_incremental(change(&format!(
                "edit-{n}"
            )))) {
                Ok(()) => break,
                Err(crate::CollabSendError::Busy) => {
                    busy += 1;
                    assert!(Instant::now() < deadline, "backpressure never cleared");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(other) => panic!("edit {n} was refused for good: {other}"),
            }
        }
    }
    let barrier = manager.begin_document_publication_barrier().unwrap();
    // The marker may spill behind the final edits. Drive the owner as
    // the live app does, so its queued durable commands reach the worker.
    tilo_test_env::wait_until("the burst is published", || {
        manager.poll();
        barrier.try_complete().unwrap().is_some()
    });
    assert!(
        manager.is_connected(),
        "a burst must not disable collaboration (busy {busy}x)"
    );
    assert_eq!(captures.lock().unwrap().record_writes.len(), total);
    assert!(manager.send(CollabRequest::document_incremental(change("after"))));
    drop(manager);
    thread.join().unwrap();
}

fn parking_transport(capacity: usize) -> (BrokerTransport, tokio_mpsc::Receiver<QueuedSdkCommand>) {
    // Plus the slot held for the next barrier, as `start_sdk` sizes it.
    let (outbound_tx, outbound_rx) = tokio_mpsc::channel(capacity + 1);
    let (_, inbound_rx) = tokio_mpsc::channel(1);
    let (action_directory_tx, action_directory_rx) = mpsc::sync_channel(1);
    let transport = BrokerTransport {
        outbound_tx,
        inbound_rx,
        document_turns: Vec::new(),
        invocation_owners: std::collections::BTreeMap::new(),
        state: BridgeState::new(),
        action_directory_rx,
        action_directory_tx,
        pending_primary: None,
        worker_stopped: false,
        invocations_stopped: false,
        spill: std::collections::VecDeque::new(),
        barrier_slot: None,
        queued_barrier: std::sync::Weak::new(),
        _thread: None,
    };
    (transport, outbound_rx)
}

tilo_test_env::scenario! {
    /// The transport's outbound queue when it is full, each case on a transport with no worker behind it.
    fn a_full_outbound_queue_parks_coalesces_and_refuses_without_closing_the_bridge;

    phase a_barrier_behind_a_full_queue_is_queued_not_parked;
    // A barrier submitted while the queue is full goes into the slot held
    // for it, behind every earlier edit. Parked in the spill, it would move
    // only when the owner next called the transport, and an owner blocked
    // on the receipt never does. A second barrier with no edit between
    // shares the first one's answer.
    {
        let (mut transport, mut shovel) = parking_transport(4);
        let mut edits = 0;
        let refused = loop {
            match transport.submit(TransportCommand::Primary(
                CollabRequest::document_incremental(vec![edits]),
            )) {
                Ok(()) => edits += 1,
                Err(error) => break error,
            }
        };
        assert_eq!(refused, TransportError::Busy);
        assert!(edits > 0);

        let first = transport.publication_barrier().unwrap();
        let second = transport.publication_barrier().unwrap();
        assert!(transport.spill.is_empty(), "a barrier is never parked");
        for n in 0..edits {
            assert!(
                matches!(
                    shovel.try_recv().map(|queued| queued.command),
                    Ok(SdkCommand::Primary(CollabRequest::Document { .. }))
                ),
                "edit {n} precedes the barrier"
            );
        }
        let Ok(QueuedSdkCommand {
            command: SdkCommand::PublicationBarrier(replies),
            ..
        }) = shovel.try_recv()
        else {
            panic!("the barrier was queued behind the edits");
        };
        assert!(
            shovel.try_recv().is_err(),
            "both barriers share one command"
        );
        replies.answer(&Ok(()));
        assert_eq!(first.wait(None), Ok(()));
        assert_eq!(second.wait(None), Ok(()));
    }

    phase a_durable_burst_waits_for_the_shovel_instead_of_disabling_collaboration;
    // A burst of durable commands, more than the queue holds by count and
    // by bytes, waits for the shovel instead of disabling collaboration,
    // and reaches it in submission order.
    {
        let (mut transport, mut shovel) = parking_transport(4);
        let total = 100;
        for n in 0..total {
            // One megabyte each: the burst is far past the byte budget too.
            let command = TransportCommand::Primary(CollabRequest::ReportHandoff {
                message: vec![u8::try_from(n).unwrap(); 1024 * 1024],
            });
            assert_eq!(transport.submit(command), Ok(()), "command {n} was refused");
            assert!(
                transport.state.is_healthy(),
                "command {n} disabled the bridge"
            );
        }
        assert!(!transport.spill.is_empty(), "the queue really was full");
        assert!(matches!(transport.status(), TransportStatus::Connecting));

        let mut seen = Vec::new();
        let mut events = Vec::new();
        while seen.len() < total {
            let mut moved = false;
            while let Ok(queued) = shovel.try_recv() {
                let SdkCommand::Primary(CollabRequest::ReportHandoff { message }) = queued.command
                else {
                    panic!("unexpected command");
                };
                seen.push(message[0]);
                moved = true;
            }
            // The owner's drain is what refills the queue after a wake.
            transport.drain(&mut events);
            assert!(moved || !transport.spill.is_empty() || seen.len() == total);
        }
        let expected = (0..total)
            .map(|n| u8::try_from(n).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(seen, expected);
        assert!(transport.state.is_healthy());
    }

    phase a_command_larger_than_the_whole_budget_is_refused_without_closing_the_bridge;
    // A single command larger than the whole outbound byte budget can never
    // fit, so it is refused to its sender, and the bridge stays up for
    // everything else.
    {
        let (mut transport, mut shovel) = parking_transport(8);
        let huge = TransportCommand::Primary(CollabRequest::ReportHandoff {
            message: vec![0; SDK_COMMAND_QUEUE_BYTES + 1024 * 1024],
        });
        assert!(matches!(
            transport.submit(huge),
            Err(TransportError::TooLarge { .. })
        ));
        assert!(transport.state.is_healthy());
        assert_eq!(
            transport.submit(TransportCommand::Primary(CollabRequest::ClearContinuation)),
            Ok(())
        );
        assert!(shovel.try_recv().is_ok());
    }

    phase parked_surfaces_coalesce_and_document_changes_wait_behind_parked_commands;
    // Commands behind a parked one stay behind it, and a newer action surface
    // replaces an older one still waiting: it is level-triggered, so a burst
    // of republications is one parked surface, not a queue of large ones.
    {
        let (mut transport, mut shovel) = parking_transport(1);
        let surface = |n: usize| {
            TransportCommand::Primary(CollabRequest::PublishActionSurface {
                delta: SurfaceDelta {
                    removed: vec![format!("page-{n}")],
                    ..SurfaceDelta::default()
                },
            })
        };
        assert_eq!(transport.submit(surface(0)), Ok(()));
        for n in 1..=500 {
            assert_eq!(transport.submit(surface(n)), Ok(()));
        }
        assert_eq!(transport.spill.len(), 1, "surfaces coalesce while parked");
        let Some(SdkCommand::Primary(CollabRequest::PublishActionSurface { delta, .. })) =
            transport.spill.front()
        else {
            panic!("the parked command is the surface");
        };
        assert_eq!(delta.removed.len(), 500, "no change is lost in the merge");
        assert_eq!(
            transport.submit(TransportCommand::Primary(
                CollabRequest::document_incremental(b"edit".to_vec())
            )),
            Err(TransportError::Busy),
            "a document change never overtakes a parked command"
        );
        assert!(transport.state.is_healthy());
        assert!(shovel.try_recv().is_ok());
        let mut events = Vec::new();
        transport.drain(&mut events);
        assert!(transport.spill.is_empty());
        assert!(shovel.try_recv().is_ok());
    }

    phase the_last_caret_of_a_burst_waits_for_room_and_is_the_one_said;
    // A caret that moves while typing fills the queue waits for room rather than
    // being dropped, and only the newest word under its key is said: the last
    // caret of a burst is the one everyone sees.
    {
        let (mut transport, mut shovel) = parking_transport(1);
        let caret = |pos: u64| {
            TransportCommand::Primary(CollabRequest::Say {
                key: PresenceKey::Tab {
                    id: "note".to_owned(),
                },
                payload: collab_types::presence::Payload::Caret { pos },
            })
        };
        assert_eq!(
            transport.submit(TransportCommand::Primary(
                CollabRequest::document_incremental(b"typed".to_vec())
            )),
            Ok(())
        );
        for pos in 1..=50 {
            assert_eq!(
                transport.submit(caret(pos)),
                Ok(()),
                "a caret is never shed"
            );
        }
        assert_eq!(transport.spill.len(), 1, "one caret waits, the newest");
        let mut said = Vec::new();
        while let Ok(queued) = shovel.try_recv() {
            said.push(queued.command);
            let mut events = Vec::new();
            transport.drain(&mut events);
        }
        let Some(SdkCommand::Primary(CollabRequest::Say {
            payload: collab_types::presence::Payload::Caret { pos },
            ..
        })) = said.last()
        else {
            panic!("the caret is said once there is room: {}", said.len());
        };
        assert_eq!(*pos, 50, "the caret said is where the burst ended");
        assert!(transport.spill.is_empty());
    }
}

/// Keys for a fresh channel group, as a ready identity service answers.
#[cfg(unix)]
fn ready_channel_keys() -> tilo_identity_client::wire::ChannelKeysReply {
    let group = key_group::KeyGroup::create(
        collab_types::WorkspaceId::new(),
        key_group::MemberKey::generate("device").unwrap(),
    )
    .unwrap();
    tilo_identity_client::wire::ChannelKeysReply {
        access: tilo_identity_client::wire::KeyAccess::Ready,
        generation: 1,
        keys: Some(group.content_keys().unwrap().to_bytes().unwrap().to_vec()),
    }
}

/// A certified identity whose service answers each question for keys with `keys` (the route's
/// app, and the generation it holds), and that signs every join. Counts each question and each
/// join it signs.
#[cfg(unix)]
struct FixtureIdentity {
    keys: Box<
        dyn Fn(
                &str,
                u64,
            )
                -> Result<tilo_identity_client::wire::ChannelKeysReply, runtime_portal::NotAvailable>
            + Send
            + Sync,
    >,
    asked: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(unix)]
impl FixtureIdentity {
    fn answering(
        keys: impl Fn(
            &str,
            u64,
        ) -> Result<
            tilo_identity_client::wire::ChannelKeysReply,
            runtime_portal::NotAvailable,
        > + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            keys: Box::new(keys),
            asked: Arc::default(),
        }
    }

    /// One whose service holds keys for every route.
    fn keyed() -> Self {
        Self::answering(|_, _| Ok(ready_channel_keys()))
    }
}

/// A manager on the fake broker at `fd`, its content sealed through `identity`, its own route
/// for `app_id` in `workspace` open.
#[cfg(unix)]
fn certified_manager(
    fd: RawFd,
    identity: impl SdkCertifiedIdentity + 'static,
    (app_id, workspace): (&str, Uuid),
) -> CollaborationManager {
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            member_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(identity)),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: app_id.into(),
        cluster_id: workspace,
        record: None,
        resource_type: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
    manager
}

#[cfg(unix)]
impl SdkCertifiedIdentity for FixtureIdentity {
    fn group_keys(
        &self,
        _: &str,
        app_id: &str,
        _: &str,
        generation: u64,
    ) -> Result<tilo_identity_client::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        (self.keys)(app_id, generation)
    }
}

/// An endpoint on a pushing broker whose identity service answers its
/// one route with `access` (or refuses, `None`): asked again on the
/// timer (`on_timer`), or left with its timer disarmed and unasked. A
/// pushed change must still make it ask afterwards.
#[cfg(unix)]
#[cfg(test)]
fn asks_while_waiting(access: Option<tilo_identity_client::wire::KeyAccess>, on_timer: bool) {
    let changes = Arc::new(tokio::sync::watch::Sender::new(0_u64));
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            key_changes: Some(Arc::clone(&changes)),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let manager = certified_manager(
        fd,
        FixtureIdentity {
            asked: Arc::clone(&asked),
            ..FixtureIdentity::answering(move |_, generation| {
                access
                    .map(|access| tilo_identity_client::wire::ChannelKeysReply {
                        access,
                        generation,
                        keys: None,
                    })
                    .ok_or(runtime_portal::NotAvailable::ServiceFdMissing)
            })
        },
        ("so.tilo.test", Uuid::new_v4()),
    );
    tilo_test_env::wait_until("the route's endpoint to ask for its channel keys", || {
        asked.load(std::sync::atomic::Ordering::Acquire) > 0
    });
    let joined = asked.load(std::sync::atomic::Ordering::Acquire);
    if on_timer {
        tilo_test_env::wait_until(
            format_args!("{access:?}: the route to be asked twice on its timer"),
            || asked.load(std::sync::atomic::Ordering::Acquire) >= joined + 2,
        );
        assert!(key_schedule(&manager).timer_rounds >= 2);
    } else {
        let seen = key_schedule(&manager);
        assert!(
            !seen.armed && !seen.marked && seen.timer_rounds == 0,
            "{access:?}: a pushed route is asked on a timer: {seen:?}"
        );
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Acquire),
            joined,
            "{access:?}: a pushed route was asked again"
        );
    }
    let before_change = asked.load(std::sync::atomic::Ordering::Acquire);
    changes.send_replace(9);
    tilo_test_env::wait_until("the endpoint to ask after its channel changed", || {
        asked.load(std::sync::atomic::Ordering::Acquire) > before_change
    });
    drop(manager);
    thread.join().unwrap();
}

/// A route the channel service answered as waiting, or as unavailable
/// (its workspace's governance unknown yet), is told when that changes: it is not asked
/// about on a timer while the broker pushes, and a pushed change asks.
#[test]
#[cfg(unix)]
fn a_route_the_channel_service_answered_waits_for_its_push() {
    use tilo_identity_client::wire::KeyAccess;
    for access in [KeyAccess::Waiting, KeyAccess::Unavailable] {
        asks_while_waiting(Some(access), false);
    }
}

/// What nothing pushes is still asked on the timer: a question the
/// identity service refused, and a removal, whose rejoin each question
/// asks for again.
#[test]
#[cfg(unix)]
fn a_refused_question_or_a_removal_is_still_asked_on_the_timer() {
    for access in [None, Some(tilo_identity_client::wire::KeyAccess::Removed)] {
        asks_while_waiting(access, true);
    }
}

/// How often a certified endpoint asked for channel keys after joining a
/// route the broker serves as committed (`committed`: within a few refresh
/// intervals) or as an ordinary op stream (once it has asked at all).
#[cfg(unix)]
#[cfg(test)]
fn channel_key_requests_on_route(committed: bool) -> usize {
    let captures = Captures {
        committed_history_fixture: committed.then(|| automerge::AutoCommit::new().save()),
        ..Captures::default()
    };
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(captures)),
        ..FakeBroker::default()
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let manager = certified_manager(
        fd,
        FixtureIdentity {
            asked: Arc::clone(&asked),
            ..FixtureIdentity::keyed()
        },
        ("so.tilo.test", Uuid::new_v4()),
    );
    if committed {
        // Asking would be the bug, so there is no event to wait for: the
        // timer is either disarmed or has started a few rounds that
        // stayed quiet.
        if key_schedule(&manager).armed {
            tilo_test_env::wait_until("three key timer rounds", || {
                key_schedule(&manager).timer_rounds >= 3
            });
        }
    } else {
        tilo_test_env::wait_until("the op route's endpoint to ask for channel keys", || {
            asked.load(std::sync::atomic::Ordering::Acquire) > 0
        });
    }
    let asked = asked.load(std::sync::atomic::Ordering::Acquire);
    drop(manager);
    thread.join().unwrap();
    asked
}

/// An endpoint keeps receiving while its identity service is slow to
/// answer a key refresh: a change made elsewhere during the refresh is
/// delivered while the service still has not answered.
#[test]
#[cfg(unix)]
fn a_slow_identity_service_never_holds_up_incoming_changes() {
    // The change is made after the first refresh tick, which the identity
    // service holds until the change has been delivered.
    const ARRIVES: Duration = Duration::from_millis(1500);
    let app_id = "so.tilo.test";
    let cluster = Uuid::new_v4();
    let group = key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(&cluster.to_string()).unwrap(),
        key_group::MemberKey::generate("device").unwrap(),
    )
    .unwrap();
    let remote = change("remote change");
    let (peer, peer_changes) = tokio_mpsc::unbounded_channel();
    let broker = FakeBroker {
        peer_changes: Some(peer_changes),
        ..FakeBroker::default()
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let released = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    // The identity service answers its first question at once, with the keys, and
    // holds every later one until released.
    let mut manager = certified_manager(
        fd,
        FixtureIdentity::answering({
            let keys = group.content_keys().unwrap().to_bytes().unwrap().to_vec();
            let (released, first) = (
                Arc::clone(&released),
                std::sync::atomic::AtomicBool::new(true),
            );
            move |_, _| {
                let first = first.swap(false, std::sync::atomic::Ordering::AcqRel);
                if !first {
                    let (released, changed) = &*released;
                    let _released = changed
                        .wait_timeout_while(
                            released.lock().unwrap(),
                            tilo_test_env::hang_bound(),
                            |released| !*released,
                        )
                        .unwrap();
                }
                Ok(tilo_identity_client::wire::ChannelKeysReply {
                    access: tilo_identity_client::wire::KeyAccess::Ready,
                    generation: 1,
                    keys: first.then(|| keys.clone()),
                })
            }
        }),
        (app_id, cluster),
    );
    thread::sleep(ARRIVES);
    peer.send(remote.clone()).unwrap();
    let events = poll_until(&mut manager, |events| {
        events
            .iter()
            .any(|event| matches!(event, CollabEvent::Document { .. }))
    });
    *released.0.lock().unwrap() = true;
    released.1.notify_all();
    assert!(
        events.iter().any(|event| event
            .document_mutation()
            .is_some_and(|mutation| changes_of(mutation.data()) == changes_of(&remote))),
        "the change must arrive while the key refresh waits: {events:?}"
    );
    drop(manager);
    thread.join().unwrap();
}

/// A committed route's owner applies each edit itself and holds no
/// channel keys, so its edits stay cleartext: the endpoint never asks
/// for keys there, while an ordinary route of the same endpoint does.
#[test]
#[cfg(unix)]
fn committed_routes_stay_out_of_channel_keys_while_op_routes_ask() {
    assert_eq!(channel_key_requests_on_route(true), 0);
    assert!(channel_key_requests_on_route(false) > 0);
}

/// A certified endpoint on one ordinary route of a broker that says when
/// channel keys change (`changes`) or does not (`None`), with the count
/// of its key requests.
#[cfg(unix)]
fn keyed_endpoint(
    changes: Option<Arc<tokio::sync::watch::Sender<u64>>>,
) -> (
    CollaborationManager,
    Arc<std::sync::atomic::AtomicUsize>,
    thread::JoinHandle<()>,
) {
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            key_changes: changes,
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let manager = certified_manager(
        fd,
        FixtureIdentity {
            asked: Arc::clone(&asked),
            ..FixtureIdentity::keyed()
        },
        ("so.tilo.test", Uuid::new_v4()),
    );
    tilo_test_env::wait_until("the route's endpoint to ask for its channel keys", || {
        asked.load(std::sync::atomic::Ordering::Acquire) > 0
    });
    (manager, asked, thread)
}

/// #3964: an endpoint holding its route's keys asks for them again only
/// when the broker says the channel changed, once per change, and never
/// on a timer: an idle app costs the identity service nothing.
#[test]
#[cfg(unix)]
fn routes_holding_keys_ask_only_when_their_channel_changes() {
    let changes = Arc::new(tokio::sync::watch::Sender::new(0_u64));
    let (manager, asked, thread) = keyed_endpoint(Some(Arc::clone(&changes)));
    let joined = asked.load(std::sync::atomic::Ordering::Acquire);
    let seen = key_schedule(&manager);
    assert!(
        !seen.armed && !seen.marked && seen.timer_rounds == 0,
        "a route holding keys is asked on a timer: {seen:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Acquire),
        joined,
        "a route holding keys asked again with nothing changed"
    );

    changes.send_replace(7);
    tilo_test_env::wait_until("the endpoint to ask after its channel changed", || {
        asked.load(std::sync::atomic::Ordering::Acquire) > joined
    });
    let seen = key_schedule(&manager);
    assert!(
        !seen.armed && !seen.marked && seen.timer_rounds == 0,
        "a change left the route asking: {seen:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Acquire),
        joined + 1,
        "one change asks once"
    );
    drop(manager);
    thread.join().unwrap();
}

/// A broker that does not say when keys change (an older one) leaves the
/// endpoint asking for every route every second, as before.
#[test]
#[cfg(unix)]
fn without_pushed_changes_routes_are_asked_every_second() {
    let (manager, asked, thread) = keyed_endpoint(None);
    let joined = asked.load(std::sync::atomic::Ordering::Acquire);
    tilo_test_env::wait_until("the endpoint to keep asking on its timer", || {
        asked.load(std::sync::atomic::Ordering::Acquire) >= joined + 2
    });
    drop(manager);
    thread.join().unwrap();
}

#[test]
#[cfg(unix)]
fn committed_queued_publication_retains_payload_and_credits_until_released() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (observed, release) = captures.lock().unwrap().hold_document();
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    assert!(manager.send(CollabRequest::document_incremental(change("first"))));
    tilo_test_env::recv("the held document RPC", &observed);
    let retained = change("retained original");
    assert!(manager.send(CollabRequest::document_incremental(retained.clone())));
    let state = broker_state(&manager);
    let key = manager.primary_route().unwrap().route.channel_id();
    let ready = state.document_reads.close_for_test(&key);
    let receipt = manager.begin_document_publication_barrier().unwrap();
    let before = publication_hold_waits(&state);
    release.send(Ok(())).unwrap();
    tilo_test_env::wait_until("the first write to be taken", || {
        captures.lock().unwrap().record_writes.len() == 1
    });
    wait_held_at_publication(&state, before);
    assert_eq!(
        receipt.try_complete(),
        Ok(None),
        "accepted publication must hold the barrier while hydration is pending"
    );
    assert!(state.document_command_bytes.0.load(Ordering::Acquire) >= retained.len());
    state.document_reads.open_for_test(&ready);
    assert_eq!(receipt.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    let captured = captures.lock().unwrap().record_writes.clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[1], changes_of(&retained));
    drop(manager);
    thread.join().unwrap();
}

#[test]
#[cfg(unix)]
fn committed_queued_publication_on_a_failed_route_is_terminal() {
    queued_publication_terminal_case(false);
}

#[test]
#[cfg(unix)]
fn committed_queued_publication_invalidation_is_terminal() {
    queued_publication_terminal_case(true);
}

#[cfg(all(unix, test))]
fn queued_publication_terminal_case(invalidate: bool) {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (observed, release) = captures.lock().unwrap().hold_document();
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    assert!(manager.send(CollabRequest::document_incremental(change("first"))));
    tilo_test_env::recv("the held document RPC", &observed);
    assert!(manager.send(CollabRequest::document_incremental(change("must replay"))));
    let state = broker_state(&manager);
    let ready = state
        .document_reads
        .close_for_test(&manager.primary_route().unwrap().route.channel_id());
    let receipt = manager.begin_document_publication_barrier().unwrap();
    let before = publication_hold_waits(&state);
    release.send(Ok(())).unwrap();
    tilo_test_env::wait_until("the first write to be taken", || {
        captures.lock().unwrap().record_writes.len() == 1
    });
    // No clock ends the wait: the publication holds until the route changes.
    wait_held_at_publication(&state, before);
    assert_eq!(receipt.try_complete(), Ok(None));
    if invalidate {
        state.fail_closed("actual test worker invalidation");
    } else {
        state
            .document_reads
            .fail_for_test(&ready, "the route's first read was refused");
    }
    let error = receipt
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap_err();
    assert!(
        matches!(error, DocumentPublicationError::Failed(_)),
        "{error}"
    );
    assert!(!manager.is_connected());
    assert!(manager.document_publication_failed());
    ready.store(true, Ordering::Release);
    assert!(!manager.send(CollabRequest::document_incremental(b"late reopen".to_vec())));
    assert_eq!(captures.lock().unwrap().record_writes.len(), 1);
    tilo_test_env::wait_until(
        "`state.command_bytes.0.load(Ordering::Acquire) == 0`",
        || state.command_bytes.0.load(Ordering::Acquire) == 0,
    );
    drop(manager);
    thread.join().unwrap();
}

/// A document beside the app's own that has not reached this device (a
/// person's second device waits for another of theirs to hand it their own
/// drafts) holds back that document alone: the barrier
/// says its change did not go, the app and its other routes stay
/// published, and the owner hears it should send the record whole once
/// the route opens. It used to stop the app's whole collaboration.
#[test]
#[cfg(unix)]
fn a_document_not_here_holds_back_only_itself() {
    let (fd, captures, thread) = stand_up_fake_broker(FakeBroker::default());
    let workspace = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", workspace);
    let drafts = RouteSpec {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace,
        document: Some(
            collab_types::document::DocumentId::of_member(&"ad".repeat(32), "chat.drafts")
                .unwrap()
                .as_str()
                .to_owned(),
        ),
    };
    manager.join_route(
        drafts.clone(),
        collab_types::document::ResourceType::new("chat.drafts"),
    );
    eventually("the drafts route to join", || {
        manager.document_route_publishable(&drafts)
    });
    // It has still to reach this device: the route is closed.
    let closed = broker_state(&manager)
        .document_reads
        .close_for_test(&drafts.channel_id());
    broker_state(&manager)
        .document_reads
        .not_here_for_test(&closed);
    assert!(
        manager
            .try_send_to_route(
                drafts.clone(),
                CollabRequest::document_incremental(change("draft"))
            )
            .is_ok()
    );
    let receipt = manager.begin_document_publication_barrier().unwrap();
    assert!(
        matches!(
            receipt.wait(Some(tilo_test_env::hang_deadline())),
            Err(DocumentPublicationError::Failed(_))
        ),
        "the barrier says the draft did not go"
    );
    assert!(
        manager.is_connected(),
        "one record's route stops nothing else"
    );
    assert!(manager.take_unpublished_route(&drafts));
    assert!(!manager.take_unpublished_route(&drafts), "said once");
    let before = captures.lock().unwrap().record_writes.len();
    assert!(manager.send(CollabRequest::document_incremental(change("own"))));
    manager
        .begin_document_publication_barrier()
        .unwrap()
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap();
    assert_eq!(
        captures.lock().unwrap().record_writes.len(),
        before + 1,
        "the app's own route still publishes"
    );
    drop(manager);
    thread.join().unwrap();
}

/// A write the store answers with a refusal is that change's own
/// failure: the barrier reports it and the bridge, which carried the
/// answer, keeps the app published. (An RPC whose outcome is unknown is
/// the closing case; see
/// `a_document_commit_whose_outcome_is_unknown_still_closes_the_bridge`.)
#[test]
#[cfg(unix)]
fn a_refused_document_rpc_fails_the_barrier_without_closing() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (observed, release) = captures.lock().unwrap().hold_document();
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, _, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    assert!(manager.send(CollabRequest::document_incremental(change("failed"))));
    tilo_test_env::recv("the held document RPC", &observed);
    let receipt = manager.begin_document_publication_barrier().unwrap();
    release.send(Err("document RPC refused".into())).unwrap();
    let error = receipt
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap_err();
    assert!(
        error.to_string().contains("document RPC refused"),
        "{error}"
    );
    assert!(
        manager.is_connected(),
        "the peer answered; the app stays published and reachable"
    );
    assert!(
        !manager.sync_lost(),
        "the app is still syncing; one refused change never reads as a lost connection"
    );
    drop(manager);
    thread.join().unwrap();
}

tilo_test_env::scenario! {
    /// What a publication barrier's receipt answers, each case on a bridge state of its own.
    fn a_document_publication_receipt_reports_what_became_of_its_changes;

    phase a_refused_change_stands_until_its_own_record_takes_a_change;
    // A refusal belongs to the record it was about: another record's change
    // going through leaves it in place, its own record's next change ends it.
    {
        let route = |record: &str| RouteSpec {
            app_id: "so.tilo.notes".into(),
            cluster_id: uuid::Uuid::from_bytes([7; 16]),
            document: Some(record.into()),
        };
        let state = BridgeState::new();
        state.note_refused_change(&route("full"), "document commit RPC", "history is full");
        state.change_sent(&route("other"));
        assert!(state.refused_change().is_some());
        assert!(state.publication_result().is_err());
        state.change_sent(&route("full"));
        assert_eq!(state.refused_change(), None);
        assert!(state.publication_result().is_ok());
    }

    phase document_publication_unavailable_and_rejected_markers_are_errors;
    {
        let mut manager = CollaborationManager::disabled();
        assert!(matches!(
            manager.begin_document_publication_barrier(),
            Err(DocumentPublicationError::Unavailable)
        ));
        let (outbound_tx, mut outbound_rx) = tokio_mpsc::channel(1);
        let (_, inbound_rx) = tokio_mpsc::channel(1);
        let (action_directory_tx, action_directory_rx) = mpsc::sync_channel(1);
        let state = BridgeState::new();
        manager.backend = Backend::Transport(Box::new(BrokerTransport {
            outbound_tx,
            inbound_rx,
            document_turns: Vec::new(),
            invocation_owners: std::collections::BTreeMap::new(),
            state,
            action_directory_rx,
            action_directory_tx,
            pending_primary: None,
            worker_stopped: false,
            invocations_stopped: false,
                spill: std::collections::VecDeque::new(),
            barrier_slot: None,
            queued_barrier: std::sync::Weak::new(),
            _thread: None,
        }));
        let earlier = manager.begin_document_publication_barrier().unwrap();
        // A full queue is backpressure: the second barrier waits behind the
        // first rather than failing, and the bridge stays up.
        let _later = manager
            .begin_document_publication_barrier()
            .expect("a barrier waits behind a full queue");
        drop(outbound_rx.try_recv().unwrap());
        assert!(
            earlier.wait(None).is_err(),
            "a barrier whose command was dropped is never reported as sent"
        );
    }

    phase document_publication_completed_prefix_is_not_poisoned_by_later_failure;
    {
        let state = BridgeState::new();
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = bridge_receipt(receiver, &state);
        reply.send(state.publication_result()).unwrap();
        state.record_publication_failure(DocumentPublicationError::Failed("later".into()));
        assert_eq!(receipt.wait(Some(Instant::now())), Ok(()));
    }

    phase document_publication_disconnection_and_pending_deadline_are_errors;
    {
        let state = BridgeState::new();
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = bridge_receipt(receiver, &state);
        assert_eq!(
            receipt.wait(Some(Instant::now())),
            Err(DocumentPublicationError::DeadlineElapsed)
        );
        drop(reply);
        let (reply, receiver) = mpsc::sync_channel(1);
        let receipt = bridge_receipt(receiver, &state);
        drop(reply);
        assert_eq!(
            receipt.wait(None),
            Err(DocumentPublicationError::WorkerStopped)
        );
    }
}

/// A receipt wired to the bridge's latched failure, as the transport builds it.
fn bridge_receipt(
    receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>,
    state: &BridgeState,
) -> DocumentPublicationReceipt {
    let state = state.clone();
    DocumentPublicationReceipt::new(receiver)
        .with_disconnect_cause(move || state.publication_result().err())
}

/// Errors logged while `body` ran on this thread.
fn errors_logged(body: impl FnOnce()) -> usize {
    use tracing_subscriber::layer::SubscriberExt as _;
    struct Errors(Arc<std::sync::atomic::AtomicUsize>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Errors {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() == tracing::Level::ERROR {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(Errors(count.clone()));
    tracing::subscriber::with_default(subscriber, body);
    count.load(Ordering::Relaxed)
}

tilo_test_env::scenario! {
    /// The inbound bridge when its app reads late or not at all, each case on a bridge and a runtime of its own.
    fn an_app_that_is_behind_or_gone_never_overloads_the_inbound_bridge;

    phase an_app_that_stopped_reading_ends_collaboration_without_an_overload_error;
    {
        let state = BridgeState::new();
        let (tx, rx) = tokio_mpsc::channel::<QueuedCollabEvent>(1);
        let inbound = InboundBridge::new(tx, state.clone());
        // The app closes: its manager, and with it the receiver, is dropped.
        drop(rx);
        let mut delivered = true;
        let errors = errors_logged(|| {
            delivered = inbound.send_reply(CollabEvent::FollowChanged { peer_id: None });
        });
        assert!(!delivered, "nothing reads the event");
        assert!(!state.is_healthy(), "collaboration ends with its reader");
        assert_eq!(errors, 0, "a closed app is not an overloaded bridge");
    }

    phase a_burst_of_events_the_app_is_behind_on_waits_for_it_and_keeps_collaboration;
    // A thousand durable events arrive while the app reads none: the
    // stream producing them waits for room, the app then reads every one
    // in order, and collaboration stays up throughout.
    {
        const BURST: u128 = 1_000;
        let state = BridgeState::new();
        let (tx, mut rx) = tokio_mpsc::channel::<QueuedCollabEvent>(SDK_EVENT_QUEUE);
        let inbound = InboundBridge::new(tx, state.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut received = Vec::new();
        let errors = errors_logged(|| {
            tokio::task::LocalSet::new().block_on(&runtime, async {
                let stream = inbound.clone();
                let producer = tokio::task::spawn_local(async move {
                    for index in 0..BURST {
                        assert!(stream.send_durable(remote_update(index)).await);
                    }
                });
                // The app is busy until the stream has filled its queue.
                while rx.len() < SDK_EVENT_QUEUE {
                    tokio::task::yield_now().await;
                }
                assert!(!producer.is_finished(), "the stream waits for the app");
                assert!(state.is_healthy(), "a full queue is no failure");
                while received.len() < usize::try_from(BURST).unwrap() {
                    let event = rx.recv().await.expect("the stream keeps delivering");
                    received.push(remote_update_index(&event.event));
                }
                producer.await.unwrap();
            });
        });
        assert_eq!(
            received,
            (0..BURST).collect::<Vec<_>>(),
            "every event, in order"
        );
        assert!(state.is_healthy(), "collaboration stays enabled");
        assert_eq!(errors, 0, "a burst is not an overload");
    }

    phase answers_the_app_is_behind_on_wait_in_order_without_blocking_their_sender;
    // Answers to the app's own requests never wait on the app (it may be
    // waiting for the task answering), yet none is lost or reordered when
    // it is behind: they queue in the reply queue until it reads again.
    {
        let state = BridgeState::new();
        let (tx, mut rx) = tokio_mpsc::channel::<QueuedCollabEvent>(SDK_EVENT_QUEUE);
        let inbound = InboundBridge::new(tx, state.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let answers = u128::try_from(SDK_EVENT_QUEUE).unwrap() * 2;
        let received = tokio::task::LocalSet::new().block_on(&runtime, async {
            let forwarder = inbound.clone();
            tokio::task::spawn_local(async move { forwarder.forward_replies().await });
            for index in 0..answers {
                assert!(inbound.send_reply(remote_update(index)));
            }
            assert!(state.is_healthy());
            let mut received = Vec::new();
            while received.len() < usize::try_from(answers).unwrap() {
                let event = rx.recv().await.expect("every answer arrives");
                received.push(remote_update_index(&event.event));
            }
            received
        });
        assert_eq!(received, (0..answers).collect::<Vec<_>>());
        assert!(state.is_healthy());
    }
}

fn remote_update(index: u128) -> CollabEvent {
    CollabEvent::RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage::Update {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(index)),
            state: None,
            correlation: None,
        },
    }
}

fn remote_update_index(event: &CollabEvent) -> u128 {
    match event {
        CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Update { invocation, .. },
        } => invocation.0.as_u128(),
        _ => panic!("not a remote update"),
    }
}

tilo_test_env::scenario! {
    /// How an RPC failure is read and what of it is logged.
    fn an_rpc_failure_is_a_refusal_only_when_the_peer_said_so_and_is_reported_bounded;

    phase only_a_peers_answer_counts_as_a_refusal;
    // Only an answer about the request ends collaboration for one change:
    // a peer saying it will not take this change is not a lost connection,
    // while a lost connection — and a missing service — leave the outcome
    // unknown.
    {
        use runtime_portal::NotAvailable;
        let typed = NotAvailable::Refused("so.tilo.chat named no kind for a new record".into());
        assert!(refused_by_peer(&typed));
        for unknown in [
            NotAvailable::ConnectionFailed(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the collaboration host closed the connection",
            )),
            NotAvailable::ServiceFdMissing,
            NotAvailable::ConnectionFailed(std::io::Error::other("no service")),
        ] {
            assert!(
                !refused_by_peer(&unknown),
                "{} is not an answer about the change",
                unknown
            );
        }
    }

    phase invocation_update_rpc_diagnostic_keeps_cause_and_existing_fail_closed_latch;
    {
        let state = BridgeState::new();
        let error = runtime_portal::NotAvailable::Refused("unknown invocation id".into());
        assert_eq!(
            bounded_rpc_diagnostic(&error),
            ("refused", "unknown invocation id".into(), false)
        );
        state.fail_closed_with_rpc_error("invocation update outcome unknown", Some((389, &error)));
        assert!(!state.is_healthy());
        assert_eq!(
            state.publication_result(),
            Err(DocumentPublicationError::Failed(
                "invocation update outcome unknown".into()
            ))
        );
        state.fail_closed("later failure");
        assert_eq!(
            state.publication_result(),
            Err(DocumentPublicationError::Failed(
                "invocation update outcome unknown".into()
            ))
        );
    }

    phase invocation_update_rpc_diagnostic_bounds_untrusted_text_without_logging_io_payload;
    {
        let error = runtime_portal::NotAvailable::Refused(format!("refused\n\0{}", "🦀".repeat(1024)));
        let (kind, detail, truncated) = bounded_rpc_diagnostic(&error);
        assert_eq!(kind, "refused");
        assert!(detail.starts_with("refused  "));
        assert_eq!(detail.chars().count(), 512);
        assert!(detail.len() <= 2048);
        assert!(!detail.chars().any(char::is_control));
        assert!(truncated);
        let io = runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private transport payload",
        ));
        assert_eq!(
            bounded_rpc_diagnostic(&io),
            ("connection_failed", "PermissionDenied".into(), false)
        );
    }
}

#[test]
#[cfg(unix)]
fn document_publication_superseded_receipt_does_not_stop_worker() {
    let mut manager = connected_manager();
    assert!(manager.send(CollabRequest::document_incremental(change("first"))));
    drop(manager.begin_document_publication_barrier().unwrap());
    assert!(manager.send(CollabRequest::document_incremental(change("second"))));
    assert_eq!(
        manager
            .begin_document_publication_barrier()
            .unwrap()
            .wait(Some(tilo_test_env::hang_deadline())),
        Ok(())
    );
    assert!(manager.is_connected());
}

#[cfg(test)]
/// A captured local edit reported as `invocation`.
fn report_capture(invocation: u128) -> crate::local_document_action::CapturedAction {
    crate::local_document_action::CapturedAction {
        ownership: None,
        invocation: Uuid::from_u128(invocation),
        capture_id: Uuid::from_u128(invocation + 100),
        revision: "surface".into(),
        published: Some(crate::local_document_action::PublishedInvocation {
            resource: "notes.notebook".into(),
            action: "apply".into(),
            arguments: serde_json::Map::new(),
        }),
        encoded: tilo_actions::EncodedAction {
            type_id: "notes.edit".into(),
            payload: b"{}".to_vec(),
        },
        context: None,
        original_heads: Vec::new(),
        mutation_hashes: Vec::new(),
    }
}

/// A document's round trip holds up the next document and an invocation's
/// outcome, never the local action reports typed meanwhile: typing at speed
/// would otherwise fill the report queue behind the changes it makes.
#[test]
fn a_waiting_document_holds_back_no_report_but_the_outcome_after_it() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (held, release) = captures.lock().unwrap().hold_document();
    let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
        captures,
        ..FakeBroker::default()
    });
    let workspace = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.notes", workspace);
    let route = RouteSpec::app("so.tilo.notes", workspace);
    tilo_test_env::wait_until("the primary route to publish", || {
        manager.poll();
        broker_state(&manager)
            .document_reads
            .publishable(&route.channel_id())
    });
    assert!(manager.send(CollabRequest::document_incremental(change("first"))));
    tilo_test_env::recv("the held document RPC", &held);
    assert!(manager.send(CollabRequest::document_incremental(change("second"))));
    for request_id in 1..=20_u64 {
        let capture = report_capture(u128::from(request_id));
        manager
            .report_local_document_action_to_route(route.clone(), request_id, capture)
            .unwrap();
    }
    manager.send(CollabRequest::UpdateInvocation {
        id: 7,
        update: tilo_actions::InvocationUpdate::Succeeded {
            invocation: tilo_actions::InvocationId::new(),
            output: tilo_actions::ActionOutput::default(),
        },
    });
    let mut answered = 0;
    tilo_test_env::wait_until("every report answered, the document still held", || {
        answered += manager
            .poll()
            .into_iter()
            .filter(|event| matches!(event, CollabEvent::LocalDocumentActionReported { .. }))
            .count();
        answered == 20
    });
    {
        let captures = captures.lock().unwrap();
        assert_eq!(captures.admits_asked, 20);
        assert_eq!(
            captures.record_writes.len(),
            1,
            "the second document waits for the first"
        );
        assert_eq!(
            captures.terminal_updates, 0,
            "the outcome waits for the document"
        );
    }
    release.send(Ok(())).unwrap();
    tilo_test_env::wait_until("the second document and the outcome", || {
        manager.poll();
        let captures = captures.lock().unwrap();
        captures.record_writes.len() == 2 && captures.terminal_updates == 1
    });
    assert!(manager.is_connected());
    drop(manager);
    worker.join().unwrap();
}

/// A report the host resolves against the app's directory waits for the
/// app's next turn. Reports and changes behind it go out and are answered
/// meanwhile, or typing would outrun them and its edits miss the history.
#[test]
fn a_waiting_local_document_report_holds_back_nothing_behind_it() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (release, held) = tokio::sync::oneshot::channel();
    captures.lock().unwrap().admit_holds.push_back(held);
    let (fd, captures, worker) = stand_up_fake_broker(FakeBroker {
        captures,
        ..FakeBroker::default()
    });
    let workspace = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.notes", workspace);
    let route = RouteSpec::app("so.tilo.notes", workspace);
    tilo_test_env::wait_until("the primary route to join", || {
        manager.poll();
        manager.primary_route().is_some()
    });
    manager
        .report_local_document_action_to_route(route.clone(), 1, report_capture(1))
        .unwrap();
    tilo_test_env::wait_until("the first report to reach the host", || {
        captures.lock().unwrap().admits_asked == 1
    });
    manager
        .report_local_document_action_to_route(route, 2, report_capture(2))
        .unwrap();
    let mut answered = Vec::new();
    tilo_test_env::wait_until("the second report's answer, the first still held", || {
        answered.extend(manager.poll().into_iter().filter_map(|event| match event {
            CollabEvent::LocalDocumentActionReported { request_id, .. } => Some(request_id),
            _ => None,
        }));
        answered.contains(&2)
    });
    assert_eq!(answered, [2], "the first report is still waiting");
    drop((release, manager));
    worker.join().unwrap();
}

#[cfg(test)]
include!("idle_wake_tests.rs");

#[tokio::test]
async fn native_invocation_delivery_turn_waits_for_every_live_owner() {
    let state = BridgeState::new();
    let primary = state.document_reads.close_for_test("original-route");
    let owner = state.document_reads.close_for_test("live-owner");
    state.document_reads.close_for_test("unowned-retained");
    state.document_reads.open_for_test(&primary);
    state
        .document_reads
        .set_document_owner_routes(vec!["original-route".into(), "live-owner".into()], 0);
    let held = state
        .document_reads
        .delivery_turn("live-owner")
        .unwrap()
        .acquire_owned()
        .await
        .unwrap();
    let wait = native_invocation_delivery_turn("original-route", &state);
    tokio::pin!(wait);
    // The live owner's turn is held, so the wait cannot complete.
    assert!(
        std::future::Future::poll(
            wait.as_mut(),
            &mut std::task::Context::from_waker(std::task::Waker::noop())
        )
        .is_pending()
    );
    state.document_reads.open_for_test(&owner);
    drop(held);
    let turn = wait.await.unwrap().unwrap();
    assert_eq!(turn.permits.len(), 2);
    assert!(!state.document_reads.publishable("unowned-retained"));
}

#[tokio::test]
async fn native_invocation_delivery_turn_rejects_original_generation_retirement() {
    let state = BridgeState::new();
    let original = state.document_reads.close_for_test("original-route");
    let held = state
        .document_reads
        .delivery_turn("original-route")
        .unwrap()
        .acquire_owned()
        .await
        .unwrap();
    let (result, ()) = tokio::join!(
        native_invocation_delivery_turn("original-route", &state),
        async {
            tokio::task::yield_now().await;
            state.document_reads.close_for_test("original-route");
            drop(held);
        }
    );
    assert_eq!(
        result.unwrap_err(),
        "original invocation document ownership was retired"
    );
    assert!(!original.load(Ordering::Acquire));
    assert!(!state.document_reads.publishable("original-route"));
}

/// A route takes invocations only once it is current: until the owner merged
/// its first committed read, the broker keeps them for an instance that can
/// answer, and none reaches this one.
#[test]
#[cfg(unix)]
fn a_route_takes_invocations_only_once_current() {
    let fixture = native_history_invocation_fixture(automerge::AutoCommit::new().save());
    let mut manager = fixture.manager;
    (fixture.invoke)(5, Uuid::from_u128(5));
    let mut read = None;
    tilo_test_env::wait_until("the route's first read", || {
        read = read.take().or_else(|| {
            manager.poll().into_iter().find_map(|event| match event {
                CollabEvent::CommittedDocument { read, .. } => Some(read),
                _ => None,
            })
        });
        read.is_some()
    });
    // The join asked for every other stream already; give the shovel turns.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        (fixture.received)(),
        0,
        "no invocation before the route is current"
    );
    read.unwrap().applied();
    tilo_test_env::wait_until("the held invocation to arrive", || {
        manager
            .poll()
            .iter()
            .any(|event| matches!(event, CollabEvent::Invocation { id: 5, .. }))
    });
    drop(manager);
    fixture.thread.join().unwrap();
}

/// An invocation waits for its document's read to catch up for as long as that
/// takes, keeping the route's ownership; it fails, saying why, only once the
/// route will not become current.
#[tokio::test(start_paused = true)]
async fn native_invocation_delivery_turn_waits_until_the_route_fails() {
    let state = BridgeState::new();
    let original = state.document_reads.close_for_test("original-route");
    let wait = native_invocation_delivery_turn("original-route", &state);
    tokio::pin!(wait);
    assert!(
        tokio::time::timeout(Duration::from_secs(60), wait.as_mut())
            .await
            .is_err(),
        "a read still catching up is waited for, however long it takes"
    );
    assert!(state.document_reads.current("original-route", &original));
    state
        .document_reads
        .join_failed("original-route", "the first read was refused".into());
    let error = wait.await.unwrap_err();
    assert!(error.contains("the first read was refused"), "{error}");
}

// The fixtures build on the native broker types, which exist only on Unix.
#[cfg(unix)]
include!("native_fixtures.rs");

#[cfg(unix)]
pub(super) fn capturing_manager() -> (CollaborationManager, Box<dyn Fn() -> Vec<Vec<Vec<u8>>>>) {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let broker = FakeBroker {
        captures: captures.clone(),
        ..FakeBroker::default()
    };
    let (fd, _, _thread) = stand_up_fake_broker(broker);
    let manager = CollaborationManager::from_sdk_fd_for_test(fd, "capture-test", Uuid::new_v4());
    let probe = Box::new(move || captures.lock().unwrap().record_writes.clone());
    (manager, probe)
}

#[cfg(unix)]
pub(super) fn connected_manager() -> CollaborationManager {
    connected_manager_for("lifecycle-test", Uuid::new_v4())
}

#[cfg(unix)]
pub(super) fn connected_manager_for(app: &str, workspace: Uuid) -> CollaborationManager {
    let broker = FakeBroker::default();
    let (fd, _, _thread) = stand_up_fake_broker(broker);
    CollaborationManager::from_sdk_fd_for_test(fd, app, workspace)
}

/// The median, 95th percentile and longest of `samples`, by nearest rank.
#[cfg(unix)]
fn percentiles(
    mut samples: Vec<crate::time::Duration>,
) -> (
    crate::time::Duration,
    crate::time::Duration,
    crate::time::Duration,
) {
    samples.sort_unstable();
    let at = |percent: usize| {
        samples
            .get(samples.len().saturating_sub(1).saturating_mul(percent) / 100)
            .copied()
            .unwrap_or_default()
    };
    (at(50), at(95), samples.last().copied().unwrap_or_default())
}

/// What one store command costs an app's own process: the command the app sends on its
/// workspace connection, carried by its broker transport to the store and answered back on the
/// connection the app's frame waits on. An action names many resources, so it pays this per ask.
#[cfg(unix)]
#[test]
fn an_apps_store_command_is_a_local_trip() {
    // The connection every store command of the process goes on is one per process: the test
    // has the process to itself.
    tilo_test_env::in_own_process(
        module_path!(),
        "an_apps_store_command_is_a_local_trip",
        || {
            const APP: &str = "so.tilo.cost";
            const ASKS: u32 = 100;
            let workspace = Uuid::new_v4();
            let broker = FakeBroker::default();
            let (fd, _captures, thread) = stand_up_fake_broker(broker);
            let manager = CollaborationManager::from_sdk_fd_for_test(fd, APP, workspace);
            let command = serde_json::to_vec(&collab_types::store::wire::Command::RecordChanges {
                record: None,
                after: Vec::new(),
                from: 0,
            })
            .unwrap();
            let ask = || {
                crate::resource_command(APP, workspace, command.clone())
                    .expect("the app's connection carries its store commands")
                    .recv()
                    .expect("the connection stays open while the command is answered")
            };
            // The first command joins the route; the trip is measured over the ones after it,
            // which is what an action's loop makes.
            let joining = std::time::Instant::now();
            ask().expect("the store answers the first command");
            let first = joining.elapsed();
            let mut samples = Vec::new();
            for ask_number in 0..ASKS {
                let started = std::time::Instant::now();
                ask().unwrap_or_else(|refusal| {
                    panic!("ask {ask_number} was not answered: {refusal}")
                });
                samples.push(started.elapsed());
            }
            let (median, p95, longest) = percentiles(samples);
            eprintln!(
                "a store command from an app process: first {first:?}, then median {median:?}, \
                 p95 {p95:?}, max {longest:?} over {ASKS} commands"
            );
            assert!(
                p95 < crate::time::Duration::from_millis(2),
                "one store command from the app process has a p95 of {p95:?} (median {median:?}, \
                 max {longest:?}): the store is on this device, so a trip through the connection \
                 is a wake and a frame, not a wait for something to happen"
            );
            drop(manager);
            thread.join().unwrap();
        },
    );
}

/// Wait for `done`, polling: the worker and the fake broker run on their
/// own threads.
pub fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done() {
        assert!(crate::time::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_apps_live_activity_reaches_the_shell_and_its_controls_come_back() {
    const APP: &str = "so.tilo.clocks";
    tilo_test_env::in_own_process(
        module_path!(),
        "an_apps_live_activity_reaches_the_shell_and_its_controls_come_back",
        || {
            let board = Arc::new(runtime_collab::LiveActivities::default());
            let captures = Captures {
                live_activities: Some((APP.to_owned(), Arc::clone(&board))),
                ..Captures::default()
            };
            let broker = FakeBroker {
                captures: Arc::new(std::sync::Mutex::new(captures)),
                ..FakeBroker::default()
            };
            let (fd, _, _thread) = stand_up_fake_broker(broker);
            // Shown before the channel joins: the join sends it.
            super::super::channel_live_activity::show(Some(serde_json::json!({"label": "Tea"})));
            let _manager = CollaborationManager::from_sdk_fd_for_test(fd, APP, Uuid::new_v4());
            eventually("the activity reaches the shell", || {
                board.activity(APP) == Some(serde_json::json!({"label": "Tea"}))
            });

            super::super::channel_live_activity::show(Some(serde_json::json!({"label": "Eggs"})));
            eventually("a change reaches the shell", || {
                board.activity(APP) == Some(serde_json::json!({"label": "Eggs"}))
            });

            board.press(APP, serde_json::json!({"command": "toggle"}));
            let mut taken = Vec::new();
            eventually("the control comes back to the app", || {
                taken.extend(super::super::channel_live_activity::take_controls());
                !taken.is_empty()
            });
            assert_eq!(taken, vec![serde_json::json!({"command": "toggle"})]);

            super::super::channel_live_activity::show(None);
            eventually("ending it takes it off the shell", || {
                board.activity(APP).is_none()
            });
        },
    );
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn manager_connects_in_the_background_without_holding_the_app;

    phase a_broker_that_is_not_there_does_not_hold_the_app_up;
    {
        // A connected socket whose far end is gone — a broker that died
        // between the launcher's connect and the app's first call. `new()`
        // runs before the first frame, so this must return and keep working.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        drop(theirs);
        let fd = ours.into_raw_fd();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_SVC_COLLAB_FD", fd.to_string());

        let mut mgr = CollaborationManager::new();
        drop(env);

        // Whichever backend it settled on, the app goes on calling these
        // every frame. Neither may panic.
        mgr.send(CollabRequest::document_snapshot(
            b"after the broker died".to_vec(),
        ));
        let _ = mgr.poll();
    }

    phase construction_returns_at_once_while_the_broker_is_silent;
    {
        // A broker that accepted the connection but never answers: the
        // primary join cannot complete until the peer goes away. The old
        // constructor waited here before the app's first frame.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let (scope, wakes) = wake_channel();
        let entry = scope.enter();
        let route = RouteSpec {
            app_id: "silent-broker-test".into(),
            cluster_id: Uuid::new_v4(),
            document: None,
        };

        // The broker stays silent for as long as `theirs` lives, so a
        // constructor that waited for it would only return once its join
        // gave up, no longer connecting.
        let mut mgr = background_manager(ours.into_raw_fd(), route.clone());
        drop(entry);

        assert_eq!(mgr.state(), CollaborationState::Connecting);
        assert!(!mgr.is_connected());
        assert!(mgr.primary_route().is_none());
        assert_eq!(mgr.requested_primary_route(), Some(&route));
        let channel = collab_types::WorkspaceId::parse(&route.cluster_id.to_string()).unwrap();
        let group = key_group::KeyGroup::create(
            channel,
            key_group::MemberKey::generate("owner").unwrap(),
        )
        .unwrap();
        assert_eq!(
            mgr.install_document_encryption(route.clone(), group),
            Err("collaboration is still connecting".into()),
            "no call may wait on a worker that is still connecting"
        );
        // The frame loop keeps calling these while connecting. With no
        // acknowledged route nothing is queued behind the connection.
        assert!(!mgr.send(CollabRequest::Say {
            key: PresenceKey::surface_pointer("silent-broker-test"),
            payload: collab_types::presence::Payload::Arrived,
        }));
        assert!(mgr.poll().is_empty());
        assert_eq!(mgr.take_state_change(), None, "still connecting");

        // The broker goes away: the join fails and the manager goes offline,
        // telling its owner through a wake rather than a timer.
        drop(theirs);
        let settled = settle(&mut mgr, &wakes);
        assert!(
            matches!(settled, CollaborationState::Offline(_)),
            "{settled:?}"
        );
        assert!(!mgr.is_connected());
        assert_eq!(mgr.take_state_change(), Some(settled));
        assert_eq!(mgr.take_state_change(), None);
        // Offline managers stay usable no-ops.
        mgr.send(CollabRequest::document_snapshot(b"after".to_vec()));
        assert!(mgr.poll().is_empty());
    }

    phase a_background_connection_becomes_ready_and_wakes_the_owner;
    {
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let (scope, wakes) = wake_channel();
        let entry = scope.enter();
        let route = RouteSpec {
            app_id: "background-connect-test".into(),
            cluster_id: Uuid::new_v4(),
            document: None,
        };
        let mut mgr = background_manager(fd, route.clone());
        drop(entry);
        assert_eq!(mgr.state(), CollaborationState::Connecting);

        let settled = settle(&mut mgr, &wakes);

        assert_eq!(settled, CollaborationState::Ready);
        assert!(mgr.is_connected());
        assert_eq!(
            mgr.primary_route().map(crate::AcceptedRoute::route),
            Some(&route)
        );
        assert_eq!(mgr.take_state_change(), Some(CollaborationState::Ready));
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(
            joined,
            vec![route.channel_id()],
            "the queued Hello joined the primary route once connected"
        );
    }

}

#[cfg(unix)]
pub(super) fn connecting_manager() -> (
    CollaborationManager,
    crate::wake::WakeScope,
    mpsc::Receiver<()>,
) {
    let broker = FakeBroker::default();
    let (fd, _, _thread) = stand_up_fake_broker(broker);
    let (scope, wakes) = wake_channel();
    let manager = {
        let _entry = scope.enter();
        background_manager(
            fd,
            RouteSpec {
                app_id: "lifecycle-test".into(),
                cluster_id: Uuid::new_v4(),
                document: None,
            },
        )
    };
    (manager, scope, wakes)
}

/// Poll until the events received so far satisfy `done`, or the hang
/// bound passes. Returns everything received either way, so the caller's
/// assertions report what did arrive.
fn poll_until(
    mgr: &mut CollaborationManager,
    mut done: impl FnMut(&[CollabEvent]) -> bool,
) -> Vec<CollabEvent> {
    let deadline = tilo_test_env::hang_deadline();
    let mut events = Vec::new();
    loop {
        events.extend(mgr.poll());
        if done(&events) || crate::time::now() >= deadline {
            return events;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

tilo_test_env::scenario! {
    fn bridge_backpressure_sheds_presence_and_defers_results;

    phase saturated_outbound_bridge_sheds_only_presence_then_fails_closed;
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let document =
            || SdkCommand::Primary(CollabRequest::document_incremental(b"durable".to_vec()));
        let presence = || {
            SdkCommand::Primary(CollabRequest::Say {
                key: PresenceKey::Tab {
                    id: "saturated".to_owned(),
                },
                payload: collab_types::presence::Payload::Pointer { x: 1.0, y: 2.0 },
            })
        };

        assert!(enqueue_sdk_command(&tx, document(), &state));
        for _ in 0..10_000 {
            assert!(
                !enqueue_sdk_command(&tx, presence(), &state),
                "a full bounded queue may shed only this transient update"
            );
        }
        assert!(state.is_healthy());
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedSdkCommand {
                command: SdkCommand::Primary(CollabRequest::Document {
                    mutation: DocumentMutation::Incremental(data),
                    ..
                }),
                ..
            })
                if data == b"durable"
        ));

        assert!(enqueue_sdk_command(&tx, document(), &state));
        assert_eq!(
            try_enqueue_sdk_command(&tx, document(), &state),
            Err(TransportError::Busy),
            "a full queue defers a document change so its sender retries it"
        );
        assert!(
            state.is_healthy(),
            "deferring a document change never disconnects"
        );
        assert!(
            !enqueue_sdk_command(
                &tx,
                SdkCommand::Primary(CollabRequest::ClearContinuation),
                &state
            ),
            "a durable control overflow is refused as busy, to be parked by the transport"
        );
        assert!(
            state.is_healthy(),
            "overload must never disable collaboration"
        );
        assert!(
            rx.try_recv().is_ok() && rx.try_recv().is_err(),
            "the bounded queue retains only its admitted durable command"
        );
    }

    phase inbound_events_wake_the_owner_once_per_queued_burst;
    {
        let mut scope = crate::wake::WakeScope::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        scope
            .bind(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let _entry = scope.enter();
        let (tx, mut rx) = tokio_mpsc::channel(4);
        let inbound = InboundBridge::new(tx, BridgeState::new());
        assert!(inbound.send_reply(CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Update {
                invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                state: None,
                correlation: None,
            },
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "admitted work must wake the owner instead of waiting for its 60Hz poll"
        );
        assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
            member: collab_types::MemberId::person("peer"),
            x: 1.0,
            y: 2.0,
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "a queued burst needs only one wake"
        );
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        inbound.state.begin_poll();
        assert!(inbound.send_reply(CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Update {
                invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                state: None,
                correlation: None,
            },
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "an arrival after poll starts must schedule another wake, even during draining"
        );
        assert!(rx.try_recv().is_ok());
        scope.close();
        inbound.state.begin_poll();
        assert!(inbound.send_reply(CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Update {
                invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                state: None,
                correlation: None,
            },
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "a closed owner cannot be woken"
        );
    }

    phase saturated_inbound_bridge_drops_presence_and_holds_results;
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());

        for _ in 0..10_000 {
            assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
                member: collab_types::MemberId::person("peer"),
                x: 1.0,
                y: 2.0,
            }));
        }
        assert!(state.is_healthy());
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedCollabEvent {
                event: CollabEvent::RemoteCursorMoved { .. },
                ..
            })
        ));

        assert!(inbound.send_reply(CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Update {
                invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                state: None,
                correlation: None,
            },
        }));
        assert!(
            inbound.send_reply(CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::Receipt(
                    tilo_actions::remote::InvocationReceipt {
                        invocation: tilo_actions::InvocationId(uuid::Uuid::nil()),
                        status: tilo_actions::remote::ReceiptStatus::Accepted,
                    }
                ),
            }),
            "an invocation lifecycle result is never silently discarded"
        );
        assert!(state.is_healthy(), "a full queue is no failure");
        assert_eq!(
            inbound.replies.state.lock().unwrap().waiting.len(),
            1,
            "the result waits for workspace"
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedCollabEvent {
                event: CollabEvent::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Update { .. }
                },
                ..
            })
        ));
        assert!(rx.try_recv().is_err());
    }

    phase byte_reservation_releases_when_the_ui_consumes_a_command;
    {
        let (tx, mut rx) = tokio_mpsc::channel(2);
        let state = BridgeState::new();
        let document = || {
            SdkCommand::Primary(CollabRequest::document_incremental(vec![
                0;
                3 * 1024 * 1024
            ]))
        };

        // Consuming its queued wrapper drops its reservation before the next
        // admission, rather than leaking the aggregate budget.
        assert!(enqueue_sdk_command(&tx, document(), &state));
        drop(rx.try_recv().expect("queued document"));
        assert!(enqueue_sdk_command(&tx, document(), &state));
        assert!(state.is_healthy());
    }

    phase oversized_inbound_action_fails_before_queueing;
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());
        let mut payload = Vec::with_capacity(5 * 1024 * 1024);
        payload.push(1);

        assert!(!inbound.send_reply(CollabEvent::WorkspaceControl {
            session_id: 1,
            event: runtime_collab::WorkspaceControlEvent::Frame {
                surface: 1,
                encoded_h264: payload,
                timestamp: 0,
                seq: 0,
            },
        }));
        assert!(!state.is_healthy());
        assert!(rx.try_recv().is_err());
    }
}

tilo_test_env::scenario! {
    fn document_budget_defers_or_refuses_without_disconnecting;

    phase over_limit_document_is_refused_explicitly_without_disconnecting;
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let limit = collab_types::MAX_DOCUMENT_TRANSFER_BYTES;
        let oversized =
            SdkCommand::Primary(CollabRequest::document_incremental(vec![0; limit + 1]));

        assert_eq!(
            try_enqueue_sdk_command(&tx, oversized, &state),
            Err(TransportError::TooLarge {
                actual: limit + 1,
                max: limit,
            })
        );
        assert!(state.is_healthy(), "one oversized change keeps the bridge");
        assert!(rx.try_recv().is_err(), "nothing of it was queued");
        assert!(enqueue_sdk_command(
            &tx,
            SdkCommand::Primary(CollabRequest::document_incremental(vec![1; 8])),
            &state
        ));
    }

    phase document_budget_is_charged_for_bytes_sent_not_spare_capacity;
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let mut data = Vec::with_capacity(SDK_DOCUMENT_COMMAND_BYTES * 2);
        data.push(0);
        let command = SdkCommand::Primary(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(data),
            commit: None,
        });

        assert!(enqueue_sdk_command(&tx, command, &state));
        assert!(
            state.document_command_bytes.0.load(Ordering::Acquire) < 1024 * 1024,
            "a mostly-empty allocation is shrunk before it is parked on the bridge"
        );
        assert!(rx.try_recv().is_ok());
    }

    phase document_budget_defers_aggregate_documents_without_disconnecting;
    {
        let (tx, rx) = tokio_mpsc::channel(SDK_COMMAND_QUEUE);
        let state = BridgeState::new();
        let mut admitted = 0;
        let refusal = loop {
            let command =
                SdkCommand::Primary(CollabRequest::document_incremental(vec![0; 1024 * 1024]));
            match try_enqueue_sdk_command(&tx, command, &state) {
                Ok(()) => admitted += 1,
                Err(error) => break error,
            }
        };

        assert_eq!(refusal, TransportError::Busy);
        assert!(admitted < SDK_COMMAND_QUEUE, "bytes, not count, bound it");
        assert!(state.is_healthy());
        assert_eq!(rx.len(), admitted);
        assert!(
            enqueue_sdk_command(
                &tx,
                SdkCommand::Primary(CollabRequest::ClearContinuation),
                &state
            ),
            "queued document bytes never starve control traffic"
        );
    }

    phase a_busy_document_send_wakes_the_owner_once_room_frees;
    {
        let mut scope = crate::wake::WakeScope::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        scope
            .bind(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let _entry = scope.enter();
        let (tx, mut rx) = tokio_mpsc::channel(SDK_COMMAND_QUEUE);
        let state = BridgeState::new();
        let change =
            || SdkCommand::Primary(CollabRequest::document_incremental(vec![0; 1024 * 1024]));
        while try_enqueue_sdk_command(&tx, change(), &state).is_ok() {}
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            0,
            "a refusal on its own does not wake the owner"
        );
        drop(rx.try_recv().expect("admitted changes are queued"));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "the shovel taking a change frees workspace, and the idle owner must hear of it \
             to retry the refused one"
        );
        assert_eq!(try_enqueue_sdk_command(&tx, change(), &state), Ok(()));
        while rx.try_recv().is_ok() {}
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "an admitted send leaves nothing armed"
        );
    }

    phase a_directory_request_wakes_the_idle_owner;
    {
        let mut scope = crate::wake::WakeScope::new();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        scope
            .bind(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        let owner = scope.handle();
        let (requests, pending) = mpsc::sync_channel(1);
        let (reply, _response) = tokio::sync::oneshot::channel();
        submit_directory_request(
            &requests,
            &owner,
            ActionDirectoryRequest::Resolve {
                revision: String::new(),
                resource: String::new(),
                action: String::new(),
                reply,
            },
        )
        .unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(pending.try_recv().is_ok());
    }

    phase a_lone_document_larger_than_the_budget_still_makes_progress;
    {
        let budget = ByteBudget::new();
        let big = budget
            .reserve_sole(SDK_DOCUMENT_EVENT_BYTES + 1, SDK_DOCUMENT_EVENT_BYTES)
            .expect("an empty budget admits one oversized item");
        assert!(
            budget.reserve_sole(1, SDK_DOCUMENT_EVENT_BYTES).is_none(),
            "nothing queues beside it"
        );
        drop(big);
        assert!(budget.reserve_sole(1, SDK_DOCUMENT_EVENT_BYTES).is_some());
    }
}

#[test]
fn disconnected_manager_is_a_no_op() {
    // No SDK, no legacy socket set — falls through to `Backend::Disabled`.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_SVC_COLLAB_FD");
    let mut mgr = CollaborationManager::new();
    assert!(!mgr.is_connected(), "no service = not connected");
    assert!(matches!(mgr.state(), CollaborationState::Offline(_)));
    assert_eq!(mgr.take_state_change(), None, "offline from the start");
    mgr.send(CollabRequest::document_snapshot(b"nope".to_vec()));
    assert!(mgr.poll().is_empty(), "no events on Disabled backend");
}

/// A wake scope whose wakes arrive on a channel, so a test can wait for
/// the manager to have news instead of polling on a timer.
pub fn wake_channel() -> (crate::wake::WakeScope, mpsc::Receiver<()>) {
    let mut scope = crate::wake::WakeScope::new();
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    scope
        .bind(move |_| {
            let _ = tx.lock_recover().send(());
        })
        .unwrap();
    (scope, rx)
}

/// Poll on every wake until the manager leaves `Connecting`.
fn settle(mgr: &mut CollaborationManager, wakes: &mpsc::Receiver<()>) -> CollaborationState {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let _ = mgr.poll();
        let state = mgr.state();
        if state != CollaborationState::Connecting {
            return state;
        }
        let left = deadline.saturating_duration_since(crate::time::now());
        assert!(
            wakes.recv_timeout(left).is_ok(),
            "gave up waiting for the connecting manager to wake its owner as it settles; \
             still Connecting after {:?}",
            tilo_test_env::hang_bound()
        );
    }
}

fn background_manager(fd: RawFd, route: RouteSpec) -> CollaborationManager {
    CollaborationManager::connect_in_background(
        SdkConnectSource::Fd(fd),
        SdkIdentitySource::Ready(Some(SdkJoinIdentity {
            member_kind: "person",
            signer: harness_document_signer(),
        })),
        route,
        None,
        None,
    )
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn sdk_transport_forwards_documents_and_presence;

    phase hello_joins_channel_and_reads_its_record;
    {
        let broker = FakeBroker::default();
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let cluster = Uuid::new_v4();
        let mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", cluster);

        tilo_test_env::wait_until("the record must be opened", || {
            !captures.lock().unwrap().joined.is_empty()
        });
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        // The channel id carries the whole address, app id included.
        assert_eq!(
            joined[0],
            channel_key::format("alice", &cluster.to_string(), ""),
        );

        // The route reads what the store holds of its record as it opens.
        tilo_test_env::wait_until("the route to read its record", || {
            !captures.lock().unwrap().record_reads.is_empty()
        });
        assert!(mgr.is_connected());
    }

    phase owned_route_documents_reach_the_routes_record;
    {
        use automerge::transaction::Transactable;
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let workspace = Uuid::new_v4();
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "shell", workspace);
        let route = RouteSpec::document("actual-owner", workspace, "tab:qualified");
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "body", "before").unwrap();
        document.commit();
        let heads = document.base_heads();
        let snapshot = document.save();
        document.put(automerge::ROOT, "body", "after").unwrap();
        collab_types::CommitNamed::commit_edit(&mut document, "fixture", "body", &[1; 32]);
        let delta = document.save_after(&heads);
        // The route refuses documents until it is joined and its document
        // mode is resolved; the first refused attempt starts that join.
        tilo_test_env::wait_until("the owned route becomes publishable", || {
            manager
                .try_send_to_route(
                    route.clone(),
                    CollabRequest::document_snapshot(snapshot.clone()),
                )
                .is_ok()
        });
        manager
            .try_send_to_route(
                route.clone(),
                CollabRequest::document_incremental(delta.clone()),
            )
            .unwrap();
        tilo_test_env::wait_until("both requests reach the route's record", || {
            captures.lock().unwrap().record_writes.len() == 2
        });
        let captured = captures.lock().unwrap();
        assert!(captured.joined.iter().any(|channel| channel
            == &channel_key::format("actual-owner", &workspace.to_string(), "tab:qualified")));
        assert_eq!(
            captured.record_writes,
            [changes_of(&snapshot), changes_of(&delta)],
            "the record whole, then the change made since, each as its changes"
        );
    }

    phase publication_barrier_resolves_after_every_earlier_document_is_published;
    // `send` only queues. A publication barrier resolves once every
    // document mutation queued before it has reached the broker, which is
    // what lets an owner end its session without discarding a change still
    // in transit.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "shell", Uuid::new_v4());
        for n in 0..8_u8 {
            assert!(manager.send(CollabRequest::document_incremental(change(&n.to_string()))));
        }
        manager
            .begin_document_publication_barrier()
            .expect("the broker transport supports publication barriers")
            .wait(Some(tilo_test_env::hang_deadline()))
            .expect("every queued document was published");
        assert_eq!(
            captures.lock().unwrap().record_writes.len(),
            8,
            "the barrier resolved before the documents ahead of it reached the broker"
        );
    }
}

tilo_test_env::scenario! {
    fn presence_round_trips_every_keyed_variant;

    phase a_word_leaves_as_its_frame_and_others_words_return_as_they_went;
    {
        use collab_types::presence::{Payload, Presence};
        let key = PresenceKey::Tab {
            id: "note-1".to_owned(),
        };
        let member = collab_types::MemberId::person("peer-bob");
        let words = [
            Payload::Pointer { x: 6.0, y: 9.0 },
            Payload::Caret { pos: 7 },
            Payload::Selection { start: 1, end: 3 },
            Payload::Typing { start: 2, end: 5 },
            Payload::Arrived,
            Payload::Left,
        ];
        let frame = |payload| Presence::app("app", Uuid::nil().to_string(), key.clone(), payload);
        let mut presence_backlog: Vec<SeedPresence> = words
            .iter()
            .map(|payload| SeedPresence {
                member: member.clone(),
                frame: frame(payload.clone()),
            })
            .collect();
        // An interaction that does not decode names nothing: it is refused rather than shown.
        presence_backlog.push(SeedPresence {
            member: member.clone(),
            frame: frame(Payload::InteractionJson("{".to_owned())),
        });
        let broker = FakeBroker {
            presence_backlog,
            ..FakeBroker::default()
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        tilo_test_env::wait_until("the record must be opened", || {
            !captures.lock().unwrap().joined.is_empty()
        });

        for payload in &words {
            mgr.send(CollabRequest::Say {
                key: key.clone(),
                payload: payload.clone(),
            });
        }
        tilo_test_env::wait_until("every word reaches the host as its frame", || {
            captures.lock().unwrap().said.len() == words.len()
        });
        let said = captures.lock().unwrap().said.clone();
        assert_eq!(
            said.iter().map(|frame| frame.payload.clone()).collect::<Vec<_>>(),
            words.to_vec(),
            "each word leaves once, in order, under the key its app chose"
        );
        assert!(said.iter().all(|frame| frame.key == key && frame.app == "alice"));

        let events = poll_until(&mut mgr, |events| {
            events
                .iter()
                .filter(|e| matches!(e, CollabEvent::RemotePresenceUpdate { .. }))
                .count()
                >= words.len()
        });
        let updates: Vec<_> = events
            .into_iter()
            .filter_map(|e| match e {
                CollabEvent::RemotePresenceUpdate { member, key, payload } => {
                    Some((member, key, payload))
                }
                _ => None,
            })
            .collect();
        let expected: Vec<_> = words
            .into_iter()
            .map(|payload| {
                let payload = PresencePayload::from_live(payload).unwrap().unwrap();
                (member.clone(), key.clone(), payload)
            })
            .collect();
        assert_eq!(updates, expected, "every word arrives as it went, by whom said it");
    }
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn sdk_streams_translate_to_ui_events;

    phase the_records_changes_reach_the_app_as_it_opens_and_as_they_are_made;
    {
        let stored = change("in the store");
        let remote = change("made elsewhere");
        let (peer, peer_changes) = tokio_mpsc::unbounded_channel();
        let broker = FakeBroker {
            record: vec![stored.clone()],
            peer_changes: Some(peer_changes),
            ..FakeBroker::default()
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        let documents = |events: Vec<CollabEvent>| -> Vec<DocumentMutation> {
            events
                .into_iter()
                .filter_map(|event| match event {
                    CollabEvent::Document { mutation, .. } => Some(mutation),
                    _ => None,
                })
                .collect()
        };
        let opened = documents(poll_until(&mut mgr, |events| {
            events
                .iter()
                .any(|e| matches!(e, CollabEvent::Document { .. }))
        }));
        assert_eq!(opened.len(), 1, "one document event expected");
        assert_eq!(changes_of(opened[0].data()), changes_of(&stored));
        let reads = captures.lock().unwrap().record_reads.len();

        peer.send(remote.clone()).unwrap();
        let changed = documents(poll_until(&mut mgr, |events| {
            events
                .iter()
                .any(|e| matches!(e, CollabEvent::Document { .. }))
        }));
        assert_eq!(changed.len(), 1, "one document event per change");
        assert_eq!(
            changes_of(changed[0].data()),
            changes_of(&remote),
            "only the change the app lacks"
        );
        assert_eq!(
            captures.lock().unwrap().record_reads.len(),
            reads + 1,
            "one read per notice"
        );
    }

    phase context_stream_translates_to_context_event;
    {
        let broker = FakeBroker {
            context_backlog: vec![SeedContext {
                permissions: vec!["edit".to_owned(), "share".to_owned()],
            }],
            terminate_context_stream: true,
            ..FakeBroker::default()
        };
        let (fd, _captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        let events = poll_until(&mut mgr, |events| {
            events
                .iter()
                .filter(|event| matches!(event, CollabEvent::Context { .. }))
                .count()
                == 2
        });

        let contexts = events
            .iter()
            .filter_map(|e| match e {
                CollabEvent::Context { permissions, .. } => Some(permissions.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(contexts.len(), 2, "source loss must withdraw its context");
        assert_eq!(contexts[0], ["edit".to_owned(), "share".to_owned()]);
        assert!(contexts[1].is_empty());
    }

}

tilo_test_env::scenario! {
    fn invocation_context_is_read_fail_closed;

    phase invocation_context_json_is_optional_and_fail_closed;
    {
        let context = tilo_actions::ActionContext::default()
            .with_purpose("Review the proposal")
            .with_group("agent-run:review", "Review", "agent-run");
        let json = serde_json::to_string(&context).unwrap();
        assert_eq!(parse_invocation_context(7, &json), Some(context));
        assert_eq!(parse_invocation_context(8, "  "), None);
        assert_eq!(parse_invocation_context(9, "{not-json"), None);
    }
}

// The process's bridge to its session transport.
#[cfg(test)]
mod bridge {
    use super::*;

    #[test]
    fn a_transport_carries_the_published_bridge_before_it_connects() {
        // An app builds its windows before the shovel has joined. A view that
        // follows a published resource then reaches the connecting transport
        // (its watch waits in the queue for the shovel) instead of being
        // refused as if the session had no collaboration service.
        let (transport, _init) =
            CollaborationManager::start_sdk(SdkConnectSource::Env, SdkIdentitySource::Ready(None))
                .expect("the shovel thread starts");
        let registered = PUBLISHED_COMMAND_BRIDGE.sender();
        assert!(
            registered.is_some(),
            "the process's bridge points at the transport while it connects"
        );
        drop(transport);
    }

    #[test]
    fn a_later_transport_takes_the_process_bridge_only_once_the_first_has_ended() {
        // A process that is its own launcher (a phone running the device's daemon) makes its
        // own connection first, and each app it opens joins over one of its own later: what the
        // process sends for its own records goes on its own connection, never on an app's,
        // which the workspace attests as that app, and does not end when that app's window does.
        let start = || {
            CollaborationManager::start_sdk(SdkConnectSource::Env, SdkIdentitySource::Ready(None))
                .expect("the shovel thread starts")
                .0
        };
        let first = start();
        let second = start();
        let carries = |transport: &BrokerTransport| {
            PUBLISHED_COMMAND_BRIDGE
                .sender()
                .is_some_and(|sender| sender.same_channel(&transport.outbound_tx))
        };
        assert!(carries(&first), "the process's own connection carries it");
        drop(first);
        assert!(
            carries(&second),
            "once the first has ended, the one that still runs carries it"
        );
        drop(second);
    }
}
