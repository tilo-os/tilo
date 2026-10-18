//! Structured `@mention`s: a reference to a room participant, not styled text.
//!
//! A mention is **not** a string that happens to start with `@`. It is a
//! resolved pointer to one participant of the room, minted when the message is
//! authored and stored in the shared CRDT next to the body (see
//! [`crate::record`]). That is what lets anything else — the person mentioned, an
//! agent watching the room, a notification badge, a search index — ask a
//! message *who it mentions* without re-parsing prose and re-guessing which
//! "@ada" meant which Ada.
//!
//! ## The shape on the wire
//!
//! Each message carries an ordered list of [`Mention`]:
//!
//! ```text
//! Mention {
//!     user:     Id,     // the participant id — authoritative
//!     start:    usize,  // byte offset of the `@` within Message::text
//!     len:      usize,  // byte length of the `@label` run
//!     is_agent: bool,   // what the room said this participant was, at authoring
//! }
//! ```
//!
//! In the document that is a `mentions` List on the message block whose entries
//! are `{ user: Str, start: Int, len: Int, agent: Bool }`.
//!
//! `user` is the same id the message's `author` field, the room roster, the
//! presence layer and message attribution all use — one identity vocabulary.
//! `start`/`len` slice [`crate::model::Message::text`], so a renderer paints the
//! run distinctly without a second parse and never disagrees with the resolver
//! about where the mention is. `is_agent` is a **snapshot** so a consumer
//! reading the saved document offline can still tell a person from an agent; a
//! live consumer that has the roster should prefer the roster, because `user`
//! is the authoritative half and a participant's kind is the room's to say.
//!
//! ## Binding
//!
//! Mentions are resolved once, at send, by [`resolve`]: every `@` in the body is
//! matched against the room's participants, longest display name first. Binding
//! at send (rather than tracking offsets while the composer is edited) means a
//! mention typed by hand, pasted, or completed from the picker all arrive as the
//! same structure, and no edit can leave an offset pointing at the wrong run.

use serde::{Deserialize, Serialize};
#[allow(unused_imports, reason = "the `JsonSchema` derive names the crate")]
use ui::tilo_resource::{self, JsonSchema};

use crate::model::Id;

/// A structured reference to one room participant inside a message body.
///
/// See the [module docs](self) for the full contract. `user` is authoritative;
/// `start`/`len` locate the run inside [`crate::model::Message::text`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, tilo_resource::Model)]
pub struct Mention {
    /// The participant referred to — the same id used for message attribution,
    /// the room roster and presence.
    #[model(atom)]
    pub user: Id,
    /// Byte offset of the `@` that opens the mention within the message text.
    pub start: usize,
    /// Byte length of the `@label` run within the message text.
    pub len: usize,
    /// What the room said this participant was when the message was written:
    /// `true` for an agent, `false` for a person. A snapshot for consumers with
    /// no live roster — `user` remains the authoritative half.
    pub is_agent: bool,
}

impl Mention {
    /// Byte offset one past the end of the mention run.
    pub fn end(&self) -> usize {
        self.start.saturating_add(self.len)
    }

    /// The `@label` run this mention covers, or `""` if the offsets do not land
    /// on character boundaries of `text` (a body edited without re-resolving).
    pub fn slice<'a>(&self, text: &'a str) -> &'a str {
        text.get(self.start..self.end()).unwrap_or("")
    }
}

/// Someone a message's `@name` can bind to: a person or an agent in the
/// room.
///
/// `name` is the room's answer, never the client's own claim about itself —
/// see `ChatApp::participants`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub id: Id,
    pub name: String,
    /// An automated agent acting in the room rather than a person. The roster
    /// draws the distinction; a mention carries it forward.
    pub is_agent: bool,
}

/// Resolve every `@`-run in `text` against the room's `participants`.
///
/// This is the binding step: it runs once, when a message is sent, and its
/// output is what the document stores. Matching is case-insensitive and
/// **longest name first**, so a room holding both "Ada" and "Ada Lovelace"
/// binds `@Ada Lovelace` to the longer one rather than to "Ada" plus stray
/// prose. An `@` that matches nobody is left as ordinary text — no mention is
/// invented for a name the room does not have.
pub fn resolve(text: &str, participants: &[Participant]) -> Vec<Mention> {
    // Longest first so "Ada Lovelace" wins over "Ada" at the same offset.
    let mut by_len: Vec<&Participant> = participants.iter().collect();
    by_len.sort_by_key(|p| std::cmp::Reverse(p.name.len()));

    // Case-insensitive matching runs against a lowered copy. `to_lowercase` can
    // change byte length for a few scripts, which would make offsets computed on
    // the copy wrong for the original; when that happens fall back to a
    // case-sensitive scan rather than emitting offsets that slice the wrong run.
    let lowered = text.to_lowercase();
    let fold = lowered.len() == text.len();
    let haystack: &str = if fold { &lowered } else { text };

    let mut out: Vec<Mention> = Vec::new();
    let mut cursor = 0usize;
    // `cursor` is always a byte offset this loop itself produced from a prior
    // valid boundary, but a caller-adjacent edit is exactly the kind of thing
    // this scan must never trust blindly — a miss here just stops resolving
    // further mentions rather than panicking.
    while let Some(tail) = text.get(cursor..) {
        let Some(rel) = tail.find('@') else {
            break;
        };
        let at = cursor + rel;
        let Some(before) = text.get(..at) else {
            break;
        };
        let opens_word = before.chars().next_back().is_none_or(char::is_whitespace);
        if !opens_word {
            cursor = at + '@'.len_utf8();
            continue;
        }
        let after = at + '@'.len_utf8();
        let mut matched: Option<usize> = None;
        let mut who: Option<&Participant> = None;
        for p in &by_len {
            if p.name.is_empty() {
                continue;
            }
            let lowered_name = p.name.to_lowercase();
            let needle: &str = if fold {
                if lowered_name.len() != p.name.len() {
                    continue;
                }
                &lowered_name
            } else {
                &p.name
            };
            // `haystack` may be a lowercased copy: even with a matching total
            // byte length, its char boundaries at `after` aren't guaranteed to
            // line up with `text`'s, so a miss here just means no match at
            // this position for any candidate — stop trying them.
            let Some(haystack_after) = haystack.get(after..) else {
                break;
            };
            if !haystack_after.starts_with(needle) {
                continue;
            }
            // A name must not run into the middle of a longer word: `@ada` in
            // "@adamant" is not a mention of Ada.
            let tail_pos = after + needle.len();
            let Some(after_name) = text.get(tail_pos..) else {
                continue;
            };
            let boundary = after_name
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if boundary {
                matched = Some(needle.len());
                who = Some(p);
                break;
            }
        }
        match (matched, who) {
            (Some(n), Some(p)) => {
                out.push(Mention {
                    user: p.id.clone(),
                    start: at,
                    len: '@'.len_utf8() + n,
                    is_agent: p.is_agent,
                });
                cursor = after + n;
            }
            _ => cursor = after,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Vec<Participant> {
        vec![
            Participant {
                id: "u_ada".into(),
                name: "Ada".into(),
                is_agent: false,
            },
            Participant {
                id: "u_lovelace".into(),
                name: "Ada Lovelace".into(),
                is_agent: false,
            },
            Participant {
                id: "agent_scribe".into(),
                name: "Scribe".into(),
                is_agent: true,
            },
        ]
    }

    #[test]
    fn resolve_maps_whole_word_room_names_to_participant_ids() {
        // Phase: a_mention_is_a_participant_id_not_a_string.
        {
            let m = resolve("morning @Ada", &room());
            assert_eq!(m.len(), 1);
            assert_eq!(m[0].user, "u_ada");
            assert_eq!(m[0].slice("morning @Ada"), "@Ada");
            assert!(!m[0].is_agent);
        }
        // Phase: the_longest_matching_name_wins.
        {
            let text = "@Ada Lovelace can you look";
            let m = resolve(text, &room());
            assert_eq!(m.len(), 1, "one mention, not Ada plus stray prose");
            assert_eq!(m[0].user, "u_lovelace");
            assert_eq!(m[0].slice(text), "@Ada Lovelace");
        }
        // Phase: an_agent_resolves_the_same_way_a_person_does.
        {
            let m = resolve("@Scribe please summarise", &room());
            assert_eq!(m[0].user, "agent_scribe");
            assert!(
                m[0].is_agent,
                "the room's agent flag travels with the mention"
            );
        }
        // Phase: an_unknown_name_is_left_as_prose.
        {
            assert!(resolve("email me @ ada@example.com", &room()).is_empty());
            assert!(resolve("@Nobody", &room()).is_empty());
        }
        // Phase: a_name_inside_a_longer_word_is_not_a_mention.
        {
            assert!(resolve("@Adamant", &room()).is_empty());
        }
        // Phase: offsets_slice_the_body_they_came_from.
        {
            let text = "cc @Ada and @Scribe";
            let m = resolve(text, &room());
            assert_eq!(m.len(), 2);
            assert_eq!(m[0].slice(text), "@Ada");
            assert_eq!(m[1].slice(text), "@Scribe");
            assert!(m[0].end() <= m[1].start);
        }
    }
}
