//! Where a remark goes, and how that is shown.
//!
//! The rule this module encodes: **the destination is chosen, not inferred.**
//! A selector sits beside the compose field showing where the message is
//! headed, and the person can change it. Nothing else may quietly redirect a
//! message — in particular, typing `@name` *pings* someone and does not move
//! the message out of the channel it says it is going to.
//!
//! That is why [`deliver`] takes the [`Destination`] as an argument rather than
//! deducing one from the body. The mentions only decide whether the delivery is
//! [`Delivery::DirectedInChannel`] (somebody is singled out, the room still
//! sees it) or a plain [`Delivery::Channel`] — never *which* channel, and never
//! whether it is private.
//!
//! ## The invariant
//!
//! Both [`Destination`] and [`Delivery`] answer [`Target`], and they must
//! always answer the *same* target. A selector that displays one destination
//! while the message goes to another is worse than no selector at all, so the
//! agreement is a property of the types and is asserted directly (see this
//! module's tests) rather than left to a rendering pass to get right.

use serde::{Deserialize, Serialize};

/// String identifier for a participant or a channel. The same vocabulary chat
/// uses for message attribution, the room roster and presence — one identity
/// space, so a mention, an author and a destination are all comparable.
pub type Id = String;

/// A structured reference to one room participant inside a remark's body.
///
/// Field-for-field the shape `apps/chat`'s `mention::Mention` already defines
/// (`user`, `start`, `len`, `is_agent`), mirrored here so this crate stays free
/// of an app dependency while remaining trivially convertible: chat's resolver
/// output maps across by field name with no reinterpretation.
///
/// `user` is authoritative; `start`/`len` locate the `@label` run inside the
/// remark text so a renderer can paint it without a second parse. `is_agent` is
/// the room's answer *at authoring time*, kept so a consumer reading the saved
/// message offline can still tell a person from an agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionRef {
    pub user: Id,
    pub start: usize,
    pub len: usize,
    pub is_agent: bool,
}

impl MentionRef {
    pub fn new(user: impl Into<Id>, start: usize, len: usize, is_agent: bool) -> Self {
        Self {
            user: user.into(),
            start,
            len,
            is_agent,
        }
    }
}

/// One participant of the room, as a sender needs to know them to resolve an
/// `@name` in a body.
///
/// The same three facts [`MentionRef`] carries forward, before resolution: who
/// they are, what the room calls them, and whether they are software. Mirrors
/// the `mention::Participant` the chat app already defines, field for field,
/// for the same reason [`MentionRef`] mirrors its `Mention` — this crate stays
/// free of an app dependency and the two convert by field name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomMember {
    pub id: Id,
    pub name: String,
    pub is_agent: bool,
}

impl RoomMember {
    pub fn new(id: impl Into<Id>, name: impl Into<String>, is_agent: bool) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            is_agent,
        }
    }
}

/// Resolve every `@name` in `text` against the room, into structured mentions.
///
/// The toolkit's compose surface deliberately leaves mentions empty — resolving
/// a name needs the roster, which the host has and a widget does not. This is
/// the function the host calls before sending, and it is why an `@claude`
/// somebody typed becomes a participant id rather than staying a word.
///
/// The rules are the chat app's, deliberately identical, because a name must
/// mean the same participant whether it was typed in the chat window or in a
/// remark about a file:
///
/// * longest display name first, so `@Ada Lovelace` beats `@Ada`;
/// * case-insensitive, except where lowering would change byte length and make
///   the offsets slice the wrong run, in which case the scan is exact;
/// * an `@` must open a word, so an e-mail address is not a mention;
/// * a name must not run into a longer word, so `@ada` in `@adamant` is not
///   one either.
///
/// Identical by assertion, not by intention: `apps/chat` holds a test that runs
/// this and its own resolver over the same bodies and fails if they disagree,
/// so the two cannot drift into resolving one name two ways.
///
/// `start` and `len` are byte offsets into `text`, so a renderer paints the run
/// without a second parse and cannot disagree with this about where it is.
pub fn resolve_mentions(text: &str, room: &[RoomMember]) -> Vec<MentionRef> {
    // Longest first so "Ada Lovelace" wins over "Ada" at the same offset.
    let mut by_len: Vec<&RoomMember> = room.iter().collect();
    by_len.sort_by_key(|m| std::cmp::Reverse(m.name.len()));

    // Case-insensitive matching runs against a lowered copy. `to_lowercase` can
    // change byte length for a few scripts, which would make offsets computed
    // on the copy wrong for the original; when that happens fall back to a
    // case-sensitive scan rather than emitting offsets that slice the wrong run.
    let lowered = text.to_lowercase();
    let fold = lowered.len() == text.len();
    let haystack: &str = if fold { &lowered } else { text };

    let mut out: Vec<MentionRef> = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find('@') {
        let at = cursor + rel;
        let opens_word = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        if !opens_word {
            cursor = at + '@'.len_utf8();
            continue;
        }
        let after = at + '@'.len_utf8();
        let mut matched: Option<usize> = None;
        let mut who: Option<&RoomMember> = None;
        for m in &by_len {
            if m.name.is_empty() {
                continue;
            }
            let lowered_name = m.name.to_lowercase();
            let needle: &str = if fold {
                if lowered_name.len() != m.name.len() {
                    continue;
                }
                &lowered_name
            } else {
                &m.name
            };
            if !haystack[after..].starts_with(needle) {
                continue;
            }
            // A name must not run into the middle of a longer word.
            let tail = after + needle.len();
            let boundary = text[tail..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if boundary {
                matched = Some(needle.len());
                who = Some(m);
                break;
            }
        }
        match (matched, who) {
            (Some(n), Some(m)) => {
                out.push(MentionRef {
                    user: m.id.clone(),
                    start: at,
                    len: '@'.len_utf8() + n,
                    is_agent: m.is_agent,
                });
                cursor = after + n;
            }
            _ => cursor = after,
        }
    }
    out
}

/// The one place a remark is delivered to. Both the chosen [`Destination`] and
/// the resulting [`Delivery`] report one of these, and they must agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Channel(&'a str),
    Participant(&'a str),
}

/// Where the person has said the remark should go. This is what the selector
/// displays, and it is chosen explicitly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    /// A channel everyone in the room can read.
    Channel { id: Id, name: String },
    /// One participant, privately. `is_agent` does not change the routing — a
    /// participant destination is private either way — but it changes what the
    /// selector must *say*, because the reason privacy is acceptable differs
    /// (see [`DestinationDisplay::agent`]).
    Participant {
        id: Id,
        name: String,
        is_agent: bool,
    },
}

impl Destination {
    pub fn channel(id: impl Into<Id>, name: impl Into<String>) -> Self {
        Destination::Channel {
            id: id.into(),
            name: name.into(),
        }
    }

    pub fn participant(id: impl Into<Id>, name: impl Into<String>, is_agent: bool) -> Self {
        Destination::Participant {
            id: id.into(),
            name: name.into(),
            is_agent,
        }
    }

    /// The single place this destination names.
    pub fn target(&self) -> Target<'_> {
        match self {
            Destination::Channel { id, .. } => Target::Channel(id),
            Destination::Participant { id, .. } => Target::Participant(id),
        }
    }

    /// Whether choosing this destination keeps the remark out of the room.
    pub fn is_private(&self) -> bool {
        matches!(self, Destination::Participant { .. })
    }

    /// What the selector must show, as structured facts rather than a rendered
    /// sentence — the UI turns these into `fluent` keys, so the control is
    /// localisable and this crate stays free of user-facing English.
    pub fn display(&self) -> DestinationDisplay<'_> {
        match self {
            Destination::Channel { name, .. } => DestinationDisplay {
                name,
                private: false,
                agent: false,
            },
            Destination::Participant {
                name, is_agent, ..
            } => DestinationDisplay {
                name,
                private: true,
                agent: *is_agent,
            },
        }
    }
}

/// The facts the destination selector renders.
///
/// `private` is stated plainly rather than inferred by the reader: a person
/// must never have to work out from a name whether the room can see what they
/// are writing. `agent` distinguishes *why* a private destination is the
/// ordinary case — an agent's reply is the visible artefact, so addressing one
/// privately loses nothing, whereas a private remark to a colleague genuinely
/// takes the context out of the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DestinationDisplay<'a> {
    pub name: &'a str,
    pub private: bool,
    pub agent: bool,
}

/// How a remark is actually delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Delivery {
    /// Posted in `channel`. Everyone there sees it; nobody is singled out.
    Channel { channel: Id },
    /// Posted in `channel` **and** addressed to `to`. The mention pings them;
    /// the room still holds the context. This is what `@name` produces.
    DirectedInChannel { channel: Id, to: Vec<Id> },
    /// Delivered to `to` alone. The channel never sees it.
    Private { to: Id },
}

impl Delivery {
    /// The single place this delivery reaches. For
    /// [`DirectedInChannel`](Self::DirectedInChannel) that is the *channel* —
    /// the mentioned people are pinged, but the channel is who can read it,
    /// which is the question the selector is answering.
    pub fn target(&self) -> Target<'_> {
        match self {
            Delivery::Channel { channel } | Delivery::DirectedInChannel { channel, .. } => {
                Target::Channel(channel)
            }
            Delivery::Private { to } => Target::Participant(to),
        }
    }

    /// Whether this delivery keeps the remark out of the room.
    pub fn is_private(&self) -> bool {
        matches!(self, Delivery::Private { .. })
    }

    /// Everyone singled out by this delivery, pinged or addressed.
    pub fn addressed(&self) -> &[Id] {
        match self {
            Delivery::Channel { .. } => &[],
            Delivery::DirectedInChannel { to, .. } => to,
            Delivery::Private { .. } => std::slice::from_ref(match self {
                Delivery::Private { to } => to,
                _ => unreachable!(),
            }),
        }
    }
}

/// Decide how a remark is delivered, from the destination the person chose and
/// the mentions in the body.
///
/// The destination decides *who can read it*; the mentions only decide who is
/// singled out within that. A mention can never move a message to a different
/// channel, and can never make a channel message private — that would make the
/// selector a lie.
pub fn deliver(destination: &Destination, mentions: &[MentionRef]) -> Delivery {
    match destination {
        Destination::Channel { id, .. } => {
            let mut to: Vec<Id> = Vec::new();
            for m in mentions {
                if !to.contains(&m.user) {
                    to.push(m.user.clone());
                }
            }
            if to.is_empty() {
                Delivery::Channel {
                    channel: id.clone(),
                }
            } else {
                Delivery::DirectedInChannel {
                    channel: id.clone(),
                    to,
                }
            }
        }
        Destination::Participant { id, .. } => Delivery::Private { to: id.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ada() -> MentionRef {
        MentionRef::new("u_ada", 0, 4, false)
    }
    fn scribe() -> MentionRef {
        MentionRef::new("agent_scribe", 0, 7, true)
    }

    /// The invariant the selector exists to uphold: whatever destination is on
    /// screen is the destination the message is delivered to. Asserted on the
    /// structured target, not on rendered text, so it keeps holding when the
    /// wording changes.
    #[test]
    fn the_displayed_destination_is_the_delivered_destination() {
        let cases = [
            Destination::channel("c_general", "general"),
            Destination::participant("u_ada", "Ada", false),
            Destination::participant("agent_scribe", "Scribe", true),
        ];
        // Every mention shape a body can have must leave the target alone.
        let mention_sets: [&[MentionRef]; 3] = [&[], &[ada()], &[ada(), scribe()]];

        for dest in &cases {
            for mentions in mention_sets {
                let d = deliver(dest, mentions);
                assert_eq!(
                    dest.target(),
                    d.target(),
                    "selector showed {:?} but delivery targets {:?}",
                    dest.target(),
                    d.target()
                );
                assert_eq!(
                    dest.is_private(),
                    d.is_private(),
                    "privacy shown and privacy delivered disagree for {dest:?}"
                );
            }
        }
    }

    /// `@name` pings and keeps the room's context — it does not redirect.
    #[test]
    fn a_mention_directs_without_leaving_the_channel() {
        let dest = Destination::channel("c_general", "general");
        let d = deliver(&dest, &[ada()]);
        assert_eq!(
            d,
            Delivery::DirectedInChannel {
                channel: "c_general".into(),
                to: vec!["u_ada".into()]
            }
        );
        assert!(!d.is_private(), "a mention must not make a message private");
        assert_eq!(d.target(), Target::Channel("c_general"));
    }

    #[test]
    fn no_mention_is_a_plain_channel_post() {
        let dest = Destination::channel("c_general", "general");
        assert_eq!(
            deliver(&dest, &[]),
            Delivery::Channel {
                channel: "c_general".into()
            }
        );
    }

    /// Choosing a participant is what makes a remark private — for a person and
    /// for an agent alike. The asymmetry is in what the selector *says*, not in
    /// where the message goes.
    #[test]
    fn a_participant_destination_is_private_for_person_and_agent_alike() {
        for (id, name, is_agent) in [("u_ada", "Ada", false), ("agent_scribe", "Scribe", true)] {
            let dest = Destination::participant(id, name, is_agent);
            let d = deliver(&dest, &[ada(), scribe()]);
            assert_eq!(d, Delivery::Private { to: id.into() });
            assert!(d.is_private());
            assert_eq!(d.addressed(), [id.to_string()]);
        }
    }

    /// The selector must state privacy rather than leave it to be inferred, and
    /// must distinguish an agent so the reason privacy is unremarkable is on
    /// screen too.
    #[test]
    fn the_selector_states_privacy_plainly() {
        let channel = Destination::channel("c_general", "general");
        let room = channel.display();
        assert_eq!(room.name, "general");
        assert!(!room.private);

        let colleague = Destination::participant("u_ada", "Ada", false);
        let person = colleague.display();
        assert!(person.private, "a private destination must say so");
        assert!(!person.agent);

        let scribe = Destination::participant("agent_scribe", "Scribe", true);
        let agent = scribe.display();
        assert!(agent.private);
        assert!(agent.agent, "an agent destination is marked as one");
    }

    fn room() -> Vec<RoomMember> {
        vec![
            RoomMember::new("u_ada", "Ada", false),
            RoomMember::new("u_lovelace", "Ada Lovelace", false),
            RoomMember::new("agent_claude", "Claude", true),
        ]
    }

    /// The one this whole path exists for: somebody writes a name and an
    /// agent's participant id comes out, so the message can be addressed to
    /// software without anybody having typed an id.
    #[test]
    fn an_agents_name_resolves_to_its_participant_id() {
        let m = resolve_mentions("@claude make square and put in open document", &room());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user, "agent_claude");
        assert!(m[0].is_agent, "the mention says it names software");
        assert_eq!(m[0].start, 0);
        assert_eq!(m[0].len, "@claude".len());
    }

    /// Resolved against the room, and the room is what decides — so a body that
    /// names nobody in it produces no mention rather than a guess.
    #[test]
    fn a_name_nobody_in_the_room_has_is_not_a_mention() {
        assert!(resolve_mentions("@nobody are you there", &room()).is_empty());
    }

    #[test]
    fn the_longest_matching_name_wins() {
        let m = resolve_mentions("@Ada Lovelace look", &room());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user, "u_lovelace");
    }

    /// An address is not a mention, and a name inside a longer word is not one
    /// either — both would ping somebody who was never named.
    #[test]
    fn an_at_that_does_not_open_a_word_is_not_a_mention() {
        assert!(resolve_mentions("write to ada@example.com", &room()).is_empty());
        assert!(resolve_mentions("@adamant about it", &room()).is_empty());
    }

    /// The offsets have to slice the run they claim to, or a renderer paints
    /// the highlight over the wrong words and a reader cuts the wrong text out.
    #[test]
    fn the_offsets_slice_the_run_they_name() {
        let text = "café — @Claude take a look";
        let m = resolve_mentions(text, &room());
        assert_eq!(m.len(), 1);
        assert_eq!(&text[m[0].start..m[0].start + m[0].len], "@Claude");
    }

    /// A mention in a channel destination directs without redirecting, which is
    /// what makes an agent-addressed remark readable by the whole room.
    #[test]
    fn a_resolved_agent_mention_directs_within_the_channel() {
        let mentions = resolve_mentions("@claude make square", &room());
        let d = deliver(&Destination::channel("c_general", "general"), &mentions);
        assert_eq!(
            d,
            Delivery::DirectedInChannel {
                channel: "c_general".into(),
                to: vec!["agent_claude".into()]
            }
        );
        assert!(!d.is_private(), "the room must still be able to read it");
    }

    #[test]
    fn repeated_mentions_of_one_person_ping_once() {
        let dest = Destination::channel("c_general", "general");
        let d = deliver(&dest, &[ada(), MentionRef::new("u_ada", 20, 4, false)]);
        assert_eq!(d.addressed(), ["u_ada".to_string()]);
    }
}
