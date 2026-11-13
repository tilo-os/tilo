//! The broker transport: the `runtime-collab` client on a worker thread (the
//! "shovel") whose `LocalSet` keeps every route's state. The owner reaches it
//! over bounded queues ([`CollabTransport::submit`] / [`CollabTransport::drain`]).
//! Overload never closes the bridge: durable commands wait in the owner's
//! ordered spill, documents defer (`Busy`) or are refused (`TooLarge`), and
//! presence waits there with only its newest word per key. Signing and verification happen on the worker.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the shovel thread bridges a blocking socket"
)]

#[cfg(test)]
use super::channel_key;
use super::route_keys::Arrived;
use super::{
    ActionCatalogue, ActionDirectoryRequest, Backend, CatalogueChange, CollabEvent, CollabRequest,
    CollabTransport, CollaborationManager, Collaborator, DocumentMutation,
    DocumentPublicationError, DocumentPublicationReceipt, PresenceKey, PresencePayload,
    PrimaryRouteState, RouteSpec, SignedOp, TransportCommand, TransportError, TransportStatus,
    Uuid, auto_hello_env, mpsc,
};
use runtime_collab::OpAdmission;
use tilo_sort::CompactSort as _;

#[path = "audio_output.rs"]
mod audio_output;
#[path = "broker_bytes.rs"]
mod bytes;
#[path = "refused_answer.rs"]
mod refused_answer;
#[path = "task_agent_route.rs"]
mod task_agent_route;
use bytes::{command_bytes, event_diagnostics, remote_action_round_trip};
#[cfg(all(unix, any(test, feature = "test-support")))]
#[path = "broker_fixtures.rs"]
mod fixtures;
#[path = "broker_published.rs"]
mod published;
pub use published::SessionResource;
#[path = "change_waits.rs"]
mod change_waits;
#[path = "committed_document.rs"]
pub(super) mod committed_document;
#[path = "held_inbound.rs"]
mod held_inbound;
#[path = "inbound_bridge.rs"]
mod inbound_bridge;
use inbound_bridge::InboundBridge;
#[path = "inbound_document.rs"]
mod inbound_document;
#[path = "realtime_stream.rs"]
mod realtime_stream;
use realtime_stream::spawn_realtime_stream;
#[path = "broker_key_fetch.rs"]
mod key_fetch;
#[path = "broker_key_schedule.rs"]
mod key_schedule;
#[path = "broker_route_join.rs"]
mod route_join;
use key_fetch::{
    FetchedKeys, KeyFetch, KeyQuery, answered_keys, certified_channel_keys, endpoint_channel_keys,
    update_route_keys,
};
use key_schedule::{KeyRound, KeySchedule, KeySignal};
#[cfg(test)]
use route_join::ROUTE_JOIN_WAIT;
use route_join::{
    JoinInputs, JoinedRoute, join_route, refuse_unjoined, start_route_join, waits_on_route,
};
#[path = "native_initial.rs"]
pub mod native_initial;
#[cfg(feature = "native")]
#[path = "broker_native_join.rs"]
mod native_join;
#[cfg(feature = "native")]
pub use native_join::NativeJoin;
#[path = "native_media.rs"]
mod native_media;
pub use committed_document::CommittedDocument;
#[cfg(test)]
use inbound_document::Refusal;
use inbound_document::{
    DecryptFailure, UndeliveredDocument, decrypt_document_payload, signed_document_event,
};

use crate::time::{Duration, Instant};
use getrandom::{SysRng, rand_core::UnwrapErr};
use runtime_portal::RawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use tilo_lock::MutexRecover as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::LocalSet;

/// Backpressure a slow UI rather than let a cross-thread queue grow unbounded.
const ACTION_DIRECTORY_REQUEST_QUEUE: usize = 64;

/// Presence is a workspace signal, not the app's request: a publish the peer
/// refused is logged, and the request that carried the update still answers.
async fn publish_presence(
    what: &'static str,
    publish: impl std::future::Future<Output = Result<(), runtime_portal::NotAvailable>>,
) {
    if let Err(error) = publish.await {
        tracing::warn!(%error, presence = what, "collab: presence publish refused");
    }
}
/// Whether the identity service answered and declined to sign, as opposed to
/// being unreachable: it reports a refusal as permission denied.
fn signing_refused(error: &runtime_portal::NotAvailable) -> bool {
    matches!(
        error,
        runtime_portal::NotAvailable::ConnectionFailed(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
    )
}

/// One deadline per host identity exchange; the client drops a failed stream,
/// so this also bounds shutdown without blocking the shovel's `LocalSet`.
const HOST_IDENTITY_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands waiting behind a slow broker. Capped by count, not bytes: the
/// protocol already bounds every payload.
const SDK_COMMAND_QUEUE: usize = 256;
/// Events waiting behind a busy event loop.
const SDK_EVENT_QUEUE: usize = 256;
/// Retained payload budget per cross-thread queue, independent of the count
/// cap because one document snapshot or argument may be large.
const SDK_COMMAND_QUEUE_BYTES: usize = 4 * 1024 * 1024;
const SDK_EVENT_QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// Budget of the action catalogue changes in flight: a big workspace's
/// catalogue neither starves action results nor fails the bridge.
const SDK_CATALOGUE_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// Document budgets, apart from control traffic so one large edit neither
/// starves nor fails action results. Outbound documents report
/// [`TransportError::Busy`], inbound ones wait for the owner to drain.
const SDK_DOCUMENT_COMMAND_BYTES: usize =
    collab_types::MAX_DOCUMENT_TRANSFER_BYTES + SDK_COMMAND_QUEUE_BYTES;
const SDK_DOCUMENT_EVENT_BYTES: usize =
    collab_types::MAX_DOCUMENT_TRANSFER_BYTES + SDK_EVENT_QUEUE_BYTES;
/// Re-check interval while an inbound document event waits for budget.
const SDK_DOCUMENT_EVENT_RETRY: Duration = Duration::from_millis(250);
const WIFI_CONTINUITY_TIMEOUT: Duration = Duration::from_secs(3);

/// The transport's outbound queue as the process reaches it from outside: the
/// session's published resources ([`SessionResource`]) and the device commands
/// ([`wifi_continuity_command`], [`speaker_command`]).
static PUBLISHED_COMMAND_BRIDGE: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio_mpsc::WeakSender<QueuedSdkCommand>>>,
> = std::sync::OnceLock::new();

/// Invoke the shell's launcher-authenticated, sealed Wi-Fi continuity path.
/// Callers should run this off the frame thread; the bounded timeout turns a
/// lost broker into an ordinary unavailable result.
pub fn wifi_continuity_command(
    command: runtime_collab::WifiContinuityCommand,
) -> Result<runtime_collab::WifiContinuityReply, String> {
    let sender = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "Wi-Fi continuity bridge is unavailable".to_string())?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or_else(|| {
            "Wi-Fi continuity requires the certified collaboration service".to_string()
        })?;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let state = BridgeState::new();
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::WifiContinuity {
            app_id: "so.tilo.settings".into(),
            command,
            reply: reply_tx,
        },
        &state,
    ) {
        return Err("Wi-Fi continuity command queue is unavailable".into());
    }
    reply_rx
        .recv_timeout(WIFI_CONTINUITY_TIMEOUT)
        .map_err(|_| "Wi-Fi continuity service did not respond".to_string())?
}

const SPEAKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// Ask the shell's speaker-group authority over the launcher-authenticated
/// Settings route. Run it off the frame thread; a lost broker times out into
/// an ordinary unavailable result.
pub fn speaker_command(
    command: runtime_collab::SpeakerCommand,
) -> Result<runtime_collab::SpeakerReply, String> {
    let sender = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "speaker service is unavailable".to_string())?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or_else(|| "speakers need the certified collaboration service".to_string())?;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let state = BridgeState::new();
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::Speakers {
            app_id: "so.tilo.settings".into(),
            command,
            reply: reply_tx,
        },
        &state,
    ) {
        return Err("speaker command queue is unavailable".into());
    }
    reply_rx
        .recv_timeout(SPEAKER_COMMAND_TIMEOUT)
        .map_err(|_| "the speaker service did not respond".to_string())?
}

static DISPLAY_COMMAND_BUDGET: std::sync::OnceLock<BridgeState> = std::sync::OnceLock::new();

/// Ask the native host to inspect or change its display transaction. Call off
/// the frame thread; the existing bridge and response wait remain bounded.
pub fn display_control(
    command: Vec<u8>,
    origin: runtime_collab::settings::DisplayOrigin,
) -> Result<Vec<u8>, String> {
    if command.len() > 64 * 1024 {
        return Err("The display request exceeds its wire bound.".into());
    }
    let sender = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "The display service is unavailable.")?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or("Displays need the certified collaboration service.")?;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::DisplayControl {
            origin,
            command,
            reply: reply_tx,
        },
        DISPLAY_COMMAND_BUDGET.get_or_init(BridgeState::new),
    ) {
        return Err("The display request queue is unavailable.".into());
    }
    reply_rx
        .recv_timeout(Duration::from_secs(4))
        .map_err(|_| "The display service did not respond.".to_string())?
}

/// What the owner thread asks of one of the session's published resources.
pub(super) enum PublishedCommand {
    /// Follow a resource's reading, handing each to `readings` (always the
    /// newest: a reading that arrives before the owner took the one before
    /// it replaces it) and the one the watch begins at to `first`, waking
    /// whatever view `wake` holds.
    Watch {
        resource: Arc<str>,
        readings: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
        wake: Arc<std::sync::Mutex<Option<tilo_rt::wake::WakeHandle>>>,
        first: mpsc::SyncSender<Result<String, String>>,
        cancel: tokio::sync::watch::Receiver<()>,
    },
    /// Take an action of one, answering the door's verdict.
    Invoke {
        resource: Arc<str>,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
}

/// Serve one [`PublishedCommand`] on the shovel, where the session's
/// services are reached.
async fn published_command(client: &runtime_collab::CollabClient, command: PublishedCommand) {
    let services = client.services();
    match command {
        PublishedCommand::Watch {
            resource,
            readings,
            wake,
            first,
            mut cancel,
        } => {
            let stream = match services.watch(&resource).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = first.try_send(Err(published_error(&error)));
                    return;
                }
            };
            tokio::task::spawn_local(async move {
                let mut first = Some(first);
                loop {
                    let reading = tokio::select! {
                        // The owner dropped its handle: the watch ends with it.
                        _ = cancel.changed() => return,
                        reading = stream.read() => reading,
                    };
                    let (delivery, ended) = match reading {
                        Ok(reading) => match reading.reading {
                            Some(reading) => (Ok(reading), false),
                            // No service is up for it yet; one may be at a
                            // later revision, so the watch stays on.
                            None => (Err(format!("No service publishes {resource}.")), false),
                        },
                        Err(error) => (Err(published_error(&error)), true),
                    };
                    if let Some(answer) = first.take() {
                        let _ = answer.try_send(delivery.clone());
                    }
                    *readings
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(delivery);
                    let view = wake
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    if let Some(view) = view {
                        view.wake();
                    }
                    if ended {
                        return;
                    }
                }
            });
        }
        PublishedCommand::Invoke {
            resource,
            action,
            arguments,
            reply,
        } => {
            let result = match services.invoke(&resource, &action, &arguments).await {
                Ok(runtime_collab::wire::Receipt::Refused { message, .. }) => Err(message),
                Ok(_taken) => Ok(()),
                Err(error) => Err(published_error(&error)),
            };
            let _ = reply.send(result);
        }
    }
}

/// Why a call on the session's services failed, as the app reports it.
fn published_error(error: &runtime_portal::NotAvailable) -> String {
    error.to_string()
}

/// Have the connected transport's worker send the app's unsent settings
/// writes ([`super::channel_settings`]). Without a connected transport they
/// wait for the primary route, whose join sends them.
pub(super) fn flush_settings() {
    let Some(sender) = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|bridge| bridge.as_ref().and_then(tokio_mpsc::WeakSender::upgrade))
    else {
        return;
    };
    let _ = enqueue_sdk_command(&sender, SdkCommand::FlushSettings, &BridgeState::new());
}

/// Have the connected transport's worker send the app's live activity
/// ([`super::channel_live_activity`]). Without a connected transport it waits
/// for the primary route, whose join sends it.
pub(super) fn flush_live_activity() {
    let Some(sender) = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .ok()
        .and_then(|bridge| bridge.as_ref().and_then(tokio_mpsc::WeakSender::upgrade))
    else {
        return;
    };
    let _ = enqueue_sdk_command(&sender, SdkCommand::FlushLiveActivity, &BridgeState::new());
}

/// Owned mpsc pair + shovel-thread handle for SDK mode. Both directions are
/// bounded. Durable overflow waits in the owner's spill rather than losing
/// an action or invocation transition or disabling the bridge; a document
/// change that does not fit is reported to its sender instead, never
/// dropped.
///
/// Dropping the manager drops `outbound_tx`; the shovel thread's
/// `.recv()` then yields `None` and the thread exits. `_thread` is kept
/// only so the `JoinHandle` isn't detached.
pub(super) struct BrokerTransport {
    outbound_tx: tokio_mpsc::Sender<QueuedSdkCommand>,
    /// Native media rides its own bounded lane so a wedged document RPC can
    /// never delay a stop obligation.
    media: Option<native_media::Sender>,
    inbound_rx: tokio_mpsc::Receiver<QueuedCollabEvent>,
    // Native read turns remain held through the owner's event batch.
    document_turns: Vec<tokio::sync::OwnedSemaphorePermit>,
    /// The document owner each invocation in the drained batch arrived for.
    invocation_owners: std::collections::BTreeMap<u64, committed_document::DeliveryOwner>,
    state: BridgeState,
    /// The receiver stays on the application's owner thread.  RPC servers
    /// own only the cloneable sender, so a host's ask cannot touch
    /// widgets directly.
    action_directory_rx: mpsc::Receiver<ActionDirectoryRequest>,
    action_directory_tx: mpsc::SyncSender<ActionDirectoryRequest>,
    /// Reply slot of the latest [`TransportCommand::ConfirmPrimaryRoute`],
    /// answered by the shovel once it has processed every earlier command.
    pending_primary: Option<mpsc::Receiver<Result<RouteSpec, String>>>,
    /// The shovel dropped a primary-route reply unanswered: it has exited.
    worker_stopped: bool,
    /// Whether [`wifi_continuity_command`] has been pointed at this
    /// transport; done once it has connected.
    command_bridge_registered: bool,
    /// Durable commands the bounded queue had no room for, in submission
    /// order. They wait here for the shovel to catch up instead of
    /// disabling collaboration; see [`Self::enqueue`].
    spill: std::collections::VecDeque<SdkCommand>,
    /// A queue slot held back for the next publication barrier. The owner
    /// usually waits on a receipt on its own thread, so a barrier parked in
    /// [`Self::spill`], which only the owner moves, could never be answered.
    /// Nothing else is admitted while the slot is spent, so a barrier that
    /// finds it spent has no document mutation ahead of it that
    /// [`Self::queued_barrier`] does not already cover.
    barrier_slot: Option<tokio_mpsc::OwnedPermit<QueuedSdkCommand>>,
    /// The barrier that last spent [`Self::barrier_slot`], while queued.
    queued_barrier: std::sync::Weak<BarrierReplies>,
    _thread: Option<thread::JoinHandle<()>>,
}

#[cfg(any(test, feature = "test-support"))]
#[cfg(unix)]
pub struct NativeHistoryInvocationFixture {
    pub manager: CollaborationManager,
    pub refresh: Box<dyn Fn(Vec<u8>, tokio::sync::oneshot::Receiver<()>)>,
    pub invoke: Box<dyn Fn(u64, Uuid)>,
    pub received: Box<dyn Fn() -> usize>,
    pub execution: Box<dyn Fn(Result<bool, String>)>,
    pub terminals: Box<dyn Fn() -> usize>,
    pub updates: Box<dyn Fn() -> Vec<(u64, tilo_actions::InvocationUpdate)>>,
    pub thread: thread::JoinHandle<()>,
}

impl BrokerTransport {
    /// The tid of the thread serving this connection's broker, once it runs:
    /// what a test holding the connection idle counts the wakes of.
    #[cfg(all(test, unix))]
    pub(super) fn shovel_tid(&self) -> Option<i32> {
        self.state.shovel_tid()
    }

    /// Hand a command to the shovel. A full bounded queue is backpressure,
    /// never a reason to disable collaboration: a durable command that does
    /// not fit waits in [`Self::spill`] (in order, with a newer action
    /// surface replacing an older one) and is sent as the shovel frees room.
    /// Presence waits there too, a newer word under the same key taking the
    /// place of the one waiting, so where someone is ends up said even when
    /// their typing filled the queue. Document changes report `Busy`.
    fn enqueue(&mut self, command: SdkCommand) -> Result<(), TransportError> {
        self.flush_spill();
        if !self.spill.is_empty() || !self.reserve_barrier_slot() {
            // Everything behind a parked command stays behind it, and
            // nothing goes ahead of the next barrier's slot.
            if command.document_len().is_some() {
                self.state.command_bytes.arm_budget_wake();
                return Err(TransportError::Busy);
            }
            self.park(command);
            return Ok(());
        }
        match admit_sdk_command(&self.outbound_tx, command, &self.state) {
            Ok(()) => Ok(()),
            Err(Refused {
                command: Some(command),
                ..
            }) => {
                self.park(command);
                Ok(())
            }
            Err(refused) => Err(refused.error),
        }
    }

    fn park(&mut self, command: SdkCommand) {
        match command {
            // A surface delta waiting for room folds into the one already
            // waiting for this app, so a burst of republications is one
            // delta however long the shovel stalls.
            SdkCommand::Primary(CollabRequest::PublishActionSurface { delta }) => {
                let waiting = self.spill.iter_mut().find_map(|parked| match parked {
                    SdkCommand::Primary(CollabRequest::PublishActionSurface { delta: older }) => {
                        Some(older)
                    }
                    _ => None,
                });
                if let Some(older) = waiting {
                    *older = std::mem::take(older).then(delta);
                } else {
                    self.spill.push_back(SdkCommand::Primary(
                        CollabRequest::PublishActionSurface { delta },
                    ));
                }
            }
            command => {
                // Presence says where someone is now: what waits under the
                // same key is older, and the newer goes to the back.
                if let Some(slot) = command.presence_slot() {
                    self.spill
                        .retain(|parked| parked.presence_slot().as_ref() != Some(&slot));
                }
                self.spill.push_back(command);
            }
        }
        // Arm before the retry so a release racing it is never missed.
        self.state.command_bytes.arm_budget_wake();
        self.flush_spill();
    }

    /// Move parked commands into the queue while it has room.
    fn flush_spill(&mut self) {
        while !self.spill.is_empty() && self.reserve_barrier_slot() {
            let Some(command) = self.spill.pop_front() else {
                return;
            };
            match admit_sdk_command(&self.outbound_tx, command, &self.state) {
                Ok(()) => {}
                Err(Refused {
                    command: Some(command),
                    ..
                }) => {
                    self.spill.push_front(command);
                    self.state.command_bytes.arm_budget_wake();
                    return;
                }
                Err(_) => {
                    // The bridge is gone; nothing parked can be sent.
                    self.spill.clear();
                    return;
                }
            }
        }
    }

    /// Hold [`Self::barrier_slot`] before admitting anything else. False
    /// while the queue is full; the owner is woken when it drains.
    fn reserve_barrier_slot(&mut self) -> bool {
        if self.barrier_slot.is_some() {
            return true;
        }
        let mut retried = false;
        loop {
            match self.outbound_tx.clone().try_reserve_owned() {
                Ok(permit) => {
                    self.barrier_slot = Some(permit);
                    return true;
                }
                Err(tokio_mpsc::error::TrySendError::Full(_)) if !retried => {
                    // Arm before the retry so a release racing it is never missed.
                    self.state.command_bytes.arm_budget_wake();
                    retried = true;
                }
                Err(tokio_mpsc::error::TrySendError::Full(_)) => return false,
                Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                    // Admission reports the closed queue.
                    self.state.fail_closed("outbound command queue");
                    return true;
                }
            }
        }
    }

    /// Queue a publication barrier in the held slot, never in the spill:
    /// it orders only document mutations, and those are never parked.
    fn queue_barrier(&mut self, reply: BarrierReply) -> Result<(), TransportError> {
        if !self.state.is_healthy() {
            return Err(TransportError::Closed);
        }
        if self.reserve_barrier_slot() {
            let Some(slot) = self.barrier_slot.take() else {
                return Err(TransportError::Closed);
            };
            let replies = Arc::new(BarrierReplies::new(reply));
            self.queued_barrier = Arc::downgrade(&replies);
            slot.send(QueuedSdkCommand {
                command: SdkCommand::PublicationBarrier(replies),
                _reservation: self.state.command_bytes.empty_reservation(),
            });
            self.reserve_barrier_slot();
            return Ok(());
        }
        // The slot is spent and nothing was admitted since: this barrier
        // covers exactly what the queued one does.
        let reply = match self.queued_barrier.upgrade() {
            Some(queued) => match queued.join(reply) {
                Ok(()) => return Ok(()),
                Err(reply) => reply,
            },
            None => reply,
        };
        if !self.state.is_healthy() {
            return Err(TransportError::Closed);
        }
        // The barrier that spent the slot was answered (or none ever did):
        // every document mutation before this one has been handled.
        let _ = reply.send(self.state.publication_result());
        Ok(())
    }

    /// Route the process's published commands and device commands through
    /// this transport.
    fn register_command_bridge(&mut self) {
        if let Ok(mut bridge) = PUBLISHED_COMMAND_BRIDGE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
        {
            *bridge = Some(self.outbound_tx.downgrade());
            self.command_bridge_registered = true;
        }
    }
}

impl CollabTransport for BrokerTransport {
    fn connection_snapshot(
        &self,
        route: &RouteSpec,
    ) -> Option<runtime_collab::committed_document::ConnectionSnapshot> {
        if self.status() != TransportStatus::Ready {
            return None;
        }
        self.state
            .connection_snapshots
            .lock()
            .ok()?
            .get(&route.channel_id())
            .cloned()
    }
    fn status(&self) -> TransportStatus {
        match self.state.connect_phase() {
            // Why the worker stopped, when it stopped setting up.
            ConnectPhase::Failed(reason) => TransportStatus::Closed(reason),
            _ if self.worker_stopped => {
                TransportStatus::Closed("collaboration worker stopped".into())
            }
            _ if !self.state.is_healthy() => {
                TransportStatus::Closed("bounded bridge overloaded or disconnected".into())
            }
            ConnectPhase::Connecting => TransportStatus::Connecting,
            ConnectPhase::Connected => TransportStatus::Ready,
        }
    }

    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        let command = match command {
            TransportCommand::Primary(CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                record_kind,
                handle,
            }) => SdkCommand::Hello(PrimaryHello {
                route: RouteSpec {
                    app_id,
                    cluster_id,
                    record,
                },
                record_kind,
                handle,
            }),
            TransportCommand::Primary(request) => SdkCommand::Primary(request),
            TransportCommand::ToRoute(route, request) => SdkCommand::ToRoute(route, request),
            TransportCommand::Join(route, record_kind) => {
                self.state
                    .route_record_kinds
                    .lock_recover()
                    .insert(route.channel_id(), record_kind.as_str().to_owned());
                SdkCommand::Join(route, Some(record_kind.as_str().to_owned()))
            }
            TransportCommand::ConfirmPrimaryRoute => {
                let (reply, pending) = mpsc::sync_channel(1);
                self.enqueue(SdkCommand::PrimaryReady(reply))?;
                self.pending_primary = Some(pending);
                return Ok(());
            }
            TransportCommand::InstallDocumentEncryption {
                route,
                group,
                reply,
            } => SdkCommand::InstallEncryption {
                spec: route,
                group,
                reply,
            },
            TransportCommand::PublishActionDirectory { app_id } => {
                SdkCommand::PublishActionDirectory {
                    app_id,
                    requests: self.action_directory_tx.clone(),
                }
            }
        };
        self.enqueue(command)
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        self.document_turns.clear();
        self.invocation_owners.clear();
        self.state.begin_poll();
        self.flush_spill();
        if !self.command_bridge_registered
            && matches!(self.state.connect_phase(), ConnectPhase::Connected)
        {
            self.register_command_bridge();
        }
        while let Ok(mut evt) = self.inbound_rx.try_recv() {
            match evt._turn.take() {
                Some(EventTurn::Invocation(turns, owner)) => {
                    self.document_turns.extend(turns);
                    if let CollabEvent::Invocation { id, .. } = &evt.event {
                        self.invocation_owners.insert(*id, owner);
                    }
                }
                Some(EventTurn::Snapshot(turn)) => drop(turn),
                None => {}
            }
            out.push(evt.event);
        }
    }

    fn set_document_owner_routes(&mut self, routes: Vec<RouteSpec>, document_generation: u64) {
        self.state.document_reads.set_document_owner_routes(
            routes.iter().map(RouteSpec::channel_id).collect(),
            document_generation,
        );
    }

    fn invocation_owner_current(&mut self, id: u64) -> bool {
        self.invocation_owners
            .remove(&id)
            .is_none_or(|owner| self.state.document_reads.owns(&owner))
    }

    fn finish_event_batch(&mut self) {
        self.document_turns.clear();
        self.invocation_owners.clear();
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        let result = match self.pending_primary.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.worker_stopped = true;
                None
            }
        };
        self.pending_primary = None;
        result
    }

    fn join_hold(&self) -> Option<super::JoinHold> {
        self.state.join_hold.lock_recover().clone()
    }

    fn wait_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        // No clock ends this wait. The worker answers once the broker has
        // answered the join, or the join failed; under load that takes what
        // it takes. A worker that stops drops the reply unanswered, which
        // ends the wait at once.
        let result = self.pending_primary.as_ref()?.recv().ok();
        if result.is_none() {
            self.worker_stopped = true;
        }
        self.pending_primary = None;
        result
    }

    fn take_action_directory_requests(&mut self, out: &mut Vec<ActionDirectoryRequest>) {
        while let Ok(request) = self.action_directory_rx.try_recv() {
            out.push(request);
        }
    }

    fn publication_barrier(
        &mut self,
    ) -> Result<DocumentPublicationReceipt, DocumentPublicationError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        if let Err(error) = self.queue_barrier(reply) {
            return Err(self
                .state
                .publication_result()
                .err()
                .unwrap_or(DocumentPublicationError::Failed(format!("{error:?}"))));
        }
        let state = self.state.clone();
        Ok(DocumentPublicationReceipt::new(receiver)
            .with_disconnect_cause(move || state.publication_result().err()))
    }

    fn document_publishable(&self, channel: Option<&str>) -> bool {
        // Withdrawn health withdraws read readiness at once: the route's
        // reader only clears its gate when it next wakes, and a route whose
        // transport failed elsewhere (a refused commit) is not publishable.
        self.state.is_healthy()
            && channel.is_some_and(|channel| self.state.document_reads.publishable(channel))
    }

    #[cfg(any(test, feature = "test-support"))]
    fn controlled_request_whole_read_for_test(&self, channel: &str) -> bool {
        // The owner's own ask (`initial`); a staged read it has not merged
        // yet is answered instead, and nothing is asked.
        self.state.document_reads.required(channel)
            && self.state.document_reads.initial(channel).is_none()
    }

    #[cfg(any(test, feature = "test-support"))]
    fn controlled_document_read_phase_for_test(&self, channel: &str) -> Option<serde_json::Value> {
        let phase = self.state.document_reads.readiness_diagnostic(channel)?;
        Some(serde_json::json!({
            "required": phase.required,
            "staged": phase.staged,
            "applied": phase.applied,
            "refresh_requested": phase.refresh_requested,
            "join_failure": self.route_join_failure(channel),
        }))
    }

    fn joined_record_kind(&self, channel: &str) -> Option<crate::record::RecordKind> {
        self.state
            .route_record_kinds
            .lock_recover()
            .get(channel)
            .map(crate::record::RecordKind::new)
    }

    fn route_join_failure(&self, channel: &str) -> Option<String> {
        self.state
            .route_join_failures
            .lock_recover()
            .get(channel)
            .cloned()
    }

    fn document_route_joining(&self, channel: &str) -> bool {
        self.state.is_healthy()
            && self.state.document_reads.resolving(channel)
            && self.route_join_failure(channel).is_none()
    }

    fn take_unpublished_route(&self, channel: &str) -> bool {
        self.state.unpublished_routes.lock_recover().remove(channel)
    }

    fn take_action_surface_failed(&mut self) -> bool {
        self.state
            .action_surface_failed
            .swap(false, Ordering::AcqRel)
    }

    fn action_directory_publication_counts(&self) -> Option<(u64, u64)> {
        self.state
            .directory_observation_enabled
            .store(true, Ordering::Release);
        Some((
            self.state.directory_rpc_successes.load(Ordering::Acquire),
            self.state.directory_rpc_failures.load(Ordering::Acquire),
        ))
    }

    fn send_native_media(
        &mut self,
        channel: String,
        request_id: u64,
        command: runtime_collab::native_media::Request,
    ) -> bool {
        self.state.is_healthy()
            && self
                .media
                .as_ref()
                .is_some_and(|media| media.send(channel, request_id, command))
    }

    fn document_publication_failed(&self) -> bool {
        self.state.publication_result().is_err()
    }

    fn publication_outcome(&self) -> Option<DocumentPublicationError> {
        self.state.publication_result().err()
    }

    fn refused_change(&self) -> Option<String> {
        self.state.refused_change()
    }

    fn fail_closed(&self, reason: &'static str) {
        self.state.fail_closed(reason);
    }

    fn private_window_binding(&self, route: &RouteSpec) -> Option<u64> {
        self.state
            .is_healthy()
            .then(|| {
                self.state
                    .document_reads
                    .private_window(&route.channel_id())
            })
            .flatten()
    }

    fn committed_document_required(&self, channel: &str) -> bool {
        self.state.document_reads.required(channel)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn prepare_committed_document(
        &mut self,
        route: RouteSpec,
        record_kind: &str,
    ) -> Option<CommittedDocument> {
        let key = route.channel_id();
        if self.state.document_reads.joining(&key) {
            let _ = self.enqueue(SdkCommand::Join(route, Some(record_kind.to_owned())));
        }
        self.state.document_reads.initial(&key)
    }
}

type BarrierReply = mpsc::SyncSender<Result<(), DocumentPublicationError>>;

/// Replies owed by one queued publication barrier. Barriers submitted while
/// it waits and nothing else could be admitted join it. Dropping the command
/// unanswered drops the replies, which their receipts report as a failure.
struct BarrierReplies(std::sync::Mutex<Option<Vec<BarrierReply>>>);

impl BarrierReplies {
    fn new(reply: BarrierReply) -> Self {
        Self(std::sync::Mutex::new(Some(vec![reply])))
    }

    /// Wait on this barrier's answer, unless it was already given.
    fn join(&self, reply: BarrierReply) -> Result<(), BarrierReply> {
        match &mut *self.0.lock_recover() {
            Some(replies) => {
                replies.push(reply);
                Ok(())
            }
            None => Err(reply),
        }
    }

    fn answer(&self, result: &Result<(), DocumentPublicationError>) {
        let replies = self.0.lock_recover().take();
        for reply in replies.into_iter().flatten() {
            let _ = reply.send(result.clone());
        }
    }
}

enum SdkCommand {
    /// One private service request to the session on `route`, the app's own
    /// route. A window asks as soon as it is built, which the live launch
    /// does while the route is still joining: the `Hello` naming the route
    /// was queued before it, so the loop has joined it by the time it
    /// serves the request.
    BrowserServiceSession {
        route: RouteSpec,
        command: runtime_collab::BrowserServiceSessionCommand,
        wake: Option<crate::WakeHandle>,
        reply: mpsc::SyncSender<Result<runtime_collab::BrowserServiceSessionReply, String>>,
    },
    /// Announce the primary route.
    Hello(PrimaryHello),
    Primary(CollabRequest),
    /// Join a route, naming the resource kind of its record when known.
    Join(RouteSpec, Option<String>),
    ToRoute(RouteSpec, CollabRequest),
    PrimaryReady(mpsc::SyncSender<Result<RouteSpec, String>>),
    /// Answered once every document mutation queued before it has been
    /// handed to the broker, with the first one that was not.
    PublicationBarrier(Arc<BarrierReplies>),
    InstallEncryption {
        spec: RouteSpec,
        group: Box<channel_crypto::ChannelGroup>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    PublishActionDirectory {
        app_id: String,
        requests: mpsc::SyncSender<ActionDirectoryRequest>,
    },
    WifiContinuity {
        app_id: String,
        command: runtime_collab::WifiContinuityCommand,
        reply: mpsc::SyncSender<Result<runtime_collab::WifiContinuityReply, String>>,
    },
    Speakers {
        app_id: String,
        command: runtime_collab::SpeakerCommand,
        reply: mpsc::SyncSender<Result<runtime_collab::SpeakerReply, String>>,
    },
    DisplayControl {
        origin: runtime_collab::settings::DisplayOrigin,
        command: Vec<u8>,
        reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
    },
    /// Read, watch or change one of the session's published resources.
    Published(PublishedCommand),
    /// Send the app's unsent settings writes over the primary route.
    FlushSettings,
    /// Send the app's live activity over the primary route.
    FlushLiveActivity,
    /// Report the channel-key schedule once the key round in flight settles.
    #[cfg(test)]
    KeyScheduleProbe(mpsc::SyncSender<KeyScheduleSeen>),
}

struct PrimaryHello {
    route: RouteSpec,
    record_kind: Option<String>,
    handle: Option<String>,
}

impl SdkCommand {
    /// Automerge bytes of a document mutation command, if this is one.
    fn document_len(&self) -> Option<usize> {
        match self {
            Self::Primary(CollabRequest::Document { mutation, .. })
            | Self::ToRoute(_, CollabRequest::Document { mutation, .. }) => {
                Some(mutation.data().len())
            }
            // Content a record references travels like its documents: its
            // own budget, retried when full, never closing the connection.
            Self::Primary(CollabRequest::OfferContent { bytes, .. })
            | Self::ToRoute(_, CollabRequest::OfferContent { bytes, .. }) => Some(bytes.len()),
            _ => None,
        }
    }

    /// Release spare capacity so the retained budget charges only the bytes
    /// that will actually be sent.
    fn shrink_document(&mut self) {
        if let Self::Primary(CollabRequest::Document { mutation, .. })
        | Self::ToRoute(_, CollabRequest::Document { mutation, .. }) = self
        {
            match mutation {
                DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                    data.shrink_to_fit();
                }
            }
        }
    }

    /// The route and key a presence word is said under: a newer word under
    /// the same one replaces it.
    fn presence_slot(&self) -> Option<(Option<RouteSpec>, collab_types::presence::PresenceKey)> {
        match self {
            Self::Primary(CollabRequest::Presence { key, .. }) => Some((None, key.clone())),
            Self::ToRoute(route, CollabRequest::Presence { key, .. }) => {
                Some((Some(route.clone()), key.clone()))
            }
            _ => None,
        }
    }

    /// Presence is visual, short-lived state. All other frames represent a
    /// durable state change, an ordered request, or a lifecycle result.
    fn is_transient_presence(&self) -> bool {
        matches!(
            self,
            Self::Primary(CollabRequest::Presence { .. })
                | Self::ToRoute(_, CollabRequest::Presence { .. })
        )
    }
}

/// Content-free names share the meter's exhaustive, platform-aware match.
fn event_kind(event: &CollabEvent) -> &'static str {
    event_diagnostics(event).0
}

fn event_bytes(event: &CollabEvent) -> Option<usize> {
    event_diagnostics(event).1
}

/// Shared failure latch. The UI reads it through `is_connected()` even when
/// overload is detected by a subscription task on the shovel thread.
#[derive(Clone)]
pub(crate) struct BridgeState {
    connection_snapshots: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                String,
                runtime_collab::committed_document::ConnectionSnapshot,
            >,
        >,
    >,
    document_reads: committed_document::Reads,
    /// The workspace commits this endpoint has seen, and the parts of
    /// actions still waiting for the rest of them: what the app applies as
    /// one action, and what history reads.
    commits: Arc<std::sync::Mutex<crate::commits::Commits>>,
    /// Why each route whose last join failed failed, by channel: a route
    /// that never joined says so instead of staying unready without a word.
    route_join_failures: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// The record routes whose queued changes were let go since their owner
    /// last asked ([`CollabTransport::take_unpublished_route`]).
    unpublished_routes: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// The kind each joined route's record holds, by channel: a change that
    /// arrives on a route awaiting its first join re-joins it naming the kind
    /// the owner opened the record with.
    route_record_kinds: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    healthy: Arc<AtomicBool>,
    publication_failure: Arc<std::sync::Mutex<Option<DocumentPublicationError>>>,
    /// Per record route, the last change the peer answered but would not
    /// accept, when nothing later has gone through on that route. It is not
    /// a publication failure: the bridge stays up and later changes and
    /// publications go on, so the owner is told which change to reconcile
    /// and never mistakes it for a lost connection (see
    /// [`Self::note_refused_change`]). Another record's change going through
    /// says nothing about this one.
    last_refused_change: Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
    /// The broker refused or failed an action-surface publication; the owner
    /// takes this and republishes.
    action_surface_failed: Arc<AtomicBool>,
    directory_rpc_successes: Arc<AtomicU64>,
    directory_rpc_failures: Arc<AtomicU64>,
    directory_observation_enabled: Arc<AtomicBool>,
    command_bytes: ByteBudget,
    event_bytes: ByteBudget,
    document_command_bytes: ByteBudget,
    document_event_bytes: ByteBudget,
    catalogue_event_bytes: ByteBudget,
    owner_wake: crate::wake::WakeHandle,
    wake_pending: Arc<AtomicBool>,
    /// How far the shovel has got establishing the session. Written by the
    /// shovel, read by the owner through [`CollabTransport::status`].
    connect: Arc<std::sync::Mutex<ConnectPhase>>,
    /// What the broker says holds up the pending primary join, while it is
    /// pending. Written by the shovel, read by the owner through
    /// [`CollabTransport::join_hold`].
    join_hold: Arc<std::sync::Mutex<Option<super::JoinHold>>>,
    /// How often a queued document mutation found its route not yet
    /// publishable and waited again.
    #[cfg(test)]
    publication_gate_waits: Arc<std::sync::atomic::AtomicUsize>,
    /// The shovel thread's tid, which a test holds the connection idle
    /// against and counts its wakes on; -1 before the thread starts.
    #[cfg(all(test, unix))]
    shovel_tid: Arc<std::sync::atomic::AtomicI32>,
}

/// Progress of the shovel's connection setup.
#[derive(Clone, Debug)]
enum ConnectPhase {
    /// Loading the join identity and reaching the broker.
    Connecting,
    /// The broker client is up; commands are being served.
    Connected,
    /// Setup failed; the shovel has exited.
    Failed(String),
}

/// Lock-free aggregate reservation for heap payload retained by one queue.
/// A reservation moves with its queued value and releases automatically when
/// the receiver consumes or drops that value, waking anyone waiting for room.
#[derive(Clone)]
struct ByteBudget(
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
    Option<BudgetWake>,
);

/// Wakes the owner once outbound room frees after one of its sends was
/// refused as busy. The owner has no timer of its own for that retry, so
/// without this a refused edit would wait for unrelated input.
#[derive(Clone)]
struct BudgetWake {
    wanted: Arc<AtomicBool>,
    owner: crate::wake::WakeHandle,
}

struct ByteReservation {
    budget: ByteBudget,
    bytes: usize,
}

impl ByteBudget {
    fn new() -> Self {
        Self(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(tokio::sync::Notify::new()),
            None,
        )
    }

    fn with_budget_wake(workspace: BudgetWake) -> Self {
        Self(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(tokio::sync::Notify::new()),
            Some(workspace),
        )
    }

    /// Ask for an owner wake at the next release of any reservation sharing
    /// this budget's latch. Arm before re-checking for workspace, so a release
    /// racing the refusal is never missed.
    fn arm_budget_wake(&self) {
        if let Some(workspace) = &self.2 {
            workspace.wanted.store(true, Ordering::Release);
        }
    }

    fn reserve(&self, bytes: usize, limit: usize) -> Option<ByteReservation> {
        self.reserve_with(bytes, limit, false)
    }

    /// A reservation of nothing, for a command that rides a held queue slot.
    fn empty_reservation(&self) -> ByteReservation {
        ByteReservation {
            budget: self.clone(),
            bytes: 0,
        }
    }

    /// Like [`Self::reserve`], but an otherwise empty budget admits one item
    /// larger than `limit`. Retained memory stays bounded by
    /// `max(limit, largest single item)`, and a lone large item can always
    /// make progress instead of waiting for space that can never appear.
    fn reserve_sole(&self, bytes: usize, limit: usize) -> Option<ByteReservation> {
        self.reserve_with(bytes, limit, true)
    }

    /// Wait until [`Self::reserve_sole`] of `bytes` would succeed: woken by
    /// each release, and at once when there is room already.
    async fn until_capacity(&self, bytes: usize, limit: usize) {
        loop {
            let released = self.1.notified();
            tokio::pin!(released);
            // Enabled before looking, so a release in between is not missed.
            released.as_mut().enable();
            let used = self.0.load(Ordering::Acquire);
            if used == 0 || used.checked_add(bytes).is_some_and(|next| next <= limit) {
                return;
            }
            released.await;
        }
    }

    fn reserve_with(&self, bytes: usize, limit: usize, sole: bool) -> Option<ByteReservation> {
        let mut used = self.0.load(Ordering::Acquire);
        loop {
            let next = used.checked_add(bytes)?;
            if next > limit && !(sole && used == 0) {
                return None;
            }
            match self
                .0
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Some(ByteReservation {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(observed) => used = observed,
            }
        }
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.1.notify_waiters();
        if let Some(workspace) = &self.budget.2
            && workspace.wanted.swap(false, Ordering::AcqRel)
        {
            workspace.owner.notify_events();
        }
    }
}

struct QueuedSdkCommand {
    command: SdkCommand,
    _reservation: ByteReservation,
}

enum EventTurn {
    Snapshot(tokio::sync::OwnedSemaphorePermit),
    /// An accepted invocation: its native document turns, which transfer to
    /// the owner until its dispatch batch ends, and the document owner it
    /// arrived for.
    Invocation(
        Vec<tokio::sync::OwnedSemaphorePermit>,
        committed_document::DeliveryOwner,
    ),
}

struct QueuedCollabEvent {
    event: CollabEvent,
    _reservation: ByteReservation,
    /// Snapshot turns release on dequeue. Native document turns transfer to
    /// the owner until its dispatch batch ends.
    _turn: Option<EventTurn>,
}

impl BridgeState {
    pub(crate) fn new() -> Self {
        let owner_wake = crate::wake::WakeHandle::new();
        // Both outbound budgets share one latch: a refused document send is
        // queued behind commands of either kind.
        let outbound_wake = BudgetWake {
            wanted: Arc::new(AtomicBool::new(false)),
            owner: owner_wake.clone(),
        };
        Self {
            connection_snapshots: Arc::default(),
            document_reads: committed_document::Reads::default(),
            commits: Arc::default(),
            route_join_failures: Arc::default(),
            unpublished_routes: Arc::default(),
            route_record_kinds: Arc::default(),
            healthy: Arc::new(AtomicBool::new(true)),
            publication_failure: Arc::new(std::sync::Mutex::new(None)),
            last_refused_change: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            action_surface_failed: Arc::new(AtomicBool::new(false)),
            directory_rpc_successes: Arc::new(AtomicU64::new(0)),
            directory_rpc_failures: Arc::new(AtomicU64::new(0)),
            directory_observation_enabled: Arc::new(AtomicBool::new(false)),
            command_bytes: ByteBudget::with_budget_wake(outbound_wake.clone()),
            event_bytes: ByteBudget::new(),
            document_command_bytes: ByteBudget::with_budget_wake(outbound_wake),
            document_event_bytes: ByteBudget::new(),
            catalogue_event_bytes: ByteBudget::new(),
            owner_wake,
            wake_pending: Arc::new(AtomicBool::new(false)),
            connect: Arc::new(std::sync::Mutex::new(ConnectPhase::Connecting)),
            join_hold: Arc::default(),
            #[cfg(test)]
            publication_gate_waits: Arc::default(),
            #[cfg(all(test, unix))]
            shovel_tid: Arc::new(std::sync::atomic::AtomicI32::new(-1)),
        }
    }

    /// The tid of the thread serving this connection's broker, once it runs.
    #[cfg(all(test, unix))]
    fn shovel_tid(&self) -> Option<i32> {
        let tid = self.shovel_tid.load(Ordering::Acquire);
        (tid > 0).then_some(tid)
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// The workspace commits this endpoint has seen. A poisoned lock is a
    /// panic in the gate: the endpoint goes on with what it has.
    pub(crate) fn commits(&self) -> std::sync::MutexGuard<'_, crate::commits::Commits> {
        self.commits.lock_recover()
    }

    fn connect_phase(&self) -> ConnectPhase {
        self.connect.lock_recover().clone()
    }

    /// Record what holds up the pending primary join, waking the owner when
    /// it changed.
    fn set_join_hold(&self, hold: Option<super::JoinHold>) {
        let changed = {
            let mut current = self.join_hold.lock_recover();
            let changed = *current != hold;
            *current = hold;
            changed
        };
        if changed {
            self.notify_events();
        }
    }

    /// Record setup progress and wake the owner to observe it.
    fn set_connect_phase(&self, phase: ConnectPhase) {
        *self.connect.lock_recover() = phase;
        self.notify_events();
    }

    fn notify_events(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            self.owner_wake.notify_events();
        }
    }

    fn begin_poll(&self) {
        // Reset BEFORE draining: a publication racing the drain either lands
        // in this batch or schedules another wake. Clearing after draining
        // could swallow a notification for an event we did not consume.
        self.wake_pending.store(false, Ordering::Release);
    }

    /// The outcome of everything published so far: a terminal failure, else
    /// the change the peer answered but would not accept, if nothing later
    /// has gone through. A refusal answers for the run of changes it covers
    /// and survives being read; it is withdrawn by the next change that goes
    /// through ([`Self::change_sent`]), never by the reading.
    fn publication_result(&self) -> Result<(), DocumentPublicationError> {
        self.publication_failure.lock_recover().clone().map_or_else(
            || match self.refused_change() {
                Some(said) => Err(DocumentPublicationError::Refused(said)),
                None => Ok(()),
            },
            Err,
        )
    }

    /// The record route `key` did not take what was queued for it: that
    /// record alone is unpublished, and its owner hears so and sends it
    /// whole once the route is open. Every other route and the app's own
    /// carry on.
    fn route_unpublished(&self, key: String) {
        tracing::warn!(
            route = key,
            "collab: a record's route was not open for its changes; they go out once it is"
        );
        self.unpublished_routes.lock_recover().insert(key);
        self.notify_events();
    }

    fn record_publication_failure(&self, error: DocumentPublicationError) {
        self.publication_failure.lock_recover().get_or_insert(error);
    }

    fn publication_failed(
        &self,
        context: &'static str,
        error: impl std::fmt::Display,
        disconnect: bool,
    ) -> DocumentPublicationError {
        let failure = DocumentPublicationError::Failed(format!("{context}: {error}"));
        self.record_publication_failure(failure.clone());
        if disconnect {
            self.fail_closed(context);
        }
        failure
    }

    /// The identity service declined to sign a change on the workspace's
    /// behalf. Later changes build on the declined one, so publication stops
    /// as it does for any failure; but the reason is kept apart, so nobody is
    /// told they are offline when the workspace said no.
    fn publication_refused(&self, error: impl std::fmt::Display) -> DocumentPublicationError {
        let refusal = DocumentPublicationError::Refused(error.to_string());
        self.record_publication_failure(refusal.clone());
        self.fail_closed("document signing refused");
        refusal
    }

    /// The peer answered one change with a definite refusal: it will not take
    /// this change. The transport carried the answer, so the bridge stays
    /// open and later changes and publications go through. Only that change
    /// is not accepted, and the owner is told so it can reconcile its local
    /// copy instead of waiting for a reconnect that would change nothing.
    fn note_refused_change(
        &self,
        route: &RouteSpec,
        context: &'static str,
        error: impl std::fmt::Display,
    ) {
        let refusal = DocumentPublicationError::Refused(format!("{context}: {error}"));
        let said = refusal.to_string();
        tracing::warn!(
            direction = context,
            %error,
            "collab: the broker refused one document change; this bridge stays open"
        );
        self.last_refused_change
            .lock_recover()
            .insert(route.channel_id(), said);
        self.notify_events();
    }

    /// A change the peer refused while nothing later has been sent on its
    /// route, if any.
    fn refused_change(&self) -> Option<String> {
        self.last_refused_change
            .lock_recover()
            .values()
            .next()
            .cloned()
    }

    /// A change of `route` went through: nothing of it is waiting to be
    /// reconciled any more.
    fn change_sent(&self, route: &RouteSpec) {
        let cleared = self
            .last_refused_change
            .lock_recover()
            .remove(&route.channel_id())
            .is_some();
        if cleared {
            self.notify_events();
        }
    }

    fn fail_closed(&self, direction: &'static str) {
        self.fail_closed_with_rpc_error(direction, None);
    }

    /// The app stopped reading its events (it closed, or its manager was
    /// dropped): collaboration ends as for [`Self::fail_closed`], logged as
    /// the ordinary end it is rather than an overload.
    fn receiver_gone(&self, direction: &'static str) {
        if self.close(direction) {
            tracing::debug!(direction, "collab: the app stopped reading its events");
            self.notify_events();
        }
    }

    /// Withdraw health and fail every waiting publication; whether this
    /// call is the one that ended a healthy bridge.
    fn close(&self, direction: &'static str) -> bool {
        // Latch before withdrawing health or dropping any queued receipt sender.
        self.record_publication_failure(DocumentPublicationError::Failed(direction.into()));
        self.document_reads.close();
        let was_healthy = self.healthy.swap(false, Ordering::AcqRel);
        // Re-notify after health publication: a construction waiter may have
        // consumed the readiness withdrawal while health was still closing.
        self.document_reads.progressed();
        was_healthy
    }

    fn fail_closed_with_rpc_error(
        &self,
        direction: &'static str,
        rejected: Option<(u64, &runtime_portal::NotAvailable)>,
    ) {
        let first = self.close(direction);
        if let Some((invocation_id, error)) = rejected {
            let (rpc_kind, rpc_error, rpc_error_truncated) = bounded_rpc_diagnostic(error);
            tracing::error!(
                direction,
                invocation_id,
                rpc_kind,
                rpc_error,
                rpc_error_truncated,
                "collab: collaboration stopped: {direction}"
            );
        } else {
            tracing::error!(direction, "collab: collaboration stopped: {direction}");
        }
        // A derailed worker has no reader left for the answer: publish it.
        if !first {
            self.notify_events();
        }
    }
}

/// Note that the shovel found nothing left waiting on it while the bridge
/// still claimed to be up. That only happens after walking away from a
/// command it left undelivered, which the owner must not keep believing;
/// every other path into this state already closed the bridge.
fn stranded_break(state: &BridgeState) {
    if state.is_healthy() {
        tracing::error!(
            "collab: the bridge stopped serving while still connected; disabling collaboration"
        );
        state.fail_closed("collaboration worker stopped serving");
    }
}

/// Whether `error` is the peer's definite answer about the request itself:
/// it read the request and would not take it. A lost, cancelled or overloaded
/// call is not an answer — its outcome is unknown, and only that may end
/// collaboration — and neither is a transport failure.
fn refused_by_peer(error: &runtime_portal::NotAvailable) -> bool {
    matches!(error, runtime_portal::NotAvailable::Refused(_))
}

/// Keep the rejected RPC's cause, never its request/update payload. A peer's
/// diagnostic is untrusted text: bound its allocation and remove control
/// characters before placing it in the ordinary session log.
fn bounded_rpc_diagnostic(error: &runtime_portal::NotAvailable) -> (&'static str, String, bool) {
    match error {
        runtime_portal::NotAvailable::Refused(reason) => {
            let mut chars = reason.chars();
            let detail: String = chars
                .by_ref()
                .take(512)
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect();
            ("refused", detail, chars.next().is_some())
        }
        runtime_portal::NotAvailable::ServiceFdMissing => {
            ("service_fd_missing", String::new(), false)
        }
        runtime_portal::NotAvailable::ConnectionFailed(error) => {
            ("connection_failed", format!("{:?}", error.kind()), false)
        }
        runtime_portal::NotAvailable::ProtocolMismatch { expected, got } => (
            "protocol_mismatch",
            format!("expected {expected}, got {got}"),
            false,
        ),
    }
}

/// Non-blocking UI enqueue. Presence may be dropped when the finite queue is
/// full; a durable command that does not fit is refused as busy, and the
/// owning transport parks it until room frees. Overload never disables the
/// bridge.
fn enqueue_sdk_command(
    tx: &tokio_mpsc::Sender<QueuedSdkCommand>,
    command: SdkCommand,
    state: &BridgeState,
) -> bool {
    try_enqueue_sdk_command(tx, command, state).is_ok()
}

/// Why a command was not queued, with the command itself when it is durable
/// and may be retried.
struct Refused {
    error: TransportError,
    command: Option<SdkCommand>,
}

impl Refused {
    fn error(error: TransportError) -> Self {
        Self {
            error,
            command: None,
        }
    }
}

fn try_enqueue_sdk_command(
    tx: &tokio_mpsc::Sender<QueuedSdkCommand>,
    command: SdkCommand,
    state: &BridgeState,
) -> Result<(), TransportError> {
    admit_sdk_command(tx, command, state).map_err(|refused| refused.error)
}

/// Enqueue with an explicit outcome. Document mutations are checked against
/// the transport limit here, before anything is queued, and use their own
/// budget: a full budget is [`TransportError::Busy`] (retry later) and an
/// over-limit change is [`TransportError::TooLarge`]. Other durable commands
/// are also `Busy` when the queue is full, and come back to the caller to
/// retry. Only a queue whose receiver is gone closes the bridge.
fn admit_sdk_command(
    tx: &tokio_mpsc::Sender<QueuedSdkCommand>,
    mut command: SdkCommand,
    state: &BridgeState,
) -> Result<(), Refused> {
    if !state.is_healthy() {
        return Err(Refused::error(TransportError::Closed));
    }
    if let Some(len) = command.document_len() {
        if let Err(collab_types::DocumentMutationEncodeError::TooLarge { actual, max }) =
            collab_types::document_transfer::check_transfer_len(len)
        {
            tracing::error!(
                bytes = actual,
                limit = max,
                "collab: document change exceeds the transfer limit; not sent"
            );
            return Err(Refused::error(TransportError::TooLarge { actual, max }));
        }
        command.shrink_document();
        let bytes = command_bytes(&command).unwrap_or(usize::MAX);
        let budget = &state.document_command_bytes;
        let Some(reservation) = budget
            .reserve_sole(bytes, SDK_DOCUMENT_COMMAND_BYTES)
            .or_else(|| {
                budget.arm_budget_wake();
                budget.reserve_sole(bytes, SDK_DOCUMENT_COMMAND_BYTES)
            })
        else {
            tracing::debug!(
                bytes = len,
                "collab: document budget full; change will be retried"
            );
            return Err(Refused::error(TransportError::Busy));
        };
        return match tx.try_send(QueuedSdkCommand {
            command,
            _reservation: reservation,
        }) {
            Ok(()) => Ok(()),
            Err(tokio_mpsc::error::TrySendError::Full(refused)) => {
                // Release the refused reservation before arming, or its own
                // drop would spend the wake on a retry that finds the queue
                // still full.
                drop(refused);
                budget.arm_budget_wake();
                if tx.capacity() > 0 {
                    state.owner_wake.notify_events();
                }
                Err(Refused::error(TransportError::Busy))
            }
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                state.fail_closed("outbound command queue");
                Err(Refused::error(TransportError::Closed))
            }
        };
    }
    let transient = command.is_transient_presence();
    let bytes = command_bytes(&command).unwrap_or(usize::MAX);
    if bytes > SDK_COMMAND_QUEUE_BYTES && !transient {
        // It can never fit, however long it waits: refuse this command to its
        // sender, and leave the bridge, which did nothing wrong, up.
        tracing::error!(
            bytes,
            limit = SDK_COMMAND_QUEUE_BYTES,
            "collab: command exceeds the outbound budget; not sent"
        );
        return Err(Refused::error(TransportError::TooLarge {
            actual: bytes,
            max: SDK_COMMAND_QUEUE_BYTES,
        }));
    }
    let Some(reservation) = state.command_bytes.reserve(bytes, SDK_COMMAND_QUEUE_BYTES) else {
        return Err(Refused {
            error: TransportError::Busy,
            command: Some(command),
        });
    };
    match tx.try_send(QueuedSdkCommand {
        command,
        _reservation: reservation,
    }) {
        Ok(()) => Ok(()),
        Err(tokio_mpsc::error::TrySendError::Full(refused)) => {
            let QueuedSdkCommand {
                command,
                _reservation,
            } = refused;
            drop(_reservation);
            Err(Refused {
                error: TransportError::Busy,
                command: Some(command),
            })
        }
        Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
            state.fail_closed("outbound command queue");
            Err(Refused::error(TransportError::Closed))
        }
    }
}

/// Endpoint reassembly of segmented document mutations, shared by every
/// route of one manager so the memory bound covers them all. Live traffic
/// and history pages are tracked separately because history arrives newest
/// page first.
#[derive(Default)]
struct DocumentReassembly {
    live: std::sync::Mutex<collab_types::DocumentReassembler>,
    history: std::sync::Mutex<collab_types::DocumentReassembler>,
}

impl DocumentReassembly {
    /// Feed one validated document envelope and its endpoint payload (MLS
    /// plaintext, or `None` for cleartext). A single-frame mutation passes
    /// through unchanged as `Some(payload)`. A segment is held until its
    /// transfer completes; the complete typed payload is then returned
    /// exactly once as `Some(Some(bytes))`. `None` means nothing to deliver.
    #[allow(
        clippy::option_option,
        reason = "nothing to deliver, a pass-through payload, or a completed transfer are distinct outcomes"
    )]
    fn complete(
        reassembler: &std::sync::Mutex<collab_types::DocumentReassembler>,
        envelope: &SignedOp,
        payload: Option<Vec<u8>>,
    ) -> Option<Option<Vec<u8>>> {
        let frame = match collab_types::DocumentFrame::decode(
            payload.as_deref().unwrap_or(&envelope.payload),
        ) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(%error, "collab: document frame does not decode; dropping");
                return None;
            }
        };
        let segment = match frame {
            collab_types::DocumentFrame::Whole(_) => return Some(payload),
            collab_types::DocumentFrame::Segment(segment) => segment,
        };
        let Ok(mut reassembler) = reassembler.lock() else {
            tracing::error!("collab: document reassembly state is poisoned; dropping segment");
            return None;
        };
        let result = reassembler.accept(envelope, segment, crate::time::now());
        for abandoned in reassembler.take_abandoned() {
            tracing::warn!(
                artifact = ?abandoned.artifact,
                bytes = abandoned.total_len,
                received = abandoned.received_segments,
                segments = abandoned.segment_count,
                reason = ?abandoned.reason,
                "collab: incomplete document transfer abandoned; a fresh snapshot is required"
            );
        }
        drop(reassembler);
        match result {
            Ok(Some(done)) => {
                tracing::debug!(
                    target: "tilo.state",
                    event = "document_reassembled",
                    bytes = done.len,
                    "reassembled segmented document mutation"
                );
                Some(Some(done.payload))
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(%error, "collab: segmented document transfer rejected");
                None
            }
        }
    }
}

/// A one-shot host handoff factory, evaluated on the SDK worker's `LocalSet`.
///
/// The platform host supplies this to shared toolkit bootstrap. It must verify
/// host/app/workspace handoff authority before returning its two connections.
/// This type neither authenticates a transport nor certifies a supplied grant.
/// The actual brokers still enforce certified joins and live signing policy.
/// No inherited service or app-local identity is used if the factory fails.
///
/// Consumed on both success and failure. There is no automatic reconnect. A
/// host retry must create a fresh connector and repeat admission for the same
/// durable identity and requested route; an old handoff must not be replayed.
pub struct HostSessionConnector {
    connect: Box<dyn FnOnce(RouteSpec) -> HostSessionFuture + Send>,
    /// What the connect waits for first: the host's word that the services
    /// it reaches are up, or will not come ([`Self::once_ready`]).
    ready: Option<Box<dyn FnOnce() -> HostReadyFuture + Send>>,
    identity_exchange_timeout: Duration,
    /// The worker's clock starts paused, so it runs ahead whenever the
    /// worker waits: a test of what no clock may decide.
    #[cfg(test)]
    paused_clock: bool,
}

type HostReadyFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>;

trait HostAsyncIo: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> HostAsyncIo for T {}
type HostAsyncStream = Pin<Box<dyn HostAsyncIo>>;

/// The descriptor lane of a host session's collaboration stream, when it
/// has one: what the app hands the broker (Files' selected objects) rides
/// that stream as SCM_RIGHTS ([`runtime_portal::DescriptorStreamSource`]).
type HostDescriptorLane = runtime_portal::DescriptorLane;

type HostSessionFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    (HostAsyncStream, HostAsyncStream, HostDescriptorLane),
                    runtime_portal::NotAvailable,
                >,
            >,
    >,
>;

impl HostSessionConnector {
    /// Supply a factory for an owned collaboration stream and broker signer.
    ///
    /// The factory runs only inside the SDK worker's Tokio runtime/LocalSet and
    /// returns separate owned collaboration and identity transports. Only the
    /// factory and its captures cross threads; its future and streams may be
    /// !Send. It must authenticate both endpoints and enforce the requested
    /// route. Shared code frames and bounds every identity exchange async.
    /// The manager waits for the factory's own outcome: a factory that
    /// cannot connect returns why.
    ///
    /// Merely returning metadata does not establish authentication. Do not use
    /// this constructor as a replacement for host transport/grant validation.
    pub fn new<F, Fut, C, I>(connect: F) -> Self
    where
        F: FnOnce(RouteSpec) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(C, I), runtime_portal::NotAvailable>> + 'static,
        C: AsyncRead + AsyncWrite + 'static,
        I: AsyncRead + AsyncWrite + 'static,
    {
        Self {
            connect: Box::new(move |route| {
                Box::pin(async move {
                    let (collaboration, identity) = connect(route).await?;
                    Ok((
                        Box::pin(collaboration) as HostAsyncStream,
                        Box::pin(identity) as HostAsyncStream,
                        None,
                    ))
                })
            }),
            ready: None,
            identity_exchange_timeout: HOST_IDENTITY_EXCHANGE_TIMEOUT,
            #[cfg(test)]
            paused_clock: false,
        }
    }

    /// [`Self::new`] for a collaboration stream that is a local socket: its
    /// writes also carry the descriptors the app hands the broker, as the
    /// launcher's inherited service connection does, so an in-process app
    /// can hand Files' selected objects over exactly as a launched one. A
    /// host whose streams carry no descriptors gives an empty lane.
    pub fn with_descriptor_lane<F, Fut, C, I>(connect: F) -> Self
    where
        F: FnOnce(RouteSpec) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(C, I), runtime_portal::NotAvailable>> + 'static,
        C: runtime_portal::DescriptorStreamSource + 'static,
        I: AsyncRead + AsyncWrite + 'static,
    {
        Self {
            connect: Box::new(move |route| {
                Box::pin(async move {
                    let (collaboration, identity) = connect(route).await?;
                    let (collaboration, lane) = collaboration.with_descriptor_lane();
                    Ok((
                        Box::pin(collaboration) as HostAsyncStream,
                        Box::pin(identity) as HostAsyncStream,
                        lane,
                    ))
                })
            }),
            ready: None,
            identity_exchange_timeout: HOST_IDENTITY_EXCHANGE_TIMEOUT,
            #[cfg(test)]
            paused_clock: false,
        }
    }
}

impl HostSessionConnector {
    /// Connect only once `ready` resolves: when the host says the services
    /// this connection reaches are listening, or that their start is over.
    /// A host showing an app while it is still starting those services
    /// (a session restoring its tabs before it binds its broker) waits for
    /// them; connecting before then finds no socket and leaves the app
    /// offline.
    /// `ready` must resolve once the host's start is over, whatever its
    /// outcome.
    #[must_use]
    pub fn once_ready<F, Fut>(mut self, ready: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + 'static,
    {
        self.ready = Some(Box::new(move || Box::pin(ready()) as HostReadyFuture));
        self
    }

    /// Run the worker on a paused clock (see `paused_clock`).
    #[cfg(test)]
    fn on_paused_clock(mut self) -> Self {
        self.paused_clock = true;
        self
    }
}

/// How the shovel thread reaches the broker. `Env` is production: the
/// launcher passed `TILO_SVC_COLLAB_FD`. `Fd` is an already-connected typed
/// endpoint, used by tests and headless diagnostics without environment races.
enum SdkConnectSource {
    Env,
    Fd(RawFd),
    Host(HostSessionConnector, RouteSpec),
}

impl SdkConnectSource {
    fn build_client(
        self,
    ) -> Result<
        (runtime_collab::CollabClient, runtime_collab::ConnectionEnd),
        runtime_portal::NotAvailable,
    > {
        match self {
            SdkConnectSource::Host(_, _) => Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::other("host session requires worker-side identity admission"),
            )),
            SdkConnectSource::Env => runtime_collab::try_connect_until_closed(),
            SdkConnectSource::Fd(fd) => {
                // SAFETY: the source owns `fd`, a connected Unix socket, and
                // hands it over here exactly once.
                let fd =
                    unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
                runtime_collab::CollabClient::from_connected_fd(fd)
            }
        }
    }
}

/// Build the identity presentation for a launcher-bound collaboration join.
///
/// Build the certified device presentation used for a launcher-bound join.
/// A launcher identity without a device certificate cannot enter the
/// collaboration session.
/// The participant kind of a headless service that holds no document of its
/// own ([`CollaborationManager::for_service`]). It stays on this side: the
/// broker admits the service as the person whose launch it is.
const SERVICE_PARTICIPANT: &str = "service";

/// The kind the broker is asked to admit a participant of `kind` as.
fn broker_participant_kind(kind: &str) -> &str {
    if kind == SERVICE_PARTICIPANT {
        "person"
    } else {
        kind
    }
}

struct SdkJoinIdentity {
    presentation: Vec<u8>,
    /// Requested broker principal kind. The broker only accepts `agent`
    /// when the named agent is admitted and this certified user may operate
    /// it; ordinary applications always leave this as `person`.
    participant_kind: &'static str,
    /// Retained on the shovel thread so document signing stays off the UI
    /// thread and the workspace broker connection is not consumed at join.
    signer: SdkDocumentSigner,
}

/// Where an endpoint with no identity service asks for its routes' channel
/// keys, as an identity service answers them: the record's workspace, its app,
/// the record, and the key generation the route holds.
pub type EndpointChannelKeys = std::sync::Arc<
    dyn Fn(
            &str,
            &str,
            &str,
            u64,
        )
            -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        + Send
        + Sync,
>;

/// Certified identity for a client connecting to an explicit collaboration
/// endpoint rather than through a launcher-inherited identity service.
#[derive(Clone)]
pub struct CertifiedEndpointIdentity {
    presentation: Vec<u8>,
    device_key: ed25519_dalek::SigningKey,
    signer: EndpointSigner,
}

/// What signs an explicit endpoint's writes and where its channel keys come
/// from.
#[derive(Clone)]
enum EndpointSigner {
    /// A test harness's endpoint: its key, and the channel keys `with_channel_keys`
    /// gave it, if any.
    Harness(Option<EndpointChannelKeys>),
    /// A task agent: the task key its desktop delegated signs, and the
    /// channel keys are always asked of an identity service.
    Delegated(EndpointChannelKeys),
}

impl CertifiedEndpointIdentity {
    pub fn new(presentation: Vec<u8>, device_key: ed25519_dalek::SigningKey) -> Self {
        Self {
            presentation,
            device_key,
            signer: EndpointSigner::Harness(None),
        }
    }

    /// An identity signing with `key` under the delegation `presentation`
    /// carries, its routes' channel keys asked through `keys`.
    pub(crate) fn delegated(
        presentation: Vec<u8>,
        key: ed25519_dalek::SigningKey,
        keys: EndpointChannelKeys,
    ) -> Self {
        Self {
            presentation,
            device_key: key,
            signer: EndpointSigner::Delegated(keys),
        }
    }

    /// The same identity, encrypting its writes under the channel keys
    /// `keys` answers, as an endpoint with an identity service does. Without
    /// them its writes are plaintext, and refused in a workspace that seals them.
    #[must_use]
    pub fn with_channel_keys(mut self, keys: EndpointChannelKeys) -> Self {
        self.signer = EndpointSigner::Harness(Some(keys));
        self
    }

    fn document_signer(&self) -> SdkDocumentSigner {
        let key = Box::new(self.device_key.clone());
        match &self.signer {
            EndpointSigner::Harness(keys) => SdkDocumentSigner::Harness(key, keys.clone()),
            EndpointSigner::Delegated(keys) => SdkDocumentSigner::Delegated(key, keys.clone()),
        }
    }
}

impl std::fmt::Debug for CertifiedEndpointIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertifiedEndpointIdentity")
            .field(
                "presentation",
                &format_args!("{} bytes", self.presentation.len()),
            )
            .field("device_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

// One signing boundary for inherited Unix services and supplied host services.
// This trait is private: platform bootstrap supplies the concrete broker client,
// never a caller-defined signer that could bypass that client's service policy.
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "certified inherited identities are supplied by Unix launch services"
    )
)]
trait SdkCertifiedIdentity: Send + Sync {
    fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>;
    fn device_certificate(&self) -> Option<&[u8]>;
    fn signing_public_key(&self) -> [u8; 32];
    fn sign_collaboration_join(
        &self,
        workspace_id: &str,
    ) -> Result<[u8; 64], runtime_portal::NotAvailable>;
    fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable>;
}

impl SdkCertifiedIdentity for std::sync::Arc<runtime_identity::BrokerSigningIdentity> {
    fn channel_keys(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable> {
        runtime_identity::BrokerSigningIdentity::channel_keys(
            self.as_ref(),
            workspace_id,
            app_id,
            record_id,
            known_generation,
        )
    }
    fn device_certificate(&self) -> Option<&[u8]> {
        self.metadata().device_certificate()
    }
    fn signing_public_key(&self) -> [u8; 32] {
        self.metadata().signing_public_key()
    }
    fn sign_collaboration_join(
        &self,
        workspace_id: &str,
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity::BrokerSigningIdentity::sign_collaboration_join(
            self.as_ref(),
            workspace_id,
        )
    }
    fn sign_collaboration_op(
        &self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
        runtime_identity::BrokerSigningIdentity::sign_collaboration_op(
            self.as_ref(),
            workspace_id,
            app_id,
            record_id,
            unsigned_op,
        )
    }
}

#[cfg(all(test, unix))]
#[path = "broker_identity_tests.rs"]
mod identity_host_fixture;

#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "certified inherited identities are supplied by Unix launch services"
    )
)]
enum SdkDocumentSigner {
    /// Shared so the identity service can be asked off the shovel thread.
    Certified(std::sync::Arc<dyn SdkCertifiedIdentity>),
    /// Explicit-FD harnesses have no launcher identity service. Their
    /// in-process broker accepts this ephemeral signer; a production broker
    /// still rejects the uncertified join before any op reaches policy.
    /// With channel keys to ask, its routes encrypt as a certified one's do.
    Harness(Box<ed25519_dalek::SigningKey>, Option<EndpointChannelKeys>),
    /// A task agent: the task key signs its writes and the delegation its
    /// desktop signed admits it; the channel keys come from its launcher's
    /// identity service, as a launched app's do.
    Delegated(Box<ed25519_dalek::SigningKey>, EndpointChannelKeys),
}

/// Identity state after the shovel thread has entered its `LocalSet`. The host
/// signer may be !Send because it is constructed there and never crosses the
/// thread boundary; inherited Unix and harness identities remain Send.
struct LocalSdkJoinIdentity {
    presentation: Vec<u8>,
    participant_kind: &'static str,
    signer: LocalSdkDocumentSigner,
}

enum LocalSdkDocumentSigner {
    Inherited(SdkDocumentSigner),
    Host(runtime_identity::AsyncBrokerSigningIdentity),
}

impl From<SdkJoinIdentity> for LocalSdkJoinIdentity {
    fn from(identity: SdkJoinIdentity) -> Self {
        Self {
            presentation: identity.presentation,
            participant_kind: identity.participant_kind,
            signer: LocalSdkDocumentSigner::Inherited(identity.signer),
        }
    }
}

fn harness_document_signer() -> SdkDocumentSigner {
    SdkDocumentSigner::Harness(
        Box::new(ed25519_dalek::SigningKey::generate(&mut UnwrapErr(SysRng))),
        None,
    )
}

/// What a harness joins with: a certified presentation in form, so a host
/// reads it, that proves nothing.
fn harness_presentation() -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: "harness".into(),
        device_certificate: b"harness certificate".to_vec(),
        proof: vec![0; 64],
    }
    .encode()
    .unwrap_or_default()
}

#[cfg(unix)]
fn sdk_join_identity(cluster_id: Uuid) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    sdk_join_identity_from(runtime_identity::load()?, cluster_id)
}

#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "certified inherited identities are supplied by Unix launch services"
    )
)]
/// Why a join cannot be certified: the identity service has no device
/// certificate for this workspace.
fn uncertified_device() -> runtime_portal::NotAvailable {
    runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "workspace identity has no certified device",
    ))
}

/// The presentation a certified identity joins with: its device certificate
/// and the proof it signed for the workspace named by `workspace_id`.
fn certified_presentation(
    workspace_id: String,
    device_certificate: Vec<u8>,
    proof: &[u8],
) -> Result<Vec<u8>, runtime_portal::NotAvailable> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id,
        device_certificate,
        proof: proof.to_vec(),
    }
    .encode()
    .map_err(|error| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("encoding certified collaboration join failed: {error}"),
        ))
    })
}

fn sdk_join_identity_from<I: SdkCertifiedIdentity + 'static>(
    identity: I,
    cluster_id: Uuid,
) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    let device_certificate = identity
        .device_certificate()
        .ok_or_else(uncertified_device)?;

    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id)?;
    let presentation = certified_presentation(workspace_id, device_certificate.to_vec(), &proof)?;
    Ok(SdkJoinIdentity {
        presentation,
        participant_kind: "person",
        signer: SdkDocumentSigner::Certified(std::sync::Arc::new(identity)),
    })
}

#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is constructed and retained only on the shovel LocalSet"
)]
async fn sdk_join_identity_from_host(
    mut identity: runtime_identity::AsyncBrokerSigningIdentity,
    cluster_id: Uuid,
) -> Result<LocalSdkJoinIdentity, runtime_portal::NotAvailable> {
    let device_certificate = identity
        .metadata()
        .device_certificate()
        .map(<[u8]>::to_vec)
        .ok_or_else(uncertified_device)?;
    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id).await?;
    let presentation = certified_presentation(workspace_id, device_certificate, &proof)?;
    Ok(LocalSdkJoinIdentity {
        presentation,
        participant_kind: "person",
        signer: LocalSdkDocumentSigner::Host(identity),
    })
}

#[cfg(not(unix))]
fn sdk_join_identity(_cluster_id: Uuid) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    Err(runtime_portal::NotAvailable::ServiceFdMissing)
}

/// What a native launch that could not connect reports: why the service
/// failed.
#[cfg(feature = "native")]
fn native_launch_refusal(error: &runtime_portal::NotAvailable) -> crate::NativeLaunchError {
    crate::NativeLaunchError::Operation(error.to_string())
}

#[cfg(all(test, unix))]
#[path = "background_join_tests.rs"]
mod background_join_tests;

/// Primary-route barrier shared by the blocking constructors and
/// [`CollaborationManager::await_primary_route`].
fn await_primary_route_on(
    transport: &mut dyn CollabTransport,
    primary_route: &mut PrimaryRouteState,
) -> Result<(), runtime_portal::NotAvailable> {
    primary_route.accepted = None;
    if transport
        .submit(TransportCommand::ConfirmPrimaryRoute)
        .is_err()
    {
        return Err(runtime_portal::NotAvailable::ConnectionFailed(
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "collaboration setup thread is unavailable or overloaded",
            ),
        ));
    }
    let answer = transport.wait_primary_route_result();
    primary_route_outcome(transport, primary_route, answer)
}

/// What the worker's `answer` to the latest primary `Hello` means for the
/// route: joined, refused with the broker's reason, or never answered
/// (`None`) because the connection ended first.
fn primary_route_outcome(
    transport: &dyn CollabTransport,
    primary_route: &mut PrimaryRouteState,
    answer: Option<Result<RouteSpec, String>>,
) -> Result<(), runtime_portal::NotAvailable> {
    match answer {
        // The broker answered the join, or the join failed: either way with
        // the broker's own reason.
        Some(result) => primary_route.accept(result).map_err(|error| {
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                error,
            ))
        }),
        None => Err(runtime_portal::NotAvailable::ConnectionFailed(
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                match transport.status() {
                    // The worker stopped before it answered: why it stopped.
                    TransportStatus::Closed(reason) => {
                        format!(
                            "collaboration setup ended before the primary route joined: {reason}"
                        )
                    }
                    // A transport that answers only when polled cannot be
                    // waited on.
                    _ => "collaboration transport cannot wait for its primary route".into(),
                },
            ),
        )),
    }
}

/// A connected broker transport and its acknowledged primary route.
struct ConnectedBroker {
    transport: BrokerTransport,
    primary_route: PrimaryRouteState,
}

/// The broker side of [`CollaborationManager::native_joining_fixture_for_test`].
#[cfg(any(test, feature = "test-support"))]
#[cfg(unix)]
pub struct JoiningProbe {
    answer: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    workspace: tokio::sync::watch::Sender<Result<runtime_collab::WorkspaceStatus, String>>,
    sent: Box<dyn Fn() -> usize>,
}
#[cfg(any(test, feature = "test-support"))]
#[cfg(unix)]
impl JoiningProbe {
    /// The broker answers the primary join it held.
    pub fn answer(&self) {
        if let Some(answer) = self.answer.lock_recover().take() {
            let _ = answer.send(());
        }
    }
    /// The broker fails the primary join it held, with
    /// [`Self::REFUSAL`] as its reason.
    pub fn refuse(&self) {
        drop(self.answer.lock_recover().take());
    }
    /// The reason [`Self::refuse`] gives.
    pub const REFUSAL: &'static str = "fixture join gate dropped";
    /// The broker reports the workspace as `status` from now on.
    pub fn set_workspace_status(&self, status: runtime_collab::WorkspaceStatus) {
        let _ = self.workspace.send(Ok(status));
    }
    /// The broker cannot be asked how the workspace stands, and why.
    pub fn workspace_unreachable(&self, reason: &str) {
        let _ = self.workspace.send(Err(reason.to_owned()));
    }
    /// How many document changes reached the broker.
    pub fn changes_sent(&self) -> usize {
        (self.sent)()
    }
}

#[cfg(any(test, feature = "test-support"))]
#[cfg(unix)]
pub struct NativeRefreshProbe {
    replace_history: Box<dyn Fn(Vec<u8>)>,
    committed_read_count: Box<dyn Fn() -> usize>,
}
#[cfg(any(test, feature = "test-support"))]
#[cfg(unix)]
impl NativeRefreshProbe {
    pub fn committed_read_count(&self) -> usize {
        (self.committed_read_count)()
    }
    pub fn replace_history(&self, history: Vec<u8>) {
        (self.replace_history)(history);
    }
}

impl CollaborationManager {
    /// Connect a headless service of `app_id` in `cluster_id` that holds no
    /// document of its own: it publishes actions and answers them. It never
    /// asks the broker for a committed document, which a workspace holds only for
    /// apps that declare resource kinds; asking would fail the connection.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the launcher did not
    /// hand down a collab service connection.
    pub fn for_service(
        app_id: impl Into<String>,
        cluster_id: Uuid,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            None,
            None,
            None,
            SERVICE_PARTICIPANT,
        )
    }

    /// Explicit typed endpoint for in-process and headless transport
    /// harnesses. Takes ownership of `fd`. It uses an ephemeral signer; a
    /// production broker rejects the uncertified join, so this cannot bypass
    /// workspace admission.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when `fd` does not bootstrap
    /// a working collab service connection.
    pub fn for_route_fd(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    /// Connect to an explicit endpoint with a certified device presentation.
    /// This is the headless equivalent of the launcher-provided identity path.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when `fd` cannot bootstrap a
    /// collaboration service connection.
    pub fn for_route_fd_with_identity(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            handle,
            Some(SdkJoinIdentity {
                presentation: identity.presentation.clone(),
                participant_kind: "person",
                signer: identity.document_signer(),
            }),
            "person",
        )
    }

    /// Synchronous form for deterministic unit harnesses. Production callers
    /// only receive the async entry point above, so identity and
    /// primary-route deadlines do not stall the application's owner/frame
    /// thread.
    #[cfg(test)]
    fn for_route_host_blocking(
        connector: HostSessionConnector,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        let app_id = app_id.into();
        let route = RouteSpec {
            app_id: app_id.clone(),
            cluster_id,
            record: record.clone(),
        };
        Self::connect_route_from(
            SdkConnectSource::Host(connector, route),
            app_id,
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
        .map(Self::from_connected)
    }

    pub fn switch_native_record(
        &mut self,
        checkout: &crate::record::RecordCheckout,
    ) -> Result<(), String> {
        let primary = self
            .primary_route
            .accepted
            .as_ref()
            .ok_or("native primary route unavailable")?
            .route
            .clone();
        let record = (!checkout.id.is_package_primary()).then(|| checkout.id.as_str().to_owned());
        if primary.record == record {
            return Ok(());
        }
        if !self.send(CollabRequest::Hello {
            app_id: primary.app_id,
            cluster_id: primary.cluster_id,
            record,
            record_kind: Some(checkout.kind.as_str().to_owned()),
            handle: crate::local_identity::own_name(),
        }) {
            return Err("native route queue unavailable".into());
        }
        self.await_primary_route()
            .map_err(|error| error.to_string())
    }

    /// The window's manager for a native launch, joined: connected to its
    /// route, or disabled where no service was configured. Waits on the
    /// calling thread for the broker's answer, however long it takes. A live
    /// window starts the join with [`Self::join_native`] instead and goes on
    /// while it runs.
    ///
    /// # Errors
    ///
    /// As [`Self::join_native`], and the broker's refusal of the route, or
    /// why the connection ended before it answered.
    #[cfg(feature = "native")]
    pub fn for_native_launch(
        kind: &crate::record::RecordKind,
        connection: crate::NativeConnection,
    ) -> Result<Self, crate::NativeLaunchError> {
        Self::join_native(kind, connection)?
            .finish_join()
            .map_err(|error| native_launch_refusal(&error))
    }

    /// The window's manager for a native launch, joining its route in the
    /// background, or disabled where no service was configured. The manager
    /// reports [`super::CollaborationState::Connecting`] until the broker
    /// answers; [`Self::poll`] takes the answer as an event, as it does for
    /// every other app, or [`Self::finish_join`] waits for it. No clock
    /// decides the join: the broker's answer, its refusal or the
    /// connection's end does.
    ///
    /// Call it on the thread, and in the wake scope, of the app the manager
    /// is for: its worker wakes that app at each step of the join.
    ///
    /// # Errors
    ///
    /// [`crate::NativeLaunchError::Operation`] where a configured service is
    /// unavailable or this device's identity cannot be loaded.
    #[cfg(feature = "native")]
    pub fn join_native(
        kind: &crate::record::RecordKind,
        connection: crate::NativeConnection,
    ) -> Result<Self, crate::NativeLaunchError> {
        let configured = crate::host::environment()
            .identity
            .collab_service_configured;
        if configured {
            media::audio_output::require_governed();
        }
        // A window the shell opened in this running process has a
        // connection of its own (`ui::window_door`); the one the launcher
        // handed the process is its first window's.
        let source = match connection {
            crate::NativeConnection::Window(None) => return Ok(Self::disabled()),
            crate::NativeConnection::Window(Some(connection)) => {
                SdkConnectSource::Fd(runtime_portal::windows::into_raw(connection))
            }
            crate::NativeConnection::Launch => {
                if runtime_portal::service_fd("collab").is_none() {
                    if configured {
                        return Err(crate::NativeLaunchError::Operation(
                            "configured native collaboration service is unavailable".into(),
                        ));
                    }
                    return Ok(Self::disabled());
                }
                SdkConnectSource::Env
            }
        };
        let (app, workspace) = auto_hello_env();
        if matches!(source, SdkConnectSource::Env) {
            super::channel_settings::install(&app);
        }
        // The window joins as the person this device is, which the session's
        // identity service answers with for the workspace it launched the
        // app into.
        tilo_startup_metrics::startup_mark("collab-connect");
        let identity =
            sdk_join_identity(workspace).map_err(|error| native_launch_refusal(&error))?;
        tilo_startup_metrics::startup_mark("collab-identity-ready");
        Ok(Self::connect_in_background(
            source,
            SdkIdentitySource::Ready(Some(identity)),
            RouteSpec {
                app_id: app,
                cluster_id: workspace,
                record: None,
            },
            Some(kind.as_str().to_owned()),
            crate::local_identity::own_name(),
        ))
    }

    /// The manager of one app instance a tilolang host runs, joining `route`
    /// in the background on `connection`, as the person whose identity the
    /// identity-service connection `identity` holds.
    ///
    /// The session opened both connections for that app's launch and handed
    /// them to the host (`runtime_portal::tilolang_host`): the broker attests
    /// `connection` against the session's launch record of the instance, never
    /// against what the host says, and refuses a hello for another app or
    /// workspace. Nothing is read from the process's environment, which is the
    /// host's, not the app's. `identity` must outlive the manager.
    #[cfg(all(feature = "native", unix))]
    pub fn join_instance(
        kind: &crate::record::RecordKind,
        connection: runtime_portal::windows::Descriptor,
        identity: std::os::fd::BorrowedFd<'_>,
        route: RouteSpec,
        handle: Option<String>,
    ) -> Result<Self, crate::NativeLaunchError> {
        let signing = runtime_identity::inherited::connect_service(identity)
            .map_err(|error| native_launch_refusal(&error))?;
        let identity = sdk_join_identity_from(signing, route.cluster_id)
            .map_err(|error| native_launch_refusal(&error))?;
        Ok(Self::connect_in_background(
            SdkConnectSource::Fd(runtime_portal::windows::into_raw(connection)),
            SdkIdentitySource::Ready(Some(identity)),
            route,
            Some(kind.as_str().to_owned()),
            handle,
        ))
    }

    /// This manager, once the primary route it is joining is joined: waits
    /// on the calling thread for the broker's answer, for as long as it
    /// takes, and fails with the broker's reason or the connection's end. A
    /// manager with nothing to join is returned as it is.
    pub fn finish_join(mut self) -> Result<Self, runtime_portal::NotAvailable> {
        if !self.awaiting_primary {
            return match &self.backend {
                super::Backend::Disabled(reason) if self.sync_expected => {
                    Err(runtime_portal::NotAvailable::ConnectionFailed(
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, reason.clone()),
                    ))
                }
                _ => Ok(self),
            };
        }
        let super::Backend::Transport(transport) = &mut self.backend else {
            return Ok(self);
        };
        let answer = transport.wait_primary_route_result();
        primary_route_outcome(transport.as_ref(), &mut self.primary_route, answer)?;
        self.awaiting_primary = false;
        tilo_startup_metrics::startup_mark("collab-primary-ready");
        // As for a manager connected before it is handed over: its owner's
        // first pump does the connect-time work on the joined route.
        self.reported_state = None;
        Ok(self)
    }

    fn for_route_from(
        source: SdkConnectSource,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::connect_route_from(
            source,
            app_id,
            cluster_id,
            record,
            handle,
            endpoint_identity,
            participant_kind,
        )
        .map(Self::from_connected)
    }

    fn from_connected(connected: ConnectedBroker) -> Self {
        let mut manager = Self::with_transport(Box::new(connected.transport));
        manager.primary_route = connected.primary_route;
        // The bootstrap already acknowledged Ready, but the owner has not
        // observed it. Its first pump must still announce the initial action
        // surface and request the document snapshot on the admitted route.
        manager.reported_state = None;
        manager
    }

    /// Start the broker transport, announce the primary route and wait for
    /// the broker's acknowledgement. Returns `Send` parts so a background
    /// bootstrap task can hand them to the owner thread.
    fn connect_route_from(
        source: SdkConnectSource,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<ConnectedBroker, runtime_portal::NotAvailable> {
        Self::connect_route_kind(
            source,
            RouteSpec {
                app_id,
                cluster_id,
                record,
            },
            None,
            handle,
            endpoint_identity,
            participant_kind,
        )
    }

    /// Initial native creation needs the application's actual root resource
    /// kind before the broker can open the default document.
    fn connect_route_kind(
        source: SdkConnectSource,
        route: RouteSpec,
        record_kind: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<ConnectedBroker, runtime_portal::NotAvailable> {
        let RouteSpec {
            app_id,
            cluster_id,
            record,
        } = route;
        // The connect's phases, so a launch's startup file splits it from the
        // rest of the time before `app-prepare`.
        tilo_startup_metrics::startup_mark("collab-connect");
        let mut identity = match endpoint_identity {
            Some(identity) => Some(identity),
            None if matches!(&source, SdkConnectSource::Host(_, _)) => None,
            None if matches!(&source, SdkConnectSource::Env) => {
                Some(sdk_join_identity(cluster_id)?)
            }
            None => Some(SdkJoinIdentity {
                presentation: harness_presentation(),
                participant_kind: "person",
                signer: harness_document_signer(),
            }),
        };
        if let Some(identity) = &mut identity {
            identity.participant_kind = participant_kind;
        }
        tilo_startup_metrics::startup_mark("collab-identity-ready");
        let mut transport = Self::spawn_sdk(source, identity)?;
        tilo_startup_metrics::startup_mark("collab-transport-ready");
        let mut primary_route = PrimaryRouteState::default();
        primary_route.announce(RouteSpec {
            app_id: app_id.clone(),
            cluster_id,
            record: record.clone(),
        });
        let _ = transport.submit(TransportCommand::Primary(CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            record_kind,
            handle,
        }));
        await_primary_route_on(&mut transport, &mut primary_route)?;
        tilo_startup_metrics::startup_mark("collab-primary-ready");
        Ok(ConnectedBroker {
            transport,
            primary_route,
        })
    }

    /// Wait until the shovel has processed the initial `Hello`, admitted its
    /// primary route (including the initial decision catalogue for an agent).
    /// Without this barrier a caller can publish transient presence immediately after
    /// construction, before the broker knows where to deliver it.
    fn await_primary_route(&mut self) -> Result<(), runtime_portal::NotAvailable> {
        self.primary_route.accepted = None;
        let super::Backend::Transport(transport) = &mut self.backend else {
            return Err(runtime_portal::NotAvailable::ServiceFdMissing);
        };
        await_primary_route_on(transport.as_mut(), &mut self.primary_route)
    }

    /// Connect a headless participant as a first-class workspace agent.
    /// Its friendly id is both the visible handle and the principal whose
    /// scoped capabilities the broker evaluates.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the collaboration
    /// service or certified join identity is unavailable.
    pub fn for_agent_route(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            None,
            "agent",
        )
    }

    /// Explicit-socket counterpart to [`Self::for_agent_route`]. Production
    /// still requires a certified endpoint identity; this method merely
    /// preserves the agent principal kind for diagnostic transports.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the socket cannot be
    /// joined or the route identity cannot be established.
    pub fn for_agent_route_fd(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            None,
            "agent",
        )
    }

    /// Certified explicit-socket agent connection used by out-of-process
    /// runtimes that do not inherit the launcher's service descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the socket or certified
    /// route join fails.
    pub fn for_agent_route_fd_with_identity(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            Some(SdkJoinIdentity {
                presentation: identity.presentation.clone(),
                participant_kind: "agent",
                signer: identity.document_signer(),
            }),
            "agent",
        )
    }

    pub(super) fn new_scoped(record: Option<String>, record_kind: Option<String>) -> Self {
        // Without an inherited service there is nothing to reach; say so
        // without starting a worker.
        if runtime_portal::service_fd("collab").is_none() {
            return Self::disabled();
        }
        let (app_id, cluster_id) = auto_hello_env();
        super::channel_settings::install(&app_id);
        Self::connect_in_background(
            SdkConnectSource::Env,
            SdkIdentitySource::Inherited(cluster_id),
            RouteSpec {
                app_id,
                cluster_id,
                record,
            },
            record_kind,
            crate::local_identity::own_name(),
        )
    }

    /// A manager on `route`, reached through the host's `connector` as an
    /// app it launched, connecting in the background: the manager reports
    /// [`super::CollaborationState::Connecting`] until the broker
    /// acknowledges the route. For a host showing an app inside its own
    /// process.
    pub fn for_route_host_in_background(
        connector: HostSessionConnector,
        route: RouteSpec,
        record_kind: Option<String>,
    ) -> Self {
        Self::connect_in_background(
            SdkConnectSource::Host(connector, route.clone()),
            SdkIdentitySource::Ready(None),
            route,
            record_kind,
            None,
        )
    }

    /// Start the broker transport and announce `route` without waiting for
    /// either. The worker loads the join identity, connects and joins; the
    /// manager reports [`super::CollaborationState::Connecting`] until
    /// [`Self::poll`] sees the primary route acknowledged, and the owner is
    /// woken at each step.
    fn connect_in_background(
        source: SdkConnectSource,
        identity: SdkIdentitySource,
        route: RouteSpec,
        record_kind: Option<String>,
        handle: Option<String>,
    ) -> Self {
        let sdk = match Self::start_sdk(source, identity) {
            Ok((sdk, _init)) => sdk,
            Err(error) => {
                // A route was asked for, so this document was meant to sync:
                // failing to reach the service is working offline.
                let mut manager = Self::offline(error.to_string());
                manager.sync_expected = true;
                return manager;
            }
        };
        let mut manager = Self::with_transport(Box::new(sdk));
        let RouteSpec {
            app_id,
            cluster_id,
            record,
        } = route;
        manager.send(CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            record_kind,
            handle,
        });
        manager.confirm_primary_route_in_background();
        manager
    }

    /// Real socket-backed SDK transport for App lifecycle integration tests.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn connected_for_lifecycle_test() -> Self {
        tests::connected_manager()
    }

    /// The routes an accepted invocation waits to be current on before it
    /// runs, as the owner last set them.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn invocation_document_routes_for_test(&self) -> Vec<String> {
        let Backend::Transport(transport) = &self.backend else {
            panic!("a broker transport");
        };
        transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
            .expect("a broker transport")
            .state
            .document_reads
            .document_owner_routes("")
    }

    /// Retire the actual socket-backed capture generation, as a rejoin does.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn retire_document_capture_for_test(&self, route: &RouteSpec) {
        let Backend::Transport(transport) = &self.backend else {
            panic!("a broker transport");
        };
        let transport = transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
            .expect("a broker transport");
        drop(
            transport
                .state
                .document_reads
                .close_for_test(&route.channel_id()),
        );
    }

    /// Block until the broker worker has finished every command submitted so
    /// far. A route join enqueues the route's initial action catalogue before
    /// its command completes, so the next [`Self::poll`] then sees everything
    /// those commands produced. Panics at the hang bound.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn wait_for_commands_for_test(&self) {
        let Backend::Transport(transport) = &self.backend else {
            panic!("a broker transport");
        };
        let state = &transport
            .as_any()
            .and_then(|any| any.downcast_ref::<BrokerTransport>())
            .expect("a broker transport")
            .state;
        let deadline = tilo_test_env::hang_deadline();
        while state.command_bytes.0.load(Ordering::Acquire) != 0
            || state.document_command_bytes.0.load(Ordering::Acquire) != 0
        {
            assert!(
                crate::time::now() < deadline,
                "broker worker still had commands in flight after {:?}",
                tilo_test_env::hang_bound()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Real socket-backed SDK transport plus a probe returning every signed
    /// envelope the broker accepted, in order.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn capturing_for_test() -> (Self, Box<dyn Fn() -> Vec<SignedOp>>) {
        tests::capturing_manager()
    }

    /// Like [`Self::connected_for_lifecycle_test`], but still connecting:
    /// returns the manager with the wake scope it reports to and a channel
    /// that receives each of its wakes.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn connecting_for_lifecycle_test() -> (Self, crate::wake::WakeScope, mpsc::Receiver<()>) {
        tests::connecting_manager()
    }

    /// Test-only: build a manager talking to an SDK broker on the given
    /// already-connected client fd, bypassing env-var lookup so two
    /// tests in one process don't race on `TILO_SVC_COLLAB_FD`.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn from_sdk_fd_for_test(fd: RawFd, app_id: &str, cluster_id: Uuid) -> Self {
        Self::from_sdk_fd_for_test_with_identity(fd, app_id, cluster_id, harness_presentation())
    }

    /// Like [`Self::from_sdk_fd_for_test`] but stamps the `join_channel`
    /// call with a caller-supplied identity blob.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn from_sdk_fd_for_test_with_identity(
        fd: RawFd,
        app_id: &str,
        cluster_id: Uuid,
        identity_bytes: Vec<u8>,
    ) -> Self {
        let sdk = Self::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: identity_bytes,
                participant_kind: "person",
                signer: harness_document_signer(),
            }),
        )
        .expect("spawn_sdk from Fd source must succeed in tests");
        let mut manager = Self::with_transport(Box::new(sdk));
        manager.send(CollabRequest::Hello {
            app_id: app_id.to_owned(),
            cluster_id,
            record: None,
            record_kind: None,
            handle: crate::local_identity::own_name(),
        });
        manager
            .await_primary_route()
            .expect("test collaboration primary route becomes ready");
        manager
    }

    /// Stand up the shovel thread + its tokio `current_thread` runtime and
    /// return at once. The shovel loads the join identity and runs
    /// `try_connect` **inside** its `LocalSet`, where every route's state
    /// stays.
    ///
    /// The transport reports [`TransportStatus::Connecting`] until the
    /// shovel is serving commands (or [`TransportStatus::Closed`] if setup
    /// failed); commands submitted meanwhile queue for it. The returned
    /// receiver carries the same outcome once, for callers that choose to
    /// wait for it.
    fn start_sdk(
        source: SdkConnectSource,
        identity: SdkIdentitySource,
    ) -> Result<(BrokerTransport, mpsc::Receiver<InitStatus>), runtime_portal::NotAvailable> {
        // One slot more than the command budget: it is held back for the
        // next publication barrier (see `BrokerTransport::barrier_slot`).
        let (outbound_tx, outbound_rx) =
            tokio_mpsc::channel::<QueuedSdkCommand>(SDK_COMMAND_QUEUE + 1);
        let (inbound_tx, inbound_rx) = tokio_mpsc::channel::<QueuedCollabEvent>(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(inbound_tx, state.clone());
        let (media_tx, media_rx) = native_media::channel(inbound.clone());
        let (action_directory_tx, action_directory_rx) =
            mpsc::sync_channel::<ActionDirectoryRequest>(ACTION_DIRECTORY_REQUEST_QUEUE);
        let (init_tx, init_rx) = mpsc::sync_channel::<InitStatus>(1);

        // A thread we can't start is a service we can't reach — map to
        // NotAvailable rather than panicking; an app must open even without collab.
        let handle = match thread::Builder::new()
            .name("ui-collab-shovel".into())
            .spawn(move || run_shovel(source, identity, outbound_rx, media_rx, inbound, init_tx))
        {
            Ok(handle) => handle,
            Err(e) => return Err(runtime_portal::NotAvailable::ConnectionFailed(e)),
        };

        let transport = BrokerTransport {
            outbound_tx,
            media: Some(media_tx),
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
            _thread: Some(handle),
        };
        Ok((transport, init_rx))
    }

    /// [`Self::start_sdk`], then wait for the shovel to connect or fail.
    /// Only for the explicit, fallible headless constructors, whose callers
    /// asked for a connected manager or an error.
    fn spawn_sdk(
        source: SdkConnectSource,
        identity: Option<SdkJoinIdentity>,
    ) -> Result<BrokerTransport, runtime_portal::NotAvailable> {
        let (mut transport, init_rx) = Self::start_sdk(source, SdkIdentitySource::Ready(identity))?;

        // No clock ends this wait: loading the join identity is an IPC round
        // trip to the identity service, and connecting one to the broker.
        // Each answers or fails, and a shovel that ends drops its reporter.
        let Ok(status) = init_rx.recv() else {
            tracing::warn!("collab: the connect path ended without reporting");
            return Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "collab connect ended without reporting",
                ),
            ));
        };

        match status {
            InitStatus::Ready => {
                transport.register_command_bridge();
                Ok(transport)
            }
            InitStatus::Failed(e) => {
                // Let the (empty-body) thread exit cleanly.
                if let Some(handle) = transport._thread.take() {
                    let _ = handle.join();
                }
                Err(e)
            }
        }
    }
}

/// A surface app's connection to its session's services: the same
/// connection every app reaches its session through, held by a process that
/// shows a window placed by the session and joins no workspace of its own.
///
/// The session records the launch and names its socket on the app's
/// environment; the app connects that socket itself and writes the handoff
/// naming the launch, so the broker attests the process the way it attests
/// any app's. What it reads through it is what every app reads: the
/// session's published resources ([`SessionResource`]) and the actions on
/// them. Held for the process's life — dropping it ends the connection and
/// every watch on it.
pub struct SessionServices {
    /// The transport, kept for its worker: dropping it ends the connection
    /// the process's published resources are reached over.
    _transport: BrokerTransport,
}

impl SessionServices {
    /// Connect to the session's services at `socket`, as the launch of
    /// `app_id` in `workspace` the session recorded for this process.
    pub fn connect(
        socket: &std::path::Path,
        app_id: &str,
        workspace: &str,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        let mut stream = tilo_files::native::socket::connect(socket)?;
        runtime_portal::launch_handoff::write(&mut stream, app_id, workspace)?;
        let fd = std::os::fd::IntoRawFd::into_raw_fd(stream);
        let transport = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                // A surface joins no workspace: it presents no identity, and the
                // broker admits it for the launch alone.
                presentation: Vec::new(),
                participant_kind: "person",
                signer: harness_document_signer(),
            }),
        )?;
        Ok(Self {
            _transport: transport,
        })
    }
}

/// Where the shovel gets the identity it joins with.
enum SdkIdentitySource {
    /// Supplied by the caller; `None` for host sessions, which obtain it
    /// from the host during setup.
    Ready(Option<SdkJoinIdentity>),
    /// The launcher-inherited workspace identity for this cluster. Loading
    /// it and signing the join are IPC round trips, so the shovel does it,
    /// never the constructing thread.
    Inherited(Uuid),
}

impl SdkIdentitySource {
    fn resolve(self) -> Result<Option<SdkJoinIdentity>, runtime_portal::NotAvailable> {
        match self {
            Self::Ready(identity) => Ok(identity),
            Self::Inherited(cluster_id) => sdk_join_identity(cluster_id).map(Some),
        }
    }
}

/// One-shot init result from the shovel thread's startup, for callers of
/// `spawn_sdk` that wait for a connected transport or an error.
enum InitStatus {
    Ready,
    Failed(runtime_portal::NotAvailable),
}

/// Report what holds up the pending join of the workspace `channel` names, as
/// the broker says it ([`super::JoinHold`]): the workspace is waiting on this
/// device, for each change of its status, or the broker cannot be asked. Ends
/// once the workspace is ready or refused (the join itself then answers), or the
/// broker cannot be reached; the join is what ends the wait, never this.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn report_join_hold(
    client: &runtime_collab::CollabClient,
    channel: &str,
    state: &BridgeState,
) {
    let mut status = client.workspace_status(channel).await;
    loop {
        match status {
            Ok(runtime_collab::WorkspaceStatus::Waiting { reason, detail }) => {
                state.set_join_hold(Some(super::JoinHold::WorkspaceWaiting {
                    reason: reason.clone(),
                    detail: detail.clone(),
                }));
                status = client
                    .await_workspace(
                        channel,
                        &runtime_collab::WorkspaceStatus::Waiting { reason, detail },
                    )
                    .await;
            }
            Ok(
                runtime_collab::WorkspaceStatus::Ready
                | runtime_collab::WorkspaceStatus::Refused(_),
            ) => {
                state.set_join_hold(None);
                return;
            }
            Err(error) => {
                state.set_join_hold(Some(super::JoinHold::Unreachable(error.to_string())));
                return;
            }
        }
    }
}

/// Publish the shovel's setup outcome: to the owner through the bridge's
/// connect phase (waking it), and to a waiting constructor, if any.
fn report_init(state: &BridgeState, init_tx: &mpsc::SyncSender<InitStatus>, status: InitStatus) {
    state.set_connect_phase(match &status {
        InitStatus::Ready => ConnectPhase::Connected,
        InitStatus::Failed(error) => ConnectPhase::Failed(error.to_string()),
    });
    let _ = init_tx.send(status);
}

/// This thread's tid: `/proc/thread-self` names the calling thread's own
/// directory, which ends in it.
#[cfg(all(test, unix))]
fn current_tid() -> std::io::Result<i32> {
    let path = std::fs::read_link("/proc/thread-self")?;
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|tid| tid.parse().ok())
        .ok_or_else(|| std::io::Error::other("no tid in /proc/thread-self"))
}

/// The shovel thread's body: owns a `current_thread` runtime + `LocalSet`
/// (where every route's state stays), stands up the SDK client, subscribes
/// its three streams, and forwards outbound requests. Exits when
/// `outbound_rx.recv()` returns `None` (manager dropped).
fn run_shovel(
    source: SdkConnectSource,
    identity: SdkIdentitySource,
    outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    media_rx: native_media::Receiver,
    inbound_tx: InboundBridge,
    init_tx: mpsc::SyncSender<InitStatus>,
) {
    let state = inbound_tx.state.clone();
    #[cfg(all(test, unix))]
    if let Ok(tid) = current_tid() {
        state.shovel_tid.store(tid, Ordering::Release);
    }
    // Loading an inherited identity is blocking IPC; this thread exists so
    // that the owner never waits for it.
    let identity = match identity.resolve() {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(?error, "collab: workspace join identity unavailable");
            report_init(&state, &init_tx, InitStatus::Failed(error));
            return;
        }
    };
    // Building `current_thread` is fallible (can OOM); surface as
    // Disabled — no service means the app runs local-only.
    let mut runtime = tokio::runtime::Builder::new_current_thread();
    runtime.enable_all();
    #[cfg(test)]
    if matches!(&source, SdkConnectSource::Host(connector, _) if connector.paused_clock) {
        runtime.start_paused(true);
    }
    let rt = match runtime.build() {
        Ok(rt) => rt,
        Err(e) => {
            report_init(
                &state,
                &init_tx,
                InitStatus::Failed(runtime_portal::NotAvailable::ConnectionFailed(e)),
            );
            return;
        }
    };

    let local = LocalSet::new();
    let init_state = state.clone();
    let replies = inbound_tx.clone();
    local.spawn_local(async move { replies.forward_replies().await });
    local.block_on(&rt, async move {
        let state = init_state;
        let session = match source {
            SdkConnectSource::Host(connector, route) => {
                let cluster_id = route.cluster_id;
                let identity_timeout = connector.identity_exchange_timeout;
                if let Some(ready) = connector.ready {
                    ready().await;
                }
                // The host's handoff ends when the host has connected the
                // session or says why it could not; no clock here guesses
                // that it never will.
                match (connector.connect)(route).await {
                    Ok((collaboration_stream, identity_stream, lane)) => {
                        let identity =
                            runtime_identity::AsyncBrokerSigningIdentity::from_connected_io(
                                identity_stream,
                                identity_timeout,
                            )
                            .await;
                        match identity {
                            Ok(identity) => {
                                match sdk_join_identity_from_host(identity, cluster_id).await {
                                    Ok(identity) => {
                                        let (client, end) =
                                            runtime_collab::CollabClient::connected_local(
                                                collaboration_stream,
                                            );
                                        let client = client.with_optional_descriptors(lane);
                                        Ok(((client, end), identity))
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            source => source.build_client().and_then(|connection| {
                identity
                    .map(|identity| (connection, identity.into()))
                    .ok_or_else(|| {
                        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "missing collaboration signing identity",
                        ))
                    })
            }),
        };
        let ((client, connection_end), identity) = match session {
            Ok(session) => session,
            Err(e) => {
                report_init(&state, &init_tx, InitStatus::Failed(e));
                return;
            }
        };
        // The transport is ready. The manager's primary Hello and its
        // `PrimaryReady` barrier are the first commands the shovel serves.
        report_init(&state, &init_tx, InitStatus::Ready);

        Box::pin(drive_shovel(
            client,
            connection_end,
            identity,
            outbound_rx,
            media_rx,
            inbound_tx,
        ))
        .await;
    });
    // However the shovel ended, the owner must look again: an unanswered
    // primary-route barrier now reads as a stopped worker.
    state.notify_events();
}

/// The channel-key schedule as the shovel holds it between rounds.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyScheduleSeen {
    /// Whether the timer would start another round.
    armed: bool,
    /// Whether a route is marked to ask in the next round.
    marked: bool,
    /// How many rounds the timer has started.
    timer_rounds: usize,
}

/// Whether any route holds no keys yet and nothing will tell it when that
/// changes, so it is asked about on the timer.
fn any_route_asking_on_timer(routes: &std::collections::HashMap<String, SdkRoute>) -> bool {
    routes.values().any(|route| {
        route
            .encryption
            .lock()
            .is_ok_and(|keys| keys.asks_on_timer())
    })
}

/// Follow `channel`'s key changes for as long as the endpoint runs, telling
/// the shovel to ask for `route`'s keys after each. The call is pending
/// without traffic while nothing changes.
fn follow_channel_keys(
    channel: runtime_collab::ChannelHandle,
    route: String,
    signals: tokio_mpsc::UnboundedSender<KeySignal>,
) {
    tokio::task::spawn_local(async move {
        let mut after = 0;
        loop {
            match channel
                .changed(runtime_collab::wire::Changes::ChannelKeys, after)
                .await
            {
                Ok(change) if change != after => {
                    after = change;
                    if signals.send(KeySignal::Changed(route.clone())).is_err() {
                        return;
                    }
                }
                Ok(_) => {
                    tracing::warn!("collab: the broker reported an unchanged channel as changed");
                    let _ = signals.send(KeySignal::Unpushed);
                    return;
                }
                Err(error) => {
                    tracing::info!(%error, "collab: the broker does not say when channel keys change; asking on a timer");
                    let _ = signals.send(KeySignal::Unpushed);
                    return;
                }
            }
        }
    });
}

/// SDK-side event loop: dispatches outbound requests to SDK calls, and
/// spawns one subscription task per stream on the first `join_channel`.
// Not `Send`: the route state it holds stays on `run_shovel`'s `LocalSet`.
#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn drive_shovel(
    client: runtime_collab::CollabClient,
    mut connection_end: runtime_collab::ConnectionEnd,
    identity: LocalSdkJoinIdentity,
    mut outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    media_rx: native_media::Receiver,
    inbound_tx: InboundBridge,
) {
    struct PrivatePersonSnapshotLifetime;
    impl Drop for PrivatePersonSnapshotLifetime {
        fn drop(&mut self) {}
    }
    let _private_person_snapshot_lifetime = PrivatePersonSnapshotLifetime;
    let LocalSdkJoinIdentity {
        presentation,
        participant_kind,
        mut signer,
    } = identity;
    let person = personal_cluster_presentation(&presentation, &mut signer).await;
    let media_routes = native_media::Routes::default();
    let _media_worker = native_media::start(media_rx, media_routes.clone());
    // Every route of a certified endpoint is an encrypted channel whose keys
    // come from the identity service. A harness with no such service and no
    // channel keys of its own stays plaintext.
    let asks_channel_keys = !matches!(
        signer,
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(_, None))
    );
    // When to ask for keys: see `KeySchedule`. Each encrypted route follows
    // its channel's changes and marks itself on `key_signals`.
    let mut key_schedule = KeySchedule::new();
    let (key_signals, mut key_signal_rx) = tokio_mpsc::unbounded_channel::<KeySignal>();
    // A certified identity service is asked on a blocking thread. A round
    // runs alongside this loop, which keeps sending while the routes keep
    // receiving; at most one round is in flight.
    let mut key_fetch: Option<KeyFetch> = None;
    #[cfg(test)]
    let mut key_timer_rounds = 0_usize;
    let mut routes = std::collections::HashMap::<String, SdkRoute>::new();
    let mut primary: Option<String> = None;
    let mut participant_handle: Option<String> = None;
    let mut primary_result = Err("primary collaboration route was not announced".to_string());
    // The app's settings over the primary route, when the shell keeps them.
    let mut settings_link: Option<SettingsLink> = None;
    // The app's live activity over the primary route, when the shell
    // presents live activities.
    let mut live_activity_link: Option<LiveActivityLink> = None;
    // The first document mutation since the last publication barrier that
    // did not reach the broker, reported to (and cleared by) that barrier.
    let mut unpublished: Option<String> = None;
    // The action directory the owner published while no primary route was
    // joined, sent as soon as one is.
    let mut unrouted_directory: Option<mpsc::SyncSender<ActionDirectoryRequest>> = None;
    // The routes joining beside this loop ([`start_route_join`]), each with
    // the commands that wait for it, in the order they came: one route's
    // join never holds up the rest of the app's connection.
    let mut joining = std::collections::HashMap::<String, Vec<(u64, QueuedSdkCommand)>>::new();
    // A `Hello` naming a route that is already joining, with the owner's
    // wait for the primary route that follows it: the route joins once, so
    // the announcement waits for that join instead of starting another.
    let mut held_primary: Vec<(u64, PrimaryHello)> = Vec::new();
    let mut held_primary_ready: Vec<(u64, QueuedSdkCommand)> = Vec::new();
    let (joined_tx, mut joined_rx) = tokio_mpsc::unbounded_channel::<JoinedRoute>();
    // Commands whose route answered, and barriers whose joins ended, taken
    // before anything newer, in the order they first came: each already had
    // its place decided, and is not held again.
    let mut replay = std::collections::VecDeque::<(u64, QueuedSdkCommand)>::new();
    // The order commands came in, so what a join held back goes out in it.
    let mut arrivals: u64 = 0;
    // Publication barriers behind joins that were running when they came,
    // with the routes each still waits for: a barrier answers for everything
    // queued before it, a route's join and the writes waiting for it
    // included, and for nothing queued after it.
    let mut held_barriers = std::collections::VecDeque::<(
        std::collections::HashSet<String>,
        u64,
        QueuedSdkCommand,
    )>::new();
    macro_rules! join_inputs {
        () => {
            JoinInputs {
                asks_channel_keys,
                key_signals: key_signals.clone(),
                client: client.clone(),
                presentation: presentation.clone(),
                person: person.clone(),
                participant_handle: participant_handle.clone(),
                participant_kind,
                inbound_tx: inbound_tx.clone(),
                media_routes: media_routes.clone(),
            }
        };
    }

    // How a route of this connection joins: the identity it joins as, as of
    // the last `Hello`.
    macro_rules! join_context {
        () => {
            SdkJoinContext {
                asks_channel_keys,
                key_signals: &key_signals,
                client: &client,
                presentation: &presentation,
                person: person.as_ref(),
                participant_handle: participant_handle.as_deref(),
                participant_kind,
                inbound_tx: &inbound_tx,
                media_routes: &media_routes,
            }
        };
    }
    loop {
        let timed = asks_channel_keys
            && key_fetch.is_none()
            && key_schedule.timed(any_route_asking_on_timer(&routes));
        let (queued, decided) = if let Some((arrival, next)) = replay.pop_front() {
            (Some((arrival, next)), true)
        } else {
            let queued = tokio::select! {
            queued = outbound_rx.recv() => queued.map(|queued| {
                arrivals = arrivals.wrapping_add(1);
                (arrivals, queued)
            }),
            Some((spec, joined)) = joined_rx.recv() => {
                let key = spec.channel_id();
                let joined_ok = joined.is_ok();
                let mut waiting = joining.remove(&key).unwrap_or_default();
                match joined {
                    Ok(route) => {
                        routes.entry(key.clone()).or_insert(route);
                        note_route_join(&inbound_tx.state, &spec, Ok(()));
                        // The owner holding changes for the route hears it
                        // opened.
                        inbound_tx.state.notify_events();
                        if asks_channel_keys {
                            // The route learns its keys (or that it waits)
                            // before the edits that waited for it go out.
                            if let Some(fetch) = key_fetch.take() {
                                apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx)
                                    .await;
                            }
                            let round = key_schedule.join_round();
                            if let Some(fetch) =
                                refresh_keys(&routes, &mut signer, &inbound_tx, &round).await
                            {
                                apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx)
                                    .await;
                            }
                        }
                    }
                    Err(reason) => {
                        // What reads the route hears that no read is coming,
                        // the app is told the route cannot be reached, and
                        // what waited for it is answered with the reason.
                        inbound_tx
                            .state
                            .document_reads
                            .join_failed(&key, reason.clone());
                        note_route_join(&inbound_tx.state, &spec, Err(reason.clone()));
                        inbound_tx.state.notify_events();
                        let _ = inbound_tx.send_reply(CollabEvent::ChannelAccess {
                            route: spec,
                            state: crate::ChannelAccessState::Unavailable,
                        });
                        for (_, waited) in std::mem::take(&mut waiting) {
                            refuse_unjoined(waited.command, &reason, &inbound_tx.state, &key);
                        }
                        // A switch to this route hears why it is not the
                        // primary route, as it would from a join of its own.
                        let before = held_primary.len();
                        held_primary.retain(|(_, hello)| hello.route.channel_id() != key);
                        if held_primary.len() != before {
                            primary = None;
                            primary_result = Err(reason);
                            inbound_tx.state.notify_events();
                            for (_, owed) in std::mem::take(&mut held_primary_ready) {
                                if let SdkCommand::PrimaryReady(reply) = owed.command {
                                    let _ = reply.send(primary_result.clone());
                                }
                            }
                        }
                    }
                }
                for (waits_for, _, _) in &mut held_barriers {
                    waits_for.remove(&key);
                }
                // The writes that waited for the route and the barriers whose
                // joins all ended go out in the order they came: a barrier
                // answers for what came before it, and not for a write that
                // came after it and waited for the same join.
                let mut released = waiting;
                while held_barriers
                    .front()
                    .is_some_and(|(waits_for, _, _)| waits_for.is_empty())
                {
                    if let Some((_, arrival, barrier)) = held_barriers.pop_front() {
                        released.push((arrival, barrier));
                    }
                }
                released.sort_by_key(|(arrival, _)| *arrival);
                replay.extend(released);
                if joined_ok {
                    // The route the switch asked for is joined: it becomes
                    // the primary route, and the owner's wait is answered
                    // with it, in the order they came.
                    let mut owed =
                        take_held_announcements(&mut held_primary, &key, &inbound_tx.state);
                    if !owed.is_empty() {
                        owed.append(&mut std::mem::take(&mut held_primary_ready));
                        owed.sort_by_key(|(arrival, _)| *arrival);
                        replay.extend(owed);
                    }
                }
                continue;
            }
            // The broker hung up: every call on this connection now fails,
            // and nothing reconnects it (a host retry is a new connector).
            // Collaboration ends here, once, so the app goes offline instead
            // of retrying calls that cannot succeed.
            () = &mut connection_end => {
                tracing::warn!(
                    primary = primary.as_deref().unwrap_or(""),
                    "collab: the broker closed this app's connection"
                );
                inbound_tx.state.fail_closed("broker connection closed");
                break;
            }
            Some(signal) = key_signal_rx.recv() => {
                key_schedule.note(signal);
                if key_fetch.is_none()
                    && let Some(round) = key_schedule.marked_round()
                {
                    key_fetch = refresh_keys(&routes, &mut signer, &inbound_tx, &round).await;
                }
                continue;
            }
            () = key_schedule.timer.as_mut(), if timed => {
                #[cfg(test)]
                {
                    key_timer_rounds += 1;
                }
                let round = key_schedule.timer_round();
                key_fetch = refresh_keys(&routes, &mut signer, &inbound_tx, &round).await;
                continue;
            }
            fetched = async {
                match key_fetch.as_mut() {
                    Some(fetch) => fetch.await,
                    None => std::future::pending().await,
                }
            }, if key_fetch.is_some() =>
            {
                key_fetch = None;
                if apply_fetched_keys(&routes, fetched, &mut signer, &inbound_tx).await {
                    key_schedule.restart();
                }
                // Routes told to ask while the round was out ask now.
                if let Some(round) = key_schedule.marked_round() {
                    key_fetch = refresh_keys(&routes, &mut signer, &inbound_tx, &round).await;
                }
                continue;
            }
            };
            (queued, false)
        };
        let Some((
            arrival,
            QueuedSdkCommand {
                command,
                _reservation,
            },
        )) = queued
        else {
            break;
        };
        // The owner only stops sending once nothing waits on this worker, and
        // a refused change left the bridge open: end it rather than drop what
        // the owner queued and leave the app believing it is still connected.
        if !inbound_tx.state.is_healthy() {
            stranded_break(&inbound_tx.state);
            break;
        }
        // A command for a route that has not joined waits for its join,
        // which a write starts; the rest of the connection goes on.
        if let Some((spec, starts)) = waits_on_route(&command).filter(|_| !decided) {
            let key = spec.channel_id();
            if !routes.contains_key(&key) && (starts || joining.contains_key(&key)) {
                joining
                    .entry(key)
                    .or_insert_with(|| {
                        start_route_join(join_inputs!(), spec, String::new(), joined_tx.clone());
                        Vec::new()
                    })
                    .push((
                        arrival,
                        QueuedSdkCommand {
                            command,
                            _reservation,
                        },
                    ));
                continue;
            }
        }
        if !decided
            && matches!(command, SdkCommand::PublicationBarrier(_))
            && (!joining.is_empty() || !held_barriers.is_empty())
        {
            held_barriers.push_back((
                joining.keys().cloned().collect(),
                arrival,
                QueuedSdkCommand {
                    command,
                    _reservation,
                },
            ));
            continue;
        }
        let routes_before = routes.len();
        let document = command.document_len().is_some();
        let mut published = true;
        match command {
            SdkCommand::Hello(hello) => {
                // A route already joining beside this loop (the record this
                // window prepared) joins once: a second join of the same
                // channel replaces the committed read the first one staged,
                // and leaves the owner with a route it cannot publish on.
                // So this announcement joins nothing and takes the route as
                // the primary one when that join ends.
                if !routes.contains_key(&hello.route.channel_id())
                    && joining.contains_key(&hello.route.channel_id())
                {
                    held_primary.push((arrival, hello));
                    continue;
                }
                let PrimaryHello {
                    route: spec,
                    record_kind,
                    handle,
                } = hello;
                participant_handle = handle;
                let key = spec.channel_id();
                // The join ends when the broker answers it or it fails, with
                // the broker's reason; a broker that goes away fails it. No
                // clock guesses that an answer is not coming: under load
                // the broker's join, and the workspace's host behind it, take
                // what they take. While it is pending the broker says what
                // holds it up, which the owner shows.
                let joined = {
                    let context = join_context!();
                    let join = ensure_sdk_route(
                        &context,
                        &mut routes,
                        &spec,
                        record_kind.as_deref().unwrap_or(""),
                    );
                    tokio::pin!(join);
                    tokio::select! {
                        biased;
                        joined = &mut join => joined,
                        () = report_join_hold(&client, &key, &inbound_tx.state) => join.await,
                    }
                };
                inbound_tx.state.set_join_hold(None);
                primary_result = joined.map(|()| spec);
                primary = primary_result.is_ok().then_some(key);
                // The startup file splits the owner's wait for `PrimaryReady`.
                tilo_startup_metrics::startup_mark("collab-route-joined");
                if let Some(route) = primary.as_ref().and_then(|key| routes.get(key)) {
                    settings_link = link_settings(&route.channel, &inbound_tx);
                    // Writes made before the route joined go out on the
                    // link's own task: the route is ready without waiting
                    // for the shell to store them.
                    if let Some(link) = settings_link.as_ref() {
                        link.flush();
                    }
                    tilo_startup_metrics::startup_mark("collab-settings-linked");
                    live_activity_link = link_live_activity(&route.channel, &inbound_tx);
                    if let Some(link) = live_activity_link.as_ref() {
                        link.flush(true).await;
                    }
                    tilo_startup_metrics::startup_mark("collab-activity-linked");
                    if let Some(requests) = unrouted_directory.take() {
                        publish_directory(route, requests, &inbound_tx).await;
                    }
                }
            }
            SdkCommand::Primary(request) => {
                if let Some(route) = primary.as_ref().and_then(|key| routes.get_mut(key)) {
                    published = dispatch_sdk_request(
                        route,
                        request,
                        &mut signer,
                        &inbound_tx,
                        RouteRole::App,
                    )
                    .await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else if matches!(
                    request,
                    CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
                ) {
                    // Document custody fails closed without a joined primary.
                    inbound_tx
                        .state
                        .fail_closed("document primary route unavailable");
                    break;
                } else {
                    tracing::warn!("collab: request sent before the primary route joined");
                    published = false;
                }
            }
            SdkCommand::Join(spec, record_kind) => {
                // The route joins beside this loop; its answer comes back on
                // `joined_rx`, failed or not, within `ROUTE_JOIN_WAIT`.
                let key = spec.channel_id();
                if routes.contains_key(&key) {
                    note_route_join(&inbound_tx.state, &spec, Ok(()));
                    inbound_tx.state.notify_events();
                } else if let std::collections::hash_map::Entry::Vacant(entry) = joining.entry(key)
                {
                    entry.insert(Vec::new());
                    start_route_join(
                        join_inputs!(),
                        spec,
                        record_kind.unwrap_or_default(),
                        joined_tx.clone(),
                    );
                }
            }
            SdkCommand::ToRoute(spec, request) => {
                let key = spec.channel_id();
                let joined = ensure_sdk_route(&join_context!(), &mut routes, &spec, "").await;
                let joined = note_route_join(&inbound_tx.state, &spec, joined);
                if joined && let Some(route) = routes.get_mut(&key) {
                    published = dispatch_sdk_request(
                        route,
                        request,
                        &mut signer,
                        &inbound_tx,
                        RouteRole::Record,
                    )
                    .await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else {
                    // The record's route did not join: that record is not
                    // shared, and says why; the app and its other records
                    // carry on.
                    published = false;
                    inbound_tx.state.route_unpublished(key);
                }
            }
            SdkCommand::PrimaryReady(reply) => {
                if !held_primary.is_empty() {
                    // The route the owner switched to is still joining: its
                    // answer is owed once that join ends and the announcement
                    // replayed here has made it the primary route.
                    held_primary_ready.push((
                        arrival,
                        QueuedSdkCommand {
                            command: SdkCommand::PrimaryReady(reply),
                            _reservation: inbound_tx.state.command_bytes.empty_reservation(),
                        },
                    ));
                    continue;
                }
                let _ = reply.send(primary_result.clone());
                // A manager still connecting settles on this answer.
                inbound_tx.state.notify_events();
            }
            SdkCommand::PublicationBarrier(replies) => {
                // A superseded receipt may have no receiver. Its prefix still ran.
                // A latched, specific publication failure (a refused signature
                // or commit) outranks the generic "did not reach" marker.
                let unpublished = unpublished.take();
                replies.answer(&inbound_tx.state.publication_result().and_then(|()| {
                    unpublished.map_or(Ok(()), |reason| {
                        Err(DocumentPublicationError::Failed(reason))
                    })
                }));
            }
            SdkCommand::InstallEncryption { spec, group, reply } => {
                let result =
                    install_sdk_encryption(join_context!(), &mut routes, spec, *group).await;
                let _ = reply.send(result);
            }
            SdkCommand::PublishActionDirectory { requests, .. } => {
                // The directory rides the primary route. One published before
                // the route joined (or while a later Hello's join is still
                // failing) is kept and sent once a route joins: the owner
                // counts it as published and never sends it again.
                match primary.as_ref().and_then(|key| routes.get(key)) {
                    Some(route) => {
                        publish_directory(route, requests, &inbound_tx).await;
                    }
                    None => {
                        unrouted_directory = Some(requests);
                    }
                }
            }
            SdkCommand::BrowserServiceSession {
                route: spec,
                command,
                wake,
                reply,
            } => {
                // The app announced its route in a `Hello` queued before it
                // asked, so the loop has joined it by now however long the
                // join took. The route the connection serves is the app's
                // current primary route (its record's), which a bound
                // component may have moved since the request was made. A
                // route that did not join answers as unavailable rather than
                // leaving the window's receipt open.
                let fallback = primary
                    .as_ref()
                    .and_then(|key| routes.get(key))
                    .filter(|route| {
                        route.spec.app_id == spec.app_id && route.spec.cluster_id == spec.cluster_id
                    })
                    .map(|route| route.spec.channel_id());
                let key = fallback.unwrap_or_else(|| spec.channel_id());
                let joined = routes.get_mut(&key).filter(|route| {
                    route.spec == spec
                        || (route.spec.app_id == spec.app_id
                            && route.spec.cluster_id == spec.cluster_id)
                });
                let result = if let Some(route) = joined {
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        route.channel.browser_service_session_command(&command),
                    )
                    .await
                    .map_err(|_| "This account could not be opened".to_owned())
                    .and_then(|result| {
                        result.map_err(|_| "This account could not be opened".to_owned())
                    })
                } else {
                    Err("This account could not be opened".to_owned())
                };
                let _ = reply.try_send(result);
                if let Some(wake) = wake {
                    wake.wake();
                }
            }
            SdkCommand::WifiContinuity {
                app_id: _,
                command,
                reply,
            } => {
                let result = if let Some(key) = primary.as_ref()
                    && let Some(route) = routes.get_mut(key)
                {
                    route
                        .channel
                        .wifi_continuity_command(&command)
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    Err("primary collaboration route is unavailable".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::Speakers {
                app_id: _,
                command,
                reply,
            } => {
                let result = if let Some(key) = primary.as_ref()
                    && let Some(route) = routes.get_mut(key)
                {
                    route
                        .channel
                        .speaker_command(&command)
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    Err("primary collaboration route is unavailable".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::DisplayControl {
                command,
                origin,
                reply,
            } => {
                let result = if let Some(link) = settings_link.as_ref() {
                    tokio::time::timeout(
                        Duration::from_secs(3),
                        link.handle.display_control(&command, origin),
                    )
                    .await
                    .map_err(|_| "The display service timed out.".to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()))
                } else {
                    Err("The admitted settings channel is unavailable.".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::Published(command) => {
                published_command(&client, command).await;
            }
            SdkCommand::FlushSettings => {
                if let Some(link) = settings_link.as_ref() {
                    link.flush();
                }
            }
            SdkCommand::FlushLiveActivity => {
                if let Some(link) = live_activity_link.as_ref() {
                    link.flush(false).await;
                }
            }
            #[cfg(test)]
            SdkCommand::KeyScheduleProbe(reply) => {
                if let Some(fetch) = key_fetch.take()
                    && apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx).await
                {
                    key_schedule.restart();
                }
                let _ = reply.send(KeyScheduleSeen {
                    armed: asks_channel_keys
                        && key_schedule.timed(any_route_asking_on_timer(&routes)),
                    marked: !key_schedule.marked.is_empty(),
                    timer_rounds: key_timer_rounds,
                });
            }
        }
        if document && !published && unpublished.is_none() {
            unpublished = Some("a document mutation did not reach the broker".into());
        }
        if asks_channel_keys && routes.len() > routes_before {
            // A route joined just now learns its keys (or that it waits)
            // before its first edit rather than at the next round. Only
            // sending waits for that; the routes keep receiving.
            if let Some(fetch) = key_fetch.take() {
                apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx).await;
            }
            let round = key_schedule.join_round();
            if let Some(fetch) = refresh_keys(&routes, &mut signer, &inbound_tx, &round).await {
                apply_fetched_keys(&routes, fetch.await, &mut signer, &inbound_tx).await;
            }
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn install_sdk_encryption(
    context: SdkJoinContext<'_>,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: RouteSpec,
    group: channel_crypto::ChannelGroup,
) -> Result<(), String> {
    let key = spec.channel_id();
    ensure_sdk_route(&context, routes, &spec, "").await?;
    let route = routes
        .get_mut(&key)
        .ok_or_else(|| "joined MLS route disappeared".to_string())?;
    let mut keys = route
        .encryption
        .lock()
        .map_err(|_| "MLS endpoint state is unavailable after a poisoned lock".to_string())?;
    keys.install_group(group);
    key_fetch::hold_content_secret(&route.spec, &keys);
    Ok(())
}

/// This device's join proof for its person's own workspace, as the identity
/// service lends it to an app that keeps records of theirs there; `None`
/// for any other app, or a connection without a certified identity.
#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is constructed and retained only on the shovel LocalSet"
)]
async fn personal_cluster_presentation(
    presentation: &[u8],
    signer: &mut LocalSdkDocumentSigner,
) -> Option<(String, Vec<u8>)> {
    #[derive(serde::Deserialize)]
    struct Root {
        user_public_key: [u8; 32],
    }
    // The person's records then cannot be reached; the log says why.
    let unavailable = |reason: &dyn std::fmt::Display| {
        tracing::warn!(%reason, "no join proof for the person's own workspace");
    };
    let launch = match runtime_collab::CertifiedJoinIdentity::decode(presentation) {
        Ok(Some(launch)) => launch,
        // A connection without a certified identity.
        Ok(None) => return None,
        Err(error) => {
            unavailable(&format_args!(
                "the launch identity does not decode: {error}"
            ));
            return None;
        }
    };
    let root: Root = match serde_json::from_slice(&launch.device_certificate) {
        Ok(root) => root,
        Err(error) => {
            unavailable(&format_args!(
                "the device certificate does not decode: {error}"
            ));
            return None;
        }
    };
    let workspace = collab_types::personal_cluster(&root.user_public_key).to_string();
    if workspace == launch.workspace_id {
        return None;
    }
    let signed = match signer {
        LocalSdkDocumentSigner::Host(identity) => identity
            .sign_collaboration_join(&workspace)
            .await
            .map_err(|error| error.to_string()),
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            let (identity, asked) = (std::sync::Arc::clone(identity), workspace.clone());
            tokio::task::spawn_blocking(move || identity.sign_collaboration_join(&asked))
                .await
                .map_err(|error| error.to_string())
                .and_then(|signed| signed.map_err(|error| error.to_string()))
        }
        LocalSdkDocumentSigner::Inherited(_) => return None,
    };
    let proof = match signed {
        Ok(proof) => proof,
        Err(error) => {
            unavailable(&format_args!("signing the join failed: {error}"));
            return None;
        }
    };
    let bytes = runtime_collab::CertifiedJoinIdentity {
        workspace_id: workspace.clone(),
        device_certificate: launch.device_certificate,
        proof: proof.to_vec(),
    }
    .encode()
    .ok()?;
    Some((workspace, bytes))
}

/// The announcements of `key`'s route held while it was joining, as commands
/// to replay once it has joined (or to refuse, when it did not), in the order
/// they came. Announcements of other routes stay held.
fn take_held_announcements(
    held: &mut Vec<(u64, PrimaryHello)>,
    key: &str,
    state: &BridgeState,
) -> Vec<(u64, QueuedSdkCommand)> {
    let mut released = Vec::new();
    let mut still_held = Vec::new();
    for (arrival, hello) in std::mem::take(held) {
        if hello.route.channel_id() == key {
            released.push((
                arrival,
                QueuedSdkCommand {
                    command: SdkCommand::Hello(hello),
                    _reservation: state.command_bytes.empty_reservation(),
                },
            ));
        } else {
            still_held.push((arrival, hello));
        }
    }
    *held = still_held;
    released
}

/// Keep `joined`, the outcome of joining `spec`, where the route's owner and
/// diagnostics read it ([`CollabTransport::route_join_failure`]): a failed
/// join's reason until a later join of the route succeeds. Whether it joined.
fn note_route_join(state: &BridgeState, spec: &RouteSpec, joined: Result<(), String>) -> bool {
    let mut failures = state.route_join_failures.lock_recover();
    match joined {
        Ok(()) => {
            failures.remove(&spec.channel_id());
            true
        }
        Err(reason) => {
            tracing::warn!(
                app = %spec.app_id,
                record = spec.record.as_deref().unwrap_or(""),
                %reason,
                "collab: route did not join"
            );
            failures.insert(spec.channel_id(), reason);
            false
        }
    }
}

#[derive(Clone, Copy)]
struct SdkJoinContext<'a> {
    asks_channel_keys: bool,
    /// Where each route's endpoint says its keys may have changed.
    key_signals: &'a tokio_mpsc::UnboundedSender<KeySignal>,
    client: &'a runtime_collab::CollabClient,
    presentation: &'a [u8],
    /// The person's own workspace and this device's join proof for it, when the
    /// identity service lends one: what a route of a record of theirs
    /// presents instead of the launch workspace's.
    person: Option<&'a (String, Vec<u8>)>,
    participant_handle: Option<&'a str>,
    participant_kind: &'a str,
    inbound_tx: &'a InboundBridge,
    media_routes: &'a native_media::Routes,
}

struct SdkRoute {
    _checks: Option<crate::lane::Registration<super::approved_checks::Endpoint>>,
    _moderation: Option<crate::lane::Registration<super::external_moderation::Endpoint>>,
    _attribution: Option<crate::lane::Registration<super::write_attribution_lane::Endpoint>>,
    spec: RouteSpec,
    channel: runtime_collab::ChannelHandle,
    presence: Option<runtime_collab::PresenceHandle>,
    workspace_controls: std::collections::HashMap<u64, runtime_collab::WorkspaceControlHandle>,
    committed: bool,
    encryption: SharedRouteKeys,
}

// Not `Send`: the route state it holds stays on the shovel thread's
// `LocalSet` (see `run_shovel`).
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn ensure_sdk_route(
    context: &SdkJoinContext<'_>,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: &RouteSpec,
    record_kind: &str,
) -> Result<(), String> {
    let channel_id = spec.channel_id();
    if routes.contains_key(&channel_id) {
        return Ok(());
    }
    let route = join_route(context, spec, record_kind).await?;
    routes.insert(channel_id, route);
    Ok(())
}

/// Bridge content arriving for `channel`'s record into
/// [`CollabEvent::RecordContent`] on `inbound_tx`. A host that keeps no
/// record content refuses the subscription, and its apps keep their content
/// to themselves.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_content_stream(
    channel: &runtime_collab::ChannelHandle,
    route: RouteSpec,
    inbound_tx: &InboundBridge,
) {
    let Ok(stream) = channel.subscribe_blobs().await else {
        return;
    };
    super::record_content::ContentLane::of(&route.app_id, route.record.as_deref().unwrap_or(""))
        .serve();
    let inbound_tx = inbound_tx.clone();
    tokio::task::spawn_local(async move {
        while let Ok(event) = stream.read().await {
            let state = match event.state {
                tilo_content::ContentState::Resolved => None,
                state => Some(state),
            };
            let event = match state {
                None => CollabEvent::RecordContent {
                    route: route.clone(),
                    hash: event.hash,
                    bytes: event.bytes,
                },
                Some(state) => CollabEvent::RecordContentState {
                    route: route.clone(),
                    hash: event.hash,
                    state,
                },
            };
            if !inbound_tx.send_document(event).await {
                break;
            }
        }
    });
}

/// Which route an inbound op arrived on, as its refusal names it.
struct RouteLabel {
    app: String,
    workspace: String,
    record: String,
    channel: String,
}

impl RouteLabel {
    fn of(spec: &RouteSpec) -> Self {
        Self {
            app: spec.app_id.clone(),
            workspace: spec.cluster_id.to_string(),
            record: spec.record.clone().unwrap_or_default(),
            channel: spec.channel_id(),
        }
    }
}

/// Bridge `channel`'s doc/incremental op stream into [`CollabEvent`]s on
/// `inbound_tx`, spawned as a `LocalSet` task so it doesn't block the
/// caller's route setup.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_op_stream(
    channel: &runtime_collab::ChannelHandle,
    label: RouteLabel,
    inbound_tx: &InboundBridge,
    encryption: SharedRouteKeys,
    key_signals: Option<(String, tokio_mpsc::UnboundedSender<KeySignal>)>,
) {
    if let Ok(stream) = channel.subscribe().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                let envelope = match postcard::from_bytes::<SignedOp>(&event.op) {
                    Ok(envelope) => envelope,
                    Err(error) => {
                        tracing::warn!(
                            "collab: signed op is not postcard SignedOp ({error}); dropping"
                        );
                        continue;
                    }
                };
                let (author, seq) = (envelope.author, envelope.seq);
                let arrived = Arrived {
                    envelope,
                    from_user_id: event.from_user_id,
                    admission: event.admission,
                };
                let signed = match signed_document_event(arrived, &encryption) {
                    Ok(signed) => signed,
                    Err(UndeliveredDocument::Held) => {
                        // Sealed under an epoch the held keys lack:
                        // ask for newer keys, once per generation.
                        if let Some((route, signals)) = key_signals.as_ref()
                            && encryption
                                .lock()
                                .is_ok_and(|mut keys| keys.wants_keys_for_held())
                        {
                            let _ = signals.send(KeySignal::Changed(route.clone()));
                        }
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            app = %label.app,
                            workspace = %label.workspace,
                            record = %label.record,
                            route = %label.channel,
                            author = %hex::encode(author.as_bytes()),
                            seq,
                            "collab: refused a live document op"
                        );
                        continue;
                    }
                };
                let Some(complete) = complete_document_event(signed.event, &tx.reassembly.live)
                else {
                    continue;
                };
                let claims: Vec<[u8; 32]> = match &complete {
                    CollabEvent::Document { envelope, .. } => {
                        envelope.claims.iter().map(|claim| claim.change).collect()
                    }
                    _ => Vec::new(),
                };
                let events = match crate::commits::gate(
                    &tx.state,
                    signed.commit,
                    &claims,
                    (label.app.as_str(), label.record.as_str()),
                    complete,
                ) {
                    crate::commits::Delivery::Now(event) => vec![event],
                    crate::commits::Delivery::Waiting => continue,
                    crate::commits::Delivery::Complete(events) => events,
                    crate::commits::Delivery::Refused(reason) => {
                        tracing::warn!(
                            reason,
                            app = %label.app,
                            workspace = %label.workspace,
                            record = %label.record,
                            author = %hex::encode(author.as_bytes()),
                            "collab: refused a document part that is not the action it names"
                        );
                        continue;
                    }
                };
                let mut open = true;
                for event in events {
                    if !tx.send_document(event).await {
                        open = false;
                        break;
                    }
                }
                if !open {
                    break;
                }
            }
        });
    }
}

/// Bridge `channel`'s collaborator/permissions context stream into
/// [`CollabEvent::Context`] on `inbound_tx`.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_context_stream(
    channel: &runtime_collab::ChannelHandle,
    route: RouteSpec,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_context().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            struct PrivateContextLifetime;
            impl Drop for PrivateContextLifetime {
                fn drop(&mut self) {}
            }
            let _private_context_lifetime = PrivateContextLifetime;
            let mut private_person_basis = None;
            while let Ok(context) = stream.read().await {
                if context.snapshot_requested {
                    if !tx
                        .send_durable(CollabEvent::DocumentSnapshotRequested {
                            route: route.clone(),
                        })
                        .await
                    {
                        return;
                    }
                    continue;
                }
                if let Some(peer_id) = context.follow_changed {
                    if !tx
                        .send_durable(CollabEvent::FollowChanged { peer_id })
                        .await
                    {
                        return;
                    }
                    continue;
                }
                if let Some(message) = context.handoff {
                    if let Some(request) = super::handoff::HandoffRequest::decode(&message) {
                        if !tx.send_durable(CollabEvent::Handoff { request }).await {
                            return;
                        }
                    } else {
                        tracing::warn!("collab: malformed handoff request dropped");
                    }
                    continue;
                }
                if !context.records_arrived.is_empty() {
                    if !tx
                        .send_durable(CollabEvent::RecordsArrived {
                            records: context.records_arrived,
                        })
                        .await
                    {
                        return;
                    }
                    continue;
                }
                if let Some(agents) = context.history_agents {
                    let agents = agents
                        .into_iter()
                        .map(|agent| {
                            let [_, red, green, blue] = agent.color.to_be_bytes();
                            collab_types::history::RosterAgent {
                                device: agent.device,
                                person: agent.person,
                                person_name: agent.person_name,
                                agent_id: agent.agent_id,
                                label: agent.label,
                                color: [red, green, blue],
                            }
                        })
                        .collect();
                    if !tx
                        .send_durable(CollabEvent::HistoryAgents {
                            cluster_id: route.cluster_id.to_string(),
                            agents,
                        })
                        .await
                    {
                        break;
                    }
                    continue;
                }
                // Membership/permission replacement fences cached private bodies
                // before delivering the new public context. Presentation changes
                // alone must not cause a private contacts refresh.
                let mut people: Vec<_> = context
                    .collaborators
                    .iter()
                    .map(|person| (person.id.clone(), person.is_agent, person.is_self))
                    .collect();
                people.compact_sort();
                let mut permissions = context.permissions.clone();
                permissions.compact_sort();
                let basis = (people, permissions);
                if private_person_basis.as_ref() != Some(&basis) {
                    private_person_basis = Some(basis);
                }
                if !tx
                    .send_durable(CollabEvent::Context {
                        collaborators: context.collaborators,
                        permissions: context.permissions,
                        apps: context.apps,
                    })
                    .await
                {
                    return;
                }
            }
            // Context is a complete owner-backed membership snapshot. Losing
            // that source must withdraw it rather than retain stale people.
            let _ = tx
                .send_durable(CollabEvent::Context {
                    collaborators: Vec::new(),
                    permissions: Vec::new(),
                    apps: Vec::new(),
                })
                .await;
        });
    }
}

#[derive(Debug)]
struct NativeInvocationTurn {
    permits: Vec<tokio::sync::OwnedSemaphorePermit>,
    deadline: Instant,
}

// Use the existing three-second native publication hydration bound. The
// accepted invocation stays on its original stream task, without another queue.
async fn native_invocation_delivery_turn(
    key: &str,
    state: &BridgeState,
) -> Result<Option<NativeInvocationTurn>, String> {
    let Some(generation) = state.document_reads.delivery_generation(key) else {
        return Ok(None);
    };
    let deadline = crate::time::now() + Duration::from_secs(3);
    loop {
        // Enabled before the checks: a change between them still wakes it.
        let changed = state.document_reads.reader_wake();
        if !state.is_healthy() || !state.document_reads.current(key, &generation) {
            return Err("original invocation document custody was retired".into());
        }
        if crate::time::now() >= deadline {
            return Err("original invocation capture did not become current".into());
        }
        let mut turns = Vec::new();
        let owners = state.document_reads.document_owner_routes(key);
        for owner in &owners {
            let semaphore = state.document_reads.delivery_turn(owner)?;
            let remaining = deadline.saturating_duration_since(crate::time::now());
            let turn = tokio::time::timeout(remaining, semaphore.acquire_owned())
                .await
                .map_err(|_| "original invocation capture did not become current")?
                .map_err(|_| "original invocation delivery turn closed")?;
            turns.push(turn);
        }
        if !state.is_healthy() || !state.document_reads.current(key, &generation) {
            return Err("original invocation document custody was retired".into());
        }
        if crate::time::now() >= deadline {
            return Err("original invocation capture did not become current".into());
        }
        if state.document_reads.document_owner_routes(key) == owners
            && owners
                .iter()
                .all(|owner| state.document_reads.publishable(owner))
        {
            return Ok(Some(NativeInvocationTurn {
                permits: turns,
                deadline,
            }));
        }
        drop(turns);
        let remaining = deadline.saturating_duration_since(crate::time::now());
        if remaining.is_zero() {
            return Err("original invocation capture did not become current".into());
        }
        let _ = tokio::time::timeout(remaining, changed).await;
    }
}

// This existing host verb rechecks the original channel's current read
// custody and exact pending job. A waited delivery cannot reuse stale authority.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn recheck_native_invocation(
    channel: &runtime_collab::ChannelHandle,
    invocation: &runtime_collab::Invocation,
) -> Result<bool, String> {
    if !channel
        .invocation_cancellation(invocation.id)
        .await
        .map_err(|error| error.to_string())?
    {
        return Ok(true);
    }
    let update = tilo_actions::InvocationUpdate::Cancelled {
        invocation: tilo_actions::InvocationId(Uuid::from_bytes(invocation.invocation_uuid)),
        message: None,
    };
    let wire = serde_json::to_string(&update).map_err(|error| error.to_string())?;
    match channel.update_invocation(invocation.id, &wire).await {
        Ok(()) => {}
        // The peer will not take this invocation's cancellation, which is
        // about this invocation; a lost call leaves its outcome unknown.
        Err(error) if refused_by_peer(&error) => tracing::warn!(
            %error,
            id = invocation.id,
            "collab: the broker refused this invocation cancellation; the bridge stays open"
        ),
        Err(error) => return Err(error.to_string()),
    }
    Ok(false)
}

#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_invocation_stream(
    channel: &runtime_collab::ChannelHandle,
    spec: &RouteSpec,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_stable_invocations().await {
        let tx = inbound_tx.clone();
        let cancellation_channel = channel.clone();
        let key = spec.channel_id();
        tokio::task::spawn_local(async move {
            while let Ok(invocation) = stream.read().await {
                // Bound to the document current on arrival: if the App
                // replaces it before dispatch, the job is refused, never run
                // in the replacement.
                let owner = tx.state.document_reads.delivery_owner(&key);
                let turn = match native_invocation_delivery_turn(&key, &tx.state).await {
                    Ok(turn) => turn,
                    Err(error) => {
                        tx.state
                            .fail_closed("native invocation capture; outcome unknown");
                        tracing::warn!(%error, "accepted invocation retains unknown outcome");
                        break;
                    }
                };
                if let Some(turn) = &turn {
                    match tokio::time::timeout_at(
                        tokio::time::Instant::from_std(turn.deadline),
                        recheck_native_invocation(&cancellation_channel, &invocation),
                    )
                    .await
                    {
                        Ok(Ok(true)) if crate::time::now() < turn.deadline => {}
                        Ok(Ok(false)) => continue,
                        result => {
                            tx.state
                                .fail_closed("native invocation authority; outcome unknown");
                            tracing::warn!(?result, "accepted invocation authority unavailable");
                            break;
                        }
                    }
                }
                let id = invocation.id;
                let context = parse_invocation_context(id, &invocation.context_json);
                if !tx
                    .send_durable_turn(
                        CollabEvent::Invocation {
                            id,
                            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(
                                invocation.invocation_uuid,
                            )),
                            resource: invocation.resource,
                            action: invocation.action,
                            argument: invocation.argument,
                            actor: invocation.actor,
                            context: context.map(Box::new),
                        },
                        Some(EventTurn::Invocation(
                            turn.map(|turn| turn.permits).unwrap_or_default(),
                            owner,
                        )),
                    )
                    .await
                {
                    break;
                }
                let cancellation_tx = tx.clone();
                let channel = cancellation_channel.clone();
                tokio::task::spawn_local(async move {
                    loop {
                        tokio::time::sleep(crate::time::Duration::from_millis(100)).await;
                        match channel.invocation_cancellation(id).await {
                            Ok(false) => {}
                            Ok(true) => {
                                let _ = cancellation_tx
                                    .send_durable(CollabEvent::InvocationCancellation { id })
                                    .await;
                                break;
                            }
                            // The broker removes terminal invocations, so an
                            // unknown-id error is also the watcher stop signal.
                            Err(_) => break,
                        }
                    }
                });
            }
        });
    }
}

/// The primary route's live-activity capability and the task following the
/// controls pressed on it.
struct LiveActivityLink {
    handle: runtime_collab::LiveActivityHandle,
    follower: tokio::task::JoinHandle<()>,
}

impl Drop for LiveActivityLink {
    fn drop(&mut self) {
        self.follower.abort();
    }
}

impl LiveActivityLink {
    /// Send the app's activity if it changed, or again after a join.
    #[allow(
        clippy::future_not_send,
        reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
    )]
    async fn flush(&self, resend: bool) {
        let Some(unsent) = super::channel_live_activity::take_unsent(resend) else {
            return;
        };
        if let Err(error) = self.handle.show(unsent.activity.as_ref()).await {
            tracing::warn!(%error, "live activity: the shell did not take it");
        }
    }
}

/// Open the primary route's live activity and follow the controls pressed
/// on it, waking the owner on each.
fn link_live_activity(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<LiveActivityLink> {
    let handle = channel.live_activity();
    let follower = {
        let handle = handle.clone();
        let wake = inbound_tx.state.owner_wake.clone();
        tokio::task::spawn_local(async move {
            let mut after = 0;
            loop {
                match handle.controls(after).await {
                    Ok((serial, controls)) => {
                        after = serial;
                        super::channel_live_activity::received(controls);
                        wake.wake();
                    }
                    Err(error) => {
                        tracing::warn!(%error, "live activity: following its controls stopped");
                        break;
                    }
                }
            }
        })
    };
    Some(LiveActivityLink { handle, follower })
}

/// The primary route's settings capability, the task following it, and the
/// task sending the app's writes.
struct SettingsLink {
    handle: runtime_collab::AppSettingsHandle,
    settings: Arc<super::channel_settings::ChannelSettings>,
    follower: tokio::task::JoinHandle<()>,
    /// Sends the app's unsent writes, oldest first, each once the shell
    /// answered the one before it. The shell answers a write once it is
    /// stored, so the worker never waits on it: one task, woken by
    /// [`Self::flush`], keeps the writes in order.
    sender: tokio::task::JoinHandle<()>,
    send_wanted: Arc<tokio::sync::Notify>,
}

impl Drop for SettingsLink {
    fn drop(&mut self) {
        self.follower.abort();
        // A write it sent and the shell has not answered counts as refused
        // (`Unanswered`); the ones it has not sent yet are answered the same.
        self.sender.abort();
        self.settings.disconnected();
    }
}

fn settings_view(
    view: &runtime_collab::SettingsView,
) -> Result<(u64, serde_json::Map<String, serde_json::Value>), String> {
    match serde_json::from_slice(&view.document_json) {
        Ok(serde_json::Value::Object(document)) => Ok((view.revision, document)),
        _ => Err("the shell's settings view is not a JSON object".into()),
    }
}

impl SettingsLink {
    /// Have the link's sender send the app's unsent writes.
    fn flush(&self) {
        self.send_wanted.notify_one();
    }
}

/// Send `settings`' unsent writes over `handle`, oldest first, until none is
/// left, and show the shell's answers.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn send_settings(
    handle: &runtime_collab::AppSettingsHandle,
    settings: &super::channel_settings::ChannelSettings,
    wake: &crate::wake::WakeHandle,
) {
    loop {
        // Every taken write is answered, by the shell or as refused when
        // this task stops first.
        let outbox: Vec<_> = settings
            .take_outbox()
            .into_iter()
            .map(|write| (write, super::channel_settings::Unanswered::new(settings)))
            .collect();
        if outbox.is_empty() {
            return;
        }
        for ((section, values, action), unanswered) in outbox {
            let answer = match serde_json::to_vec(&values) {
                Ok(values) => handle
                    .update(&section, &values, action.as_deref())
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|view| settings_view(&view)),
                Err(error) => Err(error.to_string()),
            };
            if unanswered.answer(answer) {
                wake.wake();
            }
        }
    }
}

/// Register the owner's pull directory on `route`, counting the outcome
/// for [`BrokerTransport::action_directory_publication_counts`].
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn publish_directory(
    route: &SdkRoute,
    requests: mpsc::SyncSender<ActionDirectoryRequest>,
    inbound_tx: &InboundBridge,
) {
    let directory = directory_answerer(requests, inbound_tx.state.owner_wake.clone());
    if let Err(error) = route.channel.publish_action_directory(directory).await {
        inbound_tx
            .state
            .directory_rpc_failures
            .fetch_add(1, Ordering::AcqRel);
        tracing::warn!(%error, "collab: publishing pull resource/action directory failed");
    } else {
        inbound_tx
            .state
            .directory_rpc_successes
            .fetch_add(1, Ordering::AcqRel);
    }
    if inbound_tx
        .state
        .directory_observation_enabled
        .load(Ordering::Acquire)
    {
        inbound_tx.state.notify_events();
    }
}

/// Open the primary route's settings for the installed channel settings
/// and follow the shell's view of them, waking the owner on each change.
fn link_settings(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<SettingsLink> {
    let settings = super::channel_settings::installed()?;
    let handle = channel.settings();
    settings.linked();
    let follower = {
        let handle = handle.clone();
        let settings = Arc::clone(&settings);
        let wake = inbound_tx.state.owner_wake.clone();
        tokio::task::spawn_local(async move {
            let mut after = 0;
            loop {
                let view = match handle.read(after).await {
                    Ok(view) => view,
                    Err(error) => {
                        tracing::warn!(%error, "settings: following the shell's settings stopped");
                        settings.disconnected();
                        break;
                    }
                };
                after = view.revision;
                match settings_view(&view) {
                    Ok((revision, view)) => {
                        if settings.received(revision, view) {
                            wake.wake();
                        }
                    }
                    Err(error) => tracing::warn!(%error, "settings: ignoring a view"),
                }
            }
        })
    };
    let send_wanted = Arc::new(tokio::sync::Notify::new());
    let sender = {
        let handle = handle.clone();
        let settings = Arc::clone(&settings);
        let wake = inbound_tx.state.owner_wake.clone();
        let send_wanted = Arc::clone(&send_wanted);
        tokio::task::spawn_local(async move {
            loop {
                send_wanted.notified().await;
                send_settings(&handle, &settings, &wake).await;
            }
        })
    };
    Some(SettingsLink {
        handle,
        settings,
        follower,
        sender,
        send_wanted,
    })
}

/// The ops of a history page the local broker sent for `artifact`. One
/// that does not decode is left out of the page and logged: the broker
/// pages only what it admitted, so it is a fault on that side.
pub(super) fn decode_paged_ops(
    artifact: collab_types::ArtifactId,
    ops: &[Vec<u8>],
) -> Vec<SignedOp> {
    ops.iter()
        .filter_map(|bytes| match postcard::from_bytes::<SignedOp>(bytes) {
            Ok(op) => Some(op),
            Err(error) => {
                tracing::warn!(artifact = %artifact.0, %error, "collab: history page op does not decode");
                None
            }
        })
        .collect()
}

fn parse_invocation_context(id: u64, json: &str) -> Option<tilo_actions::ActionContext> {
    if json.trim().is_empty() {
        return None;
    }
    match serde_json::from_str(json) {
        Ok(context) => Some(context),
        Err(error) => {
            tracing::warn!(%error, id, "collab: ignoring invalid invocation context");
            None
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_action_catalogue_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Result<(), String> {
    let stream = channel
        .subscribe_action_catalogue()
        .await
        .map_err(|error| format!("subscribing to action catalogue failed: {error}"))?;
    let initial = stream
        .read()
        .await
        .map_err(|error| format!("reading initial action catalogue failed: {error}"))?;
    // The initial baseline occupies the same turn as subsequent watched
    // snapshots. Otherwise a busy constructor retains both before its first
    // owner poll, which can exhaust the bridge despite each baseline fitting.
    let turns = Arc::new(tokio::sync::Semaphore::new(1));
    let turn = turns
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| error.to_string())?;
    admit_initial_action_catalogue(&initial, inbound_tx, Some(turn))?;

    let tx = inbound_tx.clone();
    let channel = channel.clone();
    tokio::task::spawn_local(async move {
        let source = BrokerCatalogue { channel, stream };
        forward_action_catalogues(source, initial, &tx, turns).await;
    });
    Ok(())
}

/// Decode and admit the initial catalogue through the same bounded owner queue.
fn admit_initial_action_catalogue(
    initial: &str,
    tx: &InboundBridge,
    turn: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<(), String> {
    let change = serde_json::from_str::<CatalogueChange>(initial)
        .map_err(|error| format!("decoding initial action catalogue failed: {error}"))?;
    if !tx.send_reply_turn(
        CollabEvent::ActionCatalogue { change },
        turn.map(EventTurn::Snapshot),
    ) {
        return Err("delivering initial action catalogue failed".to_string());
    }
    Ok(())
}

/// Where a follower's catalogue changes come from: the stream it reads, and
/// the whole catalogue asked for again when a change does not apply.
trait CatalogueSource {
    /// The next change, as the broker sent it; `Err` once the stream ended.
    async fn read(&mut self) -> Result<String, ()>;
    /// Ask the broker for the whole catalogue: its next change is a reset,
    /// not a delta.
    async fn restart(&mut self) -> Result<(), ()>;
}

/// One connection's catalogue stream, replaced by a fresh subscription when a
/// change does not apply to what the follower holds.
struct BrokerCatalogue {
    channel: runtime_collab::ChannelHandle,
    stream: runtime_collab::ActionCatalogueStream,
}

impl CatalogueSource for BrokerCatalogue {
    async fn read(&mut self) -> Result<String, ()> {
        self.stream.read().await.map_err(|_| ())
    }

    async fn restart(&mut self) -> Result<(), ()> {
        self.stream = self
            .channel
            .subscribe_action_catalogue()
            .await
            .map_err(|_| ())?;
        Ok(())
    }
}

/// Follow the catalogue after `initial` (whose admission the caller does),
/// forwarding each change once it applies to a shadow of what the follower
/// holds. At most one change is queued at a time: the next read waits until
/// the owner has dequeued the previous one, and the broker composes what
/// changed meanwhile into the next.
///
/// A change that does not apply — the revisions do not join, it names a shape
/// the follower was never sent, or its bytes do not decode — asks the broker
/// for the whole catalogue at once, and that answer is forwarded as the next
/// change. The follower is never sent a change it cannot apply, and never left
/// with an empty catalogue: it keeps the last one that did until the reset
/// arrives.
async fn forward_action_catalogues(
    mut source: impl CatalogueSource,
    initial: String,
    tx: &InboundBridge,
    turns: Arc<tokio::sync::Semaphore>,
) {
    let mut held = ActionCatalogue::default();
    let admitted = match serde_json::from_str::<CatalogueChange>(&initial) {
        Ok(change) => match held.apply(&change) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "collab: the initial action catalogue does not apply; asking for the whole catalogue"
                );
                false
            }
        },
        Err(error) => {
            tracing::warn!(%error, "collab: invalid initial action catalogue change");
            false
        }
    };
    if !admitted && source.restart().await.is_err() {
        return;
    }
    // The whole catalogue was asked for above; a snapshot that does not apply
    // to what is held cannot be fixed by asking again, so it ends the stream
    // rather than looping on the broker.
    let mut asked = !admitted;
    loop {
        let Ok(turn) = turns.clone().acquire_owned().await else {
            break;
        };
        let Ok(text) = source.read().await else {
            break;
        };
        let decoded = match serde_json::from_str::<CatalogueChange>(&text) {
            Ok(decoded) => decoded,
            Err(error) if asked => {
                tracing::error!(%error, "collab: the whole action catalogue does not decode");
                break;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "collab: invalid broker action catalogue change; asking for the whole catalogue"
                );
                if source.restart().await.is_err() {
                    break;
                }
                asked = true;
                continue;
            }
        };
        match held.apply(&decoded) {
            Ok(()) => asked = false,
            Err(error) if asked => {
                tracing::error!(%error, "collab: the whole action catalogue does not apply");
                break;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "collab: the broker's action catalogue change does not apply; asking for the whole catalogue"
                );
                if source.restart().await.is_err() {
                    break;
                }
                asked = true;
                continue;
            }
        }
        if !tx
            .send_durable_turn(
                CollabEvent::ActionCatalogue { change: decoded },
                Some(EventTurn::Snapshot(turn)),
            )
            .await
        {
            break;
        }
    }
}

/// Bridge broker-addressed task leases only when this connection is an
/// admitted agent. The server refuses the subscription for people and for
/// unadmitted agent identities, leaving their normal collaboration intact.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_task_run_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_task_runs().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(run) = stream.read().await {
                if !tx.send_durable(CollabEvent::TaskRun(run)).await {
                    break;
                }
            }
        });
    }
}

/// Open `channel`'s presence handle and, if available, bridge its stream
/// into translated [`CollabEvent`]s on `inbound_tx`. Returns the handle so
/// the caller can store it on the route (needed for outbound presence
/// sends), regardless of whether the inbound stream subscribed.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn spawn_presence_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<runtime_collab::PresenceHandle> {
    let handle = channel.presence();
    if let Ok(stream) = handle.subscribe_all().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                if let Some(event) = translate_presence_event(event)
                    && !tx.send_presence(event)
                {
                    break;
                }
            }
        });
    }
    Some(handle)
}

/// How a queued document's wait for its route ended.
enum PublicationWait {
    Open,
    /// The route did not open in time.
    Late,
    /// The bridge stopped meanwhile; the failure is recorded.
    Stopped,
}

/// Which route a request travels on: the app's own, whose history every
/// change builds on, or one record's beside it, whose failure is that
/// record's alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RouteRole {
    App,
    Record,
}

/// Keep the accepted request and its queue byte reservation on the worker
/// stack while hydration catches up. No later barrier or command can overtake
/// it. Hydration runs independently on this same LocalSet.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn wait_for_document_publication(route: &SdkRoute, state: &BridgeState) -> PublicationWait {
    let deadline = tilo_rt::timer_now() + Duration::from_secs(3);
    let key = route.spec.channel_id();
    loop {
        // Enabled before the checks: a change between them still wakes it.
        let changed = state.document_reads.reader_wake();
        if !state.is_healthy() {
            state.publication_failed(
                "queued document publication",
                "worker invalidated before admission",
                true,
            );
            return PublicationWait::Stopped;
        }
        // The deadline is final, even when readiness changes concurrently.
        if tilo_rt::timer_now() >= deadline {
            return PublicationWait::Late;
        }
        if state.document_reads.publishable(&key) {
            return PublicationWait::Open;
        }
        #[cfg(test)]
        state.publication_gate_waits.fetch_add(1, Ordering::AcqRel);
        let _ = tokio::time::timeout_at(deadline, changed).await;
    }
}

/// Sign on the shovel thread through the workspace identity service, then use
/// the one signed publication. Missing or denied certification never
/// downgrades the document to unsigned traffic. A mutation larger than one
/// frame is sent as consecutive segments, each encrypted (on MLS routes),
/// signed and validated by the broker as its own envelope.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn send_document_mutation(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    mutation: collab_types::DocumentMutation,
    authorization: Option<std::sync::Arc<dyn crate::DocumentCustody>>,
    commit: Option<collab_types::WorkspaceCommit>,
    inbound: &InboundBridge,
) -> Result<(), DocumentPublicationError> {
    if let Some(authorization) = &authorization {
        authorization
            .check(&route.spec, &mutation)
            .map_err(|error| {
                inbound
                    .state
                    .publication_failed("original document contributors", error, true)
            })?;
    }
    if authorization.is_some() {
        return Err(inbound.state.publication_failed(
            "scoped document evaluator unavailable",
            "original custody requires the actual final owner",
            true,
        ));
    }
    let frames = match mutation.encode_frames() {
        Ok(frames) => frames,
        Err(error) => {
            // The size was checked before the change was queued, so this
            // is unreachable for SDK callers; never send a partial change.
            tracing::error!(%error, "collab: typed document mutation would not encode; dropping");
            return Err(inbound.state.publication_failed(
                "document signing or encoding",
                error,
                true,
            ));
        }
    };
    let claims = origin_claims(&route.spec.app_id, &mutation);
    let commit = origin_commit(route, signer, commit, &mutation);
    let count = frames.len();
    for (index, frame) in frames.into_iter().enumerate() {
        let frame = match &commit {
            Some(commit) => match commit.frame(&frame) {
                Ok(framed) => framed,
                Err(error) => {
                    return Err(inbound.state.publication_failed(
                        "workspace commit",
                        error.to_string(),
                        true,
                    ));
                }
            },
            None => frame,
        };
        if let Err(error) = send_document_frame(route, signer, frame, &claims, &inbound.state).await
        {
            if count > 1 {
                tracing::warn!(
                    segment = index,
                    segments = count,
                    "collab: segmented document transfer stopped; receivers will discard it"
                );
            }
            return Err(error);
        }
    }
    if let Some(commit) = commit {
        inbound.state.commits().record(commit);
    }
    Ok(())
}

/// The commit this publication carries: the action across every unit its
/// caller made it as, or one this unit alone. A caller that names a commit
/// that does not cover this unit publishes its change without one: the change
/// must not be lost because its action was described wrong.
///
/// The device is the one this transport signs with: a publisher holds no
/// signing key, so the commit a caller declares names the device as zeros and
/// the broker stamps it here. The device signature over the payload then
/// signs the commit, and a receiver reads the author off that signature.
fn origin_commit(
    route: &SdkRoute,
    signer: &LocalSdkDocumentSigner,
    declared: Option<collab_types::WorkspaceCommit>,
    mutation: &collab_types::DocumentMutation,
) -> Option<collab_types::WorkspaceCommit> {
    let app = route.spec.app_id.as_str();
    let record = route.spec.record.as_deref().unwrap_or("");
    let device = signer_device(signer);
    if let Some(mut commit) = declared {
        if commit.covers(app, record) {
            commit.author.device = device;
            return Some(commit);
        }
        tracing::error!(
            app,
            record,
            "collab: refusing to publish an action commit this unit is not part of; sending the change alone"
        );
        return None;
    }
    match collab_types::WorkspaceCommit::of_mutation(
        &route.spec.cluster_id.to_string(),
        app,
        record,
        &collab_types::CommitAuthor {
            principal: None,
            device,
        },
        mutation,
    ) {
        Ok(commit) => commit,
        Err(error) => {
            tracing::warn!(%error, app, record, "collab: a published change commits to no action");
            None
        }
    }
}

/// The claims a mutation's op carries: what each of its changes is, read
/// from the history entry the executor wrote into it. A change something
/// other than the executor wrote cannot be claimed; its op then carries no
/// claims, which a receiver deciding claims refuses.
#[cfg_attr(
    debug_assertions,
    expect(
        clippy::panic,
        reason = "a debug build's check that every change this device makes is claimed"
    )
)]
fn origin_claims(
    app_id: &str,
    mutation: &collab_types::DocumentMutation,
) -> Vec<collab_types::ChangeClaim> {
    crate::app_surface::claims_of(app_id, mutation).unwrap_or_else(|error| {
        #[cfg(debug_assertions)]
        if let collab_types::DocumentMutation::Incremental(bytes) = mutation
            && collab_types::parse_changes(bytes).is_ok()
            && let Err(unnamed) = collab_types::claim::named_changes(bytes)
        {
            panic!("a document change this device made names no action: {unnamed}");
        }
        tracing::warn!(%error, "collab: a document change carries no claim");
        Vec::new()
    })
}

/// Encrypt, sign and send one typed document frame, or hold it on the
/// device while the route waits for its channel keys. A frame the broker
/// refuses is sent once more after a key refresh that found newer keys: a
/// membership change moves the channel to a new epoch, and the broker refuses
/// content sealed under an older one.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn send_document_frame(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    cleartext: Vec<u8>,
    claims: &[collab_types::ChangeClaim],
    state: &BridgeState,
) -> Result<(), DocumentPublicationError> {
    match try_send_frame(route, signer, cleartext.clone(), claims, state).await {
        FrameSend::Sent | FrameSend::Held => {
            state.change_sent(&route.spec);
            Ok(())
        }
        FrameSend::Failed(error) => Err(error),
        FrameSend::Refused(refusal) => {
            if !refresh_route_keys(route, signer).await || !refusal.is_definite() {
                return Err(frame_refused(state, &route.spec, &refusal));
            }
            match try_send_frame(route, signer, cleartext, claims, state).await {
                FrameSend::Sent | FrameSend::Held => {
                    state.change_sent(&route.spec);
                    Ok(())
                }
                FrameSend::Failed(error) => Err(error),
                FrameSend::Refused(refusal) => Err(frame_refused(state, &route.spec, &refusal)),
            }
        }
    }
}

/// What a commit the broker would not take means. A definite refusal — the
/// peer answered about this very change — fails that change alone; anything
/// else (a lost call, an overload) leaves the outcome unknown and ends the
/// bridge as it always has.
fn frame_refused(
    state: &BridgeState,
    route: &RouteSpec,
    refusal: &FrameRefusal,
) -> DocumentPublicationError {
    match refusal {
        FrameRefusal::Definite(reason) => {
            state.note_refused_change(route, "document commit RPC", reason);
            DocumentPublicationError::Refused(format!("document commit RPC: {reason}"))
        }
        FrameRefusal::Unknown(reason) => {
            state.publication_failed("document commit RPC", reason, true)
        }
    }
}

/// The device key a signed publication will carry.
fn signer_device(signer: &LocalSdkDocumentSigner) -> [u8; 32] {
    match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.signing_public_key()
        }
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(key, _) | SdkDocumentSigner::Delegated(key, _),
        ) => key.verifying_key().to_bytes(),
        LocalSdkDocumentSigner::Host(identity) => identity.metadata().signing_public_key(),
    }
}

#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn try_send_frame(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    cleartext: Vec<u8>,
    claims: &[collab_types::ChangeClaim],
    state: &BridgeState,
) -> FrameSend {
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let artifact = collab_types::document_artifact_id(&route.spec.app_id, &workspace_id, record_id);
    let (payload, encryption_metadata) =
        match prepare_document_payload(&route.encryption, artifact, cleartext, claims) {
            PreparedFrame::Ready(payload, metadata) => (payload, metadata),
            PreparedFrame::Held => return FrameSend::Held,
            PreparedFrame::Failed(reason) => {
                return FrameSend::Failed(state.publication_failed(
                    "document encryption",
                    reason,
                    true,
                ));
            }
        };
    let now = crate::time::wall();
    let author = collab_types::Ed25519PublicKey::from_bytes(signer_device(signer));
    let attestation =
        collab_types::document_attestation_id(&route.spec.app_id, &workspace_id, record_id);
    let seq = collab_types::next_unordered_document_sequence(now);
    let mut op = match encryption_metadata {
        Some(metadata) => SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpKind::Change,
            now,
            seq,
            metadata,
        ),
        None => SignedOp::new_unsigned(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpKind::Change,
            now,
            seq,
        ),
    };
    // Only a sealed op carries its changes' claims: a relay cannot read the
    // changes, and a cleartext receiver reads each claim from its change.
    if op.encryption.is_some() {
        op = op.with_claims(claims.to_vec());
    }
    op = op.with_surface(crate::app_surface::hash_of(&route.spec.app_id));
    let unsigned_op = match postcard::to_stdvec(&op) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "collab: unsigned document envelope would not encode; dropping");
            return FrameSend::Failed(state.publication_failed(
                "document signing or encoding",
                error,
                true,
            ));
        }
    };
    let signature = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => match identity
            .sign_collaboration_op(&workspace_id, &route.spec.app_id, record_id, &unsigned_op)
        {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: identity service denied document operation; dropping"
                );
                if signing_refused(&error) {
                    return FrameSend::Failed(state.publication_refused(error));
                }
                return FrameSend::Failed(state.publication_failed(
                    "document signing or encoding",
                    error,
                    true,
                ));
            }
        },
        LocalSdkDocumentSigner::Host(identity) => match identity
            .sign_collaboration_op(&workspace_id, &route.spec.app_id, record_id, &unsigned_op)
            .await
        {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: host identity service denied document operation; dropping"
                );
                return FrameSend::Failed(state.publication_failed(
                    "host identity signing",
                    error,
                    !identity.is_usable(),
                ));
            }
        },
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(key, _) | SdkDocumentSigner::Delegated(key, _),
        ) => {
            use ed25519_dalek::Signer as _;
            let signing_bytes = match op.signing_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "collab: document op would not canonicalize; dropping");
                    return FrameSend::Failed(state.publication_failed(
                        "document signing or encoding",
                        error,
                        true,
                    ));
                }
            };
            key.sign(&signing_bytes).to_bytes()
        }
    };
    op.signature = collab_types::Signature::from_bytes(signature);
    match postcard::to_stdvec(&op) {
        Ok(bytes) => match route.channel.send(&bytes).await {
            Ok(()) => FrameSend::Sent,
            Err(error) if refused_by_peer(&error) => {
                tracing::warn!(
                    %error,
                    "collab: the broker refused this document change; it alone is not accepted"
                );
                FrameSend::Refused(FrameRefusal::Definite(error.to_string()))
            }
            Err(error) => {
                tracing::warn!(%error, "collab: document commit failed; outcome unknown");
                FrameSend::Refused(FrameRefusal::Unknown(error.to_string()))
            }
        },
        Err(error) => {
            tracing::warn!(%error, "collab: signed document envelope would not encode; dropping");
            FrameSend::Failed(state.publication_failed("document encoding", error, true))
        }
    }
}

/// Seal one frame under the route's current channel content key, so every
/// member — including one added later — can read it from history. A route
/// still waiting for its keys keeps the frame on the device instead.
fn prepare_document_payload(
    encryption: &SharedRouteKeys,
    artifact: collab_types::ArtifactId,
    cleartext: Vec<u8>,
    claims: &[collab_types::ChangeClaim],
) -> PreparedFrame {
    let Ok(mut keys) = encryption.lock() else {
        return PreparedFrame::Failed("route key state lock poisoned".into());
    };
    let retention = collab_types::ContentRetention::of_frame(&cleartext);
    match keys.seal(artifact, retention, cleartext.clone()) {
        super::route_keys::Sealed::Ready(payload, metadata) => {
            PreparedFrame::Ready(payload, metadata)
        }
        super::route_keys::Sealed::Hold => {
            if keys.hold_outbound(cleartext, claims.to_vec()) {
                PreparedFrame::Held
            } else {
                PreparedFrame::Failed("too many edits waiting for channel access".into())
            }
        }
        super::route_keys::Sealed::Refused(reason) => PreparedFrame::Failed(reason),
    }
}

enum PreparedFrame {
    Ready(Vec<u8>, Option<collab_types::EncryptionMetadata>),
    Held,
    /// Why the frame can neither be sent nor held. Carried into the logged
    /// failure: the reason is what tells an operator what to fix.
    Failed(String),
}

/// How one frame's send ended.
enum FrameSend {
    Sent,
    Held,
    /// Not sealed or not signed; retrying cannot help. Already recorded.
    Failed(DocumentPublicationError),
    /// The broker would not take it, possibly because its keys were stale.
    /// Not yet recorded: a retry under refreshed keys may still succeed.
    Refused(FrameRefusal),
}

/// Why a commit call did not go through, as far as this side can tell.
enum FrameRefusal {
    /// The peer read the change and would not accept it.
    Definite(String),
    /// No answer: the call was lost, cancelled or overloaded.
    Unknown(String),
}

impl FrameRefusal {
    /// Whether the peer answered about this change rather than the call
    /// failing on its way.
    fn is_definite(&self) -> bool {
        matches!(self, Self::Definite(_))
    }
}

impl std::fmt::Display for FrameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Definite(reason) | Self::Unknown(reason) => f.write_str(reason),
        }
    }
}

/// Ask the identity service for `route`'s current keys. Returns whether
/// the route's state or keys changed.
#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is retained only on the shovel LocalSet"
)]
async fn refresh_route_keys(route: &SdkRoute, signer: &mut LocalSdkDocumentSigner) -> bool {
    let Ok(known) = route
        .encryption
        .lock()
        .map(|keys| keys.asks_identity_service().then(|| keys.generation()))
    else {
        return false;
    };
    let Some(generation) = known else {
        return false;
    };
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let reply = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            let query = KeyQuery::of(route, generation);
            let mut fetched = certified_channel_keys(std::sync::Arc::clone(identity), vec![query])
                .await
                .into_iter();
            return fetched
                .next()
                .is_some_and(|(_, _, reply)| update_route_keys(route, reply, generation));
        }
        LocalSdkDocumentSigner::Host(identity) => {
            identity
                .channel_keys(&workspace_id, &route.spec.app_id, record_id, generation)
                .await
        }
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(_, Some(keys)) | SdkDocumentSigner::Delegated(_, keys),
        ) => {
            let query = KeyQuery::of(route, generation);
            let mut fetched = endpoint_channel_keys(std::sync::Arc::clone(keys), vec![query])
                .await
                .into_iter();
            return fetched
                .next()
                .is_some_and(|(_, _, reply)| update_route_keys(route, reply, generation));
        }
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(_, None)) => return false,
    };
    update_route_keys(route, answered_keys(reply, generation), generation)
}

/// The log target of every round of key questions an endpoint starts: what
/// tells an idle desktop that keeps asking from one that asks as it backs off.
const KEY_ROUNDS: &str = "tilo::collab::key_rounds";

/// Start a round of key questions for the routes `round` names. A certified
/// service is asked off this thread and the round is
/// returned to be awaited alongside the loop; a host's asynchronous service
/// is asked here, and its answers are taken at once.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn refresh_keys(
    routes: &std::collections::HashMap<String, SdkRoute>,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
    round: &KeyRound,
) -> Option<KeyFetch> {
    let keys: EndpointChannelKeys = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            let identity = std::sync::Arc::clone(identity);
            std::sync::Arc::new(
                move |workspace_id: &str, app_id: &str, record_id: &str, generation: u64| {
                    identity.channel_keys(workspace_id, app_id, record_id, generation)
                },
            )
        }
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(_, Some(keys)) | SdkDocumentSigner::Delegated(_, keys),
        ) => std::sync::Arc::clone(keys),
        _ => {
            tracing::debug!(target: KEY_ROUNDS, "collab: asking the host for channel keys");
            refresh_all_route_keys(routes, signer, inbound_tx, round).await;
            return None;
        }
    };
    let queries: Vec<KeyQuery> = routes
        .iter()
        .filter_map(|(key, route)| {
            let keys = route.encryption.lock().ok()?;
            round
                .asks(key, &keys)
                .then(|| KeyQuery::of(route, keys.generation()))
        })
        .collect();
    if queries.is_empty() {
        return None;
    }
    tracing::debug!(
        target: KEY_ROUNDS,
        routes = queries.len(),
        "collab: asking the identity service for channel keys"
    );
    Some(Box::pin(endpoint_channel_keys(keys, queries)))
}

/// Take a finished round's answers, then deliver what waited on them.
/// Returns whether any route's state or keys changed.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn apply_fetched_keys(
    routes: &std::collections::HashMap<String, SdkRoute>,
    fetched: FetchedKeys,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) -> bool {
    let mut changed = false;
    for (key, asked, reply) in fetched {
        let Some(route) = routes.get(&key) else {
            continue;
        };
        // A resend refreshed the route while the round was out: its answer
        // is newer than this one.
        if route
            .encryption
            .lock()
            .is_ok_and(|keys| keys.generation() == asked)
        {
            changed |= update_route_keys(route, reply, asked);
        }
        if !held_inbound::deliver_waiting(route, signer, inbound_tx).await {
            return changed;
        }
    }
    changed
}

/// Bring the keys of the routes `round` names up to date, report access
/// changes to the app, then send the edits and deliver the ciphertext that
/// waited on them.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn refresh_all_route_keys(
    routes: &std::collections::HashMap<String, SdkRoute>,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
    round: &KeyRound,
) {
    for (key, route) in routes {
        if route
            .encryption
            .lock()
            .is_ok_and(|keys| round.asks(key, &keys))
        {
            refresh_route_keys(route, signer).await;
        }
        if !held_inbound::deliver_waiting(route, signer, inbound_tx).await {
            return;
        }
    }
}

/// How the host's asks of an application's pull directory are answered: a
/// one-shot request to the App's main-thread pump below. It deliberately
/// holds no `Widget`, `App`, or document reference.
fn directory_answerer(
    requests: mpsc::SyncSender<ActionDirectoryRequest>,
    owner: crate::wake::WakeHandle,
) -> runtime_collab::wire::client::Answerer {
    use runtime_collab::wire::{Answer, Ask};
    std::sync::Arc::new(move |ask: Ask| {
        let requests = requests.clone();
        let owner = owner.clone();
        Box::pin(async move {
            match ask {
                Ask::Query { query, .. } => {
                    let (reply, response) = tokio::sync::oneshot::channel();
                    submit_directory_request(
                        &requests,
                        &owner,
                        ActionDirectoryRequest::Query { query, reply },
                    )
                    .map_err(|_| {
                        "application action directory is busy or unavailable".to_owned()
                    })?;
                    // Answered on the owner's next turn, however long its
                    // current one runs (a heavy action or frame): a busy app
                    // is not a gone one. The asker owns the deadline. A
                    // stopped owner drops `reply`.
                    let page = response
                        .await
                        .map_err(|_| "application action directory is unavailable".to_owned())?;
                    serde_json::to_string(&page)
                        .map(Answer::Page)
                        .map_err(|error| error.to_string())
                }
                Ask::Resolve {
                    revision,
                    resource,
                    action,
                    local_document,
                    ..
                } => {
                    let (reply, response) = tokio::sync::oneshot::channel();
                    let request = if local_document {
                        ActionDirectoryRequest::ResolveLocalDocumentAction {
                            revision,
                            resource,
                            action,
                            reply,
                        }
                    } else {
                        ActionDirectoryRequest::Resolve {
                            revision,
                            resource,
                            action,
                            reply,
                        }
                    };
                    let resolution =
                        if submit_directory_request(&requests, &owner, request).is_err() {
                            directory_unavailable()
                        } else {
                            // As for a query: the asker's deadline, not one of ours.
                            response.await.unwrap_or_else(|_| directory_unavailable())
                        };
                    serde_json::to_string(&resolution)
                        .map(Answer::Resolution)
                        .map_err(|error| error.to_string())
                }
            }
        })
    })
}

/// Queue one request for the App's pump and wake it: an idle owner sleeps
/// until an event arrives, and nothing else announces this one.
fn submit_directory_request(
    requests: &mpsc::SyncSender<ActionDirectoryRequest>,
    owner: &crate::wake::WakeHandle,
    request: ActionDirectoryRequest,
) -> Result<(), mpsc::TrySendError<ActionDirectoryRequest>> {
    requests.try_send(request)?;
    owner.notify_events();
    Ok(())
}

fn directory_unavailable() -> runtime_collab::ActionDirectoryResolution {
    runtime_collab::ActionDirectoryResolution::Unavailable
}

type TaskActionResult = Result<(u64, tilo_actions::InvocationUpdate), String>;

fn parse_task_update(id: u64, update: &str) -> TaskActionResult {
    serde_json::from_str(update)
        .map(|update| (id, update))
        .map_err(|error| error.to_string())
}

// Not `Send`: the route state it holds stays on the shovel thread's `LocalSet`.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the request variants share one ordered dispatch boundary onto the SDK route"
)]
async fn dispatch_sdk_request(
    route: &mut SdkRoute,
    request: CollabRequest,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
    role: RouteRole,
) -> bool {
    if matches!(
        request,
        CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
    ) {
        match wait_for_document_publication(route, &inbound_tx.state).await {
            PublicationWait::Open => {}
            PublicationWait::Stopped => return false,
            // A record's route can wait on another device (its keys come
            // from one that holds them): only that record waits, and its
            // owner sends it whole once the route opens.
            PublicationWait::Late if role == RouteRole::Record => {
                inbound_tx.state.route_unpublished(route.spec.channel_id());
                return false;
            }
            PublicationWait::Late => {
                inbound_tx.state.publication_failed(
                    "queued document publication",
                    "hydration did not become current before deadline",
                    true,
                );
                return false;
            }
        }
    }
    match request {
        CollabRequest::ExternalModeration {
            command,
            on_behalf_of,
            reply,
        } => {
            super::external_moderation::execute(&route.channel, &command, on_behalf_of, &reply)
                .await;
        }
        CollabRequest::Hello { .. } => {}
        CollabRequest::SignedOperation { envelope } => match postcard::to_stdvec(&*envelope) {
            Ok(bytes) => match route.channel.send(&bytes).await {
                Ok(()) => inbound_tx.state.change_sent(&route.spec),
                // A definite refusal is the peer's answer about this signed
                // change: it alone is not accepted and the bridge stays open.
                Err(error) if refused_by_peer(&error) => inbound_tx.state.note_refused_change(
                    &route.spec,
                    "signed document commit RPC",
                    &error,
                ),
                Err(error) => {
                    inbound_tx
                        .state
                        .publication_failed("signed document commit RPC", error, true);
                }
            },
            Err(error) => {
                inbound_tx
                    .state
                    .publication_failed("signed document encoding", error, true);
            }
        },
        CollabRequest::Document {
            mutation,
            authorization,
            commit,
        } => {
            // The helper records every failure, including a policy denial that
            // leaves the identity connection usable, before returning its result.
            let outcome =
                send_document_mutation(route, signer, mutation, authorization, commit, inbound_tx)
                    .await;
            return outcome.is_ok();
        }
        CollabRequest::OfferContent { hash, bytes } => {
            if let Err(error) = route.channel.offer_blob(&hash, &bytes).await {
                tracing::warn!(%error, %hash, "collab: offering record content failed");
            }
        }
        CollabRequest::WantContent { hash } => {
            if let Err(error) = route.channel.want_blob(&hash).await {
                tracing::warn!(%error, %hash, "collab: asking for record content failed");
            }
        }
        CollabRequest::RequestDocumentSnapshot => {
            if route.committed {
                return true;
            }
            if route.channel.request_document_snapshot().await.is_err() {
                inbound_tx
                    .state
                    .fail_closed("document snapshot request RPC");
            }
        }
        CollabRequest::Presence { key, payload } => {
            let Some(presence) = route.presence.as_ref() else {
                return true;
            };
            let key_bytes = match postcard::to_stdvec(&key) {
                Ok(key_bytes) => key_bytes,
                Err(error) => {
                    tracing::warn!(%error, "collab: presence key does not encode");
                    return true;
                }
            };
            match payload {
                PresencePayload::Pointer { x, y } => {
                    publish_presence("pointer", presence.publish_pointer(&key_bytes, x, y)).await;
                }
                PresencePayload::Caret { pos } => {
                    publish_presence("caret", presence.publish_caret(&key_bytes, pos as u64)).await;
                }
                PresencePayload::Selection { start, end } => {
                    publish_presence(
                        "selection",
                        presence.publish_selection(&key_bytes, start as u64, end as u64),
                    )
                    .await;
                }
                PresencePayload::Typing { start, end } => {
                    publish_presence(
                        "typing",
                        presence.publish_typing(&key_bytes, start as u64, end as u64),
                    )
                    .await;
                }
                PresencePayload::Arrived => {
                    publish_presence("arrived", presence.publish_arrived(&key_bytes)).await;
                }
                PresencePayload::Left => {
                    publish_presence("left", presence.publish_left(&key_bytes)).await;
                }
                // Shell chrome originates these directly on the peer-presence
                // route: following is an authenticated visual cue apps cannot
                // turn into a viewport command, and availability and waves
                // are the desktop's own presence, which no app speaks for.
                PresencePayload::Following { .. } | PresencePayload::Availability { .. } => {}
                PresencePayload::Interaction(update) => {
                    let json = match serde_json::to_string(&update) {
                        Ok(json) => json,
                        Err(error) => {
                            tracing::warn!(%error, "collab: presence interaction does not encode");
                            return true;
                        }
                    };
                    publish_presence(
                        "interaction",
                        presence.publish_interaction(&key_bytes, &json),
                    )
                    .await;
                }
                PresencePayload::Status { text, automatic } => {
                    publish_presence(
                        "status",
                        presence.publish_status(&key_bytes, &text, automatic),
                    )
                    .await;
                }
                PresencePayload::Composing { composing } => {
                    publish_presence(
                        "composing",
                        presence.publish_composing(&key_bytes, composing),
                    )
                    .await;
                }
            }
        }
        CollabRequest::FetchScrollback {
            artifact,
            before_seq,
            max,
        } => {
            let (ops, decrypted_payloads, oldest_seq, exhausted) = match route
                .channel
                .fetch_scrollback_for(artifact.0.as_bytes(), before_seq, u32::from(max))
                .await
            {
                Ok(batch) => {
                    let mut payloads = Vec::new();
                    let decoded = decode_paged_ops(artifact, &batch.ops);
                    let oldest_seq = decoded.iter().map(|op| op.seq).min();
                    let ops = decoded
                        .into_iter()
                        // The local broker pages what it admitted.
                        .filter_map(|op| match decrypt_document_payload(&op, &route.encryption, OpAdmission::CleartextAdmitted) {
                            // History pages arrive newest first, so a
                            // transfer split across pages completes when its
                            // oldest segment arrives.
                            Ok(payload) => {
                                if let Ok(mut keys) = route.encryption.lock() {
                                    keys.settle(&op);
                                }
                                DocumentReassembly::complete(
                                &inbound_tx.reassembly.history,
                                &op,
                                payload,
                            )
                            .map(|payload| {
                                payloads.push(payload);
                                Box::new(op)
                            })
                            }
                            Err(DecryptFailure::Later) => {
                                // Paged again once this device holds the key.
                                None
                            }
                            Err(DecryptFailure::Refused(error)) => {
                                tracing::warn!(%error, "collab: dropping undecryptable scrollback envelope");
                                None
                            }
                        })
                        .collect();
                    (ops, payloads, oldest_seq, batch.done)
                }
                Err(error) => {
                    // The app is told the history ends here, so the reason it
                    // saw nothing must be in the journal.
                    tracing::warn!(%error, "collab: scrollback could not be read; the app is told history ends here");
                    (Vec::new(), Vec::new(), None, true)
                }
            };
            let _ = inbound_tx
                .send_document(CollabEvent::ScrollbackBatch {
                    artifact,
                    ops,
                    decrypted_payloads,
                    oldest_seq,
                    exhausted,
                })
                .await;
        }
        CollabRequest::PublishActionSurface { delta } => {
            if let Err(error) = route.channel.publish_action_surface(&delta).await {
                tracing::warn!(%error, "collab: publishing resource/action surface failed; republishing");
                inbound_tx
                    .state
                    .action_surface_failed
                    .store(true, Ordering::Release);
                inbound_tx.state.notify_events();
            }
        }
        CollabRequest::ReportContinuation {
            schema_version,
            metadata,
        } => {
            if let Err(error) = route
                .channel
                .report_continuation(schema_version, &metadata)
                .await
            {
                tracing::warn!(%error, "collab: reporting continuation metadata failed");
            }
        }
        CollabRequest::ClearContinuation => {
            if let Err(error) = route.channel.clear_continuation().await {
                tracing::warn!(%error, "collab: clearing continuation metadata failed");
            }
        }
        CollabRequest::ReportHandoff { message } => {
            if let Err(error) = route.channel.handoff(&message).await {
                tracing::warn!(%error, "collab: reporting a handoff step failed");
            }
        }
        CollabRequest::PublishSearchIndex { delta_json } => {
            if let Err(error) = route.channel.publish_search_index(&delta_json).await {
                tracing::warn!(%error, "collab: publishing search index entries failed");
            }
        }
        CollabRequest::NativeMediaCommand { request_id, .. } => {
            let _ = inbound_tx.send_reply(CollabEvent::NativeMediaCommandResult {
                request_id,
                result: Err("native media requires the bounded control lane".into()),
            });
        }
        CollabRequest::RealtimeCommand {
            request_id,
            command,
        } => {
            let result = serde_json::to_vec(&command)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    if bytes.len() > collab_types::realtime::MAX_REALTIME_WIRE_BYTES {
                        return Err("realtime command exceeds local protocol bound".into());
                    }
                    Ok(bytes)
                });
            let result = match result {
                Ok(bytes) => route
                    .channel
                    .realtime_command(&bytes)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|reply| {
                        serde_json::from_slice::<collab_types::realtime::RealtimeReply>(&reply)
                            .map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            };
            let _ =
                inbound_tx.send_reply(CollabEvent::RealtimeCommandResult { request_id, result });
        }
        CollabRequest::NearbyTransferCommand {
            request_id,
            command,
        } => {
            let result = route
                .channel
                .nearby_transfer_command(&command)
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx
                .send_reply(CollabEvent::NearbyTransferCommandResult { request_id, result });
        }
        CollabRequest::LocalFileClipboard {
            request_id,
            command,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .local_file_clipboard_command(&command, on_behalf_of.map(|id| *id.0.as_bytes()))
                .await
                .map_err(|error| error.to_string());
            let _ =
                inbound_tx.send_reply(CollabEvent::LocalFileClipboardResult { request_id, result });
        }
        CollabRequest::OpenWorkspaceControl {
            session_id,
            target_device,
        } => match route.channel.open_workspace_control(target_device).await {
            Ok(handle) => {
                let reader = handle.clone();
                let tx = inbound_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(event) = reader.read().await {
                        let terminal =
                            matches!(event, runtime_collab::WorkspaceControlEvent::Closed(_));
                        if !tx
                            .send_durable(CollabEvent::WorkspaceControl { session_id, event })
                            .await
                            || terminal
                        {
                            break;
                        }
                    }
                });
                route.workspace_controls.insert(session_id, handle);
            }
            Err(error) => {
                let _ = inbound_tx.send_reply(CollabEvent::WorkspaceControl {
                    session_id,
                    event: runtime_collab::WorkspaceControlEvent::Closed(
                        runtime_collab::WorkspaceControlCloseReason::Denied(error.to_string()),
                    ),
                });
            }
        },
        CollabRequest::WorkspaceControlInput { session_id, event } => {
            if let Some(handle) = route.workspace_controls.get(&session_id)
                && let Err(error) = handle.send_input(&event).await
            {
                let _ = inbound_tx.send_reply(CollabEvent::WorkspaceControl {
                    session_id,
                    event: runtime_collab::WorkspaceControlEvent::Closed(
                        runtime_collab::WorkspaceControlCloseReason::Denied(error.to_string()),
                    ),
                });
            }
        }
        CollabRequest::CloseWorkspaceControl { session_id } => {
            // Dropping the session's handle ends it on the host.
            route.workspace_controls.remove(&session_id);
        }
        CollabRequest::UpdateInvocation { id, update } => match serde_json::to_string(&update) {
            Ok(json) => {
                if let Err(error) = route.channel.update_invocation(id, &json).await {
                    // Broker-owned acceptance and rejected progress are not
                    // owner durability acknowledgments. A refusal is the
                    // peer's answer about this invocation alone, so the
                    // bridge stays open; a lost terminal may follow an effect
                    // and still stops subsequent outbound work.
                    if refused_by_peer(&error) {
                        if let Some(failed) = refused_answer::failure(id, &update, &error) {
                            let sent = route.channel.update_invocation(id, &failed).await;
                            refused_answer::followed(id, sent);
                        }
                    } else {
                        tracing::warn!(%error, id, "collab: updating action invocation failed");
                        if update.is_terminal() {
                            inbound_tx.state.fail_closed_with_rpc_error(
                                "invocation update outcome unknown",
                                Some((id, &error)),
                            );
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, id, "collab: encoding action update failed");
                inbound_tx.state.fail_closed("invocation update encoding");
            }
        },
        CollabRequest::RecordLocalEffect { address, update } => {
            match serde_json::to_string(&update) {
                Ok(json) => {
                    if let Err(error) = route
                        .channel
                        .record_local_effect(&address.resource, &address.action, &json)
                        .await
                    {
                        tracing::warn!(%error, "collab: recording local effect receipt failed");
                        inbound_tx
                            .state
                            .fail_closed("effect receipt outcome unknown");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "collab: encoding local effect receipt failed");
                    inbound_tx.state.fail_closed("effect receipt encoding");
                }
            }
        }
        CollabRequest::RemoteAction { message } if message.waits() => {
            // The host holds this answer until the invocation moves; the
            // connection's other traffic must not wait behind it. Polled
            // once here, so the call still goes out in the order it was
            // asked (a cancel right after its invoke stays after it).
            let channel = route.channel.clone();
            let mut round_trip =
                Box::pin(async move { remote_action_round_trip(&channel, message).await });
            let first = std::future::poll_fn(|context| {
                std::task::Poll::Ready(round_trip.as_mut().poll(context))
            })
            .await;
            match first {
                std::task::Poll::Ready(answer) => {
                    let _ = inbound_tx.send_reply(CollabEvent::RemoteAction { message: answer });
                }
                std::task::Poll::Pending => {
                    let inbound = inbound_tx.clone();
                    tokio::task::spawn_local(async move {
                        let answer = round_trip.await;
                        let _ = inbound.send_reply(CollabEvent::RemoteAction { message: answer });
                    });
                }
            }
        }
        CollabRequest::RemoteAction { message } => {
            let answer = remote_action_round_trip(&route.channel, message).await;
            let _ = inbound_tx.send_reply(CollabEvent::RemoteAction { message: answer });
        }
        CollabRequest::ReportLocalDocumentAction { request_id, report } => {
            // Only the already authenticated route owns this call. In particular,
            // do not query the directory to rebase a stale captured revision.
            let result = tokio::time::timeout(
                Duration::from_secs(4),
                route.channel.admit_local_document_action(&report),
            )
            .await
            .map_err(|_| {
                runtime_collab::LocalDocumentActionError::Uncertain(
                    "local document report timed out; admission may be unknown".to_owned(),
                )
            })
            .and_then(std::convert::identity);
            let _ = inbound_tx
                .send_reply(CollabEvent::LocalDocumentActionReported { request_id, result });
        }
        CollabRequest::LaunchInstalledApplication {
            request_id,
            app_id,
            package_root,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .launch_installed_application_for(
                    &app_id,
                    package_root.as_deref(),
                    on_behalf_of.map(|id| *id.0.as_bytes()),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_reply(CollabEvent::InstalledApplicationLaunchResult { request_id, result });
        }
        CollabRequest::ReportTaskRun {
            request_id,
            run_id,
            lease,
            update,
        } => {
            let result = route
                .channel
                .report_task_run(run_id, &lease, &update)
                .await
                .map_err(|error| error.to_string());
            if let Some(request_id) = request_id {
                let _ =
                    inbound_tx.send_reply(CollabEvent::TaskRunReportResult { request_id, result });
            } else if let Err(error) = result {
                tracing::warn!(%error, run_id, "collab: reporting task run update failed");
            }
        }
        CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id,
            lease,
            peer_id,
            provider,
            resource,
            action,
            arguments,
            wait: Some(wait),
        } => {
            // The broker holds this answer until the child moves; the
            // connection's other traffic must not wait behind it. Polled
            // once here, so the re-ask still goes out in the order it was
            // asked, as a waiting remote action does.
            let channel = route.channel.clone();
            let mut round_trip = Box::pin(async move {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::Value::Object(arguments)
                );
                let target = runtime_collab::TaskAction {
                    peer_id,
                    provider: &provider,
                    resource: &resource,
                    action: &action,
                    argument: Some(&argument),
                };
                channel
                    .observe_task_child(run_id, &lease, target, &invocation.0.to_string(), wait)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|(id, update)| parse_task_update(id, &update))
            });
            let first = std::future::poll_fn(|context| {
                std::task::Poll::Ready(round_trip.as_mut().poll(context))
            })
            .await;
            let event =
                move |result| CollabEvent::TaskActionInvocationResult { request_id, result };
            match first {
                std::task::Poll::Ready(result) => {
                    let _ = inbound_tx.send_reply(event(result));
                }
                std::task::Poll::Pending => {
                    let inbound = inbound_tx.clone();
                    tokio::task::spawn_local(async move {
                        let _ = inbound.send_reply(event(round_trip.await));
                    });
                }
            }
        }
        CollabRequest::InvokeTaskAction {
            request_id,
            invocation,
            run_id,
            lease,
            peer_id,
            provider,
            resource,
            action,
            arguments,
            ..
        } => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(arguments)
            );
            let target = runtime_collab::TaskAction {
                peer_id,
                provider: &provider,
                resource: &resource,
                action: &action,
                argument: Some(&argument),
            };
            let result = route
                .channel
                .invoke_task_action(run_id, &lease, target, &invocation.0.to_string())
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| parse_task_update(id, &update));
            let _ = inbound_tx
                .send_reply(CollabEvent::TaskActionInvocationResult { request_id, result });
        }
        CollabRequest::TaskRunCancellation {
            request_id,
            run_id,
            lease,
        } => {
            let result = route
                .channel
                .task_run_status(run_id, &lease)
                .await
                .map_err(|error| error.to_string())
                .map(|status| {
                    if let Some(control) = status.control {
                        let _ = inbound_tx.send_reply(CollabEvent::TaskRunControl {
                            request_id,
                            control,
                        });
                    }
                    status.cancelled
                });
            let _ = inbound_tx.send_reply(CollabEvent::TaskRunCancellation { request_id, result });
        }
    }
    true
}
/// Pass a single-frame document event through, or hold a segment until its
/// transfer completes and return the complete mutation exactly once.
fn complete_document_event(
    event: CollabEvent,
    reassembler: &std::sync::Mutex<collab_types::DocumentReassembler>,
) -> Option<CollabEvent> {
    let CollabEvent::Document {
        envelope,
        from_user_id,
        decrypted_payload,
    } = event
    else {
        return Some(event);
    };
    let decrypted_payload =
        DocumentReassembly::complete(reassembler, &envelope, decrypted_payload)?;
    Some(CollabEvent::Document {
        envelope,
        from_user_id,
        decrypted_payload,
    })
}

/// Shared per-route key state: the op stream opens with it, sends seal
/// with it, and key refreshes update it.
type SharedRouteKeys = std::sync::Arc<std::sync::Mutex<super::route_keys::RouteKeys>>;

/// Translate an SDK [`runtime_collab::PresenceEvent`] into a
/// `CollabEvent`. Extracted so the unit test can exercise it without a broker.
fn translate_presence_event(evt: runtime_collab::PresenceEvent) -> Option<CollabEvent> {
    // Every variant must carry a decodable semantic `PresenceKey`.
    // An undecodable scope cannot truthfully identify a target. Drop
    // it instead of inventing a tab and making malformed presence
    // look like a collaborator left some unrelated surface.
    let key: PresenceKey = match postcard::from_bytes(&evt.key_bytes) {
        Ok(key) => key,
        Err(error) => {
            tracing::warn!(user = %evt.user_id, %error, "collab: refused presence whose key does not decode");
            return None;
        }
    };
    let payload = match evt.payload {
        runtime_collab::PresencePayload::Caret { pos } => PresencePayload::Caret {
            // A wire-supplied offset that doesn't fit `usize` on this
            // target can't address anything real; clamp rather than
            // wrap it into an unrelated in-range position.
            pos: usize::try_from(pos).unwrap_or(usize::MAX),
        },
        runtime_collab::PresencePayload::Selection { start, end } => PresencePayload::Selection {
            start: usize::try_from(start).unwrap_or(usize::MAX),
            end: usize::try_from(end).unwrap_or(usize::MAX),
        },
        runtime_collab::PresencePayload::Typing { start, end } => PresencePayload::Typing {
            start: usize::try_from(start).unwrap_or(usize::MAX),
            end: usize::try_from(end).unwrap_or(usize::MAX),
        },
        runtime_collab::PresencePayload::Following { target } => {
            PresencePayload::Following { target }
        }
        runtime_collab::PresencePayload::InteractionJson(json) => {
            let update = match serde_json::from_str::<tilo_actions::SemanticInteraction>(&json)
                .map_err(|error| error.to_string())
                .and_then(|update| {
                    update
                        .validate()
                        .map(|()| update)
                        .map_err(|error| error.to_string())
                }) {
                Ok(update) => update,
                Err(error) => {
                    tracing::warn!(user = %evt.user_id, %error, "collab: refused a presence interaction");
                    return None;
                }
            };
            PresencePayload::Interaction(Box::new(update))
        }
        runtime_collab::PresencePayload::Status { text, automatic } => {
            PresencePayload::Status { text, automatic }
        }
        runtime_collab::PresencePayload::Composing { composing } => {
            PresencePayload::Composing { composing }
        }
        runtime_collab::PresencePayload::Arrived => PresencePayload::Arrived,
        runtime_collab::PresencePayload::Left => PresencePayload::Left,
        // Keeps the coordinate space its key carries.
        runtime_collab::PresencePayload::Pointer { x, y } => PresencePayload::Pointer { x, y },
    };
    Some(CollabEvent::RemotePresenceUpdate {
        user_id: evt.user_id,
        key,
        payload,
    })
}

// The tests, with the fake broker and the fixtures they stand up. The
// toolkit's own tests drive the manager through those fixtures
// (`test-support`); there the tests themselves are left out.
#[cfg(any(test, feature = "test-support"))]
#[path = "broker_tests.rs"]
mod tests;

/// The fake broker and the managers the toolkit's App-driven tests stand up
/// against it. Only for tests: nothing here is a native authority proof.
#[cfg(all(feature = "test-support", unix))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixtures for the toolkit's tests"
)]
pub mod test_support {
    use super::{CollabRequest, CollaborationManager, RouteSpec, SdkConnectSource, tests};
    use std::os::fd::RawFd;
    use uuid::Uuid;

    pub use tests::{
        Captures, FakeBroker, SharedCaptures, eventually, notebook_action_catalogue_fixture,
        stand_up_fake_broker, wake_channel,
    };

    /// A manager on the broker at `fd`, joined to `app`'s route in `workspace`.
    #[must_use]
    pub fn manager_on(fd: RawFd, app: &str, workspace: Uuid) -> CollaborationManager {
        CollaborationManager::from_sdk_fd_for_test(fd, app, workspace)
    }

    /// A manager on the broker at `fd`, joined as a native window to the
    /// route of `record` of `app` in `workspace`.
    ///
    /// # Panics
    /// When the route cannot be joined.
    #[must_use]
    pub fn manager_on_record_route(
        fd: RawFd,
        app: String,
        workspace: Uuid,
        record: Option<String>,
    ) -> CollaborationManager {
        CollaborationManager::for_route_from(
            SdkConnectSource::Fd(fd),
            app,
            workspace,
            record,
            None,
            None,
            "person",
        )
        .expect("the record route joins")
    }

    /// Whether the broker worker behind `manager` may publish on `route`.
    #[must_use]
    pub fn document_reads_publishable(manager: &CollaborationManager, route: &RouteSpec) -> bool {
        tests::broker_state(manager)
            .document_reads
            .publishable(&route.channel_id())
    }

    /// A manager joined to the Browser's route in `cluster` while the broker
    /// holds an action catalogue of `count` notebook pages: whether the
    /// route was admitted and the worker
    /// stayed healthy, and how many catalogue subscriptions it made.
    pub fn ordinary_catalogue_manager(
        cluster: Uuid,
        count: usize,
    ) -> (
        CollaborationManager,
        SharedCaptures,
        std::thread::JoinHandle<()>,
        Result<(), runtime_portal::NotAvailable>,
        bool,
    ) {
        let snapshot = notebook_action_catalogue_fixture(cluster, count, 1, "notebook");
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures {
                action_catalogue_fixture: Some(vec![snapshot]),
                ..Captures::default()
            })),
            ..FakeBroker::default()
        };
        let (fd, captures, worker) = stand_up_fake_broker(broker);
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(super::SdkJoinIdentity {
                presentation: super::harness_presentation(),
                participant_kind: "person",
                signer: super::harness_document_signer(),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::with_transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "so.tilo.browser".into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        }));
        let admission = manager.await_primary_route();
        let healthy = tests::broker_state(&manager).is_healthy();
        (manager, captures, worker, admission, healthy)
    }
}
