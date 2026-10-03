//! A direct message, as a room with two people in it.
//!
//! Not a new kind of message: an ordinary conversation in a room whose
//! membership happens to be two. What makes it private is already load-bearing —
//! traffic addressed by `CrdtRoute`, delivery failing closed on a cluster
//! mismatch, a session subscribed to the rooms it is in and to nothing else — so
//! a third person is not routed the bytes rather than kept from them by a
//! rendering pass. What this module adds is which room is a given pair's.
//!
//! The id is derived, not minted like `shell`'s random `RoomId`, whose inputs at
//! mint time are all local facts. A pair's ids are globally unique and held by
//! both ends before the room exists, so deriving removes a race a minted id has:
//! both sides open a conversation at once, both mint, and neither reads the
//! other's. The digest is over the pair sorted, so it is one room whichever way
//! round the pair is named.

use serde::{Deserialize, Serialize};

use crate::delivery::Id;

/// Domain separation for the room-id derivation. Any change here re-points every
/// existing conversation at a room nobody is in, hence the version in the string:
/// a future scheme bumps it instead. The Tilo namespace is intentionally a clean
/// break from the retired product protocol; no legacy room derivation is accepted.
const ROOM_ID_DOMAIN: &[u8] = b"tilo.direct-room.v1";

/// A room with exactly two members — the room a direct message is held in.
///
/// [`DirectRoom::between`] is the only constructor, which is what guarantees the
/// invariants the rest of this module relies on: two distinct members, neither
/// empty, held in lexicographic order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DirectRoom {
    lo: Id,
    hi: Id,
}

impl DirectRoom {
    /// The room `a` and `b` hold their direct conversation in, or `None` when the
    /// pair does not name one: a room with one person in it has nobody to be
    /// private from, and an empty id is what an unresolved participant looks like,
    /// so admitting one would collapse every unresolved pair into a shared room.
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

    /// The two members, in canonical order.
    pub fn members(&self) -> [&str; 2] {
        [&self.lo, &self.hi]
    }

    pub fn contains(&self, id: &str) -> bool {
        self.lo == id || self.hi == id
    }

    /// The other person, given one of them. `None` from outside the room: naming
    /// a member to somebody with no standing to be told is not an answer.
    pub fn other_than(&self, me: &str) -> Option<&str> {
        if me == self.lo {
            Some(&self.hi)
        } else if me == self.hi {
            Some(&self.lo)
        } else {
            None
        }
    }

    /// The room's identity as a uuid string — the value that goes wherever any
    /// other room id goes: `TILO_WORKSPACE_ID`, a `CrdtRoute`'s `cluster_id`,
    /// `shell`'s `RoomId::parse`.
    ///
    /// A domain-separated BLAKE3 digest of the sorted pair, laid out as an RFC
    /// 4122 version 8 uuid — the version reserved for vendor-defined layouts —
    /// with the variant and version bits set so it round-trips through every uuid
    /// parser in the stack. Pure: no clock, no randomness, no local state.
    pub fn room_id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ROOM_ID_DOMAIN);
        // Length-prefixed, not delimiter-joined: a separator byte is a byte an id
        // could contain, and two pairs hashing alike would share one room.
        for part in [self.lo.as_str(), self.hi.as_str()] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        // RFC 4122: version in byte 6's high nibble, variant in byte 8's top bits.
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

    /// One unordered pair names exactly one room, under an id that is stable,
    /// parseable and distinct from every other pair's.
    #[test]
    fn each_unordered_pair_has_one_stable_room_id() {
        // Phase: a pair names one room whichever way round it is given.
        let from_ada = DirectRoom::between(ADA, BOB).expect("a pair names a room");
        let from_bob = DirectRoom::between(BOB, ADA).expect("a pair names a room");
        assert_eq!(from_ada, from_bob);
        assert_eq!(from_ada.room_id(), from_bob.room_id());

        // Phase: the room id is stable and carries no local state. Asserted
        // against a literal so a change to the derivation is one somebody has
        // to make on purpose.
        let room = DirectRoom::between(ADA, BOB).unwrap();
        let id = room.room_id();
        assert_eq!(id, DirectRoom::between(ADA, BOB).unwrap().room_id());
        assert_eq!(
            id, "0b2e5696-17ae-8826-9e6b-13da8e10ab38",
            "the derivation changed; every existing direct room moves with it"
        );
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "a room id that does not parse addresses nobody"
        );

        // Phase: different pairs are different rooms.
        let carol = "u_cccccccccccccccc";
        let ada_bob = DirectRoom::between(ADA, BOB).unwrap().room_id();
        let ada_carol = DirectRoom::between(ADA, carol).unwrap().room_id();
        let bob_carol = DirectRoom::between(BOB, carol).unwrap().room_id();
        assert_ne!(ada_bob, ada_carol);
        assert_ne!(ada_bob, bob_carol);
        assert_ne!(ada_carol, bob_carol);

        // Phase: ids that differ only in where they split are different rooms.
        // Short, unrealistic ids deliberately: the collision has to be exact,
        // and the property under test is the digest's, not the id format's.
        // "a" + "bc"  and  "ab" + "c"  are both "abc" run together.
        let a = DirectRoom::between("a", "bc").unwrap();
        let b = DirectRoom::between("ab", "c").unwrap();
        assert_ne!(
            a.members(),
            b.members(),
            "the fixture must be two real pairs"
        );
        assert_ne!(
            a.room_id(),
            b.room_id(),
            "two pairs collapsed into one room — the digest is not carrying where the pair splits"
        );
    }

    /// A room needs two distinct, non-empty members; it knows them, and tells
    /// a non-member nothing.
    #[test]
    fn a_room_has_two_distinct_members_and_answers_only_about_them() {
        // Case: a person has no direct room with themselves.
        assert_eq!(DirectRoom::between(ADA, ADA), None);

        // Case: an empty id names no room.
        assert_eq!(DirectRoom::between("", BOB), None);
        assert_eq!(DirectRoom::between(ADA, ""), None);

        // Case: a room knows its two members and who the other one is.
        let room = DirectRoom::between(BOB, ADA).unwrap();
        assert_eq!(
            room.members(),
            [ADA, BOB],
            "members come back in pair order"
        );
        assert!(room.contains(ADA) && room.contains(BOB));
        assert_eq!(room.other_than(ADA), Some(BOB));
        assert_eq!(room.other_than(BOB), Some(ADA));

        // Case: a non-member is told nothing about who is in the room.
        let room = DirectRoom::between(ADA, BOB).unwrap();
        assert_eq!(room.other_than("u_cccccccccccccccc"), None);
        assert!(!room.contains("u_cccccccccccccccc"));
    }
}
