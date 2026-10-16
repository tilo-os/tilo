//! Settings' command lane to the shell's speaker groups.
//!
//! Speaker groups, consent and calibration are owned by the compositor; the
//! Settings app only asks. Devices and outputs are named here by the same
//! opaque identifiers the shell publishes in its fabric (a device id and a
//! hex endpoint selector), so no `PipeWire` node id or node name can be
//! represented on this lane.

use serde::{Deserialize, Serialize};

/// Maximum encoded size of one speaker command or reply.
pub const MAX_SPEAKER_WIRE_BYTES: usize = 64 * 1024;
pub const MAX_SPEAKER_DEVICES: usize = 64;
pub const MAX_SPEAKER_ENDPOINTS: usize = 32;
pub const MAX_SPEAKER_GROUPS: usize = 16;
pub const MAX_SPEAKER_MEMBERS: usize = 8;
pub const MAX_SPEAKER_LABEL_BYTES: usize = 256;
pub const MAX_SPEAKER_NAME_CHARS: usize = 64;

/// What one member contributes, in wire form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SpeakerRoleSpec {
    Left,
    Right,
    Center,
    Lfe,
    SurroundLeft,
    SurroundRight,
    /// Millimetres from the listening position: `x` right, `y` ahead, `z` up.
    Room {
        x_mm: i32,
        y_mm: i32,
        z_mm: i32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerMemberSpec {
    pub device: String,
    /// Hex endpoint selector the member device published.
    pub output: String,
    pub role: SpeakerRoleSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SpeakerCommand {
    /// Devices, outputs, microphones, groups and any calibration in progress.
    Snapshot,
    Create {
        name: String,
        members: Vec<SpeakerMemberSpec>,
    },
    Update {
        group: String,
        name: String,
        members: Vec<SpeakerMemberSpec>,
    },
    Delete {
        group: String,
    },
    /// Play a short sound on one output so the person can tell which speaker
    /// it is.
    Identify {
        device: String,
        output: String,
    },
    /// Measure every member of a group with every usable microphone of
    /// the person's devices, one per device.
    StartCalibration {
        group: String,
        /// Devices whose microphone sits this run out.
        excluded: Vec<String>,
        /// Device at the listening position; `None` is the middle of the
        /// microphones.
        listening: Option<String>,
    },
    CancelCalibration {
        group: String,
    },
    /// Store the proposed trims. Never happens without this command.
    ApplyCalibration {
        group: String,
    },
    /// Restore the trims from before the last applied calibration.
    RevertCalibration {
        group: String,
    },
    /// Forget a finished proposal without applying it.
    DismissCalibration {
        group: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerEndpointInfo {
    pub selector: String,
    pub label: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerDeviceInfo {
    pub id: String,
    pub label: String,
    pub online: bool,
    pub local: bool,
    /// Media continuity is on there, so it can play in a group.
    pub can_join: bool,
    pub outputs: Vec<SpeakerEndpointInfo>,
    pub microphones: Vec<SpeakerEndpointInfo>,
}

/// Where one member of a group stands right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpeakerMemberStatus {
    /// Consented (or the leader's own output) and online.
    Ready,
    /// Asked, not yet answered on that device.
    AwaitingConsent,
    /// Consented, but the device is offline.
    Offline,
    /// The device no longer offers this output.
    OutputMissing,
    /// Media continuity is off there, or it cannot join speaker groups.
    Unavailable,
    /// Not one of this person's certified devices.
    NotCertified,
}

/// A member's calibration correction in display units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerTrimInfo {
    pub delay_ms: f64,
    pub gain_db: f64,
    pub eq_bands: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerMemberInfo {
    pub device: String,
    pub device_label: String,
    pub output: String,
    pub output_label: Option<String>,
    pub role: SpeakerRoleSpec,
    pub status: SpeakerMemberStatus,
    pub trim: SpeakerTrimInfo,
}

/// A session playing on the group right now, most urgent state first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SpeakerPlaybackInfo {
    Playing,
    /// A member has not admitted the session yet.
    Connecting,
    /// A member does not have the playing device's clock yet, and plays
    /// nothing until it does.
    SyncingClocks,
    /// A member could not get the playing device's clock in time.
    ClockFailed {
        reason: String,
    },
}

/// Whether one device's microphone can listen while a group is calibrated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MicrophoneStatus {
    /// This device, or a device that already allowed it.
    Ready,
    /// Starting a calibration asks on that device.
    NotAsked,
    /// Asked; not yet answered on that device.
    AwaitingConsent,
    Declined,
    Offline,
    /// Media continuity is off there.
    Unavailable,
}

impl MicrophoneStatus {
    /// Whether a microphone in this state can take part in a run.
    #[must_use]
    pub const fn usable(self) -> bool {
        matches!(self, Self::Ready | Self::NotAsked | Self::AwaitingConsent)
    }
}

/// One device's microphone as a calibration candidate for a group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationMicrophoneInfo {
    pub device: String,
    pub device_label: String,
    /// The microphone's own label on that device.
    pub label: String,
    pub local: bool,
    pub status: MicrophoneStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerGroupInfo {
    pub id: String,
    pub name: String,
    pub leader_label: String,
    /// Only the device that created a group can edit, calibrate or delete it.
    pub led_here: bool,
    pub members: Vec<SpeakerMemberInfo>,
    /// A session this device plays to the group, or plays in as a member.
    pub playback: Option<SpeakerPlaybackInfo>,
    /// Microphones that can calibrate this group, one per device; only for
    /// groups led here.
    pub microphones: Vec<CalibrationMicrophoneInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CalibrationMemberState {
    Waiting,
    Measuring,
    /// `delay_ms` is when the speaker's sound arrives at the listening
    /// position. With that speaker's own device's microphone listening too,
    /// the arrival splits into its output latency and the flight time.
    Measured {
        delay_ms: f64,
        level_db: f64,
        output_latency_ms: Option<f64>,
        acoustic_ms: Option<f64>,
    },
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationMemberInfo {
    pub device: String,
    pub label: String,
    pub state: CalibrationMemberState,
    pub before: SpeakerTrimInfo,
    /// Present once every member is measured.
    pub after: Option<SpeakerTrimInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CalibrationPhaseInfo {
    /// Remote microphones were asked and have not all answered.
    WaitingForMicrophones,
    Measuring,
    /// Measured; nothing changes until the person applies it.
    Proposed,
    /// Applied; can still be reverted.
    Applied,
    Failed,
}

/// What one microphone contributed to a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CalibrationMicrophoneState {
    AwaitingConsent,
    Declined,
    Listening,
    /// Heard `heard` speakers; `rejected` of its readings disagreed with the
    /// other microphones and were set aside.
    Used {
        heard: u8,
        rejected: u8,
    },
    /// Heard none of the speakers: too far away.
    NotHeard,
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationMicrophoneRun {
    pub device: String,
    pub label: String,
    /// This microphone is at the listening position.
    pub listening: bool,
    pub state: CalibrationMicrophoneState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationInfo {
    pub group: String,
    pub phase: CalibrationPhaseInfo,
    pub members: Vec<CalibrationMemberInfo>,
    pub microphones: Vec<CalibrationMicrophoneRun>,
    /// Where delays are aligned for; `None` is the middle of the microphones.
    pub listening_label: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerSnapshot {
    /// Media continuity is on here; without it there are no speaker groups.
    pub media_enabled: bool,
    pub devices: Vec<SpeakerDeviceInfo>,
    pub groups: Vec<SpeakerGroupInfo>,
    pub calibrations: Vec<CalibrationInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SpeakerReply {
    Snapshot { snapshot: SpeakerSnapshot },
    Done { group: Option<String> },
    Rejected { reason: String },
}

fn valid_label(value: &str) -> bool {
    value.len() <= MAX_SPEAKER_LABEL_BYTES && !value.chars().any(char::is_control)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'))
}

fn valid_hex16(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_SPEAKER_NAME_CHARS
        && !value.chars().any(char::is_control)
}

fn valid_members(members: &[SpeakerMemberSpec]) -> bool {
    !members.is_empty()
        && members.len() <= MAX_SPEAKER_MEMBERS
        && members
            .iter()
            .all(|member| valid_id(&member.device) && valid_hex16(&member.output))
}

impl SpeakerCommand {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Snapshot => true,
            Self::Create { name, members } => valid_name(name) && valid_members(members),
            Self::Update {
                group,
                name,
                members,
            } => valid_hex16(group) && valid_name(name) && valid_members(members),
            Self::Delete { group }
            | Self::CancelCalibration { group }
            | Self::ApplyCalibration { group }
            | Self::RevertCalibration { group }
            | Self::DismissCalibration { group } => valid_hex16(group),
            Self::Identify { device, output } => valid_id(device) && valid_hex16(output),
            Self::StartCalibration {
                group,
                excluded,
                listening,
            } => {
                valid_hex16(group)
                    && excluded.len() <= MAX_SPEAKER_DEVICES
                    && excluded.iter().all(|device| valid_id(device))
                    && listening.as_deref().is_none_or(valid_id)
            }
        }
    }
}

impl SpeakerReply {
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Snapshot { snapshot } => {
                snapshot.devices.len() <= MAX_SPEAKER_DEVICES
                    && snapshot.groups.len() <= MAX_SPEAKER_GROUPS
                    && snapshot.calibrations.len() <= MAX_SPEAKER_GROUPS
                    && snapshot.devices.iter().all(|device| {
                        valid_label(&device.label)
                            && device.outputs.len() <= MAX_SPEAKER_ENDPOINTS
                            && device.microphones.len() <= MAX_SPEAKER_ENDPOINTS
                    })
                    && snapshot.groups.iter().all(|group| {
                        group.members.len() <= MAX_SPEAKER_MEMBERS
                            && match &group.playback {
                                Some(SpeakerPlaybackInfo::ClockFailed { reason }) => {
                                    valid_label(reason)
                                }
                                _ => true,
                            }
                            && group.microphones.len() <= MAX_SPEAKER_DEVICES
                            && group.microphones.iter().all(|microphone| {
                                valid_label(&microphone.device_label)
                                    && valid_label(&microphone.label)
                            })
                    })
                    && snapshot.calibrations.iter().all(|run| {
                        run.members.len() <= MAX_SPEAKER_MEMBERS
                            && run.microphones.len() <= MAX_SPEAKER_DEVICES
                            && run
                                .microphones
                                .iter()
                                .all(|microphone| valid_label(&microphone.label))
                            && run.listening_label.as_deref().is_none_or(valid_label)
                    })
            }
            Self::Done { group } => group.as_deref().is_none_or(valid_hex16),
            Self::Rejected { reason } => !reason.is_empty() && valid_label(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_and_reject_node_shaped_identifiers() {
        let create = SpeakerCommand::Create {
            name: "Living room".into(),
            members: vec![SpeakerMemberSpec {
                device: "desk".into(),
                output: "ab".repeat(16),
                role: SpeakerRoleSpec::Room {
                    x_mm: -1500,
                    y_mm: 2000,
                    z_mm: 0,
                },
            }],
        };
        assert!(create.valid());
        let json = serde_json::to_vec(&create).unwrap();
        assert_eq!(
            serde_json::from_slice::<SpeakerCommand>(&json).unwrap(),
            create
        );
        let node_name = SpeakerCommand::Identify {
            device: "desk".into(),
            output: "alsa_output.pci-0000_00_1f.3.analog-stereo".into(),
        };
        assert!(!node_name.valid());
        assert!(
            !SpeakerCommand::Create {
                name: " ".into(),
                members: Vec::new()
            }
            .valid()
        );
        assert!(
            serde_json::from_str::<SpeakerCommand>(
                r#"{"command":"identify","device":"desk","output":"00","node":57}"#
            )
            .is_err()
        );
    }

    #[test]
    fn calibration_commands_name_devices_not_nodes() {
        let start = SpeakerCommand::StartCalibration {
            group: "0f".repeat(16),
            excluded: vec!["phone".into()],
            listening: Some("desk".into()),
        };
        assert!(start.valid());
        let json = serde_json::to_vec(&start).unwrap();
        assert_eq!(
            serde_json::from_slice::<SpeakerCommand>(&json).unwrap(),
            start
        );
        assert!(
            !SpeakerCommand::StartCalibration {
                group: "0f".repeat(16),
                excluded: vec!["alsa input 57".into()],
                listening: None,
            }
            .valid()
        );
        assert!(
            !SpeakerCommand::StartCalibration {
                group: "0f".repeat(16),
                excluded: Vec::new(),
                listening: Some(String::new()),
            }
            .valid()
        );
    }

    #[test]
    fn snapshots_with_microphones_round_trip() {
        let snapshot = SpeakerSnapshot {
            media_enabled: true,
            devices: Vec::new(),
            groups: vec![SpeakerGroupInfo {
                id: "0f".repeat(16),
                name: "Living room".into(),
                leader_label: "Desk".into(),
                led_here: true,
                members: Vec::new(),
                playback: None,
                microphones: vec![CalibrationMicrophoneInfo {
                    device: "phone".into(),
                    device_label: "Phone".into(),
                    label: "Microphone".into(),
                    local: false,
                    status: MicrophoneStatus::AwaitingConsent,
                }],
            }],
            calibrations: vec![CalibrationInfo {
                group: "0f".repeat(16),
                phase: CalibrationPhaseInfo::WaitingForMicrophones,
                members: Vec::new(),
                microphones: vec![CalibrationMicrophoneRun {
                    device: "phone".into(),
                    label: "Phone".into(),
                    listening: true,
                    state: CalibrationMicrophoneState::Used {
                        heard: 2,
                        rejected: 1,
                    },
                }],
                listening_label: Some("Phone".into()),
                message: None,
            }],
        };
        let reply = SpeakerReply::Snapshot { snapshot };
        assert!(reply.valid());
        let json = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            serde_json::from_slice::<SpeakerReply>(&json).unwrap(),
            reply
        );
    }

    #[test]
    fn replies_are_bounded() {
        assert!(
            !SpeakerReply::Rejected {
                reason: String::new()
            }
            .valid()
        );
        assert!(
            SpeakerReply::Done {
                group: Some("0f".repeat(16))
            }
            .valid()
        );
        assert!(
            !SpeakerReply::Done {
                group: Some("group".into())
            }
            .valid()
        );
    }
}
