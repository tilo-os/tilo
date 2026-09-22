//! Capability-safe live audio routing between certified personal devices.
//!
//! PipeWire node identities never leave this process. The source shell resolves
//! a short-lived picker token to a freshly enumerated local stream, the target
//! shell asks for explicit consent, and approval mints a random capability
//! bound to the exact `(source, target, session)` tuple. PCM is deliberately a
//! small version-1 transport: 48 kHz stereo S16LE in bounded 20 ms frames.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore};

use crate::shell::device_fabric::{
    MAX_REMOTE_AUDIO_FRAME_BYTES, MAX_REMOTE_AUDIO_SESSIONS, PersonalFabricAction,
    PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS, REMOTE_AUDIO_FRAME_SAMPLES,
    REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_RATE_HZ,
};
use crate::shell::network::ShellNetwork;
use crate::shell::personal_continuity::DeviceId;
use crate::shell::physical_gamepad::PhysicalGamepadBinding;

const PENDING_TTL: Duration = Duration::from_secs(30);
const SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const CAPTURE_QUEUE: usize = 12;
const PLAYBACK_QUEUE: usize = 12;
const PIPE_BUFFER: usize = 16 * 1024;
const SEND_BUDGET: usize = 8;
const MAX_PENDING_PCM_BYTES: usize = MAX_REMOTE_AUDIO_FRAME_BYTES * SEND_BUDGET * 2;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalAudioSelection {
    System,
    Application {
        serial: u32,
        label: String,
        focus: PhysicalGamepadBinding,
    },
}

fn selection_focus_valid(
    selection: &LocalAudioSelection,
    focus: Option<&PhysicalGamepadBinding>,
) -> bool {
    match selection {
        LocalAudioSelection::System => true,
        LocalAudioSelection::Application { focus: bound, .. } => focus == Some(bound),
    }
}

fn frame_matches(
    source: &DeviceId,
    target: &DeviceId,
    authorization: [u8; 16],
    last_sequence: u64,
    claimed_source: &DeviceId,
    claimed_target: &DeviceId,
    claimed_authorization: [u8; 16],
    sequence: u64,
) -> bool {
    claimed_source == source
        && claimed_target == target
        && claimed_authorization == authorization
        && sequence == last_sequence.saturating_add(1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAudioSourceView {
    pub token: u64,
    pub label: String,
    pub application: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAudioPhase {
    AwaitingConsent,
    Streaming,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAudioRouteView {
    pub session_id: [u8; 16],
    pub peer: DeviceId,
    pub label: String,
    pub inbound: bool,
    pub phase: RemoteAudioPhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAudioConsentView {
    pub session_id: [u8; 16],
    pub source: DeviceId,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteAudioView {
    pub sources: Vec<RemoteAudioSourceView>,
    pub routes: Vec<RemoteAudioRouteView>,
    pub consents: Vec<RemoteAudioConsentView>,
}

#[derive(Clone)]
struct PendingInbound {
    source: DeviceId,
    target: DeviceId,
    label: String,
    received: Instant,
}

struct OutboundRoute {
    target: DeviceId,
    label: String,
    selection: LocalAudioSelection,
    authorization: Option<[u8; 16]>,
    sequence: u64,
    created: Instant,
    capture: Option<PcmCapture>,
    pending_pcm: Vec<u8>,
}

struct InboundRoute {
    source: DeviceId,
    target: DeviceId,
    label: String,
    authorization: [u8; 16],
    last_sequence: u64,
    created: Instant,
    sink: Option<PcmSink>,
}

#[derive(Default)]
struct RemoteAudioState {
    source_tokens: BTreeMap<u64, LocalAudioSelection>,
    pending: BTreeMap<[u8; 16], PendingInbound>,
    outbound: BTreeMap<[u8; 16], OutboundRoute>,
    inbound: BTreeMap<[u8; 16], InboundRoute>,
}

struct Egress {
    target: DeviceId,
    action: PersonalFabricAction,
    /// The exact source-side route whose media send this message advances.
    /// A transport failure terminates only this route, never another stream
    /// that happens to use the same certified peer.
    outbound_session: Option<[u8; 16]>,
}

fn queue_egress(
    egress: &mut Vec<Egress>,
    target: DeviceId,
    action: PersonalFabricAction,
    outbound_session: Option<[u8; 16]>,
) {
    egress.push(Egress {
        target,
        action,
        outbound_session,
    });
}

fn append_bounded_pcm(pending: &mut Vec<u8>, bytes: &[u8]) {
    pending.extend_from_slice(bytes);
    if pending.len() > MAX_PENDING_PCM_BYTES {
        let excess = pending.len() - MAX_PENDING_PCM_BYTES;
        // Stereo S16LE is four bytes per sample frame. Drop old audio at a
        // sample boundary: live routing favours bounded latency over replaying
        // a growing backlog after transport backpressure clears.
        let discard = excess.saturating_add(3) / 4 * 4;
        pending.drain(..discard.min(pending.len()));
    }
}

fn reconcile_lifecycle(
    state: &mut RemoteAudioState,
    enabled: bool,
    online: &BTreeSet<DeviceId>,
    focus: Option<&PhysicalGamepadBinding>,
    now: Instant,
) -> Vec<Egress> {
    let mut egress = Vec::new();
    if !enabled {
        for (session_id, pending) in std::mem::take(&mut state.pending) {
            queue_egress(
                &mut egress,
                pending.source,
                PersonalFabricAction::RemoteAudioDecision {
                    session_id,
                    accepted: false,
                    authorization: None,
                },
                None,
            );
        }
        for (session_id, route) in std::mem::take(&mut state.outbound) {
            let action = route.authorization.map_or(
                PersonalFabricAction::RemoteAudioCancel { session_id },
                |authorization| PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization,
                },
            );
            queue_egress(&mut egress, route.target, action, Some(session_id));
        }
        for (session_id, route) in std::mem::take(&mut state.inbound) {
            queue_egress(
                &mut egress,
                route.source,
                PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization: route.authorization,
                },
                None,
            );
        }
        return egress;
    }

    let stale_pending = state
        .pending
        .iter()
        .filter_map(|(session_id, pending)| {
            (now.duration_since(pending.received) > PENDING_TTL
                || !online.contains(&pending.source))
            .then_some(*session_id)
        })
        .collect::<Vec<_>>();
    for session_id in stale_pending {
        if let Some(pending) = state.pending.remove(&session_id) {
            queue_egress(
                &mut egress,
                pending.source,
                PersonalFabricAction::RemoteAudioDecision {
                    session_id,
                    accepted: false,
                    authorization: None,
                },
                None,
            );
        }
    }

    let stale_inbound = state
        .inbound
        .iter()
        .filter_map(|(session_id, route)| {
            (now.duration_since(route.created) > SESSION_TTL || !online.contains(&route.source))
                .then_some(*session_id)
        })
        .collect::<Vec<_>>();
    for session_id in stale_inbound {
        if let Some(route) = state.inbound.remove(&session_id) {
            queue_egress(
                &mut egress,
                route.source,
                PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization: route.authorization,
                },
                None,
            );
        }
    }

    let stale_outbound = state
        .outbound
        .iter()
        .filter_map(|(session_id, route)| {
            (now.duration_since(route.created) > SESSION_TTL
                || !online.contains(&route.target)
                || !selection_focus_valid(&route.selection, focus))
            .then_some(*session_id)
        })
        .collect::<Vec<_>>();
    for session_id in stale_outbound {
        if let Some(route) = state.outbound.remove(&session_id) {
            let action = route.authorization.map_or(
                PersonalFabricAction::RemoteAudioCancel { session_id },
                |authorization| PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization,
                },
            );
            queue_egress(&mut egress, route.target, action, Some(session_id));
        }
    }
    egress
}

fn apply_send_failure(state: &mut RemoteAudioState, outbound_session: Option<[u8; 16]>) {
    if let Some(session_id) = outbound_session {
        state.outbound.remove(&session_id);
    }
}

fn terminate_capture_failure(
    state: &mut RemoteAudioState,
    session_id: [u8; 16],
    egress: &mut Vec<Egress>,
) {
    let Some(route) = state.outbound.remove(&session_id) else {
        return;
    };
    if let Some(authorization) = route.authorization {
        queue_egress(
            egress,
            route.target,
            PersonalFabricAction::RemoteAudioStop {
                session_id,
                authorization,
            },
            Some(session_id),
        );
    }
}

fn state() -> &'static Mutex<RemoteAudioState> {
    static STATE: OnceLock<Mutex<RemoteAudioState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(RemoteAudioState::default()))
}

fn random_nonzero_16() -> [u8; 16] {
    loop {
        let mut value = [0_u8; 16];
        OsRng.fill_bytes(&mut value);
        if value != [0; 16] {
            return value;
        }
    }
}

fn random_nonzero_u64(existing: &BTreeMap<u64, LocalAudioSelection>) -> u64 {
    loop {
        let value = OsRng.next_u64();
        if value != 0 && !existing.contains_key(&value) {
            return value;
        }
    }
}

fn reconcile_source_tokens(
    state: &mut RemoteAudioState,
    desired: Vec<LocalAudioSelection>,
) -> Vec<RemoteAudioSourceView> {
    state
        .source_tokens
        .retain(|_, existing| desired.contains(existing));
    desired
        .into_iter()
        .map(|selection| {
            let token = state
                .source_tokens
                .iter()
                .find_map(|(token, existing)| (existing == &selection).then_some(*token))
                .unwrap_or_else(|| {
                    let token = random_nonzero_u64(&state.source_tokens);
                    state.source_tokens.insert(token, selection.clone());
                    token
                });
            let (label, application) = match selection {
                LocalAudioSelection::System => ("All system audio".into(), false),
                LocalAudioSelection::Application { label, .. } => (label, true),
            };
            RemoteAudioSourceView {
                token,
                label,
                application,
            }
        })
        .collect()
}

/// Refresh the shell-owned source picker. Tokens are process-local and are
/// retained only while their freshly enumerated source and focus binding stay
/// available, so neither apps nor peers ever see node ids.
pub fn snapshot(focus: Option<&PhysicalGamepadBinding>) -> RemoteAudioView {
    let graph = media::audio::enumerate();
    let Ok(mut state) = state().lock() else {
        return RemoteAudioView::default();
    };
    let mut desired = vec![LocalAudioSelection::System];
    if let Some(focus) = focus {
        for stream in graph.playback_streams.into_iter().take(64) {
            let label = stream.label();
            let selection = LocalAudioSelection::Application {
                serial: stream.serial,
                label,
                focus: focus.clone(),
            };
            if !desired.contains(&selection) {
                desired.push(selection);
            }
        }
    }
    let sources = reconcile_source_tokens(&mut state, desired);
    let mut routes = state
        .outbound
        .iter()
        .map(|(session_id, route)| RemoteAudioRouteView {
            session_id: *session_id,
            peer: route.target.clone(),
            label: route.label.clone(),
            inbound: false,
            phase: if route.authorization.is_some() {
                RemoteAudioPhase::Streaming
            } else {
                RemoteAudioPhase::AwaitingConsent
            },
        })
        .chain(
            state
                .inbound
                .iter()
                .map(|(session_id, route)| RemoteAudioRouteView {
                    session_id: *session_id,
                    peer: route.source.clone(),
                    label: route.label.clone(),
                    inbound: true,
                    phase: RemoteAudioPhase::Streaming,
                }),
        )
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| left.label.cmp(&right.label));
    let mut consents = state
        .pending
        .iter()
        .map(|(session_id, pending)| RemoteAudioConsentView {
            session_id: *session_id,
            source: pending.source.clone(),
            label: pending.label.clone(),
        })
        .collect::<Vec<_>>();
    consents.sort_by(|left, right| left.label.cmp(&right.label));
    RemoteAudioView {
        sources,
        routes,
        consents,
    }
}

/// Offer one freshly resolved local source to one exact certified device.
pub fn request(
    network: &ShellNetwork,
    source_token: u64,
    target: &DeviceId,
) -> Result<[u8; 16], String> {
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device)
        .ok_or_else(|| "this device has no certified personal identity".to_string())?;
    let fabric = network.device_fabric();
    let snapshot = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| "the target is not an enrolled device".to_string())?;
    if target == &local || !snapshot.presence.online || !snapshot.presence.capabilities.audio_output
    {
        return Err("the target is not an online audio output".into());
    }
    let (session_id, selection, label) = {
        let mut state = state()
            .lock()
            .map_err(|_| "remote audio service is unavailable".to_string())?;
        if state.outbound.len() + state.inbound.len() + state.pending.len()
            >= MAX_REMOTE_AUDIO_SESSIONS
        {
            return Err("too many remote audio routes are active".into());
        }
        let selection = state
            .source_tokens
            .remove(&source_token)
            .ok_or_else(|| "that audio source picker entry expired".to_string())?;
        let label = match &selection {
            LocalAudioSelection::System => "All system audio".to_string(),
            LocalAudioSelection::Application { label, .. } => label.clone(),
        };
        let session_id = random_nonzero_16();
        state.outbound.insert(
            session_id,
            OutboundRoute {
                target: target.clone(),
                label: label.clone(),
                selection: selection.clone(),
                authorization: None,
                sequence: 0,
                created: Instant::now(),
                capture: None,
                pending_pcm: Vec::new(),
            },
        );
        (session_id, selection, label)
    };
    let action = PersonalFabricAction::RemoteAudioOffer {
        session_id,
        protocol_version: REMOTE_AUDIO_PROTOCOL_VERSION,
        label,
        sample_rate_hz: REMOTE_AUDIO_SAMPLE_RATE_HZ,
        channels: REMOTE_AUDIO_CHANNELS,
        frame_samples: REMOTE_AUDIO_FRAME_SAMPLES,
    };
    if let Err(error) = network.send_personal_fabric_action(target, action) {
        if let Ok(mut state) = state().lock() {
            state.outbound.remove(&session_id);
            state.source_tokens.insert(source_token, selection);
        }
        return Err(error);
    }
    Ok(session_id)
}

/// Resolve one target-local consent row. Approval starts the local sink before
/// minting and sending the capability, so false readiness is impossible.
pub fn authorize(
    network: &ShellNetwork,
    session_id: [u8; 16],
    approved: bool,
) -> Result<(), String> {
    let pending = state()
        .lock()
        .map_err(|_| "remote audio service is unavailable".to_string())?
        .pending
        .remove(&session_id)
        .ok_or_else(|| "that remote audio request expired".to_string())?;
    let authorization = approved.then(random_nonzero_16);
    if let Some(authorization) = authorization {
        let sink = match PcmSink::start() {
            Ok(sink) => sink,
            Err(error) => {
                if let Ok(mut state) = state().lock() {
                    state.pending.insert(session_id, pending);
                }
                return Err(error);
            }
        };
        let mut state = state()
            .lock()
            .map_err(|_| "remote audio service is unavailable".to_string())?;
        if state.outbound.len() + state.inbound.len() >= MAX_REMOTE_AUDIO_SESSIONS {
            state.pending.insert(session_id, pending);
            return Err("too many remote audio routes are active".into());
        }
        state.inbound.insert(
            session_id,
            InboundRoute {
                source: pending.source.clone(),
                target: pending.target,
                label: pending.label,
                authorization,
                last_sequence: 0,
                created: Instant::now(),
                sink: Some(sink),
            },
        );
    }
    let result = network.send_personal_fabric_action(
        &pending.source,
        PersonalFabricAction::RemoteAudioDecision {
            session_id,
            accepted: approved,
            authorization,
        },
    );
    if result.is_err()
        && let Ok(mut state) = state().lock()
    {
        state.inbound.remove(&session_id);
    }
    result.map(|_| ())
}

/// Stop either direction locally and notify the exact peer when an issued
/// capability exists.
pub fn stop(network: &ShellNetwork, session_id: [u8; 16]) -> Result<(), String> {
    let stopped = {
        let mut state = state()
            .lock()
            .map_err(|_| "remote audio service is unavailable".to_string())?;
        if let Some(route) = state.outbound.remove(&session_id) {
            let action = route.authorization.map_or(
                PersonalFabricAction::RemoteAudioCancel { session_id },
                |authorization| PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization,
                },
            );
            Some((route.target, action))
        } else {
            state.inbound.remove(&session_id).map(|route| {
                (
                    route.source,
                    PersonalFabricAction::RemoteAudioStop {
                        session_id,
                        authorization: route.authorization,
                    },
                )
            })
        }
    };
    let Some((peer, action)) = stopped else {
        return Err("that remote audio route is no longer active".into());
    };
    network
        .send_personal_fabric_action(&peer, action)
        .map(|_| ())
}

/// Consume one already-authenticated fabric envelope. Returns true when it is
/// a remote-audio action, including rejected/stale actions.
pub fn handle_action(network: &ShellNetwork, envelope: &PersonalFabricActionEnvelope) -> bool {
    match &envelope.action {
        PersonalFabricAction::RemoteAudioOffer {
            session_id, label, ..
        } => {
            if let Ok(mut state) = state().lock()
                && state.pending.len() + state.inbound.len() + state.outbound.len()
                    < MAX_REMOTE_AUDIO_SESSIONS
                && !state.pending.contains_key(session_id)
                && !state.inbound.contains_key(session_id)
            {
                state.pending.insert(
                    *session_id,
                    PendingInbound {
                        source: envelope.source.clone(),
                        target: envelope.target.clone(),
                        label: label.clone(),
                        received: Instant::now(),
                    },
                );
            }
            true
        }
        PersonalFabricAction::RemoteAudioDecision {
            session_id,
            accepted,
            authorization,
        } => {
            let mut remove = false;
            if let Ok(mut state) = state().lock()
                && let Some(route) = state.outbound.get_mut(session_id)
                && route.target == envelope.source
                && route.authorization.is_none()
            {
                if *accepted {
                    if let Some(authorization) = authorization {
                        match PcmCapture::start(&route.selection) {
                            Ok(capture) => {
                                route.authorization = Some(*authorization);
                                route.capture = Some(capture);
                                route.created = Instant::now();
                            }
                            Err(_) => remove = true,
                        }
                    }
                } else {
                    remove = true;
                }
                if remove {
                    state.outbound.remove(session_id);
                }
            }
            if remove && let Some(authorization) = authorization {
                let _ = network.send_personal_fabric_action(
                    &envelope.source,
                    PersonalFabricAction::RemoteAudioStop {
                        session_id: *session_id,
                        authorization: *authorization,
                    },
                );
            }
            true
        }
        PersonalFabricAction::RemoteAudioFrame {
            session_id,
            authorization,
            sequence,
            pcm_s16le,
        } => {
            let mut failed = false;
            if let Ok(mut state) = state().lock() {
                if let Some(route) = state.inbound.get_mut(session_id)
                    && frame_matches(
                        &route.source,
                        &route.target,
                        route.authorization,
                        route.last_sequence,
                        &envelope.source,
                        &envelope.target,
                        *authorization,
                        *sequence,
                    )
                {
                    if route
                        .sink
                        .as_mut()
                        .is_some_and(|sink| sink.try_write(pcm_s16le.clone()).is_ok())
                    {
                        route.last_sequence = *sequence;
                    } else {
                        failed = true;
                    }
                }
                if failed {
                    state.inbound.remove(session_id);
                }
            }
            if failed {
                let _ = network.send_personal_fabric_action(
                    &envelope.source,
                    PersonalFabricAction::RemoteAudioStop {
                        session_id: *session_id,
                        authorization: *authorization,
                    },
                );
            }
            true
        }
        PersonalFabricAction::RemoteAudioStop {
            session_id,
            authorization,
        } => {
            if let Ok(mut state) = state().lock() {
                let remove_inbound = state.inbound.get(session_id).is_some_and(|route| {
                    route.source == envelope.source && route.authorization == *authorization
                });
                let remove_outbound = state.outbound.get(session_id).is_some_and(|route| {
                    route.target == envelope.source && route.authorization == Some(*authorization)
                });
                if remove_inbound {
                    state.inbound.remove(session_id);
                }
                if remove_outbound {
                    state.outbound.remove(session_id);
                }
            }
            true
        }
        PersonalFabricAction::RemoteAudioCancel { session_id } => {
            if let Ok(mut state) = state().lock() {
                let remove_pending = state
                    .pending
                    .get(session_id)
                    .is_some_and(|pending| pending.source == envelope.source);
                let remove_inbound = state
                    .inbound
                    .get(session_id)
                    .is_some_and(|route| route.source == envelope.source);
                if remove_pending {
                    state.pending.remove(session_id);
                }
                if remove_inbound {
                    state.inbound.remove(session_id);
                }
            }
            true
        }
        _ => false,
    }
}

/// Drive capture, bounded frame transmission, and all lifecycle revocations.
pub fn pump(network: &ShellNetwork, enabled: bool, focus: Option<&PhysicalGamepadBinding>) {
    let online = network
        .device_fabric()
        .online_devices()
        .map(|presence| presence.device.clone())
        .collect::<BTreeSet<_>>();
    let now = Instant::now();
    let mut egress = Vec::new();
    let Ok(mut guard) = state().lock() else {
        return;
    };
    egress.extend(reconcile_lifecycle(
        &mut guard, enabled, &online, focus, now,
    ));
    if !enabled {
        drop(guard);
        for message in egress {
            let _ = network.send_personal_fabric_action(&message.target, message.action);
        }
        return;
    }
    let sessions = guard.outbound.keys().copied().collect::<Vec<_>>();
    for session_id in sessions {
        let Some(route) = guard.outbound.get_mut(&session_id) else {
            continue;
        };
        let Some(capture) = route.capture.as_mut() else {
            continue;
        };
        match capture.drain() {
            Ok(bytes) => append_bounded_pcm(&mut route.pending_pcm, &bytes),
            Err(_) => {
                terminate_capture_failure(&mut guard, session_id, &mut egress);
                continue;
            }
        }
        for _ in 0..SEND_BUDGET {
            if route.pending_pcm.len() < MAX_REMOTE_AUDIO_FRAME_BYTES {
                break;
            }
            let tail = route.pending_pcm.split_off(MAX_REMOTE_AUDIO_FRAME_BYTES);
            let pcm_s16le = std::mem::replace(&mut route.pending_pcm, tail);
            route.sequence = route.sequence.saturating_add(1);
            let Some(authorization) = route.authorization else {
                break;
            };
            queue_egress(
                &mut egress,
                route.target.clone(),
                PersonalFabricAction::RemoteAudioFrame {
                    session_id,
                    authorization,
                    sequence: route.sequence,
                    pcm_s16le,
                },
                Some(session_id),
            );
        }
    }
    drop(guard);
    for message in egress {
        let outbound_session = message.outbound_session;
        if network
            .send_personal_fabric_action(&message.target, message.action)
            .is_err()
            && let Ok(mut state) = state().lock()
        {
            apply_send_failure(&mut state, outbound_session);
        }
    }
}

struct PcmCapture {
    child: Child,
    receiver: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
}

impl PcmCapture {
    fn start(selection: &LocalAudioSelection) -> Result<Self, String> {
        let target = match selection {
            LocalAudioSelection::System => media::audio::enumerate()
                .default_sink()
                .map(|sink| sink.serial)
                .ok_or_else(|| "no local PipeWire output is available".to_string())?,
            LocalAudioSelection::Application { serial, .. } => {
                let current = media::audio::enumerate()
                    .playback_streams
                    .into_iter()
                    .any(|stream| stream.serial == *serial);
                if !current {
                    return Err("that application audio stream ended".into());
                }
                *serial
            }
        };
        let properties = match selection {
            LocalAudioSelection::System => {
                "{ stream.capture.sink = true node.name = tilo-remote-audio-source }"
            }
            LocalAudioSelection::Application { .. } => {
                "{ node.name = tilo-remote-application-audio-source }"
            }
        };
        let mut child = Command::new("pw-record")
            .args(["--target", &target.to_string(), "--properties", properties])
            .args(raw_audio_args())
            .args(["--media-category", "Capture", "--raw", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start PipeWire remote audio capture: {error}"))?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("check PipeWire remote audio capture: {error}"))?
        {
            return Err(format!("PipeWire remote audio capture ended ({status})"));
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "PipeWire capture did not expose audio".to_string())?;
        let (sender, receiver) = sync_channel(CAPTURE_QUEUE);
        let reader = std::thread::Builder::new()
            .name("tilo-remote-audio-capture".into())
            .spawn(move || read_capture(stdout, &sender))
            .map_err(|error| format!("start remote audio reader: {error}"))?;
        Ok(Self {
            child,
            receiver,
            reader: Some(reader),
        })
    }

    fn drain(&mut self) -> Result<Vec<u8>, String> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("check remote audio capture: {error}"))?
        {
            return Err(format!("remote audio capture ended ({status})"));
        }
        let mut bytes = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(TryRecvError::Empty) => return Ok(bytes),
                Err(TryRecvError::Disconnected) => {
                    return Err("remote audio capture disconnected".into());
                }
            }
        }
    }
}

impl Drop for PcmCapture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct PcmSink {
    child: Child,
    sender: Option<SyncSender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
}

impl PcmSink {
    fn start() -> Result<Self, String> {
        let mut child = Command::new("pw-play")
            .args(raw_audio_args())
            .args(["--media-category", "Playback", "--raw", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start PipeWire remote audio sink: {error}"))?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("check PipeWire remote audio sink: {error}"))?
        {
            return Err(format!("PipeWire remote audio sink ended ({status})"));
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "PipeWire sink did not expose audio input".to_string())?;
        let (sender, receiver) = sync_channel(PLAYBACK_QUEUE);
        let writer = std::thread::Builder::new()
            .name("tilo-remote-audio-playback".into())
            .spawn(move || write_playback(stdin, &receiver))
            .map_err(|error| format!("start remote audio playback writer: {error}"))?;
        Ok(Self {
            child,
            sender: Some(sender),
            writer: Some(writer),
        })
    }

    fn try_write(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("check remote audio sink: {error}"))?
        {
            return Err(format!("remote audio sink ended ({status})"));
        }
        match self.sender.as_ref().map(|sender| sender.try_send(bytes)) {
            Some(Ok(())) | Some(Err(TrySendError::Full(_))) => Ok(()),
            Some(Err(TrySendError::Disconnected(_))) | None => {
                Err("remote audio sink disconnected".into())
            }
        }
    }
}

impl Drop for PcmSink {
    fn drop(&mut self) {
        self.sender.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn raw_audio_args() -> [&'static str; 12] {
    [
        "--media-type",
        "Audio",
        "--latency",
        "20ms",
        "--rate",
        "48000",
        "--channels",
        "2",
        "--channel-map",
        "Stereo",
        "--format",
        "s16",
    ]
}

fn read_capture(mut stdout: impl Read, sender: &SyncSender<Vec<u8>>) {
    let mut buffer = vec![0_u8; PIPE_BUFFER];
    while let Ok(read) = stdout.read(&mut buffer) {
        if read == 0 {
            break;
        }
        match sender.try_send(buffer[..read].to_vec()) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
}

fn write_playback(mut stdin: impl Write, receiver: &Receiver<Vec<u8>>) {
    while let Ok(bytes) = receiver.recv() {
        if stdin.write_all(&bytes).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(session: &str) -> PhysicalGamepadBinding {
        PhysicalGamepadBinding {
            app_id: "so.tilo.music".into(),
            workspace_id: "work".into(),
            session_id: session.into(),
        }
    }

    fn outbound(
        target: &DeviceId,
        selection: LocalAudioSelection,
        authorization: Option<[u8; 16]>,
        created: Instant,
    ) -> OutboundRoute {
        OutboundRoute {
            target: target.clone(),
            label: "Music".into(),
            selection,
            authorization,
            sequence: 0,
            created,
            capture: None,
            pending_pcm: Vec::new(),
        }
    }

    #[test]
    fn replay_and_foreign_capabilities_cannot_advance_an_inbound_route() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let authorization = [7; 16];
        assert!(frame_matches(
            &source,
            &target,
            authorization,
            8,
            &source,
            &target,
            authorization,
            9,
        ));
        assert!(!frame_matches(
            &source,
            &target,
            authorization,
            8,
            &source,
            &target,
            authorization,
            8,
        ));
        assert!(!frame_matches(
            &source,
            &target,
            authorization,
            8,
            &DeviceId::new("other").unwrap(),
            &target,
            authorization,
            9,
        ));
        assert!(!frame_matches(
            &source,
            &target,
            authorization,
            8,
            &source,
            &target,
            [9; 16],
            9,
        ));
    }

    #[test]
    fn application_routes_are_bound_to_the_locally_verified_focus() {
        let focus = binding("surface-1");
        let selection = LocalAudioSelection::Application {
            serial: 42,
            label: "Music".into(),
            focus: focus.clone(),
        };
        assert!(selection_focus_valid(&selection, Some(&focus)));
        let other = PhysicalGamepadBinding {
            session_id: "surface-2".into(),
            ..focus
        };
        assert!(!selection_focus_valid(&selection, Some(&other)));
        assert!(!selection_focus_valid(&selection, None));
    }

    #[test]
    fn picker_tokens_survive_refresh_only_while_the_source_binding_exists() {
        let focus = binding("surface-1");
        let application = LocalAudioSelection::Application {
            serial: 42,
            label: "Music".into(),
            focus,
        };
        let desired = vec![LocalAudioSelection::System, application.clone()];
        let mut state = RemoteAudioState::default();
        let first = reconcile_source_tokens(&mut state, desired.clone());
        let refreshed = reconcile_source_tokens(&mut state, desired);
        assert_eq!(first, refreshed);
        assert_eq!(state.source_tokens.len(), 2);

        let without_application =
            reconcile_source_tokens(&mut state, vec![LocalAudioSelection::System]);
        assert_eq!(without_application.len(), 1);
        assert_eq!(state.source_tokens.len(), 1);
        assert!(
            !state
                .source_tokens
                .values()
                .any(|source| source == &application)
        );
    }

    #[test]
    fn pending_pcm_keeps_only_the_newest_bounded_audio() {
        let mut pending = vec![1; MAX_PENDING_PCM_BYTES];
        append_bounded_pcm(&mut pending, &vec![2; MAX_PENDING_PCM_BYTES]);
        assert!(pending.len() <= MAX_PENDING_PCM_BYTES);
        assert!(pending.iter().all(|byte| *byte == 2));
        assert_eq!(pending.len() % 4, 0, "stereo S16LE stays sample-aligned");
    }

    #[test]
    fn policy_disable_revokes_pending_and_outbound_routes() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        state.pending.insert(
            [1; 16],
            PendingInbound {
                source: source.clone(),
                target: target.clone(),
                label: "Music".into(),
                received: now,
            },
        );
        state.outbound.insert(
            [2; 16],
            outbound(&target, LocalAudioSelection::System, Some([3; 16]), now),
        );
        state.inbound.insert(
            [4; 16],
            InboundRoute {
                source: source.clone(),
                target: target.clone(),
                label: "Podcast".into(),
                authorization: [5; 16],
                last_sequence: 7,
                created: now,
                sink: None,
            },
        );

        let egress = reconcile_lifecycle(&mut state, false, &BTreeSet::new(), None, now);
        assert!(state.pending.is_empty());
        assert!(state.outbound.is_empty());
        assert!(state.inbound.is_empty());
        assert_eq!(egress.len(), 3);
        assert!(egress.iter().any(|message| {
            message.target == source
                && message.action
                    == PersonalFabricAction::RemoteAudioDecision {
                        session_id: [1; 16],
                        accepted: false,
                        authorization: None,
                    }
        }));
        assert!(egress.iter().any(|message| {
            message.target == target
                && message.action
                    == PersonalFabricAction::RemoteAudioStop {
                        session_id: [2; 16],
                        authorization: [3; 16],
                    }
        }));
        assert!(egress.iter().any(|message| {
            message.target == source
                && message.action
                    == PersonalFabricAction::RemoteAudioStop {
                        session_id: [4; 16],
                        authorization: [5; 16],
                    }
        }));
    }

    #[test]
    fn peer_and_focus_loss_revoke_only_the_exact_routes() {
        let gone = DeviceId::new("gone").unwrap();
        let online_peer = DeviceId::new("online").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        state.outbound.insert(
            [1; 16],
            outbound(&gone, LocalAudioSelection::System, Some([7; 16]), now),
        );
        state.outbound.insert(
            [2; 16],
            outbound(
                &online_peer,
                LocalAudioSelection::Application {
                    serial: 42,
                    label: "Music".into(),
                    focus: binding("old-focus"),
                },
                None,
                now,
            ),
        );
        state.outbound.insert(
            [3; 16],
            outbound(&online_peer, LocalAudioSelection::System, None, now),
        );
        let online = BTreeSet::from([online_peer]);

        let egress =
            reconcile_lifecycle(&mut state, true, &online, Some(&binding("new-focus")), now);
        assert_eq!(
            state.outbound.keys().copied().collect::<Vec<_>>(),
            vec![[3; 16]]
        );
        assert_eq!(egress.len(), 2);
        assert!(
            egress
                .iter()
                .any(|message| message.outbound_session == Some([1; 16]))
        );
        assert!(
            egress
                .iter()
                .any(|message| message.outbound_session == Some([2; 16]))
        );
    }

    #[test]
    fn stale_consent_is_denied_and_fresh_consent_remains() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        for (id, received) in [
            ([1; 16], now - PENDING_TTL - Duration::from_millis(1)),
            ([2; 16], now),
        ] {
            state.pending.insert(
                id,
                PendingInbound {
                    source: source.clone(),
                    target: target.clone(),
                    label: "Music".into(),
                    received,
                },
            );
        }
        let egress = reconcile_lifecycle(&mut state, true, &BTreeSet::from([source]), None, now);
        assert_eq!(
            state.pending.keys().copied().collect::<Vec<_>>(),
            vec![[2; 16]]
        );
        assert_eq!(egress.len(), 1);
        assert_eq!(
            egress[0].action,
            PersonalFabricAction::RemoteAudioDecision {
                session_id: [1; 16],
                accepted: false,
                authorization: None,
            }
        );
    }

    #[test]
    fn send_failure_is_terminal_for_only_its_exact_route() {
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        state.outbound.insert(
            [1; 16],
            outbound(&target, LocalAudioSelection::System, Some([4; 16]), now),
        );
        state.outbound.insert(
            [2; 16],
            outbound(&target, LocalAudioSelection::System, Some([5; 16]), now),
        );

        apply_send_failure(&mut state, Some([1; 16]));
        assert!(!state.outbound.contains_key(&[1; 16]));
        assert!(state.outbound.contains_key(&[2; 16]));
    }

    #[test]
    fn capture_failure_stops_the_exact_authorized_peer_route() {
        let target = DeviceId::new("target").unwrap();
        let mut state = RemoteAudioState::default();
        state.outbound.insert(
            [1; 16],
            outbound(
                &target,
                LocalAudioSelection::System,
                Some([9; 16]),
                Instant::now(),
            ),
        );
        let mut egress = Vec::new();

        terminate_capture_failure(&mut state, [1; 16], &mut egress);

        assert!(state.outbound.is_empty());
        assert_eq!(egress.len(), 1);
        assert_eq!(egress[0].target, target);
        assert_eq!(egress[0].outbound_session, Some([1; 16]));
        assert_eq!(
            egress[0].action,
            PersonalFabricAction::RemoteAudioStop {
                session_id: [1; 16],
                authorization: [9; 16],
            }
        );
    }
}
