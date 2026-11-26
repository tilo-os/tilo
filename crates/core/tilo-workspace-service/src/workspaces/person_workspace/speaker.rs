//! Speaker groups across the person's devices, in the record of their devices.
//!
//! A group is the work of the device that leads it: it names the members, each one's output, role
//! and calibration trim ([`SpeakerLedGroup`]), and only that device's session writes it. A member
//! device writes its own consent ([`SpeakerConsent`]), which covers the group exactly as the
//! leader wrote it, and its own outputs and microphones ([`SpeakerEndpoint`]). One playback of a
//! group is one entry ([`SpeakerPlayback`]): the playing device offers it to a member, the member
//! admits it with a capability bound to that exact source, and either ends it.
//!
//! Every device of the person reads all of it from this one record, and every change is a typed
//! action of the person's own workspace. What stays live between the devices is the audio itself.
//!
//! The record keeps a member's role, trim and band as strings and integers, so the stored form
//! survives changes to the types. The types below and their rules are what those values mean:
//! every device validates the record by parsing it into them ([`SpeakerRole::from_record`]) and
//! asking the same rules, so a limit is written once here and nowhere else.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tilo_resource::Model;

use super::devices::Devices;

/// Groups one device may lead.
pub const MAX_GROUPS: usize = 16;
/// Members of one group.
pub const MAX_MEMBERS: usize = 8;
/// Consents one device may keep.
pub const MAX_CONSENTS: usize = 16;
/// Outputs and microphones one device may offer.
pub const MAX_ENDPOINTS: usize = 32;
/// Playbacks the record may hold at once.
pub const MAX_PLAYBACKS: usize = 32;
/// Characters in a group's name.
pub const MAX_NAME_CHARS: usize = 64;
/// Bytes in an endpoint's label.
pub const MAX_LABEL_BYTES: usize = 128;
/// Deepest delay a trim may add: sound travels about 170 m in this time.
pub const MAX_DELAY_NS: u64 = 500_000_000;
/// Deepest cut a trim may apply, in millibels.
pub const MIN_GAIN_MB: i32 = -4_000;
/// Highest lift a trim may apply, in millibels.
pub const MAX_GAIN_MB: i32 = 600;
/// Bands in one member's trim.
pub const MAX_EQ_BANDS: usize = 8;
/// Room points are millimetres from the listening position; ±50 m bounds every real room.
pub const MAX_ROOM_MM: i32 = 50_000;
/// A room point further than this to one side of the listener plays that side of a stereo source.
pub const ROOM_SIDE_MM: i32 = 250;

/// Which side of a stereo pair a member plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StereoSide {
    Left,
    Right,
}

/// A channel of a surround set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurroundChannel {
    Center,
    Lfe,
    SurroundLeft,
    SurroundRight,
}

/// A position relative to the listening position, in millimetres: `x` to the listener's right,
/// `y` straight ahead, `z` up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoomPoint {
    pub x_mm: i32,
    pub y_mm: i32,
    pub z_mm: i32,
}

impl RoomPoint {
    /// Whether the point lies inside [`MAX_ROOM_MM`] of the listener on every axis.
    #[must_use]
    pub fn valid(self) -> bool {
        [self.x_mm, self.y_mm, self.z_mm]
            .into_iter()
            .all(|value| value.unsigned_abs() <= MAX_ROOM_MM.unsigned_abs())
    }
}

/// What one member contributes to a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerRole {
    Stereo(StereoSide),
    Surround(SurroundChannel),
    RoomPoint(RoomPoint),
}

impl SpeakerRole {
    /// The catalogue key of the role's short noun phrase ("right speaker"), written to stand alone
    /// after "as" or at the start of a status row.
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

    /// The role `value` names in a record, such as `"surround-left"` or `"room:0:2000:0"`, or
    /// `None` for a name no role has and a room point that does not hold exactly three
    /// coordinates.
    #[must_use]
    pub fn from_record(value: &str) -> Option<Self> {
        Some(match value {
            "left" => Self::Stereo(StereoSide::Left),
            "right" => Self::Stereo(StereoSide::Right),
            "center" => Self::Surround(SurroundChannel::Center),
            "lfe" => Self::Surround(SurroundChannel::Lfe),
            "surround-left" => Self::Surround(SurroundChannel::SurroundLeft),
            "surround-right" => Self::Surround(SurroundChannel::SurroundRight),
            _ => {
                let mut parts = value.strip_prefix("room:")?.split(':');
                let x_mm = parts.next()?.parse().ok()?;
                let y_mm = parts.next()?.parse().ok()?;
                let z_mm = parts.next()?.parse().ok()?;
                if parts.next().is_some() {
                    return None;
                }
                Self::RoomPoint(RoomPoint { x_mm, y_mm, z_mm })
            }
        })
    }

    /// The role as a record writes it.
    #[must_use]
    pub fn to_record(self) -> String {
        match self {
            Self::Stereo(StereoSide::Left) => "left".to_owned(),
            Self::Stereo(StereoSide::Right) => "right".to_owned(),
            Self::Surround(SurroundChannel::Center) => "center".to_owned(),
            Self::Surround(SurroundChannel::Lfe) => "lfe".to_owned(),
            Self::Surround(SurroundChannel::SurroundLeft) => "surround-left".to_owned(),
            Self::Surround(SurroundChannel::SurroundRight) => "surround-right".to_owned(),
            Self::RoomPoint(point) => {
                format!("room:{}:{}:{}", point.x_mm, point.y_mm, point.z_mm)
            }
        }
    }

    /// Whether the role names a place a member may play.
    #[must_use]
    pub fn valid(self) -> bool {
        match self {
            Self::RoomPoint(point) => point.valid(),
            Self::Stereo(_) | Self::Surround(_) => true,
        }
    }
}

/// One output or microphone a device offers to speaker groups.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerEndpoint {
    /// Opaque, compositor-minted name for the endpoint, 16 bytes in hex.
    pub selector: String,
    /// It is a microphone; otherwise an output.
    pub microphone: bool,
    pub label: String,
    pub is_default: bool,
}

/// One peaking filter of a member's trim, in integer units so records compare exactly.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerEqBand {
    pub center_hz: u32,
    /// Millibels; calibration only ever cuts.
    pub gain_mb: i32,
    /// Quality factor × 1000.
    pub q_milli: u32,
}

impl SpeakerEqBand {
    /// Whether the band lies in the range a calibration may produce.
    #[must_use]
    pub fn valid(self) -> bool {
        (10..=24_000).contains(&self.center_hz)
            && (-2_400..=MAX_GAIN_MB).contains(&self.gain_mb)
            && (100..=30_000).contains(&self.q_milli)
    }
}

/// One member of a group as its leader wrote it.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerLedMember {
    /// The member device, by key (lowercase hex).
    pub device: String,
    /// The output it plays on, 16 bytes in hex.
    pub output: String,
    /// What it contributes: `left`, `right`, `center`, `lfe`, `surround-left`,
    /// `surround-right`, or `room:<x_mm>:<y_mm>:<z_mm>`.
    pub role: String,
    /// Extra playout delay, ns.
    pub delay_ns: u64,
    /// Level change, millibels.
    pub gain_mb: i32,
    pub eq: Vec<SpeakerEqBand>,
}

/// One group a device leads.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerLedGroup {
    /// The group's identity, 16 bytes in hex, minted by its leader.
    pub id: String,
    pub name: String,
    pub members: Vec<SpeakerLedMember>,
    /// When the leader last wrote it, ms since the epoch.
    pub at_ms: i64,
}

impl SpeakerLedGroup {
    /// The member for `device`, as the leader wrote it.
    #[must_use]
    pub fn member(&self, device: &str) -> Option<&SpeakerLedMember> {
        self.members.iter().find(|member| member.device == device)
    }
}

/// What one device agreed to of another device's group.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerConsent {
    /// The group, 16 bytes in hex.
    pub group: String,
    /// The device that leads it, by key.
    pub leader: String,
    /// It is a consent to play in the group; otherwise to let it listen with a microphone.
    pub play: bool,
    /// The output it plays on, or empty for a listening consent.
    pub output: String,
    /// The role it plays, or empty for a listening consent.
    pub role: String,
    /// The microphone it listens with, or empty for a playing consent.
    pub microphone: String,
    /// Whether the person approved it; a declined ask is kept so they are not asked again.
    pub approved: bool,
    /// When the person answered, ms since the epoch.
    pub at_ms: i64,
}

/// What one device keeps of speaker groups: what it offers, leads and agreed to.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerDevice {
    pub endpoints: Vec<SpeakerEndpoint>,
    pub groups: Vec<SpeakerLedGroup>,
    pub consents: Vec<SpeakerConsent>,
}

/// How far one playback offered to a member got.
#[derive(
    Model, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum PlaybackStage {
    /// Offered: the member decides whether to admit it.
    #[default]
    Offered,
    /// The member admitted it, with the capability in [`SpeakerPlayback::authorization`].
    Admitted,
    /// The member refused it.
    Refused,
}

/// One playback of a group to one member device.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerPlayback {
    /// The playing session this route belongs to, 16 bytes in hex.
    pub session: String,
    /// The playing device, by key.
    pub source: String,
    /// The member device it plays on, by key.
    pub target: String,
    /// The group, 16 bytes in hex.
    pub group: String,
    pub stage: PlaybackStage,
    /// The member's capability bound to this exact source, 16 bytes in hex once admitted.
    pub authorization: String,
    pub at_ms: i64,
}

/// The devices a group member names, from the record `record`.
#[must_use]
pub fn group<'a>(record: &'a Devices, id: &str) -> Option<&'a SpeakerLedGroup> {
    record
        .speaker_devices
        .values()
        .flat_map(|device| &device.groups)
        .find(|group| group.id == id)
}

/// Every group the record holds, in an order that does not move as devices write.
#[must_use]
pub fn groups(record: &Devices) -> Vec<&SpeakerLedGroup> {
    let mut groups = record
        .speaker_devices
        .values()
        .flat_map(|device| &device.groups)
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    groups
}

/// Whether `role` is one a member may contribute.
#[must_use]
pub fn valid_role(role: &str) -> bool {
    SpeakerRole::from_record(role).is_some_and(SpeakerRole::valid)
}

/// Whether `roles` form a group the source can play: each role once, a stereo side with its
/// partner, and either all room points or none.
#[must_use]
pub fn roles_hold(roles: &[SpeakerRole]) -> bool {
    let mut seen = BTreeSet::new();
    if !roles.iter().all(|role| seen.insert(*role)) {
        return false;
    }
    let room = roles
        .iter()
        .filter(|role| matches!(role, SpeakerRole::RoomPoint(_)))
        .count();
    if room != 0 && room != roles.len() {
        return false;
    }
    let has = |role: SpeakerRole| seen.contains(&role);
    let left = has(SpeakerRole::Stereo(StereoSide::Left));
    let right = has(SpeakerRole::Stereo(StereoSide::Right));
    let surround_left = has(SpeakerRole::Surround(SurroundChannel::SurroundLeft));
    let surround_right = has(SpeakerRole::Surround(SurroundChannel::SurroundRight));
    let any_surround = roles
        .iter()
        .any(|role| matches!(role, SpeakerRole::Surround(_)));
    left == right && surround_left == surround_right && !(any_surround && !(left && right))
}

/// Whether the roles of `members` form a group the source can play.
#[must_use]
pub fn valid_roles(members: &[SpeakerLedMember]) -> bool {
    let Some(roles) = members
        .iter()
        .map(|member| SpeakerRole::from_record(&member.role))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    roles_hold(&roles)
}

/// Whether a trim holds: a bounded delay, level and bands.
#[must_use]
pub fn trim_holds(delay_ns: u64, gain_mb: i32, eq: &[SpeakerEqBand]) -> bool {
    delay_ns <= MAX_DELAY_NS
        && (MIN_GAIN_MB..=MAX_GAIN_MB).contains(&gain_mb)
        && eq.len() <= MAX_EQ_BANDS
        && eq.iter().all(|band| band.valid())
}

/// Whether `member`'s trim holds.
#[must_use]
pub fn valid_trim(member: &SpeakerLedMember) -> bool {
    trim_holds(member.delay_ns, member.gain_mb, &member.eq)
}

/// Whether `value` is a usable group name: trimmed, bounded, printable.
#[must_use]
pub fn valid_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() == value.len()
        && value.chars().count() <= MAX_NAME_CHARS
        && !value.chars().any(char::is_control)
}

/// Whether `value` is a usable endpoint label: non-empty, bounded, printable.
#[must_use]
pub fn valid_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_LABEL_BYTES && !value.chars().any(char::is_control)
}

/// The 16 bytes `value` spells in canonical lowercase hex.
#[must_use]
pub fn selector_bytes(value: &str) -> Option<[u8; 16]> {
    let mut bytes = [0; 16];
    (hex::decode_to_slice(value, &mut bytes).is_ok() && value == hex::encode(bytes))
        .then_some(bytes)
}

/// Whether `value` is a selector: 16 bytes in lowercase hex.
#[must_use]
pub fn valid_selector(value: &str) -> bool {
    selector_bytes(value).is_some()
}

/// `groups`, with `group` added or replacing the one with its id: the count bounded.
///
/// # Errors
/// The group's id, name, members or an earlier group's limit.
pub fn keep_group(groups: &mut Vec<SpeakerLedGroup>, group: SpeakerLedGroup) -> Result<(), String> {
    if !valid_selector(&group.id) {
        return Err("a group is named by 16 bytes in hex".to_owned());
    }
    if !valid_name(&group.name) {
        return Err(format!(
            "a group is called in one line of 1 to {MAX_NAME_CHARS} characters"
        ));
    }
    if group.members.is_empty() || group.members.len() > MAX_MEMBERS {
        return Err(format!("a group has 1 to {MAX_MEMBERS} speakers"));
    }
    for member in &group.members {
        if !valid_selector(&member.output) || !valid_role(&member.role) || !valid_trim(member) {
            return Err("a group member names an output, a role and a trim that hold".to_owned());
        }
    }
    if !valid_roles(&group.members) {
        return Err(
            "a group's roles pair as a stereo or surround set, or are all room points".to_owned(),
        );
    }
    if let Some(kept) = groups.iter_mut().find(|kept| kept.id == group.id) {
        *kept = group;
    } else {
        if groups.len() >= MAX_GROUPS {
            return Err(format!("a device leads at most {MAX_GROUPS} groups"));
        }
        groups.push(group);
    }
    Ok(())
}

/// `members`, decoded from the postcard bytes its action carries.
///
/// # Errors
/// The bytes do not read as members.
pub fn decode_members(hex_bytes: &str) -> Result<Vec<SpeakerLedMember>, String> {
    let bytes = hex::decode(hex_bytes).map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes).map_err(|error| format!("a group's members do not read: {error}"))
}

/// `endpoints`, decoded from the postcard bytes its action carries.
///
/// # Errors
/// The bytes do not read as endpoints.
pub fn decode_endpoints(hex_bytes: &str) -> Result<Vec<SpeakerEndpoint>, String> {
    let bytes = hex::decode(hex_bytes).map_err(|error| error.to_string())?;
    postcard::from_bytes(&bytes)
        .map_err(|error| format!("a device's endpoints do not read: {error}"))
}

/// Where a member's output stands among `device`'s offered endpoints: an output selector.
#[must_use]
pub fn offers_output(device: &SpeakerDevice, output: &str) -> bool {
    device
        .endpoints
        .iter()
        .any(|endpoint| !endpoint.microphone && endpoint.selector == output)
}

/// Whether `device` offers `microphone`.
#[must_use]
pub fn offers_microphone(device: &SpeakerDevice, microphone: &str) -> bool {
    device
        .endpoints
        .iter()
        .any(|endpoint| endpoint.microphone && endpoint.selector == microphone)
}
