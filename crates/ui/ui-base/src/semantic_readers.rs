//! What an app registers to answer semantic reads of its document: a reader's closed request
//! decoder, exact result schema and projection, and the widgets that carry them. The SDK
//! selects and runs a reader ([`crate::semantic_document_read`]).

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::ResourceSpec;

pub const MAX_REQUEST_BYTES: usize = 8 * 1024;
/// The longest opaque cursor a paged read hands out or accepts.
pub const MAX_CURSOR_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticReadError {
    InvalidRegistration,
    InvalidContract,
    InvalidRequest,
    Limit,
    InvalidOwner,
    MissingQualifiedOwner,
    Pending,
    StaleCursor,
    Malformed,
}

impl SemanticReadError {
    /// A stable code and a message written for the calling agent.
    pub fn describe(self) -> (&'static str, &'static str) {
        match self {
            Self::InvalidRegistration | Self::InvalidContract => (
                "read_contract_invalid",
                "The reader's declaration does not match its registration",
            ),
            Self::InvalidRequest => ("invalid_arguments", "The read request is invalid"),
            Self::Limit => (
                "read_limit",
                "The read exceeds its limits; ask for a smaller page",
            ),
            Self::InvalidOwner | Self::MissingQualifiedOwner => (
                "read_owner_unavailable",
                "The document is not the caller's current canonical record",
            ),
            Self::Pending => (
                "document_edit_pending",
                "The document has an uncommitted edit; retry the read",
            ),
            Self::StaleCursor => (
                "stale_cursor",
                "The document changed since this cursor; read again from the start",
            ),
            Self::Malformed => ("document_malformed", "The document cannot be read"),
        }
    }
}

impl From<tilo_doc::semantic_read::ReadError> for SemanticReadError {
    fn from(error: tilo_doc::semantic_read::ReadError) -> Self {
        use tilo_doc::semantic_read::ReadError;
        match error {
            ReadError::Pending => Self::Pending,
            ReadError::StaleCursor => Self::StaleCursor,
            ReadError::InvalidCursor => Self::InvalidRequest,
            ReadError::Limit => Self::Limit,
            ReadError::Malformed => Self::Malformed,
        }
    }
}

/// The exact result schema of a read whose projection yields `content`: the SDK stamps
/// `version`, `record` and `basis` around it.
#[must_use]
pub fn result_schema(content: serde_json::Value, paged: bool) -> serde_json::Value {
    let next_cursor = if paged {
        serde_json::json!({"type":["string","null"],"maxLength":MAX_CURSOR_BYTES})
    } else {
        serde_json::json!({"type":"null"})
    };
    serde_json::json!({"type":"object","required":["version","record","basis","content","nextCursor"],"additionalProperties":false,"properties":{
        "version":{"type":"integer","enum":[1]},
        "record":{"type":"object","required":["workspaceId","appId","recordId"],"additionalProperties":false,"properties":{
            "workspaceId":{"type":["string","null"],"format":"uuid"},"appId":{"type":"string","minLength":1,"maxLength":256},"recordId":{"type":"string","maxLength":512}
        }},
        "basis":{"type":"object","required":["heads"],"additionalProperties":false,"properties":{
            "heads":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[0-9a-f]{64}$"}}
        }},
        "content":content,
        "nextCursor":next_cursor
    }})
}

/// A reader's closed request, decoded from its payload.
pub fn decode_request<T: serde::de::DeserializeOwned>(
    payload: &[u8],
) -> Result<T, SemanticReadError> {
    serde_json::from_slice(payload).map_err(|_| SemanticReadError::InvalidRequest)
}

/// The cursor an earlier page handed out, decoded; `None` reads from the start.
pub fn decode_cursor<C: serde::de::DeserializeOwned>(
    cursor: Option<&str>,
) -> Result<Option<C>, SemanticReadError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.len() > MAX_CURSOR_BYTES {
        return Err(SemanticReadError::Limit);
    }
    serde_json::from_str(cursor)
        .map(Some)
        .map_err(|_| SemanticReadError::InvalidRequest)
}

/// The opaque cursor for the page after this one, when there is one.
pub fn encode_cursor<C: serde::Serialize>(
    next: Option<&C>,
) -> Result<Option<String>, SemanticReadError> {
    next.map(|next| {
        serde_json::to_string(next)
            .ok()
            .filter(|cursor| cursor.len() <= MAX_CURSOR_BYTES)
            .ok_or(SemanticReadError::Limit)
    })
    .transpose()
}

/// What a registered read's published action is called, its parameters and
/// the action [`read_action`] publishes: the declaration is the read, made
/// in `tilo_resource` so a surface can only publish declared reads.
pub use tilo_resource::read::{Declared, NoReadParams, ReadName, read_action};

/// One page an app's projection produced at the heads it was given.
pub struct ReadPage {
    pub content: serde_json::Value,
    pub next_cursor: Option<String>,
}

/// An app projection over `document` at exactly `heads`, for a request the registration's
/// decoder accepted.
pub type ReadProjection =
    fn(&AutoCommit, &[ChangeHash], &[u8]) -> Result<ReadPage, SemanticReadError>;

/// App-owned code paired with a closed request decoder and exact result schema. This
/// public registration cannot create a frame, response stamp, or permission.
pub struct RegisteredSemanticReader {
    pub operation_type: &'static str,
    pub schema: fn() -> &'static serde_json::Value,
    pub validate: fn(&ResourceSpec, &[u8]) -> Result<(), SemanticReadError>,
    pub project: ReadProjection,
}

impl RegisteredSemanticReader {
    pub(crate) fn operation_type(&self) -> &'static str {
        self.operation_type
    }

    pub const fn new(
        operation_type: &'static str,
        schema: fn() -> &'static serde_json::Value,
        validate: fn(&ResourceSpec, &[u8]) -> Result<(), SemanticReadError>,
        project: ReadProjection,
    ) -> Self {
        Self {
            operation_type,
            schema,
            validate,
            project,
        }
    }
}

/// A widget whose document answers semantic reads through registered readers.
pub trait SemanticReaders {
    fn semantic_readers(&self) -> &'static [RegisteredSemanticReader];
}

/// The readers `widget` registers itself, not those of its children.
pub(crate) fn readers_of(
    widget: &(impl crate::widget::Widget + ?Sized),
) -> &'static [RegisteredSemanticReader] {
    crate::widget::capability::capability::<dyn SemanticReaders>(widget)
        .map_or(&[], SemanticReaders::semantic_readers)
}

/// Whether any widget in `widget`'s retained tree registers a reader: a tree
/// that registers none offers no registered read, whatever it publishes.
pub fn registers_readers(widget: &dyn crate::widget::Widget) -> bool {
    if !readers_of(widget).is_empty() {
        return true;
    }
    let mut found = false;
    widget.for_each_child(&mut |child| {
        found = found || registers_readers(child);
    });
    found
}

/// The reader registered anywhere in `widget`'s retained tree for `type_id`.
pub fn registered_in_tree(
    widget: &dyn crate::widget::Widget,
    type_id: &str,
) -> Option<&'static [RegisteredSemanticReader]> {
    let readers = readers_of(widget);
    if readers
        .iter()
        .any(|reader| reader.operation_type == type_id)
    {
        return Some(readers);
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = registered_in_tree(child, type_id);
        }
    });
    found
}
