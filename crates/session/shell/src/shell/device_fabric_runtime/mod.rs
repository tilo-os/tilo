//! Native-session bridge between compositor state and the personal fabric.
//!
//! The bridge deliberately samples stable state rather than putting network or
//! hardware discovery on the render path. Remote clipboard entries become
//! history, not the live Wayland selection, until explicitly activated.

mod display_auto_leases;
mod display_refresh;
#[cfg(test)]
mod display_refresh_tests;
pub(crate) mod person_layout;
mod receive_recovery;
mod remote_capture;
mod spatial_cleanup;
mod spatial_protocol;
#[cfg(test)]
mod spatial_revocation_tests;
mod spatial_withdrawal;
mod window_views;

pub use window_views::{WindowViewer, publish_window_views, window_viewers};

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use getrandom::{
    SysRng,
    rand_core::{TryRng as _, UnwrapErr},
};
use rand_core::Rng;
use serde_json::{Map, Value};
use tilo_clock::unix_now_ms;

use crate::network::actor::{PersonalNearbyTransferFrame, PersonalTransferStatus};
use crate::notifications::{
    ContinuityNotificationCommand, ContinuityOutboundEvent, NotificationBridge,
};

use crate::compositor::selection::{
    SharedSelection, clear_shared_clipboard_history, merge_shared_clipboard_history,
    shareable_clipboard_history,
};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric::{
    ClipboardEntry, ClipboardPayload, DeviceFabric, DeviceFabricSnapshot, DeviceKind,
    DevicePresence, DisplayEdge, DisplayPlacement, DisplayRect, FABRIC_SCHEMA_VERSION,
    FabricCapabilities, FabricInputEvent, FabricStamp, InputLease, MAX_INPUT_LEASE_MS,
    MediaHandoffFailure, MediaSession, MediaSourceLocator, PersonalFabricAction, RoamingSetting,
};
use crate::shell::network::{ShellNetwork, fabric_monotonic_us};

type ProductionGamepadFactory =
    os_actions::LinuxVirtualGamepadFactory<os_actions::UinputGamepadDeviceProvider>;
type ProductionGamepadBroker = os_actions::ForwardedPeripheralBroker<ProductionGamepadFactory>;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const PRESENCE_HEARTBEAT: Duration = Duration::from_secs(10);
const MAX_QUEUED_SPATIAL_INPUT: usize = 256;
const MAX_SPATIAL_INPUT_FLUSH_PER_FRAME: usize = 32;
const MAX_PENDING_INCOMING_MEDIA: usize = 16;
const MAX_NEARBY_STAGING_JOBS: usize = 4;
const MAX_NEARBY_STAGING_JOBS_PER_OWNER: usize = 2;
const INCOMING_MEDIA_WAIT: Duration = Duration::from_secs(60 * 60);
const OUTBOUND_MEDIA_HANDOFF_WAIT: Duration = INCOMING_MEDIA_WAIT;
const MEDIA_HANDOFF_HISTORY_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_MEDIA_HANDOFF_HISTORY: usize = 32;
const BLUETOOTH_HANDOFF_WAIT: Duration = Duration::from_secs(75);
const BLUETOOTH_HANDOFF_DISCOVERY: Duration = Duration::from_secs(6);
const MAX_BLUETOOTH_HANDOFFS: usize = 32;
const BLUETOOTH_HANDOFF_HISTORY_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_BLUETOOTH_HANDOFF_HISTORY: usize = 32;
const MAX_GAMEPAD_FORWARDINGS: usize = 8;
const MAX_PENDING_NEARBY_CONSENTS: usize = 64;
const MAX_MEDIA_AUTHORIZATIONS: usize = 64;
const NEARBY_CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
const DEVICE_HEALTH_SAMPLE_TTL: Duration = Duration::from_secs(10);

struct InboundGamepadForwarding {
    source: crate::shell::personal_continuity::DeviceId,
    lease_nonce: [u8; 16],
    identity: os_actions::ForwardedPeripheralIdentity,
    device: os_actions::PeripheralDeviceId,
    lease: os_actions::PeripheralLeaseId,
}

fn finish_gamepad_grant_delivery<T, R, E>(
    sessions: &mut BTreeMap<[u8; 16], T>,
    forwarding_id: [u8; 16],
    delivery: &Result<R, E>,
    mut teardown: impl FnMut(T),
) {
    if delivery.is_err()
        && let Some(session) = sessions.remove(&forwarding_id)
    {
        teardown(session);
    }
}

#[derive(Clone, Debug)]
struct OutboundGamepadForwarding {
    target: crate::shell::personal_continuity::DeviceId,
    lease_nonce: Option<[u8; 16]>,
    sequence: u64,
    requested: Instant,
}

#[derive(Default)]
struct OutboundGamepadState {
    sessions: BTreeMap<[u8; 16], OutboundGamepadForwarding>,
}

#[derive(Clone, Debug)]
struct PendingGamepadAuthorization {
    source: crate::shell::personal_continuity::DeviceId,
    target: crate::shell::personal_continuity::DeviceId,
    app_id: String,
    workspace_id: String,
    session_id: String,
    label: String,
    received: Instant,
}

impl PendingGamepadAuthorization {
    fn matches_local_focus(
        &self,
        focus: Option<&crate::shell::physical_gamepad::PhysicalGamepadBinding>,
    ) -> bool {
        focus.is_some_and(|focus| {
            self.app_id == focus.app_id
                && self.workspace_id == focus.workspace_id
                && self.session_id == focus.session_id
        })
    }
}

fn identity_matches_local_focus(
    identity: &os_actions::ForwardedPeripheralIdentity,
    focus: Option<&crate::shell::physical_gamepad::PhysicalGamepadBinding>,
) -> bool {
    focus.is_some_and(|focus| {
        identity.app_id() == focus.app_id
            && identity.workspace_id() == focus.workspace_id
            && identity.session_id() == focus.session_id
    })
}

#[derive(Default)]
struct GamepadAuthorizationState {
    pending: BTreeMap<[u8; 16], PendingGamepadAuthorization>,
    decisions: VecDeque<([u8; 16], bool)>,
}

impl GamepadAuthorizationState {
    fn queue(
        &mut self,
        forwarding_id: [u8; 16],
        request: PendingGamepadAuthorization,
        inbound_exists: bool,
        inbound_count: usize,
    ) -> bool {
        if inbound_exists
            || self.pending.contains_key(&forwarding_id)
            || inbound_count.saturating_add(self.pending.len()) >= MAX_GAMEPAD_FORWARDINGS
        {
            return false;
        }
        self.pending.insert(forwarding_id, request);
        true
    }

    fn decide(&mut self, forwarding_id: [u8; 16], approved: bool) -> bool {
        if !self.pending.contains_key(&forwarding_id)
            || self.decisions.iter().any(|(id, _)| *id == forwarding_id)
        {
            return false;
        }
        self.decisions.push_back((forwarding_id, approved));
        true
    }
}

fn outbound_gamepad_state() -> &'static Mutex<OutboundGamepadState> {
    static STATE: OnceLock<Mutex<OutboundGamepadState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(OutboundGamepadState::default()))
}

fn gamepad_authorization_state() -> &'static Mutex<GamepadAuthorizationState> {
    static STATE: OnceLock<Mutex<GamepadAuthorizationState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(GamepadAuthorizationState::default()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NearbyDropEvent {
    Sending {
        file_name: String,
        peer: crate::shell::personal_continuity::DeviceId,
    },
    Sent {
        file_name: String,
        peer: crate::shell::personal_continuity::DeviceId,
    },
    Failed {
        file_name: String,
        peer: crate::shell::personal_continuity::DeviceId,
    },
    Received {
        path: PathBuf,
        peer: crate::shell::personal_continuity::DeviceId,
    },
}

#[derive(Default)]
struct NearbyDropState {
    events: VecDeque<NearbyDropEvent>,
    pending: BTreeMap<[u8; 16], (crate::shell::personal_continuity::DeviceId, String, u64)>,
    large_offers: BTreeMap<
        [u8; 16],
        (
            crate::shell::personal_continuity::DeviceId,
            [u8; 16],
            String,
        ),
    >,
    progress: BTreeMap<
        [u8; 16],
        (
            crate::shell::personal_continuity::DeviceId,
            u64,
            u64,
            PersonalTransferStatus,
        ),
    >,
    last_offer: BTreeMap<[u8; 16], Instant>,
    incoming: BTreeMap<[u8; 16], PendingNearbyConsent>,
    destinations: BTreeMap<[u8; 16], crate::shell::nearby_transfer::SecureNearbyDestination>,
    history: VecDeque<NearbyTransferHistory>,
    /// Exact sender/transfer pairs a queued `ResumeMedia` authorised. Only
    /// these media-handoff offers are accepted, without a Files consent, and
    /// only into the private media cache.
    media_authorized: BTreeSet<(crate::shell::personal_continuity::DeviceId, [u8; 16])>,
}

struct NearbyTransferHistory {
    transfer_id: [u8; 16],
    peer: crate::shell::personal_continuity::DeviceId,
    name: String,
    phase: runtime_collab::NearbyTransferPhase,
    total_bytes: u64,
}

enum PendingNearbyPayload {
    Inline {
        bytes: Vec<u8>,
        content_hash: [u8; 32],
    },
    Durable {
        kind: crate::shell::nearby_transfer::TransferKind,
        content_hash: [u8; 32],
    },
}

struct PendingNearbyConsent {
    source: crate::shell::personal_continuity::DeviceId,
    request_id: [u8; 16],
    name: String,
    total_bytes: u64,
    received: Instant,
    payload: PendingNearbyPayload,
}

impl NearbyDropState {
    fn insert_consent(&mut self, transfer_id: [u8; 16], offer: PendingNearbyConsent) -> bool {
        if self.incoming.contains_key(&transfer_id)
            || self.incoming.len() >= MAX_PENDING_NEARBY_CONSENTS
        {
            return false;
        }
        self.incoming.insert(transfer_id, offer);
        true
    }

    fn record_history(&mut self, entry: NearbyTransferHistory) {
        self.history
            .retain(|existing| existing.transfer_id != entry.transfer_id);
        self.history.push_front(entry);
        self.history.truncate(32);
    }

    /// Authorise the media-handoff object a `ResumeMedia` names. Bounded:
    /// the oldest authorisation gives way, and its offer is then refused.
    fn authorize_media(
        &mut self,
        source: crate::shell::personal_continuity::DeviceId,
        transfer_id: [u8; 16],
    ) {
        while self.media_authorized.len() >= MAX_MEDIA_AUTHORIZATIONS {
            if self.media_authorized.pop_first().is_none() {
                break;
            }
        }
        self.media_authorized.insert((source, transfer_id));
    }
}

fn nearby_drop_state() -> &'static Mutex<NearbyDropState> {
    static STATE: OnceLock<Mutex<NearbyDropState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(NearbyDropState::default()))
}

fn nearby_transfer_root() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo").join("nearby-transfers"))
}

fn nearby_store_cell() -> &'static Mutex<Option<crate::shell::nearby_transfer::NearbyTransferStore>>
{
    static STORE: OnceLock<Mutex<Option<crate::shell::nearby_transfer::NearbyTransferStore>>> =
        OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

fn with_nearby_transfer_store<T>(
    operation: impl FnOnce(&mut crate::shell::nearby_transfer::NearbyTransferStore) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = nearby_store_cell()
        .lock()
        .map_err(|_| "nearby transfer store is unavailable".to_string())?;
    if guard.is_none() {
        let root = nearby_transfer_root()
            .ok_or_else(|| "nearby transfer state directory is unavailable".to_string())?;
        *guard = Some(crate::shell::nearby_transfer::NearbyTransferStore::open(
            root,
            unix_now_ms(),
        )?);
        remove_legacy_media_cache();
    }
    operation(
        guard
            .as_mut()
            .ok_or_else(|| "nearby transfer store is unavailable".to_string())?,
    )
}

/// Run `operation` only against a store this session already opened, and
/// only when no staging worker holds it: the frame loop never waits for a
/// large copy. Callers either retry on a later frame or leave cleanup to the
/// pass that drops unreferenced media-handoff state.
fn with_open_nearby_transfer_store<T>(
    operation: impl FnOnce(&mut crate::shell::nearby_transfer::NearbyTransferStore) -> T,
) -> Option<T> {
    nearby_store_cell()
        .try_lock()
        .ok()
        .and_then(|mut guard| guard.as_mut().map(operation))
}

/// The retired media-handoff lane kept its own cache. Its objects were only
/// ever session-local player inputs, so nothing there is still referenced.
fn remove_legacy_media_cache() {
    if let Some(legacy) = tilo_paths::cache_dir().map(|root| root.join("tilo/personal-media")) {
        let _ = std::fs::remove_dir_all(legacy);
    }
}

fn try_outbox_entries() -> Vec<crate::shell::nearby_transfer::OutboxEntry> {
    try_nearby_entries(crate::shell::nearby_transfer::NearbyTransferStore::outbox)
}

fn try_inbox_entries() -> Vec<crate::shell::nearby_transfer::InboxEntry> {
    try_nearby_entries(crate::shell::nearby_transfer::NearbyTransferStore::inbox)
}

/// A copy of the store's `entries`, opening the store on first use. A store
/// that is busy or cannot open reads as empty rather than blocking.
fn try_nearby_entries<T: Clone>(
    entries: impl FnOnce(&crate::shell::nearby_transfer::NearbyTransferStore) -> &[T],
) -> Vec<T> {
    let Ok(mut guard) = nearby_store_cell().try_lock() else {
        return Vec::new();
    };
    if guard.is_none() {
        let Some(root) = nearby_transfer_root() else {
            return Vec::new();
        };
        let Ok(store) =
            crate::shell::nearby_transfer::NearbyTransferStore::open(root, unix_now_ms())
        else {
            return Vec::new();
        };
        *guard = Some(store);
    }
    guard
        .as_ref()
        .map_or_else(Vec::new, |store| entries(store).to_vec())
}

struct NearbyStagingResult {
    transfer_id: [u8; 16],
    target: crate::shell::personal_continuity::DeviceId,
    name: String,
    result: Result<(), String>,
    /// The media handoff this object carries, when it is not a Nearby send.
    media_request: Option<[u8; 16]>,
}

#[derive(Default)]
struct NearbyStagingAdmission {
    owners: BTreeMap<String, usize>,
    generation: u64,
}

impl NearbyStagingAdmission {
    fn acquire(&mut self, owner: &str) -> bool {
        let total = self.owners.values().sum::<usize>();
        let held = self.owners.get(owner).copied().unwrap_or_default();
        if total >= MAX_NEARBY_STAGING_JOBS || held >= MAX_NEARBY_STAGING_JOBS_PER_OWNER {
            return false;
        }
        // `held` is below the per-owner cap here, so this never saturates.
        self.owners
            .insert(owner.to_string(), held.saturating_add(1));
        true
    }

    fn release(&mut self, owner: &str) {
        let Some(held) = self.owners.get_mut(owner) else {
            return;
        };
        *held = held.saturating_sub(1);
        if *held == 0 {
            self.owners.remove(owner);
        }
    }

    fn release_if_generation(&mut self, owner: &str, generation: u64) {
        if self.generation == generation {
            self.release(owner);
        }
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.owners.clear();
    }
}

fn nearby_staging_admission() -> &'static Mutex<NearbyStagingAdmission> {
    static ADMISSION: OnceLock<Mutex<NearbyStagingAdmission>> = OnceLock::new();
    ADMISSION.get_or_init(|| Mutex::new(NearbyStagingAdmission::default()))
}

struct NearbyStagingPermit {
    owner: String,
    generation: u64,
}

impl NearbyStagingPermit {
    fn acquire(owner: &str) -> Result<Self, String> {
        let mut admission = nearby_staging_admission()
            .lock()
            .map_err(|_| "Nearby Send staging admission is unavailable".to_string())?;
        let admitted = admission.acquire(owner);
        // Read the generation in the same critical section as the admission.
        let generation = admission.generation;
        drop(admission);
        admitted
            .then(|| Self {
                owner: owner.to_string(),
                generation,
            })
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-nearby-busy"))
    }

    fn valid(&self) -> bool {
        nearby_staging_admission()
            .lock()
            .is_ok_and(|admission| admission.generation == self.generation)
    }
}

impl Drop for NearbyStagingPermit {
    fn drop(&mut self) {
        if let Ok(mut admission) = nearby_staging_admission().lock() {
            admission.release_if_generation(&self.owner, self.generation);
        }
    }
}

fn nearby_staging_queue() -> &'static (
    mpsc::Sender<NearbyStagingResult>,
    Mutex<mpsc::Receiver<NearbyStagingResult>>,
) {
    static QUEUE: OnceLock<(
        mpsc::Sender<NearbyStagingResult>,
        Mutex<mpsc::Receiver<NearbyStagingResult>>,
    )> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        (sender, Mutex::new(receiver))
    })
}

pub fn take_nearby_drop_events() -> Vec<NearbyDropEvent> {
    nearby_drop_state()
        .lock()
        .map_or_else(|_| Vec::new(), |mut state| state.events.drain(..).collect())
}

pub(crate) fn record_nearby_transfer_progress(
    target: crate::shell::personal_continuity::DeviceId,
    transfer_id: [u8; 16],
    sent_bytes: u64,
    total_bytes: u64,
    status: PersonalTransferStatus,
) {
    // The one transfer lane serves Nearby sends and media handoffs alike.
    let policy = ContinuityPolicySnapshot::load();
    if !policy.permits(ContinuityCapability::FileTransfer)
        && !policy.permits(ContinuityCapability::Media)
    {
        return;
    }
    if let Ok(mut state) = nearby_drop_state().lock() {
        state
            .progress
            .insert(transfer_id, (target, sent_bytes, total_bytes, status));
        if matches!(
            status,
            PersonalTransferStatus::Failed | PersonalTransferStatus::Cancelled
        ) {
            state.large_offers.remove(&transfer_id);
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "chunk and finish frames share one validated receive path"
)]
pub(crate) fn handle_nearby_transfer_frame(
    network: &ShellNetwork,
    source: &crate::shell::personal_continuity::DeviceId,
    frame: PersonalNearbyTransferFrame,
) {
    let (transfer_id, request_id) = match &frame {
        PersonalNearbyTransferFrame::Chunk {
            transfer_id,
            request_id,
            ..
        }
        | PersonalNearbyTransferFrame::Finish {
            transfer_id,
            request_id,
            ..
        }
        | PersonalNearbyTransferFrame::Abort {
            transfer_id,
            request_id,
        } => (*transfer_id, *request_id),
    };
    // Bytes land only in an inbox entry admitted for this exact sender and
    // request, and only while the continuity switch for its kind is on.
    let kind = with_nearby_transfer_store(|store| {
        Ok(store
            .inbox()
            .iter()
            .find(|entry| {
                entry.id == transfer_id
                    && entry.source == source.as_str()
                    && entry.request_id == request_id
            })
            .map(|entry| entry.kind))
    })
    .ok()
    .flatten()
    .filter(|kind| ContinuityPolicySnapshot::load().permits(kind.capability()));
    let Some(kind) = kind else {
        return;
    };
    match frame {
        PersonalNearbyTransferFrame::Chunk { offset, bytes, .. } => {
            if bytes.len() <= crate::network::actor::PERSONAL_TRANSFER_CHUNK_BYTES {
                let _ =
                    with_nearby_transfer_store(|store| store.append(transfer_id, offset, &bytes));
            }
        }
        PersonalNearbyTransferFrame::Finish { content_hash, .. }
            if kind == crate::shell::nearby_transfer::TransferKind::MediaHandoff =>
        {
            finish_media_handoff(network, source, (transfer_id, request_id), content_hash);
        }
        PersonalNearbyTransferFrame::Finish { content_hash, .. } => {
            let history = try_inbox_entries()
                .into_iter()
                .find(|entry| entry.id == transfer_id)
                .map(|entry| (entry.name, entry.total_bytes));
            let expected = with_nearby_transfer_store(|store| {
                Ok(store
                    .inbox()
                    .iter()
                    .find(|entry| entry.id == transfer_id)
                    .map(|entry| entry.content_hash))
            })
            .ok()
            .flatten();
            let destination = nearby_drop_state()
                .lock()
                .ok()
                .and_then(|mut state| state.destinations.remove(&transfer_id));
            let installed = expected
                .filter(|expected| expected == &content_hash)
                .zip(destination)
                .and_then(|(_, destination)| {
                    with_nearby_transfer_store(|store| {
                        store.finish_secure(transfer_id, &destination)
                    })
                    .ok()
                });
            if installed.is_none() {
                let _ = with_nearby_transfer_store(|store| store.cancel_incoming(transfer_id));
            }
            if let Some(path) = installed.as_ref()
                && let Ok(mut state) = nearby_drop_state().lock()
            {
                state.events.push_back(NearbyDropEvent::Received {
                    path: path.clone(),
                    peer: source.clone(),
                });
            }
            if let Some((name, total_bytes)) = history
                && let Ok(mut state) = nearby_drop_state().lock()
            {
                state.record_history(NearbyTransferHistory {
                    transfer_id,
                    peer: source.clone(),
                    name,
                    phase: if installed.is_some() {
                        runtime_collab::NearbyTransferPhase::Completed
                    } else {
                        runtime_collab::NearbyTransferPhase::Failed
                    },
                    total_bytes,
                });
            }
            let _ = network.send_personal_fabric_action(
                source,
                PersonalFabricAction::NearbyTransferResult {
                    request_id,
                    transfer_id,
                    accepted: installed.is_some(),
                },
            );
        }
        // A transport abort is resumable: keep only the last durable prefix.
        PersonalNearbyTransferFrame::Abort { .. } => {}
    }
}

/// Resolve a handoff's verified object, or say what the handoff should do
/// instead: wait while its bytes are still arriving under a live
/// authorisation, or give up once a refused, failed or revoked receive
/// leaves nothing to wait for.
fn cached_media_handoff(
    source: &crate::shell::personal_continuity::DeviceId,
    transfer_id: [u8; 16],
) -> Result<PathBuf, IncomingMediaLaunch> {
    // A staging worker holding the store only delays the lookup.
    let Ok(mut store) = nearby_store_cell().try_lock() else {
        return Err(IncomingMediaLaunch::Waiting);
    };
    let cached = store
        .as_mut()
        .and_then(|store| store.cached_media(source.as_str(), transfer_id));
    drop(store);
    if let Some(path) = cached {
        return Ok(path);
    }
    let arriving = nearby_drop_state().lock().is_ok_and(|state| {
        state
            .media_authorized
            .contains(&(source.clone(), transfer_id))
    });
    Err(if arriving {
        IncomingMediaLaunch::Waiting
    } else {
        IncomingMediaLaunch::Rejected(MediaHandoffFailure::DestinationUnavailable)
    })
}

/// Verify a completed media-handoff object into the private media cache.
/// The handoff's authorisation is spent here, so a repeated offer of the
/// same object is refused rather than received twice.
fn finish_media_handoff(
    network: &ShellNetwork,
    source: &crate::shell::personal_continuity::DeviceId,
    (transfer_id, request_id): ([u8; 16], [u8; 16]),
    content_hash: [u8; 32],
) {
    let authorized = nearby_drop_state().lock().is_ok_and(|mut state| {
        state
            .media_authorized
            .remove(&(source.clone(), transfer_id))
    });
    let cached = authorized
        .then(|| {
            with_nearby_transfer_store(|store| {
                let offered = store
                    .inbox()
                    .iter()
                    .find(|entry| entry.id == transfer_id)
                    .map(|entry| entry.content_hash);
                if offered != Some(content_hash) {
                    return Err("media handoff finish does not match its offer".to_string());
                }
                store.finish_to_cache(transfer_id)
            })
            .ok()
        })
        .flatten();
    if cached.is_none() {
        let _ = with_nearby_transfer_store(|store| store.cancel_incoming(transfer_id));
    }
    let _ = network.send_personal_fabric_action(
        source,
        PersonalFabricAction::NearbyTransferResult {
            request_id,
            transfer_id,
            accepted: cached.is_some(),
        },
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingIncomingMedia {
    received: Instant,
    request_id: [u8; 16],
    source: crate::shell::personal_continuity::DeviceId,
    session_id: String,
    app_id: String,
    workspace_id: String,
    title: String,
    locator: MediaSourceLocator,
    position_ms: u64,
    resume_playing: bool,
    has_video: bool,
    route: crate::shell::network::AuthenticatedPersonalRoute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IncomingMediaLaunch {
    Waiting,
    Launched,
    Rejected(MediaHandoffFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaHandoffPhase {
    Pending,
    Completed,
    Failed(MediaHandoffFailure),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaHandoffStatusView {
    pub request_id: [u8; 16],
    pub session_id: String,
    pub title: String,
    pub target: crate::shell::personal_continuity::DeviceId,
    pub target_label: String,
    pub phase: MediaHandoffPhase,
}

#[derive(Clone)]
struct MediaHandoffStatus {
    request_id: [u8; 16],
    session_id: String,
    title: String,
    target: crate::shell::personal_continuity::DeviceId,
    phase: MediaHandoffPhase,
    updated: Instant,
    route: crate::shell::network::AuthenticatedPersonalRoute,
    personal_transfer_id: Option<[u8; 16]>,
}

/// Stop the local-file transfer behind one media handoff: its stream, its
/// offer at the destination, and its staged copy. A copy still being staged
/// is dropped when staging finishes and finds no pending handoff.
fn cancel_status_personal_transfer(network: &ShellNetwork, status: &MediaHandoffStatus) {
    let Some(transfer_id) = status.personal_transfer_id else {
        return;
    };
    let _ = network.cancel_nearby_transfer_stream(&status.target, transfer_id);
    let offered = nearby_drop_state().lock().ok().and_then(|mut state| {
        state.progress.remove(&transfer_id);
        state.last_offer.remove(&transfer_id);
        state
            .large_offers
            .remove(&transfer_id)
            .filter(|(target, _, _)| target == &status.target)
    });
    if let Some((target, request_id, _)) = offered {
        let _ = network.send_personal_fabric_action(
            &target,
            PersonalFabricAction::NearbyTransferCancel {
                request_id,
                transfer_id,
            },
        );
    }
    let _ = with_open_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
}

/// Whether a still-pending media handoff carries this staged object to `target`.
fn media_transfer_pending(
    transfer_id: [u8; 16],
    target: &crate::shell::personal_continuity::DeviceId,
) -> bool {
    media_handoff_statuses().lock().is_ok_and(|statuses| {
        statuses.values().any(|status| {
            status.personal_transfer_id == Some(transfer_id)
                && &status.target == target
                && status.phase == MediaHandoffPhase::Pending
        })
    })
}

fn media_handoff_statuses() -> &'static Mutex<BTreeMap<[u8; 16], MediaHandoffStatus>> {
    static STATUSES: OnceLock<Mutex<BTreeMap<[u8; 16], MediaHandoffStatus>>> = OnceLock::new();
    STATUSES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn record_media_handoff_status(status: MediaHandoffStatus) {
    let Ok(mut statuses) = media_handoff_statuses().lock() else {
        return;
    };
    statuses.retain(|_, existing| existing.updated.elapsed() < MEDIA_HANDOFF_HISTORY_TTL);
    while statuses.len() >= MAX_MEDIA_HANDOFF_HISTORY {
        let Some(oldest) = statuses
            .iter()
            .min_by_key(|(_, existing)| existing.updated)
            .map(|(request_id, _)| *request_id)
        else {
            break;
        };
        statuses.remove(&oldest);
    }
    statuses.insert(status.request_id, status);
}

fn finish_media_handoff_status(request_id: [u8; 16], phase: MediaHandoffPhase) {
    if let Ok(mut statuses) = media_handoff_statuses().lock()
        && let Some(status) = statuses.get_mut(&request_id)
    {
        status.phase = phase;
        status.updated = Instant::now();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OutboundSpatialInput {
    sequence: Option<u64>,
    target: crate::shell::personal_continuity::DeviceId,
    lease_nonce: [u8; 16],
    event: FabricInputEvent,
}

#[derive(Default)]
struct SpatialInputState {
    protocol: spatial_protocol::SpatialProtocol,
    cleanup: spatial_cleanup::SpatialCleanup,
    withdrawals: spatial_withdrawal::SpatialWithdrawals,
    /// Permissions this device issued and must enforce on inbound input.
    issued: BTreeMap<[u8; 16], InputLease>,
    automatic_issued: BTreeMap<crate::shell::personal_continuity::DeviceId, [u8; 16]>,
    issued_routes: BTreeMap<[u8; 16], crate::shell::network::AuthenticatedPersonalRoute>,
    /// Permissions other devices issued to this controller.
    received: BTreeMap<crate::shell::personal_continuity::DeviceId, InputLease>,
    /// The exact certified topic/endpoint/epoch that delivered each grant.
    received_routes: BTreeMap<
        crate::shell::personal_continuity::DeviceId,
        crate::shell::network::AuthenticatedPersonalRoute,
    >,
    inbound: VecDeque<(
        crate::shell::personal_continuity::DeviceId,
        FabricInputEvent,
    )>,
    release_controllers: VecDeque<crate::shell::personal_continuity::DeviceId>,
    outbound: VecDeque<OutboundSpatialInput>,
    /// Last authenticated transport-departure generation consumed from the
    /// network's bounded history. A cursor prevents an old departure from
    /// revoking authority freshly granted after the same device reconnects.
    last_peer_departure_generation: u64,
    placement_override: Option<DisplayRect>,
}

impl SpatialInputState {
    fn release_controller(&mut self, controller: &crate::shell::personal_continuity::DeviceId) {
        // A down edge admitted before revocation must not be delivered after
        // the compositor consumes that controller's seat release barrier.
        self.inbound.retain(|(source, _)| source != controller);
        if !self.release_controllers.contains(controller) {
            self.release_controllers.push_back(controller.clone());
        }
    }

    /// Restore only shared-plane origin from the existing owner checkpoint.
    /// Dimensions always come from live native output geometry.
    fn placed_local_rect(
        &mut self,
        current: Option<DisplayRect>,
        previous: Option<&DisplayPlacement>,
    ) -> Option<DisplayRect> {
        let current = current?;
        let origin = self
            .placement_override
            .or_else(|| previous.map(|display| display.rect));
        let rect = origin.map_or(current, |origin| DisplayRect {
            x: origin.x,
            y: origin.y,
            ..current
        });
        self.placement_override = Some(rect);
        Some(rect)
    }

    fn accept_display_move(
        &mut self,
        fabric: &DeviceFabric,
        target: &crate::shell::personal_continuity::DeviceId,
        expected: &FabricStamp,
        rect: DisplayRect,
    ) -> bool {
        let Some(display) = fabric
            .snapshots()
            .get(target)
            .and_then(|snapshot| snapshot.display.as_ref())
        else {
            return false;
        };
        if !display.accepts_move(expected, rect)
            || !fabric.display_destination_clear(target, rect)
            || self
                .placement_override
                .is_some_and(|pending| pending != display.rect)
        {
            return false;
        }
        self.placement_override = Some(rect);
        true
    }

    fn apply_checked_display_move(
        &mut self,
        fabric: &DeviceFabric,
        target: &crate::shell::personal_continuity::DeviceId,
        action: &PersonalFabricAction,
    ) {
        if let PersonalFabricAction::SetDisplayPlacementChecked {
            expected,
            rect,
            expires_unix_ms,
        } = action
            && unix_now_ms() <= *expires_unix_ms
        {
            self.accept_display_move(fabric, target, expected, *rect);
        }
    }

    fn accept_lease_bound_input(
        &mut self,
        route: &crate::shell::network::AuthenticatedPersonalRoute,
        target: &crate::shell::personal_continuity::DeviceId,
        input: ([u8; 16], FabricInputEvent),
        now: u64,
    ) {
        if !self.legacy_motion_overtaken(input.0, input.1)
            && self.input_is_permitted(route, target, input, now)
        {
            self.inbound.push_back((route.device().clone(), input.1));
        }
    }

    fn queue_lease_bound_input(
        &mut self,
        controller: &crate::shell::personal_continuity::DeviceId,
        input: OutboundSpatialInput,
        now: u64,
    ) -> Result<(), String> {
        if !crate::shell::device_fabric::permits_spatial_input(
            self.received.get(&input.target),
            controller,
            &input.target,
            (input.lease_nonce, input.event),
            now,
        ) {
            return Err(crate::shell::i18n::t(
                "device-fabric-error-control-not-granted",
            ));
        }
        self.queue_outbound(input)
    }

    fn queue_outbound(&mut self, input: OutboundSpatialInput) -> Result<(), String> {
        if self.cleanup.blocks(&input.target, input.lease_nonce) {
            return Err(crate::shell::i18n::t(
                "device-fabric-error-input-cleanup-pending",
            ));
        }
        if matches!(input.event, FabricInputEvent::PointerMotion { .. })
            && let Some(last) = self.outbound.back_mut()
            && last.target == input.target
            && last.lease_nonce == input.lease_nonce
            && matches!(last.event, FabricInputEvent::PointerMotion { .. })
        {
            *last = input;
            return Ok(());
        }
        if self.outbound.len() >= MAX_QUEUED_SPATIAL_INPUT {
            return Err("spatial input queue is full".into());
        }
        self.outbound.push_back(input);
        Ok(())
    }
}

fn input_control_operations() -> &'static Mutex<()> {
    static OPERATIONS: OnceLock<Mutex<()>> = OnceLock::new();
    OPERATIONS.get_or_init(|| Mutex::new(()))
}

fn spatial_input_state() -> &'static Mutex<SpatialInputState> {
    static STATE: OnceLock<Mutex<SpatialInputState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SpatialInputState::default()))
}

/// This device's power source and whether energy saver is on.
type LocalPower = (crate::shell::spatial_calibration::DevicePower, bool);

/// A peer's health report, addressed.
type HealthReply = (
    crate::shell::personal_continuity::DeviceId,
    crate::shell::spatial_calibration::DeviceHealthReport,
);

/// Probes answered once the in-flight power sample lands; older ones are
/// dropped first.
const MAX_PENDING_HEALTH_PROBES: usize = 32;

/// A probe older than a fabric action may be is not worth answering.
const PENDING_HEALTH_PROBE_TTL: Duration =
    Duration::from_millis(crate::shell::device_fabric::MAX_FABRIC_ACTION_AGE_MS);

/// A peer's probe waiting for the power sample.
struct PendingHealthProbe {
    peer: crate::shell::personal_continuity::DeviceId,
    nonce: [u8; 16],
    received: Instant,
}

#[derive(Default)]
struct DeviceHealthRuntimeState {
    tracker: crate::shell::spatial_calibration::DeviceHealthTracker,
    last_probe: Option<Instant>,
    local_power: Option<(
        Instant,
        crate::shell::spatial_calibration::DevicePower,
        bool,
    )>,
    /// The power sample running on its own thread, if one is.
    power_sample: Option<mpsc::Receiver<LocalPower>>,
    pending_probes: VecDeque<PendingHealthProbe>,
}

fn device_health_state() -> &'static Mutex<DeviceHealthRuntimeState> {
    static STATE: OnceLock<Mutex<DeviceHealthRuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(DeviceHealthRuntimeState::default()))
}

fn fabric_clock_state() -> &'static Mutex<crate::shell::fabric_clock::FabricClockCore> {
    static STATE: OnceLock<Mutex<crate::shell::fabric_clock::FabricClockCore>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(crate::shell::fabric_clock::FabricClockCore::default()))
}

fn with_fabric_clock<T>(
    apply: impl FnOnce(&mut crate::shell::fabric_clock::FabricClockCore) -> T,
) -> T {
    let mut core = fabric_clock_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    apply(&mut core)
}

/// Keeps one peer's clock estimate fresh: the fabric clock probes a peer
/// only while a handle for it is alive, at
/// [`CLOCK_SYNC_PROBE_INTERVAL`](crate::shell::fabric_clock::CLOCK_SYNC_PROBE_INTERVAL),
/// and only while Media continuity is on. Dropping the last handle stops
/// probing and forgets the estimate.
#[must_use = "the peer's clock is kept only while the handle is held"]
#[derive(Debug)]
pub struct FabricClockSync {
    peer: crate::shell::personal_continuity::DeviceId,
}

impl FabricClockSync {
    #[must_use]
    pub fn peer(&self) -> &crate::shell::personal_continuity::DeviceId {
        &self.peer
    }

    /// The peer's current clock model; see [`fabric_peer_clock`].
    #[must_use]
    pub fn estimate(&self) -> Option<crate::shell::fabric_clock::ClockEstimate> {
        fabric_peer_clock(&self.peer)
    }

    /// Whether audio timed against the peer can play now, and why not.
    #[must_use]
    pub fn state(&self) -> crate::shell::fabric_clock::ClockSyncState {
        let now_us = fabric_monotonic_us();
        with_fabric_clock(|core| core.state(&self.peer, Instant::now(), now_us))
    }
}

impl Drop for FabricClockSync {
    fn drop(&mut self) {
        with_fabric_clock(|core| core.release(&self.peer));
    }
}

/// Keep `peer`'s clock synchronised for as long as the returned handle
/// lives.
pub fn hold_fabric_clock_sync(
    peer: crate::shell::personal_continuity::DeviceId,
) -> FabricClockSync {
    with_fabric_clock(|core| core.hold(peer.clone(), Instant::now()));
    FabricClockSync { peer }
}

/// The certified peer's clock relative to [`fabric_monotonic_us`], or `None`
/// while nothing holds it in sync or before its first clock report.
#[must_use]
pub fn fabric_peer_clock(
    peer: &crate::shell::personal_continuity::DeviceId,
) -> Option<crate::shell::fabric_clock::ClockEstimate> {
    let now_us = fabric_monotonic_us();
    with_fabric_clock(|core| core.estimate(peer, now_us))
}

fn spatial_calibration_state()
-> &'static Mutex<crate::shell::spatial_calibration::SpatialCalibrationState> {
    static STATE: OnceLock<Mutex<crate::shell::spatial_calibration::SpatialCalibrationState>> =
        OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(crate::shell::spatial_calibration::SpatialCalibrationState::default())
    })
}

/// The cached power sample, while it is fresh enough to report.
fn fresh_local_power(state: &DeviceHealthRuntimeState, now: Instant) -> Option<LocalPower> {
    state
        .local_power
        .filter(|(sampled, _, _)| {
            now.checked_duration_since(*sampled)
                .is_some_and(|age| age < DEVICE_HEALTH_SAMPLE_TTL)
        })
        .map(|(_, power, saver)| (power, saver))
}

/// Read this device's power source and profile. `powerprofilesctl` is a
/// subprocess bounded at 2.5 s, so this runs only on a sampling thread.
fn sample_local_power() -> LocalPower {
    let battery = os_actions::BatteryInfo::from_system();
    let power = if !battery.present {
        crate::shell::spatial_calibration::DevicePower::External
    } else if battery.on_ac || battery.status.eq_ignore_ascii_case("charging") {
        crate::shell::spatial_calibration::DevicePower::Charging(
            u8::try_from(battery.capacity.min(100)).unwrap_or(100),
        )
    } else {
        crate::shell::spatial_calibration::DevicePower::Battery(
            u8::try_from(battery.capacity.min(100)).unwrap_or(100),
        )
    };
    let saver = matches!(
        os_actions::get_power_profile(),
        Ok(os_actions::PowerProfile::Saver)
    );
    (power, saver)
}

fn health_report(
    nonce: [u8; 16],
    (power, energy_saver): LocalPower,
) -> crate::shell::spatial_calibration::DeviceHealthReport {
    crate::shell::spatial_calibration::DeviceHealthReport {
        probe_nonce: nonce,
        power,
        energy_saver,
    }
}

/// Answer `peer`'s probe from a fresh sample, or queue it behind a sample
/// taken by `sample` on its own thread. A peer decides when its probes
/// arrive, so nothing here may wait on the power daemon.
fn answer_health_probe(
    state: &mut DeviceHealthRuntimeState,
    peer: crate::shell::personal_continuity::DeviceId,
    nonce: [u8; 16],
    now: Instant,
    sample: impl FnOnce() -> LocalPower + Send + 'static,
) -> Option<HealthReply> {
    if let Some(local) = fresh_local_power(state, now) {
        return Some((peer, health_report(nonce, local)));
    }
    if state.pending_probes.len() >= MAX_PENDING_HEALTH_PROBES {
        state.pending_probes.pop_front();
    }
    state.pending_probes.push_back(PendingHealthProbe {
        peer,
        nonce,
        received: now,
    });
    if state.power_sample.is_none() {
        let (sampled, receiver) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("tilo-health-sample".into())
            .spawn(move || {
                let _ = sampled.send(sample());
            });
        match spawned {
            Ok(_) => state.power_sample = Some(receiver),
            Err(error) => {
                tracing::warn!(%error, "could not sample local power for a health probe");
                state.pending_probes.clear();
            }
        }
    }
    None
}

/// Take a finished power sample and the replies to every probe that waited
/// for it and is still recent.
fn finish_power_sample(state: &mut DeviceHealthRuntimeState, now: Instant) -> Vec<HealthReply> {
    let Some(receiver) = state.power_sample.as_ref() else {
        return Vec::new();
    };
    let local = match receiver.try_recv() {
        Ok(local) => local,
        Err(mpsc::TryRecvError::Empty) => return Vec::new(),
        Err(mpsc::TryRecvError::Disconnected) => {
            state.power_sample = None;
            state.pending_probes.clear();
            return Vec::new();
        }
    };
    state.power_sample = None;
    state.local_power = Some((now, local.0, local.1));
    std::mem::take(&mut state.pending_probes)
        .into_iter()
        .filter(|probe| {
            now.checked_duration_since(probe.received)
                .is_none_or(|age| age < PENDING_HEALTH_PROBE_TTL)
        })
        .map(|probe| (probe.peer, health_report(probe.nonce, local)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BluetoothHandoffPhase {
    AwaitingReady,
    InspectingTarget,
    AwaitingLocalAuthorization,
    PairingReady,
    Ready,
    DisconnectingSource,
    AwaitingFinish,
    PairingTarget,
    ConnectingTarget,
    AwaitingAcceptance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BluetoothHandoffFailure {
    InspectTarget,
    TargetUnavailable,
    DisconnectSource,
    PairTarget,
    ConnectTarget,
    Send,
    TimedOut,
    Rollback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BluetoothHandoffPhaseView {
    PreparingTarget,
    AwaitingApproval,
    ReleasingSource,
    PairingTarget,
    ConnectingTarget,
    AwaitingConfirmation,
    Completed,
    Failed(BluetoothHandoffFailure),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothHandoffStatusView {
    pub request_id: [u8; 16],
    pub accessory_name: String,
    pub peer_label: String,
    pub outbound: bool,
    pub phase: BluetoothHandoffPhaseView,
}

#[derive(Clone, Debug)]
struct BluetoothHandoffTransaction {
    peer: crate::shell::personal_continuity::DeviceId,
    address: String,
    name: String,
    outbound: bool,
    phase: BluetoothHandoffPhase,
    authorization: Option<[u8; 16]>,
    started: Instant,
}

impl BluetoothHandoffTransaction {
    fn view_phase(&self) -> BluetoothHandoffPhaseView {
        match self.phase {
            BluetoothHandoffPhase::AwaitingReady | BluetoothHandoffPhase::InspectingTarget => {
                BluetoothHandoffPhaseView::PreparingTarget
            }
            BluetoothHandoffPhase::AwaitingLocalAuthorization
            | BluetoothHandoffPhase::PairingReady => BluetoothHandoffPhaseView::AwaitingApproval,
            BluetoothHandoffPhase::Ready
            | BluetoothHandoffPhase::DisconnectingSource
            | BluetoothHandoffPhase::AwaitingFinish => BluetoothHandoffPhaseView::ReleasingSource,
            BluetoothHandoffPhase::PairingTarget => BluetoothHandoffPhaseView::PairingTarget,
            BluetoothHandoffPhase::ConnectingTarget => BluetoothHandoffPhaseView::ConnectingTarget,
            BluetoothHandoffPhase::AwaitingAcceptance => {
                BluetoothHandoffPhaseView::AwaitingConfirmation
            }
        }
    }
}

#[derive(Clone, Debug)]
struct BluetoothHandoffOutcome {
    transaction: BluetoothHandoffTransaction,
    phase: BluetoothHandoffPhaseView,
    updated: Instant,
}

impl BluetoothHandoffTransaction {
    fn matches(
        &self,
        peer: &crate::shell::personal_continuity::DeviceId,
        address: &str,
        outbound: bool,
        phase: BluetoothHandoffPhase,
    ) -> bool {
        self.outbound == outbound
            && &self.peer == peer
            && self.address.eq_ignore_ascii_case(address)
            && self.phase == phase
    }

    fn accepts_pairing_authorization(
        &self,
        peer: &crate::shell::personal_continuity::DeviceId,
        address: &str,
        authorization: [u8; 16],
    ) -> bool {
        self.matches(peer, address, false, BluetoothHandoffPhase::PairingReady)
            && self.authorization == Some(authorization)
    }
}

#[derive(Debug)]
enum BluetoothWorkerOutcome {
    TargetInspected(Result<os_actions::BluetoothHandoffTarget, String>),
    SourceDisconnected(Result<(), String>),
    TargetPaired(Result<(), String>),
    TargetConnected(Result<(), String>),
    SourceReconnected(Result<(), String>),
}

#[derive(Debug)]
struct BluetoothWorkerResult {
    request_id: [u8; 16],
    address: String,
    outcome: BluetoothWorkerOutcome,
}

struct BluetoothHandoffState {
    transactions: BTreeMap<[u8; 16], BluetoothHandoffTransaction>,
    outcomes: BTreeMap<[u8; 16], BluetoothHandoffOutcome>,
    result_tx: mpsc::Sender<BluetoothWorkerResult>,
    result_rx: mpsc::Receiver<BluetoothWorkerResult>,
}

impl BluetoothHandoffState {
    fn new() -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        Self {
            transactions: BTreeMap::new(),
            outcomes: BTreeMap::new(),
            result_tx,
            result_rx,
        }
    }

    fn start_worker(
        &self,
        request_id: [u8; 16],
        address: String,
        operation: BluetoothWorkerOperation,
    ) {
        let result_tx = self.result_tx.clone();
        std::thread::spawn(move || {
            let outcome = match operation {
                BluetoothWorkerOperation::InspectTarget => BluetoothWorkerOutcome::TargetInspected(
                    os_actions::inspect_bluetooth_handoff_target(
                        &address,
                        BLUETOOTH_HANDOFF_DISCOVERY,
                    ),
                ),
                BluetoothWorkerOperation::DisconnectSource => {
                    BluetoothWorkerOutcome::SourceDisconnected(os_actions::bt_disconnect(&address))
                }
                BluetoothWorkerOperation::PairTarget => BluetoothWorkerOutcome::TargetPaired(
                    os_actions::pair_bluetooth_handoff(&address),
                ),
                BluetoothWorkerOperation::ConnectTarget => {
                    BluetoothWorkerOutcome::TargetConnected(os_actions::bt_connect(&address))
                }
                BluetoothWorkerOperation::ReconnectSource => {
                    BluetoothWorkerOutcome::SourceReconnected(os_actions::bt_connect(&address))
                }
            };
            let _ = result_tx.send(BluetoothWorkerResult {
                request_id,
                address,
                outcome,
            });
        });
    }

    fn record_outcome(
        &mut self,
        request_id: [u8; 16],
        transaction: BluetoothHandoffTransaction,
        phase: BluetoothHandoffPhaseView,
    ) {
        self.outcomes
            .retain(|_, outcome| outcome.updated.elapsed() < BLUETOOTH_HANDOFF_HISTORY_TTL);
        while self.outcomes.len() >= MAX_BLUETOOTH_HANDOFF_HISTORY {
            let Some(oldest) = self
                .outcomes
                .iter()
                .min_by_key(|(_, outcome)| outcome.updated)
                .map(|(request_id, _)| *request_id)
            else {
                break;
            };
            self.outcomes.remove(&oldest);
        }
        self.outcomes.insert(
            request_id,
            BluetoothHandoffOutcome {
                transaction,
                phase,
                updated: Instant::now(),
            },
        );
    }

    fn finish(
        &mut self,
        request_id: [u8; 16],
        phase: BluetoothHandoffPhaseView,
    ) -> Option<BluetoothHandoffTransaction> {
        let transaction = self.transactions.remove(&request_id)?;
        self.record_outcome(request_id, transaction.clone(), phase);
        Some(transaction)
    }
}

#[derive(Clone, Copy, Debug)]
enum BluetoothWorkerOperation {
    InspectTarget,
    DisconnectSource,
    PairTarget,
    ConnectTarget,
    ReconnectSource,
}

fn bluetooth_handoff_state() -> &'static Mutex<BluetoothHandoffState> {
    static STATE: OnceLock<Mutex<BluetoothHandoffState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(BluetoothHandoffState::new()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BluetoothRollback {
    None,
    ReconnectSource,
    DisconnectTarget,
}

fn cancellation_rollback(transaction: &BluetoothHandoffTransaction) -> BluetoothRollback {
    if transaction.outbound && transaction.phase == BluetoothHandoffPhase::AwaitingFinish {
        BluetoothRollback::ReconnectSource
    } else if !transaction.outbound
        && transaction.phase == BluetoothHandoffPhase::AwaitingAcceptance
    {
        BluetoothRollback::DisconnectTarget
    } else {
        BluetoothRollback::None
    }
}

fn stale_worker_rollback(outcome: &BluetoothWorkerOutcome) -> BluetoothRollback {
    match outcome {
        BluetoothWorkerOutcome::SourceDisconnected(Ok(())) => BluetoothRollback::ReconnectSource,
        BluetoothWorkerOutcome::TargetPaired(Ok(()))
        | BluetoothWorkerOutcome::TargetConnected(Ok(())) => BluetoothRollback::DisconnectTarget,
        _ => BluetoothRollback::None,
    }
}

fn bluetooth_peer_online(
    transaction: &BluetoothHandoffTransaction,
    online: &BTreeSet<crate::shell::personal_continuity::DeviceId>,
) -> bool {
    online.contains(&transaction.peer)
}

fn run_bluetooth_rollback(
    state: &BluetoothHandoffState,
    request_id: [u8; 16],
    address: String,
    rollback: BluetoothRollback,
) {
    match rollback {
        BluetoothRollback::None => {}
        BluetoothRollback::ReconnectSource => state.start_worker(
            request_id,
            address,
            BluetoothWorkerOperation::ReconnectSource,
        ),
        BluetoothRollback::DisconnectTarget => {
            std::thread::spawn(move || {
                let _ = os_actions::bt_disconnect(&address);
            });
        }
    }
}

fn rollback_bluetooth_transaction(
    state: &BluetoothHandoffState,
    request_id: [u8; 16],
    transaction: &BluetoothHandoffTransaction,
) {
    run_bluetooth_rollback(
        state,
        request_id,
        transaction.address.clone(),
        cancellation_rollback(transaction),
    );
    // A disconnect/pair/connect worker that completes after removal is stale.
    // The completion path reconnects a released source or disconnects a newly
    // connected target, so cancellation remains safe across worker races.
}

fn finish_bluetooth_transactions(
    state: &mut BluetoothHandoffState,
    request_ids: &[[u8; 16]],
    phase: BluetoothHandoffPhaseView,
) -> Vec<([u8; 16], BluetoothHandoffTransaction, BluetoothRollback)> {
    request_ids
        .iter()
        .filter_map(|request_id| {
            let transaction = state.finish(*request_id, phase)?;
            let rollback = cancellation_rollback(&transaction);
            Some((*request_id, transaction, rollback))
        })
        .collect()
}

/// Stable local facts supplied by the compositor without blocking discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalFabricCapabilities {
    pub display: Option<DisplayRect>,
    pub pointer: bool,
    pub keyboard: bool,
    pub audio_output: bool,
}

/// Stateful, low-frequency fabric pump owned by the native compositor loop.
pub struct DeviceFabricRuntime {
    counter: u64,
    last_sample: Option<Instant>,
    display_refresh: display_refresh::DisplayRefresh,
    last_publish: Option<Instant>,
    local_fingerprint: Option<[u8; 32]>,
    merged_settings_fingerprint: Option<[u8; 32]>,
    roaming_provenance_fingerprint: Option<[u8; 32]>,
    applied_remote_settings: BTreeMap<String, Value>,
    settings_origin_initialized: bool,
    pending_incoming_media: BTreeMap<[u8; 16], PendingIncomingMedia>,
    waiting_incoming_media: BTreeMap<[u8; 16], PendingIncomingMedia>,
    notification_bridge: NotificationBridge,
    realtime_adapter: Option<crate::shell::realtime_session::SharedRealtimeFabricAdapter>,
    realtime_online_peers: BTreeSet<crate::shell::personal_continuity::DeviceId>,
    remote_capture: remote_capture::RemoteCaptureIntegration,
    tethering: crate::shell::personal_tethering_runtime::PersonalTetheringRuntime,
    gamepad_broker: ProductionGamepadBroker,
    inbound_gamepads: BTreeMap<[u8; 16], InboundGamepadForwarding>,
    local_app_focus: Option<crate::shell::physical_gamepad::PhysicalGamepadBinding>,
    policy: Option<ContinuityPolicySnapshot>,
    policy_cache: Option<ContinuityPolicySnapshot>,
    policy_store_stamp: Option<SystemTime>,
    last_policy_store_check: Option<Instant>,
}

impl DeviceFabricRuntime {
    const POLICY_STORE_POLL_INTERVAL: Duration = Duration::from_millis(750);

    pub fn new(network: &ShellNetwork, notification_bridge: NotificationBridge) -> Self {
        crate::shell::device_unlock_runtime::spawn_enrol_socket();
        let counter = network.device_fabric().max_counter();
        Self {
            counter,
            last_sample: None,
            display_refresh: display_refresh::DisplayRefresh::new(network.loop_waker()),
            last_publish: None,
            local_fingerprint: None,
            merged_settings_fingerprint: None,
            roaming_provenance_fingerprint: None,
            applied_remote_settings: BTreeMap::new(),
            settings_origin_initialized: false,
            pending_incoming_media: BTreeMap::new(),
            waiting_incoming_media: BTreeMap::new(),
            notification_bridge,
            realtime_adapter: None,
            realtime_online_peers: BTreeSet::new(),
            remote_capture: remote_capture::RemoteCaptureIntegration::default(),
            tethering: crate::shell::personal_tethering_runtime::PersonalTetheringRuntime::default(
            ),
            gamepad_broker: os_actions::ForwardedPeripheralBroker::new(
                os_actions::LinuxVirtualGamepadFactory::new(
                    os_actions::UinputGamepadDeviceProvider,
                ),
            ),
            inbound_gamepads: BTreeMap::new(),
            local_app_focus: None,
            policy: None,
            policy_cache: None,
            policy_store_stamp: None,
            last_policy_store_check: None,
        }
    }

    /// Tell the person how their request to wake a device ended.
    fn notify_wake_outcomes(&self) {
        use crate::shell::device_fabric::WakeOutcome;
        for event in crate::shell::device_wake::drain_events() {
            let device = event.label.as_str();
            let (summary, body) = match event.outcome {
                WakeOutcome::Sent { .. } => continue,
                WakeOutcome::Rejoined => (
                    crate::shell::i18n::t_args(
                        "device-wake-rejoined",
                        &[("device", device.into())],
                    ),
                    String::new(),
                ),
                WakeOutcome::NotRejoined => (
                    crate::shell::i18n::t_args(
                        "device-wake-not-rejoined",
                        &[("device", device.into())],
                    ),
                    crate::shell::i18n::t("device-wake-not-rejoined-body"),
                ),
                WakeOutcome::NotOnTargetLan | WakeOutcome::SendFailed => (
                    crate::shell::i18n::t_args("device-wake-failed", &[("device", device.into())]),
                    crate::shell::i18n::t("device-wake-failed-body"),
                ),
            };
            self.notification_bridge
                .push_incoming(crate::notifications::IncomingNotification {
                    dbus_id: self.notification_bridge.alloc_id(),
                    app_name: crate::shell::i18n::t("device-wake-app-name"),
                    summary,
                    body,
                    actions: Vec::new(),
                    expire_timeout: -1,
                    urgency: 1,
                    caller: None,
                    badge_count: None,
                    contact_endpoint: None,
                    sound: None,
                });
        }
    }

    fn current_policy(&mut self, now: Instant) -> ContinuityPolicySnapshot {
        if self.policy_cache.is_some() && self.policy_cache_fresh(now) {
            return self.policy_cache.unwrap_or_default();
        }
        self.last_policy_store_check = Some(now);
        let stamp = settings_store::settings_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok());
        if self.policy_cache.is_none() || stamp != self.policy_store_stamp {
            self.policy_cache = Some(ContinuityPolicySnapshot::load());
            self.policy_store_stamp = stamp;
        }
        self.policy_cache.unwrap_or_default()
    }

    fn policy_cache_fresh(&self, now: Instant) -> bool {
        self.last_policy_store_check.is_some_and(|checked| {
            now.saturating_duration_since(checked) < Self::POLICY_STORE_POLL_INTERVAL
        })
    }

    pub fn set_realtime_adapter(
        &mut self,
        adapter: crate::shell::realtime_session::SharedRealtimeFabricAdapter,
    ) {
        self.realtime_adapter = Some(adapter);
    }

    pub(crate) fn set_remote_capture_runtime(
        &mut self,
        runtime: crate::shell::remote_capture::runtime::SharedRemoteCaptureRuntime,
    ) {
        self.remote_capture.install(runtime);
    }

    /// Supply the compositor's current locally verified app focus. Remote
    /// claims are never allowed to establish this authority.
    pub fn set_local_app_focus(
        &mut self,
        focus: Option<crate::shell::physical_gamepad::PhysicalGamepadBinding>,
    ) {
        self.remote_capture
            .note_focus_change(self.local_app_focus.as_ref(), focus.as_ref());
        self.local_app_focus = focus;
    }

    /// Sample local state and project authenticated remote state into the OS.
    #[expect(
        clippy::too_many_lines,
        reason = "the per-frame pump keeps its policy-ordered subsystem steps in one sequence"
    )]
    pub fn pump(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        local: LocalFabricCapabilities,
    ) {
        let changes = self.display_refresh.take_changes(local);
        if changes.resample {
            self.last_sample = None;
        }
        if changes.policy {
            // Keep the last enforced policy: teardown compares against it.
            self.policy_cache = None;
            self.last_policy_store_check = None;
        }
        let policy = self.current_policy(Instant::now());
        self.enforce_policy(network, selection, policy);
        // Always drain Bluetooth worker completions. Policy teardown removes
        // transaction authority immediately, but an already-running BlueZ
        // operation must still observe stale completion and roll hardware back.
        pump_bluetooth_handoffs(network);
        if policy.permits(ContinuityCapability::Media) {
            self.prune_media_routes(network);
            pump_media_handoff_statuses(network);
            self.expire_incoming_media(network);
        }
        if policy.permits(ContinuityCapability::TypedGamepad)
            || !self.inbound_gamepads.is_empty()
            || outbound_gamepad_state()
                .lock()
                .is_ok_and(|state| !state.sessions.is_empty())
            || gamepad_authorization_state()
                .lock()
                .is_ok_and(|state| !state.pending.is_empty() || !state.decisions.is_empty())
        {
            self.maintain_gamepad_forwarding(network, policy);
        }
        self.handle_fabric_actions(network, policy);
        self.tethering.pump(
            network,
            policy.permits(ContinuityCapability::WifiSettings),
            unix_now_ms(),
        );
        pump_device_health(network, policy);
        crate::shell::device_wake::pump(network);
        self.notify_wake_outcomes();
        pump_fabric_clock(network, policy);
        if policy.permits(ContinuityCapability::Media) {
            self.remote_capture.pump(network, policy);
        }
        crate::shell::remote_audio::pump(
            network,
            policy.permits(ContinuityCapability::Media),
            self.local_app_focus.as_ref(),
        );
        crate::shell::speaker_group_runtime::pump(
            network,
            policy.permits(ContinuityCapability::Media),
        );
        self.pump_realtime_continuity(network, policy);
        self.pump_notification_continuity(network, policy);
        resume_nearby_transfers(network, policy);
        if policy.permits(ContinuityCapability::Media) {
            self.retry_waiting_media(network);
        }
        spatial_withdrawal::flush(network);
        if policy.permits(ContinuityCapability::DisplayInput) {
            flush_spatial_input(network);
        }
        if policy.permits(ContinuityCapability::Media) {
            self.acknowledge_opened_media(network);
        }
        let now = Instant::now();
        if self
            .last_sample
            .is_some_and(|sample| now.duration_since(sample) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.last_sample = Some(now);
        if policy.permits(ContinuityCapability::Media) {
            crate::shell::mpris::refresh_handoff_reports();
        } else {
            crate::shell::mpris::withdraw_handoff_reports();
        }
        let Some((device, label)) = network.local_device_fabric_identity() else {
            publish_device_displays(network, policy);
            return;
        };
        if !self.settings_origin_initialized {
            self.seed_remote_setting_origins(network, &device);
            self.settings_origin_initialized = true;
        }
        self.counter = self.counter.max(network.device_fabric().max_counter());
        let previous = network.device_fabric().snapshots().get(&device).cloned();
        let tentative_stamp = FabricStamp {
            counter: self.counter.saturating_add(1).max(1),
            device: device.clone(),
        };
        let mut local = local;
        if let Ok(mut spatial) = spatial_input_state().lock() {
            local.display = spatial.placed_local_rect(
                local.display,
                previous
                    .as_ref()
                    .and_then(|snapshot| snapshot.display.as_ref()),
            );
        }
        let mut candidate = build_local_snapshot(
            &device,
            label,
            local,
            selection,
            previous.as_ref(),
            &self.applied_remote_settings,
            &tentative_stamp,
            policy,
        );
        let hardware = policy
            .permits(ContinuityCapability::DisplayInput)
            .then(settings_store::hardware::DisplayHardwareSnapshot::load)
            .flatten();
        let rich_outputs = hardware.as_ref().and_then(native_display_outputs);
        if let Some(display) = candidate.display.as_mut() {
            display.outputs = hardware
                .as_ref()
                .map(native_display_connectors)
                .unwrap_or_default();
        }
        candidate.presence.wake_routes = crate::shell::device_wake::local_routes(
            previous
                .as_ref()
                .map(|snapshot| snapshot.presence.wake_routes.as_slice())
                .unwrap_or_default(),
        );
        // Connectors are populated after build_local_snapshot; preserve their
        // exact layout stamp only once the complete observation is available.
        preserve_unchanged_stamps(&mut candidate, previous.as_ref());
        if let (Some(display), Some(outputs)) = (&mut candidate.display, &rich_outputs) {
            refresh_display_observation_revision(
                display,
                &network.device_fabric(),
                outputs,
                &tentative_stamp,
            );
        }
        let fingerprint = local_observation_fingerprint(&candidate, rich_outputs.as_deref());
        let heartbeat_due = self
            .last_publish
            .is_none_or(|published| now.duration_since(published) >= PRESENCE_HEARTBEAT);
        if fingerprint != self.local_fingerprint || heartbeat_due {
            self.counter = tentative_stamp.counter;
            if fingerprint == self.local_fingerprint {
                if let Some(mut heartbeat) = previous {
                    heartbeat.presence.stamp = tentative_stamp;
                    heartbeat.presence.last_seen_unix_ms = unix_now_ms();
                    candidate = heartbeat;
                }
            } else {
                candidate.presence.stamp = tentative_stamp;
                candidate.presence.last_seen_unix_ms = unix_now_ms();
            }
            if network.upsert_local_device_fabric(candidate).is_ok() {
                self.local_fingerprint = fingerprint;
                self.last_publish = Some(now);
            }
        }
        if let Some(outputs) = rich_outputs
            && let Some(stamp) = network
                .device_fabric()
                .snapshots()
                .get(&device)
                .and_then(|snapshot| snapshot.display.as_ref())
                .map(|display| display.stamp.clone())
            && let Err(error) = network.publish_local_display_outputs(&stamp, outputs)
        {
            tracing::debug!(%error, "could not publish rich display observations");
        }
        self.merge_remote_state(network, selection, &device, policy);
        publish_device_displays(network, policy);
        crate::shell::device_unlock_runtime::refresh(network);
        if policy.permits(ContinuityCapability::WifiSettings)
            && let Err(error) = network.refresh_wifi_credential_recipients()
        {
            tracing::warn!(%error, "could not refresh per-device Wi-Fi credential envelopes");
        }
    }

    #[cfg(test)]
    pub(super) fn enforce_policy_for_test(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        policy: ContinuityPolicySnapshot,
    ) {
        self.enforce_policy(network, selection, policy);
    }

    fn enforce_policy(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        policy: ContinuityPolicySnapshot,
    ) {
        let previous = self.policy.replace(policy);
        let newly_disabled = |capability| {
            previous.map_or_else(
                || !policy.permits(capability),
                |old| policy.revoked_since(old, capability),
            )
        };

        if newly_disabled(ContinuityCapability::Clipboard) {
            clear_shared_clipboard_history(selection);
        }

        if newly_disabled(ContinuityCapability::Notifications) {
            let _ = self
                .notification_bridge
                .retain_remote_continuity_sources(&HashSet::new());
            let _ = self.notification_bridge.drain_continuity_outbound();
            let _ = self.notification_bridge.drain_continuity_command_egress();
        }
        if newly_disabled(ContinuityCapability::RealtimeSessions) {
            if let Some(adapter) = self.realtime_adapter.as_ref() {
                for peer in &self.realtime_online_peers {
                    adapter.peer_offline(peer, Instant::now());
                }
                adapter.tick(Instant::now());
                // Cancellation is teardown, not a new projection, so deliver
                // the adapter's terminal signals before suppressing the plane.
                for egress in adapter.drain_egress() {
                    if let crate::shell::realtime_session::RealtimeFabricSignal::Cancel(
                        session_id,
                    ) = egress.signal
                    {
                        let _ = network.send_personal_fabric_action(
                            &egress.target,
                            PersonalFabricAction::RealtimeCancel { session_id },
                        );
                    }
                }
            }
            self.realtime_online_peers.clear();
        }
        if newly_disabled(ContinuityCapability::DisplayInput) {
            tear_down_spatial_input(network);
            if let Ok(mut state) = device_health_state().lock() {
                state.tracker.clear();
                state.last_probe = None;
            }
        }
        if newly_disabled(ContinuityCapability::FileTransfer) {
            tear_down_nearby_transfers(network);
        }
        if newly_disabled(ContinuityCapability::Media) {
            with_fabric_clock(|core| core.set_enabled(false, Instant::now()));
            self.tear_down_incoming_media(network, MediaHandoffFailure::Cancelled);
            tear_down_media_handoffs(network);
            tear_down_bluetooth_handoffs(network);
            self.remote_capture.request_teardown_all();
        }
    }

    fn seed_remote_setting_origins(
        &mut self,
        network: &ShellNetwork,
        local_device: &crate::shell::personal_continuity::DeviceId,
    ) {
        let current = roaming_setting_values();
        self.applied_remote_settings = network
            .device_fabric()
            .merged_setting_records()
            .into_iter()
            .filter(|(key, item)| {
                item.stamp.device != *local_device && current.get(key) == Some(&item.value)
            })
            .map(|(key, item)| (key, item.value))
            .collect();
    }

    fn merge_remote_state(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        local_device: &crate::shell::personal_continuity::DeviceId,
        policy: ContinuityPolicySnapshot,
    ) {
        let fabric = network.device_fabric();
        if let Err(error) = publish_roaming_provenance(
            &fabric,
            policy,
            &mut self.roaming_provenance_fingerprint,
            None,
        ) {
            tracing::warn!(%error, "could not publish roaming settings provenance");
        }
        if policy.permits(ContinuityCapability::Clipboard) {
            let history = fabric.merged_clipboard();
            let entries = history.entries().cloned().collect::<Vec<_>>();
            merge_shared_clipboard_history(selection, &entries);
        }

        let records: BTreeMap<_, _> = fabric
            .merged_setting_records()
            .into_iter()
            .filter(|(key, _)| permits_roaming_setting(policy, key))
            .collect();
        let fingerprint = serde_json::to_vec(&records)
            .ok()
            .map(|bytes| *blake3::hash(&bytes).as_bytes());
        if fingerprint != self.merged_settings_fingerprint {
            let merged = records
                .iter()
                .map(|(key, item)| (key.clone(), item.value.clone()))
                .collect();
            apply_roaming_settings(&merged);
            self.merged_settings_fingerprint = fingerprint;
        }
        self.applied_remote_settings = records
            .into_iter()
            .filter(|(_, item)| item.stamp.device != *local_device)
            .map(|(key, item)| (key, item.value))
            .collect();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each received action is admitted by route and policy once, then offered to its handlers in one priority order"
    )]
    fn handle_fabric_actions(&mut self, network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
        prune_spatial_leases(network);
        self.remote_capture.drain_pending_actions(network);
        for received in network.take_personal_fabric_actions() {
            let crate::shell::network::ReceivedPersonalFabricAction {
                envelope,
                route,
                received_monotonic_us,
            } = received;
            if !network.personal_route_is_current(&route)
                || !policy.permits_action(&envelope.action)
            {
                continue;
            }
            if crate::shell::remote_audio::handle_action(network, &envelope) {
                continue;
            }
            if crate::shell::speaker_group_runtime::handle_action(network, &envelope) {
                continue;
            }
            if handle_device_health_action(network, &envelope) {
                continue;
            }
            if crate::shell::device_wake::handle_action(network, &envelope) {
                continue;
            }
            if handle_fabric_clock_action(network, &envelope, received_monotonic_us) {
                continue;
            }
            if self
                .tethering
                .handle_action(network, &envelope, unix_now_ms())
            {
                continue;
            }
            if remote_capture::RemoteCaptureIntegration::handles(&envelope.action) {
                self.remote_capture.handle_action(
                    crate::shell::network::ReceivedPersonalFabricAction {
                        envelope,
                        route,
                        received_monotonic_us,
                    },
                );
                continue;
            }
            if self.handle_gamepad_action(network, &envelope) {
                continue;
            }
            if self.handle_notification_action(network, &envelope) {
                continue;
            }
            if self.handle_realtime_action(&envelope) {
                continue;
            }
            match envelope.action {
                PersonalFabricAction::ResumeMedia {
                    session_id,
                    app_id,
                    workspace_id,
                    title,
                    locator,
                    position_ms,
                    resume_playing,
                    has_video,
                } => {
                    let request = PendingIncomingMedia {
                        received: Instant::now(),
                        request_id: envelope.request_id,
                        source: envelope.source,
                        session_id,
                        app_id,
                        workspace_id,
                        title,
                        locator,
                        position_ms,
                        resume_playing,
                        has_video,
                        route,
                    };
                    // A local file arrives as a media-handoff transfer. This
                    // handoff, not a Files consent, admits exactly the object
                    // its locator names, and only while it can wait for it.
                    let admits_transfer =
                        if let MediaSourceLocator::PersonalTransfer { transfer_id, .. } =
                            &request.locator
                        {
                            let room =
                                self.waiting_incoming_media.len() < MAX_PENDING_INCOMING_MEDIA;
                            if room && let Ok(mut state) = nearby_drop_state().lock() {
                                state.authorize_media(request.source.clone(), *transfer_id);
                            }
                            room
                        } else {
                            false
                        };
                    match self.try_launch_incoming_media(network, &request) {
                        IncomingMediaLaunch::Waiting if admits_transfer => {
                            self.waiting_incoming_media
                                .insert(request.request_id, request);
                        }
                        IncomingMediaLaunch::Waiting => Self::reject_incoming_media(
                            network,
                            &request,
                            MediaHandoffFailure::DestinationUnavailable,
                        ),
                        IncomingMediaLaunch::Rejected(failure) => {
                            Self::reject_incoming_media(network, &request, failure);
                        }
                        IncomingMediaLaunch::Launched => {}
                    }
                }
                PersonalFabricAction::MediaAccepted {
                    request_id,
                    session_id,
                } => {
                    let expected_target =
                        media_handoff_statuses().lock().ok().and_then(|statuses| {
                            statuses.get(&request_id).map(|status| {
                                status.target == envelope.source
                                    && status.route == route
                                    && status.phase == MediaHandoffPhase::Pending
                            })
                        });
                    if expected_target != Some(true) {
                        continue;
                    }
                    let Some(Some(report)) = network.with_current_personal_route(&route, || {
                        media::handoff::accept_outbound_handoff(request_id, &session_id)
                    }) else {
                        continue;
                    };
                    if let Some(controller) = report.controller_bus_name {
                        let _ = crate::shell::mpris::pause_named(&controller);
                    }
                    finish_media_handoff_status(request_id, MediaHandoffPhase::Completed);
                }
                PersonalFabricAction::MediaRejected {
                    request_id,
                    session_id,
                    failure,
                } => {
                    let rejected = media_handoff_statuses().lock().ok().and_then(|statuses| {
                        statuses
                            .get(&request_id)
                            .filter(|status| {
                                status.target == envelope.source
                                    && status.route == route
                                    && status.session_id == session_id
                                    && status.phase == MediaHandoffPhase::Pending
                            })
                            .cloned()
                    });
                    if let Some(status) = rejected {
                        cancel_status_personal_transfer(network, &status);
                        if media::handoff::cancel_outbound_handoff(request_id, &session_id)
                            .is_some()
                        {
                            finish_media_handoff_status(
                                request_id,
                                MediaHandoffPhase::Failed(failure),
                            );
                        }
                    }
                }
                PersonalFabricAction::MediaCancelled {
                    request_id,
                    session_id,
                } => {
                    let cancelled = self.take_incoming_media(|pending| {
                        pending.request_id == request_id
                            && pending.source == envelope.source
                            && pending.session_id == session_id
                            && pending.route == route
                    });
                    Self::terminate_incoming_media(network, cancelled, None);
                }
                action @ (PersonalFabricAction::NearbyDrop { .. }
                | PersonalFabricAction::NearbyDropResult { .. }
                | PersonalFabricAction::NearbyTransferOffer { .. }
                | PersonalFabricAction::NearbyTransferReady { .. }
                | PersonalFabricAction::NearbyTransferCancel { .. }
                | PersonalFabricAction::NearbyTransferRestart { .. }
                | PersonalFabricAction::NearbyTransferResult { .. }) => {
                    handle_nearby_drop_action(
                        network,
                        envelope.request_id,
                        envelope.source,
                        action,
                    );
                }
                action @ (PersonalFabricAction::BluetoothHandoffPrepare { .. }
                | PersonalFabricAction::BluetoothHandoffPrepared { .. }
                | PersonalFabricAction::BluetoothHandoffCommit { .. }
                | PersonalFabricAction::BluetoothHandoffFinished { .. }
                | PersonalFabricAction::BluetoothHandoffAccepted { .. }
                | PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
                | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }
                | PersonalFabricAction::BluetoothHandoffCancel { .. }) => {
                    handle_bluetooth_handoff_action(
                        network,
                        envelope.request_id,
                        &envelope.source,
                        action,
                    );
                }
                PersonalFabricAction::WindowViews { views, decode } => {
                    window_views::record(envelope.source, views, &decode);
                }
                action => handle_spatial_action(network, &route, &envelope.target, action),
            }
        }
    }

    fn handle_gamepad_action(
        &mut self,
        network: &ShellNetwork,
        envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::GamepadForwardRequest { .. } => {
                self.queue_gamepad_request(network, envelope);
            }
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id,
                accepted,
                lease_nonce,
            } => {
                if let Ok(mut state) = outbound_gamepad_state().lock()
                    && let Some(session) = state.sessions.get_mut(forwarding_id)
                    && session.target == envelope.source
                {
                    if *accepted {
                        session.lease_nonce = Some(*lease_nonce);
                        session.sequence = 0;
                    } else {
                        state.sessions.remove(forwarding_id);
                    }
                }
            }
            PersonalFabricAction::GamepadForwardEvent {
                forwarding_id,
                lease_nonce,
                sequence,
                event,
            } => {
                let local_focus = self.local_app_focus.as_ref();
                let focus_valid = self
                    .inbound_gamepads
                    .get(forwarding_id)
                    .is_none_or(|session| {
                        identity_matches_local_focus(&session.identity, local_focus)
                    });
                let result = self
                    .inbound_gamepads
                    .get(forwarding_id)
                    .filter(|session| {
                        session.source == envelope.source
                            && session.lease_nonce == *lease_nonce
                            && identity_matches_local_focus(&session.identity, local_focus)
                    })
                    .map(|session| {
                        self.gamepad_broker.submit(
                            session.lease,
                            &session.identity,
                            *sequence,
                            *event,
                            Instant::now(),
                        )
                    });
                if !focus_valid
                    || matches!(
                        result,
                        Some(Err(os_actions::PeripheralBrokerError::InvalidLease))
                    )
                {
                    self.remove_inbound_gamepad(*forwarding_id);
                }
            }
            PersonalFabricAction::GamepadForwardStop {
                forwarding_id,
                lease_nonce,
            } => {
                let inbound = self
                    .inbound_gamepads
                    .get(forwarding_id)
                    .is_some_and(|session| {
                        session.source == envelope.source && session.lease_nonce == *lease_nonce
                    });
                if inbound {
                    self.remove_inbound_gamepad(*forwarding_id);
                }
                if let Ok(mut state) = outbound_gamepad_state().lock()
                    && state.sessions.get(forwarding_id).is_some_and(|session| {
                        session.target == envelope.source
                            && session.lease_nonce == Some(*lease_nonce)
                    })
                {
                    state.sessions.remove(forwarding_id);
                }
            }
            _ => return false,
        }
        true
    }

    /// Queue the `GamepadForwardRequest` that `envelope` carries for the
    /// user's decision, rejecting it when it cannot be queued.
    fn queue_gamepad_request(
        &self,
        network: &ShellNetwork,
        envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    ) {
        let PersonalFabricAction::GamepadForwardRequest {
            forwarding_id,
            app_id,
            workspace_id,
            session_id,
            label,
        } = &envelope.action
        else {
            return;
        };
        let forwarding_id = *forwarding_id;
        let source = &envelope.source;
        let reject = PersonalFabricAction::GamepadForwardGrant {
            forwarding_id,
            accepted: false,
            lease_nonce: [0; 16],
        };
        let Ok(mut authorizations) = gamepad_authorization_state().lock() else {
            let _ = network.send_personal_fabric_action(source, reject);
            return;
        };
        let request = PendingGamepadAuthorization {
            source: source.clone(),
            target: envelope.target.clone(),
            app_id: app_id.clone(),
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            label: label.clone(),
            received: Instant::now(),
        };
        if !authorizations.queue(
            forwarding_id,
            request,
            self.inbound_gamepads.contains_key(&forwarding_id),
            self.inbound_gamepads.len(),
        ) {
            let _ = network.send_personal_fabric_action(source, reject);
        }
    }

    fn resolve_gamepad_authorizations(&mut self, network: &ShellNetwork) {
        let decisions = gamepad_authorization_state().lock().map_or_else(
            |_| Vec::new(),
            |mut state| state.decisions.drain(..).collect::<Vec<_>>(),
        );
        for (forwarding_id, approved) in decisions {
            let pending = gamepad_authorization_state()
                .lock()
                .ok()
                .and_then(|mut state| state.pending.remove(&forwarding_id));
            let Some(pending) = pending else {
                continue;
            };
            if !approved {
                let _ = network.send_personal_fabric_action(
                    &pending.source,
                    PersonalFabricAction::GamepadForwardGrant {
                        forwarding_id,
                        accepted: false,
                        lease_nonce: [0; 16],
                    },
                );
                continue;
            }
            if !pending.matches_local_focus(self.local_app_focus.as_ref()) {
                let _ = network.send_personal_fabric_action(
                    &pending.source,
                    PersonalFabricAction::GamepadForwardGrant {
                        forwarding_id,
                        accepted: false,
                        lease_nonce: [0; 16],
                    },
                );
                continue;
            }
            let source_online = network
                .device_fabric()
                .snapshots()
                .get(&pending.source)
                .is_some_and(|snapshot| snapshot.presence.online);
            if !source_online {
                continue;
            }
            self.grant_gamepad_request(network, forwarding_id, &pending);
        }
    }

    fn grant_gamepad_request(
        &mut self,
        network: &ShellNetwork,
        forwarding_id: [u8; 16],
        pending: &PendingGamepadAuthorization,
    ) {
        let reject = || PersonalFabricAction::GamepadForwardGrant {
            forwarding_id,
            accepted: false,
            lease_nonce: [0; 16],
        };
        // The remote claim has already been compared byte-for-byte with the
        // compositor's current local app/workspace/session focus. Only that
        // locally verified binding may become broker focus.
        let Ok(identity) = os_actions::ForwardedPeripheralIdentity::new_after_verification(
            pending.source.as_str(),
            pending.target.as_str(),
            &pending.app_id,
            &pending.workspace_id,
            &pending.session_id,
        ) else {
            let _ = network.send_personal_fabric_action(&pending.source, reject());
            return;
        };
        self.gamepad_broker.set_focus(Some(identity.clone()));
        let Ok(device) = self.gamepad_broker.register_gamepad(&pending.label) else {
            let _ = network.send_personal_fabric_action(&pending.source, reject());
            return;
        };
        let now = Instant::now();
        let lease = self
            .gamepad_broker
            .authorize(&identity, device, now)
            .and_then(|proof| self.gamepad_broker.acquire(proof, &identity, device, now));
        let Ok(lease) = lease else {
            self.gamepad_broker.remove_device(device);
            let _ = network.send_personal_fabric_action(&pending.source, reject());
            return;
        };
        let Some(lease_nonce) = secure_nonce() else {
            self.gamepad_broker.revoke(lease);
            self.gamepad_broker.remove_device(device);
            let _ = network.send_personal_fabric_action(&pending.source, reject());
            return;
        };
        if let Some(previous) = self.inbound_gamepads.insert(
            forwarding_id,
            InboundGamepadForwarding {
                source: pending.source.clone(),
                lease_nonce,
                identity,
                device,
                lease,
            },
        ) {
            self.gamepad_broker.revoke(previous.lease);
            self.gamepad_broker.remove_device(previous.device);
        }
        let delivery = network.send_personal_fabric_action(
            &pending.source,
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id,
                accepted: true,
                lease_nonce,
            },
        );
        let broker = &mut self.gamepad_broker;
        finish_gamepad_grant_delivery(
            &mut self.inbound_gamepads,
            forwarding_id,
            &delivery,
            |session| {
                broker.revoke(session.lease);
                broker.remove_device(session.device);
            },
        );
        self.restore_inbound_gamepad_focus();
    }

    fn remove_inbound_gamepad(&mut self, forwarding_id: [u8; 16]) {
        if let Some(session) = self.inbound_gamepads.remove(&forwarding_id) {
            self.gamepad_broker.revoke(session.lease);
            self.gamepad_broker.remove_device(session.device);
        }
        self.restore_inbound_gamepad_focus();
    }

    fn restore_inbound_gamepad_focus(&mut self) {
        self.gamepad_broker.set_focus(
            self.inbound_gamepads
                .values()
                .next()
                .map(|session| session.identity.clone()),
        );
    }

    fn maintain_gamepad_forwarding(
        &mut self,
        network: &ShellNetwork,
        policy: ContinuityPolicySnapshot,
    ) {
        if policy.permits(ContinuityCapability::TypedGamepad) {
            self.resolve_gamepad_authorizations(network);
        } else if let Ok(mut state) = gamepad_authorization_state().lock() {
            state.pending.clear();
            state.decisions.clear();
        }
        self.gamepad_broker.reap_expired(Instant::now());
        let online = network.online_device_ids();
        let expired_pending = gamepad_authorization_state().lock().map_or_else(
            |_| Vec::new(),
            |mut state| {
                let expired = state
                    .pending
                    .iter()
                    .filter_map(|(id, request)| {
                        (!online.contains(&request.source)
                            || request.received.elapsed() > os_actions::AUTHORIZATION_TTL)
                            .then_some((*id, request.source.clone()))
                    })
                    .collect::<Vec<_>>();
                for (id, _) in &expired {
                    state.pending.remove(id);
                }
                expired
            },
        );
        for (forwarding_id, source) in expired_pending {
            let _ = network.send_personal_fabric_action(
                &source,
                PersonalFabricAction::GamepadForwardGrant {
                    forwarding_id,
                    accepted: false,
                    lease_nonce: [0; 16],
                },
            );
        }
        let ended = self
            .inbound_gamepads
            .iter()
            .filter_map(|(id, session)| {
                (!policy.permits(ContinuityCapability::TypedGamepad)
                    || !online.contains(&session.source)
                    || !identity_matches_local_focus(
                        &session.identity,
                        self.local_app_focus.as_ref(),
                    )
                    || !self.gamepad_broker.has_lease(session.lease))
                .then_some((*id, session.source.clone(), session.lease_nonce))
            })
            .collect::<Vec<_>>();
        for (forwarding_id, source, lease_nonce) in ended {
            self.remove_inbound_gamepad(forwarding_id);
            let _ = network.send_personal_fabric_action(
                &source,
                PersonalFabricAction::GamepadForwardStop {
                    forwarding_id,
                    lease_nonce,
                },
            );
        }
        if let Ok(mut state) = outbound_gamepad_state().lock() {
            let ended = state
                .sessions
                .iter()
                .filter_map(|(id, session)| {
                    (!policy.permits(ContinuityCapability::TypedGamepad)
                        || !online.contains(&session.target)
                        || (session.lease_nonce.is_none()
                            && session.requested.elapsed() > os_actions::AUTHORIZATION_TTL))
                        .then_some((*id, session.clone()))
                })
                .collect::<Vec<_>>();
            for (forwarding_id, session) in ended {
                state.sessions.remove(&forwarding_id);
                if let Some(lease_nonce) = session.lease_nonce {
                    let _ = network.send_personal_fabric_action(
                        &session.target,
                        PersonalFabricAction::GamepadForwardStop {
                            forwarding_id,
                            lease_nonce,
                        },
                    );
                }
            }
        }
    }

    fn handle_realtime_action(
        &self,
        envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    ) -> bool {
        use crate::shell::realtime_session::RealtimeFabricSignal;

        let signal = match &envelope.action {
            PersonalFabricAction::RealtimeOffer { offer } => {
                RealtimeFabricSignal::Offer(offer.clone())
            }
            PersonalFabricAction::RealtimeAccept { session_id } => {
                RealtimeFabricSignal::Accept(*session_id)
            }
            PersonalFabricAction::RealtimeMediaReady { session_id } => {
                RealtimeFabricSignal::MediaReady(*session_id)
            }
            PersonalFabricAction::RealtimeStopSource { session_id } => {
                RealtimeFabricSignal::StopSource(*session_id)
            }
            PersonalFabricAction::RealtimeCancel { session_id } => {
                RealtimeFabricSignal::Cancel(*session_id)
            }
            _ => return false,
        };
        if let Some(adapter) = self.realtime_adapter.as_ref() {
            let _ = adapter.receive_authenticated(
                &envelope.source,
                &envelope.target,
                signal,
                Instant::now(),
                unix_now_ms(),
            );
        }
        true
    }

    fn pump_realtime_continuity(
        &mut self,
        network: &ShellNetwork,
        policy: ContinuityPolicySnapshot,
    ) {
        use crate::shell::realtime_session::RealtimeFabricSignal;

        if !policy.permits(ContinuityCapability::RealtimeSessions) {
            if let Some(adapter) = self.realtime_adapter.as_ref() {
                let peers = network
                    .with_device_fabric(|fabric| {
                        fabric.snapshots().keys().cloned().collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for peer in peers {
                    adapter.peer_offline(&peer, Instant::now());
                }
                adapter.tick(Instant::now());
                let _ = adapter.drain_egress();
            }
            return;
        }
        let Some(adapter) = self.realtime_adapter.as_ref() else {
            return;
        };
        let online = network.online_device_ids();
        for peer in self.realtime_online_peers.difference(&online) {
            adapter.peer_offline(peer, Instant::now());
        }
        self.realtime_online_peers = online;
        adapter.tick(Instant::now());
        for egress in adapter.drain_egress() {
            let action = match egress.signal {
                RealtimeFabricSignal::Offer(offer) => PersonalFabricAction::RealtimeOffer { offer },
                RealtimeFabricSignal::Accept(session_id) => {
                    PersonalFabricAction::RealtimeAccept { session_id }
                }
                RealtimeFabricSignal::MediaReady(session_id) => {
                    PersonalFabricAction::RealtimeMediaReady { session_id }
                }
                RealtimeFabricSignal::StopSource(session_id) => {
                    PersonalFabricAction::RealtimeStopSource { session_id }
                }
                RealtimeFabricSignal::Cancel(session_id) => {
                    PersonalFabricAction::RealtimeCancel { session_id }
                }
            };
            let _ = network.send_personal_fabric_action(&egress.target, action);
        }
    }

    fn handle_notification_action(
        &self,
        network: &ShellNetwork,
        envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::NotificationProject { notification } => {
                let source_label = {
                    let fabric = network.device_fabric();
                    let Some(source) = fabric
                        .snapshots()
                        .get(&envelope.source)
                        .filter(|snapshot| snapshot.presence.online)
                    else {
                        return true;
                    };
                    source.presence.label.clone()
                };
                let _ = self.notification_bridge.receive_continuity_projection(
                    envelope.source.as_str(),
                    &source_label,
                    notification.clone(),
                    unix_now_ms(),
                );
            }
            PersonalFabricAction::NotificationRetire { id } => {
                let _ = self
                    .notification_bridge
                    .retire_remote_continuity(envelope.source.as_str(), *id);
            }
            PersonalFabricAction::NotificationDismiss { id } => {
                let _ = self.notification_bridge.apply_continuity_command(
                    *id,
                    &ContinuityNotificationCommand::Dismiss,
                    unix_now_ms(),
                );
            }
            PersonalFabricAction::NotificationInvoke { id, action_key } => {
                let _ = self.notification_bridge.apply_continuity_command(
                    *id,
                    &ContinuityNotificationCommand::Invoke {
                        action_key: action_key.clone(),
                    },
                    unix_now_ms(),
                );
            }
            _ => return false,
        }
        true
    }

    fn pump_notification_continuity(
        &self,
        network: &ShellNetwork,
        policy: ContinuityPolicySnapshot,
    ) {
        if !policy.permits(ContinuityCapability::Notifications) {
            let _ = self
                .notification_bridge
                .retain_remote_continuity_sources(&HashSet::new());
            let _ = self.notification_bridge.drain_continuity_outbound();
            let _ = self.notification_bridge.drain_continuity_command_egress();
            return;
        }
        let Some((local, _)) = network.local_device_fabric_identity() else {
            let _ = self
                .notification_bridge
                .retain_remote_continuity_sources(&HashSet::new());
            return;
        };
        let peers = network
            .online_device_ids()
            .into_iter()
            .filter(|peer| *peer != local)
            .collect::<Vec<_>>();
        let online_sources = peers
            .iter()
            .map(|peer| peer.as_str().to_string())
            .collect::<HashSet<_>>();
        let _ = self
            .notification_bridge
            .retain_remote_continuity_sources(&online_sources);
        for event in self.notification_bridge.drain_continuity_outbound() {
            for peer in &peers {
                let action = match &event {
                    ContinuityOutboundEvent::Project(notification) => {
                        PersonalFabricAction::NotificationProject {
                            notification: notification.clone(),
                        }
                    }
                    ContinuityOutboundEvent::Retire(id) => {
                        PersonalFabricAction::NotificationRetire { id: *id }
                    }
                };
                let _ = network.send_personal_fabric_action(peer, action);
            }
        }
        for egress in self.notification_bridge.drain_continuity_command_egress() {
            let Ok(target) = crate::shell::personal_continuity::DeviceId::new(egress.target_device)
            else {
                continue;
            };
            let action = match egress.command {
                ContinuityNotificationCommand::Dismiss => {
                    PersonalFabricAction::NotificationDismiss { id: egress.id }
                }
                ContinuityNotificationCommand::Invoke { action_key } => {
                    PersonalFabricAction::NotificationInvoke {
                        id: egress.id,
                        action_key,
                    }
                }
            };
            let _ = network.send_personal_fabric_action(&target, action);
        }
    }

    fn try_launch_incoming_media(
        &mut self,
        network: &ShellNetwork,
        request: &PendingIncomingMedia,
    ) -> IncomingMediaLaunch {
        if !network.personal_route_is_current(&request.route) {
            return IncomingMediaLaunch::Rejected(MediaHandoffFailure::DestinationUnavailable);
        }
        let Some((local, _)) = network.local_device_fabric_identity() else {
            return IncomingMediaLaunch::Rejected(MediaHandoffFailure::DestinationUnavailable);
        };
        let Some(presence) = network.device_fabric().snapshots().get(&local).cloned() else {
            return IncomingMediaLaunch::Rejected(MediaHandoffFailure::DestinationUnavailable);
        };
        let capable = if request.has_video {
            presence.presence.capabilities.video_output
        } else {
            presence.presence.capabilities.audio_output
        };
        let Some(room) = crate::shell::state::RoomId::parse(&request.workspace_id) else {
            return IncomingMediaLaunch::Rejected(MediaHandoffFailure::InvalidWorkspace);
        };
        if !capable {
            return IncomingMediaLaunch::Rejected(MediaHandoffFailure::UnsupportedOutput);
        }
        let verified_path = match &request.locator {
            MediaSourceLocator::PersonalTransfer { transfer_id, .. } => {
                match cached_media_handoff(&request.source, *transfer_id) {
                    Ok(path) => Some(path),
                    Err(launch) => return launch,
                }
            }
            _ => None,
        };
        let resume_source = verified_path.as_ref().map_or_else(
            || match &request.locator {
                MediaSourceLocator::Workspace { path } => {
                    Some(crate::shell::mpris::ResumeSource::LocalFile(
                        media::Source::workspace_root().join(path),
                    ))
                }
                MediaSourceLocator::RemoteStream { url } => {
                    Some(crate::shell::mpris::ResumeSource::Uri(url.clone()))
                }
                MediaSourceLocator::PersonalTransfer { .. }
                | MediaSourceLocator::AppResource { .. } => None,
            },
            |path| Some(crate::shell::mpris::ResumeSource::LocalFile(path.clone())),
        );
        let launch = verified_path.as_ref().map_or_else(
            || {
                crate::shell::launcher::launch_media_handoff(
                    &request.app_id,
                    room,
                    &request.locator,
                    &request.title,
                    request.request_id,
                    request.position_ms,
                    request.resume_playing,
                )
            },
            |path| {
                crate::shell::launcher::launch_verified_media_handoff(
                    &request.app_id,
                    room,
                    path,
                    &request.title,
                    request.request_id,
                    request.position_ms,
                    request.resume_playing,
                )
            },
        );
        if launch.is_ok() {
            self.pending_incoming_media
                .insert(request.request_id, request.clone());
            if let Some(source) = resume_source
                && !crate::shell::mpris::resume_after_launch(crate::shell::mpris::ResumeRequest {
                    request_id: request.request_id,
                    package_id: request.app_id.clone(),
                    source,
                    position_ms: request.position_ms,
                    playing: request.resume_playing,
                })
            {
                self.pending_incoming_media.remove(&request.request_id);
                let _ = crate::shell::mpris::cancel_resume_after_launch(request.request_id);
                return IncomingMediaLaunch::Rejected(MediaHandoffFailure::OpenFailed);
            }
            IncomingMediaLaunch::Launched
        } else {
            IncomingMediaLaunch::Rejected(MediaHandoffFailure::OpenFailed)
        }
    }

    fn reject_incoming_media(
        network: &ShellNetwork,
        request: &PendingIncomingMedia,
        failure: MediaHandoffFailure,
    ) {
        Self::discard_incoming_media_transfer(request);
        let _ = network.send_personal_fabric_action_on_route(
            &request.route,
            PersonalFabricAction::MediaRejected {
                request_id: request.request_id,
                session_id: request.session_id.clone(),
                failure,
            },
        );
    }

    /// Revoke the exact sender/transfer authorisation behind one handoff and
    /// any bytes it brought. A verified object stays while a player lease or
    /// another binding still references it.
    fn discard_incoming_media_transfer(request: &PendingIncomingMedia) {
        if let MediaSourceLocator::PersonalTransfer { transfer_id, .. } = &request.locator {
            if let Ok(mut state) = nearby_drop_state().lock() {
                state
                    .media_authorized
                    .remove(&(request.source.clone(), *transfer_id));
            }
            let _ = with_open_nearby_transfer_store(|store| {
                store.release_media(request.source.as_str(), *transfer_id)
            });
        }
    }

    fn retry_waiting_media(&mut self, network: &ShellNetwork) {
        let waiting = std::mem::take(&mut self.waiting_incoming_media);
        for (request_id, request) in waiting {
            match self.try_launch_incoming_media(network, &request) {
                IncomingMediaLaunch::Waiting => {
                    self.waiting_incoming_media.insert(request_id, request);
                }
                IncomingMediaLaunch::Rejected(failure) => {
                    Self::reject_incoming_media(network, &request, failure);
                }
                IncomingMediaLaunch::Launched => {}
            }
        }
    }

    fn acknowledge_opened_media(&mut self, network: &ShellNetwork) {
        let mut accepted = media::handoff::accepted_handoff_requests();
        accepted.sort_unstable();
        accepted.dedup();
        for request_id in accepted {
            let Some(pending) = self.pending_incoming_media.remove(&request_id) else {
                continue;
            };
            Self::discard_incoming_media_transfer(&pending);
            if network
                .send_personal_fabric_action_on_route(
                    &pending.route,
                    PersonalFabricAction::MediaAccepted {
                        request_id,
                        session_id: pending.session_id,
                    },
                )
                .is_err()
            {
                let _ = crate::shell::mpris::cancel_resume_after_launch(request_id);
            }
        }
        for accepted in crate::shell::mpris::take_accepted_handoff_resumes() {
            let Some(pending) = self.pending_incoming_media.remove(&accepted.request_id) else {
                continue;
            };
            Self::discard_incoming_media_transfer(&pending);
            let delivery = network.send_personal_fabric_action_on_route(
                &pending.route,
                PersonalFabricAction::MediaAccepted {
                    request_id: accepted.request_id,
                    session_id: pending.session_id,
                },
            );
            compensate_failed_media_acceptance(
                &delivery,
                accepted.controller_bus_name.as_deref(),
                accepted.expected_uri.as_deref(),
                crate::shell::mpris::pause_named_if_uri,
            );
        }
    }

    fn take_incoming_media(
        &mut self,
        mut predicate: impl FnMut(&PendingIncomingMedia) -> bool,
    ) -> Vec<PendingIncomingMedia> {
        let waiting = self
            .waiting_incoming_media
            .iter()
            .filter(|(_, pending)| predicate(pending))
            .map(|(request_id, _)| *request_id)
            .collect::<Vec<_>>();
        let pending = self
            .pending_incoming_media
            .iter()
            .filter(|(_, pending)| predicate(pending))
            .map(|(request_id, _)| *request_id)
            .collect::<Vec<_>>();
        waiting
            .into_iter()
            .filter_map(|request_id| self.waiting_incoming_media.remove(&request_id))
            .chain(
                pending
                    .into_iter()
                    .filter_map(|request_id| self.pending_incoming_media.remove(&request_id)),
            )
            .collect()
    }

    fn terminate_incoming_media(
        network: &ShellNetwork,
        requests: Vec<PendingIncomingMedia>,
        failure: Option<MediaHandoffFailure>,
    ) {
        for request in requests {
            let _ = crate::shell::mpris::cancel_resume_after_launch(request.request_id);
            Self::discard_incoming_media_transfer(&request);
            if let Some(failure) = failure {
                Self::reject_incoming_media(network, &request, failure);
            }
        }
    }

    fn tear_down_incoming_media(&mut self, network: &ShellNetwork, failure: MediaHandoffFailure) {
        let incoming = self.take_incoming_media(|_| true);
        Self::terminate_incoming_media(network, incoming, Some(failure));
    }

    fn prune_media_routes(&mut self, network: &ShellNetwork) {
        let departed =
            self.take_incoming_media(|pending| !network.personal_route_is_current(&pending.route));
        Self::terminate_incoming_media(network, departed, None);
        if let Ok(mut statuses) = media_handoff_statuses().lock() {
            let now = Instant::now();
            for status in statuses.values_mut().filter(|status| {
                status.phase == MediaHandoffPhase::Pending
                    && !network.personal_route_is_current(&status.route)
            }) {
                let _ =
                    media::handoff::cancel_outbound_handoff(status.request_id, &status.session_id);
                cancel_status_personal_transfer(network, status);
                status.phase =
                    MediaHandoffPhase::Failed(MediaHandoffFailure::DestinationUnavailable);
                status.updated = now;
            }
        }
    }

    fn expire_incoming_media(&mut self, network: &ShellNetwork) {
        let expired =
            self.take_incoming_media(|pending| pending.received.elapsed() >= INCOMING_MEDIA_WAIT);
        Self::terminate_incoming_media(network, expired, Some(MediaHandoffFailure::TimedOut));
    }
}

#[cfg(test)]
mod policy_cache_tests {
    use super::*;

    fn policy_cache_fresh(last_check: Option<Instant>, now: Instant) -> bool {
        last_check.is_some_and(|checked| {
            now.saturating_duration_since(checked) < DeviceFabricRuntime::POLICY_STORE_POLL_INTERVAL
        })
    }

    #[test]
    fn cached_policy_avoids_reloading_within_the_frame_loop_window() {
        let now = Instant::now();
        assert!(policy_cache_fresh(
            Some(now),
            now + Duration::from_millis(749)
        ));
        assert!(!policy_cache_fresh(
            Some(now),
            now + Duration::from_millis(750)
        ));
    }
}

fn expire_nearby_consents(network: &ShellNetwork) {
    let expired = nearby_drop_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            state
                .incoming
                .extract_if(.., |_, pending| {
                    pending.received.elapsed() >= NEARBY_CONSENT_TIMEOUT
                })
                .collect::<Vec<_>>()
        },
    );
    for (transfer_id, pending) in expired {
        let action = match pending.payload {
            PendingNearbyPayload::Inline { .. } => PersonalFabricAction::NearbyDropResult {
                request_id: pending.request_id,
                file_name: pending.name,
                accepted: false,
            },
            PendingNearbyPayload::Durable { .. } => PersonalFabricAction::NearbyTransferResult {
                request_id: pending.request_id,
                transfer_id,
                accepted: false,
            },
        };
        let _ = network.send_personal_fabric_action(&pending.source, action);
    }
}

/// Drive the outgoing side of the one transfer lane: settle finished staging,
/// then offer every staged object the continuity policy still allows. A
/// media-handoff object lives exactly as long as its pending handoff.
fn resume_nearby_transfers(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    if policy.permits(ContinuityCapability::FileTransfer) {
        expire_nearby_consents(network);
    }
    settle_nearby_staging(network, policy);
    let entries = try_outbox_entries();
    if let Ok(mut state) = nearby_drop_state().lock() {
        state
            .last_offer
            .retain(|transfer_id, _| entries.iter().any(|entry| entry.id == *transfer_id));
    }
    for entry in entries {
        if !policy.permits(entry.kind.capability()) {
            continue;
        }
        let Ok(target) = crate::shell::personal_continuity::DeviceId::new(entry.target.clone())
        else {
            continue;
        };
        if entry.kind == crate::shell::nearby_transfer::TransferKind::MediaHandoff
            && !media_transfer_pending(entry.id, &target)
        {
            let _ = with_nearby_transfer_store(|store| store.cancel_outgoing(entry.id));
            continue;
        }
        let should_offer = nearby_drop_state().lock().is_ok_and(|state| {
            !state.large_offers.contains_key(&entry.id)
                && state
                    .last_offer
                    .get(&entry.id)
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(10))
        });
        if !should_offer {
            continue;
        }
        if let Ok(mut state) = nearby_drop_state().lock() {
            state.last_offer.insert(entry.id, Instant::now());
        }
        if let Ok(request) = network.send_personal_fabric_action(
            &target,
            PersonalFabricAction::NearbyTransferOffer {
                transfer_id: entry.id,
                name: entry.name.clone(),
                kind: entry.kind,
                total_bytes: entry.total_bytes,
                content_hash: entry.content_hash,
            },
        ) && let Ok(mut state) = nearby_drop_state().lock()
        {
            state
                .large_offers
                .insert(entry.id, (target, request, entry.name));
        }
    }
}

/// Collect finished staging. A Nearby send whose switch is now off is
/// dropped; a media handoff that failed to stage fails its handoff, and one
/// that is no longer pending loses its staged copy.
fn settle_nearby_staging(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    let staged = nearby_staging_queue().1.lock().map_or_else(
        |_| Vec::new(),
        |receiver| receiver.try_iter().collect::<Vec<_>>(),
    );
    for staged in staged {
        match staged.media_request {
            Some(request_id) => {
                let pending = policy.permits(ContinuityCapability::Media)
                    && media_transfer_pending(staged.transfer_id, &staged.target);
                if staged.result.is_ok() && !pending {
                    let _ = with_nearby_transfer_store(|store| {
                        store.cancel_outgoing(staged.transfer_id)
                    });
                }
                if staged.result.is_err() && pending {
                    fail_media_handoff(network, request_id, MediaHandoffFailure::OpenFailed);
                }
            }
            None if !policy.permits(ContinuityCapability::FileTransfer) => {
                if staged.result.is_ok() {
                    let _ = with_nearby_transfer_store(|store| {
                        store.cancel_outgoing(staged.transfer_id)
                    });
                }
            }
            None => {
                if staged.result.is_err()
                    && let Ok(mut state) = nearby_drop_state().lock()
                {
                    state.events.push_back(NearbyDropEvent::Failed {
                        file_name: staged.name,
                        peer: staged.target,
                    });
                }
            }
        }
    }
}

/// End one still-pending outbound media handoff on the source's own failure,
/// telling the destination it was withdrawn.
fn fail_media_handoff(network: &ShellNetwork, request_id: [u8; 16], failure: MediaHandoffFailure) {
    let status = media_handoff_statuses()
        .lock()
        .ok()
        .and_then(|statuses| statuses.get(&request_id).cloned())
        .filter(|status| status.phase == MediaHandoffPhase::Pending);
    let Some(status) = status else {
        return;
    };
    let _ = media::handoff::cancel_outbound_handoff(request_id, &status.session_id);
    cancel_status_personal_transfer(network, &status);
    finish_media_handoff_status(request_id, MediaHandoffPhase::Failed(failure));
    let _ = network.send_personal_fabric_action_on_route(
        &status.route,
        PersonalFabricAction::MediaCancelled {
            request_id,
            session_id: status.session_id,
        },
    );
}

fn prune_spatial_state(
    spatial: &mut SpatialInputState,
    now: u64,
    departures: &[crate::shell::network::PeerDeparture],
    is_online: impl Fn(&crate::shell::personal_continuity::DeviceId) -> bool,
) {
    let unseen = departures
        .iter()
        .filter(|departure| departure.generation > spatial.last_peer_departure_generation)
        .collect::<Vec<_>>();
    let history_gap = unseen.first().is_some_and(|departure| {
        departure.generation > spatial.last_peer_departure_generation.saturating_add(1)
    });
    let mut departed = unseen
        .iter()
        .filter_map(|departure| {
            // Direct personal peers use the endpoint key as DeviceId. Network
            // admission proves the device certificate key equals these
            // endpoint bytes before recording an authenticated departure, so
            // a relay/service endpoint cannot alias an enrolled controller.
            crate::shell::personal_continuity::DeviceId::new(hex::encode(
                departure.endpoint_id.as_bytes(),
            ))
            .ok()
        })
        .collect::<BTreeSet<_>>();
    if history_gap {
        // The history is intentionally bounded. If this frame missed entries,
        // fail closed rather than leave an unknown departed controller holding
        // local seat state.
        departed.extend(
            spatial
                .issued
                .values()
                .map(|lease| lease.controller.clone()),
        );
        departed.extend(spatial.received.keys().cloned());
    }
    if let Some(generation) = departures
        .last()
        .map(|departure| departure.generation)
        .filter(|generation| *generation > spatial.last_peer_departure_generation)
    {
        spatial.last_peer_departure_generation = generation;
    }

    let issued_to_release = spatial
        .issued
        .values()
        .filter(|lease| {
            lease.validate_at(now).is_err()
                || departed.contains(&lease.controller)
                || !is_online(&lease.controller)
        })
        .map(|lease| lease.controller.clone())
        .collect::<Vec<_>>();
    spatial.issued.retain(|_, lease| {
        lease.validate_at(now).is_ok()
            && !departed.contains(&lease.controller)
            && is_online(&lease.controller)
    });
    spatial
        .issued_routes
        .retain(|nonce, _| spatial.issued.contains_key(nonce));
    spatial
        .automatic_issued
        .retain(|_, nonce| spatial.issued.contains_key(nonce));
    spatial.received.retain(|target, lease| {
        lease.validate_at(now).is_ok() && !departed.contains(target) && is_online(target)
    });
    for controller in issued_to_release {
        spatial.release_controller(&controller);
    }
    let received = spatial.received.clone();
    spatial
        .received_routes
        .retain(|device, _| received.contains_key(device));
    spatial.outbound.retain(|input| {
        received
            .get(&input.target)
            .is_some_and(|lease| lease.nonce == input.lease_nonce && lease.validate_at(now).is_ok())
    });
    spatial.retain_spatial_protocol();
}

fn prune_spatial_leases(network: &ShellNetwork) {
    // Read routes before taking the state mutex; queue admission takes the
    // route registry before the state mutex and must not invert that order.
    let routes = spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |spatial| {
            spatial
                .issued_routes
                .values()
                .chain(spatial.received_routes.values())
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let withdrawn = routes
        .into_iter()
        .filter(|route| !network.personal_route_is_current(route))
        .map(|route| route.device().clone())
        .collect::<BTreeSet<_>>();
    if let Ok(mut spatial) = spatial_input_state().lock() {
        let fabric = network.device_fabric();
        prune_spatial_state(
            &mut spatial,
            unix_now_ms(),
            &network.peer_departures(),
            |device| {
                !withdrawn.contains(device)
                    && fabric
                        .snapshots()
                        .get(device)
                        .is_some_and(|snapshot| snapshot.presence.online)
            },
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive exact-peer state machine keeps inline delivery, offer, resume, restart, cancellation, and result binding auditable together"
)]
fn handle_nearby_drop_action(
    network: &ShellNetwork,
    envelope_request_id: [u8; 16],
    source: crate::shell::personal_continuity::DeviceId,
    action: PersonalFabricAction,
) {
    match action {
        PersonalFabricAction::NearbyDrop {
            file_name,
            content_hash,
            bytes,
        } => {
            let total_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let pending = PendingNearbyConsent {
                source: source.clone(),
                request_id: envelope_request_id,
                name: file_name.clone(),
                total_bytes,
                received: Instant::now(),
                payload: PendingNearbyPayload::Inline {
                    bytes,
                    content_hash,
                },
            };
            let queued = nearby_drop_state()
                .lock()
                .is_ok_and(|mut state| state.insert_consent(envelope_request_id, pending));
            if !queued {
                let _ = network.send_personal_fabric_action(
                    &source,
                    PersonalFabricAction::NearbyDropResult {
                        request_id: envelope_request_id,
                        file_name,
                        accepted: false,
                    },
                );
            }
        }
        PersonalFabricAction::NearbyDropResult {
            request_id,
            file_name,
            accepted,
        } => {
            if let Ok(mut state) = nearby_drop_state().lock()
                && state
                    .pending
                    .remove(&request_id)
                    .is_some_and(|(peer, pending_name, _)| {
                        peer == source && pending_name == file_name
                    })
            {
                state.events.push_back(if accepted {
                    NearbyDropEvent::Sent {
                        file_name,
                        peer: source,
                    }
                } else {
                    NearbyDropEvent::Failed {
                        file_name,
                        peer: source,
                    }
                });
            }
        }
        PersonalFabricAction::NearbyTransferOffer {
            transfer_id,
            name,
            kind: crate::shell::nearby_transfer::TransferKind::MediaHandoff,
            total_bytes,
            content_hash,
        } => accept_media_handoff_offer(
            network,
            &source,
            crate::shell::nearby_transfer::IncomingManifest {
                id: transfer_id,
                source: source.as_str().to_string(),
                request_id: envelope_request_id,
                name,
                kind: crate::shell::nearby_transfer::TransferKind::MediaHandoff,
                total_bytes,
                content_hash,
            },
        ),
        PersonalFabricAction::NearbyTransferOffer {
            transfer_id,
            name,
            kind,
            total_bytes,
            content_hash,
        } => {
            let pending = PendingNearbyConsent {
                source: source.clone(),
                request_id: envelope_request_id,
                name,
                total_bytes,
                received: Instant::now(),
                payload: PendingNearbyPayload::Durable { kind, content_hash },
            };
            let queued = nearby_drop_state()
                .lock()
                .is_ok_and(|mut state| state.insert_consent(transfer_id, pending));
            if !queued {
                let _ = network.send_personal_fabric_action(
                    &source,
                    PersonalFabricAction::NearbyTransferResult {
                        request_id: envelope_request_id,
                        transfer_id,
                        accepted: false,
                    },
                );
            }
        }
        PersonalFabricAction::NearbyTransferReady {
            request_id,
            transfer_id,
            committed_offset,
            prefix_hash,
        } => {
            let pending = nearby_drop_state().lock().ok().and_then(|state| {
                state.large_offers.get(&transfer_id).cloned().filter(
                    |(target, offer_request, _)| target == &source && offer_request == &request_id,
                )
            });
            let Some((target, _, _)) = pending else {
                return;
            };
            let manifest = with_nearby_transfer_store(|store| {
                let entry = store
                    .outbox()
                    .iter()
                    .find(|entry| entry.id == transfer_id && entry.target == target.as_str())
                    .cloned()
                    .ok_or_else(|| "unknown nearby transfer offer".to_string())?;
                let path = store.object_path(transfer_id);
                let local_prefix =
                    crate::shell::nearby_transfer::hash_prefix(&path, committed_offset)?;
                Ok((entry, path, local_prefix))
            });
            let Ok((entry, path, local_prefix)) = manifest else {
                return;
            };
            if !transfer_kind_permitted(entry.kind) {
                return;
            }
            if local_prefix != prefix_hash {
                let _ = network.send_personal_fabric_action(
                    &source,
                    PersonalFabricAction::NearbyTransferRestart {
                        request_id,
                        transfer_id,
                    },
                );
                return;
            }
            let _ = network.start_nearby_transfer(
                &target,
                crate::network::actor::NearbyTransferCheckpoint {
                    transfer_id,
                    request_id,
                    total_bytes: entry.total_bytes,
                    resume_offset: committed_offset,
                    prefix_hash,
                    content_hash: entry.content_hash,
                },
                &path,
            );
        }
        PersonalFabricAction::NearbyTransferRestart {
            request_id,
            transfer_id,
        } => {
            let valid = incoming_transfer_kind(&source, request_id, transfer_id)
                .is_some_and(transfer_kind_permitted);
            if valid
                && let Ok(prefix_hash) =
                    with_nearby_transfer_store(|store| store.restart_incoming(transfer_id))
            {
                let _ = network.send_personal_fabric_action(
                    &source,
                    PersonalFabricAction::NearbyTransferReady {
                        request_id,
                        transfer_id,
                        committed_offset: 0,
                        prefix_hash,
                    },
                );
            }
        }
        PersonalFabricAction::NearbyTransferCancel {
            request_id,
            transfer_id,
        } => {
            if let Ok(mut state) = nearby_drop_state().lock()
                && state.incoming.get(&transfer_id).is_some_and(|pending| {
                    pending.source == source && pending.request_id == request_id
                })
            {
                state.incoming.remove(&transfer_id);
            }
            if incoming_transfer_kind(&source, request_id, transfer_id)
                .is_some_and(transfer_kind_permitted)
            {
                let _ = with_nearby_transfer_store(|store| store.cancel_incoming(transfer_id));
                if let Ok(mut state) = nearby_drop_state().lock() {
                    state.destinations.remove(&transfer_id);
                    state
                        .media_authorized
                        .remove(&(source.clone(), transfer_id));
                }
            }
            let outgoing = transfer_kind_permitted(
                outgoing_transfer_kind(transfer_id)
                    .unwrap_or(crate::shell::nearby_transfer::TransferKind::File),
            )
            .then(|| {
                nearby_drop_state().lock().ok().and_then(|mut state| {
                    let pending = state
                        .large_offers
                        .get(&transfer_id)
                        .cloned()
                        .filter(|(target, offer, _)| target == &source && offer == &request_id);
                    if pending.is_some() {
                        state.large_offers.remove(&transfer_id);
                    }
                    pending
                })
            })
            .flatten();
            if outgoing.is_some() {
                let _ = network.cancel_nearby_transfer_stream(&source, transfer_id);
                let _ = with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
            }
        }
        PersonalFabricAction::NearbyTransferResult {
            request_id,
            transfer_id,
            accepted,
        } => {
            let kind = outgoing_transfer_kind(transfer_id)
                .unwrap_or(crate::shell::nearby_transfer::TransferKind::File);
            if !transfer_kind_permitted(kind) {
                return;
            }
            let media = kind == crate::shell::nearby_transfer::TransferKind::MediaHandoff;
            let pending = nearby_drop_state().lock().ok().and_then(|mut state| {
                let pending = state
                    .large_offers
                    .get(&transfer_id)
                    .cloned()
                    .filter(|(target, offer, _)| target == &source && offer == &request_id);
                // A refused media offer is offered again while its handoff
                // is pending: the destination may not have seen the
                // `ResumeMedia` that authorises it yet.
                if (accepted || media) && pending.is_some() {
                    state.large_offers.remove(&transfer_id);
                }
                pending
            });
            if let Some((peer, _, file_name)) = pending {
                if accepted {
                    let _ = with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
                }
                // A media handoff reports through its own status, never as
                // a Nearby send.
                if !media && let Ok(mut state) = nearby_drop_state().lock() {
                    state.events.push_back(if accepted {
                        NearbyDropEvent::Sent { file_name, peer }
                    } else {
                        NearbyDropEvent::Failed { file_name, peer }
                    });
                }
            }
        }
        _ => {}
    }
}

/// Whether the continuity switch that governs `kind` is on right now.
fn transfer_kind_permitted(kind: crate::shell::nearby_transfer::TransferKind) -> bool {
    ContinuityPolicySnapshot::load().permits(kind.capability())
}

/// The kind of the receive this exact sender and request own, if any.
fn incoming_transfer_kind(
    source: &crate::shell::personal_continuity::DeviceId,
    request_id: [u8; 16],
    transfer_id: [u8; 16],
) -> Option<crate::shell::nearby_transfer::TransferKind> {
    with_nearby_transfer_store(|store| {
        Ok(store
            .inbox()
            .iter()
            .find(|entry| {
                entry.id == transfer_id
                    && entry.source == source.as_str()
                    && entry.request_id == request_id
            })
            .map(|entry| entry.kind))
    })
    .ok()
    .flatten()
}

/// The kind of one staged outgoing object, if it is still staged.
fn outgoing_transfer_kind(
    transfer_id: [u8; 16],
) -> Option<crate::shell::nearby_transfer::TransferKind> {
    with_nearby_transfer_store(|store| {
        Ok(store
            .outbox()
            .iter()
            .find(|entry| entry.id == transfer_id)
            .map(|entry| entry.kind))
    })
    .ok()
    .flatten()
}

/// Accept a media-handoff offer that a waiting `ResumeMedia` authorised, into
/// the private media cache, resuming from any verified prefix. Every other
/// media-handoff offer is refused; none ever reaches the Files consent queue.
fn accept_media_handoff_offer(
    network: &ShellNetwork,
    source: &crate::shell::personal_continuity::DeviceId,
    manifest: crate::shell::nearby_transfer::IncomingManifest,
) {
    let transfer_id = manifest.id;
    let request_id = manifest.request_id;
    let authorized = nearby_drop_state().lock().is_ok_and(|state| {
        state
            .media_authorized
            .contains(&(source.clone(), transfer_id))
    });
    let checkpoint = authorized
        .then(|| {
            let accepted = with_nearby_transfer_store(|store| {
                store.accept_incoming_offer(manifest, unix_now_ms())
            });
            if accepted.is_err()
                && let Ok(mut state) = nearby_drop_state().lock()
            {
                // The receive cannot proceed, so the waiting handoff fails
                // now instead of waiting out its timeout.
                state
                    .media_authorized
                    .remove(&(source.clone(), transfer_id));
            }
            accepted.ok()
        })
        .flatten();
    let action = match checkpoint {
        Some((committed_offset, prefix_hash)) => PersonalFabricAction::NearbyTransferReady {
            request_id,
            transfer_id,
            committed_offset,
            prefix_hash,
        },
        None => PersonalFabricAction::NearbyTransferResult {
            request_id,
            transfer_id,
            accepted: false,
        },
    };
    let _ = network.send_personal_fabric_action(source, action);
}

fn handle_device_health_action(
    network: &ShellNetwork,
    envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
) -> bool {
    let now = Instant::now();
    match &envelope.action {
        PersonalFabricAction::DeviceHealthProbe { nonce } => {
            let reply = device_health_state().lock().ok().and_then(|mut state| {
                answer_health_probe(
                    &mut state,
                    envelope.source.clone(),
                    *nonce,
                    now,
                    sample_local_power,
                )
            });
            if let Some((peer, report)) = reply {
                let _ = network.send_personal_fabric_action(
                    &peer,
                    PersonalFabricAction::DeviceHealthReport { report },
                );
            }
            true
        }
        PersonalFabricAction::DeviceHealthReport { report } => {
            if let Ok(mut state) = device_health_state().lock() {
                let _ = state.tracker.accept_report(&envelope.source, *report, now);
            }
            true
        }
        _ => false,
    }
}

/// Answer a peer's clock probe with timestamps only, or feed a report into
/// the estimate of a peer this device holds in sync. The policy gate has
/// already admitted both under Media continuity.
fn handle_fabric_clock_action(
    network: &ShellNetwork,
    envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    received_monotonic_us: u64,
) -> bool {
    match &envelope.action {
        PersonalFabricAction::FabricClockProbe { nonce, sent_us } => {
            let reply = with_fabric_clock(|core| {
                core.answer(
                    *nonce,
                    *sent_us,
                    received_monotonic_us,
                    fabric_monotonic_us(),
                )
            });
            if let Some(reply) = reply {
                let _ = network.send_personal_fabric_action(&envelope.source, reply);
            }
            true
        }
        PersonalFabricAction::FabricClockReport { report } => {
            with_fabric_clock(|core| {
                core.accept(
                    &envelope.source,
                    *report,
                    Instant::now(),
                    received_monotonic_us,
                )
            });
            true
        }
        _ => false,
    }
}

/// Every online peer, once per health cadence.
fn health_probe_targets(
    state: &mut DeviceHealthRuntimeState,
    online: Vec<crate::shell::personal_continuity::DeviceId>,
    now: Instant,
) -> Vec<crate::shell::personal_continuity::DeviceId> {
    let due = state.last_probe.is_none_or(|sampled| {
        now.checked_duration_since(sampled)
            .is_none_or(|age| age >= crate::shell::spatial_calibration::HEALTH_PROBE_INTERVAL)
    });
    if !due {
        return Vec::new();
    }
    state.last_probe = Some(now);
    online
}

/// Probe every online peer something holds in sync, while Media continuity
/// is on. Nothing else starts the fabric clock.
fn pump_fabric_clock(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    let now = Instant::now();
    let enabled = policy.permits(ContinuityCapability::Media);
    with_fabric_clock(|core| core.set_enabled(enabled, now));
    if !enabled {
        return;
    }
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    // Read in place, every frame; released before the clock lock is taken.
    let (known, online_peers) = network
        .with_device_fabric(|fabric| {
            let snapshots = fabric.snapshots();
            let online = snapshots
                .values()
                .filter(|snapshot| snapshot.presence.online)
                .map(|snapshot| snapshot.owner.clone())
                .collect::<BTreeSet<_>>();
            (snapshots.keys().cloned().collect::<BTreeSet<_>>(), online)
        })
        .unwrap_or_default();
    let online = |peer: &crate::shell::personal_continuity::DeviceId| {
        local.as_ref() != Some(peer) && online_peers.contains(peer)
    };
    let now_us = fabric_monotonic_us();
    let probes = with_fabric_clock(|core| {
        core.retain_peers(|peer| known.contains(peer));
        core.due_probes(online, now, now_us, || {
            let mut nonce = [0; 16];
            while nonce == [0; 16] {
                UnwrapErr(SysRng).fill_bytes(&mut nonce);
            }
            nonce
        })
    });
    for (peer, probe) in probes {
        let nonce = match &probe {
            PersonalFabricAction::FabricClockProbe { nonce, .. } => *nonce,
            _ => continue,
        };
        if network.send_personal_fabric_action(&peer, probe).is_err() {
            with_fabric_clock(|core| core.abandon(&peer, nonce));
        }
    }
}

fn pump_device_health(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    let now = Instant::now();
    let replies = device_health_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| finish_power_sample(&mut state, now),
    );
    if !policy.permits(ContinuityCapability::DisplayInput) {
        return;
    }
    for (peer, report) in replies {
        let _ = network.send_personal_fabric_action(
            &peer,
            PersonalFabricAction::DeviceHealthReport { report },
        );
    }
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let (all_peers, online) = network
        .with_device_fabric(|fabric| {
            let snapshots = fabric.snapshots();
            let online = snapshots
                .values()
                .filter(|snapshot| {
                    snapshot.presence.online && local.as_ref() != Some(&snapshot.owner)
                })
                .map(|snapshot| snapshot.owner.clone())
                .collect::<Vec<_>>();
            (snapshots.keys().cloned().collect::<BTreeSet<_>>(), online)
        })
        .unwrap_or_default();
    let mut probes = Vec::new();
    {
        let Ok(mut state) = device_health_state().lock() else {
            return;
        };
        state.tracker.retain_peers(|peer| all_peers.contains(peer));
        for peer in health_probe_targets(&mut state, online, now) {
            let mut nonce = [0; 16];
            while nonce == [0; 16] {
                UnwrapErr(SysRng).fill_bytes(&mut nonce);
            }
            probes.push((peer, nonce));
        }
    }
    for (peer, nonce) in probes {
        let begun = device_health_state()
            .lock()
            .is_ok_and(|mut state| state.tracker.begin_probe(peer.clone(), nonce, now));
        if begun
            && network
                .send_personal_fabric_action(
                    &peer,
                    PersonalFabricAction::DeviceHealthProbe { nonce },
                )
                .is_err()
            && let Ok(mut state) = device_health_state().lock()
        {
            state.tracker.abandon_probe(&peer, nonce);
        }
    }
}

pub(super) fn handle_spatial_action(
    network: &ShellNetwork,
    route: &crate::shell::network::AuthenticatedPersonalRoute,
    target: &crate::shell::personal_continuity::DeviceId,
    action: PersonalFabricAction,
) {
    let Ok(mut spatial) = spatial_input_state().lock() else {
        return;
    };
    let reply = apply_spatial_action(&mut spatial, network, route, target, action);
    spatial.retain_spatial_protocol();
    drop(spatial);
    if let Some(action) = reply {
        let _ = network.send_personal_fabric_action_on_route(route, action);
    }
}

fn apply_spatial_action(
    spatial: &mut SpatialInputState,
    network: &ShellNetwork,
    route: &crate::shell::network::AuthenticatedPersonalRoute,
    target: &crate::shell::personal_continuity::DeviceId,
    action: PersonalFabricAction,
) -> Option<PersonalFabricAction> {
    let source = route.device().clone();
    let mut reply = None;
    match action {
        action @ PersonalFabricAction::SetDisplayPlacementChecked { .. } => {
            spatial.apply_checked_display_move(&network.device_fabric(), target, &action);
        }
        PersonalFabricAction::SetDisplayPlacement { rect } => {
            spatial.placement_override = Some(rect);
        }
        PersonalFabricAction::InputLeaseGranted { lease } => {
            if lease.validate_at(unix_now_ms()).is_ok() {
                spatial.receive_spatial_grant(source, route.clone(), lease);
            }
        }
        PersonalFabricAction::InputLeaseRevoked { nonce } => {
            spatial.accept_spatial_withdrawal(route, target, nonce);
        }
        action @ (PersonalFabricAction::SpatialInputProtocol { .. }
        | PersonalFabricAction::SpatialInputSequenced { .. }) => {
            reply = spatial.handle_spatial_protocol_action(route, target, &action, unix_now_ms());
        }
        PersonalFabricAction::Input { lease_nonce, event } => {
            spatial.accept_lease_bound_input(route, target, (lease_nonce, event), unix_now_ms());
        }
        PersonalFabricAction::ResumeMedia { .. }
        | PersonalFabricAction::MediaAccepted { .. }
        | PersonalFabricAction::MediaRejected { .. }
        | PersonalFabricAction::MediaCancelled { .. }
        | PersonalFabricAction::RemoteAudioOffer { .. }
        | PersonalFabricAction::RemoteAudioDecision { .. }
        | PersonalFabricAction::RemoteAudioFrame { .. }
        | PersonalFabricAction::RemoteAudioStop { .. }
        | PersonalFabricAction::RemoteAudioKeepalive { .. }
        | PersonalFabricAction::RemoteAudioCancel { .. }
        | PersonalFabricAction::SpeakerGroupInvite { .. }
        | PersonalFabricAction::SpeakerGroupDecision { .. }
        | PersonalFabricAction::SpeakerGroupLeave { .. }
        | PersonalFabricAction::SpeakerGroupSessionOffer { .. }
        | PersonalFabricAction::SpeakerGroupSessionDecision { .. }
        | PersonalFabricAction::SpeakerGroupSessionStop { .. }
        | PersonalFabricAction::SpeakerIdentify { .. }
        | PersonalFabricAction::SpeakerSweep { .. }
        | PersonalFabricAction::SpeakerListenInvite { .. }
        | PersonalFabricAction::SpeakerListenDecision { .. }
        | PersonalFabricAction::SpeakerListen { .. }
        | PersonalFabricAction::SpeakerHeard { .. }
        | PersonalFabricAction::WindowViews { .. }
        | PersonalFabricAction::WakeDevice { .. }
        | PersonalFabricAction::WakeReport { .. }
        | PersonalFabricAction::WakeProbe { .. }
        | PersonalFabricAction::WakeProbeReply { .. }
        | PersonalFabricAction::RemoteCaptureSignal { .. }
        | PersonalFabricAction::RemoteCaptureChunk { .. }
        | PersonalFabricAction::DeviceHealthProbe { .. }
        | PersonalFabricAction::DeviceHealthReport { .. }
        | PersonalFabricAction::FabricClockProbe { .. }
        | PersonalFabricAction::FabricClockReport { .. }
        | PersonalFabricAction::Tethering { .. }
        | PersonalFabricAction::BluetoothHandoffPrepare { .. }
        | PersonalFabricAction::BluetoothHandoffPrepared { .. }
        | PersonalFabricAction::BluetoothHandoffCommit { .. }
        | PersonalFabricAction::BluetoothHandoffFinished { .. }
        | PersonalFabricAction::BluetoothHandoffAccepted { .. }
        | PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
        | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }
        | PersonalFabricAction::BluetoothHandoffCancel { .. }
        | PersonalFabricAction::NearbyDrop { .. }
        | PersonalFabricAction::NearbyDropResult { .. }
        | PersonalFabricAction::NearbyTransferOffer { .. }
        | PersonalFabricAction::NearbyTransferReady { .. }
        | PersonalFabricAction::NearbyTransferCancel { .. }
        | PersonalFabricAction::NearbyTransferRestart { .. }
        | PersonalFabricAction::NearbyTransferResult { .. }
        | PersonalFabricAction::NotificationProject { .. }
        | PersonalFabricAction::NotificationRetire { .. }
        | PersonalFabricAction::NotificationDismiss { .. }
        | PersonalFabricAction::NotificationInvoke { .. }
        | PersonalFabricAction::RealtimeOffer { .. }
        | PersonalFabricAction::RealtimeAccept { .. }
        | PersonalFabricAction::RealtimeMediaReady { .. }
        | PersonalFabricAction::RealtimeStopSource { .. }
        | PersonalFabricAction::RealtimeCancel { .. }
        | PersonalFabricAction::GamepadForwardRequest { .. }
        | PersonalFabricAction::GamepadForwardGrant { .. }
        | PersonalFabricAction::GamepadForwardEvent { .. }
        | PersonalFabricAction::GamepadForwardStop { .. } => {}
    }
    reply
}

#[allow(
    clippy::too_many_lines,
    reason = "the four authenticated Bluetooth transaction messages are kept together so every phase transition and rollback is auditable"
)]
fn handle_bluetooth_handoff_action(
    network: &ShellNetwork,
    envelope_request_id: [u8; 16],
    source: &crate::shell::personal_continuity::DeviceId,
    action: PersonalFabricAction,
) {
    match action {
        PersonalFabricAction::BluetoothHandoffPrepare { address, name } => {
            let accepted = bluetooth_handoff_state().lock().is_ok_and(|mut state| {
                if state.transactions.len() >= MAX_BLUETOOTH_HANDOFFS
                    || state.transactions.contains_key(&envelope_request_id)
                {
                    return false;
                }
                state.transactions.insert(
                    envelope_request_id,
                    BluetoothHandoffTransaction {
                        peer: source.clone(),
                        address: address.clone(),
                        name,
                        outbound: false,
                        phase: BluetoothHandoffPhase::InspectingTarget,
                        authorization: None,
                        started: Instant::now(),
                    },
                );
                state.start_worker(
                    envelope_request_id,
                    address.clone(),
                    BluetoothWorkerOperation::InspectTarget,
                );
                true
            });
            if !accepted {
                let _ = network.send_personal_fabric_action(
                    source,
                    PersonalFabricAction::BluetoothHandoffPrepared {
                        request_id: envelope_request_id,
                        address,
                        ready: false,
                    },
                );
            }
        }
        PersonalFabricAction::BluetoothHandoffPrepared {
            request_id,
            address,
            ready,
        } => {
            let Ok(mut state) = bluetooth_handoff_state().lock() else {
                return;
            };
            let valid = state
                .transactions
                .get(&request_id)
                .is_some_and(|transaction| {
                    transaction.matches(
                        source,
                        &address,
                        true,
                        BluetoothHandoffPhase::AwaitingReady,
                    )
                });
            if !valid {
                return;
            }
            if !ready {
                state.finish(
                    request_id,
                    BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::TargetUnavailable),
                );
                tracing::warn!(
                    accessory = address,
                    "Bluetooth handoff target declined or could not verify this accessory"
                );
                return;
            }
            if let Some(transaction) = state.transactions.get_mut(&request_id) {
                transaction.phase = BluetoothHandoffPhase::DisconnectingSource;
            }
            state.start_worker(
                request_id,
                address,
                BluetoothWorkerOperation::DisconnectSource,
            );
        }
        PersonalFabricAction::BluetoothHandoffPairingOffered {
            request_id,
            address,
            authorization,
        } => {
            let Ok(mut state) = bluetooth_handoff_state().lock() else {
                return;
            };
            let valid = state
                .transactions
                .get(&request_id)
                .is_some_and(|transaction| {
                    transaction.matches(
                        source,
                        &address,
                        true,
                        BluetoothHandoffPhase::AwaitingReady,
                    )
                });
            if !valid {
                return;
            }
            if let Some(transaction) = state.transactions.get_mut(&request_id) {
                transaction.phase = BluetoothHandoffPhase::DisconnectingSource;
                transaction.authorization = Some(authorization);
                transaction.started = Instant::now();
            }
            state.start_worker(
                request_id,
                address,
                BluetoothWorkerOperation::DisconnectSource,
            );
        }
        PersonalFabricAction::BluetoothHandoffCommit {
            request_id,
            address,
        } => {
            let accepted = bluetooth_handoff_state().lock().is_ok_and(|mut state| {
                let valid = state
                    .transactions
                    .get(&request_id)
                    .is_some_and(|transaction| {
                        transaction.matches(source, &address, false, BluetoothHandoffPhase::Ready)
                    });
                if !valid {
                    return false;
                }
                if let Some(transaction) = state.transactions.get_mut(&request_id) {
                    transaction.phase = BluetoothHandoffPhase::ConnectingTarget;
                }
                state.start_worker(
                    request_id,
                    address.clone(),
                    BluetoothWorkerOperation::ConnectTarget,
                );
                true
            });
            if !accepted {
                let _ = network.send_personal_fabric_action(
                    source,
                    PersonalFabricAction::BluetoothHandoffFinished {
                        request_id,
                        address,
                        connected: false,
                    },
                );
            }
        }
        PersonalFabricAction::BluetoothHandoffPairingCommit {
            request_id,
            address,
            authorization,
        } => {
            let accepted = bluetooth_handoff_state().lock().is_ok_and(|mut state| {
                let valid = state
                    .transactions
                    .get(&request_id)
                    .is_some_and(|transaction| {
                        transaction.accepts_pairing_authorization(source, &address, authorization)
                    });
                if !valid {
                    return false;
                }
                if let Some(transaction) = state.transactions.get_mut(&request_id) {
                    transaction.phase = BluetoothHandoffPhase::PairingTarget;
                    transaction.authorization = None;
                    transaction.started = Instant::now();
                }
                state.start_worker(
                    request_id,
                    address.clone(),
                    BluetoothWorkerOperation::PairTarget,
                );
                true
            });
            if !accepted {
                let _ = network.send_personal_fabric_action(
                    source,
                    PersonalFabricAction::BluetoothHandoffFinished {
                        request_id,
                        address,
                        connected: false,
                    },
                );
            }
        }
        PersonalFabricAction::BluetoothHandoffFinished {
            request_id,
            address,
            connected,
        } => {
            let Ok(mut state) = bluetooth_handoff_state().lock() else {
                return;
            };
            let valid = state
                .transactions
                .get(&request_id)
                .is_some_and(|transaction| {
                    transaction.matches(
                        source,
                        &address,
                        true,
                        BluetoothHandoffPhase::AwaitingFinish,
                    )
                });
            if !valid {
                return;
            }
            if connected {
                drop(state);
                let accepted = network
                    .send_personal_fabric_action(
                        source,
                        PersonalFabricAction::BluetoothHandoffAccepted {
                            request_id,
                            address: address.clone(),
                        },
                    )
                    .is_ok();
                if accepted
                    && let Ok(mut state) = bluetooth_handoff_state().lock()
                    && let Some(transaction) =
                        state.finish(request_id, BluetoothHandoffPhaseView::Completed)
                {
                    tracing::info!(
                        accessory = transaction.name,
                        target = transaction.peer.as_str(),
                        "Bluetooth accessory handoff completed"
                    );
                } else if !accepted && let Ok(mut state) = bluetooth_handoff_state().lock() {
                    state.finish(
                        request_id,
                        BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Send),
                    );
                    state.start_worker(
                        request_id,
                        address,
                        BluetoothWorkerOperation::ReconnectSource,
                    );
                }
            } else {
                state.finish(
                    request_id,
                    BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::ConnectTarget),
                );
                state.start_worker(
                    request_id,
                    address,
                    BluetoothWorkerOperation::ReconnectSource,
                );
            }
        }
        PersonalFabricAction::BluetoothHandoffAccepted {
            request_id,
            address,
        } => {
            let Ok(mut state) = bluetooth_handoff_state().lock() else {
                return;
            };
            let valid = state
                .transactions
                .get(&request_id)
                .is_some_and(|transaction| {
                    transaction.matches(
                        source,
                        &address,
                        false,
                        BluetoothHandoffPhase::AwaitingAcceptance,
                    )
                });
            if valid {
                state.finish(request_id, BluetoothHandoffPhaseView::Completed);
            }
        }
        PersonalFabricAction::BluetoothHandoffCancel {
            request_id,
            address,
        } => {
            let Ok(mut state) = bluetooth_handoff_state().lock() else {
                return;
            };
            let valid = state
                .transactions
                .get(&request_id)
                .is_some_and(|transaction| {
                    transaction.peer == *source
                        && transaction.address.eq_ignore_ascii_case(&address)
                });
            if !valid {
                return;
            }
            if let Some(transaction) =
                state.finish(request_id, BluetoothHandoffPhaseView::Cancelled)
            {
                rollback_bluetooth_transaction(&state, request_id, &transaction);
            }
        }
        _ => {}
    }
}

fn tear_down_bluetooth_handoffs(network: &ShellNetwork) {
    let transactions = bluetooth_handoff_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            let request_ids = state.transactions.keys().copied().collect::<Vec<_>>();
            let transactions = finish_bluetooth_transactions(
                &mut state,
                &request_ids,
                BluetoothHandoffPhaseView::Cancelled,
            );
            for (request_id, transaction, rollback) in &transactions {
                run_bluetooth_rollback(&state, *request_id, transaction.address.clone(), *rollback);
            }
            transactions
        },
    );
    for (request_id, transaction, _) in transactions {
        let _ = network.send_personal_fabric_action(
            &transaction.peer,
            PersonalFabricAction::BluetoothHandoffCancel {
                request_id,
                address: transaction.address,
            },
        );
    }
}

fn pump_bluetooth_handoffs(network: &ShellNetwork) {
    let results = bluetooth_handoff_state().lock().map_or_else(
        |_| Vec::new(),
        |state| state.result_rx.try_iter().collect::<Vec<_>>(),
    );
    for result in results {
        finish_bluetooth_worker(network, result);
    }

    let online = network.online_device_ids();
    let lost = bluetooth_handoff_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            let request_ids = state
                .transactions
                .iter()
                .filter(|(_, transaction)| !bluetooth_peer_online(transaction, &online))
                .map(|(request_id, _)| *request_id)
                .collect::<Vec<_>>();
            let lost = finish_bluetooth_transactions(
                &mut state,
                &request_ids,
                BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::TargetUnavailable),
            );
            for (request_id, transaction, rollback) in &lost {
                run_bluetooth_rollback(&state, *request_id, transaction.address.clone(), *rollback);
            }
            lost
        },
    );
    for (request_id, transaction, _) in lost {
        let _ = network.send_personal_fabric_action(
            &transaction.peer,
            PersonalFabricAction::BluetoothHandoffCancel {
                request_id,
                address: transaction.address,
            },
        );
    }

    let expired = bluetooth_handoff_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            let expired = state
                .transactions
                .iter()
                .filter(|(_, transaction)| transaction.started.elapsed() >= BLUETOOTH_HANDOFF_WAIT)
                .map(|(request_id, transaction)| (*request_id, transaction.clone()))
                .collect::<Vec<_>>();
            for (request_id, _) in &expired {
                state.finish(
                    *request_id,
                    BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::TimedOut),
                );
            }
            expired
        },
    );
    for (request_id, transaction) in expired {
        if transaction.outbound
            && matches!(
                transaction.phase,
                BluetoothHandoffPhase::DisconnectingSource | BluetoothHandoffPhase::AwaitingFinish
            )
            && let Ok(state) = bluetooth_handoff_state().lock()
        {
            state.start_worker(
                request_id,
                transaction.address.clone(),
                BluetoothWorkerOperation::ReconnectSource,
            );
        }
        if !transaction.outbound && transaction.phase == BluetoothHandoffPhase::AwaitingAcceptance {
            let address = transaction.address.clone();
            std::thread::spawn(move || {
                let _ = os_actions::bt_disconnect(&address);
            });
        }
        tracing::warn!(
            accessory = transaction.name,
            "Bluetooth accessory handoff timed out"
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "worker completions mirror the Bluetooth transaction phases and keep rollback adjacent to each hardware outcome"
)]
fn finish_bluetooth_worker(network: &ShellNetwork, result: BluetoothWorkerResult) {
    let request_id = result.request_id;
    let address = result.address;
    match result.outcome {
        BluetoothWorkerOutcome::TargetInspected(inspection) => {
            let inspected = match inspection {
                Ok(inspected) => Some(inspected),
                Err(error) => {
                    tracing::warn!(%error, "could not verify Bluetooth accessory on target");
                    None
                }
            };
            let response = bluetooth_handoff_state().lock().ok().and_then(|mut state| {
                let transaction = state.transactions.get_mut(&request_id)?;
                if transaction.outbound
                    || !transaction.address.eq_ignore_ascii_case(&address)
                    || transaction.phase != BluetoothHandoffPhase::InspectingTarget
                {
                    return None;
                }
                match inspected {
                    Some(os_actions::BluetoothHandoffTarget::Paired) => {
                        transaction.phase = BluetoothHandoffPhase::Ready;
                        Some((transaction.clone(), true))
                    }
                    Some(os_actions::BluetoothHandoffTarget::Pairable) => {
                        transaction.phase = BluetoothHandoffPhase::AwaitingLocalAuthorization;
                        None
                    }
                    None => {
                        let transaction = transaction.clone();
                        state.finish(
                            request_id,
                            BluetoothHandoffPhaseView::Failed(
                                BluetoothHandoffFailure::InspectTarget,
                            ),
                        );
                        Some((transaction, false))
                    }
                }
            });
            if let Some((transaction, ready)) = response
                && network
                    .send_personal_fabric_action(
                        &transaction.peer,
                        PersonalFabricAction::BluetoothHandoffPrepared {
                            request_id,
                            address,
                            ready,
                        },
                    )
                    .is_err()
                && ready
                && let Ok(mut state) = bluetooth_handoff_state().lock()
            {
                state.finish(
                    request_id,
                    BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Send),
                );
            }
        }
        BluetoothWorkerOutcome::SourceDisconnected(disconnected) => {
            let stale_rollback = stale_worker_rollback(
                &BluetoothWorkerOutcome::SourceDisconnected(disconnected.clone()),
            );
            if let Err(error) = disconnected {
                tracing::warn!(%error, "could not release Bluetooth accessory for handoff");
                if let Ok(mut state) = bluetooth_handoff_state().lock() {
                    state.finish(
                        request_id,
                        BluetoothHandoffPhaseView::Failed(
                            BluetoothHandoffFailure::DisconnectSource,
                        ),
                    );
                }
                return;
            }
            let transaction = bluetooth_handoff_state().lock().ok().and_then(|mut state| {
                let transaction = state.transactions.get_mut(&request_id)?;
                if !transaction.outbound
                    || !transaction.address.eq_ignore_ascii_case(&address)
                    || transaction.phase != BluetoothHandoffPhase::DisconnectingSource
                {
                    return None;
                }
                transaction.phase = BluetoothHandoffPhase::AwaitingFinish;
                Some(transaction.clone())
            });
            let Some(transaction) = transaction else {
                if let Ok(state) = bluetooth_handoff_state().lock() {
                    run_bluetooth_rollback(&state, request_id, address, stale_rollback);
                }
                return;
            };
            let action = transaction.authorization.map_or_else(
                || PersonalFabricAction::BluetoothHandoffCommit {
                    request_id,
                    address: address.clone(),
                },
                |authorization| PersonalFabricAction::BluetoothHandoffPairingCommit {
                    request_id,
                    address: address.clone(),
                    authorization,
                },
            );
            if network
                .send_personal_fabric_action(&transaction.peer, action)
                .is_err()
                && let Ok(mut state) = bluetooth_handoff_state().lock()
            {
                state.finish(
                    request_id,
                    BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Send),
                );
                state.start_worker(
                    request_id,
                    address,
                    BluetoothWorkerOperation::ReconnectSource,
                );
            }
        }
        outcome @ (BluetoothWorkerOutcome::TargetPaired(_)
        | BluetoothWorkerOutcome::TargetConnected(_)) => {
            let stale_rollback = stale_worker_rollback(&outcome);
            let (connected, expected_phase) = match outcome {
                BluetoothWorkerOutcome::TargetPaired(result) => {
                    (result, BluetoothHandoffPhase::PairingTarget)
                }
                BluetoothWorkerOutcome::TargetConnected(result) => {
                    (result, BluetoothHandoffPhase::ConnectingTarget)
                }
                _ => return,
            };
            let success = connected.is_ok();
            if let Err(error) = connected {
                tracing::warn!(%error, "Bluetooth accessory could not connect on target");
            }
            let transaction = bluetooth_handoff_state().lock().ok().and_then(|mut state| {
                let valid = state
                    .transactions
                    .get(&request_id)
                    .is_some_and(|transaction| {
                        !transaction.outbound
                            && transaction.address.eq_ignore_ascii_case(&address)
                            && transaction.phase == expected_phase
                    });
                if !valid {
                    return None;
                }
                if success {
                    let transaction = state.transactions.get_mut(&request_id)?;
                    transaction.phase = BluetoothHandoffPhase::AwaitingAcceptance;
                    Some(transaction.clone())
                } else {
                    let failure = if expected_phase == BluetoothHandoffPhase::PairingTarget {
                        BluetoothHandoffFailure::PairTarget
                    } else {
                        BluetoothHandoffFailure::ConnectTarget
                    };
                    state.finish(request_id, BluetoothHandoffPhaseView::Failed(failure))
                }
            });
            let Some(transaction) = transaction else {
                if let Ok(state) = bluetooth_handoff_state().lock() {
                    run_bluetooth_rollback(&state, request_id, address, stale_rollback);
                }
                return;
            };
            let send = network.send_personal_fabric_action(
                &transaction.peer,
                PersonalFabricAction::BluetoothHandoffFinished {
                    request_id,
                    address: address.clone(),
                    connected: success,
                },
            );
            if send.is_err() {
                if let Ok(mut state) = bluetooth_handoff_state().lock() {
                    state.finish(
                        request_id,
                        BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Send),
                    );
                }
                if success {
                    std::thread::spawn(move || {
                        let _ = os_actions::bt_disconnect(&address);
                    });
                }
            }
        }
        BluetoothWorkerOutcome::SourceReconnected(reconnected) => {
            if let Err(error) = reconnected {
                tracing::error!(%error, "could not restore Bluetooth accessory after failed handoff");
                if let Ok(mut state) = bluetooth_handoff_state().lock()
                    && let Some(outcome) = state.outcomes.get_mut(&request_id)
                {
                    outcome.phase =
                        BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Rollback);
                    outcome.updated = Instant::now();
                }
            }
        }
    }
}

/// Move one Bluetooth accessory to another certified Tilo device without ever
/// copying its bond keys. An existing target bond is reused; a newly discovered
/// accessory requires an explicit, transaction-bound approval on the target.
///
/// # Errors
///
/// Returns an error when the address/name is malformed, the destination is
/// not online and certified, another handoff exhausted the bounded queue, or
/// the authenticated prepare action cannot be sent.
pub fn request_bluetooth_handoff(
    network: &ShellNetwork,
    address: &str,
    name: &str,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::Media)?;
    if !super::device_fabric::valid_bluetooth_address(address)
        || name.is_empty()
        || name.len() > 128
        || name.chars().any(char::is_control)
    {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-bt-identity-invalid",
        ));
    }
    let accessory = os_actions::list_bt_devices()?
        .into_iter()
        .find(|device| device.mac.eq_ignore_ascii_case(address) && device.connected)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-bt-not-connected"))?;
    if accessory.name.is_empty()
        || accessory.name.len() > 128
        || accessory.name.chars().any(char::is_control)
    {
        return Err(crate::shell::i18n::t("device-fabric-error-bt-name-invalid"));
    }
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    if target == &local {
        return Err(crate::shell::i18n::t("device-fabric-error-bt-already-here"));
    }
    let fabric = network.device_fabric();
    let destination = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-target-not-enrolled"))?;
    if !destination.presence.online {
        return Err(crate::shell::i18n::t("device-fabric-error-target-offline"));
    }
    if bluetooth_handoff_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-bt-coordinator"))?
        .transactions
        .len()
        >= MAX_BLUETOOTH_HANDOFFS
    {
        return Err(crate::shell::i18n::t("device-fabric-error-bt-too-many"));
    }
    let request_id = network.send_personal_fabric_action(
        target,
        PersonalFabricAction::BluetoothHandoffPrepare {
            address: address.to_ascii_uppercase(),
            name: accessory.name.clone(),
        },
    )?;
    let mut state = bluetooth_handoff_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-bt-coordinator"))?;
    state.transactions.insert(
        request_id,
        BluetoothHandoffTransaction {
            peer: target.clone(),
            address: address.to_ascii_uppercase(),
            name: accessory.name,
            outbound: true,
            phase: BluetoothHandoffPhase::AwaitingReady,
            authorization: None,
            started: Instant::now(),
        },
    );
    drop(state);
    Ok(request_id)
}

/// Resolve one target-local approval prompt for a newly discovered accessory.
/// Approval creates a fresh one-shot token bound to this transaction; denial
/// and every failed send remove the pending request.
///
/// # Errors
///
/// Returns an error when the prompt is stale, belongs to an outbound handoff,
/// or its authenticated response cannot be delivered to the source.
pub fn authorize_bluetooth_handoff(
    network: &ShellNetwork,
    request_id: [u8; 16],
    approved: bool,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::Media)?;
    let (transaction, authorization) = {
        let mut state = bluetooth_handoff_state()
            .lock()
            .map_err(|_| crate::shell::i18n::t("device-fabric-error-bt-coordinator"))?;
        let transaction = state
            .transactions
            .get_mut(&request_id)
            .filter(|transaction| {
                !transaction.outbound
                    && transaction.phase == BluetoothHandoffPhase::AwaitingLocalAuthorization
            })
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-bt-approval-gone"))?;
        let transaction = transaction.clone();
        let authorization = if approved {
            let mut authorization = [0; 16];
            while authorization == [0; 16] {
                UnwrapErr(SysRng).fill_bytes(&mut authorization);
            }
            if let Some(pending) = state.transactions.get_mut(&request_id) {
                pending.phase = BluetoothHandoffPhase::PairingReady;
                pending.authorization = Some(authorization);
                pending.started = Instant::now();
            }
            Some(authorization)
        } else {
            state.finish(request_id, BluetoothHandoffPhaseView::Cancelled);
            None
        };
        drop(state);
        (transaction, authorization)
    };

    let action = if let Some(authorization) = authorization {
        PersonalFabricAction::BluetoothHandoffPairingOffered {
            request_id,
            address: transaction.address.clone(),
            authorization,
        }
    } else {
        PersonalFabricAction::BluetoothHandoffPrepared {
            request_id,
            address: transaction.address.clone(),
            ready: false,
        }
    };
    if let Err(error) = network.send_personal_fabric_action(&transaction.peer, action) {
        if let Ok(mut state) = bluetooth_handoff_state().lock() {
            if approved {
                state.finish(
                    request_id,
                    BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Send),
                );
            } else if let Some(outcome) = state.outcomes.get_mut(&request_id) {
                outcome.phase = BluetoothHandoffPhaseView::Failed(BluetoothHandoffFailure::Send);
                outcome.updated = Instant::now();
            }
        }
        return Err(error);
    }
    Ok(())
}

/// Cancel one exact Bluetooth transaction and roll back whichever side has
/// already changed hardware state.
///
/// # Errors
///
/// Returns an error for stale requests or when authenticated cancellation
/// could not be delivered. Local rollback still runs on delivery failure.
pub fn cancel_bluetooth_handoff(
    network: &ShellNetwork,
    request_id: [u8; 16],
) -> Result<(), String> {
    require_continuity(ContinuityCapability::Media)?;
    let transaction = bluetooth_handoff_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-bt-coordinator"))?
        .transactions
        .get(&request_id)
        .cloned()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-bt-inactive"))?;
    let delivery = network.send_personal_fabric_action(
        &transaction.peer,
        PersonalFabricAction::BluetoothHandoffCancel {
            request_id,
            address: transaction.address.clone(),
        },
    );
    if let Ok(mut state) = bluetooth_handoff_state().lock()
        && let Some(transaction) = state.finish(request_id, BluetoothHandoffPhaseView::Cancelled)
    {
        rollback_bluetooth_transaction(&state, request_id, &transaction);
    }
    delivery.map(|_| ())
}

/// Retry a recent outbound failure after rollback has made the accessory local
/// and connected again.
///
/// # Errors
///
/// Returns an error for inbound/completed/stale outcomes or if source rollback
/// has not yet restored the accessory.
pub fn retry_bluetooth_handoff(
    network: &ShellNetwork,
    request_id: [u8; 16],
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::Media)?;
    let transaction = bluetooth_handoff_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-bt-coordinator"))?
        .outcomes
        .get(&request_id)
        .filter(|outcome| {
            outcome.transaction.outbound
                && matches!(
                    outcome.phase,
                    BluetoothHandoffPhaseView::Failed(_) | BluetoothHandoffPhaseView::Cancelled
                )
        })
        .map(|outcome| outcome.transaction.clone())
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-bt-no-retry"))?;
    let new_request = request_bluetooth_handoff(
        network,
        &transaction.address,
        &transaction.name,
        &transaction.peer,
    )?;
    if let Ok(mut state) = bluetooth_handoff_state().lock() {
        state.outcomes.remove(&request_id);
    }
    Ok(new_request)
}

/// Initiate a transactional move of one locally playing session. The source
/// remains audible until the destination reports that it actually opened the
/// handed-off source and sends an authenticated acknowledgement.
///
/// # Errors
///
/// Returns an error if the session disappeared, the target cannot render it,
/// the source is not portable, or the authenticated fabric send fails.
pub fn request_media_handoff(
    network: &ShellNetwork,
    session_id: &str,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::Media)?;
    let report = media::handoff::playback_report(session_id)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-media-session-gone"))?;
    let transfer = network
        .device_fabric()
        .handoff_media(session_id, target)
        .map_err(|error| error.to_string())?;
    let (locator, local_transfer) = match &report.source {
        media::handoff::HandoffSource::Workspace { path } => (
            MediaSourceLocator::Workspace {
                path: path.to_string_lossy().into_owned(),
            },
            None,
        ),
        media::handoff::HandoffSource::RemoteStream { url } => {
            (MediaSourceLocator::RemoteStream { url: url.clone() }, None)
        }
        media::handoff::HandoffSource::AppResource { uri } => {
            (MediaSourceLocator::AppResource { uri: uri.clone() }, None)
        }
        media::handoff::HandoffSource::LocalFile { path } => {
            let transfer = PreparedMediaTransfer::prepare(path)?;
            (
                MediaSourceLocator::PersonalTransfer {
                    transfer_id: transfer.transfer_id,
                    file_name: transfer.name.clone(),
                },
                Some(transfer),
            )
        }
    };
    let action = PersonalFabricAction::ResumeMedia {
        session_id: report.session.id.clone(),
        app_id: report.session.app_id.clone(),
        workspace_id: report.workspace_id.clone(),
        title: report.session.title.clone(),
        locator,
        position_ms: transfer.position_ms,
        resume_playing: transfer.resume_playing,
        has_video: report.session.has_video,
    };
    let route = network
        .current_personal_route(target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-route"))?;
    // A staging slot is taken before the destination learns of the handoff,
    // so a busy source refuses up front instead of withdrawing it.
    let permit = local_transfer
        .as_ref()
        .map(|_| NearbyStagingPermit::acquire("shell:media-handoff"))
        .transpose()?;
    let request_id = network.send_personal_fabric_action_on_route(&route, action)?;
    let personal_transfer_id = local_transfer.as_ref().map(|transfer| transfer.transfer_id);
    // Recorded before staging starts: staging that finishes with no pending
    // handoff drops its copy.
    record_media_handoff_status(MediaHandoffStatus {
        request_id,
        session_id: report.session.id.clone(),
        title: report.session.title.clone(),
        target: target.clone(),
        phase: MediaHandoffPhase::Pending,
        updated: Instant::now(),
        route,
        personal_transfer_id,
    });
    media::handoff::remember_outbound_handoff(request_id, report);
    if let Some((transfer, permit)) = local_transfer.zip(permit)
        && let Err(error) = transfer.stage(target, request_id, permit)
    {
        fail_media_handoff(network, request_id, MediaHandoffFailure::OpenFailed);
        return Err(error);
    }
    Ok(request_id)
}

/// The local file behind one media handoff, checked before the destination
/// is told about it. The file is staged into the transfer spool off the frame
/// thread and then offered as a [`TransferKind::MediaHandoff`] object, so the
/// handoff resumes from the destination's verified prefix after a drop.
///
/// [`TransferKind::MediaHandoff`]: crate::shell::nearby_transfer::TransferKind::MediaHandoff
struct PreparedMediaTransfer {
    transfer_id: [u8; 16],
    name: String,
    source: PathBuf,
}

impl PreparedMediaTransfer {
    fn prepare(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| format!("media file cannot be inspected: {error}"))?;
        if !metadata.file_type().is_file()
            || metadata.len() == 0
            || metadata.len() > crate::shell::nearby_transfer::MAX_TRANSFER_BYTES
        {
            return Err("media source is not a supported regular file".into());
        }
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .and_then(portable_media_name)
            .ok_or_else(|| "media file has no portable name".to_string())?;
        let mut transfer_id = [0_u8; 16];
        while transfer_id == [0; 16] {
            UnwrapErr(SysRng).fill_bytes(&mut transfer_id);
        }
        Ok(Self {
            transfer_id,
            name,
            source: path.to_path_buf(),
        })
    }

    fn stage(
        self,
        target: &crate::shell::personal_continuity::DeviceId,
        request_id: [u8; 16],
        permit: NearbyStagingPermit,
    ) -> Result<(), String> {
        let sender = nearby_staging_queue().0.clone();
        let target = target.clone();
        std::thread::Builder::new()
            .name("tilo-media-stage".into())
            .spawn(move || {
                let _permit = permit;
                let result = with_nearby_transfer_store(|store| {
                    store
                        .stage_media_with_id(
                            &self.source,
                            self.name.clone(),
                            target.as_str().to_string(),
                            unix_now_ms(),
                            self.transfer_id,
                        )
                        .map(|_| ())
                });
                let _ = sender.send(NearbyStagingResult {
                    transfer_id: self.transfer_id,
                    target,
                    name: self.name,
                    result,
                    media_request: Some(request_id),
                });
            })
            .map(|_| ())
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "device-fabric-error-transfer-start",
                    &[("error", error.to_string().into())],
                )
            })
    }
}

/// A display name the transfer lane accepts: separators and controls become
/// `_`, and the name is cut to at most 255 bytes on a character boundary.
fn portable_media_name(name: &str) -> Option<String> {
    let mut sanitized = String::new();
    for character in name.chars() {
        let character = if character.is_control() || matches!(character, '/' | '\\') {
            '_'
        } else {
            character
        };
        if sanitized.len().saturating_add(character.len_utf8()) > 255 {
            break;
        }
        sanitized.push(character);
    }
    super::device_fabric::valid_nearby_file_name(&sanitized).then_some(sanitized)
}

pub(crate) fn media_handoff_status_views(network: &ShellNetwork) -> Vec<MediaHandoffStatusView> {
    let Ok(mut statuses) = media_handoff_statuses().lock() else {
        return Vec::new();
    };
    let now = Instant::now();
    statuses.retain(|_, status| {
        now.checked_duration_since(status.updated)
            .is_none_or(|elapsed| elapsed < MEDIA_HANDOFF_HISTORY_TTL)
    });
    let fabric = network.device_fabric();
    let mut views = statuses
        .values()
        .map(|status| MediaHandoffStatusView {
            request_id: status.request_id,
            session_id: status.session_id.clone(),
            title: status.title.clone(),
            target: status.target.clone(),
            target_label: fabric.snapshots().get(&status.target).map_or_else(
                || status.target.as_str().to_string(),
                |snapshot| snapshot.presence.label.clone(),
            ),
            phase: status.phase,
        })
        .collect::<Vec<_>>();
    views.sort_by(|left, right| left.title.cmp(&right.title));
    views
}

fn pump_media_handoff_statuses(network: &ShellNetwork) {
    let now = Instant::now();
    let expired = media_handoff_statuses().lock().map_or_else(
        |_| Vec::new(),
        |mut statuses| {
            statuses
                .values_mut()
                .filter_map(|status| {
                    (status.phase == MediaHandoffPhase::Pending
                        && now
                            .checked_duration_since(status.updated)
                            .is_some_and(|elapsed| elapsed >= OUTBOUND_MEDIA_HANDOFF_WAIT))
                    .then(|| {
                        let _ = media::handoff::cancel_outbound_handoff(
                            status.request_id,
                            &status.session_id,
                        );
                        cancel_status_personal_transfer(network, status);
                        status.phase = MediaHandoffPhase::Failed(MediaHandoffFailure::TimedOut);
                        status.updated = now;
                        (
                            status.route.clone(),
                            PersonalFabricAction::MediaCancelled {
                                request_id: status.request_id,
                                session_id: status.session_id.clone(),
                            },
                        )
                    })
                })
                .collect::<Vec<_>>()
        },
    );
    for (route, action) in expired {
        let _ = network.send_personal_fabric_action_on_route(&route, action);
    }
}

fn compensate_failed_media_acceptance<T>(
    delivery: &Result<T, String>,
    controller_bus_name: Option<&str>,
    expected_uri: Option<&str>,
    mut pause: impl FnMut(&str, &str) -> bool,
) -> bool {
    let Some((controller, expected_uri)) = controller_bus_name
        .zip(expected_uri)
        .filter(|_| delivery.is_err())
    else {
        return false;
    };
    // Player mutation completed before route teardown won the worker race,
    // but its acknowledgement could not be enqueued under the same route.
    // Pausing the exact launcher-verified controller is the compensating edge.
    pause(controller, expected_uri)
}

fn tear_down_media_handoffs(network: &ShellNetwork) {
    let pending = media_handoff_statuses().lock().map_or_else(
        |_| Vec::new(),
        |mut statuses| {
            std::mem::take(&mut *statuses)
                .into_values()
                .filter(|status| status.phase == MediaHandoffPhase::Pending)
                .collect::<Vec<_>>()
        },
    );
    for status in pending {
        let _ = media::handoff::cancel_outbound_handoff(status.request_id, &status.session_id);
        cancel_status_personal_transfer(network, &status);
        let _ = network.send_personal_fabric_action_on_route(
            &status.route,
            PersonalFabricAction::MediaCancelled {
                request_id: status.request_id,
                session_id: status.session_id,
            },
        );
    }
}

/// Cancel one exact still-pending handoff. Local playback was never paused,
/// and the destination receives only the request/session tuple it already saw.
pub(crate) fn cancel_media_handoff(
    network: &ShellNetwork,
    request_id: [u8; 16],
) -> Result<(), String> {
    let status = media_handoff_statuses()
        .lock()
        .map_err(|_| "media handoff status is unavailable".to_string())?
        .get(&request_id)
        .cloned()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-handoff-gone"))?;
    if status.phase != MediaHandoffPhase::Pending {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-handoff-not-pending",
        ));
    }
    let delivery = network.send_personal_fabric_action_on_route(
        &status.route,
        PersonalFabricAction::MediaCancelled {
            request_id,
            session_id: status.session_id.clone(),
        },
    );
    cancel_status_personal_transfer(network, &status);
    let _ = media::handoff::cancel_outbound_handoff(request_id, &status.session_id);
    finish_media_handoff_status(request_id, MediaHandoffPhase::Cancelled);
    delivery.map(|_| ())
}

/// Retry the logical session/target captured by a failed or timed-out row.
pub(crate) fn retry_media_handoff(
    network: &ShellNetwork,
    request_id: [u8; 16],
) -> Result<[u8; 16], String> {
    let status = media_handoff_statuses()
        .lock()
        .map_err(|_| "media handoff status is unavailable".to_string())?
        .get(&request_id)
        .cloned()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-handoff-gone"))?;
    if !matches!(
        status.phase,
        MediaHandoffPhase::Failed(_) | MediaHandoffPhase::Cancelled
    ) {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-handoff-no-retry",
        ));
    }
    request_media_handoff(network, &status.session_id, &status.target)
}

/// Start a shell-owned live audio route using one opaque source-picker token.
/// `PipeWire` node ids are resolved only inside [`crate::shell::remote_audio`].
///
/// # Errors
///
/// Returns an error when media continuity is disabled, the picker token is
/// stale, or the exact certified target is unavailable.
pub fn request_remote_audio(
    network: &ShellNetwork,
    source_token: u64,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::Media)?;
    crate::shell::remote_audio::request(network, source_token, target)
}

/// Resolve a target-local remote-audio consent prompt.
///
/// # Errors
///
/// Returns an error for disabled policy, stale consent, sink startup failure,
/// or exact-peer delivery failure.
pub fn authorize_remote_audio(
    network: &ShellNetwork,
    session_id: [u8; 16],
    approved: bool,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::Media)?;
    crate::shell::remote_audio::authorize(network, session_id, approved)
}

/// Stop an active remote-audio route and revoke its target-issued capability.
///
/// # Errors
///
/// Returns an error when the route is stale or its exact peer cannot be
/// notified.
pub fn stop_remote_audio(network: &ShellNetwork, session_id: [u8; 16]) -> Result<(), String> {
    crate::shell::remote_audio::stop(network, session_id)
}

#[must_use]
pub fn remote_audio_view(
    focus: Option<&crate::shell::physical_gamepad::PhysicalGamepadBinding>,
) -> crate::shell::remote_audio::RemoteAudioView {
    crate::shell::remote_audio::snapshot(focus)
}

/// Route one live-audio picker source to a speaker group instead of a
/// single device. Each member admits the session under its own consent.
///
/// # Errors
///
/// Returns an error when media continuity is disabled, the group is not
/// available, or none of its speakers is ready.
pub fn request_speaker_group_audio(
    network: &ShellNetwork,
    source_token: u64,
    group: crate::shell::speaker_group::SpeakerGroupId,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::Media)?;
    crate::shell::speaker_group_runtime::start_session(
        network,
        group,
        crate::shell::speaker_group_runtime::GroupAudioSource::PickerToken(source_token),
    )
}

/// Play one local media session on a speaker group. The media keeps
/// decoding here; only its sound moves to the group.
///
/// # Errors
///
/// Returns an error when media continuity is disabled, the session is not
/// playing here, or the group is not available.
pub fn play_media_on_speaker_group(
    network: &ShellNetwork,
    session_id: &str,
    group: crate::shell::speaker_group::SpeakerGroupId,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::Media)?;
    if !media::handoff::playback_sessions()
        .iter()
        .any(|session| session.id == session_id)
    {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-media-session-gone",
        ));
    }
    crate::shell::speaker_group_runtime::start_session(
        network,
        group,
        crate::shell::speaker_group_runtime::GroupAudioSource::MediaSession(session_id.into()),
    )
}

/// Guard for every speaker-group edit and consent: all of it is media
/// continuity.
///
/// # Errors
///
/// Returns an error when media continuity is disabled on this device.
pub fn require_speaker_groups() -> Result<(), String> {
    require_continuity(ContinuityCapability::Media)
}

/// Send one file or folder to an authenticated exact target. Files up to
/// 48 KiB use the inline fast path; everything else is staged immutably into
/// the durable resumable object spool.
///
/// # Errors
///
/// Returns an error for an unsafe/oversized source or unavailable fabric route.
#[expect(
    clippy::too_many_lines,
    reason = "validation, staging and the offer stay in one ordered send path"
)]
pub fn request_nearby_drop(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    path: &Path,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::FileTransfer)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| super::device_fabric::valid_nearby_file_name(name))
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-name-not-portable"))?
        .to_string();
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        crate::shell::i18n::t_args(
            "device-fabric-error-inspect-failed",
            &[("error", error.to_string().into())],
        )
    })?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(crate::shell::i18n::t("device-fabric-error-drop-one-item"));
    }
    if metadata.is_dir()
        || metadata.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES as u64
    {
        let permit = NearbyStagingPermit::acquire("shell:spatial-drop")?;
        let target = target.clone();
        let worker_target = target.clone();
        let source = path.to_path_buf();
        let name = file_name.clone();
        let sender = nearby_staging_queue().0.clone();
        let mut transfer_id = [0; 16];
        while transfer_id == [0; 16] {
            UnwrapErr(SysRng).fill_bytes(&mut transfer_id);
        }
        std::thread::Builder::new()
            .name("tilo-nearby-stage".into())
            .spawn(move || {
                let permit = permit;
                let result = with_nearby_transfer_store(|store| {
                    store
                        .stage_with_id(
                            &source,
                            worker_target.as_str().to_string(),
                            unix_now_ms(),
                            transfer_id,
                        )
                        .map(|_| ())
                });
                if !permit.valid() {
                    if result.is_ok() {
                        let _ =
                            with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
                    }
                    return;
                }
                let _ = sender.send(NearbyStagingResult {
                    transfer_id,
                    target: worker_target,
                    name,
                    result,
                    media_request: None,
                });
            })
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "device-fabric-error-transfer-start",
                    &[("error", error.to_string().into())],
                )
            })?;
        if let Ok(mut state) = nearby_drop_state().lock() {
            state.events.push_back(NearbyDropEvent::Sending {
                file_name,
                peer: target,
            });
        }
        return Ok(transfer_id);
    }
    if metadata.len() == 0 {
        return Err(crate::shell::i18n::t("device-fabric-error-drop-empty"));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            crate::shell::i18n::t_args(
                "device-fabric-error-open-failed",
                &[("error", error.to_string().into())],
            )
        })?;
    let mut bytes = Vec::new();
    file.take((crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            crate::shell::i18n::t_args(
                "device-fabric-error-read-failed",
                &[("error", error.to_string().into())],
            )
        })?;
    if bytes.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES {
        return Err(crate::shell::i18n::t("device-fabric-error-file-changed"));
    }
    let total_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let request = network.send_personal_fabric_action(
        target,
        PersonalFabricAction::NearbyDrop {
            content_hash: *blake3::hash(&bytes).as_bytes(),
            bytes,
            file_name: file_name.clone(),
        },
    )?;
    if let Ok(mut state) = nearby_drop_state().lock() {
        if state.pending.len() >= 256
            && let Some(oldest) = state.pending.keys().next().copied()
        {
            state.pending.remove(&oldest);
        }
        state
            .pending
            .insert(request, (target.clone(), file_name.clone(), total_bytes));
        state.events.push_back(NearbyDropEvent::Sending {
            file_name,
            peer: target.clone(),
        });
    }
    Ok(request)
}

/// Send from an already-open Files grant capability. The descriptor, rather
/// than an app-provided path, crosses the deferred staging boundary.
///
/// # Errors
///
/// Returns a message when file transfer is not enabled, the source cannot be
/// inspected or read, is empty, or changes while read, the owner has no free
/// staging slot, the staging worker cannot start, or the offer cannot be sent
/// to `target`.
pub fn request_nearby_drop_secure(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    source: crate::shell::nearby_transfer::SecureNearbySource,
    staging_owner: &str,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::FileTransfer)?;
    let metadata = source.metadata()?;
    let file_name = source.name().to_string();
    if metadata.is_dir()
        || metadata.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES as u64
    {
        let permit = NearbyStagingPermit::acquire(staging_owner)?;
        let target = target.clone();
        let worker_target = target.clone();
        let name = file_name.clone();
        let sender = nearby_staging_queue().0.clone();
        let mut transfer_id = [0; 16];
        while transfer_id == [0; 16] {
            UnwrapErr(SysRng).fill_bytes(&mut transfer_id);
        }
        std::thread::Builder::new()
            .name("tilo-nearby-stage".into())
            .spawn(move || {
                let permit = permit;
                let result = with_nearby_transfer_store(|store| {
                    store
                        .stage_secure_with_id(
                            source,
                            worker_target.as_str().to_string(),
                            unix_now_ms(),
                            transfer_id,
                        )
                        .map(|_| ())
                });
                if !permit.valid() {
                    if result.is_ok() {
                        let _ =
                            with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
                    }
                    return;
                }
                let _ = sender.send(NearbyStagingResult {
                    transfer_id,
                    target: worker_target,
                    name,
                    result,
                    media_request: None,
                });
            })
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "device-fabric-error-transfer-start",
                    &[("error", error.to_string().into())],
                )
            })?;
        if let Ok(mut state) = nearby_drop_state().lock() {
            state.events.push_back(NearbyDropEvent::Sending {
                file_name,
                peer: target,
            });
        }
        return Ok(transfer_id);
    }
    if metadata.len() == 0 {
        return Err(crate::shell::i18n::t("device-fabric-error-drop-empty"));
    }
    let mut bytes = Vec::new();
    source
        .into_file()
        .take((crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            crate::shell::i18n::t_args(
                "device-fabric-error-read-failed",
                &[("error", error.to_string().into())],
            )
        })?;
    if bytes.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES {
        return Err(crate::shell::i18n::t("device-fabric-error-file-changed"));
    }
    let total_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let request = network.send_personal_fabric_action(
        target,
        PersonalFabricAction::NearbyDrop {
            content_hash: *blake3::hash(&bytes).as_bytes(),
            bytes,
            file_name: file_name.clone(),
        },
    )?;
    if let Ok(mut state) = nearby_drop_state().lock() {
        state
            .pending
            .insert(request, (target.clone(), file_name.clone(), total_bytes));
        state.events.push_back(NearbyDropEvent::Sending {
            file_name,
            peer: target.clone(),
        });
    }
    Ok(request)
}

/// Cancel one durable transfer locally and tell its exact peer to discard the
/// matching partial. An unrelated device/request pair cannot cancel it.
///
/// # Errors
///
/// Returns an error when the transfer is not active, its exact peer cannot be
/// reached, or durable local cleanup fails.
pub fn cancel_nearby_transfer(network: &ShellNetwork, transfer_id: [u8; 16]) -> Result<(), String> {
    require_continuity(ContinuityCapability::FileTransfer)?;
    let pending = nearby_drop_state()
        .lock()
        .map_err(|_| "nearby transfer state is unavailable")?
        .large_offers
        .get(&transfer_id)
        .cloned()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-transfer-inactive"))?;
    let (target, request_id, _) = pending;
    let _ = network.cancel_nearby_transfer_stream(&target, transfer_id);
    network.send_personal_fabric_action(
        &target,
        PersonalFabricAction::NearbyTransferCancel {
            request_id,
            transfer_id,
        },
    )?;
    with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id))?;
    if let Ok(mut state) = nearby_drop_state().lock() {
        state.large_offers.remove(&transfer_id);
        state.progress.remove(&transfer_id);
    }
    Ok(())
}

/// Resolve one receiver-side offer after explicit Files UI consent.
///
/// # Errors
///
/// Returns an error for an unknown/expired offer or when durable receive state
/// cannot be created.
#[expect(
    clippy::too_many_lines,
    reason = "the consent decision validates, stages and replies in one ordered path"
)]
pub fn decide_nearby_transfer_at(
    network: &ShellNetwork,
    transfer_id: [u8; 16],
    accept: bool,
    destination: Option<crate::shell::nearby_transfer::SecureNearbyDestination>,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::FileTransfer)?;
    if accept && destination.is_none() {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-destination-unapproved",
        ));
    }
    let pending = nearby_drop_state()
        .lock()
        .map_err(|_| "nearby transfer state is unavailable".to_string())?
        .incoming
        .remove(&transfer_id);
    let Some(pending) = pending else {
        if !accept {
            let declined = {
                let mut state = nearby_drop_state()
                    .lock()
                    .map_err(|_| "nearby transfer state is unavailable".to_string())?;
                with_nearby_transfer_store(|store| {
                    receive_recovery::decline_recovered_receive(store, &mut state, transfer_id)
                })
            }?;
            declined.notify(|peer, action| network.send_personal_fabric_action(peer, action));
            return Ok(());
        }
        let destination = destination
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-destination-expired"))?;
        let resumed = with_nearby_transfer_store(|store| {
            // A media handoff is never delivered to a Files destination.
            let entry = store
                .inbox()
                .iter()
                .find(|entry| {
                    entry.id == transfer_id
                        && entry.kind != crate::shell::nearby_transfer::TransferKind::MediaHandoff
                })
                .cloned()
                .ok_or_else(|| {
                    crate::shell::i18n::t("device-fabric-error-transfer-unrecoverable")
                })?;
            let (committed_offset, prefix_hash) = store.begin_incoming(
                crate::shell::nearby_transfer::IncomingManifest {
                    id: entry.id,
                    source: entry.source.clone(),
                    request_id: entry.request_id,
                    name: entry.name.clone(),
                    kind: entry.kind,
                    total_bytes: entry.total_bytes,
                    content_hash: entry.content_hash,
                },
                unix_now_ms(),
            )?;
            Ok((entry, committed_offset, prefix_hash))
        })?;
        let source = crate::shell::personal_continuity::DeviceId::new(resumed.0.source)
            .map_err(|error| error.to_string())?;
        nearby_drop_state()
            .lock()
            .map_err(|_| "nearby transfer state is unavailable".to_string())?
            .destinations
            .insert(transfer_id, destination);
        network.send_personal_fabric_action(
            &source,
            PersonalFabricAction::NearbyTransferReady {
                request_id: resumed.0.request_id,
                transfer_id,
                committed_offset: resumed.1,
                prefix_hash: resumed.2,
            },
        )?;
        return Ok(());
    };
    match pending.payload {
        PendingNearbyPayload::Inline {
            bytes,
            content_hash,
        } => {
            let install_result = accept.then(|| {
                if bytes.is_empty()
                    || bytes.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES
                    || blake3::hash(&bytes).as_bytes() != &content_hash
                {
                    return Err("invalid nearby drop".into());
                }
                destination
                    .as_ref()
                    .ok_or_else(|| {
                        crate::shell::i18n::t("device-fabric-error-destination-expired")
                    })?
                    .install_inline(&pending.name, &bytes)
            });
            let accepted = install_result.as_ref().is_some_and(Result::is_ok);
            if let Some(Ok(path)) = install_result.as_ref()
                && let Ok(mut state) = nearby_drop_state().lock()
            {
                state.events.push_back(NearbyDropEvent::Received {
                    path: path.clone(),
                    peer: pending.source.clone(),
                });
            }
            if let Ok(mut state) = nearby_drop_state().lock() {
                state.record_history(NearbyTransferHistory {
                    transfer_id,
                    peer: pending.source.clone(),
                    name: pending.name.clone(),
                    phase: if accepted {
                        runtime_collab::NearbyTransferPhase::Completed
                    } else if accept {
                        runtime_collab::NearbyTransferPhase::Failed
                    } else {
                        runtime_collab::NearbyTransferPhase::Cancelled
                    },
                    total_bytes: pending.total_bytes,
                });
            }
            network.send_personal_fabric_action(
                &pending.source,
                PersonalFabricAction::NearbyDropResult {
                    request_id: pending.request_id,
                    file_name: pending.name,
                    accepted,
                },
            )?;
            if let Some(Err(error)) = install_result {
                return Err(error);
            }
        }
        PendingNearbyPayload::Durable { kind, content_hash } => {
            if !accept {
                let discarded = with_nearby_transfer_store(|store| {
                    store.decline_incoming_offer(&crate::shell::nearby_transfer::IncomingManifest {
                        id: transfer_id,
                        source: pending.source.as_str().to_string(),
                        request_id: pending.request_id,
                        name: pending.name.clone(),
                        kind,
                        total_bytes: pending.total_bytes,
                        content_hash,
                    })
                })?;
                if let Ok(mut state) = nearby_drop_state().lock() {
                    if discarded {
                        state.destinations.remove(&transfer_id);
                        state.progress.remove(&transfer_id);
                    }
                    state.record_history(NearbyTransferHistory {
                        transfer_id,
                        peer: pending.source.clone(),
                        name: pending.name.clone(),
                        phase: runtime_collab::NearbyTransferPhase::Cancelled,
                        total_bytes: pending.total_bytes,
                    });
                }
                network.send_personal_fabric_action(
                    &pending.source,
                    PersonalFabricAction::NearbyTransferResult {
                        request_id: pending.request_id,
                        transfer_id,
                        accepted: false,
                    },
                )?;
                return Ok(());
            }
            let (committed_offset, prefix_hash) = with_nearby_transfer_store(|store| {
                store.accept_incoming_offer(
                    crate::shell::nearby_transfer::IncomingManifest {
                        id: transfer_id,
                        source: pending.source.as_str().to_string(),
                        request_id: pending.request_id,
                        name: pending.name,
                        kind,
                        total_bytes: pending.total_bytes,
                        content_hash,
                    },
                    unix_now_ms(),
                )
            })?;
            let destination = destination
                .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-destination-expired"))?;
            nearby_drop_state()
                .lock()
                .map_err(|_| "nearby transfer state is unavailable".to_string())?
                .destinations
                .insert(transfer_id, destination);
            network.send_personal_fabric_action(
                &pending.source,
                PersonalFabricAction::NearbyTransferReady {
                    request_id: pending.request_id,
                    transfer_id,
                    committed_offset,
                    prefix_hash,
                },
            )?;
        }
    }
    Ok(())
}

/// Make one durable failed/queued send eligible for immediate re-offer.
///
/// # Errors
///
/// Returns an error when the transfer no longer exists in the durable outbox.
pub fn retry_nearby_transfer(network: &ShellNetwork, transfer_id: [u8; 16]) -> Result<(), String> {
    require_continuity(ContinuityCapability::FileTransfer)?;
    let exists = try_outbox_entries().iter().any(|entry| {
        entry.id == transfer_id
            && entry.kind != crate::shell::nearby_transfer::TransferKind::MediaHandoff
    });
    if !exists {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-transfer-no-retry",
        ));
    }
    if let Ok(mut state) = nearby_drop_state().lock() {
        state.large_offers.remove(&transfer_id);
        state.progress.remove(&transfer_id);
        state.last_offer.remove(&transfer_id);
    }
    resume_nearby_transfers(network, ContinuityPolicySnapshot::load());
    Ok(())
}

/// Project path-free, identity-redacted transfer state for Files.
pub struct NearbyTransferRuntimeStatus {
    pub transfer_id: [u8; 16],
    pub name: String,
    pub device_label: String,
    pub direction: runtime_collab::NearbyTransferDirection,
    pub phase: runtime_collab::NearbyTransferPhase,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
}

#[expect(
    clippy::too_many_lines,
    reason = "one projection maps every transfer phase to its status row"
)]
pub fn nearby_transfer_statuses(network: &ShellNetwork) -> Vec<NearbyTransferRuntimeStatus> {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::FileTransfer) {
        return Vec::new();
    }
    expire_nearby_consents(network);
    let fabric = network.device_fabric();
    let label = |device: &crate::shell::personal_continuity::DeviceId| {
        fabric.snapshots().get(device).map_or_else(
            || crate::shell::i18n::t("device-fabric-nearby-device"),
            |snapshot| snapshot.presence.label.clone(),
        )
    };
    let Ok(state) = nearby_drop_state().lock() else {
        return Vec::new();
    };
    let mut statuses = state
        .incoming
        .iter()
        .map(|(transfer_id, pending)| NearbyTransferRuntimeStatus {
            transfer_id: *transfer_id,
            name: pending.name.clone(),
            device_label: label(&pending.source),
            direction: runtime_collab::NearbyTransferDirection::Receiving,
            phase: runtime_collab::NearbyTransferPhase::AwaitingConsent,
            transferred_bytes: 0,
            total_bytes: pending.total_bytes,
        })
        .collect::<Vec<_>>();
    statuses.extend(
        state
            .pending
            .iter()
            .map(
                |(transfer_id, (target, name, total_bytes))| NearbyTransferRuntimeStatus {
                    transfer_id: *transfer_id,
                    name: name.clone(),
                    device_label: label(target),
                    direction: runtime_collab::NearbyTransferDirection::Sending,
                    phase: runtime_collab::NearbyTransferPhase::Queued,
                    transferred_bytes: 0,
                    total_bytes: *total_bytes,
                },
            ),
    );
    statuses.extend(
        state
            .history
            .iter()
            .map(|entry| NearbyTransferRuntimeStatus {
                transfer_id: entry.transfer_id,
                name: entry.name.clone(),
                device_label: label(&entry.peer),
                direction: runtime_collab::NearbyTransferDirection::Receiving,
                phase: entry.phase,
                transferred_bytes: if entry.phase == runtime_collab::NearbyTransferPhase::Completed
                {
                    entry.total_bytes
                } else {
                    0
                },
                total_bytes: entry.total_bytes,
            }),
    );
    statuses.truncate(runtime_collab::MAX_NEARBY_TRANSFER_STATUSES);
    // A media handoff's object reports through the handoff, not Files.
    let nearby_send = |kind: crate::shell::nearby_transfer::TransferKind| {
        kind != crate::shell::nearby_transfer::TransferKind::MediaHandoff
    };
    for entry in try_inbox_entries()
        .into_iter()
        .filter(|entry| nearby_send(entry.kind))
    {
        if statuses.len() >= runtime_collab::MAX_NEARBY_TRANSFER_STATUSES {
            break;
        }
        let Ok(source) = crate::shell::personal_continuity::DeviceId::new(entry.source) else {
            continue;
        };
        statuses.push(NearbyTransferRuntimeStatus {
            transfer_id: entry.id,
            name: entry.name,
            device_label: label(&source),
            direction: runtime_collab::NearbyTransferDirection::Receiving,
            phase: if state.destinations.contains_key(&entry.id) {
                runtime_collab::NearbyTransferPhase::Transferring
            } else {
                runtime_collab::NearbyTransferPhase::AwaitingDestination
            },
            transferred_bytes: entry.committed_offset.min(entry.total_bytes),
            total_bytes: entry.total_bytes,
        });
    }
    let outbox = try_outbox_entries();
    for entry in outbox.into_iter().filter(|entry| nearby_send(entry.kind)) {
        if statuses.len() >= runtime_collab::MAX_NEARBY_TRANSFER_STATUSES {
            break;
        }
        let Ok(target) = crate::shell::personal_continuity::DeviceId::new(entry.target) else {
            continue;
        };
        let (sent, total, phase) = state.progress.get(&entry.id).map_or(
            (
                0,
                entry.total_bytes,
                runtime_collab::NearbyTransferPhase::Queued,
            ),
            |(_, sent, total, status)| {
                let phase = match status {
                    PersonalTransferStatus::Started | PersonalTransferStatus::Progress => {
                        runtime_collab::NearbyTransferPhase::Transferring
                    }
                    PersonalTransferStatus::Completed => {
                        runtime_collab::NearbyTransferPhase::Completed
                    }
                    PersonalTransferStatus::Failed | PersonalTransferStatus::Cancelled => {
                        runtime_collab::NearbyTransferPhase::Failed
                    }
                };
                (*sent, *total, phase)
            },
        );
        statuses.push(NearbyTransferRuntimeStatus {
            transfer_id: entry.id,
            name: entry.name,
            device_label: label(&target),
            direction: runtime_collab::NearbyTransferDirection::Sending,
            phase,
            transferred_bytes: sent.min(total),
            total_bytes: total,
        });
    }
    statuses.truncate(runtime_collab::MAX_NEARBY_TRANSFER_STATUSES);
    statuses
}

/// Outgoing streams on the one transfer lane, Nearby sends and media
/// handoffs alike.
fn fabric_transfer_views() -> Vec<crate::shell::overlay_host::FabricTransferView> {
    use crate::network::actor::PersonalTransferStatus;
    use crate::shell::overlay_host::{FabricTransferStatus, FabricTransferView};

    nearby_drop_state().lock().map_or_else(
        |_| Vec::new(),
        |state| {
            state
                .progress
                .values()
                .cloned()
                .map(
                    |(target, sent_bytes, total_bytes, status)| FabricTransferView {
                        target,
                        sent_bytes,
                        total_bytes,
                        status: match status {
                            PersonalTransferStatus::Started | PersonalTransferStatus::Progress => {
                                FabricTransferStatus::Sending
                            }
                            PersonalTransferStatus::Completed => FabricTransferStatus::Sent,
                            PersonalTransferStatus::Failed | PersonalTransferStatus::Cancelled => {
                                FabricTransferStatus::Failed
                            }
                        },
                    },
                )
                .collect::<Vec<_>>()
        },
    )
}

fn bluetooth_authorization_views(
    fabric: &crate::shell::device_fabric::DeviceFabric,
) -> Vec<crate::shell::overlay_host::BluetoothHandoffAuthorizationView> {
    use crate::shell::overlay_host::BluetoothHandoffAuthorizationView;

    let mut authorizations = bluetooth_handoff_state().lock().map_or_else(
        |_| Vec::new(),
        |state| {
            state
                .transactions
                .iter()
                .filter(|(_, transaction)| {
                    !transaction.outbound
                        && transaction.phase == BluetoothHandoffPhase::AwaitingLocalAuthorization
                })
                .map(
                    |(request_id, transaction)| BluetoothHandoffAuthorizationView {
                        request_id: *request_id,
                        accessory_name: transaction.name.clone(),
                        source_label: fabric.snapshots().get(&transaction.peer).map_or_else(
                            || transaction.peer.as_str().to_string(),
                            |snapshot| snapshot.presence.label.clone(),
                        ),
                    },
                )
                .collect::<Vec<_>>()
        },
    );
    authorizations.sort_by(|left, right| {
        left.source_label
            .cmp(&right.source_label)
            .then_with(|| left.accessory_name.cmp(&right.accessory_name))
    });
    authorizations
}

fn bluetooth_handoff_status_views(
    fabric: &crate::shell::device_fabric::DeviceFabric,
) -> Vec<BluetoothHandoffStatusView> {
    let Ok(mut state) = bluetooth_handoff_state().lock() else {
        return Vec::new();
    };
    state
        .outcomes
        .retain(|_, outcome| outcome.updated.elapsed() < BLUETOOTH_HANDOFF_HISTORY_TTL);
    let label = |transaction: &BluetoothHandoffTransaction| {
        fabric.snapshots().get(&transaction.peer).map_or_else(
            || transaction.peer.as_str().to_string(),
            |snapshot| snapshot.presence.label.clone(),
        )
    };
    let mut statuses = state
        .transactions
        .iter()
        .map(|(request_id, transaction)| BluetoothHandoffStatusView {
            request_id: *request_id,
            accessory_name: transaction.name.clone(),
            peer_label: label(transaction),
            outbound: transaction.outbound,
            phase: transaction.view_phase(),
        })
        .chain(
            state
                .outcomes
                .iter()
                .map(|(request_id, outcome)| BluetoothHandoffStatusView {
                    request_id: *request_id,
                    accessory_name: outcome.transaction.name.clone(),
                    peer_label: label(&outcome.transaction),
                    outbound: outcome.transaction.outbound,
                    phase: outcome.phase,
                }),
        )
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| {
        left.accessory_name
            .cmp(&right.accessory_name)
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    statuses
}

fn gamepad_authorization_views(
    fabric: &crate::shell::device_fabric::DeviceFabric,
) -> Vec<crate::shell::overlay_host::GamepadAuthorizationView> {
    let mut authorizations = pending_typed_gamepad_authorizations()
        .into_iter()
        .map(
            |request| crate::shell::overlay_host::GamepadAuthorizationView {
                forwarding_id: request.forwarding_id,
                controller_label: request.label,
                source_label: fabric.snapshots().get(&request.source).map_or_else(
                    || request.source.as_str().to_string(),
                    |snapshot| snapshot.presence.label.clone(),
                ),
                app_id: request.app_id,
            },
        )
        .collect::<Vec<_>>();
    authorizations.sort_by(|left, right| {
        left.source_label
            .cmp(&right.source_label)
            .then_with(|| left.controller_label.cmp(&right.controller_label))
    });
    authorizations
}

fn active_spatial_control_devices(
    now_unix_ms: u64,
) -> (
    Vec<crate::shell::personal_continuity::DeviceId>,
    Vec<crate::shell::personal_continuity::DeviceId>,
) {
    spatial_input_state().lock().map_or_else(
        |_| (Vec::new(), Vec::new()),
        |spatial| {
            (
                spatial
                    .issued
                    .values()
                    .filter(|lease| lease.validate_at(now_unix_ms).is_ok())
                    .map(|lease| lease.controller.clone())
                    .collect(),
                spatial
                    .received
                    .values()
                    .filter(|lease| lease.validate_at(now_unix_ms).is_ok())
                    .map(|lease| lease.target.clone())
                    .collect(),
            )
        },
    )
}

/// The person's certified devices as the personal fabric sees them, for
/// running each of their schedules on one device.
pub struct FabricDeviceDirectory {
    network: Arc<ShellNetwork>,
}

impl FabricDeviceDirectory {
    #[must_use]
    pub fn new(network: Arc<ShellNetwork>) -> Self {
        Self { network }
    }
}

impl crate::shell::automation_triggers::DeviceDirectory for FabricDeviceDirectory {
    fn personal_devices(&self) -> crate::shell::automation_triggers::PersonalDevices {
        // Borrowed, not `device_fabric()`: that clones every snapshot, and
        // this runs on the compositor thread on every trigger observation.
        crate::shell::automation_triggers::PersonalDevices {
            local: self
                .network
                .local_device_fabric_identity()
                .map(|(device, _)| device.as_str().to_string()),
            online: self
                .network
                .with_device_fabric(|fabric| {
                    fabric
                        .online_devices()
                        .map(|presence| presence.device.as_str().to_string())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// Build the bounded presentation model for Quick Settings. Every remote row
/// comes from a snapshot authored by that certified device; local `PipeWire`
/// outputs are deliberately kept separate from personal-fabric identities.
///
/// The local outputs and inputs come from `graph`, which the caller keeps
/// cached: enumerating `PipeWire` is a subprocess, and this view is rebuilt
/// on the compositor thread.
#[expect(
    clippy::too_many_lines,
    reason = "one projection builds every Quick Settings fabric row from the same snapshot"
)]
pub fn device_fabric_view(
    network: &ShellNetwork,
    graph: media::audio::AudioGraph,
) -> crate::shell::overlay_host::DeviceFabricView {
    use crate::shell::overlay_host::{
        DeviceFabricView, FabricDeviceView, FabricMediaView, LocalMediaInputView,
        LocalMediaOutputView,
    };

    let policy = ContinuityPolicySnapshot::load();
    let fabric = network.device_fabric();
    let local_id = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let (issued_to, received_from) = if policy.permits(ContinuityCapability::DisplayInput) {
        active_spatial_control_devices(unix_now_ms())
    } else {
        (Vec::new(), Vec::new())
    };
    let mut devices = fabric
        .snapshots()
        .values()
        .map(|snapshot| {
            let caps = snapshot.presence.capabilities;
            FabricDeviceView {
                id: snapshot.owner.clone(),
                label: snapshot.presence.label.clone(),
                kind: snapshot.presence.kind,
                is_local: local_id.as_ref() == Some(&snapshot.owner),
                online: snapshot.presence.online,
                audio_output: caps.audio_output,
                video_output: caps.video_output,
                display: caps.display,
                controls_local: issued_to.contains(&snapshot.owner),
                local_can_control: received_from.contains(&snapshot.owner),
            }
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        right
            .is_local
            .cmp(&left.is_local)
            .then_with(|| right.online.cmp(&left.online))
            .then_with(|| left.label.cmp(&right.label))
    });

    let local_media = if policy.permits(ContinuityCapability::Media) {
        local_id
            .as_ref()
            .and_then(|device| fabric.snapshots().get(device))
            .map(|snapshot| {
                snapshot
                    .media
                    .iter()
                    .map(|session| FabricMediaView {
                        session_id: session.id.clone(),
                        title: session.title.clone(),
                        playing: session.playing,
                        has_video: session.has_video,
                        targets: devices
                            .iter()
                            .filter(|device| {
                                !device.is_local
                                    && fabric.handoff_media(&session.id, &device.id).is_ok()
                            })
                            .map(|device| device.id.clone())
                            .collect(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut local_inputs = graph
        .sources
        .iter()
        .map(|source| LocalMediaInputView {
            node_id: source.id,
            label: source.description.clone(),
            is_default: source.is_default,
            level_percent: source.volume.map(input_level_percent),
            muted: source.muted.unwrap_or(false),
        })
        .collect::<Vec<_>>();
    local_inputs.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.label.cmp(&right.label))
    });
    let mut local_outputs = graph
        .sinks
        .into_iter()
        .map(|sink| LocalMediaOutputView {
            node_id: sink.id,
            label: sink.description,
            is_default: sink.is_default,
            bluetooth_codec: sink.codec,
        })
        .collect::<Vec<_>>();
    local_outputs.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.label.cmp(&right.label))
    });

    let transfers = if policy.permits(ContinuityCapability::FileTransfer)
        || policy.permits(ContinuityCapability::Media)
    {
        fabric_transfer_views()
    } else {
        Vec::new()
    };
    let bluetooth_authorizations = if policy.permits(ContinuityCapability::Media) {
        bluetooth_authorization_views(&fabric)
    } else {
        Vec::new()
    };
    let gamepad_authorizations = if policy.permits(ContinuityCapability::TypedGamepad) {
        gamepad_authorization_views(&fabric)
    } else {
        Vec::new()
    };
    let (device_health, calibration_proposals, revertible_calibrations) = if policy
        .permits(ContinuityCapability::DisplayInput)
    {
        let mut route_counts = BTreeMap::<_, u8>::new();
        for peer in issued_to.iter().chain(received_from.iter()) {
            let count = route_counts.entry(peer.clone()).or_default();
            *count = count.saturating_add(1);
        }
        for transfer in &transfers {
            if transfer.status == crate::shell::overlay_host::FabricTransferStatus::Sending {
                let count = route_counts.entry(transfer.target.clone()).or_default();
                *count = count.saturating_add(1);
            }
        }
        if policy.permits(ContinuityCapability::Media) {
            for route in crate::shell::remote_audio::active_routes() {
                let count = route_counts.entry(route.peer).or_default();
                *count = count.saturating_add(1);
            }
        }
        let now = Instant::now();
        let now_unix_ms = unix_now_ms();
        let observations = device_health_state().lock().map_or_else(
            |_| Vec::new(),
            |state| {
                fabric
                    .snapshots()
                    .values()
                    .filter(|snapshot| local_id.as_ref() != Some(&snapshot.owner))
                    .map(|snapshot| {
                        state.tracker.observation(
                            &snapshot.owner,
                            true,
                            snapshot.presence.online,
                            route_counts
                                .get(&snapshot.owner)
                                .copied()
                                .unwrap_or_default(),
                            now,
                            now_unix_ms,
                            snapshot.presence.last_seen_unix_ms,
                        )
                    })
                    .collect::<Vec<_>>()
            },
        );
        let health = crate::shell::spatial_calibration::summarize_health(observations, now_unix_ms);
        let proposals = spatial_calibration_proposals(network);
        let reverts = spatial_calibration_state()
            .lock()
            .map_or_else(|_| Vec::new(), |state| state.revertible_peers());
        (health, proposals, reverts)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };

    DeviceFabricView {
        displays: if policy.permits(ContinuityCapability::DisplayInput) {
            observed_device_displays(&fabric, local_id.as_ref())
        } else {
            Vec::new()
        },
        bluetooth_authorizations,
        bluetooth_handoffs: bluetooth_handoff_status_views(&fabric),
        gamepad_authorizations,
        local_gamepads: Vec::new(),
        devices,
        local_media,
        media_handoffs: media_handoff_status_views(network),
        local_outputs,
        local_inputs,
        transfers,
        remote_audio: crate::shell::remote_audio::RemoteAudioView::default(),
        speaker_groups: if policy.permits(ContinuityCapability::Media) {
            crate::shell::speaker_group_runtime::view(network)
        } else {
            crate::shell::speaker_group_runtime::SpeakerGroupsView::default()
        },
        device_health,
        calibration_proposals,
        revertible_calibrations,
        tethering: crate::shell::personal_tethering_runtime::tethering_statuses(),
        remote_capture: crate::shell::remote_capture::runtime::active_view(),
        device_unlocks: crate::shell::device_unlock_runtime::pending_device_unlocks(),
    }
}

/// Route existing local playback streams and future default-following streams
/// to one currently enumerated output, including a connected Bluetooth sink.
///
/// # Errors
///
/// Returns an error if the node disappeared or `PipeWire` rejects any graph
/// edit. The node id is resolved against a fresh graph before it is used.
pub fn route_local_audio(node_id: u32) -> Result<(), String> {
    let graph = media::audio::enumerate();
    let sink = graph
        .sink_by_id(node_id)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-output-gone"))?;
    let target_serial = sink.serial;
    let control = media::audio::real_control();
    control.set_default_sink(node_id)?;
    for stream in graph.playback_streams {
        control.route_stream(stream.id, target_serial)?;
    }
    Ok(())
}

/// An input level in `0.0..=1.0` as a whole percentage.
#[must_use]
pub fn input_level_percent(level: f32) -> u8 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the level is clamped to 0..=1 first, so the percentage is 0..=100"
    )]
    let percent = (level.clamp(0.0, 1.0) * 100.0).round() as u8;
    percent
}

/// How far one step of the input level moves.
const INPUT_LEVEL_STEP: f32 = 0.05;

/// Record from one currently enumerated input from now on: make it the
/// default source and move every running capture stream onto it.
///
/// # Errors
///
/// Returns an error if the input disappeared or `PipeWire` rejects any graph
/// edit. The node id is resolved against a fresh graph before it is used.
pub fn select_local_input(node_id: u32) -> Result<(), String> {
    let graph = media::audio::enumerate();
    let source = graph
        .sources
        .iter()
        .find(|source| source.id == node_id)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-input-gone"))?;
    let target_serial = source.serial;
    let control = media::audio::real_control();
    control.set_default_source(node_id)?;
    for stream in graph.record_streams {
        control.route_stream(stream.id, target_serial)?;
    }
    Ok(())
}

/// The input level one step up or down from `current`, within `0..=1`.
#[must_use]
pub fn stepped_input_level(current: Option<f32>, raise: bool) -> f32 {
    let current = current.unwrap_or(0.0);
    let next = if raise {
        current + INPUT_LEVEL_STEP
    } else {
        current - INPUT_LEVEL_STEP
    };
    ((next / INPUT_LEVEL_STEP).round() * INPUT_LEVEL_STEP).clamp(0.0, 1.0)
}

#[cfg(test)]
#[test]
fn input_level_steps_snap_to_the_grid_and_stay_in_range() {
    assert!((stepped_input_level(Some(0.62), true) - 0.65).abs() < 1e-6);
    assert!((stepped_input_level(Some(0.62), false) - 0.55).abs() < 1e-6);
    assert!((stepped_input_level(Some(0.99), true) - 1.0).abs() < 1e-6);
    assert!(stepped_input_level(Some(0.01), false).abs() < 1e-6);
    assert!((stepped_input_level(None, true) - 0.05).abs() < 1e-6);
    assert_eq!(input_level_percent(0.625), 63);
    assert_eq!(input_level_percent(7.0), 100);
}

/// Raise or lower one input's level by a step. Raising a muted input unmutes
/// it: reaching for "louder" means "let them hear me".
///
/// # Errors
///
/// Returns an error if the input disappeared or `wpctl` rejects the change.
pub fn step_input_level(node_id: u32, raise: bool) -> Result<(), String> {
    let graph = media::audio::enumerate();
    let source = graph
        .sources
        .iter()
        .find(|source| source.id == node_id)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-input-gone"))?;
    let control = media::audio::real_control();
    control.set_node_volume(node_id, stepped_input_level(source.volume, raise))?;
    if raise && source.muted == Some(true) {
        control.set_node_mute(node_id, false)?;
    }
    Ok(())
}

fn calibration_devices(
    network: &ShellNetwork,
) -> Vec<crate::shell::spatial_calibration::CalibrationDevice> {
    network
        .device_fabric()
        .snapshots()
        .values()
        .map(
            |snapshot| crate::shell::spatial_calibration::CalibrationDevice {
                id: snapshot.owner.clone(),
                certified: true,
                online: snapshot.presence.online,
                display: snapshot.presence.capabilities.display,
                rect: snapshot.display.as_ref().map(|display| display.rect),
            },
        )
        .collect()
}

fn spatial_calibration_proposals(
    network: &ShellNetwork,
) -> Vec<crate::shell::spatial_calibration::CalibrationProposal> {
    if require_continuity(ContinuityCapability::DisplayInput).is_err() {
        return Vec::new();
    }
    let Some((local, _)) = network.local_device_fabric_identity() else {
        return Vec::new();
    };
    spatial_calibration_state().lock().map_or_else(
        |_| Vec::new(),
        |state| state.proposals(&local, &calibration_devices(network), unix_now_ms()),
    )
}

/// Apply one still-current proposal. The target authors its own placement and
/// the previous exact rectangle is retained locally for one explicit revert.
///
/// # Errors
///
/// Returns a message when no current proposal exists for `target`, the
/// placement cannot be sent, or calibration state is unavailable.
pub fn apply_spatial_calibration(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
    let proposal = spatial_calibration_proposals(network)
        .into_iter()
        .find(|proposal| &proposal.peer == target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-proposal-stale"))?;
    network.send_personal_fabric_action(
        target,
        PersonalFabricAction::SetDisplayPlacement {
            rect: proposal.proposed,
        },
    )?;
    spatial_calibration_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-spatial-unavailable"))?
        .remember_apply(&proposal);
    Ok(())
}

/// Drop the current calibration proposal for `target`.
///
/// # Errors
///
/// Returns a message when calibration state is unavailable.
pub fn dismiss_spatial_calibration(
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
    spatial_calibration_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-spatial-unavailable"))?
        .dismiss(target);
    Ok(())
}

/// Restore the placement `target` had before its last applied calibration.
///
/// # Errors
///
/// Returns a message when display continuity is not enabled, calibration
/// state is unavailable, nothing was applied, or the placement cannot be sent.
pub fn revert_spatial_calibration(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::DisplayInput)?;
    let previous = spatial_calibration_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-spatial-unavailable"))?
        .revert_rect(target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-applied-calibration"))?;
    network.send_personal_fabric_action(
        target,
        PersonalFabricAction::SetDisplayPlacement { rect: previous },
    )?;
    spatial_calibration_state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-spatial-unavailable"))?
        .finish_revert(target);
    Ok(())
}

/// Place a certified display immediately beside the requested edge. The
/// receiving device authors the resulting placement in its own next snapshot.
///
/// # Errors
///
/// Returns an error if either display is unavailable or the exact-target send
/// fails.
pub fn place_device(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    edge: DisplayEdge,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::DisplayInput)?;
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let fabric = network.device_fabric();
    let local_rect = fabric
        .snapshots()
        .get(&local)
        .and_then(|snapshot| snapshot.display.as_ref())
        .map(|display| display.rect)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-placement"))?;
    let target_snapshot = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-target-not-enrolled"))?;
    if !target_snapshot.presence.online
        || !target_snapshot.presence.capabilities.display
        || !target_snapshot.presence.capabilities.pointer
    {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-target-not-display",
        ));
    }
    let target_rect = target_snapshot
        .display
        .as_ref()
        .map(|display| display.rect)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-target-no-display"))?;
    let target_width = i32::try_from(target_rect.width).unwrap_or(i32::MAX);
    let target_height = i32::try_from(target_rect.height).unwrap_or(i32::MAX);
    let rect = match edge {
        DisplayEdge::Left => DisplayRect {
            x: local_rect.x.saturating_sub(target_width),
            y: local_rect.y,
            width: target_rect.width,
            height: target_rect.height,
        },
        DisplayEdge::Right => DisplayRect {
            x: local_rect.x.saturating_add_unsigned(local_rect.width),
            y: local_rect.y,
            width: target_rect.width,
            height: target_rect.height,
        },
        DisplayEdge::Top => DisplayRect {
            x: local_rect.x,
            y: local_rect.y.saturating_sub(target_height),
            width: target_rect.width,
            height: target_rect.height,
        },
        DisplayEdge::Bottom => DisplayRect {
            x: local_rect.x,
            y: local_rect.y.saturating_add_unsigned(local_rect.height),
            width: target_rect.width,
            height: target_rect.height,
        },
    };
    if fabric.snapshots().iter().any(|(device, snapshot)| {
        device != &local
            && device != target
            && snapshot
                .display
                .as_ref()
                .is_some_and(|display| rect.intersects(display.rect))
    }) {
        return Err(crate::shell::i18n::t("device-fabric-error-side-taken"));
    }
    network
        .send_personal_fabric_action(target, PersonalFabricAction::SetDisplayPlacement { rect })
        .map(|_| ())
}

/// Request a translation over an exact certified connector observation.
/// This does not issue or renew an input lease.
/// # Errors
/// Rejects offline, local, stale, overlapping or invalid display observations.
pub fn move_device_display(
    network: &ShellNetwork,
    request: &settings_store::device_displays::DeviceDisplayMove,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::DisplayInput)?;
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let fabric = network.device_fabric();
    let (target, expected, rect) = checked_display_move(&fabric, &local, request)
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-layout-changed"))?;
    let route = network
        .current_personal_route(&target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-layout-changed"))?;
    network
        .send_personal_fabric_action_on_route(
            &route,
            PersonalFabricAction::SetDisplayPlacementChecked {
                expected,
                rect,
                expires_unix_ms: unix_now_ms()
                    .saturating_add(crate::shell::device_fabric::MAX_DISPLAY_MOVE_AGE_MS),
            },
        )
        .map(|_| ())
}

fn checked_display_move(
    fabric: &DeviceFabric,
    local: &crate::shell::personal_continuity::DeviceId,
    request: &settings_store::device_displays::DeviceDisplayMove,
) -> Result<
    (
        crate::shell::personal_continuity::DeviceId,
        crate::shell::device_fabric::FabricStamp,
        DisplayRect,
    ),
    crate::shell::device_fabric::FabricError,
> {
    use crate::shell::device_fabric::FabricError;
    let target = crate::shell::personal_continuity::DeviceId::new(request.screen.device.clone())
        .map_err(|_| FabricError::InvalidIdentifier)?;
    let snapshot = fabric
        .snapshots()
        .get(&target)
        .ok_or(FabricError::UnknownDevice)?;
    if target == *local || !snapshot.presence.online || !snapshot.presence.capabilities.display {
        return Err(FabricError::InvalidDisplayPlacement);
    }
    if !observed_device_displays(fabric, Some(local)).contains(&request.screen) {
        return Err(FabricError::ConflictingRevision);
    }
    let display = snapshot
        .display
        .as_ref()
        .ok_or(FabricError::InvalidDisplayPlacement)?;
    let rect = DisplayRect {
        x: display
            .rect
            .x
            .checked_add(
                request
                    .x
                    .checked_sub(request.screen.x)
                    .ok_or(FabricError::InvalidDisplayPlacement)?,
            )
            .ok_or(FabricError::InvalidDisplayPlacement)?,
        y: display
            .rect
            .y
            .checked_add(
                request
                    .y
                    .checked_sub(request.screen.y)
                    .ok_or(FabricError::InvalidDisplayPlacement)?,
            )
            .ok_or(FabricError::InvalidDisplayPlacement)?,
        ..display.rect
    };
    if !display.accepts_move(&display.stamp, rect) {
        return Err(FabricError::InvalidDisplayPlacement);
    }
    if !fabric.display_destination_clear(&target, rect) {
        return Err(FabricError::InvalidDisplayPlacement);
    }
    Ok((target, display.stamp.clone(), rect))
}

/// Explicitly allow one certified device to control this device until the
/// short lease expires or is revoked.
///
/// # Errors
///
/// Returns an error when either identity/capability is unavailable, entropy
/// fails, or the exact-target grant cannot be delivered.
pub fn grant_input_control(
    network: &ShellNetwork,
    controller: &crate::shell::personal_continuity::DeviceId,
) -> Result<InputLease, String> {
    let _operation = input_control_operations()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-input-operation-unavailable"))?;
    let lease = grant_input_control_inner(network, controller, false)?;
    display_auto_leases::record_decline(network, controller, false)?;
    Ok(lease)
}

fn grant_input_control_automatic(
    network: &ShellNetwork,
    controller: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
    let _operation = input_control_operations()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-input-operation-unavailable"))?;
    prune_spatial_leases(network);
    let policy = ContinuityPolicySnapshot::load();
    let view = person_layout::current(network);
    if !display_auto_leases::wanted_controllers(network, view.as_deref(), policy)
        .contains(controller)
    {
        return Ok(());
    }
    let Some(route) = network.current_personal_route(controller) else {
        return Ok(());
    };
    let renew = {
        let spatial = spatial_input_state()
            .lock()
            .map_err(|_| "spatial input state is unavailable".to_string())?;
        !spatial.automatic_route_withdrawn(&route)
            && spatial.automatic_needs_grant(controller, unix_now_ms())
    };
    if renew {
        grant_input_control_inner(network, controller, true)?;
    }
    Ok(())
}

fn grant_input_control_inner(
    network: &ShellNetwork,
    controller: &crate::shell::personal_continuity::DeviceId,
    automatic: bool,
) -> Result<InputLease, String> {
    require_continuity(ContinuityCapability::DisplayInput)?;
    if !network.personal_device_is_enrolled(controller) {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-controller-not-enrolled",
        ));
    }
    // Advance the authenticated-departure cursor before minting replacement
    // authority, so retained history cannot revoke a fresh reconnect lease.
    prune_spatial_leases(network);
    let (target, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let fabric = network.device_fabric();
    let presence = fabric
        .snapshots()
        .get(controller)
        .map(|snapshot| &snapshot.presence)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-controller-not-enrolled"))?;
    if !presence.online || !(presence.capabilities.pointer || presence.capabilities.keyboard) {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-controller-no-input",
        ));
    }
    let mut nonce = [0_u8; 16];
    SysRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| "secure randomness is unavailable".to_string())?;
    if nonce == [0; 16] {
        return Err("secure randomness returned an invalid lease nonce".into());
    }
    let now = unix_now_ms();
    let lease = InputLease {
        controller: controller.clone(),
        target,
        issued_unix_ms: now,
        expires_unix_ms: now.saturating_add(MAX_INPUT_LEASE_MS),
        nonce,
        allow_keyboard: presence.capabilities.keyboard,
        allow_pointer: presence.capabilities.pointer,
    };
    let route = network
        .current_personal_route(controller)
        .ok_or_else(|| "controller has no current certified route".to_string())?;
    network.send_personal_fabric_action_on_route(
        &route,
        PersonalFabricAction::InputLeaseGranted {
            lease: lease.clone(),
        },
    )?;
    if let Ok(mut spatial) = spatial_input_state().lock() {
        let replaced = spatial
            .issued
            .values()
            .any(|current| current.controller == *controller);
        spatial
            .issued
            .retain(|_, current| current.controller != *controller);
        if replaced {
            spatial.release_controller(controller);
        }
        let retained = spatial.issued.keys().copied().collect::<BTreeSet<_>>();
        spatial
            .issued_routes
            .retain(|nonce, _| retained.contains(nonce));
        spatial.issued_routes.insert(nonce, route);
        spatial.issued.insert(nonce, lease.clone());
        spatial.automatic_issued.remove(controller);
        if !automatic {
            spatial.resume_controller_explicitly(controller);
        }
        if automatic {
            spatial.automatic_issued.insert(controller.clone(), nonce);
        }
    }
    Ok(lease)
}

/// Revoke every active lease this device issued to `controller` and notify it
/// so edge forwarding stops immediately on both sides.
///
/// # Errors
///
/// Returns an error if no lease exists or a revoke cannot be delivered. Local
/// access is still revoked immediately if the peer cannot be reached.
pub fn revoke_input_control(
    network: &ShellNetwork,
    controller: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
    let _operation = input_control_operations()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-input-operation-unavailable"))?;
    let persisted = display_auto_leases::record_decline(network, controller, true);
    let revoked = revoke_input_control_inner(network, controller, None);
    revoked.and(persisted)
}

fn revoke_input_control_automatic(
    network: &ShellNetwork,
    controller: &crate::shell::personal_continuity::DeviceId,
    nonce: [u8; 16],
) -> Result<(), String> {
    let _operation = input_control_operations()
        .lock()
        .map_err(|_| crate::shell::i18n::t("device-fabric-error-input-operation-unavailable"))?;
    if !spatial_input_state()
        .lock()
        .is_ok_and(|state| state.automatic_lease_matches(controller, nonce))
    {
        return Ok(());
    }
    revoke_input_control_inner(network, controller, Some(nonce))
}

fn revoke_input_control_inner(
    network: &ShellNetwork,
    controller: &crate::shell::personal_continuity::DeviceId,
    expected: Option<[u8; 16]>,
) -> Result<(), String> {
    let leases = spatial_input_state()
        .lock()
        .map_err(|_| "spatial input state is unavailable".to_string())?
        .issued
        .values()
        .filter(|lease| {
            &lease.controller == controller && expected.is_none_or(|nonce| lease.nonce == nonce)
        })
        .cloned()
        .collect::<Vec<_>>();
    if leases.is_empty() {
        return Err(crate::shell::i18n::t("device-fabric-error-no-lease"));
    }
    let mut delivery_error = None;
    for lease in &leases {
        if let Err(error) = network.send_personal_fabric_action(
            controller,
            PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
        ) {
            delivery_error.get_or_insert(error);
        }
    }
    if let Ok(mut spatial) = spatial_input_state().lock() {
        let revoked = leases
            .iter()
            .map(|lease| lease.nonce)
            .collect::<BTreeSet<_>>();
        let removed = spatial.issued.keys().any(|nonce| revoked.contains(nonce));
        spatial.issued.retain(|nonce, _| !revoked.contains(nonce));
        spatial
            .issued_routes
            .retain(|nonce, _| !revoked.contains(nonce));
        spatial
            .automatic_issued
            .retain(|_, nonce| !revoked.contains(nonce));
        if removed {
            spatial.release_controller(controller);
        }
    }
    delivery_error.map_or(Ok(()), Err)
}

/// Current exact-controller pointer authority, never inferred from layout or
/// same-person identity. Gestures retain this nonce until they end.
#[must_use]
pub fn spatial_pointer_lease(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Option<InputLease> {
    require_continuity(ContinuityCapability::DisplayInput).ok()?;
    if !network.personal_device_is_enrolled(target) {
        return None;
    }
    prune_spatial_leases(network);
    let (controller, _) = network.local_device_fabric_identity()?;
    let (lease, route) = {
        let spatial = spatial_input_state().lock().ok()?;
        (
            spatial.received.get(target)?.clone(),
            spatial.received_routes.get(target)?.clone(),
        )
    };
    if !network.personal_route_is_current(&route) {
        return None;
    }
    (lease.binds(&controller, target)
        && lease.allow_pointer
        && lease.validate_at(unix_now_ms()).is_ok())
    .then_some(lease)
}

/// Queue an event only under the exact authority that began its gesture.
///
/// # Errors
/// Rejects withdrawn policy, identity, replaced/revoked/expired authority or
/// an event outside that lease's capabilities. Replacement grants never adopt
/// a gesture's held keys or buttons.
pub fn send_spatial_input_bound(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    nonce: [u8; 16],
    event: FabricInputEvent,
) -> Result<(), String> {
    with_spatial_input_authority(network, target, |spatial, controller| {
        spatial.queue_lease_bound_input(
            controller,
            OutboundSpatialInput {
                sequence: None,
                target: target.clone(),
                lease_nonce: nonce,
                event,
            },
            unix_now_ms(),
        )
    })
}

fn with_spatial_input_authority(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    operation: impl FnOnce(
        &mut SpatialInputState,
        &crate::shell::personal_continuity::DeviceId,
    ) -> Result<(), String>,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::DisplayInput)?;
    if !network.personal_device_is_enrolled(target) {
        return Err(crate::shell::i18n::t(
            "device-fabric-error-control-not-granted",
        ));
    }
    let (controller, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let route = spatial_input_state()
        .lock()
        .map_err(|_| "spatial input state is unavailable".to_string())?
        .received_routes
        .get(target)
        .cloned()
        .ok_or_else(|| "spatial input grant has no current certified route".to_string())?;
    network
        .with_current_personal_route(&route, || {
            let mut spatial = spatial_input_state()
                .lock()
                .map_err(|_| "spatial input state is unavailable".to_string())?;
            operation(&mut spatial, &controller)
        })
        .ok_or_else(|| "spatial input grant route is no longer current".to_string())?
}

/// Transfer held input releases to a bounded cleanup lane under the same
/// exact authority as ordinary input. A full ordinary queue cannot lose them.
///
/// # Errors
/// Rejects withdrawn policy, identity, route, lease or capabilities, and any
/// event other than a valid key/button release. No authority is minted here.
pub fn queue_spatial_input_cleanup_bound(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    nonce: [u8; 16],
    events: Vec<FabricInputEvent>,
) -> Result<(), String> {
    with_spatial_input_authority(network, target, |spatial, controller| {
        spatial.queue_cleanup_bound(controller, target, (nonce, events), unix_now_ms())
    })
}

pub(super) fn flush_spatial_input(network: &ShellNetwork) {
    spatial_withdrawal::flush(network);
    spatial_protocol::probe(network);
    for _ in 0..MAX_SPATIAL_INPUT_FLUSH_PER_FRAME {
        let prepared = spatial_input_state().lock().ok().and_then(|mut spatial| {
            let (mut input, cleanup) = spatial.next_spatial_input()?;
            let action = spatial
                .prepare_spatial_input(&mut input, unix_now_ms())
                .ok()?;
            if cleanup {
                spatial.cleanup.prepared(&input);
            }
            let route = spatial.received_routes.get(&input.target).cloned();
            Some((input, cleanup, route, action))
        });
        let Some((input, cleanup, route, action)) = prepared else {
            return;
        };
        let Some(route) = route.filter(|route| network.personal_route_is_current(route)) else {
            continue;
        };
        if network
            .send_personal_fabric_action_on_route(&route, action)
            .is_err()
        {
            if !cleanup && let Ok(mut spatial) = spatial_input_state().lock() {
                spatial.outbound.push_front(input);
            }
            return;
        }
        if cleanup && let Ok(mut spatial) = spatial_input_state().lock() {
            spatial.cleanup.delivered(&input);
        }
    }
}

/// One causal native-seat phase. Releases precede every surviving input in a
/// frame, so an old lease's barrier cannot erase a replacement lease's edge.
pub(crate) enum SpatialInputBatch {
    Releases(Vec<crate::shell::personal_continuity::DeviceId>),
    Inputs(
        Vec<(
            crate::shell::personal_continuity::DeviceId,
            FabricInputEvent,
        )>,
    ),
}

/// Take release barriers and admitted input under one state lock. Revocation
/// already discards its old queued edges; fresh authority can enqueue afterward.
pub(crate) fn take_spatial_input_batches() -> [SpatialInputBatch; 2] {
    spatial_input_state().lock().map_or_else(
        |_| {
            [
                SpatialInputBatch::Releases(Vec::new()),
                SpatialInputBatch::Inputs(Vec::new()),
            ]
        },
        |mut spatial| spatial.take_native_batches(),
    )
}

impl SpatialInputState {
    fn take_native_batches(&mut self) -> [SpatialInputBatch; 2] {
        [
            SpatialInputBatch::Releases(self.release_controllers.drain(..).collect()),
            SpatialInputBatch::Inputs(self.inbound.drain(..).collect()),
        ]
    }
}

/// Drain input that already passed exact-route, lease, expiry, and capability
/// validation. The native compositor is the sole consumer.
pub fn take_spatial_input() -> Vec<(
    crate::shell::personal_continuity::DeviceId,
    FabricInputEvent,
)> {
    spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |mut spatial| spatial.inbound.drain(..).collect(),
    )
}

/// Drain controllers whose target-issued lease was revoked, expired, or lost
/// with the device. The compositor releases only input actually attributed to
/// that controller, so cleanup cannot release a sibling controller's keys.
pub fn take_spatial_input_releases() -> Vec<crate::shell::personal_continuity::DeviceId> {
    spatial_input_state().lock().map_or_else(
        |_| Vec::new(),
        |mut spatial| spatial.release_controllers.drain(..).collect(),
    )
}

fn native_display_connectors(
    snapshot: &settings_store::hardware::DisplayHardwareSnapshot,
) -> Vec<crate::shell::device_fabric::DisplayConnectorPlacement> {
    use crate::shell::device_fabric::DisplayConnectorPlacement;
    snapshot
        .layout
        .iter()
        .filter_map(|placement| {
            let output = snapshot
                .outputs
                .iter()
                .find(|output| output.descriptor.connector == placement.connector)?;
            if !output.scale.is_finite() || !(0.5..=4.0).contains(&output.scale) {
                return None;
            }
            let scale_milli = format!("{:.0}", output.scale * 1000.0)
                .parse::<u16>()
                .ok()?;
            Some(DisplayConnectorPlacement {
                connector: placement.connector.clone(),
                label: output.descriptor.label().to_owned(),
                rect: DisplayRect {
                    x: placement.x,
                    y: placement.y,
                    width: placement.width,
                    height: placement.height,
                },
                scale_milli,
            })
        })
        .take(32)
        .collect()
}

fn native_display_outputs(
    snapshot: &settings_store::hardware::DisplayHardwareSnapshot,
) -> Option<Vec<crate::shell::device_fabric::DisplayOutputObservation>> {
    use crate::shell::device_fabric::DisplayOutputObservation;
    use settings_store::person_displays::{DeviceOutput, MAX_DEVICE_OUTPUTS};
    if snapshot.outputs.len() > MAX_DEVICE_OUTPUTS {
        return None;
    }
    let identities = settings_store::hardware::DisplaySet::of(
        snapshot
            .outputs
            .iter()
            .map(|output| (&output.descriptor).into()),
    );
    snapshot
        .outputs
        .iter()
        .map(|output| {
            if !output.scale.is_finite() || !(0.5..=4.0).contains(&output.scale) {
                return None;
            }
            let connector = &output.descriptor.connector;
            let placement = snapshot
                .layout
                .iter()
                .find(|placement| &placement.connector == connector);
            let name = output
                .descriptor
                .monitor
                .as_ref()
                .map(settings_store::hardware::MonitorIdentity::display_name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| output.descriptor.label().to_owned());
            let facts = DeviceOutput {
                id: identities.members.get(connector)?.clone(),
                name,
                builtin: output.builtin,
                x: placement.map_or(0, |placement| placement.x),
                y: placement.map_or(0, |placement| placement.y),
                width: placement.map_or(0, |placement| placement.width),
                height: placement.map_or(0, |placement| placement.height),
                mode_width: output.current_mode.width,
                mode_height: output.current_mode.height,
                refresh_mhz: output.current_mode.refresh_mhz,
                scale_milli: format!("{:.0}", output.scale * 1000.0).parse().ok()?,
                connected: true,
                enabled: placement.is_some(),
            };
            facts.valid().then(|| DisplayOutputObservation {
                connector: connector.clone(),
                output: facts,
            })
        })
        .collect()
}

/// The same certified observations feed Settings and the shell host view.
pub fn observed_device_displays(
    fabric: &DeviceFabric,
    local: Option<&crate::shell::personal_continuity::DeviceId>,
) -> Vec<settings_store::device_displays::DeviceDisplay> {
    fabric
        .snapshots()
        .values()
        .flat_map(|snapshot| {
            snapshot.display.iter().flat_map(move |display| {
                display.outputs.iter().filter_map(move |output| {
                    output.rect.logical_placement()?;
                    let x = display.rect.x.checked_add(output.rect.x)?;
                    let y = display.rect.y.checked_add(output.rect.y)?;
                    DisplayRect {
                        x,
                        y,
                        ..output.rect
                    }
                    .logical_placement()?;
                    Some(settings_store::device_displays::DeviceDisplay {
                        device: snapshot.owner.as_str().to_owned(),
                        device_label: snapshot.presence.label.clone(),
                        connector: output.connector.clone(),
                        output_id: fabric
                            .display_outputs(&snapshot.owner)
                            .and_then(|observations| {
                                observations
                                    .iter()
                                    .find(|observed| observed.connector == output.connector)
                            })
                            .map(|observed| observed.output.id.clone()),
                        label: output.label.clone(),
                        online: snapshot.presence.online,
                        local: local == Some(&snapshot.owner),
                        x,
                        y,
                        width: output.rect.width,
                        height: output.rect.height,
                        scale_milli: output.scale_milli,
                        revision: display.stamp.counter,
                    })
                })
            })
        })
        .collect()
}

fn publish_device_displays(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    person_layout::refresh(network, policy);
    let Ok((publisher_pid, publisher_start_ticks)) = settings_store::hardware::publisher_identity()
    else {
        return;
    };
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let mut snapshot = settings_store::device_displays::DeviceDisplaySnapshot {
        publisher_pid,
        publisher_start_ticks,
        displays: if local.is_some() && policy.permits(ContinuityCapability::DisplayInput) {
            observed_device_displays(&network.device_fabric(), local.as_ref())
        } else {
            Vec::new()
        },
    };
    snapshot.displays.retain(|display| {
        crate::shell::personal_continuity::DeviceId::new(display.device.clone())
            .is_ok_and(|owner| network.personal_device_is_enrolled(&owner))
    });
    for display in &mut snapshot.displays {
        if !display.local {
            display.online &=
                crate::shell::personal_continuity::DeviceId::new(display.device.clone())
                    .is_ok_and(|owner| network.current_personal_route(&owner).is_some());
        }
    }
    // Both files are written through an fsync'd atomic replace, which waits on
    // the filesystem's journal: on the compositor's loop that is a stall of
    // however long the disk is busy. The durable writer's thread takes it, the
    // latest snapshot of each file winning.
    let writer = crate::shell::durable_writer::global();
    writer.submit(
        std::path::PathBuf::from("device-fabric/device-display-state"),
        move || {
            if let Err(error) = snapshot.save() {
                tracing::debug!(%error, "could not publish device display observations");
            }
        },
    );
    let view = settings_store::person_displays::PersonDisplays {
        schema_version: settings_store::person_displays::SCHEMA_VERSION,
        publisher_pid,
        publisher_start_ticks,
        local_device: local
            .as_ref()
            .map_or_else(String::new, |device| device.as_str().to_owned()),
        devices: local
            .as_ref()
            .filter(|_| policy.permits(ContinuityCapability::DisplayInput))
            .map_or_else(Vec::new, |local| network.person_display_devices(local)),
    };
    writer.submit(
        std::path::PathBuf::from("device-fabric/person-displays"),
        move || {
            if let Err(error) = view.save() {
                tracing::debug!(%error, "could not publish person display observations");
            }
        },
    );
}

fn build_local_snapshot(
    device: &crate::shell::personal_continuity::DeviceId,
    label: String,
    local: LocalFabricCapabilities,
    selection: &SharedSelection,
    previous: Option<&DeviceFabricSnapshot>,
    applied_remote_settings: &BTreeMap<String, Value>,
    stamp: &FabricStamp,
    policy: ContinuityPolicySnapshot,
) -> DeviceFabricSnapshot {
    let media = if policy.permits(ContinuityCapability::Media) {
        media::handoff::playback_sessions()
            .into_iter()
            .map(|session| MediaSession {
                id: session.id,
                app_id: session.app_id,
                title: session.title,
                content_id: session.content_id,
                position_ms: session.position_ms,
                duration_ms: session.duration_ms,
                playing: session.playing,
                has_video: session.has_video,
                output_device: device.clone(),
                stamp: stamp.clone(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let clipboard = if policy.permits(ContinuityCapability::Clipboard) {
        shareable_clipboard(device, selection, stamp.counter)
    } else {
        Vec::new()
    };
    let settings = reconcile_roaming_settings(
        permitted_roaming_setting_values(&settings_store::load(), policy),
        previous,
        applied_remote_settings,
        stamp,
    );
    let wifi = if policy.permits(ContinuityCapability::WifiSettings) {
        previous.map_or_else(Vec::new, |state| state.wifi.clone())
    } else {
        Vec::new()
    };
    let display = if policy.permits(ContinuityCapability::DisplayInput) {
        local.display.map(|rect| DisplayPlacement {
            outputs: Vec::new(),
            device: device.clone(),
            rect,
            scale_milli: 1000,
            stamp: stamp.clone(),
        })
    } else {
        None
    };
    let mut snapshot = DeviceFabricSnapshot {
        schema_version: FABRIC_SCHEMA_VERSION,
        owner: device.clone(),
        presence: DevicePresence {
            device: device.clone(),
            label,
            kind: local_device_kind(),
            capabilities: policy_capabilities(policy, local),
            online: true,
            last_seen_unix_ms: 0,
            wake_routes: Vec::new(),
            stamp: stamp.clone(),
        },
        media,
        clipboard,
        settings,
        wifi,
        display,
        // Enrolments are authored once, by `device_unlock_runtime`, and kept.
        unlock: previous.map_or_else(Default::default, |state| state.unlock.clone()),
        speakers: crate::shell::speaker_group_runtime::authored_state(
            device,
            previous.map(|state| &state.speakers),
            stamp,
            policy.permits(ContinuityCapability::Media),
        ),
    };
    preserve_unchanged_stamps(&mut snapshot, previous);
    snapshot
}

fn policy_capabilities(
    policy: ContinuityPolicySnapshot,
    local: LocalFabricCapabilities,
) -> FabricCapabilities {
    FabricCapabilities {
        display: policy.permits(ContinuityCapability::DisplayInput) && local.display.is_some(),
        pointer: policy.permits(ContinuityCapability::DisplayInput) && local.pointer,
        keyboard: policy.permits(ContinuityCapability::DisplayInput) && local.keyboard,
        audio_output: policy.permits(ContinuityCapability::Media) && local.audio_output,
        video_output: (policy.permits(ContinuityCapability::Media)
            || policy.permits(ContinuityCapability::DisplayInput))
            && local.display.is_some(),
        clipboard: policy.permits(ContinuityCapability::Clipboard),
        roaming_settings: policy.permits(ContinuityCapability::WifiSettings)
            || policy.permits(ContinuityCapability::DisplayInput),
        secret_store: policy.permits(ContinuityCapability::WifiSettings),
        app_continuation: policy.permits(ContinuityCapability::AppContinuations),
        audio_group: policy.permits(ContinuityCapability::Media) && local.audio_output,
    }
}

fn refresh_display_observation_revision(
    display: &mut DisplayPlacement,
    fabric: &DeviceFabric,
    outputs: &[crate::shell::device_fabric::DisplayOutputObservation],
    stamp: &FabricStamp,
) {
    if fabric.display_outputs(&display.device) != Some(outputs) {
        // Mode or identity changes matter even when logical geometry is unchanged.
        display.stamp = stamp.clone();
    }
}

fn local_observation_fingerprint(
    snapshot: &DeviceFabricSnapshot,
    outputs: Option<&[crate::shell::device_fabric::DisplayOutputObservation]>,
) -> Option<[u8; 32]> {
    postcard::to_allocvec(&(local_snapshot_fingerprint(snapshot), outputs))
        .ok()
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
}

fn local_snapshot_fingerprint(snapshot: &DeviceFabricSnapshot) -> Option<[u8; 32]> {
    let mut comparable = snapshot.clone();
    let neutral = FabricStamp {
        counter: 0,
        device: snapshot.owner.clone(),
    };
    stamp_snapshot(&mut comparable, &neutral, 0);
    postcard::to_allocvec(&(
        &comparable,
        comparable.display.as_ref().map(|display| &display.outputs),
    ))
    .ok()
    .map(|bytes| *blake3::hash(&bytes).as_bytes())
}

fn preserve_unchanged_stamps(
    candidate: &mut DeviceFabricSnapshot,
    previous: Option<&DeviceFabricSnapshot>,
) {
    let Some(previous) = previous else {
        return;
    };
    for item in &mut candidate.media {
        if let Some(old) = previous.media.iter().find(|old| old.id == item.id) {
            let mut comparable = item.clone();
            comparable.stamp = old.stamp.clone();
            if comparable == *old {
                item.stamp = old.stamp.clone();
            }
        }
    }
    for item in &mut candidate.clipboard {
        if let Some(old) = previous.clipboard.iter().find(|old| old.id == item.id) {
            let mut comparable = item.clone();
            comparable.stamp = old.stamp.clone();
            if comparable == *old {
                item.stamp = old.stamp.clone();
            }
        }
    }
    if let (Some(display), Some(old)) = (&mut candidate.display, &previous.display) {
        let mut comparable = display.clone();
        comparable.stamp = old.stamp.clone();
        if comparable == *old {
            display.stamp = old.stamp.clone();
        }
    }
}

fn stamp_snapshot(snapshot: &mut DeviceFabricSnapshot, stamp: &FabricStamp, now_unix_ms: u64) {
    snapshot.presence.stamp = stamp.clone();
    snapshot.presence.last_seen_unix_ms = now_unix_ms;
    for item in &mut snapshot.media {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.clipboard {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.settings {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.wifi {
        item.stamp = stamp.clone();
    }
    if let Some(display) = snapshot.display.as_mut() {
        display.stamp = stamp.clone();
    }
    for item in &mut snapshot.speakers.groups {
        item.stamp = stamp.clone();
    }
    for item in &mut snapshot.speakers.memberships {
        item.stamp = stamp.clone();
    }
}

fn shareable_clipboard(
    device: &crate::shell::personal_continuity::DeviceId,
    selection: &SharedSelection,
    counter: u64,
) -> Vec<ClipboardEntry> {
    shareable_clipboard_history(selection)
        .into_iter()
        .filter_map(|item| {
            let payload = if item.mime == "text/plain;charset=utf-8" {
                ClipboardPayload::Text(String::from_utf8(item.bytes).ok()?)
            } else {
                ClipboardPayload::Binary {
                    mime: item.mime,
                    bytes: item.bytes,
                }
            };
            let mut entry = ClipboardEntry::shareable(
                payload,
                device.clone(),
                item.created_unix_ms,
                FabricStamp {
                    counter,
                    device: device.clone(),
                },
            )
            .ok()?;
            entry.pinned = item.pinned;
            Some(entry)
        })
        .collect()
}

fn roaming_setting_values() -> BTreeMap<String, Value> {
    roaming_setting_values_from(&settings_store::load())
}

fn roaming_setting_values_from(document: &Map<String, Value>) -> BTreeMap<String, Value> {
    settings_store::continuity::personal_values(document)
}

fn permitted_roaming_setting_values(
    document: &Map<String, Value>,
    policy: ContinuityPolicySnapshot,
) -> BTreeMap<String, Value> {
    roaming_setting_values_from(document)
        .into_iter()
        .filter(|(key, _)| permits_roaming_setting(policy, key))
        .collect()
}

fn permits_roaming_setting(policy: ContinuityPolicySnapshot, key: &str) -> bool {
    let capability = if key == settings_store::person_displays::ARRANGEMENT_KEY {
        ContinuityCapability::DisplayInput
    } else {
        ContinuityCapability::WifiSettings
    };
    policy.permits(capability)
}

fn roaming_provenance(
    fabric: &DeviceFabric,
    policy: ContinuityPolicySnapshot,
) -> settings_store::roaming_provenance::RoamingProvenance {
    use settings_store::roaming_provenance::{
        Origin, RoamingProvenance, SettingResolution, WifiResolution,
    };

    if !policy.permits(ContinuityCapability::WifiSettings)
        && !policy.permits(ContinuityCapability::DisplayInput)
    {
        return RoamingProvenance::default();
    }
    let origin = |stamp: &FabricStamp| Origin {
        device_id: stamp.device.as_str().to_string(),
        device_label: fabric.snapshots().get(&stamp.device).map_or_else(
            || stamp.device.as_str().to_string(),
            |snapshot| snapshot.presence.label.clone(),
        ),
        counter: stamp.counter,
    };
    let mut settings_by_key = BTreeMap::<&str, Vec<&RoamingSetting>>::new();
    for setting in fabric
        .snapshots()
        .values()
        .flat_map(|snapshot| &snapshot.settings)
        .filter(|setting| {
            settings_store::continuity::is_personal(&setting.key)
                && permits_roaming_setting(policy, &setting.key)
        })
    {
        settings_by_key
            .entry(&setting.key)
            .or_default()
            .push(setting);
    }
    let settings = settings_by_key
        .into_iter()
        .filter_map(|(key, records)| {
            let winner = records
                .iter()
                .copied()
                .max_by(|left, right| left.stamp.cmp(&right.stamp))?;
            let alternatives = records
                .into_iter()
                .filter(|record| {
                    record.stamp.device != winner.stamp.device && record.value != winner.value
                })
                .map(|record| origin(&record.stamp))
                .collect();
            Some(SettingResolution {
                key: key.to_string(),
                winner: origin(&winner.stamp),
                alternatives,
            })
        })
        .collect();

    let mut wifi_by_network =
        BTreeMap::<[u8; 32], Vec<&crate::shell::device_fabric::WifiCredential>>::new();
    for credential in fabric
        .snapshots()
        .values()
        .flat_map(|snapshot| &snapshot.wifi)
        .filter(|_| policy.permits(ContinuityCapability::WifiSettings))
    {
        wifi_by_network
            .entry(credential.network_id)
            .or_default()
            .push(credential);
    }
    let wifi = wifi_by_network
        .into_iter()
        .filter_map(|(network_id, records)| {
            let winner = records
                .iter()
                .copied()
                .max_by(|left, right| left.stamp.cmp(&right.stamp))?;
            let alternatives = records
                .into_iter()
                .filter(|record| record.stamp.device != winner.stamp.device)
                .map(|record| origin(&record.stamp))
                .collect();
            Some(WifiResolution {
                network_id: hex::encode(network_id),
                ssid: winner.ssid.clone(),
                security: String::from(winner.security),
                winner: origin(&winner.stamp),
                alternatives,
            })
        })
        .collect();
    RoamingProvenance {
        schema_version: settings_store::roaming_provenance::SCHEMA_VERSION,
        generated_unix_ms: 0,
        settings,
        wifi,
    }
}

fn publish_roaming_provenance(
    fabric: &DeviceFabric,
    policy: ContinuityPolicySnapshot,
    previous_fingerprint: &mut Option<[u8; 32]>,
    path: Option<&Path>,
) -> Result<bool, settings_store::roaming_provenance::ProvenanceError> {
    let mut projection = roaming_provenance(fabric, policy);
    let fingerprint = serde_json::to_vec(&projection)
        .ok()
        .map(|bytes| *blake3::hash(&bytes).as_bytes());
    if fingerprint == *previous_fingerprint {
        return Ok(false);
    }
    projection.generated_unix_ms = unix_now_ms();
    match path {
        Some(path) => projection.save_at(path)?,
        None => projection.save()?,
    }
    *previous_fingerprint = fingerprint;
    Ok(true)
}

fn reconcile_roaming_settings(
    current: BTreeMap<String, Value>,
    previous: Option<&DeviceFabricSnapshot>,
    applied_remote: &BTreeMap<String, Value>,
    stamp: &FabricStamp,
) -> Vec<RoamingSetting> {
    current
        .into_iter()
        .filter_map(|(key, value)| {
            let old = previous
                .and_then(|snapshot| snapshot.settings.iter().find(|setting| setting.key == key));
            if let Some(remote_value) = applied_remote.get(&key) {
                if remote_value == &value {
                    return old.cloned();
                }
                // A value diverging from what this runtime last applied is an
                // explicit local edit, even when it happens to equal stale
                // local state from before the remote winner arrived.
                return Some(RoamingSetting {
                    key,
                    value,
                    stamp: stamp.clone(),
                });
            }
            if let Some(old) = old
                && old.value == value
            {
                return Some(old.clone());
            }
            Some(RoamingSetting {
                key,
                value,
                stamp: stamp.clone(),
            })
        })
        .collect()
}

fn apply_roaming_settings(settings: &BTreeMap<String, Value>) {
    let mut sections = BTreeMap::<String, Map<String, Value>>::new();
    for (key, value) in settings {
        if !super::device_fabric::valid_setting_key(key) {
            tracing::warn!(key, "ignored non-roaming setting from personal fabric");
            continue;
        }
        let Some(rest) = key.strip_prefix("tilo.") else {
            continue;
        };
        let Some((section, item)) = rest.split_once('.') else {
            continue;
        };
        sections
            .entry(section.to_string())
            .or_default()
            .insert(item.to_string(), value.clone());
    }
    for (section, values) in sections {
        if let Err(error) = settings_store::update_section(&section, values) {
            tracing::warn!(%error, section, "could not apply roaming device setting");
        }
    }
}

fn local_device_kind() -> DeviceKind {
    let chassis = std::fs::read_to_string("/sys/class/dmi/id/chassis_type")
        .ok()
        .and_then(|value| value.trim().parse::<u8>().ok());
    match chassis {
        Some(8 | 9 | 10 | 14 | 30 | 31 | 32) => DeviceKind::Laptop,
        Some(11 | 28 | 29) => DeviceKind::Tablet,
        _ => DeviceKind::Desktop,
    }
}

fn require_continuity(capability: ContinuityCapability) -> Result<(), String> {
    ContinuityPolicySnapshot::load()
        .permits(capability)
        .then_some(())
        .ok_or_else(|| crate::shell::i18n::t(continuity_disabled_key(capability)))
}

/// The whole sentence saying `capability` is switched off here.
const fn continuity_disabled_key(capability: ContinuityCapability) -> &'static str {
    match capability {
        ContinuityCapability::Clipboard => "continuity-disabled-clipboard",
        ContinuityCapability::Notifications => "continuity-disabled-notifications",
        ContinuityCapability::WifiSettings => "continuity-disabled-wifi-settings",
        ContinuityCapability::Media => "continuity-disabled-media",
        ContinuityCapability::DisplayInput => "continuity-disabled-display-input",
        ContinuityCapability::FileTransfer => "continuity-disabled-file-transfer",
        ContinuityCapability::RealtimeSessions => "continuity-disabled-realtime",
        ContinuityCapability::TypedGamepad => "continuity-disabled-gamepad",
        ContinuityCapability::AppContinuations => "continuity-disabled-app-continuations",
    }
}

/// Ask one certified device to expose a constrained virtual gamepad for an
/// exact app/workspace/session binding. The target's grant is asynchronous;
/// callers can poll [`typed_gamepad_forwarding_ready`] before sending input.
///
/// # Errors
///
/// Returns an error when continuity policy denies forwarding, the identity is
/// malformed, capacity is exhausted, or the authenticated route cannot send.
pub fn request_typed_gamepad_forwarding(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    app_id: &str,
    workspace_id: &str,
    session_id: &str,
    label: &str,
) -> Result<[u8; 16], String> {
    require_continuity(ContinuityCapability::TypedGamepad)?;
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| "local certified-device identity is unavailable".to_string())?;
    os_actions::ForwardedPeripheralIdentity::new_after_verification(
        local.as_str(),
        target.as_str(),
        app_id,
        workspace_id,
        session_id,
    )
    .map_err(|error| error.to_string())?;
    if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
        return Err("gamepad label is invalid".into());
    }
    let forwarding_id =
        secure_nonce().ok_or_else(|| "secure randomness is unavailable".to_string())?;
    {
        let state = outbound_gamepad_state()
            .lock()
            .map_err(|_| "typed-gamepad state is unavailable".to_string())?;
        if state.sessions.len() >= MAX_GAMEPAD_FORWARDINGS {
            return Err(crate::shell::i18n::t(
                "device-fabric-error-gamepad-capacity",
            ));
        }
    }
    network.send_personal_fabric_action(
        target,
        PersonalFabricAction::GamepadForwardRequest {
            forwarding_id,
            app_id: app_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            label: label.to_owned(),
        },
    )?;
    outbound_gamepad_state()
        .lock()
        .map_err(|_| "typed-gamepad state is unavailable".to_string())?
        .sessions
        .insert(
            forwarding_id,
            OutboundGamepadForwarding {
                target: target.clone(),
                lease_nonce: None,
                sequence: 0,
                requested: Instant::now(),
            },
        );
    Ok(forwarding_id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTypedGamepadAuthorization {
    pub forwarding_id: [u8; 16],
    pub source: crate::shell::personal_continuity::DeviceId,
    pub app_id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub label: String,
}

/// Snapshot locally pending requests for a shell consent surface. Merely
/// reading a request never creates a virtual input device.
#[must_use]
pub fn pending_typed_gamepad_authorizations() -> Vec<PendingTypedGamepadAuthorization> {
    gamepad_authorization_state().lock().map_or_else(
        |_| Vec::new(),
        |state| {
            state
                .pending
                .iter()
                .map(
                    |(forwarding_id, request)| PendingTypedGamepadAuthorization {
                        forwarding_id: *forwarding_id,
                        source: request.source.clone(),
                        app_id: request.app_id.clone(),
                        workspace_id: request.workspace_id.clone(),
                        session_id: request.session_id.clone(),
                        label: request.label.clone(),
                    },
                )
                .collect()
        },
    )
}

/// Record an explicit local consent decision. The session runtime consumes
/// this command and creates the fixed virtual device only on approval.
///
/// # Errors
///
/// Returns an error for disabled policy, an unknown request, or a duplicate
/// decision. The remote peer cannot call this local shell boundary.
pub fn authorize_typed_gamepad_forwarding(
    forwarding_id: [u8; 16],
    approved: bool,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::TypedGamepad)?;
    let decided = gamepad_authorization_state()
        .lock()
        .map_err(|_| "typed-gamepad authorization state is unavailable".to_string())?
        .decide(forwarding_id, approved);
    if !decided {
        return Err(crate::shell::i18n::t("device-fabric-error-gamepad-decided"));
    }
    Ok(())
}

#[must_use]
pub fn typed_gamepad_forwarding_ready(forwarding_id: [u8; 16]) -> bool {
    outbound_gamepad_state().lock().is_ok_and(|state| {
        state
            .sessions
            .get(&forwarding_id)
            .is_some_and(|session| session.lease_nonce.is_some())
    })
}

/// Whether an outbound request is still pending or leased. A rejected or
/// remotely stopped request disappears, allowing the physical source owner to
/// close its descriptor promptly instead of waiting for the consent timeout.
#[must_use]
pub fn typed_gamepad_forwarding_exists(forwarding_id: [u8; 16]) -> bool {
    outbound_gamepad_state()
        .lock()
        .is_ok_and(|state| state.sessions.contains_key(&forwarding_id))
}

/// Send one sanitized logical controller event. No raw transport report,
/// descriptor, key code, or caller-selected device capability can cross this
/// interface.
///
/// # Errors
///
/// Returns an error for disabled policy, unknown/ungranted forwarding, invalid
/// semantic input, sequence exhaustion, or delivery failure.
pub fn send_typed_gamepad_event(
    network: &ShellNetwork,
    forwarding_id: [u8; 16],
    event: os_actions::ForwardedGamepadEvent,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::TypedGamepad)?;
    if !event.is_valid() {
        return Err("invalid sanitized gamepad event".into());
    }
    let (target, lease_nonce, sequence) = {
        let state = outbound_gamepad_state()
            .lock()
            .map_err(|_| "typed-gamepad state is unavailable".to_string())?;
        let session = state
            .sessions
            .get(&forwarding_id)
            .ok_or_else(|| "typed-gamepad forwarding is unknown".to_string())?;
        let lease_nonce = session
            .lease_nonce
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-gamepad-no-lease"))?;
        let sequence = session
            .sequence
            .checked_add(1)
            .ok_or_else(|| "typed-gamepad event sequence is exhausted".to_string())?;
        let target = session.target.clone();
        drop(state);
        (target, lease_nonce, sequence)
    };
    network.send_personal_fabric_action(
        &target,
        PersonalFabricAction::GamepadForwardEvent {
            forwarding_id,
            lease_nonce,
            sequence,
            event,
        },
    )?;
    if let Ok(mut state) = outbound_gamepad_state().lock()
        && let Some(session) = state.sessions.get_mut(&forwarding_id)
        && session.target == target
        && session.lease_nonce == Some(lease_nonce)
    {
        session.sequence = sequence;
    }
    Ok(())
}

/// End an exact forwarding and ask the target to neutralize every control.
/// Local state is removed even when delivery fails.
///
/// # Errors
///
/// Returns an error when the forwarding is unknown or teardown delivery fails.
pub fn stop_typed_gamepad_forwarding(
    network: &ShellNetwork,
    forwarding_id: [u8; 16],
) -> Result<(), String> {
    let session = outbound_gamepad_state()
        .lock()
        .map_err(|_| "typed-gamepad state is unavailable".to_string())?
        .sessions
        .remove(&forwarding_id)
        .ok_or_else(|| "typed-gamepad forwarding is unknown".to_string())?;
    let Some(lease_nonce) = session.lease_nonce else {
        return Ok(());
    };
    network
        .send_personal_fabric_action(
            &session.target,
            PersonalFabricAction::GamepadForwardStop {
                forwarding_id,
                lease_nonce,
            },
        )
        .map(|_| ())
}

fn secure_nonce() -> Option<[u8; 16]> {
    let mut nonce = [0; 16];
    SysRng.try_fill_bytes(&mut nonce).ok()?;
    (nonce != [0; 16]).then_some(nonce)
}

fn tear_down_spatial_input(network: &ShellNetwork) {
    if let Ok(mut spatial) = spatial_input_state().lock() {
        spatial.queue_controller_withdrawals();
        let issued = spatial.issued.values().cloned().collect::<Vec<_>>();
        for lease in issued {
            let _ = network.send_personal_fabric_action(
                &lease.controller,
                PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
            );
            if !spatial.release_controllers.contains(&lease.controller) {
                spatial
                    .release_controllers
                    .push_back(lease.controller.clone());
            }
        }
        spatial.issued.clear();
        spatial.received.clear();
        spatial.received_routes.clear();
        spatial.issued_routes.clear();
        spatial.automatic_issued.clear();
        spatial.outbound.clear();
        spatial.cleanup.clear();
        spatial.inbound.clear();
        spatial.retain_spatial_protocol();
    }
    spatial_withdrawal::flush(network);
}

fn tear_down_nearby_transfers(network: &ShellNetwork) {
    if let Ok(mut admission) = nearby_staging_admission().lock() {
        admission.invalidate();
    }
    // Media handoffs share the lane but answer to the Media switch.
    let media_outgoing = with_nearby_transfer_store(|store| {
        Ok(store
            .outbox()
            .iter()
            .filter(|entry| entry.kind == crate::shell::nearby_transfer::TransferKind::MediaHandoff)
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>())
    })
    .unwrap_or_default();
    let active = nearby_drop_state().lock().map_or_else(
        |_| Vec::new(),
        |state| {
            state
                .large_offers
                .iter()
                .filter(|(transfer_id, _)| !media_outgoing.contains(*transfer_id))
                .map(|(transfer_id, (target, request_id, _))| {
                    (*transfer_id, target.clone(), *request_id)
                })
                .collect::<Vec<_>>()
        },
    );
    for (transfer_id, target, request_id) in &active {
        let _ = network.cancel_nearby_transfer_stream(target, *transfer_id);
        let _ = network.send_personal_fabric_action(
            target,
            PersonalFabricAction::NearbyTransferCancel {
                request_id: *request_id,
                transfer_id: *transfer_id,
            },
        );
    }
    let incoming = nearby_drop_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            std::mem::take(&mut state.incoming)
                .into_iter()
                .collect::<Vec<_>>()
        },
    );
    for (transfer_id, pending) in incoming {
        let action = match pending.payload {
            PendingNearbyPayload::Inline { .. } => PersonalFabricAction::NearbyDropResult {
                request_id: pending.request_id,
                file_name: pending.name,
                accepted: false,
            },
            PendingNearbyPayload::Durable { .. } => PersonalFabricAction::NearbyTransferResult {
                request_id: pending.request_id,
                transfer_id,
                accepted: false,
            },
        };
        let _ = network.send_personal_fabric_action(&pending.source, action);
    }
    let _ = with_nearby_transfer_store(|store| {
        let incoming = store
            .inbox()
            .iter()
            .filter(|entry| entry.kind != crate::shell::nearby_transfer::TransferKind::MediaHandoff)
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        for transfer_id in incoming {
            let _ = store.cancel_incoming(transfer_id);
        }
        for (transfer_id, _, _) in &active {
            let _ = store.cancel_outgoing(*transfer_id);
        }
        Ok(())
    });
    if let Ok(mut state) = nearby_drop_state().lock() {
        state.pending.clear();
        state
            .large_offers
            .retain(|transfer_id, _| media_outgoing.contains(transfer_id));
        state
            .progress
            .retain(|transfer_id, _| media_outgoing.contains(transfer_id));
        state
            .last_offer
            .retain(|transfer_id, _| media_outgoing.contains(transfer_id));
        state.incoming.clear();
        state.destinations.clear();
        state.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::personal_continuity::DeviceId;

    #[test]
    fn rich_display_native_observation_preserves_identity_mode_scale_and_inactive_outputs() {
        use settings_store::hardware::{
            DisplayHardwareSnapshot, DisplayOutputSnapshot, LiveOutputPlacement, MonitorIdentity,
            OutputDescriptor, OutputMode,
        };
        let panel = DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: "eDP-1".into(),
                connector_label: String::new(),
                modes: Vec::new(),
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: Some(MonitorIdentity {
                    make: "PanelCo".into(),
                    model: "Panel".into(),
                    serial: "123".into(),
                    physical_size_mm: None,
                }),
            },
            current_mode: OutputMode {
                width: 2560,
                height: 1600,
                refresh_mhz: 59_940,
            },
            scale: 2.0,
            builtin: true,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        };
        let mut inactive = panel.clone();
        inactive.descriptor.connector = "DP-2".into();
        inactive.descriptor.monitor = None;
        inactive.builtin = false;
        let mut hardware = DisplayHardwareSnapshot {
            outputs: vec![panel, inactive],
            layout: vec![LiveOutputPlacement {
                connector: "eDP-1".into(),
                x: 40,
                y: 20,
                width: 1280,
                height: 800,
            }],
            ..Default::default()
        };
        let outputs = native_display_outputs(&hardware).unwrap();
        assert_eq!(outputs.len(), 2);
        let panel = &outputs[0].output;
        assert!(panel.builtin && panel.connected && panel.enabled);
        assert_eq!(
            (panel.x, panel.y, panel.width, panel.height),
            (40, 20, 1280, 800)
        );
        assert_eq!(
            (panel.mode_width, panel.mode_height, panel.refresh_mhz),
            (2560, 1600, 59_940)
        );
        assert_eq!(panel.scale_milli, 2000);
        assert_eq!(panel.name, "PanelCo Panel");
        assert!(!outputs[1].output.enabled);
        assert_eq!(outputs[1].output.width, 0);
        hardware.outputs[0].descriptor.connector = "eDP-9".into();
        assert_eq!(
            native_display_outputs(&hardware).unwrap()[0].output.id,
            panel.id
        );
        hardware.outputs[0].scale = f64::NAN;
        assert!(native_display_outputs(&hardware).is_none());
    }

    #[test]
    fn rich_display_mode_change_gets_a_new_revision_without_geometry_change() {
        let owner = DeviceId::new("local-mode-test").unwrap();
        let previous = drag_snapshot(owner.clone(), 0);
        let old_revision = previous.display.as_ref().unwrap().stamp.clone();
        let mut fabric = DeviceFabric::default();
        fabric.upsert(previous.clone()).unwrap();
        let mut outputs = vec![crate::shell::device_fabric::DisplayOutputObservation {
            connector: "eDP-1".into(),
            output: settings_store::person_displays::DeviceOutput {
                id: "monitor:panel".into(),
                name: "Panel".into(),
                builtin: true,
                x: 0,
                y: 0,
                width: 1280,
                height: 720,
                mode_width: 1600,
                mode_height: 900,
                refresh_mhz: 60_000,
                scale_milli: 1250,
                connected: true,
                enabled: true,
            },
        }];
        fabric
            .observe_display_outputs(&owner, &old_revision, outputs.clone())
            .unwrap();
        let fingerprint = local_observation_fingerprint(&previous, Some(&outputs));
        let next_revision = FabricStamp {
            counter: old_revision.counter + 1,
            device: owner,
        };
        let mut candidate = previous.clone();
        candidate.display.as_mut().unwrap().stamp = next_revision.clone();
        preserve_unchanged_stamps(&mut candidate, Some(&previous));
        refresh_display_observation_revision(
            candidate.display.as_mut().unwrap(),
            &fabric,
            &outputs,
            &next_revision,
        );
        assert_eq!(candidate.display.as_ref().unwrap().stamp, old_revision);
        outputs[0].output.refresh_mhz = 59_940;
        refresh_display_observation_revision(
            candidate.display.as_mut().unwrap(),
            &fabric,
            &outputs,
            &next_revision,
        );
        assert_eq!(candidate.display.as_ref().unwrap().stamp, next_revision);
        assert_eq!(
            candidate.display.as_ref().unwrap().rect,
            previous.display.unwrap().rect
        );
        assert_ne!(
            local_observation_fingerprint(&candidate, Some(&outputs)),
            fingerprint
        );
    }

    fn roaming_test_policy(wifi_settings: bool, display_input: bool) -> ContinuityPolicySnapshot {
        ContinuityPolicySnapshot::from_document(
            serde_json::json!({
                "continuity": { "enabled": true, "wifi_settings": wifi_settings,
                    "display_input": display_input }
            })
            .as_object()
            .unwrap(),
        )
    }

    #[test]
    fn display_arrangement_policy_keeps_settings_and_credentials_independent() {
        let device = DeviceId::new("desktop").unwrap();
        let mut snapshot = provenance_snapshot(device, "Desktop", 1, Some(Value::Bool(true)));
        let arrangement_key = settings_store::person_displays::ARRANGEMENT_KEY;
        snapshot.settings.push(RoamingSetting {
            key: arrangement_key.into(),
            value: settings_store::person_displays::Arrangement::default().to_value(),
            stamp: snapshot.presence.stamp.clone(),
        });
        let mut fabric = DeviceFabric::default();
        fabric.upsert(snapshot).unwrap();
        for (wifi, displays) in [(false, false), (true, false), (false, true), (true, true)] {
            let policy = roaming_test_policy(wifi, displays);
            let document = serde_json::json!({
                "display": { "arrangement": { "displays": [] } },
                "appearance": { "mode": true }
            });
            let records = permitted_roaming_setting_values(document.as_object().unwrap(), policy);
            assert_eq!(records.contains_key(arrangement_key), displays);
            assert_eq!(records.contains_key("tilo.appearance.mode"), wifi);
            let projection = roaming_provenance(&fabric, policy);
            assert_eq!(
                projection
                    .settings
                    .iter()
                    .any(|item| item.key == arrangement_key),
                displays
            );
            assert_eq!(
                projection
                    .settings
                    .iter()
                    .any(|item| item.key == "tilo.appearance.mode"),
                wifi
            );
            let caps = policy_capabilities(
                policy,
                LocalFabricCapabilities {
                    display: None,
                    pointer: false,
                    keyboard: false,
                    audio_output: false,
                },
            );
            assert_eq!(caps.roaming_settings, wifi || displays);
            assert_eq!(caps.secret_store, wifi);
        }
        assert!(!permits_roaming_setting(
            ContinuityPolicySnapshot::default(),
            arrangement_key
        ));
    }

    fn drag_snapshot(device: DeviceId, x: i32) -> DeviceFabricSnapshot {
        let mut snapshot = provenance_snapshot(device.clone(), "Laptop", 7, None);
        snapshot.presence.capabilities.display = true;
        snapshot.display = Some(DisplayPlacement {
            device: device.clone(),
            rect: DisplayRect {
                x,
                y: 0,
                width: 1280,
                height: 720,
            },
            scale_milli: 1250,
            stamp: FabricStamp { counter: 7, device },
            outputs: vec![crate::shell::device_fabric::DisplayConnectorPlacement {
                connector: "eDP-1".into(),
                label: "Panel".into(),
                rect: DisplayRect {
                    x: 0,
                    y: 0,
                    width: 1280,
                    height: 720,
                },
                scale_milli: 1250,
            }],
        });
        snapshot
    }

    fn drag_pair() -> (
        DeviceFabric,
        DeviceId,
        DeviceId,
        settings_store::device_displays::DeviceDisplayMove,
    ) {
        let local = DeviceId::new("local-drag").unwrap();
        let peer = DeviceId::new("peer-drag").unwrap();
        let mut fabric = DeviceFabric::default();
        fabric.upsert(drag_snapshot(local.clone(), 0)).unwrap();
        fabric.upsert(drag_snapshot(peer.clone(), 1280)).unwrap();
        let screen = observed_device_displays(&fabric, Some(&local))
            .into_iter()
            .find(|screen| !screen.local)
            .unwrap();
        let request = settings_store::device_displays::DeviceDisplayMove {
            screen,
            x: 0,
            y: 720,
        };
        (fabric, local, peer, request)
    }

    #[test]
    fn shared_pointer_layout_retains_checked_origins_and_observed_output_offsets() {
        use crate::shell::device_fabric::DisplayOutputObservation;
        use settings_store::person_displays::{Arrangement, DeviceOutput};
        let (mut fabric, local, remote, _) = drag_pair();
        let unknown = person_layout::resolve(
            &fabric,
            &local,
            &Arrangement::default(),
            &fabric.person_devices(&local),
        );
        assert!(unknown.layout.displays.is_empty());
        assert_eq!(unknown.legacy_targets, vec![remote.clone()]);
        // Negative local output origins must not turn a device bounding box
        // into its desktop origin. Both the admitted connector and rich facts
        // report the same desktop-relative coordinate.
        for owner in [&local, &remote] {
            let mut snapshot = fabric.snapshots().get(owner).unwrap().clone();
            snapshot.display.as_mut().unwrap().stamp.counter += 1;
            snapshot.presence.stamp.counter += 1;
            snapshot.display.as_mut().unwrap().outputs[0].rect.x = -120;
            let display = snapshot.display.as_ref().unwrap().clone();
            fabric.upsert(snapshot).unwrap();
            let connector = &display.outputs[0];
            fabric
                .observe_display_outputs(
                    owner,
                    &display.stamp,
                    vec![DisplayOutputObservation {
                        connector: connector.connector.clone(),
                        output: DeviceOutput {
                            id: format!("monitor:{}", owner.as_str()),
                            name: "Panel".into(),
                            builtin: true,
                            x: connector.rect.x,
                            y: connector.rect.y,
                            width: connector.rect.width,
                            height: connector.rect.height,
                            mode_width: 1600,
                            mode_height: 900,
                            refresh_mhz: 60_000,
                            scale_milli: connector.scale_milli,
                            connected: true,
                            enabled: true,
                        },
                    }],
                )
                .unwrap();
        }
        let view = person_layout::resolve(
            &fabric,
            &local,
            &Arrangement::default(),
            &fabric.person_devices(&local),
        );
        assert!(view.legacy_targets.is_empty());
        let local_display = view
            .layout
            .find(local.as_str(), &format!("monitor:{}", local.as_str()))
            .unwrap();
        let remote_display = view
            .layout
            .find(remote.as_str(), &format!("monitor:{}", remote.as_str()))
            .unwrap();
        assert_eq!(local_display.local_rect.x, -120);
        assert_eq!(local_display.rect.x, -120);
        assert_eq!(remote_display.local_rect.x, -120);
        assert_eq!(remote_display.rect.x, 1160);
        assert_eq!(remote_display.to_device((1160.5, 40.25)), (-119.5, 40.25));
        assert_eq!(remote_display.to_shared((-119.5, 40.25)), (1160.5, 40.25));
        // The checked target-owned placement remains the anchor until an
        // explicit arrangement pins this device elsewhere.
        let mut snapshot = fabric.snapshots().get(&remote).unwrap().clone();
        let display = snapshot.display.as_mut().unwrap();
        display.rect.x = 2000;
        display.stamp.counter += 1;
        snapshot.presence.stamp.counter += 1;
        let stamp = display.stamp.clone();
        let facts = fabric.display_outputs(&remote).unwrap().to_vec();
        fabric.upsert(snapshot).unwrap();
        fabric
            .observe_display_outputs(&remote, &stamp, facts)
            .unwrap();
        assert!(
            !view.is_current(&fabric, &local),
            "a stale pointer layout must stop routing immediately"
        );
        let moved = person_layout::resolve(
            &fabric,
            &local,
            &Arrangement::default(),
            &fabric.person_devices(&local),
        );
        assert!(moved.is_current(&fabric, &local));
        assert!(!moved.is_current(&fabric, &remote));
        assert_eq!(moved.layout.device_bounds(remote.as_str()).unwrap().x, 1880);
    }

    #[test]
    fn display_drag_expired_pump_cannot_apply_a_delayed_gesture() {
        let (fabric, local, peer, request) = drag_pair();
        let (_, expected, rect) = checked_display_move(&fabric, &local, &request).unwrap();
        let mut input = SpatialInputState::default();
        input.apply_checked_display_move(
            &fabric,
            &peer,
            &PersonalFabricAction::SetDisplayPlacementChecked {
                expected,
                rect,
                expires_unix_ms: 0,
            },
        );
        assert!(input.placement_override.is_none());
        assert!(input.issued.is_empty() && input.received.is_empty());
    }

    #[test]
    fn display_drag_validates_exact_revision_geometry_and_online_owner() {
        let (mut fabric, local, peer, request) = drag_pair();
        let (target, expected, rect) = checked_display_move(&fabric, &local, &request).unwrap();
        assert_eq!(target, peer);
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (0, 720, 1280, 720)
        );
        let owner = fabric
            .snapshots()
            .get(&peer)
            .unwrap()
            .display
            .as_ref()
            .unwrap();
        let mut input = SpatialInputState::default();
        assert!(input.accept_display_move(&fabric, &peer, &expected, rect));
        assert_eq!(input.placement_override, Some(rect));
        assert!(
            !input.accept_display_move(
                &fabric,
                &peer,
                &expected,
                DisplayRect {
                    x: -1280,
                    y: 0,
                    ..rect
                }
            ),
            "another concurrent drag over that old revision cannot replace an unpublished move"
        );
        assert!(
            input.issued.is_empty() && input.received.is_empty() && input.inbound.is_empty(),
            "arranging screens grants no input access"
        );

        let mut resize = rect;
        resize.width += 1;
        assert!(
            !owner.accepts_move(&expected, resize),
            "placement is not a remote mode change"
        );
        let mut forged = expected.clone();
        forged.device = local.clone();
        assert!(!owner.accepts_move(&forged, rect));
        let mut stale = request.clone();
        stale.screen.revision -= 1;
        assert!(checked_display_move(&fabric, &local, &stale).is_err());
        let mut collision = request.clone();
        collision.y = 0;
        assert!(checked_display_move(&fabric, &local, &collision).is_err());
        let mut detached = request.clone();
        detached.y = 2000;
        assert!(
            checked_display_move(&fabric, &local, &detached).is_err(),
            "absolute moves must leave a real screen edge to cross"
        );
        let mut overflow = request.clone();
        overflow.x = i32::MAX;
        assert!(checked_display_move(&fabric, &local, &overflow).is_err());
        let mut changed = drag_snapshot(peer.clone(), 1280);
        changed.presence.online = false;
        changed.presence.stamp.counter += 1;
        fabric.upsert(changed).unwrap();
        assert!(checked_display_move(&fabric, &local, &request).is_err());
        assert_eq!(
            request.screen.scale_milli, 1250,
            "native logical coordinates preserve scale observation"
        );
    }

    #[test]
    fn display_drag_publication_preserves_stamps_and_restores_live_extents() {
        let (fabric, local, peer, request) = drag_pair();
        let (_, expected, rect) = checked_display_move(&fabric, &local, &request).unwrap();
        let before = fabric.snapshots().get(&peer).unwrap().clone();
        let fingerprint = local_snapshot_fingerprint(&before).unwrap();
        assert_eq!(
            before.display.as_ref().unwrap().stamp.counter,
            7,
            "fingerprinting never erases the authored publication stamp"
        );
        let mut newer = before.clone();
        stamp_snapshot(
            &mut newer,
            &FabricStamp {
                counter: 8,
                device: peer.clone(),
            },
            42,
        );
        assert_eq!(
            local_snapshot_fingerprint(&newer),
            Some(fingerprint),
            "heartbeat does not change topology identity"
        );
        newer.display.as_mut().unwrap().rect = rect;
        preserve_unchanged_stamps(&mut newer, Some(&before));
        assert_eq!(newer.display.as_ref().unwrap().stamp.counter, 8);
        assert_ne!(local_snapshot_fingerprint(&newer), Some(fingerprint));
        assert!(
            !newer
                .display
                .as_ref()
                .unwrap()
                .accepts_move(&expected, rect),
            "a new owner publication rejects the old drag"
        );
        let mut saved = DeviceFabric::default();
        saved.upsert(newer).unwrap();
        let checkpoint =
            DeviceFabric::decode_checkpoint(&saved.encode_checkpoint().unwrap()).unwrap();
        let previous = checkpoint.snapshots().get(&peer).unwrap().display.as_ref();
        assert!(
            !previous
                .unwrap()
                .accepts_move(&previous.unwrap().stamp, rect),
            "a checkpoint is not live connector discovery after restart"
        );

        let mut restarted = SpatialInputState::default();
        let changed_mode = DisplayRect {
            x: 0,
            y: 0,
            width: 640,
            height: 360,
        };
        assert_eq!(
            restarted.placed_local_rect(Some(changed_mode), previous),
            Some(DisplayRect {
                x: rect.x,
                y: rect.y,
                ..changed_mode
            }),
            "reconnect uses existing checkpoint origin and current DPI/extent"
        );
        assert!(
            restarted.placed_local_rect(None, previous).is_none(),
            "a stored placement cannot resurrect unplugged screens"
        );
    }

    #[test]
    fn roaming_settings_travel_through_the_session_records_and_their_copy() {
        let dir = tempfile::tempdir().unwrap();
        let paths = settings_store::SessionPaths::beside(dir.path().join("settings.json"));
        let open = || {
            settings_store::install_source(Some(Arc::new(
                settings_store::SessionSettings::open(paths.clone()).unwrap(),
            )));
        };
        open();

        // A winner from another device reaches this process's readers and the
        // copy the compositor boots from and apps read.
        apply_roaming_settings(&BTreeMap::from([(
            "tilo.appearance.mode".to_string(),
            Value::from("light"),
        )]));
        assert_eq!(settings_store::load_section("appearance")["mode"], "light");
        assert_eq!(
            settings_store::load_section_at(&paths.copy, "appearance")["mode"],
            "light"
        );

        // Settings, in its own process, writes the copy; the fabric publishes
        // that edit, and a device-local leaf beside it stays out.
        settings_store::update_section_at(
            &paths.copy,
            "appearance",
            serde_json::json!({"mode": "dark", "wallpaper_path": "/home/ada/sea.png"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(
            roaming_setting_values(),
            BTreeMap::from([("tilo.appearance.mode".to_string(), Value::from("dark"))])
        );

        // A restarted session reads the same records.
        settings_store::install_source(None);
        std::fs::remove_file(&paths.copy).unwrap();
        open();
        assert_eq!(
            settings_store::load_section("appearance"),
            serde_json::json!({"mode": "dark", "wallpaper_path": "/home/ada/sea.png"})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(
            settings_store::load_section_at(&paths.copy, "appearance")["mode"],
            "dark"
        );
        settings_store::install_source(None);
    }

    #[test]
    fn health_probes_every_online_peer_once_per_cadence() {
        let speaker = DeviceId::new("health-speaker").unwrap();
        let tablet = DeviceId::new("health-tablet").unwrap();
        let online = || vec![speaker.clone(), tablet.clone()];
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut state = DeviceHealthRuntimeState::default();
        assert_eq!(health_probe_targets(&mut state, online(), at(0)), online());
        assert!(health_probe_targets(&mut state, online(), at(300)).is_empty());
        assert_eq!(
            health_probe_targets(&mut state, online(), at(10_000)),
            online()
        );
    }

    #[test]
    fn health_probe_never_samples_power_inline() {
        use crate::shell::spatial_calibration::DevicePower;
        let peer = DeviceId::new("health-probe-peer").unwrap();
        let start = Instant::now();
        let mut state = DeviceHealthRuntimeState::default();
        let (release, gate) = mpsc::channel::<()>();
        let (entered, sampling) = mpsc::channel::<()>();
        let blocked_sampler = move || {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            (DevicePower::Battery(40), true)
        };

        assert!(
            answer_health_probe(&mut state, peer.clone(), [3; 16], start, blocked_sampler)
                .is_none(),
            "an uncached probe waits for a background sample"
        );
        sampling
            .recv_timeout(Duration::from_secs(10))
            .expect("the sample runs on its own thread");
        assert!(
            answer_health_probe(&mut state, peer.clone(), [4; 16], start, || {
                panic!("one power sample at a time")
            })
            .is_none()
        );
        assert!(
            finish_power_sample(&mut state, start).is_empty(),
            "the sampler is still blocked, and so is nothing else"
        );

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let replies = loop {
            let replies = finish_power_sample(&mut state, start);
            if !replies.is_empty() || Instant::now() > deadline {
                break replies;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            replies
                .iter()
                .map(|(to, report)| (
                    to.clone(),
                    report.probe_nonce,
                    report.power,
                    report.energy_saver
                ))
                .collect::<Vec<_>>(),
            vec![
                (peer.clone(), [3; 16], DevicePower::Battery(40), true),
                (peer.clone(), [4; 16], DevicePower::Battery(40), true),
            ],
            "both waiting probes are answered from the one sample"
        );

        let cached = answer_health_probe(
            &mut state,
            peer.clone(),
            [5; 16],
            start + Duration::from_secs(1),
            || panic!("a fresh sample is answered from the cache"),
        );
        assert_eq!(
            cached.map(|(to, report)| (to, report.probe_nonce)),
            Some((peer, [5; 16]))
        );
    }

    #[test]
    fn clock_sync_handles_are_counted_until_the_last_is_dropped() {
        let peer = DeviceId::new("clock-handle-peer").unwrap();
        let held = || with_fabric_clock(|core| core.is_held(&peer));
        let first = hold_fabric_clock_sync(peer.clone());
        let second = hold_fabric_clock_sync(peer.clone());
        assert!(held());
        drop(first);
        assert!(held());
        assert_eq!(second.peer(), &peer);
        assert!(second.estimate().is_none());
        drop(second);
        assert!(!held());
    }

    fn provenance_snapshot(
        device: DeviceId,
        label: &str,
        counter: u64,
        setting: Option<Value>,
    ) -> DeviceFabricSnapshot {
        let stamp = FabricStamp {
            counter,
            device: device.clone(),
        };
        DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: device.clone(),
            presence: DevicePresence {
                device,
                label: label.into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities::default(),
                online: true,
                last_seen_unix_ms: counter,
                wake_routes: Vec::new(),
                stamp: stamp.clone(),
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: setting
                .map(|value| RoamingSetting {
                    key: "tilo.appearance.mode".into(),
                    value,
                    stamp,
                })
                .into_iter()
                .collect(),
            wifi: Vec::new(),
            display: None,
            unlock: crate::shell::device_unlock::UnlockFabricState::default(),
            speakers: crate::shell::speaker_group::SpeakerFabricState::default(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn nearby_runtime_bounds_history_staging_and_consent_and_obeys_the_kill_switch() {
        fn nearby_receive_history_is_bounded_and_replaces_transfer_health() {
            let peer = DeviceId::new("history-peer").expect("valid device");
            let mut state = NearbyDropState::default();
            for index in 0..40_u8 {
                state.record_history(NearbyTransferHistory {
                    transfer_id: [index; 16],
                    peer: peer.clone(),
                    name: format!("item-{index}"),
                    phase: runtime_collab::NearbyTransferPhase::Completed,
                    total_bytes: 1,
                });
            }
            assert_eq!(state.history.len(), 32);
            state.record_history(NearbyTransferHistory {
                transfer_id: [39; 16],
                peer,
                name: "item-39".into(),
                phase: runtime_collab::NearbyTransferPhase::Failed,
                total_bytes: 1,
            });
            assert_eq!(state.history.len(), 32);
            assert_eq!(
                state.history.front().map(|entry| entry.phase),
                Some(runtime_collab::NearbyTransferPhase::Failed)
            );
        }

        fn nearby_staging_admission_bounds_global_and_per_owner_work() {
            let mut admission = NearbyStagingAdmission::default();
            assert!(admission.acquire("workspace-a"));
            assert!(admission.acquire("workspace-a"));
            assert!(!admission.acquire("workspace-a"));
            assert!(admission.acquire("workspace-b"));
            assert!(admission.acquire("workspace-c"));
            assert!(!admission.acquire("workspace-d"));
            admission.release("workspace-a");
            assert!(admission.acquire("workspace-d"));
            admission.release("workspace-a");
            assert!(!admission.owners.contains_key("workspace-a"));
            let generation = admission.generation;
            admission.invalidate();
            assert!(admission.owners.is_empty());
            assert_ne!(admission.generation, generation);
            assert!(admission.acquire("workspace-a"));
            admission.release_if_generation("workspace-a", generation);
            assert_eq!(admission.owners.get("workspace-a"), Some(&1));
            let current_generation = admission.generation;
            admission.release_if_generation("workspace-a", current_generation);
            assert!(!admission.owners.contains_key("workspace-a"));
        }

        fn nearby_consent_queue_is_bounded_and_requires_explicit_resolution() {
            let source = DeviceId::new("source-device").expect("valid device");
            let mut state = NearbyDropState::default();
            for index in 1..=MAX_PENDING_NEARBY_CONSENTS {
                let mut id = [0_u8; 16];
                id[..8]
                    .copy_from_slice(&u64::try_from(index).expect("bounded index").to_le_bytes());
                assert!(state.insert_consent(
                    id,
                    PendingNearbyConsent {
                        source: source.clone(),
                        request_id: id,
                        name: format!("file-{index}"),
                        total_bytes: 1,
                        received: Instant::now(),
                        payload: PendingNearbyPayload::Inline {
                            bytes: vec![1],
                            content_hash: *blake3::hash(&[1]).as_bytes(),
                        },
                    },
                ));
            }
            let first_id = 1_u64.to_le_bytes();
            let mut duplicate_id = [0_u8; 16];
            duplicate_id[..8].copy_from_slice(&first_id);
            let colliding_source = DeviceId::new("colliding-device").expect("valid device");
            assert!(!state.insert_consent(
                duplicate_id,
                PendingNearbyConsent {
                    source: colliding_source,
                    request_id: [0xaa; 16],
                    name: "collision".into(),
                    total_bytes: 1,
                    received: Instant::now(),
                    payload: PendingNearbyPayload::Inline {
                        bytes: vec![2],
                        content_hash: *blake3::hash(&[2]).as_bytes(),
                    },
                },
            ));
            assert_eq!(
                state
                    .incoming
                    .get(&duplicate_id)
                    .map(|pending| pending.source.as_str()),
                Some("source-device")
            );
            assert_eq!(state.incoming.len(), MAX_PENDING_NEARBY_CONSENTS);
            assert!(!state.insert_consent(
                [0xff; 16],
                PendingNearbyConsent {
                    source,
                    request_id: [0xff; 16],
                    name: "overflow".into(),
                    total_bytes: 1,
                    received: Instant::now(),
                    payload: PendingNearbyPayload::Inline {
                        bytes: vec![1],
                        content_hash: *blake3::hash(&[1]).as_bytes(),
                    },
                },
            ));
            assert!(state.events.is_empty());
        }

        fn master_kill_switch_removes_runtime_advertised_capabilities() {
            let local = LocalFabricCapabilities {
                display: Some(DisplayRect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                }),
                pointer: true,
                keyboard: true,
                audio_output: true,
            };
            let document = serde_json::json!({"continuity": {
                "enabled": false,
                "clipboard": true,
                "wifi_settings": true,
                "media": true,
                "display_input": true,
                "app_continuations": true
            }});
            let policy = ContinuityPolicySnapshot::from_document(document.as_object().unwrap());
            assert_eq!(
                policy_capabilities(policy, local),
                FabricCapabilities::default()
            );
        }

        nearby_receive_history_is_bounded_and_replaces_transfer_health();
        nearby_staging_admission_bounds_global_and_per_owner_work();
        nearby_consent_queue_is_bounded_and_requires_explicit_resolution();
        master_kill_switch_removes_runtime_advertised_capabilities();
    }

    #[test]
    fn media_handoff_offer_is_admitted_only_by_its_exact_waiting_handoff() {
        fn unsolicited_media_offer_never_reaches_consent_or_storage() {
            let (network, _commands) = crate::shell::network::shell_network_for_test(
                crate::shell::state::WorkspaceTree::default().shared(),
                0x004d_4f46_4645,
            );
            let source = DeviceId::new("unsolicited-media-source").unwrap();
            let transfer_id = [0x7d; 16];
            handle_nearby_drop_action(
                &network,
                [0x7e; 16],
                source.clone(),
                PersonalFabricAction::NearbyTransferOffer {
                    transfer_id,
                    name: "unsolicited.mp4".into(),
                    kind: crate::shell::nearby_transfer::TransferKind::MediaHandoff,
                    total_bytes: 4,
                    content_hash: *blake3::hash(b"evil").as_bytes(),
                },
            );
            let state = nearby_drop_state().lock().unwrap();
            assert!(
                !state.incoming.contains_key(&transfer_id),
                "a media handoff never asks Files for consent"
            );
            assert!(!state.media_authorized.contains(&(source, transfer_id)));
            drop(state);
            assert!(
                with_open_nearby_transfer_store(|store| {
                    store.inbox().iter().any(|entry| entry.id == transfer_id)
                })
                .is_none_or(|stored| !stored),
                "an enrolled device alone cannot allocate transfer storage"
            );
        }

        fn media_authorisation_is_exact_and_bounded() {
            let source = DeviceId::new("media-source").unwrap();
            let sibling = DeviceId::new("sibling-source").unwrap();
            let mut state = NearbyDropState::default();
            state.authorize_media(source.clone(), [1; 16]);
            assert!(state.media_authorized.contains(&(source.clone(), [1; 16])));
            assert!(!state.media_authorized.contains(&(sibling, [1; 16])));
            assert!(!state.media_authorized.contains(&(source.clone(), [2; 16])));
            for index in 0..u8::try_from(MAX_MEDIA_AUTHORIZATIONS).unwrap() {
                state.authorize_media(source.clone(), [index.wrapping_add(10); 16]);
            }
            assert_eq!(state.media_authorized.len(), MAX_MEDIA_AUTHORIZATIONS);
        }

        fn media_names_stay_portable_on_the_transfer_lane() {
            assert_eq!(
                portable_media_name("live/at\\home\u{7}.flac").as_deref(),
                Some("live_at_home_.flac")
            );
            let long = "é".repeat(200);
            let bounded = portable_media_name(&long).unwrap();
            assert!(bounded.len() <= 255);
            assert!(long.starts_with(&bounded));
            assert_eq!(portable_media_name(".."), None);
            assert_eq!(portable_media_name(""), None);
        }

        unsolicited_media_offer_never_reaches_consent_or_storage();
        media_authorisation_is_exact_and_bounded();
        media_names_stay_portable_on_the_transfer_lane();
    }

    fn pending_media(
        request_id: [u8; 16],
        source: DeviceId,
        received: Instant,
    ) -> PendingIncomingMedia {
        let route = crate::shell::network::AuthenticatedPersonalRoute::for_test(source.clone());
        PendingIncomingMedia {
            received,
            request_id,
            source,
            session_id: format!("session-{}", request_id[0]),
            app_id: "so.tilo.music".into(),
            workspace_id: "00000000-0000-0000-0000-000000000001".into(),
            title: "Track".into(),
            locator: MediaSourceLocator::RemoteStream {
                url: "https://media.example/track.mp3".into(),
            },
            position_ms: 1_000,
            resume_playing: true,
            has_video: false,
            route,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_requests_cancel_tear_down_and_fail_only_on_their_exact_route() {
        fn source_media_cancel_uses_exact_target_and_only_personal_transfer_id() {
            let (network, mut commands) = crate::shell::network::shell_network_for_test(
                crate::shell::state::WorkspaceTree::default().shared(),
                0x4341_4e43_454c,
            );
            let endpoint = iroh::SecretKey::from_bytes(&[0xC1; 32]).public();
            let target = DeviceId::new(hex::encode(endpoint.as_bytes())).unwrap();
            let transfer_id = [0xC2; 16];
            let status = MediaHandoffStatus {
                request_id: [0xC3; 16],
                session_id: "personal-file-session".into(),
                title: "Track".into(),
                target: target.clone(),
                phase: MediaHandoffPhase::Pending,
                updated: Instant::now(),
                route: crate::shell::network::AuthenticatedPersonalRoute::for_test(target.clone()),
                personal_transfer_id: Some(transfer_id),
            };
            nearby_drop_state()
                .lock()
                .unwrap()
                .large_offers
                .insert(transfer_id, (target, [0xC4; 16], "film.mkv".into()));
            cancel_status_personal_transfer(&network, &status);
            assert!(matches!(
                commands.try_recv(),
                Ok(crate::network::actor::NetCmd::CancelNearbyTransfer {
                    target: cancelled_target,
                    transfer_id: cancelled_id,
                }) if cancelled_target == endpoint && cancelled_id == transfer_id
            ));
            assert!(
                !nearby_drop_state()
                    .lock()
                    .unwrap()
                    .large_offers
                    .contains_key(&transfer_id),
                "the destination's offer is withdrawn with the handoff"
            );
            while commands.try_recv().is_ok() {}

            let streaming = MediaHandoffStatus {
                personal_transfer_id: None,
                ..status
            };
            cancel_status_personal_transfer(&network, &streaming);
            assert!(commands.try_recv().is_err());
        }

        fn incoming_media_cancel_is_exact_and_timeout_clears_late_authority() {
            let (network, _commands) = crate::shell::network::shell_network_for_test(
                crate::shell::state::WorkspaceTree::default().shared(),
                0x004d_4544_4941,
            );
            let mut runtime = DeviceFabricRuntime::new(&network, NotificationBridge::new());
            let source = DeviceId::new("media-source").unwrap();
            let exact = pending_media([31; 16], source.clone(), Instant::now());
            let sibling = pending_media([32; 16], source, Instant::now());
            let expired = pending_media(
                [33; 16],
                DeviceId::new("expired-source").unwrap(),
                Instant::now()
                    .checked_sub(INCOMING_MEDIA_WAIT + Duration::from_secs(1))
                    .unwrap(),
            );
            runtime
                .waiting_incoming_media
                .insert(exact.request_id, exact.clone());
            runtime
                .pending_incoming_media
                .insert(sibling.request_id, sibling.clone());
            runtime
                .waiting_incoming_media
                .insert(expired.request_id, expired);

            let cancelled = runtime.take_incoming_media(|pending| {
                pending.request_id == exact.request_id
                    && pending.source == exact.source
                    && pending.session_id == exact.session_id
            });
            DeviceFabricRuntime::terminate_incoming_media(&network, cancelled, None);
            assert!(
                !runtime
                    .waiting_incoming_media
                    .contains_key(&exact.request_id)
            );
            assert_eq!(
                runtime.pending_incoming_media.get(&sibling.request_id),
                Some(&sibling)
            );
            runtime.expire_incoming_media(&network);
            assert!(runtime.waiting_incoming_media.is_empty());
            assert_eq!(runtime.pending_incoming_media.len(), 1);
        }

        fn media_policy_teardown_revokes_waiting_and_launched_requests() {
            let (network, _commands) = crate::shell::network::shell_network_for_test(
                crate::shell::state::WorkspaceTree::default().shared(),
                0x504f_4c49_4359,
            );
            let mut runtime = DeviceFabricRuntime::new(&network, NotificationBridge::new());
            let source = DeviceId::new("policy-media-source").unwrap();
            let waiting = pending_media([34; 16], source.clone(), Instant::now());
            let launched = pending_media([35; 16], source, Instant::now());
            runtime
                .waiting_incoming_media
                .insert(waiting.request_id, waiting);
            runtime
                .pending_incoming_media
                .insert(launched.request_id, launched);

            runtime.tear_down_incoming_media(&network, MediaHandoffFailure::Cancelled);

            assert!(runtime.waiting_incoming_media.is_empty());
            assert!(runtime.pending_incoming_media.is_empty());
        }

        fn media_route_loss_is_exact_across_workspace_and_reconnect_epochs() {
            let (network, _commands) = crate::shell::network::shell_network_for_test(
                crate::shell::state::WorkspaceTree::default().shared(),
                0x0045_504f_4348,
            );
            let endpoint = iroh::SecretKey::from_bytes(&[0x91; 32]).public();
            let sibling_endpoint = iroh::SecretKey::from_bytes(&[0x92; 32]).public();
            let old_epoch = crate::network::actor::MembershipEpoch::for_test(7);
            let sibling_epoch = crate::network::actor::MembershipEpoch::for_test(8);
            let route =
                network.install_personal_route_for_test(77, endpoint, old_epoch, [0x93; 32]);
            let sibling_route = network.install_personal_route_for_test(
                77,
                sibling_endpoint,
                sibling_epoch,
                [0x93; 32],
            );
            let mut runtime = DeviceFabricRuntime::new(&network, NotificationBridge::new());
            let mut departed = pending_media([0x94; 16], route.device().clone(), Instant::now());
            departed.route = route;
            let mut sibling =
                pending_media([0x95; 16], sibling_route.device().clone(), Instant::now());
            sibling.route = sibling_route;
            runtime
                .pending_incoming_media
                .insert(departed.request_id, departed.clone());
            runtime
                .pending_incoming_media
                .insert(sibling.request_id, sibling.clone());

            network
                .event_tx
                .try_send(crate::network::actor::NetEvent::PeerLeft {
                    ws: 78,
                    peer: endpoint,
                    cause: crate::network::actor::PeerDepartureCause::LivenessTimedOut,
                    membership_epoch: Some(old_epoch),
                })
                .unwrap();
            network.tick();
            runtime.prune_media_routes(&network);
            assert!(
                runtime
                    .pending_incoming_media
                    .contains_key(&departed.request_id)
            );

            network
                .event_tx
                .try_send(crate::network::actor::NetEvent::PeerLeft {
                    ws: 77,
                    peer: endpoint,
                    cause: crate::network::actor::PeerDepartureCause::LivenessTimedOut,
                    membership_epoch: Some(crate::network::actor::MembershipEpoch::for_test(6)),
                })
                .unwrap();
            network.tick();
            runtime.prune_media_routes(&network);
            assert!(
                runtime
                    .pending_incoming_media
                    .contains_key(&departed.request_id)
            );

            network
                .event_tx
                .try_send(crate::network::actor::NetEvent::PeerLeft {
                    ws: 77,
                    peer: endpoint,
                    cause: crate::network::actor::PeerDepartureCause::LivenessTimedOut,
                    membership_epoch: Some(old_epoch),
                })
                .unwrap();
            network.tick();
            runtime.prune_media_routes(&network);
            assert!(
                !runtime
                    .pending_incoming_media
                    .contains_key(&departed.request_id)
            );
            assert_eq!(
                runtime.pending_incoming_media.get(&sibling.request_id),
                Some(&sibling)
            );
        }

        fn failed_mpris_acceptance_ack_pauses_only_the_exact_controller() {
            let mut paused = Vec::new();
            assert!(compensate_failed_media_acceptance(
                &Err::<(), _>("route departed".into()),
                Some("org.mpris.MediaPlayer2.exact"),
                Some("https://media.example/exact.mp3"),
                |controller, uri| {
                    paused.push((controller.to_string(), uri.to_string()));
                    true
                },
            ));
            assert_eq!(
                paused,
                [(
                    "org.mpris.MediaPlayer2.exact".to_string(),
                    "https://media.example/exact.mp3".to_string()
                )]
            );
            assert!(!compensate_failed_media_acceptance(
                &Ok(()),
                Some("org.mpris.MediaPlayer2.must-not-pause"),
                Some("https://media.example/unrelated.mp3"),
                |_, _| true,
            ));
        }

        source_media_cancel_uses_exact_target_and_only_personal_transfer_id();
        incoming_media_cancel_is_exact_and_timeout_clears_late_authority();
        media_policy_teardown_revokes_waiting_and_launched_requests();
        media_route_loss_is_exact_across_workspace_and_reconnect_epochs();
        failed_mpris_acceptance_ack_pauses_only_the_exact_controller();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn imported_state_is_stamped_but_never_reauthored() {
        fn clipboard_policy_transition_clears_imports_and_never_reauthors_them() {
            use crate::compositor::selection::{
                SelectionStore, Target, clipboard_history, pin_history_item, set_mirror_text,
            };

            let policy = |clipboard| {
                let document = serde_json::json!({"continuity": {
                    "enabled": true,
                    "clipboard": clipboard,
                    "notifications": true,
                    "wifi_settings": true,
                    "media": true,
                    "display_input": true,
                    "file_transfer": true,
                    "realtime_sessions": true,
                    "typed_gamepad": true,
                    "app_continuations": true
                }});
                ContinuityPolicySnapshot::from_document(document.as_object().unwrap())
            };
            let local = DeviceId::new("local-clipboard-device").unwrap();
            let remote = DeviceId::new("remote-clipboard-device").unwrap();
            let selection = std::sync::Arc::new(std::sync::Mutex::new(SelectionStore::default()));
            assert!(set_mirror_text(
                &selection,
                Target::Clipboard,
                "keep this local copy"
            ));
            let local_id = clipboard_history(&selection)[0].id;
            assert!(pin_history_item(&selection, local_id, true));

            let mut imported = ClipboardEntry::shareable(
                ClipboardPayload::Text("remote private copy".into()),
                remote.clone(),
                42,
                FabricStamp {
                    counter: 7,
                    device: remote,
                },
            )
            .unwrap();
            imported.pinned = true;
            merge_shared_clipboard_history(&selection, std::slice::from_ref(&imported));
            assert_eq!(clipboard_history(&selection).len(), 2);

            let (network, _commands) = crate::shell::network::shell_network_for_test(
                crate::shell::state::WorkspaceTree::default().shared(),
                1,
            );
            let mut runtime = DeviceFabricRuntime::new(&network, NotificationBridge::new());
            let enabled = policy(true);
            runtime.enforce_policy(&network, &selection, enabled);
            let stamp = FabricStamp {
                counter: 10,
                device: local.clone(),
            };
            let capabilities = LocalFabricCapabilities {
                display: None,
                pointer: false,
                keyboard: false,
                audio_output: false,
            };
            let enabled_snapshot = build_local_snapshot(
                &local,
                "Local".into(),
                capabilities,
                &selection,
                None,
                &BTreeMap::new(),
                &stamp,
                enabled,
            );
            assert_eq!(enabled_snapshot.clipboard.len(), 1);
            assert_eq!(enabled_snapshot.clipboard[0].origin, local);

            let disabled = policy(false);
            runtime.enforce_policy(&network, &selection, disabled);
            let history = clipboard_history(&selection);
            assert_eq!(
                history.len(),
                1,
                "remote history must be revoked immediately"
            );
            assert_eq!(
                history[0].id, local_id,
                "local pinned history is device-private"
            );
            assert!(history[0].pinned);
            let disabled_snapshot = build_local_snapshot(
                &local,
                "Local".into(),
                capabilities,
                &selection,
                Some(&enabled_snapshot),
                &BTreeMap::new(),
                &stamp,
                disabled,
            );
            assert!(disabled_snapshot.clipboard.is_empty());
            assert!(!disabled_snapshot.presence.capabilities.clipboard);

            runtime.enforce_policy(&network, &selection, enabled);
            merge_shared_clipboard_history(&selection, std::slice::from_ref(&imported));
            assert_eq!(clipboard_history(&selection).len(), 2);
            let reenabled_snapshot = build_local_snapshot(
                &local,
                "Local".into(),
                capabilities,
                &selection,
                Some(&disabled_snapshot),
                &BTreeMap::new(),
                &stamp,
                enabled,
            );
            assert_eq!(reenabled_snapshot.clipboard.len(), 1);
            assert_eq!(reenabled_snapshot.clipboard[0].origin, local);
            assert_ne!(
                reenabled_snapshot.clipboard[0].id, imported.id,
                "an imported entry must not be re-authored after policy re-enable"
            );
        }

        fn snapshot_stamping_updates_every_authored_record() {
            let device = DeviceId::new("laptop").unwrap();
            let mut snapshot = DeviceFabricSnapshot {
                schema_version: FABRIC_SCHEMA_VERSION,
                owner: device.clone(),
                presence: DevicePresence {
                    device: device.clone(),
                    label: "Laptop".into(),
                    kind: DeviceKind::Laptop,
                    capabilities: FabricCapabilities::default(),
                    online: true,
                    last_seen_unix_ms: 0,
                    wake_routes: Vec::new(),
                    stamp: FabricStamp {
                        counter: 0,
                        device: device.clone(),
                    },
                },
                media: Vec::new(),
                clipboard: Vec::new(),
                settings: Vec::new(),
                wifi: Vec::new(),
                display: None,
                unlock: crate::shell::device_unlock::UnlockFabricState::default(),
                speakers: crate::shell::speaker_group::SpeakerFabricState::default(),
            };
            let stamp = FabricStamp { counter: 9, device };
            stamp_snapshot(&mut snapshot, &stamp, 42);
            assert_eq!(snapshot.presence.stamp, stamp);
            assert_eq!(snapshot.presence.last_seen_unix_ms, 42);
        }

        fn remotely_applied_setting_is_not_reauthored_but_a_user_edit_is() {
            let device = DeviceId::new("laptop").unwrap();
            let old_stamp = FabricStamp {
                counter: 3,
                device: device.clone(),
            };
            let mut previous = DeviceFabricSnapshot {
                schema_version: FABRIC_SCHEMA_VERSION,
                owner: device.clone(),
                presence: DevicePresence {
                    device: device.clone(),
                    label: "Laptop".into(),
                    kind: DeviceKind::Laptop,
                    capabilities: FabricCapabilities::default(),
                    online: true,
                    last_seen_unix_ms: 0,
                    wake_routes: Vec::new(),
                    stamp: old_stamp.clone(),
                },
                media: Vec::new(),
                clipboard: Vec::new(),
                settings: vec![RoamingSetting {
                    key: "tilo.appearance.mode".into(),
                    value: Value::Bool(false),
                    stamp: old_stamp,
                }],
                wifi: Vec::new(),
                display: None,
                unlock: crate::shell::device_unlock::UnlockFabricState::default(),
                speakers: crate::shell::speaker_group::SpeakerFabricState::default(),
            };
            let next_stamp = FabricStamp { counter: 9, device };
            let applied_remote =
                BTreeMap::from([("tilo.appearance.mode".into(), Value::Bool(true))]);

            let received = reconcile_roaming_settings(
                BTreeMap::from([("tilo.appearance.mode".into(), Value::Bool(true))]),
                Some(&previous),
                &applied_remote,
                &next_stamp,
            );
            assert_eq!(received, previous.settings);

            let edited = reconcile_roaming_settings(
                BTreeMap::from([("tilo.appearance.mode".into(), Value::Bool(false))]),
                Some(&previous),
                &applied_remote,
                &next_stamp,
            );
            assert_eq!(edited[0].stamp, next_stamp);
            assert_eq!(edited[0].value, Value::Bool(false));

            previous.settings.clear();
            let bootstrap = reconcile_roaming_settings(
                BTreeMap::from([("tilo.appearance.mode".into(), Value::Bool(true))]),
                Some(&previous),
                &applied_remote,
                &next_stamp,
            );
            assert!(bootstrap.is_empty());
        }

        clipboard_policy_transition_clears_imports_and_never_reauthors_them();
        snapshot_stamping_updates_every_authored_record();
        remotely_applied_setting_is_not_reauthored_but_a_user_edit_is();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bluetooth_handoffs_are_bound_typed_and_roll_back_exactly() {
        fn bluetooth_handoff_phases_bind_peer_direction_and_accessory() {
            let target = DeviceId::new("target").unwrap();
            let other = DeviceId::new("other").unwrap();
            let transaction = BluetoothHandoffTransaction {
                peer: target.clone(),
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Headphones".into(),
                outbound: true,
                phase: BluetoothHandoffPhase::AwaitingReady,
                authorization: None,
                started: Instant::now(),
            };
            assert!(transaction.matches(
                &target,
                "aa:bb:cc:dd:ee:ff",
                true,
                BluetoothHandoffPhase::AwaitingReady
            ));
            assert!(!transaction.matches(
                &other,
                "AA:BB:CC:DD:EE:FF",
                true,
                BluetoothHandoffPhase::AwaitingReady
            ));
            assert!(!transaction.matches(
                &target,
                "AA:BB:CC:DD:EE:00",
                true,
                BluetoothHandoffPhase::AwaitingReady
            ));
            assert!(!transaction.matches(
                &target,
                "AA:BB:CC:DD:EE:FF",
                false,
                BluetoothHandoffPhase::AwaitingReady
            ));
            assert!(!transaction.matches(
                &target,
                "AA:BB:CC:DD:EE:FF",
                true,
                BluetoothHandoffPhase::AwaitingFinish
            ));

            let authorized = BluetoothHandoffTransaction {
                outbound: false,
                phase: BluetoothHandoffPhase::PairingReady,
                authorization: Some([7; 16]),
                ..transaction
            };
            assert!(authorized.accepts_pairing_authorization(
                &target,
                "aa:bb:cc:dd:ee:ff",
                [7; 16]
            ));
            assert!(!authorized.accepts_pairing_authorization(
                &target,
                "AA:BB:CC:DD:EE:FF",
                [8; 16]
            ));
            assert!(!authorized.accepts_pairing_authorization(
                &other,
                "AA:BB:CC:DD:EE:FF",
                [7; 16]
            ));
        }

        fn bluetooth_handoff_outcomes_are_typed_and_bounded() {
            let peer = DeviceId::new("target").unwrap();
            let failures = [
                BluetoothHandoffFailure::InspectTarget,
                BluetoothHandoffFailure::TargetUnavailable,
                BluetoothHandoffFailure::DisconnectSource,
                BluetoothHandoffFailure::PairTarget,
                BluetoothHandoffFailure::ConnectTarget,
                BluetoothHandoffFailure::Send,
                BluetoothHandoffFailure::TimedOut,
                BluetoothHandoffFailure::Rollback,
            ];
            let mut state = BluetoothHandoffState::new();
            for index in 0..(MAX_BLUETOOTH_HANDOFF_HISTORY + failures.len()) {
                let request_id = [u8::try_from(index + 1).unwrap(); 16];
                let transaction = BluetoothHandoffTransaction {
                    peer: peer.clone(),
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    name: format!("Headphones {index}"),
                    outbound: true,
                    phase: BluetoothHandoffPhase::AwaitingFinish,
                    authorization: None,
                    started: Instant::now(),
                };
                state.record_outcome(
                    request_id,
                    transaction,
                    BluetoothHandoffPhaseView::Failed(failures[index % failures.len()]),
                );
            }
            assert_eq!(state.outcomes.len(), MAX_BLUETOOTH_HANDOFF_HISTORY);
            assert!(state.outcomes.values().all(|outcome| matches!(
                outcome.phase,
                BluetoothHandoffPhaseView::Failed(
                    BluetoothHandoffFailure::InspectTarget
                        | BluetoothHandoffFailure::TargetUnavailable
                        | BluetoothHandoffFailure::DisconnectSource
                        | BluetoothHandoffFailure::PairTarget
                        | BluetoothHandoffFailure::ConnectTarget
                        | BluetoothHandoffFailure::Send
                        | BluetoothHandoffFailure::TimedOut
                        | BluetoothHandoffFailure::Rollback
                )
            )));
        }

        fn bluetooth_policy_teardown_plans_source_and_target_rollback() {
            let peer = DeviceId::new("target").unwrap();
            let transaction = |outbound, phase| BluetoothHandoffTransaction {
                peer: peer.clone(),
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Headphones".into(),
                outbound,
                phase,
                authorization: None,
                started: Instant::now(),
            };
            let mut state = BluetoothHandoffState::new();
            state.transactions.insert(
                [1; 16],
                transaction(true, BluetoothHandoffPhase::AwaitingFinish),
            );
            state.transactions.insert(
                [2; 16],
                transaction(false, BluetoothHandoffPhase::AwaitingAcceptance),
            );

            let rollback = finish_bluetooth_transactions(
                &mut state,
                &[[1; 16], [2; 16]],
                BluetoothHandoffPhaseView::Cancelled,
            );
            assert!(state.transactions.is_empty());
            assert_eq!(state.outcomes.len(), 2);
            assert_eq!(rollback[0].2, BluetoothRollback::ReconnectSource);
            assert_eq!(rollback[1].2, BluetoothRollback::DisconnectTarget);
        }

        fn bluetooth_cancel_race_rolls_back_stale_worker_success() {
            assert_eq!(
                stale_worker_rollback(&BluetoothWorkerOutcome::SourceDisconnected(Ok(()))),
                BluetoothRollback::ReconnectSource
            );
            assert_eq!(
                stale_worker_rollback(&BluetoothWorkerOutcome::TargetPaired(Ok(()))),
                BluetoothRollback::DisconnectTarget
            );
            assert_eq!(
                stale_worker_rollback(&BluetoothWorkerOutcome::TargetConnected(Ok(()))),
                BluetoothRollback::DisconnectTarget
            );
            assert_eq!(
                stale_worker_rollback(&BluetoothWorkerOutcome::TargetConnected(Err(
                    "failed".into()
                ))),
                BluetoothRollback::None
            );
        }

        fn bluetooth_peer_loss_is_bound_to_the_exact_authenticated_peer() {
            let target = DeviceId::new("target").unwrap();
            let other = DeviceId::new("other").unwrap();
            let transaction = BluetoothHandoffTransaction {
                peer: target.clone(),
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Headphones".into(),
                outbound: true,
                phase: BluetoothHandoffPhase::AwaitingFinish,
                authorization: None,
                started: Instant::now(),
            };
            assert!(!bluetooth_peer_online(
                &transaction,
                &BTreeSet::from([other])
            ));
            assert!(bluetooth_peer_online(
                &transaction,
                &BTreeSet::from([target])
            ));
        }

        bluetooth_handoff_phases_bind_peer_direction_and_accessory();
        bluetooth_handoff_outcomes_are_typed_and_bounded();
        bluetooth_policy_teardown_plans_source_and_target_rollback();
        bluetooth_cancel_race_rolls_back_stale_worker_success();
        bluetooth_peer_loss_is_bound_to_the_exact_authenticated_peer();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn roaming_provenance_reports_winners_without_secrets() {
        fn roaming_projection_includes_personal_preferences_and_excludes_hardware_state() {
            let document = serde_json::json!({
                "locale": {"currency": "EUR", "time_format": "H24"},
                "appearance": {
                    "mode": "light",
                    "studio": {"id": "aurora"},
                    "wallpaper_path": "/home/person/private.png"
                },
                "input": {
                    "keyboard_layouts": ["us", "de"],
                    "tablets": {"056a:0374:Wacom Intuos S Pen": {"output": "DP-2"}}
                },
                "audio": {"mono_output": true, "output_device": "USB DAC"},
                "display": {
                    "night_light_temperature": 3700,
                    "scale": 1.5,
                    "auto_brightness": true
                },
                "shortcuts": {"bindings": {"search": {"key": "k"}}},
                "quick_settings": {"controls": ["wifi", "bluetooth"]},
                "text_assistance": {"auto_correct": false},
                "power": {"auto_suspend": false},
                "agent_providers": {"api_key": "must-not-roam"}
            });
            let projected = roaming_setting_values_from(document.as_object().unwrap());

            for key in [
                "tilo.locale.currency",
                "tilo.locale.time_format",
                "tilo.appearance.mode",
                "tilo.appearance.studio",
                "tilo.input.keyboard_layouts",
                "tilo.audio.mono_output",
                "tilo.display.night_light_temperature",
                "tilo.shortcuts.bindings",
                "tilo.quick_settings.controls",
                "tilo.text_assistance.auto_correct",
            ] {
                assert!(projected.contains_key(key), "missing roaming key {key}");
            }
            for key in [
                "tilo.appearance.wallpaper_path",
                "tilo.input.tablets",
                "tilo.audio.output_device",
                "tilo.display.scale",
                "tilo.display.auto_brightness",
                "tilo.power.auto_suspend",
                "tilo.agent_providers.api_key",
            ] {
                assert!(!projected.contains_key(key), "leaked local key {key}");
            }
        }

        fn provenance_reports_the_deterministic_winner_and_only_divergent_origins() {
            let desktop = DeviceId::new("desktop").unwrap();
            let laptop = DeviceId::new("laptop").unwrap();
            let tablet = DeviceId::new("tablet").unwrap();
            let mut fabric = DeviceFabric::default();
            fabric
                .upsert(provenance_snapshot(
                    desktop,
                    "Desktop",
                    4,
                    Some(serde_json::json!("Light")),
                ))
                .unwrap();
            fabric
                .upsert(provenance_snapshot(
                    laptop,
                    "Laptop",
                    5,
                    Some(serde_json::json!("Dark")),
                ))
                .unwrap();
            fabric
                .upsert(provenance_snapshot(
                    tablet,
                    "Tablet",
                    3,
                    Some(serde_json::json!("Dark")),
                ))
                .unwrap();

            let projection = roaming_provenance(&fabric, roaming_test_policy(true, false));
            assert_eq!(projection.settings.len(), 1);
            assert_eq!(projection.settings[0].winner.device_label, "Laptop");
            assert_eq!(projection.settings[0].winner.counter, 5);
            assert_eq!(projection.settings[0].alternatives.len(), 1);
            assert_eq!(
                projection.settings[0].alternatives[0].device_label,
                "Desktop"
            );
            assert!(
                roaming_provenance(&fabric, ContinuityPolicySnapshot::default())
                    .settings
                    .is_empty()
            );
        }

        fn unchanged_provenance_is_not_rewritten_and_settings_can_reload_it() {
            let mut fabric = DeviceFabric::default();
            fabric
                .upsert(provenance_snapshot(
                    DeviceId::new("laptop").unwrap(),
                    "Laptop",
                    2,
                    Some(serde_json::json!("Dark")),
                ))
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir
                .path()
                .join(settings_store::roaming_provenance::FILE_NAME);
            let mut fingerprint = None;
            assert!(
                publish_roaming_provenance(
                    &fabric,
                    roaming_test_policy(true, false),
                    &mut fingerprint,
                    Some(&path)
                )
                .unwrap()
            );
            let first =
                settings_store::roaming_provenance::RoamingProvenance::load_at(&path).unwrap();
            assert_eq!(first.settings[0].winner.device_label, "Laptop");
            assert!(
                !publish_roaming_provenance(
                    &fabric,
                    roaming_test_policy(true, false),
                    &mut fingerprint,
                    Some(&path)
                )
                .unwrap()
            );
            let second =
                settings_store::roaming_provenance::RoamingProvenance::load_at(&path).unwrap();
            assert_eq!(second, first);
        }

        fn wifi_provenance_exposes_origins_but_neither_envelopes_nor_passwords() {
            use ed25519_dalek::SigningKey;

            let first_key = SigningKey::from_bytes(&[1; 32]);
            let second_key = SigningKey::from_bytes(&[2; 32]);
            let first = DeviceId::new(hex::encode(first_key.verifying_key().to_bytes())).unwrap();
            let second = DeviceId::new(hex::encode(second_key.verifying_key().to_bytes())).unwrap();
            let recipients = [first.clone(), second.clone()];
            let first_stamp = FabricStamp {
                counter: 3,
                device: first.clone(),
            };
            let second_stamp = FabricStamp {
                counter: 4,
                device: second.clone(),
            };
            let first_credential = crate::shell::device_fabric_secrets::seal_wifi_credential(
                "Home",
                os_actions::WifiSecurity::Wpa2,
                "first-secret",
                &first,
                recipients.clone(),
                first_stamp,
            )
            .unwrap();
            let second_credential = crate::shell::device_fabric_secrets::seal_wifi_credential(
                "Home",
                os_actions::WifiSecurity::Wpa2,
                "second-secret",
                &second,
                recipients,
                second_stamp,
            )
            .unwrap();
            let mut first_snapshot = provenance_snapshot(first, "Desktop", 3, None);
            first_snapshot.wifi.push(first_credential);
            let mut second_snapshot = provenance_snapshot(second, "Laptop", 4, None);
            second_snapshot.wifi.push(second_credential);
            let mut fabric = DeviceFabric::default();
            fabric.upsert(first_snapshot).unwrap();
            fabric.upsert(second_snapshot).unwrap();

            let projection = roaming_provenance(&fabric, roaming_test_policy(true, false));
            assert_eq!(projection.wifi.len(), 1);
            assert_eq!(projection.wifi[0].ssid, "Home");
            assert_eq!(projection.wifi[0].winner.device_label, "Laptop");
            assert_eq!(projection.wifi[0].alternatives[0].device_label, "Desktop");
            let encoded = serde_json::to_string(&projection).unwrap();
            for secret in [
                "first-secret",
                "second-secret",
                "sealed_secret",
                "ciphertext",
            ] {
                assert!(!encoded.contains(secret));
            }
        }

        roaming_projection_includes_personal_preferences_and_excludes_hardware_state();
        provenance_reports_the_deterministic_winner_and_only_divergent_origins();
        unchanged_provenance_is_not_rewritten_and_settings_can_reload_it();
        wifi_provenance_exposes_origins_but_neither_envelopes_nor_passwords();
    }

    #[test]
    fn spatial_input_requires_the_grant_route_epoch_and_live_nonce() {
        use crate::shell::network::AuthenticatedPersonalRoute;
        let controller = DeviceId::new("controller").unwrap();
        let target = DeviceId::new("target").unwrap();
        let route = AuthenticatedPersonalRoute::for_test_epoch(controller.clone(), 7);
        let rejoined = AuthenticatedPersonalRoute::for_test_epoch(controller.clone(), 8);
        let foreign = AuthenticatedPersonalRoute::for_test(DeviceId::new("foreign").unwrap());
        let lease = InputLease {
            controller: controller.clone(),
            target: target.clone(),
            issued_unix_ms: 100,
            expires_unix_ms: 200,
            nonce: [7; 16],
            allow_pointer: true,
            allow_keyboard: false,
        };
        let input = (
            lease.nonce,
            FabricInputEvent::PointerButton {
                button: 0x110,
                pressed: true,
            },
        );
        let mut spatial = SpatialInputState::default();
        spatial.issued.insert(lease.nonce, lease.clone());
        spatial.issued_routes.insert(lease.nonce, route.clone());
        spatial.accept_lease_bound_input(&route, &target, input, 150);
        assert_eq!(
            spatial.inbound.pop_front(),
            Some((controller.clone(), input.1))
        );
        for (candidate, destination, event, now) in [
            (&rejoined, &target, input, 150),
            (&foreign, &target, input, 150),
            (&route, &controller, input, 150),
            (&route, &target, ([8; 16], input.1), 150),
            (&route, &target, input, 201),
            (
                &route,
                &target,
                (
                    lease.nonce,
                    FabricInputEvent::Key {
                        keycode: 30,
                        pressed: true,
                        modifiers: 0,
                    },
                ),
                150,
            ),
        ] {
            spatial.accept_lease_bound_input(candidate, destination, event, now);
            assert!(spatial.inbound.is_empty());
        }
        // Withdrawing the exact route releases the seat even if the device
        // still appears online and the wall-clock lease has not expired.
        spatial.accept_lease_bound_input(&route, &target, input, 150);
        assert_eq!(spatial.inbound.len(), 1);
        prune_spatial_state(&mut spatial, 150, &[], |_| false);
        assert!(
            spatial.inbound.is_empty(),
            "revocation must discard a queued down edge"
        );
        assert_eq!(spatial.release_controllers.pop_front(), Some(controller));
        assert!(spatial.issued_routes.is_empty());
        spatial.accept_lease_bound_input(&route, &target, input, 150);
        assert!(spatial.inbound.is_empty());
    }

    #[test]
    fn spatial_gestures_cannot_adopt_replacement_or_foreign_leases() {
        let controller = DeviceId::new("controller").unwrap();
        let target = DeviceId::new("target").unwrap();
        let foreign = DeviceId::new("foreign").unwrap();
        let mut spatial = SpatialInputState::default();
        let lease = InputLease {
            controller: controller.clone(),
            target: target.clone(),
            issued_unix_ms: 100,
            expires_unix_ms: 200,
            nonce: [7; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        spatial.received.insert(target.clone(), lease.clone());
        let event = |nonce, event| OutboundSpatialInput {
            sequence: None,
            target: target.clone(),
            lease_nonce: nonce,
            event,
        };
        let press = FabricInputEvent::Key {
            keycode: 30,
            pressed: true,
            modifiers: 0,
        };
        let release = FabricInputEvent::Key {
            keycode: 30,
            pressed: false,
            modifiers: 0,
        };
        spatial
            .queue_lease_bound_input(&controller, event(lease.nonce, press), 150)
            .unwrap();
        assert!(
            spatial
                .queue_lease_bound_input(&foreign, event(lease.nonce, press), 150)
                .is_err()
        );
        assert!(
            spatial
                .queue_lease_bound_input(&controller, event(lease.nonce, release), 201)
                .is_err()
        );
        // A new grant may accept a new gesture, never the previous gesture's
        // down/up edges. Pruning removes the old queue under the same contract.
        let mut replacement = lease.clone();
        replacement.nonce = [8; 16];
        spatial.received.insert(target.clone(), replacement.clone());
        assert!(
            spatial
                .queue_lease_bound_input(&controller, event(lease.nonce, release), 150)
                .is_err()
        );
        prune_spatial_state(&mut spatial, 150, &[], |_| true);
        assert!(spatial.outbound.is_empty());
        spatial
            .queue_lease_bound_input(&controller, event(replacement.nonce, press), 150)
            .unwrap();
        spatial
            .queue_lease_bound_input(&controller, event(replacement.nonce, release), 150)
            .unwrap();
        assert_eq!(spatial.outbound.len(), 2);
        assert!(
            spatial
                .outbound
                .iter()
                .all(|input| input.lease_nonce == replacement.nonce)
        );
        spatial.received.remove(&target);
        assert!(
            spatial
                .queue_lease_bound_input(&controller, event(replacement.nonce, press), 150)
                .is_err()
        );
        prune_spatial_state(&mut spatial, 150, &[], |_| true);
        assert!(spatial.outbound.is_empty());
        // A keyboard-only lease stays usable by the existing semantic sender.
        replacement.allow_pointer = false;
        spatial.received.insert(target.clone(), replacement.clone());
        spatial
            .queue_lease_bound_input(&controller, event(replacement.nonce, press), 150)
            .unwrap();
        assert!(
            spatial
                .queue_lease_bound_input(
                    &controller,
                    event(
                        replacement.nonce,
                        FabricInputEvent::PointerMotion { x: 1, y: 2 }
                    ),
                    150
                )
                .is_err()
        );
    }

    #[test]
    fn remote_input_coalesces_and_departure_revokes_the_exact_controller() {
        fn pointer_bursts_coalesce_without_crossing_discrete_input() {
            let target = DeviceId::new("tablet").unwrap();
            let motion = |x| OutboundSpatialInput {
                sequence: None,
                target: target.clone(),
                lease_nonce: [7; 16],
                event: FabricInputEvent::PointerMotion { x, y: 20 },
            };
            let mut state = SpatialInputState::default();
            state.queue_outbound(motion(10)).unwrap();
            state.queue_outbound(motion(11)).unwrap();
            assert_eq!(state.outbound.len(), 1);
            assert_eq!(state.outbound.front(), Some(&motion(11)));

            state
                .queue_outbound(OutboundSpatialInput {
                    sequence: None,
                    target: target.clone(),
                    lease_nonce: [7; 16],
                    event: FabricInputEvent::PointerButton {
                        button: 0x110,
                        pressed: true,
                    },
                })
                .unwrap();
            state.queue_outbound(motion(12)).unwrap();
            state.queue_outbound(motion(13)).unwrap();
            assert_eq!(state.outbound.len(), 3);
            assert_eq!(state.outbound.back(), Some(&motion(13)));
            assert!(matches!(
                state.outbound.get(1).map(|input| input.event),
                Some(FabricInputEvent::PointerButton { pressed: true, .. })
            ));
        }

        fn authenticated_departure_revokes_only_exact_controller_and_is_not_replayed() {
            let departed_endpoint = iroh::SecretKey::from_bytes(&[41; 32]).public();
            let sibling_endpoint = iroh::SecretKey::from_bytes(&[42; 32]).public();
            let departed = DeviceId::new(hex::encode(departed_endpoint.as_bytes())).unwrap();
            let sibling = DeviceId::new(hex::encode(sibling_endpoint.as_bytes())).unwrap();
            let target = DeviceId::new("target").unwrap();
            let now = 10_000_u64;
            let lease = |controller: DeviceId, nonce: [u8; 16]| InputLease {
                controller,
                target: target.clone(),
                issued_unix_ms: now,
                expires_unix_ms: now.saturating_add(1_000),
                nonce,
                allow_keyboard: true,
                allow_pointer: true,
            };
            let mut spatial = SpatialInputState {
                last_peer_departure_generation: 7,
                ..SpatialInputState::default()
            };
            spatial
                .issued
                .insert([1; 16], lease(departed.clone(), [1; 16]));
            spatial.issued.insert([2; 16], lease(sibling, [2; 16]));
            let departures = vec![crate::shell::network::PeerDeparture {
                generation: 8,
                endpoint_id: departed_endpoint,
                membership_epoch: Some(crate::network::actor::MembershipEpoch::for_test(1)),
                session_handle: 1,
                route: crate::shell::collab_broker::CrdtRoute::cluster("personal"),
                cause: crate::shell::network::PeerDepartureCause::LivenessTimedOut,
                user_public_key: Some([3; 32]),
                participant_name: None,
                observed_at: std::time::SystemTime::now(),
            }];

            prune_spatial_state(&mut spatial, now, &departures, |_| true);
            assert!(!spatial.issued.contains_key(&[1; 16]));
            assert!(spatial.issued.contains_key(&[2; 16]));
            assert_eq!(
                spatial.release_controllers.iter().collect::<Vec<_>>(),
                vec![&departed]
            );

            spatial.release_controllers.clear();
            spatial.issued.insert([3; 16], lease(departed, [3; 16]));
            prune_spatial_state(&mut spatial, now, &departures, |_| true);
            assert!(spatial.issued.contains_key(&[3; 16]));
            assert!(spatial.issued.contains_key(&[2; 16]));
            assert!(spatial.release_controllers.is_empty());
        }

        pointer_bursts_coalesce_without_crossing_discrete_input();
        authenticated_departure_revokes_only_exact_controller_and_is_not_replayed();
    }

    fn pending_gamepad(source: &str) -> PendingGamepadAuthorization {
        PendingGamepadAuthorization {
            source: DeviceId::new(source).unwrap(),
            target: DeviceId::new("target").unwrap(),
            app_id: "so.tilo.game".into(),
            workspace_id: "workspace".into(),
            session_id: "session".into(),
            label: "Controller".into(),
            received: Instant::now(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn gamepad_forwarding_requires_one_local_focused_decision_and_tears_down_cleanly() {
        fn gamepad_request_requires_one_local_decision_and_rejects_duplicate_id() {
            let mut state = GamepadAuthorizationState::default();
            assert!(state.queue([1; 16], pending_gamepad("source"), false, 0));
            assert!(!state.queue([1; 16], pending_gamepad("attacker"), false, 0));
            assert!(state.decide([1; 16], true));
            assert!(!state.decide([1; 16], false));
            assert_eq!(state.decisions, VecDeque::from([([1; 16], true)]));
        }

        fn gamepad_queue_rejects_when_forwardings_are_full_even_at_usize_max() {
            let mut state = GamepadAuthorizationState::default();
            assert!(!state.queue(
                [3; 16],
                pending_gamepad("source"),
                false,
                MAX_GAMEPAD_FORWARDINGS
            ));
            assert!(!state.queue([3; 16], pending_gamepad("source"), false, usize::MAX));
            assert!(state.pending.is_empty());
            assert!(state.queue([3; 16], pending_gamepad("source"), false, 0));
            assert!(!state.queue([4; 16], pending_gamepad("source"), false, usize::MAX));
        }

        fn active_gamepad_lease_blocks_same_forwarding_id_before_consent() {
            let mut state = GamepadAuthorizationState::default();
            assert!(!state.queue([2; 16], pending_gamepad("source"), true, 1));
            assert!(state.pending.is_empty());
        }

        fn gamepad_approval_requires_exact_compositor_local_focus() {
            let pending = pending_gamepad("source");
            let exact = crate::shell::physical_gamepad::PhysicalGamepadBinding {
                app_id: "so.tilo.game".into(),
                workspace_id: "workspace".into(),
                session_id: "session".into(),
            };
            assert!(pending.matches_local_focus(Some(&exact)));
            assert!(!pending.matches_local_focus(None));
            for mismatch in [
                crate::shell::physical_gamepad::PhysicalGamepadBinding {
                    app_id: "so.tilo.other".into(),
                    ..exact.clone()
                },
                crate::shell::physical_gamepad::PhysicalGamepadBinding {
                    workspace_id: "other-workspace".into(),
                    ..exact.clone()
                },
                crate::shell::physical_gamepad::PhysicalGamepadBinding {
                    session_id: "other-session".into(),
                    ..exact
                },
            ] {
                assert!(!pending.matches_local_focus(Some(&mismatch)));
            }
        }

        fn active_gamepad_identity_becomes_invalid_on_local_focus_loss() {
            let identity = os_actions::ForwardedPeripheralIdentity::new_after_verification(
                "source",
                "target",
                "so.tilo.game",
                "workspace",
                "session",
            )
            .unwrap();
            let exact = crate::shell::physical_gamepad::PhysicalGamepadBinding {
                app_id: "so.tilo.game".into(),
                workspace_id: "workspace".into(),
                session_id: "session".into(),
            };
            assert!(identity_matches_local_focus(&identity, Some(&exact)));
            assert!(!identity_matches_local_focus(&identity, None));
            assert!(!identity_matches_local_focus(
                &identity,
                Some(&crate::shell::physical_gamepad::PhysicalGamepadBinding {
                    session_id: "new-tab".into(),
                    ..exact
                })
            ));
        }

        fn failed_gamepad_grant_send_tears_down_inserted_session() {
            let mut sessions = BTreeMap::from([([9; 16], "virtual-gamepad")]);
            let mut torn_down = Vec::new();
            finish_gamepad_grant_delivery(
                &mut sessions,
                [9; 16],
                &Err::<(), _>("route vanished"),
                |session| torn_down.push(session),
            );
            assert!(sessions.is_empty());
            assert_eq!(torn_down, ["virtual-gamepad"]);

            sessions.insert([8; 16], "still-live");
            finish_gamepad_grant_delivery(&mut sessions, [8; 16], &Ok::<_, &str>(()), |session| {
                torn_down.push(session);
            });
            assert_eq!(sessions.get(&[8; 16]), Some(&"still-live"));
            assert_eq!(torn_down, ["virtual-gamepad"]);
        }

        gamepad_request_requires_one_local_decision_and_rejects_duplicate_id();
        gamepad_queue_rejects_when_forwardings_are_full_even_at_usize_max();
        active_gamepad_lease_blocks_same_forwarding_id_before_consent();
        gamepad_approval_requires_exact_compositor_local_focus();
        active_gamepad_identity_becomes_invalid_on_local_focus_loss();
        failed_gamepad_grant_send_tears_down_inserted_session();
    }
}
