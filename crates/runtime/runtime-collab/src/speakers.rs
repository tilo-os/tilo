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

impl SpeakerRoleSpec {
    /// Short human description, as consent prompts phrase it.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Left => "left speaker",
            Self::Right => "right speaker",
            Self::Center => "centre speaker",
            Self::Lfe => "subwoofer",
            Self::SurroundLeft => "left surround speaker",
            Self::SurroundRight => "right surround speaker",
            Self::Room { .. } => "room speaker",
        }
    }
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
    /// Measure every member of a group with one microphone.
    StartCalibration {
        group: String,
        microphone_device: String,
        microphone: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpeakerMemberStatus {
    Ready,
    AwaitingConsent,
    Offline,
    OutputMissing,
    Unavailable,
    NotCertified,
}

impl SpeakerMemberStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::AwaitingConsent => "Waiting for approval on that device",
            Self::Offline => "Offline",
            Self::OutputMissing => "Output unplugged",
            Self::Unavailable => "Media continuity is off there",
            Self::NotCertified => "Not one of your devices",
        }
    }
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

impl SpeakerPlaybackInfo {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Connecting => "Connecting…",
            Self::SyncingClocks => "Syncing clocks…",
            Self::ClockFailed { .. } => "Can't play in time",
        }
    }
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CalibrationMemberState {
    Waiting,
    Measuring,
    Measured { delay_ms: f64, level_db: f64 },
    Failed { reason: String },
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
    Measuring,
    /// Measured; nothing changes until the person applies it.
    Proposed,
    /// Applied; can still be reverted.
    Applied,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationInfo {
    pub group: String,
    pub phase: CalibrationPhaseInfo,
    pub members: Vec<CalibrationMemberInfo>,
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
                microphone_device,
                microphone,
            } => valid_hex16(group) && valid_id(microphone_device) && valid_hex16(microphone),
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
