//! Mechanical projection source over a committed document. It is not a caller/record
//! authority witness.
use super::{
    AutoCommit, Basis, Limits, ObjId, ObjType, ReadDoc, ReadError, ScalarValueRef, TextPosition,
    Transactable, ValueRef, Work, escaped_bytes, field, object_fingerprint, validate_limits,
};
use std::ops::ControlFlow;
use tilo_sort::CompactSort as _;

/// Maximum operations a projected document may hold.
const MAX_OPERATIONS: u64 = 1_000_000;
/// Maximum heads a projection basis may bind.
const MAX_HEADS: usize = 64;

pub struct Source<'a> {
    doc: &'a AutoCommit,
    heads: &'a [automerge::ChangeHash],
}
/// A visited value: an object of a known kind, or a scalar borrowed for the visit.
pub enum Value<'v> {
    Object(ObjType, ObjId),
    Scalar(&'v ScalarValueRef<'v>),
}
impl Value<'_> {
    pub fn object(self, wanted: ObjType) -> Option<ObjId> {
        match self {
            Self::Object(kind, object) if kind == wanted => Some(object),
            Self::Object(..) | Self::Scalar(_) => None,
        }
    }
}
pub struct Entry<'v> {
    pub index: usize,
    pub value: Value<'v>,
}
fn value<'v>(value: &'v ValueRef<'_>, object: ObjId) -> Value<'v> {
    match value {
        ValueRef::Object(kind) => Value::Object(*kind, object),
        ValueRef::Scalar(scalar) => Value::Scalar(scalar),
    }
}
/// Lowercase hexadecimal digit of a nibble (`0..16`).
fn hex_digit(nibble: u8) -> char {
    char::from_digit(u32::from(nibble), 16).unwrap_or('0')
}
impl<'a> Source<'a> {
    /// Supplied heads are data, not a binding witness.
    pub fn new(doc: &'a AutoCommit, heads: &'a [automerge::ChangeHash]) -> Self {
        Self { doc, heads }
    }
    pub fn root(&self) -> ObjId {
        automerge::ROOT
    }
    /// Bind a projection basis to the source's complete head set.
    ///
    /// # Errors
    ///
    /// [`ReadError::Pending`] for uncommitted operations, [`ReadError::Limit`] for invalid limits,
    /// oversized documents or more than 64 heads, and [`ReadError::Malformed`] for duplicate heads.
    pub fn begin(&self, limits: Limits, max_items: usize) -> Result<(Basis, Work), ReadError> {
        validate_limits(limits, max_items)?;
        if self.doc.pending_ops() != 0 {
            return Err(ReadError::Pending);
        }
        if self.doc.stats().num_ops > MAX_OPERATIONS || self.heads.len() > MAX_HEADS {
            return Err(ReadError::Limit);
        }
        let mut heads = [automerge::ChangeHash([0; 32]); MAX_HEADS];
        let count = self.heads.len();
        let heads = heads.get_mut(..count).ok_or(ReadError::Limit)?;
        heads.copy_from_slice(self.heads);
        heads.compact_sort();
        if heads
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left == right))
        {
            return Err(ReadError::Malformed);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"qdoc-semantic-basis-v1\0");
        let mut encoded = Vec::with_capacity(count);
        for head in heads {
            hasher.update(&head.0);
            let mut text = String::with_capacity(64);
            for byte in head.0 {
                text.push(hex_digit(byte >> 4));
                text.push(hex_digit(byte & 15));
            }
            encoded.push(text);
        }
        Ok((
            Basis {
                heads: encoded,
                fingerprint: *hasher.finalize().as_bytes(),
            },
            Work::new(limits.scan_entries),
        ))
    }
    /// Canonical cursor scope of `object` (see [`object_fingerprint`]).
    ///
    /// # Errors
    ///
    /// [`ReadError::Limit`] for an actor id over 256 bytes.
    pub fn fingerprint(&self, object: &ObjId) -> Result<[u8; 32], ReadError> {
        object_fingerprint(object)
    }
    /// Visit the winning value under `key`, if any.
    ///
    /// # Errors
    ///
    /// [`ReadError::Limit`] when `key` exceeds 256 bytes, or any error returned by `visitor`.
    pub fn field(
        &self,
        object: &ObjId,
        key: &str,
        mut visitor: impl for<'v> FnMut(Value<'v>) -> Result<(), ReadError>,
    ) -> Result<(), ReadError> {
        if let Some(item) = field(self.doc, object, key)? {
            visitor(value(&item.value, item.id()))?;
        }
        Ok(())
    }
    /// The winning object of `kind` under `key`, if any.
    ///
    /// # Errors
    ///
    /// As [`Source::field`].
    pub fn object(
        &self,
        object: &ObjId,
        key: &str,
        kind: ObjType,
    ) -> Result<Option<ObjId>, ReadError> {
        let mut result = None;
        self.field(object, key, |value| {
            result = value.object(kind);
            Ok(())
        })?;
        Ok(result)
    }
    /// The attribute map of `block` (heading level, list numbering, a cell's
    /// place), if it has one.
    ///
    /// # Errors
    ///
    /// As [`Source::field`].
    pub fn block_attrs(&self, block: &ObjId) -> Result<Option<ObjId>, ReadError> {
        self.object(block, crate::schema::BLOCK_ATTRS, ObjType::Map)
    }
    /// The winning string under `key`, or empty when it is absent or not a string.
    ///
    /// # Errors
    ///
    /// As [`Source::field`], and [`ReadError::Limit`] when the string exceeds `maximum` bytes.
    pub fn string(&self, object: &ObjId, key: &str, maximum: usize) -> Result<String, ReadError> {
        let mut result = String::new();
        self.field(object, key, |value| {
            if let Value::Scalar(ScalarValueRef::Str(text)) = value {
                if text.len() > maximum {
                    return Err(ReadError::Limit);
                }
                result.push_str(text);
            }
            Ok(())
        })?;
        Ok(result)
    }
    /// Visit the winning byte payload under `key`, if any.
    ///
    /// # Errors
    ///
    /// As [`Source::field`].
    pub fn bytes(
        &self,
        object: &ObjId,
        key: &str,
        mut visitor: impl FnMut(&[u8]) -> Result<(), ReadError>,
    ) -> Result<(), ReadError> {
        self.field(object, key, |value| {
            if let Value::Scalar(ScalarValueRef::Bytes(bytes)) = value {
                visitor(bytes)?;
            }
            Ok(())
        })
    }
    /// Visit every visible map entry until `visitor` breaks.
    ///
    /// # Errors
    ///
    /// [`ReadError::Limit`] when the scan budget is exhausted, or any error returned by `visitor`.
    pub fn map(
        &self,
        object: &ObjId,
        work: &Work,
        mut visitor: impl for<'k, 'v> FnMut(&'k str, Value<'v>) -> Result<ControlFlow<()>, ReadError>,
    ) -> Result<(), ReadError> {
        for item in self.doc.map_range(object, ..) {
            work.visit()?;
            if visitor(&item.key, value(&item.value, item.id()))?.is_break() {
                break;
            }
        }
        Ok(())
    }
    /// Visit every visible list element until `visitor` breaks.
    ///
    /// # Errors
    ///
    /// As [`Source::map`].
    pub fn list(
        &self,
        object: &ObjId,
        work: &Work,
        mut visitor: impl for<'v> FnMut(Entry<'v>) -> Result<ControlFlow<()>, ReadError>,
    ) -> Result<(), ReadError> {
        for item in self.doc.list_range(object, ..) {
            work.visit()?;
            let entry = Entry {
                index: item.index,
                value: value(&item.value, item.id()),
            };
            if visitor(entry)?.is_break() {
                break;
            }
        }
        Ok(())
    }
    /// A bounded text fragment from `position` (see [`super::text_fragment`]).
    ///
    /// Elements are addressed by their text index, so the read seeks straight to the cursor and
    /// never visits mark boundaries, which occupy no text index.
    ///
    /// # Errors
    ///
    /// [`ReadError::InvalidCursor`] for a cursor outside the text or not at an element start,
    /// [`ReadError::Limit`] when the scan or escaped budget cannot make progress, and
    /// [`ReadError::Malformed`] for a grapheme-indexed document or an unreadable text object.
    pub fn text_fragment(
        &self,
        text: Option<&ObjId>,
        position: TextPosition,
        escaped_budget: usize,
        work: &Work,
    ) -> Result<(String, TextPosition, bool), ReadError> {
        let Some(text) = text else {
            return if position == TextPosition::default() {
                Ok((String::new(), position, true))
            } else {
                Err(ReadError::InvalidCursor)
            };
        };
        let encoding = self.doc.text_encoding();
        let length = self.doc.length(text);
        if position.element >= length {
            return if position == TextPosition::default() {
                Ok((String::new(), position, true))
            } else {
                Err(ReadError::InvalidCursor)
            };
        }
        let element = |index: usize| {
            self.doc
                .get(text, index)
                .map_err(|_| ReadError::Malformed)?
                .ok_or(ReadError::Malformed)
        };
        if let Some(previous) = position.element.checked_sub(1)
            && element(previous)?.1 == element(position.element)?.1
        {
            return Err(ReadError::InvalidCursor);
        }
        let mut output = String::new();
        let mut spent = 0usize;
        let mut at = position.element;
        while at < length {
            work.visit()?;
            let (value, _) = element(at)?;
            let value = match &value {
                automerge::Value::Scalar(scalar) => match scalar.as_ref() {
                    automerge::ScalarValue::Str(s) => s.as_str(),
                    _ => OBJECT_REPLACEMENT,
                },
                automerge::Value::Object(_) => OBJECT_REPLACEMENT,
            };
            let width = text_width(encoding, value)?;
            let start = if at == position.element {
                position.byte
            } else {
                0
            };
            let suffix = value.get(start..).ok_or(ReadError::InvalidCursor)?;
            for (offset, c) in suffix.char_indices() {
                work.visit()?;
                let size = escaped_bytes(c);
                let Some(total) = spent.checked_add(size).filter(|sum| *sum <= escaped_budget)
                else {
                    if output.is_empty() {
                        return Err(ReadError::Limit);
                    }
                    let next = TextPosition {
                        element: at,
                        byte: start.checked_add(offset).ok_or(ReadError::Limit)?,
                    };
                    return Ok((output, next, false));
                };
                output.push(c);
                spent = total;
            }
            at = at.checked_add(width).ok_or(ReadError::Limit)?;
        }
        Ok((
            output,
            TextPosition {
                element: at,
                byte: 0,
            },
            true,
        ))
    }
    /// The inline marks over `fragment`, what [`Source::text_fragment`] read from `position`,
    /// clipped to it, with `start`/`end` in characters of `fragment`.
    ///
    /// # Errors
    ///
    /// [`ReadError::Limit`] when the scan budget is exhausted, and [`ReadError::Malformed`] for a
    /// grapheme-indexed document or an unreadable text object.
    pub fn fragment_marks(
        &self,
        text: Option<&ObjId>,
        position: TextPosition,
        fragment: &str,
        work: &Work,
    ) -> Result<Vec<crate::document::MarkSpan>, ReadError> {
        let Some(text) = text.filter(|_| !fragment.is_empty()) else {
            return Ok(Vec::new());
        };
        let encoding = self.doc.text_encoding();
        let lead = match position.byte {
            0 => 0,
            byte => match self.doc.get(text, position.element) {
                Ok(Some((automerge::Value::Scalar(scalar), _))) => match scalar.as_ref() {
                    automerge::ScalarValue::Str(s) => {
                        text_width(encoding, s.get(..byte).ok_or(ReadError::Malformed)?)?
                    }
                    _ => return Err(ReadError::Malformed),
                },
                _ => return Err(ReadError::Malformed),
            },
        };
        let start = position.element.checked_add(lead).ok_or(ReadError::Limit)?;
        let mut starts = Vec::with_capacity(fragment.len().saturating_add(1));
        let mut end = start;
        let mut buffer = [0u8; 4];
        for c in fragment.chars() {
            starts.push(end);
            end = end
                .checked_add(text_width(encoding, c.encode_utf8(&mut buffer))?)
                .ok_or(ReadError::Limit)?;
        }
        starts.push(end);
        let offset = |index: usize| starts.partition_point(|at| *at < index);
        let mut marks = Vec::new();
        for mark in self.doc.marks(text).map_err(|_| ReadError::Malformed)? {
            work.visit()?;
            let (from, to) = (mark.start.max(start), mark.end.min(end));
            if from < to && !mark.value.is_null() {
                marks.push(crate::document::MarkSpan {
                    start: offset(from),
                    end: offset(to),
                    name: mark.name.to_string(),
                    value: (&mark.value).into(),
                });
            }
        }
        Ok(marks)
    }
}

/// How Automerge renders a text element that is not a string.
const OBJECT_REPLACEMENT: &str = "\u{fffc}";

/// Text-index width of one element under the document's encoding. A visible element is never
/// zero-width: the index seek skips such elements.
fn text_width(encoding: automerge::TextEncoding, value: &str) -> Result<usize, ReadError> {
    let width = match encoding {
        automerge::TextEncoding::UnicodeCodePoint => value.chars().count(),
        automerge::TextEncoding::Utf8CodeUnit => value.len(),
        automerge::TextEncoding::Utf16CodeUnit => value.encode_utf16().count(),
        automerge::TextEncoding::GraphemeCluster => return Err(ReadError::Malformed),
    };
    if width == 0 {
        return Err(ReadError::Malformed);
    }
    Ok(width)
}
