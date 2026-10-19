//! Closed local call controls. Identity and capture authority never come from this wire.
use serde::{Deserialize, Serialize};

/// One message's bound. A call has no participant limit; a status names
/// each participant's stream (129 bytes) and person (64), so this carries
/// well over a thousand of them, and a [`Command::Pictures`] answer always
/// at least one whole picture of the largest layer
/// ([`MAX_PICTURE_BYTES`] in base64).
pub const MAX_WIRE_BYTES: usize = 1024 * 1024;
pub const MAX_INPUTS: usize = 128;
/// The largest picture one [`Picture`] carries: a tilo-video/1 object
/// (`tilo_moq::video::MAX_OBJECT_BYTES`).
pub const MAX_PICTURE_BYTES: usize = 512 * 1024;
/// Pictures one tile set may ask for at once: what a call view shows.
pub const MAX_SHOWN: usize = 64;
/// The longest a [`Command::Pictures`] is held before it is answered
/// without pictures.
pub const PICTURES_HOLD: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    /// Route-owner sequence; zero is a local template or a Status query.
    pub intent: u64,
    pub command: Command,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Join {
        channel_id: String,
    },
    SetMuted {
        muted: bool,
    },
    SelectInput {
        input_id: String,
    },
    Leave,
    Status,
    /// [`Command::Status`] once it differs from the status at `revision`
    /// ([`Snapshot::revision`]), or after [`WATCH_HOLD`] when nothing
    /// changed: how an app hears who joins, leaves and talks the moment it
    /// happens, with one question outstanding instead of a poll.
    Watch {
        revision: u64,
    },
    /// Turn this person's camera on or off in the call. Turning it on asks
    /// for the camera through the normal permission prompt, once for as
    /// long as the person allows it there.
    SetCamera {
        on: bool,
    },
    /// Share this person's screen in the call, or stop. Starting asks
    /// through the normal permission prompt.
    SetScreen {
        on: bool,
    },
    /// The pictures this app shows now, each at the layer that fits where
    /// it shows it: the call receives exactly these and nothing else.
    Show {
        pictures: Vec<Shown>,
    },
    /// The pictures received of what [`Command::Show`] asked for since the
    /// cursor `after` ([`Snapshot::pictures_cursor`]), answered as soon as
    /// there are any or after [`PICTURES_HOLD`].
    Pictures {
        after: u64,
    },
}

/// What a picture shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PictureSource {
    Camera,
    Screen,
}

/// How large a picture is received: `tilo_moq::video::Layer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PictureLayer {
    /// A face in a filmstrip or a small grid: 320 × 180.
    Low,
    /// A tile in a grid: 640 × 360 (cameras only).
    Mid,
    /// The active speaker or the stage.
    High,
}

/// One picture an app shows: a person's camera or screen at one layer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shown {
    /// The person (user root key, lowercase hex).
    pub person: String,
    pub source: PictureSource,
    pub layer: PictureLayer,
}

/// One received picture: an encoded tilo-video/1 object of one stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Picture {
    /// The person it shows (user root key, lowercase hex).
    pub person: String,
    /// The stream it belongs to: one participation's source, for as long
    /// as that participation lasts. A person on two devices, or back after
    /// leaving, is another stream; a decoder starts over with each.
    pub stream: String,
    pub source: PictureSource,
    /// The tilo-video/1 object (header and access unit), in base64 on the
    /// wire.
    #[serde(with = "base64_bytes")]
    pub object: Vec<u8>,
}

impl Picture {
    #[must_use]
    pub fn valid(&self) -> bool {
        person_valid(&self.person)
            && identifier(&self.stream)
            && !self.object.is_empty()
            && self.object.len() <= MAX_PICTURE_BYTES
    }

    /// The coded picture inside the object, as the tilo-video/1 header
    /// frames it (`tilo_moq::video`: version, flags, codec, layer, sequence,
    /// capture time, width, height, payload length, reserved; 32 bytes), or
    /// `None` for an object that does not hold together.
    #[must_use]
    pub fn unit(&self) -> Option<PictureUnit<'_>> {
        let header = self.object.get(..PICTURE_HEADER_BYTES)?;
        let payload = self.object.get(PICTURE_HEADER_BYTES..)?;
        let u16_at = |at: usize| -> Option<u16> {
            Some(u16::from_le_bytes(
                header.get(at..at.checked_add(2)?)?.try_into().ok()?,
            ))
        };
        let length = u32::from_le_bytes(header.get(24..28)?.try_into().ok()?);
        let codec = match header.get(2)? {
            1 => PictureCodec::H264,
            2 => PictureCodec::Vp8,
            3 => PictureCodec::Vp9,
            4 => PictureCodec::Av1,
            _ => return None,
        };
        (header.first() == Some(&1)
            && usize::try_from(length).ok() == Some(payload.len())
            && !payload.is_empty())
        .then_some(PictureUnit {
            keyframe: (header.get(1)? & 1) == 1,
            codec,
            width: u16_at(20)?,
            height: u16_at(22)?,
            payload,
        })
    }
}

/// Bytes of the tilo-video/1 header in front of every coded picture.
pub const PICTURE_HEADER_BYTES: usize = 32;

impl PictureUnit<'_> {
    /// This unit as a tilo-video/1 object of `layer`, the `sequence`th of its
    /// track, captured `timestamp_us` into the stream: what a call carries.
    #[must_use]
    pub fn to_object(&self, layer: PictureLayer, sequence: u64, timestamp_us: u64) -> Vec<u8> {
        let length = u32::try_from(self.payload.len()).unwrap_or(u32::MAX);
        let mut object =
            Vec::with_capacity(PICTURE_HEADER_BYTES.saturating_add(self.payload.len()));
        object.extend_from_slice(&[
            1,
            u8::from(self.keyframe),
            match self.codec {
                PictureCodec::H264 => 1,
                PictureCodec::Vp8 => 2,
                PictureCodec::Vp9 => 3,
                PictureCodec::Av1 => 4,
            },
            match layer {
                PictureLayer::Low => 0,
                PictureLayer::Mid => 1,
                PictureLayer::High => 2,
            },
        ]);
        object.extend_from_slice(&sequence.to_le_bytes());
        object.extend_from_slice(&timestamp_us.to_le_bytes());
        object.extend_from_slice(&self.width.to_le_bytes());
        object.extend_from_slice(&self.height.to_le_bytes());
        object.extend_from_slice(&length.to_le_bytes());
        object.extend_from_slice(&[0; 4]);
        object.extend_from_slice(self.payload);
        object
    }
}

/// How a picture is coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PictureCodec {
    H264,
    Vp8,
    Vp9,
    Av1,
}

/// The coded picture a [`Picture`] carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PictureUnit<'a> {
    /// A decoder can start here.
    pub keyframe: bool,
    pub codec: PictureCodec,
    pub width: u16,
    pub height: u16,
    /// The access unit.
    pub payload: &'a [u8],
}

mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

/// The longest a [`Command::Watch`] is held before it is answered unchanged.
pub const WATCH_HOLD: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Unavailable,
    Idle,
    Preparing,
    Joining,
    Live,
    Reconnecting,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Issue {
    Unavailable,
    Denied,
    PermissionDenied,
    SourceLost,
    CaptureFailed,
    TransportFailed,
    SessionEnded,
    Stale,
    /// The person did not allow the camera.
    CameraDenied,
    /// The camera could not start, or stopped.
    CameraLost,
    /// The person did not allow sharing the screen.
    ScreenDenied,
    /// Sharing the screen could not start, or stopped.
    ScreenLost,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub id: String,
    pub label: String,
}

/// Current authority's publisher streams, never an inferred participant roster.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub revision: Option<u64>,
    pub publication: Option<String>,
    pub publishers: Vec<String>,
}
impl Inventory {
    pub fn valid(&self) -> bool {
        if self
            .publishers
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left >= right))
        {
            return false;
        }
        if self.revision.is_none() && (self.publication.is_some() || !self.publishers.is_empty()) {
            return false;
        }
        let mut call = None;
        for namespace in self.publication.iter().chain(self.publishers.iter()) {
            let Some(prefix) = namespace_valid(namespace)
                .then(|| namespace.get(..64))
                .flatten()
            else {
                return false;
            };
            if call.is_some_and(|prior| prior != prefix) {
                return false;
            }
            call = Some(prefix);
        }
        self.publication
            .as_ref()
            .is_none_or(|own| !self.publishers.contains(own))
    }
}
fn namespace_valid(value: &str) -> bool {
    value.len() == 129
        && value.as_bytes().get(64) == Some(&b'/')
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 64 || b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is its own wire field of the status: muted, capturing, camera, screen"
)]
pub struct Snapshot {
    pub version: u8,
    pub state: State,
    pub muted: bool,
    pub capture_active: bool,
    pub input_id: Option<String>,
    pub inputs: Vec<Input>,
    pub issue: Option<Issue>,
    #[serde(default)]
    pub inventory_revision: Option<u64>,
    #[serde(default)]
    pub publication: Option<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    /// Who is in the call of each of this route's voice channels, before
    /// and while this device takes part: what a voice channel shows.
    #[serde(default)]
    pub rosters: Vec<Roster>,
    /// The people this device's call hears talking now (person ids, sorted
    /// and distinct), this person among them while their microphone is
    /// heard: what the speaking rings show.
    #[serde(default)]
    pub speaking: Vec<String>,
    /// This person's camera is on in the call. Like every field added
    /// after the first version, written only when set, so an app that does
    /// not know it reads a status without it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub camera: bool,
    /// This person shares a screen in the call.
    #[serde(default, skip_serializing_if = "is_false")]
    pub screen: bool,
    /// Advances whenever anything above changes; what a
    /// [`Command::Watch`] waits past.
    #[serde(default)]
    pub revision: u64,
    /// The pictures a [`Command::Pictures`] answer carries, oldest first;
    /// empty in every other answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pictures: Vec<Picture>,
    /// Where the next [`Command::Pictures`] continues.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pictures_cursor: u64,
}

/// The people in one voice channel's call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    pub channel_id: String,
    /// Person ids (a user root key in lowercase hex), sorted and distinct:
    /// a person on two devices is in the call once.
    pub people: Vec<String>,
    /// Those of `people` whose camera is on, sorted and distinct.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<String>,
    /// Those of `people` who share a screen, sorted and distinct.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<String>,
}

impl Roster {
    #[must_use]
    pub fn valid(&self) -> bool {
        let among = |subset: &[String]| {
            people_valid(subset)
                && subset
                    .iter()
                    .all(|person| self.people.binary_search(person).is_ok())
        };
        identifier(&self.channel_id)
            && people_valid(&self.people)
            && among(&self.cameras)
            && among(&self.screens)
    }
}

/// A person id: a user root key in lowercase hex.
fn person_valid(person: &str) -> bool {
    person.len() == 64
        && person
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Person ids: a user root key in lowercase hex each, sorted and distinct.
fn people_valid(people: &[String]) -> bool {
    people.iter().all(|person| person_valid(person))
        && people
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left < right))
}

impl Request {
    #[must_use]
    pub const fn new(command: Command) -> Self {
        Self {
            version: 1,
            intent: 0,
            command,
        }
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.version == 1
            && match &self.command {
                Command::Join { channel_id } => identifier(channel_id),
                Command::SelectInput { input_id } => identifier(input_id),
                Command::Show { pictures } => {
                    pictures.len() <= MAX_SHOWN
                        && pictures.iter().all(|shown| {
                            person_valid(&shown.person)
                                && (shown.source == PictureSource::Camera
                                    || shown.layer != PictureLayer::Mid)
                        })
                        && pictures.windows(2).all(|pair| {
                            matches!(pair, [left, right]
                                if (&left.person, left.source) < (&right.person, right.source))
                        })
                }
                Command::SetMuted { .. }
                | Command::SetCamera { .. }
                | Command::SetScreen { .. }
                | Command::Leave
                | Command::Status
                | Command::Watch { .. }
                | Command::Pictures { .. } => true,
            }
    }

    /// Whether this command carries a route intent: everything that changes
    /// the call. A status, a watch and collecting pictures only read, and
    /// saying which pictures are shown changes nothing of the call itself.
    #[must_use]
    pub const fn carries_intent(&self) -> bool {
        !matches!(
            self.command,
            Command::Status
                | Command::Watch { .. }
                | Command::Pictures { .. }
                | Command::Show { .. }
        )
    }

    pub fn dispatch_valid(&self) -> bool {
        self.valid()
            && match self.command {
                Command::Status
                | Command::Watch { .. }
                | Command::Pictures { .. }
                | Command::Show { .. } => self.intent == 0,
                Command::Leave => self.intent > 0,
                _ => self.intent > 0 && self.intent < u64::MAX,
            }
    }

    /// Encode a valid command for the wire.
    ///
    /// # Errors
    ///
    /// Refuses an invalid command or an encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        encode(self, self.valid())
    }

    /// Decode one bounded, valid command.
    ///
    /// # Errors
    ///
    /// Refuses malformed or invalid input.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let value: Self = decode(bytes)?;
        value
            .valid()
            .then_some(value)
            .ok_or("invalid local media command")
    }
}

impl Snapshot {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            version: 1,
            state: State::Unavailable,
            muted: true,
            capture_active: false,
            input_id: None,
            inputs: Vec::new(),
            issue: Some(Issue::Unavailable),
            inventory_revision: None,
            publication: None,
            publishers: Vec::new(),
            rosters: Vec::new(),
            speaking: Vec::new(),
            camera: false,
            screen: false,
            revision: 0,
            pictures: Vec::new(),
            pictures_cursor: 0,
        }
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        let mut ids = std::collections::BTreeSet::new();
        let inventory = Inventory {
            revision: self.inventory_revision,
            publication: self.publication.clone(),
            publishers: self.publishers.clone(),
        };
        self.version == 1
            && inventory.valid()
            && (self.state == State::Live
                || (self.inventory_revision.is_none()
                    && self.publication.is_none()
                    && self.publishers.is_empty()))
            && (!self.capture_active
                || (!self.muted && self.state == State::Live && self.input_id.is_some()))
            && self.input_id.as_deref().is_none_or(identifier)
            && self.rosters.iter().all(Roster::valid)
            && people_valid(&self.speaking)
            // Nothing shows outside a call.
            && (!matches!(self.state, State::Unavailable | State::Idle | State::Ended)
                || (!self.camera && !self.screen))
            && self.pictures.iter().all(Picture::valid)
            && self
                .rosters
                .windows(2)
                .all(|pair| matches!(pair, [left, right] if left.channel_id < right.channel_id))
            && self.inputs.len() <= MAX_INPUTS
            && self.inputs.iter().all(|input| {
                identifier(&input.id) && identifier(&input.label) && ids.insert(&input.id)
            })
    }

    /// Encode a valid status for the wire.
    ///
    /// # Errors
    ///
    /// Refuses an invalid status or an encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        encode(self, self.valid())
    }

    /// Decode one bounded, valid status.
    ///
    /// # Errors
    ///
    /// Refuses malformed or invalid input.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let value: Self = decode(bytes)?;
        value
            .valid()
            .then_some(value)
            .ok_or("invalid local media status")
    }
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn encode(value: &impl Serialize, valid: bool) -> Result<Vec<u8>, &'static str> {
    if !valid {
        return Err("invalid local media value");
    }
    let bytes = serde_json::to_vec(value).map_err(|_| "invalid local media value")?;
    (bytes.len() <= MAX_WIRE_BYTES)
        .then_some(bytes)
        .ok_or("local media value exceeds bound")
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, &'static str> {
    if bytes.len() > MAX_WIRE_BYTES {
        return Err("local media value exceeds bound");
    }
    serde_json::from_slice(bytes).map_err(|_| "invalid local media value")
}

#[cfg(test)]
mod tests;
