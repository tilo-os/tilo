//! Capability-safe live audio routing between certified personal devices.
//!
//! `PipeWire` node identities never leave this process. The source shell resolves
//! a short-lived picker token to a freshly enumerated local stream, the target
//! shell asks for explicit consent, and approval mints a random capability
//! bound to the exact `(source, target, session)` tuple. PCM is deliberately a
//! small transport: 48 kHz stereo S16LE in bounded 20 ms frames.
//!
//! Every frame carries its media timestamp (the index of its first sample on
//! the source's monotonic clock) and the source-clock time at which that
//! sample plays. The source is the group leader: every target that receives
//! the same stream plays each sample at that one instant, converted to its own
//! clock through the device fabric's peer clock estimate, so two devices form
//! a synchronized stereo pair or a larger speaker group. Each inbound route
//! plays one [`ChannelRole`] (both channels by default) through a native
//! `PipeWire` stream whose device delay feeds the schedule. The source's own
//! local playback could join the group by playing the same frames through the
//! same playout with an identity clock.
//!
//! The transport is lossy by design: each frame travels on its own stream and
//! a slow send is dropped rather than retried. The receiver's jitter buffer
//! reorders by media timestamp, ignores duplicates, conceals gaps with a short
//! fade to silence, and never waits for a missing frame; keepalives from both
//! peers tell a paused source from a dead link.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use media::pw_stream::{
    AudioStreams, CaptureChunk, PlaybackConfig, PlaybackSource, PlaybackStream,
};
use media::sync_playout::{
    AudioFrame, ChannelRole, Payload, PlayoutConfig, PushOutcome, SyncPlayout,
};
use rand_core::Rng;

use crate::shell::device_fabric::{
    MAX_REMOTE_AUDIO_FRAME_BYTES, MAX_REMOTE_AUDIO_SESSIONS, PersonalFabricAction,
    PersonalFabricActionEnvelope, REMOTE_AUDIO_CHANNELS, REMOTE_AUDIO_FRAME_SAMPLES,
    REMOTE_AUDIO_GROUP_LATENCY_NS, REMOTE_AUDIO_PROTOCOL_VERSION, REMOTE_AUDIO_SAMPLE_FRAME_BYTES,
    REMOTE_AUDIO_SAMPLE_RATE_HZ, remote_audio_media_ns,
};
use crate::shell::device_fabric_runtime::{
    FabricClockSync, fabric_peer_clock, hold_fabric_clock_sync,
};
use crate::shell::fabric_clock::{ClockSyncFailure, ClockSyncState, PeerPlayoutClock};
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
/// How long an interrupted route stays listed so the loss is visible.
const INTERRUPTED_NOTICE_TTL: Duration = Duration::from_secs(30);
/// How stale the cached application-stream list may get before the picker
/// refreshes it off the calling thread.
const AUDIO_GRAPH_REFRESH: Duration = Duration::from_secs(2);
const CAPTURE_QUEUE: usize = 12;
/// `node.name` of the stream an inbound route plays through.
const PLAYBACK_NODE_NAME: &str = "tilo-remote-audio-sink";
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAudioSourceView {
    pub token: u64,
    pub label: String,
    pub application: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAudioPhase {
    AwaitingConsent,
    /// Approved, but the playing device has no estimate of the source's
    /// clock yet, so nothing can play in time.
    SyncingClock,
    /// The playing device could not learn the source's clock within its
    /// deadline. The route stays up and plays once the clock arrives.
    ClockFailed(ClockSyncFailure),
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
    pending: PendingAudio,
    /// Set for a speaker-group route: the member admitted it under its own
    /// group consent, and it captures the group's sink.
    tap: Option<Arc<crate::shell::speaker_group_audio::GroupTap>>,
    /// What the target last reported about its clock of this device, from
    /// its keepalives. `None` until the first one.
    peer_playout: Option<ClockSyncState>,
}

impl OutboundRoute {
    /// Whether the target can play what this route sends in time.
    fn playout(&self) -> ClockSyncState {
        self.peer_playout.unwrap_or(ClockSyncState::Syncing)
    }
}

struct InboundRoute {
    source: DeviceId,
    target: DeviceId,
    label: String,
    authorization: [u8; 16],
    /// When the route was approved, then when the last frame was queued.
    last_audio: Instant,
    /// When the source last sent any authenticated frame or keepalive.
    last_heard: Instant,
    last_keepalive: Instant,
    playback: Option<InboundPlayback>,
    /// Keeps the source's clock estimate fresh while the route plays.
    /// `None` only for a route whose playout needs no peer clock.
    clock_sync: Option<FabricClockSync>,
    /// A speaker-group route: every frame is admitted again against the
    /// member's group consent.
    group: bool,
}

impl InboundRoute {
    /// Whether this device can play the route in time.
    fn playout(&self) -> ClockSyncState {
        self.clock_sync
            .as_ref()
            .map_or(ClockSyncState::Ready, FabricClockSync::state)
    }
}

/// How a route in either direction is presented once it is approved.
const fn approved_phase(playout: ClockSyncState) -> RemoteAudioPhase {
    match playout {
        ClockSyncState::Ready => RemoteAudioPhase::Streaming,
        ClockSyncState::Syncing => RemoteAudioPhase::SyncingClock,
        ClockSyncState::Failed(reason) => RemoteAudioPhase::ClockFailed(reason),
    }
}

/// This device's own output playing as a member of a group session it
/// plays from: the group's frames go straight into the same timed playout
/// the remote members use, on an identity clock.
struct LocalGroupRoute {
    capture: PcmCapture,
    pending: PendingAudio,
    playback: InboundPlayback,
    sequence: u64,
    _tap: Arc<crate::shell::speaker_group_audio::GroupTap>,
}

/// Where a group route's sound comes from on the playing device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupRouteSource {
    /// An entry of the live-audio source picker.
    Picker(u64),
    /// Everything this device plays.
    System,
}

/// How a member renders its part of a group: which output, which channel
/// role, and the calibration trims.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupPlayback {
    pub sink_serial: u32,
    pub role: ChannelRole,
    pub trim_ns: i64,
    pub gain_db: f64,
    pub eq: Vec<media::calibration::EqBand>,
}

impl GroupPlayback {
    #[must_use]
    pub fn new(
        sink_serial: u32,
        role: ChannelRole,
        trim: &crate::shell::speaker_group::SpeakerTrim,
    ) -> Self {
        Self {
            sink_serial,
            role,
            trim_ns: i64::try_from(trim.delay_ns).unwrap_or(i64::MAX),
            gain_db: trim.gain_db(),
            eq: trim.eq_bands(),
        }
    }
}

/// Calibration gain and room EQ applied after the timed playout.
struct PostFilter {
    gain: f32,
    sections: Vec<media::calibration::BiquadCoefficients>,
    /// Transposed direct form II state, `[z1, z2]` per section per channel.
    state: Vec<[f64; 2]>,
    channels: usize,
}

impl PostFilter {
    fn new(playback: &GroupPlayback) -> Self {
        let channels = playback.role.channels();
        let sections = playback
            .eq
            .iter()
            .map(|band| band.coefficients(REMOTE_AUDIO_SAMPLE_RATE_HZ))
            .collect::<Vec<_>>();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "calibration gain is bounded to a few tens of dB"
        )]
        let gain = 10.0_f64.powf(playback.gain_db / 20.0) as f32;
        Self {
            gain,
            state: vec![[0.0; 2]; sections.len().saturating_mul(channels)],
            sections,
            channels,
        }
    }

    fn apply(&mut self, out: &mut [f32]) {
        let channels = self.channels.max(1);
        for (index, sample) in out.iter_mut().enumerate() {
            let channel = index.checked_rem(channels).unwrap_or(0);
            let mut value = f64::from(*sample);
            for (section_index, coefficients) in self.sections.iter().enumerate() {
                let Some(state) = self.state.get_mut(
                    section_index
                        .saturating_mul(channels)
                        .saturating_add(channel),
                ) else {
                    continue;
                };
                let output = coefficients.b0.mul_add(value, state[0]);
                state[0] = coefficients.b1.mul_add(value, -coefficients.a1 * output) + state[1];
                state[1] = coefficients.b2.mul_add(value, -coefficients.a2 * output);
                value = output;
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "audio samples are bounded to full scale"
            )]
            let filtered = value as f32;
            *sample = (filtered * self.gain).clamp(-1.0, 1.0);
        }
    }
}

/// The member side of an inbound route: the jitter buffer and drift-steered
/// playout, and the source clock it schedules against.
struct RoutePlayout {
    playout: SyncPlayout,
    /// Maps the source's clock to this device's; `None` until the fabric has
    /// an estimate, during which the route plays silence.
    clock: Option<PeerPlayoutClock>,
    /// A speaker-group member's calibration, applied after playout.
    post: Option<PostFilter>,
}

/// Shared between the fabric handler that queues frames and the playback
/// stream that renders them.
#[derive(Clone)]
struct SharedPlayout(Arc<Mutex<RoutePlayout>>);

impl SharedPlayout {
    fn lock(&self) -> std::sync::MutexGuard<'_, RoutePlayout> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PlaybackSource for SharedPlayout {
    fn render(&mut self, presentation_ns: i64, out: &mut [f32]) {
        let mut route = self.lock();
        let RoutePlayout {
            playout,
            clock,
            post,
        } = &mut *route;
        match clock {
            Some(clock) => playout.render(clock, presentation_ns, out),
            None => out.fill(0.0),
        }
        if let Some(post) = post {
            post.apply(out);
        }
        drop(route);
    }
}

struct InboundPlayback {
    role: ChannelRole,
    shared: SharedPlayout,
    stream: Box<dyn PlaybackStream>,
}

/// Opens the playback stream for an inbound route playing `role`.
fn start_playback(
    streams: &dyn AudioStreams,
    label: &str,
    role: ChannelRole,
) -> Result<InboundPlayback, String> {
    start_playback_on(streams, label, role, None)
}

/// As [`start_playback`], for a speaker-group member: pinned to its output,
/// delayed by its trim, with its calibration gain and EQ.
fn start_group_playback(
    streams: &dyn AudioStreams,
    label: &str,
    group: &GroupPlayback,
) -> Result<InboundPlayback, String> {
    let playback = start_playback_on(streams, label, group.role, Some(group))?;
    playback.shared.lock().playout.set_trim_ns(group.trim_ns);
    Ok(playback)
}

fn start_playback_on(
    streams: &dyn AudioStreams,
    label: &str,
    role: ChannelRole,
    group: Option<&GroupPlayback>,
) -> Result<InboundPlayback, String> {
    let latency = i64::try_from(REMOTE_AUDIO_GROUP_LATENCY_NS).unwrap_or(i64::MAX);
    let shared = SharedPlayout(Arc::new(Mutex::new(RoutePlayout {
        playout: SyncPlayout::new(PlayoutConfig::new(
            REMOTE_AUDIO_SAMPLE_RATE_HZ,
            latency,
            role,
        )),
        clock: None,
        post: group
            .filter(|group| !group.eq.is_empty() || group.gain_db != 0.0)
            .map(PostFilter::new),
    })));
    let mut config =
        PlaybackConfig::for_role(PLAYBACK_NODE_NAME, label, REMOTE_AUDIO_SAMPLE_RATE_HZ, role);
    config.target_serial = group.map(|group| group.sink_serial);
    let stream = streams
        .open_playback(&config, Box::new(shared.clone()))
        .map_err(|error| format!("start remote audio playback: {error}"))?;
    Ok(InboundPlayback {
        role,
        shared,
        stream,
    })
}

/// One authenticated frame as the receiver sees it.
struct ReceivedFrame<'a> {
    sequence: u64,
    media_ts: u64,
    presentation_ns: u64,
    pcm_s16le: &'a [u8],
}

/// Queues one frame for playout at its presentation time. `clock` is the
/// latest estimate of the source's clock; without one the last known
/// estimate stays in use.
fn ingest_frame(
    playback: &InboundPlayback,
    frame: &ReceivedFrame<'_>,
    clock: Option<PeerPlayoutClock>,
) -> Result<PushOutcome, String> {
    if let Some(failure) = playback.stream.failure() {
        return Err(failure);
    }
    let media_ts =
        i64::try_from(frame.media_ts).map_err(|_| "frame timestamp out of range".to_string())?;
    let latency = remote_audio_media_ns(frame.media_ts)
        .and_then(|media_ns| frame.presentation_ns.checked_sub(media_ns))
        .and_then(|latency| i64::try_from(latency).ok())
        .ok_or_else(|| "frame presentation time out of range".to_string())?;
    let samples = frame
        .pcm_s16le
        .chunks_exact(2)
        .map(|pair| <[u8; 2]>::try_from(pair).map_or(0, i16::from_le_bytes))
        .collect();
    let mut route = playback.shared.lock();
    if clock.is_some() {
        route.clock = clock;
    }
    if route.playout.config().group_latency_ns != latency {
        route.playout.set_group_latency_ns(latency);
    }
    let outcome = route.playout.push(&AudioFrame {
        seq: frame.sequence,
        media_ts,
        channels: u16::from(REMOTE_AUDIO_CHANNELS),
        payload: Payload::I16(samples),
    });
    drop(route);
    Ok(outcome)
}

/// Where a frame's first sample sits on the source clock and when it plays:
/// `(media_ts, presentation_ns)`.
fn frame_schedule(first_frame: i64) -> Option<(u64, u64)> {
    let media_ts = u64::try_from(first_frame).ok()?;
    let presentation_ns =
        remote_audio_media_ns(media_ts)?.checked_add(REMOTE_AUDIO_GROUP_LATENCY_NS)?;
    Some((media_ts, presentation_ns))
}

/// A run of captured samples with no gap in it.
struct AudioRun {
    first_frame: i64,
    pcm: Vec<u8>,
}

impl AudioRun {
    fn end_frame(&self) -> i64 {
        self.first_frame.saturating_add(frames_in(self.pcm.len()))
    }
}

fn frames_in(bytes: usize) -> i64 {
    i64::try_from(bytes / REMOTE_AUDIO_SAMPLE_FRAME_BYTES).unwrap_or(i64::MAX)
}

/// Captured audio waiting to be framed. A gap in the capture timestamps
/// closes a run: the frames before it go out, the last one short, and the
/// next frame starts at the new timestamp, so the receiver sees the gap
/// instead of time-shifted audio.
#[derive(Default)]
struct PendingAudio {
    runs: VecDeque<AudioRun>,
}

impl PendingAudio {
    fn push(&mut self, chunk: CaptureChunk) {
        if chunk.pcm_s16le.is_empty() {
            return;
        }
        match self.runs.back_mut() {
            Some(run) if run.end_frame() == chunk.first_frame => {
                run.pcm.extend_from_slice(&chunk.pcm_s16le);
            }
            _ => self.runs.push_back(AudioRun {
                first_frame: chunk.first_frame,
                pcm: chunk.pcm_s16le,
            }),
        }
        self.bound();
    }

    fn len(&self) -> usize {
        self.runs.iter().map(|run| run.pcm.len()).sum()
    }

    /// Drops the oldest audio beyond the bound, at a sample boundary: live
    /// routing favours bounded latency over replaying a backlog after
    /// transport backpressure clears.
    fn bound(&mut self) {
        let excess = self.len().saturating_sub(MAX_PENDING_PCM_BYTES);
        let mut discard = excess
            .checked_next_multiple_of(REMOTE_AUDIO_SAMPLE_FRAME_BYTES)
            .unwrap_or(usize::MAX);
        while discard > 0 {
            let Some(front) = self.runs.front_mut() else {
                return;
            };
            if front.pcm.len() <= discard {
                discard = discard.saturating_sub(front.pcm.len());
                self.runs.pop_front();
            } else {
                front.pcm.drain(..discard);
                front.first_frame = front.first_frame.saturating_add(frames_in(discard));
                discard = 0;
            }
        }
    }

    /// The next frame to send as `(first frame, pcm)`: a full frame from the
    /// oldest run, or the rest of the oldest run once a gap has closed it.
    fn next_frame(&mut self) -> Option<(i64, Vec<u8>)> {
        loop {
            let closed = self.runs.len() > 1;
            let front = self.runs.front_mut()?;
            if front.pcm.len() >= MAX_REMOTE_AUDIO_FRAME_BYTES {
                let tail = front.pcm.split_off(MAX_REMOTE_AUDIO_FRAME_BYTES);
                let pcm = std::mem::replace(&mut front.pcm, tail);
                let first_frame = front.first_frame;
                front.first_frame = first_frame.saturating_add(frames_in(pcm.len()));
                return Some((first_frame, pcm));
            }
            if !closed {
                return None;
            }
            let run = self.runs.pop_front()?;
            if !run.pcm.is_empty() {
                return Some((run.first_frame, run.pcm));
            }
        }
    }
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
    local: BTreeMap<[u8; 16], LocalGroupRoute>,
}

impl RemoteAudioState {
    /// Nothing for [`pump`] to reconcile, capture, send or expire. Picker
    /// source tokens need no pumping.
    fn is_idle(&self) -> bool {
        self.pending.is_empty()
            && self.outbound.is_empty()
            && self.inbound.is_empty()
            && self.interrupted.is_empty()
            && self.local.is_empty()
    }
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
        state.local.clear();
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
                    playout: None,
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
                    playout: Some(route.playout()),
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
                LocalAudioSelection::System => (
                    crate::shell::i18n::t("remote-audio-all-system-audio"),
                    false,
                ),
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
        .filter(|(_, route)| route.tap.is_none())
        .map(|(session_id, route)| RemoteAudioRouteView {
            session_id: *session_id,
            peer: route.target.clone(),
            label: route.label.clone(),
            inbound: false,
            phase: if route.authorization.is_some() {
                approved_phase(route.playout())
            } else {
                RemoteAudioPhase::AwaitingConsent
            },
        });
    let inbound =
        state
            .inbound
            .iter()
            .filter(|(_, route)| !route.group)
            .map(|(session_id, route)| RemoteAudioRouteView {
                session_id: *session_id,
                peer: route.source.clone(),
                label: route.label.clone(),
                inbound: true,
                phase: approved_phase(route.playout()),
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
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let fabric = network.device_fabric();
    let snapshot = fabric
        .snapshots()
        .get(target)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-target-not-enrolled"))?;
    if target == &local || !snapshot.presence.online || !snapshot.presence.capabilities.audio_output
    {
        return Err(crate::shell::i18n::t(
            "remote-audio-error-target-not-output",
        ));
    }
    let (session_id, selection, label) = {
        let mut state = state()
            .lock()
            .map_err(|_| crate::shell::i18n::t("remote-audio-error-unavailable"))?;
        if state
            .outbound
            .len()
            .saturating_add(state.inbound.len())
            .saturating_add(state.pending.len())
            >= MAX_REMOTE_AUDIO_SESSIONS
        {
            return Err(crate::shell::i18n::t("remote-audio-error-too-many"));
        }
        let selection = state
            .source_tokens
            .remove(&source_token)
            .ok_or_else(|| crate::shell::i18n::t("remote-audio-error-source-expired"))?;
        let label = match &selection {
            LocalAudioSelection::System => crate::shell::i18n::t("remote-audio-all-system-audio"),
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
                pending: PendingAudio::default(),
                tap: None,
                peer_playout: None,
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

/// Resolve one target-local consent row. Approval starts the local playback
/// stream before minting and sending the capability, so false readiness is
/// impossible. The route plays both channels until [`set_role`] picks one.
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
        .map_err(|_| crate::shell::i18n::t("remote-audio-error-unavailable"))?
        .pending
        .remove(&session_id)
        .ok_or_else(|| crate::shell::i18n::t("remote-audio-error-request-expired"))?;
    let authorization = approved.then(random_nonzero_16);
    if let Some(authorization) = authorization {
        let playback = match start_playback(
            media::pw_stream::native_streams(),
            &pending.label,
            ChannelRole::Stereo,
        ) {
            Ok(playback) => playback,
            Err(error) => {
                if let Ok(mut state) = state().lock() {
                    state.pending.insert(session_id, pending);
                }
                return Err(error);
            }
        };
        let mut state = state()
            .lock()
            .map_err(|_| crate::shell::i18n::t("remote-audio-error-unavailable"))?;
        if state.outbound.len().saturating_add(state.inbound.len()) >= MAX_REMOTE_AUDIO_SESSIONS {
            state.pending.insert(session_id, pending);
            return Err(crate::shell::i18n::t("remote-audio-error-too-many"));
        }
        let now = Instant::now();
        state.inbound.insert(
            session_id,
            InboundRoute {
                source: pending.source.clone(),
                target: pending.target,
                label: pending.label,
                authorization,
                last_audio: now,
                last_heard: now,
                last_keepalive: now,
                playback: Some(playback),
                clock_sync: Some(hold_fabric_clock_sync(pending.source.clone())),
                group: false,
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

/// Choose which part of the stereo stream this device plays for an inbound
/// route: one side of a stereo pair, a mono, centre or LFE feed, or both
/// channels. The route's playback stream restarts with the role's channel
/// count, so the switch costs one group latency of audio.
///
/// # Errors
///
/// Returns a message when the route is no longer active or the new playback
/// stream cannot start.
pub fn set_role(session_id: [u8; 16], role: ChannelRole) -> Result<(), String> {
    set_role_with(
        state(),
        media::pw_stream::native_streams(),
        session_id,
        role,
    )
}

fn set_role_with(
    state: &Mutex<RemoteAudioState>,
    streams: &dyn AudioStreams,
    session_id: [u8; 16],
    role: ChannelRole,
) -> Result<(), String> {
    let inactive = || crate::shell::i18n::t("remote-audio-error-route-inactive");
    let current = state.lock().unwrap_or_else(PoisonError::into_inner);
    let route = current.inbound.get(&session_id).map(|route| {
        (
            route.label.clone(),
            route.playback.as_ref().map(|playback| playback.role),
        )
    });
    drop(current);
    let (label, playing) = route.ok_or_else(inactive)?;
    if playing == Some(role) {
        return Ok(());
    }
    // Opening a stream talks to the daemon; never while holding the state.
    let playback = start_playback(streams, &label, role)?;
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let route = state.inbound.get_mut(&session_id).ok_or_else(inactive)?;
    if let Some(previous) = route.playback.as_ref() {
        let clock = previous.shared.lock().clock;
        playback.shared.lock().clock = clock;
    }
    route.playback = Some(playback);
    drop(state);
    Ok(())
}

fn tap_for(
    state: &Mutex<RemoteAudioState>,
    source: GroupRouteSource,
) -> Result<Arc<crate::shell::speaker_group_audio::GroupTap>, String> {
    use crate::shell::speaker_group_audio::{TapSelection, acquire};
    let selection = match source {
        GroupRouteSource::System => TapSelection::System,
        GroupRouteSource::Picker(token) => {
            let chosen = state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .source_tokens
                .get(&token)
                .cloned()
                .ok_or_else(|| crate::shell::i18n::t("remote-audio-error-source-expired"))?;
            match chosen {
                LocalAudioSelection::System => TapSelection::System,
                LocalAudioSelection::Application { serial, .. } => {
                    TapSelection::Application(serial)
                }
            }
        }
    };
    acquire(selection)
}

/// Send a speaker group's audio to one member that admitted `route` under
/// its group consent. The member-issued `authorization` rides on every frame.
///
/// # Errors
///
/// Returns a message when the source cannot be captured or too many routes
/// are active.
pub fn start_group_send(
    route: [u8; 16],
    target: &DeviceId,
    authorization: [u8; 16],
    source: GroupRouteSource,
) -> Result<(), String> {
    let tap = tap_for(state(), source)?;
    let capture = PcmCapture::group_sink(tap.serial())?;
    let mut state = state().lock().unwrap_or_else(PoisonError::into_inner);
    if state.outbound.len().saturating_add(state.inbound.len()) >= MAX_REMOTE_AUDIO_SESSIONS {
        return Err(crate::shell::i18n::t("remote-audio-error-too-many"));
    }
    let now = Instant::now();
    state.outbound.insert(
        route,
        OutboundRoute {
            target: target.clone(),
            label: crate::shell::i18n::t("remote-audio-speaker-group"),
            selection: LocalAudioSelection::System,
            authorization: Some(authorization),
            sequence: 0,
            last_audio: now,
            last_heard: now,
            last_keepalive: now,
            capture: Some(capture),
            pending: PendingAudio::default(),
            tap: Some(tap),
            peer_playout: None,
        },
    );
    drop(state);
    Ok(())
}

/// Play a speaker group's audio on one of this device's own outputs, as a
/// member, through the same timed playout the remote members use.
///
/// # Errors
///
/// Returns a message when the source cannot be captured or the output
/// cannot be opened.
pub fn start_group_local(
    route: [u8; 16],
    source: GroupRouteSource,
    playback: &GroupPlayback,
) -> Result<(), String> {
    let tap = tap_for(state(), source)?;
    let capture = PcmCapture::group_sink(tap.serial())?;
    let playback = start_group_playback(
        media::pw_stream::native_streams(),
        "Speaker group",
        playback,
    )?;
    playback.shared.lock().clock = Some(PeerPlayoutClock::new(0, 0, 0.0));
    state()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .local
        .insert(
            route,
            LocalGroupRoute {
                capture,
                pending: PendingAudio::default(),
                playback,
                sequence: 0,
                _tap: tap,
            },
        );
    Ok(())
}

/// Play what `source` sends under `route`, as this device's part of a group
/// it consented to. Frames are admitted only while that consent holds.
///
/// # Errors
///
/// Returns a message when the output cannot be opened or too many routes are
/// active.
pub fn start_group_receive(
    route: [u8; 16],
    source: &DeviceId,
    local: &DeviceId,
    authorization: [u8; 16],
    playback: &GroupPlayback,
) -> Result<(), String> {
    let playback = start_group_playback(
        media::pw_stream::native_streams(),
        "Speaker group",
        playback,
    )?;
    let mut state = state().lock().unwrap_or_else(PoisonError::into_inner);
    if state.outbound.len().saturating_add(state.inbound.len()) >= MAX_REMOTE_AUDIO_SESSIONS {
        return Err(crate::shell::i18n::t("remote-audio-error-too-many"));
    }
    let now = Instant::now();
    state.inbound.insert(
        route,
        InboundRoute {
            source: source.clone(),
            target: local.clone(),
            label: crate::shell::i18n::t("remote-audio-speaker-group"),
            authorization,
            last_audio: now,
            last_heard: now,
            last_keepalive: now,
            playback: Some(playback),
            clock_sync: Some(hold_fabric_clock_sync(source.clone())),
            group: true,
        },
    );
    drop(state);
    Ok(())
}

/// End one group route here. The speaker-group runtime tells the peer.
pub fn end_group_route(route: [u8; 16]) {
    let mut state = state().lock().unwrap_or_else(PoisonError::into_inner);
    if state
        .outbound
        .get(&route)
        .is_some_and(|outbound| outbound.tap.is_some())
    {
        state.outbound.remove(&route);
    }
    if state
        .inbound
        .get(&route)
        .is_some_and(|inbound| inbound.group)
    {
        state.inbound.remove(&route);
    }
    state.local.remove(&route);
}

/// Whether a running group route can play in time: for a member this device
/// sends to, what it last reported; for a member playing here, this device's
/// own clock of the source. `None` once the route is gone.
#[must_use]
pub fn group_route_playout(route: [u8; 16]) -> Option<ClockSyncState> {
    let state = state().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(outbound) = state.outbound.get(&route)
        && outbound.tap.is_some()
    {
        return Some(outbound.playout());
    }
    if let Some(inbound) = state.inbound.get(&route)
        && inbound.group
    {
        return Some(inbound.playout());
    }
    state
        .local
        .contains_key(&route)
        .then_some(ClockSyncState::Ready)
}

/// [`group_route_playout`] for every running group route at once.
#[must_use]
pub fn group_route_playouts() -> BTreeMap<[u8; 16], ClockSyncState> {
    let state = state().lock().unwrap_or_else(PoisonError::into_inner);
    let outbound = state
        .outbound
        .iter()
        .filter(|(_, route)| route.tap.is_some())
        .map(|(route, outbound)| (*route, outbound.playout()));
    let inbound = state
        .inbound
        .iter()
        .filter(|(_, route)| route.group)
        .map(|(route, inbound)| (*route, inbound.playout()));
    let local = state
        .local
        .keys()
        .map(|route| (*route, ClockSyncState::Ready));
    let playouts = outbound.chain(inbound).chain(local).collect();
    drop(state);
    playouts
}

/// Whether a group route is still running here; the audio path ends one on
/// its own when its link or output fails.
#[must_use]
pub fn has_group_route(route: [u8; 16]) -> bool {
    let state = state().lock().unwrap_or_else(PoisonError::into_inner);
    state
        .outbound
        .get(&route)
        .is_some_and(|outbound| outbound.tap.is_some())
        || state
            .inbound
            .get(&route)
            .is_some_and(|inbound| inbound.group)
        || state.local.contains_key(&route)
}

/// Feed each local member its group's frames on the identity clock. A
/// failed capture or output ends only that route.
fn pump_local_group_routes(state: &mut RemoteAudioState) {
    let mut failed = Vec::new();
    for (route_id, route) in &mut state.local {
        let Ok(chunks) = route.capture.drain() else {
            failed.push(*route_id);
            continue;
        };
        for chunk in chunks {
            route.pending.push(chunk);
        }
        while let Some((first_frame, pcm_s16le)) = route.pending.next_frame() {
            let Some((media_ts, presentation_ns)) = frame_schedule(first_frame) else {
                continue;
            };
            route.sequence = route.sequence.saturating_add(1);
            let frame = ReceivedFrame {
                sequence: route.sequence,
                media_ts,
                presentation_ns,
                pcm_s16le: &pcm_s16le,
            };
            if ingest_frame(&route.playback, &frame, None).is_err() {
                failed.push(*route_id);
                break;
            }
        }
    }
    for route in failed {
        state.local.remove(&route);
    }
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
            .map_err(|_| crate::shell::i18n::t("remote-audio-error-unavailable"))?;
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
        return Err(crate::shell::i18n::t("remote-audio-error-route-inactive"));
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
            media_ts,
            presentation_ns,
            pcm_s16le,
        } => {
            // Read before taking the route state: the estimate has its own lock.
            let clock =
                fabric_peer_clock(&envelope.source).map(|estimate| estimate.playout_clock());
            let mut failed = false;
            if let Ok(mut state) = state().lock() {
                if let Some(route) = state.inbound.get_mut(session_id)
                    && (!route.group
                        || crate::shell::speaker_group_runtime::admits_route(
                            *session_id,
                            &envelope.source,
                        ))
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
                    let frame = ReceivedFrame {
                        sequence: *sequence,
                        media_ts: *media_ts,
                        presentation_ns: *presentation_ns,
                        pcm_s16le,
                    };
                    match route
                        .playback
                        .as_ref()
                        .map(|playback| ingest_frame(playback, &frame, clock))
                    {
                        Some(Ok(PushOutcome::Queued)) => route.last_audio = now,
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => failed = true,
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
            playout,
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
                    if playout.is_some() {
                        route.peer_playout = *playout;
                    }
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
    pump_state(state(), network, enabled, focus);
}

/// [`pump`] over `state`. Runs every compositor frame, so with no route,
/// request or notice it returns before touching the device fabric, and
/// otherwise reads the fabric in place rather than cloning it.
fn pump_state(
    state: &Mutex<RemoteAudioState>,
    network: &ShellNetwork,
    enabled: bool,
    focus: Option<&PhysicalGamepadBinding>,
) {
    if state.lock().is_ok_and(|state| state.is_idle()) {
        return;
    }
    let online = network.online_device_ids();
    let now = Instant::now();
    let mut egress = Vec::new();
    let Ok(mut guard) = state.lock() else {
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
        if let Ok(chunks) = capture.drain() {
            for chunk in chunks {
                route.pending.push(chunk);
            }
        } else {
            terminate_capture_failure(&mut guard, session_id, &mut egress);
            continue;
        }
        let Some(authorization) = route.authorization else {
            continue;
        };
        for _ in 0..SEND_BUDGET {
            let Some((first_frame, pcm_s16le)) = route.pending.next_frame() else {
                break;
            };
            let Some((media_ts, presentation_ns)) = frame_schedule(first_frame) else {
                continue;
            };
            route.sequence = route.sequence.saturating_add(1);
            route.last_audio = now;
            queue_egress(
                &mut egress,
                route.target.clone(),
                PersonalFabricAction::RemoteAudioFrame {
                    session_id,
                    authorization,
                    sequence: route.sequence,
                    media_ts,
                    presentation_ns,
                    pcm_s16le,
                },
                Some(session_id),
            );
        }
    }
    pump_local_group_routes(&mut guard);
    egress.extend(due_keepalives(&mut guard, now));
    drop(guard);
    for message in egress {
        let outbound_session = message.outbound_session;
        if let Err(error) = network.try_send_personal_fabric_action(&message.target, message.action)
            && let Ok(mut state) = state.lock()
        {
            apply_send_failure(&mut state, outbound_session, &error, Instant::now());
        }
    }
}

/// Records the chosen local node through a native `PipeWire` capture stream
/// pinned to that node: it fails rather than fall back to the default source,
/// which may be the microphone.
struct PcmCapture {
    stream: Box<dyn media::pw_stream::CaptureStream>,
}

impl PcmCapture {
    fn start(selection: &LocalAudioSelection) -> Result<Self, String> {
        let target = match selection {
            LocalAudioSelection::System => media::audio::enumerate()
                .default_sink()
                .map(|sink| media::pw_stream::CaptureTarget {
                    serial: sink.serial,
                    monitor_sink: true,
                })
                .ok_or_else(|| crate::shell::i18n::t("remote-audio-error-no-output"))?,
            LocalAudioSelection::Application { serial, .. } => {
                let current = media::audio::enumerate()
                    .playback_streams
                    .into_iter()
                    .any(|stream| stream.serial == *serial);
                if !current {
                    return Err(crate::shell::i18n::t("remote-audio-error-stream-ended"));
                }
                media::pw_stream::CaptureTarget {
                    serial: *serial,
                    monitor_sink: false,
                }
            }
        };
        let node_name = match selection {
            LocalAudioSelection::System => "tilo-remote-audio-source",
            LocalAudioSelection::Application { .. } => "tilo-remote-application-audio-source",
        };
        let config = media::pw_stream::CaptureConfig {
            node_name: node_name.into(),
            target,
            rate: REMOTE_AUDIO_SAMPLE_RATE_HZ,
            channels: u16::from(REMOTE_AUDIO_CHANNELS),
            queue_chunks: CAPTURE_QUEUE,
        };
        let stream = media::pw_stream::native_streams()
            .open_capture(&config)
            .map_err(|error| format!("start remote audio capture: {error}"))?;
        Ok(Self { stream })
    }

    /// Capture a speaker group's sink.
    fn group_sink(serial: u32) -> Result<Self, String> {
        let config = media::pw_stream::CaptureConfig {
            node_name: "tilo-speaker-group-source".into(),
            target: media::pw_stream::CaptureTarget {
                serial,
                monitor_sink: true,
            },
            rate: REMOTE_AUDIO_SAMPLE_RATE_HZ,
            channels: u16::from(REMOTE_AUDIO_CHANNELS),
            queue_chunks: CAPTURE_QUEUE,
        };
        let stream = media::pw_stream::native_streams()
            .open_capture(&config)
            .map_err(|error| format!("start speaker group capture: {error}"))?;
        Ok(Self { stream })
    }

    fn drain(&mut self) -> Result<Vec<CaptureChunk>, String> {
        self.stream
            .drain()
            .map_err(|error| format!("remote audio capture ended: {error}"))
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "fixture arithmetic on literal sample counts, levels and times"
)]
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
            pending: PendingAudio::default(),
            tap: None,
            peer_playout: None,
        }
    }

    fn inbound(source: &DeviceId, target: &DeviceId, at: Instant) -> InboundRoute {
        InboundRoute {
            source: source.clone(),
            target: target.clone(),
            label: "Podcast".into(),
            authorization: [5; 16],
            last_audio: at,
            last_heard: at,
            last_keepalive: at,
            playback: None,
            clock_sync: None,
            group: false,
        }
    }

    fn before(now: Instant, elapsed: Duration) -> Instant {
        now.checked_sub(elapsed).unwrap()
    }

    #[test]
    fn pump_does_not_clone_fabric_when_idle() {
        let (network, mut commands) = crate::shell::network::shell_network_for_test(
            crate::shell::state::WorkspaceTree::default().shared(),
            0xA0D1,
        );
        let state = Mutex::new(RemoteAudioState::default());
        for _ in 0..100 {
            pump_state(&state, &network, true, None);
        }
        assert_eq!(
            network.device_fabric_reads(),
            (0, 0),
            "an idle pump does not look at the fabric"
        );
        assert!(commands.try_recv().is_err(), "an idle pump sends nothing");

        // A live notice needs the online set, read in place.
        state.lock().unwrap().interrupted.insert(
            [9; 16],
            InterruptedRoute {
                peer: DeviceId::new("peer").unwrap(),
                label: "Music".into(),
                inbound: false,
                at: Instant::now(),
            },
        );
        pump_state(&state, &network, true, None);
        assert_eq!(
            network.device_fabric_reads(),
            (0, 1),
            "borrowed, never cloned"
        );
        assert_eq!(state.lock().unwrap().interrupted.len(), 1);
    }

    #[test]
    fn audio_routes_advance_only_with_verified_capabilities_focus_and_bindings() {
        // Phase `foreign_capabilities_cannot_feed_an_inbound_route`.
        {
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

        // Phase `application_routes_are_bound_to_the_locally_verified_focus`.
        {
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

        // Phase `picker_tokens_survive_refresh_only_while_the_source_binding_exists`.
        {
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
    }

    #[derive(Default)]
    struct FakeStreams {
        opened: Mutex<Vec<(PlaybackConfig, Box<dyn PlaybackSource>)>>,
        failing: bool,
    }

    impl FakeStreams {
        fn take(&self) -> (PlaybackConfig, Box<dyn PlaybackSource>) {
            self.opened.lock().unwrap().remove(0)
        }
    }

    struct FakePlayback {
        failing: bool,
    }

    impl PlaybackStream for FakePlayback {
        fn timing(&self) -> Option<media::pw_stream::DeviceTiming> {
            None
        }

        fn failure(&self) -> Option<String> {
            self.failing.then(|| "the device was unplugged".into())
        }
    }

    impl AudioStreams for FakeStreams {
        fn open_playback(
            &self,
            config: &PlaybackConfig,
            source: Box<dyn PlaybackSource>,
        ) -> Result<Box<dyn PlaybackStream>, media::pw_stream::StreamError> {
            self.opened.lock().unwrap().push((config.clone(), source));
            Ok(Box::new(FakePlayback {
                failing: self.failing,
            }))
        }

        fn open_capture(
            &self,
            _config: &media::pw_stream::CaptureConfig,
        ) -> Result<Box<dyn media::pw_stream::CaptureStream>, media::pw_stream::StreamError>
        {
            Err(media::pw_stream::StreamError::Unavailable(
                "these tests play only".into(),
            ))
        }
    }

    const RATE: i64 = REMOTE_AUDIO_SAMPLE_RATE_HZ as i64;
    const FRAME_SAMPLES: i64 = REMOTE_AUDIO_FRAME_SAMPLES as i64;
    /// The source booted five seconds before this device.
    const SOURCE_AHEAD_NS: i64 = 5_000_000_000;
    const LATENCY_NS: i64 = REMOTE_AUDIO_GROUP_LATENCY_NS as i64;
    /// The test stream starts 100 s after the source booted.
    const START_FRAME: i64 = 100 * RATE;

    fn source_clock() -> PeerPlayoutClock {
        PeerPlayoutClock::new(0, SOURCE_AHEAD_NS, 0.0)
    }

    fn sample_ns(frame: i64) -> i64 {
        frame * 1_000_000_000 / RATE
    }

    /// Where on this device's clock the source's `frame` plays.
    fn local_playout_ns(frame: i64) -> i64 {
        sample_ns(frame) - SOURCE_AHEAD_NS + LATENCY_NS
    }

    /// One 20 ms frame holding constant levels.
    fn level_pcm(left: f32, right: f32) -> Vec<u8> {
        let left = ((left * 32_767.0) as i16).to_le_bytes();
        let right = ((right * 32_767.0) as i16).to_le_bytes();
        (0..FRAME_SAMPLES)
            .flat_map(|_| [left[0], left[1], right[0], right[1]])
            .collect()
    }

    fn deliver(
        playback: &InboundPlayback,
        index: i64,
        pcm: &[u8],
        clock: Option<PeerPlayoutClock>,
    ) -> PushOutcome {
        let first_frame = START_FRAME + index * FRAME_SAMPLES;
        let (media_ts, presentation_ns) = frame_schedule(first_frame).unwrap();
        ingest_frame(
            playback,
            &ReceivedFrame {
                sequence: u64::try_from(index + 1).unwrap(),
                media_ts,
                presentation_ns,
                pcm_s16le: pcm,
            },
            clock,
        )
        .unwrap()
    }

    #[test]
    fn frames_are_scheduled_one_group_latency_after_capture() {
        let (media_ts, presentation_ns) = frame_schedule(START_FRAME).unwrap();
        assert_eq!(media_ts, 4_800_000);
        assert_eq!(
            presentation_ns,
            100_000_000_000 + REMOTE_AUDIO_GROUP_LATENCY_NS
        );
        assert_eq!(frame_schedule(-1), None);
    }

    #[test]
    fn jittered_lost_and_reordered_frames_play_continuously_at_their_presentation_time() {
        const FRAMES: i64 = 40;
        const LOST: i64 = 7;
        const DUPLICATED: i64 = 10;
        let level = |index: i64| 0.1 + 0.02 * (index % 20) as f32;
        let streams = FakeStreams::default();
        let playback = start_playback(&streams, "Podcast", ChannelRole::Stereo).unwrap();
        let (config, mut source) = streams.take();
        assert_eq!(config.channels, 2);

        // Each frame leaves once its last sample is captured and crosses the
        // network in 2–12 ms; frame 3 is overtaken by frame 4, frame 7 never
        // arrives and frame 10 arrives twice.
        let mut arrivals = Vec::new();
        for index in 0..FRAMES {
            if index == LOST {
                continue;
            }
            let sent = sample_ns(START_FRAME + (index + 1) * FRAME_SAMPLES) - SOURCE_AHEAD_NS;
            let transit = if index == 3 {
                30_000_000
            } else {
                2_000_000 + (index * 7 % 11) * 1_000_000
            };
            arrivals.push((sent + transit, index));
            if index == DUPLICATED {
                arrivals.push((sent + transit + 3_000_000, index));
            }
        }
        arrivals.sort_unstable();

        // The device pulls 10 ms blocks, each presented when it is rendered.
        let block = 480_usize;
        let block_ns = 10_000_000;
        let start = arrivals[0].0;
        let end = local_playout_ns(START_FRAME + (FRAMES + 1) * FRAME_SAMPLES);
        let mut output = Vec::new();
        let mut delivered = 0;
        let mut now = start;
        while now < end {
            while delivered < arrivals.len() && arrivals[delivered].0 <= now {
                let index = arrivals[delivered].1;
                let pcm = level_pcm(level(index), -level(index));
                deliver(&playback, index, &pcm, Some(source_clock()));
                delivered += 1;
            }
            let mut out = vec![0.0; block * 2];
            source.render(now, &mut out);
            output.extend(out);
            now += block_ns;
        }

        let stats = *playback.shared.lock().playout.stats();
        assert_eq!(stats.duplicates, 1);
        assert_eq!(
            stats.late, 0,
            "every frame arrived within the group latency"
        );
        let at = |local_ns: i64| {
            let index =
                usize::try_from(((local_ns - start) * RATE + 500_000_000) / 1_000_000_000).unwrap();
            (output[index * 2], output[index * 2 + 1])
        };
        for index in 1..FRAMES - 1 {
            let first = START_FRAME + index * FRAME_SAMPLES;
            if index == LOST {
                let (left, right) = at(local_playout_ns(first + FRAME_SAMPLES / 2));
                assert!(
                    left.abs() < 0.01 && right.abs() < 0.01,
                    "the lost frame is silent"
                );
                continue;
            }
            if (LOST - 1..=LOST + 1).contains(&index) {
                continue;
            }
            // Away from the frame edges, where the resampler rings between
            // levels, every sample is the frame's level at its scheduled time.
            for offset in (64..FRAME_SAMPLES - 64).step_by(32) {
                let (left, right) = at(local_playout_ns(first + offset));
                assert!(
                    (left - level(index)).abs() < 0.005 && (right + level(index)).abs() < 0.005,
                    "frame {index} sample {offset}: ({left}, {right})"
                );
            }
        }
    }

    #[test]
    fn a_route_plays_silence_until_the_source_clock_is_known() {
        let streams = FakeStreams::default();
        let playback = start_playback(&streams, "Podcast", ChannelRole::Stereo).unwrap();
        let (_, mut source) = streams.take();
        let pcm = level_pcm(0.4, 0.4);
        for index in 0..3 {
            assert_eq!(deliver(&playback, index, &pcm, None), PushOutcome::Queued);
        }
        let mut out = vec![1.0; 960];
        source.render(local_playout_ns(START_FRAME + FRAME_SAMPLES), &mut out);
        assert!(out.iter().all(|sample| sample.abs() < f32::EPSILON));

        deliver(&playback, 3, &pcm, Some(source_clock()));
        let mut out = vec![0.0; 960];
        source.render(
            local_playout_ns(START_FRAME + 3 * FRAME_SAMPLES + 100),
            &mut out,
        );
        assert!(
            (out[400] - 0.4).abs() < 0.005,
            "plays once the clock is known"
        );
    }

    #[test]
    fn each_route_plays_its_own_channel_role() {
        let streams = FakeStreams::default();
        let left = start_playback(&streams, "Left speaker", ChannelRole::Left).unwrap();
        let right = start_playback(&streams, "Right speaker", ChannelRole::Right).unwrap();
        let (left_config, mut left_source) = streams.take();
        let (right_config, mut right_source) = streams.take();
        assert_eq!((left_config.channels, right_config.channels), (1, 1));
        let pcm = level_pcm(0.3, -0.2);
        for playback in [&left, &right] {
            for index in 0..3 {
                deliver(playback, index, &pcm, Some(source_clock()));
            }
        }
        let now = local_playout_ns(START_FRAME + FRAME_SAMPLES);
        let mut left_out = vec![0.0; 480];
        let mut right_out = vec![0.0; 480];
        left_source.render(now, &mut left_out);
        right_source.render(now, &mut right_out);
        assert!((left_out[240] - 0.3).abs() < 0.005, "{}", left_out[240]);
        assert!((right_out[240] + 0.2).abs() < 0.005, "{}", right_out[240]);
    }

    #[test]
    fn a_group_member_plays_on_its_output_with_its_calibration() {
        let streams = FakeStreams::default();
        let group = GroupPlayback {
            sink_serial: 77,
            role: ChannelRole::Left,
            trim_ns: 0,
            gain_db: -6.0,
            eq: Vec::new(),
        };
        let playback = start_group_playback(&streams, "Speaker group", &group).unwrap();
        let (config, mut source) = streams.take();
        assert_eq!(
            config.target_serial,
            Some(77),
            "pinned to the member's output"
        );
        assert_eq!(config.channels, 1);
        let pcm = level_pcm(0.4, -0.2);
        for index in 0..3 {
            deliver(&playback, index, &pcm, Some(source_clock()));
        }
        let mut out = vec![0.0; 480];
        source.render(local_playout_ns(START_FRAME + FRAME_SAMPLES), &mut out);
        let expected = 0.4 * 10.0_f32.powf(-6.0 / 20.0);
        assert!((out[240] - expected).abs() < 0.005, "{}", out[240]);
    }

    #[test]
    fn the_room_eq_cuts_only_its_band() {
        let group = GroupPlayback {
            sink_serial: 1,
            role: ChannelRole::Mono,
            trim_ns: 0,
            gain_db: 0.0,
            eq: vec![media::calibration::EqBand {
                center_hz: 60.0,
                gain_db: -12.0,
                q: 2.0,
            }],
        };
        let level = |hz: f64| {
            let mut filter = PostFilter::new(&group);
            let mut peak = 0.0_f32;
            for index in 0..48_000_u32 {
                let phase = std::f64::consts::TAU * hz * f64::from(index) / 48_000.0;
                let mut sample = [phase.sin() as f32 * 0.5];
                filter.apply(&mut sample);
                if index > 24_000 {
                    peak = peak.max(sample[0].abs());
                }
            }
            peak
        };
        assert!(level(60.0) < 0.2, "the room mode is cut: {}", level(60.0));
        assert!(
            (level(2_000.0) - 0.5).abs() < 0.02,
            "the midrange is untouched"
        );
    }

    #[test]
    fn a_route_switches_role_and_keeps_its_clock() {
        let streams = FakeStreams::default();
        let source = DeviceId::new("source").unwrap();
        let target = DeviceId::new("target").unwrap();
        let mut route = inbound(&source, &target, Instant::now());
        let playback = start_playback(&streams, "Podcast", ChannelRole::Stereo).unwrap();
        playback.shared.lock().clock = Some(source_clock());
        route.playback = Some(playback);
        let state = Mutex::new(RemoteAudioState::default());
        state.lock().unwrap().inbound.insert([1; 16], route);

        set_role_with(&state, &streams, [1; 16], ChannelRole::Left).unwrap();
        let guard = state.lock().unwrap();
        let playback = guard.inbound[&[1; 16]].playback.as_ref().unwrap();
        let role = playback.role;
        let shared = playback.shared.clone();
        drop(guard);
        let playout = shared.lock();
        let playout_role = playout.playout.config().role;
        let clock = playout.clock;
        drop(playout);
        assert_eq!(role, ChannelRole::Left);
        assert_eq!(playout_role, ChannelRole::Left);
        assert_eq!(clock, Some(source_clock()));
        let channels = streams
            .opened
            .lock()
            .unwrap()
            .iter()
            .map(|(config, _)| config.channels)
            .collect::<Vec<_>>();
        assert_eq!(channels, vec![2, 1], "a one-sided role plays mono");
        set_role_with(&state, &streams, [1; 16], ChannelRole::Left).unwrap();
        assert_eq!(
            streams.opened.lock().unwrap().len(),
            2,
            "choosing the current role changes nothing"
        );
        assert!(set_role_with(&state, &streams, [9; 16], ChannelRole::Right).is_err());
    }

    #[test]
    fn a_failed_playback_stream_rejects_frames_so_the_route_is_interrupted() {
        let streams = FakeStreams {
            failing: true,
            ..FakeStreams::default()
        };
        let playback = start_playback(&streams, "Podcast", ChannelRole::Stereo).unwrap();
        let pcm = level_pcm(0.1, 0.1);
        let (media_ts, presentation_ns) = frame_schedule(START_FRAME).unwrap();
        let outcome = ingest_frame(
            &playback,
            &ReceivedFrame {
                sequence: 1,
                media_ts,
                presentation_ns,
                pcm_s16le: &pcm,
            },
            Some(source_clock()),
        );
        assert_eq!(outcome, Err("the device was unplugged".into()));
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
                        playout: None,
                    }
        }));
        assert!(egress.iter().any(|message| {
            message.target == source
                && message.action
                    == PersonalFabricAction::RemoteAudioKeepalive {
                        session_id: [3; 16],
                        authorization: [5; 16],
                        playout: Some(ClockSyncState::Ready),
                    }
        }));
        assert!(due_keepalives(&mut state, now).is_empty());
    }

    #[test]
    fn an_approved_route_shows_the_players_clock_instead_of_streaming_silence() {
        let target = DeviceId::new("target").unwrap();
        let now = Instant::now();
        let mut state = RemoteAudioState::default();
        state.outbound.insert(
            [1; 16],
            outbound(&target, LocalAudioSelection::System, Some([9; 16]), now),
        );
        let phase = |state: &RemoteAudioState| route_views(state, false)[0].phase;
        assert_eq!(
            phase(&state),
            RemoteAudioPhase::SyncingClock,
            "nothing is known to play before the target reports its clock"
        );
        let route = state.outbound.get_mut(&[1; 16]).unwrap();
        route.peer_playout = Some(ClockSyncState::Failed(ClockSyncFailure::NoReply));
        assert_eq!(
            phase(&state),
            RemoteAudioPhase::ClockFailed(ClockSyncFailure::NoReply)
        );
        state.outbound.get_mut(&[1; 16]).unwrap().peer_playout = Some(ClockSyncState::Ready);
        assert_eq!(phase(&state), RemoteAudioPhase::Streaming);
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

    fn chunk(first_frame: i64, frames: usize, byte: u8) -> CaptureChunk {
        CaptureChunk {
            first_frame,
            pcm_s16le: vec![byte; frames * REMOTE_AUDIO_SAMPLE_FRAME_BYTES],
        }
    }

    #[test]
    fn contiguous_capture_is_framed_with_continuous_timestamps() {
        let mut pending = PendingAudio::default();
        pending.push(chunk(1_000, 700, 1));
        assert_eq!(pending.next_frame(), None, "less than a frame waits");
        pending.push(chunk(1_700, 700, 2));
        let (first, pcm) = pending.next_frame().unwrap();
        assert_eq!(first, 1_000);
        assert_eq!(pcm.len(), MAX_REMOTE_AUDIO_FRAME_BYTES);
        assert_eq!(pending.next_frame(), None);
        pending.push(chunk(2_400, 520, 3));
        assert_eq!(pending.next_frame().map(|(first, _)| first), Some(1_960));
    }

    #[test]
    fn a_capture_gap_sends_the_run_before_it_short_and_restarts_at_the_new_timestamp() {
        let mut pending = PendingAudio::default();
        pending.push(chunk(1_000, 1_200, 1));
        pending.push(chunk(5_000, 960, 2));
        let frames = std::iter::from_fn(|| pending.next_frame())
            .map(|(first, pcm)| (first, pcm.len() / REMOTE_AUDIO_SAMPLE_FRAME_BYTES, pcm[0]))
            .collect::<Vec<_>>();
        assert_eq!(
            frames,
            vec![(1_000, 960, 1), (1_960, 240, 1), (5_000, 960, 2)]
        );
    }

    #[test]
    fn pending_audio_keeps_only_the_newest_bounded_audio() {
        let frames = MAX_PENDING_PCM_BYTES / REMOTE_AUDIO_SAMPLE_FRAME_BYTES;
        let offset = i64::try_from(frames).unwrap();
        let mut pending = PendingAudio::default();
        pending.push(chunk(0, frames, 1));
        pending.push(chunk(offset, frames, 2));
        assert_eq!(pending.len(), MAX_PENDING_PCM_BYTES);
        let (first, pcm) = pending.next_frame().unwrap();
        assert_eq!(first, offset, "the dropped audio's timestamps went with it");
        assert!(pcm.iter().all(|byte| *byte == 2));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn audio_route_revocation_hits_only_the_exact_affected_routes() {
        // Phase `policy_disable_revokes_pending_and_outbound_routes`.
        {
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

        // Phase `peer_and_focus_loss_revoke_only_the_exact_routes`.
        {
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

        // Phase `stale_consent_is_denied_and_fresh_consent_remains`.
        {
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
            let egress =
                reconcile_lifecycle(&mut state, true, &BTreeSet::from([source]), None, now);
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

        // Phase `transport_failure_interrupts_only_its_exact_route`.
        {
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

        // Phase `capture_failure_stops_the_exact_authorized_peer_route`.
        {
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
}
