//! Speaker groups: several outputs on a person's certified devices playing as
//! one stereo pair, surround set, or set of points around a room.
//!
//! A group is authored by the device that created it (its leader) and roams
//! in that device's fabric snapshot, exactly like a display placement: no
//! other device can forge or edit it. Membership is the other half. Each
//! member device asks its own user once ("Join 'Living room' as the right
//! speaker?") and, on approval, authors a [`SpeakerMembership`] in *its* own
//! snapshot. A member plays for a group only while both records agree on the
//! group, the output and the role, so changing a member's role or output asks
//! again, and leaving, deleting the group or disabling media continuity ends
//! it.
//!
//! Calibration may listen with the microphones of any of the person's
//! devices, not only the leader's. Each such device asks its own user once
//! ("Let the desk use this microphone to calibrate 'Living room'?") and, on
//! approval, authors a [`SpeakerListener`] in its own snapshot, bound to the
//! group, its leader and that one microphone.
//!
//! Outputs are named by [`EndpointSelector`]s the owning compositor mints from
//! its local audio graph. `PipeWire` node ids and node names never leave the
//! device; a selector means something only to the compositor that published
//! it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::shell::device_fabric::{FabricError, FabricStamp};
use crate::shell::personal_continuity::DeviceId;
pub use runtime_collab::SpeakerMemberStatus;

pub const MAX_SPEAKER_GROUPS: usize = 16;
pub const MAX_SPEAKER_GROUP_MEMBERS: usize = 8;
pub const MAX_SPEAKER_MEMBERSHIPS: usize = 16;
pub const MAX_SPEAKER_LISTENERS: usize = 16;
pub const MAX_AUDIO_ENDPOINTS: usize = 32;
pub const MAX_SPEAKER_GROUP_NAME_CHARS: usize = 64;
pub const MAX_AUDIO_ENDPOINT_LABEL_BYTES: usize = 128;
/// Longest alignment delay a calibration may assign: sound travels about
/// 170 m in this time, far beyond any room.
pub const MAX_SPEAKER_DELAY_NS: u64 = 500_000_000;
/// Deepest level trim, in millibels (hundredths of a dB).
pub const MIN_SPEAKER_GAIN_MB: i32 = -4_000;
/// Calibration only cuts; a small manual lift stays representable.
pub const MAX_SPEAKER_GAIN_MB: i32 = 600;
pub const MAX_SPEAKER_EQ_BANDS: usize = 8;
/// Room points are millimetres from the listening position; ±50 m bounds
/// every real room.
pub const MAX_ROOM_COORDINATE_MM: i32 = 50_000;
/// A room point further than this to one side of the listener plays that
/// side of a stereo source, provided some other member covers the opposite
/// side.
pub const ROOM_SIDE_THRESHOLD_MM: i32 = 250;

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
        let mut bytes = [0; 16];
        hex::decode_to_slice(value, &mut bytes).ok()?;
        let id = Self(bytes);
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
        let mut bytes = [0; 16];
        hex::decode_to_slice(value, &mut bytes).ok()?;
        let selector = Self(bytes);
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
        if !self.selector.valid() || !valid_label(&self.label, MAX_AUDIO_ENDPOINT_LABEL_BYTES) {
            return Err(FabricError::InvalidSpeakerGroup);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StereoSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurroundChannel {
    Center,
    Lfe,
    SurroundLeft,
    SurroundRight,
}

/// A position relative to the listening position, in millimetres: `x` to
/// the listener's right, `y` straight ahead, `z` up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoomPoint {
    pub x_mm: i32,
    pub y_mm: i32,
    pub z_mm: i32,
}

impl RoomPoint {
    fn valid(self) -> bool {
        [self.x_mm, self.y_mm, self.z_mm]
            .into_iter()
            .all(|value| value.unsigned_abs() <= MAX_ROOM_COORDINATE_MM.unsigned_abs())
    }
}

/// What one member contributes to the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerRole {
    Stereo(StereoSide),
    Surround(SurroundChannel),
    RoomPoint(RoomPoint),
}

impl SpeakerRole {
    /// The catalogue key of the role's short noun phrase ("right speaker"),
    /// written to stand alone after "as" or at the start of a status row.
    #[must_use]
    pub const fn describe_key(self) -> &'static str {
        match self {
            Self::Stereo(StereoSide::Left) => "speaker-role-left",
            Self::Stereo(StereoSide::Right) => "speaker-role-right",
            Self::Surround(SurroundChannel::Center) => "speaker-role-centre",
            Self::Surround(SurroundChannel::Lfe) => "speaker-role-subwoofer",
            Self::Surround(SurroundChannel::SurroundLeft) => "speaker-role-surround-left",
            Self::Surround(SurroundChannel::SurroundRight) => "speaker-role-surround-right",
            Self::RoomPoint(_) => "speaker-role-room",
        }
    }

    /// Short human description, used in consent prompts and status rows.
    #[must_use]
    pub fn describe(self) -> String {
        crate::shell::i18n::t(self.describe_key())
    }

    #[must_use]
    pub fn valid(self) -> bool {
        match self {
            Self::RoomPoint(point) => point.valid(),
            Self::Stereo(_) | Self::Surround(_) => true,
        }
    }
}

/// One peaking filter, in integer units so records compare exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerEqBand {
    pub center_hz: u32,
    /// Millibels; calibration only ever cuts.
    pub gain_mb: i32,
    /// Quality factor × 1000.
    pub q_milli: u32,
}

impl SpeakerEqBand {
    fn valid(self) -> bool {
        (10..=24_000).contains(&self.center_hz)
            && (-2_400..=MAX_SPEAKER_GAIN_MB).contains(&self.gain_mb)
            && (100..=30_000).contains(&self.q_milli)
    }
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
        self.delay_ns <= MAX_SPEAKER_DELAY_NS
            && (MIN_SPEAKER_GAIN_MB..=MAX_SPEAKER_GAIN_MB).contains(&self.gain_mb)
            && self.eq.len() <= MAX_SPEAKER_EQ_BANDS
            && self.eq.iter().all(|band| band.valid())
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
    let mut roles = BTreeSet::new();
    for member in members {
        if !outputs.insert((&member.device, member.output)) || !roles.insert(member.role) {
            return invalid;
        }
    }
    let room = members
        .iter()
        .filter(|member| matches!(member.role, SpeakerRole::RoomPoint(_)))
        .count();
    if room != 0 && room != members.len() {
        return invalid;
    }
    let has = |role| roles.contains(&role);
    let left = has(SpeakerRole::Stereo(StereoSide::Left));
    let right = has(SpeakerRole::Stereo(StereoSide::Right));
    let surround_left = has(SpeakerRole::Surround(SurroundChannel::SurroundLeft));
    let surround_right = has(SpeakerRole::Surround(SurroundChannel::SurroundRight));
    let any_surround = roles
        .iter()
        .any(|role| matches!(role, SpeakerRole::Surround(_)));
    if left != right || surround_left != surround_right || (any_surround && !(left && right)) {
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

/// The speaker-group slice of one device's fabric snapshot.
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
    /// # Errors
    ///
    /// Returns the first violated bound, authorship or structural rule.
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if self.endpoints.len() > MAX_AUDIO_ENDPOINTS
            || self.groups.len() > MAX_SPEAKER_GROUPS
            || self.memberships.len() > MAX_SPEAKER_MEMBERSHIPS
            || self.listeners.len() > MAX_SPEAKER_LISTENERS
        {
            return Err(FabricError::SnapshotTooLarge);
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

/// Whether `value` is a usable group name: trimmed, bounded, printable.
#[must_use]
pub fn valid_group_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() == value.len()
        && value.chars().count() <= MAX_SPEAKER_GROUP_NAME_CHARS
        && !value.chars().any(char::is_control)
}

fn valid_label(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
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
            leader: id("desk"),
            members,
            stamp: FabricStamp {
                counter: 1,
                device: id("desk"),
            },
        }
    }

    const LEFT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Left);
    const RIGHT: SpeakerRole = SpeakerRole::Stereo(StereoSide::Right);

    #[test]
    fn a_stereo_pair_across_two_devices_is_valid_and_includes_the_leader() {
        let pair = group(vec![member("desk", LEFT), member("shelf", RIGHT)]);
        assert_eq!(pair.validate(&id("desk")), Ok(()));
        assert_eq!(
            pair.channel_roles(),
            vec![ChannelRole::Left, ChannelRole::Right]
        );
    }

    #[test]
    fn only_the_leader_authors_its_group() {
        let pair = group(vec![member("desk", LEFT), member("shelf", RIGHT)]);
        assert_eq!(
            pair.validate(&id("shelf")),
            Err(FabricError::ForeignAuthorship)
        );
    }

    #[test]
    fn roles_must_be_unique_and_complete() {
        for members in [
            vec![member("desk", LEFT), member("shelf", LEFT)],
            vec![member("desk", LEFT)],
            vec![
                member("desk", LEFT),
                member("shelf", RIGHT),
                member("tv", SpeakerRole::Surround(SurroundChannel::SurroundLeft)),
            ],
            vec![member("tv", SpeakerRole::Surround(SurroundChannel::Center))],
            vec![
                member("desk", LEFT),
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
                group(members.clone()).validate(&id("desk")),
                Err(FabricError::InvalidSpeakerGroup),
                "{members:?}"
            );
        }
    }

    #[test]
    fn one_output_cannot_play_two_roles() {
        let mut right = member("desk", RIGHT);
        right.output = member("desk", LEFT).output;
        assert_eq!(
            group(vec![member("desk", LEFT), right]).validate(&id("desk")),
            Err(FabricError::InvalidSpeakerGroup)
        );
    }

    #[test]
    fn two_outputs_on_one_device_can_form_a_pair() {
        let mut right = member("desk", RIGHT);
        right.output = EndpointSelector::mint(&id("desk"), "bluez_output.speaker");
        assert_eq!(
            group(vec![member("desk", LEFT), right]).validate(&id("desk")),
            Ok(())
        );
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
        assert_eq!(set.validate(&id("desk")), Ok(()));
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
        assert_eq!(spread.validate(&id("desk")), Ok(()));
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
            group(vec![member("a", at(10)), member("b", at(10))]).validate(&id("desk")),
            Err(FabricError::InvalidSpeakerGroup)
        );
        assert_eq!(
            group(vec![member("a", at(MAX_ROOM_COORDINATE_MM + 1))]).validate(&id("desk")),
            Err(FabricError::InvalidSpeakerGroup)
        );
    }

    #[test]
    fn names_are_bounded_and_trimmed() {
        for name in ["", "  padded ", "tab\there", &"x".repeat(65)] {
            let mut named = group(vec![member("desk", LEFT), member("shelf", RIGHT)]);
            named.name = name.into();
            assert_eq!(
                named.validate(&id("desk")),
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
        let pair = group(vec![member("desk", LEFT), member("shelf", RIGHT)]);
        let right = pair.member(&id("shelf")).unwrap();
        let consent = SpeakerMembership {
            group: pair.id,
            leader: id("desk"),
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
            consent.validate(&id("desk")),
            Err(FabricError::ForeignAuthorship)
        );
    }

    #[test]
    fn selectors_are_stable_per_device_and_disclose_nothing() {
        let name = "alsa_output.pci-0000_66_00.6.analog-stereo";
        let a = EndpointSelector::mint(&id("desk"), name);
        assert_eq!(a, EndpointSelector::mint(&id("desk"), name));
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
        assert_eq!(crowded.validate(&owner), Err(FabricError::SnapshotTooLarge));
    }
}
