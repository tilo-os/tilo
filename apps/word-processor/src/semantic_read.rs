//! Immutable body-text pages over the actual qdoc layout. Not a public action or authority witness.
pub mod action;
mod native;
use automerge::AutoCommit;
pub use native::{NativePage, read_text_native};
use qdoc::semantic_read::{Basis, Limits, ReadError, TextPosition};
use serde::{Deserialize, Serialize};
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
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub basis: Basis,
    pub segments: Vec<Segment>,
    pub next: Option<Cursor>,
}
use automerge::{ObjId, ObjType, ROOT};
use qdoc::semantic_read::{
    json_len,
    source::{Object, Source},
};
use std::{cmp::Ordering, ops::ControlFlow};
struct Candidate<'doc> {
    id: String,
    order: Vec<u8>,
    object: Object<'doc>,
}
fn compare(order: &[u8], id: &str, other_order: &[u8], other_id: &str) -> Ordering {
    qdoc::order::cmp_keys(order, other_order).then(id.cmp(other_id))
}
/// Root-record convenience projection; the owner still supplies/stamps the canonical record route.
pub fn read_text(
    doc: &AutoCommit,
    owner_heads: &[automerge::ChangeHash],
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    read_text_at(doc, owner_heads, &ROOT, cursor, limits)
}
/// One order scan retains only the next requested number of blocks plus one lookahead.
/// Invalid order rows are skipped exactly as qdoc::model::blocks_in_order does; valid oversized
/// metadata or exhausted scan work refuses the page rather than silently dropping content.
pub fn read_text_at(
    doc: &AutoCommit,
    owner_heads: &[automerge::ChangeHash],
    root: &ObjId,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    let source = Source::legacy(doc, owner_heads);
    let root = source.legacy_object(root)?;
    read_source(&source, &root, cursor, limits)
}
fn read_source<'doc>(
    source: &Source<'_, 'doc, '_>,
    root: &Object<'doc>,
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
        .object(root, qdoc::schema::KEY_BLOCKS, ObjType::Map, &work)?
        .ok_or(ReadError::Malformed)?;
    for key in [
        qdoc::schema::KEY_STYLES,
        qdoc::schema::KEY_PRESERVED,
        qdoc::schema::KEY_META,
    ] {
        source
            .object(root, key, ObjType::Map, &work)?
            .ok_or(ReadError::Malformed)?;
    }
    let capacity = limits.items + 1;
    let mut candidates: Vec<Candidate> = Vec::with_capacity(capacity);
    let mut cursor_found = cursor.is_none();
    source.map(&blocks, &work, |key, value| {
        let Some(block) = value.object(ObjType::Map) else {
            return Ok(ControlFlow::Continue(()));
        };
        source.bytes(&block, qdoc::schema::BLOCK_ORDER, &work, |order| {
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
        let text = source.object(
            &candidate.object,
            qdoc::schema::BLOCK_TEXT,
            ObjType::Text,
            &work,
        )?;
        let previous = page.next.take();
        page.segments.push(Segment {
            block: candidate.id.clone(),
            kind: source.string(&candidate.object, qdoc::schema::BLOCK_KIND, 64, &work)?,
            text: String::new(),
            complete: false,
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
            source.text_fragment(text.as_ref(), position, available, &work)?;
        let segment = page.segments.last_mut().ok_or(ReadError::Malformed)?;
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
