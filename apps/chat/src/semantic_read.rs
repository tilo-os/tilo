//! Immutable message pages over `ChatDoc` storage. No action publication or caller authority is minted here.
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
    let list = match source.object(&source.root(), "messages", ObjType::Map)? {
        Some(messages) => source.object(&messages, channel, ObjType::List)?,
        None => None,
    };
    let mut candidates = Vec::with_capacity(limits.items + 1);
    let mut cursor_found = cursor.is_none();
    if let Some(list) = list {
        source.list(&list, &work, |item| {
            let object = item.value.object(ObjType::Map);
            if let Some(cursor) = cursor {
                if item.index == cursor.index {
                    cursor_found = object.is_some();
                }
                if item.index < cursor.index || (item.index == cursor.index && cursor.finished) {
                    return Ok(ControlFlow::Continue(()));
                }
            }
            if let Some(object) = object {
                candidates.push((item.index, object));
            }
            Ok(if candidates.len() == limits.items + 1 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            })
        })?;
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
    for (index, (row, message)) in candidates.iter().take(limits.items).enumerate() {
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
            id: source.string(message, "id", 128)?,
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
