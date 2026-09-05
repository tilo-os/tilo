//! Registry-owner signed, replay-resistant catalogue snapshots.

use std::collections::HashSet;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{INDEX_DOMAIN, RegistryEntry, verify_entry_signature};

pub const SNAPSHOT_PROTOCOL: u16 = 2;
pub const MAX_SNAPSHOT_VALIDITY_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const MAX_SNAPSHOT_ENTRIES: usize = 10_000;
pub const MAX_SNAPSHOT_PUBLISHERS: usize = 4_096;
pub const MAX_SNAPSHOT_ENCODED_BYTES: usize = 8 * 1024 * 1024;

/// Public policy authorized by the owner. Operator-only notes are deliberately
/// excluded: clients need the authorization, not private bookkeeping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherAuthorization {
    pub pubkey_hex: String,
    pub can_add_new: bool,
}

/// One immutable view of a registry. The signature binds every entry, the
/// publisher authorization set, expiry, predecessor and moderation-log head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRegistrySnapshot {
    pub protocol: u16,
    pub sequence: u64,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub previous_snapshot_hash_hex: String,
    pub moderation_head_hex: String,
    pub publishers: Vec<PublisherAuthorization>,
    pub entries: Vec<RegistryEntry>,
    pub owner_signature_hex: String,
}

#[derive(Serialize)]
struct SnapshotSigningPayload<'a> {
    protocol: u16,
    sequence: u64,
    issued_at_unix: u64,
    expires_at_unix: u64,
    previous_snapshot_hash_hex: &'a str,
    moderation_head_hex: &'a str,
    publishers: &'a [PublisherAuthorization],
    entries: &'a [RegistryEntry],
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("unsupported registry snapshot protocol")]
    Protocol,
    #[error("registry snapshot sequence or predecessor is invalid")]
    Sequence,
    #[error("registry snapshot is expired, not yet valid, or valid for too long")]
    Freshness,
    #[error("registry snapshot exceeds a client safety bound")]
    Oversize,
    #[error("registry snapshot contains malformed or duplicate identities")]
    Shape,
    #[error("registry owner key is invalid")]
    OwnerKey,
    #[error("registry owner signature is invalid")]
    OwnerSignature,
    #[error("an entry has an invalid publisher signature")]
    PublisherSignature,
    #[error("an entry's publisher is not authorized by the registry owner")]
    UnauthorizedPublisher,
    #[error("canonical registry snapshot encoding failed")]
    Encoding,
}

fn payload(snapshot: &SignedRegistrySnapshot) -> SnapshotSigningPayload<'_> {
    SnapshotSigningPayload {
        protocol: snapshot.protocol,
        sequence: snapshot.sequence,
        issued_at_unix: snapshot.issued_at_unix,
        expires_at_unix: snapshot.expires_at_unix,
        previous_snapshot_hash_hex: &snapshot.previous_snapshot_hash_hex,
        moderation_head_hex: &snapshot.moderation_head_hex,
        publishers: &snapshot.publishers,
        entries: &snapshot.entries,
    }
}

/// Digest signed by the registry owner.
///
/// # Errors
/// Returns an encoding error if the canonical payload cannot be serialized.
pub fn snapshot_signing_message(
    snapshot: &SignedRegistrySnapshot,
) -> Result<[u8; 32], SnapshotError> {
    let body = postcard::to_stdvec(&payload(snapshot)).map_err(|_| SnapshotError::Encoding)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(INDEX_DOMAIN);
    hasher.update(&body);
    Ok(*hasher.finalize().as_bytes())
}

/// Hash identifying the exact signed snapshot, used as the next snapshot's
/// predecessor and by clients' durable anti-rollback checkpoints.
///
/// # Errors
/// Returns an encoding error if the signed snapshot cannot be serialized.
pub fn snapshot_hash(snapshot: &SignedRegistrySnapshot) -> Result<String, SnapshotError> {
    let bytes = postcard::to_stdvec(snapshot).map_err(|_| SnapshotError::Encoding)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Fill the detached owner signature. This is intended for offline tooling;
/// the registry serving process never needs an owner private key.
///
/// # Errors
/// Returns an encoding error if the canonical payload cannot be serialized.
pub fn sign_snapshot(
    snapshot: &mut SignedRegistrySnapshot,
    key: &SigningKey,
) -> Result<(), SnapshotError> {
    snapshot.owner_signature_hex.clear();
    snapshot.owner_signature_hex =
        hex::encode(key.sign(&snapshot_signing_message(snapshot)?).to_bytes());
    Ok(())
}

fn valid_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes.saturating_mul(2)
        && hex::decode(value).is_ok_and(|decoded| decoded.len() == bytes)
}

/// Verify owner signature, freshness, structural bounds, publisher signatures,
/// and owner authorization. A successful return is the proof behind the Store's
/// "verified" badge.
///
/// # Errors
/// Returns the precise freshness, shape, signature, or authorization failure.
pub fn verify_snapshot(
    snapshot: &SignedRegistrySnapshot,
    pinned_owner_pubkey_hex: &str,
    now_unix: u64,
) -> Result<(), SnapshotError> {
    if snapshot.protocol != SNAPSHOT_PROTOCOL {
        return Err(SnapshotError::Protocol);
    }
    if snapshot.sequence == 0
        || (snapshot.sequence == 1 && !snapshot.previous_snapshot_hash_hex.is_empty())
        || (snapshot.sequence > 1 && !valid_hex(&snapshot.previous_snapshot_hash_hex, 32))
    {
        return Err(SnapshotError::Sequence);
    }
    if snapshot.issued_at_unix > now_unix.saturating_add(300)
        || snapshot.expires_at_unix < now_unix
        || snapshot.expires_at_unix <= snapshot.issued_at_unix
        || snapshot
            .expires_at_unix
            .saturating_sub(snapshot.issued_at_unix)
            > MAX_SNAPSHOT_VALIDITY_SECONDS
    {
        return Err(SnapshotError::Freshness);
    }
    if snapshot.entries.len() > MAX_SNAPSHOT_ENTRIES
        || snapshot.publishers.len() > MAX_SNAPSHOT_PUBLISHERS
        || snapshot
            .entries
            .iter()
            .any(|entry| !entry_within_bounds(entry))
    {
        return Err(SnapshotError::Oversize);
    }
    if !snapshot.moderation_head_hex.is_empty() && !valid_hex(&snapshot.moderation_head_hex, 32) {
        return Err(SnapshotError::Shape);
    }

    let owner_bytes: [u8; 32] = hex::decode(pinned_owner_pubkey_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(SnapshotError::OwnerKey)?;
    if owner_bytes == [0; 32] {
        return Err(SnapshotError::OwnerKey);
    }
    let owner = VerifyingKey::from_bytes(&owner_bytes).map_err(|_| SnapshotError::OwnerKey)?;
    let signature = Signature::from_slice(
        &hex::decode(&snapshot.owner_signature_hex).map_err(|_| SnapshotError::OwnerSignature)?,
    )
    .map_err(|_| SnapshotError::OwnerSignature)?;
    owner
        .verify(&snapshot_signing_message(snapshot)?, &signature)
        .map_err(|_| SnapshotError::OwnerSignature)?;

    let mut publishers = HashSet::with_capacity(snapshot.publishers.len());
    for publisher in &snapshot.publishers {
        if !valid_hex(&publisher.pubkey_hex, 32)
            || !publishers.insert(publisher.pubkey_hex.to_ascii_lowercase())
        {
            return Err(SnapshotError::Shape);
        }
    }
    let mut packages = HashSet::with_capacity(snapshot.entries.len());
    for entry in &snapshot.entries {
        if entry.package_id.is_empty()
            || entry.package_id.len() > 255
            || !packages.insert(entry.package_id.clone())
        {
            return Err(SnapshotError::Shape);
        }
        if !verify_entry_signature(entry) {
            return Err(SnapshotError::PublisherSignature);
        }
        if !publishers.contains(&entry.publisher_pubkey_hex.to_ascii_lowercase()) {
            return Err(SnapshotError::UnauthorizedPublisher);
        }
    }
    Ok(())
}

fn entry_within_bounds(entry: &RegistryEntry) -> bool {
    entry.package_id.len() <= 255
        && entry.name.len() <= 256
        && entry.description.len() <= 8_192
        && entry.categories.len() <= 64
        && entry
            .categories
            .iter()
            .all(|category| category.len() <= 128)
        && entry
            .icon_url
            .as_ref()
            .is_none_or(|value| value.len() <= 4_096)
        && entry
            .source_url
            .as_ref()
            .is_none_or(|value| value.len() <= 4_096)
        && entry
            .homepage
            .as_ref()
            .is_none_or(|value| value.len() <= 4_096)
        && entry.publisher_pubkey_hex.len() == 64
        && entry.signature_hex.len() == 128
        && entry.versions.len() <= 128
        && entry.versions.iter().all(|version| {
            version.version.len() <= 128
                && version.qpkg_url.len() <= 4_096
                && version.qpkg_root_hash_hex.len() == 64
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RegistryVersion, sign_entry};

    fn fixture() -> (SignedRegistrySnapshot, SigningKey) {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let publisher = SigningKey::from_bytes(&[7; 32]);
        let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
        let mut entry = RegistryEntry {
            package_id: "dev.qos.notes".into(),
            name: "Notes".into(),
            description: "Notes".into(),
            categories: vec!["Productivity".into()],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: publisher_hex.clone(),
            versions: vec![RegistryVersion {
                version: "1".into(),
                qpkg_url: "https://apps.qos.dev/n.qpkg".into(),
                qpkg_root_hash_hex: "ab".repeat(32),
                qpkg_size_bytes: 1,
                released_at_unix: 1,
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &publisher);
        let mut snapshot = SignedRegistrySnapshot {
            protocol: SNAPSHOT_PROTOCOL,
            sequence: 1,
            issued_at_unix: 10,
            expires_at_unix: 20,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: vec![PublisherAuthorization {
                pubkey_hex: publisher_hex,
                can_add_new: true,
            }],
            entries: vec![entry],
            owner_signature_hex: String::new(),
        };
        sign_snapshot(&mut snapshot, &owner).unwrap();
        (snapshot, owner)
    }

    #[test]
    fn exact_snapshot_verifies_and_tamper_fails() {
        let (mut snapshot, owner) = fixture();
        verify_snapshot(
            &snapshot,
            &hex::encode(owner.verifying_key().to_bytes()),
            15,
        )
        .unwrap();
        snapshot.entries[0].name.push_str(" hijacked");
        assert_eq!(
            verify_snapshot(
                &snapshot,
                &hex::encode(owner.verifying_key().to_bytes()),
                15
            ),
            Err(SnapshotError::OwnerSignature)
        );
    }

    #[test]
    fn expired_and_unauthorized_snapshots_fail_closed() {
        let (mut snapshot, owner) = fixture();
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        assert_eq!(
            verify_snapshot(&snapshot, &owner_hex, 21),
            Err(SnapshotError::Freshness)
        );
        snapshot.publishers.clear();
        sign_snapshot(&mut snapshot, &owner).unwrap();
        assert_eq!(
            verify_snapshot(&snapshot, &owner_hex, 15),
            Err(SnapshotError::UnauthorizedPublisher)
        );
    }

    #[test]
    fn oversized_authorization_set_fails_even_with_valid_owner_signature() {
        let (mut snapshot, owner) = fixture();
        snapshot.publishers = (0..=MAX_SNAPSHOT_PUBLISHERS)
            .map(|index| PublisherAuthorization {
                pubkey_hex: format!("{index:064x}"),
                can_add_new: false,
            })
            .collect();
        sign_snapshot(&mut snapshot, &owner).unwrap();
        assert_eq!(
            verify_snapshot(
                &snapshot,
                &hex::encode(owner.verifying_key().to_bytes()),
                15
            ),
            Err(SnapshotError::Oversize)
        );
    }
}
