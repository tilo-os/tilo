//! Where a person's own records live: their personal store's cluster, and the
//! ids of the records it holds.
//!
//! A person's own data — their accounts, their contacts, their Settings and
//! each app's personal state — is kept in their personal store
//! (`tilo_peer_state::PersonalStore`), which every device of theirs holds and
//! no one else reaches. The store needs only one thing agreed on in advance:
//! the cluster its documents are routed under. That cluster is derived from
//! the person's root public key alone, so two of their devices agree on where
//! a document lives without naming each other, and an app's route for one of
//! those records names the same cluster the store carries it on.
//!
//! Nothing is joined to open it: the store admits a write when the device that
//! signed it holds a certificate the root signed, and the root's signature
//! says who may write. There is no roster to consult and no id anyone is
//! admitted into.

use sha2::{Digest as _, Sha256};

/// Domain separation for the derivation, so this id cannot collide with
/// another hash of the same key.
const DOMAIN: &[u8] = b"tilo-personal-store-v1\0";

/// The cluster of the personal store of the person whose root public key is
/// `root`: a version-8 uuid every device of that person derives alike,
/// without asking anyone.
#[must_use]
pub fn personal_cluster(root: &[u8; 32]) -> uuid::Uuid {
    let digest = Sha256::new()
        .chain_update(DOMAIN)
        .chain_update(root)
        .finalize();
    let mut bytes = [0u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest) {
        *byte = digested;
    }
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// Whether `cluster` is the personal store's of the person whose root public
/// key is `root`.
#[must_use]
pub fn is_personal_cluster(root: &[u8; 32], cluster: &uuid::Uuid) -> bool {
    personal_cluster(root) == *cluster
}

/// The personal store's cluster of the person a
/// [`Principal::Person`](crate::Principal) id names (the lowercase hex of
/// their root public key); `None` for anything else.
#[must_use]
pub fn personal_cluster_of(person: &str) -> Option<uuid::Uuid> {
    let mut key = [0u8; 32];
    hex::decode_to_slice(person, &mut key).ok()?;
    Some(personal_cluster(&key))
}

/// The workspace id records of one person were kept under before their store
/// existed: the person room, which no longer exists.
///
/// Read-only, and only by the migration that moves what was stored there into
/// the person's own store (their own scope's records, keyed in the device's
/// own store by this id). Never write under it and never derive it for a new
/// record: a device that never had one starts empty.
#[must_use]
pub fn retired_person_workspace(root: &[u8; 32]) -> uuid::Uuid {
    /// The retired derivation's domain, kept so the old key can be found.
    const RETIRED_DOMAIN: &[u8] = b"tilo-person-room-v1\0";
    let digest = Sha256::new()
        .chain_update(RETIRED_DOMAIN)
        .chain_update(root)
        .finalize();
    let mut bytes = [0u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest) {
        *byte = digested;
    }
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

/// What every person record's id derives from: the app's base record
/// (`tab:0`), whose role the person's record of a kind plays.
const PERSON_RECORD_BASE: &str = "tab:0000000000000000";

/// The id of the person's record of `kind`: one per kind, the same on every
/// device (`ui::record::person::record_id`).
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
    use super::*;

    #[test]
    fn a_person_record_names_its_kind() {
        let record = person_record_id("terminal.history");
        assert_eq!(record, "derived:terminal.history@tab:0000000000000000");
        assert_eq!(person_record_kind(&record), Some("terminal.history"));
        for other in [
            "tab:000000000000005c",
            "tab:0000000000000000",
            "derived:a@b@tab:0000000000000000",
            "derived:terminal.history@derived:default@tab:0000000000000000",
            "derived:notes@tab:000000000000005c",
        ] {
            assert_eq!(person_record_kind(other), None, "{other}");
        }
    }

    /// Every device of a person derives one cluster for their store, nobody
    /// else's derives it, and it is not the workspace their records used to
    /// be kept under.
    #[test]
    fn a_person_has_one_store_every_device_derives_and_nobody_else_shares() {
        let ada = [7u8; 32];
        let bob = [8u8; 32];
        let cluster = personal_cluster(&ada);
        assert_eq!(
            cluster,
            personal_cluster(&ada),
            "every device derives the same cluster"
        );
        assert_ne!(
            cluster,
            personal_cluster(&bob),
            "another person has another store"
        );
        assert!(!cluster.is_nil());
        assert_eq!(cluster.get_version_num(), 8);
        assert!(is_personal_cluster(&ada, &cluster));
        assert!(!is_personal_cluster(&bob, &cluster));
        assert_eq!(personal_cluster_of(&hex::encode(ada)), Some(cluster));
        assert_eq!(personal_cluster_of("not a key"), None);
        assert_eq!(personal_cluster_of(&hex::encode([1u8; 31])), None);
        assert_ne!(
            cluster,
            retired_person_workspace(&ada),
            "the store is not the room its records were kept in"
        );
    }
}
