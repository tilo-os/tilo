//! The broker transport: the `runtime-collab` collaboration client on a
//! worker thread the transport owns.
//!
//! The worker (the "shovel") runs a `current_thread` Tokio runtime and a
//! `LocalSet` that keeps every route's state on that thread. The owner thread
//! reaches it over a pair of bounded queues: [`CollabTransport::submit`]
//! enqueues commands and [`CollabTransport::drain`] empties the event queue.
//! Overload never closes the bridge. Only transient presence is shed;
//! other durable commands that do not fit wait in the owner's ordered spill
//! (a newer action surface replaces an older one) until the shovel frees
//! room. Document mutations have their own budget and are deferred (`Busy`)
//! or refused (`TooLarge`). Joining,
//! document signing and envelope verification all happen on the worker with
//! the certified launcher (or host-supplied) identity.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the shovel thread bridges a blocking socket"
)]

#[cfg(test)]
use super::channel_key;
use super::route_keys::Arrived;
use super::{
    ActionDirectoryRequest, Backend, BrokerActionCatalogueWire, BrokerActionSurface, CollabEvent,
    CollabRequest, CollabTransport, CollaborationManager, Collaborator, DocumentMutation,
    DocumentPublicationError, DocumentPublicationReceipt, ExportedAction, ExportedInput,
    PresenceKey, PresencePayload, PrimaryRouteState, RouteSpec, SignedOp, TransportCommand,
    TransportError, TransportStatus, Uuid, auto_hello_env, mpsc, presence_handle_env,
};
use runtime_collab::OpAdmission;
use tilo_sort::CompactSort as _;

// Worker-owned Cloud facilities: committed-document reads, the paged
// directory client, managed sender attempts and the native media lane all
// share this worker's queues, byte budgets and route handles.
#[path = "audio_output.rs"]
mod audio_output;
#[path = "broker_bytes.rs"]
mod bytes;
#[path = "refused_answer.rs"]
mod refused_answer;
#[path = "task_agent_route.rs"]
mod task_agent_route;
use bytes::{command_bytes, event_diagnostics, fit_catalogue, remote_action_round_trip};
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
use key_fetch::{
    FetchedKeys, KeyFetch, KeyQuery, answered_keys, certified_channel_keys, endpoint_channel_keys,
    update_route_keys,
};
use key_schedule::{KeyRound, KeySchedule, KeySignal};
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

/// A disconnected/slow UI must apply backpressure to broker callbacks rather
/// than allow an unbounded cross-thread queue to become a memory `DoS`.
const ACTION_DIRECTORY_REQUEST_QUEUE: usize = 64;

/// Whether the identity service answered and declined to sign, as opposed
/// to being unreachable: it reports a refusal as permission denied.
/// Presence is a room signal, not the app's request: a publish the peer
/// refused is logged where it happened, and the request that carried the
/// update still answers.
async fn publish_presence(
    what: &'static str,
    publish: impl std::future::Future<Output = Result<(), runtime_portal::NotAvailable>>,
) {
    if let Err(error) = publish.await {
        tracing::debug!(%error, presence = what, "collab: presence publish refused");
    }
}
fn signing_refused(error: &runtime_portal::NotAvailable) -> bool {
    matches!(
        error,
        runtime_portal::NotAvailable::ConnectionFailed(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
    )
}

/// One absolute deadline for each host identity request/response exchange.
/// The identity client drops a failed stream, so this also bounds shutdown of
/// partial and late replies without blocking the shovel's `LocalSet`.
const HOST_IDENTITY_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of UI-to-shovel commands waiting behind a slow broker.
/// This deliberately caps commands, rather than bytes, because the protocol
/// already bounds its payloads at each broker boundary.
const SDK_COMMAND_QUEUE: usize = 256;
/// Maximum number of shovel-to-UI events waiting behind a busy event loop.
const SDK_EVENT_QUEUE: usize = 256;
/// Aggregate retained payload budget for each cross-thread queue. This is
/// independent of the item-count cap because a single document snapshot or
/// invocation argument may be large.
const SDK_COMMAND_QUEUE_BYTES: usize = 4 * 1024 * 1024;
const SDK_EVENT_QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// The retained budget of the one action catalogue snapshot in flight
/// (each read waits for the previous one to be taken): apart from control
/// events, so a large workspace's catalogue neither starves action results
/// nor fails the bridge. A snapshot larger than this is trimmed to fit
/// ([`fit_catalogue`]).
const SDK_CATALOGUE_EVENT_BYTES: usize = 16 * 1024 * 1024;
/// Separate retained budgets for document traffic. A document change may be
/// as large as [`collab_types::MAX_DOCUMENT_TRANSFER_BYTES`]; accounting it
/// apart from control traffic means one large edit in flight can neither
/// starve action results nor make them fail the bridge. When the budget is
/// exhausted, outbound documents report [`TransportError::Busy`] so the
/// caller retries, and inbound document events wait for the owner to drain
/// rather than disconnecting.
const SDK_DOCUMENT_COMMAND_BYTES: usize =
    collab_types::MAX_DOCUMENT_TRANSFER_BYTES + SDK_COMMAND_QUEUE_BYTES;
const SDK_DOCUMENT_EVENT_BYTES: usize =
    collab_types::MAX_DOCUMENT_TRANSFER_BYTES + SDK_EVENT_QUEUE_BYTES;
/// Re-check interval while an inbound document event waits for budget.
const SDK_DOCUMENT_EVENT_RETRY: Duration = Duration::from_millis(250);
const WIFI_CONTINUITY_TIMEOUT: Duration = Duration::from_secs(3);

/// The connected transport's outbound queue, as the process reaches it from
/// outside the transport: the session's published resources
/// ([`SessionResource`]) and the device commands
/// ([`wifi_continuity_command`], [`speaker_command`]).
static PUBLISHED_COMMAND_BRIDGE: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio_mpsc::WeakSender<QueuedSdkCommand>>>,
> = std::sync::OnceLock::new();

/// Invoke the shell's launcher-authenticated, sealed Wi-Fi continuity path.
/// Callers should run this off the frame thread; the bounded timeout turns a
/// lost broker into an ordinary unavailable result.
///
/// # Errors
///
/// Returns an unavailable message when the collaboration bridge is absent,
/// its bounded queue rejects the command, or the broker does not reply in time.
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
///
/// # Errors
///
/// Returns an unavailable message when the collaboration bridge is absent,
/// its bounded queue rejects the command, or the broker does not reply in time.
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

pub fn subscribe_person_chat(
    peer: String,
    original: Vec<u8>,
    token: uuid::Uuid,
    cancel: tokio::sync::watch::Receiver<()>,
) -> Result<(), String> {
    let sender = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "private person bridge unavailable")?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or("private conversations need the certified native host")?;
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::SubscribePersonChat {
            peer,
            original,
            token,
            cancel,
        },
        DISPLAY_COMMAND_BUDGET.get_or_init(BridgeState::new),
    ) {
        return Err("private subscription queue unavailable".into());
    }
    Ok(())
}

pub fn person_chat_command(command: Vec<u8>, original: Vec<u8>) -> Result<Vec<u8>, String> {
    if command.len() > 24 * 1024 || !matches!(original.len(), 0 | 16) {
        return Err("invalid private conversation request".into());
    }
    let sender = PUBLISHED_COMMAND_BRIDGE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .map_err(|_| "private person bridge unavailable")?
        .as_ref()
        .and_then(tokio_mpsc::WeakSender::upgrade)
        .ok_or("private conversations need the certified native host")?;
    let (reply, receiver) = mpsc::sync_channel(1);
    if !enqueue_sdk_command(
        &sender,
        SdkCommand::PersonChat {
            command,
            original,
            reply,
        },
        DISPLAY_COMMAND_BUDGET.get_or_init(BridgeState::new),
    ) {
        return Err("private conversation queue unavailable".into());
    }
    receiver
        .recv_timeout(Duration::from_secs(4))
        .map_err(|_| "private conversation reply unavailable".to_string())?
}

/// Ask the native host to inspect or change its display transaction. Call off
/// the frame thread; the existing bridge and response wait remain bounded.
///
/// # Errors
/// Reports disconnected authority, refused requests and timed out replies.
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
    /// Hand a command to the shovel. A full bounded queue is backpressure,
    /// never a reason to disable collaboration: a durable command that does
    /// not fit waits in [`Self::spill`] (in order, with a newer action
    /// surface replacing an older one) and is sent as the shovel frees room.
    /// Presence is shed and document changes report `Busy`, as before.
    fn enqueue(&mut self, command: SdkCommand) -> Result<(), TransportError> {
        self.flush_spill();
        if !self.spill.is_empty() || !self.reserve_barrier_slot() {
            // Everything behind a parked command stays behind it, and
            // nothing goes ahead of the next barrier's slot.
            if command.is_transient_presence() {
                return Err(TransportError::Shed);
            }
            if command.document_len().is_some() {
                self.state.command_bytes.arm_room_wake();
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
            // A whole surface supersedes an older one still waiting.
            SdkCommand::Primary(CollabRequest::PublishActions { actions }) => {
                let waiting = self.spill.iter_mut().find(|parked| {
                    matches!(
                        parked,
                        SdkCommand::Primary(CollabRequest::PublishActions { .. })
                    )
                });
                let whole = SdkCommand::Primary(CollabRequest::PublishActions { actions });
                if let Some(older) = waiting {
                    *older = whole;
                } else {
                    self.spill.push_back(whole);
                }
            }
            command => self.spill.push_back(command),
        }
        // Arm before the retry so a release racing it is never missed.
        self.state.command_bytes.arm_room_wake();
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
                    self.state.command_bytes.arm_room_wake();
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
                    self.state.command_bytes.arm_room_wake();
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
            crate::person_chat::invalidate();
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
    BrowserServiceSession {
        route: RouteSpec,
        command: runtime_collab::BrowserServiceSessionCommand,
        wake: Option<crate::WakeHandle>,
        reply: mpsc::SyncSender<Result<runtime_collab::BrowserServiceSessionReply, String>>,
    },

    Initialize {
        route: RouteSpec,
        request: runtime_collab::committed_document::Request,
        reply: mpsc::SyncSender<Result<[u8; 32], String>>,
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
    SubscribePersonChat {
        peer: String,
        original: Vec<u8>,
        token: uuid::Uuid,
        cancel: tokio::sync::watch::Receiver<()>,
    },
    PersonChat {
        command: Vec<u8>,
        original: Vec<u8>,
        reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
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
struct BridgeState {
    connection_snapshots: Arc<
        std::sync::Mutex<
            std::collections::HashMap<
                String,
                runtime_collab::committed_document::ConnectionSnapshot,
            >,
        >,
    >,
    document_reads: committed_document::Reads,
    /// Why each route whose last join failed failed, by channel: a route
    /// that never joined says so instead of staying unready without a word.
    route_join_failures: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// The kind each joined route's record holds, by channel: a change that
    /// arrives on a route awaiting its first join re-joins it naming the kind
    /// the owner opened the record with.
    route_record_kinds: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    healthy: Arc<AtomicBool>,
    publication_failure: Arc<std::sync::Mutex<Option<DocumentPublicationError>>>,
    /// The last change the peer answered but would not accept, when nothing
    /// later has gone through. It is not a publication failure: the bridge
    /// stays up and later changes and publications go on, so the owner is
    /// told which change to reconcile and never mistakes it for a lost
    /// connection (see [`Self::note_refused_change`]).
    last_refused_change: Arc<std::sync::Mutex<Option<String>>>,
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
struct ByteBudget(Arc<AtomicUsize>, Arc<tokio::sync::Notify>, Option<RoomWake>);

/// Wakes the owner once outbound room frees after one of its sends was
/// refused as busy. The owner has no timer of its own for that retry, so
/// without this a refused edit would wait for unrelated input.
#[derive(Clone)]
struct RoomWake {
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

    fn with_room_wake(room: RoomWake) -> Self {
        Self(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(tokio::sync::Notify::new()),
            Some(room),
        )
    }

    /// Ask for an owner wake at the next release of any reservation sharing
    /// this budget's latch. Arm before re-checking for room, so a release
    /// racing the refusal is never missed.
    fn arm_room_wake(&self) {
        if let Some(room) = &self.2 {
            room.wanted.store(true, Ordering::Release);
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
    async fn until_room(&self, bytes: usize, limit: usize) {
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
        if let Some(room) = &self.budget.2
            && room.wanted.swap(false, Ordering::AcqRel)
        {
            room.owner.notify_events();
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
    fn new() -> Self {
        let owner_wake = crate::wake::WakeHandle::new();
        // Both outbound budgets share one latch: a refused document send is
        // queued behind commands of either kind.
        let outbound_room = RoomWake {
            wanted: Arc::new(AtomicBool::new(false)),
            owner: owner_wake.clone(),
        };
        Self {
            connection_snapshots: Arc::default(),
            document_reads: committed_document::Reads::default(),
            route_join_failures: Arc::default(),
            route_record_kinds: Arc::default(),
            healthy: Arc::new(AtomicBool::new(true)),
            publication_failure: Arc::new(std::sync::Mutex::new(None)),
            last_refused_change: Arc::new(std::sync::Mutex::new(None)),
            action_surface_failed: Arc::new(AtomicBool::new(false)),
            directory_rpc_successes: Arc::new(AtomicU64::new(0)),
            directory_rpc_failures: Arc::new(AtomicU64::new(0)),
            directory_observation_enabled: Arc::new(AtomicBool::new(false)),
            command_bytes: ByteBudget::with_room_wake(outbound_room.clone()),
            event_bytes: ByteBudget::new(),
            document_command_bytes: ByteBudget::with_room_wake(outbound_room),
            document_event_bytes: ByteBudget::new(),
            catalogue_event_bytes: ByteBudget::new(),
            owner_wake,
            wake_pending: Arc::new(AtomicBool::new(false)),
            connect: Arc::new(std::sync::Mutex::new(ConnectPhase::Connecting)),
            join_hold: Arc::default(),
            #[cfg(test)]
            publication_gate_waits: Arc::default(),
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
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
    fn note_refused_change(&self, context: &'static str, error: impl std::fmt::Display) {
        let refusal = DocumentPublicationError::Refused(format!("{context}: {error}"));
        let said = refusal.to_string();
        tracing::warn!(
            direction = context,
            %error,
            "collab: the broker refused one document change; this bridge stays open"
        );
        *self.last_refused_change.lock_recover() = Some(said);
        self.notify_events();
    }

    /// A change the peer refused while nothing later has been sent, if any.
    fn refused_change(&self) -> Option<String> {
        self.last_refused_change.lock_recover().clone()
    }

    /// A change went through: nothing is waiting to be reconciled any more.
    fn change_sent(&self) {
        let cleared = self.last_refused_change.lock_recover().take().is_some();
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
                budget.arm_room_wake();
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
                budget.arm_room_wake();
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
        if transient {
            return Err(Refused::error(TransportError::Shed));
        }
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
            if transient {
                return Err(Refused::error(TransportError::Shed));
            }
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
/// keys, as an identity service answers them: the record's room, its app,
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
    /// them its writes are plaintext, and refused in a room that seals them.
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
fn sdk_join_identity_from<I: SdkCertifiedIdentity + 'static>(
    identity: I,
    cluster_id: Uuid,
) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    let device_certificate = identity.device_certificate().ok_or_else(|| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "workspace identity has no certified device",
        ))
    })?;

    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id)?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id,
        device_certificate: device_certificate.to_vec(),
        proof: proof.to_vec(),
    }
    .encode()
    .map_err(|error| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("encoding certified collaboration join failed: {error}"),
        ))
    })?;
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
        .ok_or_else(|| {
            runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "workspace identity has no certified device",
            ))
        })?;
    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id).await?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
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
    })?;
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

/// What a native launch that could not connect reports: that the workspace
/// needs an identity chosen, which the launch waits for, or why the service
/// failed.
#[cfg(feature = "native")]
fn native_launch_refusal(error: &runtime_portal::NotAvailable) -> crate::NativeLaunchError {
    if runtime_identity::is_no_identity_selected(error) {
        crate::NativeLaunchError::IdentityRequired
    } else {
        crate::NativeLaunchError::Operation(error.to_string())
    }
}

#[cfg(all(test, feature = "native"))]
#[path = "native_launch_refusal_tests.rs"]
mod native_launch_refusal_tests;

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
    room: tokio::sync::watch::Sender<Result<runtime_collab::RoomStatus, String>>,
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
    /// The broker reports the room as `status` from now on.
    pub fn set_room(&self, status: runtime_collab::RoomStatus) {
        let _ = self.room.send(Ok(status));
    }
    /// The broker cannot be asked how the room stands, and why.
    pub fn room_unreachable(&self, reason: &str) {
        let _ = self.room.send(Err(reason.to_owned()));
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
    /// Connect to the launcher-inherited typed collaboration service and
    /// announce an explicit route. Headless participants use this instead of
    /// relying on process-wide `QUICK_APP_ID` / `TILO_WORKSPACE_ID` variables.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the launcher did not
    /// hand down a collab service connection.
    pub fn for_route(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    /// Connect a headless service of `app_id` in `cluster_id` that holds no
    /// document of its own: it publishes actions and answers them. It never
    /// asks the broker for a committed document, which a room holds only for
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

    /// Hold `route`, a record of `kind` that need not exist yet, over `fd`,
    /// a connection to the broker its host made itself, joined as the
    /// certified device `identity` presents: what a host holding a record
    /// without a window does when it reaches its broker its own way (one of
    /// several sessions in one process). A route the broker does not take
    /// leaves the manager offline, as a background connect's does.
    #[must_use]
    pub fn for_record_fd_with_identity(
        fd: RawFd,
        route: RouteSpec,
        kind: Option<String>,
        handle: Option<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Self {
        match Self::connect_route_kind(
            SdkConnectSource::Fd(fd),
            route,
            kind,
            handle,
            Some(SdkJoinIdentity {
                presentation: identity.presentation.clone(),
                participant_kind: "person",
                signer: identity.document_signer(),
            }),
            "person",
        ) {
            Ok(connected) => Self::from_connected(connected),
            Err(error) => {
                let mut manager = Self::offline(error.to_string());
                manager.sync_expected = true;
                manager
            }
        }
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
            handle: presence_handle_env(),
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
    /// [`crate::NativeLaunchError::IdentityRequired`] while the workspace
    /// has no identity selected (someone can choose one, and the launch waits
    /// for it); [`crate::NativeLaunchError::Operation`] where a configured
    /// service is unavailable or the join identity cannot be loaded.
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
        let (app, room) = auto_hello_env();
        if matches!(source, SdkConnectSource::Env) {
            super::channel_settings::install(&app);
        }
        // The window joins as the person this launch is. Loading who that
        // is answers at once, and says whether someone has to choose first,
        // which the launch waits for before it opens anything.
        tilo_startup_metrics::startup_mark("collab-connect");
        let identity = sdk_join_identity(room).map_err(|error| native_launch_refusal(&error))?;
        tilo_startup_metrics::startup_mark("collab-identity-ready");
        Ok(Self::connect_in_background(
            source,
            SdkIdentitySource::Ready(Some(identity)),
            RouteSpec {
                app_id: app,
                cluster_id: room,
                record: None,
            },
            Some(kind.as_str().to_owned()),
            presence_handle_env(),
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
    /// room. Nothing is read from the process's environment, which is the
    /// host's, not the app's. `identity` must outlive the manager.
    ///
    /// # Errors
    ///
    /// The identity service did not answer, or its identity has no certified
    /// device.
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
    ///
    /// # Errors
    ///
    /// The broker refused the route, or the connection ended before it
    /// answered; a manager that was to sync and could not reach its service
    /// says why.
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

    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_history_invocation_fixture_for_test(
        history: Vec<u8>,
    ) -> NativeHistoryInvocationFixture {
        tests::native_history_invocation_fixture(history)
    }

    /// Wire/SDK ordering fixture only; not a native authority proof.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture(app, room, history)
    }

    /// Socket-backed ordering fixture on an actual acknowledged named route.
    /// This fixture does not establish native caller authority.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_named_fixture_for_test(route: RouteSpec, history: Vec<u8>) -> Self {
        let (mut manager, _) =
            tests::native_initial_fixture(&route.app_id, route.cluster_id, history);
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: route.record,
            record_kind: None,
            handle: None,
        }));
        manager
            .await_primary_route()
            .expect("actual named route acknowledgement");
        manager
    }

    /// [`Self::native_initial_named_fixture_for_test`] whose broker admits
    /// the connection's own invocations against the surface it published;
    /// the probe lists each invocation as it arrived and whether it was
    /// offered then. It refuses every committed read of the `refused`
    /// routes, so they never open.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_named_invocation_fixture_for_test(
        route: RouteSpec,
        history: Vec<u8>,
        refused: &[RouteSpec],
    ) -> (
        Self,
        Box<dyn Fn() -> Vec<(tilo_actions::ActionAddress, bool)>>,
    ) {
        let (mut manager, invocations) = tests::native_initial_invocation_fixture(
            &route.app_id,
            route.cluster_id,
            history,
            refused.iter().map(RouteSpec::channel_id).collect(),
        );
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: route.record,
            record_kind: None,
            handle: None,
        }));
        manager
            .await_primary_route()
            .expect("actual named route acknowledgement");
        (manager, invocations)
    }

    /// A connected route whose document reads are not Required, as a
    /// standalone runtime admits it.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn connected_fixture_for_test(app: &str, room: Uuid) -> Self {
        tests::connected_manager_for(app, room)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_fixture_gated_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture_gated(app, room, history, Some(gate))
    }

    /// Socket-backed committed-read fixture; the probe observes real recheck requests.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_parked_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_parked_fixture(app, room, history, gate)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_refresh_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (Self, NativeRefreshProbe) {
        tests::native_refresh_fixture(app, room, history)
    }

    /// A native app's manager joining `route` in the background as
    /// [`Self::join_native`] does, over a fake broker that holds the join
    /// until the probe answers or refuses it, as a loaded broker does. The
    /// route's committed history is `history`, of the record kind `kind`;
    /// the room stands as `room` until the probe says otherwise. Make it in
    /// the app's wake scope, as a launch does.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_joining_fixture_for_test(
        route: RouteSpec,
        kind: &str,
        history: Vec<u8>,
        room: runtime_collab::RoomStatus,
    ) -> (Self, JoiningProbe) {
        let (answer, gate) = tokio::sync::oneshot::channel();
        let (room_status, status) = tokio::sync::watch::channel(Ok(room));
        let broker = tests::FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(tests::Captures {
                committed_history_fixture: Some(history),
                join_gates: std::collections::VecDeque::from([gate]),
                room_status: Some(status),
                ..tests::Captures::default()
            })),
            ..tests::FakeBroker::default()
        };
        let (fd, captures, _thread) = tests::stand_up_fake_broker(broker);
        let manager = Self::connect_in_background(
            SdkConnectSource::Fd(fd),
            SdkIdentitySource::Ready(Some(SdkJoinIdentity {
                presentation: harness_presentation(),
                participant_kind: "person",
                signer: harness_document_signer(),
            })),
            route,
            Some(kind.to_owned()),
            None,
        );
        let probe = JoiningProbe {
            answer: std::sync::Mutex::new(Some(answer)),
            room: room_status,
            sent: Box::new(move || captures.lock().unwrap().signed_ops.len()),
        };
        (manager, probe)
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
            presence_handle_env(),
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
            handle: presence_handle_env(),
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
/// shows a window placed by the session and joins no room of its own.
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
    ///
    /// # Errors
    ///
    /// The socket cannot be reached, the handoff cannot be written, or the
    /// broker does not answer the connection.
    #[cfg(unix)]
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
                // A surface joins no room: it presents no identity, and the
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

/// Report what holds up the pending join of the room `channel` names, as the
/// broker says it ([`super::JoinHold`]): the room is waiting on this device,
/// for each change of its status, or the broker cannot be asked. Ends once
/// the room is ready or refused (the join itself then answers), or the
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
    let mut status = client.room_status(channel).await;
    loop {
        match status {
            Ok(runtime_collab::RoomStatus::Waiting { reason, detail }) => {
                state.set_join_hold(Some(super::JoinHold::RoomWaiting {
                    reason: reason.clone(),
                    detail: detail.clone(),
                }));
                status = client
                    .await_room(
                        channel,
                        &runtime_collab::RoomStatus::Waiting { reason, detail },
                    )
                    .await;
            }
            Ok(runtime_collab::RoomStatus::Ready | runtime_collab::RoomStatus::Refused(_)) => {
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
            match channel.channel_keys_changed(after).await {
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
                    tracing::debug!(%error, "collab: the broker does not say when channel keys change; asking on a timer");
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
        fn drop(&mut self) {
            crate::person_chat::invalidate();
        }
    }
    let _private_person_snapshot_lifetime = PrivatePersonSnapshotLifetime;
    let LocalSdkJoinIdentity {
        presentation,
        participant_kind,
        mut signer,
    } = identity;
    let person = person_room_presentation(&presentation, &mut signer).await;
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

    loop {
        let timed = asks_channel_keys
            && key_fetch.is_none()
            && key_schedule.timed(any_route_asking_on_timer(&routes));
        let queued = tokio::select! {
            queued = outbound_rx.recv() => queued,
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
        let Some(QueuedSdkCommand {
            command,
            _reservation,
        }) = queued
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
        let routes_before = routes.len();
        let document = command.document_len().is_some();
        let mut published = true;
        match command {
            SdkCommand::Hello(PrimaryHello {
                route: spec,
                record_kind,
                handle,
            }) => {
                participant_handle = handle;
                let key = spec.channel_id();
                // The join ends when the broker answers it or it fails, with
                // the broker's reason; a broker that goes away fails it. No
                // clock guesses that an answer is not coming: under load
                // the broker's join, and the room's host behind it, take
                // what they take. While it is pending the broker says what
                // holds it up, which the owner shows.
                let joined = {
                    let context = SdkJoinContext {
                        asks_channel_keys,
                        key_signals: &key_signals,
                        client: &client,
                        presentation: &presentation,
                        person: person.as_ref(),
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                        media_routes: &media_routes,
                    };
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
                crate::person_chat::invalidate();
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
                    published =
                        dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
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
                let joined = ensure_sdk_route(
                    &SdkJoinContext {
                        asks_channel_keys,
                        key_signals: &key_signals,
                        client: &client,
                        presentation: &presentation,
                        person: person.as_ref(),
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                        media_routes: &media_routes,
                    },
                    &mut routes,
                    &spec,
                    record_kind.as_deref().unwrap_or(""),
                )
                .await;
                // A read waiting for this route's first read hears that none
                // is coming.
                if let Err(error) = &joined {
                    inbound_tx
                        .state
                        .document_reads
                        .join_failed(&spec.channel_id(), error.clone());
                }
                note_route_join(&inbound_tx.state, &spec, joined);
            }
            SdkCommand::ToRoute(spec, request) => {
                let key = spec.channel_id();
                let joined = ensure_sdk_route(
                    &SdkJoinContext {
                        asks_channel_keys,
                        key_signals: &key_signals,
                        client: &client,
                        presentation: &presentation,
                        person: person.as_ref(),
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                        media_routes: &media_routes,
                    },
                    &mut routes,
                    &spec,
                    "",
                )
                .await;
                let joined = note_route_join(&inbound_tx.state, &spec, joined);
                if joined && let Some(route) = routes.get_mut(&key) {
                    published =
                        dispatch_sdk_request(route, request, &mut signer, &inbound_tx).await;
                    if !inbound_tx.state.is_healthy() {
                        break;
                    }
                } else {
                    published = false;
                    inbound_tx.state.fail_closed("document route join");
                }
            }
            SdkCommand::Initialize {
                route,
                request,
                reply,
            } => {
                let result = if let Some(current) = routes.get(&route.channel_id()) {
                    match current.channel.committed_document(&request).await {
                        Ok(runtime_collab::committed_document::Reply::Initialized { revision }) => {
                            Ok(revision)
                        }
                        Ok(_) => Err("application initialization reply invalid".into()),
                        Err(error) => Err(error.to_string()),
                    }
                } else {
                    Err("application initialization route unavailable".into())
                };
                let _ = reply.send(result);
            }
            SdkCommand::PrimaryReady(reply) => {
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
                let result = install_sdk_encryption(
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
                    },
                    &mut routes,
                    spec,
                    *group,
                )
                .await;
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
                let result = if let Some(route) = routes
                    .get_mut(&spec.channel_id())
                    .filter(|route| route.spec == spec)
                {
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
            SdkCommand::SubscribePersonChat {
                peer,
                original,
                token,
                mut cancel,
            } => {
                if let Some(key) = primary.as_ref()
                    && let Some(route) = routes.get(key)
                {
                    let channel = route.channel.clone();
                    tokio::task::spawn_local(async move {
                        let follow = async {
                            let stream = channel
                                .subscribe_person_chat(&peer, &original)
                                .await
                                .map_err(|error| error.to_string())?;
                            loop {
                                let (_, bytes) =
                                    stream.read().await.map_err(|error| error.to_string())?;
                                if !crate::person_chat::subscription_current(token, &peer) {
                                    return Ok::<(), String>(());
                                }
                                crate::person_chat::subscription_snapshot(token, &peer, &bytes)?;
                            }
                        };
                        tokio::select! {
                            _ = cancel.changed() => {}
                            result = follow => {
                                if result.is_err() {
                                    crate::person_chat::subscription_failed(token, &peer);
                                }
                            }
                        }
                    });
                } else {
                    crate::person_chat::subscription_failed(token, &peer);
                }
            }
            SdkCommand::PersonChat {
                command,
                original,
                reply,
            } => {
                let result = if let Some(key) = primary.as_ref()
                    && let Some(route) = routes.get_mut(key)
                {
                    tokio::time::timeout(Duration::from_secs(3), async {
                        if let Ok(request) = serde_json::from_slice::<
                            runtime_collab::private_draft::Request,
                        >(&command)
                        {
                            route.channel.private_draft(&request, &original).await
                        } else {
                            route.channel.person_chat(&command, &original).await
                        }
                    })
                    .await
                    .map_err(|_| "private conversation host timed out".to_string())
                    .and_then(|result| result.map_err(|error| error.to_string()))
                } else {
                    Err("private conversation primary channel unavailable".into())
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
    route
        .encryption
        .lock()
        .map_err(|_| "MLS endpoint state is unavailable after a poisoned lock".to_string())?
        .install_group(group);
    Ok(())
}

/// This device's join proof for its person's own room, as the identity
/// service lends it to an app that keeps records of theirs there; `None`
/// for any other app, or a connection without a certified identity.
#[allow(
    clippy::future_not_send,
    reason = "the host identity transport is constructed and retained only on the shovel LocalSet"
)]
async fn person_room_presentation(
    presentation: &[u8],
    signer: &mut LocalSdkDocumentSigner,
) -> Option<(String, Vec<u8>)> {
    #[derive(serde::Deserialize)]
    struct Root {
        user_public_key: [u8; 32],
    }
    // The person's records then cannot be reached; the log says why.
    let unavailable = |reason: &dyn std::fmt::Display| {
        tracing::warn!(%reason, "no join proof for the person's own room");
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
    let room = collab_types::person_room(&root.user_public_key).to_string();
    if room == launch.workspace_id {
        return None;
    }
    let signed = match signer {
        LocalSdkDocumentSigner::Host(identity) => identity
            .sign_collaboration_join(&room)
            .await
            .map_err(|error| error.to_string()),
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            let (identity, asked) = (std::sync::Arc::clone(identity), room.clone());
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
        workspace_id: room.clone(),
        device_certificate: launch.device_certificate,
        proof: proof.to_vec(),
    }
    .encode()
    .ok()?;
    Some((room, bytes))
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
    /// The person's own room and this device's join proof for it, when the
    /// identity service lends one: what a route of a record of theirs
    /// presents instead of the launch room's.
    person: Option<&'a (String, Vec<u8>)>,
    participant_handle: Option<&'a str>,
    participant_kind: &'a str,
    inbound_tx: &'a InboundBridge,
    media_routes: &'a native_media::Routes,
}

struct SdkRoute {
    _checks: Option<super::approved_checks::Registration>,
    _moderation: Option<super::external_moderation::Registration>,
    _attribution: Option<super::write_attribution_lane::Registration>,
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
    let SdkJoinContext {
        asks_channel_keys,
        key_signals,
        client,
        presentation,
        person,
        participant_handle,
        participant_kind,
        inbound_tx,
        media_routes,
    } = *context;
    let channel_id = spec.channel_id();
    if routes.contains_key(&channel_id) {
        return Ok(());
    }
    inbound_tx
        .state
        .connection_snapshots
        .lock_recover()
        .remove(&channel_id);
    let identity_bytes = match person {
        Some((room, proof)) if *room == spec.cluster_id.to_string() => proof.as_slice(),
        _ => presentation,
    };
    // Each round trip of the join is marked, so a launch's startup line says which one
    // its wait for the route went to (the route's join has been most of a cold start).
    tilo_startup_metrics::startup_mark("collab-join-start");
    let channel = match client
        .join_channel_as_kind(
            &channel_id,
            identity_bytes,
            participant_handle.unwrap_or(""),
            broker_participant_kind(participant_kind),
            record_kind,
        )
        .await
    {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(
                "collab: joining channel {channel_id} failed ({error}); route unavailable"
            );
            return Err(format!("{}: {error}", super::ROUTE_JOIN_FAILED));
        }
    };
    tilo_startup_metrics::startup_mark("collab-join-channel");

    // Only the explicit agent constructors consume the eager workspace
    // decision catalogue. Ordinary App owners discard that event and expose
    // their own actions through publication and the bounded pull directory;
    // acquiring every other app's inventory can prevent an unrelated app from
    // starting. This selects a consumer, not authority: broker admission and
    // per-action permissions are unchanged.
    if participant_kind == "agent" {
        // Queue the first decision baseline before document traffic so an
        // agent cannot start deciding against stale local fallback data.
        spawn_action_catalogue_stream(&channel, inbound_tx).await?;
    }
    let committed = committed_document::install(
        &channel,
        spec,
        inbound_tx,
        committed_document::NativeReadJoin {
            client: client.clone(),
            presentation: identity_bytes.to_vec(),
            participant_handle: participant_handle.unwrap_or("").into(),
            participant_kind: participant_kind.into(),
            record_kind: record_kind.into(),
        },
    )
    .await?;
    tilo_startup_metrics::startup_mark("collab-join-committed-read");
    if let Ok(runtime_collab::committed_document::Reply::ConnectionSnapshot(snapshot)) = channel
        .committed_document(&runtime_collab::committed_document::Request::ConnectionSnapshot)
        .await
    {
        if snapshot.app_id == spec.app_id
            && snapshot.room == spec.cluster_id.to_string()
            && snapshot.document == spec.record.as_deref().unwrap_or("")
        {
            inbound_tx
                .state
                .connection_snapshots
                .lock_recover()
                .insert(channel_id.clone(), snapshot);
        }
    }
    tilo_startup_metrics::startup_mark("collab-join-connection-snapshot");
    if committed {
        // This reply contains only the original host-retained launch instance;
        // a record label or app environment never becomes private authority.
        let instance = match channel
            .committed_document(&runtime_collab::committed_document::Request::PrivateWindowBinding)
            .await
        {
            Ok(runtime_collab::committed_document::Reply::PrivateWindowBinding { instance }) => {
                instance
            }
            _ => None,
        };
        inbound_tx
            .state
            .document_reads
            .bind_private_window(&channel_id, instance)?;
    }

    // A room that seals its records takes every write encrypted under the
    // record channel's keys, whoever reads the record for the app. A route
    // whose endpoint has no keys to ask for refuses each write there.
    let sealed = matches!(
        channel
            .committed_document(&runtime_collab::committed_document::Request::Writes)
            .await,
        Ok(runtime_collab::committed_document::Reply::Writes { sealed: true })
    );
    // A committed route's owner admits and applies each edit itself, and a
    // committing host holds no channel keys: it could neither open a sealed
    // edit nor accept cleartext on the route after one. Committed routes
    // therefore stay cleartext end to end.
    let encrypted = asks_channel_keys && (sealed || !committed);
    let encryption = std::sync::Arc::new(std::sync::Mutex::new(if encrypted {
        super::route_keys::RouteKeys::requested()
    } else if sealed {
        tracing::warn!(
            app = %spec.app_id,
            room = %spec.cluster_id,
            "collab: this room seals every write and this connection holds no channel keys; its writes are refused"
        );
        super::route_keys::RouteKeys::unsealable()
    } else {
        super::route_keys::RouteKeys::plain()
    }));
    if encrypted {
        follow_channel_keys(channel.clone(), channel_id.clone(), key_signals.clone());
    }
    tilo_startup_metrics::startup_mark("collab-join-window-binding");
    change_waits::follow(&spec.app_id, &channel, inbound_tx);
    // The streams are independent: subscribing to them together costs the
    // join one broker round trip instead of one per stream.
    let ((), (), (), (), (), (), presence) = tokio::join!(
        async {
            if !committed {
                spawn_op_stream(
                    &channel,
                    RouteLabel::of(spec),
                    inbound_tx,
                    std::sync::Arc::clone(&encryption),
                    encrypted.then(|| (channel_id.clone(), key_signals.clone())),
                )
                .await;
            }
        },
        spawn_context_stream(&channel, spec.clone(), inbound_tx),
        spawn_invocation_stream(&channel, spec, inbound_tx),
        spawn_realtime_stream(&channel, inbound_tx),
        async {
            // This intentionally fails quietly for person sessions. An
            // admitted agent gets a separate broker-addressed queue; a
            // document agent never has to infer task work from room traffic.
            if !crate::host::environment().identity.managed_agent_context {
                spawn_task_run_stream(&channel, inbound_tx).await;
            }
        },
        spawn_content_stream(&channel, spec.clone(), inbound_tx),
        spawn_presence_stream(&channel, inbound_tx),
    );
    tilo_startup_metrics::startup_mark("collab-join-streams");
    // Subscriptions are installed first so an existing holder's response
    // cannot race ahead of our document stream.
    if !committed && let Err(error) = channel.request_document_snapshot().await {
        tracing::warn!(%error, "collab: late-join snapshot request failed");
    }

    let checks = super::approved_checks::register(channel_id.clone(), channel.clone())
        .map_err(|error| {
            tracing::warn!(%error, "collab: approved project check lane unavailable");
            error
        })
        .ok();
    let moderation = super::external_moderation::register(channel_id.clone(), channel.clone())
        .map_err(|error| {
            tracing::warn!(%error, "collab: external moderation lane unavailable");
            error
        })
        .ok();
    let attribution = super::write_attribution_lane::register(channel_id.clone(), channel.clone())
        .map_err(|error| {
            tracing::warn!(%error, "collab: write attribution lane unavailable");
            error
        })
        .ok();
    media_routes.insert(channel_id.clone(), channel.clone());
    if spec.record.is_none()
        && participant_kind == "person"
        && crate::host::environment()
            .identity
            .collab_service_configured
    {
        audio_output::install(channel.clone());
    }
    routes.insert(
        channel_id,
        SdkRoute {
            _checks: checks,
            _moderation: moderation,
            _attribution: attribution,
            spec: spec.clone(),
            channel,
            presence,
            workspace_controls: std::collections::HashMap::new(),
            committed,
            encryption,
        },
    );
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
                let translated = match postcard::from_bytes::<SignedOp>(&event.op) {
                    Ok(envelope) => {
                        let (author, seq) = (envelope.author, envelope.seq);
                        let arrived = Arrived {
                            envelope,
                            from_user_id: event.from_user_id,
                            admission: event.admission,
                        };
                        match signed_document_event(arrived, &encryption) {
                            Ok(event) => event,
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
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            "collab: signed op is not postcard SignedOp ({error}); dropping"
                        );
                        continue;
                    }
                };
                let Some(complete) = complete_document_event(translated, &tx.reassembly.live)
                else {
                    continue;
                };
                if !tx.send_document(complete).await {
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
                fn drop(&mut self) {
                    crate::person_chat::invalidate();
                }
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
                    crate::person_chat::invalidate();
                    private_person_basis = Some(basis);
                }
                if !tx
                    .send_durable(CollabEvent::Context {
                        collaborators: context.collaborators,
                        permissions: context.permissions,
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
        if crate::time::now() >= deadline {
            return Err("original invocation capture did not become current".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
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
                        None,
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
    tokio::task::spawn_local(async move {
        forward_action_catalogues(async || stream.read().await.map_err(|_| ()), &tx, turns).await;
    });
    Ok(())
}

/// Decode and admit the initial snapshot through the same bounded owner queue.
fn admit_initial_action_catalogue(
    initial: &str,
    tx: &InboundBridge,
    turn: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<(), String> {
    let catalogue = serde_json::from_str::<BrokerActionCatalogueWire>(initial)
        .map_err(|error| format!("decoding initial action catalogue failed: {error}"))?;
    if !tx.send_reply_turn(
        CollabEvent::ActionCatalogue {
            surfaces: catalogue.surfaces,
        },
        turn.map(EventTurn::Snapshot),
        Some(initial.len()),
    ) {
        return Err("delivering initial action catalogue failed".to_string());
    }
    Ok(())
}

/// Forward catalogue snapshots from `read` until it or the bridge fails.
/// Each read returns the broker's latest full catalogue (a watch), so at
/// most one catalogue is queued at a time: the next read waits until the
/// owner has dequeued the previous one, and intermediate snapshots coalesce
/// in the broker. A busy owner (a headless worker between pumps) otherwise
/// accumulated one large catalogue per publication until the bounded bridge
/// failed closed.
async fn forward_action_catalogues(
    mut read: impl AsyncFnMut() -> Result<String, ()>,
    tx: &InboundBridge,
    turns: Arc<tokio::sync::Semaphore>,
) {
    loop {
        let Ok(turn) = turns.clone().acquire_owned().await else {
            break;
        };
        let Ok(snapshot) = read().await else {
            break;
        };
        match serde_json::from_str::<BrokerActionCatalogueWire>(&snapshot) {
            Ok(catalogue) => {
                if !tx
                    .send_durable_turn(
                        CollabEvent::ActionCatalogue {
                            surfaces: catalogue.surfaces,
                        },
                        Some(EventTurn::Snapshot(turn)),
                        Some(snapshot.len()),
                    )
                    .await
                {
                    break;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "collab: invalid broker action catalogue");
            }
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

/// Keep the accepted request and its queue byte reservation on the worker
/// stack while hydration catches up. No later barrier or command can overtake
/// it. Hydration runs independently on this same LocalSet.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
async fn wait_for_document_publication(
    route: &SdkRoute,
    state: &BridgeState,
) -> Result<(), DocumentPublicationError> {
    let deadline = tilo_rt::timer_now() + Duration::from_secs(3);
    let key = route.spec.channel_id();
    loop {
        if !state.is_healthy() {
            return Err(state.publication_failed(
                "queued document publication",
                "worker invalidated before admission",
                true,
            ));
        }
        // The deadline is terminal, even when readiness changes concurrently.
        if tilo_rt::timer_now() >= deadline {
            return Err(state.publication_failed(
                "queued document publication",
                "hydration did not become current before deadline",
                true,
            ));
        }
        if state.document_reads.publishable(&key) {
            return Ok(());
        }
        #[cfg(test)]
        state.publication_gate_waits.fetch_add(1, Ordering::AcqRel);
        tokio::time::sleep(Duration::from_millis(10)).await;
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
    inbound: &InboundBridge,
) -> Result<(), DocumentPublicationError> {
    wait_for_document_publication(route, &inbound.state).await?;
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
    let count = frames.len();
    for (index, frame) in frames.into_iter().enumerate() {
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
    Ok(())
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
            state.change_sent();
            Ok(())
        }
        FrameSend::Failed(error) => Err(error),
        FrameSend::Refused(refusal) => {
            if !refresh_route_keys(route, signer).await || !refusal.is_definite() {
                return Err(frame_refused(state, &refusal));
            }
            match try_send_frame(route, signer, cleartext, claims, state).await {
                FrameSend::Sent | FrameSend::Held => {
                    state.change_sent();
                    Ok(())
                }
                FrameSend::Failed(error) => Err(error),
                FrameSend::Refused(refusal) => Err(frame_refused(state, &refusal)),
            }
        }
    }
}

/// What a commit the broker would not take means. A definite refusal — the
/// peer answered about this very change — fails that change alone; anything
/// else (a lost call, an overload) leaves the outcome unknown and ends the
/// bridge as it always has.
fn frame_refused(state: &BridgeState, refusal: &FrameRefusal) -> DocumentPublicationError {
    match refusal {
        FrameRefusal::Definite(reason) => {
            state.note_refused_change("document commit RPC", reason);
            DocumentPublicationError::Refused(format!("document commit RPC: {reason}"))
        }
        FrameRefusal::Unknown(reason) => {
            state.publication_failed("document commit RPC", reason, true)
        }
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
    let author = match signer {
        LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Certified(identity)) => {
            identity.signing_public_key()
        }
        LocalSdkDocumentSigner::Inherited(
            SdkDocumentSigner::Harness(key, _) | SdkDocumentSigner::Delegated(key, _),
        ) => key.verifying_key().to_bytes(),
        LocalSdkDocumentSigner::Host(identity) => identity.metadata().signing_public_key(),
    };
    let author = collab_types::Ed25519PublicKey::from_bytes(author);
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
) -> bool {
    if matches!(
        request,
        CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
    ) && wait_for_document_publication(route, &inbound_tx.state)
        .await
        .is_err()
    {
        return false;
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
                Ok(()) => inbound_tx.state.change_sent(),
                // A definite refusal is the peer's answer about this signed
                // change: it alone is not accepted and the bridge stays open.
                Err(error) if refused_by_peer(&error) => inbound_tx
                    .state
                    .note_refused_change("signed document commit RPC", &error),
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
        } => {
            // The helper records every failure, including a policy denial that
            // leaves the identity connection usable, before returning its result.
            let outcome =
                send_document_mutation(route, signer, mutation, authorization, inbound_tx).await;
            return outcome.is_ok();
        }
        CollabRequest::OfferContent { hash, bytes } => {
            if let Err(error) = route.channel.offer_blob(&hash, &bytes).await {
                tracing::debug!(%error, %hash, "collab: offering record content failed");
            }
        }
        CollabRequest::WantContent { hash } => {
            if let Err(error) = route.channel.want_blob(&hash).await {
                tracing::debug!(%error, %hash, "collab: asking for record content failed");
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
                Err(_) => (Vec::new(), Vec::new(), None, true),
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
        CollabRequest::PublishActions { actions } => {
            if let Err(error) = route.channel.publish_actions(&actions).await {
                tracing::warn!(%error, "collab: publishing resource/action surface failed; republishing");
                inbound_tx
                    .state
                    .action_surface_failed
                    .store(true, Ordering::Release);
                inbound_tx.state.notify_events();
            }
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
        CollabRequest::SelectedDiscovery {
            request_id,
            command,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .local_file_selected_discovery(&command, on_behalf_of.map(|id| *id.0.as_bytes()))
                .await
                .map_err(|error| error.to_string());
            let _ =
                inbound_tx.send_reply(CollabEvent::SelectedDiscoveryResult { request_id, result });
        }
        CollabRequest::OpenSelectedV2 {
            request_id,
            command,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .local_file_open_selected_v2(&command, Some(*on_behalf_of.0.as_bytes()))
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx.send_reply(CollabEvent::OpenSelectedV2Result { request_id, result });
        }
        CollabRequest::OpenSelected {
            request_id,
            command,
            on_behalf_of,
        } => {
            let result = route
                .channel
                .local_file_open_selected(&command, Some(*on_behalf_of.0.as_bytes()))
                .await
                .map_err(|error| error.to_string());
            let _ = inbound_tx.send_reply(CollabEvent::OpenSelectedResult { request_id, result });
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
mod tests {
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
        reason = "the tests and the fixtures they share with the toolkit's tests are test code"
    )]
    use super::*;
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

    /// Wait until the shovel has seen a queued document mutation find its
    /// route not publishable twice: the mutation is held at the gate.
    #[cfg(test)]
    fn wait_held_at_publication_gate(state: &BridgeState) {
        let before = state.publication_gate_waits.load(Ordering::Acquire);
        tilo_test_env::wait_until(
            "the queued mutation to wait at the publication gate",
            || state.publication_gate_waits.load(Ordering::Acquire) >= before + 2,
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
        /// its room's hosts have none of.
        pub committed_absent_channels: Vec<String>,
        /// Hold committed-read `Await`s until this is notified, as the broker
        /// does until the read changes. Without it every `Await` fails, as a
        /// call to a broker can, and the reader re-Opens at its own pace.
        pub committed_awaits: Option<Arc<tokio::sync::Notify>>,
        /// End the next held `Await` though the read did not change, as a
        /// broker does when an announced change leaves the reader's Open to
        /// decide.
        pub committed_await_stale: bool,
        /// Answer Opens `Busy`, as a broker whose byte budget other reads
        /// hold: the reader's place in its queue. While set, a held `Await`
        /// waits for the turn (cleared, then [`Captures::committed_awaits`]
        /// notified) as the broker's does.
        pub committed_busy: bool,
        /// An Open was answered `Busy`: once the turn comes the held
        /// `Await` answers `Stale`, as the broker's does.
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
        /// The room's status as the broker reports it, or why the broker
        /// cannot be asked; without it the host refuses to say.
        pub room_status:
            Option<tokio::sync::watch::Receiver<Result<runtime_collab::RoomStatus, String>>>,
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
            while let Ok(Some(call)) = wire::FRAMING.read_value_async::<_, Call>(&mut reader).await
            {
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
                                vec![serde_json::json!({"version": 1, "surfaces": []}).to_string()]
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
                Operation::Room { seen, .. } => {
                    let Some(mut status) = self.captures().room_status.clone() else {
                        return answered(Err("the fixture reports no room status".into()));
                    };
                    Box::pin(async move {
                        loop {
                            let now = status.borrow_and_update().clone();
                            match (now, &seen) {
                                (Err(reason), _) => return Err(reason),
                                (Ok(now), None) => return Ok(Reply::Room(now)),
                                (Ok(now), Some(seen)) if now != *seen => {
                                    return Ok(Reply::Room(now));
                                }
                                (Ok(_), Some(_)) => {}
                            }
                            status
                                .changed()
                                .await
                                .map_err(|_| "room status ended".to_owned())?;
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
                        presence_backlog: Rc::new(RefCell::new(
                            host.broker.presence_backlog.clone(),
                        )),
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
                Publication::Actions(actions) => {
                    let actions: Vec<runtime_collab::ExportedAction> =
                        match serde_json::from_str(&actions) {
                            Ok(actions) => actions,
                            Err(error) => return answered(Err(error.to_string())),
                        };
                    let actions = actions
                        .into_iter()
                        .map(|action| (action.resource_label, action.action))
                        .collect();
                    let mut captures = self.captures();
                    if captures.refuse_action_publications > 0 {
                        captures.refuse_action_publications -= 1;
                        captures.refused_action_publications += 1;
                        return answered(Err("current host worker authority unavailable".into()));
                    }
                    captures
                        .action_publications
                        .push((channel.app_id(), actions));
                    answered(Ok(Reply::Done))
                }
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

        #[expect(
            clippy::too_many_lines,
            reason = "the committed read's every answer, as the broker gives it"
        )]
        fn committed_document(&self, channel: &FakeChannel, body: &[u8]) -> FakeAnswer {
            use runtime_collab::committed_document::{Reply as Read, Request as Ask};
            let encode = |reply: Read| reply.encode().map(Reply::Answer).map_err(Into::into);
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
                            let stale = std::mem::take(&mut captures.committed_await_stale);
                            if !stale && captures.committed_busy {
                                Some(known_revision)
                            } else if std::mem::take(&mut captures.committed_queued) {
                                None
                            } else {
                                (!stale
                                    && captures.committed_open_failure.is_none()
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
                    encode(Read::Stale)
                });
            }
            let result = (|| {
                let request = decoded?;
                let mut captures = self.captures();
                captures.committed_reads.push(request.clone());
                let changes = captures.committed_changes.clone();
                let opens = matches!(request, Ask::Open { .. } | Ask::OpenChanges { .. });
                if opens {
                    if let Some(error) = captures.committed_open_failure.clone() {
                        return Err(error);
                    }
                    if captures
                        .committed_open_refused_channels
                        .contains(&channel.channel_id)
                    {
                        return Err("committed read custody refused for this route".into());
                    }
                }
                let busy = opens && captures.committed_busy;
                let current_revision = captures.committed_revision.unwrap_or([7; 32]);
                let absent = captures
                    .committed_absent_channels
                    .contains(&channel.channel_id)
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
    }

    /// A client connected to a fake host serving `broker` on this
    /// `LocalSet`, and its captures.
    pub(super) fn fake_client(
        broker: FakeBroker,
    ) -> (runtime_collab::CollabClient, SharedCaptures) {
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
    pub fn stand_up_fake_broker(
        broker: FakeBroker,
    ) -> (RawFd, SharedCaptures, thread::JoinHandle<()>) {
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
        let room = Uuid::new_v4();
        let manager = CollaborationManager::for_route_from(
            SdkConnectSource::Fd(fd),
            "so.tilo.notes".into(),
            room,
            Some("tab:0000000000000003".into()),
            presence_handle_env(),
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
            mutation: collab_types::DocumentMutation::Snapshot(
                b"committed worker mutation".to_vec(),
            ),

            authorization: None,
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
        let room = Uuid::new_v4();
        let primary = RouteSpec::record("so.tilo.notes", room, "tab:0000000000000001");
        let beside = RouteSpec::record(
            "so.tilo.chat",
            room,
            collab_types::workspace_record_id("tilo.workspace.emoji"),
        );
        let (mut manager, _) =
            CollaborationManager::native_initial_named_invocation_fixture_for_test(
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

    fn parking_transport(
        capacity: usize,
    ) -> (BrokerTransport, tokio_mpsc::Receiver<QueuedSdkCommand>) {
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
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
            // Admitted to the channel, so the edit reaches the signer.
            Ok(ready_channel_keys())
        }
        fn device_certificate(&self) -> Option<&[u8]> {
            Some(b"fixture")
        }
        fn signing_public_key(&self) -> [u8; 32] {
            [5; 32]
        }
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
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
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Ok(ready_channel_keys())
        }
        fn device_certificate(&self) -> Option<&[u8]> {
            Some(b"fixture")
        }
        fn signing_public_key(&self) -> [u8; 32] {
            [5; 32]
        }
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
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
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
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
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
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
    /// (its room's governance unknown yet), is told when that changes: it is not asked
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
        ) -> Result<runtime_identity::wire::ChannelKeysReply, runtime_portal::NotAvailable>
        {
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
        fn sign_collaboration_join(
            &self,
            _: &str,
        ) -> Result<[u8; 64], runtime_portal::NotAvailable> {
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
        assert!(
            manager
                .try_send_to_route(
                    route,
                    CollabRequest::document_incremental(b"refused".to_vec())
                )
                .is_ok()
        );
        let pending = manager.begin_document_publication_barrier().unwrap();
        wait_held_at_publication_gate(&broker_state(&manager));
        assert_eq!(
            pending.try_complete(),
            Ok(None),
            "certified publication must still await exact route readiness"
        );
        assert!(captures.lock().unwrap().signed_ops.is_empty());
        ready.store(true, Ordering::Release);
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
        release.send(Ok(())).unwrap();
        tilo_test_env::wait_until("`captures.lock().unwrap().signed_ops.len() == 1`", || {
            captures.lock().unwrap().signed_ops.len() == 1
        });
        wait_held_at_publication_gate(&state);
        assert_eq!(
            receipt.try_complete(),
            Ok(None),
            "accepted publication must hold the barrier while hydration is pending"
        );
        assert!(
            state.document_command_bytes.0.load(Ordering::Acquire) >= b"retained original".len()
        );
        ready.store(true, Ordering::Release);
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
        let error =
            runtime_portal::NotAvailable::Refused(format!("refused\n\0{}", "🦀".repeat(1024)));
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

    #[tokio::test]
    async fn native_invocation_delivery_turn_waits_for_every_live_owner() {
        let state = BridgeState::new();
        let primary = state.document_reads.close_for_test("original-route");
        let owner = state.document_reads.close_for_test("live-owner");
        state.document_reads.close_for_test("unowned-retained");
        primary.store(true, Ordering::Release);
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
        owner.store(true, Ordering::Release);
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
        let manager =
            CollaborationManager::from_sdk_fd_for_test(fd, "capture-test", Uuid::new_v4());
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
    pub(super) fn connected_manager_for(app: &str, room: Uuid) -> CollaborationManager {
        let broker = FakeBroker::default();
        let (fd, _, _thread) = stand_up_fake_broker(broker);
        CollaborationManager::from_sdk_fd_for_test(fd, app, room)
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
                super::super::channel_live_activity::show(Some(
                    serde_json::json!({"label": "Tea"}),
                ));
                let _manager = CollaborationManager::from_sdk_fd_for_test(fd, APP, Uuid::new_v4());
                eventually("the activity reaches the shell", || {
                    board.activity(APP) == Some(serde_json::json!({"label": "Tea"}))
                });

                super::super::channel_live_activity::show(Some(
                    serde_json::json!({"label": "Eggs"}),
                ));
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

                        authorization: None,
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
                "the result waits for room"
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
                "the shovel taking a change frees room, and the idle owner must hear of it \
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
            let mut manager =
                CollaborationManager::from_sdk_fd_for_test(fd, "shell", Uuid::new_v4());
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
            .unwrap();
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
            let alice_endpoint = group_route_keys(alice);
            let PreparedFrame::Ready(ciphertext, metadata) =
                prepare_document_payload(&alice_endpoint, artifact, plaintext.clone(), &[])
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
                let event = signed_document_event(
                    Arrived {
                        envelope: op.clone(),
                        from_user_id: "alice".into(),
                        admission: OpAdmission::EncryptionRequired,
                    },
                    &endpoint,
                )
                .unwrap();
                assert!(matches!(
                    event.document_mutation(),
                    Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
                ));
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
            .unwrap();
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
    #[test]
    fn initial_action_catalogue_admission_accounts_for_retained_payload() {
        let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());
        let snapshot = |bytes: usize| {
            format!(
                r#"{{"surfaces":[{{"peer_id":1,"app_id":"so.tilo.notes","cluster_id":"c","record_id":"{}","live":true,"actions":[]}}]}}"#,
                "r".repeat(bytes)
            )
        };
        let initial = snapshot(700 * 1024);
        admit_initial_action_catalogue(&initial, &inbound, None).unwrap();
        let queued = rx.try_recv().expect("initial catalogue reaches the owner");
        assert_eq!(event_kind(&queued.event), "ActionCatalogue");
        let retained = event_bytes(&queued.event).unwrap();
        assert_eq!(
            state.catalogue_event_bytes.0.load(Ordering::Acquire),
            retained
        );
        assert_eq!(
            state.event_bytes.0.load(Ordering::Acquire),
            0,
            "a catalogue is charged to its own budget, not the control events'"
        );
        assert!(retained >= 700 * 1024);
        drop(queued);
        assert_eq!(state.catalogue_event_bytes.0.load(Ordering::Acquire), 0);
        assert!(state.is_healthy());

        let oversized = snapshot(SDK_CATALOGUE_EVENT_BYTES);
        admit_initial_action_catalogue(&oversized, &inbound, None).unwrap();
        assert!(
            state.is_healthy(),
            "an oversized snapshot is trimmed to fit rather than failing the bridge"
        );
        let queued = rx
            .try_recv()
            .expect("the trimmed catalogue reaches the owner");
        let CollabEvent::ActionCatalogue { surfaces } = &queued.event else {
            panic!("expected an action catalogue");
        };
        assert!(surfaces.is_empty(), "the one surface too large is dropped");
        drop(queued);
        assert_eq!(state.catalogue_event_bytes.0.load(Ordering::Acquire), 0);
        assert_eq!(state.event_bytes.0.load(Ordering::Acquire), 0);
    }

    /// A headless worker is drained only between pumps. Large catalogue
    /// snapshots (one per host publication) must not pile up in its bounded
    /// bridge: one stays queued, the rest coalesce to the latest.
    #[test]
    fn action_catalogue_reader_keeps_one_snapshot_queued_for_a_busy_owner() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge::new(tx, state.clone());
        // ~700 KiB per snapshot, as with 34 mounted workers: six exceed the budget.
        let snapshot = |sequence: u64| {
            format!(
                r#"{{"surfaces":[{{"peer_id":{sequence},"app_id":"so.tilo.notes","cluster_id":"c","record_id":"{}","live":true,"actions":[]}}]}}"#,
                "r".repeat(700 * 1024)
            )
        };
        let published = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let reads = published.clone();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async move {
            let forwarder = inbound.clone();
            tokio::task::spawn_local(async move {
                forward_action_catalogues(
                    async || {
                        let next = reads.get() + 1;
                        reads.set(next);
                        Ok(snapshot(next))
                    },
                    &forwarder,
                    Arc::new(tokio::sync::Semaphore::new(1)),
                )
                .await;
            });
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            assert!(state.is_healthy(), "a busy owner does not fail the bridge");
            let first = rx.try_recv().expect("one catalogue queued");
            assert!(
                rx.try_recv().is_err(),
                "only one catalogue is queued at a time"
            );
            drop(first);
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            assert!(matches!(
                rx.try_recv(),
                Ok(QueuedCollabEvent {
                    event: CollabEvent::ActionCatalogue { ref surfaces },
                    ..
                }) if surfaces[0].peer_id == 2
            ));
            assert!(state.is_healthy());
        });
        assert_eq!(published.get(), 2, "no read while a catalogue is queued");
    }
    pub fn notebook_action_catalogue_fixture(cluster: Uuid, count: usize) -> String {
        let actions: Vec<_> = (0..count).map(|index| serde_json::json!({
            "resource": format!("notes.page:{index:032x}"),
            "action": "open", "resource_kind": "note",
            "resource_label": format!("Notebook page {index}"),
            "action_label": "Open page",
            // Keep realistic full action metadata in this transport fixture: the
            // initial/watch and oversized tests assert the retained-byte bounds.
            "description": "Open this page in its notebook. Keep its notebook context, identify the original page, and show the existing page through the ordinary local reader. This catalogue description represents a full app-provided action explanation, including its expected navigation result.",
            "operation_kind": tilo_actions::OperationKind::View,
            "params": [tilo_actions::ActionParam::required("page", tilo_actions::ParamType::Resource, "The page to open")],
            "contract": tilo_actions::ActionContract::view(),
            "input": ExportedInput::None
        })).collect();
        serde_json::json!({"version": 1, "surfaces": [{
            "peer_id": 1, "app_id": "so.tilo.notes", "cluster_id": cluster.to_string(),
            "record_id": "notebook", "live": true, "actions": actions
        }]})
        .to_string()
    }

    /// Production SDK socket, join and initial/watch delivery; this fixture
    /// does not certify broker authorization.
    #[test]
    #[cfg(unix)]
    fn initial_catalogue_and_watch_share_one_turn_for_a_busy_native_owner() {
        let cluster = Uuid::new_v4();
        let snapshot = notebook_action_catalogue_fixture(cluster, 1025);
        let decoded: BrokerActionCatalogueWire = serde_json::from_str(&snapshot).unwrap();
        let retained = event_bytes(&CollabEvent::ActionCatalogue {
            surfaces: decoded.surfaces,
        })
        .expect("the declared catalogue has a finite retained size");
        eprintln!(
            "catalogue fixture serialized_bytes={} retained_bytes={retained}",
            snapshot.len()
        );
        assert!(
            retained < SDK_EVENT_QUEUE_BYTES,
            "the complete baseline fits"
        );
        assert!(
            retained * 2 > SDK_EVENT_QUEUE_BYTES,
            "initial plus watch would overflow"
        );
        let mut updated: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        updated["surfaces"][0]["peer_id"] = serde_json::json!(2);
        let captures = Captures {
            action_catalogue_fixture: Some(vec![snapshot, updated.to_string()]),
            ..Captures::default()
        };
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(captures)),
            ..FakeBroker::default()
        };
        let (fd, _captures, worker) = stand_up_fake_broker(broker);
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: harness_presentation(),
                participant_kind: "agent",
                signer: harness_document_signer(),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::with_transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "so.tilo.notes".into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        }));
        let admission = manager.await_primary_route();
        let state = broker_state(&manager);
        let healthy = state.is_healthy();
        let initial = manager.poll().into_iter().find_map(|event| match event {
            CollabEvent::ActionCatalogue { surfaces } => Some(surfaces),
            _ => None,
        });
        // The first normal owner poll releases the initial turn. The broker
        // watch can then deliver its complete next baseline without overflow.
        let mut watched = None;
        if healthy {
            eventually(
                "the watched catalogue reaches the ordinary owner poll",
                || {
                    watched = manager.poll().into_iter().find_map(|event| match event {
                        CollabEvent::ActionCatalogue { surfaces } => Some(surfaces),
                        _ => None,
                    });
                    watched.is_some()
                },
            );
        }
        drop(manager);
        worker.join().unwrap();
        assert!(
            admission.is_ok(),
            "startup must admit the route: {admission:?}"
        );
        assert!(
            healthy,
            "initial and watched baselines must not disconnect the application"
        );
        assert_eq!(
            initial.unwrap()[0].actions.len(),
            1025,
            "complete first baseline"
        );
        let watched = watched.unwrap();
        assert_eq!(watched[0].actions.len(), 1025, "complete watched baseline");
        assert_eq!(
            watched[0].peer_id, 2,
            "the watch delivered its next snapshot"
        );
        assert!(state.is_healthy());
    }

    /// An agent in a large workspace (many open windows, many apps) receives
    /// a catalogue larger than the control events' budget whole and stays
    /// connected: charged to that budget, it once failed the agent's whole
    /// connection before its first task run.
    #[test]
    #[cfg(unix)]
    fn an_agent_receives_a_catalogue_larger_than_the_control_budget() {
        let cluster = Uuid::new_v4();
        let snapshot = notebook_action_catalogue_fixture(cluster, 2049);
        let decoded: BrokerActionCatalogueWire = serde_json::from_str(&snapshot).unwrap();
        let retained = event_bytes(&CollabEvent::ActionCatalogue {
            surfaces: decoded.surfaces,
        })
        .unwrap();
        assert!(
            retained > SDK_EVENT_QUEUE_BYTES,
            "larger than the control budget"
        );
        assert!(
            retained < SDK_CATALOGUE_EVENT_BYTES,
            "within its own budget"
        );
        let broker = FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(Captures {
                action_catalogue_fixture: Some(vec![snapshot]),
                ..Captures::default()
            })),
            ..FakeBroker::default()
        };
        let (fd, _captures, worker) = stand_up_fake_broker(broker);
        let sdk = CollaborationManager::spawn_sdk(
            SdkConnectSource::Fd(fd),
            Some(SdkJoinIdentity {
                presentation: harness_presentation(),
                participant_kind: "agent",
                signer: harness_document_signer(),
            }),
        )
        .unwrap();
        let mut manager = CollaborationManager::with_transport(Box::new(sdk));
        assert!(manager.send(CollabRequest::Hello {
            app_id: "so.tilo.agent".into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        }));
        let admission = manager.await_primary_route();
        let state = broker_state(&manager);
        let catalogue = manager.poll().into_iter().find_map(|event| match event {
            CollabEvent::ActionCatalogue { surfaces } => Some(surfaces),
            _ => None,
        });
        let healthy = state.is_healthy();
        drop(manager);
        worker.join().unwrap();
        assert!(admission.is_ok(), "the agent joins: {admission:?}");
        assert!(healthy, "the catalogue must not disconnect the agent");
        assert_eq!(catalogue.expect("the catalogue")[0].actions.len(), 2049);
    }

    /// A catalogue too large even for its own budget is trimmed rather than
    /// failing the bridge: apps only offered to open go first, the last
    /// listed first, and what is kept fits.
    #[test]
    fn an_oversized_catalogue_keeps_the_open_windows_first() {
        let surface = |app: &str, live: bool, actions: usize| {
            let wire: BrokerActionCatalogueWire = serde_json::from_str(
                &notebook_action_catalogue_fixture(Uuid::from_u128(1), actions),
            )
            .unwrap();
            let mut surface = wire.surfaces.into_iter().next().unwrap();
            surface.app_id = app.into();
            surface.live = live;
            surface
        };
        let surfaces = vec![
            surface("so.tilo.notes", true, 40),
            surface("host:one", false, 1),
            surface("so.tilo.settings", true, 40),
            surface("host:two", false, 1),
        ];
        let size = |surfaces: &[BrokerActionSurface]| {
            event_bytes(&CollabEvent::ActionCatalogue {
                surfaces: surfaces.to_vec(),
            })
            .unwrap()
        };
        let whole = size(&surfaces);
        let (kept, dropped) = fit_catalogue(surfaces.clone(), whole);
        assert_eq!(
            (kept.len(), dropped.len()),
            (4, 0),
            "a fitting catalogue is whole"
        );

        let limit = size(&surfaces[..3]) - 1;
        let (kept, dropped) = fit_catalogue(surfaces, limit);
        assert_eq!(dropped, ["host:two", "host:one"], "inert surfaces go first");
        assert_eq!(
            kept.iter()
                .map(|surface| surface.app_id.as_str())
                .collect::<Vec<_>>(),
            ["so.tilo.notes", "so.tilo.settings"]
        );
        assert!(size(&kept) <= limit);
    }
}

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

    /// A manager on the broker at `fd`, joined to `app`'s route in `room`.
    #[must_use]
    pub fn manager_on(fd: RawFd, app: &str, room: Uuid) -> CollaborationManager {
        CollaborationManager::from_sdk_fd_for_test(fd, app, room)
    }

    /// A manager on the broker at `fd`, joined as a native window to the
    /// route of `record` of `app` in `room`.
    ///
    /// # Panics
    /// When the route cannot be joined.
    #[must_use]
    pub fn manager_on_record_route(
        fd: RawFd,
        app: String,
        room: Uuid,
        record: Option<String>,
    ) -> CollaborationManager {
        CollaborationManager::for_route_from(
            SdkConnectSource::Fd(fd),
            app,
            room,
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
    /// holds an action catalogue of `count` notebooks, larger than the
    /// worker's event budget: whether the route was admitted and the worker
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
        let snapshot = notebook_action_catalogue_fixture(cluster, count);
        let decoded: crate::BrokerActionCatalogueWire = serde_json::from_str(&snapshot).unwrap();
        let retained = super::event_bytes(&crate::CollabEvent::ActionCatalogue {
            surfaces: decoded.surfaces,
        })
        .unwrap();
        eprintln!(
            "startup fixture serialized_bytes={} retained_bytes={retained}",
            snapshot.len()
        );
        assert!(
            retained > super::SDK_EVENT_QUEUE_BYTES,
            "prove the existing bound matters"
        );
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
