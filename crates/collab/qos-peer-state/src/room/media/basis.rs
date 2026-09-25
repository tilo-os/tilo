//! One security-basis algorithm, with separately charged native temporary work.
use super::{MediaPolicyError, read::Error};
use crate::resources::{
    PeerResourceError,
    json::{JsonHeld, JsonLimits},
};
use crate::room::read::collections::Collections;
use crate::room::read::{self, CopyValue, LegacyCopy, LegacyRead, ReadError, RoomRead, TurnCopy};
use crate::state::GovernedIdentity;
use automerge::resource_limits::{NativeResourceError, NativeWorkTurn};
use collab_types::caps::CapOverride;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

pub(crate) struct BasisLimits {
    pub(crate) collection_bytes: usize,
    pub(crate) json: JsonLimits,
}
trait View<T> {
    fn value(&self) -> &T;
}
impl View<GovernedIdentity> for GovernedIdentity {
    fn value(&self) -> &Self {
        self
    }
}
impl View<CapOverride> for CapOverride {
    fn value(&self) -> &Self {
        self
    }
}
impl<T: crate::resources::json::Schema> View<T> for JsonHeld<T> {
    fn value(&self) -> &T {
        self.value()
    }
}
trait Decode {
    type Identity: View<GovernedIdentity>;
    type Capability: View<CapOverride>;
    fn identity(&self, bytes: &[u8]) -> Result<Self::Identity, Error>;
    fn capability(&self, bytes: &[u8]) -> Result<Self::Capability, Error>;
}
struct Legacy;
impl Decode for Legacy {
    type Identity = GovernedIdentity;
    type Capability = CapOverride;
    fn identity(&self, bytes: &[u8]) -> Result<Self::Identity, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
    fn capability(&self, bytes: &[u8]) -> Result<Self::Capability, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
}
struct Bounded<'a> {
    turn: &'a NativeWorkTurn,
    limits: JsonLimits,
}
impl Decode for Bounded<'_> {
    type Identity = JsonHeld<GovernedIdentity>;
    type Capability = JsonHeld<CapOverride>;
    fn identity(&self, bytes: &[u8]) -> Result<Self::Identity, Error> {
        JsonHeld::decode(bytes, self.limits, self.turn).map_err(decode_error)
    }
    fn capability(&self, bytes: &[u8]) -> Result<Self::Capability, Error> {
        JsonHeld::decode(bytes, self.limits, self.turn).map_err(decode_error)
    }
}
fn decode_error(error: PeerResourceError) -> Error {
    match error {
        PeerResourceError::Malformed => policy(MediaPolicyError::Malformed),
        PeerResourceError::Capacity => Error::Decode(error),
    }
}
fn policy(error: MediaPolicyError) -> Error {
    Error::Policy(error)
}
fn strict_error(error: ReadError<MediaPolicyError>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Resource(error) => Error::Resource(error),
        ReadError::Visitor(error) => policy(error),
    }
}
fn visit_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Resource(error) => Error::Resource(error),
        ReadError::Visitor(error) => error,
    }
}

pub(super) fn legacy(
    room: &automerge::AutoCommit,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    project(
        &LegacyRead(room),
        &LegacyCopy,
        &Legacy,
        extra,
        &mut Work {
            collections: Collections::unqualified(),
        },
    )
    .map_err(super::read::legacy_error)
}
pub(crate) fn bounded<R: RoomRead>(
    room: &R,
    extra: &[RevocationEntry],
    turn: &NativeWorkTurn,
    limits: BasisLimits,
) -> Result<[u8; 32], Error> {
    bounded_entries(room, extra.iter(), turn, limits)
}
/// The already verified local union can be borrowed directly; no cloned input
/// vector is needed. The existing projection still pays all canonical work.
pub(crate) fn bounded_entries<'a, R: RoomRead>(
    room: &R,
    extra: impl IntoIterator<Item = &'a RevocationEntry>,
    turn: &NativeWorkTurn,
    limits: BasisLimits,
) -> Result<[u8; 32], Error> {
    // Reserve before the first owned collection; this caller ceiling is a
    // refusal policy, not a promise that arbitrary policy fits.
    let mut work = Work {
        collections: Collections::native(turn, limits.collection_bytes).map_err(Error::Resource)?,
    };
    project(
        room,
        &TurnCopy(turn),
        &Bounded {
            turn,
            limits: limits.json,
        },
        extra,
        &mut work,
    )
}

fn project<'a, R: RoomRead, C: CopyValue, D: Decode>(
    room: &R,
    copies: &C,
    decoder: &D,
    extra: impl IntoIterator<Item = &'a RevocationEntry>,
    work: &mut Work,
) -> Result<[u8; 32], Error> {
    let owner = read::strict_string(room, copies, &room.root(), "owner").map_err(strict_error)?;
    let owner_key = super::canonical_key(owner.as_ref()).map_err(policy)?;
    let caps = read::strict_map(room, &room.root(), "capabilities").map_err(strict_error)?;
    let mut capabilities = BTreeMap::new();
    room.visit_keys(&caps, |key| {
        let bytes = read::strict_bytes(room, copies, &caps, key).map_err(strict_error)?;
        let value = decoder.capability(bytes.as_ref())?;
        let encoded = work.encoded(value.value())?;
        let key = work.text(key)?;
        work.tree_entry::<String, Vec<u8>>()?;
        capabilities.insert(key, encoded);
        Ok(())
    })
    .map_err(visit_error)?;
    let peers = read::strict_map(room, &room.root(), "peers").map_err(strict_error)?;
    let mut roster = BTreeSet::new();
    room.visit_keys(&peers, |key| {
        read::strict_string(room, copies, &peers, key).map_err(strict_error)?;
        let device = super::canonical_key(key).map_err(policy)?;
        work.tree_entry::<[u8; 32], ()>()?;
        roster.insert(device);
        Ok(())
    })
    .map_err(visit_error)?;
    let identities =
        read::strict_map(room, &room.root(), "peer_identities_v1").map_err(strict_error)?;
    let mut rows = BTreeMap::new();
    let mut certificates = BTreeMap::new();
    let mut revocations = Vec::new();
    room.visit_keys(&identities, |key| {
        let bytes = read::strict_bytes(room, copies, &identities, key).map_err(strict_error)?;
        let held = decoder.identity(bytes.as_ref())?;
        let row = held.value();
        let device = super::canonical_key(key).map_err(policy)?;
        if device != row.device_cert.device_public_key {
            return Err(policy(MediaPolicyError::Binding));
        }
        tilo_user_identity::verify_device_cert(&row.device_cert)
            .map_err(|_| policy(MediaPolicyError::Binding))?;
        let mut admissions = BTreeSet::new();
        for proof in row.admission.iter().chain(&row.admissions) {
            if proof.device_public_key != device
                || proof.user_public_key != row.device_cert.user_public_key
            {
                return Err(policy(MediaPolicyError::Binding));
            }
            let bytes = work.encoded(proof)?;
            work.tree_entry::<Vec<u8>, ()>()?;
            admissions.insert(bytes);
        }
        let mut delegations = BTreeSet::new();
        for delegation in &row.workspace_delegations {
            let bytes = work.encoded(delegation)?;
            work.tree_entry::<Vec<u8>, ()>()?;
            delegations.insert(bytes);
        }
        for entry in &row.revocations {
            if entry.user_public_key != row.device_cert.user_public_key {
                return Err(policy(MediaPolicyError::Binding));
            }
            work.push(&mut revocations, entry.clone())?;
        }
        work.tree_entry::<[u8; 32], [u8; 32]>()?;
        certificates.insert(device, row.device_cert.user_public_key);
        let cert = DeviceCert {
            user_public_key: row.device_cert.user_public_key,
            device_public_key: row.device_cert.device_public_key,
            device_label: work.text(&row.device_cert.device_label)?,
            capabilities: row.device_cert.capabilities.clone(),
            issued_at_ns: row.device_cert.issued_at_ns,
            expires_at_ns: row.device_cert.expires_at_ns,
            signature: row.device_cert.signature,
        };
        work.tree_entry::<[u8; 32], (DeviceCert, BTreeSet<Vec<u8>>, BTreeSet<Vec<u8>>)>()?;
        rows.insert(device, (cert, admissions, delegations));
        Ok(())
    })
    .map_err(visit_error)?;
    if !certificates.values().any(|root| *root == owner_key)
        || roster
            .iter()
            .any(|device| !certificates.contains_key(device))
    {
        return Err(policy(MediaPolicyError::Binding));
    }
    for entry in extra {
        work.push(&mut revocations, entry.clone())?;
    }
    let mut encoded = BTreeMap::new();
    for entry in revocations {
        if !certificates
            .values()
            .any(|root| *root == entry.user_public_key)
            || certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|root| *root != entry.user_public_key)
        {
            return Err(policy(MediaPolicyError::Binding));
        }
        let bytes = work.encoded(&entry)?;
        work.tree_entry::<Vec<u8>, RevocationEntry>()?;
        encoded.insert(bytes, entry);
    }
    // Keep the existing verified union, including equal-timestamp behavior.
    // Its input order is canonical original entry bytes, unchanged from legacy.
    work.revocation_table(encoded.len())?;
    let mut verified = RevocationList::new();
    for entry in encoded.into_values() {
        verified
            .insert(entry)
            .map_err(|_| policy(MediaPolicyError::Binding))?;
    }
    let mut revocations = BTreeMap::new();
    for entry in verified.iter() {
        work.tree_entry::<[u8; 32], &RevocationEntry>()?;
        revocations.insert(entry.revoked_device_public_key, entry);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-room-media-auth-basis-v1\0");
    struct Hash<'a>(&'a mut blake3::Hasher);
    impl postcard::ser_flavors::Flavor for Hash<'_> {
        type Output = ();
        fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
            self.0.update(&[byte]);
            Ok(())
        }
        fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
            self.0.update(bytes);
            Ok(())
        }
        fn finalize(self) -> postcard::Result<()> {
            Ok(())
        }
    }
    postcard::serialize_with_flavor(
        &(owner_key, capabilities, roster, rows, revocations),
        Hash(&mut hasher),
    )
    .map_err(|_| policy(MediaPolicyError::Malformed))?;
    Ok(*hasher.finalize().as_bytes())
}

// Keep this work alive after all project temporaries. The shared private
// budget preserves the reviewed cumulative debit and growth-overlap rule.
struct Work {
    collections: Collections,
}
impl Work {
    fn debit(&mut self, bytes: Option<usize>) -> Result<(), Error> {
        self.collections.debit(bytes).map_err(Error::Resource)
    }
    fn tree_entry<K, V>(&mut self) -> Result<(), Error> {
        self.collections
            .tree_entry::<K, V>()
            .map_err(Error::Resource)
    }
    fn text(&mut self, value: &str) -> Result<String, Error> {
        self.collections.text(value).map_err(Error::Resource)
    }
    fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<(), Error> {
        self.collections
            .push(values, value)
            .map_err(Error::Resource)
    }
    fn encoded<T: Serialize>(&mut self, value: &T) -> Result<Vec<u8>, Error> {
        struct Count(usize);
        impl postcard::ser_flavors::Flavor for Count {
            type Output = usize;
            fn try_push(&mut self, _: u8) -> postcard::Result<()> {
                self.try_extend(&[0])
            }
            fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .ok_or(postcard::Error::SerializeBufferFull)?;
                Ok(())
            }
            fn finalize(self) -> postcard::Result<usize> {
                Ok(self.0)
            }
        }
        // Only the audited certificate/proof/capability DTOs reach this helper;
        // their Serialize implementations borrow fields without temporary heap.
        let length = postcard::serialize_with_flavor(value, Count(0))
            .map_err(|_| policy(MediaPolicyError::Malformed))?;
        let mut bytes = self.collections.zeroed(length).map_err(Error::Resource)?;
        postcard::to_slice(value, &mut bytes).map_err(|_| policy(MediaPolicyError::Malformed))?;
        Ok(bytes)
    }
    fn revocation_table(&mut self, count: usize) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        // Same pinned std/hashbrown0.17.1 concrete map layout audited by the
        // bounded revocation restore:4/8/16 initial buckets, then7/8 loading.
        let capacity = || Error::Resource(NativeResourceError::WorkBytes);
        let buckets = if count < 4 {
            4
        } else if count < 8 {
            8
        } else if count < 15 {
            16
        } else {
            (count.checked_mul(8).ok_or_else(capacity)? / 7)
                .checked_next_power_of_two()
                .ok_or_else(capacity)?
        };
        type Entry = ([u8; 32], RevocationEntry);
        let alignment = align_of::<Entry>().max(16);
        let data = size_of::<Entry>()
            .checked_mul(buckets)
            .ok_or_else(capacity)?;
        let padded = data.checked_add(alignment - 1).ok_or_else(capacity)? & !(alignment - 1);
        self.debit(
            padded
                .checked_add(buckets)
                .and_then(|n| n.checked_add(16))
                .and_then(|n| n.checked_mul(2)),
        )
    }
}
#[cfg(test)]
mod tests;
