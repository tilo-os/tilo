//! Governor succession: the identity that governs a room hands it from a
//! revoked governing device to another of its devices.
//!
//! Governance belongs to the identity. Its root signs a [`GovernorSuccession`]
//! naming the room, the revoked governing device, the root's revocation of
//! it, the successor device's certificate and the room history the revoked
//! governor wrote up to the revocation (`cutoff`). A replica accepts it only
//! when everything checks against the root the room already trusts, and only
//! one successor per revoked governor: a second, different record for the
//! same governor contests the room, and no successor of it is accepted.
//!
//! The room document carries the records it accepted, keyed by the revoked
//! governor ([`SUCCESSIONS_FIELD`]). Walking them from the governor a replica
//! was pinned to gives the current one, so governance written under an
//! earlier governor stays valid up to its cutoff and no further.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc};
use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// Room-document map of accepted successions, keyed by the hex key of the
/// governing device each one replaces.
pub const SUCCESSIONS_FIELD: &str = "governor_successions_v1";
const SUCCESSION_VERSION: u32 = 1;
const SUCCESSION_DOMAIN: &[u8] = b"tilo/governor-succession/v1\0";
/// Longest chain of successions a replica walks.
pub const MAX_SUCCESSIONS: usize = 16;
/// Most cutoff heads one record names.
const MAX_CUTOFF_HEADS: usize = 64;

/// The root's statement that `successor` governs `room` in place of the
/// revoked `previous` governing device, whose governance ends at `cutoff`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernorSuccession {
    pub version: u32,
    pub room: String,
    pub previous: DeviceCert,
    pub revocation: RevocationEntry,
    pub successor: DeviceCert,
    /// Heads of the room document that end the previous governor's history.
    pub cutoff: Vec<[u8; 32]>,
    #[serde(with = "serde_bytes_64")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct SuccessionView<'a> {
    version: u32,
    room: &'a str,
    previous: &'a DeviceCert,
    revocation: &'a RevocationEntry,
    successor: &'a DeviceCert,
    cutoff: &'a [[u8; 32]],
}

impl GovernorSuccession {
    /// Sign a succession with the identity's root key.
    ///
    /// # Errors
    ///
    /// Returns an error if `identity_key` is not the root of both devices, the
    /// revocation does not revoke `previous` under that root, or the cutoff
    /// is too long.
    pub fn sign(
        room: &str,
        previous: DeviceCert,
        revocation: RevocationEntry,
        successor: DeviceCert,
        cutoff: Vec<[u8; 32]>,
        identity_key: &ed25519_dalek::SigningKey,
    ) -> Result<Self, String> {
        let room = uuid::Uuid::parse_str(room)
            .map_err(|_| "succession room is not a UUID".to_string())?
            .to_string();
        let mut record = Self {
            version: SUCCESSION_VERSION,
            room,
            previous,
            revocation,
            successor,
            cutoff,
            signature: [0; 64],
        };
        if identity_key.verifying_key().to_bytes() != record.previous.user_public_key {
            return Err("succession signer is not the governing identity's root".into());
        }
        record.check_shape()?;
        record.signature = identity_key.sign(&record.digest()?).to_bytes();
        Ok(record)
    }

    fn digest(&self) -> Result<[u8; 32], String> {
        let encoded = postcard::to_stdvec(&SuccessionView {
            version: self.version,
            room: &self.room,
            previous: &self.previous,
            revocation: &self.revocation,
            successor: &self.successor,
            cutoff: &self.cutoff,
        })
        .map_err(|error| error.to_string())?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(SUCCESSION_DOMAIN);
        hasher.update(&encoded);
        Ok(*hasher.finalize().as_bytes())
    }

    fn check_shape(&self) -> Result<(), String> {
        let root = self.previous.user_public_key;
        if self.version != SUCCESSION_VERSION {
            return Err("unknown succession version".into());
        }
        if self.successor.user_public_key != root {
            return Err("successor belongs to another identity".into());
        }
        if self.successor.device_public_key == self.previous.device_public_key {
            return Err("a governor cannot succeed itself".into());
        }
        if self.revocation.user_public_key != root
            || self.revocation.revoked_device_public_key != self.previous.device_public_key
        {
            return Err("succession revocation does not revoke the previous governor".into());
        }
        if self.cutoff.is_empty() || self.cutoff.len() > MAX_CUTOFF_HEADS {
            return Err("succession cutoff names no room history".into());
        }
        let capabilities = &self.successor.capabilities;
        if capabilities.foreign
            || !(capabilities.can_own_workspaces || capabilities.can_act_as_host)
        {
            return Err("successor may not govern workspaces".into());
        }
        Ok(())
    }

    /// Verify the record for `room` against the governing device `pinned`
    /// a replica trusts, at `now_ns`.
    ///
    /// # Errors
    ///
    /// Returns why the record does not hand `pinned`'s room to its successor:
    /// another room or governor, another identity, a revocation that is not
    /// the root's revocation of `pinned`, an invalid successor certificate, or
    /// a signature the root did not make.
    pub fn verify(&self, room: &str, pinned: &DeviceCert, now_ns: u64) -> Result<(), String> {
        self.verify_at(room, pinned, Some(now_ns))
    }

    /// As [`Self::verify`]; without a time, the successor's certificate is
    /// checked as issued, for a record a replica already followed while it
    /// was current.
    fn verify_at(
        &self,
        room: &str,
        pinned: &DeviceCert,
        now_ns: Option<u64>,
    ) -> Result<(), String> {
        if self.room != room {
            return Err("succession names another room".into());
        }
        if self.previous != *pinned {
            return Err("succession replaces another governor".into());
        }
        self.verify_signed()?;
        tilo_user_identity::verify_device_cert_at(
            &self.successor,
            now_ns.unwrap_or(self.successor.issued_at_ns),
        )
        .map_err(|error| format!("successor certificate: {error}"))
    }

    /// The record is well formed, its revocation and the record itself
    /// signed by the previous governor's root.
    fn verify_signed(&self) -> Result<(), String> {
        self.check_shape()?;
        tilo_user_identity::verify_revocation_entry(&self.revocation).map_err(|_| {
            "succession revocation is not signed by the identity's root".to_string()
        })?;
        let identity_key = ed25519_dalek::VerifyingKey::from_bytes(&self.previous.user_public_key)
            .map_err(|_| "invalid identity root key".to_string())?;
        identity_key
            .verify(
                &self.digest()?,
                &ed25519_dalek::Signature::from_bytes(&self.signature),
            )
            .map_err(|_| "succession is not signed by the identity's root".to_string())
    }

    /// The document key this record is kept under.
    #[must_use]
    pub fn key(&self) -> String {
        hex::encode(self.previous.device_public_key)
    }

    /// Check that `doc` holds the previous governor's history exactly up to
    /// the cutoff: every cutoff head is present and every change the
    /// previous governor wrote is one of its ancestors.
    ///
    /// # Errors
    ///
    /// Returns an error if a cutoff head is missing or the previous governor
    /// wrote past it.
    pub fn check_cutoff(&self, doc: &AutoCommit) -> Result<(), String> {
        let mut doc = doc.clone();
        let changes = doc
            .get_changes_meta(&[])
            .into_iter()
            .map(|meta| (meta.hash, (meta.actor.to_bytes().to_vec(), meta.deps)))
            .collect::<BTreeMap<_, _>>();
        let mut covered = BTreeSet::new();
        let mut queue = self
            .cutoff
            .iter()
            .map(|head| ChangeHash(*head))
            .collect::<VecDeque<_>>();
        while let Some(hash) = queue.pop_front() {
            if !covered.insert(hash) {
                continue;
            }
            let (_, deps) = changes
                .get(&hash)
                .ok_or("room history lacks the succession cutoff")?;
            queue.extend(deps.iter().copied());
        }
        let previous = self.previous.device_public_key;
        if changes
            .iter()
            .any(|(hash, (actor, _))| actor.as_slice() == previous && !covered.contains(hash))
        {
            return Err(
                "the revoked governor wrote room history after its succession cutoff".into(),
            );
        }
        Ok(())
    }
}

/// Every succession `doc` records, by the hex key of the governor each one
/// replaces. A governor may have several when writers recorded different
/// ones concurrently, in the same map or in maps they created concurrently.
///
/// # Errors
///
/// Returns an error if a recorded succession does not decode.
pub fn recorded_all(
    doc: &impl ReadDoc,
) -> Result<BTreeMap<String, Vec<GovernorSuccession>>, String> {
    let mut all: BTreeMap<String, Vec<GovernorSuccession>> = BTreeMap::new();
    for (value, map) in doc
        .get_all(ROOT, SUCCESSIONS_FIELD)
        .map_err(|error| error.to_string())?
    {
        if !matches!(value, automerge::Value::Object(automerge::ObjType::Map)) {
            return Err("recorded successions are not a map".into());
        }
        for key in doc.keys(&map) {
            for (value, _) in doc.get_all(&map, &key).map_err(|error| error.to_string())? {
                let automerge::Value::Scalar(scalar) = value else {
                    return Err("recorded succession is not bytes".into());
                };
                let automerge::ScalarValue::Bytes(bytes) = scalar.as_ref() else {
                    return Err("recorded succession is not bytes".into());
                };
                let record: GovernorSuccession =
                    postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
                // A record kept under another governor's key claims nothing.
                if record.key() != key {
                    continue;
                }
                let records = all.entry(key.clone()).or_default();
                if !records.contains(&record) {
                    records.push(record);
                }
            }
        }
    }
    for records in all.values_mut() {
        records.sort_by_key(|record| record.signature);
    }
    Ok(all)
}

/// The successions `doc` records for the governing device `previous`.
///
/// # Errors
///
/// Returns an error if a recorded succession does not decode.
pub fn recorded(
    doc: &impl ReadDoc,
    previous: &[u8; 32],
) -> Result<Vec<GovernorSuccession>, String> {
    Ok(recorded_all(doc)?
        .remove(&hex::encode(previous))
        .unwrap_or_default())
}

/// How a replica walks the successions its room records.
#[derive(Clone, Copy, Debug)]
pub enum Walk {
    /// Following a succession now: each link verified at this time, and the
    /// room history checked against each cutoff.
    Follow { now_ns: u64 },
    /// Re-reading successions the replica followed before: each link's
    /// signatures, without the time or the history it was checked at then.
    Followed,
}

/// Walk the successions `doc` records from the governing device `pinned`:
/// the chain of records leading to the current governor, verified link by
/// link. Empty when `doc` records no succession of `pinned`.
///
/// # Errors
///
/// Returns an error if a governor has more than one validly signed successor
/// (the room is contested), a link does not verify, the previous governor
/// wrote past its cutoff, or the chain is too long.
pub fn chain(
    doc: &AutoCommit,
    room: &str,
    pinned: &DeviceCert,
    walk: Walk,
) -> Result<Vec<GovernorSuccession>, String> {
    let now_ns = match walk {
        Walk::Follow { now_ns } => Some(now_ns),
        Walk::Followed => None,
    };
    let all = recorded_all(doc)?;
    let mut links = Vec::new();
    let mut current = pinned.clone();
    loop {
        // A record the root did not sign for this governor is no claim on it.
        let mut records = all
            .get(&hex::encode(current.device_public_key))
            .into_iter()
            .flatten()
            .filter(|record| record.verify_at(room, &current, now_ns).is_ok())
            .cloned()
            .collect::<Vec<_>>();
        let record = match records.len() {
            0 => return Ok(links),
            1 => records.remove(0),
            _ => {
                return Err(format!(
                    "contested governor succession: {} successors recorded for governor {}",
                    records.len(),
                    hex::encode(current.device_public_key)
                ));
            }
        };
        if links.len() >= MAX_SUCCESSIONS {
            return Err("governor succession chain is too long".into());
        }
        if matches!(walk, Walk::Follow { .. }) {
            record.check_cutoff(doc)?;
        }
        current = record.successor.clone();
        links.push(record);
    }
}

/// Whether `doc` records a chain of successions from the governing device
/// `from` to `to`: work `from` signed as governor before its cutoff stays the
/// room's under `to`. Each link was verified when a replica followed it.
#[must_use]
pub fn succeeded(doc: &impl ReadDoc, from: &[u8; 32], to: &[u8; 32]) -> bool {
    let Ok(all) = recorded_all(doc) else {
        return false;
    };
    let mut current = *from;
    for _ in 0..MAX_SUCCESSIONS {
        let Some([record]) = all.get(&hex::encode(current)).map(Vec::as_slice) else {
            return false;
        };
        if record.verify_signed().is_err() {
            return false;
        }
        current = record.successor.device_public_key;
        if current == *to {
            return true;
        }
    }
    false
}

mod serde_bytes_64 {
    use serde::de::Error as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        <[u8; 64]>::try_from(bytes.as_slice())
            .map_err(|_| D::Error::custom("expected a 64-byte signature"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, RevocationReason, sign_device_cert};

    const ROOM: &str = "6f9619ff-8b86-4011-b42d-00c04fc964ff";

    fn device(root: &SigningKey, seed: u8, capabilities: DeviceCapabilities) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
            device_label: "device".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn revocation(root: &SigningKey, device: &DeviceCert) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device.device_public_key,
            revoked_at_ns: 5,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn a_succession_vouches_for_the_previous_governor_only_up_to_its_cutoff() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let previous = device(&root, 2, DeviceCapabilities::host());
        let successor = device(&root, 3, DeviceCapabilities::primary());
        let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
            previous.device_public_key.to_vec(),
        ));
        doc.put(ROOT, "name", "before").unwrap();
        doc.commit();
        let cutoff = doc.get_heads().into_iter().map(|head| head.0).collect();
        let record = GovernorSuccession::sign(
            ROOM,
            previous.clone(),
            revocation(&root, &previous),
            successor,
            cutoff,
            &root,
        )
        .unwrap();
        record.verify(ROOM, &previous, 10).unwrap();
        record.check_cutoff(&doc).unwrap();

        // Another writer's later change is not the previous governor's.
        let mut other = doc.fork().with_actor(automerge::ActorId::from(vec![9; 32]));
        other.put(ROOT, "topic", "member").unwrap();
        other.commit();
        record.check_cutoff(&other).unwrap();

        // The previous governor writing on is refused.
        doc.put(ROOT, "name", "after").unwrap();
        doc.commit();
        assert!(record.check_cutoff(&doc).is_err());

        // A history without the cutoff is not the one the root vouched for.
        assert!(record.check_cutoff(&AutoCommit::new()).is_err());
    }

    #[test]
    fn only_the_governing_identity_root_hands_its_governor_on() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let other = SigningKey::from_bytes(&[7; 32]);
        let previous = device(&root, 2, DeviceCapabilities::host());
        let successor = device(&root, 3, DeviceCapabilities::primary());
        let stranger = device(&other, 4, DeviceCapabilities::primary());
        let entry = revocation(&root, &previous);
        let sign = |successor: DeviceCert, entry: RevocationEntry, key: &SigningKey| {
            GovernorSuccession::sign(ROOM, previous.clone(), entry, successor, vec![[0; 32]], key)
        };
        assert!(sign(successor.clone(), entry.clone(), &other).is_err());
        assert!(sign(stranger, entry.clone(), &root).is_err());
        assert!(sign(previous.clone(), entry.clone(), &root).is_err());
        assert!(sign(successor.clone(), revocation(&root, &successor), &root).is_err());
        let mut viewer = successor.clone();
        viewer.capabilities = DeviceCapabilities::host();
        viewer.capabilities.can_act_as_host = false;
        sign_device_cert(&mut viewer, &root).unwrap();
        assert!(sign(viewer, entry.clone(), &root).is_err());

        let record = sign(successor.clone(), entry, &root).unwrap();
        record.verify(ROOM, &previous, 10).unwrap();
        assert!(
            record.verify(ROOM, &successor, 10).is_err(),
            "another governor"
        );
        let mut tampered = record.clone();
        tampered.cutoff = vec![[1; 32]];
        assert!(tampered.verify(ROOM, &previous, 10).is_err());
        let mut unrevoked = record;
        unrevoked.revocation.signature = [3; 64];
        assert!(unrevoked.verify(ROOM, &previous, 10).is_err());
    }
}
