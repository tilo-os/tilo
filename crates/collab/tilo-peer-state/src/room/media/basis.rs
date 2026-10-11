//! One security-basis algorithm for both storage modes.
use super::{MediaPolicyError, read::Error};
use crate::resources::{
    PeerResourceError,
    json::{self, JsonLimits},
};
use crate::room::read::collections::Collections;
use crate::room::read::{self, Capacity, LegacyRead, ReadError, RoomRead};
use crate::state::GovernedIdentity;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tilo_action_policy::{Grant, Role};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

#[derive(Clone, Copy)]
pub(crate) struct BasisLimits {
    pub(crate) collection_bytes: usize,
    pub(crate) json: JsonLimits,
}
trait Decode {
    fn identity(&self, bytes: &[u8]) -> Result<GovernedIdentity, Error>;
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error>;
    fn member(&self, bytes: &[u8]) -> Result<Role, Error>;
}
struct Legacy;
impl Decode for Legacy {
    fn identity(&self, bytes: &[u8]) -> Result<GovernedIdentity, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
    fn member(&self, bytes: &[u8]) -> Result<Role, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
}
struct Bounded {
    limits: JsonLimits,
}
impl Decode for Bounded {
    fn identity(&self, bytes: &[u8]) -> Result<GovernedIdentity, Error> {
        json::decode(bytes, self.limits).map_err(decode_error)
    }
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error> {
        json::decode(bytes, self.limits).map_err(decode_error)
    }
    fn member(&self, bytes: &[u8]) -> Result<Role, Error> {
        json::decode(bytes, self.limits).map_err(decode_error)
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
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => policy(error),
    }
}
fn visit_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => error,
    }
}

pub(super) fn legacy(
    room: &automerge::AutoCommit,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    project(
        &LegacyRead(room),
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
    limits: BasisLimits,
) -> Result<[u8; 32], Error> {
    bounded_entries(room, extra.iter(), limits)
}
/// The already verified local union is borrowed directly.
pub(crate) fn bounded_entries<'a, R: RoomRead>(
    room: &R,
    extra: impl IntoIterator<Item = &'a RevocationEntry>,
    limits: BasisLimits,
) -> Result<[u8; 32], Error> {
    let mut work = Work {
        collections: Collections::limited(limits.collection_bytes),
    };
    project(
        room,
        &Bounded {
            limits: limits.json,
        },
        extra,
        &mut work,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "one canonical basis projection, in wire order"
)]
fn project<'a, R: RoomRead, D: Decode>(
    room: &R,
    decoder: &D,
    extra: impl IntoIterator<Item = &'a RevocationEntry>,
    work: &mut Work,
) -> Result<[u8; 32], Error> {
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
    let owner = read::strict_string(room, &room.root(), "owner").map_err(strict_error)?;
    let owner_key = super::canonical_key(&owner).map_err(policy)?;
    let grant_map = read::strict_map(room, &room.root(), "grants").map_err(strict_error)?;
    let mut grants = BTreeMap::new();
    room.visit_keys(&grant_map, |key| {
        let bytes = read::strict_bytes(room, &grant_map, key).map_err(strict_error)?;
        let value = decoder.grant(&bytes)?;
        let encoded = work.encoded(&value)?;
        let key = work.text(key)?;
        work.tree_entry::<String, Vec<u8>>()?;
        grants.insert(key, encoded);
        Ok(())
    })
    .map_err(visit_error)?;
    let member_map = read::strict_map(room, &room.root(), "members").map_err(strict_error)?;
    let mut members = BTreeMap::new();
    room.visit_keys(&member_map, |key| {
        let bytes = read::strict_bytes(room, &member_map, key).map_err(strict_error)?;
        let role = decoder.member(&bytes)?;
        let key = work.text(key)?;
        work.tree_entry::<String, Role>()?;
        members.insert(key, role);
        Ok(())
    })
    .map_err(visit_error)?;
    let peers = read::strict_map(room, &room.root(), "peers").map_err(strict_error)?;
    let mut roster = BTreeSet::new();
    room.visit_keys(&peers, |key| {
        read::strict_string(room, &peers, key).map_err(strict_error)?;
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
        let bytes = read::strict_bytes(room, &identities, key).map_err(strict_error)?;
        let row = &decoder.identity(&bytes)?;
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
    postcard::serialize_with_flavor(
        &(owner_key, members, grants, roster, rows, revocations),
        Hash(&mut hasher),
    )
    .map_err(|_| policy(MediaPolicyError::Malformed))?;
    Ok(*hasher.finalize().as_bytes())
}

/// The collections limit for one basis projection.
struct Work {
    collections: Collections,
}
impl Work {
    fn debit(&mut self, bytes: Option<usize>) -> Result<(), Error> {
        self.collections.debit(bytes).map_err(capacity)
    }
    fn tree_entry<K, V>(&mut self) -> Result<(), Error> {
        self.collections.tree_entry::<K, V>().map_err(capacity)
    }
    fn text(&mut self, value: &str) -> Result<String, Error> {
        self.collections.text(value).map_err(capacity)
    }
    fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<(), Error> {
        self.collections.push(values, value).map_err(capacity)
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
        let length = postcard::serialize_with_flavor(value, Count(0))
            .map_err(|_| policy(MediaPolicyError::Malformed))?;
        let mut bytes = self.collections.zeroed(length).map_err(capacity)?;
        postcard::to_slice(value, &mut bytes).map_err(|_| policy(MediaPolicyError::Malformed))?;
        Ok(bytes)
    }
    fn revocation_table(&mut self, count: usize) -> Result<(), Error> {
        self.debit(count.checked_mul(size_of::<([u8; 32], RevocationEntry)>().saturating_mul(2)))
    }
}
fn capacity(_: Capacity) -> Error {
    Error::Capacity
}
#[cfg(test)]
mod tests;
