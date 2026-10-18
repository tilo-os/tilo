//! Registry-owner signed, replay-resistant catalogue snapshots.
//!
//! A snapshot lists app and bot entries alike under `entries`; the owner
//! signature and the snapshot's hash (its identity in the chain and in
//! client checkpoints) cover all of them.

use std::collections::HashSet;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{INDEX_DOMAIN, PACKAGE_FORMAT_VERSION, RegistryEntry, verify_entry_signature};

const CHANNEL_AUTHORIZATION_DOMAIN: &[u8] = b"tilo-registry-channel-authorization-v1\0";
const DELEGATED_INDEX_DOMAIN: &[u8] = b"tilo-registry-index-v3\0";
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

/// Root-signed authority for one short-lived online channel key.
///
/// The exact publisher policy and safety bounds are part of the certificate,
/// so possession of the delegated key cannot add publishers or widen policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelAuthorization {
    pub protocol: u32,
    pub generation: u64,
    pub channel_pubkey_hex: String,
    pub not_before_unix: u64,
    pub expires_at_unix: u64,
    pub max_snapshot_validity_seconds: u64,
    pub max_snapshot_entries: usize,
    pub publishers: Vec<PublisherAuthorization>,
    pub owner_signature_hex: String,
}

/// One immutable view of a registry. The signature binds every entry, the
/// publisher authorization set, expiry, predecessor and moderation-log head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRegistrySnapshot {
    pub protocol: u32,
    pub sequence: u64,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub previous_snapshot_hash_hex: String,
    pub moderation_head_hex: String,
    pub publishers: Vec<PublisherAuthorization>,
    /// App and bot entries.
    pub entries: Vec<RegistryEntry>,
    pub owner_signature_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_authorization: Option<ChannelAuthorization>,
}

#[derive(Serialize)]
struct SnapshotSigningPayload<'a> {
    protocol: u32,
    sequence: u64,
    issued_at_unix: u64,
    expires_at_unix: u64,
    previous_snapshot_hash_hex: &'a str,
    moderation_head_hex: &'a str,
    publishers: &'a [PublisherAuthorization],
    entries: &'a [RegistryEntry],
}

#[derive(Serialize)]
struct DelegatedSnapshotSigningPayload<'a> {
    protocol: u32,
    sequence: u64,
    issued_at_unix: u64,
    expires_at_unix: u64,
    previous_snapshot_hash_hex: &'a str,
    moderation_head_hex: &'a str,
    publishers: &'a [PublisherAuthorization],
    entries: &'a [RegistryEntry],
    channel_authorization: &'a ChannelAuthorization,
}

#[derive(Serialize)]
struct ChannelAuthorizationPayload<'a> {
    protocol: u32,
    generation: u64,
    channel_pubkey_hex: &'a str,
    not_before_unix: u64,
    expires_at_unix: u64,
    max_snapshot_validity_seconds: u64,
    max_snapshot_entries: usize,
    publishers: &'a [PublisherAuthorization],
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("registry snapshot is not at this device's package format version")]
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
    #[error("registry channel authorization is invalid")]
    ChannelAuthorization,
    #[error("registry channel signature or delegated scope is invalid")]
    ChannelSignature,
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
    let (domain, body) = if let Some(authorization) = &snapshot.channel_authorization {
        let payload = DelegatedSnapshotSigningPayload {
            protocol: snapshot.protocol,
            sequence: snapshot.sequence,
            issued_at_unix: snapshot.issued_at_unix,
            expires_at_unix: snapshot.expires_at_unix,
            previous_snapshot_hash_hex: &snapshot.previous_snapshot_hash_hex,
            moderation_head_hex: &snapshot.moderation_head_hex,
            publishers: &snapshot.publishers,
            entries: &snapshot.entries,
            channel_authorization: authorization,
        };
        (
            DELEGATED_INDEX_DOMAIN,
            postcard::to_stdvec(&payload).map_err(|_| SnapshotError::Encoding)?,
        )
    } else {
        (
            INDEX_DOMAIN,
            postcard::to_stdvec(&payload(snapshot)).map_err(|_| SnapshotError::Encoding)?,
        )
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
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
    snapshot.protocol = PACKAGE_FORMAT_VERSION;
    snapshot.channel_authorization = None;
    snapshot.owner_signature_hex.clear();
    snapshot.owner_signature_hex =
        hex::encode(key.sign(&snapshot_signing_message(snapshot)?).to_bytes());
    Ok(())
}

fn authorization_signing_message(
    authorization: &ChannelAuthorization,
) -> Result<[u8; 32], SnapshotError> {
    let body = postcard::to_stdvec(&ChannelAuthorizationPayload {
        protocol: authorization.protocol,
        generation: authorization.generation,
        channel_pubkey_hex: &authorization.channel_pubkey_hex,
        not_before_unix: authorization.not_before_unix,
        expires_at_unix: authorization.expires_at_unix,
        max_snapshot_validity_seconds: authorization.max_snapshot_validity_seconds,
        max_snapshot_entries: authorization.max_snapshot_entries,
        publishers: &authorization.publishers,
    })
    .map_err(|_| SnapshotError::Encoding)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(CHANNEL_AUTHORIZATION_DOMAIN);
    hasher.update(&body);
    Ok(*hasher.finalize().as_bytes())
}

/// Root-sign a narrowly scoped channel authorization.
///
/// # Errors
/// Returns an encoding error if the canonical authorization cannot be serialized.
pub fn sign_channel_authorization(
    authorization: &mut ChannelAuthorization,
    owner_key: &SigningKey,
) -> Result<(), SnapshotError> {
    authorization.owner_signature_hex.clear();
    authorization.owner_signature_hex = hex::encode(
        owner_key
            .sign(&authorization_signing_message(authorization)?)
            .to_bytes(),
    );
    Ok(())
}

/// Sign a snapshot with a root-authorized channel key.
///
/// # Errors
/// Returns an encoding error if the canonical snapshot cannot be serialized.
pub fn sign_snapshot_delegated(
    snapshot: &mut SignedRegistrySnapshot,
    channel_key: &SigningKey,
    authorization: ChannelAuthorization,
) -> Result<(), SnapshotError> {
    snapshot.protocol = PACKAGE_FORMAT_VERSION;
    snapshot.channel_authorization = Some(authorization);
    snapshot.owner_signature_hex.clear();
    snapshot.owner_signature_hex = hex::encode(
        channel_key
            .sign(&snapshot_signing_message(snapshot)?)
            .to_bytes(),
    );
    Ok(())
}

pub(crate) fn valid_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes.saturating_mul(2)
        && hex::decode(value).is_ok_and(|decoded| decoded.len() == bytes)
}

/// Whether a snapshot issued at `issued_at_unix` and expiring at
/// `expires_at_unix` may be used at `now_unix`: issued no further ahead than
/// the clock skew a verifier tolerates, and not yet expired. The only part
/// of [`verify_snapshot`] and [`verify_compact_snapshot`] that depends on
/// the time, so a server that verified a snapshot once re-checks just this.
#[must_use]
pub fn within_validity_window(issued_at_unix: u64, expires_at_unix: u64, now_unix: u64) -> bool {
    issued_at_unix <= now_unix.saturating_add(300) && expires_at_unix >= now_unix
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
    if snapshot.protocol != PACKAGE_FORMAT_VERSION {
        return Err(SnapshotError::Protocol);
    }
    let header = SignedHeader {
        sequence: snapshot.sequence,
        issued_at_unix: snapshot.issued_at_unix,
        expires_at_unix: snapshot.expires_at_unix,
        previous_snapshot_hash_hex: &snapshot.previous_snapshot_hash_hex,
        moderation_head_hex: &snapshot.moderation_head_hex,
        publishers: &snapshot.publishers,
        entry_count: snapshot.entries.len(),
        signature_hex: &snapshot.owner_signature_hex,
        channel_authorization: snapshot.channel_authorization.as_ref(),
    };
    header.check_window(now_unix)?;
    if snapshot
        .entries
        .iter()
        .any(|entry| !entry_within_bounds(entry))
    {
        return Err(SnapshotError::Oversize);
    }
    let publishers = header.check_signature(
        pinned_owner_pubkey_hex,
        &snapshot_signing_message(snapshot)?,
    )?;
    let mut packages = HashSet::with_capacity(snapshot.entries.len());
    for entry in &snapshot.entries {
        check_entry_identity(entry, &mut packages, &publishers)?;
        if !verify_entry_signature(entry) {
            return Err(SnapshotError::PublisherSignature);
        }
    }
    Ok(())
}

/// The signed fields every snapshot format shares, borrowed for verification.
#[derive(Clone, Copy)]
pub(crate) struct SignedHeader<'a> {
    pub sequence: u64,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub previous_snapshot_hash_hex: &'a str,
    pub moderation_head_hex: &'a str,
    pub publishers: &'a [PublisherAuthorization],
    pub entry_count: usize,
    pub signature_hex: &'a str,
    pub channel_authorization: Option<&'a ChannelAuthorization>,
}

impl SignedHeader<'_> {
    /// Sequence shape, freshness and size bounds.
    pub(crate) fn check_window(&self, now_unix: u64) -> Result<(), SnapshotError> {
        if self.sequence == 0
            || (self.sequence == 1 && !self.previous_snapshot_hash_hex.is_empty())
            || (self.sequence > 1 && !valid_hex(self.previous_snapshot_hash_hex, 32))
        {
            return Err(SnapshotError::Sequence);
        }
        if !within_validity_window(self.issued_at_unix, self.expires_at_unix, now_unix)
            || self.expires_at_unix <= self.issued_at_unix
            || self.expires_at_unix.saturating_sub(self.issued_at_unix)
                > MAX_SNAPSHOT_VALIDITY_SECONDS
        {
            return Err(SnapshotError::Freshness);
        }
        if self.entry_count > MAX_SNAPSHOT_ENTRIES
            || self.publishers.len() > MAX_SNAPSHOT_PUBLISHERS
        {
            return Err(SnapshotError::Oversize);
        }
        Ok(())
    }

    /// The owner's or the authorized channel's signature over `message`, and
    /// the shape of the publisher set. Returns the authorized publishers.
    pub(crate) fn check_signature(
        &self,
        pinned_owner_pubkey_hex: &str,
        message: &[u8; 32],
    ) -> Result<HashSet<String>, SnapshotError> {
        if !self.moderation_head_hex.is_empty() && !valid_hex(self.moderation_head_hex, 32) {
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
            &hex::decode(self.signature_hex).map_err(|_| SnapshotError::OwnerSignature)?,
        )
        .map_err(|_| SnapshotError::OwnerSignature)?;

        if let Some(authorization) = self.channel_authorization {
            verify_channel_authorization_scope(authorization, &owner, &self.scope())?;
            let channel_bytes: [u8; 32] = hex::decode(&authorization.channel_pubkey_hex)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or(SnapshotError::ChannelAuthorization)?;
            let channel = VerifyingKey::from_bytes(&channel_bytes)
                .map_err(|_| SnapshotError::ChannelAuthorization)?;
            channel
                .verify(message, &signature)
                .map_err(|_| SnapshotError::ChannelSignature)?;
        } else {
            owner
                .verify(message, &signature)
                .map_err(|_| SnapshotError::OwnerSignature)?;
        }

        let mut publishers = HashSet::with_capacity(self.publishers.len());
        for publisher in self.publishers {
            if !valid_hex(&publisher.pubkey_hex, 32)
                || !publishers.insert(publisher.pubkey_hex.to_ascii_lowercase())
            {
                return Err(SnapshotError::Shape);
            }
        }
        Ok(publishers)
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            issued_at_unix: self.issued_at_unix,
            expires_at_unix: self.expires_at_unix,
            entry_count: self.entry_count,
            publishers: self.publishers,
        }
    }
}

/// A unique, bounded package id from an authorized publisher.
pub(crate) fn check_entry_identity(
    entry: &RegistryEntry,
    packages: &mut HashSet<String>,
    publishers: &HashSet<String>,
) -> Result<(), SnapshotError> {
    if entry.package_id.is_empty()
        || entry.package_id.len() > 255
        || !packages.insert(entry.package_id.clone())
    {
        return Err(SnapshotError::Shape);
    }
    if !publishers.contains(&entry.publisher_pubkey_hex.to_ascii_lowercase()) {
        return Err(SnapshotError::UnauthorizedPublisher);
    }
    Ok(())
}

/// What a channel authorization bounds.
struct Scope<'a> {
    issued_at_unix: u64,
    expires_at_unix: u64,
    entry_count: usize,
    publishers: &'a [PublisherAuthorization],
}

fn verify_channel_authorization_scope(
    authorization: &ChannelAuthorization,
    owner: &VerifyingKey,
    snapshot: &Scope<'_>,
) -> Result<(), SnapshotError> {
    if authorization.protocol != PACKAGE_FORMAT_VERSION
        || authorization.generation == 0
        || !valid_hex(&authorization.channel_pubkey_hex, 32)
        || authorization.not_before_unix >= authorization.expires_at_unix
        || authorization.max_snapshot_validity_seconds == 0
        || authorization.max_snapshot_validity_seconds > MAX_SNAPSHOT_VALIDITY_SECONDS
        || authorization.max_snapshot_entries == 0
        || authorization.max_snapshot_entries > MAX_SNAPSHOT_ENTRIES
        || snapshot.issued_at_unix < authorization.not_before_unix
        || snapshot.expires_at_unix > authorization.expires_at_unix
        || snapshot
            .expires_at_unix
            .saturating_sub(snapshot.issued_at_unix)
            > authorization.max_snapshot_validity_seconds
        || snapshot.entry_count > authorization.max_snapshot_entries
        || snapshot.publishers != authorization.publishers.as_slice()
    {
        return Err(SnapshotError::ChannelAuthorization);
    }
    let mut previous = None;
    for publisher in &authorization.publishers {
        if !valid_hex(&publisher.pubkey_hex, 32)
            || previous
                .as_ref()
                .is_some_and(|prior: &String| prior >= &publisher.pubkey_hex)
        {
            return Err(SnapshotError::ChannelAuthorization);
        }
        previous = Some(publisher.pubkey_hex.clone());
    }
    let signature = Signature::from_slice(
        &hex::decode(&authorization.owner_signature_hex)
            .map_err(|_| SnapshotError::ChannelAuthorization)?,
    )
    .map_err(|_| SnapshotError::ChannelAuthorization)?;
    owner
        .verify(&authorization_signing_message(authorization)?, &signature)
        .map_err(|_| SnapshotError::ChannelAuthorization)
}

/// Verify a root-signed channel certificate before an online key is used.
///
/// # Errors
/// Returns a key, signature, shape, scope, or freshness error when the
/// authorization is not currently valid for this registry owner.
pub fn verify_channel_authorization(
    authorization: &ChannelAuthorization,
    pinned_owner_pubkey_hex: &str,
    now_unix: u64,
) -> Result<(), SnapshotError> {
    let owner_bytes: [u8; 32] = hex::decode(pinned_owner_pubkey_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(SnapshotError::OwnerKey)?;
    let owner = VerifyingKey::from_bytes(&owner_bytes).map_err(|_| SnapshotError::OwnerKey)?;
    if now_unix < authorization.not_before_unix || now_unix > authorization.expires_at_unix {
        return Err(SnapshotError::ChannelAuthorization);
    }
    verify_channel_authorization_scope(
        authorization,
        &owner,
        &Scope {
            issued_at_unix: now_unix,
            expires_at_unix: now_unix.saturating_add(1),
            entry_count: 0,
            publishers: &authorization.publishers,
        },
    )
}

pub(crate) fn entry_within_bounds(entry: &RegistryEntry) -> bool {
    entry.validate().is_ok()
        && entry.package_id.len() <= 255
        && entry.categories.len() <= 64
        && entry
            .categories
            .iter()
            .all(|category| category.len() <= 128)
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
                && match &version.artifact {
                    crate::VersionArtifact::Tapp(tapp) => {
                        tapp.tapp_url.len() <= 4_096
                            && tapp.tapp_root_hash_hex.len() == 64
                            && tapp
                                .tapp_root_hash_hex
                                .bytes()
                                .all(|byte| byte.is_ascii_hexdigit())
                            && tapp.tapp_blake3_hex.len() == 64
                            && tapp
                                .tapp_blake3_hex
                                .bytes()
                                .all(|byte| byte.is_ascii_hexdigit())
                            && tapp.target.isa.len() <= crate::target::MAX_TARGET_ISA_BYTES
                            && tapp.target.runtime.as_ref().is_none_or(|runtime| {
                                runtime.len() <= crate::target::MAX_TARGET_RUNTIME_BYTES
                            })
                    }
                    crate::VersionArtifact::Bot(bot) => {
                        bot.program_url.len() <= 4_096
                            && bot.program_blake3_hex.len() == 64
                            && bot.program_size_bytes <= crate::entry::MAX_BOT_PROGRAM_BYTES
                    }
                }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, RegistryVersion, TappArtifact, VersionArtifact, sign_entry};

    fn fixture() -> (SignedRegistrySnapshot, SigningKey) {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let publisher = SigningKey::from_bytes(&[7; 32]);
        let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
        let mut entry = RegistryEntry {
            package_id: "so.tilo.notes".into(),
            kind: EntryKind::App,
            name: "Notes".into(),
            description: "Notes".into(),
            tagline: "Write together".into(),
            images: crate::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: publisher_hex.clone(),
            versions: vec![RegistryVersion {
                version: "1".into(),
                released_at_unix: 1,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url: "https://apps.tilo.so/n.tapp".into(),
                    tapp_root_hash_hex: "ab".repeat(32),
                    tapp_size_bytes: 1,
                    tapp_blake3_hex: "cd".repeat(32),
                    target: crate::RegistryTarget {
                        platform: crate::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &publisher);
        let mut snapshot = SignedRegistrySnapshot {
            protocol: PACKAGE_FORMAT_VERSION,
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
            channel_authorization: None,
        };
        sign_snapshot(&mut snapshot, &owner).unwrap();
        (snapshot, owner)
    }

    fn with_bot(snapshot: &SignedRegistrySnapshot) -> SignedRegistrySnapshot {
        let publisher = SigningKey::from_bytes(&[7; 32]);
        let hash = "ef".repeat(32);
        let mut bot = RegistryEntry {
            package_id: "so.tilo.bots.greeter".into(),
            kind: EntryKind::Bot,
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                released_at_unix: 1,
                artifact: VersionArtifact::Bot(crate::BotArtifact {
                    program_url: crate::bot_program_url("https://apps.tilo.so", &hash),
                    program_blake3_hex: hash,
                    program_size_bytes: 10,
                    requires: crate::BotRequirements::default(),
                }),
            }],
            ..snapshot.entries[0].clone()
        };
        sign_entry(&mut bot, &publisher);
        let mut mixed = snapshot.clone();
        mixed.entries.insert(0, bot);
        mixed
    }

    #[test]
    fn bot_entries_are_signed_with_the_app_entries() {
        let (plain, owner) = fixture();
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let mut mixed = with_bot(&plain);
        sign_snapshot(&mut mixed, &owner).unwrap();
        assert_eq!(verify_snapshot(&mixed, &owner_hex, 15), Ok(()));
        // phase: bots_change_the_signature_and_the_identity
        {
            assert_ne!(mixed.owner_signature_hex, plain.owner_signature_hex);
            assert_ne!(snapshot_hash(&mixed), snapshot_hash(&plain));
        }
        // phase: wire_round_trip_keeps_both_kinds
        {
            let json = serde_json::to_value(&mixed).unwrap();
            assert_eq!(json["entries"].as_array().unwrap().len(), 2);
            let read: SignedRegistrySnapshot = serde_json::from_value(json).unwrap();
            assert_eq!(read, mixed);
            assert_eq!(verify_snapshot(&read, &owner_hex, 15), Ok(()));
        }
        // phase: a_dropped_or_added_bot_breaks_the_signature
        {
            let mut dropped = mixed.clone();
            dropped.entries.retain(|entry| entry.kind == EntryKind::App);
            assert_eq!(
                verify_snapshot(&dropped, &owner_hex, 15),
                Err(SnapshotError::OwnerSignature)
            );
            let mut added = with_bot(&plain);
            added.owner_signature_hex = plain.owner_signature_hex.clone();
            assert_eq!(
                verify_snapshot(&added, &owner_hex, 15),
                Err(SnapshotError::OwnerSignature)
            );
        }
    }

    #[test]
    fn snapshot_verification_fails_closed() {
        // phase: exact_snapshot_verifies_and_tamper_fails
        {
            let (mut snapshot, owner) = fixture();
            verify_snapshot(
                &snapshot,
                &hex::encode(owner.verifying_key().to_bytes()),
                15,
            )
            .unwrap();
            snapshot.entries[0]
                .name
                .0
                .get_mut("en")
                .unwrap()
                .push_str(" hijacked");
            assert_eq!(
                verify_snapshot(
                    &snapshot,
                    &hex::encode(owner.verifying_key().to_bytes()),
                    15
                ),
                Err(SnapshotError::OwnerSignature)
            );
        }
        // phase: another_package_format_version_is_refused
        {
            // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
            let (mut snapshot, owner) = fixture();
            let owner_hex = hex::encode(owner.verifying_key().to_bytes());
            for other in [0, 1, 2, 3, 4, 5] {
                if other == PACKAGE_FORMAT_VERSION {
                    continue;
                }
                snapshot.protocol = other;
                assert_eq!(
                    verify_snapshot(&snapshot, &owner_hex, 15),
                    Err(SnapshotError::Protocol),
                    "nothing is read at package format {other}"
                );
            }
        }
        // phase: expired_and_unauthorized_snapshots_fail_closed
        {
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
        // phase: oversized_authorization_set_fails_even_with_valid_owner_signature
        {
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
        // phase: delegated_snapshot_is_root_scoped_and_time_bounded
        {
            let (mut snapshot, owner) = fixture();
            let channel = SigningKey::from_bytes(&[9; 32]);
            let mut authorization = ChannelAuthorization {
                protocol: PACKAGE_FORMAT_VERSION,
                generation: 1,
                channel_pubkey_hex: hex::encode(channel.verifying_key().to_bytes()),
                not_before_unix: 5,
                expires_at_unix: 30,
                max_snapshot_validity_seconds: 15,
                max_snapshot_entries: 10,
                publishers: snapshot.publishers.clone(),
                owner_signature_hex: String::new(),
            };
            sign_channel_authorization(&mut authorization, &owner).unwrap();
            sign_snapshot_delegated(&mut snapshot, &channel, authorization.clone()).unwrap();
            let owner_hex = hex::encode(owner.verifying_key().to_bytes());
            verify_snapshot(&snapshot, &owner_hex, 15).unwrap();

            let mut widened = snapshot.clone();
            widened.publishers.push(PublisherAuthorization {
                pubkey_hex: "ab".repeat(32),
                can_add_new: true,
            });
            sign_snapshot_delegated(&mut widened, &channel, authorization.clone()).unwrap();
            assert_eq!(
                verify_snapshot(&widened, &owner_hex, 15),
                Err(SnapshotError::ChannelAuthorization)
            );

            let mut forged = authorization;
            forged.max_snapshot_entries = 100;
            sign_snapshot_delegated(&mut snapshot, &channel, forged).unwrap();
            assert_eq!(
                verify_snapshot(&snapshot, &owner_hex, 15),
                Err(SnapshotError::ChannelAuthorization)
            );
        }
    }
}
