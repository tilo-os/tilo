//! The security-basis algorithm.
use super::{MediaPolicyError, read::Error};
use crate::cached_document::CachedDocument;
use crate::state::GovernedIdentity;
use crate::workspace::read::collections::Collections;
use crate::workspace::read::{self, Capacity, LegacyRead, ReadError, WorkspaceRead};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tilo_action_policy::{Grant, Role};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

trait Decode {
    fn identity(&self, key: &str, bytes: &[u8]) -> Result<GovernedIdentity, Error>;
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error>;
    fn member(&self, bytes: &[u8]) -> Result<Role, Error>;
}
struct Legacy;
impl Decode for Legacy {
    fn identity(&self, _key: &str, bytes: &[u8]) -> Result<GovernedIdentity, Error> {
        GovernedIdentity::decode(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
    fn member(&self, bytes: &[u8]) -> Result<Role, Error> {
        serde_json::from_slice(bytes).map_err(|_| policy(MediaPolicyError::Malformed))
    }
}
/// The identity rows as their governance state decoded them once, the rest
/// as [`Legacy`] decodes it.
struct Decoded<'a>(&'a CachedDocument);
impl Decode for Decoded<'_> {
    fn identity(&self, key: &str, bytes: &[u8]) -> Result<GovernedIdentity, Error> {
        match self.0.decoded_identity(key, bytes) {
            Some(identity) => Ok(identity.clone()),
            None => Legacy.identity(key, bytes),
        }
    }
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error> {
        Legacy.grant(bytes)
    }
    fn member(&self, bytes: &[u8]) -> Result<Role, Error> {
        Legacy.member(bytes)
    }
}
fn policy(error: MediaPolicyError) -> Error {
    Error::Policy(error)
}
pub(super) fn strict_error(error: ReadError<MediaPolicyError>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => policy(error),
    }
}
pub(super) fn visit_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => policy(MediaPolicyError::Malformed),
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => error,
    }
}

pub(super) fn legacy(
    workspace: &automerge::AutoCommit,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    project(
        &LegacyRead(workspace),
        &Legacy,
        extra,
        &mut Work {
            collections: Collections::unqualified(),
        },
    )
    .map_err(super::read::legacy_error)
}

/// [`legacy`] of a governance state whose identity rows are decoded once for
/// it.
pub(super) fn decoded(
    workspace: &CachedDocument,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    project(
        &LegacyRead(&**workspace),
        &Decoded(workspace),
        extra,
        &mut Work {
            collections: Collections::unqualified(),
        },
    )
    .map_err(super::read::legacy_error)
}

#[expect(
    clippy::too_many_lines,
    reason = "one canonical basis projection, in wire order"
)]
fn project<'a, R: WorkspaceRead, D: Decode>(
    workspace: &R,
    decoder: &D,
    extra: impl IntoIterator<Item = &'a RevocationEntry>,
    work: &mut Work,
) -> Result<[u8; 32], Error> {
    let owner = read::strict_string(workspace, &workspace.root(), "owner").map_err(strict_error)?;
    let owner_key = super::canonical_key(&owner).map_err(policy)?;
    let grant_map =
        read::strict_map(workspace, &workspace.root(), "grants").map_err(strict_error)?;
    let mut grants = BTreeMap::new();
    workspace
        .visit_keys(&grant_map, |key| {
            let bytes = read::strict_bytes(workspace, &grant_map, key).map_err(strict_error)?;
            let value = decoder.grant(&bytes)?;
            let encoded = work.encoded(&value)?;
            let key = work.text(key)?;
            work.tree_entry::<String, Vec<u8>>()?;
            grants.insert(key, encoded);
            Ok(())
        })
        .map_err(visit_error)?;
    let member_map =
        read::strict_map(workspace, &workspace.root(), "members").map_err(strict_error)?;
    let mut members = BTreeMap::new();
    workspace
        .visit_keys(&member_map, |key| {
            let bytes = read::strict_bytes(workspace, &member_map, key).map_err(strict_error)?;
            let role = decoder.member(&bytes)?;
            let key = work.text(key)?;
            work.tree_entry::<String, Role>()?;
            members.insert(key, role);
            Ok(())
        })
        .map_err(visit_error)?;
    let peers = read::strict_map(workspace, &workspace.root(), "peers").map_err(strict_error)?;
    let mut roster = BTreeSet::new();
    workspace
        .visit_keys(&peers, |key| {
            read::strict_string(workspace, &peers, key).map_err(strict_error)?;
            let device = super::canonical_key(key).map_err(policy)?;
            work.tree_entry::<[u8; 32], ()>()?;
            roster.insert(device);
            Ok(())
        })
        .map_err(visit_error)?;
    let identities = read::strict_map(workspace, &workspace.root(), "peer_identities_v1")
        .map_err(strict_error)?;
    let mut rows = BTreeMap::new();
    let mut certificates = BTreeMap::new();
    let mut revocations = Vec::new();
    workspace
        .visit_keys(&identities, |key| {
            let bytes = read::strict_bytes(workspace, &identities, key).map_err(strict_error)?;
            let row = &decoder.identity(key, &bytes)?;
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
            // A delegation a build before the owner-root layout wrote,
            // hashed in the place it always held: the stored policy's
            // authorization basis is a digest over this projection, and a
            // workspace whose rows carry one still names it.
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
            let cert = work.cert(&row.device_cert)?;
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
    // The domain stored policies were already dated under: a workspace's
    // authorization basis keeps its digest, whatever the code around it is
    // called now.
    hasher.update(b"tilo-room-media-auth-basis-v1\0");
    crate::postcard_sink::hash_into(
        &(owner_key, members, grants, roster, rows, revocations),
        &mut hasher,
        usize::MAX,
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
    /// A copy of `value` within the limit, with the certificate that issued
    /// it, which a minted grant's signature and so its basis rest on.
    fn cert(&mut self, value: &DeviceCert) -> Result<DeviceCert, Error> {
        Ok(DeviceCert {
            user_public_key: value.user_public_key,
            device_public_key: value.device_public_key,
            device_label: self.text(&value.device_label)?,
            capabilities: value.capabilities.clone(),
            issued_at_ns: value.issued_at_ns,
            expires_at_ns: value.expires_at_ns,
            signature: value.signature,
            issuer: match value.issuer.as_deref() {
                Some(issuer) => Some(Box::new(self.cert(issuer)?)),
                None => None,
            },
        })
    }
    fn encoded<T: Serialize>(&mut self, value: &T) -> Result<Vec<u8>, Error> {
        let length = crate::postcard_sink::encoded_len(value, usize::MAX)
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
