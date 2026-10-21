//! Signed changesets: one device's record operations of one publish batch
//! (a native epoch, or any other group of changes committed together),
//! signed once and applied atomically everywhere.
//!
//! A changeset names, per record, the contiguous range of the author's
//! envelopes it contains and a digest of their exact bytes. The leaves are
//! sorted by record and folded into a Merkle root, and the author's device
//! signs the root together with the workspace, its own actor, the
//! changeset's per-device sequence and the leaf count (`storage.md` §8.3).
//!
//! A peer that may read the whole workspace receives every leaf and so
//! knows exactly which envelopes make the changeset complete: it applies
//! them in one durable transaction, or not at all. A peer that may read
//! only some records can check each of its records against the signed root
//! with an inclusion proof of `32·log₂(n)` bytes, learning nothing about the
//! other leaves.
//!
//! The envelopes themselves stay individually signed
//! ([`RecordEnvelopeV1`]), so peers that do not know changesets keep
//! verifying and applying them one stream at a time.

use collab_types::{
    CanonicalError, Ed25519PublicKey, Signature, VerifyError, canonical_bytes, sign_event,
    verify_event,
};
use fs_protocol::{FileActor, FileRecordId};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::record_replication::RecordEnvelopeV1;

/// Wire version of [`ChangesetV1`].
pub const CHANGESET_VERSION: u16 = 1;
const CHANGESET_SIGNATURE_DOMAIN: &str = "tilo.collaborative-fs.record-changeset.v1";
const LEAF_DIGEST_DOMAIN: &[u8] = b"tilo.collaborative-fs.changeset-leaf-envelopes.v1\0";
const MERKLE_LEAF_DOMAIN: &[u8] = b"tilo.collaborative-fs.changeset-merkle-leaf.v1\0";
const MERKLE_NODE_DOMAIN: &[u8] = b"tilo.collaborative-fs.changeset-merkle-node.v1\0";

/// The author's envelopes of one record inside a changeset: sequences
/// `first_sequence..=last_sequence` of the changeset's actor, whose exact
/// canonical bytes hash to `digest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangesetLeafV1 {
    pub record_id: FileRecordId,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub digest: [u8; 32],
}

/// Identity of one changeset: its author's device and per-device sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChangesetId {
    pub workspace_id: Uuid,
    pub device: [u8; 32],
    pub sequence: u64,
}

/// The signed header of a changeset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangesetV1 {
    pub protocol_version: u16,
    pub workspace_id: Uuid,
    /// The author. Every envelope of the changeset is this actor's.
    pub actor: FileActor,
    /// Strictly increasing per authoring device, starting at one.
    pub sequence: u64,
    pub created_at_ms: u64,
    pub leaf_count: u64,
    /// Merkle root over the leaves, sorted by record.
    pub root: [u8; 32],
    /// Ed25519 signature by `actor.device`.
    pub signature: Signature,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ChangesetSigningView<'a> {
    domain: &'static str,
    protocol_version: u16,
    workspace_id: &'a Uuid,
    actor: &'a FileActor,
    sequence: u64,
    created_at_ms: u64,
    leaf_count: u64,
    root: &'a [u8; 32],
}

/// A changeset header with every leaf, as stored and as sent to peers that
/// may read the whole workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedChangesetV1 {
    pub header: ChangesetV1,
    pub leaves: Vec<ChangesetLeafV1>,
}

impl ChangesetV1 {
    fn signing_view(&self) -> ChangesetSigningView<'_> {
        ChangesetSigningView {
            domain: CHANGESET_SIGNATURE_DOMAIN,
            protocol_version: self.protocol_version,
            workspace_id: &self.workspace_id,
            actor: &self.actor,
            sequence: self.sequence,
            created_at_ms: self.created_at_ms,
            leaf_count: self.leaf_count,
            root: &self.root,
        }
    }

    #[must_use]
    pub fn id(&self) -> ChangesetId {
        ChangesetId {
            workspace_id: self.workspace_id,
            device: *self.actor.device.as_bytes(),
            sequence: self.sequence,
        }
    }

    /// Verify the schema and the author's signature.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported version, a zero sequence or leaf
    /// count, or a signature that does not verify.
    pub fn verify(&self) -> Result<(), ChangesetError> {
        if self.protocol_version != CHANGESET_VERSION {
            return Err(ChangesetError::UnsupportedVersion(self.protocol_version));
        }
        if self.sequence == 0 || self.leaf_count == 0 {
            return Err(ChangesetError::Empty);
        }
        verify_event(&self.signing_view(), &self.signature, &self.actor.device)?;
        Ok(())
    }

    /// Canonical bytes of the header, for storage.
    ///
    /// # Errors
    ///
    /// Returns a canonical encoding error.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }
}

impl SignedChangesetV1 {
    /// Build and sign a changeset over `leaves` (any order; they are sorted).
    ///
    /// # Errors
    ///
    /// Returns an error when there are no leaves, a record appears twice,
    /// or the signing key is not the actor's device.
    pub fn sign(
        workspace_id: Uuid,
        actor: FileActor,
        sequence: u64,
        created_at_ms: u64,
        mut leaves: Vec<ChangesetLeafV1>,
        device_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, ChangesetError> {
        if Ed25519PublicKey(device_key.verifying_key().to_bytes()) != actor.device {
            return Err(ChangesetError::WrongDeviceKey);
        }
        leaves.sort_by_key(|leaf| leaf.record_id);
        validate_leaves(&leaves)?;
        let mut header = ChangesetV1 {
            protocol_version: CHANGESET_VERSION,
            workspace_id,
            actor,
            sequence,
            created_at_ms,
            leaf_count: leaves.len() as u64,
            root: merkle_root(&leaves),
            signature: Signature::from_bytes([0; 64]),
        };
        header.signature = sign_event(&header.signing_view(), device_key)?;
        Ok(Self { header, leaves })
    }

    /// Verify the signature and that the leaves are exactly the signed set.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid header, unsorted or duplicate
    /// leaves, a wrong leaf count, or leaves that do not hash to the root.
    pub fn verify(&self) -> Result<(), ChangesetError> {
        self.header.verify()?;
        if self.leaves.len() as u64 != self.header.leaf_count {
            return Err(ChangesetError::LeafCountMismatch);
        }
        validate_leaves(&self.leaves)?;
        if merkle_root(&self.leaves) != self.header.root {
            return Err(ChangesetError::RootMismatch);
        }
        Ok(())
    }

    /// Canonical bytes, for storage.
    ///
    /// # Errors
    ///
    /// Returns a canonical encoding error.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }

    /// Decode stored canonical bytes. The signature is not checked again:
    /// it was verified when the changeset was made or accepted.
    ///
    /// # Errors
    ///
    /// Returns a canonical decoding error.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CanonicalError> {
        collab_types::canonical::from_canonical_bytes(bytes)
    }

    /// The leaf of `record_id`, if the changeset touches it.
    #[must_use]
    pub fn leaf(&self, record_id: FileRecordId) -> Option<&ChangesetLeafV1> {
        self.leaves
            .binary_search_by_key(&record_id, |leaf| leaf.record_id)
            .ok()
            .and_then(|index| self.leaves.get(index))
    }
}

fn validate_leaves(leaves: &[ChangesetLeafV1]) -> Result<(), ChangesetError> {
    if leaves.is_empty() {
        return Err(ChangesetError::Empty);
    }
    for leaf in leaves {
        if leaf.first_sequence == 0 || leaf.last_sequence < leaf.first_sequence {
            return Err(ChangesetError::InvalidLeaf(leaf.record_id));
        }
    }
    if leaves
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if a.record_id >= b.record_id))
    {
        return Err(ChangesetError::UnsortedLeaves);
    }
    Ok(())
}

/// Digest of one leaf's envelopes, in sequence order.
///
/// # Errors
///
/// Returns a canonical encoding error.
pub fn envelopes_digest<'a>(
    envelopes: impl IntoIterator<Item = &'a RecordEnvelopeV1>,
) -> Result<[u8; 32], CanonicalError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LEAF_DIGEST_DOMAIN);
    for envelope in envelopes {
        let bytes = envelope.canonical_bytes()?;
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn leaf_hash(leaf: &ChangesetLeafV1) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MERKLE_LEAF_DOMAIN);
    hasher.update(leaf.record_id.0.as_bytes());
    hasher.update(&leaf.first_sequence.to_be_bytes());
    hasher.update(&leaf.last_sequence.to_be_bytes());
    hasher.update(&leaf.digest);
    *hasher.finalize().as_bytes()
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MERKLE_NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

/// One level up: pairs are hashed, an odd last node is carried unchanged.
fn next_level(level: &[[u8; 32]]) -> Vec<[u8; 32]> {
    level
        .chunks(2)
        .filter_map(|pair| match pair {
            [left, right] => Some(node_hash(left, right)),
            [single] => Some(*single),
            _ => None,
        })
        .collect()
}

/// Merkle root over `leaves` in the given order.
#[must_use]
pub fn merkle_root(leaves: &[ChangesetLeafV1]) -> [u8; 32] {
    let mut level: Vec<[u8; 32]> = leaves.iter().map(leaf_hash).collect();
    while level.len() > 1 {
        level = next_level(&level);
    }
    level.first().copied().unwrap_or([0; 32])
}

#[derive(Debug, Error)]
pub enum ChangesetError {
    #[error("unsupported changeset version {0}")]
    UnsupportedVersion(u16),
    #[error("a changeset needs a sequence and at least one leaf")]
    Empty,
    #[error("changeset leaf for record {0:?} has an invalid sequence range")]
    InvalidLeaf(FileRecordId),
    #[error("changeset leaves are not sorted by record, or a record appears twice")]
    UnsortedLeaves,
    #[error("changeset leaf count does not match its signed header")]
    LeafCountMismatch,
    #[error("changeset leaves do not hash to the signed root")]
    RootMismatch,
    #[error("the signing key is not the changeset actor's device key")]
    WrongDeviceKey,
    #[error(transparent)]
    Signature(#[from] VerifyError),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::generate_keypair;

    fn leaves(n: usize) -> Vec<ChangesetLeafV1> {
        (0..n)
            .map(|i| ChangesetLeafV1 {
                record_id: FileRecordId::new(),
                first_sequence: 1,
                last_sequence: (i as u64 % 3).saturating_add(1),
                digest: [u8::try_from(i % 251).unwrap(); 32],
            })
            .collect()
    }

    fn actor() -> (ed25519_dalek::SigningKey, FileActor) {
        let (device_key, device) = generate_keypair();
        let (_, person) = generate_keypair();
        (device_key, FileActor { person, device })
    }

    #[test]
    fn a_signed_changeset_verifies_and_any_change_to_it_does_not() {
        let (key, actor) = actor();
        let signed = SignedChangesetV1::sign(Uuid::new_v4(), actor, 1, 7, leaves(5), &key).unwrap();
        signed.verify().unwrap();
        let bytes = signed.canonical_bytes().unwrap();
        assert_eq!(
            SignedChangesetV1::from_canonical_bytes(&bytes).unwrap(),
            signed
        );

        let mut dropped = signed.clone();
        dropped.leaves.pop();
        assert!(matches!(
            dropped.verify(),
            Err(ChangesetError::LeafCountMismatch)
        ));
        let mut altered = signed.clone();
        altered.leaves[2].last_sequence += 1;
        assert!(matches!(
            altered.verify(),
            Err(ChangesetError::RootMismatch)
        ));
        let mut resequenced = signed;
        resequenced.header.sequence = 2;
        assert!(matches!(
            resequenced.verify(),
            Err(ChangesetError::Signature(_))
        ));
    }

    #[test]
    fn only_the_actors_device_signs_and_empty_changesets_do_not_exist() {
        let (_, actor) = actor();
        let (other, _) = generate_keypair();
        assert!(matches!(
            SignedChangesetV1::sign(Uuid::new_v4(), actor, 1, 0, leaves(1), &other),
            Err(ChangesetError::WrongDeviceKey)
        ));
        let (key, actor) = self::actor();
        assert!(matches!(
            SignedChangesetV1::sign(Uuid::new_v4(), actor, 1, 0, Vec::new(), &key),
            Err(ChangesetError::Empty)
        ));
        let mut twice = leaves(1);
        twice.push(twice[0]);
        assert!(matches!(
            SignedChangesetV1::sign(Uuid::new_v4(), actor, 1, 0, twice, &key),
            Err(ChangesetError::UnsortedLeaves)
        ));
    }
}
