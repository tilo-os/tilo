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
use tilo_automerge::ApplyIncoming;
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
type RoomHistory = BTreeMap<ChangeHash, (Vec<u8>, Vec<ChangeHash>)>;

fn ancestors(
    changes: &RoomHistory,
    heads: impl IntoIterator<Item = ChangeHash>,
) -> Result<BTreeSet<ChangeHash>, String> {
    let mut covered = BTreeSet::new();
    let mut queue = heads.into_iter().collect::<VecDeque<_>>();
    while let Some(hash) = queue.pop_front() {
        if !covered.insert(hash) {
            continue;
        }
        let (_, deps) = changes
            .get(&hash)
            .ok_or("room history lacks the succession cutoff")?;
        queue.extend(deps.iter().copied());
    }
    Ok(covered)
}
/// Most records one relayed succession message carries: a whole chain, and
/// a rival for each of its links.
pub const MAX_RELAYED: usize = 2 * MAX_SUCCESSIONS;

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

    /// Check custody before the succession: every cutoff head is present,
    /// every previous-governor change is covered, and later changes do not
    /// alter governance, regardless of their Automerge actor.
    ///
    /// # Errors
    ///
    /// Returns an error if a cutoff head is missing, the previous governor
    /// wrote past it, or another actor changed governance past it.
    pub fn check_cutoff(&self, doc: &AutoCommit) -> Result<(), String> {
        self.check_causal_history(doc, false)
    }

    /// A published succession permits its successor's transition and the
    /// history descending from it. Governance before that transition must
    /// still be covered, including changes later reverted or merged away.
    pub(crate) fn check_recorded_history(&self, doc: &AutoCommit) -> Result<(), String> {
        self.check_causal_history(doc, true)
    }

    fn check_causal_history(&self, doc: &AutoCommit, recorded: bool) -> Result<(), String> {
        let mut doc = doc.clone();
        doc.reset_diff_cursor();
        let changes = doc
            .get_changes_meta(&[])
            .into_iter()
            .map(|meta| (meta.hash, (meta.actor.to_bytes().to_vec(), meta.deps)))
            .collect::<RoomHistory>();
        let covered = ancestors(&changes, self.cutoff.iter().map(|head| ChangeHash(*head)))?;
        let previous = self.previous.device_public_key;
        if changes
            .iter()
            .any(|(hash, (actor, _))| actor.as_slice() == previous && !covered.contains(hash))
        {
            return Err(
                "the revoked governor wrote room history after its succession cutoff".into(),
            );
        }
        // Read causal projections of already admitted state. Isolation does
        // not replay or decode change bytes in the host, unlike fork_at.
        let mut before = doc.clone();
        let mut after = doc;
        let key = self.key();
        for (hash, (actor, deps)) in &changes {
            if covered.contains(hash) {
                continue;
            }
            before.isolate_at(deps);
            let preceding = recorded_all(&before)?;
            let contains = |records: &BTreeMap<String, Vec<Self>>| {
                records
                    .get(&key)
                    .is_some_and(|records| records.contains(self))
            };
            after.isolate_at(&[*hash]);
            if actor.as_slice() != self.successor.device_public_key
                && crate::topology::validate(Some(&before), &after, &hex::encode(actor), "")
                    .is_err()
            {
                return Err("room topology changed outside its actor's ownership after the succession cutoff".into());
            }
            if recorded && contains(&preceding) {
                continue;
            }
            let following = recorded_all(&after)?;
            if recorded
                && contains(&following)
                && actor.as_slice() == self.successor.device_public_key
            {
                let preceding_history = ancestors(&changes, deps.iter().copied())?;
                if !covered.is_subset(&preceding_history) {
                    return Err("the succession transition does not descend from its cutoff".into());
                }
                continue;
            }
            if !crate::room::governance_unchanged(Some(&before), &after) || preceding != following {
                return Err("room governance changed after the succession cutoff before its successor took over".into());
            }
        }
        Ok(())
    }

    /// Check the signed causal transition and all governance before it.
    /// Authenticated delivery does not excuse unrelated history past the cutoff.
    ///
    /// # Errors
    ///
    /// Returns an error for incomplete cutoff or unvouched governance history.
    pub fn check_history(&self, doc: &AutoCommit) -> Result<(), String> {
        self.check_recorded_history(doc)
    }

    /// As [`Self::check_cutoff`] for a replica following this record before
    /// the room history that carries it reaches it: when `doc` lacks part of
    /// the cutoff, whether the previous governor wrote past it is decided
    /// when that history arrives, so only a complete cutoff is checked here.
    ///
    /// # Errors
    ///
    /// Returns an error if `doc` holds the whole cutoff and its history past
    /// the cutoff is not the successor's ([`Self::check_history`]).
    pub fn check_cutoff_held(&self, doc: &AutoCommit) -> Result<(), String> {
        if self
            .cutoff
            .iter()
            .all(|head| doc.get_change_by_hash(&ChangeHash(*head)).is_some())
        {
            self.check_history(doc)
        } else {
            Ok(())
        }
    }
}

/// Check `doc` against every link of `chain`: no governor on it wrote past
/// its own cutoff, and the history past the last cutoff is the last
/// successor's ([`GovernorSuccession::check_history`]). The last cutoff's
/// history holds every earlier one. With `held`, a cutoff `doc` does not
/// hold yet is decided when the history arrives
/// ([`GovernorSuccession::check_cutoff_held`]).
///
/// # Errors
///
/// Returns the first link the history does not satisfy.
pub fn check_chain(
    chain: &[GovernorSuccession],
    doc: &AutoCommit,
    held: bool,
) -> Result<(), String> {
    let Some((last, earlier)) = chain.split_last() else {
        return Ok(());
    };
    let holds = |link: &GovernorSuccession| {
        link.cutoff
            .iter()
            .all(|head| doc.get_change_by_hash(&ChangeHash(*head)).is_some())
    };
    for link in earlier {
        if !held || holds(link) {
            link.check_recorded_history(doc)?;
        }
    }
    if !held || holds(last) {
        last.check_history(doc)?;
    }
    Ok(())
}

/// Successions by the hex key of the governor each one replaces.
pub type Recorded = BTreeMap<String, Vec<GovernorSuccession>>;

/// Add `record` under its own governor's key, once.
fn insert(all: &mut Recorded, record: GovernorSuccession) {
    let records = all.entry(record.key()).or_default();
    if !records.contains(&record) {
        records.push(record);
        records.sort_by_key(|record| record.signature);
    }
}

/// Every succession `doc` records, by the hex key of the governor each one
/// replaces. A governor may have several when writers recorded different
/// ones concurrently, in the same map or in maps they created concurrently.
///
/// # Errors
///
/// Returns an error if a recorded succession does not decode.
pub fn recorded_all(doc: &impl ReadDoc) -> Result<Recorded, String> {
    let mut all = Recorded::new();
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
                if record.key() == key {
                    insert(&mut all, record);
                }
            }
        }
    }
    Ok(all)
}

/// What `doc` records together with `extra`: successions a replica followed
/// before its room history recorded them.
///
/// # Errors
///
/// Returns an error if a recorded succession does not decode.
pub fn merged(doc: Option<&AutoCommit>, extra: &[GovernorSuccession]) -> Result<Recorded, String> {
    let mut all = match doc {
        Some(doc) => recorded_all(doc)?,
        None => Recorded::new(),
    };
    for record in extra {
        insert(&mut all, record.clone());
    }
    Ok(all)
}

/// Decode records relayed in a [`crate::NeighbourMsg::GovernorSuccession`].
///
/// # Errors
///
/// Returns an error if there are too many records or one does not decode.
pub fn decode_relayed(records: &[Vec<u8>]) -> Result<Vec<GovernorSuccession>, String> {
    if records.len() > MAX_RELAYED {
        return Err("too many relayed governor successions".into());
    }
    records
        .iter()
        .map(|bytes| postcard::from_bytes(bytes).map_err(|error| error.to_string()))
        .collect()
}

/// Encode `all` for relaying, in a canonical order so devices relaying the
/// same records send the same message.
///
/// # Errors
///
/// Returns an error if a record does not encode.
pub fn encode_relayed(all: &Recorded) -> Result<Vec<Vec<u8>>, String> {
    let mut records = all
        .values()
        .flatten()
        .map(|record| postcard::to_stdvec(record).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if records.len() > MAX_RELAYED {
        return Err("too many governor successions to relay".into());
    }
    records.sort();
    Ok(records)
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
    /// Following records relayed without the room history that carries them:
    /// each link verified at this time; the caller checks the cutoffs against
    /// the history it holds.
    Relayed { now_ns: u64 },
    /// Re-reading successions the replica followed before: each link's
    /// signatures and history, without requiring certificates to remain live.
    Followed,
}

/// Recheck stored history under the governing identity, including when a
/// caller already pins its successor. Expired historical certificates remain
/// valid evidence of a succession followed while they were live.
pub(crate) fn check_recorded_ancestry(
    doc: &AutoCommit,
    room: &str,
    authority: &DeviceCert,
) -> Result<(), String> {
    for record in recorded_all(doc)?.into_values().flatten() {
        if record.previous.user_public_key == authority.user_public_key
            && record.verify_at(room, &record.previous, None).is_ok()
        {
            record.check_recorded_history(doc)?;
        }
    }
    Ok(())
}

/// Recheck pending signed links even when restore already pins their successor.
pub(crate) fn check_followed_ancestry(
    followed: &[GovernorSuccession],
    doc: Option<&AutoCommit>,
    room: &str,
    authority: &DeviceCert,
) -> Result<(), String> {
    let Some(first) = followed.first() else {
        return Ok(());
    };
    if followed.len() > MAX_SUCCESSIONS
        || first.previous.user_public_key != authority.user_public_key
    {
        return Err("invalid followed governor succession chain".into());
    }
    let mut current = &first.previous;
    for link in followed {
        link.verify_at(room, current, None)?;
        current = &link.successor;
    }
    if current != authority {
        return Err("durable admission namespace mismatch".into());
    }
    if let Some(doc) = doc {
        check_chain(followed, doc, true)?;
    }
    Ok(())
}

fn distinct_successors(records: &[GovernorSuccession]) -> usize {
    records
        .iter()
        .map(|record| record.successor.device_public_key)
        .collect::<BTreeSet<_>>()
        .len()
}

fn contested(count: usize, governor: &[u8; 32]) -> String {
    format!(
        "contested governor succession: {count} successors recorded for governor {}",
        hex::encode(governor)
    )
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
    chain_in(&recorded_all(doc)?, Some(doc), room, pinned, walk)
}

/// As [`chain`], over the successions `all`; `doc` is the room history a
/// [`Walk::Follow`] checks each cutoff against.
///
/// # Errors
///
/// As [`chain`]; a [`Walk::Follow`] without a history fails.
pub fn chain_in(
    all: &Recorded,
    doc: Option<&AutoCommit>,
    room: &str,
    pinned: &DeviceCert,
    walk: Walk,
) -> Result<Vec<GovernorSuccession>, String> {
    let now_ns = match walk {
        Walk::Follow { now_ns } | Walk::Relayed { now_ns } => Some(now_ns),
        Walk::Followed => None,
    };
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
        // The root signing one successor again (another cutoff, say) names
        // no rival; only a different successor contests the room.
        let successors = distinct_successors(&records);
        let record = match successors {
            0 => return Ok(links),
            1 => records.remove(0),
            count => return Err(contested(count, &current.device_public_key)),
        };
        if links.len() >= MAX_SUCCESSIONS {
            return Err("governor succession chain is too long".into());
        }
        if let Some(doc) = doc {
            if matches!(walk, Walk::Follow { .. })
                || (!matches!(walk, Walk::Relayed { .. })
                    && recorded(doc, &record.previous.device_public_key)?.contains(&record))
            {
                record.check_recorded_history(doc)?;
            }
        } else if matches!(walk, Walk::Follow { .. }) {
            return Err("room history missing".into());
        }
        current = record.successor.clone();
        links.push(record);
    }
}

/// The chain of successions in `all` that ends at the governing device `to`,
/// first link first: how a replica pinned to `to` from the start reads the
/// room's earlier governors. Empty when no record names `to` as successor.
/// Each link's signatures are checked, not the time.
///
/// # Errors
///
/// Returns an error if a governor on the chain has more than one validly
/// signed successor (the room is contested), or the chain is too long.
pub fn lineage(
    all: &Recorded,
    room: &str,
    to: &DeviceCert,
) -> Result<Vec<GovernorSuccession>, String> {
    let valid = |record: &&GovernorSuccession| {
        record.room == room
            && record.previous.user_public_key == to.user_public_key
            && record.verify_signed().is_ok()
    };
    let mut links = Vec::new();
    let mut current = to.clone();
    loop {
        let mut naming = all
            .values()
            .flatten()
            .filter(valid)
            .filter(|record| record.successor == current)
            .collect::<Vec<_>>();
        let Some(record) = naming.pop() else {
            links.reverse();
            return Ok(links);
        };
        if !naming.is_empty() {
            return Err("governor succession names one successor for two governors".into());
        }
        let rivals = distinct_successors(
            &all.get(&record.key())
                .into_iter()
                .flatten()
                .filter(valid)
                .cloned()
                .collect::<Vec<_>>(),
        );
        if rivals > 1 {
            return Err(contested(rivals, &record.previous.device_public_key));
        }
        if links.len() >= MAX_SUCCESSIONS {
            return Err("governor succession chain is too long".into());
        }
        current = record.previous.clone();
        links.push(record.clone());
    }
}

/// Whether `doc` records a chain of successions from the governing device
/// `from` to `to`: work `from` signed as governor before its cutoff stays the
/// room's under `to`. Each link was verified when a replica followed it.
#[must_use]
pub fn succeeded(doc: &impl ReadDoc, from: &[u8; 32], to: &[u8; 32]) -> bool {
    recorded_all(doc).is_ok_and(|all| succeeded_in(&all, from, to))
}

/// As [`succeeded`], over the successions `all`.
#[must_use]
pub fn succeeded_in(all: &Recorded, from: &[u8; 32], to: &[u8; 32]) -> bool {
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

    #[test]
    fn a_replica_pinned_to_a_successor_reads_its_lineage() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let first = device(&root, 2, DeviceCapabilities::host());
        let second = device(&root, 3, DeviceCapabilities::primary());
        let third = device(&root, 4, DeviceCapabilities::host());
        let mut doc = AutoCommit::new()
            .with_actor(automerge::ActorId::from(first.device_public_key.to_vec()));
        doc.put(ROOT, "name", "room").unwrap();
        doc.commit();
        let cutoff: Vec<[u8; 32]> = doc.get_heads().into_iter().map(|head| head.0).collect();
        let link = |previous: &DeviceCert, successor: &DeviceCert| {
            GovernorSuccession::sign(
                ROOM,
                previous.clone(),
                revocation(&root, previous),
                successor.clone(),
                cutoff.clone(),
                &root,
            )
            .unwrap()
        };
        let one = link(&first, &second);
        let two = link(&second, &third);
        let all = merged(None, &[two.clone(), one.clone()]).unwrap();
        assert_eq!(
            lineage(&all, ROOM, &third).unwrap(),
            vec![one.clone(), two.clone()]
        );
        assert_eq!(lineage(&all, ROOM, &second).unwrap(), vec![one.clone()]);
        assert!(lineage(&all, ROOM, &first).unwrap().is_empty());
        assert!(succeeded_in(
            &all,
            &first.device_public_key,
            &third.device_public_key
        ));

        // A second successor for a governor on the chain contests it.
        let rival = link(&first, &third);
        let contested = merged(None, &[one.clone(), two.clone(), rival]).unwrap();
        assert!(lineage(&contested, ROOM, &third).is_err());
        assert!(chain_in(&contested, None, ROOM, &first, Walk::Relayed { now_ns: 10 }).is_err());

        // Relayed records decode as encoded, in one order whoever relays them.
        let relayed = encode_relayed(&all).unwrap();
        assert_eq!(
            relayed,
            encode_relayed(&merged(None, &[one, two]).unwrap()).unwrap()
        );
        assert_eq!(
            merged(None, &decode_relayed(&relayed).unwrap()).unwrap(),
            all
        );
        assert!(decode_relayed(&vec![relayed[0].clone(); MAX_RELAYED + 1]).is_err());
    }

    #[test]
    fn a_replica_behind_the_cutoff_decides_it_when_the_history_arrives() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let previous = device(&root, 2, DeviceCapabilities::host());
        let successor = device(&root, 3, DeviceCapabilities::primary());
        let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
            previous.device_public_key.to_vec(),
        ));
        doc.put(ROOT, "name", "before").unwrap();
        doc.commit();
        let behind = doc.fork();
        doc.put(ROOT, "name", "cutoff").unwrap();
        doc.commit();
        let record = GovernorSuccession::sign(
            ROOM,
            previous.clone(),
            revocation(&root, &previous),
            successor,
            doc.get_heads().into_iter().map(|head| head.0).collect(),
            &root,
        )
        .unwrap();
        record.check_cutoff_held(&behind).unwrap();
        assert!(record.check_cutoff(&behind).is_err());
        record.check_cutoff_held(&doc).unwrap();
        let mut past = doc.fork().with_actor(automerge::ActorId::from(
            previous.device_public_key.to_vec(),
        ));
        past.put(ROOT, "name", "after").unwrap();
        past.commit();
        assert!(record.check_cutoff_held(&past).is_err());
    }
}
