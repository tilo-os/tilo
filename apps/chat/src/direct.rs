//! Direct conversations, as chat sees them.
//!
//! A direct message is not a kind of message this app invents. It is an
//! ordinary conversation in a room whose membership is two — see
//! [`qos_reference::DirectRoom`], which owns the vocabulary and the derivation
//! so that chat, the shell and an out-of-process agent all name the same room
//! for the same pair.
//!
//! What lives here is the small amount chat itself has to answer:
//!
//! * **Which room would I share with this person?** [`room_with`] — used to
//!   start a conversation, and used again every time to find the same one
//!   rather than a second one, because the room id *is* the pair.
//! * **What kind of room am I in?** [`classify`] — a chat window opened into a
//!   direct room has to know, because the same surface reads very differently
//!   depending on whether the person beside you is everybody or one person.
//!
//! ## Nothing is stored
//!
//! There is deliberately no list of "your DMs" in the chat document, and no
//! new shared state at all.
//!
//! Writing one there would leak: chat's document is the *workspace's*, so a row
//! saying Ada and Bob have a conversation would be readable by everybody in the
//! workspace. The content would be private and the fact of it would not, which
//! is a worse failure than none, because it looks like it worked.
//!
//! It is also unnecessary. Membership already answers it: a session is in the
//! rooms it is in, the direct ones among those are the direct conversations,
//! and which person each belongs to is recovered from the room by
//! [`classify`]. So the list is derived, per peer, from something already true
//! — and on a first run, before anybody has messaged anybody, it is empty.
//! That emptiness is correct rather than a placeholder waiting to be filled.

use qos_reference::DirectRoom;

use crate::model::Id;

/// What sort of room a chat window is looking at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomKind {
    /// A workspace room: many people, many channels, the ordinary case.
    Workspace,
    /// A room with two people in it. `with` is the other one — the person this
    /// window is a conversation with.
    Direct { with: Id },
}

impl RoomKind {
    /// The other person, when this is a direct conversation.
    pub fn direct_peer(&self) -> Option<&str> {
        match self {
            RoomKind::Direct { with } => Some(with),
            RoomKind::Workspace => None,
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, RoomKind::Direct { .. })
    }
}

/// The room `me` and `them` hold their conversation in, as a room id.
///
/// `None` when the pair names no room — you have no direct conversation with
/// yourself, and an unresolved participant has no id to address.
///
/// Pure, and the same on both machines: starting a conversation is not an act
/// that creates anything, it is working out which room the two of you were
/// always going to use. That is what makes "find the existing one rather than
/// open a second" true by construction instead of by a lookup that can race.
pub fn room_with(me: &str, them: &str) -> Option<String> {
    DirectRoom::between(me, them).map(|r| r.room_id())
}

/// Work out what kind of room `room_id` is, from this peer's point of view.
///
/// A direct room announces itself: its id is the derivation over its two
/// members, so checking it is asking "is there somebody here such that the room
/// we would share is this one". Nothing is read off a flag a peer could set,
/// and nothing is read off a name — a workspace called "Ada" is a workspace.
///
/// `candidates` is who this peer knows about — the room's roster. Somebody the
/// roster has not named yet cannot be recovered, and the honest answer then is
/// [`RoomKind::Workspace`]: a room whose second member this peer cannot name is
/// not a conversation it can label, and labelling it with a guess would put the
/// wrong person's name at the top of a private conversation.
pub fn classify<'a>(
    room_id: &str,
    me: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> RoomKind {
    if room_id.is_empty() || me.is_empty() {
        return RoomKind::Workspace;
    }
    for them in candidates {
        if them == me {
            continue;
        }
        if room_with(me, them).as_deref() == Some(room_id) {
            return RoomKind::Direct {
                with: them.to_string(),
            };
        }
    }
    RoomKind::Workspace
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "u_1f4c3a9b2d7e5061";
    const THEM: &str = "u_9a0b1c2d3e4f5061";
    const OTHER: &str = "u_5d6e7f8091a2b3c4";

    /// Opening a conversation twice opens the same one. This is the whole
    /// reason the room is derived rather than minted, so it is asserted rather
    /// than left to a lookup.
    #[test]
    fn starting_a_conversation_twice_names_one_room() {
        let first = room_with(ME, THEM).expect("two people name a room");
        let second = room_with(ME, THEM).expect("two people name a room");
        assert_eq!(first, second);
        // And from the other person's side, which is the case a mint would get
        // wrong: both of them opening at once, each holding a different room.
        assert_eq!(first, room_with(THEM, ME).unwrap());
    }

    #[test]
    fn there_is_no_direct_room_with_yourself() {
        assert_eq!(room_with(ME, ME), None);
    }

    /// A window opened into the pair's room knows it is a conversation, and
    /// knows who with — recovered from the room, not from a stored label.
    #[test]
    fn a_window_in_the_pairs_room_knows_who_it_is_talking_to() {
        let room = room_with(ME, THEM).unwrap();
        let kind = classify(&room, ME, [THEM, OTHER]);
        assert_eq!(kind, RoomKind::Direct { with: THEM.into() });
        assert_eq!(kind.direct_peer(), Some(THEM));
        assert!(kind.is_direct());
    }

    /// The same room, from the other person's window, names the first one.
    #[test]
    fn each_side_names_the_other() {
        let room = room_with(ME, THEM).unwrap();
        assert_eq!(classify(&room, THEM, [ME, OTHER]).direct_peer(), Some(ME));
    }

    /// An ordinary workspace room is not a conversation with anybody, and must
    /// not be labelled as one. A false positive here would put a person's name
    /// at the top of a room full of people.
    #[test]
    fn a_workspace_room_is_not_a_direct_conversation() {
        let workspace = "3f6d5a5c-1c1a-4b64-9f6b-2f4c2b6a8d11";
        let kind = classify(workspace, ME, [THEM, OTHER]);
        assert_eq!(kind, RoomKind::Workspace);
        assert_eq!(kind.direct_peer(), None);
    }

    /// Somebody else's conversation is not this peer's, even though the room is
    /// a direct one. Only a room `me` is a member of can be classified, because
    /// the derivation only matches when one side is `me`.
    #[test]
    fn a_conversation_between_two_other_people_is_not_mine() {
        let theirs = room_with(THEM, OTHER).unwrap();
        assert_eq!(classify(&theirs, ME, [THEM, OTHER]), RoomKind::Workspace);
    }

    /// A second member the roster has not named yet cannot be recovered, and a
    /// guess would put the wrong name at the top of a private conversation.
    #[test]
    fn a_room_whose_other_member_is_unknown_is_not_labelled() {
        let room = room_with(ME, THEM).unwrap();
        assert_eq!(
            classify(&room, ME, [OTHER]),
            RoomKind::Workspace,
            "the second member was not in the roster and was named anyway"
        );
    }

    /// Nothing to classify against.
    #[test]
    fn an_unset_room_or_identity_classifies_as_a_workspace() {
        assert_eq!(classify("", ME, [THEM]), RoomKind::Workspace);
        assert_eq!(classify("anything", "", [THEM]), RoomKind::Workspace);
    }
}
