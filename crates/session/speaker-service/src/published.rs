//! The person's speakers as `so.tilo.device`'s `speakers` resource: the devices, their outputs and
//! microphones, the groups they form, each group's calibration run, and the verbs a view presses.
//!
//! This is the same reading and the same commands Settings' Rust window sends over the broker
//! ([`crate::settings`]), published as a resource so a tilolang view — Settings' own speakers page,
//! the shell or an agent — reads one resource and calls its own published actions. What a call
//! changes is the fabric's speaker groups, so no view holds a copy of it.

use serde::{Deserialize, Serialize};
use tilo_network_actor::fabric::link::FabricLink;

use runtime_collab::{
    CalibrationInfo, CalibrationMemberInfo, CalibrationMemberState, CalibrationMicrophoneRun,
    CalibrationMicrophoneState, CalibrationPhaseInfo, MicrophoneStatus, SpeakerCommand,
    SpeakerDeviceInfo, SpeakerGroupInfo, SpeakerMemberInfo, SpeakerMemberSpec, SpeakerMemberStatus,
    SpeakerPlaybackInfo, SpeakerRoleSpec, SpeakerSnapshot,
};

/// One endpoint a device plays through or records from.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct SpeakerEndpoint {
    /// The opaque selector the device publishes it as, which every action names it by.
    pub selector: String,
    pub label: String,
    /// Whether it is the device's own default.
    pub is_default: bool,
}

/// One of the person's devices, with what it can play and hear.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default)]
pub struct SpeakerDevice {
    /// The fabric id the group actions name it by.
    pub id: String,
    pub label: String,
    /// It has a live session with this device now.
    pub online: bool,
    /// This machine.
    pub local: bool,
    /// Media continuity is on there, so it can join a group.
    pub can_join: bool,
    pub outputs: Vec<SpeakerEndpoint>,
    pub microphones: Vec<SpeakerEndpoint>,
}

/// Where a speaker stands in a group: a named channel, or a point in the room around the person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(tag = "role", rename_all = "kebab-case")]
pub enum SpeakerRole {
    #[default]
    Left,
    Right,
    Center,
    Lfe,
    SurroundLeft,
    SurroundRight,
    /// Millimetres from the listening position: `x` right, `y` ahead, `z` up.
    Room { x_mm: i32, y_mm: i32, z_mm: i32 },
}

/// Where one member of a group stands right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SpeakerMemberState {
    /// Consented (or the leader's own output) and online.
    #[default]
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

/// A member's calibration correction, in display units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct SpeakerTrim {
    pub delay_ms: f64,
    pub gain_db: f64,
    pub eq_bands: i32,
}

/// One speaker of a group.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct SpeakerMember {
    pub device: String,
    pub device_label: String,
    pub output: String,
    /// What the device calls that output; empty when it no longer offers it.
    pub output_label: String,
    pub role: SpeakerRole,
    pub state: SpeakerMemberState,
    pub trim: SpeakerTrim,
}

/// What plays on a group right now, most urgent state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum SpeakerPlayback {
    Playing,
    /// A member has not admitted the session yet.
    Connecting,
    /// A member does not have the playing device's clock yet.
    SyncingClocks,
    /// A member could not get the playing device's clock in time.
    ClockFailed { reason: String },
}

impl Default for SpeakerPlayback {
    fn default() -> Self {
        Self::Playing
    }
}

/// Whether one device's microphone can listen while a group is calibrated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SpeakerMicrophoneState {
    /// This device, or a device that already allowed it.
    #[default]
    Ready,
    /// Starting a calibration asks on that device.
    NotAsked,
    Asked,
    Declined,
    Offline,
    /// Media continuity is off there.
    Unavailable,
}

/// One device's microphone, as a candidate to calibrate a group with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct SpeakerMicrophone {
    pub device: String,
    pub device_label: String,
    pub label: String,
    /// It sits on this machine.
    pub local: bool,
    pub state: SpeakerMicrophoneState,
}

/// A group of speakers across the person's devices playing as one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct SpeakerGroup {
    /// The group's id, which every action names it by.
    pub id: String,
    pub name: String,
    /// What the device leading the group is called.
    pub leader_label: String,
    /// Only the device that made a group can edit, calibrate or delete it.
    pub led_here: bool,
    pub members: Vec<SpeakerMember>,
    /// What plays on it now; nothing while it plays nothing.
    pub playback: Option<SpeakerPlayback>,
    /// This device plays everything on it: the set chosen as the output.
    pub output_here: bool,
    /// The microphones that can calibrate it, one per device; only for a group led here.
    pub microphones: Vec<SpeakerMicrophone>,
}

/// What one speaker of a calibration run has measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum CalibrationMemberState {
    Waiting,
    Measuring,
    /// `delay_ms` is when the speaker's sound arrives at the listening position. With that
    /// speaker's own device's microphone listening too, the arrival splits into its output
    /// latency and the flight time.
    Measured {
        delay_ms: f64,
        level_db: f64,
        output_latency_ms: Option<f64>,
        acoustic_ms: Option<f64>,
    },
    Failed { reason: String },
}

impl Default for CalibrationMemberState {
    fn default() -> Self {
        Self::Waiting
    }
}

/// One speaker in a calibration run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct CalibrationMember {
    pub device: String,
    pub label: String,
    pub state: CalibrationMemberState,
    pub before: SpeakerTrim,
    /// What the run proposes for it; present once every speaker is measured.
    pub after: Option<SpeakerTrim>,
}

/// What one microphone contributed to a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum CalibrationMicrophoneState {
    AwaitingConsent,
    Declined,
    Listening,
    /// Heard `heard` speakers; `rejected` of its readings disagreed with the other microphones
    /// and were set aside.
    Used { heard: i32, rejected: i32 },
    /// Heard none of the speakers: too far away.
    NotHeard,
    Failed { reason: String },
}

impl Default for CalibrationMicrophoneState {
    fn default() -> Self {
        Self::AwaitingConsent
    }
}

/// One microphone in a calibration run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct CalibrationMicrophone {
    pub device: String,
    pub label: String,
    /// This microphone is at the listening position.
    pub listening: bool,
    pub state: CalibrationMicrophoneState,
}

/// Where a group's calibration run stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CalibrationPhase {
    /// Remote microphones were asked and have not all answered.
    #[default]
    WaitingForMicrophones,
    Measuring,
    /// Measured; nothing changes until the person applies it.
    Proposed,
    /// Applied; can still be reverted.
    Applied,
    Failed,
}

/// One group's calibration run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct SpeakerCalibration {
    /// The group the run measures.
    pub group: String,
    pub phase: CalibrationPhase,
    pub members: Vec<CalibrationMember>,
    pub microphones: Vec<CalibrationMicrophone>,
    /// Where delays are aligned for, by the device's label; empty for the middle of the
    /// microphones.
    pub listening_label: String,
    /// What the run says as it goes; empty while it says nothing.
    pub message: String,
}

/// The person's speakers as a view of another language reads them (`device.speakers`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct Speakers {
    /// Media continuity is on, so the person's devices can play as one set.
    pub media_enabled: bool,
    pub devices: Vec<SpeakerDevice>,
    pub groups: Vec<SpeakerGroup>,
    /// The runs this device started, one per group.
    pub calibrations: Vec<SpeakerCalibration>,
}

impl Speakers {
    /// The snapshot as the resource shows it.
    #[must_use]
    pub fn of(snapshot: &SpeakerSnapshot) -> Self {
        Self {
            media_enabled: snapshot.media_enabled,
            devices: snapshot.devices.iter().map(device).collect(),
            groups: snapshot.groups.iter().map(group).collect(),
            calibrations: snapshot.calibrations.iter().map(calibration).collect(),
        }
    }
}

fn endpoint(endpoint: &runtime_collab::SpeakerEndpointInfo) -> SpeakerEndpoint {
    SpeakerEndpoint {
        selector: endpoint.selector.clone(),
        label: endpoint.label.clone(),
        is_default: endpoint.is_default,
    }
}

fn device(device: &SpeakerDeviceInfo) -> SpeakerDevice {
    SpeakerDevice {
        id: device.id.clone(),
        label: device.label.clone(),
        online: device.online,
        local: device.local,
        can_join: device.can_join,
        outputs: device.outputs.iter().map(endpoint).collect(),
        microphones: device.microphones.iter().map(endpoint).collect(),
    }
}

const fn role(role: SpeakerRoleSpec) -> SpeakerRole {
    match role {
        SpeakerRoleSpec::Left => SpeakerRole::Left,
        SpeakerRoleSpec::Right => SpeakerRole::Right,
        SpeakerRoleSpec::Center => SpeakerRole::Center,
        SpeakerRoleSpec::Lfe => SpeakerRole::Lfe,
        SpeakerRoleSpec::SurroundLeft => SpeakerRole::SurroundLeft,
        SpeakerRoleSpec::SurroundRight => SpeakerRole::SurroundRight,
        SpeakerRoleSpec::Room { x_mm, y_mm, z_mm } => SpeakerRole::Room { x_mm, y_mm, z_mm },
    }
}

fn trim(trim: &runtime_collab::SpeakerTrimInfo) -> SpeakerTrim {
    SpeakerTrim {
        delay_ms: trim.delay_ms,
        gain_db: trim.gain_db,
        eq_bands: i32::from(trim.eq_bands),
    }
}

const fn member_state(state: SpeakerMemberStatus) -> SpeakerMemberState {
    match state {
        SpeakerMemberStatus::Ready => SpeakerMemberState::Ready,
        SpeakerMemberStatus::AwaitingConsent => SpeakerMemberState::AwaitingConsent,
        SpeakerMemberStatus::Offline => SpeakerMemberState::Offline,
        SpeakerMemberStatus::OutputMissing => SpeakerMemberState::OutputMissing,
        SpeakerMemberStatus::Unavailable => SpeakerMemberState::Unavailable,
        SpeakerMemberStatus::NotCertified => SpeakerMemberState::NotCertified,
    }
}

fn member(member: &SpeakerMemberInfo) -> SpeakerMember {
    SpeakerMember {
        device: member.device.clone(),
        device_label: member.device_label.clone(),
        output: member.output.clone(),
        output_label: member.output_label.clone().unwrap_or_default(),
        role: role(member.role),
        state: member_state(member.status),
        trim: trim(&member.trim),
    }
}

fn playback(playback: &SpeakerPlaybackInfo) -> SpeakerPlayback {
    match playback {
        SpeakerPlaybackInfo::Playing => SpeakerPlayback::Playing,
        SpeakerPlaybackInfo::Connecting => SpeakerPlayback::Connecting,
        SpeakerPlaybackInfo::SyncingClocks => SpeakerPlayback::SyncingClocks,
        SpeakerPlaybackInfo::ClockFailed { reason } => {
            SpeakerPlayback::ClockFailed {
                reason: reason.clone(),
            }
        }
    }
}

const fn microphone_state(state: MicrophoneStatus) -> SpeakerMicrophoneState {
    match state {
        MicrophoneStatus::Ready => SpeakerMicrophoneState::Ready,
        MicrophoneStatus::NotAsked => SpeakerMicrophoneState::NotAsked,
        MicrophoneStatus::AwaitingConsent => SpeakerMicrophoneState::Asked,
        MicrophoneStatus::Declined => SpeakerMicrophoneState::Declined,
        MicrophoneStatus::Offline => SpeakerMicrophoneState::Offline,
        MicrophoneStatus::Unavailable => SpeakerMicrophoneState::Unavailable,
    }
}

fn group(group: &SpeakerGroupInfo) -> SpeakerGroup {
    SpeakerGroup {
        id: group.id.clone(),
        name: group.name.clone(),
        leader_label: group.leader_label.clone(),
        led_here: group.led_here,
        members: group.members.iter().map(member).collect(),
        playback: group.playback.as_ref().map(playback),
        output_here: group.output_here,
        microphones: group
            .microphones
            .iter()
            .map(|microphone| SpeakerMicrophone {
                device: microphone.device.clone(),
                device_label: microphone.device_label.clone(),
                label: microphone.label.clone(),
                local: microphone.local,
                state: microphone_state(microphone.status),
            })
            .collect(),
    }
}

const fn phase(phase: CalibrationPhaseInfo) -> CalibrationPhase {
    match phase {
        CalibrationPhaseInfo::WaitingForMicrophones => CalibrationPhase::WaitingForMicrophones,
        CalibrationPhaseInfo::Measuring => CalibrationPhase::Measuring,
        CalibrationPhaseInfo::Proposed => CalibrationPhase::Proposed,
        CalibrationPhaseInfo::Applied => CalibrationPhase::Applied,
        CalibrationPhaseInfo::Failed => CalibrationPhase::Failed,
    }
}

fn member_progress(state: &CalibrationMemberState) -> CalibrationMemberState {
    match state {
        CalibrationMemberState::Waiting => CalibrationMemberState::Waiting,
        CalibrationMemberState::Measuring => CalibrationMemberState::Measuring,
        CalibrationMemberState::Measured {
            delay_ms,
            level_db,
            output_latency_ms,
            acoustic_ms,
        } => CalibrationMemberState::Measured {
            delay_ms: *delay_ms,
            level_db: *level_db,
            output_latency_ms: *output_latency_ms,
            acoustic_ms: *acoustic_ms,
        },
        CalibrationMemberState::Failed { reason } => CalibrationMemberState::Failed {
            reason: reason.clone(),
        },
    }
}

fn heard(count: u8) -> i32 {
    i32::from(count)
}

fn microphone_use(state: &CalibrationMicrophoneState) -> CalibrationMicrophoneState {
    match state {
        CalibrationMicrophoneState::AwaitingConsent => {
            CalibrationMicrophoneState::AwaitingConsent
        }
        CalibrationMicrophoneState::Declined => CalibrationMicrophoneState::Declined,
        CalibrationMicrophoneState::Listening => CalibrationMicrophoneState::Listening,
        CalibrationMicrophoneState::Used { heard, rejected } => CalibrationMicrophoneState::Used {
            heard: heard(*heard),
            rejected: heard(*rejected),
        },
        CalibrationMicrophoneState::NotHeard => CalibrationMicrophoneState::NotHeard,
        CalibrationMicrophoneState::Failed { reason } => CalibrationMicrophoneState::Failed {
            reason: reason.clone(),
        },
    }
}

fn member_measured(member: &CalibrationMemberInfo) -> CalibrationMember {
    CalibrationMember {
        device: member.device.clone(),
        label: member.label.clone(),
        state: member_progress(&member.state),
        before: trim(&member.before),
        after: member.after.as_ref().map(trim),
    }
}

fn microphone_run(microphone: &CalibrationMicrophoneRun) -> CalibrationMicrophone {
    CalibrationMicrophone {
        device: microphone.device.clone(),
        label: microphone.label.clone(),
        listening: microphone.listening,
        state: microphone_use(&microphone.state),
    }
}

fn calibration(calibration: &CalibrationInfo) -> SpeakerCalibration {
    SpeakerCalibration {
        group: calibration.group.clone(),
        phase: phase(calibration.phase),
        members: calibration.members.iter().map(member_measured).collect(),
        microphones: calibration
            .microphones
            .iter()
            .map(microphone_run)
            .collect(),
        listening_label: calibration.listening_label.clone().unwrap_or_default(),
        message: calibration.message.clone().unwrap_or_default(),
    }
}

/// One speaker of a group as an action names it: the device, which of its outputs, and where it
/// stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema)]
#[serde(default)]
pub struct SpeakerMemberChoice {
    /// The device's fabric id, as the reading lists it.
    pub device: String,
    /// The output's selector, as the reading lists it.
    pub output: String,
    pub role: SpeakerRole,
}

impl From<SpeakerMemberChoice> for SpeakerMemberSpec {
    fn from(choice: SpeakerMemberChoice) -> Self {
        Self {
            device: choice.device,
            output: choice.output,
            role: role_spec(choice.role),
        }
    }
}

const fn role_spec(role: SpeakerRole) -> SpeakerRoleSpec {
    match role {
        SpeakerRole::Left => SpeakerRoleSpec::Left,
        SpeakerRole::Right => SpeakerRoleSpec::Right,
        SpeakerRole::Center => SpeakerRoleSpec::Center,
        SpeakerRole::Lfe => SpeakerRoleSpec::Lfe,
        SpeakerRole::SurroundLeft => SpeakerRoleSpec::SurroundLeft,
        SpeakerRole::SurroundRight => SpeakerRoleSpec::SurroundRight,
        SpeakerRole::Room { x_mm, y_mm, z_mm } => SpeakerRoleSpec::Room { x_mm, y_mm, z_mm },
    }
}

/// What the person asks of their speakers, declared once: form a group, change it, remove it, make
/// it the output, sound one output, and measure and keep a correction for it.
///
/// The commands reach the fabric the speaker groups live in, so each is an effect outside the
/// record that only a device of the person's can carry out.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.device.speakers")]
pub enum SpeakerEffect {
    /// Form a group of speakers across the person's devices
    #[effect(kind = "outside")]
    #[effect(own_device)]
    CreateGroup {
        /// What the group is called
        #[param("What the group is called")]
        name: String,
        /// The speakers that join it, in the order they play
        #[param("The speakers that join it")]
        members: Vec<SpeakerMemberChoice>,
    },
    /// Change a group's name or its speakers
    #[effect(kind = "outside")]
    #[effect(own_device)]
    UpdateGroup {
        /// The group's id, as the reading lists it
        #[param("The group's id")]
        group: String,
        /// What it is called now
        #[param("What the group is called")]
        name: String,
        /// The speakers it plays through now
        #[param("The speakers that join it")]
        members: Vec<SpeakerMemberChoice>,
    },
    /// Remove a group
    #[effect(kind = "outside")]
    #[effect(own_device)]
    #[effect(risk = tilo_actions::RiskLevel::Destructive)]
    DeleteGroup {
        /// The group's id, as the reading lists it
        #[param("The group's id")]
        group: String,
    },
    /// Play a short sound on one output, so the person can tell which speaker it is
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Identify {
        /// The device's fabric id
        #[param("The device, as the reading lists it")]
        device: String,
        /// The output's selector
        #[param("The output, as the reading lists it")]
        output: String,
    },
    /// Measure every speaker of a group with the microphones of the person's devices
    #[effect(kind = "outside")]
    #[effect(own_device)]
    StartCalibration {
        /// The group's id
        #[param("The group's id")]
        group: String,
        /// Devices whose microphone sits this run out
        #[param("The devices whose microphone sits this run out")]
        excluded: Vec<String>,
        /// The device at the listening position; left out for the middle of the microphones
        #[param("The device at the listening position")]
        listening: Option<String>,
    },
    /// Stop a run without keeping anything it measured
    #[effect(kind = "outside")]
    #[effect(own_device)]
    CancelCalibration {
        /// The group's id
        #[param("The group's id")]
        group: String,
    },
    /// Keep what a run measured
    #[effect(kind = "outside")]
    #[effect(own_device)]
    ApplyCalibration {
        /// The group's id
        #[param("The group's id")]
        group: String,
    },
    /// Put back the correction from before the last one kept
    #[effect(kind = "outside")]
    #[effect(own_device)]
    RevertCalibration {
        /// The group's id
        #[param("The group's id")]
        group: String,
    },
    /// Forget a finished measurement without keeping it
    #[effect(kind = "outside")]
    #[effect(own_device)]
    DismissCalibration {
        /// The group's id
        #[param("The group's id")]
        group: String,
    },
    /// Play everything this device plays on a group, as on a chosen output
    #[effect(kind = "outside")]
    #[effect(own_device)]
    UseAsOutput {
        /// The group's id
        #[param("The group's id")]
        group: String,
    },
    /// Play on this device's own outputs again
    #[effect(kind = "outside")]
    #[effect(own_device)]
    StopUsingAsOutput,
}

impl SpeakerEffect {
    /// The command this asks of the fabric.
    ///
    /// # Errors
    /// The effect is a reading and asks nothing.
    pub fn command(self) -> Result<SpeakerCommand, String> {
        Ok(match self {
            Self::CreateGroup { name, members } => SpeakerCommand::Create {
                name,
                members: members.into_iter().map(Into::into).collect(),
            },
            Self::UpdateGroup {
                group,
                name,
                members,
            } => SpeakerCommand::Update {
                group,
                name,
                members: members.into_iter().map(Into::into).collect(),
            },
            Self::DeleteGroup { group } => SpeakerCommand::Delete { group },
            Self::Identify { device, output } => SpeakerCommand::Identify { device, output },
            Self::StartCalibration {
                group,
                excluded,
                listening,
            } => SpeakerCommand::StartCalibration {
                group,
                excluded,
                listening,
            },
            Self::CancelCalibration { group } => SpeakerCommand::CancelCalibration { group },
            Self::ApplyCalibration { group } => SpeakerCommand::ApplyCalibration { group },
            Self::RevertCalibration { group } => SpeakerCommand::RevertCalibration { group },
            Self::DismissCalibration { group } => SpeakerCommand::DismissCalibration { group },
            Self::UseAsOutput { group } => SpeakerCommand::UseAsOutput { group },
            Self::StopUsingAsOutput => SpeakerCommand::StopUsingAsOutput,
        })
    }
}

/// The speakers as the session publishes them: the reading and the verbs, which a tilolang view
/// reads as `device.speakers` and calls through the session's API.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Speakers, SpeakerEffect>("speakers", "Speakers")
}

/// The speakers as they stand now, as the resource shows them.
#[must_use]
pub fn reading(network: &dyn FabricLink) -> Speakers {
    Speakers::of(&crate::settings::snapshot_of(network))
}
