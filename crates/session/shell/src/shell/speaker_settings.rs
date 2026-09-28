//! The Settings app's door to speaker groups.
//!
//! Settings asks through the launcher-authenticated collaboration broker
//! with [`runtime_collab::SpeakerCommand`]; this module answers from the
//! live fabric and the speaker-group runtime, and owns the calibration runs
//! Settings starts. Devices and outputs cross this boundary only as fabric
//! device ids and hex endpoint selectors.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use runtime_collab::{
    CalibrationInfo, CalibrationMemberInfo, CalibrationMemberState, CalibrationPhaseInfo,
    SpeakerCommand, SpeakerDeviceInfo, SpeakerEndpointInfo, SpeakerGroupInfo, SpeakerMemberInfo,
    SpeakerMemberSpec, SpeakerMemberStatus, SpeakerReply, SpeakerRoleSpec, SpeakerSnapshot,
    SpeakerTrimInfo,
};

use crate::shell::device_fabric::DeviceFabric;
use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_calibration::{CalibrationPhase, CalibrationRun, MemberProgress};
use crate::shell::speaker_group::{
    AudioEndpointKind, EndpointSelector, RoomPoint, SpeakerGroupId, SpeakerMemberState,
    SpeakerRole, SpeakerTrim, StereoSide, SurroundChannel,
};
use crate::shell::speaker_group_runtime::MemberSpec;

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

const fn status(state: SpeakerMemberState) -> SpeakerMemberStatus {
    match state {
        SpeakerMemberState::Ready => SpeakerMemberStatus::Ready,
        SpeakerMemberState::AwaitingConsent => SpeakerMemberStatus::AwaitingConsent,
        SpeakerMemberState::Offline => SpeakerMemberStatus::Offline,
        SpeakerMemberState::OutputMissing => SpeakerMemberStatus::OutputMissing,
        SpeakerMemberState::Unavailable => SpeakerMemberStatus::Unavailable,
        SpeakerMemberState::NotCertified => SpeakerMemberStatus::NotCertified,
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
                // Calibration listens on the device that runs it, so only this
                // device's microphones are offered.
                microphones: if &snapshot.owner == local {
                    endpoints(AudioEndpointKind::Microphone)
                } else {
                    Vec::new()
                },
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

    /// Advance every run, and drop runs whose group changed or vanished.
    pub fn pump(&mut self, fabric: &DeviceFabric) {
        self.runs.retain(|group, run| {
            fabric
                .speaker_group(*group)
                .is_some_and(|published| run.matches(published))
                || run.phase() == CalibrationPhase::Measuring
        });
        for run in self.runs.values_mut() {
            run.tick();
        }
    }

    #[must_use]
    pub fn info(&self) -> Vec<CalibrationInfo> {
        self.runs.values().map(calibration_info).collect()
    }
}

#[must_use]
pub fn calibration_info(run: &CalibrationRun) -> CalibrationInfo {
    let after = run.after();
    CalibrationInfo {
        group: run.group().to_hex(),
        phase: match run.phase() {
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
                    Some(MemberProgress::Measured(measurement)) => {
                        CalibrationMemberState::Measured {
                            delay_ms: i64_ms(measurement.arrival_ns),
                            level_db: measurement.level_db,
                        }
                    }
                    Some(MemberProgress::Failed(reason)) => CalibrationMemberState::Failed {
                        reason: reason.clone(),
                    },
                    Some(MemberProgress::Waiting) | None => CalibrationMemberState::Waiting,
                },
                before: run.before().get(index).map(trim_info).unwrap_or_default(),
                after: after.and_then(|after| after.get(index)).map(trim_info),
            })
            .collect(),
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
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            calibrations: Calibrations::default(),
            last_identify: None,
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
    if let Ok(mut state) = state().lock() {
        state.calibrations.pump(&fabric);
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
                    status: status(member.state),
                    trim: trim_info(&member.trim),
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

/// Answer one command from Settings.
#[must_use]
pub fn handle(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    command: SpeakerCommand,
) -> SpeakerReply {
    let result = match command {
        SpeakerCommand::Snapshot => {
            return SpeakerReply::Snapshot {
                snapshot: snapshot(network),
            };
        }
        command => crate::shell::device_fabric_runtime::require_speaker_groups()
            .map_err(|_| crate::shell::i18n::t("speaker-error-media-off"))
            .and_then(|()| run_command(network, command)),
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
            microphone_device,
            microphone,
        } => start_calibration(network, &group, &microphone_device, &microphone)
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

/// Measure with this device's microphone; remote members are asked over the
/// fabric to play the sweep at the scheduled instant.
fn timed_source(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    local: DeviceId,
) -> Box<dyn crate::shell::speaker_calibration::MeasurementSource> {
    let network = std::sync::Arc::downgrade(network);
    Box::new(crate::shell::speaker_calibration::TimedMeasurement::new(
        local,
        Box::new(move |request, start_ns| {
            let network = network
                .upgrade()
                .ok_or_else(|| crate::shell::i18n::t("speaker-error-network-unavailable"))?;
            network
                .send_personal_fabric_action(
                    &request.member,
                    crate::shell::device_fabric::PersonalFabricAction::SpeakerSweep {
                        group: request.group,
                        start_ns,
                    },
                )
                .map(|_| ())
        }),
    ))
}

fn start_calibration(
    network: &std::sync::Arc<crate::shell::network::ShellNetwork>,
    group: &str,
    microphone_device: &str,
    microphone: &str,
) -> Result<(), String> {
    let id = parse_group(group)?;
    let microphone_device = DeviceId::new(microphone_device.to_string())
        .map_err(|_| crate::shell::i18n::t("speaker-error-device-invalid"))?;
    let microphone = EndpointSelector::from_hex(microphone)
        .ok_or_else(|| crate::shell::i18n::t("speaker-error-microphone-invalid"))?;
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
    let offers_microphone = fabric
        .snapshots()
        .get(&microphone_device)
        .is_some_and(|snapshot| {
            snapshot.presence.online
                && snapshot.speakers.endpoints.iter().any(|endpoint| {
                    endpoint.selector == microphone
                        && endpoint.kind == AudioEndpointKind::Microphone
                })
        });
    if !offers_microphone {
        return Err(crate::shell::i18n::t(
            "speaker-error-microphone-unavailable",
        ));
    }
    if fabric
        .speaker_member_states(published)
        .iter()
        .any(|state| *state != SpeakerMemberState::Ready)
    {
        return Err(crate::shell::i18n::t("speaker-error-not-all-ready"));
    }
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
    let run = CalibrationRun::start(
        published,
        &labels,
        microphone_device,
        microphone,
        timed_source(network, local),
    );
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
}
