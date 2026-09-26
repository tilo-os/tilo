//! Where a remark goes, and how that is shown.
//!
//! The destination is chosen, not inferred: a selector beside the compose field
//! shows where the message is headed, and nothing else may redirect it. That is
//! why [`deliver`] takes the [`Destination`] rather than deducing one from the
//! body — typing `@name` pings somebody, and never moves the message out of the
//! channel it says it is going to.
//!
//! Both [`Destination`] and [`Delivery`] answer [`Target`], and must answer the
//! *same* target: a selector that displays one destination while the message
//! goes to another is worse than no selector at all.

use serde::{Deserialize, Serialize};

/// String identifier for a participant or a channel — chat's own vocabulary for
/// attribution, roster and presence, so a mention, an author and a destination
/// are all comparable.
pub type Id = String;

/// A structured reference to one room participant inside a remark's body.
///
/// The shape `apps/chat`'s `mention::Mention` defines, mirrored so this crate
/// needs no app dependency and the two convert by field name. `is_agent` is the
/// room's answer *at authoring time*, kept so a consumer reading the message
/// offline can still tell a person from software.
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
/// `@name` in a body. Mirrors chat's `mention::Participant` field for field,
/// for the same reason [`MentionRef`] mirrors its `Mention`.
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

/// Resolve every `@name` in `text` against the room, into structured mentions
/// whose `start`/`len` are byte offsets into `text`.
///
/// The toolkit's compose surface leaves mentions empty: resolving a name needs
/// the roster, which the host has and a widget does not.
///
/// The rules are chat's own — longest name first, case-insensitive, an `@` must
/// open a word, a name must not run into a longer one — so a name means the same
/// participant wherever it was typed. `apps/chat` holds a test running both
/// resolvers over the same bodies, so the two cannot drift.
pub fn resolve_mentions(text: &str, room: &[RoomMember]) -> Vec<MentionRef> {
    // Longest first so "Ada Lovelace" wins over "Ada" at the same offset.
    let mut by_len: Vec<&RoomMember> = room.iter().collect();
    by_len.sort_by_key(|m| std::cmp::Reverse(m.name.len()));

    // `to_lowercase` can change byte length, which would make offsets computed on
    // the lowered copy wrong for the original; when it does, scan case-sensitively
    // rather than emit offsets that slice the wrong run.
    let lowered = text.to_lowercase();
    let fold = lowered.len() == text.len();
    let haystack: &str = if fold { &lowered } else { text };

    let mut out: Vec<MentionRef> = Vec::new();
    let mut cursor: usize = 0;
    // `cursor`/`at`/`after`/`tail` are all byte offsets this loop derives
    // from `find`/`starts_with`/char-boundary walks over `text`, so they
    // always land on char boundaries — but `.get()` + `saturating_add`
    // says so without leaning on that proof at every slice/add.
    while let Some(tail_str) = text.get(cursor..) {
        let Some(rel) = tail_str.find('@') else {
            break;
        };
        let at = cursor.saturating_add(rel);
        let opens_word = text
            .get(..at)
            .and_then(|s| s.chars().next_back())
            .is_none_or(char::is_whitespace);
        if !opens_word {
            cursor = at.saturating_add('@'.len_utf8());
            continue;
        }
        let after = at.saturating_add('@'.len_utf8());
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
            let Some(rest) = haystack.get(after..) else {
                break;
            };
            if !rest.starts_with(needle) {
                continue;
            }
            let tail = after.saturating_add(needle.len());
            let boundary = text
                .get(tail..)
                .and_then(|s| s.chars().next())
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
                    len: '@'.len_utf8().saturating_add(n),
                    is_agent: m.is_agent,
                });
                cursor = after.saturating_add(n);
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

/// Where the person has said the remark should go — what the selector displays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    /// A channel everyone in the room can read.
    Channel { id: Id, name: String },
    /// One participant, privately. `is_agent` does not change the routing — a
    /// participant destination is private either way — but it changes what the
    /// selector must *say* (see [`DestinationDisplay::agent`]).
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

    pub fn target(&self) -> Target<'_> {
        match self {
            Destination::Channel { id, .. } => Target::Channel(id),
            Destination::Participant { id, .. } => Target::Participant(id),
        }
    }

    pub fn is_private(&self) -> bool {
        matches!(self, Destination::Participant { .. })
    }

    /// Structured facts rather than a rendered sentence: the UI keys them into
    /// `fluent`, so this crate stays free of user-facing English.
    pub fn display(&self) -> DestinationDisplay<'_> {
        match self {
            Destination::Channel { name, .. } => DestinationDisplay {
                name,
                private: false,
                agent: false,
            },
            Destination::Participant { name, is_agent, .. } => DestinationDisplay {
                name,
                private: true,
                agent: *is_agent,
            },
        }
    }
}

/// The facts the destination selector renders. `private` is stated rather than
/// inferred — nobody should have to work out from a name whether the room can
/// read what they write — and `agent` says why a private destination is
/// unremarkable, an agent's reply being the visible artefact anyway.
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
    /// For [`DirectedInChannel`](Self::DirectedInChannel) this is the *channel*:
    /// the mentioned are pinged, but the channel is who can read it, which is the
    /// question the selector answers.
    pub fn target(&self) -> Target<'_> {
        match self {
            Delivery::Channel { channel } | Delivery::DirectedInChannel { channel, .. } => {
                Target::Channel(channel)
            }
            Delivery::Private { to } => Target::Participant(to),
        }
    }

    pub fn is_private(&self) -> bool {
        matches!(self, Delivery::Private { .. })
    }

    /// Everyone singled out by this delivery, pinged or addressed.
    pub fn addressed(&self) -> &[Id] {
        match self {
            Delivery::Channel { .. } => &[],
            Delivery::DirectedInChannel { to, .. } => to,
            Delivery::Private { to } => std::slice::from_ref(to),
        }
    }
}

/// Decide how a remark is delivered, from the destination the person chose and
/// the mentions in the body. The destination decides *who can read it*; mentions
/// only decide who is singled out within that, because a mention that moved a
/// message or made it private would make the selector a lie.
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

    /// What the selector shows is what gets delivered: target and privacy
    /// agree, mentions direct inside a channel without making it private,
    /// and a participant destination is private for people and agents alike.
    #[test]
    fn delivery_goes_where_the_selector_says_with_the_privacy_it_shows() {
        // Phase: the displayed destination is the delivered destination.
        // Asserted on the structured target, not on rendered text, so it keeps
        // holding when the wording changes.
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

        // Phase: a mention directs without leaving the channel.
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

        // Phase: no mention is a plain channel post.
        let dest = Destination::channel("c_general", "general");
        assert_eq!(
            deliver(&dest, &[]),
            Delivery::Channel {
                channel: "c_general".into()
            }
        );

        // Phase: repeated mentions of one person ping once.
        let dest = Destination::channel("c_general", "general");
        let d = deliver(&dest, &[ada(), MentionRef::new("u_ada", 20, 4, false)]);
        assert_eq!(d.addressed(), ["u_ada".to_string()]);

        // Phase: a participant destination is private for person and agent alike.
        for (id, name, is_agent) in [("u_ada", "Ada", false), ("agent_scribe", "Scribe", true)] {
            let dest = Destination::participant(id, name, is_agent);
            let d = deliver(&dest, &[ada(), scribe()]);
            assert_eq!(d, Delivery::Private { to: id.into() }, "case {id}");
            assert!(d.is_private(), "case {id}");
            assert_eq!(d.addressed(), [id.to_string()], "case {id}");
        }

        // Phase: the selector states privacy plainly.
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

    /// `@name` resolves only to someone in the room, by longest name, at a
    /// word start, with byte offsets — and a resolved agent mention directs
    /// within the channel.
    #[test]
    fn mentions_resolve_only_to_named_room_members() {
        // Case: an agent's name resolves to its participant id.
        let m = resolve_mentions("@claude make square and put in open document", &room());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user, "agent_claude");
        assert!(m[0].is_agent, "the mention says it names software");
        assert_eq!(m[0].start, 0);
        assert_eq!(m[0].len, "@claude".len());

        // Case: a name nobody in the room has is not a mention.
        assert!(resolve_mentions("@nobody are you there", &room()).is_empty());

        // Case: the longest matching name wins.
        let m = resolve_mentions("@Ada Lovelace look", &room());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].user, "u_lovelace");

        // Case: an `@` that does not open a word is not a mention. Both inputs
        // would otherwise ping somebody who was never named.
        assert!(resolve_mentions("write to ada@example.com", &room()).is_empty());
        assert!(resolve_mentions("@adamant about it", &room()).is_empty());

        // Case: the offsets slice the run they name. Multi-byte characters
        // before the `@`, so an offset counted in anything but bytes slices the
        // wrong run.
        let text = "café — @Claude take a look";
        let m = resolve_mentions(text, &room());
        assert_eq!(m.len(), 1);
        assert_eq!(&text[m[0].start..m[0].start + m[0].len], "@Claude");

        // Case: a resolved agent mention directs within the channel.
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
}
