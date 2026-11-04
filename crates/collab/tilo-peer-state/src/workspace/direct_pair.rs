//! Which two people a pair record is theirs, proven.
//!
//! A record's id cannot be turned back into its readers, so the workspace
//! records the pair beside it: `direct_pairs_v1` holds, for each pair id, the
//! two people it is between, signed by a device of one of them. Anyone in the
//! workspace can then derive a record's id ([`collab_types::direct_pair_id`])
//! and ask who may read it ([`proven_pair`]); nobody can name a pair they are
//! not one of themselves, and the workspace's governor, which merges and
//! relays what members write, cannot register one on someone's behalf.
//!
//! The map is additions-only and an entry the workspace holds is never
//! changed: a pair is written once, and the proof is asked again at every
//! read, against the workspace's revocations then. Admission checks each entry
//! a batch adds ([`super::admit`]), and every door reads through
//! [`StatePeer::direct_pair`](crate::StatePeer).

use automerge::AutoCommit;
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _};
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationList};

/// The workspace key holding every pair, `{pair id → announcement}`.
pub const DIRECT_PAIRS_KEY: &str = "direct_pairs_v1";

/// Domain of [`DirectPair::signing_digest`].
const DIRECT_PAIR_DOMAIN: &str = "tilo-direct-pair-v1";

/// The most pairs one workspace holds. A pair is written once and never
/// leaves, so the workspace's document grows with each: a member can open no
/// more than this many.
pub const MAX_DIRECT_PAIRS: usize = 1024;

/// A pair as the workspace records it: who it is between, and a device's proof
/// that it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectPair {
    /// The two people, lowercase hex roots, in ascending order.
    pub people: [String; 2],
    pub proof: DirectPairProof,
}

/// A device's signature over one pair in one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectPairProof {
    /// The signing device's certificate, as its identity layer encodes it.
    pub device_cert: serde_json::Value,
    /// The device's Ed25519 signature over [`DirectPair::signing_digest`],
    /// lowercase hex.
    pub signature: String,
}

impl DirectPair {
    /// `people`'s pair in `workspace`, signed by `device` with `key`: the pair
    /// id it is the record of, and the announcement to record under it.
    ///
    /// # Errors
    ///
    /// `key` is not `device`'s, the two people are not two distinct roots, or
    /// the announcement does not encode.
    pub fn signed(
        workspace: &str,
        people: [String; 2],
        device: &DeviceCert,
        key: &SigningKey,
    ) -> Result<(String, Self), String> {
        if key.verifying_key().to_bytes() != device.device_public_key {
            return Err("the signing key is not the certified device's".into());
        }
        let (pair, mut announcement, digest) = Self::unsigned(workspace, people, device)?;
        let signature = key.sign(&digest);
        announcement.proof.signature = hex::encode(signature.to_bytes());
        Ok((pair, announcement))
    }

    /// [`Self::signed`] up to its signature, for a device whose key signs
    /// where it is kept: the pair id, the announcement still unsigned, and
    /// what [`Self::with_signature`] takes.
    ///
    /// # Errors
    ///
    /// The two people are not two distinct roots, or the announcement does not
    /// encode.
    pub fn unsigned(
        workspace: &str,
        people: [String; 2],
        device: &DeviceCert,
    ) -> Result<(String, Self, [u8; 32]), String> {
        let mut people = people;
        people.sort_unstable();
        let pair = collab_types::direct_pair_id(workspace, &people[0], &people[1])
            .ok_or("a pair is two distinct people of this workspace")?;
        let announcement = Self {
            people,
            proof: DirectPairProof {
                device_cert: serde_json::to_value(device).map_err(|error| error.to_string())?,
                signature: String::new(),
            },
        };
        let digest = Self::signing_digest(workspace, &pair);
        Ok((pair, announcement, digest))
    }

    /// This announcement signed: `signature` is the device's Ed25519 signature
    /// over what [`Self::unsigned`] answered with.
    #[must_use]
    pub fn with_signature(mut self, signature: &[u8; 64]) -> Self {
        self.proof.signature = hex::encode(signature);
        self
    }

    /// What the signing device signs: the workspace and the pair id, so a
    /// proof neither moves to another workspace nor names another pair.
    #[must_use]
    fn signing_digest(workspace: &str, pair: &str) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key(DIRECT_PAIR_DOMAIN);
        hasher.update(&(workspace.len() as u64).to_le_bytes());
        hasher.update(workspace.as_bytes());
        hasher.update(&(pair.len() as u64).to_le_bytes());
        hasher.update(pair.as_bytes());
        *hasher.finalize().as_bytes()
    }
}

/// The two people of `announcement`, the pair `pair` of `workspace`, whose
/// proof holds at `now_ns`: a certificate that chains to one of their roots
/// and is current, a device the workspace has not revoked, that device's
/// signature over the pair, and two sorted people the pair id derives from.
/// `None` for anything else, so a pair nobody's device proved is read by no
/// one.
#[must_use]
pub fn proven_pair(
    workspace: &str,
    pair: &str,
    announcement: &DirectPair,
    revocations: &RevocationList,
    now_ns: u64,
) -> Option<[String; 2]> {
    let people = announcement.people.clone();
    if people[0] >= people[1] {
        return None;
    }
    if collab_types::direct_pair_id(workspace, &people[0], &people[1]).as_deref() != Some(pair) {
        return None;
    }
    let proof = &announcement.proof;
    let device: DeviceCert = serde_json::from_value(proof.device_cert.clone()).ok()?;
    if people
        .iter()
        .all(|person| *person != hex::encode(device.user_public_key))
    {
        return None;
    }
    tilo_user_identity::verify_device_cert_at(&device, now_ns).ok()?;
    if revocations.is_revoked(&device.device_public_key) {
        return None;
    }
    let signature: [u8; 64] = hex::decode(&proof.signature).ok()?.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&device.device_public_key)
        .ok()?
        .verify(
            &DirectPair::signing_digest(workspace, pair),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .ok()?;
    Some(people)
}

/// Every pair `doc` holds, exactly as stored and keyed by pair id.
#[must_use]
pub fn direct_pairs_of(doc: &AutoCommit) -> std::collections::BTreeMap<String, Vec<u8>> {
    use automerge::{ROOT, ReadDoc as _, Value};
    let mut pairs = std::collections::BTreeMap::new();
    let Some((Value::Object(automerge::ObjType::Map), map)) =
        doc.get(ROOT, DIRECT_PAIRS_KEY).ok().flatten()
    else {
        return pairs;
    };
    for pair in doc.keys(&map) {
        if let Some((value, _)) = doc.get(&map, &pair).ok().flatten() {
            pairs.insert(pair, super::held_bytes(&value));
        }
    }
    pairs
}

#[cfg(test)]
#[path = "direct_pair_tests.rs"]
pub(crate) mod tests;
