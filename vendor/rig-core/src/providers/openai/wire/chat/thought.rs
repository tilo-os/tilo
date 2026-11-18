//! A thought a provider writes into the content of the message it belongs
//! to.
//!
//! Google's OpenAI-compatible endpoint returns a thought summary, where one
//! is asked for, inside the assistant message's own `content`, and its
//! documentation names no form for it. These forms are read here, alone or
//! together: a delta its provider marks with `extra_content.google.thought`,
//! a content part marked `thought`, and text between `<thought>` and
//! `</thought>` at the head of a content. Each is the message's reasoning
//! and never its answer text.

use serde_json::{Map, Value};

/// The tag that opens a thought at the head of a content.
const OPEN: &str = "<thought>";
/// The tag that closes it.
const CLOSE: &str = "</thought>";

/// Whether its provider marks `delta`, or a whole message, as a thought.
pub(super) fn marked(delta: &Map<String, Value>) -> bool {
    delta
        .get("extra_content")
        .and_then(|extra| extra.pointer("/google/thought"))
        == Some(&Value::Bool(true))
}

/// Whether its provider marks the content part `part` as a thought, as
/// Google's own interface marks one.
pub(super) fn part(part: &Value) -> bool {
    part.get("thought") == Some(&Value::Bool(true))
}

/// A marked delta's `extra_content` without the mark: what else it carries,
/// or `None` when the mark was all of it.
pub(super) fn unmarked(extra: &Value) -> Option<Value> {
    let mut extra = extra.as_object()?.clone();
    let emptied = match extra.get_mut("google") {
        Some(Value::Object(google)) => {
            google.remove("thought");
            google.is_empty()
        }
        _ => false,
    };
    if emptied {
        extra.remove("google");
    }
    (!extra.is_empty()).then_some(Value::Object(extra))
}

/// Take the thought at the head of a whole message's `content` out of it,
/// and a closing tag left at its head by a thought that came marked.
pub(super) fn strip(message: &mut Map<String, Value>) {
    let Some(Value::String(content)) = message.get_mut("content") else {
        return;
    };
    let seen = content.trim_start();
    let answer = if let Some(inside) = seen.strip_prefix(OPEN) {
        inside
            .find(CLOSE)
            .and_then(|end| inside.get(end + CLOSE.len()..))
            .unwrap_or_default()
    } else if let Some(after) = seen.strip_prefix(CLOSE) {
        after
    } else {
        return;
    };
    *content = answer.to_owned();
}

/// Where a content stands against the thought at its head.
#[derive(Default, PartialEq)]
enum At {
    /// Nothing has come but what may still become the opening tag.
    #[default]
    Head,
    /// Inside the thought.
    Thought,
    /// Past the thought, or in a content that began with none.
    Answer,
}

/// Reads the thought at the head of one message's content apart from the
/// answer after it, in whatever pieces the content arrives.
#[derive(Default)]
pub(super) struct Thoughts {
    at: At,
    /// What came and may still be part of a tag.
    held: String,
}

impl Thoughts {
    /// Take the content's next `text`. What is known now comes back in
    /// order: each piece, and whether it is the thought's.
    pub(super) fn take(&mut self, text: &str) -> Vec<(bool, String)> {
        let mut known = Vec::new();
        self.held.push_str(text);
        loop {
            match self.at {
                At::Head => {
                    let seen = self.held.trim_start();
                    if let Some(inside) = seen.strip_prefix(OPEN) {
                        self.held = inside.to_owned();
                        self.at = At::Thought;
                    } else if OPEN.starts_with(seen) {
                        break;
                    } else {
                        self.at = At::Answer;
                    }
                }
                At::Thought => {
                    if let Some(end) = self.held.find(CLOSE) {
                        let after = self.held.split_off(end + CLOSE.len());
                        self.held.truncate(end);
                        known.push((true, std::mem::replace(&mut self.held, after)));
                        self.at = At::Answer;
                    } else {
                        // The longest tail that may be the closing tag's
                        // beginning stays held.
                        let tail = (1..CLOSE.len())
                            .rev()
                            .find(|&length| {
                                CLOSE
                                    .get(..length)
                                    .is_some_and(|begun| self.held.ends_with(begun))
                            })
                            .unwrap_or(0);
                        let tail = self.held.split_off(self.held.len() - tail);
                        known.push((true, std::mem::replace(&mut self.held, tail)));
                        break;
                    }
                }
                At::Answer => {
                    known.push((false, std::mem::take(&mut self.held)));
                    break;
                }
            }
        }
        known.retain(|(_, piece)| !piece.is_empty());
        known
    }

    /// The content ended: what was still held, and whether it is the
    /// thought's.
    pub(super) fn end(&mut self) -> Option<(bool, String)> {
        let held = std::mem::take(&mut self.held);
        (!held.is_empty()).then(|| (self.at == At::Thought, held))
    }
}
