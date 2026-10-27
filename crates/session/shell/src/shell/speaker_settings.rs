//! The Settings app's door to speaker groups.
//!
//! Settings asks through the launcher-authenticated collaboration broker
//! with [`runtime_collab::SpeakerCommand`]; this module answers from the
//! live fabric and the speaker-group runtime, and owns the calibration runs
//! Settings starts. Devices and outputs cross this boundary only as fabric
//! device ids and hex endpoint selectors.
//!
//! Settings does not ask on a timer. While it waits (the broker's
//! `speakerChanged`), each fabric pump run and each speaker command compares
//! a digest of what a snapshot would show with the last one, and moves a
//! revision when it differs; the wait resolves on that revision, which names
//! nothing, and Settings then asks for one snapshot.

use std::collections::BTreeMap;
use std::hash::{Hash as _, Hasher as _};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use runtime_collab::{
    CalibrationInfo, CalibrationMemberInfo, CalibrationMemberState, CalibrationMicrophoneInfo,
    CalibrationMicrophoneRun, CalibrationMicrophoneState, CalibrationPhaseInfo, MicrophoneStatus,
    SpeakerCommand, SpeakerDeviceInfo, SpeakerEndpointInfo, SpeakerGroupInfo, SpeakerMemberInfo,
    SpeakerMemberSpec, SpeakerMemberStatus, SpeakerPlaybackInfo, SpeakerReply, SpeakerRoleSpec,
    SpeakerSnapshot, SpeakerTrimInfo,
};

use crate::shell::device_fabric::DeviceFabric;
use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_calibration::{
    CalibrationPhase, CalibrationRun, MemberProgress, MicrophoneConsent, MicrophoneProgress,
    RunMicrophone,
};
use crate::shell::speaker_group::{
    AudioEndpointKind, EndpointSelector, RoomPoint, SpeakerGroupId, SpeakerRole, SpeakerTrim,
    StereoSide, SurroundChannel,
};
use crate::shell::speaker_group_runtime::{
    ListenerCandidate, ListenerState, MemberSpec, SpeakerGroupsView, SpeakerSessionStatus,
};

/// Shortest interval between two identify sounds on one device.
pub const IDENTIFY_INTERVAL: Duration = Duration::from_millis(800);

#[must_use]
pub fn role_from_spec(spec: SpeakerRoleSpec) -> SpeakerRole {
    match spec {
        SpeakerRoleSpec::Left => SpeakerRole::Stereo(StereoSide::Left),
        SpeakerRoleSpec::Right => SpeakerRole::Stereo(StereoSide::Right),
        SpeakerRoleSpec::Center => SpeakerRole::Surround(SurroundChannel::Center),
        SpeakerRoleSpec::Lfe => SpeakerRole::Surround(SurroundChannel::Lfe),
        SpeakerRoleSpec::SurroundLeft => SpeakerRole::Surround(SurroundChannel::SurroundLeft),
        SpeakerRoleSpec::SurroundRight => SpeakerRole::Surround(SurroundChannel::SurroundRight),
        SpeakerRoleSpec::Room { x_mm, y_mm, z_mm } => {
            SpeakerRole::RoomPoint(RoomPoint { x_mm, y_mm, z_mm })
        }
    }
}

#[must_use]
pub fn role_to_spec(role: SpeakerRole) -> SpeakerRoleSpec {
    match role {
        SpeakerRole::Stereo(StereoSide::Left) => SpeakerRoleSpec::Left,
        SpeakerRole::Stereo(StereoSide::Right) => SpeakerRoleSpec::Right,
        SpeakerRole::Surround(SurroundChannel::Center) => SpeakerRoleSpec::Center,
        SpeakerRole::Surround(SurroundChannel::Lfe) => SpeakerRoleSpec::Lfe,
        SpeakerRole::Surround(SurroundChannel::SurroundLeft) => SpeakerRoleSpec::SurroundLeft,
        SpeakerRole::Surround(SurroundChannel::SurroundRight) => SpeakerRoleSpec::SurroundRight,
        SpeakerRole::RoomPoint(point) => SpeakerRoleSpec::Room {
            x_mm: point.x_mm,
            y_mm: point.y_mm,
            z_mm: point.z_mm,
        },
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "trims are bounded to half a second and forty dB; display rounding is intended"
)]
fn trim_info(trim: &SpeakerTrim) -> SpeakerTrimInfo {
    SpeakerTrimInfo {
        delay_ms: trim.delay_ns as f64 / 1e6,
        gain_db: trim.gain_db(),
        eq_bands: u8::try_from(trim.eq.len()).unwrap_or(u8::MAX),
    }
}

fn parse_members(members: &[SpeakerMemberSpec]) -> Result<Vec<MemberSpec>, String> {
    members
        .iter()
        .map(|member| {
            Ok(MemberSpec {
                device: DeviceId::new(member.device.clone())
                    .map_err(|_| crate::shell::i18n::t("speaker-error-device-invalid"))?,
                output: EndpointSelector::from_hex(&member.output)
                    .ok_or_else(|| crate::shell::i18n::t("speaker-error-output-invalid"))?,
                role: role_from_spec(member.role),
            })
        })
        .collect()
}

fn parse_group(value: &str) -> Result<SpeakerGroupId, String> {
    SpeakerGroupId::from_hex(value)
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-group-invalid"))
}

/// The most urgent state of any session on `group`, as Settings shows it.
#[must_use]
pub fn playback_info(
    view: &SpeakerGroupsView,
    group: SpeakerGroupId,
) -> Option<SpeakerPlaybackInfo> {
    let urgency = |status: SpeakerSessionStatus| match status {
        SpeakerSessionStatus::ClockFailed(_) => 0,
        SpeakerSessionStatus::SyncingClocks => 1,
        SpeakerSessionStatus::Connecting => 2,
        SpeakerSessionStatus::Playing => 3,
    };
    let status = view
        .sessions
        .iter()
        .filter(|session| session.group == group)
        .map(crate::shell::speaker_group_runtime::SpeakerSessionView::status)
        .min_by_key(|status| urgency(*status))?;
    Some(match status {
        SpeakerSessionStatus::Playing => SpeakerPlaybackInfo::Playing,
        SpeakerSessionStatus::Connecting => SpeakerPlaybackInfo::Connecting,
        SpeakerSessionStatus::SyncingClocks => SpeakerPlaybackInfo::SyncingClocks,
        SpeakerSessionStatus::ClockFailed(reason) => SpeakerPlaybackInfo::ClockFailed {
            reason: bounded_reason(crate::shell::i18n::t(reason.message_key())),
        },
    })
}

/// Every device's endpoints as Settings shows them.
#[must_use]
pub fn devices(fabric: &DeviceFabric, local: &DeviceId) -> Vec<SpeakerDeviceInfo> {
    fabric
        .snapshots()
        .values()
        .take(runtime_collab::MAX_SPEAKER_DEVICES)
        .map(|snapshot| {
            let endpoints = |kind| {
                snapshot
                    .speakers
                    .endpoints
                    .iter()
                    .filter(|endpoint| endpoint.kind == kind)
                    .map(|endpoint| SpeakerEndpointInfo {
                        selector: endpoint.selector.to_hex(),
                        label: endpoint.label.clone(),
                        is_default: endpoint.is_default,
                    })
                    .collect::<Vec<_>>()
            };
            SpeakerDeviceInfo {
                id: snapshot.owner.as_str().to_string(),
                label: snapshot.presence.label.clone(),
                online: snapshot.presence.online,
                local: &snapshot.owner == local,
                can_join: &snapshot.owner == local || snapshot.presence.capabilities.audio_group,
                outputs: endpoints(AudioEndpointKind::Output),
                microphones: endpoints(AudioEndpointKind::Microphone),
            }
        })
        .collect()
}

/// Calibration runs this device started, keyed by group.
#[derive(Default)]
pub struct Calibrations {
    runs: BTreeMap<SpeakerGroupId, CalibrationRun>,
}

impl Calibrations {
    pub fn insert(&mut self, run: CalibrationRun) {
        self.runs.insert(run.group(), run);
    }

    pub fn get_mut(&mut self, group: SpeakerGroupId) -> Option<&mut CalibrationRun> {
        self.runs.get_mut(&group)
    }

    pub fn remove(&mut self, group: SpeakerGroupId) -> Option<CalibrationRun> {
        self.runs.remove(&group)
    }

    /// Groups of the runs still waiting for microphone consent.
    #[must_use]
    pub fn awaiting_consent(&self) -> Vec<SpeakerGroupId> {
        self.runs
            .iter()
            .filter(|(_, run)| run.phase() == CalibrationPhase::WaitingForMicrophones)
            .map(|(group, _)| *group)
            .collect()
    }

    /// Advance every run, and drop runs whose group changed or vanished.
    /// `consent` answers, per group, where each device's microphone prompt
    /// stands.
    pub fn pump(
        &mut self,
        fabric: &DeviceFabric,
        consent: &BTreeMap<SpeakerGroupId, Vec<ListenerCandidate>>,
        now: Instant,
    ) {
        self.runs.retain(|group, run| {
            fabric
                .speaker_group(*group)
                .is_some_and(|published| run.matches(published))
                || matches!(
                    run.phase(),
                    CalibrationPhase::Measuring | CalibrationPhase::WaitingForMicrophones
                )
        });
        for (group, run) in &mut self.runs {
            if let Some(candidates) = consent.get(group) {
                run.update_consent(|device| microphone_consent(candidates, device), now);
            }
            run.tick();
        }
    }

    #[must_use]
    pub fn info(&self) -> Vec<CalibrationInfo> {
        self.runs.values().map(calibration_info).collect()
    }
}

/// Where one device's microphone prompt stands for a run.
fn microphone_consent(candidates: &[ListenerCandidate], device: &DeviceId) -> MicrophoneConsent {
    match candidates
        .iter()
        .find(|candidate| &candidate.device == device)
        .map(|candidate| candidate.state)
    {
        Some(ListenerState::Ready) => MicrophoneConsent::Given,
        Some(ListenerState::NotAsked | ListenerState::AwaitingConsent) => MicrophoneConsent::Asked,
        Some(ListenerState::Declined | ListenerState::Offline | ListenerState::Unavailable)
        | None => MicrophoneConsent::Refused,
    }
}

const fn microphone_status(state: ListenerState) -> MicrophoneStatus {
    match state {
        ListenerState::Ready => MicrophoneStatus::Ready,
        ListenerState::NotAsked => MicrophoneStatus::NotAsked,
        ListenerState::AwaitingConsent => MicrophoneStatus::AwaitingConsent,
        ListenerState::Declined => MicrophoneStatus::Declined,
        ListenerState::Offline => MicrophoneStatus::Offline,
        ListenerState::Unavailable => MicrophoneStatus::Unavailable,
    }
}

fn microphone_label(candidate: &ListenerCandidate) -> String {
    format!("{} · {}", candidate.device_label, candidate.label)
}

fn saturating_u8(value: usize) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

#[must_use]
pub fn calibration_info(run: &CalibrationRun) -> CalibrationInfo {
    let after = run.after();
    CalibrationInfo {
        group: run.group().to_hex(),
        phase: match run.phase() {
            CalibrationPhase::WaitingForMicrophones => CalibrationPhaseInfo::WaitingForMicrophones,
            CalibrationPhase::Measuring => CalibrationPhaseInfo::Measuring,
            CalibrationPhase::Proposed => CalibrationPhaseInfo::Proposed,
            CalibrationPhase::Applied => CalibrationPhaseInfo::Applied,
            CalibrationPhase::Failed => CalibrationPhaseInfo::Failed,
        },
        members: run
            .members()
            .iter()
            .enumerate()
            .map(|(index, member)| CalibrationMemberInfo {
                device: member.device.as_str().to_string(),
                label: member.label.clone(),
                state: match run.progress().get(index) {
                    Some(MemberProgress::Measuring) => CalibrationMemberState::Measuring,
                    Some(MemberProgress::Measured(result)) => CalibrationMemberState::Measured {
                        delay_ms: i64_ms(result.arrival_ns),
                        level_db: result.level_db,
                        output_latency_ms: result.output_latency_ns.map(i64_ms),
                        acoustic_ms: result.acoustic_ns.map(i64_ms),
                    },
                    Some(MemberProgress::Failed(reason)) => CalibrationMemberState::Failed {
                        reason: reason.clone(),
                    },
                    Some(MemberProgress::Waiting) | None => CalibrationMemberState::Waiting,
                },
                before: run.before().get(index).map(trim_info).unwrap_or_default(),
                after: after.and_then(|after| after.get(index)).map(trim_info),
            })
            .collect(),
        microphones: run
            .microphones()
            .iter()
            .zip(run.microphone_progress())
            .enumerate()
            .map(|(index, (microphone, progress))| CalibrationMicrophoneRun {
                device: microphone.device.as_str().to_string(),
                label: microphone.label.clone(),
                listening: index == run.listening(),
                state: match progress {
                    MicrophoneProgress::AwaitingConsent => {
                        CalibrationMicrophoneState::AwaitingConsent
                    }
                    MicrophoneProgress::Declined => CalibrationMicrophoneState::Declined,
                    MicrophoneProgress::Listening => CalibrationMicrophoneState::Listening,
                    MicrophoneProgress::Used { heard, rejected } => {
                        CalibrationMicrophoneState::Used {
                            heard: saturating_u8(*heard),
                            rejected: saturating_u8(*rejected),
                        }
                    }
                    MicrophoneProgress::NotHeard => CalibrationMicrophoneState::NotHeard,
                    MicrophoneProgress::Failed(reason) => CalibrationMicrophoneState::Failed {
                        reason: reason.clone(),
                    },
                },
            })
            .collect(),
        listening_label: run
            .microphones()
            .get(run.listening())
            .map(|microphone| microphone.label.clone()),
        message: run.message().map(str::to_string),
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "arrival delays are bounded to a fraction of a second"
)]
fn i64_ms(ns: i64) -> f64 {
    ns as f64 / 1e6
}

struct State {
    calibrations: Calibrations,
    last_identify: Option<Instant>,
    /// The group this device plays everything on, and the session doing
    /// it: the speaker set chosen as the output in Sound.
    output: Option<(SpeakerGroupId, [u8; 16])>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            calibrations: Calibrations::default(),
            last_identify: None,
            output: None,
        })
    })
}

/// Whether an identify sound may play now, recording it if so.
pub(crate) fn admit_identify(now: Instant) -> bool {
    let Ok(mut state) = state().lock() else {
        return false;
    };
    if state
        .last_identify
        .is_some_and(|last| now.saturating_duration_since(last) < IDENTIFY_INTERVAL)
    {
        return false;
    }
    state.last_identify = Some(now);
    true
}

/// Advance calibration runs; called from the fabric runtime's pump.
pub(crate) fn pump(network: &crate::shell::network::ShellNetwork) {
    let fabric = network.device_fabric();
    let waiting = state()
        .lock()
        .map(|state| state.calibrations.awaiting_consent())
        .unwrap_or_default();
    let consent = waiting
        .into_iter()
        .filter_map(|group| {
            let published = fabric.speaker_group(group)?;
            Some((
                group,
                crate::shell::speaker_group_runtime::listener_candidates(network, published),
            ))
        })
        .collect::<BTreeMap<_, _>>();
    if let Ok(mut state) = state().lock() {
        state.calibrations.pump(&fabric, &consent, Instant::now());
    }
}

fn snapshot(network: &crate::shell::network::ShellNetwork) -> SpeakerSnapshot {
    let media_enabled = crate::shell::device_fabric_runtime::require_speaker_groups().is_ok();
    let Some((local, _)) = network.local_device_fabric_identity() else {
        return SpeakerSnapshot {
            media_enabled,
            ..SpeakerSnapshot::default()
        };
    };
    if !media_enabled {
        return SpeakerSnapshot::default();
    }
    let fabric = network.device_fabric();
    let view = crate::shell::speaker_group_runtime::view(network);
    let output = output_group(&view);
    let groups = view
        .groups
        .iter()
        .take(runtime_collab::MAX_SPEAKER_GROUPS)
        .map(|group| SpeakerGroupInfo {
            id: group.id.to_hex(),
            name: group.name.clone(),
            leader_label: group.leader_label.clone(),
            led_here: group.led_locally,
            members: group
                .members
                .iter()
                .map(|member| SpeakerMemberInfo {
                    device: member.device.as_str().to_string(),
                    device_label: member.device_label.clone(),
                    output: member.output.to_hex(),
                    output_label: member.output_label.clone(),
                    role: role_to_spec(member.role),
                    status: member.state,
                    trim: trim_info(&member.trim),
                })
                .collect(),
            playback: playback_info(&view, group.id),
            output_here: output == Some(group.id),
            microphones: fabric
                .speaker_group(group.id)
                .filter(|_| group.led_locally)
                .map(|published| {
                    crate::shell::speaker_group_runtime::listener_candidates(network, published)
                })
                .unwrap_or_default()
                .iter()
                .take(runtime_collab::MAX_SPEAKER_DEVICES)
                .map(|candidate| CalibrationMicrophoneInfo {
                    device: candidate.device.as_str().to_string(),
                    device_label: candidate.device_label.clone(),
                    label: candidate.label.clone(),
                    local: candidate.local,
                    status: microphone_status(candidate.state),
                })
                .collect(),
        })
        .collect();
    let calibrations = state()
        .lock()
        .map_or_else(|_| Vec::new(), |state| state.calibrations.info());
    SpeakerSnapshot {
        media_enabled,
        devices: devices(&fabric, &local),
        groups,
        calibrations,
    }
}

/// When what Settings sees may have changed: a revision, and the digest of
/// the view it was last moved for.
struct ViewRevision {
    /// Starts at 1, so a first wait (from 0) answers at once and fetches
    /// the first view.
    changes: watch::Sender<u64>,
    /// The digest of the view last served or observed; `None` before the
    /// first.
    observed: Mutex<Option<u64>>,
}

impl ViewRevision {
    fn new() -> Self {
        Self {
            changes: watch::Sender::new(1),
            observed: Mutex::new(None),
        }
    }

    /// Whether a wait is pending: only then would anyone hear of a change.
    fn waited(&self) -> bool {
        self.changes.receiver_count() > 0
    }

    /// Record `snapshot` as what Settings sees now, and move the revision
    /// when it differs from what was seen before. The first one moves
    /// nothing: no view was served before it, and a first wait answers at
    /// once anyway.
    fn note(&self, snapshot: &SpeakerSnapshot) {
        let digest = view_digest(snapshot);
        let changed = self
            .observed
            .lock()
            .is_ok_and(|mut observed| observed.replace(digest).is_some_and(|last| last != digest));
        if changed {
            self.changes
                .send_modify(|revision| *revision = revision.wrapping_add(1).max(1));
        }
    }

    /// Resolves once the revision moves past `after`, with the revision to
    /// wait from next. Subscribed now, not when first polled, so a change
    /// in between is still seen.
    fn wait(&self, after: u64) -> impl std::future::Future<Output = u64> + Send + 'static {
        let mut changes = self.changes.subscribe();
        async move {
            loop {
                let current = *changes.borrow_and_update();
                if current != after {
                    return current;
                }
                if changes.changed().await.is_err() {
                    // The revision outlives every wait and never closes.
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

/// The revision Settings' waits follow.
fn revision() -> &'static ViewRevision {
    static REVISION: OnceLock<ViewRevision> = OnceLock::new();
    REVISION.get_or_init(ViewRevision::new)
}

/// A digest of everything a snapshot shows: two snapshots that would look
/// different to Settings never share one (but for a hash collision).
fn view_digest(snapshot: &SpeakerSnapshot) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    // A snapshot that fails to encode cannot be sent either; it hashes as
    // empty, like a refusal.
    serde_json::to_vec(snapshot)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// Compare what a snapshot would show now with what was last seen, and
/// announce a change when it differs. Only while Settings waits: with no
/// wait pending nobody would hear, and the snapshot is not built at all.
/// Called from each fabric pump run and after each speaker command.
pub(crate) fn observe(network: &crate::shell::network::ShellNetwork) {
    if revision().waited() {
        revision().note(&snapshot(network));
    }
}

/// Resolves once what a snapshot would show may have changed since `after`,
/// with the revision to wait from next. With speaker groups off and no
/// device changing it stays pending and costs no wakeup.
pub(crate) fn changed(
    network: &crate::shell::network::ShellNetwork,
    after: u64,
) -> impl std::future::Future<Output = u64> + Send + 'static {
    let wait = revision().wait(after);
    // Nothing was observed while no wait was pending: look once now.
    observe(network);
    wait
}

#[must_use]
pub fn handle(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    command: SpeakerCommand,
) -> SpeakerReply {
    let result = match command {
        SpeakerCommand::Snapshot => {
            let snapshot = snapshot(network);
            // What Settings is shown now: a change it has not seen moves
            // the revision, so every other waiting window asks too.
            revision().note(&snapshot);
            return SpeakerReply::Snapshot { snapshot };
        }
        command => {
            let result = crate::shell::device_fabric_runtime::require_speaker_groups()
                .map_err(|_| crate::shell::i18n::t("speaker-error-media-off"))
                .and_then(|()| run_command(network, command));
            // A create, edit, calibration step or output change moves the
            // view now, without waiting for a compositor turn.
            observe(network);
            result
        }
    };
    match result {
        Ok(group) => SpeakerReply::Done { group },
        Err(reason) => SpeakerReply::Rejected {
            reason: bounded_reason(reason),
        },
    }
}

fn bounded_reason(mut reason: String) -> String {
    if reason.is_empty() {
        reason = crate::shell::i18n::t("speaker-error-request-failed");
    }
    while reason.len() > runtime_collab::MAX_SPEAKER_LABEL_BYTES {
        reason.pop();
    }
    reason
}

fn run_command(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    command: SpeakerCommand,
) -> Result<Option<String>, String> {
    use crate::shell::speaker_group_runtime as groups;
    match command {
        SpeakerCommand::Snapshot => Ok(None),
        SpeakerCommand::Create { name, members } => {
            groups::create_group(network, &name, &parse_members(&members)?)
                .map(|group| Some(group.to_hex()))
        }
        SpeakerCommand::Update {
            group,
            name,
            members,
        } => {
            let id = parse_group(&group)?;
            groups::update_group(network, id, &name, &parse_members(&members)?)
                .map(|()| Some(group))
        }
        SpeakerCommand::Delete { group } => {
            let id = parse_group(&group)?;
            if let Ok(mut state) = state().lock() {
                state.calibrations.remove(id);
            }
            groups::delete_group(network, id).map(|()| None)
        }
        SpeakerCommand::Identify { device, output } => {
            let device = DeviceId::new(device)
                .map_err(|_| crate::shell::i18n::t("speaker-error-device-invalid"))?;
            let output = EndpointSelector::from_hex(&output)
                .ok_or_else(|| crate::shell::i18n::t("speaker-error-output-invalid"))?;
            groups::identify(network, &device, output).map(|()| None)
        }
        SpeakerCommand::StartCalibration {
            group,
            excluded,
            listening,
        } => start_calibration(network, &group, &excluded, listening.as_deref())
            .map(|()| Some(group)),
        SpeakerCommand::CancelCalibration { group } => {
            let id = parse_group(&group)?;
            with_run(id, |run| {
                run.cancel();
                Ok(())
            })
            .map(|()| Some(group))
        }
        SpeakerCommand::ApplyCalibration { group } => {
            let id = parse_group(&group)?;
            let trims = with_run(id, |run| {
                run.proposed_trims()
                    .ok_or_else(|| crate::shell::i18n::t("speaker-error-no-calibration"))
            })?;
            groups::set_trims(network, id, &trims)?;
            with_run(id, |run| {
                run.mark_applied();
                Ok(())
            })
            .map(|()| Some(group))
        }
        SpeakerCommand::RevertCalibration { group } => {
            let id = parse_group(&group)?;
            let trims = with_run(id, |run| {
                run.previous_trims()
                    .ok_or_else(|| crate::shell::i18n::t("speaker-error-no-applied-calibration"))
            })?;
            groups::set_trims(network, id, &trims)?;
            if let Ok(mut state) = state().lock() {
                state.calibrations.remove(id);
            }
            Ok(Some(group))
        }
        SpeakerCommand::DismissCalibration { group } => {
            let id = parse_group(&group)?;
            if let Ok(mut state) = state().lock() {
                state.calibrations.remove(id);
            }
            Ok(Some(group))
        }
        SpeakerCommand::UseAsOutput { group } => {
            let id = parse_group(&group)?;
            use_as_output(network, id).map(|()| Some(group))
        }
        SpeakerCommand::StopUsingAsOutput => {
            stop_output(network);
            Ok(None)
        }
    }
}

/// The group this device plays everything on, while that session lives.
fn output_group(view: &SpeakerGroupsView) -> Option<SpeakerGroupId> {
    let mut state = state().lock().ok()?;
    let (group, session) = state.output?;
    if view
        .sessions
        .iter()
        .any(|live| live.session_id == session && !live.inbound)
    {
        Some(group)
    } else {
        // The session ended on its own (the set went away, its members
        // left): this device plays on its own outputs again.
        state.output = None;
        None
    }
}

/// Play everything this device plays on `group`, as an output chosen in
/// Sound: one such group at a time, so choosing another ends the first.
fn use_as_output(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    group: SpeakerGroupId,
) -> Result<(), String> {
    use crate::shell::speaker_group_runtime as groups;
    let view = groups::view(network);
    if output_group(&view) == Some(group) {
        return Ok(());
    }
    stop_output(network);
    let session = groups::start_session(network, group, groups::GroupAudioSource::System)?;
    if let Ok(mut state) = state().lock() {
        state.output = Some((group, session));
    }
    Ok(())
}

/// End the session playing everything here on a group, if there is one.
fn stop_output(network: &crate::shell::network::ShellNetwork) {
    let previous = state()
        .lock()
        .ok()
        .and_then(|mut state| state.output.take());
    if let Some((_, session)) = previous {
        // Already ended is as good as stopped.
        let _ = crate::shell::speaker_group_runtime::stop_session(network, session);
    }
}

fn with_run<T>(
    group: SpeakerGroupId,
    apply: impl FnOnce(&mut CalibrationRun) -> Result<T, String>,
) -> Result<T, String> {
    let mut state = state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("speaker-error-calibration-unavailable"))?;
    let result = state
        .calibrations
        .get_mut(group)
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-calibration-gone"))
        .and_then(apply);
    drop(state);
    result
}

/// Measure with the timed audio path; remote members and listening devices
/// are asked over the fabric.
fn timed_source(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    local: DeviceId,
    peers: &[DeviceId],
) -> Box<dyn crate::shell::speaker_calibration::MeasurementSource> {
    let network = std::sync::Arc::downgrade(network);
    Box::new(crate::shell::speaker_calibration::TimedMeasurement::new(
        local,
        Box::new(move |target, action| {
            let network = network
                .upgrade()
                .ok_or_else(|| crate::shell::i18n::t("speaker-error-network-unavailable"))?;
            network
                .send_personal_fabric_action(target, action)
                .map(|_| ())
        }),
        peers,
    ))
}

/// The microphones a run listens with: every candidate that is not
/// excluded and could still take part, and which of them sits at the
/// listening position (`listening`, or this device when unnamed).
///
/// # Errors
///
/// When the listening device is excluded, cannot take part, or has no
/// microphone.
pub fn plan_microphones(
    candidates: &[ListenerCandidate],
    excluded: &[DeviceId],
    listening: Option<&DeviceId>,
) -> Result<(Vec<RunMicrophone>, usize), String> {
    let microphones = candidates
        .iter()
        .filter(|candidate| !excluded.contains(&candidate.device))
        .filter_map(|candidate| {
            let consent = match candidate.state {
                ListenerState::Ready => MicrophoneConsent::Given,
                ListenerState::NotAsked | ListenerState::AwaitingConsent => {
                    MicrophoneConsent::Asked
                }
                ListenerState::Declined | ListenerState::Offline | ListenerState::Unavailable => {
                    return None;
                }
            };
            Some(RunMicrophone {
                device: candidate.device.clone(),
                microphone: candidate.microphone,
                label: microphone_label(candidate),
                consent,
            })
        })
        .collect::<Vec<_>>();
    let listening = listening.map_or_else(
        || {
            candidates
                .iter()
                .find(|candidate| candidate.local)
                .map(|candidate| candidate.device.clone())
        },
        |device| Some(device.clone()),
    );
    let index = listening
        .and_then(|device| {
            microphones
                .iter()
                .position(|microphone| microphone.device == device)
        })
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-listening-unavailable"))?;
    Ok((microphones, index))
}

fn parse_device(value: &str) -> Result<DeviceId, String> {
    DeviceId::new(value.to_string())
        .map_err(|_| crate::shell::i18n::t("speaker-error-device-invalid"))
}

fn start_calibration(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    group: &str,
    excluded: &[String],
    listening: Option<&str>,
) -> Result<(), String> {
    let id = parse_group(group)?;
    let excluded = excluded
        .iter()
        .map(|device| parse_device(device))
        .collect::<Result<Vec<_>, _>>()?;
    let listening = listening.map(parse_device).transpose()?;
    let (local, _) = network
        .local_device_fabric_identity()
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
    let fabric = network.device_fabric();
    let published = fabric
        .speaker_group(id)
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-group-unavailable"))?;
    if published.leader != local {
        return Err(crate::shell::i18n::t("speaker-error-calibrate-on-leader"));
    }
    if fabric
        .speaker_member_states(published)
        .iter()
        .any(|state| *state != SpeakerMemberStatus::Ready)
    {
        return Err(crate::shell::i18n::t("speaker-error-not-all-ready"));
    }
    let candidates = crate::shell::speaker_group_runtime::listener_candidates(network, published);
    let (microphones, listening) = plan_microphones(&candidates, &excluded, listening.as_ref())?;
    let ask = microphones
        .iter()
        .filter(|microphone| microphone.consent == MicrophoneConsent::Asked)
        .map(|microphone| (microphone.device.clone(), microphone.microphone))
        .collect::<Vec<_>>();
    crate::shell::speaker_group_runtime::invite_listeners(network, published, &ask)?;
    let labels = published
        .members
        .iter()
        .map(|member| {
            fabric.snapshots().get(&member.device).map_or_else(
                || member.device.as_str().to_string(),
                |snapshot| snapshot.presence.label.clone(),
            )
        })
        .collect::<Vec<_>>();
    let peers = published
        .members
        .iter()
        .map(|member| member.device.clone())
        .chain(
            microphones
                .iter()
                .map(|microphone| microphone.device.clone()),
        )
        .collect::<Vec<_>>();
    let run = CalibrationRun::start(
        published,
        &labels,
        microphones,
        listening,
        timed_source(network, local, &peers),
        Box::new(crate::random::nonzero::<16>),
        Instant::now(),
    )?;
    state()
        .lock()
        .map_err(|_| crate::shell::i18n::t("speaker-error-calibration-unavailable"))?
        .calibrations
        .insert(run);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_survives_the_settings_wire() {
        for spec in [
            SpeakerRoleSpec::Left,
            SpeakerRoleSpec::Right,
            SpeakerRoleSpec::Center,
            SpeakerRoleSpec::Lfe,
            SpeakerRoleSpec::SurroundLeft,
            SpeakerRoleSpec::SurroundRight,
            SpeakerRoleSpec::Room {
                x_mm: -1200,
                y_mm: 2400,
                z_mm: 300,
            },
        ] {
            assert_eq!(role_to_spec(role_from_spec(spec)), spec);
        }
    }

    #[test]
    fn member_specs_reject_node_shaped_outputs() {
        let member = |output: &str| SpeakerMemberSpec {
            device: "desk".into(),
            output: output.into(),
            role: SpeakerRoleSpec::Left,
        };
        assert!(parse_members(&[member(&"ab".repeat(16))]).is_ok());
        assert!(parse_members(&[member("57")]).is_err());
        assert!(parse_members(&[member(&"0".repeat(32))]).is_err());
    }

    #[test]
    fn identify_sounds_are_rate_limited() {
        let now = Instant::now() + Duration::from_secs(3600);
        assert!(admit_identify(now));
        assert!(!admit_identify(now + Duration::from_millis(100)));
        assert!(admit_identify(now + IDENTIFY_INTERVAL));
    }

    #[test]
    fn reasons_are_bounded_for_the_wire() {
        assert_eq!(bounded_reason(String::new()), "the speaker request failed");
        assert_eq!(bounded_reason("x".repeat(1000)).len(), 256);
    }

    /// A desk leading a stereo pair with a shelf that has said yes.
    fn pair() -> SpeakerSnapshot {
        let device = |id: &str| SpeakerDeviceInfo {
            id: id.into(),
            label: id.into(),
            online: true,
            local: id == "desk",
            can_join: true,
            outputs: vec![SpeakerEndpointInfo {
                selector: "0a".repeat(16),
                label: "Speakers".into(),
                is_default: true,
            }],
            microphones: Vec::new(),
        };
        let member = |id: &str, role| SpeakerMemberInfo {
            device: id.into(),
            device_label: id.into(),
            output: "0a".repeat(16),
            output_label: Some("Speakers".into()),
            role,
            status: SpeakerMemberStatus::Ready,
            trim: SpeakerTrimInfo::default(),
        };
        SpeakerSnapshot {
            media_enabled: true,
            devices: vec![device("desk"), device("shelf")],
            groups: vec![SpeakerGroupInfo {
                id: "cd".repeat(16),
                name: "Living room".into(),
                leader_label: "desk".into(),
                led_here: true,
                members: vec![
                    member("desk", SpeakerRoleSpec::Left),
                    member("shelf", SpeakerRoleSpec::Right),
                ],
                playback: None,
                microphones: Vec::new(),
                output_here: false,
            }],
            calibrations: vec![CalibrationInfo {
                group: "cd".repeat(16),
                phase: CalibrationPhaseInfo::WaitingForMicrophones,
                members: Vec::new(),
                microphones: Vec::new(),
                listening_label: None,
                message: None,
            }],
        }
    }

    /// Poll `wait` once: its answer if it has one now.
    fn answered(
        wait: &mut std::pin::Pin<Box<impl std::future::Future<Output = u64>>>,
    ) -> Option<u64> {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match std::future::Future::poll(wait.as_mut(), &mut cx) {
            std::task::Poll::Ready(change) => Some(change),
            std::task::Poll::Pending => None,
        }
    }

    #[test]
    fn a_first_wait_answers_at_once_and_a_seen_view_waits() {
        let revision = ViewRevision::new();
        assert!(!revision.waited(), "nobody waits, so nothing is observed");
        let first = answered(&mut Box::pin(revision.wait(0))).expect("a first wait answers");
        let mut wait = Box::pin(revision.wait(first));
        assert!(revision.waited(), "a pending wait is heard");
        revision.note(&pair());
        assert_eq!(
            answered(&mut wait),
            None,
            "the first view served moves nothing"
        );
        revision.note(&pair());
        revision.note(&pair());
        assert_eq!(answered(&mut wait), None, "an unchanged view never wakes");
        drop(wait);
        assert!(!revision.waited(), "an answered wait stops the observing");
    }

    #[test]
    fn every_change_settings_would_see_moves_the_revision() {
        type Edit = fn(&mut SpeakerSnapshot);
        let changes: [(&str, Edit); 7] = [
            ("a device goes offline", |view| {
                view.devices[1].online = false;
            }),
            ("a device comes online", |view| {
                let mut kitchen = view.devices[1].clone();
                kitchen.id = "kitchen".into();
                view.devices.push(kitchen);
            }),
            ("a group is deleted", |view| view.groups.clear()),
            ("a group is created", |view| {
                let mut den = view.groups[0].clone();
                den.id = "ef".repeat(16);
                view.groups.push(den);
            }),
            ("a member is yet to answer", |view| {
                view.groups[0].members[1].status = SpeakerMemberStatus::AwaitingConsent;
            }),
            ("a calibration measures", |view| {
                view.calibrations[0].phase = CalibrationPhaseInfo::Measuring;
            }),
            ("a set starts playing", |view| {
                view.groups[0].playback = Some(SpeakerPlaybackInfo::Playing);
            }),
        ];
        let revision = ViewRevision::new();
        let mut after = answered(&mut Box::pin(revision.wait(0))).expect("a first wait answers");
        revision.note(&pair());
        for (what, edit) in changes {
            let mut wait = Box::pin(revision.wait(after));
            let mut view = pair();
            edit(&mut view);
            revision.note(&view);
            let change = answered(&mut wait).unwrap_or_else(|| panic!("{what}: Settings hears"));
            assert_ne!(change, after, "{what}");
            assert_ne!(change, 0, "{what}: never reads as a first wait");
            after = change;

            let mut back = Box::pin(revision.wait(after));
            revision.note(&view);
            assert_eq!(answered(&mut back), None, "{what}: heard once");
            revision.note(&pair());
            after = answered(&mut back).unwrap_or_else(|| panic!("{what}: and when it ends"));
        }
    }
}
