//! Native-session bridge between compositor state and the personal fabric.
//!
//! The bridge deliberately samples stable state rather than putting network or
//! hardware discovery on the render path. Remote clipboard entries become
//! history, not the live Wayland selection, until explicitly activated.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use rand_core::{OsRng, RngCore};
use serde_json::{Map, Value};

use crate::compositor::selection::{
    SharedSelection, merge_shared_clipboard_history, shareable_clipboard_history,
};
use crate::shell::device_fabric::{
    ClipboardEntry, ClipboardPayload, DeviceFabricSnapshot, DeviceKind, DevicePresence,
    DisplayPlacement, DisplayRect, FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricInputEvent,
    FabricStamp, InputLease, MAX_INPUT_LEASE_MS, MediaSession, MediaSourceLocator,
    PersonalFabricAction, RoamingSetting,
};
use crate::shell::network::ShellNetwork;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const PRESENCE_HEARTBEAT: Duration = Duration::from_secs(10);

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
    placement_override: Option<DisplayRect>,
}

fn spatial_input_state() -> &'static Mutex<SpatialInputState> {
    static STATE: OnceLock<Mutex<SpatialInputState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SpatialInputState::default()))
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
}

impl DeviceFabricRuntime {
    pub fn new(network: &ShellNetwork) -> Self {
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
        }
    }

    /// Sample local state and project authenticated remote state into the OS.
    pub fn pump(
        &mut self,
        network: &ShellNetwork,
        selection: &SharedSelection,
        local: LocalFabricCapabilities,
    ) {
        self.handle_fabric_actions(network);
        self.acknowledge_opened_media(network);
        let now = Instant::now();
        if self
            .last_sample
            .is_some_and(|sample| now.duration_since(sample) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.last_sample = Some(now);
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
        self.merge_remote_state(network, selection, &device);
        if let Err(error) = network.refresh_wifi_credential_recipients() {
            tracing::warn!(%error, "could not refresh per-device Wi-Fi credential envelopes");
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
    ) {
        let fabric = network.device_fabric();
        let history = fabric.merged_clipboard();
        let entries = history.entries().cloned().collect::<Vec<_>>();
        merge_shared_clipboard_history(selection, &entries);

        let records = fabric.merged_setting_records();
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

    fn handle_fabric_actions(&mut self, network: &ShellNetwork) {
        prune_spatial_leases();
        for envelope in network.take_personal_fabric_actions() {
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
                    let Some((local, _)) = network.local_device_fabric_identity() else {
                        continue;
                    };
                    let Some(presence) = network.device_fabric().snapshots().get(&local).cloned()
                    else {
                        continue;
                    };
                    let capable = if has_video {
                        presence.presence.capabilities.video_output
                    } else {
                        presence.presence.capabilities.audio_output
                    };
                    let Some(room) = crate::shell::state::RoomId::parse(&workspace_id) else {
                        continue;
                    };
                    if !capable {
                        continue;
                    }
                    if crate::shell::launcher::launch_media_handoff(
                        &app_id,
                        room,
                        &locator,
                        &title,
                        envelope.request_id,
                        position_ms,
                        resume_playing,
                    )
                    .is_ok()
                    {
                        self.pending_incoming_media
                            .insert(envelope.request_id, (envelope.source, session_id));
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
                action => handle_spatial_action(envelope.source, &envelope.target, action),
            }
        }
    }

    fn acknowledge_opened_media(&mut self, network: &ShellNetwork) {
        for request_id in media::handoff::accepted_handoff_requests() {
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

fn prune_spatial_leases() {
    if let Ok(mut spatial) = spatial_input_state().lock() {
        let now = unix_ms();
        spatial
            .issued
            .retain(|_, lease| lease.validate_at(now).is_ok());
        spatial
            .received
            .retain(|_, lease| lease.validate_at(now).is_ok());
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
        }
        PersonalFabricAction::InputLeaseGranted { lease } => {
            if lease.validate_at(unix_ms()).is_ok() {
                spatial.received.insert(source, lease);
            }
        }
        PersonalFabricAction::InputLeaseRevoked { nonce } => {
            spatial
                .received
                .retain(|device, lease| device != &source || lease.nonce != nonce);
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
        PersonalFabricAction::ResumeMedia { .. } | PersonalFabricAction::MediaAccepted { .. } => {}
    }
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
    let report = media::handoff::playback_report(session_id)
        .ok_or_else(|| "that media session is no longer available".to_string())?;
    let transfer = network
        .device_fabric()
        .handoff_media(session_id, target)
        .map_err(|error| error.to_string())?;
    let locator = match &report.source {
        media::handoff::HandoffSource::Workspace { path } => MediaSourceLocator::Workspace {
            path: path.to_string_lossy().into_owned(),
        },
        media::handoff::HandoffSource::RemoteStream { url } => {
            MediaSourceLocator::RemoteStream { url: url.clone() }
        }
        media::handoff::HandoffSource::LocalFile { .. } => {
            return Err(
                "This file is device-local. Add it to Files to hydrate it on another device."
                    .into(),
            );
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
    media::handoff::remember_outbound_handoff(request_id, report);
    Ok(request_id)
}

/// Build the bounded presentation model for Quick Settings. Every remote row
/// comes from a snapshot authored by that certified device; local `PipeWire`
/// outputs are deliberately kept separate from personal-fabric identities.
pub fn device_fabric_view(network: &ShellNetwork) -> crate::shell::overlay_host::DeviceFabricView {
    use crate::shell::overlay_host::{
        DeviceFabricView, FabricDeviceView, FabricMediaView, LocalMediaOutputView,
    };

    let fabric = network.device_fabric();
    let local_id = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let now = unix_ms();
    let (issued_to, received_from) = spatial_input_state().lock().map_or_else(
        |_| (Vec::new(), Vec::new()),
        |spatial| {
            (
                spatial
                    .issued
                    .values()
                    .filter(|lease| lease.validate_at(now).is_ok())
                    .map(|lease| lease.controller.clone())
                    .collect::<Vec<_>>(),
                spatial
                    .received
                    .values()
                    .filter(|lease| lease.validate_at(now).is_ok())
                    .map(|lease| lease.target.clone())
                    .collect::<Vec<_>>(),
            )
        },
    );
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

    let local_media = local_id
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
        .unwrap_or_default();

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

    DeviceFabricView {
        devices,
        local_media,
        local_outputs,
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

/// Place a certified display immediately to the right of this device. The
/// receiving device authors the resulting placement in its own next snapshot.
///
/// # Errors
///
/// Returns an error if either display is unavailable or the exact-target send
/// fails.
pub fn place_device_right(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
) -> Result<(), String> {
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
    let target_rect = fabric
        .snapshots()
        .get(target)
        .and_then(|snapshot| snapshot.display.as_ref())
        .map(|display| display.rect)
        .ok_or_else(|| "the target has no published display".to_string())?;
    let rect = DisplayRect {
        x: local_rect.x.saturating_add_unsigned(local_rect.width),
        y: local_rect.y,
        width: target_rect.width,
        height: target_rect.height,
    };
    network
        .send_personal_fabric_action(target, PersonalFabricAction::SetDisplayPlacement { rect })
        .map(|_| ())
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
        spatial
            .issued
            .retain(|_, current| current.controller != *controller);
        spatial.issued.insert(nonce, lease.clone());
    }
    Ok(lease)
}

/// Revoke every active lease this device issued to `controller` and notify it
/// so edge forwarding stops immediately on both sides.
///
/// # Errors
///
/// Returns an error if no lease exists or a revoke cannot be delivered.
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
    for lease in &leases {
        network.send_personal_fabric_action(
            controller,
            PersonalFabricAction::InputLeaseRevoked { nonce: lease.nonce },
        )?;
    }
    if let Ok(mut spatial) = spatial_input_state().lock() {
        spatial
            .issued
            .retain(|_, lease| &lease.controller != controller);
    }
    Ok(())
}

/// Send one lease-bound input event to an adjacent certified target.
///
/// # Errors
///
/// Returns an error if the target has not granted control, the lease expired,
/// its capability excludes the event, or transport delivery fails.
pub fn send_spatial_input(
    network: &ShellNetwork,
    target: &crate::shell::personal_continuity::DeviceId,
    event: FabricInputEvent,
) -> Result<(), String> {
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
    network
        .send_personal_fabric_action(
            target,
            PersonalFabricAction::Input {
                lease_nonce: lease.nonce,
                event,
            },
        )
        .map(|_| ())
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

fn build_local_snapshot(
    device: &crate::shell::personal_continuity::DeviceId,
    label: String,
    local: LocalFabricCapabilities,
    selection: &SharedSelection,
    previous: Option<&DeviceFabricSnapshot>,
    applied_remote_settings: &BTreeMap<String, Value>,
    stamp: &FabricStamp,
) -> DeviceFabricSnapshot {
    let mut snapshot = DeviceFabricSnapshot {
        schema_version: FABRIC_SCHEMA_VERSION,
        owner: device.clone(),
        presence: DevicePresence {
            device: device.clone(),
            label,
            kind: local_device_kind(),
            capabilities: FabricCapabilities {
                display: local.display.is_some(),
                pointer: local.pointer,
                keyboard: local.keyboard,
                audio_output: local.audio_output,
                video_output: local.display.is_some(),
                clipboard: true,
                roaming_settings: true,
                secret_store: true,
                app_continuation: true,
            },
            online: true,
            last_seen_unix_ms: 0,
            stamp: stamp.clone(),
        },
        media: media::handoff::playback_sessions()
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
            .collect(),
        clipboard: shareable_clipboard(device, selection, stamp.counter),
        settings: reconcile_roaming_settings(
            roaming_setting_values(),
            previous,
            applied_remote_settings,
            stamp,
        ),
        wifi: previous.map_or_else(Vec::new, |state| state.wifi.clone()),
        display: local.display.map(|rect| DisplayPlacement {
            device: device.clone(),
            rect,
            scale_milli: 1000,
            stamp: stamp.clone(),
        }),
    };
    preserve_unchanged_stamps(&mut snapshot, previous);
    snapshot
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
    let document = settings_store::load();
    let mut out = BTreeMap::new();
    for section in [
        "accessibility",
        "appearance",
        "region",
        "notifications",
        "input",
        "audio",
    ] {
        let Some(Value::Object(values)) = document.get(section) else {
            continue;
        };
        for (key, value) in values {
            if section == "appearance" && matches!(key.as_str(), "wallpaper" | "wallpaper_path") {
                continue;
            }
            if section == "audio" && !matches!(key.as_str(), "balance" | "mono_output") {
                continue;
            }
            out.insert(format!("qos.{section}.{key}"), value.clone());
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
        let Some(rest) = key.strip_prefix("qos.") else {
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
                key: "qos.appearance.dark".into(),
                value: Value::Bool(false),
                stamp: old_stamp,
            }],
            wifi: Vec::new(),
            display: None,
        };
        let next_stamp = FabricStamp { counter: 9, device };
        let applied_remote = BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(true))]);

        let received = reconcile_roaming_settings(
            BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(true))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert_eq!(received, previous.settings);

        let edited = reconcile_roaming_settings(
            BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(false))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert_eq!(edited[0].stamp, next_stamp);
        assert_eq!(edited[0].value, Value::Bool(false));

        previous.settings.clear();
        let bootstrap = reconcile_roaming_settings(
            BTreeMap::from([("qos.appearance.dark".into(), Value::Bool(true))]),
            Some(&previous),
            &applied_remote,
            &next_stamp,
        );
        assert!(bootstrap.is_empty());
    }
}
