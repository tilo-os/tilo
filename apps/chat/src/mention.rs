//! Structured `@mention`s: a reference to a room participant, not styled text.
//!
//! A mention is **not** a string that happens to start with `@`. It is a
//! resolved pointer to one participant of the room, minted when the message is
//! authored and stored in the shared CRDT next to the body (see
//! [`crate::doc`]). That is what lets anything else — the person mentioned, an
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

use crate::model::Id;

/// A structured reference to one room participant inside a message body.
///
/// See the [module docs](self) for the full contract. `user` is authoritative;
/// `start`/`len` locate the run inside [`crate::model::Message::text`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// The participant referred to — the same id used for message attribution,
    /// the room roster and presence.
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

/// Someone the composer can offer at `@`: a person or an agent in the room.
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

/// The `@`-run the caret currently sits in, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionQuery {
    /// Byte offset of the `@`.
    pub start: usize,
    /// Text typed after the `@`, up to the caret.
    pub query: String,
}

/// How long an `@`-run may grow before it stops being somebody's name. Spaces
/// are allowed inside a run because real names have them; this is what stops a
/// stray `@` from holding the picker open over a whole paragraph.
const MAX_QUERY_LEN: usize = 40;

/// Find the `@`-run the caret is inside, so the composer knows whether to offer
/// participants. Returns `None` when the caret is not in one.
///
/// A run starts at an `@` that opens a word (start of text, or preceded by
/// whitespace) and ends at the caret. It is abandoned once it grows past
/// `MAX_QUERY_LEN` or crosses a newline — at that point the person is writing
/// prose that contains an `@`, not picking somebody.
pub fn query_at_caret(text: &str, caret: usize) -> Option<MentionQuery> {
    let caret = caret.min(text.len());
    let head = text.get(..caret)?;
    let at = head.rfind('@')?;
    // The `@` must open a word: nothing before it, or whitespace.
    let opens_word = head
        .get(..at)?
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if !opens_word {
        return None;
    }
    let query = head.get(at + '@'.len_utf8()..)?;
    if query.len() > MAX_QUERY_LEN || query.contains('\n') || query.contains('@') {
        return None;
    }
    Some(MentionQuery {
        start: at,
        query: query.to_string(),
    })
}

/// Whether the composer should be offering participants right now, and for
/// what — [`query_at_caret`] plus the two things that make an open picker
/// wrong.
///
/// Returns `None` when the caret is not in an `@`-run, when nobody in the room
/// matches what has been typed (an open list of nothing is noise), or when the
/// run is *already* a completed mention — `@Ada Lovelace ` with the closing
/// space typed. That last case is why the raw lexer is not enough on its own:
/// names contain spaces, so a run cannot be ended by whitespace alone, and
/// without the room to compare against, a finished pick keeps the list up.
pub fn picker_query(
    text: &str,
    caret: usize,
    participants: &[Participant],
) -> Option<MentionQuery> {
    let q = query_at_caret(text, caret)?;
    // Already picked: the run is exactly one participant's name plus the space
    // `apply_completion` appends to close it.
    if let Some(name) = q.query.strip_suffix(' ') {
        let done = participants
            .iter()
            .any(|p| !p.name.is_empty() && p.name.eq_ignore_ascii_case(name));
        if done {
            return None;
        }
    }
    if candidates(participants, &q.query).is_empty() {
        return None;
    }
    Some(q)
}

/// The participants worth offering for `query`, best first.
///
/// Ranks a prefix match on the whole name above a prefix match on any later
/// word ("lov" finds "Ada Lovelace") above a plain substring, and falls back to
/// the room's own order within a rank so the list is stable while typing. An
/// empty query offers everyone.
pub fn candidates<'a>(participants: &'a [Participant], query: &str) -> Vec<&'a Participant> {
    let q = query.trim().to_lowercase();
    let mut ranked: Vec<(u8, usize, &Participant)> = Vec::new();
    for (i, p) in participants.iter().enumerate() {
        let name = p.name.to_lowercase();
        let rank = if q.is_empty() || name.starts_with(&q) {
            0
        } else if name.split_whitespace().any(|w| w.starts_with(&q)) {
            1
        } else if name.contains(&q) {
            2
        } else if p.id.to_lowercase().contains(&q) {
            3
        } else {
            continue;
        };
        ranked.push((rank, i, p));
    }
    ranked.sort_by_key(|(rank, i, _)| (*rank, *i));
    ranked.into_iter().map(|(_, _, p)| p).collect()
}

/// Replace the `@`-run described by `q` with a completed `@Name ` and report the
/// new caret (just past the inserted space).
///
/// The trailing space is what closes the run: without it the next character
/// typed would re-open the picker on a name that is already chosen.
pub fn apply_completion(
    text: &str,
    q: &MentionQuery,
    caret: usize,
    who: &Participant,
) -> (String, usize) {
    let caret = caret.min(text.len());
    let end = caret.max(q.start);
    let mut out = String::with_capacity(text.len() + who.name.len());
    out.push_str(text.get(..q.start).unwrap_or(""));
    out.push('@');
    out.push_str(&who.name);
    out.push(' ');
    let new_caret = out.len();
    out.push_str(text.get(end..).unwrap_or(""));
    (out, new_caret)
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

    #[test]
    fn picker_opens_on_a_leading_at_and_ranks_room_candidates() {
        // Phase: the_caret_query_opens_only_on_a_word_leading_at.
        {
            assert_eq!(query_at_caret("hi @ad", 6).unwrap().query, "ad");
            assert_eq!(query_at_caret("hi @ad", 6).unwrap().start, 3);
            assert!(
                query_at_caret("mail@ad", 7).is_none(),
                "an infix @ is an address"
            );
            assert!(
                query_at_caret("hi @a\nb", 7).is_none(),
                "a newline ends the run"
            );
        }
        // Phase: candidates_rank_prefix_then_word_then_substring.
        {
            let r = room();
            let c = candidates(&r, "ada");
            assert_eq!(c[0].id, "u_ada");
            let c = candidates(&r, "lov");
            assert_eq!(c[0].id, "u_lovelace", "a later word still matches");
            assert_eq!(
                candidates(&r, "").len(),
                3,
                "an empty query offers the room"
            );
        }
        // Phase: the_picker_stays_shut_when_the_room_offers_nobody.
        {
            assert!(picker_query("hi @zz", 6, &room()).is_none());
            assert!(picker_query("hi @ad", 6, &room()).is_some());
        }
    }

    #[test]
    fn completion_inserts_the_name_keeps_the_tail_and_closes_the_run() {
        // Phase: completion_closes_the_run_so_the_picker_does_not_reopen.
        {
            let q = query_at_caret("hi @ad", 6).unwrap();
            let who = Participant {
                id: "u_lovelace".into(),
                name: "Ada Lovelace".into(),
                is_agent: false,
            };
            let (text, caret) = apply_completion("hi @ad", &q, 6, &who);
            assert_eq!(text, "hi @Ada Lovelace ");
            assert_eq!(caret, text.len());
            assert!(
                picker_query(&text, caret, &room()).is_none(),
                "a finished pick must not leave the list up"
            );
        }
        // Phase: completing_mid_sentence_keeps_the_tail.
        {
            let q = query_at_caret("hi @ad, look", 6).unwrap();
            let who = Participant {
                id: "u_ada".into(),
                name: "Ada".into(),
                is_agent: false,
            };
            let (text, caret) = apply_completion("hi @ad, look", &q, 6, &who);
            assert_eq!(text, "hi @Ada , look");
            assert_eq!(&text[..caret], "hi @Ada ");
        }
    }
}
