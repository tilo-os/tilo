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
use tilo_sort::CompactSort as _;

pub mod retirement;

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

/// The exact result schema of a read whose projection yields `content`: the
/// SDK stamps `version`, `record` and `basis` around it. A `paged` read
/// returns an opaque `nextCursor` or null; any other read always returns null.
#[must_use]
pub fn result_schema(content: serde_json::Value, paged: bool) -> serde_json::Value {
    let next_cursor = if paged {
        serde_json::json!({"type":["string","null"],"maxLength":MAX_CURSOR_BYTES})
    } else {
        serde_json::json!({"type":"null"})
    };
    serde_json::json!({"type":"object","required":["version","record","basis","content","nextCursor"],"additionalProperties":false,"properties":{
        "version":{"type":"integer","enum":[1]},
        "record":{"type":"object","required":["roomId","appId","recordId"],"additionalProperties":false,"properties":{
            "roomId":{"type":["string","null"],"format":"uuid"},"appId":{"type":"string","minLength":1,"maxLength":256},"recordId":{"type":"string","maxLength":512}
        }},
        "basis":{"type":"object","required":["heads"],"additionalProperties":false,"properties":{
            "heads":{"type":"array","maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[0-9a-f]{64}$"}}
        }},
        "content":content,
        "nextCursor":next_cursor
    }})
}

/// A reader's closed request, decoded from its payload. The SDK has already
/// refused a payload over [`MAX_REQUEST_BYTES`].
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

/// What a registered read's published action is called: its id, its label
/// and the line that says what it takes.
#[derive(Clone, Copy, Debug)]
pub struct ReadName<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub description: &'a str,
}

/// The parameters of a read that takes none.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, tilo_resource::JsonSchema)]
pub struct NoReadParams {}

/// The published action of the registered read whose operation is
/// `operation`. Its parameters are the fields of `P` as `P`'s JSON Schema
/// describes them (doc comments, `#[schema(range(..))]` bounds, choices); a
/// caller's arguments, bound strictly to them, become a `P` with serde, and
/// `request` turns that into the reader's request. It answers `schema`, and
/// it is a view of the document: it changes nothing.
#[must_use]
pub fn read_action<P, Q>(
    name: ReadName<'_>,
    operation: &'static str,
    schema: fn() -> &'static serde_json::Value,
    request: impl Fn(P) -> Q + Send + Sync + 'static,
) -> ActionSpec
where
    P: serde::de::DeserializeOwned + tilo_resource::JsonSchema,
    Q: serde::Serialize,
{
    use tilo_actions::{ActionContract, OperationKind, ResultSpec};
    let params = tilo_resource::__private::flat_params(&P::json_schema());
    ActionSpec::typed_operation(
        name.id,
        name.label,
        name.description,
        OperationKind::Effect,
        operation,
        params,
        move |args| {
            serde_json::from_value::<P>(serde_json::Value::Object(args.to_json()))
                .ok()
                .and_then(|params| {
                    EncodedAction::encode_operation(operation, &request(params)).ok()
                })
                // An empty payload is no request: the reader refuses it.
                .unwrap_or_else(|| EncodedAction {
                    type_id: operation.into(),
                    payload: Vec::new(),
                })
        },
    )
    .with_contract(ActionContract {
        result: ResultSpec::Json {
            schema: schema().clone(),
        },
        ..ActionContract::view()
    })
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
        use tilo_actions::{ActionKind, ExecutionMode, OperationKind, ResultSpec};
        if !self
            .resource
            .actions
            .iter()
            .any(|actual| std::ptr::eq(actual, self.spec))
            || self.spec.kind != OperationKind::Effect
            || self.spec.operation_type.as_deref() != Some(self.registration.operation_type)
            || self.encoded.type_id != self.registration.operation_type
            || self.spec.contract.kind != ActionKind::Read
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
        hex.compact_sort();
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

/// Inert descriptors for reads registered by the actual retained widget tree.
/// This admits no invocation or document: execution still needs the normal
/// bound SDK door and its canonical owner loan. Generic effects are excluded.
pub fn registered_operations(
    widget: &dyn crate::widget::Widget,
    resources: &[Resource],
) -> Vec<tilo_actions::ActionDescriptor> {
    fn collect(
        widget: &dyn crate::widget::Widget,
        resource: &Resource,
        addresses: &mut std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for spec in &resource.actions {
            let Some(type_id) = spec.operation_type.as_deref() else {
                continue;
            };
            if type_id.is_empty() {
                continue;
            }
            let Some(table) = registered_in_tree(widget, type_id) else {
                continue;
            };
            let mut matching = table
                .iter()
                .filter(|reader| reader.operation_type == type_id);
            let Some(registration) = matching.next() else {
                continue;
            };
            if matching.next().is_some() {
                continue;
            }
            let encoded = EncodedAction {
                type_id: type_id.to_owned(),
                payload: Vec::new(),
            };
            if (SelectedSemanticRead {
                registration,
                resource,
                spec,
                encoded: &encoded,
            })
            .check_contract()
            .is_ok()
            {
                addresses.insert(tilo_actions::ActionAddress::new(
                    resource.id.as_str(),
                    &spec.id,
                ));
            }
        }
        for child in &resource.children {
            collect(widget, child, addresses);
        }
    }
    let mut addresses = std::collections::HashSet::new();
    for resource in resources {
        collect(widget, resource, &mut addresses);
    }
    tilo_actions::door::survey(resources)
        .into_iter()
        .filter(|descriptor| addresses.contains(&descriptor.address))
        .collect()
}

/// A widget whose document answers semantic reads through registered
/// readers. Registration alone supplies no qualified owner frame or
/// disclosure. Declared with [`crate::provides!`].
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

/// The reader registered anywhere in `widget`'s retained tree for `type_id`.
pub(crate) fn registered_in_tree(
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

#[cfg(test)]
mod tests;
