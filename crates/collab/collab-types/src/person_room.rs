//! The person's own room: the room of records that belong to one person
//! and follow them across their devices, never to a collaborator.
//!
//! A shared room has a roster: people are invited, granted roles, removed.
//! The person's own room has none. Its id is derived from the person's root
//! public key, and its members are, by that derivation, exactly the devices
//! that root certified. Nobody can be invited into it, because there is no
//! list to add them to: a broker admits a join naming it only from a device
//! certified by the same root, and a device sends its records only to its
//! own person's other devices.
//!
//! Watch history, browsing history, learned words and reading positions live
//! here: the person's own data, too large and too much like content for a
//! setting, and never part of a room someone else is in.

use sha2::{Digest as _, Sha256};

/// Domain separation for the derivation, so this id cannot collide with
/// another hash of the same key.
const DOMAIN: &[u8] = b"tilo-person-room-v1\0";

/// The id of the room of the person whose root public key is
/// `user_public_key`: a version-8 uuid every device of that person derives
/// alike, without asking anyone.
#[must_use]
pub fn person_room(user_public_key: &[u8; 32]) -> uuid::Uuid {
    let digest = Sha256::new()
        .chain_update(DOMAIN)
        .chain_update(user_public_key)
        .finalize();
    let mut bytes = [0u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest) {
        *byte = digested;
    }
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// Whether `room` is the room of the person whose root public key is
/// `user_public_key`.
#[must_use]
pub fn is_person_room(user_public_key: &[u8; 32], room: &uuid::Uuid) -> bool {
    person_room(user_public_key) == *room
}

/// The room of the person a [`Principal::Person`](crate::Principal) id names
/// (the lowercase hex of their root public key); `None` for anything else.
#[must_use]
pub fn person_room_of(person: &str) -> Option<uuid::Uuid> {
    let mut key = [0u8; 32];
    hex::decode_to_slice(person, &mut key).ok()?;
    Some(person_room(&key))
}

/// What every person record's id derives from: the record an app opens in
/// a room when nothing names another (`derived:default@tab:0`).
const PERSON_RECORD_BASE: &str = "derived:default@tab:0000000000000000";

/// The id of the person's record of `kind`: one per kind, the same on
/// every device (`ui::record::person::record_id`).
#[must_use]
pub fn person_record_id(kind: &str) -> String {
    format!("derived:{kind}@{PERSON_RECORD_BASE}")
}

/// The kind a person record's id names; `None` for any other record.
#[must_use]
pub fn person_record_kind(record: &str) -> Option<&str> {
    record
        .strip_prefix("derived:")?
        .strip_suffix(PERSON_RECORD_BASE)?
        .strip_suffix('@')
        .filter(|kind| !kind.is_empty() && !kind.contains('@'))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_person_record_names_its_kind() {
        let record = super::person_record_id("terminal.history");
        assert_eq!(
            record,
            "derived:terminal.history@derived:default@tab:0000000000000000"
        );
        assert_eq!(super::person_record_kind(&record), Some("terminal.history"));
        for other in [
            "tab:000000000000005c",
            "derived:default@tab:0000000000000000",
            "derived:a@b@derived:default@tab:0000000000000000",
            "derived:notes@tab:000000000000005c",
        ] {
            assert_eq!(super::person_record_kind(other), None, "{other}");
        }
    }

    use super::*;

    #[test]
    fn a_person_has_one_room_every_device_derives_and_nobody_else_shares() {
        let ada = [7u8; 32];
        let bob = [8u8; 32];
        let room = person_room(&ada);
        assert_eq!(
            room,
            person_room(&ada),
            "every device derives the same room"
        );
        assert_ne!(room, person_room(&bob), "another person has another room");
        assert!(!room.is_nil());
        assert_eq!(room.get_version_num(), 8);
        assert!(is_person_room(&ada, &room));
        assert!(!is_person_room(&bob, &room));
        assert_eq!(person_room_of(&hex::encode(ada)), Some(room));
        assert_eq!(person_room_of("not a key"), None);
        assert_eq!(person_room_of(&hex::encode([1u8; 31])), None);
    }
}
