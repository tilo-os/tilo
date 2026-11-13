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
    clippy::unreachable,
    clippy::string_slice,
    clippy::disallowed_methods,
    clippy::future_not_send,
    reason = "the tests and the fixtures they share with the toolkit's tests are test code"
)]
use super::*;
use runtime_collab::{ActionCatalogue, BrokerActionSurface, SurfaceDelta};

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
fn publication_gate_waits(state: &BridgeState) -> usize {
    state.publication_gate_waits.load(Ordering::Acquire)
}

/// Wait until the shovel has seen a queued document mutation find its
/// route not publishable since `before` ([`publication_gate_waits`]):
/// the mutation is held at the gate until the route changes. Read after
/// the mutation was queued, the count may already hold its one wait,
/// which then sleeps to its deadline without counting again.
#[cfg(test)]
fn wait_held_at_publication_gate(state: &BridgeState, before: usize) {
    tilo_test_env::wait_until(
        "the queued mutation to wait at the publication gate",
        || state.publication_gate_waits.load(Ordering::Acquire) > before,
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
include!("committed_changes_tests.rs");
use crate::CollaborationState;

fn plain_route_keys() -> SharedRouteKeys {
    std::sync::Arc::new(std::sync::Mutex::new(
        super::super::route_keys::RouteKeys::plain(),
    ))
}

fn group_route_keys(group: channel_crypto::ChannelGroup) -> SharedRouteKeys {
    let keys = plain_route_keys();
    keys.lock().unwrap().install_group(group);
    keys
}

use crate::time::{Duration, Instant};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
#[cfg(unix)]
use std::os::fd::IntoRawFd;
use std::rc::Rc;

use runtime_collab::wire::{
    self, Call, ContextUpdate, Event, Handle, HostFrame, InvocationCall, LiveActivityCall,
    Operation, Publication, Reply, Service, SettingsCall, Topic,
};
use tokio::io::AsyncWriteExt as _;

/// Changes the fake broker hands a reader holding `from`.
#[derive(Clone)]
pub struct FixtureChanges {
    from: [u8; 32],
    base: Vec<[u8; 32]>,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub struct Captures {
    /// How long after it is asked for the op stream hands out each op.
    pub op_stream_delay: Option<Duration>,
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
    pub committed_recheck_gate: Option<tokio::sync::oneshot::Receiver<()>>,
    pub committed_ack_gate: Option<(mpsc::Sender<()>, tokio::sync::oneshot::Receiver<()>)>,
    pub committed_private_binding: Option<u64>,
    pub committed_ack_failure: Option<String>,
    pub committed_ack_reply: Option<runtime_collab::committed_document::Reply>,
    /// Refuse every committed Open, as a broker does for a revoked peer.
    pub committed_open_failure: Option<String>,
    /// Refuse committed Opens on these channels only, as the session
    /// host does for a route this connection holds no read custody of.
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
    /// Hold each next join until its gate opens, as a starved broker
    /// does.
    pub join_gates: std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>,
    /// Refuse every ungated join, as a broker does for a revoked peer.
    pub join_refusal: Option<String>,
    /// The workspace's status as the broker reports it, or why the broker
    /// cannot be asked; without it the host refuses to say.
    pub workspace_status:
        Option<tokio::sync::watch::Receiver<Result<runtime_collab::WorkspaceStatus, String>>>,
    pub committed_reads: Vec<runtime_collab::committed_document::Request>,
    pub media_commands: Vec<runtime_collab::native_media::Request>,
    pub media_active_fixture: bool,
    pub media_capture_active: bool,
    pub media_pending_consent: bool,
    pub media_joined: bool,
    pub media_stops: usize,
    pub media_gates: std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>,
    /// Lose the connection on the next document commit: the peer may or
    /// may not have kept the change, an unknown outcome.
    pub fail_document: bool,
    /// Refuse this many document commits the way a broker that will not
    /// take the change answers: a definite error, not a lost call.
    pub refuse_documents: usize,
    pub refused_documents: usize,
    pub document_gates: std::collections::VecDeque<DocumentSendGate>,
    /// Refuse every non-terminal invocation update, and lose the
    /// connection on the first terminal one, whose delivery is then
    /// unknown.
    pub fail_terminal: bool,
    /// Refuse this many invocation updates the way a broker that will not
    /// take them answers: a definite error, not a lost call.
    pub refuse_terminals: usize,
    pub terminal_updates: usize,
    pub invocation_updates: Vec<(u64, tilo_actions::InvocationUpdate)>,
    pub joined: Vec<(String, Vec<u8>)>,
    pub joined_record_kinds: Vec<String>,
    pub signed_ops: Vec<Vec<u8>>,
    pub pointer_publishes: Vec<(Vec<u8>, f32, f32)>,
    pub caret_publishes: Vec<(Vec<u8>, u64)>,
    pub selection_publishes: Vec<(Vec<u8>, u64, u64)>,
    pub typing_publishes: Vec<(Vec<u8>, u64, u64)>,
    pub arrived: Vec<Vec<u8>>,
    pub left: Vec<Vec<u8>>,
    pub scrollback_requests: Vec<(Vec<u8>, u64, u32)>,
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
    pub action_catalogue_fixture: Option<Vec<String>>,
    pub action_catalogue_subscriptions: usize,
    /// The channel service whose changes `Changed { ChannelKeys }`
    /// follows; without one the host refuses the wait.
    pub key_changes: Option<Arc<tokio::sync::watch::Sender<u64>>>,
    /// Answers this channel's private service requests, as the session
    /// does on an app's route; without one the call is unimplemented,
    /// as on a broker with no custodian behind it.
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

pub struct DocumentSendGate {
    entered: mpsc::Sender<()>,
    release: tokio::sync::oneshot::Receiver<Result<(), String>>,
}

/// Shared between the fake host's thread and the test.
pub type SharedCaptures = std::sync::Arc<std::sync::Mutex<Captures>>;

#[derive(Clone)]
pub struct SeedPresence {
    user_id: String,
    key_bytes: Vec<u8>,
    payload: collab_types::PeerPresencePayload,
}

#[derive(Clone)]
pub struct SeedContext {
    collaborators: Vec<Collaborator>,
    permissions: Vec<String>,
}

#[derive(Clone)]
pub struct SeedScrollback {
    ops: Vec<Vec<u8>>,
    done: bool,
}

/// `(op bytes, author, verdict)` the fake host's op topic delivers.
type OpBacklog = Rc<RefCell<Vec<(Vec<u8>, String, OpAdmission)>>>;

/// What a fake host's channels start with, and what it records.
#[derive(Default)]
pub struct FakeBroker {
    pub captures: SharedCaptures,
    pub op_backlog: Vec<(Vec<u8>, String, OpAdmission)>,
    pub presence_backlog: Vec<SeedPresence>,
    pub context_backlog: Vec<SeedContext>,
    pub terminate_context_stream: bool,
    pub scrollback_response: Option<SeedScrollback>,
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
        let mut captures = captures.lock().unwrap();
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
                return Err("committed read custody refused for this route".into());
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
        let gate = if matches!(reply, Read::Unchanged { .. }) {
            captures.committed_recheck_gate.take()
        } else if matches!(reply, Read::Cancelled) {
            captures.committed_ack_gate.take().map(|(entered, gate)| {
                entered.send(()).unwrap();
                gate
            })
        } else {
            None
        };
        Ok((encode(reply)?, gate))
    })();
    Box::pin(async move {
        let (reply, gate) = result?;
        if let Some(gate) = gate {
            gate.await
                .map_err(|_| "fixture recheck gate dropped".to_owned())?;
        }
        Ok(reply)
    })
}

/// One channel a fake host's connection joined.
struct FakeChannel {
    channel_id: String,
    op_backlog: OpBacklog,
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
    join_gate: Option<HostJoinGate>,
    frames: tokio_mpsc::UnboundedSender<HostFrame>,
    channels: RefCell<HashMap<Handle, Rc<FakeChannel>>>,
    subscriptions: RefCell<HashMap<u64, (Handle, tokio::task::AbortHandle)>>,
    next_handle: Cell<u64>,
    /// Ends the connection unanswered: a call whose outcome is unknown.
    hang_up: Rc<tokio::sync::Notify>,
}

/// Serve `broker` as the collaboration host on `stream` until either
/// side closes it. Runs on a `LocalSet`.
#[expect(
    clippy::future_not_send,
    reason = "the fake host keeps its channels in `Rc`s on the `LocalSet` it serves on"
)]
pub(super) async fn serve_fake_broker<S>(stream: S, broker: FakeBroker)
where
    S: AsyncRead + AsyncWrite + 'static,
{
    serve_fake_host(stream, broker, None).await;
}

#[expect(
    clippy::future_not_send,
    reason = "the fake host keeps its channels in `Rc`s on the `LocalSet` it serves on"
)]
async fn serve_fake_host<S>(stream: S, broker: FakeBroker, join_gate: Option<HostJoinGate>)
where
    S: AsyncRead + AsyncWrite + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (frames, mut outgoing) = tokio_mpsc::unbounded_channel();
    let host = Rc::new(FakeHost {
        broker,
        join_gate,
        frames,
        channels: RefCell::default(),
        subscriptions: RefCell::default(),
        next_handle: Cell::new(1),
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
    let hang_up = host.hang_up.clone();
    tokio::select! {
        () = write => {}
        () = read => {}
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

    fn channel(&self, handle: Handle) -> Result<Rc<FakeChannel>, String> {
        self.channels
            .borrow()
            .get(&handle)
            .cloned()
            .ok_or_else(|| format!("no channel {handle:?} on this connection"))
    }

    /// Lose the connection, never answering the call at hand.
    fn hang_up(&self) -> FakeAnswer {
        self.hang_up.notify_one();
        Box::pin(std::future::pending())
    }

    fn accept(self: &Rc<Self>, Call { id, request }: Call) {
        match request {
            Operation::Subscribe { channel, topic } => self.subscribe(id, channel, topic),
            Operation::Unsubscribe { subscription } => {
                if let Some((_, task)) = self.subscriptions.borrow_mut().remove(&subscription) {
                    task.abort();
                }
            }
            Operation::Close { handle } => {
                self.channels.borrow_mut().remove(&handle);
                self.subscriptions
                    .borrow_mut()
                    .retain(|_, (channel, task)| {
                        let open = *channel != handle;
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

    fn subscribe(self: &Rc<Self>, id: u64, handle: Handle, topic: Topic) {
        let feed = Feed {
            frames: self.frames.clone(),
            subscription: id,
        };
        let delivery = self
            .channel(handle)
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
            .insert(id, (handle, task.abort_handle()));
    }

    /// What `topic` delivers on `channel`, ending with why it ended.
    fn topic(
        &self,
        channel: &Rc<FakeChannel>,
        topic: Topic,
        feed: Feed,
    ) -> Result<Pin<Box<dyn Future<Output = String>>>, String> {
        let captures = self.broker.captures.clone();
        Ok(match topic {
            Topic::Ops => {
                let backlog = channel.op_backlog.clone();
                Box::pin(async move {
                    loop {
                        let next = {
                            let mut backlog = backlog.borrow_mut();
                            (!backlog.is_empty()).then(|| backlog.remove(0))
                        };
                        let Some((op, from_user_id, admission)) = next else {
                            // Clients that outrun the backlog wait, as on
                            // a quiet channel.
                            return std::future::pending().await;
                        };
                        let delay = captures.lock().unwrap().op_stream_delay;
                        if let Some(delay) = delay {
                            tokio::time::sleep(delay).await;
                        }
                        feed.send(Event::Op {
                            op,
                            from_user_id,
                            admission,
                        });
                    }
                })
            }
            Topic::Presence => {
                let backlog = std::mem::take(&mut *channel.presence_backlog.borrow_mut());
                Box::pin(async move {
                    for seed in backlog {
                        feed.send(Event::Presence {
                            user_id: seed.user_id,
                            presence: collab_types::PeerPresence {
                                key: seed.key_bytes,
                                payload: seed.payload,
                            },
                        });
                    }
                    std::future::pending().await
                })
            }
            Topic::Context => {
                let backlog = std::mem::take(&mut *channel.context_backlog.borrow_mut());
                let terminate = self.broker.terminate_context_stream;
                Box::pin(async move {
                    for seed in backlog {
                        feed.send(Event::Context(ContextUpdate::Snapshot {
                            collaborators: seed.collaborators,
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
                                }));
                            }
                            None => tokio::time::sleep(Duration::from_millis(1)).await,
                        }
                    }
                })
            }
            Topic::Catalogue => {
                let snapshots = {
                    let mut captures = captures.lock().unwrap();
                    captures.action_catalogue_subscriptions += 1;
                    captures
                        .action_catalogue_fixture
                        .clone()
                        .unwrap_or_else(|| {
                            vec![serde_json::to_string(&CatalogueChange::snapshot(&[])).unwrap()]
                        })
                };
                Box::pin(async move {
                    for snapshot in snapshots {
                        feed.send(Event::Catalogue(snapshot));
                    }
                    std::future::pending().await
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
            Operation::Join(join) => self.join(join),
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
                channel,
                publication,
            } => match self.channel(channel) {
                Ok(channel) => self.publish(&channel, publication),
                Err(error) => answered(Err(error)),
            },
            Operation::Command {
                channel,
                service,
                body,
                ..
            } => match self.channel(channel) {
                Ok(channel) => match service {
                    Service::CommittedDocument => self.committed_document(&channel, &body),
                    Service::NativeMedia => self.native_media(&body),
                    Service::RemoteAction => answered(self.remote_action(&body)),
                    Service::BrowserServiceSession => answered(self.browser_service_session(&body)),
                    other => answered(Err(format!("the fixture serves no {other:?}"))),
                },
                Err(error) => answered(Err(error)),
            },
            Operation::Changed {
                of: wire::Changes::ChannelKeys,
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
            Operation::Settings { channel, call } => {
                let settings = self
                    .channel(channel)
                    .and_then(|channel| self.settings(&channel));
                let settings = match settings {
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
            }
            Operation::LiveActivity { channel, call } => {
                let activity = self
                    .channel(channel)
                    .and_then(|channel| self.live_activity(&channel));
                let activity = match activity {
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
            }
            Operation::Read {
                read:
                    wire::Read::History {
                        artifact,
                        before_seq,
                        max,
                    },
                ..
            } => {
                self.captures().scrollback_requests.push((
                    artifact.0.as_bytes().to_vec(),
                    before_seq,
                    max,
                ));
                let seed = self
                    .broker
                    .scrollback_response
                    .clone()
                    .unwrap_or(SeedScrollback {
                        ops: Vec::new(),
                        done: true,
                    });
                answered(Ok(Reply::History {
                    ops: seed.ops,
                    exhausted: seed.done,
                }))
            }
            other => answered(Err(format!("the fixture does not answer {other:?}"))),
        }
    }

    fn join(self: &Rc<Self>, join: wire::Join) -> FakeAnswer {
        let gate = self
            .join_gate
            .as_ref()
            .map(|gate| gate.attempt(&join.channel));
        let mut captures = self.captures();
        captures
            .joined
            .push((join.channel.clone(), join.identity.clone()));
        captures
            .joined_record_kinds
            .push(join.record_kind.clone().unwrap_or_default());
        let held = captures.join_gates.pop_front();
        if held.is_none()
            && let Some(error) = captures.join_refusal.clone()
        {
            return answered(Err(error));
        }
        drop(captures);
        let host = Rc::clone(self);
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await?;
            }
            if let Some(held) = held {
                held.await
                    .map_err(|_| "fixture join gate dropped".to_owned())?;
            }
            let handle = Handle(host.next_handle.get());
            host.next_handle.set(handle.0 + 1);
            host.channels.borrow_mut().insert(
                handle,
                Rc::new(FakeChannel {
                    channel_id: join.channel,
                    op_backlog: Rc::new(RefCell::new(host.broker.op_backlog.clone())),
                    presence_backlog: Rc::new(RefCell::new(host.broker.presence_backlog.clone())),
                    context_backlog: Rc::new(RefCell::new(host.broker.context_backlog.clone())),
                    settings: RefCell::default(),
                    live_activity: RefCell::default(),
                }),
            );
            Ok(Reply::Opened(handle))
        })
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
            Publication::Presence(collab_types::PeerPresence { key, payload }) => {
                use collab_types::PeerPresencePayload as P;
                let mut captures = self.captures();
                match payload {
                    P::Pointer { x, y } => captures.pointer_publishes.push((key, x, y)),
                    P::Caret { pos } => captures.caret_publishes.push((key, pos)),
                    P::Selection { start, end } => {
                        captures.selection_publishes.push((key, start, end));
                    }
                    P::Typing { start, end } => {
                        captures.typing_publishes.push((key, start, end));
                    }
                    P::Arrived => captures.arrived.push(key),
                    P::Left => captures.left.push(key),
                    _ => {}
                }
                answered(Ok(Reply::Done))
            }
            Publication::Document(op) => self.send_document(&op),
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

    fn send_document(&self, op: &SignedOp) -> FakeAnswer {
        let mut captures = self.captures();
        if captures.fail_document {
            // The call is lost on its way back: the peer may or may not
            // have kept the change, which is an unknown outcome, not an
            // answer. A refusal below is the opposite: the peer read the
            // change and said it will not take this one.
            drop(captures);
            return self.hang_up();
        }
        if captures.refuse_documents > 0 {
            captures.refuse_documents -= 1;
            captures.refused_documents += 1;
            tracing::warn!("collab: fixture refuses one document change");
            return answered(Err("so.tilo.chat named no kind for a new record".into()));
        }
        captures.signed_ops.push(postcard::to_stdvec(op).unwrap());
        if let Some(gate) = captures.document_gates.pop_front() {
            gate.entered.send(()).unwrap();
            return Box::pin(async move {
                gate.release
                    .await
                    .map_err(|_| "document gate dropped".to_owned())?
                    .map(|()| Reply::Done)
            });
        }
        answered(Ok(Reply::Done))
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
    /// A private service request, answered as the session's custodian
    /// would through `service_sessions`; refused without one.
    fn browser_service_session(&self, body: &[u8]) -> Result<Reply, String> {
        let Some(serve) = self.captures().service_sessions.clone() else {
            return Err("fixture has no custodian".into());
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
            ActionHostError, InvocationReceipt, ReceiptStatus, RemoteActionFrame,
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
            tilo_actions::remote::refuse(&frame.message, ActionHostError::UnknownAction)
                .expect("an invocation has a refusal")
        };
        RemoteActionFrame::new(answer)
            .encode()
            .map(Reply::Answer)
            .map_err(|error| error.to_string())
    }

    fn native_media(&self, body: &[u8]) -> FakeAnswer {
        let request = match runtime_collab::native_media::Request::decode(body) {
            Ok(request) => request,
            Err(error) => return answered(Err(error.to_string())),
        };
        assert!(request.dispatch_valid());
        let gate = {
            let mut captures = self.captures();
            if captures.media_active_fixture {
                use runtime_collab::native_media::Command;
                match &request.command {
                    Command::SetMuted { muted: true } | Command::Leave => {
                        captures.media_capture_active = false;
                        captures.media_pending_consent = false;
                        captures.media_stops += 1;
                        if matches!(request.command, Command::Leave) {
                            captures.media_joined = false;
                        }
                    }
                    Command::SetMuted { muted: false }
                        if !captures.media_capture_active && captures.media_joined =>
                    {
                        captures.media_pending_consent = true;
                    }
                    _ => {}
                }
            }
            captures.media_commands.push(request);
            captures.media_gates.pop_front()
        };
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await
                    .map_err(|_| "native media gate dropped".to_owned())?;
            }
            Ok(Reply::Answer(
                runtime_collab::native_media::Snapshot::unavailable()
                    .encode()
                    .unwrap(),
            ))
        })
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

/// A client connected to a fake host serving `broker` on this
/// `LocalSet`, and its captures.
pub(super) fn fake_client(broker: FakeBroker) -> (runtime_collab::CollabClient, SharedCaptures) {
    let captures = broker.captures.clone();
    let (client, host) = tokio::io::duplex(64 * 1024);
    tokio::task::spawn_local(serve_fake_broker(host, broker));
    let (client, _end) = runtime_collab::CollabClient::connected_local(client);
    (client, captures)
}

fn host_identity_fixture(
    certificate: Option<Vec<u8>>,
    replies: Vec<runtime_identity::wire::Response>,
) -> (tokio::io::DuplexStream, Arc<std::sync::Mutex<Vec<u8>>>) {
    host_identity_fixture_with_keys(certificate, replies, None)
}

/// A scripted identity service. `ChannelKeys` requests arrive on the
/// endpoint's own refresh cadence, so they are answered from `keys`
/// (or as unavailable) without consuming the scripted `replies`.
fn host_identity_fixture_with_keys(
    certificate: Option<Vec<u8>>,
    replies: Vec<runtime_identity::wire::Response>,
    keys: Option<(u64, Vec<u8>)>,
) -> (tokio::io::DuplexStream, Arc<std::sync::Mutex<Vec<u8>>>) {
    use runtime_identity::wire::Response;
    let mut responses = vec![Response::Metadata {
        display_name: "Host fixture".into(),
        avatar_color: [1, 2, 3],
        group_public_key: [4; 32],
        signing_public_key: [5; 32],
        device_certificate: certificate,
        threshold: 1,
        total_shares: 1,
        share_index: 1,
    }];
    responses.extend(replies);
    let output = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = output.clone();
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    tokio::task::spawn_local(async move {
        let framing = runtime_identity::wire::FRAMING;
        let mut responses = responses.into_iter();
        loop {
            let Ok(Some(body)) = framing.read_async(&mut server).await else {
                return;
            };
            captured
                .lock()
                .unwrap()
                .extend_from_slice(&framing.encode(&body).unwrap());
            let asks_keys = matches!(
                postcard::from_bytes::<runtime_identity::wire::Request>(&body),
                Ok(runtime_identity::wire::Request::ChannelKeys { .. })
            );
            let response = if asks_keys {
                match &keys {
                    Some((generation, keys)) => Response::ChannelKeys {
                        access: runtime_identity::wire::ChannelAccess::Ready,
                        generation: *generation,
                        keys: keys.clone(),
                    },
                    None => Response::ChannelKeys {
                        access: runtime_identity::wire::ChannelAccess::Unavailable,
                        generation: 0,
                        keys: Vec::new(),
                    },
                }
            } else {
                let Some(response) = responses.next() else {
                    return;
                };
                response
            };
            if framing
                .write_value_async(&mut server, &response)
                .await
                .is_err()
            {
                return;
            }
        }
    });
    (client, output)
}

fn host_identity_requests(bytes: &[u8]) -> Vec<runtime_identity::wire::Request> {
    let mut remaining = bytes;
    let mut requests = Vec::new();
    while !remaining.is_empty() {
        if remaining.len() < 4 {
            break;
        }
        let len = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
        if remaining.len() < 4 + len {
            break;
        }
        requests.push(postcard::from_bytes(&remaining[4..4 + len]).unwrap());
        remaining = &remaining[4 + len..];
    }
    requests
}

#[test]
#[cfg(unix)]
fn inherited_broker_identity_delegates_join_keys_and_signing_refusals() {
    use runtime_identity::wire::{FRAMING, Request, Response};
    use std::os::fd::AsFd as _;

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
            Some(Request::CollaborationJoin { workspace_id }) if workspace_id == expected_workspace
        ));
        FRAMING
            .write_value(&mut host, &Response::Signature(vec![7; 64]))
            .unwrap();
        assert!(matches!(
            FRAMING.read_value::<_, Request>(&mut host).unwrap(),
            Some(Request::ChannelKeys { workspace_id, app_id, record_id, known_generation })
                if workspace_id == expected_workspace && app_id == "notes"
                    && record_id == "page" && known_generation == 3
        ));
        FRAMING
            .write_value(
                &mut host,
                &Response::ChannelKeys {
                    access: runtime_identity::wire::ChannelAccess::Removed,
                    generation: 4,
                    keys: Vec::new(),
                },
            )
            .unwrap();
        assert!(matches!(
            FRAMING.read_value::<_, Request>(&mut host).unwrap(),
            Some(Request::CollaborationOp { workspace_id, app_id, record_id, unsigned_op })
                if workspace_id == expected_workspace && app_id == "notes"
                    && record_id == "page" && unsigned_op == b"operation"
        ));
        FRAMING
            .write_value(&mut host, &Response::Denied("role revoked".into()))
            .unwrap();
    });
    let identity = runtime_identity::inherited::connect_service(app.as_fd()).unwrap();
    let joined = sdk_join_identity_from(identity, cluster_id).unwrap();
    assert_eq!(joined.participant_kind, "person");
    let presentation = runtime_collab::CertifiedJoinIdentity::decode(&joined.presentation)
        .unwrap()
        .unwrap();
    assert_eq!(presentation.workspace_id, workspace);
    assert_eq!(presentation.device_certificate, b"fixture");
    assert_eq!(presentation.proof, [7; 64]);
    let SdkDocumentSigner::Certified(signer) = joined.signer else {
        panic!("an inherited identity must retain its broker signer");
    };
    assert_eq!(signer.device_certificate(), Some(b"fixture".as_slice()));
    assert_eq!(signer.signing_public_key(), [5; 32]);
    let keys = signer.channel_keys(&workspace, "notes", "page", 3).unwrap();
    assert_eq!(keys.access, runtime_identity::wire::ChannelAccess::Removed);
    assert_eq!(keys.generation, 4);
    let error = signer
        .sign_collaboration_op(&workspace, "notes", "page", b"operation")
        .unwrap_err();
    match error {
        runtime_portal::NotAvailable::ConnectionFailed(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            assert_eq!(error.to_string(), "role revoked");
        }
        other => panic!("expected the broker refusal, got {other}"),
    }
    worker.join().unwrap();
}

#[test]
fn host_connector_handoff_fails_closed_without_fallback() {
    // Case `host_connector_failure_is_consumed_once_without_inherited_fallback`.
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let cluster_id = Uuid::new_v4();
        let expected = RouteSpec {
            app_id: "so.tilo.notes".into(),
            cluster_id,
            record: Some("note".into()),
        };
        let connector = HostSessionConnector::new(move |route| async move {
            seen.fetch_add(1, Ordering::SeqCst);
            assert_eq!(route, expected);
            assert_eq!(thread::current().name(), Some("ui-collab-shovel"));
            assert!(tokio::runtime::Handle::try_current().is_ok());
            // A !Send value across await proves this future stays on LocalSet.
            let local = Rc::new(());
            tokio::task::yield_now().await;
            drop(local);
            Err::<(tokio::io::DuplexStream, tokio::io::DuplexStream), _>(
                runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture host refused its grant",
                )),
            )
        });
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            cluster_id,
            Some("note".into()),
            None,
        );
        match result {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(error.to_string(), "fixture host refused its grant");
            }
            _ => panic!("host refusal must reach caller"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // Case `host_connector_without_certificate_cannot_use_harness_identity`.
    {
        let connector = HostSessionConnector::new(move |_| async move {
            let (identity, _) = host_identity_fixture(None, vec![]);
            let (stream, _peer) = tokio::io::duplex(4096);
            Ok((stream, identity))
        });
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            Uuid::new_v4(),
            None,
            None,
        );
        match result {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(
                    error.to_string(),
                    "workspace identity has no certified device"
                );
            }
            _ => panic!("missing certificate must refuse before joining"),
        }
    }

    // Case `host_connector_join_proof_refusal_reaches_caller`.
    {
        let connector = HostSessionConnector::new(move |_| async move {
            let (identity, _) = host_identity_fixture(
                Some(b"fixture".to_vec()),
                vec![runtime_identity::wire::Response::Denied(
                    "fixture device revoked".into(),
                )],
            );
            let (stream, _peer) = tokio::io::duplex(4096);
            Ok((stream, identity))
        });
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            Uuid::new_v4(),
            None,
            None,
        );
        match result {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert_eq!(error.to_string(), "fixture device revoked");
            }
            _ => panic!("proof refusal must not become an unsigned join"),
        }
    }

    // Case `host_connector_uses_certified_presentation_and_broker_document_policy`.
    {
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let broker_captures = captures.clone();
        let (requests_tx, requests_rx) = mpsc::sync_channel(1);
        let cluster_id = Uuid::new_v4();
        let channel =
            collab_types::document_channel_id("so.tilo.notes", &cluster_id.to_string(), "note");
        let keys = channel_crypto::ChannelGroup::create(
            channel,
            channel_crypto::MemberKey::generate("device").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap()
        .to_bytes()
        .unwrap()
        .to_vec();
        let connector = HostSessionConnector::new(move |_| async move {
            let (identity, requests) = host_identity_fixture_with_keys(
                Some(b"fixture certificate".to_vec()),
                vec![
                    runtime_identity::wire::Response::Signature(vec![7; 64]),
                    runtime_identity::wire::Response::Denied("fixture role revoked".into()),
                    runtime_identity::wire::Response::Signature(vec![8; 64]),
                ],
                Some((1, keys)),
            );
            requests_tx.send(requests).unwrap();
            let broker = FakeBroker {
                captures: broker_captures,
                ..FakeBroker::default()
            };
            let (client, server) = tokio::io::duplex(64 * 1024);
            tokio::task::spawn_local(serve_fake_broker(server, broker));
            Ok((client, identity))
        });
        let mut manager = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            cluster_id,
            Some("note".into()),
            None,
        )
        .unwrap();
        // Returning a manager must include the existing Hello/primary barrier.
        assert!(manager.is_connected());
        assert!(manager.primary_route.current(true).is_some());
        assert_eq!(manager.state(), CollaborationState::Ready);
        assert_eq!(manager.take_state_change(), Some(CollaborationState::Ready));
        assert_eq!(manager.take_state_change(), None);
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(
            joined[0].0,
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        let presentation = runtime_collab::CertifiedJoinIdentity::decode(&joined[0].1)
            .unwrap()
            .unwrap();
        assert_eq!(presentation.workspace_id, cluster_id.to_string());
        assert_eq!(presentation.device_certificate, b"fixture certificate");
        assert_eq!(presentation.proof, vec![7; 64]);
        let requests = tilo_test_env::recv("the host identity requests", &requests_rx);
        manager.send(CollabRequest::document_incremental(
            b"fixture document change".to_vec(),
        ));
        let is_op = |request: &runtime_identity::wire::Request| {
            matches!(
                request,
                runtime_identity::wire::Request::CollaborationOp { .. }
            )
        };
        tilo_test_env::wait_until(
            "`host_identity_requests(&requests.lock().unwrap()).iter().any(is_op)`",
            || {
                host_identity_requests(&requests.lock().unwrap())
                    .iter()
                    .any(is_op)
            },
        );
        let requests = host_identity_requests(&requests.lock().unwrap());
        assert!(
            matches!(&requests[1], runtime_identity::wire::Request::CollaborationJoin { workspace_id } if workspace_id == &cluster_id.to_string())
        );
        assert!(
            matches!(&requests[2], runtime_identity::wire::Request::ChannelKeys { app_id, record_id, .. } if app_id == "so.tilo.notes" && record_id == "note"),
            "a joined route asks for its channel keys before its first edit"
        );
        let op = requests.iter().find(|request| is_op(request)).unwrap();
        match op {
            runtime_identity::wire::Request::CollaborationOp {
                workspace_id,
                app_id,
                record_id,
                unsigned_op,
            } => {
                assert_eq!(workspace_id, &cluster_id.to_string());
                assert_eq!(app_id, "so.tilo.notes");
                assert_eq!(record_id, "note");
                let op: SignedOp = postcard::from_bytes(unsigned_op).unwrap();
                assert_eq!(
                    op.author,
                    collab_types::Ed25519PublicKey::from_bytes([5; 32])
                );
                assert_ne!(op.payload, b"fixture document change".to_vec());
                assert_eq!(
                    op.encryption.map(|metadata| metadata.channel),
                    Some(channel),
                    "a certified endpoint seals under the route's channel"
                );
            }
            _ => panic!("document must use the broker policy path"),
        }
        // Synchronize behind the mutation: a denied signature cannot be sent
        // through the harness or unsigned path after this readiness barrier.
        manager.await_primary_route().unwrap();
        assert!(captures.lock().unwrap().signed_ops.is_empty());
        assert!(manager.send(CollabRequest::document_incremental(
            b"later allowed change".to_vec()
        )));
        assert_document_publication_failure(&mut manager, "fixture role revoked");
        assert!(
            manager.is_connected(),
            "policy refusal leaves identity transport usable"
        );
        assert_eq!(
            captures.lock().unwrap().signed_ops.len(),
            1,
            "later RPC succeeds without erasing the earlier prefix failure"
        );
    }

    // Case `host_connector_refused_primary_join_is_not_connected_success`.
    {
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let (connector, release) =
            host_join_gate_connector(false, calls.clone(), attempt_tx, stop_tx, || {});
        let cluster_id = Uuid::new_v4();
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            cluster_id,
            Some("note".into()),
            None,
        );
        drop(release);
        match result {
            Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
                assert!(error.to_string().contains("fixture primary join refused"));
            }
            _ => panic!("stream and identity bootstrap must not bypass refused Hello"),
        }
        assert_eq!(
            tilo_test_env::recv("the primary join attempt", &attempt_rx),
            channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            attempt_rx.try_recv().is_err(),
            "must not retry another route"
        );
        tilo_test_env::recv("fixture broker stops after failed manager", &stop_rx);
    }
}

/// A host's handoff ends with the host's own outcome, however late: the
/// worker's clock is paused and runs a minute ahead while the handoff
/// is pending, and the host's reason is what the caller hears.
#[test]
fn host_connector_waits_for_the_hosts_handoff_and_reports_its_reason() {
    struct PendingHandoff(mpsc::Sender<()>);
    impl Drop for PendingHandoff {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    let (minute_tx, minute_rx) = mpsc::sync_channel(1);
    let (fail_tx, fail_rx) = tokio::sync::oneshot::channel::<()>();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let connector = HostSessionConnector::new(move |_| async move {
        let _handoff = PendingHandoff(dropped_tx);
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let _ = minute_tx.send(());
        });
        let _ = fail_rx.await;
        Err::<(tokio::io::DuplexStream, tokio::io::DuplexStream), _>(
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::other(
                "the host could not reach its broker",
            )),
        )
    })
    .on_paused_clock();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let caller = thread::spawn(move || {
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            Uuid::new_v4(),
            None,
            None,
        );
        let _ = result_tx.send(result.map(drop));
    });
    tilo_test_env::recv(
        "a minute to pass on the worker's clock while the handoff is pending",
        &minute_rx,
    );
    assert!(
        matches!(result_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the caller still waits for the host"
    );
    fail_tx.send(()).unwrap();
    let result = tilo_test_env::recv("the host's outcome to reach the caller", &result_rx);
    caller.join().unwrap();
    match result {
        Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
            assert!(
                error
                    .to_string()
                    .contains("the host could not reach its broker"),
                "the caller hears the host's reason: {error}"
            );
        }
        _ => panic!("a failed handoff cannot produce a connected session"),
    }
    tilo_test_env::recv(
        "the failed handoff releases its owned resources",
        &dropped_rx,
    );
}

/// A primary join waits for the broker's answer, however late. The
/// worker's clock is paused and runs a minute ahead while the broker
/// holds the join, far past any deadline a clock could have set; the
/// broker's answer then opens the manager. Nothing is retried meanwhile.
#[test]
fn a_primary_join_waits_for_a_late_broker_answer() {
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (minute_tx, minute_rx) = mpsc::sync_channel(1);
    let (connector, release) =
        host_join_gate_connector(true, calls.clone(), attempt_tx, stop_tx, move || {
            // On the worker's paused clock: a minute once the broker
            // holds the join.
            tokio::task::spawn_local(async move {
                if entered_rx.await.is_ok() {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let _ = minute_tx.send(());
                }
            });
        });
    let connector = connector.on_paused_clock();
    let cluster_id = Uuid::new_v4();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let caller = thread::spawn(move || {
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            cluster_id,
            Some("note".into()),
            None,
        );
        let _ = result_tx.send(result.map(|manager| {
            manager
                .primary_route()
                .map(|witness| witness.route.record.clone())
        }));
    });
    assert_eq!(
        tilo_test_env::recv("the primary join attempt", &attempt_rx),
        channel_key::format("so.tilo.notes", &cluster_id.to_string(), "note")
    );
    entered_tx.send(()).unwrap();
    tilo_test_env::recv(
        "a minute to pass on the worker's clock while the broker holds the join",
        &minute_rx,
    );
    assert!(
        matches!(result_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the join still waits for the broker's answer"
    );
    drop(release);
    let joined = tilo_test_env::recv("the broker's late answer", &result_rx);
    caller
        .join()
        .expect("the caller returns once the broker answered");
    match joined {
        Ok(route) => assert_eq!(route, Some(Some("note".into()))),
        Err(error) => panic!("a late answer opens the manager, not {error}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        attempt_rx.try_recv().is_err(),
        "must not retry another route"
    );
    tilo_test_env::recv("the fixture broker stops once the manager closes", &stop_rx);
}

/// A primary join the broker fails ends the caller's wait with the
/// broker's reason as soon as it fails, after any delay.
#[test]
fn a_failed_primary_join_reports_its_reason_at_once() {
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (minute_tx, minute_rx) = mpsc::sync_channel(1);
    let (connector, release) =
        host_join_gate_connector(true, calls.clone(), attempt_tx, stop_tx, move || {
            tokio::task::spawn_local(async move {
                if entered_rx.await.is_ok() {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let _ = minute_tx.send(());
                }
            });
        });
    let connector = connector.on_paused_clock();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let caller = thread::spawn(move || {
        let result = CollaborationManager::for_route_host_blocking(
            connector,
            "so.tilo.notes",
            Uuid::new_v4(),
            Some("note".into()),
            None,
        );
        let _ = result_tx.send(result.map(drop));
    });
    tilo_test_env::recv("the primary join attempt", &attempt_rx);
    entered_tx.send(()).unwrap();
    tilo_test_env::recv(
        "a minute to pass on the worker's clock while the broker holds the join",
        &minute_rx,
    );
    // The broker fails the join it held.
    release.fail();
    let result = tilo_test_env::recv("the failed join to reach the caller", &result_rx);
    caller.join().unwrap();
    match result {
        Err(runtime_portal::NotAvailable::ConnectionFailed(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
            assert!(
                error.to_string().contains("fixture gate dropped"),
                "the caller hears the broker's reason: {error}"
            );
        }
        _ => panic!("a failed join never returns a connected manager"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        attempt_rx.try_recv().is_err(),
        "must not retry another route"
    );
    tilo_test_env::recv(
        "the fixture broker stops after the failed manager",
        &stop_rx,
    );
}

// The successful fake host, changing only its join answer: the delayed
// case joins as it does once released.
struct HostJoinGate {
    release: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
    attempted: mpsc::Sender<String>,
    stopped: mpsc::Sender<()>,
}

impl Drop for HostJoinGate {
    fn drop(&mut self) {
        let _ = self.stopped.send(());
    }
}

impl HostJoinGate {
    /// Hold the join of `route` until released; refuse it at once when
    /// nothing will release it.
    fn attempt(&self, route: &str) -> Pin<Box<dyn Future<Output = Result<(), String>>>> {
        self.attempted.send(route.to_owned()).unwrap();
        let release = self.release.borrow_mut().take();
        Box::pin(async move {
            release
                .ok_or("fixture primary join refused")?
                .await
                .map_err(|_| "fixture gate dropped".to_owned())
        })
    }
}

// Releasing on unwind also lets the worker finish if an assertion fails.
struct HostJoinRelease(Option<tokio::sync::oneshot::Sender<()>>);
impl HostJoinRelease {
    /// Fail the held join instead of answering it.
    fn fail(mut self) {
        drop(self.0.take());
    }
}
impl Drop for HostJoinRelease {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

/// `on_worker` runs on the manager's worker as it connects.
fn host_join_gate_connector(
    delayed: bool,
    calls: Arc<AtomicUsize>,
    attempted: mpsc::Sender<String>,
    stopped: mpsc::Sender<()>,
    on_worker: impl FnOnce() + Send + 'static,
) -> (HostSessionConnector, HostJoinRelease) {
    let (release, wait) = tokio::sync::oneshot::channel();
    let connector = HostSessionConnector::new(move |_| async move {
        calls.fetch_add(1, Ordering::SeqCst);
        on_worker();
        let (identity, _) = host_identity_fixture(
            Some(b"fixture certificate".to_vec()),
            vec![runtime_identity::wire::Response::Signature(vec![7; 64])],
        );
        let (client, server) = tokio::io::duplex(64 * 1024);
        // The broker is another process in production: serve it on its
        // own thread, so it ends only when the manager closes the
        // connection, never because the manager's worker ended.
        thread::Builder::new()
            .name("ui-collab-test-join-gate".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let gate = HostJoinGate {
                    release: RefCell::new(delayed.then_some(wait)),
                    attempted,
                    stopped,
                };
                LocalSet::new().block_on(
                    &runtime,
                    serve_fake_host(server, FakeBroker::default(), Some(gate)),
                );
            })
            .unwrap();
        Ok((client, identity))
    });
    (connector, HostJoinRelease(Some(release)))
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
            presentation: harness_presentation(),
            participant_kind: "person",
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
        mutation: collab_types::DocumentMutation::Snapshot(b"committed worker mutation".to_vec()),

        authorization: None,
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
    let primary = RouteSpec::record("so.tilo.notes", workspace, "tab:0000000000000001");
    let beside = RouteSpec::record(
        "so.tilo.chat",
        workspace,
        collab_types::workspace_record_id("tilo.workspace.emoji"),
    );
    let (mut manager, _) = CollaborationManager::native_initial_named_invocation_fixture_for_test(
        primary.clone(),
        automerge::AutoCommit::new().save(),
        std::slice::from_ref(&beside),
    );
    assert_eq!(manager.route_join_failure(&beside), None);
    manager.join_route(
        beside.clone(),
        crate::record::RecordKind::new("tilo.rich-text"),
    );
    tilo_test_env::wait_until("the refused join to say why", || {
        manager.route_join_failure(&beside).is_some()
    });
    let reason = manager.route_join_failure(&beside).unwrap();
    assert!(
        reason.contains("committed read custody refused for this route"),
        "{reason}"
    );
    assert_eq!(
        manager.route_join_failure(&primary),
        None,
        "the window's own route joined"
    );
}

/// A peer may answer one commit with a definite refusal — it read the
/// change and will not take it, here because the record it would create
/// was named without its kind. That is not a lost connection: the bridge
/// stays open, the app stays published and reachable, and the very next
/// change goes through.
#[test]
fn a_refused_document_commit_fails_only_that_change() {
    let broker = FakeBroker::default();
    let (mut manager, captured, thread) = committed_route_over_fake_broker(broker);
    captured.lock().unwrap().refuse_documents = 1;
    assert!(manager.send(CollabRequest::document_incremental(
        b"refused change".to_vec()
    )));
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
    assert!(manager.send(CollabRequest::document_incremental(
        b"later change".to_vec()
    )));
    let later = manager.begin_document_publication_barrier().unwrap();
    assert_eq!(later.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    assert_eq!(captured.lock().unwrap().signed_ops.len(), 1);
    assert!(manager.is_connected());
    drop(manager);
    thread.join().unwrap();
}

/// The outcome-unknown half of the same path: a commit call that never
/// got an answer still ends collaboration, as it always has.
#[test]
fn a_document_commit_whose_outcome_is_unknown_still_closes_the_bridge() {
    let broker = FakeBroker::default();
    let (mut manager, captured, thread) = committed_route_over_fake_broker(broker);
    captured.lock().unwrap().fail_document = true;
    assert!(manager.send(CollabRequest::document_incremental(
        b"unknown change".to_vec()
    )));
    tilo_test_env::wait_until("`!manager.is_connected()`", || !manager.is_connected());
    assert!(
        !format!("{:?}", manager.document_publication_failure()).contains("Refused"),
        "an unknown outcome is not a refusal"
    );
    assert_eq!(manager.refused_change(), None);
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
    let mut gates = Vec::new();
    for _ in 0..2 {
        let (entered, observed) = mpsc::channel();
        let (release, receiver) = tokio::sync::oneshot::channel();
        captures
            .lock()
            .unwrap()
            .document_gates
            .push_back(DocumentSendGate {
                entered,
                release: receiver,
            });
        // Dropping the sender on unwind releases the fake RPC with Err.
        gates.push((observed, release));
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
        record: Some("second".into()),
    };
    manager.join_route(
        secondary.clone(),
        crate::record::RecordKind::new("tilo.rich-text"),
    );
    tilo_test_env::wait_until(
        "`broker_state(&manager).document_reads.publishable(&secondary.channel_id())`",
        || {
            broker_state(&manager)
                .document_reads
                .publishable(&secondary.channel_id())
        },
    );
    assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
    let first = manager.begin_document_publication_barrier().unwrap();
    assert!(
        manager
            .try_send_to_route(
                secondary,
                CollabRequest::document_incremental(b"second".to_vec())
            )
            .is_ok()
    );
    let second = manager.begin_document_publication_barrier().unwrap();
    let mut gates = gates.into_iter();
    let (entered, release) = gates.next().unwrap();
    tilo_test_env::recv("the gated document RPC", &entered);
    assert_eq!(first.try_complete(), Ok(None));
    assert_eq!(second.try_complete(), Ok(None));
    release.send(Ok(())).unwrap();
    let (entered, release) = gates.next().unwrap();
    tilo_test_env::recv("the gated document RPC", &entered);
    // The second RPC can only begin after the first marker has resolved.
    assert_eq!(first.wait(Some(Instant::now())), Ok(()));
    assert_eq!(second.try_complete(), Ok(None));
    release.send(Ok(())).unwrap();
    assert_eq!(second.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    assert_eq!(captures.lock().unwrap().signed_ops.len(), 2);
    assert!(manager.is_connected());
    drop(manager);
    thread.join().unwrap();
}

/// A burst of edits many times the outbound queue must be absorbed by
/// backpressure: the app is told `Busy` and retries, the channel stays
/// up, and every edit reaches the broker exactly once, in order.
#[test]
fn a_flood_of_edits_is_backpressured_not_fatal() {
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures::default())),
        op_backlog: vec![],
        presence_backlog: vec![],
        context_backlog: vec![],
        scrollback_response: None,
        terminate_context_stream: false,
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    let total = SDK_COMMAND_QUEUE * 3;
    let mut busy = 0usize;
    for n in 0..total {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match manager.try_send(CollabRequest::document_incremental(
                format!("edit-{n}").into_bytes(),
            )) {
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
    assert_eq!(captures.lock().unwrap().signed_ops.len(), total);
    assert!(manager.send(CollabRequest::document_incremental(b"after".to_vec())));
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
        media: None,
        inbound_rx,
        document_turns: Vec::new(),
        invocation_owners: std::collections::BTreeMap::new(),
        state: BridgeState::new(),
        action_directory_rx,
        action_directory_tx,
        pending_primary: None,
        worker_stopped: false,
        command_bridge_registered: false,
        spill: std::collections::VecDeque::new(),
        barrier_slot: None,
        queued_barrier: std::sync::Weak::new(),
        _thread: None,
    };
    (transport, outbound_rx)
}

/// A barrier submitted while the queue is full goes into the slot held
/// for it, behind every earlier edit. Parked in the spill, it would move
/// only when the owner next called the transport, and an owner blocked
/// on the receipt never does. A second barrier with no edit between
/// shares the first one's answer.
#[test]
fn a_barrier_behind_a_full_queue_is_queued_not_parked() {
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

/// A burst of durable commands, more than the queue holds by count and
/// by bytes, waits for the shovel instead of disabling collaboration,
/// and reaches it in submission order.
#[test]
fn a_durable_burst_waits_for_the_shovel_instead_of_disabling_collaboration() {
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

/// A single command larger than the whole outbound byte budget can never
/// fit, so it is refused to its sender, and the bridge stays up for
/// everything else.
#[test]
fn a_command_larger_than_the_whole_budget_is_refused_without_closing_the_bridge() {
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

/// Commands behind a parked one stay behind it, and a newer action surface
/// replaces an older one still waiting: it is level-triggered, so a burst
/// of republications is one parked surface, not a queue of large ones.
#[test]
fn parked_surfaces_coalesce_and_document_changes_wait_behind_parked_commands() {
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

/// A caret that moves while typing fills the queue waits for room rather than
/// being dropped, and only the newest word under its key is said: the last
/// caret of a burst is the one everyone sees.
#[test]
fn the_last_caret_of_a_burst_waits_for_room_and_is_the_one_said() {
    let (mut transport, mut shovel) = parking_transport(1);
    let caret = |pos: usize| {
        TransportCommand::Primary(CollabRequest::Presence {
            key: PresenceKey::Tab {
                id: "note".to_owned(),
            },
            payload: PresencePayload::Caret { pos },
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
    let Some(SdkCommand::Primary(CollabRequest::Presence {
        payload: PresencePayload::Caret { pos },
        ..
    })) = said.last()
    else {
        panic!("the caret is said once there is room: {}", said.len());
    };
    assert_eq!(*pos, 50, "the caret said is where the burst ended");
    assert!(transport.spill.is_empty());
}

#[cfg(unix)]
struct RefusedDocumentIdentity;

/// Keys for a fresh channel group, as a ready identity service answers.
#[cfg(unix)]
fn ready_channel_keys() -> runtime_identity::wire::ChannelKeysReply {
    let group = channel_crypto::ChannelGroup::create(
        collab_types::ChannelId::new(),
        channel_crypto::MemberKey::generate("device").unwrap(),
    )
    .unwrap();
    runtime_identity::wire::ChannelKeysReply {
        access: runtime_identity::wire::ChannelAccess::Ready,
        generation: 1,
        keys: Some(group.content_keys().unwrap().to_bytes().unwrap().to_vec()),
    }
}

#[cfg(unix)]
impl SdkCertifiedIdentity for RefusedDocumentIdentity {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        // Admitted to the channel, so the edit reaches the signer.
        Ok(ready_channel_keys())
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        [5; 32]
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([7; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "certified signing refused",
        )
        .into())
    }
}

/// A certified identity whose service holds keys for every channel, and
/// counts how often an endpoint asks for them.
#[cfg(unix)]
struct KeyedIdentity {
    asked: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(unix)]
impl SdkCertifiedIdentity for KeyedIdentity {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(ready_channel_keys())
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        [5; 32]
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([7; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([9; 64])
    }
}

/// A certified identity whose channel service answers every route with
/// `access`, or that refuses every question (`None`).
#[cfg(unix)]
struct AnsweringIdentity {
    asked: Arc<std::sync::atomic::AtomicUsize>,
    access: Option<runtime_identity::wire::ChannelAccess>,
}

#[cfg(unix)]
impl SdkCertifiedIdentity for AnsweringIdentity {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.access
            .map(|access| runtime_identity::wire::ChannelKeysReply {
                access,
                generation,
                keys: None,
            })
            .ok_or(runtime_portal::NotAvailable::ServiceFdMissing)
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        [5; 32]
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([7; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([9; 64])
    }
}

#[cfg(unix)]
include!("held_outbound_tests.rs");

/// An endpoint on a pushing broker whose identity service answers its
/// one route with `access` (or refuses, `None`): asked again on the
/// timer (`on_timer`), or left with its timer disarmed and unasked. A
/// pushed change must still make it ask afterwards.
#[cfg(unix)]
#[cfg(test)]
fn asks_while_waiting(access: Option<runtime_identity::wire::ChannelAccess>, on_timer: bool) {
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
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(AnsweringIdentity {
                asked: Arc::clone(&asked),
                access,
            })),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: "so.tilo.test".into(),
        cluster_id: Uuid::new_v4(),
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
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

#[test]
#[cfg(unix)]
fn a_member_waiting_for_keys_shows_the_cleartext_the_broker_admitted() {
    let app_id = "so.tilo.test";
    let cluster = Uuid::new_v4();
    let artifact = collab_types::document_artifact_id(app_id, &cluster.to_string(), "");
    let op = |seq, text: &[u8], admission| {
        let op = SignedOp::new_unsigned(
            DocumentMutation::Incremental(text.to_vec())
                .encode()
                .unwrap(),
            artifact,
            collab_types::Ed25519PublicKey::from_bytes([3; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            seq,
        );
        (
            postcard::to_stdvec(&op).unwrap(),
            "host".to_owned(),
            admission,
        )
    };
    let broker = FakeBroker {
        op_backlog: vec![
            op(1, b"general", OpAdmission::CleartextAdmitted),
            op(2, b"must be sealed", OpAdmission::EncryptionRequired),
            op(3, b"random", OpAdmission::CleartextAdmitted),
        ],
        ..FakeBroker::default()
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(AnsweringIdentity {
                asked: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                access: Some(runtime_identity::wire::ChannelAccess::Waiting),
            })),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: app_id.into(),
        cluster_id: cluster,
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
    let shown = |events: &[CollabEvent]| -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|event| match event.document_mutation() {
                Some(Ok(DocumentMutation::Incremental(text))) => Some(text),
                _ => None,
            })
            .collect()
    };
    let events = poll_until(&mut manager, |events| shown(events).len() == 2);
    assert_eq!(shown(&events), [b"general".to_vec(), b"random".to_vec()]);
    drop(manager);
    thread.join().unwrap();
}

/// A route the channel service answered as waiting, or as unavailable
/// (its workspace's governance unknown yet), is told when that changes: it is not asked
/// about on a timer while the broker pushes, and a pushed change asks.
#[test]
#[cfg(unix)]
fn a_route_the_channel_service_answered_waits_for_its_push() {
    use runtime_identity::wire::ChannelAccess;
    for access in [ChannelAccess::Waiting, ChannelAccess::Unavailable] {
        asks_while_waiting(Some(access), false);
    }
}

/// What nothing pushes is still asked on the timer: a question the
/// identity service refused, and a removal, whose rejoin each question
/// asks for again.
#[test]
#[cfg(unix)]
fn a_refused_question_or_a_removal_is_still_asked_on_the_timer() {
    for access in [None, Some(runtime_identity::wire::ChannelAccess::Removed)] {
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
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(KeyedIdentity {
                asked: Arc::clone(&asked),
            })),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: "so.tilo.test".into(),
        cluster_id: Uuid::new_v4(),
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
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

/// A certified identity service that answers its first question at
/// once, with `keys`, and holds every later one until `released`.
#[cfg(unix)]
struct SlowIdentity {
    keys: Vec<u8>,
    asked: std::sync::atomic::AtomicUsize,
    released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(unix)]
impl SlowIdentity {
    fn release(released: &(std::sync::Mutex<bool>, std::sync::Condvar)) {
        *released.0.lock().unwrap() = true;
        released.1.notify_all();
    }
}

#[cfg(unix)]
impl SdkCertifiedIdentity for SlowIdentity {
    fn channel_keys(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        let first = self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0;
        if !first {
            let (released, changed) = &*self.released;
            let _released = changed
                .wait_timeout_while(
                    released.lock().unwrap(),
                    tilo_test_env::hang_bound(),
                    |released| !*released,
                )
                .unwrap();
        }
        Ok(runtime_identity::wire::ChannelKeysReply {
            access: runtime_identity::wire::ChannelAccess::Ready,
            generation: 1,
            keys: first.then(|| self.keys.clone()),
        })
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        [5; 32]
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([7; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([9; 64])
    }
}

/// An endpoint keeps receiving while its identity service is slow to
/// answer a key refresh: an op that arrives during the refresh is
/// delivered while the service still has not answered.
#[test]
#[cfg(unix)]
fn a_slow_identity_service_never_holds_up_incoming_ops() {
    // The op arrives after the first refresh tick, which the identity
    // service holds until the op has been delivered.
    const ARRIVES: Duration = Duration::from_millis(1500);
    let app_id = "so.tilo.test";
    let cluster = Uuid::new_v4();
    let workspace = cluster.to_string();
    let channel = collab_types::document_channel_id(app_id, &workspace, "");
    let artifact = collab_types::document_artifact_id(app_id, &workspace, "");
    let group = channel_crypto::ChannelGroup::create(
        channel,
        channel_crypto::MemberKey::generate("device").unwrap(),
    )
    .unwrap();
    let frame = collab_types::DocumentMutation::Incremental(b"remote change".to_vec())
        .encode()
        .unwrap();
    let (metadata, ciphertext) = group
        .seal_content(
            artifact,
            collab_types::ContentRetention::of_frame(&frame),
            &frame,
        )
        .unwrap();
    let op = SignedOp::new_unsigned_with_encryption(
        ciphertext,
        artifact,
        collab_types::Ed25519PublicKey::from_bytes([3; 32]),
        collab_types::AttestationId::new(),
        collab_types::OpKind::Change,
        crate::time::SystemTime::UNIX_EPOCH,
        1,
        metadata,
    );
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            op_stream_delay: Some(ARRIVES),
            ..Captures::default()
        })),
        op_backlog: vec![(
            postcard::to_stdvec(&op).unwrap(),
            "peer".to_owned(),
            OpAdmission::EncryptionRequired,
        )],
        ..FakeBroker::default()
    };
    let (fd, _captures, thread) = stand_up_fake_broker(broker);
    let released = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(SlowIdentity {
                keys: group.content_keys().unwrap().to_bytes().unwrap().to_vec(),
                asked: std::sync::atomic::AtomicUsize::new(0),
                released: Arc::clone(&released),
            })),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: app_id.into(),
        cluster_id: cluster,
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
    let events = poll_until(&mut manager, |events| {
        events
            .iter()
            .any(|event| matches!(event, CollabEvent::Document { .. }))
    });
    SlowIdentity::release(&released);
    assert!(
        events.iter().any(|event| matches!(
            event,
            CollabEvent::Document {
                decrypted_payload: Some(payload),
                ..
            } if *payload == frame
        )),
        "the op must arrive, opened, while the key refresh is held: {events:?}"
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

/// A certified identity service that, like the real one, refuses channel
/// keys for a route that names no app (a cluster route) and answers a
/// real app route with keys. Counts every question.
#[cfg(unix)]
struct ClusterRefusingIdentity {
    asked: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(unix)]
impl SdkCertifiedIdentity for ClusterRefusingIdentity {
    fn channel_keys(
        &self,
        _: &str,
        app_id: &str,
        _: &str,
        _: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if app_id.is_empty() {
            // The identity service refuses a route of no app.
            return Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "channel keys requested for a different launch route",
                ),
            ));
        }
        Ok(ready_channel_keys())
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        Some(b"fixture")
    }
    fn signing_public_key(&self) -> [u8; 32] {
        [5; 32]
    }
    fn sign_collaboration_join(&self, _: &str) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([7; 64])
    }
    fn sign_collaboration_op(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        Ok([9; 64])
    }
}

/// A cluster-scope route (an empty app id, the workspace-wide discovery
/// route an agent joins) names no app, so the identity service refuses
/// it channel keys — and a route left waiting for an answer it cannot get
/// is asked again every second by the key timer for the whole life of the
/// connection. It carries no record and writes nothing, so it asks for no
/// keys at all and the timer never names it.
///
/// The broker here pushes channel changes, so the timer asks nothing once
/// every route either holds its keys or is plaintext. Before the join the
/// app's own route settles to holding its keys, which disarms the timer;
/// after a cluster route joins, the timer must stay disarmed and no more
/// question may be asked. Pre-fix the cluster route waits for keys it can
/// never get, which arms the timer and asks it every round.
#[test]
#[cfg(unix)]
fn a_cluster_route_asks_for_no_channel_keys_and_polls_nothing() {
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
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(ClusterRefusingIdentity {
                asked: Arc::clone(&asked),
            })),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: "so.tilo.test".into(),
        cluster_id: Uuid::new_v4(),
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
    // The app's own route learns its keys at its join round and holds
    // them; the timer is then disarmed (a pushing broker asks nothing
    // that holds keys and nothing that is plaintext).
    tilo_test_env::wait_until("the app route's join round to be applied", || {
        !key_schedule(&manager).armed
    });
    let settled = asked.load(std::sync::atomic::Ordering::Acquire);
    assert!(settled > 0, "the app's own route asked for its keys");
    manager.join_route(
        RouteSpec::cluster(Uuid::new_v4()),
        crate::record::RecordKind::new(""),
    );
    // The join's key round, were there one, has run before this probe is
    // answered (the shovel serves its commands in order, and the probe
    // applies any round still out). A cluster route asks nothing, so the
    // timer stays disarmed and no route was asked for keys.
    let seen = key_schedule(&manager);
    assert!(
        !seen.armed && seen.timer_rounds == 0,
        "a cluster route leaves the key timer armed to ask forever: {seen:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::Acquire),
        settled,
        "a cluster route asked the identity service for channel keys it cannot be granted"
    );
    drop(manager);
    thread.join().unwrap();
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
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(KeyedIdentity {
                asked: Arc::clone(&asked),
            })),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: "so.tilo.test".into(),
        cluster_id: Uuid::new_v4(),
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
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
fn document_publication_certified_signing_refusal_never_reports_success() {
    let broker = FakeBroker::default();
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let sdk = CollaborationManager::spawn_sdk(
        SdkConnectSource::Fd(fd),
        Some(SdkJoinIdentity {
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: SdkDocumentSigner::Certified(Arc::new(RefusedDocumentIdentity)),
        }),
    )
    .unwrap();
    let mut manager = CollaborationManager::disabled();
    manager.backend = Backend::Transport(Box::new(sdk));
    assert!(manager.send(CollabRequest::Hello {
        app_id: "worker".into(),
        cluster_id: Uuid::new_v4(),
        record: None,
        record_kind: None,
        handle: None,
    }));
    manager.await_primary_route().unwrap();
    let route = manager.primary_route().unwrap().route.clone();
    let ready = broker_state(&manager)
        .document_reads
        .close_for_test(&route.channel_id());
    let before = publication_gate_waits(&broker_state(&manager));
    assert!(
        manager
            .try_send_to_route(
                route,
                CollabRequest::document_incremental(b"refused".to_vec())
            )
            .is_ok()
    );
    let pending = manager.begin_document_publication_barrier().unwrap();
    wait_held_at_publication_gate(&broker_state(&manager), before);
    assert_eq!(
        pending.try_complete(),
        Ok(None),
        "certified publication must still await exact route readiness"
    );
    assert!(captures.lock().unwrap().signed_ops.is_empty());
    broker_state(&manager).document_reads.open_for_test(&ready);
    // Signing may fail before the marker enqueue; either path must carry the cause.
    let error = match manager.begin_document_publication_barrier() {
        Ok(receipt) => receipt
            .wait(Some(tilo_test_env::hang_deadline()))
            .unwrap_err(),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("certified signing refused"),
        "{error}"
    );
    assert!(captures.lock().unwrap().signed_ops.is_empty());
    assert!(!manager.is_connected());
    assert!(
        format!("{:?}", manager.document_publication_failure()).contains("Refused"),
        "a refusal is told apart from a lost connection"
    );
    drop(manager);
    thread.join().unwrap();
}

#[test]
#[cfg(unix)]
fn committed_queued_publication_retains_payload_and_credits_until_gate_reopens() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (entered, observed) = mpsc::channel();
    let (release, receiver) = tokio::sync::oneshot::channel();
    captures
        .lock()
        .unwrap()
        .document_gates
        .push_back(DocumentSendGate {
            entered,
            release: receiver,
        });
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
    tilo_test_env::recv("the gated document RPC", &observed);
    assert!(manager.send(CollabRequest::document_incremental(
        b"retained original".to_vec()
    )));
    let state = broker_state(&manager);
    let key = manager.primary_route().unwrap().route.channel_id();
    let ready = state.document_reads.close_for_test(&key);
    let receipt = manager.begin_document_publication_barrier().unwrap();
    let before = publication_gate_waits(&state);
    release.send(Ok(())).unwrap();
    tilo_test_env::wait_until("`captures.lock().unwrap().signed_ops.len() == 1`", || {
        captures.lock().unwrap().signed_ops.len() == 1
    });
    wait_held_at_publication_gate(&state, before);
    assert_eq!(
        receipt.try_complete(),
        Ok(None),
        "accepted publication must hold the barrier while hydration is pending"
    );
    assert!(state.document_command_bytes.0.load(Ordering::Acquire) >= b"retained original".len());
    state.document_reads.open_for_test(&ready);
    assert_eq!(receipt.wait(Some(tilo_test_env::hang_deadline())), Ok(()));
    let captured = captures.lock().unwrap().signed_ops.clone();
    assert_eq!(captured.len(), 2);
    let envelope: SignedOp = postcard::from_bytes(&captured[1]).unwrap();
    assert_eq!(
        collab_types::DocumentMutation::decode(&envelope.payload).unwrap(),
        collab_types::DocumentMutation::Incremental(b"retained original".to_vec())
    );
    drop(manager);
    thread.join().unwrap();
}

#[test]
#[cfg(unix)]
fn committed_queued_publication_deadline_is_terminal() {
    queued_publication_terminal_case(false);
}

#[test]
#[cfg(unix)]
fn committed_queued_publication_invalidation_is_terminal() {
    queued_publication_terminal_case(true);
}

#[cfg(unix)]
fn queued_publication_terminal_case(invalidate: bool) {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (entered, observed) = mpsc::channel();
    let (release, receiver) = tokio::sync::oneshot::channel();
    captures
        .lock()
        .unwrap()
        .document_gates
        .push_back(DocumentSendGate {
            entered,
            release: receiver,
        });
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, captures, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
    tilo_test_env::recv("the gated document RPC", &observed);
    assert!(manager.send(CollabRequest::document_incremental(b"must replay".to_vec())));
    let state = broker_state(&manager);
    let ready = state
        .document_reads
        .close_for_test(&manager.primary_route().unwrap().route.channel_id());
    let receipt = manager.begin_document_publication_barrier().unwrap();
    release.send(Ok(())).unwrap();
    tilo_test_env::wait_until("`captures.lock().unwrap().signed_ops.len() == 1`", || {
        captures.lock().unwrap().signed_ops.len() == 1
    });
    if invalidate {
        state.fail_closed("actual test worker invalidation");
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
    assert_eq!(captures.lock().unwrap().signed_ops.len(), 1);
    tilo_test_env::wait_until(
        "`state.command_bytes.0.load(Ordering::Acquire) == 0`",
        || state.command_bytes.0.load(Ordering::Acquire) == 0,
    );
    drop(manager);
    thread.join().unwrap();
}

/// A record's route that does not open in time for its changes (a
/// person's second device waits for another of theirs to hand it the
/// keys of their own drafts) holds back that record alone: the barrier
/// says its change did not go, the app and its other routes stay
/// published, and the owner hears it should send the record whole once
/// the route opens. It used to stop the app's whole collaboration.
#[test]
#[cfg(unix)]
fn a_record_route_not_open_in_time_holds_back_only_that_record() {
    let (fd, captures, thread) = stand_up_fake_broker(FakeBroker::default());
    let workspace = Uuid::new_v4();
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.chat", workspace);
    let drafts = RouteSpec {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace,
        record: Some("member:ada:chat.drafts".into()),
    };
    manager.join_route(
        drafts.clone(),
        crate::record::RecordKind::new("chat.drafts"),
    );
    eventually("the drafts route to join", || {
        manager.document_route_publishable(&drafts)
    });
    // Its committed history is still to be read: the route is closed.
    let _closed = broker_state(&manager)
        .document_reads
        .close_for_test(&drafts.channel_id());
    assert!(
        manager
            .try_send_to_route(
                drafts.clone(),
                CollabRequest::document_incremental(b"draft".to_vec())
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
    let before = captures.lock().unwrap().signed_ops.len();
    assert!(manager.send(CollabRequest::document_incremental(b"own".to_vec())));
    manager
        .begin_document_publication_barrier()
        .unwrap()
        .wait(Some(tilo_test_env::hang_deadline()))
        .unwrap();
    assert_eq!(
        captures.lock().unwrap().signed_ops.len(),
        before + 1,
        "the app's own route still publishes"
    );
    drop(manager);
    thread.join().unwrap();
}

/// A commit RPC the peer answers with a refusal is that change's own
/// failure: the barrier reports it and the bridge, which carried the
/// answer, keeps the app published. (An RPC whose outcome is unknown is
/// the closing case; see
/// `a_document_commit_whose_outcome_is_unknown_still_closes_the_bridge`.)
#[test]
#[cfg(unix)]
fn a_refused_document_rpc_fails_the_barrier_without_closing() {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let (entered, observed) = mpsc::channel();
    let (release, receiver) = tokio::sync::oneshot::channel();
    captures
        .lock()
        .unwrap()
        .document_gates
        .push_back(DocumentSendGate {
            entered,
            release: receiver,
        });
    let broker = FakeBroker {
        captures,
        ..FakeBroker::default()
    };
    let (fd, _, thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "worker", Uuid::new_v4());
    assert!(manager.send(CollabRequest::document_incremental(b"failed".to_vec())));
    tilo_test_env::recv("the gated document RPC", &observed);
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

/// A refusal belongs to the record it was about: another record's change
/// going through leaves it standing, its own record's next change ends it.
#[test]
fn a_refused_change_stands_until_its_own_record_takes_a_change() {
    let route = |record: &str| RouteSpec {
        app_id: "so.tilo.notes".into(),
        cluster_id: uuid::Uuid::from_bytes([7; 16]),
        record: Some(record.into()),
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

#[test]
fn document_publication_unavailable_and_rejected_markers_are_errors() {
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
        media: None,
        inbound_rx,
        document_turns: Vec::new(),
        invocation_owners: std::collections::BTreeMap::new(),
        state,
        action_directory_rx,
        action_directory_tx,
        pending_primary: None,
        worker_stopped: false,
        command_bridge_registered: false,
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

/// A receipt wired to the bridge's latched failure, as the transport builds it.
fn bridge_receipt(
    receiver: mpsc::Receiver<Result<(), DocumentPublicationError>>,
    state: &BridgeState,
) -> DocumentPublicationReceipt {
    let state = state.clone();
    DocumentPublicationReceipt::new(receiver)
        .with_disconnect_cause(move || state.publication_result().err())
}

#[test]
fn document_publication_completed_prefix_is_not_poisoned_by_later_failure() {
    let state = BridgeState::new();
    let (reply, receiver) = mpsc::sync_channel(1);
    let receipt = bridge_receipt(receiver, &state);
    reply.send(state.publication_result()).unwrap();
    state.record_publication_failure(DocumentPublicationError::Failed("later".into()));
    assert_eq!(receipt.wait(Some(Instant::now())), Ok(()));
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

#[test]
fn an_app_that_stopped_reading_ends_collaboration_without_an_overload_error() {
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

fn remote_update(index: u128) -> CollabEvent {
    CollabEvent::RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage::Update {
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(index)),
            state: None,
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

/// A thousand durable events arrive while the app reads none: the
/// stream producing them waits for room, the app then reads every one
/// in order, and collaboration stays up throughout.
#[test]
fn a_burst_of_events_the_app_is_behind_on_waits_for_it_and_keeps_collaboration() {
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

/// Answers to the app's own requests never wait on the app (it may be
/// waiting for the task answering), yet none is lost or reordered when
/// it is behind: they queue in the reply lane until it reads again.
#[test]
fn answers_the_app_is_behind_on_wait_in_order_without_blocking_their_sender() {
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

/// Only an answer about the request ends collaboration for one change:
/// a peer saying it will not take this change is not a lost connection,
/// while a lost connection — and a missing service — leave the outcome
/// unknown.
#[test]
fn only_a_peers_answer_counts_as_a_refusal() {
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

#[test]
fn invocation_update_rpc_diagnostic_keeps_cause_and_existing_fail_closed_latch() {
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

#[test]
fn invocation_update_rpc_diagnostic_bounds_untrusted_text_without_logging_io_payload() {
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

#[test]
fn document_publication_disconnection_and_pending_deadline_are_errors() {
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

#[test]
#[cfg(unix)]
fn document_publication_superseded_receipt_does_not_stop_worker() {
    let mut manager = connected_manager();
    assert!(manager.send(CollabRequest::document_incremental(b"first".to_vec())));
    drop(manager.begin_document_publication_barrier().unwrap());
    assert!(manager.send(CollabRequest::document_incremental(b"second".to_vec())));
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
include!("native_media_tests.rs");

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
        "original invocation document custody was retired"
    );
    assert!(!original.load(Ordering::Acquire));
    assert!(!state.document_reads.publishable("original-route"));
}

#[tokio::test]
async fn native_invocation_delivery_turn_timeout_preserves_unapplied_original_custody() {
    let state = BridgeState::new();
    let original = state.document_reads.close_for_test("original-route");
    let result = native_invocation_delivery_turn("original-route", &state).await;
    assert_eq!(
        result.unwrap_err(),
        "original invocation capture did not become current"
    );
    assert!(state.document_reads.current("original-route", &original));
    assert!(!original.load(Ordering::Acquire));
    assert!(!state.document_reads.publishable("original-route"));
}

include!("native_fixtures.rs");

#[cfg(unix)]
pub(super) fn capturing_manager() -> (CollaborationManager, Box<dyn Fn() -> Vec<SignedOp>>) {
    let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
    let broker = FakeBroker {
        captures: captures.clone(),
        ..FakeBroker::default()
    };
    let (fd, _, _thread) = stand_up_fake_broker(broker);
    let manager = CollaborationManager::from_sdk_fd_for_test(fd, "capture-test", Uuid::new_v4());
    let probe = Box::new(move || {
        captures
            .lock()
            .unwrap()
            .signed_ops
            .iter()
            .map(|bytes| postcard::from_bytes::<SignedOp>(bytes).unwrap())
            .collect()
    });
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

#[cfg(unix)]
#[test]
fn manager_connects_in_the_background_without_holding_the_app() {
    // Case `a_broker_that_is_not_there_does_not_hold_the_app_up`.
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

    // Case `construction_returns_at_once_while_the_broker_is_silent`.
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
            record: None,
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
        let channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id.to_string(), "");
        let group = channel_crypto::ChannelGroup::create(
            channel,
            channel_crypto::MemberKey::generate("owner").unwrap(),
        )
        .unwrap();
        assert_eq!(
            mgr.install_document_encryption(route.clone(), group),
            Err("collaboration is still connecting".into()),
            "no call may wait on a worker that is still connecting"
        );
        // The frame loop keeps calling these while connecting. With no
        // acknowledged route nothing is queued behind the connection.
        assert!(!mgr.send(CollabRequest::Presence {
            key: PresenceKey::surface_pointer("silent-broker-test"),
            payload: PresencePayload::Arrived,
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

    // Case `a_background_connection_becomes_ready_and_wakes_the_owner`.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let (scope, wakes) = wake_channel();
        let entry = scope.enter();
        let route = RouteSpec {
            app_id: "background-connect-test".into(),
            cluster_id: Uuid::new_v4(),
            record: None,
        };
        let mut mgr = background_manager(fd, route.clone());
        drop(entry);
        assert_eq!(mgr.state(), CollaborationState::Connecting);

        let settled = settle(&mut mgr, &wakes);

        assert_eq!(settled, CollaborationState::Ready);
        assert!(mgr.is_connected());
        assert_eq!(
            mgr.primary_route().map(crate::PrimaryRouteWitness::route),
            Some(&route)
        );
        assert_eq!(mgr.take_state_change(), Some(CollaborationState::Ready));
        let joined: Vec<String> = captures
            .lock()
            .unwrap()
            .joined
            .iter()
            .map(|(channel, _)| channel.clone())
            .collect();
        assert_eq!(
            joined,
            vec![route.channel_id()],
            "the queued Hello joined the primary route once connected"
        );
    }

    // Case `explicit_test_join_forwards_its_identity_presentation`.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let cluster = Uuid::new_v4();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: cluster.to_string(),
            device_certificate: b"certified test certificate".to_vec(),
            proof: vec![5; 64],
        }
        .encode()
        .unwrap();
        let _mgr = CollaborationManager::from_sdk_fd_for_test_with_identity(
            fd,
            "alice",
            cluster,
            presentation.clone(),
        );

        tilo_test_env::wait_until("join_channel must be observed", || {
            !captures.lock().unwrap().joined.is_empty()
        });
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].1, presentation);
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
                record: None,
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

#[test]
fn bridge_backpressure_sheds_presence_and_defers_results() {
    // Case `saturated_outbound_bridge_sheds_only_presence_then_fails_closed`.
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let document =
            || SdkCommand::Primary(CollabRequest::document_incremental(b"durable".to_vec()));
        let presence = || {
            SdkCommand::Primary(CollabRequest::Presence {
                key: PresenceKey::Tab {
                    id: "saturated".to_owned(),
                },
                payload: PresencePayload::Pointer { x: 1.0, y: 2.0 },
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
                SdkCommand::Primary(CollabRequest::RequestDocumentSnapshot),
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

    // Case `inbound_events_wake_the_owner_once_per_queued_burst`.
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
            },
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "admitted work must wake the owner instead of waiting for its 60Hz poll"
        );
        assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
            user_id: "peer".into(),
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
            },
        }));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            2,
            "a closed owner cannot be woken"
        );
    }

    // Case `saturated_inbound_bridge_drops_presence_and_holds_results`.
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());

        for _ in 0..10_000 {
            assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
                user_id: "peer".to_owned(),
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

    // Case `byte_reservation_releases_when_the_ui_consumes_a_command`.
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

    // Case `oversized_inbound_action_fails_before_queueing`.
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

#[test]
fn document_budget_defers_or_refuses_without_disconnecting() {
    // Case `over_limit_document_is_refused_explicitly_without_disconnecting`.
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

    // Case `document_budget_is_charged_for_bytes_sent_not_spare_capacity`.
    {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let mut data = Vec::with_capacity(SDK_DOCUMENT_COMMAND_BYTES * 2);
        data.push(0);
        let command = SdkCommand::Primary(CollabRequest::Document {
            mutation: collab_types::DocumentMutation::Snapshot(data),
            authorization: None,
            commit: None,
        });

        assert!(enqueue_sdk_command(&tx, command, &state));
        assert!(
            state.document_command_bytes.0.load(Ordering::Acquire) < 1024 * 1024,
            "a mostly-empty allocation is shrunk before it is parked on the bridge"
        );
        assert!(rx.try_recv().is_ok());
    }

    // Case `document_budget_defers_aggregate_documents_without_disconnecting`.
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
                SdkCommand::Primary(CollabRequest::RequestDocumentSnapshot),
                &state
            ),
            "queued document bytes never starve control traffic"
        );
    }

    // Case `a_busy_document_send_wakes_the_owner_once_room_frees`.
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

    // Case `a_directory_request_wakes_the_idle_owner`.
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

    // Case `a_lone_document_larger_than_the_budget_still_makes_progress`.
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
            presentation: harness_presentation(),
            participant_kind: "person",
            signer: harness_document_signer(),
        })),
        route,
        None,
        None,
    )
}

#[cfg(unix)]
#[test]
fn sdk_transport_forwards_signed_documents_and_presence() {
    // Case `hello_joins_channel_and_send_forwards_signed_op`.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let cluster = Uuid::new_v4();
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", cluster);

        tilo_test_env::wait_until("join_channel must be observed", || {
            !captures.lock().unwrap().joined.is_empty()
        });
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        // The channel id carries the whole address, app id included.
        assert_eq!(
            joined[0].0,
            channel_key::format("alice", &cluster.to_string(), ""),
        );

        let mut op = SignedOp::new_unsigned(
            b"payload".to_vec(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([1u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            1,
        );
        op.signature = collab_types::Signature::from_bytes([0u8; 64]);
        let expected_bytes = postcard::to_stdvec(&op).unwrap();
        mgr.send(CollabRequest::SignedOperation {
            envelope: Box::new(op),
        });

        tilo_test_env::wait_until("send() must reach the SDK", || {
            !captures.lock().unwrap().signed_ops.is_empty()
        });
        let seen = captures.lock().unwrap().signed_ops.clone();
        assert_eq!(seen, vec![expected_bytes]);
    }

    // Case `owned_route_raw_documents_use_typed_signed_sdk_transport`.
    {
        use automerge::transaction::Transactable;
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let workspace = Uuid::new_v4();
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "shell", workspace);
        let route = RouteSpec::record("actual-owner", workspace, "tab:qualified");
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "body", "before").unwrap();
        document.commit();
        let heads = document.base_heads();
        let snapshot = document.save();
        document.put(automerge::ROOT, "body", "after").unwrap();
        collab_types::CommitNamed::commit_edit(&mut document, "fixture", "body");
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
        tilo_test_env::wait_until("both raw requests reach SDK signed RPC", || {
            captures.lock().unwrap().signed_ops.len() == 2
        });
        let captured = captures.lock().unwrap();
        assert!(captured.joined.iter().any(|(channel, _)| channel
            == &channel_key::format("actual-owner", &workspace.to_string(), "tab:qualified")));
        for (index, bytes) in captured.signed_ops.iter().enumerate() {
            let envelope: SignedOp = postcard::from_bytes(bytes).unwrap();
            collab_types::validate_document_payload_route(
                "actual-owner",
                &workspace.to_string(),
                "tab:qualified",
                &envelope,
            )
            .unwrap();
            assert!(
                envelope.encryption.is_none(),
                "harness route is explicitly unencrypted"
            );
            let expected = if index == 0 {
                collab_types::DocumentMutation::Snapshot(snapshot.clone())
            } else {
                collab_types::DocumentMutation::Incremental(delta.clone())
            };
            assert_eq!(
                collab_types::DocumentMutation::decode(&envelope.payload).unwrap(),
                expected
            );
            assert_ne!(
                envelope.signature,
                collab_types::Signature::from_bytes([0; 64])
            );
        }
    }

    // Case `publication_barrier_resolves_after_every_earlier_document_is_published`:
    // `send` only queues. A publication barrier resolves once every
    // document mutation queued before it has reached the broker, which is
    // what lets an owner end its session without discarding a change still
    // in transit.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _thread) = stand_up_fake_broker(broker);
        let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "shell", Uuid::new_v4());
        for n in 0..8_u8 {
            assert!(manager.send(CollabRequest::document_incremental(vec![n; 16])));
        }
        manager
            .begin_document_publication_barrier()
            .expect("the broker transport supports publication barriers")
            .wait(Some(tilo_test_env::hang_deadline()))
            .expect("every queued document was published");
        assert_eq!(
            captures.lock().unwrap().signed_ops.len(),
            8,
            "the barrier resolved before the documents ahead of it reached the broker"
        );
    }

    // Case `presence_variants_map_to_correct_sdk_publishes`:
    // Every presence variant reaches the SDK method that keeps its key —
    // a pointer publishes keyed, like a caret, rather than through
    // `publish_cursor(x, y)`, which has nowhere to put one.
    {
        let broker = FakeBroker::default();
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        tilo_test_env::wait_until("join_channel must be observed", || {
            !captures.lock().unwrap().joined.is_empty()
        });

        let key = PresenceKey::Tab {
            id: "note-1".to_owned(),
        };
        let expected_key_bytes = postcard::to_stdvec(&key).unwrap();

        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Pointer { x: 1.5, y: 2.5 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 12 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Selection { start: 3, end: 9 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Typing { start: 4, end: 10 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Arrived,
        });
        mgr.send(CollabRequest::Presence {
            key,
            payload: PresencePayload::Left,
        });

        tilo_test_env::wait_until(
            "every presence variant must reach the corresponding SDK method",
            || {
                let c = captures.lock().unwrap();
                c.pointer_publishes.len() == 1
                    && c.caret_publishes.len() == 1
                    && c.selection_publishes.len() == 1
                    && c.typing_publishes.len() == 1
                    && c.arrived.len() == 1
                    && c.left.len() == 1
            },
        );
        let (
            pointer_publishes,
            caret_publishes,
            selection_publishes,
            typing_publishes,
            arrived,
            left,
        ) = {
            let c = captures.lock().unwrap();
            (
                c.pointer_publishes.clone(),
                c.caret_publishes.clone(),
                c.selection_publishes.clone(),
                c.typing_publishes.clone(),
                c.arrived.clone(),
                c.left.clone(),
            )
        };
        assert_eq!(
            pointer_publishes,
            vec![(expected_key_bytes.clone(), 1.5, 2.5)],
            "a pointer carries the key its publisher chose, like every other \
         keyed variant here",
        );
        assert_eq!(caret_publishes, vec![(expected_key_bytes.clone(), 12)]);
        assert_eq!(
            selection_publishes,
            vec![(expected_key_bytes.clone(), 3, 9)]
        );
        assert_eq!(typing_publishes, vec![(expected_key_bytes.clone(), 4, 10)]);
        assert_eq!(arrived, vec![expected_key_bytes.clone()]);
        assert_eq!(left, vec![expected_key_bytes]);
    }
}

#[test]
fn signed_documents_seal_snapshots_and_decrypt_history() {
    // Case `signed_document_event_preserves_snapshot_kind`.
    {
        let op = SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(b"whole document".to_vec())
                .encode()
                .unwrap(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([3_u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            43,
        );
        let event = signed_document_event(
            Arrived {
                envelope: op,
                from_user_id: "peer-alice".into(),
                admission: OpAdmission::CleartextAdmitted,
            },
            &plain_route_keys(),
        )
        .unwrap()
        .event;
        assert!(matches!(
            &event,
            CollabEvent::Document { from_user_id, .. } if from_user_id == "peer-alice"
        ));
        assert!(matches!(
            event.document_mutation(),
            Some(Ok(DocumentMutation::Snapshot(data))) if data == b"whole document"
        ));
    }

    // Case `signed_document_event_decrypts_content_keys_for_history_and_late_joiners`.
    {
        let channel = collab_types::ChannelId::new();
        let artifact = collab_types::ArtifactId::new();
        let alice_key = channel_crypto::MemberKey::generate("alice").unwrap();
        let bob_key = channel_crypto::MemberKey::generate("bob").unwrap();
        let mut alice = channel_crypto::ChannelGroup::create(channel, alice_key).unwrap();
        let addition = alice.admit(bob_key.fresh_key_package().unwrap()).unwrap();
        let bob = channel_crypto::ChannelGroup::process_welcome_with_history(
            channel,
            &addition.welcome,
            &addition.key_history,
            bob_key,
        )
        .unwrap();
        let plaintext = DocumentMutation::Incremental(b"secret change".to_vec())
            .encode()
            .unwrap();
        // The action commit a sealed part carries rides inside what only
        // the unit's readers open.
        let commit = collab_types::WorkspaceCommit::new(
            "workspace",
            collab_types::CommitAuthor {
                principal: Some("alice".into()),
                device: [9; 32],
            },
            1,
            None,
            None,
            vec![collab_types::CommitPart {
                app: "chat".into(),
                record: "main".into(),
                changes: vec![[1; 32]],
            }],
        );
        let sealed_plaintext = commit.frame(&plaintext).unwrap();
        let alice_endpoint = group_route_keys(alice);
        let PreparedFrame::Ready(ciphertext, metadata) =
            prepare_document_payload(&alice_endpoint, artifact, sealed_plaintext, &[])
        else {
            panic!("a provisioned endpoint seals at once");
        };
        let metadata = metadata.expect("a provisioned endpoint never sends plaintext");
        assert_eq!(metadata.channel, channel);
        assert_eq!(
            metadata.retention(),
            Some(collab_types::ContentRetention::Delta)
        );
        let build = |payload: Vec<u8>, artifact, metadata| {
            SignedOp::new_unsigned_with_encryption(
                payload,
                artifact,
                collab_types::Ed25519PublicKey::from_bytes([9; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpKind::Change,
                crate::time::SystemTime::UNIX_EPOCH,
                1,
                metadata,
            )
        };
        let op = build(ciphertext.clone(), artifact, metadata);
        let endpoint = group_route_keys(bob);
        for _ in 0..2 {
            // Live delivery and a later scrollback read of the same op.
            let signed = signed_document_event(
                Arrived {
                    envelope: op.clone(),
                    from_user_id: "alice".into(),
                    admission: OpAdmission::EncryptionRequired,
                },
                &endpoint,
            )
            .unwrap();
            assert!(matches!(
                signed.event.document_mutation(),
                Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
            ));
            assert_eq!(
                signed.commit,
                Some(commit.clone()),
                "the part's action comes out of the sealed payload"
            );
        }
        let plaintext_downgrade = SignedOp::new_unsigned(
            plaintext,
            artifact,
            collab_types::Ed25519PublicKey::from_bytes([9; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            2,
        );
        let refusal = |op| match signed_document_event(
            Arrived {
                envelope: op,
                from_user_id: "alice".into(),
                admission: OpAdmission::EncryptionRequired,
            },
            &endpoint,
        ) {
            Err(UndeliveredDocument::Refused(refusal)) => refusal,
            _ => panic!("refused"),
        };
        let downgrade = refusal(plaintext_downgrade);
        assert!(
            matches!(
                downgrade,
                Refusal::Unopened(crate::route_keys::Never::Downgrade)
            ),
            "cleartext the broker says must be sealed is refused, and says so"
        );
        assert_eq!(
            downgrade.to_string(),
            "cleartext the broker says must be sealed"
        );
        assert!(
            matches!(
                refusal(build(
                    ciphertext.clone(),
                    collab_types::ArtifactId::new(),
                    metadata
                )),
                Refusal::Unopened(crate::route_keys::Never::Undecryptable)
            ),
            "content is bound to its artifact"
        );
        let mislabeled = collab_types::EncryptionMetadata::content_key(
            metadata.epoch,
            channel,
            collab_types::ContentRetention::Checkpoint,
        );
        assert!(
            matches!(
                refusal(build(ciphertext, artifact, mislabeled)),
                Refusal::RetentionMismatch
            ),
            "the clear storage class must match the content"
        );

        // A member added afterwards reads the op from history.
        let carol_key = channel_crypto::MemberKey::generate("carol").unwrap();
        let addition = {
            let mut keys = alice_endpoint.lock().unwrap();
            let super::super::route_keys::Keys::Group(group) = &mut keys.keys else {
                panic!("alice's endpoint holds her group");
            };
            group.admit(carol_key.fresh_key_package().unwrap()).unwrap()
        };
        let carol = channel_crypto::ChannelGroup::process_welcome_with_history(
            channel,
            &addition.welcome,
            &addition.key_history,
            carol_key,
        )
        .unwrap();
        let carol_endpoint = group_route_keys(carol);
        let event = signed_document_event(
            Arrived {
                envelope: op,
                from_user_id: "alice".into(),
                admission: OpAdmission::EncryptionRequired,
            },
            &carol_endpoint,
        )
        .unwrap()
        .event;
        assert!(matches!(
            event.document_mutation(),
            Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
        ));
    }

    // Case `snapshots_are_sealed_as_retained_checkpoints`.
    {
        let snapshot = DocumentMutation::Snapshot(b"whole".to_vec())
            .encode()
            .unwrap();
        assert_eq!(
            collab_types::ContentRetention::of_frame(&snapshot),
            collab_types::ContentRetention::Checkpoint
        );
        let big = DocumentMutation::Snapshot(
            vec![1; 3 * collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES],
        )
        .encode_frames()
        .unwrap();
        let classes: Vec<_> = big
            .iter()
            .map(|frame| collab_types::ContentRetention::of_frame(frame))
            .collect();
        assert_eq!(
            classes.first(),
            Some(&collab_types::ContentRetention::Checkpoint)
        );
        assert!(
            classes
                .iter()
                .skip(1)
                .all(|class| *class == collab_types::ContentRetention::Delta)
        );
    }

    // Case `joiner_decrypts_fetched_history_with_no_other_member_online`:
    // A member added after the history was written fetches it through the
    // real SDK transport and decrypts every page with the key history it
    // received on joining — no other member is running.
    {
        let app_id = "so.tilo.history-test";
        let cluster = Uuid::from_u128(7);
        let workspace = cluster.to_string();
        let channel = collab_types::document_channel_id(app_id, &workspace, "");
        let artifact = collab_types::document_artifact_id(app_id, &workspace, "");
        let mut alice = channel_crypto::ChannelGroup::create(
            channel,
            channel_crypto::MemberKey::generate("alice").unwrap(),
        )
        .unwrap();
        let history: Vec<Vec<u8>> = (1..=20_u64)
            .map(|seq| {
                let frame = collab_types::DocumentMutation::Incremental(
                    format!("change {seq}").into_bytes(),
                )
                .encode()
                .unwrap();
                let (metadata, ciphertext) = alice
                    .seal_content(
                        artifact,
                        collab_types::ContentRetention::of_frame(&frame),
                        &frame,
                    )
                    .unwrap();
                let op = SignedOp::new_unsigned_with_encryption(
                    ciphertext,
                    artifact,
                    collab_types::Ed25519PublicKey::from_bytes([3; 32]),
                    collab_types::AttestationId::new(),
                    collab_types::OpKind::Change,
                    crate::time::SystemTime::UNIX_EPOCH,
                    seq,
                    metadata,
                );
                postcard::to_stdvec(&op).unwrap()
            })
            .collect();
        let carol_key = channel_crypto::MemberKey::generate("carol").unwrap();
        let addition = alice.admit(carol_key.fresh_key_package().unwrap()).unwrap();
        let carol = channel_crypto::ChannelGroup::process_welcome_with_history(
            channel,
            &addition.welcome,
            &addition.key_history,
            carol_key,
        )
        .unwrap();
        drop(alice);

        let broker = FakeBroker {
            scrollback_response: Some(SeedScrollback {
                ops: history,
                done: true,
            }),
            ..FakeBroker::default()
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, app_id, cluster);
        tilo_test_env::wait_until("`!captures.lock().unwrap().joined.is_empty()`", || {
            !captures.lock().unwrap().joined.is_empty()
        });
        mgr.install_document_encryption(
            RouteSpec {
                app_id: app_id.to_owned(),
                cluster_id: cluster,
                record: None,
            },
            carol,
        )
        .unwrap();
        mgr.send(CollabRequest::FetchScrollback {
            artifact,
            before_seq: 21,
            max: 50,
        });
        let events = poll_until(&mut mgr, |events| {
            events
                .iter()
                .any(|e| matches!(e, CollabEvent::ScrollbackBatch { .. }))
        });
        let (ops, payloads, oldest_seq) = events
            .into_iter()
            .find_map(|e| match e {
                CollabEvent::ScrollbackBatch {
                    ops,
                    decrypted_payloads,
                    oldest_seq,
                    ..
                } => Some((ops, decrypted_payloads, oldest_seq)),
                _ => None,
            })
            .unwrap();
        assert_eq!(ops.len(), 20);
        assert_eq!(oldest_seq, Some(1));
        for (index, payload) in payloads.iter().enumerate() {
            assert_eq!(
                collab_types::DocumentMutation::decode(payload.as_deref().unwrap()).unwrap(),
                collab_types::DocumentMutation::Incremental(
                    format!("change {}", index + 1).into_bytes()
                )
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn sdk_streams_translate_to_ui_events() {
    // Case `signed_op_stream_translates_to_signed_crdt_incremental`.
    {
        let mut op = SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(b"remote-payload".to_vec())
                .encode()
                .unwrap(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([2u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            42,
        );
        op.signature = collab_types::Signature::from_bytes([0u8; 64]);
        let op_bytes = postcard::to_stdvec(&op).unwrap();

        let broker = FakeBroker {
            op_backlog: vec![(
                op_bytes,
                "peer-bob".to_owned(),
                OpAdmission::CleartextAdmitted,
            )],
            ..FakeBroker::default()
        };
        let (fd, _captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        let events = poll_until(&mut mgr, |events| {
            events
                .iter()
                .any(|e| matches!(e, CollabEvent::Document { .. }))
        });

        let signed: Vec<_> = events
            .into_iter()
            .filter_map(|e| match e {
                CollabEvent::Document {
                    envelope,
                    from_user_id,
                    ..
                } => Some((envelope, from_user_id)),
                _ => None,
            })
            .collect();
        assert_eq!(signed.len(), 1, "one document event expected");
        assert_eq!(signed[0].1, "peer-bob");
        assert_eq!(
            collab_types::DocumentMutation::decode(&signed[0].0.payload).unwrap(),
            collab_types::DocumentMutation::Incremental(b"remote-payload".to_vec())
        );
        assert_eq!(signed[0].0.seq, 42);
    }

    // Case `context_stream_translates_to_context_event`.
    {
        let broker = FakeBroker {
            context_backlog: vec![SeedContext {
                collaborators: vec![
                    Collaborator {
                        id: "alice".to_owned(),
                        name: "Alice".to_owned(),
                        colour: Some(collab_types::presence::PresenceHue::Orange),
                        is_agent: false,
                        is_self: false,
                        joined_at: None,
                    },
                    Collaborator {
                        id: "bob".to_owned(),
                        name: "Bob".to_owned(),
                        colour: None,
                        is_agent: false,
                        is_self: false,
                        joined_at: None,
                    },
                ],
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
                CollabEvent::Context {
                    collaborators,
                    permissions,
                    ..
                } => Some((collaborators.clone(), permissions.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(contexts.len(), 2, "source loss must withdraw its context");
        let (collabs, perms) = &contexts[0];
        assert_eq!(collabs.len(), 2);
        assert_eq!(collabs[0].id, "alice");
        assert_eq!(collabs[0].name, "Alice");
        assert_eq!(
            collabs[0].colour,
            Some(collab_types::presence::PresenceHue::Orange)
        );
        assert_eq!(collabs[1].colour, None);
        assert_eq!(perms, &["edit".to_owned(), "share".to_owned()]);
        assert!(contexts[1].0.is_empty());
        assert!(contexts[1].1.is_empty());
    }

    // Case `presence_stream_translates_every_variant`.
    {
        let key = PresenceKey::Tab {
            id: "note-1".to_owned(),
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();
        let presence_backlog = vec![
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: collab_types::PeerPresencePayload::Pointer { x: 6.0, y: 9.0 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: collab_types::PeerPresencePayload::Caret { pos: 7 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: collab_types::PeerPresencePayload::Selection { start: 1, end: 3 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: collab_types::PeerPresencePayload::Typing { start: 2, end: 5 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: collab_types::PeerPresencePayload::Arrived,
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes,
                payload: collab_types::PeerPresencePayload::Left,
            },
        ];
        let broker = FakeBroker {
            presence_backlog,
            ..FakeBroker::default()
        };
        let (fd, _captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        // Other events (connection state, context) can arrive in between, so
        // wait for the six presence events themselves, not for any six.
        let events = poll_until(&mut mgr, |events| {
            events
                .iter()
                .filter(|e| matches!(e, CollabEvent::RemotePresenceUpdate { .. }))
                .count()
                >= 6
        });

        let updates: Vec<_> = events
            .into_iter()
            .filter_map(|e| match e {
                CollabEvent::RemotePresenceUpdate { key, payload, .. } => Some((key, payload)),
                _ => None,
            })
            .collect();
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Pointer { x: 6.0, y: 9.0 })),
            "keyed pointer retains its coordinate-space key"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Caret { pos: 7 })),
            "caret variant translated"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Selection { start: 1, end: 3 })),
            "selection variant translated"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Typing { start: 2, end: 5 })),
            "typing remains distinct from selection"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Arrived)),
            "arrived variant translated"
        );
        assert!(
            updates.contains(&(key, PresencePayload::Left)),
            "left variant translated"
        );
    }

    // Case `fetch_scrollback_returns_batch_event`.
    {
        let payload = collab_types::DocumentMutation::Incremental(b"old-payload".to_vec())
            .encode()
            .unwrap();
        let mut op = SignedOp::new_unsigned(
            payload,
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([3u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            5,
        );
        op.signature = collab_types::Signature::from_bytes([0u8; 64]);
        let op_bytes = postcard::to_stdvec(&op).unwrap();
        let seed = SeedScrollback {
            ops: vec![op_bytes],
            done: true,
        };
        let broker = FakeBroker {
            scrollback_response: Some(seed),
            ..FakeBroker::default()
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        tilo_test_env::wait_until("join_channel must precede scrollback fetch", || {
            !captures.lock().unwrap().joined.is_empty()
        });

        let artifact = collab_types::ArtifactId::new();
        mgr.send(CollabRequest::FetchScrollback {
            artifact,
            before_seq: 10,
            max: 50,
        });
        let events = poll_until(&mut mgr, |events| {
            events
                .iter()
                .any(|e| matches!(e, CollabEvent::ScrollbackBatch { .. }))
        });
        let batch = events
            .into_iter()
            .find_map(|e| match e {
                CollabEvent::ScrollbackBatch { ops, exhausted, .. } => Some((ops, exhausted)),
                _ => None,
            })
            .expect("must receive a ScrollbackBatch event");
        assert_eq!(batch.0.len(), 1);
        assert_eq!(
            collab_types::DocumentMutation::decode(&batch.0[0].payload).unwrap(),
            collab_types::DocumentMutation::Incremental(b"old-payload".to_vec())
        );
        assert!(batch.1, "seeded broker reports exhausted");
        assert_eq!(
            captures.lock().unwrap().scrollback_requests,
            vec![(artifact.0.as_bytes().to_vec(), 10, 50)],
            "artifact / before_seq / max round-tripped verbatim",
        );
    }
}

#[test]
fn wire_translation_is_symmetric_and_fail_closed() {
    // Case `translate_presence_event_is_symmetric_over_all_payload_variants`.
    {
        // Straight unit test of `translate_presence_event` — no broker or
        // threading needed. Malformed scopes are deliberately not projected.
        let key = PresenceKey::Widget {
            path: vec![collab_types::presence::PathSegment::Field("body".into())],
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes,
            payload: runtime_collab::PresencePayload::Caret { pos: 3 },
        })
        .expect("caret event");
        if let CollabEvent::RemotePresenceUpdate {
            key: k, payload, ..
        } = e
        {
            assert_eq!(k, key);
            assert_eq!(payload, PresencePayload::Caret { pos: 3 });
        } else {
            panic!("expected RemotePresenceUpdate");
        }

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes: vec![0xFF, 0xFF, 0xFF],
            payload: runtime_collab::PresencePayload::Arrived,
        });
        assert!(
            e.is_none(),
            "an undecodable target is not semantic presence"
        );
    }

    // Case `invocation_context_json_is_optional_and_fail_closed`.
    {
        let context = tilo_actions::ActionContext::default()
            .with_purpose("Review the proposal")
            .with_group("task:review", "Review", "task");
        let json = serde_json::to_string(&context).unwrap();
        assert_eq!(parse_invocation_context(7, &json), Some(context));
        assert_eq!(parse_invocation_context(8, "  "), None);
        assert_eq!(parse_invocation_context(9, "{not-json"), None);
    }
}

fn assert_document_publication_failure(manager: &mut CollaborationManager, expected: &str) {
    let error = manager
        .begin_document_publication_barrier()
        .and_then(|receipt| receipt.wait(Some(tilo_test_env::hang_deadline())))
        .unwrap_err();
    assert!(error.to_string().contains(expected), "{error}");
}
fn notebook_action(index: usize) -> serde_json::Value {
    serde_json::json!({
        "resource": format!("notes.page:{index:032x}"),
        "action": "open", "resource_kind": "note",
        "resource_label": format!("Notebook page {index}"),
        "action_label": "Open page",
        "description": "Open this page in its notebook. Keep its notebook context, identify the original page, and show the existing page through the ordinary local reader. This catalogue description represents a full app-provided action explanation, including its expected navigation result.",
        "operation_kind": tilo_actions::OperationKind::View,
        "params": [tilo_actions::ActionParam::required("page", tilo_actions::ParamType::Resource, "The page to open")],
        "contract": tilo_actions::ActionContract::view(),
        "input": crate::ExportedInput::None
    })
}

/// The whole catalogue of one notebook window, connection `peer` on
/// `record`, offering `count` pages, as the broker sends it first.
pub fn notebook_action_catalogue_fixture(
    cluster: Uuid,
    count: usize,
    peer: u64,
    record: &str,
) -> String {
    let surface = BrokerActionSurface {
        actions: (0..count)
            .map(|index| serde_json::from_value(notebook_action(index)).unwrap())
            .collect(),
        ..BrokerActionSurface::with_id(runtime_collab::CatalogueSurfaceId {
            peer_id: peer,
            provider: None,
            app_id: "so.tilo.notes".into(),
            cluster_id: cluster.to_string(),
            record_id: record.into(),
            live: true,
        })
    };
    serde_json::to_string(&CatalogueChange::snapshot(&[surface])).unwrap()
}

// The action catalogue's admission, coalescing and resync over the broker.
#[cfg(test)]
#[path = "broker_catalogue_tests.rs"]
mod catalogue;
