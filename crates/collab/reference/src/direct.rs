//! A direct message, as a room with two people in it.
//!
//! ## Why a room, and not a private message path
//!
//! qOS puts collaboration *below* applications: a workspace carries its own
//! collaborators, permissions and host, and an app receives that rather than
//! implementing it. Chat has already broken that rule once — it invented its
//! own `Workspace`, its own roster and its own `Presence` beside the ones the
//! OS owns — so the temptation here is the one to resist. A "private message"
//! feature inside chat would be a third parallel concept, with its own
//! membership, its own delivery and its own set of ways to leak.
//!
//! A direct message is not a new kind of message. It is an **ordinary
//! conversation in a room whose membership happens to be two**. Everything
//! that makes a room private is already built and already load-bearing: the
//! room is the collaboration cluster, its traffic is addressed by
//! `CrdtRoute`, delivery fails closed on a cluster mismatch, and the peer
//! network subscribes a session to the rooms it is in and to nothing else. A
//! third person in the workspace is not in this room, so the bytes are not
//! routed to them — not hidden from them by a rendering pass.
//!
//! That is the whole design. What this module adds is the one thing the room
//! machinery does not already answer: **which** room is the room for a given
//! pair of people.
//!
//! ## The id is derived, and that is not a contradiction
//!
//! `shell`'s `RoomId` is a random v4 uuid, and says so at length: the identity
//! has to be unique between machines that have never met and have no channel
//! to negotiate over, and every derivation available at mint time — a local
//! counter, the hostname, the user name, the wall clock — is either not unique
//! or needs exactly the coordination being avoided.
//!
//! Every input in that list is a *local* fact. A direct room's inputs are not:
//! they are the two participants' own ids, each one derived from an Ed25519
//! public key, globally unique, and — this is the part that matters — **already
//! held by both ends before the room exists**. Ada can name Bob and Bob can
//! name Ada without either of them having spoken. So the case the random id was
//! chosen to solve does not arise here, and deriving buys the property a random
//! id cannot have:
//!
//! > Two people cannot end up in two different direct rooms with each other.
//!
//! A minted id would need one side to create the room and the other to be told
//! which one it was, which is a race with a real losing branch: both sides open
//! a conversation at once, both mint, and each writes into a room the other is
//! not reading. With the id derived from the pair there is nothing to race —
//! [`DirectRoom::between`] is a pure function, and "find the existing one
//! rather than make a second" is answered by construction rather than by a
//! lookup that can miss.
//!
//! The derivation is over the *canonical* pair (the two ids sorted), so
//! `between(ada, bob)` and `between(bob, ada)` are the same room, which is the
//! same requirement stated from the other side.

use serde::{Deserialize, Serialize};

use crate::delivery::Id;

/// Domain separation for the room-id derivation. Any change to this string
/// changes every derived room id, so it carries a version: a future scheme
/// bumps it rather than silently re-pointing existing conversations at rooms
/// nobody is in.
const ROOM_ID_DOMAIN: &[u8] = b"qos.direct-room.v1";

/// A room with exactly two members — the room a direct message is held in.
///
/// Constructed only through [`DirectRoom::between`], which is what guarantees
/// the invariants the rest of this module relies on: two distinct members,
/// neither of them empty, held in a canonical order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DirectRoom {
    /// The lexicographically smaller member id.
    lo: Id,
    /// The lexicographically larger member id.
    hi: Id,
}

impl DirectRoom {
    /// The room `a` and `b` hold their direct conversation in, or `None` when
    /// the pair does not name one.
    ///
    /// Refused, rather than fudged, in two cases:
    ///
    /// * **`a == b`.** A room with one person in it is not a direct message,
    ///   and calling it one would attach a privacy claim to something that has
    ///   nobody to be private from. A note to yourself is a real thing to want
    ///   and it is not this; giving it a "direct room" would put a
    ///   confidentiality boundary on screen that means nothing.
    /// * **either id is empty.** An empty id is what an unresolved participant
    ///   looks like, and admitting it would collapse every unresolved pair into
    ///   one shared room — the failure that is worst precisely because it looks
    ///   like it worked.
    pub fn between(a: &str, b: &str) -> Option<Self> {
        if a.is_empty() || b.is_empty() || a == b {
            return None;
        }
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        Some(Self {
            lo: lo.to_string(),
            hi: hi.to_string(),
        })
    }

    /// The two members, in canonical order. Always exactly two, always
    /// distinct — [`Self::between`] is the only way to build one.
    pub fn members(&self) -> [&str; 2] {
        [&self.lo, &self.hi]
    }

    /// Whether `id` is one of the two people in this room.
    pub fn contains(&self, id: &str) -> bool {
        self.lo == id || self.hi == id
    }

    /// The other person, given one of them. `None` when `me` is not a member —
    /// asking who the *other* party is from outside the room has no answer, and
    /// returning one of the two members anyway would name a person to somebody
    /// with no standing to be told.
    pub fn other_than(&self, me: &str) -> Option<&str> {
        if me == self.lo {
            Some(&self.hi)
        } else if me == self.hi {
            Some(&self.lo)
        } else {
            None
        }
    }

    /// The room's identity, as a uuid string — the value that goes where any
    /// other room id goes: `QOS_WORKSPACE_ID`, a `CrdtRoute`'s `cluster_id`,
    /// `shell`'s `RoomId::parse`.
    ///
    /// Derived from the canonical pair with a domain-separated BLAKE3 digest,
    /// laid out as an RFC 4122 version 8 uuid — the version reserved for
    /// vendor-defined layouts, which is exactly what this is. The variant and
    /// version bits are set so the value round-trips through every uuid parser
    /// in the stack rather than being a uuid-shaped string that some of them
    /// reject.
    ///
    /// Pure: no clock, no randomness, no local state. Two machines that have
    /// never met compute the same answer, which is the whole point.
    pub fn room_id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ROOM_ID_DOMAIN);
        // Length-prefixed rather than delimiter-joined. A separator byte is a
        // byte an id could contain, and two different pairs hashing to one room
        // would put two conversations in one place.
        for part in [self.lo.as_str(), self.hi.as_str()] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        // RFC 4122: version in the high nibble of byte 6, variant in the top
        // two bits of byte 8.
        bytes[6] = (bytes[6] & 0x0f) | 0x80; // version 8 — custom
        bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
        uuid::Uuid::from_bytes(bytes).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADA: &str = "u_1f4c3a9b2d7e5061";
    const BOB: &str = "u_9a0b1c2d3e4f5061";

    /// The property the whole design rests on: whichever way round the pair is
    /// named, it is one room. Without this, "open a DM with Bob" and "open a DM
    /// with Ada" are two different rooms and neither person reads the other.
    #[test]
    fn a_pair_names_one_room_whichever_way_round_it_is_given() {
        let from_ada = DirectRoom::between(ADA, BOB).expect("a pair names a room");
        let from_bob = DirectRoom::between(BOB, ADA).expect("a pair names a room");
        assert_eq!(from_ada, from_bob);
        assert_eq!(from_ada.room_id(), from_bob.room_id());
    }

    /// Derived, so it is the same on every machine and in every process — the
    /// substitute for a negotiation neither side can hold before the room
    /// exists. Asserted against a literal so a change to the derivation is a
    /// change somebody has to make on purpose: every existing conversation
    /// moves to a room nobody is in.
    #[test]
    fn the_room_id_is_stable_and_carries_no_local_state() {
        let room = DirectRoom::between(ADA, BOB).unwrap();
        let id = room.room_id();
        assert_eq!(id, DirectRoom::between(ADA, BOB).unwrap().room_id());
        assert_eq!(
            id, "37046bec-72ae-88dc-a787-2728deaa6095",
            "the derivation changed; every existing direct room moves with it"
        );
        // It has to be a room id everywhere a room id is taken.
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "a room id that does not parse addresses nobody"
        );
    }

    /// Different pairs are different rooms. The failure this rules out is the
    /// one that looks like success: two conversations delivered into one place.
    #[test]
    fn different_pairs_are_different_rooms() {
        let carol = "u_cccccccccccccccc";
        let ada_bob = DirectRoom::between(ADA, BOB).unwrap().room_id();
        let ada_carol = DirectRoom::between(ADA, carol).unwrap().room_id();
        let bob_carol = DirectRoom::between(BOB, carol).unwrap().room_id();
        assert_ne!(ada_bob, ada_carol);
        assert_ne!(ada_bob, bob_carol);
        assert_ne!(ada_carol, bob_carol);
    }

    /// Two pairs whose ids run together into the same string. Concatenating
    /// them and hashing that would make these one room — two people's
    /// conversation delivered into a third pair's. Short, unrealistic ids
    /// deliberately: the collision has to be exact, and the property under test
    /// is the digest's, not the id format's.
    #[test]
    fn ids_that_differ_only_in_where_they_split_are_different_rooms() {
        // "a" + "bc"  and  "ab" + "c"  are both "abc" run together.
        let a = DirectRoom::between("a", "bc").unwrap();
        let b = DirectRoom::between("ab", "c").unwrap();
        assert_ne!(a.members(), b.members(), "the fixture must be two real pairs");
        assert_ne!(
            a.room_id(),
            b.room_id(),
            "two pairs collapsed into one room — the digest is not carrying where the pair splits"
        );
    }

    /// A room with one person in it is not a direct message.
    #[test]
    fn a_person_has_no_direct_room_with_themselves() {
        assert_eq!(DirectRoom::between(ADA, ADA), None);
    }

    /// An unresolved participant is what an empty id looks like, and admitting
    /// one would collapse every unresolved pair into a single shared room.
    #[test]
    fn an_empty_id_names_no_room() {
        assert_eq!(DirectRoom::between("", BOB), None);
        assert_eq!(DirectRoom::between(ADA, ""), None);
    }

    #[test]
    fn a_room_knows_its_two_members_and_who_the_other_one_is() {
        let room = DirectRoom::between(BOB, ADA).unwrap();
        assert_eq!(room.members(), [ADA, BOB], "members come back in pair order");
        assert!(room.contains(ADA) && room.contains(BOB));
        assert_eq!(room.other_than(ADA), Some(BOB));
        assert_eq!(room.other_than(BOB), Some(ADA));
    }

    /// Asking from outside the room has no answer, and inventing one would name
    /// a member to somebody with no standing to be told.
    #[test]
    fn a_non_member_is_told_nothing_about_who_is_in_the_room() {
        let room = DirectRoom::between(ADA, BOB).unwrap();
        assert_eq!(room.other_than("u_cccccccccccccccc"), None);
        assert!(!room.contains("u_cccccccccccccccc"));
    }
}
