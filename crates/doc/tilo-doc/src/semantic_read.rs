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
use std::{cell::Cell, io::Write};

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

/// Inert cursor scope for the actual bound tilo-doc object, using canonical counter/actor identity.
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
fn field<'a>(
    doc: &'a AutoCommit,
    obj: &ObjId,
    key: &str,
) -> Result<Option<automerge::iter::MapRangeItem<'a>>, ReadError> {
    if key.len() > 256 {
        return Err(ReadError::Limit);
    }
    Ok(doc.map_range(obj, key.to_owned()..=key.to_owned()).next())
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
