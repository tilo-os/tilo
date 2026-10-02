//! Immutable body-text pages over the actual qdoc layout. Not a public action or authority witness.
pub mod action;
pub mod changes;
pub mod markdown;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_doc::semantic_read::{Basis, Limits, ReadError, TextPosition};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    basis: [u8; 32],
    scope: [u8; 32],
    block: String,
    order: Vec<u8>,
    position: TextPosition,
    finished: bool,
}
#[derive(Debug, Serialize)]
pub struct Segment {
    pub block: String,
    pub kind: String,
    pub text: String,
    pub complete: bool,
    #[serde(flatten)]
    pub shape: Shape,
    /// The inline marks over `text`, in its characters; Markdown renders them.
    #[serde(skip)]
    pub marks: Vec<tilo_doc::document::MarkSpan>,
}
/// Where a block sits in the document's structure: a heading's or list
/// item's level, a list's numbering, a cell's table, row and column.
#[derive(Debug, Default, Serialize)]
pub struct Shape {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ordered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nesting: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<i64>,
}
impl Shape {
    /// The shape `attrs`, a materialized block's attributes, give it.
    pub fn of_attrs(attrs: &std::collections::BTreeMap<String, tilo_doc::AttrVal>) -> Self {
        let int = |key: &str| match attrs.get(key) {
            Some(tilo_doc::AttrVal::Int(value)) => Some(*value),
            _ => None,
        };
        Self {
            level: int("level"),
            ordered: match attrs.get("ordered") {
                Some(tilo_doc::AttrVal::Bool(flag)) => Some(*flag),
                _ => None,
            },
            nesting: int("nesting"),
            table: match attrs.get("table") {
                Some(tilo_doc::AttrVal::Str(table)) => Some(table.clone()),
                _ => None,
            },
            row: int("row"),
            col: int("col"),
        }
    }
}
fn shape(source: &Source<'_>, block: &ObjId) -> Result<Shape, ReadError> {
    let mut shape = Shape::default();
    let Some(attrs) = source.block_attrs(block)? else {
        return Ok(shape);
    };
    let int = |key: &str| {
        let mut found = None;
        source.field(&attrs, key, |value| {
            if let source::Value::Scalar(scalar) = value {
                found = match scalar {
                    automerge::ScalarValueRef::Int(n) => Some(*n),
                    automerge::ScalarValueRef::Uint(n) => i64::try_from(*n).ok(),
                    _ => None,
                };
            }
            Ok(())
        })?;
        Ok::<_, ReadError>(found)
    };
    shape.level = int("level")?;
    shape.nesting = int("nesting")?;
    shape.row = int("row")?;
    shape.col = int("col")?;
    source.field(&attrs, "ordered", |value| {
        if let source::Value::Scalar(automerge::ScalarValueRef::Boolean(flag)) = value {
            shape.ordered = Some(*flag);
        }
        Ok(())
    })?;
    let table = source.string(&attrs, "table", 128)?;
    shape.table = (!table.is_empty()).then_some(table);
    Ok(shape)
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub basis: Basis,
    pub segments: Vec<Segment>,
    pub next: Option<Cursor>,
}
/// The SDK's refusal for a projection error.
pub(crate) fn read_error(error: &ReadError) -> ui::semantic_document_read::SemanticReadError {
    use ui::semantic_document_read::SemanticReadError;
    match error {
        ReadError::Pending => SemanticReadError::Pending,
        ReadError::StaleCursor => SemanticReadError::StaleCursor,
        ReadError::InvalidCursor => SemanticReadError::InvalidRequest,
        ReadError::Limit => SemanticReadError::Limit,
        ReadError::Malformed => SemanticReadError::Malformed,
    }
}
use automerge::{ObjId, ObjType, ROOT};
use std::{cmp::Ordering, ops::ControlFlow};
use tilo_doc::semantic_read::{
    json_len,
    source::{self, Source},
};
struct Candidate {
    id: String,
    order: Vec<u8>,
    object: ObjId,
}
fn compare(order: &[u8], id: &str, other_order: &[u8], other_id: &str) -> Ordering {
    tilo_resource::order::bytes::cmp(order, other_order).then(id.cmp(other_id))
}
/// Root-record convenience projection; the owner still supplies/stamps the canonical record route.
///
/// # Errors
///
/// Returns a [`ReadError`] when the document has pending operations, the cursor
/// is stale or invalid for this basis, or the page exceeds its limits.
pub fn read_text(
    doc: &AutoCommit,
    owner_heads: &[automerge::ChangeHash],
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    read_text_at(doc, owner_heads, &ROOT, cursor, limits)
}
/// One order scan retains only the next requested number of blocks plus one lookahead.
/// Invalid order rows are skipped exactly as `tilo_doc::model::blocks_in_order` does; valid oversized
/// metadata or exhausted scan work refuses the page rather than silently dropping content.
///
/// # Errors
///
/// Returns a [`ReadError`] when the document has pending operations, the cursor
/// is stale or invalid for this basis, or the page exceeds its limits.
pub fn read_text_at(
    doc: &AutoCommit,
    owner_heads: &[automerge::ChangeHash],
    root: &ObjId,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    read_source(&Source::new(doc, owner_heads), root, cursor, limits)
}
#[expect(
    clippy::too_many_lines,
    reason = "one bounded page scan; cursor, lookahead and limit accounting share its state"
)]
fn read_source(
    source: &Source<'_>,
    root: &ObjId,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    let (basis, work) = source.begin(limits, 64)?;
    let scope = source.fingerprint(root)?;
    if let Some(cursor) = cursor {
        if cursor.scope != scope {
            return Err(ReadError::InvalidCursor);
        }
        if cursor.finished && cursor.position != TextPosition::default() {
            return Err(ReadError::InvalidCursor);
        }
        if cursor.basis != basis.fingerprint {
            return Err(ReadError::StaleCursor);
        }
        if cursor.block.len() > 256 || cursor.order.len() > 256 {
            return Err(ReadError::InvalidCursor);
        }
        json_len(cursor, 1024).map_err(|_| ReadError::InvalidCursor)?;
    }
    let blocks = source
        .object(root, tilo_doc::schema::KEY_BLOCKS, ObjType::Map)?
        .ok_or(ReadError::Malformed)?;
    for key in [
        tilo_doc::schema::KEY_STYLES,
        tilo_doc::schema::KEY_PRESERVED,
        tilo_doc::schema::KEY_META,
    ] {
        source
            .object(root, key, ObjType::Map)?
            .ok_or(ReadError::Malformed)?;
    }
    let capacity = limits.items + 1;
    let mut candidates: Vec<Candidate> = Vec::with_capacity(capacity);
    let mut cursor_found = cursor.is_none();
    source.map(&blocks, &work, |key, value| {
        let Some(block) = value.object(ObjType::Map) else {
            return Ok(ControlFlow::Continue(()));
        };
        source.bytes(&block, tilo_doc::schema::BLOCK_ORDER, |order| {
            if order.len() > 256 || key.len() > 256 {
                return Err(ReadError::Limit);
            }
            if let Some(cursor) = cursor {
                if key == cursor.block {
                    if order != cursor.order {
                        return Err(ReadError::InvalidCursor);
                    }
                    cursor_found = true;
                }
                let cmp = compare(order, key, &cursor.order, &cursor.block);
                if cmp.is_lt() || (cmp.is_eq() && cursor.finished) {
                    return Ok(());
                }
            }
            let at = candidates
                .binary_search_by(|c| compare(&c.order, &c.id, order, key))
                .unwrap_or_else(|i| i);
            if at >= capacity {
                return Ok(());
            }
            if candidates.len() == capacity {
                candidates.pop();
            }
            candidates.insert(
                at,
                Candidate {
                    id: key.to_owned(),
                    order: order.to_owned(),
                    object: block.clone(),
                },
            );
            Ok(())
        })?;
        Ok(ControlFlow::Continue(()))
    })?;
    if !cursor_found {
        return Err(ReadError::InvalidCursor);
    }
    let mut page = Page {
        basis,
        segments: Vec::with_capacity(limits.items),
        next: None,
    };
    for (index, candidate) in candidates.iter().take(limits.items).enumerate() {
        let position = cursor
            .filter(|c| c.block == candidate.id && !c.finished)
            .map(|c| c.position)
            .unwrap_or_default();
        let text_object = source.object(
            &candidate.object,
            tilo_doc::schema::BLOCK_TEXT,
            ObjType::Text,
        )?;
        let previous = page.next.take();
        page.segments.push(Segment {
            block: candidate.id.clone(),
            kind: source.string(&candidate.object, tilo_doc::schema::BLOCK_KIND, 64)?,
            text: String::new(),
            complete: false,
            shape: shape(source, &candidate.object)?,
            marks: Vec::new(),
        });
        let mut continuation = Cursor {
            basis: page.basis.fingerprint,
            scope,
            block: candidate.id.clone(),
            order: candidate.order.clone(),
            position: TextPosition {
                element: usize::MAX,
                byte: usize::MAX,
            },
            finished: false,
        };
        json_len(&continuation, 1024)?;
        page.next = Some(continuation.clone());
        let envelope = json_len(&page, limits.json_bytes);
        let available = envelope
            .ok()
            .and_then(|n| limits.json_bytes.checked_sub(n))
            .filter(|n| *n >= 6);
        let Some(available) = available else {
            page.segments.pop();
            page.next = previous;
            if page.segments.is_empty() {
                return Err(ReadError::Limit);
            }
            break;
        };
        let (text, next, complete) =
            source.text_fragment(text_object.as_ref(), position, available, &work)?;
        let segment = page.segments.last_mut().ok_or(ReadError::Malformed)?;
        segment.marks = source.fragment_marks(text_object.as_ref(), position, &text, &work)?;
        segment.text = text;
        segment.complete = complete;
        continuation.position = if complete {
            TextPosition::default()
        } else {
            next
        };
        continuation.finished = complete;
        page.next = if complete && index + 1 == candidates.len() {
            None
        } else {
            Some(continuation)
        };
        if !complete {
            break;
        }
    }
    json_len(&page, limits.json_bytes)?;
    Ok(page)
}
#[cfg(test)]
mod tests;
