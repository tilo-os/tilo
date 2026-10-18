//! What the supervisor and an app worker say to each other.
//!
//! Each message is one postcard frame behind a little-endian length prefix
//! (`wire-frame`). Values that carry JSON (`serde_json::Value` in arguments,
//! outputs and schemas) travel as JSON text inside the frame, since postcard
//! cannot decode a self-describing value.
//!
//! Nothing here can carry a key: no variant holds a signing key, a
//! certificate's secret half or a session. The worker receives app ids,
//! record ids, the participant's public name, Automerge bytes and the read
//! policy a caller already holds, and returns descriptors, outputs and
//! unsigned Automerge bytes the supervisor admits before it signs them.

use serde::{Deserialize, Serialize};
use tilo_actions::door::DoorError;
use wire_frame::Framing;

/// Frames in either direction: a record's whole document, with room.
pub(crate) const FRAMING: Framing = Framing::little_endian(64 * 1024 * 1024);

/// Bumped whenever a message changes shape. A supervisor and a worker are the
/// same executable, so a mismatch means a stale binary on disk.
pub(crate) const PROTOCOL_VERSION: u32 = 3;

/// What a worker is started to do. It decides what the worker prepares
/// before it confines itself: a reader loads the fonts a component lays its
/// text out in, which it can no longer open afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Hold one record as a replica of its resource ([`crate::Replica`]).
    Replica,
    /// Discover the views one existing resource offers.
    Views,
    /// Mount one record's component to answer its registered reads.
    Read,
}

impl Mode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Replica => "replica",
            Self::Views => "views",
            Self::Read => "read",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "replica" => Some(Self::Replica),
            "views" => Some(Self::Views),
            "read" => Some(Self::Read),
            _ => None,
        }
    }
}

/// The supervisor's requests.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Request {
    /// Hold a record ([`Mode::Replica`]): a new one when `seed` is `None`,
    /// a new one under the record named when it has no changes, otherwise
    /// the record named with its durable changes in order.
    Open {
        app_id: String,
        room: String,
        participant: String,
        /// JSON `ReplicaActor`: a bot's standing nests maps postcard
        /// cannot describe.
        actor: String,
        seed: Option<(String, Vec<Vec<u8>>)>,
    },
    /// Every operation the held record publishes now.
    Survey,
    /// Perform one published operation.
    Invoke {
        /// JSON `ActionAddress`.
        address: String,
        /// JSON object of arguments.
        arguments: String,
        confirm: bool,
        /// JSON `ActionInvoker` admitted for this invocation, when not the
        /// participant itself.
        invoker: Option<String>,
        /// JSON `ActionContext` of this invocation.
        context: Option<String>,
        /// JSON `Appearance` the door admitted, if any.
        appearance: Option<String>,
    },
    /// The holder's own changes since the last call.
    TakeMutation,
    /// Merge another participant's durable change.
    Apply { snapshot: bool, bytes: Vec<u8> },
    /// Merge another participant's durable change into `record`, one the
    /// held record's app keeps beside it (a part of its chain).
    ApplyRecord {
        record: String,
        snapshot: bool,
        bytes: Vec<u8>,
    },
    /// The holder's own changes to the records held beside the held one
    /// since the last call.
    TakeRecordMutations,
    /// Where the last operation left the participant.
    Presence,
    /// Load an existing document of `app_id` to discover its views
    /// ([`Mode::Views`]).
    ViewsLoad { app_id: String, doc: Vec<u8> },
    /// The views of one resource of the loaded document under `policy`.
    Views {
        /// JSON `ViewResourceQuery`.
        wanted: String,
        /// JSON `ViewReadPolicy`.
        policy: String,
    },
    /// Mount a record's component over its retained source ([`Mode::Read`]).
    ReadPrepare {
        app_id: String,
        room: String,
        record: String,
        source: Vec<u8>,
        /// JSON `ViewReadPolicy`.
        policy: String,
    },
    /// Replace the mounted component's permissions with a fresh policy.
    ReadPolicy {
        /// JSON `ViewReadPolicy`.
        policy: String,
    },
    /// The registered reads the mounted component offers.
    ReadOperations,
    /// Perform one registered read.
    ReadInvoke {
        /// JSON `ActionAddress`.
        address: String,
        /// JSON object of arguments.
        arguments: String,
    },
    /// Report what the confinement still lets the worker do at `path`.
    Probe { path: String },
}

/// A worker's replies.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    /// Sent once, after the confinement is in force.
    Ready { version: u32 },
    /// The worker could not confine itself and serves nothing.
    Unconfined(String),
    /// The record is held; its id.
    Opened { record: String },
    /// JSON `Vec<ActionDescriptor>`.
    Descriptors(String),
    /// JSON `ActionOutput`.
    Output(String),
    /// The resource's door refused or the action failed.
    Refused(WireDoorError),
    /// The holder's changes: whether they are a whole document, and the bytes.
    Mutation(Option<(bool, Vec<u8>)>),
    /// The holder's changes to records held beside the held one: each
    /// record's id, whether it is a whole document, and the bytes.
    RecordMutations(Vec<(String, bool, Vec<u8>)>),
    /// JSON `(LiveAnchor, LiveAnchor)`.
    Presence(Option<String>),
    /// The request was carried out.
    Done,
    /// The request could not be carried out.
    Failed(String),
    /// What a probe could do.
    Probe(ProbeReport),
}

/// What the confinement let a probe do; every field must come back `false`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent verdict per probe"
)]
pub struct ProbeReport {
    /// The file at the probed path could be opened for reading.
    pub opened_file: bool,
    /// A file could be created next to it.
    pub created_file: bool,
    /// An internet socket could be bound or connected.
    pub inet_socket: bool,
    /// Another program could be started.
    pub exec: bool,
}

/// [`DoorError`] as it crosses the boundary.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WireDoorError {
    Unknown(String),
    MissingArgument(String),
    UnexpectedArgument(String),
    InvalidArguments(String),
    ArgumentOutOfRange {
        param: String,
        violation: tilo_actions::ArgViolation,
    },
    Unavailable {
        code: String,
        reason: String,
    },
    ConfirmationRequired,
    UnregisteredType(String),
    Failed(String),
}

impl WireDoorError {
    pub(crate) fn from_door(error: &DoorError) -> Self {
        let address = |address| serde_json::to_string(address).unwrap_or_default();
        match error {
            DoorError::Unknown(at) => Self::Unknown(address(at)),
            DoorError::MissingArgument(at) => Self::MissingArgument(address(at)),
            DoorError::UnexpectedArgument(at) => Self::UnexpectedArgument(address(at)),
            DoorError::InvalidArguments(reason) => Self::InvalidArguments(reason.clone()),
            DoorError::ArgumentOutOfRange { param, violation } => Self::ArgumentOutOfRange {
                param: param.clone(),
                violation: violation.clone(),
            },
            DoorError::Unavailable { code, reason } => Self::Unavailable {
                code: code.clone(),
                reason: reason.clone(),
            },
            DoorError::ConfirmationRequired => Self::ConfirmationRequired,
            DoorError::UnregisteredType(kind) => Self::UnregisteredType(kind.clone()),
            DoorError::Failed(message) => Self::Failed(message.clone()),
        }
    }

    /// The door's error, addressed at `fallback` where the worker's address
    /// does not decode.
    pub(crate) fn into_door(self, fallback: &tilo_actions::door::ActionAddress) -> DoorError {
        let address = |text: &str| serde_json::from_str(text).unwrap_or_else(|_| fallback.clone());
        match self {
            Self::Unknown(at) => DoorError::Unknown(address(&at)),
            Self::MissingArgument(at) => DoorError::MissingArgument(address(&at)),
            Self::UnexpectedArgument(at) => DoorError::UnexpectedArgument(address(&at)),
            Self::InvalidArguments(reason) => DoorError::InvalidArguments(reason),
            Self::ArgumentOutOfRange { param, violation } => {
                DoorError::ArgumentOutOfRange { param, violation }
            }
            Self::Unavailable { code, reason } => DoorError::Unavailable { code, reason },
            Self::ConfirmationRequired => DoorError::ConfirmationRequired,
            Self::UnregisteredType(kind) => DoorError::UnregisteredType(kind),
            Self::Failed(message) => DoorError::Failed(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn door_errors_survive_the_boundary() {
        let at = tilo_actions::door::ActionAddress {
            resource: "r".into(),
            action: "a".into(),
        };
        for error in [
            DoorError::Unknown(at.clone()),
            DoorError::MissingArgument(at.clone()),
            DoorError::UnexpectedArgument(at.clone()),
            DoorError::InvalidArguments("x".into()),
            DoorError::Unavailable {
                code: "c".into(),
                reason: "r".into(),
            },
            DoorError::ConfirmationRequired,
            DoorError::UnregisteredType("t".into()),
            DoorError::Failed("f".into()),
        ] {
            let wire: WireDoorError = postcard::from_bytes(
                &postcard::to_stdvec(&WireDoorError::from_door(&error)).unwrap(),
            )
            .unwrap();
            assert_eq!(wire.into_door(&at), error);
        }
    }

    #[test]
    fn modes_round_trip_by_name() {
        for mode in [Mode::Replica, Mode::Views, Mode::Read] {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(Mode::parse("other"), None);
    }
}
