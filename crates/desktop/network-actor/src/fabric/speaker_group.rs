//! Speaker groups: several outputs on a person's certified devices playing as
//! one stereo pair, surround set, or set of points around a room.
//!
//! A group is authored by the device that created it (its leader) and kept
//! in that device's own part of the person's store: no other device can
//! forge or edit it. Membership is the other half. Each
//! member device asks its own user once ("Join 'Living room' as the right
//! speaker?") and, on approval, authors a [`SpeakerMembership`] in *its* own
//! part. A member plays for a group only while both records agree on the
//! group, the output and the role, so changing a member's role or output asks
//! again, and leaving, deleting the group or disabling media continuity ends
//! it.
//!
//! Calibration may listen with the microphones of any of the person's
//! devices, not only the leader's. Each such device asks its own user once
//! ("Let the shell use this microphone to calibrate 'Living room'?") and, on
//! approval, authors a [`SpeakerListener`] in its own part, bound to the
//! group, its leader and that one microphone.
//!
//! Outputs are named by [`EndpointSelector`]s the owning compositor mints from
//! its local audio graph. `PipeWire` node ids and node names never leave the
//! device; a selector means something only to the compositor that published
//! it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::fabric::DeviceId;
use crate::fabric::FabricError;
use crate::fabric::FabricStamp;
pub use runtime_collab::SpeakerMemberStatus;
// The rules a group, its members and their trims keep are the stored record's: the person's
// records write them as strings and integers, and both sides read the one copy in the
// workspace service.
use tilo_workspace_service::workspaces::person_workspace::speaker as speaker_record;
pub use tilo_workspace_service::workspaces::person_workspace::speaker::{
    MAX_CONSENTS as MAX_SPEAKER_MEMBERSHIPS, MAX_CONSENTS as MAX_SPEAKER_LISTENERS,
    MAX_DELAY_NS as MAX_SPEAKER_DELAY_NS, MAX_ENDPOINTS as MAX_AUDIO_ENDPOINTS,
    MAX_EQ_BANDS as MAX_SPEAKER_EQ_BANDS, MAX_GAIN_MB as MAX_SPEAKER_GAIN_MB,
    MAX_GROUPS as MAX_SPEAKER_GROUPS, MAX_LABEL_BYTES as MAX_AUDIO_ENDPOINT_LABEL_BYTES,
    MAX_MEMBERS as MAX_SPEAKER_GROUP_MEMBERS, MAX_NAME_CHARS as MAX_SPEAKER_GROUP_NAME_CHARS,
    MAX_ROOM_MM as MAX_ROOM_COORDINATE_MM, MIN_GAIN_MB as MIN_SPEAKER_GAIN_MB,
    ROOM_SIDE_MM as ROOM_SIDE_THRESHOLD_MM, RoomPoint, SpeakerEqBand, SpeakerRole, StereoSide,
    SurroundChannel, valid_name as valid_group_name,
};

const SELECTOR_DOMAIN: &str = "so.tilo.speaker-group.endpoint-selector.v1";

/// Random identity of one group, minted by its leader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SpeakerGroupId(pub [u8; 16]);

impl SpeakerGroupId {
    #[must_use]
    pub fn valid(self) -> bool {
        self.0 != [0; 16]
    }

    /// Lowercase hex, the form actions and settings use to name a group.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    #[must_use]
    pub fn from_hex(value: &str) -> Option<Self> {
        let id = Self(speaker_record::selector_bytes(value)?);
        id.valid().then_some(id)
    }
}

/// Opaque, compositor-minted name for one audio endpoint on one device.
///
/// It is derived from the device identity and the endpoint's stable local
/// name, so it survives hotplug and restarts without ever disclosing the
/// `PipeWire` node id or node name it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EndpointSelector(pub [u8; 16]);

impl EndpointSelector {
    /// Mint the selector this device publishes for one local endpoint.
    #[must_use]
    pub fn mint(device: &DeviceId, local_node_name: &str) -> Self {
        let mut material = Vec::with_capacity(
            device
                .as_str()
                .len()
                .saturating_add(local_node_name.len())
                .saturating_add(1),
        );
        material.extend_from_slice(device.as_str().as_bytes());
        material.push(0);
        material.extend_from_slice(local_node_name.as_bytes());
        let digest = blake3::derive_key(SELECTOR_DOMAIN, &material);
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        if bytes == [0; 16] {
            bytes[0] = 1;
        }
        Self(bytes)
    }

    #[must_use]
    pub fn valid(self) -> bool {
        self.0 != [0; 16]
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    #[must_use]
    pub fn from_hex(value: &str) -> Option<Self> {
        let selector = Self(speaker_record::selector_bytes(value)?);
        selector.valid().then_some(selector)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioEndpointKind {
    Output,
    Microphone,
}

/// One output or microphone a device offers to speaker groups and
/// calibration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEndpoint {
    pub selector: EndpointSelector,
    pub kind: AudioEndpointKind,
    pub label: String,
    pub is_default: bool,
}

impl AudioEndpoint {
    fn validate(&self) -> Result<(), FabricError> {
        if !self.selector.valid() || !speaker_record::valid_label(&self.label) {
            return Err(FabricError::InvalidSpeakerGroup);
        }
        Ok(())
    }
}

/// Short human description of `role`, used in consent prompts and status rows.
#[must_use]
pub fn describe_role(role: SpeakerRole) -> String {
    crate::i18n::t(role.describe_key())
}

/// Per-member corrections a calibration produces.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerTrim {
    /// Extra playout delay so this member arrives with the latest one.
    pub delay_ns: u64,
    /// Level change in millibels so this member matches the quietest one.
    pub gain_mb: i32,
    pub eq: Vec<SpeakerEqBand>,
}

impl SpeakerTrim {
    /// Convert one member's measured correction into a roaming record.
    #[must_use]
    pub fn from_calibration(calibration: &media::calibration::MemberCalibration) -> Self {
        let delay_ns = u64::try_from(calibration.delay_trim_ns.max(0))
            .unwrap_or(0)
            .min(MAX_SPEAKER_DELAY_NS);
        let gain_mb = round_mb(calibration.gain_trim_db).clamp(MIN_SPEAKER_GAIN_MB, 0);
        let eq = calibration
            .eq
            .iter()
            .take(MAX_SPEAKER_EQ_BANDS)
            .filter_map(|band| {
                let band = SpeakerEqBand {
                    center_hz: round_u32(band.center_hz),
                    gain_mb: round_mb(band.gain_db).clamp(-2_400, 0),
                    q_milli: round_u32(band.q * 1000.0),
                };
                band.valid().then_some(band)
            })
            .collect();
        Self {
            delay_ns,
            gain_mb,
            eq,
        }
    }

    /// Gain in dB, as the playout path applies it.
    #[must_use]
    pub fn gain_db(&self) -> f64 {
        f64::from(self.gain_mb) / 100.0
    }

    /// Filters in the form `media::calibration` evaluates.
    #[must_use]
    pub fn eq_bands(&self) -> Vec<media::calibration::EqBand> {
        self.eq
            .iter()
            .map(|band| media::calibration::EqBand {
                center_hz: f64::from(band.center_hz),
                gain_db: f64::from(band.gain_mb) / 100.0,
                q: f64::from(band.q_milli) / 1000.0,
            })
            .collect()
    }

    fn valid(&self) -> bool {
        speaker_record::trim_holds(self.delay_ns, self.gain_mb, &self.eq)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerGroupMember {
    pub device: DeviceId,
    pub output: EndpointSelector,
    pub role: SpeakerRole,
    pub trim: SpeakerTrim,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerGroup {
    pub id: SpeakerGroupId,
    pub name: String,
    /// The device that authored this group. Its own outputs may be members
    /// without a separate consent: the group was created there.
    pub leader: DeviceId,
    pub members: Vec<SpeakerGroupMember>,
    pub stamp: FabricStamp,
}

impl SpeakerGroup {
    /// Structural rules that hold regardless of which devices are online.
    ///
    /// # Errors
    ///
    /// [`FabricError::ForeignAuthorship`] when `owner` did not author it, and
    /// [`FabricError::InvalidSpeakerGroup`] for a bad name, trim or member
    /// set: duplicate outputs, duplicate roles, a stereo side or surround
    /// channel without its partner, or room points mixed with channel roles.
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.leader != owner || &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        if !self.id.valid()
            || !valid_group_name(&self.name)
            || self.members.is_empty()
            || self.members.len() > MAX_SPEAKER_GROUP_MEMBERS
        {
            return Err(FabricError::InvalidSpeakerGroup);
        }
        validate_members(&self.members)
    }

    #[must_use]
    pub fn member(&self, device: &DeviceId) -> Option<&SpeakerGroupMember> {
        self.members.iter().find(|member| &member.device == device)
    }

    /// The part of the source each member renders, in member order.
    #[must_use]
    pub fn channel_roles(&self) -> Vec<media::sync_playout::ChannelRole> {
        channel_roles(&self.members)
    }
}

/// Check a member list the way [`SpeakerGroup::validate`] does. Setup flows
/// use it to explain what is still missing before anything is saved.
///
/// # Errors
///
/// [`FabricError::InvalidSpeakerGroup`] for the first violated rule.
pub fn validate_members(members: &[SpeakerGroupMember]) -> Result<(), FabricError> {
    let invalid = Err(FabricError::InvalidSpeakerGroup);
    if members
        .iter()
        .any(|member| !member.output.valid() || !member.role.valid() || !member.trim.valid())
    {
        return invalid;
    }
    let mut outputs = BTreeSet::new();
    if !members
        .iter()
        .all(|member| outputs.insert((&member.device, member.output)))
    {
        return invalid;
    }
    let roles = members.iter().map(|member| member.role).collect::<Vec<_>>();
    if !speaker_record::roles_hold(&roles) {
        return invalid;
    }
    Ok(())
}

/// Map each member's role onto what `sync_playout` renders from a stereo
/// source. Surround sides take the matching stereo side. Room points play a
/// side only when another member stands on the opposite side of the
/// listener; otherwise the member would silently drop half the music, so it
/// plays the mono downmix instead.
#[must_use]
pub fn channel_roles(members: &[SpeakerGroupMember]) -> Vec<media::sync_playout::ChannelRole> {
    use media::sync_playout::ChannelRole;
    let side = |point: RoomPoint| {
        if point.x_mm < -ROOM_SIDE_THRESHOLD_MM {
            Some(StereoSide::Left)
        } else if point.x_mm > ROOM_SIDE_THRESHOLD_MM {
            Some(StereoSide::Right)
        } else {
            None
        }
    };
    let room_sides = members
        .iter()
        .filter_map(|member| match member.role {
            SpeakerRole::RoomPoint(point) => side(point),
            SpeakerRole::Stereo(_) | SpeakerRole::Surround(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let room_is_stereo = room_sides.len() == 2;
    members
        .iter()
        .map(|member| match member.role {
            SpeakerRole::Stereo(StereoSide::Left)
            | SpeakerRole::Surround(SurroundChannel::SurroundLeft) => ChannelRole::Left,
            SpeakerRole::Stereo(StereoSide::Right)
            | SpeakerRole::Surround(SurroundChannel::SurroundRight) => ChannelRole::Right,
            SpeakerRole::Surround(SurroundChannel::Center) => ChannelRole::Center,
            SpeakerRole::Surround(SurroundChannel::Lfe) => ChannelRole::Lfe,
            SpeakerRole::RoomPoint(point) => match side(point).filter(|_| room_is_stereo) {
                Some(StereoSide::Left) => ChannelRole::Left,
                Some(StereoSide::Right) => ChannelRole::Right,
                None => ChannelRole::Mono,
            },
        })
        .collect()
}

/// A member device's durable, locally approved consent to play one exact
/// role on one exact output for one exact group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerMembership {
    pub group: SpeakerGroupId,
    pub leader: DeviceId,
    pub output: EndpointSelector,
    pub role: SpeakerRole,
    pub stamp: FabricStamp,
}

impl SpeakerMembership {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        // The leader's own outputs need no membership, so one naming itself
        // as leader is malformed rather than redundant.
        if !self.group.valid()
            || &self.leader == owner
            || !self.output.valid()
            || !self.role.valid()
        {
            return Err(FabricError::InvalidSpeakerGroup);
        }
        Ok(())
    }

    /// Whether this consent covers `member` of `group` as it stands now.
    #[must_use]
    pub fn covers(&self, group: &SpeakerGroup, member: &SpeakerGroupMember) -> bool {
        let same_group = group.id == self.group;
        same_group
            && self.leader == group.leader
            && self.output == member.output
            && self.role == member.role
    }
}

/// A device's durable, locally approved consent to let one group's leader
/// listen with one exact microphone while it calibrates that group. The
/// recording never leaves the device: only the arrival time, level and
/// response measured from it do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerListener {
    pub group: SpeakerGroupId,
    pub leader: DeviceId,
    pub microphone: EndpointSelector,
    pub stamp: FabricStamp,
}

impl SpeakerListener {
    fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        // The leader listens with its own microphones without asking.
        if !self.group.valid() || &self.leader == owner || !self.microphone.valid() {
            return Err(FabricError::InvalidSpeakerGroup);
        }
        Ok(())
    }

    /// Whether this consent covers `microphone` for `group` as its leader
    /// publishes it now.
    #[must_use]
    pub fn covers(&self, group: &SpeakerGroup, microphone: EndpointSelector) -> bool {
        let same_group = self.group == group.id;
        same_group && self.leader == group.leader && self.microphone == microphone
    }
}

/// What one device keeps of speaker groups in its part of the person's store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerFabricState {
    /// Outputs and microphones this device offers, named by selector.
    pub endpoints: Vec<AudioEndpoint>,
    /// Groups this device leads.
    pub groups: Vec<SpeakerGroup>,
    /// Groups led elsewhere that this device agreed to play in.
    pub memberships: Vec<SpeakerMembership>,
    /// Groups led elsewhere that may listen with one of this device's
    /// microphones while calibrating.
    pub listeners: Vec<SpeakerListener>,
}

impl SpeakerFabricState {
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if self.endpoints.len() > MAX_AUDIO_ENDPOINTS
            || self.groups.len() > MAX_SPEAKER_GROUPS
            || self.memberships.len() > MAX_SPEAKER_MEMBERSHIPS
            || self.listeners.len() > MAX_SPEAKER_LISTENERS
        {
            return Err(FabricError::TooLarge);
        }
        for endpoint in &self.endpoints {
            endpoint.validate()?;
        }
        for group in &self.groups {
            group.validate(owner)?;
        }
        for membership in &self.memberships {
            membership.validate(owner)?;
        }
        for listener in &self.listeners {
            listener.validate(owner)?;
        }
        let unique = |ok: bool| {
            if ok {
                Ok(())
            } else {
                Err(FabricError::DuplicateItem)
            }
        };
        let mut selectors = BTreeSet::new();
        unique(
            self.endpoints
                .iter()
                .all(|endpoint| selectors.insert(endpoint.selector)),
        )?;
        let mut groups = BTreeSet::new();
        unique(self.groups.iter().all(|group| groups.insert(group.id)))?;
        let mut memberships = BTreeSet::new();
        unique(
            self.memberships
                .iter()
                .all(|membership| memberships.insert(membership.group)),
        )?;
        let mut listeners = BTreeSet::new();
        unique(
            self.listeners
                .iter()
                .all(|listener| listeners.insert(listener.group)),
        )?;
        Ok(())
    }

    #[must_use]
    pub fn output(&self, selector: EndpointSelector) -> Option<&AudioEndpoint> {
        self.endpoints.iter().find(|endpoint| {
            endpoint.selector == selector && endpoint.kind == AudioEndpointKind::Output
        })
    }

    #[must_use]
    pub fn microphone(&self, selector: EndpointSelector) -> Option<&AudioEndpoint> {
        self.endpoints.iter().find(|endpoint| {
            endpoint.selector == selector && endpoint.kind == AudioEndpointKind::Microphone
        })
    }

    /// The microphone calibration listens with: the default one, else the
    /// first.
    #[must_use]
    pub fn calibration_microphone(&self) -> Option<&AudioEndpoint> {
        let mut microphones = self
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.kind == AudioEndpointKind::Microphone);
        let first = microphones.clone().next();
        microphones.find(|endpoint| endpoint.is_default).or(first)
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is rounded and clamped into the i32 range first"
)]
fn round_mb(db: f64) -> i32 {
    if db.is_finite() {
        (db * 100.0)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    } else {
        0
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped into the u32 range first"
)]
fn round_u32(value: f64) -> u32 {
    if value.is_finite() {
        value.round().clamp(0.0, f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

/// Most points of a heard sweep's magnitude response a reading carries.
pub const MAX_RESPONSE_POINTS: usize = 512;
/// Furthest from the scheduled start a reported arrival may be.
const MAX_REPORTED_ARRIVAL_NS: i64 = 1_000_000_000;

/// Why a microphone has no reading of a sweep. Typed, so no process detail
/// of the listening device crosses the fabric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HearingFailure {
    /// Nothing stood out of the noise: too far away, or muted.
    NoArrival,
    /// That device has not agreed to listen for this group.
    NotAllowed,
    MicrophoneGone,
    /// The request arrived too late, or too far ahead, to record around.
    OutOfWindow,
    /// That device is already recording another sweep.
    Busy,
    CaptureFailed,
    /// No estimate of that device's clock yet.
    ClockUnknown,
    /// No answer before the sweep's deadline.
    NoAnswer,
}

impl HearingFailure {
    #[must_use]
    pub fn describe(self) -> String {
        crate::i18n::t(match self {
            Self::NoArrival => "speaker-calibration-reason-not-heard",
            Self::NotAllowed => "speaker-calibration-reason-not-allowed",
            Self::MicrophoneGone => "speaker-calibration-reason-microphone-gone",
            Self::OutOfWindow => "speaker-calibration-reason-out-of-window",
            Self::Busy => "speaker-calibration-reason-busy",
            Self::CaptureFailed => "speaker-calibration-reason-capture-failed",
            Self::ClockUnknown => "speaker-calibration-reason-clock-unknown",
            Self::NoAnswer => "speaker-calibration-reason-no-answer",
        })
    }
}

/// What one microphone heard of one sweep, as the listening device reports
/// it: numbers measured from the recording, never the recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeakerHearing {
    Heard {
        /// Direct sound after the scheduled start, on the listening device's
        /// clock.
        arrival_ns: i64,
        /// Level and peak-to-noise ratio in hundredths of a dB.
        level_cdb: i32,
        snr_cdb: i32,
        /// The smoothed magnitude response on the sweep's fixed grid, in
        /// hundredths of a dB.
        response_cdb: Vec<i16>,
    },
    NotHeard {
        failure: HearingFailure,
    },
}

impl SpeakerHearing {
    /// Bounded and plausible, as the fabric admits it.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Heard {
                arrival_ns,
                response_cdb,
                ..
            } => {
                (-MAX_REPORTED_ARRIVAL_NS..=MAX_REPORTED_ARRIVAL_NS).contains(arrival_ns)
                    && response_cdb.len() <= MAX_RESPONSE_POINTS
            }
            Self::NotHeard { .. } => true,
        }
    }

    #[must_use]
    pub const fn failure(&self) -> Option<HearingFailure> {
        match self {
            Self::Heard { .. } => None,
            Self::NotHeard { failure } => Some(*failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use media::sync_playout::ChannelRole;

    use super::*;

    fn id(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }

    fn member(device: &str, role: SpeakerRole) -> SpeakerGroupMember {
        SpeakerGroupMember {
            device: id(device),
            output: EndpointSelector::mint(&id(device), "alsa_output.analog-stereo"),
            role,
            trim: SpeakerTrim::default(),
        }
    }

    fn group(members: Vec<SpeakerGroupMember>) -> SpeakerGroup {
        SpeakerGroup {
            id: SpeakerGroupId([7; 16]),
            name: "Living room".into(),
            leader: id("tower"),
            members,
            stamp: FabricStamp {
                counter: 1,
                device: id("tower"),
            },
        }
    }

    const LEFT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Left);
    const RIGHT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Right);

    #[test]
    fn a_stereo_pair_across_two_devices_is_valid_and_includes_the_leader() {
        let pair = group(vec![member("tower", LEFT), member("shelf", RIGHT)]);
        assert_eq!(pair.validate(&id("tower")), Ok(()));
        assert_eq!(
            pair.channel_roles(),
            vec![ChannelRole::Left, ChannelRole::Right]
        );
    }

    #[test]
    fn roles_must_be_unique_and_complete() {
        for members in [
            vec![member("tower", LEFT), member("shelf", LEFT)],
            vec![member("tower", LEFT)],
            vec![
                member("tower", LEFT),
                member("shelf", RIGHT),
                member("tv", SpeakerRole::Surround(SurroundChannel::SurroundLeft)),
            ],
            vec![member("tv", SpeakerRole::Surround(SurroundChannel::Center))],
            vec![
                member("tower", LEFT),
                member("shelf", RIGHT),
                member(
                    "tv",
                    SpeakerRole::RoomPoint(RoomPoint {
                        x_mm: 0,
                        y_mm: 2000,
                        z_mm: 0,
                    }),
                ),
            ],
        ] {
            assert_eq!(
                group(members.clone()).validate(&id("tower")),
                Err(FabricError::InvalidSpeakerGroup),
                "{members:?}"
            );
        }
    }

    /// A left and a right member, validated as one device: each row is the output the right
    /// member plays on, the device that validates the group as its author, and the verdict.
    #[test]
    fn a_pair_is_valid_only_from_its_leader_with_an_output_for_each_role() {
        let right_on = |output: EndpointSelector| {
            let mut right = member("tower", RIGHT);
            right.output = output;
            right
        };
        let rows = [
            (
                "only_the_leader_authors_its_group",
                vec![member("tower", LEFT), member("shelf", RIGHT)],
                "shelf",
                Err(FabricError::ForeignAuthorship),
            ),
            (
                "one_output_cannot_play_two_roles",
                vec![
                    member("tower", LEFT),
                    right_on(member("tower", LEFT).output),
                ],
                "tower",
                Err(FabricError::InvalidSpeakerGroup),
            ),
            (
                "two_outputs_on_one_device_can_form_a_pair",
                vec![
                    member("tower", LEFT),
                    right_on(EndpointSelector::mint(&id("tower"), "bluez_output.speaker")),
                ],
                "tower",
                Ok(()),
            ),
        ];
        for (row, members, author, verdict) in rows {
            assert_eq!(group(members).validate(&id(author)), verdict, "row `{row}`");
        }
    }

    #[test]
    fn a_five_one_set_maps_every_channel() {
        let surround = |channel| SpeakerRole::Surround(channel);
        let set = group(vec![
            member("a", LEFT),
            member("b", RIGHT),
            member("c", surround(SurroundChannel::Center)),
            member("d", surround(SurroundChannel::Lfe)),
            member("e", surround(SurroundChannel::SurroundLeft)),
            member("f", surround(SurroundChannel::SurroundRight)),
        ]);
        assert_eq!(set.validate(&id("tower")), Ok(()));
        assert_eq!(
            set.channel_roles(),
            vec![
                ChannelRole::Left,
                ChannelRole::Right,
                ChannelRole::Center,
                ChannelRole::Lfe,
                ChannelRole::Left,
                ChannelRole::Right,
            ]
        );
    }

    #[test]
    fn room_points_play_a_side_only_when_the_other_side_is_covered() {
        let point = |x_mm| {
            SpeakerRole::RoomPoint(RoomPoint {
                x_mm,
                y_mm: 2_000,
                z_mm: 0,
            })
        };
        let spread = group(vec![
            member("a", point(-1_500)),
            member("b", point(1_500)),
            member("c", point(0)),
        ]);
        assert_eq!(spread.validate(&id("tower")), Ok(()));
        assert_eq!(
            spread.channel_roles(),
            vec![ChannelRole::Left, ChannelRole::Right, ChannelRole::Mono]
        );
        let lopsided = group(vec![member("a", point(-1_500)), member("b", point(-3_000))]);
        assert_eq!(
            lopsided.channel_roles(),
            vec![ChannelRole::Mono, ChannelRole::Mono]
        );
    }

    #[test]
    fn room_points_must_be_distinct_and_inside_the_room_bound() {
        let at = |x_mm| {
            SpeakerRole::RoomPoint(RoomPoint {
                x_mm,
                y_mm: 0,
                z_mm: 0,
            })
        };
        assert_eq!(
            group(vec![member("a", at(10)), member("b", at(10))]).validate(&id("tower")),
            Err(FabricError::InvalidSpeakerGroup)
        );
        assert_eq!(
            group(vec![member("a", at(MAX_ROOM_COORDINATE_MM + 1))]).validate(&id("tower")),
            Err(FabricError::InvalidSpeakerGroup)
        );
    }

    #[test]
    fn names_are_bounded_and_trimmed() {
        for name in ["", "  padded ", "tab\there", &"x".repeat(65)] {
            let mut named = group(vec![member("tower", LEFT), member("shelf", RIGHT)]);
            named.name = name.into();
            assert_eq!(
                named.validate(&id("tower")),
                Err(FabricError::InvalidSpeakerGroup),
                "{name:?}"
            );
        }
    }

    #[test]
    fn calibration_trims_round_trip_into_bounded_integers() {
        let calibration = media::calibration::MemberCalibration {
            delay_trim_ns: 3_456_789,
            gain_trim_db: -2.345,
            eq: vec![media::calibration::EqBand {
                center_hz: 62.4,
                gain_db: -6.02,
                q: 4.3,
            }],
        };
        let trim = SpeakerTrim::from_calibration(&calibration);
        assert_eq!(trim.delay_ns, 3_456_789);
        assert_eq!(trim.gain_mb, -235);
        assert_eq!(
            trim.eq,
            vec![SpeakerEqBand {
                center_hz: 62,
                gain_mb: -602,
                q_milli: 4_300,
            }]
        );
        assert!(trim.valid());
        let wild = media::calibration::MemberCalibration {
            delay_trim_ns: i64::MAX,
            gain_trim_db: f64::NAN,
            eq: Vec::new(),
        };
        let clamped = SpeakerTrim::from_calibration(&wild);
        assert_eq!(clamped.delay_ns, MAX_SPEAKER_DELAY_NS);
        assert_eq!(clamped.gain_mb, 0);
    }

    #[test]
    fn membership_covers_only_the_exact_group_output_and_role() {
        let pair = group(vec![member("tower", LEFT), member("shelf", RIGHT)]);
        let right = pair.member(&id("shelf")).unwrap();
        let consent = SpeakerMembership {
            group: pair.id,
            leader: id("tower"),
            output: right.output,
            role: RIGHT,
            stamp: FabricStamp {
                counter: 1,
                device: id("shelf"),
            },
        };
        assert_eq!(consent.validate(&id("shelf")), Ok(()));
        assert!(consent.covers(&pair, right));
        let mut swapped = pair.clone();
        swapped.members[1].role = LEFT;
        swapped.members[0].role = RIGHT;
        assert!(!consent.covers(&swapped, &swapped.members[1]));
        let mut elsewhere = pair.clone();
        elsewhere.leader = id("phone");
        assert!(!consent.covers(&elsewhere, right));
        assert_eq!(
            consent.validate(&id("tower")),
            Err(FabricError::ForeignAuthorship)
        );
    }

    #[test]
    fn selectors_are_stable_per_device_and_disclose_nothing() {
        let name = "alsa_output.pci-0000_66_00.6.analog-stereo";
        let a = EndpointSelector::mint(&id("tower"), name);
        assert_eq!(a, EndpointSelector::mint(&id("tower"), name));
        assert_ne!(a, EndpointSelector::mint(&id("shelf"), name));
        assert!(!a.to_hex().contains("alsa"));
        assert_eq!(EndpointSelector::from_hex(&a.to_hex()), Some(a));
        assert_eq!(EndpointSelector::from_hex(&"0".repeat(32)), None);
    }

    #[test]
    fn fabric_state_rejects_duplicates_and_oversize() {
        let owner = id("shelf");
        let endpoint = AudioEndpoint {
            selector: EndpointSelector::mint(&owner, "out"),
            kind: AudioEndpointKind::Output,
            label: "Speakers".into(),
            is_default: true,
        };
        let state = SpeakerFabricState {
            endpoints: vec![endpoint.clone(), endpoint],
            ..SpeakerFabricState::default()
        };
        assert_eq!(state.validate(&owner), Err(FabricError::DuplicateItem));
        let crowded = SpeakerFabricState {
            endpoints: (0..=MAX_AUDIO_ENDPOINTS)
                .map(|index| AudioEndpoint {
                    selector: EndpointSelector::mint(&owner, &index.to_string()),
                    kind: AudioEndpointKind::Output,
                    label: "Out".into(),
                    is_default: false,
                })
                .collect(),
            ..SpeakerFabricState::default()
        };
        assert_eq!(crowded.validate(&owner), Err(FabricError::TooLarge));
    }
}
