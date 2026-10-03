//! App reader registration and the SDK's committed-read admission.
//!
//! An app registers a reader: a closed request decoder, an exact result schema
//! and a projection over its document at given heads. Registration confers no
//! authority. The SDK alone runs a selected reader, and only over a document it
//! can identify as the caller's canonical record: the record store's current
//! loan of that exact document for a hosted invocation (whose route the scope
//! verified against the owner), or the app's mounted primary document on its
//! current primary route. The read binds to the document's committed heads,
//! never commits a pending edit, and rechecks the original caller immediately
//! before disclosure.
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};
use tilo_actions::{ActionSpec, EncodedAction, Resource};

pub mod retirement;

pub const MAX_REQUEST_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticReadError {
    InvalidRegistration,
    InvalidContract,
    InvalidRequest,
    Limit,
    InvalidOwner,
    MissingQualifiedOwner,
    /// The document has an uncommitted edit; a later read sees it committed.
    Pending,
    /// The cursor belongs to an earlier committed basis; restart without it.
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

/// One page an app's projection produced at the heads it was given.
pub struct ReadPage {
    pub content: serde_json::Value,
    pub next_cursor: Option<String>,
}

/// An app projection over `document` at exactly `heads`, for a request the
/// registration's decoder accepted.
pub type ReadProjection =
    fn(&AutoCommit, &[ChangeHash], &[u8]) -> Result<ReadPage, SemanticReadError>;

/// App-owned code paired with a closed request decoder and exact result schema.
/// Only the actual resolved Widget's table participates in SDK selection. This
/// public registration cannot create a frame, response stamp, or permission.
pub struct RegisteredSemanticReader {
    operation_type: &'static str,
    schema: fn() -> &'static serde_json::Value,
    validate: fn(&Resource, &[u8]) -> Result<(), SemanticReadError>,
    project: ReadProjection,
}

impl RegisteredSemanticReader {
    pub(crate) fn operation_type(&self) -> &'static str {
        self.operation_type
    }

    pub const fn new(
        operation_type: &'static str,
        schema: fn() -> &'static serde_json::Value,
        validate: fn(&Resource, &[u8]) -> Result<(), SemanticReadError>,
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

/// A borrowed selection from the actual app table and exact current resource.
/// It owns no copied client contract and exposes no constructor outside the SDK.
pub(crate) struct SelectedSemanticRead<'a> {
    registration: &'a RegisteredSemanticReader,
    resource: &'a Resource,
    spec: &'a ActionSpec,
    encoded: &'a EncodedAction,
}

pub(crate) fn select<'a>(
    table: &'a [RegisteredSemanticReader],
    resource: &'a Resource,
    spec: &'a ActionSpec,
    encoded: &'a EncodedAction,
) -> Result<Option<SelectedSemanticRead<'a>>, SemanticReadError> {
    let mut matching = table.iter().filter(|entry| {
        Some(entry.operation_type) == spec.operation_type.as_deref()
            || entry.operation_type == encoded.type_id
    });
    let Some(registration) = matching.next() else {
        return Ok(None);
    };
    if registration.operation_type.is_empty() || matching.next().is_some() {
        return Err(SemanticReadError::InvalidRegistration);
    }
    let selected = SelectedSemanticRead {
        registration,
        resource,
        spec,
        encoded,
    };
    selected.check_contract()?;
    if encoded.payload.len() > MAX_REQUEST_BYTES {
        return Err(SemanticReadError::Limit);
    }
    (registration.validate)(resource, &encoded.payload)?;
    Ok(Some(selected))
}

impl SelectedSemanticRead<'_> {
    fn check_contract(&self) -> Result<(), SemanticReadError> {
        use tilo_actions::{ExecutionMode, OperationKind, PermissionRequirement, ResultSpec};
        if !self
            .resource
            .actions
            .iter()
            .any(|actual| std::ptr::eq(actual, self.spec))
            || self.spec.kind != OperationKind::Effect
            || self.spec.operation_type.as_deref() != Some(self.registration.operation_type)
            || self.encoded.type_id != self.registration.operation_type
            || self.spec.contract.permission != PermissionRequirement::Read
            || self.spec.contract.execution != ExecutionMode::Immediate
            || !matches!(&self.spec.contract.result, ResultSpec::Json { schema } if schema == (self.registration.schema)())
        {
            return Err(SemanticReadError::InvalidContract);
        }
        Ok(())
    }

    /// Run the reader over the caller's canonical record at its committed heads.
    ///
    /// `document` must be the record store's current loan of the record (the
    /// scope's, for a hosted invocation) or the app's mounted primary document. Nothing is
    /// committed: a document with a pending edit refuses. The original caller is
    /// rechecked after projection, immediately before the page is returned.
    pub(crate) fn read(
        &self,
        scope: Option<&crate::invocation_scope::ResourceInvocationScope>,
        document: &AutoCommit,
    ) -> Result<tilo_actions::ActionOutput, SemanticReadError> {
        self.check_contract()?;
        let record = match scope {
            Some(scope) => {
                scope
                    .check_read_loan(document)
                    .map_err(|_| SemanticReadError::InvalidOwner)?;
                crate::invocation_scope::RecordStamp::from(scope.route())
            }
            None => crate::invocation_scope::identified_record(document)
                .ok_or(SemanticReadError::MissingQualifiedOwner)?,
        };
        if document.pending_ops() != 0 {
            return Err(SemanticReadError::Pending);
        }
        let heads = document.base_heads();
        let page = (self.registration.project)(document, &heads, &self.encoded.payload)?;
        let mut hex: Vec<String> = heads.iter().map(ToString::to_string).collect();
        hex.sort_unstable();
        let result = serde_json::json!({
            "version": 1,
            "record": {
                "roomId": record.room.map(|room| room.to_string()),
                "appId": record.app_id,
                "recordId": record.record_id,
            },
            "basis": { "heads": hex },
            "content": page.content,
            "nextCursor": page.next_cursor,
        });
        if let Some(scope) = scope {
            scope.check().map_err(|_| SemanticReadError::InvalidOwner)?;
        }
        Ok(tilo_actions::ActionOutput {
            value: Some(result),
            resources: Vec::new(),
        })
    }
}

/// The reader registered anywhere in `widget`'s retained tree for `type_id`.
pub(crate) fn registered_in_tree(
    widget: &dyn crate::widget::Widget,
    type_id: &str,
) -> Option<&'static [RegisteredSemanticReader]> {
    if widget
        .semantic_readers()
        .iter()
        .any(|reader| reader.operation_type == type_id)
    {
        return Some(widget.semantic_readers());
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = registered_in_tree(child, type_id);
        }
    });
    found
}

#[cfg(test)]
mod tests;
