//! Capability-safe live audio routing between certified personal devices.
//!
//! `PipeWire` node identities never leave this process. The source shell resolves
//! a short-lived picker token to a freshly enumerated local stream, the target
//! shell asks for explicit consent, and approval mints a random capability
//! bound to the exact `(source, target, session)` tuple. PCM is deliberately a
//! small version-1 transport: 48 kHz stereo S16LE in bounded 20 ms frames.
//!
//! The transport is lossy by design: each frame travels on its own stream and
//! a slow send is dropped rather than retried. The receiver therefore plays
//! any newer frame, conceals short gaps with silence, and relies on
//! keepalives from both peers to tell a paused source from a dead link.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;

use crate::shell::device_fabric::{
    MAX_REMOTE_AUDIO_FRAME_BYTES, MAX_REMOTE_AUDIO_SESSIONS, PersonalFabricAction,
    PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS, REMOTE_AUDIO_FRAME_SAMPLES,
    REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_FRAME_BYTES, REMOTE_AUDIO_SAMPLE_RATE_HZ,
};
use crate::shell::network::{PersonalFabricSendError, ShellNetwork};
use crate::shell::personal_continuity::DeviceId;
use crate::shell::physical_gamepad::PhysicalGamepadBinding;

const PENDING_TTL: Duration = Duration::from_secs(30);
/// How long a source waits for the target's consent decision. Longer than
/// the target's own consent window so its decline, when it arrives, wins.
const CONSENT_WAIT_TTL: Duration = Duration::from_secs(60);
/// An approved route that carries no audio for this long expires, even while
/// both peers keep it alive with keepalives.
const IDLE_SESSION_TTL: Duration = Duration::from_secs(15 * 60);
/// An approved route whose peer sent nothing authenticated for this long has
/// lost its link and is interrupted.
const PEER_SILENCE_TIMEOUT: Duration = Duration::from_secs(3);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);
/// Lost frames are concealed with at most this many frames of silence
/// (100 ms); a longer outage resumes at the newest frame instead of adding
/// latency.
const MAX_CONCEALED_FRAMES: u64 = 5;
/// How long an interrupted route stays listed so the loss is visible.
const INTERRUPTED_NOTICE_TTL: Duration = Duration::from_secs(30);
/// How stale the cached application-stream list may get before the picker
/// refreshes it off the calling thread.
const AUDIO_GRAPH_REFRESH: Duration = Duration::from_secs(2);
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
    claimed_source: &DeviceId,
    claimed_target: &DeviceId,
    claimed_authorization: [u8; 16],
) -> bool {
    claimed_source == source && claimed_target == target && claimed_authorization == authorization
}

/// Decide whether a frame is playable after `last_sequence`. Returns how many
/// lost frames to conceal before it, or `None` for a duplicate or a frame
/// that arrived after a newer one.
fn admit_sequence(last_sequence: u64, sequence: u64) -> Option<u64> {
    (sequence > last_sequence).then(|| {
        sequence
            .saturating_sub(last_sequence)
            .saturating_sub(1)
            .min(MAX_CONCEALED_FRAMES)
    })
}

/// Prefix one received frame with silence standing in for lost frames.
fn with_concealment(concealed_frames: u64, pcm_s16le: &[u8]) -> Vec<u8> {
    let silence = usize::try_from(concealed_frames)
        .unwrap_or(0)
        .saturating_mul(MAX_REMOTE_AUDIO_FRAME_BYTES);
    let mut pcm = vec![0; silence];
    pcm.extend_from_slice(pcm_s16le);
    pcm
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
    /// The link or a local audio endpoint failed and the route stopped. The
    /// row stays briefly so the loss is visible; stopping it dismisses it.
    Interrupted,
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
    /// When the request was sent, then when the last frame was sent.
    last_audio: Instant,
    /// When the target last sent an authenticated keepalive.
    last_heard: Instant,
    last_keepalive: Instant,
    capture: Option<PcmCapture>,
    pending_pcm: Vec<u8>,
}

struct InboundRoute {
    source: DeviceId,
    target: DeviceId,
    label: String,
    authorization: [u8; 16],
    last_sequence: u64,
    /// When the route was approved, then when the last frame was played.
    last_audio: Instant,
    /// When the source last sent any authenticated frame or keepalive.
    last_heard: Instant,
    last_keepalive: Instant,
    sink: Option<PcmSink>,
}

struct InterruptedRoute {
    peer: DeviceId,
    label: String,
    inbound: bool,
    at: Instant,
}

#[derive(Default)]
struct RemoteAudioState {
    source_tokens: BTreeMap<u64, LocalAudioSelection>,
    pending: BTreeMap<[u8; 16], PendingInbound>,
    outbound: BTreeMap<[u8; 16], OutboundRoute>,
    inbound: BTreeMap<[u8; 16], InboundRoute>,
    interrupted: BTreeMap<[u8; 16], InterruptedRoute>,
}

fn record_interruption(
    state: &mut RemoteAudioState,
    session_id: [u8; 16],
    route: InterruptedRoute,
) {
    while state.interrupted.len() >= MAX_REMOTE_AUDIO_SESSIONS {
        let Some(oldest) = state
            .interrupted
            .iter()
            .min_by_key(|(_, notice)| notice.at)
            .map(|(id, _)| *id)
        else {
            break;
        };
        state.interrupted.remove(&oldest);
    }
    state.interrupted.insert(session_id, route);
}

/// Why lifecycle reconciliation ends an active route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteEnd {
    /// Policy, focus, consent timeout or idleness: an orderly stop.
    Expired,
    /// The peer went away or fell silent while the route was approved.
    Interrupted,
}

fn inbound_end(
    route: &InboundRoute,
    online: &BTreeSet<DeviceId>,
    now: Instant,
) -> Option<RouteEnd> {
    if !online.contains(&route.source)
        || now.duration_since(route.last_heard) > PEER_SILENCE_TIMEOUT
    {
        Some(RouteEnd::Interrupted)
    } else if now.duration_since(route.last_audio) > IDLE_SESSION_TTL {
        Some(RouteEnd::Expired)
    } else {
        None
    }
}

fn outbound_end(
    route: &OutboundRoute,
    online: &BTreeSet<DeviceId>,
    focus: Option<&PhysicalGamepadBinding>,
    now: Instant,
) -> Option<RouteEnd> {
    let online = online.contains(&route.target);
    if route.authorization.is_none() {
        return (!online
            || now.duration_since(route.last_audio) > CONSENT_WAIT_TTL
            || !selection_focus_valid(&route.selection, focus))
        .then_some(RouteEnd::Expired);
    }
    if !online || now.duration_since(route.last_heard) > PEER_SILENCE_TIMEOUT {
        Some(RouteEnd::Interrupted)
    } else if now.duration_since(route.last_audio) > IDLE_SESSION_TTL
        || !selection_focus_valid(&route.selection, focus)
    {
        Some(RouteEnd::Expired)
    } else {
        None
    }
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
        let excess = pending.len().saturating_sub(MAX_PENDING_PCM_BYTES);
        // Stereo S16LE is four bytes per sample frame. Drop old audio at a
        // sample boundary: live routing favours bounded latency over replaying
        // a growing backlog after transport backpressure clears. A rounded-up
        // count past usize::MAX still clamps to dropping the whole buffer.
        let discard = excess.checked_next_multiple_of(4).unwrap_or(usize::MAX);
        pending.drain(..discard.min(pending.len()));
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one reconciliation applies policy, presence and focus teardown to every route kind"
)]
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
        state.interrupted.clear();
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
            inbound_end(route, online, now).map(|end| (*session_id, end))
        })
        .collect::<Vec<_>>();
    for (session_id, end) in stale_inbound {
        if let Some(route) = state.inbound.remove(&session_id) {
            queue_egress(
                &mut egress,
                route.source.clone(),
                PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization: route.authorization,
                },
                None,
            );
            if end == RouteEnd::Interrupted {
                record_interruption(
                    state,
                    session_id,
                    InterruptedRoute {
                        peer: route.source,
                        label: route.label,
                        inbound: true,
                        at: now,
                    },
                );
            }
        }
    }

    let stale_outbound = state
        .outbound
        .iter()
        .filter_map(|(session_id, route)| {
            outbound_end(route, online, focus, now).map(|end| (*session_id, end))
        })
        .collect::<Vec<_>>();
    for (session_id, end) in stale_outbound {
        if let Some(route) = state.outbound.remove(&session_id) {
            let action = route.authorization.map_or(
                PersonalFabricAction::RemoteAudioCancel { session_id },
                |authorization| PersonalFabricAction::RemoteAudioStop {
                    session_id,
                    authorization,
                },
            );
            queue_egress(&mut egress, route.target.clone(), action, Some(session_id));
            if end == RouteEnd::Interrupted {
                record_interruption(
                    state,
                    session_id,
                    InterruptedRoute {
                        peer: route.target,
                        label: route.label,
                        inbound: false,
                        at: now,
                    },
                );
            }
        }
    }

    state
        .interrupted
        .retain(|_, notice| now.duration_since(notice.at) <= INTERRUPTED_NOTICE_TTL);
    egress
}

/// Queue keepalives for every approved route whose interval elapsed, in both
/// directions, so each peer can tell a paused route from a lost link.
fn due_keepalives(state: &mut RemoteAudioState, now: Instant) -> Vec<Egress> {
    let mut egress = Vec::new();
    for (session_id, route) in &mut state.outbound {
        let Some(authorization) = route.authorization else {
            continue;
        };
        if now.duration_since(route.last_keepalive) >= KEEPALIVE_INTERVAL {
            route.last_keepalive = now;
            queue_egress(
                &mut egress,
                route.target.clone(),
                PersonalFabricAction::RemoteAudioKeepalive {
                    session_id: *session_id,
                    authorization,
                },
                Some(*session_id),
            );
        }
    }
    for (session_id, route) in &mut state.inbound {
        if now.duration_since(route.last_keepalive) >= KEEPALIVE_INTERVAL {
            route.last_keepalive = now;
            queue_egress(
                &mut egress,
                route.source.clone(),
                PersonalFabricAction::RemoteAudioKeepalive {
                    session_id: *session_id,
                    authorization: route.authorization,
                },
                None,
            );
        }
    }
    egress
}

/// A full transport queue drops only the message that met it: live media
/// favours the next frame over a torn-down route. Any other failure cannot
/// succeed on retry and interrupts the exact route that sent it.
fn apply_send_failure(
    state: &mut RemoteAudioState,
    outbound_session: Option<[u8; 16]>,
    error: &PersonalFabricSendError,
    now: Instant,
) {
    if matches!(error, PersonalFabricSendError::Backpressure) {
        return;
    }
    if let Some(session_id) = outbound_session
        && let Some(route) = state.outbound.remove(&session_id)
        && route.authorization.is_some()
    {
        record_interruption(
            state,
            session_id,
            InterruptedRoute {
                peer: route.target,
                label: route.label,
                inbound: false,
                at: now,
            },
        );
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
        UnwrapErr(SysRng).fill_bytes(&mut value);
        if value != [0; 16] {
            return value;
        }
    }
}

fn random_nonzero_u64(existing: &BTreeMap<u64, LocalAudioSelection>) -> u64 {
    loop {
        let value = UnwrapErr(SysRng).next_u64();
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

#[derive(Default)]
struct PlaybackStreamCache {
    /// `(serial, label)` of each application playback stream, capped to the
    /// picker's size, with when it was enumerated.
    streams: Option<(Instant, Vec<(u32, String)>)>,
    refreshing: bool,
}

fn playback_stream_cache() -> &'static Mutex<PlaybackStreamCache> {
    static CACHE: OnceLock<Mutex<PlaybackStreamCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(PlaybackStreamCache::default()))
}

/// The most recently enumerated application playback streams. Enumeration
/// spawns `pw-dump`, so a stale list is refreshed on a worker thread and the
/// caller — the compositor thread when Quick Settings polls — never waits.
fn cached_playback_streams(now: Instant) -> Vec<(u32, String)> {
    let Ok(mut cache) = playback_stream_cache().lock() else {
        return Vec::new();
    };
    let stale = cache
        .streams
        .as_ref()
        .is_none_or(|(at, _)| now.duration_since(*at) >= AUDIO_GRAPH_REFRESH);
    if stale && !cache.refreshing {
        let spawned = std::thread::Builder::new()
            .name("tilo-remote-audio-sources".into())
            .spawn(|| {
                let streams = media::audio::enumerate()
                    .playback_streams
                    .into_iter()
                    .take(64)
                    .map(|stream| (stream.serial, stream.label()))
                    .collect();
                if let Ok(mut cache) = playback_stream_cache().lock() {
                    cache.streams = Some((Instant::now(), streams));
                    cache.refreshing = false;
                }
            });
        cache.refreshing = spawned.is_ok();
    }
    cache
        .streams
        .as_ref()
        .map_or_else(Vec::new, |(_, streams)| streams.clone())
}

fn route_views(state: &RemoteAudioState, include_interrupted: bool) -> Vec<RemoteAudioRouteView> {
    let outbound = state
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
        });
    let inbound = state
        .inbound
        .iter()
        .map(|(session_id, route)| RemoteAudioRouteView {
            session_id: *session_id,
            peer: route.source.clone(),
            label: route.label.clone(),
            inbound: true,
            phase: RemoteAudioPhase::Streaming,
        });
    let interrupted = state
        .interrupted
        .iter()
        .filter(|_| include_interrupted)
        .map(|(session_id, notice)| RemoteAudioRouteView {
            session_id: *session_id,
            peer: notice.peer.clone(),
            label: notice.label.clone(),
            inbound: notice.inbound,
            phase: RemoteAudioPhase::Interrupted,
        });
    let mut routes = outbound
        .chain(inbound)
        .chain(interrupted)
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| left.label.cmp(&right.label));
    routes
}

/// Active routes only, without touching the source picker.
pub fn active_routes() -> Vec<RemoteAudioRouteView> {
    state()
        .lock()
        .map_or_else(|_| Vec::new(), |state| route_views(&state, false))
}

/// Refresh the shell-owned source picker. Tokens are process-local and are
/// retained only while their enumerated source and focus binding stay
/// available, so neither apps nor peers ever see node ids. Starting a route
/// re-checks the stream against a fresh enumeration.
pub fn snapshot(focus: Option<&PhysicalGamepadBinding>) -> RemoteAudioView {
    let streams = if focus.is_some() {
        cached_playback_streams(Instant::now())
    } else {
        Vec::new()
    };
    let Ok(mut state) = state().lock() else {
        return RemoteAudioView::default();
    };
    let mut desired = vec![LocalAudioSelection::System];
    if let Some(focus) = focus {
        for (serial, label) in streams {
            let selection = LocalAudioSelection::Application {
                serial,
                label,
                focus: focus.clone(),
            };
            if !desired.contains(&selection) {
                desired.push(selection);
            }
        }
    }
    let sources = reconcile_source_tokens(&mut state, desired);
    let routes = route_views(&state, true);
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
///
/// # Errors
///
/// Returns a message when this device has no certified identity, `target` is
/// not an online enrolled audio output, too many routes are active, the
/// source picker entry expired, the service is unavailable, or the offer
/// cannot be sent.
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
        if state
            .outbound
            .len()
            .saturating_add(state.inbound.len())
            .saturating_add(state.pending.len())
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
        let now = Instant::now();
        state.outbound.insert(
            session_id,
            OutboundRoute {
                target: target.clone(),
                label: label.clone(),
                selection: selection.clone(),
                authorization: None,
                sequence: 0,
                last_audio: now,
                last_heard: now,
                last_keepalive: now,
                capture: None,
                pending_pcm: Vec::new(),
            },
        );
        drop(state);
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
///
/// # Errors
///
/// Returns a message when the service is unavailable, the request expired,
/// the local sink cannot start, too many routes are active, or the decision
/// cannot be sent.
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
        if state.outbound.len().saturating_add(state.inbound.len()) >= MAX_REMOTE_AUDIO_SESSIONS {
            state.pending.insert(session_id, pending);
            return Err("too many remote audio routes are active".into());
        }
        let now = Instant::now();
        state.inbound.insert(
            session_id,
            InboundRoute {
                source: pending.source.clone(),
                target: pending.target,
                label: pending.label,
                authorization,
                last_sequence: 0,
                last_audio: now,
                last_heard: now,
                last_keepalive: now,
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
///
/// # Errors
///
/// Returns a message when the service is unavailable, the route is no longer
/// active, or the peer cannot be notified. Stopping an interrupted route only
/// dismisses its notice; its peer was already told when it ended.
pub fn stop(network: &ShellNetwork, session_id: [u8; 16]) -> Result<(), String> {
    let stopped = {
        let mut state = state()
            .lock()
            .map_err(|_| "remote audio service is unavailable".to_string())?;
        if state.interrupted.remove(&session_id).is_some() {
            return Ok(());
        }
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
#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive handler validates each remote-audio action beside its state change"
)]
pub fn handle_action(network: &ShellNetwork, envelope: &PersonalFabricActionEnvelope) -> bool {
    match &envelope.action {
        PersonalFabricAction::RemoteAudioOffer {
            session_id, label, ..
        } => {
            if let Ok(mut state) = state().lock()
                && state
                    .pending
                    .len()
                    .saturating_add(state.inbound.len())
                    .saturating_add(state.outbound.len())
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
                                let now = Instant::now();
                                route.authorization = Some(*authorization);
                                route.capture = Some(capture);
                                route.last_audio = now;
                                route.last_heard = now;
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
                        &envelope.source,
                        &envelope.target,
                        *authorization,
                    )
                {
                    let now = Instant::now();
                    route.last_heard = now;
                    // Frames travel on independent streams and a slow send
                    // is dropped, so play any newer frame, conceal the gap,
                    // and ignore duplicates or frames overtaken in flight.
                    if let Some(concealed) = admit_sequence(route.last_sequence, *sequence) {
                        let pcm = with_concealment(concealed, pcm_s16le);
                        if route
                            .sink
                            .as_mut()
                            .is_some_and(|sink| sink.try_write(pcm).is_ok())
                        {
                            route.last_sequence = *sequence;
                            route.last_audio = now;
                        } else {
                            failed = true;
                        }
                    }
                }
                if failed && let Some(route) = state.inbound.remove(session_id) {
                    record_interruption(
                        &mut state,
                        *session_id,
                        InterruptedRoute {
                            peer: route.source,
                            label: route.label,
                            inbound: true,
                            at: Instant::now(),
                        },
                    );
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
                // A route still awaiting consent accepts the target's stop
                // too: the target minted the capability, and its decision
                // may have been lost on the way here.
                let remove_outbound = state.outbound.get(session_id).is_some_and(|route| {
                    route.target == envelope.source
                        && route
                            .authorization
                            .is_none_or(|issued| issued == *authorization)
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
        PersonalFabricAction::RemoteAudioKeepalive {
            session_id,
            authorization,
        } => {
            if let Ok(mut state) = state().lock() {
                let now = Instant::now();
                if let Some(route) = state.inbound.get_mut(session_id)
                    && frame_matches(
                        &route.source,
                        &route.target,
                        route.authorization,
                        &envelope.source,
                        &envelope.target,
                        *authorization,
                    )
                {
                    route.last_heard = now;
                }
                if let Some(route) = state.outbound.get_mut(session_id)
                    && route.target == envelope.source
                    && route.authorization == Some(*authorization)
                {
                    route.last_heard = now;
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
        if let Ok(bytes) = capture.drain() {
            append_bounded_pcm(&mut route.pending_pcm, &bytes);
        } else {
            terminate_capture_failure(&mut guard, session_id, &mut egress);
            continue;
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
            route.last_audio = now;
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
    egress.extend(due_keepalives(&mut guard, now));
    drop(guard);
    for message in egress {
        let outbound_session = message.outbound_session;
        if let Err(error) = network.try_send_personal_fabric_action(&message.target, message.action)
            && let Ok(mut state) = state().lock()
        {
            apply_send_failure(&mut state, outbound_session, &error, Instant::now());
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
            Some(Ok(()) | Err(TrySendError::Full(_))) => Ok(()),
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

/// Split off every whole stereo S16LE sample frame, leaving a torn trailing
/// frame in `pending` for the next read to complete.
fn take_whole_sample_frames(pending: &mut Vec<u8>) -> Vec<u8> {
    let torn = pending
        .len()
        .checked_rem(REMOTE_AUDIO_SAMPLE_FRAME_BYTES)
        .unwrap_or(0);
    let whole = pending.len().saturating_sub(torn);
    let tail = pending.split_off(whole);
    std::mem::replace(pending, tail)
}

/// Forward capture in whole sample frames only. A pipe read can end mid
/// frame, and dropping such a chunk under backpressure would shift every
/// later sample by a byte or a channel; whole-frame chunks can be dropped
/// without misaligning the stream.
fn read_capture(mut stdout: impl Read, sender: &SyncSender<Vec<u8>>) {
    let mut buffer = vec![0_u8; PIPE_BUFFER];
    let mut pending = Vec::with_capacity(PIPE_BUFFER + REMOTE_AUDIO_SAMPLE_FRAME_BYTES);
    loop {
        let read = match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        #[expect(
            clippy::indexing_slicing,
            reason = "Read::read never reports more bytes than the buffer it was given"
        )]
        pending.extend_from_slice(&buffer[..read]);
        let chunk = take_whole_sample_frames(&mut pending);
        if chunk.is_empty() {
            continue;
        }
        match sender.try_send(chunk) {
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
        at: Instant,
    ) -> OutboundRoute {
        OutboundRoute {
            target: target.clone(),
            label: "Music".into(),
            selection,
            authorization,
            sequence: 0,
            last_audio: at,
            last_heard: at,
            last_keepalive: at,
            capture: None,
            pending_pcm: Vec::new(),
        }
    }

    fn inbound(source: &DeviceId, target: &DeviceId, at: Instant) -> InboundRoute {
        InboundRoute {
            source: source.clone(),
            target: target.clone(),
            label: "Podcast".into(),
            authorization: [5; 16],
            last_sequence: 7,
            last_audio: at,
            last_heard: at,
            last_keepalive: at,
            sink: None,
        }
    }

    fn before(now: Instant, elapsed: Duration) -> Instant {
        now.checked_sub(elapsed).unwrap()
    }

    #[test]
    fn foreign_capabilities_cannot_feed_an_inbound_route() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let authorization = [7; 16];
        assert!(frame_matches(
            &source,
            &target,
            authorization,
            &source,
            &target,
            authorization,
        ));
        assert!(!frame_matches(
            &source,
            &target,
            authorization,
            &DeviceId::new("other").unwrap(),
            &target,
            authorization,
        ));
        assert!(!frame_matches(
            &source,
            &target,
            authorization,
            &source,
            &DeviceId::new("other").unwrap(),
            authorization,
        ));
        assert!(!frame_matches(
            &source,
            &target,
            authorization,
            &source,
            &target,
            [9; 16],
        ));
    }

    #[test]
    fn the_next_frame_plays_without_concealment() {
        assert_eq!(admit_sequence(0, 1), Some(0));
        assert_eq!(admit_sequence(8, 9), Some(0));
    }

    #[test]
    fn duplicate_and_overtaken_frames_are_dropped() {
        assert_eq!(admit_sequence(8, 8), None, "duplicate");
        assert_eq!(admit_sequence(8, 7), None, "arrived after a newer frame");
        assert_eq!(admit_sequence(u64::MAX, 1), None);
    }

    #[test]
    fn a_lost_frame_is_concealed_and_the_route_keeps_playing() {
        // Frame 9 was lost; 10 still plays, preceded by one frame of silence.
        let concealed = admit_sequence(8, 10).unwrap();
        assert_eq!(concealed, 1);
        let frame = vec![1; MAX_REMOTE_AUDIO_FRAME_BYTES];
        let pcm = with_concealment(concealed, &frame);
        assert_eq!(pcm.len(), 2 * MAX_REMOTE_AUDIO_FRAME_BYTES);
        assert!(
            pcm[..MAX_REMOTE_AUDIO_FRAME_BYTES]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(
            pcm[MAX_REMOTE_AUDIO_FRAME_BYTES..]
                .iter()
                .all(|byte| *byte == 1)
        );
    }

    #[test]
    fn a_long_outage_conceals_only_a_bounded_amount_of_silence() {
        let concealed = admit_sequence(8, 1_008).unwrap();
        assert_eq!(concealed, MAX_CONCEALED_FRAMES);
        let pcm = with_concealment(concealed, &[1; 4]);
        assert_eq!(
            pcm.len(),
            5 * MAX_REMOTE_AUDIO_FRAME_BYTES + 4,
            "100 ms of silence at most"
        );
    }

    #[test]
    fn reordered_frames_play_the_newest_and_drop_the_straggler() {
        let mut last = 8;
        let mut played = Vec::new();
        for sequence in [10, 9, 11] {
            if admit_sequence(last, sequence).is_some() {
                last = sequence;
                played.push(sequence);
            }
        }
        assert_eq!(played, vec![10, 11]);
    }

    #[test]
    fn a_silent_inbound_link_is_interrupted_and_surfaced() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        let mut route = inbound(&source, &target, now);
        route.last_heard = before(
            now,
            PEER_SILENCE_TIMEOUT.saturating_add(Duration::from_millis(1)),
        );
        state.inbound.insert([4; 16], route);
        state
            .inbound
            .insert([6; 16], inbound(&source, &target, now));

        let egress = reconcile_lifecycle(
            &mut state,
            true,
            &BTreeSet::from([source.clone()]),
            None,
            now,
        );

        assert_eq!(
            state.inbound.keys().copied().collect::<Vec<_>>(),
            vec![[6; 16]]
        );
        assert_eq!(egress.len(), 1);
        assert_eq!(
            egress[0].action,
            PersonalFabricAction::RemoteAudioStop {
                session_id: [4; 16],
                authorization: [5; 16],
            }
        );
        let routes = route_views(&state, true);
        let interrupted = routes
            .iter()
            .find(|route| route.session_id == [4; 16])
            .unwrap();
        assert_eq!(interrupted.phase, RemoteAudioPhase::Interrupted);
        assert!(interrupted.inbound);
        assert_eq!(interrupted.peer, source);
        assert!(
            route_views(&state, false)
                .iter()
                .all(|route| route.phase != RemoteAudioPhase::Interrupted),
            "active route counts exclude interrupted notices"
        );
    }

    #[test]
    fn a_paused_but_alive_route_is_neither_interrupted_nor_expired() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        let mut route = inbound(&source, &target, now);
        // No audio for a minute, but the source's keepalives keep arriving.
        route.last_audio = before(now, Duration::from_secs(60));
        state.inbound.insert([4; 16], route);
        let mut outgoing = outbound(&target, LocalAudioSelection::System, Some([9; 16]), now);
        outgoing.last_audio = before(now, Duration::from_secs(60));
        state.outbound.insert([7; 16], outgoing);

        let egress = reconcile_lifecycle(
            &mut state,
            true,
            &BTreeSet::from([source, target]),
            None,
            now,
        );
        assert!(egress.is_empty());
        assert!(state.inbound.contains_key(&[4; 16]));
        assert!(state.outbound.contains_key(&[7; 16]));
        assert!(state.interrupted.is_empty());
    }

    #[test]
    fn active_routes_outlive_the_idle_window_while_audio_flows() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let approved_long_ago = before(
            now,
            IDLE_SESSION_TTL.saturating_add(Duration::from_secs(60)),
        );
        let mut state = RemoteAudioState::default();
        let mut streaming = inbound(&source, &target, approved_long_ago);
        streaming.last_audio = now;
        streaming.last_heard = now;
        state.inbound.insert([1; 16], streaming);
        let mut idle = inbound(&source, &target, approved_long_ago);
        idle.last_heard = now;
        state.inbound.insert([2; 16], idle);

        let egress = reconcile_lifecycle(&mut state, true, &BTreeSet::from([source]), None, now);
        assert_eq!(
            state.inbound.keys().copied().collect::<Vec<_>>(),
            vec![[1; 16]]
        );
        assert_eq!(egress.len(), 1, "the idle route still expires");
        assert!(
            state.interrupted.is_empty(),
            "expiry is not an interruption"
        );
    }

    #[test]
    fn a_source_without_target_keepalives_is_interrupted() {
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        let mut route = outbound(&target, LocalAudioSelection::System, Some([9; 16]), now);
        route.last_heard = before(
            now,
            PEER_SILENCE_TIMEOUT.saturating_add(Duration::from_millis(1)),
        );
        state.outbound.insert([1; 16], route);
        // Awaiting consent has no keepalives yet and must not time out early.
        let mut awaiting = outbound(&target, LocalAudioSelection::System, None, now);
        awaiting.last_heard = before(now, Duration::from_secs(10));
        state.outbound.insert([2; 16], awaiting);

        let egress = reconcile_lifecycle(&mut state, true, &BTreeSet::from([target]), None, now);
        assert_eq!(
            state.outbound.keys().copied().collect::<Vec<_>>(),
            vec![[2; 16]]
        );
        assert_eq!(egress.len(), 1);
        assert_eq!(
            state.interrupted.get(&[1; 16]).map(|notice| notice.inbound),
            Some(false)
        );
    }

    #[test]
    fn interrupted_notices_expire_and_stay_bounded() {
        let peer = DeviceId::new("peer").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        let notice = |age_secs: u64| InterruptedRoute {
            peer: peer.clone(),
            label: "Music".into(),
            inbound: true,
            at: before(now, Duration::from_secs(age_secs)),
        };
        for (id, age) in [(1, 20), (2, 19), (3, 18), (4, 17), (5, 16)] {
            record_interruption(&mut state, [id; 16], notice(age));
        }
        assert_eq!(state.interrupted.len(), MAX_REMOTE_AUDIO_SESSIONS);
        assert!(
            !state.interrupted.contains_key(&[1; 16]),
            "the oldest notice made room"
        );

        record_interruption(&mut state, [9; 16], notice(0));
        let later = now.checked_add(Duration::from_secs(20)).unwrap();
        reconcile_lifecycle(&mut state, true, &BTreeSet::new(), None, later);
        assert_eq!(
            state.interrupted.keys().copied().collect::<Vec<_>>(),
            vec![[9; 16]],
            "only notices inside the window remain"
        );
        reconcile_lifecycle(&mut state, false, &BTreeSet::new(), None, later);
        assert!(state.interrupted.is_empty(), "disabling clears notices");
    }

    #[test]
    fn keepalives_flow_both_ways_once_per_interval() {
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let due = before(now, KEEPALIVE_INTERVAL);
        let mut state = RemoteAudioState::default();
        state.outbound.insert(
            [1; 16],
            outbound(&target, LocalAudioSelection::System, Some([9; 16]), due),
        );
        state.outbound.insert(
            [2; 16],
            outbound(&target, LocalAudioSelection::System, None, due),
        );
        state
            .inbound
            .insert([3; 16], inbound(&source, &target, due));

        let egress = due_keepalives(&mut state, now);
        assert_eq!(egress.len(), 2, "no keepalive before consent");
        assert!(egress.iter().any(|message| {
            message.target == target
                && message.outbound_session == Some([1; 16])
                && message.action
                    == PersonalFabricAction::RemoteAudioKeepalive {
                        session_id: [1; 16],
                        authorization: [9; 16],
                    }
        }));
        assert!(egress.iter().any(|message| {
            message.target == source
                && message.action
                    == PersonalFabricAction::RemoteAudioKeepalive {
                        session_id: [3; 16],
                        authorization: [5; 16],
                    }
        }));
        assert!(due_keepalives(&mut state, now).is_empty());
    }

    #[test]
    fn a_full_transport_queue_drops_the_frame_but_keeps_the_route() {
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        state.outbound.insert(
            [1; 16],
            outbound(&target, LocalAudioSelection::System, Some([4; 16]), now),
        );

        apply_send_failure(
            &mut state,
            Some([1; 16]),
            &PersonalFabricSendError::Backpressure,
            now,
        );
        assert!(state.outbound.contains_key(&[1; 16]));
        assert!(state.interrupted.is_empty());
    }

    #[test]
    fn capture_reads_forward_only_whole_sample_frames() {
        struct TornReads {
            chunks: Vec<Vec<u8>>,
        }
        impl Read for TornReads {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.chunks.is_empty() {
                    return Ok(0);
                }
                let chunk = self.chunks.remove(0);
                buffer[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }
        let stream = (0..40_u8).collect::<Vec<_>>();
        let reads = TornReads {
            chunks: vec![
                stream[..3].to_vec(),
                stream[3..4].to_vec(),
                stream[4..11].to_vec(),
                stream[11..13].to_vec(),
                stream[13..40].to_vec(),
            ],
        };
        let (sender, receiver) = sync_channel(16);
        read_capture(reads, &sender);
        drop(sender);
        let chunks = receiver.iter().collect::<Vec<_>>();
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() % 4 == 0 && !chunk.is_empty())
        );
        assert_eq!(chunks.concat(), stream);
    }

    #[test]
    fn capture_backpressure_drops_whole_frames_and_stays_aligned() {
        struct OddReads {
            remaining: usize,
            next: u8,
        }
        impl Read for OddReads {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let len = self.remaining.min(7);
                for byte in &mut buffer[..len] {
                    *byte = self.next;
                    self.next = self.next.wrapping_add(1);
                }
                self.remaining = self.remaining.saturating_sub(len);
                Ok(len)
            }
        }
        // A one-slot queue that is never drained keeps only the first chunk
        // and drops the rest; what survives still starts on a frame boundary.
        let (sender, receiver) = sync_channel(1);
        read_capture(
            OddReads {
                remaining: 7 * 20,
                next: 0,
            },
            &sender,
        );
        let kept = receiver.try_recv().unwrap();
        assert_eq!(kept, vec![0, 1, 2, 3]);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn whole_sample_frames_carry_the_torn_remainder() {
        let mut pending = vec![1, 2, 3, 4, 5, 6];
        assert_eq!(take_whole_sample_frames(&mut pending), vec![1, 2, 3, 4]);
        assert_eq!(pending, vec![5, 6]);
        pending.extend_from_slice(&[7, 8]);
        assert_eq!(take_whole_sample_frames(&mut pending), vec![5, 6, 7, 8]);
        assert!(pending.is_empty());
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
    fn pending_pcm_rounds_partial_overflow_up_to_a_sample_frame() {
        let mut at_limit = vec![1; MAX_PENDING_PCM_BYTES];
        append_bounded_pcm(&mut at_limit, &[]);
        assert_eq!(
            at_limit.len(),
            MAX_PENDING_PCM_BYTES,
            "at the limit keeps everything"
        );

        // Five bytes over the limit drops two whole sample frames (eight bytes).
        let mut over = vec![1; MAX_PENDING_PCM_BYTES];
        append_bounded_pcm(&mut over, &[2; 5]);
        assert_eq!(over.len(), MAX_PENDING_PCM_BYTES - 3);
        assert_eq!(over.last(), Some(&2));
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
        state
            .inbound
            .insert([4; 16], inbound(&source, &target, now));

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
            (
                [1; 16],
                now.checked_sub(PENDING_TTL)
                    .and_then(|stale| stale.checked_sub(Duration::from_millis(1)))
                    .unwrap(),
            ),
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
    fn transport_failure_interrupts_only_its_exact_route() {
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

        apply_send_failure(
            &mut state,
            Some([1; 16]),
            &PersonalFabricSendError::Rejected("personal fabric transport is closed".into()),
            now,
        );
        assert!(!state.outbound.contains_key(&[1; 16]));
        assert!(state.outbound.contains_key(&[2; 16]));
        assert_eq!(
            route_views(&state, true)
                .iter()
                .find(|route| route.session_id == [1; 16])
                .map(|route| route.phase),
            Some(RemoteAudioPhase::Interrupted)
        );
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
