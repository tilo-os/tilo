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
    sink: PcmSink,
}

#[derive(Default)]
struct RemoteAudioState {
    source_tokens: BTreeMap<u64, LocalAudioSelection>,
    pending: BTreeMap<[u8; 16], PendingInbound>,
    outbound: BTreeMap<[u8; 16], OutboundRoute>,
    inbound: BTreeMap<[u8; 16], InboundRoute>,
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

/// Refresh the shell-owned source picker. Tokens are process-local and are
/// replaced on every snapshot, so neither apps nor peers ever see node ids.
pub fn snapshot(focus: Option<&PhysicalGamepadBinding>) -> RemoteAudioView {
    let graph = media::audio::enumerate();
    let Ok(mut state) = state().lock() else {
        return RemoteAudioView::default();
    };
    state.source_tokens.clear();
    let system_token = random_nonzero_u64(&state.source_tokens);
    state
        .source_tokens
        .insert(system_token, LocalAudioSelection::System);
    let mut sources = vec![RemoteAudioSourceView {
        token: system_token,
        label: "All system audio".into(),
        application: false,
    }];
    if let Some(focus) = focus {
        for stream in graph.playback_streams.into_iter().take(64) {
            let token = random_nonzero_u64(&state.source_tokens);
            let label = stream.label();
            state.source_tokens.insert(
                token,
                LocalAudioSelection::Application {
                    serial: stream.serial,
                    label: label.clone(),
                    focus: focus.clone(),
                },
            );
            sources.push(RemoteAudioSourceView {
                token,
                label,
                application: true,
            });
        }
    }
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
        if state.outbound.len() + state.inbound.len() >= MAX_REMOTE_AUDIO_SESSIONS {
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
        let sink = PcmSink::start()?;
        let mut state = state()
            .lock()
            .map_err(|_| "remote audio service is unavailable".to_string())?;
        if state.outbound.len() + state.inbound.len() >= MAX_REMOTE_AUDIO_SESSIONS {
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
                sink,
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
            route
                .authorization
                .map(|authorization| (route.target, authorization))
        } else {
            state
                .inbound
                .remove(&session_id)
                .map(|route| (route.source, route.authorization))
        }
    };
    let Some((peer, authorization)) = stopped else {
        return Err("that remote audio route is no longer active".into());
    };
    network
        .send_personal_fabric_action(
            &peer,
            PersonalFabricAction::RemoteAudioStop {
                session_id,
                authorization,
            },
        )
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
                && state.pending.len() < MAX_REMOTE_AUDIO_SESSIONS
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
            if let Ok(mut state) = state().lock()
                && let Some(route) = state.inbound.get_mut(session_id)
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
                && route.sink.try_write(pcm_s16le.clone()).is_ok()
            {
                route.last_sequence = *sequence;
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
    if !enabled {
        for (session_id, pending) in &guard.pending {
            egress.push((
                pending.source.clone(),
                PersonalFabricAction::RemoteAudioDecision {
                    session_id: *session_id,
                    accepted: false,
                    authorization: None,
                },
            ));
        }
        for (session_id, route) in &guard.outbound {
            if let Some(authorization) = route.authorization {
                egress.push((
                    route.target.clone(),
                    PersonalFabricAction::RemoteAudioStop {
                        session_id: *session_id,
                        authorization,
                    },
                ));
            }
        }
        for (session_id, route) in &guard.inbound {
            egress.push((
                route.source.clone(),
                PersonalFabricAction::RemoteAudioStop {
                    session_id: *session_id,
                    authorization: route.authorization,
                },
            ));
        }
        guard.pending.clear();
        guard.outbound.clear();
        guard.inbound.clear();
        drop(guard);
        for (target, action) in egress {
            let _ = network.send_personal_fabric_action(&target, action);
        }
        return;
    }
    guard.pending.retain(|_, pending| {
        now.duration_since(pending.received) <= PENDING_TTL && online.contains(&pending.source)
    });
    guard.inbound.retain(|_, route| {
        now.duration_since(route.created) <= SESSION_TTL && online.contains(&route.source)
    });
    let sessions = guard.outbound.keys().copied().collect::<Vec<_>>();
    for session_id in sessions {
        let Some(route) = guard.outbound.get_mut(&session_id) else {
            continue;
        };
        let focus_valid = selection_focus_valid(&route.selection, focus);
        if now.duration_since(route.created) > SESSION_TTL
            || !online.contains(&route.target)
            || !focus_valid
        {
            if let Some(authorization) = route.authorization {
                egress.push((
                    route.target.clone(),
                    PersonalFabricAction::RemoteAudioStop {
                        session_id,
                        authorization,
                    },
                ));
            }
            guard.outbound.remove(&session_id);
            continue;
        }
        let Some(capture) = route.capture.as_mut() else {
            continue;
        };
        match capture.drain() {
            Ok(bytes) => route.pending_pcm.extend_from_slice(&bytes),
            Err(_) => {
                guard.outbound.remove(&session_id);
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
            egress.push((
                route.target.clone(),
                PersonalFabricAction::RemoteAudioFrame {
                    session_id,
                    authorization,
                    sequence: route.sequence,
                    pcm_s16le,
                },
            ));
        }
    }
    drop(guard);
    for (target, action) in egress {
        if network
            .send_personal_fabric_action(&target, action)
            .is_err()
            && let Ok(mut state) = state().lock()
        {
            state.outbound.retain(|_, route| route.target != target);
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

    fn try_write(&self, bytes: Vec<u8>) -> Result<(), String> {
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
        let focus = PhysicalGamepadBinding {
            app_id: "dev.tilo.music".into(),
            workspace_id: "work".into(),
            session_id: "surface-1".into(),
        };
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
}
