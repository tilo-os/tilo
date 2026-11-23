//! What the supervisor and an app worker say to each other.
//!
//! Each message is one postcard frame behind a little-endian length prefix
//! (`wire-frame`). Values that carry JSON (`serde_json::Value` in arguments,
//! outputs and schemas) travel as JSON text inside the frame, since postcard
//! cannot decode a self-describing value.
//!
//! Nothing here can carry a key: no variant holds a signing key, a
//! certificate's secret half or a session. The worker receives app ids,
//! record ids, the member's public name, Automerge bytes and the read
//! policy a caller already holds, and returns descriptors, outputs and
//! unsigned Automerge bytes the supervisor admits before it signs them.
//!
//! Content a record references (a picture's bytes, named by hash) crosses
//! one item at a time each way: the supervisor puts what an operation names
//! and what the record refers to, and takes what the app stored and the
//! hashes it asked for and lacked. What comes back is hashed again before
//! the workspace keeps it.

use serde::{Deserialize, Serialize};
use tilo_actions::api::ApiError;
use wire_frame::Framing;

/// Frames in either direction: a record's whole document, with workspace.
pub(crate) const FRAMING: Framing = Framing::little_endian(64 * 1024 * 1024);

/// Most bytes of one content item that cross in a frame, leaving the
/// frame's encoding workspace.
pub(crate) const MAX_CONTENT_ITEM_BYTES: usize = FRAMING.max_len().saturating_sub(64 * 1024);

/// Most bytes of content one [`Reply::Content`] carries; the rest waits for
/// the next take.
pub(crate) const CONTENT_BATCH_BYTES: usize = 32 * 1024 * 1024;

/// Bumped whenever a message changes shape. A supervisor and a worker are the
/// same executable, so a mismatch means a stale binary on disk.
pub(crate) const PROTOCOL_VERSION: u32 = 16;

/// What a worker is started to do, and what each of its slots does. The
/// process's mode decides what it prepares before it confines itself (a
/// reader the fonts a component lays its text out in, which it can no longer
/// open afterwards); a slot's mode, sent with every request, what the slot
/// answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// Which of a worker's slots a request is for. A worker serves many
/// records, views and reads at once, each in a slot of its own: what one
/// slot holds is never handed to a request for another.
pub(crate) type Slot = u64;

/// The supervisor's requests, each sent as `(Slot, Mode, Region, Request)`: the
/// slot it is for, what that slot does, and the region its views
/// renders ([`super::region`]). Each reply comes back as
/// `(Slot, Reply)`, a slot's in the order it was asked; [`Request::Close`] is
/// not answered.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Request {
    /// Let go of everything the slot holds.
    Close,
    /// Hold a record ([`Mode::Replica`]): a new one when `seed` is `None`,
    /// a new one under the record named when it has no changes, otherwise
    /// the record named with its durable changes in order.
    Open {
        app_id: String,
        workspace: String,
        member: tilo_actions::MemberId,
        /// JSON `ReplicaActor`: a bot's access nests maps postcard
        /// cannot describe.
        actor: String,
        seed: Option<(String, Vec<Vec<u8>>)>,
        /// The held document's store asks its workspace for the documents the view opens and
        /// does not hold ([`crate::Replica::ask_workspace_for_missing`]).
        asks_workspace: bool,
    },
    /// Every operation the held record publishes now.
    Survey,
    /// Every operation the held record publishes now, each in the row of the
    /// resource holding it: what a caller listing one resource of the record
    /// reads its children by.
    SurveyRows,
    /// One page of what the held record publishes, as a catalogue query
    /// lists it: the operations the JSON `CatalogueQuery` admits, from the
    /// `offset`th on, at most `take` of them, with how many it admits in
    /// all ([`Reply::Page`]). Operations this host cannot render are left
    /// out first (`can_render`). A page costs its rows, never the
    /// encoding of every operation the record holds.
    SurveyPage {
        query: String,
        can_render: bool,
        offset: u64,
        take: u64,
    },
    /// The one operation `address` names, or none: an answer to a host that
    /// already knows what it runs, without listing the record's resources.
    Resolve {
        /// JSON `ActionAddress`.
        address: String,
    },
    /// Perform one published operation.
    Invoke {
        /// JSON `ActionAddress`.
        address: String,
        /// JSON object of arguments.
        arguments: String,
        confirm: bool,
        /// JSON `ActionInvoker` admitted for this invocation, when not the
        /// member itself.
        invoker: Option<String>,
        /// JSON `ActionContext` of this invocation.
        context: Option<String>,
        /// JSON `Appearance` the API admitted, if any.
        appearance: Option<String>,
        /// The workspace's members, each by what it calls them
        /// ([`ui::tilo_resource::members`]).
        members: Vec<(tilo_actions::MemberId, String)>,
        /// The value of each sealed content reference the arguments name that
        /// the workspace keeps already: the reference and the stored form the
        /// document keeps it by. The operation takes it by reference, and none
        /// of its bytes crosses for it.
        given: Vec<(String, String)>,
    },
    /// The holder's own changes since the last call.
    TakeMutation,
    /// Merge another member's durable change.
    Apply { snapshot: bool, bytes: Vec<u8> },
    /// Merge another member's durable change into `record`, one the
    /// held record's app keeps beside it (a part of its chain).
    ApplyDocument {
        record: String,
        snapshot: bool,
        bytes: Vec<u8>,
    },
    /// The holder's own changes to the records held beside the held one
    /// since the last call.
    TakeDocumentMutations,
    /// Where the last operation left the member.
    Presence,
    /// Take the held record's store to come from its workspace's hosts
    /// ([`crate::Replica::ask_workspace_for_missing`]).
    AskWorkspace,
    /// What the held record's store asks its workspace's hosts for.
    Wanted,
    /// The workspace's hosts answered `record`, which it asked for: they hold none of it, or
    /// it is another person's own.
    Answered {
        record: String,
        answer: ui::resource::WorkspaceAnswer,
    },
    /// What an existing document of `app_id` is called ([`Mode::Views`]):
    /// [`crate::AppCatalogue::resource_title`]. The document is read for the
    /// answer and dropped; the worker answers any number of these.
    Title { app_id: String, doc: Vec<u8> },
    /// What search finds in an existing document of `app_id`
    /// ([`Mode::Views`]): the entries its app publishes to the search index,
    /// each a resource of the record by what it says (a note by its title
    /// and its words), answered with [`Reply::Entries`]. Read and dropped as
    /// [`Self::Title`] is.
    Entries { app_id: String, doc: Vec<u8> },
    /// Mount a record's component over its retained source ([`Mode::Read`]).
    ReadPrepare {
        app_id: String,
        workspace: String,
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
    /// Whether the mounted component registers any reader at all, answered
    /// [`Reply::Registers`].
    ReadRegisters,
    /// Perform one registered read.
    ReadInvoke {
        /// JSON `ActionAddress`.
        address: String,
        /// JSON object of arguments.
        arguments: String,
    },
    /// Report what the confinement still lets the worker do at `path`.
    Probe { path: String },
    /// Keep one content item where the app's code reads it, as `reference`
    /// names it (sealed under its key when it carries one); answered with
    /// [`Reply::Stored`].
    PutContent { reference: String, bytes: Vec<u8> },
    /// What the app stored since the last take and what it asked for but
    /// lacked; answered with [`Reply::Content`].
    TakeContent,
    /// Hold the slot's thread for `millis`, then answer [`Reply::Done`]: a
    /// request that takes long, for the tests of how one is borne.
    #[cfg(test)]
    Stall { millis: u64 },
}

impl Request {
    /// Whether the request leaves what the held document offers as it was: it reads or
    /// collects, and neither the document, nor what its holder may do, nor the members it
    /// knows move by it. An action found by its address before such a request is the same
    /// action after it.
    pub(crate) const fn moves_nothing(&self) -> bool {
        matches!(
            self,
            Self::TakeMutation | Self::TakeDocumentMutations | Self::Presence | Self::TakeContent
        )
    }

    /// What the request asks, as a log line names it.
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Open { .. } => "open",
            Self::Survey => "survey",
            Self::SurveyRows => "survey-rows",
            Self::SurveyPage { .. } => "survey-page",
            Self::Resolve { .. } => "resolve",
            Self::Invoke { .. } => "invoke",
            Self::TakeMutation => "take-mutation",
            Self::Apply { .. } => "apply",
            Self::ApplyDocument { .. } => "apply-document",
            Self::TakeDocumentMutations => "take-document-mutations",
            Self::Presence => "presence",
            Self::AskWorkspace => "ask-workspace",
            Self::Wanted => "wanted",
            Self::Answered { .. } => "answered",
            Self::Title { .. } => "title",
            Self::Entries { .. } => "entries",
            Self::ReadPrepare { .. } => "read-prepare",
            Self::ReadPolicy { .. } => "read-policy",
            Self::ReadOperations => "read-operations",
            Self::ReadRegisters => "read-registers",
            Self::ReadInvoke { .. } => "read-invoke",
            Self::Probe { .. } => "probe",
            Self::PutContent { .. } => "put-content",
            Self::TakeContent => "take-content",
            #[cfg(test)]
            Self::Stall { .. } => "stall",
        }
    }
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
    /// One page of a survey: how many operations the query admits in all,
    /// and the JSON `Vec<CatalogueRow>` of those on the page, each in the
    /// row of the resource holding it; `arriving` when the page reads parts of a chain still
    /// on their way to the worker, and is not what it will be once they are in.
    Page {
        total: u64,
        rows: String,
        arriving: bool,
    },
    /// JSON `ActionOutput`.
    Output(String),
    /// The resource's API refused or the action failed.
    Refused(WireApiError),
    /// The holder's changes: whether they are a whole document, and the bytes.
    Mutation(Option<(bool, Vec<u8>)>),
    /// The holder's changes to records held beside the held one: each one's
    /// id, the changes it made, and, for a part it started, the part whole.
    DocumentMutations(Vec<TakenDocument>),
    /// JSON `(LiveAnchor, LiveAnchor)`.
    Presence(Option<String>),
    /// Record ids the held record's store asks its workspace's hosts for.
    Wanted(Vec<String>),
    /// What a document is called, when its app names it.
    Title(Option<String>),
    /// JSON `Vec<IndexEntry>`: what search finds in a document. Not
    /// trusted: a host bounds each entry before it ranks it.
    Entries(String),
    /// Whether the mounted component registers any reader.
    Registers(bool),
    /// The request was carried out.
    Done,
    /// The request could not be carried out.
    Failed(String),
    /// What a probe could do.
    Probe(ProbeReport),
    /// The content was kept under this hash.
    Stored(String),
    /// Content the app stored since the last take, at most
    /// [`CONTENT_BATCH_BYTES`] of it unless one item is larger, and the
    /// hashes it asked for that the worker lacks. Neither is trusted.
    Content {
        offered: Vec<Vec<u8>>,
        wanted: Vec<String>,
    },
}

/// One record held beside the held one that changed, as the worker hands it
/// over.
#[derive(Debug, Serialize, Deserialize)]
pub struct TakenDocument {
    /// The record that changed.
    pub record: String,
    /// This member's changes since the last take.
    pub changes: Vec<u8>,
    /// The record whole, for a part the worker started.
    pub whole: Option<Vec<u8>>,
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
    pub created_file: bool,
    /// An internet socket could be bound or connected.
    pub inet_socket: bool,
    /// Another program could be started.
    pub exec: bool,
}

/// [`ApiError`] as it crosses the boundary.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum WireApiError {
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
    Refused {
        code: String,
        reason: String,
    },
}

impl WireApiError {
    pub(crate) fn from_api(error: &ApiError) -> Self {
        let address = |address| serde_json::to_string(address).unwrap_or_default();
        match error {
            ApiError::Unknown(at) => Self::Unknown(address(at)),
            ApiError::MissingArgument(at) => Self::MissingArgument(address(at)),
            ApiError::UnexpectedArgument(at) => Self::UnexpectedArgument(address(at)),
            ApiError::InvalidArguments(reason) => Self::InvalidArguments(reason.clone()),
            ApiError::ArgumentOutOfRange { param, violation } => Self::ArgumentOutOfRange {
                param: param.clone(),
                violation: violation.clone(),
            },
            ApiError::Unavailable { code, reason } => Self::Unavailable {
                code: code.clone(),
                reason: reason.clone(),
            },
            ApiError::Refused { code, reason } => Self::Refused {
                code: code.clone(),
                reason: reason.clone(),
            },
            ApiError::ConfirmationRequired => Self::ConfirmationRequired,
            ApiError::UnregisteredType(kind) => Self::UnregisteredType(kind.clone()),
            ApiError::Failed(message) => Self::Failed(message.clone()),
        }
    }

    /// The API's error, addressed at `fallback` where the worker's address
    /// does not decode.
    pub(crate) fn into_api(self, fallback: &tilo_actions::api::ActionAddress) -> ApiError {
        let address = |text: &str| serde_json::from_str(text).unwrap_or_else(|_| fallback.clone());
        match self {
            Self::Unknown(at) => ApiError::Unknown(address(&at)),
            Self::MissingArgument(at) => ApiError::MissingArgument(address(&at)),
            Self::UnexpectedArgument(at) => ApiError::UnexpectedArgument(address(&at)),
            Self::InvalidArguments(reason) => ApiError::InvalidArguments(reason),
            Self::ArgumentOutOfRange { param, violation } => {
                ApiError::ArgumentOutOfRange { param, violation }
            }
            Self::Unavailable { code, reason } => ApiError::Unavailable { code, reason },
            Self::Refused { code, reason } => ApiError::Refused { code, reason },
            Self::ConfirmationRequired => ApiError::ConfirmationRequired,
            Self::UnregisteredType(kind) => ApiError::UnregisteredType(kind),
            Self::Failed(message) => ApiError::Failed(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_survive_the_boundary() {
        let at = tilo_actions::api::ActionAddress {
            resource: "r".into(),
            action: "a".into(),
        };
        for error in [
            ApiError::Unknown(at.clone()),
            ApiError::MissingArgument(at.clone()),
            ApiError::UnexpectedArgument(at.clone()),
            ApiError::InvalidArguments("x".into()),
            ApiError::Unavailable {
                code: "c".into(),
                reason: "r".into(),
            },
            ApiError::ConfirmationRequired,
            ApiError::UnregisteredType("t".into()),
            ApiError::Failed("f".into()),
        ] {
            let wire: WireApiError = postcard::from_bytes(
                &postcard::to_stdvec(&WireApiError::from_api(&error)).unwrap(),
            )
            .unwrap();
            assert_eq!(wire.into_api(&at), error);
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
