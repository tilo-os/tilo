//! A thought a provider writes into the content of the message it belongs
//! to.
//!
//! Google's OpenAI-compatible endpoint returns a thought summary, where one
//! is asked for, inside the assistant message's own `content`, and its
//! documentation names no form for it. These forms are read here, alone or
//! together: a delta or message its provider marks with
//! `extra_content.google.thought`, a content part marked `thought`, and, at
//! Google's endpoint only, text between `<thought>` and `</thought>` at the
//! head of a content. Each is the message's thought and never its answer.

use serde_json::Value;

/// The tag that opens a thought at the head of a content.
const OPEN: &str = "<thought>";
/// The tag that closes it.
const CLOSE: &str = "</thought>";

/// Whether its provider marks `delta`, or a whole message, as a thought.
pub(super) fn marked(delta: &Value) -> bool {
    delta.pointer("/extra_content/google/thought") == Some(&Value::Bool(true))
}

/// Whether its provider marks the content part `part` as a thought, as
/// Google's own interface marks one.
pub(super) fn part(part: &Value) -> bool {
    part.get("thought") == Some(&Value::Bool(true))
}

/// A whole content split at the thought at its head: the thought, and the
/// answer after it. A content that begins with no thought is all answer; a
/// closing tag at its head, left by a thought that came marked, is dropped.
pub(super) fn split(content: &str) -> (Option<&str>, &str) {
    let seen = content.trim_start();
    if let Some(inside) = seen.strip_prefix(OPEN) {
        return match inside.find(CLOSE) {
            Some(end) => (
                inside.get(..end),
                inside
                    .get(end.saturating_add(CLOSE.len())..)
                    .unwrap_or_default(),
            ),
            None => (Some(inside), ""),
        };
    }
    if let Some(after) = seen.strip_prefix(CLOSE) {
        return (None, after);
    }
    (None, content)
}

/// Where a content stands against the thought at its head.
#[derive(Default, PartialEq, Eq)]
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
                        let after = self.held.split_off(end.saturating_add(CLOSE.len()));
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
                        let tail = self.held.split_off(self.held.len().saturating_sub(tail));
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
