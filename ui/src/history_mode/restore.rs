//! Checked content-only historical restoration. The caller owns authorization and commit.
use automerge::{
    AutoCommit, AutomergeError, ChangeHash, ObjType, ReadDoc, TextEncoding,
    transaction::Transactable,
};
use std::collections::BTreeSet;
mod projection;
mod writer;
pub use projection::ContentProjection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreValueType {
    Object(ObjType),
    Scalar(ScalarType),
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

#[derive(Debug)]
pub struct HistoryRestoreSchema {
    pub id: &'static str,
    pub version: u32,
    pub roots: &'static [HistoryRestoreRoot],
    pub preserved_roots: &'static [&'static str],
    pub validate: fn(&AutoCommit) -> Result<(), String>,
}

#[derive(Clone, Debug)]
pub struct PreparedContentRestore {
    schema: &'static HistoryRestoreSchema,
    current_heads: Vec<ChangeHash>,
    target_heads: Vec<ChangeHash>,
    current: ContentProjection,
    target: ContentProjection,
    protected: ContentProjection,
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
/// Targets with content conflicts or rich-text blocks are unsupported. All
/// replayed sequence marks use `ExpandMark::None` for their current ranges.
/// Internal object identities of changed content roots may be recreated.
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
    validate_schema(schema)?;
    require_committed(doc)?;
    let mut source = doc.clone();
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
            .apply_changes(target_doc.get_changes(&[]))
            .map_err(|error| RestoreError::read("historical target", &error))?;
        target_doc = encoded;
    }
    let target = validated_content(&target_doc, schema)?;
    let current = validated_content(doc, schema)?;
    let protected = projection::protected(doc, schema)?;
    let prepared = PreparedContentRestore {
        schema,
        current_heads,
        target_heads,
        current,
        target,
        protected,
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
    if projection::protected(doc, prepared.schema)? != prepared.protected {
        return Err(RestoreError::ProtectedStateChanged);
    }
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
    copy.integrate();
    if visible != sorted_heads(&copy.get_heads()) {
        return Err(RestoreError::StaleReview);
    }
    Ok(visible)
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
fn validate_schema(schema: &HistoryRestoreSchema) -> Result<(), RestoreError> {
    if schema.id.is_empty() || schema.version == 0 || schema.roots.is_empty() {
        return Err(RestoreError::InvalidSchema(
            "schema id, nonzero version and content roots are required".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for root in schema.roots {
        if root.key.is_empty()
            || root.key.starts_with("qos:")
            || schema.preserved_roots.contains(&root.key)
            || !seen.insert(root.key)
        {
            return Err(RestoreError::InvalidSchema(format!(
                "duplicate, empty or protected root {}",
                root.key
            )));
        }
    }
    Ok(())
}
fn validated_content(
    doc: &AutoCommit,
    schema: &HistoryRestoreSchema,
) -> Result<ContentProjection, RestoreError> {
    let projection = projection::content(doc, schema)?;
    (schema.validate)(doc).map_err(RestoreError::SchemaInvalid)?;
    Ok(projection)
}
fn write_and_check(
    doc: &mut AutoCommit,
    prepared: &PreparedContentRestore,
) -> Result<(), RestoreError> {
    writer::write(doc, &prepared.current, &prepared.target)?;
    if validated_content(doc, prepared.schema)? != prepared.target {
        return Err(RestoreError::ContentPostconditionMismatch);
    }
    if projection::protected(doc, prepared.schema)? != prepared.protected {
        return Err(RestoreError::ProtectedStateChanged);
    }
    Ok(())
}

#[cfg(test)]
#[path = "restore/tests.rs"]
mod tests;
