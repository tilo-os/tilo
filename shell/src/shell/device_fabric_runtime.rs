//! Native-session bridge between compositor state and the personal fabric.
//!
//! The bridge deliberately samples stable state rather than putting network or
//! hardware discovery on the render path. Remote clipboard entries become
//! history, not the live Wayland selection, until explicitly activated.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime};

use rand_core::{OsRng, RngCore};
use serde_json::{Map, Value};

use crate::network::actor::{PersonalMediaTransferStatus, PersonalNearbyTransferFrame};
use crate::notifications::{
    ContinuityNotificationCommand, ContinuityOutboundEvent, NotificationBridge,
};

use crate::compositor::selection::{
    SharedSelection, clear_shared_clipboard_history, merge_shared_clipboard_history,
    shareable_clipboard_history,
};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::device_fabric::{
    ClipboardEntry, ClipboardPayload, DISPLAY_STREAM_PROTOCOL_VERSION, DeviceFabricSnapshot,
    DeviceKind, DevicePresence, DisplayEdge, DisplayPlacement, DisplayRect, DisplayStreamCodec,
    DisplayStreamProfile, FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricInputEvent, FabricStamp,
    InputLease, MAX_DISPLAY_SESSIONS, MAX_INPUT_LEASE_MS, MediaSession, MediaSourceLocator,
    PersonalDisplayMessage, PersonalFabricAction, RoamingSetting,
};
use crate::shell::network::ShellNetwork;

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
const BLUETOOTH_HANDOFF_WAIT: Duration = Duration::from_secs(75);
const BLUETOOTH_HANDOFF_DISCOVERY: Duration = Duration::from_secs(6);
const MAX_BLUETOOTH_HANDOFFS: usize = 32;
const DISPLAY_OFFER_RETRY: Duration = Duration::from_secs(2);
const DISPLAY_KEEPALIVE: Duration = Duration::from_secs(5);
const DISPLAY_SESSION_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_GAMEPAD_FORWARDINGS: usize = 8;
const MAX_PENDING_NEARBY_CONSENTS: usize = 64;
const NEARBY_CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

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
            || inbound_count + self.pending.len() >= MAX_GAMEPAD_FORWARDINGS
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

#[derive(Clone, Debug)]
struct OutboundDisplaySession {
    session_id: [u8; 16],
    profile: DisplayStreamProfile,
    accepted: bool,
    last_offer: Option<Instant>,
    sequence: u64,
    request_keyframe: bool,
}

#[derive(Clone, Debug)]
struct InboundDisplaySession {
    session_id: [u8; 16],
    source: crate::shell::personal_continuity::DeviceId,
    profile: DisplayStreamProfile,
    last_activity: Instant,
    last_sequence: u64,
}

#[derive(Default)]
struct PersonalDisplayState {
    outbound: BTreeMap<crate::shell::personal_continuity::DeviceId, OutboundDisplaySession>,
    inbound: Option<InboundDisplaySession>,
    events: VecDeque<PersonalDisplayEvent>,
}

impl PersonalDisplayState {
    fn queue_event(&mut self, event: PersonalDisplayEvent) {
        const CAPACITY: usize = MAX_DISPLAY_SESSIONS * 4;
        if self.events.len() >= CAPACITY {
            if let Some(frame) = self
                .events
                .iter()
                .position(|queued| matches!(queued, PersonalDisplayEvent::Frame { .. }))
            {
                self.events.remove(frame);
            } else {
                self.events.pop_front();
            }
        }
        self.events.push_back(event);
    }

    fn install_inbound(&mut self, session: InboundDisplaySession) {
        if let Some(current) = self.inbound.as_mut()
            && current.session_id == session.session_id
            && current.source == session.source
        {
            current.profile = session.profile;
            current.last_activity = session.last_activity;
            return;
        }
        if let Some(previous) = self.inbound.replace(session.clone())
            && previous.session_id != session.session_id
        {
            self.queue_event(PersonalDisplayEvent::Ended {
                session_id: previous.session_id,
            });
        }
        self.queue_event(PersonalDisplayEvent::Started {
            session_id: session.session_id,
            source: session.source,
            profile: session.profile,
        });
    }

    fn accept_frame(
        &mut self,
        session_id: [u8; 16],
        source: &crate::shell::personal_continuity::DeviceId,
        sequence: u64,
        width: u32,
        height: u32,
        now: Instant,
    ) -> bool {
        let Some(session) = self.inbound.as_mut().filter(|session| {
            session.session_id == session_id
                && &session.source == source
                && sequence > session.last_sequence
                && width <= session.profile.max_width
                && height <= session.profile.max_height
        }) else {
            return false;
        };
        session.last_sequence = sequence;
        session.last_activity = now;
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalDisplayEvent {
    Started {
        session_id: [u8; 16],
        source: crate::shell::personal_continuity::DeviceId,
        profile: DisplayStreamProfile,
    },
    Frame {
        session_id: [u8; 16],
        sequence: u64,
        presentation_time_us: u64,
        keyframe: bool,
        width: u32,
        height: u32,
        annex_b: Vec<u8>,
    },
    Ended {
        session_id: [u8; 16],
    },
}

fn personal_display_state() -> &'static Mutex<PersonalDisplayState> {
    static STATE: OnceLock<Mutex<PersonalDisplayState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(PersonalDisplayState::default()))
}

fn default_display_profile() -> DisplayStreamProfile {
    DisplayStreamProfile {
        codec: DisplayStreamCodec::H264AnnexB,
        max_width: 1920,
        max_height: 1080,
        max_fps: 60,
        max_bitrate_kbps: 12_000,
    }
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
            PersonalMediaTransferStatus,
        ),
    >,
    last_offer: BTreeMap<[u8; 16], Instant>,
    incoming: BTreeMap<[u8; 16], PendingNearbyConsent>,
    destinations: BTreeMap<[u8; 16], crate::shell::nearby_transfer::SecureNearbyDestination>,
    history: VecDeque<NearbyTransferHistory>,
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
}

fn nearby_drop_state() -> &'static Mutex<NearbyDropState> {
    static STATE: OnceLock<Mutex<NearbyDropState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(NearbyDropState::default()))
}

fn nearby_transfer_root() -> Option<PathBuf> {
    dirs::state_dir().map(|root| root.join("tilo").join("nearby-transfers"))
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
            unix_millis(),
        )?);
    }
    operation(
        guard
            .as_mut()
            .ok_or_else(|| "nearby transfer store is unavailable".to_string())?,
    )
}

fn try_outbox_entries() -> Vec<crate::shell::nearby_transfer::OutboxEntry> {
    let Ok(mut guard) = nearby_store_cell().try_lock() else {
        return Vec::new();
    };
    if guard.is_none() {
        let Some(root) = nearby_transfer_root() else {
            return Vec::new();
        };
        let Ok(store) =
            crate::shell::nearby_transfer::NearbyTransferStore::open(root, unix_millis())
        else {
            return Vec::new();
        };
        *guard = Some(store);
    }
    guard
        .as_ref()
        .map_or_else(Vec::new, |store| store.outbox().to_vec())
}

fn try_inbox_entries() -> Vec<crate::shell::nearby_transfer::InboxEntry> {
    let Ok(mut guard) = nearby_store_cell().try_lock() else {
        return Vec::new();
    };
    if guard.is_none() {
        let Some(root) = nearby_transfer_root() else {
            return Vec::new();
        };
        let Ok(store) =
            crate::shell::nearby_transfer::NearbyTransferStore::open(root, unix_millis())
        else {
            return Vec::new();
        };
        *guard = Some(store);
    }
    guard
        .as_ref()
        .map_or_else(Vec::new, |store| store.inbox().to_vec())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

struct NearbyStagingResult {
    transfer_id: [u8; 16],
    target: crate::shell::personal_continuity::DeviceId,
    name: String,
    result: Result<(), String>,
}

#[derive(Default)]
struct NearbyStagingAdmission {
    owners: BTreeMap<String, usize>,
    generation: u64,
}

impl NearbyStagingAdmission {
    fn acquire(&mut self, owner: &str) -> bool {
        let total = self.owners.values().sum::<usize>();
        let owned = self.owners.get(owner).copied().unwrap_or_default();
        if total >= MAX_NEARBY_STAGING_JOBS || owned >= MAX_NEARBY_STAGING_JOBS_PER_OWNER {
            return false;
        }
        self.owners.insert(owner.to_string(), owned + 1);
        true
    }

    fn release(&mut self, owner: &str) {
        let Some(owned) = self.owners.get_mut(owner) else {
            return;
        };
        *owned = owned.saturating_sub(1);
        if *owned == 0 {
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
        admitted
            .then(|| Self {
                owner: owner.to_string(),
                generation: admission.generation,
            })
            .ok_or_else(|| {
                "Nearby Send staging is busy; retry when another transfer finishes".into()
            })
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
    status: PersonalMediaTransferStatus,
) {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::FileTransfer) {
        return;
    }
    if let Ok(mut state) = nearby_drop_state().lock() {
        state
            .progress
            .insert(transfer_id, (target, sent_bytes, total_bytes, status));
        if status == PersonalMediaTransferStatus::Failed {
            state.large_offers.remove(&transfer_id);
        }
    }
}

pub(crate) fn handle_nearby_transfer_frame(
    network: &ShellNetwork,
    source: &crate::shell::personal_continuity::DeviceId,
    frame: PersonalNearbyTransferFrame,
) {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::FileTransfer) {
        return;
    }
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
    let authorized = with_nearby_transfer_store(|store| {
        Ok(store.inbox().iter().any(|entry| {
            entry.id == transfer_id
                && entry.source == source.as_str()
                && entry.request_id == request_id
        }))
    })
    .unwrap_or(false);
    if !authorized {
        return;
    }
    match frame {
        PersonalNearbyTransferFrame::Chunk { offset, bytes, .. } => {
            if bytes.len() <= crate::network::actor::PERSONAL_MEDIA_CHUNK_BYTES {
                let _ =
                    with_nearby_transfer_store(|store| store.append(transfer_id, offset, &bytes));
            }
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

#[derive(Clone)]
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OutboundSpatialInput {
    target: crate::shell::personal_continuity::DeviceId,
    lease_nonce: [u8; 16],
    event: FabricInputEvent,
}

#[derive(Default)]
struct SpatialInputState {
    /// Permissions this device issued and must enforce on inbound input.
    issued: BTreeMap<[u8; 16], InputLease>,
    /// Permissions other devices issued to this controller.
    received: BTreeMap<crate::shell::personal_continuity::DeviceId, InputLease>,
    inbound: VecDeque<(
        crate::shell::personal_continuity::DeviceId,
        FabricInputEvent,
    )>,
    release_controllers: VecDeque<crate::shell::personal_continuity::DeviceId>,
    outbound: VecDeque<OutboundSpatialInput>,
    placement_override: Option<DisplayRect>,
    placement_stream_source: Option<crate::shell::personal_continuity::DeviceId>,
}

impl SpatialInputState {
    fn queue_outbound(&mut self, input: OutboundSpatialInput) -> Result<(), String> {
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

fn spatial_input_state() -> &'static Mutex<SpatialInputState> {
    static STATE: OnceLock<Mutex<SpatialInputState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SpatialInputState::default()))
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
    result_tx: mpsc::Sender<BluetoothWorkerResult>,
    result_rx: mpsc::Receiver<BluetoothWorkerResult>,
}

impl BluetoothHandoffState {
    fn new() -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        Self {
            transactions: BTreeMap::new(),
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
    last_publish: Option<Instant>,
    local_fingerprint: Option<[u8; 32]>,
    merged_settings_fingerprint: Option<[u8; 32]>,
    applied_remote_settings: BTreeMap<String, Value>,
    settings_origin_initialized: bool,
    pending_incoming_media:
        BTreeMap<[u8; 16], (crate::shell::personal_continuity::DeviceId, String)>,
    waiting_incoming_media: BTreeMap<[u8; 16], PendingIncomingMedia>,
    notification_bridge: NotificationBridge,
    realtime_adapter: Option<crate::shell::realtime_session::SharedRealtimeFabricAdapter>,
    realtime_online_peers: BTreeSet<crate::shell::personal_continuity::DeviceId>,
    gamepad_broker: ProductionGamepadBroker,
    inbound_gamepads: BTreeMap<[u8; 16], InboundGamepadForwarding>,
    local_gamepad_focus: Option<crate::shell::physical_gamepad::PhysicalGamepadBinding>,
    policy: Option<ContinuityPolicySnapshot>,
}

impl DeviceFabricRuntime {
    pub fn new(network: &ShellNetwork, notification_bridge: NotificationBridge) -> Self {
        let counter = network.device_fabric().max_counter();
        Self {
            counter,
            last_sample: None,
            last_publish: None,
            local_fingerprint: None,
            merged_settings_fingerprint: None,
            applied_remote_settings: BTreeMap::new(),
            settings_origin_initialized: false,
            pending_incoming_media: BTreeMap::new(),
            waiting_incoming_media: BTreeMap::new(),
            notification_bridge,
            realtime_adapter: None,
            realtime_online_peers: BTreeSet::new(),
            gamepad_broker: os_actions::ForwardedPeripheralBroker::new(
                os_actions::LinuxVirtualGamepadFactory::new(
                    os_actions::UinputGamepadDeviceProvider,
                ),
            ),
            inbound_gamepads: BTreeMap::new(),
            local_gamepad_focus: None,
            policy: None,
        }
    }

    pub fn set_realtime_adapter(
        &mut self,
        adapter: crate::shell::realtime_session::SharedRealtimeFabricAdapter,
    ) {
        self.realtime_adapter = Some(adapter);
    }

    /// Supply the compositor's current locally verified app focus. Remote
    /// gamepad claims are never allowed to establish this authority.
    pub fn set_local_gamepad_focus(
        &mut self,
        focus: Option<crate::shell::physical_gamepad::PhysicalGamepadBinding>,
    ) {
        self.local_gamepad_focus = focus;
    }

    /// Sample local state and project authenticated remote state into the OS.
    pub fn pump(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        local: LocalFabricCapabilities,
    ) {
        let policy = ContinuityPolicySnapshot::load();
        self.enforce_policy(network, selection, policy);
        if policy.permits(ContinuityCapability::Media) {
            pump_bluetooth_handoffs(network);
        }
        self.maintain_gamepad_forwarding(network, policy);
        self.handle_fabric_actions(network, policy);
        crate::shell::remote_audio::pump(
            network,
            policy.permits(ContinuityCapability::Media),
            self.local_gamepad_focus.as_ref(),
        );
        self.pump_realtime_continuity(network, policy);
        self.pump_notification_continuity(network, policy);
        if policy.permits(ContinuityCapability::FileTransfer) {
            resume_nearby_transfers(network);
        } else {
            discard_nearby_staging_results();
        }
        pump_personal_displays(network, policy);
        if policy.permits(ContinuityCapability::Media) {
            self.retry_waiting_media(network);
        }
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
        crate::shell::mpris::refresh_handoff_reports();
        let Some((device, label)) = network.local_device_fabric_identity() else {
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
        if let Ok(spatial) = spatial_input_state().lock()
            && let Some(rect) = spatial.placement_override
        {
            local.display = Some(rect);
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
        let neutral_stamp = FabricStamp {
            counter: 0,
            device: device.clone(),
        };
        stamp_snapshot(&mut candidate, &neutral_stamp, 0);
        let fingerprint = postcard::to_allocvec(&candidate)
            .ok()
            .map(|bytes| *blake3::hash(&bytes).as_bytes());
        let heartbeat_due = self
            .last_publish
            .is_none_or(|published| now.duration_since(published) >= PRESENCE_HEARTBEAT);
        if fingerprint != self.local_fingerprint || heartbeat_due {
            self.counter = tentative_stamp.counter;
            if fingerprint == self.local_fingerprint {
                if let Some(mut heartbeat) = previous {
                    heartbeat.presence.stamp = tentative_stamp;
                    heartbeat.presence.last_seen_unix_ms = unix_ms();
                    candidate = heartbeat;
                }
            } else {
                candidate.presence.stamp = tentative_stamp;
                candidate.presence.last_seen_unix_ms = unix_ms();
            }
            if network.upsert_local_device_fabric(candidate).is_ok() {
                self.local_fingerprint = fingerprint;
                self.last_publish = Some(now);
            }
        }
        self.merge_remote_state(network, selection, &device, policy);
        if policy.permits(ContinuityCapability::WifiSettings)
            && let Err(error) = network.refresh_wifi_credential_recipients()
        {
            tracing::warn!(%error, "could not refresh per-device Wi-Fi credential envelopes");
        }
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
            tear_down_display_and_input(network);
        }
        if newly_disabled(ContinuityCapability::FileTransfer) {
            tear_down_nearby_transfers(network);
        }
        if newly_disabled(ContinuityCapability::Media) {
            self.pending_incoming_media.clear();
            self.waiting_incoming_media.clear();
            if let Ok(mut state) = bluetooth_handoff_state().lock() {
                state.transactions.clear();
                while state.result_rx.try_recv().is_ok() {}
            }
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
        if policy.permits(ContinuityCapability::Clipboard) {
            let history = fabric.merged_clipboard();
            let entries = history.entries().cloned().collect::<Vec<_>>();
            merge_shared_clipboard_history(selection, &entries);
        }

        let records = if policy.permits(ContinuityCapability::WifiSettings) {
            fabric.merged_setting_records()
        } else {
            BTreeMap::new()
        };
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

    fn handle_fabric_actions(&mut self, network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
        prune_spatial_leases(network);
        for envelope in network.take_personal_fabric_actions() {
            if !policy.permits_action(&envelope.action) {
                continue;
            }
            if crate::shell::remote_audio::handle_action(network, &envelope) {
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
                    };
                    if !self.try_launch_incoming_media(network, &request)
                        && matches!(request.locator, MediaSourceLocator::PersonalTransfer { .. })
                        && self.waiting_incoming_media.len() < MAX_PENDING_INCOMING_MEDIA
                    {
                        self.waiting_incoming_media
                            .insert(request.request_id, request);
                    }
                }
                PersonalFabricAction::MediaAccepted {
                    request_id,
                    session_id,
                } => {
                    let Some(report) =
                        media::handoff::accept_outbound_handoff(request_id, &session_id)
                    else {
                        continue;
                    };
                    if let Some(controller) = report.controller_bus_name {
                        let _ = crate::shell::mpris::pause_named(&controller);
                    }
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
                | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }) => {
                    handle_bluetooth_handoff_action(
                        network,
                        envelope.request_id,
                        &envelope.source,
                        action,
                    );
                }
                action => handle_spatial_action(envelope.source, &envelope.target, action),
            }
        }
    }

    fn handle_gamepad_action(
        &mut self,
        network: &ShellNetwork,
        envelope: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    ) -> bool {
        match &envelope.action {
            PersonalFabricAction::GamepadForwardRequest {
                forwarding_id,
                app_id,
                workspace_id,
                session_id,
                label,
            } => {
                self.queue_gamepad_request(
                    network,
                    &envelope.source,
                    &envelope.target,
                    *forwarding_id,
                    app_id,
                    workspace_id,
                    session_id,
                    label,
                );
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
                let local_focus = self.local_gamepad_focus.as_ref();
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

    #[allow(
        clippy::too_many_arguments,
        reason = "every authenticated wire binding stays explicit at the privileged broker boundary"
    )]
    fn queue_gamepad_request(
        &mut self,
        network: &ShellNetwork,
        source: &crate::shell::personal_continuity::DeviceId,
        target: &crate::shell::personal_continuity::DeviceId,
        forwarding_id: [u8; 16],
        app_id: &str,
        workspace_id: &str,
        session_id: &str,
        label: &str,
    ) {
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
            target: target.clone(),
            app_id: app_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            label: label.to_owned(),
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
            if !pending.matches_local_focus(self.local_gamepad_focus.as_ref()) {
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
            self.grant_gamepad_request(network, forwarding_id, pending);
        }
    }

    fn grant_gamepad_request(
        &mut self,
        network: &ShellNetwork,
        forwarding_id: [u8; 16],
        pending: PendingGamepadAuthorization,
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
        let online = network
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.presence.online)
            .map(|snapshot| snapshot.owner.clone())
            .collect::<BTreeSet<_>>();
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
                        self.local_gamepad_focus.as_ref(),
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
                unix_ms(),
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
                    .device_fabric()
                    .snapshots()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
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
        let online = network
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.presence.online)
            .map(|snapshot| snapshot.owner.clone())
            .collect::<BTreeSet<_>>();
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
                    unix_ms(),
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
                    unix_ms(),
                );
            }
            PersonalFabricAction::NotificationInvoke { id, action_key } => {
                let _ = self.notification_bridge.apply_continuity_command(
                    *id,
                    &ContinuityNotificationCommand::Invoke {
                        action_key: action_key.clone(),
                    },
                    unix_ms(),
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
            .device_fabric()
            .snapshots()
            .values()
            .filter(|snapshot| snapshot.owner != local && snapshot.presence.online)
            .map(|snapshot| snapshot.owner.clone())
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
    ) -> bool {
        let Some((local, _)) = network.local_device_fabric_identity() else {
            return false;
        };
        let Some(presence) = network.device_fabric().snapshots().get(&local).cloned() else {
            return false;
        };
        let capable = if request.has_video {
            presence.presence.capabilities.video_output
        } else {
            presence.presence.capabilities.audio_output
        };
        let Some(room) = crate::shell::state::RoomId::parse(&request.workspace_id) else {
            return true;
        };
        if !capable {
            return true;
        }
        let verified_path = match &request.locator {
            MediaSourceLocator::PersonalTransfer { transfer_id, .. } => {
                let Some(path) =
                    network.completed_personal_media_transfer(&request.source, *transfer_id)
                else {
                    return false;
                };
                Some(path)
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
        let launched = launch.is_ok();
        if launched {
            self.pending_incoming_media.insert(
                request.request_id,
                (request.source.clone(), request.session_id.clone()),
            );
            if let Some(source) = resume_source {
                let _ =
                    crate::shell::mpris::resume_after_launch(crate::shell::mpris::ResumeRequest {
                        request_id: request.request_id,
                        package_id: request.app_id.clone(),
                        source,
                        position_ms: request.position_ms,
                        playing: request.resume_playing,
                    });
            }
        }
        true
    }

    fn retry_waiting_media(&mut self, network: &ShellNetwork) {
        let waiting = std::mem::take(&mut self.waiting_incoming_media);
        for (request_id, request) in waiting {
            if request.received.elapsed() >= INCOMING_MEDIA_WAIT {
                continue;
            }
            if !self.try_launch_incoming_media(network, &request) {
                self.waiting_incoming_media.insert(request_id, request);
            }
        }
    }

    fn acknowledge_opened_media(&mut self, network: &ShellNetwork) {
        let mut accepted = media::handoff::accepted_handoff_requests();
        accepted.extend(crate::shell::mpris::take_accepted_handoff_requests());
        accepted.sort_unstable();
        accepted.dedup();
        for request_id in accepted {
            let Some((source, session_id)) = self.pending_incoming_media.remove(&request_id) else {
                continue;
            };
            let _ = network.send_personal_fabric_action(
                &source,
                PersonalFabricAction::MediaAccepted {
                    request_id,
                    session_id,
                },
            );
        }
    }
}

fn expire_nearby_consents(network: &ShellNetwork) {
    let expired = nearby_drop_state().lock().map_or_else(
        |_| Vec::new(),
        |mut state| {
            let ids = state
                .incoming
                .iter()
                .filter(|(_, pending)| pending.received.elapsed() >= NEARBY_CONSENT_TIMEOUT)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| state.incoming.remove(&id).map(|pending| (id, pending)))
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

fn resume_nearby_transfers(network: &ShellNetwork) {
    expire_nearby_consents(network);
    if let Ok(receiver) = nearby_staging_queue().1.lock() {
        while let Ok(staged) = receiver.try_recv() {
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
    let entries = try_outbox_entries();
    if let Ok(mut state) = nearby_drop_state().lock() {
        state
            .last_offer
            .retain(|transfer_id, _| entries.iter().any(|entry| entry.id == *transfer_id));
    }
    for entry in entries {
        let Ok(target) = crate::shell::personal_continuity::DeviceId::new(entry.target.clone())
        else {
            continue;
        };
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

fn discard_nearby_staging_results() {
    if let Ok(receiver) = nearby_staging_queue().1.lock() {
        while let Ok(staged) = receiver.try_recv() {
            if staged.result.is_ok() {
                let _ =
                    with_nearby_transfer_store(|store| store.cancel_outgoing(staged.transfer_id));
            }
        }
    }
}

fn prune_spatial_leases(network: &ShellNetwork) {
    if let Ok(mut spatial) = spatial_input_state().lock() {
        let now = unix_ms();
        let fabric = network.device_fabric();
        let issued_to_release = spatial
            .issued
            .values()
            .filter(|lease| {
                lease.validate_at(now).is_err()
                    || fabric
                        .snapshots()
                        .get(&lease.controller)
                        .is_none_or(|snapshot| !snapshot.presence.online)
            })
            .map(|lease| lease.controller.clone())
            .collect::<Vec<_>>();
        spatial.issued.retain(|_, lease| {
            lease.validate_at(now).is_ok()
                && fabric
                    .snapshots()
                    .get(&lease.controller)
                    .is_some_and(|snapshot| snapshot.presence.online)
        });
        spatial.received.retain(|target, lease| {
            lease.validate_at(now).is_ok()
                && fabric
                    .snapshots()
                    .get(target)
                    .is_some_and(|snapshot| snapshot.presence.online)
        });
        for controller in issued_to_release {
            if !spatial.release_controllers.contains(&controller) {
                spatial.release_controllers.push_back(controller);
            }
        }
        let received = spatial.received.clone();
        spatial.outbound.retain(|input| {
            received.get(&input.target).is_some_and(|lease| {
                lease.nonce == input.lease_nonce && lease.validate_at(now).is_ok()
            })
        });
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
                transfer_id,
                request_id,
                entry.total_bytes,
                committed_offset,
                prefix_hash,
                entry.content_hash,
                &path,
            );
        }
        PersonalFabricAction::NearbyTransferRestart {
            request_id,
            transfer_id,
        } => {
            let valid = with_nearby_transfer_store(|store| {
                Ok(store.inbox().iter().any(|entry| {
                    entry.id == transfer_id
                        && entry.source == source.as_str()
                        && entry.request_id == request_id
                }))
            })
            .unwrap_or(false);
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
            let incoming = with_nearby_transfer_store(|store| {
                Ok(store.inbox().iter().any(|entry| {
                    entry.id == transfer_id
                        && entry.source == source.as_str()
                        && entry.request_id == request_id
                }))
            })
            .unwrap_or(false);
            if incoming {
                let _ = with_nearby_transfer_store(|store| store.cancel_incoming(transfer_id));
                if let Ok(mut state) = nearby_drop_state().lock() {
                    state.destinations.remove(&transfer_id);
                }
            }
            let outgoing = nearby_drop_state().lock().ok().and_then(|mut state| {
                let pending = state
                    .large_offers
                    .get(&transfer_id)
                    .cloned()
                    .filter(|(target, offer, _)| target == &source && offer == &request_id);
                if pending.is_some() {
                    state.large_offers.remove(&transfer_id);
                }
                pending
            });
            if outgoing.is_some() {
                let _ = with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
            }
        }
        PersonalFabricAction::NearbyTransferResult {
            request_id,
            transfer_id,
            accepted,
        } => {
            let pending = nearby_drop_state().lock().ok().and_then(|mut state| {
                let pending = state
                    .large_offers
                    .get(&transfer_id)
                    .cloned()
                    .filter(|(target, offer, _)| target == &source && offer == &request_id);
                if accepted && pending.is_some() {
                    state.large_offers.remove(&transfer_id);
                }
                pending
            });
            if let Some((peer, _, file_name)) = pending {
                if accepted {
                    let _ = with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
                }
                if let Ok(mut state) = nearby_drop_state().lock() {
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

fn handle_spatial_action(
    source: crate::shell::personal_continuity::DeviceId,
    target: &crate::shell::personal_continuity::DeviceId,
    action: PersonalFabricAction,
) {
    let Ok(mut spatial) = spatial_input_state().lock() else {
        return;
    };
    match action {
        PersonalFabricAction::SetDisplayPlacement { rect } => {
            spatial.placement_override = Some(rect);
            spatial.placement_stream_source = Some(source);
        }
        PersonalFabricAction::InputLeaseGranted { lease } => {
            if lease.validate_at(unix_ms()).is_ok() {
                spatial.outbound.retain(|input| input.target != source);
                spatial.received.insert(source, lease);
            }
        }
        PersonalFabricAction::InputLeaseRevoked { nonce } => {
            spatial
                .received
                .retain(|device, lease| device != &source || lease.nonce != nonce);
            spatial
                .outbound
                .retain(|input| input.target != source || input.lease_nonce != nonce);
        }
        PersonalFabricAction::Input { lease_nonce, event } => {
            let accepted = spatial.issued.get(&lease_nonce).is_some_and(|lease| {
                lease.binds(&source, target)
                    && lease.validate_at(unix_ms()).is_ok()
                    && match event {
                        FabricInputEvent::PointerMotion { .. }
                        | FabricInputEvent::PointerButton { .. }
                        | FabricInputEvent::Scroll { .. } => lease.allow_pointer,
                        FabricInputEvent::Key { .. } => lease.allow_keyboard,
                    }
            });
            if accepted {
                spatial.inbound.push_back((source, event));
            }
        }
        PersonalFabricAction::ResumeMedia { .. }
        | PersonalFabricAction::MediaAccepted { .. }
        | PersonalFabricAction::RemoteAudioOffer { .. }
        | PersonalFabricAction::RemoteAudioDecision { .. }
        | PersonalFabricAction::RemoteAudioFrame { .. }
        | PersonalFabricAction::RemoteAudioStop { .. }
        | PersonalFabricAction::BluetoothHandoffPrepare { .. }
        | PersonalFabricAction::BluetoothHandoffPrepared { .. }
        | PersonalFabricAction::BluetoothHandoffCommit { .. }
        | PersonalFabricAction::BluetoothHandoffFinished { .. }
        | PersonalFabricAction::BluetoothHandoffAccepted { .. }
        | PersonalFabricAction::BluetoothHandoffPairingOffered { .. }
        | PersonalFabricAction::BluetoothHandoffPairingCommit { .. }
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
                state.transactions.remove(&request_id);
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
                            address,
                        },
                    )
                    .is_ok();
                if accepted
                    && let Ok(mut state) = bluetooth_handoff_state().lock()
                    && let Some(transaction) = state.transactions.remove(&request_id)
                {
                    tracing::info!(
                        accessory = transaction.name,
                        target = transaction.peer.as_str(),
                        "Bluetooth accessory handoff completed"
                    );
                }
            } else {
                state.transactions.remove(&request_id);
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
                state.transactions.remove(&request_id);
            }
        }
        _ => {}
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
                state.transactions.remove(request_id);
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
                        state.transactions.remove(&request_id);
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
                state.transactions.remove(&request_id);
            }
        }
        BluetoothWorkerOutcome::SourceDisconnected(disconnected) => {
            if let Err(error) = disconnected {
                tracing::warn!(%error, "could not release Bluetooth accessory for handoff");
                if let Ok(mut state) = bluetooth_handoff_state().lock() {
                    state.transactions.remove(&request_id);
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
                    state.start_worker(
                        request_id,
                        address,
                        BluetoothWorkerOperation::ReconnectSource,
                    );
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
                state.transactions.remove(&request_id);
                state.start_worker(
                    request_id,
                    address,
                    BluetoothWorkerOperation::ReconnectSource,
                );
            }
        }
        outcome @ (BluetoothWorkerOutcome::TargetPaired(_)
        | BluetoothWorkerOutcome::TargetConnected(_)) => {
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
                    state.transactions.remove(&request_id)
                }
            });
            let Some(transaction) = transaction else {
                if success {
                    std::thread::spawn(move || {
                        let _ = os_actions::bt_disconnect(&address);
                    });
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
                    state.transactions.remove(&request_id);
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
        return Err("that Bluetooth accessory identity is invalid".into());
    }
    let accessory = os_actions::list_bt_devices()?
        .into_iter()
        .find(|device| device.mac.eq_ignore_ascii_case(address) && device.connected)
        .ok_or_else(|| {
            "only a paired accessory currently connected to this device can be handed off"
                .to_string()
        })?;
    if accessory.name.is_empty()
        || accessory.name.len() > 128
        || accessory.name.chars().any(char::is_control)
    {
        return Err("the local Bluetooth accessory name is invalid".into());
    }
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| "this device has no certified personal identity".to_string())?;
    if target == &local {
        return Err("the Bluetooth accessory is already on this device".into());
    }
    let fabric = network.device_fabric();
    let destination = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| "the target is not an enrolled device".to_string())?;
    if !destination.presence.online {
        return Err("the target device is offline".into());
    }
    if bluetooth_handoff_state()
        .lock()
        .map_err(|_| "Bluetooth handoff coordinator is unavailable".to_string())?
        .transactions
        .len()
        >= MAX_BLUETOOTH_HANDOFFS
    {
        return Err("too many Bluetooth handoffs are already in progress".into());
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
        .map_err(|_| "Bluetooth handoff coordinator is unavailable".to_string())?;
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
            .map_err(|_| "Bluetooth handoff coordinator is unavailable".to_string())?;
        let transaction = state
            .transactions
            .get_mut(&request_id)
            .filter(|transaction| {
                !transaction.outbound
                    && transaction.phase == BluetoothHandoffPhase::AwaitingLocalAuthorization
            })
            .ok_or_else(|| "that Bluetooth handoff approval is no longer pending".to_string())?;
        let transaction = transaction.clone();
        let authorization = if approved {
            let mut authorization = [0; 16];
            while authorization == [0; 16] {
                OsRng.fill_bytes(&mut authorization);
            }
            if let Some(pending) = state.transactions.get_mut(&request_id) {
                pending.phase = BluetoothHandoffPhase::PairingReady;
                pending.authorization = Some(authorization);
                pending.started = Instant::now();
            }
            Some(authorization)
        } else {
            state.transactions.remove(&request_id);
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
            state.transactions.remove(&request_id);
        }
        return Err(error);
    }
    Ok(())
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
        .ok_or_else(|| "that media session is no longer available".to_string())?;
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
            let transfer = network.prepare_personal_media_transfer(target, path)?;
            (
                MediaSourceLocator::PersonalTransfer {
                    transfer_id: transfer.transfer_id,
                    file_name: transfer.file_name.clone(),
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
    let request_id = network.send_personal_fabric_action(target, action)?;
    if let Some(transfer) = local_transfer {
        network.start_personal_media_transfer(transfer)?;
    }
    media::handoff::remember_outbound_handoff(request_id, report);
    Ok(request_id)
}

/// Start a shell-owned live audio route using one opaque source-picker token.
/// PipeWire node ids are resolved only inside [`crate::shell::remote_audio`].
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

/// Send one file or folder to an authenticated exact target. Files up to
/// 48 KiB use the inline fast path; everything else is staged immutably into
/// the durable resumable object spool.
///
/// # Errors
///
/// Returns an error for an unsafe/oversized source or unavailable fabric route.
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
        .ok_or_else(|| "file name is not portable".to_string())?
        .to_string();
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("source cannot be inspected: {error}"))?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err("nearby drop requires one regular file or folder".into());
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
            OsRng.fill_bytes(&mut transfer_id);
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
                            unix_millis(),
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
                });
            })
            .map_err(|error| format!("nearby transfer could not start: {error}"))?;
        if let Ok(mut state) = nearby_drop_state().lock() {
            state.events.push_back(NearbyDropEvent::Sending {
                file_name,
                peer: target,
            });
        }
        return Ok(transfer_id);
    }
    if metadata.len() == 0 {
        return Err("nearby drop cannot send an empty file".into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("file cannot be opened: {error}"))?;
    let mut bytes = Vec::new();
    file.take((crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("file cannot be read: {error}"))?;
    if bytes.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES {
        return Err("file changed while being read".into());
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
            OsRng.fill_bytes(&mut transfer_id);
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
                            unix_millis(),
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
                });
            })
            .map_err(|error| format!("nearby transfer could not start: {error}"))?;
        if let Ok(mut state) = nearby_drop_state().lock() {
            state.events.push_back(NearbyDropEvent::Sending {
                file_name,
                peer: target,
            });
        }
        return Ok(transfer_id);
    }
    if metadata.len() == 0 {
        return Err("nearby drop cannot send an empty file".into());
    }
    let mut bytes = Vec::new();
    source
        .into_file()
        .take((crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("file cannot be read: {error}"))?;
    if bytes.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES {
        return Err("file changed while being read".into());
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
        .ok_or_else(|| "nearby transfer is not active".to_string())?;
    let (target, request_id, _) = pending;
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
pub fn decide_nearby_transfer_at(
    network: &ShellNetwork,
    transfer_id: [u8; 16],
    accept: bool,
    destination: Option<crate::shell::nearby_transfer::SecureNearbyDestination>,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::FileTransfer)?;
    if accept && destination.is_none() {
        return Err("Nearby Send requires an approved destination".into());
    }
    let pending = nearby_drop_state()
        .lock()
        .map_err(|_| "nearby transfer state is unavailable".to_string())?
        .incoming
        .remove(&transfer_id);
    let Some(pending) = pending else {
        if !accept {
            return Err("nearby transfer consent expired".into());
        }
        let destination =
            destination.ok_or_else(|| "Nearby Send destination expired".to_string())?;
        let resumed = with_nearby_transfer_store(|store| {
            let entry = store
                .inbox()
                .iter()
                .find(|entry| entry.id == transfer_id)
                .cloned()
                .ok_or_else(|| "nearby transfer is no longer recoverable".to_string())?;
            let (committed_offset, prefix_hash) = store.begin_incoming(
                entry.id,
                entry.source.clone(),
                entry.request_id,
                entry.name.clone(),
                entry.kind,
                entry.total_bytes,
                entry.content_hash,
                unix_millis(),
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
                    .ok_or_else(|| "Nearby Send destination expired".to_string())?
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
                if let Ok(mut state) = nearby_drop_state().lock() {
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
                store.begin_incoming(
                    transfer_id,
                    pending.source.as_str().to_string(),
                    pending.request_id,
                    pending.name,
                    kind,
                    pending.total_bytes,
                    content_hash,
                    unix_millis(),
                )
            })?;
            let destination =
                destination.ok_or_else(|| "Nearby Send destination expired".to_string())?;
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
    let exists = try_outbox_entries()
        .iter()
        .any(|entry| entry.id == transfer_id);
    if !exists {
        return Err("nearby transfer is no longer retryable".into());
    }
    if let Ok(mut state) = nearby_drop_state().lock() {
        state.large_offers.remove(&transfer_id);
        state.progress.remove(&transfer_id);
        state.last_offer.remove(&transfer_id);
    }
    resume_nearby_transfers(network);
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

pub fn nearby_transfer_statuses(network: &ShellNetwork) -> Vec<NearbyTransferRuntimeStatus> {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::FileTransfer) {
        return Vec::new();
    }
    expire_nearby_consents(network);
    let fabric = network.device_fabric();
    let label = |device: &crate::shell::personal_continuity::DeviceId| {
        fabric.snapshots().get(device).map_or_else(
            || "Nearby device".to_string(),
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
    for entry in try_inbox_entries() {
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
    for entry in outbox {
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
                    PersonalMediaTransferStatus::Started
                    | PersonalMediaTransferStatus::Progress => {
                        runtime_collab::NearbyTransferPhase::Transferring
                    }
                    PersonalMediaTransferStatus::Completed => {
                        runtime_collab::NearbyTransferPhase::Completed
                    }
                    PersonalMediaTransferStatus::Failed => {
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

fn install_nearby_drop(
    file_name: &str,
    bytes: &[u8],
    expected: [u8; 32],
) -> Result<PathBuf, String> {
    if !super::device_fabric::valid_nearby_file_name(file_name)
        || bytes.is_empty()
        || bytes.len() > crate::shell::device_fabric::MAX_NEARBY_DROP_BYTES
        || blake3::hash(bytes).as_bytes() != &expected
    {
        return Err("invalid nearby drop".into());
    }
    let root = dirs::download_dir()
        .ok_or_else(|| "Downloads is unavailable".to_string())?
        .join("Tilo Nearby");
    install_nearby_drop_at(&root, file_name, bytes)
}

fn install_nearby_drop_at(root: &Path, file_name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    if root.exists() {
        if !std::fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            return Err("nearby drop folder is unsafe".into());
        }
    } else {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|error| error.to_string())?;
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    for suffix in 0..1000 {
        let name = if suffix == 0 {
            file_name.to_string()
        } else {
            let path = Path::new(file_name);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
            path.extension().and_then(|e| e.to_str()).map_or_else(
                || format!("{stem} ({suffix})"),
                |ext| format!("{stem} ({suffix}).{ext}"),
            )
        };
        let path = root.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(mut file) => {
                if file.write_all(bytes).and_then(|()| file.sync_all()).is_ok() {
                    return Ok(path);
                }
                let _ = std::fs::remove_file(&path);
                return Err("nearby drop could not be saved".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("nearby drop folder has too many name collisions".into())
}

fn fabric_transfer_views(
    network: &ShellNetwork,
) -> Vec<crate::shell::overlay_host::FabricTransferView> {
    use crate::network::actor::PersonalMediaTransferStatus;
    use crate::shell::overlay_host::{FabricTransferStatus, FabricTransferView};

    let media = network
        .outgoing_personal_media_transfers()
        .into_iter()
        .map(|transfer| FabricTransferView {
            target: transfer.target,
            sent_bytes: transfer.sent_bytes,
            total_bytes: transfer.total_bytes,
            status: match transfer.status {
                PersonalMediaTransferStatus::Started | PersonalMediaTransferStatus::Progress => {
                    FabricTransferStatus::Sending
                }
                PersonalMediaTransferStatus::Completed => FabricTransferStatus::Sent,
                PersonalMediaTransferStatus::Failed => FabricTransferStatus::Failed,
            },
        });
    let nearby = nearby_drop_state().lock().map_or_else(
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
                            PersonalMediaTransferStatus::Started
                            | PersonalMediaTransferStatus::Progress => {
                                FabricTransferStatus::Sending
                            }
                            PersonalMediaTransferStatus::Completed => FabricTransferStatus::Sent,
                            PersonalMediaTransferStatus::Failed => FabricTransferStatus::Failed,
                        },
                    },
                )
                .collect::<Vec<_>>()
        },
    );
    media.chain(nearby).collect()
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

/// Build the bounded presentation model for Quick Settings. Every remote row
/// comes from a snapshot authored by that certified device; local `PipeWire`
/// outputs are deliberately kept separate from personal-fabric identities.
pub fn device_fabric_view(network: &ShellNetwork) -> crate::shell::overlay_host::DeviceFabricView {
    use crate::shell::overlay_host::{
        DeviceFabricView, FabricDeviceView, FabricMediaView, LocalMediaOutputView,
    };

    let policy = ContinuityPolicySnapshot::load();
    let fabric = network.device_fabric();
    let local_id = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let (issued_to, received_from) = if policy.permits(ContinuityCapability::DisplayInput) {
        active_spatial_control_devices(unix_ms())
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

    let mut local_outputs = media::audio::enumerate()
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

    let transfers = if policy.permits(ContinuityCapability::FileTransfer) {
        fabric_transfer_views(network)
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

    DeviceFabricView {
        bluetooth_authorizations,
        gamepad_authorizations,
        local_gamepads: Vec::new(),
        devices,
        local_media,
        local_outputs,
        transfers,
        remote_audio: Default::default(),
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
        .ok_or_else(|| "that audio output is no longer connected".to_string())?;
    let target_serial = sink.serial;
    let control = media::audio::real_control();
    control.set_default_sink(node_id)?;
    for stream in graph.playback_streams {
        control.route_stream(stream.id, target_serial)?;
    }
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
        .ok_or_else(|| "this device has no certified personal identity".to_string())?;
    let fabric = network.device_fabric();
    let local_rect = fabric
        .snapshots()
        .get(&local)
        .and_then(|snapshot| snapshot.display.as_ref())
        .map(|display| display.rect)
        .ok_or_else(|| "this device has no published display placement".to_string())?;
    let target_snapshot = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| "the target is not an enrolled device".to_string())?;
    if !target_snapshot.presence.online
        || !target_snapshot.presence.capabilities.display
        || !target_snapshot.presence.capabilities.pointer
    {
        return Err("the target is not an online controllable display".into());
    }
    let target_rect = target_snapshot
        .display
        .as_ref()
        .map(|display| display.rect)
        .ok_or_else(|| "the target has no published display".to_string())?;
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
        return Err("another enrolled display already occupies that side".into());
    }
    network
        .send_personal_fabric_action(target, PersonalFabricAction::SetDisplayPlacement { rect })
        .map(|_| ())
}

/// Compatibility helper for callers that use the original right-edge action.
///
/// # Errors
///
/// Returns the same placement or delivery errors as [`place_device`].
pub fn place_device_right(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
    place_device(network, target, DisplayEdge::Right)
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
    require_continuity(ContinuityCapability::DisplayInput)?;
    let (target, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| "this device has no certified personal identity".to_string())?;
    let fabric = network.device_fabric();
    let presence = fabric
        .snapshots()
        .get(controller)
        .map(|snapshot| &snapshot.presence)
        .ok_or_else(|| "the controller is not an enrolled device".to_string())?;
    if !presence.online || !(presence.capabilities.pointer || presence.capabilities.keyboard) {
        return Err("the controller has no online pointer or keyboard capability".into());
    }
    let mut nonce = [0_u8; 16];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| "secure randomness is unavailable".to_string())?;
    if nonce == [0; 16] {
        return Err("secure randomness returned an invalid lease nonce".into());
    }
    let now = unix_ms();
    let lease = InputLease {
        controller: controller.clone(),
        target,
        issued_unix_ms: now,
        expires_unix_ms: now.saturating_add(MAX_INPUT_LEASE_MS),
        nonce,
        allow_keyboard: presence.capabilities.keyboard,
        allow_pointer: presence.capabilities.pointer,
    };
    network.send_personal_fabric_action(
        controller,
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
        if replaced && !spatial.release_controllers.contains(controller) {
            spatial.release_controllers.push_back(controller.clone());
        }
        spatial.issued.insert(nonce, lease.clone());
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
    let leases = spatial_input_state()
        .lock()
        .map_err(|_| "spatial input state is unavailable".to_string())?
        .issued
        .values()
        .filter(|lease| &lease.controller == controller)
        .cloned()
        .collect::<Vec<_>>();
    if leases.is_empty() {
        return Err("that device does not have an active control lease".into());
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
        spatial
            .issued
            .retain(|_, lease| &lease.controller != controller);
        if !spatial.release_controllers.contains(controller) {
            spatial.release_controllers.push_back(controller.clone());
        }
    }
    delivery_error.map_or(Ok(()), Err)
}

/// Send one lease-bound input event to an adjacent certified target.
///
/// # Errors
///
/// Returns an error if the target has not granted control, the lease expired,
/// its capability excludes the event, or transport delivery fails.
pub fn send_spatial_input(
    _network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    event: FabricInputEvent,
) -> Result<(), String> {
    require_continuity(ContinuityCapability::DisplayInput)?;
    let lease = spatial_input_state()
        .lock()
        .map_err(|_| "spatial input state is unavailable".to_string())?
        .received
        .get(target)
        .cloned()
        .ok_or_else(|| "that device has not granted control".to_string())?;
    lease
        .validate_at(unix_ms())
        .map_err(|error| error.to_string())?;
    let allowed = match event {
        FabricInputEvent::PointerMotion { .. }
        | FabricInputEvent::PointerButton { .. }
        | FabricInputEvent::Scroll { .. } => lease.allow_pointer,
        FabricInputEvent::Key { .. } => lease.allow_keyboard,
    };
    if !allowed {
        return Err("the active lease does not allow that input device".into());
    }
    spatial_input_state()
        .lock()
        .map_err(|_| "spatial input state is unavailable".to_string())?
        .queue_outbound(OutboundSpatialInput {
            target: target.clone(),
            lease_nonce: lease.nonce,
            event,
        })
}

fn flush_spatial_input(network: &ShellNetwork) {
    for _ in 0..MAX_SPATIAL_INPUT_FLUSH_PER_FRAME {
        let input = spatial_input_state()
            .lock()
            .ok()
            .and_then(|mut spatial| spatial.outbound.pop_front());
        let Some(input) = input else {
            return;
        };
        let result = network.send_personal_fabric_action(
            &input.target,
            PersonalFabricAction::Input {
                lease_nonce: input.lease_nonce,
                event: input.event,
            },
        );
        if result.is_err() {
            if let Ok(mut spatial) = spatial_input_state().lock() {
                spatial.outbound.push_front(input);
            }
            return;
        }
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

/// Drain compositor-facing display lifecycle. Network ingress is already
/// identity-, placement-, session-, and capability-checked before this point.
pub fn take_personal_display_events() -> Vec<PersonalDisplayEvent> {
    personal_display_state()
        .lock()
        .map_or_else(|_| Vec::new(), |mut state| state.events.drain(..).collect())
}

/// The strictest profile accepted by all active receivers. A single encoded
/// whole-desktop packet can therefore be fanned out without exceeding any
/// negotiated target ceiling.
pub fn personal_display_capture_profile() -> Option<tilo_projector::CaptureProfile> {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::DisplayInput) {
        return None;
    }
    let profile = {
        let state = personal_display_state().lock().ok()?;
        let mut sessions = state.outbound.values().filter(|session| session.accepted);
        let first = sessions.next()?.profile;
        let profile = sessions.fold(first, |profile, session| DisplayStreamProfile {
            codec: profile.codec,
            max_width: profile.max_width.min(session.profile.max_width),
            max_height: profile.max_height.min(session.profile.max_height),
            max_fps: profile.max_fps.min(session.profile.max_fps),
            max_bitrate_kbps: profile
                .max_bitrate_kbps
                .min(session.profile.max_bitrate_kbps),
        });
        drop(state);
        profile
    };
    Some(tilo_projector::CaptureProfile::new(
        profile.max_width,
        profile.max_height,
        u32::from(profile.max_fps),
        profile.max_bitrate_kbps,
    ))
}

/// Fan one encoded frame to every accepted exact-target session. Failed sends
/// are backpressure, not an invitation to buffer; the next frame supersedes it.
pub fn publish_personal_display_packet(
    network: &ShellNetwork,
    packet: &tilo_projector::VideoPacket,
) -> bool {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::DisplayInput) {
        return false;
    }
    let Some((source, _)) = network.local_device_fabric_identity() else {
        return false;
    };
    let Ok(mut state) = personal_display_state().lock() else {
        return false;
    };
    let mut delivered = false;
    for (target, session) in &mut state.outbound {
        if !session.accepted {
            continue;
        }
        session.sequence = session.sequence.saturating_add(1).max(1);
        let message = PersonalDisplayMessage::Frame {
            session_id: session.session_id,
            source: source.clone(),
            target: target.clone(),
            sequence: session.sequence,
            presentation_time_us: packet.presentation_time_us,
            keyframe: packet.keyframe,
            width: packet.width,
            height: packet.height,
            annex_b: packet.annex_b.clone(),
        };
        delivered |= network.send_personal_display(target, message).is_ok();
    }
    delivered
}

pub fn report_personal_display_decode(
    network: &ShellNetwork,
    session_id: [u8; 16],
    sequence: u64,
    outcome: crate::shell::peer_view::PushOutcome,
) {
    if !ContinuityPolicySnapshot::load().permits(ContinuityCapability::DisplayInput) {
        return;
    }
    let Some((target, _)) = network.local_device_fabric_identity() else {
        return;
    };
    let Ok(state) = personal_display_state().lock() else {
        return;
    };
    let Some(session) = state
        .inbound
        .as_ref()
        .filter(|session| session.session_id == session_id)
    else {
        return;
    };
    let (queue_pressure, request_keyframe) = match outcome {
        crate::shell::peer_view::PushOutcome::Decoded => (0, false),
        crate::shell::peer_view::PushOutcome::DroppedOldest => (100, false),
        crate::shell::peer_view::PushOutcome::DecodeFailed => (100, true),
    };
    let source = session.source.clone();
    drop(state);
    let _ = network.send_personal_display(
        &source,
        PersonalDisplayMessage::Feedback {
            session_id,
            source: source.clone(),
            target,
            consumed_sequence: sequence,
            queue_pressure,
            request_keyframe,
        },
    );
}

pub fn take_personal_display_keyframe_request() -> bool {
    personal_display_state().lock().is_ok_and(|mut state| {
        let requested = state
            .outbound
            .values()
            .any(|session| session.request_keyframe);
        for session in state.outbound.values_mut() {
            session.request_keyframe = false;
        }
        requested
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "session reconciliation and its exhaustive wire transition match stay together as one fail-closed boundary"
)]
fn pump_personal_displays(network: &ShellNetwork, policy: ContinuityPolicySnapshot) {
    if !policy.permits(ContinuityCapability::DisplayInput) {
        // Drain transport ingress while disabled so stale offers cannot become
        // live if the user opts in later.
        let _ = network.take_personal_display_messages();
        return;
    }
    let Some((local, _)) = network.local_device_fabric_identity() else {
        return;
    };
    let fabric = network.device_fabric();
    let desired = fabric
        .snapshots()
        .values()
        .filter(|snapshot| snapshot.owner != local)
        .filter(|snapshot| snapshot.presence.online && snapshot.presence.capabilities.video_output)
        .filter(|snapshot| {
            snapshot
                .display
                .as_ref()
                .is_some_and(|display| display.stream_source.as_ref() == Some(&local))
        })
        .take(MAX_DISPLAY_SESSIONS)
        .map(|snapshot| snapshot.owner.clone())
        .collect::<Vec<_>>();
    let authorized_inbound = fabric
        .snapshots()
        .get(&local)
        .and_then(|snapshot| snapshot.display.as_ref())
        .and_then(|display| display.stream_source.clone())
        .filter(|source| {
            fabric
                .snapshots()
                .get(source)
                .is_some_and(|snapshot| snapshot.presence.online)
        });
    drop(fabric);

    let now = Instant::now();
    let mut sends = Vec::new();
    let Ok(mut state) = personal_display_state().lock() else {
        return;
    };
    if state.inbound.as_ref().is_some_and(|session| {
        authorized_inbound.as_ref() != Some(&session.source)
            || now.saturating_duration_since(session.last_activity) >= DISPLAY_SESSION_TIMEOUT
    }) && let Some(session) = state.inbound.take()
    {
        state.queue_event(PersonalDisplayEvent::Ended {
            session_id: session.session_id,
        });
    }
    let stale_targets = state
        .outbound
        .keys()
        .filter(|target| !desired.contains(target))
        .cloned()
        .collect::<Vec<_>>();
    for target in stale_targets {
        if let Some(session) = state.outbound.remove(&target) {
            sends.push((
                target.clone(),
                PersonalDisplayMessage::End {
                    session_id: session.session_id,
                    source: local.clone(),
                    target: target.clone(),
                },
            ));
        }
    }
    for target in desired {
        let session = state.outbound.entry(target.clone()).or_insert_with(|| {
            let mut session_id = [0_u8; 16];
            while session_id == [0; 16] {
                OsRng.fill_bytes(&mut session_id);
            }
            OutboundDisplaySession {
                session_id,
                profile: default_display_profile(),
                accepted: false,
                last_offer: None,
                sequence: 0,
                request_keyframe: false,
            }
        });
        let offer_interval = if session.accepted {
            DISPLAY_KEEPALIVE
        } else {
            DISPLAY_OFFER_RETRY
        };
        if session
            .last_offer
            .is_none_or(|last| now.saturating_duration_since(last) >= offer_interval)
        {
            session.last_offer = Some(now);
            sends.push((
                target.clone(),
                PersonalDisplayMessage::Offer {
                    protocol_version: DISPLAY_STREAM_PROTOCOL_VERSION,
                    session_id: session.session_id,
                    source: local.clone(),
                    target,
                    profile: session.profile,
                    issued_unix_ms: unix_ms(),
                },
            ));
        }
    }

    for message in network.take_personal_display_messages() {
        match message {
            PersonalDisplayMessage::Offer {
                session_id,
                source,
                target,
                profile,
                ..
            } if target == local => {
                let negotiated = profile.negotiate(default_display_profile());
                let Some(profile) = negotiated else { continue };
                state.install_inbound(InboundDisplaySession {
                    session_id,
                    source: source.clone(),
                    profile,
                    last_activity: now,
                    last_sequence: 0,
                });
                sends.push((
                    source.clone(),
                    PersonalDisplayMessage::Accept {
                        protocol_version: DISPLAY_STREAM_PROTOCOL_VERSION,
                        session_id,
                        source,
                        target: local.clone(),
                        profile,
                    },
                ));
            }
            PersonalDisplayMessage::Accept {
                session_id,
                source,
                target,
                profile,
                ..
            } if source == local => {
                if let Some(session) = state.outbound.get_mut(&target)
                    && session.session_id == session_id
                    && session.profile.negotiate(profile) == Some(profile)
                {
                    session.profile = profile;
                    session.accepted = true;
                }
            }
            PersonalDisplayMessage::Frame {
                session_id,
                source,
                target,
                sequence,
                presentation_time_us,
                keyframe,
                width,
                height,
                annex_b,
            } if target == local => {
                if state.accept_frame(session_id, &source, sequence, width, height, now) {
                    state.events.retain(|event| !matches!(event, PersonalDisplayEvent::Frame { session_id: queued, .. } if queued == &session_id));
                    state.queue_event(PersonalDisplayEvent::Frame {
                        session_id,
                        sequence,
                        presentation_time_us,
                        keyframe,
                        width,
                        height,
                        annex_b,
                    });
                }
            }
            PersonalDisplayMessage::Feedback {
                session_id,
                source,
                target,
                queue_pressure,
                request_keyframe,
                ..
            } if source == local => {
                if let Some(session) = state.outbound.get_mut(&target)
                    && session.session_id == session_id
                {
                    session.request_keyframe |= request_keyframe || queue_pressure >= 80;
                }
            }
            PersonalDisplayMessage::End {
                session_id, target, ..
            } if target == local
                && state
                    .inbound
                    .as_ref()
                    .is_some_and(|session| session.session_id == session_id) =>
            {
                state.inbound = None;
                state.queue_event(PersonalDisplayEvent::Ended { session_id });
            }
            _ => {}
        }
    }
    drop(state);
    for (target, message) in sends {
        let _ = network.send_personal_display(&target, message);
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "snapshot construction receives each sampled runtime input explicitly"
)]
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
    let (settings, wifi) = if policy.permits(ContinuityCapability::WifiSettings) {
        (
            reconcile_roaming_settings(
                roaming_setting_values(),
                previous,
                applied_remote_settings,
                stamp,
            ),
            previous.map_or_else(Vec::new, |state| state.wifi.clone()),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let display = if policy.permits(ContinuityCapability::DisplayInput) {
        local.display.map(|rect| DisplayPlacement {
            device: device.clone(),
            rect,
            scale_milli: 1000,
            stream_source: spatial_input_state()
                .lock()
                .ok()
                .and_then(|state| state.placement_stream_source.clone())
                .or_else(|| {
                    previous.and_then(|state| state.display.as_ref()?.stream_source.clone())
                }),
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
            stamp: stamp.clone(),
        },
        media,
        clipboard,
        settings,
        wifi,
        display,
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
        roaming_settings: policy.permits(ContinuityCapability::WifiSettings),
        secret_store: policy.permits(ContinuityCapability::WifiSettings),
        app_continuation: policy.permits(ContinuityCapability::AppContinuations),
    }
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
    let mut out = BTreeMap::new();
    for (section, value) in document {
        let Value::Object(values) = value else {
            continue;
        };
        for (key, value) in values {
            let fabric_key = format!("tilo.{section}.{key}");
            if super::device_fabric::valid_setting_key(&fabric_key) {
                out.insert(fabric_key, value.clone());
            }
        }
    }
    out
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
        .ok_or_else(|| {
            format!(
                "{} continuity is disabled on this device",
                capability.label()
            )
        })
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
            return Err("typed-gamepad forwarding capacity is exhausted".into());
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
    let mut state = gamepad_authorization_state()
        .lock()
        .map_err(|_| "typed-gamepad authorization state is unavailable".to_string())?;
    if !state.decide(forwarding_id, approved) {
        return Err("typed-gamepad authorization is unknown or already decided".into());
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
            .ok_or_else(|| "typed-gamepad target has not granted a lease".to_string())?;
        let sequence = session
            .sequence
            .checked_add(1)
            .ok_or_else(|| "typed-gamepad event sequence is exhausted".to_string())?;
        (session.target.clone(), lease_nonce, sequence)
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
    OsRng.try_fill_bytes(&mut nonce).ok()?;
    (nonce != [0; 16]).then_some(nonce)
}

fn tear_down_display_and_input(network: &ShellNetwork) {
    if let Some((local, _)) = network.local_device_fabric_identity()
        && let Ok(mut state) = personal_display_state().lock()
    {
        for (target, session) in std::mem::take(&mut state.outbound) {
            let _ = network.send_personal_display(
                &target,
                PersonalDisplayMessage::End {
                    session_id: session.session_id,
                    source: local.clone(),
                    target: target.clone(),
                },
            );
        }
        if let Some(session) = state.inbound.take() {
            state.queue_event(PersonalDisplayEvent::Ended {
                session_id: session.session_id,
            });
        }
        state
            .events
            .retain(|event| !matches!(event, PersonalDisplayEvent::Frame { .. }));
    }

    if let Ok(mut spatial) = spatial_input_state().lock() {
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
        spatial.outbound.clear();
        spatial.inbound.clear();
    }
    let _ = network.take_personal_display_messages();
}

fn tear_down_nearby_transfers(network: &ShellNetwork) {
    if let Ok(mut admission) = nearby_staging_admission().lock() {
        admission.invalidate();
    }
    let active = nearby_drop_state().lock().map_or_else(
        |_| Vec::new(),
        |state| {
            state
                .large_offers
                .iter()
                .map(|(transfer_id, (target, request_id, _))| {
                    (*transfer_id, target.clone(), *request_id)
                })
                .collect::<Vec<_>>()
        },
    );
    for (transfer_id, target, request_id) in &active {
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
        state.large_offers.clear();
        state.progress.clear();
        state.last_offer.clear();
        state.incoming.clear();
        state.destinations.clear();
        state.history.clear();
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::personal_continuity::DeviceId;

    #[test]
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

    #[test]
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

    #[test]
    fn nearby_consent_queue_is_bounded_and_requires_explicit_resolution() {
        let source = DeviceId::new("source-device").expect("valid device");
        let mut state = NearbyDropState::default();
        for index in 1..=MAX_PENDING_NEARBY_CONSENTS {
            let mut id = [0_u8; 16];
            id[..8].copy_from_slice(&u64::try_from(index).expect("bounded index").to_le_bytes());
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

    #[test]
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

    #[test]
    fn nearby_drop_never_overwrites_and_rejects_a_symlinked_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("nearby");
        let first = install_nearby_drop_at(&root, "note.txt", b"one").unwrap();
        let second = install_nearby_drop_at(&root, "note.txt", b"two").unwrap();
        assert_eq!(std::fs::read(first).unwrap(), b"one");
        assert_eq!(
            second.file_name().and_then(|name| name.to_str()),
            Some("note (1).txt")
        );
        let unsafe_root = temp.path().join("unsafe");
        std::os::unix::fs::symlink(&root, &unsafe_root).unwrap();
        assert!(install_nearby_drop_at(&unsafe_root, "other.txt", b"no").is_err());
    }

    #[test]
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
        assert!(authorized.accepts_pairing_authorization(&target, "aa:bb:cc:dd:ee:ff", [7; 16]));
        assert!(!authorized.accepts_pairing_authorization(&target, "AA:BB:CC:DD:EE:FF", [8; 16]));
        assert!(!authorized.accepts_pairing_authorization(&other, "AA:BB:CC:DD:EE:FF", [7; 16]));
    }

    #[test]
    fn roaming_projection_includes_personal_preferences_and_excludes_hardware_state() {
        let document = serde_json::json!({
            "locale": {"currency": "EUR", "time_format": "H24"},
            "appearance": {
                "theme": "Nord",
                "studio": {"id": "aurora"},
                "wallpaper_path": "/home/person/private.png"
            },
            "input": {"keyboard_layouts": ["us", "de"], "pen_output": "DP-2"},
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
            "tilo.appearance.theme",
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
            "tilo.input.pen_output",
            "tilo.audio.output_device",
            "tilo.display.scale",
            "tilo.display.auto_brightness",
            "tilo.power.auto_suspend",
            "tilo.agent_providers.api_key",
        ] {
            assert!(!projected.contains_key(key), "leaked local key {key}");
        }
    }

    #[test]
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
        };
        let stamp = FabricStamp { counter: 9, device };
        stamp_snapshot(&mut snapshot, &stamp, 42);
        assert_eq!(snapshot.presence.stamp, stamp);
        assert_eq!(snapshot.presence.last_seen_unix_ms, 42);
    }

    #[test]
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
                stamp: old_stamp.clone(),
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: vec![RoamingSetting {
                key: "tilo.appearance.theme".into(),
                value: Value::Bool(false),
                stamp: old_stamp,
            }],
            wifi: Vec::new(),
            display: None,
        };
        let next_stamp = FabricStamp { counter: 9, device };
        let applied_remote = BTreeMap::from([("tilo.appearance.theme".into(), Value::Bool(true))]);

        let received = reconcile_roaming_settings(
            BTreeMap::from([("tilo.appearance.theme".into(), Value::Bool(true))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert_eq!(received, previous.settings);

        let edited = reconcile_roaming_settings(
            BTreeMap::from([("tilo.appearance.theme".into(), Value::Bool(false))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert_eq!(edited[0].stamp, next_stamp);
        assert_eq!(edited[0].value, Value::Bool(false));

        previous.settings.clear();
        let bootstrap = reconcile_roaming_settings(
            BTreeMap::from([("tilo.appearance.theme".into(), Value::Bool(true))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert!(bootstrap.is_empty());
    }

    #[test]
    fn pointer_bursts_coalesce_without_crossing_discrete_input() {
        let target = DeviceId::new("tablet").unwrap();
        let motion = |x| OutboundSpatialInput {
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

    #[test]
    fn replacing_a_personal_display_session_ends_the_old_surface_first() {
        let profile = default_display_profile();
        let mut state = PersonalDisplayState::default();
        state.install_inbound(InboundDisplaySession {
            session_id: [1; 16],
            source: DeviceId::new("desktop").unwrap(),
            profile,
            last_activity: Instant::now(),
            last_sequence: 0,
        });
        state.events.clear();
        state.install_inbound(InboundDisplaySession {
            session_id: [2; 16],
            source: DeviceId::new("desktop").unwrap(),
            profile,
            last_activity: Instant::now(),
            last_sequence: 0,
        });
        assert!(matches!(
            state.events.pop_front(),
            Some(PersonalDisplayEvent::Ended { session_id }) if session_id == [1; 16]
        ));
        assert!(matches!(
            state.events.pop_front(),
            Some(PersonalDisplayEvent::Started { session_id, .. }) if session_id == [2; 16]
        ));
    }

    #[test]
    fn personal_display_rejects_late_and_replayed_frames() {
        let source = DeviceId::new("desktop").unwrap();
        let mut state = PersonalDisplayState::default();
        state.install_inbound(InboundDisplaySession {
            session_id: [9; 16],
            source: source.clone(),
            profile: default_display_profile(),
            last_activity: Instant::now(),
            last_sequence: 0,
        });
        assert!(state.accept_frame([9; 16], &source, 2, 1280, 720, Instant::now()));
        assert!(!state.accept_frame([9; 16], &source, 1, 1280, 720, Instant::now()));
        assert!(!state.accept_frame([9; 16], &source, 2, 1280, 720, Instant::now()));
        assert_eq!(state.inbound.as_ref().unwrap().last_sequence, 2);
    }

    fn pending_gamepad(source: &str) -> PendingGamepadAuthorization {
        PendingGamepadAuthorization {
            source: DeviceId::new(source).unwrap(),
            target: DeviceId::new("target").unwrap(),
            app_id: "dev.tilo.game".into(),
            workspace_id: "workspace".into(),
            session_id: "session".into(),
            label: "Controller".into(),
            received: Instant::now(),
        }
    }

    #[test]
    fn gamepad_request_requires_one_local_decision_and_rejects_duplicate_id() {
        let mut state = GamepadAuthorizationState::default();
        assert!(state.queue([1; 16], pending_gamepad("source"), false, 0));
        assert!(!state.queue([1; 16], pending_gamepad("attacker"), false, 0));
        assert!(state.decide([1; 16], true));
        assert!(!state.decide([1; 16], false));
        assert_eq!(state.decisions, VecDeque::from([([1; 16], true)]));
    }

    #[test]
    fn active_gamepad_lease_blocks_same_forwarding_id_before_consent() {
        let mut state = GamepadAuthorizationState::default();
        assert!(!state.queue([2; 16], pending_gamepad("source"), true, 1));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn gamepad_approval_requires_exact_compositor_local_focus() {
        let pending = pending_gamepad("source");
        let exact = crate::shell::physical_gamepad::PhysicalGamepadBinding {
            app_id: "dev.tilo.game".into(),
            workspace_id: "workspace".into(),
            session_id: "session".into(),
        };
        assert!(pending.matches_local_focus(Some(&exact)));
        assert!(!pending.matches_local_focus(None));
        for mismatch in [
            crate::shell::physical_gamepad::PhysicalGamepadBinding {
                app_id: "dev.tilo.other".into(),
                ..exact.clone()
            },
            crate::shell::physical_gamepad::PhysicalGamepadBinding {
                workspace_id: "other-workspace".into(),
                ..exact.clone()
            },
            crate::shell::physical_gamepad::PhysicalGamepadBinding {
                session_id: "other-session".into(),
                ..exact.clone()
            },
        ] {
            assert!(!pending.matches_local_focus(Some(&mismatch)));
        }
    }

    #[test]
    fn active_gamepad_identity_becomes_invalid_on_local_focus_loss() {
        let identity = os_actions::ForwardedPeripheralIdentity::new_after_verification(
            "source",
            "target",
            "dev.tilo.game",
            "workspace",
            "session",
        )
        .unwrap();
        let exact = crate::shell::physical_gamepad::PhysicalGamepadBinding {
            app_id: "dev.tilo.game".into(),
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

    #[test]
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
            torn_down.push(session)
        });
        assert_eq!(sessions.get(&[8; 16]), Some(&"still-live"));
        assert_eq!(torn_down, ["virtual-gamepad"]);
    }
}
