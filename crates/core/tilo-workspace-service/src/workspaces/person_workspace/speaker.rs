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
    match role.split_once(':') {
        Some(("room", point)) => {
            let mut parts = point.split(':');
            let (Some(x), Some(y), Some(z), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            [x, y, z].into_iter().all(|coordinate| {
                coordinate
                    .parse::<i32>()
                    .is_ok_and(|value| value.unsigned_abs() <= MAX_ROOM_MM.unsigned_abs())
            })
        }
        Some(_) => false,
        None => matches!(
            role,
            "left" | "right" | "center" | "lfe" | "surround-left" | "surround-right"
        ),
    }
}

/// Whether the roles of `members` form a group the source can play: each role once, a stereo
/// side with its partner, and either all room points or none.
#[must_use]
pub fn valid_roles(members: &[SpeakerLedMember]) -> bool {
    let mut roles = std::collections::BTreeSet::new();
    if !members
        .iter()
        .all(|member| roles.insert(member.role.as_str()))
    {
        return false;
    }
    let room = members
        .iter()
        .filter(|member| member.role.starts_with("room:"))
        .count();
    if room != 0 && room != members.len() {
        return false;
    }
    let has = |role: &str| roles.contains(role);
    let left = has("left");
    let right = has("right");
    let surround_left = has("surround-left");
    let surround_right = has("surround-right");
    let any_surround = members.iter().any(|member| {
        member.role.starts_with("surround-") || member.role == "center" || member.role == "lfe"
    });
    left == right && surround_left == surround_right && !(any_surround && !(left && right))
}

/// Whether a trim holds: a bounded delay, level and bands.
#[must_use]
pub fn valid_trim(member: &SpeakerLedMember) -> bool {
    member.delay_ns <= MAX_DELAY_NS
        && (MIN_GAIN_MB..=MAX_GAIN_MB).contains(&member.gain_mb)
        && member.eq.len() <= MAX_EQ_BANDS
        && member.eq.iter().all(|band| {
            (10..=24_000).contains(&band.center_hz)
                && (-2_400..=MAX_GAIN_MB).contains(&band.gain_mb)
                && (100..=30_000).contains(&band.q_milli)
        })
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

/// Whether `value` is a selector: 16 bytes in lowercase hex.
#[must_use]
pub fn valid_selector(value: &str) -> bool {
    let mut bytes = [0; 16];
    hex::decode_to_slice(value, &mut bytes).is_ok() && value == hex::encode(bytes)
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
