//! Compact catalogue snapshots and per-entry version indexes.
//!
//! A full snapshot carries every retained version of every entry. A compact
//! snapshot carries, per entry, only
//! the newest version of each target and release channel
//! ([`newest_versions`]), plus the BLAKE3 hash and size of that entry's
//! version index: a static file holding the complete publisher-signed entry.
//! Clients fetch an index only to install or roll back to an older version
//! and check it with [`verify_version_index`] against the signed hash.
//!
//! The registry signs both forms at every sequence with the same key. The
//! compact form signs the hash of its full sibling as `snapshot_hash_hex`;
//! that hash is the snapshot's identity in the chain and in client
//! checkpoints, so a client may switch between the forms without tripping
//! its rollback or fork checks.
//!
//! Both forms, the version indexes and the channel authorizations are part
//! of the Store package format and carry its one version,
//! [`crate::PACKAGE_FORMAT_VERSION`], in their `protocol` field; a reader
//! accepts that number only.

use std::collections::HashSet;

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::snapshot::{SignedHeader, check_entry_identity, entry_within_bounds, valid_hex};
use crate::{
    ChannelAuthorization, PACKAGE_FORMAT_VERSION, PublisherAuthorization, RegistryEntry,
    RegistryTarget, RegistryVersion, SignedRegistrySnapshot, SnapshotError, snapshot_hash,
    verify_entry_signature, verify_snapshot,
};

const COMPACT_INDEX_DOMAIN: &[u8] = b"tilo-registry-compact-index-v4\0";
/// Largest version index a client fetches. An entry holds at most
/// [`crate::entry::MAX_ENTRY_VERSIONS`] versions of a few hundred bytes each.
pub const MAX_VERSION_INDEX_BYTES: u64 = 1024 * 1024;

/// Where an entry's complete version list lives, by content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionIndexRef {
    pub blake3_hex: String,
    pub size_bytes: u64,
}

/// One entry of a compact snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactRegistryEntry {
    /// The publisher's entry with `versions` reduced to [`newest_versions`].
    /// `signature_hex` is the publisher's signature over the complete entry
    /// in the index; it does not verify against this reduced copy.
    pub entry: RegistryEntry,
    pub version_index: VersionIndexRef,
}

/// The compact form of one signed snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactRegistrySnapshot {
    pub protocol: u32,
    pub sequence: u64,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub previous_snapshot_hash_hex: String,
    pub moderation_head_hex: String,
    /// Hash of the full snapshot at this sequence: the identity clients
    /// checkpoint and the next snapshot names as its predecessor.
    pub snapshot_hash_hex: String,
    pub publishers: Vec<PublisherAuthorization>,
    /// App and bot entries.
    pub entries: Vec<CompactRegistryEntry>,
    /// The owner's signature, or the authorized channel key's when
    /// `channel_authorization` is present.
    pub signature_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_authorization: Option<ChannelAuthorization>,
}

/// A version index file: the complete publisher-signed entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryVersionIndex {
    pub protocol: u32,
    pub entry: RegistryEntry,
}

#[derive(Serialize)]
struct CompactSigningPayload<'a> {
    protocol: u32,
    sequence: u64,
    issued_at_unix: u64,
    expires_at_unix: u64,
    previous_snapshot_hash_hex: &'a str,
    moderation_head_hex: &'a str,
    snapshot_hash_hex: &'a str,
    publishers: &'a [PublisherAuthorization],
    entries: &'a [CompactRegistryEntry],
    channel_authorization: Option<&'a ChannelAuthorization>,
}

/// Why a version index was refused.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VersionIndexError {
    #[error("version index is not the size the snapshot signs")]
    Size,
    #[error("version index does not hash to the value the snapshot signs")]
    Hash,
    #[error("version index is not JSON of the expected shape: {0}")]
    Decode(String),
    #[error("unsupported version index protocol")]
    Protocol,
    #[error("version index belongs to {found}, not {expected}")]
    WrongEntry { expected: String, found: String },
    #[error("version index entry has an invalid publisher signature")]
    PublisherSignature,
    #[error("version index entry exceeds a client safety bound")]
    Oversize,
    #[error("version index does not match the snapshot's entry")]
    Mismatch,
}

/// The versions a compact snapshot keeps: for each exact target (bots all
/// share one), the newest release and the newest pre-release, in their
/// original order. A client's
/// newest installable version on any channel is always among them.
pub fn newest_versions(versions: &[RegistryVersion]) -> Vec<RegistryVersion> {
    let mut newest: Vec<(Option<&RegistryTarget>, bool, usize)> = Vec::new();
    for (index, version) in versions.iter().enumerate() {
        let prerelease = crate::version::is_prerelease(&version.version);
        match newest
            .iter_mut()
            .find(|(target, channel, _)| *target == version.target() && *channel == prerelease)
        {
            Some((_, _, best)) => {
                let current = versions.get(*best).map(|best| &best.version);
                if current.is_some_and(|current| {
                    crate::version::compare(&version.version, current).is_gt()
                }) {
                    *best = index;
                }
            }
            None => newest.push((version.target(), prerelease, index)),
        }
    }
    let keep: HashSet<usize> = newest.into_iter().map(|(_, _, index)| index).collect();
    versions
        .iter()
        .enumerate()
        .filter(|(index, _)| keep.contains(index))
        .map(|(_, version)| version.clone())
        .collect()
}

/// The exact bytes of `entry`'s version index.
///
/// # Errors
/// Returns an encoding error if the entry cannot be serialized.
pub fn version_index_bytes(entry: &RegistryEntry) -> Result<Vec<u8>, SnapshotError> {
    serde_json::to_vec(&RegistryVersionIndex {
        protocol: PACKAGE_FORMAT_VERSION,
        entry: entry.clone(),
    })
    .map_err(|_| SnapshotError::Encoding)
}

/// A version index file named by its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionIndexFile {
    pub blake3_hex: String,
    pub bytes: Vec<u8>,
}

/// The unsigned compact form of `full` and the version index files it names.
/// Sign it with [`sign_compact_snapshot`] using the key that signed `full`.
///
/// # Errors
/// Returns an encoding error if the snapshot or an entry cannot be serialized.
pub fn compact_snapshot(
    full: &SignedRegistrySnapshot,
) -> Result<(CompactRegistrySnapshot, Vec<VersionIndexFile>), SnapshotError> {
    let mut entries = Vec::with_capacity(full.entries.len());
    let mut indexes = Vec::with_capacity(full.entries.len());
    for entry in &full.entries {
        let bytes = version_index_bytes(entry)?;
        let blake3_hex = blake3::hash(&bytes).to_hex().to_string();
        let mut reduced = entry.clone();
        reduced.versions = newest_versions(&entry.versions);
        entries.push(CompactRegistryEntry {
            entry: reduced,
            version_index: VersionIndexRef {
                blake3_hex: blake3_hex.clone(),
                size_bytes: bytes.len() as u64,
            },
        });
        indexes.push(VersionIndexFile { blake3_hex, bytes });
    }
    Ok((
        CompactRegistrySnapshot {
            protocol: PACKAGE_FORMAT_VERSION,
            sequence: full.sequence,
            issued_at_unix: full.issued_at_unix,
            expires_at_unix: full.expires_at_unix,
            previous_snapshot_hash_hex: full.previous_snapshot_hash_hex.clone(),
            moderation_head_hex: full.moderation_head_hex.clone(),
            snapshot_hash_hex: snapshot_hash(full)?,
            publishers: full.publishers.clone(),
            entries,
            signature_hex: String::new(),
            channel_authorization: full.channel_authorization.clone(),
        },
        indexes,
    ))
}

/// Digest the owner or channel key signs.
///
/// # Errors
/// Returns an encoding error if the canonical payload cannot be serialized.
pub fn compact_signing_message(
    snapshot: &CompactRegistrySnapshot,
) -> Result<[u8; 32], SnapshotError> {
    let body = postcard::to_stdvec(&CompactSigningPayload {
        protocol: snapshot.protocol,
        sequence: snapshot.sequence,
        issued_at_unix: snapshot.issued_at_unix,
        expires_at_unix: snapshot.expires_at_unix,
        previous_snapshot_hash_hex: &snapshot.previous_snapshot_hash_hex,
        moderation_head_hex: &snapshot.moderation_head_hex,
        snapshot_hash_hex: &snapshot.snapshot_hash_hex,
        publishers: &snapshot.publishers,
        entries: &snapshot.entries,
        channel_authorization: snapshot.channel_authorization.as_ref(),
    })
    .map_err(|_| SnapshotError::Encoding)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(COMPACT_INDEX_DOMAIN);
    hasher.update(&body);
    Ok(*hasher.finalize().as_bytes())
}

/// Sign a compact snapshot with the owner key, or with the channel key when
/// it carries a channel authorization.
///
/// # Errors
/// Returns an encoding error if the canonical payload cannot be serialized.
pub fn sign_compact_snapshot(
    snapshot: &mut CompactRegistrySnapshot,
    key: &SigningKey,
) -> Result<(), SnapshotError> {
    snapshot.protocol = PACKAGE_FORMAT_VERSION;
    snapshot.signature_hex.clear();
    snapshot.signature_hex = hex::encode(key.sign(&compact_signing_message(snapshot)?).to_bytes());
    Ok(())
}

/// Hash naming the exact signed compact bytes, for HTTP revalidation.
///
/// # Errors
/// Returns an encoding error if the snapshot cannot be serialized.
pub fn compact_snapshot_hash(snapshot: &CompactRegistrySnapshot) -> Result<String, SnapshotError> {
    let bytes = postcard::to_stdvec(snapshot).map_err(|_| SnapshotError::Encoding)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Verify a compact snapshot as [`verify_snapshot`] verifies a full one:
/// signature, freshness, bounds and publisher authorization. Publisher
/// signatures cover complete entries, so they are checked per entry by
/// [`verify_version_index`], and on install by the package's own signature.
///
/// # Errors
/// Returns the precise freshness, shape, signature, or authorization failure.
pub fn verify_compact_snapshot(
    snapshot: &CompactRegistrySnapshot,
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
        signature_hex: &snapshot.signature_hex,
        channel_authorization: snapshot.channel_authorization.as_ref(),
    };
    header.check_window(now_unix)?;
    if snapshot.entries.iter().any(|compact| {
        !entry_within_bounds(&compact.entry)
            || compact.version_index.size_bytes == 0
            || compact.version_index.size_bytes > MAX_VERSION_INDEX_BYTES
    }) {
        return Err(SnapshotError::Oversize);
    }
    if !valid_hex(&snapshot.snapshot_hash_hex, 32)
        || snapshot.entries.iter().any(|compact| {
            !valid_hex(&compact.version_index.blake3_hex, 32)
                || newest_versions(&compact.entry.versions) != compact.entry.versions
        })
    {
        return Err(SnapshotError::Shape);
    }
    let publishers =
        header.check_signature(pinned_owner_pubkey_hex, &compact_signing_message(snapshot)?)?;
    let mut packages = HashSet::with_capacity(snapshot.entries.len());
    for compact in &snapshot.entries {
        check_entry_identity(&compact.entry, &mut packages, &publishers)?;
    }
    Ok(())
}

/// Check version index `bytes` against the entry of a verified compact
/// snapshot that names it, and return the complete publisher-signed entry.
///
/// # Errors
/// Returns [`VersionIndexError`] when the bytes are not the signed index,
/// belong to another entry, are not signed by the entry's publisher, or do
/// not reduce to the snapshot's entry.
pub fn verify_version_index(
    compact: &CompactRegistryEntry,
    bytes: &[u8],
) -> Result<RegistryEntry, VersionIndexError> {
    if bytes.len() as u64 != compact.version_index.size_bytes {
        return Err(VersionIndexError::Size);
    }
    if !blake3::hash(bytes)
        .to_hex()
        .eq_ignore_ascii_case(&compact.version_index.blake3_hex)
    {
        return Err(VersionIndexError::Hash);
    }
    let index: RegistryVersionIndex = serde_json::from_slice(bytes)
        .map_err(|error| VersionIndexError::Decode(error.to_string()))?;
    if index.protocol != PACKAGE_FORMAT_VERSION {
        return Err(VersionIndexError::Protocol);
    }
    let full = index.entry;
    if full.package_id != compact.entry.package_id {
        return Err(VersionIndexError::WrongEntry {
            expected: compact.entry.package_id.clone(),
            found: full.package_id,
        });
    }
    if !entry_within_bounds(&full) {
        return Err(VersionIndexError::Oversize);
    }
    if !verify_entry_signature(&full) {
        return Err(VersionIndexError::PublisherSignature);
    }
    let mut reduced = full.clone();
    reduced.versions = newest_versions(&full.versions);
    if reduced != compact.entry {
        return Err(VersionIndexError::Mismatch);
    }
    Ok(full)
}

/// A snapshot in either wire form, as served at `/entries`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogueSnapshot {
    Full(SignedRegistrySnapshot),
    Compact(CompactRegistrySnapshot),
}

/// Tells the two forms apart: only the compact one names its full sibling.
#[derive(Deserialize)]
struct FormProbe {
    #[serde(default)]
    snapshot_hash_hex: Option<serde::de::IgnoredAny>,
}

/// One entry of a verified catalogue, and its version index when the
/// snapshot was compact.
#[derive(Clone, Copy, Debug)]
pub struct CatalogueEntry<'a> {
    pub entry: &'a RegistryEntry,
    pub version_index: Option<&'a VersionIndexRef>,
}

impl CatalogueSnapshot {
    /// Decode either form; the compact one names its full sibling's hash.
    /// The `protocol` both carry is checked when the snapshot is verified.
    ///
    /// # Errors
    /// Returns the JSON error for anything that is neither form.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let probe: FormProbe = serde_json::from_slice(bytes)?;
        if probe.snapshot_hash_hex.is_some() {
            serde_json::from_slice(bytes).map(Self::Compact)
        } else {
            serde_json::from_slice(bytes).map(Self::Full)
        }
    }

    /// Verify the snapshot in its own form.
    ///
    /// # Errors
    /// Returns the precise freshness, shape, signature, or authorization failure.
    pub fn verify(
        &self,
        pinned_owner_pubkey_hex: &str,
        now_unix: u64,
    ) -> Result<(), SnapshotError> {
        match self {
            Self::Full(full) => verify_snapshot(full, pinned_owner_pubkey_hex, now_unix),
            Self::Compact(compact) => {
                verify_compact_snapshot(compact, pinned_owner_pubkey_hex, now_unix)
            }
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Self::Full(full) => full.sequence,
            Self::Compact(compact) => compact.sequence,
        }
    }

    /// The snapshot's identity for anti-rollback checkpoints: the full
    /// snapshot's hash, whichever form was fetched.
    ///
    /// # Errors
    /// Returns an encoding error if a full snapshot cannot be serialized.
    pub fn checkpoint_hash(&self) -> Result<String, SnapshotError> {
        match self {
            Self::Full(full) => snapshot_hash(full),
            Self::Compact(compact) => Ok(compact.snapshot_hash_hex.to_ascii_lowercase()),
        }
    }

    pub fn moderation_head_hex(&self) -> &str {
        match self {
            Self::Full(full) => &full.moderation_head_hex,
            Self::Compact(compact) => &compact.moderation_head_hex,
        }
    }

    pub fn entries(&self) -> Vec<CatalogueEntry<'_>> {
        match self {
            Self::Full(full) => full
                .entries
                .iter()
                .map(|entry| CatalogueEntry {
                    entry,
                    version_index: None,
                })
                .collect(),
            Self::Compact(compact) => compact
                .entries
                .iter()
                .map(|compact| CatalogueEntry {
                    entry: &compact.entry,
                    version_index: Some(&compact.version_index),
                })
                .collect(),
        }
    }

    /// The compact entry for `package_id`, when this snapshot is compact.
    pub fn compact_entry(&self, package_id: &str) -> Option<&CompactRegistryEntry> {
        match self {
            Self::Full(_) => None,
            Self::Compact(compact) => compact
                .entries
                .iter()
                .find(|compact| compact.entry.package_id == package_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EntryKind, TappArtifact, VersionArtifact, sign_channel_authorization, sign_entry,
        sign_snapshot, sign_snapshot_delegated,
    };

    fn version(number: &str, isa: &str) -> RegistryVersion {
        RegistryVersion {
            version: number.into(),
            released_at_unix: 1,
            artifact: VersionArtifact::Tapp(TappArtifact {
                tapp_url: format!("https://apps.tilo.so/artifacts/{number}-{isa}.tapp"),
                tapp_root_hash_hex: "ab".repeat(32),
                tapp_size_bytes: 1,
                tapp_blake3_hex: "cd".repeat(32),
                target: RegistryTarget {
                    platform: crate::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: isa.into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }
    }

    fn entry(package_id: &str, versions: Vec<RegistryVersion>) -> RegistryEntry {
        let publisher = SigningKey::from_bytes(&[7; 32]);
        let mut entry = RegistryEntry {
            package_id: package_id.into(),
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
            publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
            versions,
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &publisher);
        entry
    }

    fn full(owner: &SigningKey) -> SignedRegistrySnapshot {
        let notes = entry(
            "so.tilo.notes",
            vec![
                version("3.0.0-dev.1", "x86_64-v3"),
                version("2.0.0", "x86_64-v3"),
                version("1.1.0", "x86_64"),
                version("1.0.0", "x86_64-v3"),
            ],
        );
        let sheets = entry("so.tilo.sheets", vec![version("5", "x86_64-v3")]);
        let mut snapshot = SignedRegistrySnapshot {
            protocol: PACKAGE_FORMAT_VERSION,
            sequence: 1,
            issued_at_unix: 10,
            expires_at_unix: 20,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: vec![PublisherAuthorization {
                pubkey_hex: notes.publisher_pubkey_hex.clone(),
                can_add_new: true,
            }],
            entries: vec![notes, sheets],
            owner_signature_hex: String::new(),
            channel_authorization: None,
        };
        sign_snapshot(&mut snapshot, owner).unwrap();
        snapshot
    }

    fn compact(
        owner: &SigningKey,
    ) -> (
        SignedRegistrySnapshot,
        CompactRegistrySnapshot,
        Vec<VersionIndexFile>,
    ) {
        let full = full(owner);
        let (mut compact, indexes) = compact_snapshot(&full).unwrap();
        sign_compact_snapshot(&mut compact, owner).unwrap();
        (full, compact, indexes)
    }

    #[test]
    fn compact_bots_are_signed_with_the_app_entries() {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let (_, plain, _) = compact(&owner);
        let mut full = full(&owner);
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
            ..full.entries[0].clone()
        };
        sign_entry(&mut bot, &SigningKey::from_bytes(&[7; 32]));
        full.entries.insert(1, bot);
        sign_snapshot(&mut full, &owner).unwrap();
        let (mut mixed, indexes) = compact_snapshot(&full).unwrap();
        sign_compact_snapshot(&mut mixed, &owner).unwrap();
        assert_eq!(indexes.len(), 3);
        verify_compact_snapshot(&mixed, &owner_hex, 15).unwrap();
        assert_ne!(mixed.signature_hex, plain.signature_hex);
        assert_ne!(mixed.snapshot_hash_hex, plain.snapshot_hash_hex);
        let json = serde_json::to_value(&mixed).unwrap();
        assert_eq!(json["entries"].as_array().unwrap().len(), 3);
        let read: CompactRegistrySnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(read, mixed);
        verify_compact_snapshot(&read, &owner_hex, 15).unwrap();
        let mut dropped = mixed.clone();
        dropped
            .entries
            .retain(|compact| compact.entry.kind == EntryKind::App);
        assert_eq!(
            verify_compact_snapshot(&dropped, &owner_hex, 15),
            Err(SnapshotError::OwnerSignature)
        );
    }

    #[test]
    fn compact_snapshots_keep_the_newest_version_per_target_and_channel() {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let (full, compact, indexes) = compact(&owner);
        verify_compact_snapshot(&compact, &owner_hex, 15).unwrap();
        let kept: Vec<_> = compact.entries[0]
            .entry
            .versions
            .iter()
            .map(|version| version.version.as_str())
            .collect();
        assert_eq!(kept, ["3.0.0-dev.1", "2.0.0", "1.1.0"]);
        assert_eq!(
            compact.snapshot_hash_hex,
            crate::snapshot_hash(&full).unwrap()
        );
        let parsed = CatalogueSnapshot::from_json(&serde_json::to_vec(&compact).unwrap()).unwrap();
        assert_eq!(
            parsed.checkpoint_hash().unwrap(),
            CatalogueSnapshot::Full(full.clone())
                .checkpoint_hash()
                .unwrap()
        );
        parsed.verify(&owner_hex, 15).unwrap();
        assert!(matches!(
            CatalogueSnapshot::from_json(&serde_json::to_vec(&full).unwrap()).unwrap(),
            CatalogueSnapshot::Full(_)
        ));

        // Every older version is still committed to, through the index.
        let restored = verify_version_index(&compact.entries[0], &indexes[0].bytes).unwrap();
        assert_eq!(restored, full.entries[0]);

        // Tampering with the compact form breaks its signature.
        let mut tampered = compact.clone();
        tampered.entries[0].entry.versions.remove(1);
        assert_eq!(
            verify_compact_snapshot(&tampered, &owner_hex, 15),
            Err(SnapshotError::OwnerSignature)
        );
        let mut tampered = compact.clone();
        tampered.entries[0].version_index = compact.entries[1].version_index.clone();
        assert_eq!(
            verify_compact_snapshot(&tampered, &owner_hex, 15),
            Err(SnapshotError::OwnerSignature)
        );
        let mut tampered = compact.clone();
        tampered.snapshot_hash_hex = "ee".repeat(32);
        assert_eq!(
            verify_compact_snapshot(&tampered, &owner_hex, 15),
            Err(SnapshotError::OwnerSignature)
        );
        // A signer may not keep more than the newest versions either.
        let mut padded = compact.clone();
        padded.entries[0].entry.versions = full.entries[0].versions.clone();
        sign_compact_snapshot(&mut padded, &owner).unwrap();
        assert_eq!(
            verify_compact_snapshot(&padded, &owner_hex, 15),
            Err(SnapshotError::Shape)
        );
        assert_eq!(
            verify_compact_snapshot(&compact, &owner_hex, 21),
            Err(SnapshotError::Freshness)
        );
    }

    #[test]
    fn tampered_version_indexes_are_rejected() {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let (full, compact, indexes) = compact(&owner);
        let notes = &compact.entries[0];

        // Bytes that are not the signed index.
        let mut flipped = indexes[0].bytes.clone();
        let last = flipped.len() - 2;
        flipped[last] ^= 1;
        assert_eq!(
            verify_version_index(notes, &flipped),
            Err(VersionIndexError::Hash)
        );
        let truncated = &indexes[0].bytes[..indexes[0].bytes.len() - 1];
        assert_eq!(
            verify_version_index(notes, truncated),
            Err(VersionIndexError::Size)
        );
        assert_eq!(
            verify_version_index(notes, &indexes[1].bytes),
            Err(VersionIndexError::Size)
        );

        // A signer that commits to the wrong file is caught by its content.
        let committed = |bytes: &[u8]| {
            let mut entry = notes.clone();
            entry.version_index = VersionIndexRef {
                blake3_hex: blake3::hash(bytes).to_hex().to_string(),
                size_bytes: bytes.len() as u64,
            };
            verify_version_index(&entry, bytes)
        };
        assert_eq!(
            committed(&indexes[1].bytes),
            Err(VersionIndexError::WrongEntry {
                expected: "so.tilo.notes".into(),
                found: "so.tilo.sheets".into()
            })
        );
        let mut truncated = full.entries[0].clone();
        truncated.versions.pop();
        assert_eq!(
            committed(&version_index_bytes(&truncated).unwrap()),
            Err(VersionIndexError::PublisherSignature)
        );
        let mut reordered = full.entries[0].clone();
        reordered.versions.swap(2, 3);
        assert_eq!(
            committed(&version_index_bytes(&reordered).unwrap()),
            Err(VersionIndexError::PublisherSignature)
        );
        // A validly signed but older entry of the same package does not
        // reduce to the snapshot's entry.
        let older = entry("so.tilo.notes", full.entries[0].versions[1..].to_vec());
        assert_eq!(
            committed(&version_index_bytes(&older).unwrap()),
            Err(VersionIndexError::Mismatch)
        );
        assert!(matches!(
            committed(b"{\"protocol\":1}"),
            Err(VersionIndexError::Decode(_))
        ));
    }

    #[test]
    fn delegated_compact_snapshots_are_channel_signed_and_scoped() {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let channel = SigningKey::from_bytes(&[9; 32]);
        let mut full = full(&owner);
        let mut authorization = ChannelAuthorization {
            protocol: PACKAGE_FORMAT_VERSION,
            generation: 1,
            channel_pubkey_hex: hex::encode(channel.verifying_key().to_bytes()),
            not_before_unix: 5,
            expires_at_unix: 30,
            max_snapshot_validity_seconds: 15,
            max_snapshot_entries: 10,
            publishers: full.publishers.clone(),
            owner_signature_hex: String::new(),
        };
        sign_channel_authorization(&mut authorization, &owner).unwrap();
        sign_snapshot_delegated(&mut full, &channel, authorization).unwrap();
        let (mut compact, _) = compact_snapshot(&full).unwrap();
        sign_compact_snapshot(&mut compact, &channel).unwrap();
        verify_compact_snapshot(&compact, &owner_hex, 15).unwrap();

        let mut by_owner = compact.clone();
        sign_compact_snapshot(&mut by_owner, &owner).unwrap();
        assert_eq!(
            verify_compact_snapshot(&by_owner, &owner_hex, 15),
            Err(SnapshotError::ChannelSignature)
        );
        let mut unscoped = compact;
        unscoped.channel_authorization = None;
        sign_compact_snapshot(&mut unscoped, &channel).unwrap();
        assert_eq!(
            verify_compact_snapshot(&unscoped, &owner_hex, 15),
            Err(SnapshotError::OwnerSignature)
        );
    }
}
