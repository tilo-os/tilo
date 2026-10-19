//! Closed local call controls. Identity and capture authority never come from this wire.
use serde::{Deserialize, Serialize};

/// One message's bound. A call has no participant limit; a status names
/// each participant's stream (129 bytes) and person (64), so this carries
/// well over a thousand of them.
pub const MAX_WIRE_BYTES: usize = 256 * 1024;
pub const MAX_INPUTS: usize = 128;

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
    Join { channel_id: String },
    SetMuted { muted: bool },
    SelectInput { input_id: String },
    Leave,
    Status,
}

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
}

/// The people in one voice channel's call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    pub channel_id: String,
    /// Person ids (a user root key in lowercase hex), sorted and distinct:
    /// a person on two devices is in the call once.
    pub people: Vec<String>,
}

impl Roster {
    #[must_use]
    pub fn valid(&self) -> bool {
        identifier(&self.channel_id)
            && self.people.iter().all(|person| {
                person.len() == 64
                    && person
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            && self
                .people
                .windows(2)
                .all(|pair| matches!(pair, [left, right] if left < right))
    }
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
                Command::SetMuted { .. } | Command::Leave | Command::Status => true,
            }
    }

    pub fn dispatch_valid(&self) -> bool {
        self.valid()
            && match self.command {
                Command::Status => self.intent == 0,
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
