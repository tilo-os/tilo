//! Immutable message pages over the chat record's storage: a channel's messages in the
//! order of the record's message collection. No action publication or caller authority is
//! minted here.
pub mod action;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_doc::semantic_read::{Basis, Limits, ReadError, TextPosition};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    basis: [u8; 32],
    channel: String,
    index: usize,
    position: TextPosition,
    finished: bool,
}
#[derive(Debug, Serialize)]
pub struct Message {
    pub id: String,
    pub author: String,
    pub ts: i64,
    pub parent: String,
    pub edited: bool,
    pub text: String,
    pub complete: bool,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub basis: Basis,
    pub channel: String,
    pub messages: Vec<Message>,
    pub next: Option<Cursor>,
}
use automerge::{ObjType, ScalarValueRef};
use std::ops::ControlFlow;
use tilo_doc::semantic_read::{
    json_len,
    source::{Source, Value},
};
/// Read only the selected channel's canonical message list. A page retains at most the requested
/// messages plus one lookahead; text is streamed element by element.
///
/// # Errors
///
/// Returns a [`ReadError`] when the document has pending operations, the cursor
/// is stale or invalid for this channel, or the page exceeds its limits.
pub fn read_messages(
    doc: &AutoCommit,
    owner_heads: &[automerge::ChangeHash],
    channel: &str,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    read_source(&Source::new(doc, owner_heads), channel, cursor, limits)
}
#[expect(
    clippy::too_many_lines,
    reason = "one bounded page scan; cursor, lookahead and limit accounting share its state"
)]
fn read_source(
    source: &Source<'_>,
    channel: &str,
    cursor: Option<&Cursor>,
    limits: Limits,
) -> Result<Page, ReadError> {
    let (basis, work) = source.begin(limits, 32)?;
    if channel.is_empty() || channel.len() > 128 {
        return Err(ReadError::Limit);
    }
    if let Some(cursor) = cursor {
        if cursor.finished && cursor.position != TextPosition::default() {
            return Err(ReadError::InvalidCursor);
        }
        if cursor.channel != channel {
            return Err(ReadError::InvalidCursor);
        }
        if cursor.basis != basis.fingerprint {
            return Err(ReadError::StaleCursor);
        }
        json_len(cursor, 1024).map_err(|_| ReadError::InvalidCursor)?;
    }
    let channels = source
        .object(&source.root(), "channels", ObjType::Map)?
        .ok_or(ReadError::Malformed)?;
    source
        .object(&channels, channel, ObjType::Map)?
        .ok_or(ReadError::Malformed)?;
    // The record keeps every channel's messages in one collection, each
    // item `{ k: order key, v: message }` under its id: this channel's, in
    // collection order.
    let mut rows = Vec::new();
    if let Some(messages) = source.object(&source.root(), "messages", ObjType::Map)? {
        let mut items = Vec::new();
        source.map(&messages, &work, |id, value| {
            if let Some(item) = value.object(ObjType::Map) {
                items.push((id.to_owned(), item));
            }
            Ok(ControlFlow::Continue(()))
        })?;
        for (id, item) in items {
            let Some(message) = source.object(&item, "v", ObjType::Map)? else {
                continue;
            };
            if source.string(&message, "channel", 128)? != channel {
                continue;
            }
            rows.push((source.string(&item, "k", 256)?, id, message));
        }
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut candidates = Vec::with_capacity(limits.items + 1);
    let mut cursor_found = cursor.is_none();
    for (index, (_, id, message)) in rows.into_iter().enumerate() {
        if let Some(cursor) = cursor {
            if index == cursor.index {
                cursor_found = true;
            }
            if index < cursor.index || (index == cursor.index && cursor.finished) {
                continue;
            }
        }
        candidates.push((index, (id, message)));
        if candidates.len() == limits.items + 1 {
            break;
        }
    }
    if !cursor_found {
        return Err(ReadError::InvalidCursor);
    }
    let mut page = Page {
        basis,
        channel: channel.to_owned(),
        messages: Vec::with_capacity(limits.items),
        next: None,
    };
    for (index, (row, (id, message))) in candidates.iter().take(limits.items).enumerate() {
        let position = cursor
            .filter(|c| c.index == *row && !c.finished)
            .map(|c| c.position)
            .unwrap_or_default();
        let text = source.object(message, "text", ObjType::Text)?;
        let previous = page.next.take();
        let mut ts = 0;
        source.field(message, "ts", |value| {
            ts = match value {
                Value::Scalar(
                    ScalarValueRef::Int(ts)
                    | ScalarValueRef::Counter(ts)
                    | ScalarValueRef::Timestamp(ts),
                ) => *ts,
                Value::Scalar(ScalarValueRef::Uint(ts)) => *ts as i64,
                Value::Scalar(ScalarValueRef::F64(ts)) => *ts as i64,
                _ => 0,
            };
            Ok(())
        })?;
        let mut edited = false;
        source.field(message, "edited", |value| {
            edited = matches!(value, Value::Scalar(ScalarValueRef::Boolean(true)));
            Ok(())
        })?;
        page.messages.push(Message {
            id: id.clone(),
            author: source.string(message, "author", 256)?,
            ts,
            parent: source.string(message, "parent", 128)?,
            edited,
            text: String::new(),
            complete: false,
        });
        let mut continuation = Cursor {
            basis: page.basis.fingerprint,
            channel: channel.to_owned(),
            index: *row,
            position: TextPosition {
                element: usize::MAX,
                byte: usize::MAX,
            },
            finished: false,
        };
        json_len(&continuation, 1024)?;
        page.next = Some(continuation.clone());
        let available = json_len(&page, limits.json_bytes)
            .ok()
            .and_then(|n| limits.json_bytes.checked_sub(n))
            .filter(|n| *n >= 6);
        let Some(available) = available else {
            page.messages.pop();
            page.next = previous;
            if page.messages.is_empty() {
                return Err(ReadError::Limit);
            }
            break;
        };
        let (text, next, complete) =
            source.text_fragment(text.as_ref(), position, available, &work)?;
        let message = page.messages.last_mut().ok_or(ReadError::Malformed)?;
        message.text = text;
        message.complete = complete;
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
