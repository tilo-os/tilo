//! A file's bytes as a field of the record: [`ByteBuffer`].
//!
//! The bytes are stored as a list of pages of a few KiB each. An edit
//! rewrites only the pages it touches: typing over one byte of a large file
//! writes one page, an insert grows one page (split when it gets long), and
//! collaborators editing different pages merge cleanly. Two people editing
//! the same page concurrently keep one of their versions of it.
//!
//! Its edits are field edits in its own vocabulary, a [`BufferEdit`]:
//! `HexEditor::paths().bytes.perform(&edit)`. They apply at the path and
//! read only the pages they touch, never the whole file.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValue, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ui::tilo_actions::Scalar;
use ui::tilo_resource::path::OpDescription;
use ui::tilo_resource::{
    self, ActionCx, Delta, Doc, EditOp, JsonSchema, Model, ModelError, Operations, Refusal,
    Segment, Slot,
};

use crate::model::{BufferEdit, common_suffix};

/// The bytes a page holds when a file is opened or a long page is split.
pub(crate) const PAGE: usize = 4096;
/// The longest a page grows by inserts before it is split.
pub(crate) const MAX_PAGE: usize = 2 * PAGE;

/// Bytes as hexadecimal text: how they travel in an edit, a verb's
/// arguments and history, and what a person or an agent writes
/// (`"de ad be ef"`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Hex(pub Vec<u8>);

impl fmt::Debug for Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl Hex {
    /// The bytes as lowercase hexadecimal digits, two per byte.
    #[must_use]
    pub fn text(&self) -> String {
        hex_text(&self.0)
    }
}

impl From<Vec<u8>> for Hex {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for Hex {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text())
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        crate::actions::parse_bytes(&text)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Hex {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "string",
            "description": "hexadecimal bytes, e.g. `de ad be ef`",
        })
    }
}

fn hex_text(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// A file's bytes, kept in pages. See the [module docs](self).
#[derive(Clone, Default)]
pub struct ByteBuffer {
    pages: Vec<Arc<[u8]>>,
}

impl ByteBuffer {
    /// `bytes` as a buffer of fresh pages.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            pages: paginate(bytes).into_iter().map(Arc::from).collect(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pages.iter().map(|page| page.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pages.iter().all(|page| page.is_empty())
    }

    /// Every byte, in order.
    pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        self.pages.iter().flat_map(|page| page.iter().copied())
    }

    /// The bytes as one contiguous copy.
    #[must_use]
    pub fn to_vec(&self) -> Vec<u8> {
        self.pages.concat()
    }

    /// How many pages hold the bytes.
    #[must_use]
    pub fn page_lengths(&self) -> Vec<usize> {
        self.pages.iter().map(|page| page.len()).collect()
    }

    /// Make `edit` to these bytes, as the edit at its path does to the
    /// stored pages: a verb's change, written back page by page.
    ///
    /// # Errors
    /// Refuses an offset beyond the end of the buffer.
    pub fn edit(&mut self, edit: &BufferEdit) -> Result<(), Refusal> {
        let lengths = self.page_lengths();
        let pages = &self.pages;
        let spliced = splice(&lengths, edit, |range| {
            Ok(pages
                .get(range)
                .unwrap_or_default()
                .iter()
                .map(|page| page.to_vec())
                .collect())
        })
        .map_err(|error| Refusal::new(error.to_string()))?;
        if let Some((range, new)) = spliced {
            self.pages
                .splice(range, new.into_iter().map(Arc::<[u8]>::from));
        }
        Ok(())
    }
}

impl PartialEq for ByteBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<T: AsRef<[u8]>> PartialEq<T> for ByteBuffer {
    fn eq(&self, other: &T) -> bool {
        let other = other.as_ref();
        self.len() == other.len() && self.iter().eq(other.iter().copied())
    }
}

impl fmt::Debug for ByteBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SHOWN: usize = 64;
        let head: Vec<u8> = self.iter().take(SHOWN).collect();
        write!(f, "ByteBuffer({} bytes: {}", self.len(), hex_text(&head))?;
        if self.len() > SHOWN {
            f.write_str("…")?;
        }
        f.write_str(")")
    }
}

fn paginate(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes.chunks(PAGE).map(<[u8]>::to_vec).collect()
}

fn beyond(offset: usize, len: usize) -> ModelError {
    ModelError::Refused(format!(
        "offset {offset} is beyond the end of the {len}-byte buffer"
    ))
}

/// The page holding `offset` and where in it; the end of the buffer is the
/// end of its last page.
fn locate(lengths: &[usize], offset: usize) -> Option<(usize, usize)> {
    let mut start = 0;
    for (index, length) in lengths.iter().enumerate() {
        if offset < start + length {
            return Some((index, offset - start));
        }
        start += length;
    }
    let (last, length) = lengths.iter().enumerate().next_back()?;
    (offset == start).then_some((last, *length))
}

/// The pages that hold any byte of `from..to`, and the offset the first of
/// them starts at.
fn pages_over(lengths: &[usize], from: usize, to: usize) -> (Range<usize>, usize) {
    let mut start = 0;
    let mut first = None;
    let mut first_start = 0;
    let mut last = 0;
    for (index, length) in lengths.iter().enumerate() {
        let end = start + length;
        if end > from && start < to {
            if first.is_none() {
                first = Some(index);
                first_start = start;
            }
            last = index + 1;
        }
        start = end;
    }
    match first {
        Some(first) => (first..last, first_start),
        None => (0..0, 0),
    }
}

/// The range of pages an edit replaces and the pages that take their place.
type Spliced = (Range<usize>, Vec<Vec<u8>>);

/// What `edit` does to pages of `lengths`: the range of pages it replaces
/// and the pages that take their place, or `None` when it changes nothing.
/// `read` copies the pages of a range, so only the touched ones are read.
fn splice(
    lengths: &[usize],
    edit: &BufferEdit,
    read: impl Fn(Range<usize>) -> Result<Vec<Vec<u8>>, ModelError>,
) -> Result<Option<Spliced>, ModelError> {
    let len: usize = lengths.iter().sum();
    match edit {
        BufferEdit::Insert { offset, bytes } => {
            let (offset, bytes) = (*offset, &bytes.0);
            if offset > len {
                return Err(beyond(offset, len));
            }
            if bytes.is_empty() {
                return Ok(None);
            }
            let Some((index, at)) = locate(lengths, offset) else {
                return Ok(Some((0..0, paginate(bytes))));
            };
            let mut page = read(index..index + 1)?.concat();
            page.splice(at..at, bytes.iter().copied());
            let new = if page.len() > MAX_PAGE {
                paginate(&page)
            } else {
                vec![page]
            };
            Ok(Some((index..index + 1, new)))
        }
        BufferEdit::Delete { offset, len: count } => {
            if *count == 0 {
                return Ok(None);
            }
            if *offset >= len {
                return Err(beyond(*offset, len));
            }
            let end = offset.saturating_add(*count).min(len);
            let (range, first_start) = pages_over(lengths, *offset, end);
            let mut new = read(range.clone())?;
            let mut start = first_start;
            for page in &mut new {
                let (from, to) = (start, start + page.len());
                start = to;
                let cut_from = (*offset).max(from);
                let cut_to = end.min(to);
                if cut_from < cut_to {
                    page.drain(cut_from - from..cut_to - from);
                }
            }
            new.retain(|page| !page.is_empty());
            Ok(Some((range, new)))
        }
        BufferEdit::Overwrite { offset, bytes } => {
            let bytes = &bytes.0;
            if bytes.is_empty() {
                return Ok(None);
            }
            if *offset >= len {
                return Err(beyond(*offset, len));
            }
            let end = offset.saturating_add(bytes.len()).min(len);
            let (range, first_start) = pages_over(lengths, *offset, end);
            let mut new = read(range.clone())?;
            let mut start = first_start;
            for page in &mut new {
                let page_start = start;
                start += page.len();
                for (index, slot) in page.iter_mut().enumerate() {
                    if let Some(byte) = (page_start + index)
                        .checked_sub(*offset)
                        .and_then(|at| bytes.get(at))
                    {
                        *slot = *byte;
                    }
                }
            }
            Ok(Some((range, new)))
        }
    }
}

/// The page at `index` of the stored list, borrowed.
fn with_page<T>(
    doc: &Doc,
    list: &ObjId,
    index: usize,
    read: impl FnOnce(&[u8]) -> T,
) -> Result<T, ModelError> {
    match doc.get(list, index)? {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Bytes(page) => Ok(read(page)),
            _ => Err(ModelError::shape("bytes").at(&index.to_string())),
        },
        _ => Err(ModelError::shape("bytes").at(&index.to_string())),
    }
}

/// How long each stored page is, read without copying any of them.
fn stored_lengths(doc: &Doc, list: &ObjId) -> Result<Vec<usize>, ModelError> {
    (0..doc.length(list))
        .map(|index| with_page(doc, list, index, <[u8]>::len))
        .collect()
}

/// Write `new` over the stored pages `range`, touching only the pages that
/// differ.
fn write_pages(
    doc: &mut Doc,
    list: &ObjId,
    range: Range<usize>,
    new: &[Vec<u8>],
) -> Result<(), ModelError> {
    let base = range.start;
    let old = (range)
        .map(|index| with_page(doc, list, index, <[u8]>::to_vec))
        .collect::<Result<Vec<_>, _>>()?;
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let old_rest = old.get(prefix..).unwrap_or_default();
    let new_rest = new.get(prefix..).unwrap_or_default();
    let suffix = common_suffix(old_rest, new_rest);
    let old_middle = old_rest.get(..old_rest.len() - suffix).unwrap_or_default();
    let new_middle = new_rest.get(..new_rest.len() - suffix).unwrap_or_default();
    let start = base + prefix;
    for (index, (was, page)) in old_middle.iter().zip(new_middle).enumerate() {
        if was != page {
            doc.put(list, start + index, ScalarValue::Bytes(page.clone()))?;
        }
    }
    let common = old_middle.len().min(new_middle.len());
    for _ in common..old_middle.len() {
        doc.delete(list, start + common)?;
    }
    for (index, page) in new_middle.iter().enumerate().skip(common) {
        doc.insert(list, start + index, ScalarValue::Bytes(page.clone()))?;
    }
    Ok(())
}

/// How many leading and trailing stored pages equal `pages`' own, compared
/// in place.
fn shared_ends(doc: &Doc, list: &ObjId, pages: &[Arc<[u8]>]) -> Result<(usize, usize), ModelError> {
    let stored = doc.length(list);
    let mut prefix = 0;
    while prefix < stored.min(pages.len())
        && with_page(doc, list, prefix, |page| {
            Some(page) == pages.get(prefix).map(|p| &**p)
        })?
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < (stored - prefix).min(pages.len() - prefix)
        && with_page(doc, list, stored - 1 - suffix, |page| {
            Some(page) == pages.get(pages.len() - 1 - suffix).map(|p| &**p)
        })?
    {
        suffix += 1;
    }
    Ok((prefix, suffix))
}

impl Operations for ByteBuffer {
    type Op = BufferEdit;

    /// The next nibble or byte typed, the next byte deleted.
    fn coalesces(previous: &BufferEdit, next: &BufferEdit) -> bool {
        previous.continues(next)
    }

    fn describe(op: &BufferEdit) -> OpDescription {
        let (kind, offset, count) = match op {
            BufferEdit::Insert { offset, bytes } => (EditOpKind::Insert, offset, bytes.0.len()),
            BufferEdit::Delete { offset, len } => (EditOpKind::Remove, offset, *len),
            BufferEdit::Overwrite { offset, bytes } => (EditOpKind::Set, offset, bytes.0.len()),
        };
        let number = |value: usize| Scalar::from_json(&serde_json::Value::from(value));
        OpDescription {
            op: kind,
            args: [("offset", number(*offset)), ("count", number(count))]
                .into_iter()
                .filter_map(|(name, value)| Some((name.to_owned(), value?)))
                .collect(),
        }
    }
}

use ui::tilo_actions::EditOp as EditOpKind;

impl Model for ByteBuffer {
    tilo_resource::leaf_paths!();
    tilo_resource::operations_model!();

    /// The bytes as hexadecimal text.
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(hex_text(&self.to_vec()))
    }

    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        match value {
            serde_json::Value::String(text) => crate::actions::parse_bytes(text)
                .map(|bytes| Self::of(&bytes))
                .map_err(ModelError::invalid),
            serde_json::Value::Null => Ok(Self::default()),
            _ => Err(ModelError::shape("hexadecimal bytes")),
        }
    }

    fn diff<R>(&self, after: &Self, path: &[Segment], out: &mut Vec<tilo_resource::Edit<R>>) {
        if self != after {
            out.push(tilo_resource::Edit::new(
                path.to_vec(),
                EditOp::Set {
                    value: after.to_json(),
                },
            ));
        }
    }

    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        if !matches!(value, Value::Object(ObjType::List)) {
            return Err(ModelError::shape("pages of bytes"));
        }
        let pages = (0..doc.length(id))
            .map(|index| with_page(doc, id, index, |page| Arc::<[u8]>::from(page)))
            .collect::<Result<_, _>>()?;
        Ok(Self { pages })
    }

    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }

    /// Write the pages that differ from the stored ones: a verb that
    /// changed one page writes that page.
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let list = slot.object(doc, ObjType::List)?;
        let (prefix, suffix) = shared_ends(doc, &list, &self.pages)?;
        let stored = doc.length(&list);
        let new: Vec<Vec<u8>> = self
            .pages
            .get(prefix..self.pages.len() - suffix)
            .unwrap_or_default()
            .iter()
            .map(|page| page.to_vec())
            .collect();
        write_pages(doc, &list, prefix..stored - suffix, &new)
    }

    /// Re-read the pages that changed, keeping the ones that did not.
    fn refresh(
        &mut self,
        doc: &Doc,
        value: &Value<'_>,
        id: &ObjId,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        if !matches!(value, Value::Object(ObjType::List)) {
            return Err(ModelError::shape("pages of bytes"));
        }
        if !delta.is_everything() && !delta.touches(id) {
            return Ok(());
        }
        let (prefix, suffix) = shared_ends(doc, id, &self.pages)?;
        let stored = doc.length(id);
        let middle = (prefix..stored - suffix)
            .map(|index| with_page(doc, id, index, |page| Arc::<[u8]>::from(page)))
            .collect::<Result<Vec<_>, _>>()?;
        let keep = self.pages.len() - suffix;
        self.pages.splice(prefix..keep, middle);
        Ok(())
    }

    /// Make the edit to the stored pages it touches, reading no others.
    fn edit(
        doc: &mut Doc,
        slot: Slot<'_>,
        path: &[Segment],
        op: &EditOp,
        _cx: &mut ActionCx,
    ) -> Result<(), ModelError> {
        if let Some(segment) = path.first() {
            return Err(ModelError::invalid(format!(
                "nothing inside the bytes is called `{segment}`"
            )));
        }
        let edit = match op {
            EditOp::Rich { rich } => serde_json::from_value::<BufferEdit>(rich.clone())
                .map_err(|error| ModelError::invalid(error.to_string()))?,
            EditOp::Set { value } => return Self::from_json(value)?.store(doc, slot),
            other => return Err(ModelError::unsupported(other)),
        };
        let list = slot.object(doc, ObjType::List)?;
        let lengths = stored_lengths(doc, &list)?;
        let spliced = splice(&lengths, &edit, |range| {
            range
                .map(|index| with_page(doc, &list, index, <[u8]>::to_vec))
                .collect()
        })?;
        match spliced {
            Some((range, new)) => write_pages(doc, &list, range, &new),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_of_a_value_match_the_edits_at_its_path() {
        let file: Vec<u8> = (0..2 * PAGE + 7).map(|i| (i % 251) as u8).collect();
        let mut buffer = ByteBuffer::of(&file);
        let mut expected = file;
        buffer
            .edit(&BufferEdit::Insert {
                offset: PAGE + 2,
                bytes: Hex(vec![1, 2]),
            })
            .unwrap();
        expected.splice(PAGE + 2..PAGE + 2, [1, 2]);
        buffer
            .edit(&BufferEdit::Delete {
                offset: 1,
                len: PAGE,
            })
            .unwrap();
        expected.drain(1..=PAGE);
        buffer
            .edit(&BufferEdit::Overwrite {
                offset: 0,
                bytes: Hex(vec![9]),
            })
            .unwrap();
        expected[0] = 9;
        assert_eq!(buffer, expected);
        assert!(
            buffer
                .edit(&BufferEdit::Overwrite {
                    offset: expected.len(),
                    bytes: Hex(vec![1]),
                })
                .is_err()
        );
    }

    #[test]
    fn bytes_travel_as_hexadecimal_text() {
        let edit = BufferEdit::Overwrite {
            offset: 2,
            bytes: Hex(vec![0xde, 0xad]),
        };
        let json = serde_json::to_value(&edit).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"Overwrite": {"offset": 2, "bytes": "dead"}})
        );
        let parsed: BufferEdit = serde_json::from_value(
            serde_json::json!({"Insert": {"offset": 0, "bytes": "DE-AD be ef"}}),
        )
        .unwrap();
        assert_eq!(
            parsed,
            BufferEdit::Insert {
                offset: 0,
                bytes: Hex(vec![0xde, 0xad, 0xbe, 0xef])
            }
        );
    }
}
