//! Checked content-only historical restoration. The caller owns authorization and commit.
use automerge::{
    AutoCommit, AutomergeError, ChangeHash, ObjType, ReadDoc, TextEncoding,
    transaction::Transactable,
};
use tilo_automerge::ApplyIncoming;
mod import;
mod projection;
pub use import::prepare_external_content_import;
mod schema;
mod selection;
mod writer;
pub use projection::ContentProjection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreValueType {
    Object(ObjType),
    Scalar(ScalarType),
    /// Whatever the property holds. For content whose stored representation
    /// may vary (text written as a scalar by an older schema, an integer read
    /// as a float); the schema's `validate` functions own its shape.
    Any,
}

impl RestoreValueType {
    /// Whether a stored value of type `actual` satisfies this expectation.
    pub fn accepts(self, actual: Self) -> bool {
        self == Self::Any || self == actual
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarType {
    Bytes,
    String,
    Int,
    Uint,
    Float,
    Counter,
    Timestamp,
    Boolean,
    Null,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
pub struct HistoryRestoreRoot {
    pub key: &'static str,
    pub required: bool,
    pub expected_type: RestoreValueType,
}

/// Declarative family captures are nonempty UTF-8 strings without control characters.
/// Their byte length is in `1..=max_capture_bytes` (maximum 256). Affixes
/// are ASCII, at most 256 bytes each; the nonempty prefix contains `.` or `:`.
/// Keys with the fixed affixes but invalid captures fail content validation.
#[derive(Clone, Copy, Debug)]
pub enum HistoryRootPattern {
    Exact(&'static str),
    Affix {
        prefix: &'static str,
        suffix: &'static str,
        max_capture_bytes: usize,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct HistoryRootSelector {
    pub pattern: HistoryRootPattern,
    /// An exact property must exist; a family must have at least one member.
    pub required: bool,
    pub node: HistoryRestoreNode,
}
#[derive(Clone, Copy, Debug)]
pub enum HistoryRestoreNode {
    Whole {
        expected_type: RestoreValueType,
    },
    /// Author every immediate Map property except 1..=64 exact retained names.
    /// Names are case-sensitive, nonempty, unique, and at most 1024 UTF-8
    /// bytes. They are literal properties, not paths. Optional structural
    /// absence normalizes to an empty authored Map.
    MapContentExcept {
        preserved_fields: &'static [&'static str],
    },
    /// Retain the current container and reconcile only declared map children.
    /// Lists/text must be selected whole; positional overlays are not supported.
    MapOverlay(&'static HistoryMapOverlay),
}
#[derive(Debug)]
pub struct HistoryMapOverlay {
    pub fields: &'static [HistoryMapField],
    /// Select membership for all keys other than the fixed fields.
    pub entries: Option<&'static HistoryMapEntries>,
}
#[derive(Clone, Copy, Debug)]
pub struct HistoryMapField {
    pub key: &'static str,
    pub required: bool,
    pub node: HistoryRestoreNode,
}
#[derive(Debug)]
pub struct HistoryMapEntries {
    /// Nonempty application identity, bounded to at most 1024 UTF-8 bytes.
    pub key_limit_bytes: usize,
    pub node: HistoryRestoreNode,
    pub membership: HistoryMembership,
    pub on_create: PreservedOnCreate,
}
#[derive(Clone, Copy, Debug)]
pub enum HistoryMembership {
    /// Deleting an identity deletes its complete subtree, including protected
    /// descendants. Retained identities keep current unselected descendants.
    TargetAuthoritative,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreservedOnCreate {
    /// Copy selected fields only; the combined result validator must accept
    /// missing operational fields. Historical operational values never return.
    Omit,
    /// Refuse any target identity missing from the current keyed map.
    Reject,
}

#[derive(Debug)]
pub struct HistoryRestoreSchema {
    pub id: &'static str,
    pub version: u32,
    /// Convenience exact whole-node selectors; checked together with `selectors`.
    pub roots: &'static [HistoryRestoreRoot],
    pub selectors: &'static [HistoryRootSelector],
    pub preserved_roots: &'static [&'static str],
    pub preserved: &'static [HistoryRootPattern],
    /// Read-only validation of current/target selected content and shape.
    pub validate: fn(&AutoCommit) -> Result<(), String>,
    /// Read-only validation of combined restored content/current operational state.
    pub validate_result: fn(&AutoCommit) -> Result<(), String>,
}

#[derive(Clone, Debug)]
pub struct PreparedContentRestore {
    schema: &'static HistoryRestoreSchema,
    current_heads: Vec<ChangeHash>,
    target_heads: Vec<ChangeHash>,
    current: ContentProjection,
    target: ContentProjection,
    protected: ContentProjection,
    protected_identities: selection::IdentityPaths,
    encoding: TextEncoding,
}
impl PreparedContentRestore {
    pub fn schema(&self) -> &'static HistoryRestoreSchema {
        self.schema
    }
    pub fn current_heads(&self) -> &[ChangeHash] {
        &self.current_heads
    }
    pub fn target_heads(&self) -> &[ChangeHash] {
        &self.target_heads
    }
    pub fn target_projection(&self) -> &ContentProjection {
        &self.target
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Changed,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    InvalidSchema(String),
    PendingEdits,
    StaleReview,
    InvalidTargetHeads,
    SchemaInvalid(String),
    UnsupportedConflict(String),
    UnsupportedContent(String),
    Automerge(String),
    ContentPostconditionMismatch,
    ProtectedStateChanged,
}
impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSchema(reason) => write!(f, "invalid History restore schema: {reason}"),
            Self::PendingEdits => f.write_str("finish pending edits before restoring a version"),
            Self::StaleReview => f.write_str("the live record changed after the restore review"),
            Self::InvalidTargetHeads => {
                f.write_str("the selected version does not exist in this record")
            }
            Self::SchemaInvalid(reason) => write!(f, "content schema validation failed: {reason}"),
            Self::UnsupportedConflict(path) => {
                write!(f, "conflicted content cannot be restored at {path}")
            }
            Self::UnsupportedContent(reason) => {
                write!(f, "unsupported historical content: {reason}")
            }
            Self::Automerge(reason) => {
                write!(f, "History restore document operation failed: {reason}")
            }
            Self::ContentPostconditionMismatch => {
                f.write_str("restored content does not match the selected version")
            }
            Self::ProtectedStateChanged => {
                f.write_str("restore would change protected record state")
            }
        }
    }
}
impl std::error::Error for RestoreError {}
impl RestoreError {
    fn read(path: &str, error: &AutomergeError) -> Self {
        Self::Automerge(format!("read {path}: {error}"))
    }
    fn write(path: &str, error: &AutomergeError) -> Self {
        Self::Automerge(format!("write {path}: {error}"))
    }
}

/// Prepare exact content restoration without mutating or committing the input.
///
/// Both current and historical content must satisfy the registered schema.
/// Targets with content conflicts or rich-text blocks are unsupported, as is
/// any record whose history created a Table object. All
/// replayed sequence marks use `ExpandMark::None` for their current ranges.
/// Whole Map values at map-addressed properties retain their current
/// object identities when the current and target kinds match, recursively for
/// map-addressed descendants. Other changed whole values may be recreated.
/// Mixed structural ancestors and retained entities keep current identities.
/// This value grants no authority: the host must check the reviewed resource,
/// lifetime and current permission inside its exclusive record action frame.
///
/// # Errors
/// Returns a typed error for pending edits, stale heads, invalid registration,
/// unavailable target history, unsupported content, or failed validation.
pub fn prepare_content_restore(
    doc: &AutoCommit,
    schema: &'static HistoryRestoreSchema,
    current_heads: &[ChangeHash],
    target_heads: &[ChangeHash],
) -> Result<PreparedContentRestore, RestoreError> {
    schema::validate(schema)?;
    require_committed(doc)?;
    let mut source = doc.clone();
    refuse_tables(&mut source)?;
    let current_heads = sorted_heads(current_heads);
    if committed_heads(doc)? != current_heads {
        return Err(RestoreError::StaleReview);
    }
    let target_heads = sorted_heads(target_heads);
    if target_heads
        .iter()
        .any(|hash| source.get_change_by_hash(hash).is_none())
    {
        return Err(RestoreError::InvalidTargetHeads);
    }
    let mut target_doc = source
        .fork_at(&target_heads)
        .map_err(|_| RestoreError::InvalidTargetHeads)?;
    // Automerge 0.8 fork_at constructs a default-encoding document. Rebuild
    // only the historical read model with the live encoding so mark positions
    // have the same units in the target, trial and live postconditions.
    if target_doc.text_encoding() != doc.text_encoding() {
        let mut encoded = AutoCommit::new_with_encoding(doc.text_encoding());
        encoded
            .apply_incoming(target_doc.get_changes(&[]))
            .map_err(|error| RestoreError::read("historical target", &error))?;
        target_doc = encoded;
    }
    let target = validated_content(&target_doc, schema)?;
    let current = validated_content(doc, schema)?;
    (schema.validate_result)(doc).map_err(RestoreError::SchemaInvalid)?;
    let (protected, protected_identities) = selection::protected(doc, schema, &target, None)?;
    let prepared = PreparedContentRestore {
        schema,
        current_heads,
        target_heads,
        current,
        target,
        protected,
        protected_identities,
        encoding: doc.text_encoding(),
    };
    if prepared.current != prepared.target {
        let mut trial = source.fork();
        write_and_check(&mut trial, &prepared)?;
    }
    Ok(prepared)
}

/// Apply to the live document's pending transaction; the caller commits once.
/// An unchanged target leaves no pending operations and needs no action entry.
/// The host must refresh cached content object handles after a changed result.
///
/// # Errors
/// Returns an error without changing the document if review/preconditions fail.
/// Any writer, validator or postcondition error rolls back the entire newly
/// opened transaction. Existing pending transactions are refused, never rolled back.
pub fn apply_prepared_content_restore(
    doc: &mut AutoCommit,
    prepared: &PreparedContentRestore,
) -> Result<RestoreOutcome, RestoreError> {
    require_committed(doc)?;
    if committed_heads(doc)? != prepared.current_heads || doc.text_encoding() != prepared.encoding {
        return Err(RestoreError::StaleReview);
    }
    if validated_content(doc, prepared.schema)? != prepared.current {
        return Err(RestoreError::StaleReview);
    }
    if selection::protected(
        doc,
        prepared.schema,
        &prepared.target,
        Some(&prepared.protected_identities),
    )?
    .0 != prepared.protected
    {
        return Err(RestoreError::ProtectedStateChanged);
    }
    (prepared.schema.validate_result)(doc).map_err(RestoreError::SchemaInvalid)?;
    if prepared.current == prepared.target {
        return Ok(RestoreOutcome::Unchanged);
    }
    if let Err(error) = write_and_check(doc, prepared) {
        doc.rollback();
        return Err(error);
    }
    Ok(RestoreOutcome::Changed)
}

fn committed_heads(doc: &AutoCommit) -> Result<Vec<ChangeHash>, RestoreError> {
    require_committed(doc)?;
    let mut copy = doc.clone();
    let visible = sorted_heads(&copy.get_heads());
    copy.integrate_all();
    if visible != sorted_heads(&copy.get_heads()) {
        return Err(RestoreError::StaleReview);
    }
    Ok(visible)
}

/// Refuse a record whose history ever created a Table object. Stock automerge
/// 0.12 cannot address or apply populated Tables (writes fail and applying
/// such changes can panic), and no Tilo app creates them. The check reads the
/// encoded history only, so it runs before any fork, merge or re-encoding.
fn refuse_tables(doc: &mut AutoCommit) -> Result<(), RestoreError> {
    let creates_table = doc.get_changes(&[]).iter().any(|change| {
        change
            .decode()
            .operations
            .iter()
            .any(|op| matches!(op.action, automerge::legacy::OpType::Make(ObjType::Table)))
    });
    if creates_table {
        return Err(RestoreError::UnsupportedContent(
            "records containing Table objects cannot be restored".into(),
        ));
    }
    Ok(())
}

fn sorted_heads(heads: &[ChangeHash]) -> Vec<ChangeHash> {
    let mut result = heads.to_vec();
    result.sort_unstable();
    result.dedup();
    result
}
fn require_committed(doc: &AutoCommit) -> Result<(), RestoreError> {
    if doc.pending_ops() > 0 {
        Err(RestoreError::PendingEdits)
    } else {
        Ok(())
    }
}
fn validated_content(
    doc: &AutoCommit,
    schema: &HistoryRestoreSchema,
) -> Result<ContentProjection, RestoreError> {
    let projection = selection::content(doc, schema)?;
    (schema.validate)(doc).map_err(RestoreError::SchemaInvalid)?;
    Ok(projection)
}
fn write_and_check(
    doc: &mut AutoCommit,
    prepared: &PreparedContentRestore,
) -> Result<(), RestoreError> {
    writer::write(doc, prepared.schema, &prepared.current, &prepared.target)?;
    (prepared.schema.validate_result)(doc).map_err(RestoreError::SchemaInvalid)?;
    if validated_content(doc, prepared.schema)? != prepared.target {
        return Err(RestoreError::ContentPostconditionMismatch);
    }
    if selection::protected(
        doc,
        prepared.schema,
        &prepared.target,
        Some(&prepared.protected_identities),
    )?
    .0 != prepared.protected
    {
        return Err(RestoreError::ProtectedStateChanged);
    }
    Ok(())
}

#[cfg(test)]
#[path = "restore/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "restore/selector_tests.rs"]
mod selector_tests;
