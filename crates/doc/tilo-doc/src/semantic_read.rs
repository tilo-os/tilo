//! Bounded immutable app projections. These helpers confer no authorization.
use serde::{Deserialize, Serialize};
pub mod source;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub items: usize,
    pub json_bytes: usize,
    pub scan_entries: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            items: 32,
            json_bytes: 64 * 1024,
            scan_entries: 100_000,
        }
    }
}
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("document has uncommitted operations")]
    Pending,
    #[error("cursor belongs to a different committed basis")]
    StaleCursor,
    #[error("invalid semantic cursor")]
    InvalidCursor,
    #[error("semantic read limit exceeded")]
    Limit,
    #[error("required semantic container is malformed")]
    Malformed,
}
#[derive(Clone, Debug, Serialize)]
pub struct Basis {
    pub heads: Vec<String>,
    #[serde(skip)]
    pub fingerprint: [u8; 32],
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextPosition {
    pub element: usize,
    pub byte: usize,
}

use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, ScalarValueRef, ValueRef, transaction::Transactable,
};
use std::{borrow::Cow, cell::Cell, io::Write};

/// Counts visible rows/characters visited by the projection, including skipped prefixes,
/// against [`Limits::scan_entries`]. Iterator internals are additionally bounded by the
/// document operation ceiling checked in [`begin`].
pub struct Work {
    remaining: Cell<usize>,
}
impl Work {
    fn new(scan_entries: usize) -> Self {
        Self {
            remaining: Cell::new(scan_entries),
        }
    }
    /// Charge one visited row or character.
    ///
    /// # Errors
    ///
    /// [`ReadError::Limit`] once the scan budget is exhausted.
    pub fn visit(&self) -> Result<(), ReadError> {
        let remaining = self
            .remaining
            .get()
            .checked_sub(1)
            .ok_or(ReadError::Limit)?;
        self.remaining.set(remaining);
        Ok(())
    }
}
/// Validates all scalar page ceilings before any output allocation.
///
/// # Errors
///
/// [`ReadError::Limit`] when any ceiling is zero or out of range.
pub fn validate_limits(limits: Limits, max_items: usize) -> Result<(), ReadError> {
    if limits.items == 0
        || limits.items > max_items
        || !(512..=65_536).contains(&limits.json_bytes)
        || limits.scan_entries == 0
        || limits.scan_entries > 1_000_000
    {
        return Err(ReadError::Limit);
    }
    Ok(())
}

/// Prepare a projection from the complete bounded head slice supplied by its actual owner frame.
/// The slice is data, not authority: this helper cannot prove it belongs to `doc`. The registered
/// SDK reader must bind that exact slice/document and stamp the current route before disclosure.
/// No legacy head getter is called: even immutable `base_heads` can clone an unbounded isolation
/// vector. Pending operations are refused without committing them.
///
/// # Errors
///
/// [`ReadError::Pending`] for uncommitted operations, [`ReadError::Limit`] for invalid limits or
/// oversized documents/head slices, and [`ReadError::Malformed`] for duplicate heads.
pub fn begin(
    doc: &AutoCommit,
    owner_heads: &[automerge::ChangeHash],
    limits: Limits,
    max_items: usize,
) -> Result<(Basis, Work), ReadError> {
    source::Source::new(doc, owner_heads).begin(limits, max_items)
}

/// Inert cursor scope for the actual bound qdoc object, using canonical counter/actor identity.
/// The cached actor-table index is deliberately excluded. This is not a record/caller witness.
///
/// # Errors
///
/// [`ReadError::Limit`] when the actor id exceeds 256 bytes.
pub fn object_fingerprint(object: &ObjId) -> Result<[u8; 32], ReadError> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"qdoc-semantic-object-v1\0");
    match object {
        ObjId::Root => {
            hash.update(&[0]);
        }
        ObjId::Id(counter, actor, _) => {
            if actor.to_bytes().len() > 256 {
                return Err(ReadError::Limit);
            }
            hash.update(&[1]);
            hash.update(&counter.to_le_bytes());
            hash.update(actor.to_bytes());
        }
    }
    Ok(*hash.finalize().as_bytes())
}

/// Exact winning map entry, preserving the existing app's winner semantics without converting
/// borrowed scalar payloads to the legacy owned Value representation.
///
/// # Errors
///
/// [`ReadError::Limit`] when `key` exceeds 256 bytes.
pub fn field<'a>(
    doc: &'a AutoCommit,
    obj: &ObjId,
    key: &str,
) -> Result<Option<automerge::iter::MapRangeItem<'a>>, ReadError> {
    if key.len() > 256 {
        return Err(ReadError::Limit);
    }
    Ok(doc.map_range(obj, key.to_owned()..=key.to_owned()).next())
}

/// The winning object of `kind` under `key`, if any.
///
/// # Errors
///
/// As [`field`].
pub fn object(
    doc: &AutoCommit,
    obj: &ObjId,
    key: &str,
    kind: ObjType,
) -> Result<Option<ObjId>, ReadError> {
    Ok(field(doc, obj, key)?
        .filter(|v| v.value == ValueRef::Object(kind))
        .map(|v| v.id()))
}

/// The winning string under `key`, or empty when it is absent or not a string.
///
/// # Errors
///
/// As [`field`], and [`ReadError::Limit`] when the string exceeds `maximum` bytes.
pub fn string(
    doc: &AutoCommit,
    obj: &ObjId,
    key: &str,
    maximum: usize,
) -> Result<String, ReadError> {
    match field(doc, obj, key)?.map(|v| v.value) {
        Some(ValueRef::Scalar(ScalarValueRef::Str(s))) => {
            if s.len() > maximum {
                return Err(ReadError::Limit);
            }
            Ok(s.into_owned())
        }
        _ => Ok(String::new()),
    }
}

/// The winning byte payload under `key`, if any.
///
/// # Errors
///
/// As [`field`].
pub fn bytes<'a>(
    doc: &'a AutoCommit,
    obj: &ObjId,
    key: &str,
) -> Result<Option<Cow<'a, [u8]>>, ReadError> {
    Ok(match field(doc, obj, key)?.map(|v| v.value) {
        Some(ValueRef::Scalar(ScalarValueRef::Bytes(bytes))) => Some(bytes),
        _ => None,
    })
}

/// Count the actual serde JSON representation without allocating it. The caller may reserve
/// that exact output amount in its owner before serializing; this helper grants no credit.
///
/// # Errors
///
/// [`ReadError::Limit`] when the representation exceeds `maximum` bytes or fails to serialize.
pub fn json_len(value: &impl Serialize, maximum: usize) -> Result<usize, ReadError> {
    struct Count {
        used: usize,
        maximum: usize,
    }
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let next = self
                .used
                .checked_add(bytes.len())
                .filter(|n| *n <= self.maximum)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
            self.used = next;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count { used: 0, maximum };
    serde_json::to_writer(&mut count, value).map_err(|_| ReadError::Limit)?;
    Ok(count.used)
}
fn escaped_bytes(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
        c if c <= '\u{1f}' => 6,
        c => c.len_utf8(),
    }
}

/// A fragment's cursor is the text index where an element starts plus a UTF-8 byte offset
/// *within* that element. This handles multi-character Automerge string elements without
/// assuming text indices equal bytes or Unicode scalar indices. Non-string text atoms render
/// U+FFFC, matching Automerge's plain text projection; marks occupy no text index and are never
/// visited.
///
/// # Errors
///
/// [`ReadError::InvalidCursor`] for a cursor outside the text and [`ReadError::Limit`] when the
/// scan or escaped budget cannot make progress.
pub fn text_fragment(
    doc: &AutoCommit,
    text: Option<&ObjId>,
    position: TextPosition,
    escaped_budget: usize,
    work: &Work,
) -> Result<(String, TextPosition, bool), ReadError> {
    source::Source::new(doc, &[]).text_fragment(text, position, escaped_budget, work)
}
