//! A workspace's security basis: one digest over the governance a device's
//! standing rests on (the owner, the membership, the devices and their
//! revocations), so two replicas compare what they admit by one value.
use crate::cached_document::CachedDocument;
use crate::state::DeviceRow;
use crate::workspace::read::{self, WorkspaceRead, policy_error};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tilo_user_identity::{RevocationEntry, RevocationList};

/// Why a workspace's governance has no security basis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BasisError {
    Malformed,
    Conflict,
    /// A field the workspace's governance needs is not there.
    Missing,
    /// A key is not a canonical nonzero key.
    Key,
    /// A device row of the workspace does not hold together.
    Identity,
    /// The workspace's owner has no admitted device.
    OwnerUnadmitted,
    /// The workspace keeps a name for this key, which is no member's, no
    /// device's and not the owner's.
    NamesNobody([u8; 32]),
    /// A revocation names a person the workspace does not hold, or another
    /// person's device.
    Revocation,
}

impl std::fmt::Display for BasisError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Malformed => "malformed workspace governance",
            Self::Conflict => "conflicting workspace governance",
            Self::Missing => "a field the workspace's governance needs is missing",
            Self::Key => "a key is not a canonical nonzero key",
            Self::Identity => {
                "a device row of the workspace does not hold together: its key, certificate or admission"
            }
            Self::OwnerUnadmitted => "the workspace's owner has no admitted device",
            Self::NamesNobody(key) => {
                return write!(
                    formatter,
                    "the workspace keeps a name for {}, who is no longer a member nor one of its devices",
                    hex::encode(key)
                );
            }
            Self::Revocation => {
                "a revocation names a person the workspace does not hold, or another person's device"
            }
        })
    }
}

impl std::error::Error for BasisError {}

/// Canonical security consistency digest, never an authority credential. Only
/// the governance's security fields participate; presentation labels do not. Extra
/// revocations are verified and intersect local knowledge before publication.
///
/// # Errors
///
/// Returns a [`BasisError`] if the security fields are malformed or an extra
/// revocation fails verification.
pub fn security_basis(
    doc: &automerge::AutoCommit,
    extra_revocations: &[RevocationEntry],
) -> Result<[u8; 32], BasisError> {
    of_document(doc, extra_revocations)
}

/// [`security_basis`] of a governance state, reading the identity rows it
/// decoded once rather than decoding each again.
pub(crate) fn security_basis_of(
    doc: &CachedDocument,
    extra_revocations: &[RevocationEntry],
) -> Result<[u8; 32], BasisError> {
    decoded(doc, extra_revocations)
}

/// A canonical lowercase-hex, nonzero 32-byte key.
fn canonical_key(value: &str) -> Result<[u8; 32], BasisError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BasisError::Key);
    }
    let mut key = [0; 32];
    hex::decode_to_slice(value, &mut key).map_err(|_| BasisError::Key)?;
    if key == [0; 32] {
        return Err(BasisError::Key);
    }
    Ok(key)
}

/// The security basis of `workspace` under the revocations `extra` adds.
fn of_document(
    workspace: &automerge::AutoCommit,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], BasisError> {
    project(
        workspace,
        None,
        &crate::members::replay_declared(workspace),
        extra,
    )
}

/// [`of_document`] of a governance state whose identity rows are decoded
/// once for it.
fn decoded(workspace: &CachedDocument, extra: &[RevocationEntry]) -> Result<[u8; 32], BasisError> {
    project(
        &**workspace,
        Some(workspace),
        &crate::members::replay_declared(workspace),
        extra,
    )
}

fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>, BasisError> {
    postcard::to_allocvec(value).map_err(|_| BasisError::Malformed)
}

#[expect(
    clippy::too_many_lines,
    reason = "one canonical basis projection, in wire order"
)]
fn project<'a>(
    workspace: &impl WorkspaceRead,
    decoded: Option<&CachedDocument>,
    membership: &tilo_action_policy::WorkspacePolicy,
    extra: impl IntoIterator<Item = &'a RevocationEntry>,
) -> Result<[u8; 32], BasisError> {
    let root = automerge::ROOT;
    let owner = read::strict_string(workspace, &root, "owner").map_err(policy_error)?;
    let owner_key = canonical_key(&owner)?;
    // The membership as the log makes it: who is in, and each grant by id.
    let mut grants = BTreeMap::new();
    for grant in &membership.grants {
        grants.insert(grant.id.as_str().to_owned(), encoded(grant)?);
    }
    let mut members = BTreeMap::new();
    for (person, role) in &membership.members {
        members.insert(person.clone(), *role);
    }
    let peers = read::strict_map(workspace, &root, "peers").map_err(policy_error)?;
    // The names the workspace keeps, by the key each is for: a device's, or a
    // person's root for someone it holds by root alone (a declared owner,
    // before any device of theirs arrived). Which is which the identity rows
    // below say.
    let mut named = BTreeSet::new();
    workspace
        .visit_entries(&peers, read::Selection::All, &mut |key, values| {
            read::strict_string_of(values).map_err(policy_error)?;
            named.insert(canonical_key(key)?);
            Ok(())
        })
        .map_err(policy_error)?;
    let identities =
        read::strict_map(workspace, &root, "peer_identities_v1").map_err(policy_error)?;
    let mut rows = BTreeMap::new();
    let mut certificates = BTreeMap::new();
    let mut revocations = Vec::new();
    workspace
        .visit_entries(&identities, read::Selection::All, &mut |key, values| {
            let bytes = read::strict_bytes_of(values).map_err(policy_error)?;
            let row = &match decoded.and_then(|doc| doc.decoded_identity(key, &bytes)) {
                Some(identity) => identity.clone(),
                None => DeviceRow::decode(&bytes).map_err(|_| BasisError::Malformed)?,
            };
            let device = canonical_key(key)?;
            if device != row.device_cert.device_public_key {
                return Err(BasisError::Identity);
            }
            tilo_user_identity::verify_device_cert(&row.device_cert)
                .map_err(|_| BasisError::Identity)?;
            let mut admissions = BTreeSet::new();
            for proof in row.admission.iter().chain(&row.admissions) {
                if proof.device_public_key != device
                    || proof.user_public_key != row.device_cert.user_public_key
                {
                    return Err(BasisError::Identity);
                }
                admissions.insert(encoded(proof)?);
            }
            // The row's delegations held this place in the projection's
            // shape, and a stored policy's authorization basis is a digest
            // over that shape: an empty set keeps every current row's digest
            // as it was.
            let delegations: BTreeSet<Vec<u8>> = BTreeSet::new();
            for entry in &row.revocations {
                if entry.user_public_key != row.device_cert.user_public_key {
                    return Err(BasisError::Revocation);
                }
                revocations.push(entry.clone());
            }
            certificates.insert(device, row.device_cert.user_public_key);
            rows.insert(device, (row.device_cert.clone(), admissions, delegations));
            Ok(())
        })
        .map_err(policy_error)?;
    if !certificates.values().any(|root| *root == owner_key) {
        return Err(BasisError::OwnerUnadmitted);
    }
    // The roster is the devices the policy's authorization rests on: every
    // device name is one of them. A name kept under a root names nobody's
    // device — it is the person's own, which no grant turns on — so it is none
    // of the roster and only has to name somebody the membership or the owner
    // holds. The owner is whoever the log hands the workspace to, as well as
    // the root that founded it: a host founds a workspace and hands it to the
    // person it is for, who is then no member beside it.
    let mut roster = BTreeSet::new();
    for key in named {
        let person = hex::encode(key);
        if certificates.contains_key(&key) {
            roster.insert(key);
        } else if key != owner_key
            && !membership.is_owner(&person)
            && !members.contains_key(&person)
        {
            return Err(BasisError::NamesNobody(key));
        }
    }
    revocations.extend(extra.into_iter().cloned());
    let mut by_bytes = BTreeMap::new();
    for entry in revocations {
        if !certificates
            .values()
            .any(|root| *root == entry.user_public_key)
            || certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|root| *root != entry.user_public_key)
        {
            return Err(BasisError::Revocation);
        }
        by_bytes.insert(encoded(&entry)?, entry);
    }
    // Keep the existing verified union, including equal-timestamp behavior.
    // Its input order is canonical entry bytes.
    let mut verified = RevocationList::new();
    for entry in by_bytes.into_values() {
        verified.insert(entry).map_err(|_| BasisError::Revocation)?;
    }
    let mut revocations = BTreeMap::new();
    for entry in verified.iter() {
        revocations.insert(entry.revoked_device_public_key, entry);
    }
    let mut hasher = blake3::Hasher::new();
    // The domain stored policies were already dated under: a workspace's
    // authorization basis keeps its digest, whatever the code around it is
    // called now.
    hasher.update(b"tilo-workspace-media-auth-basis-v1\0");
    crate::postcard_sink::hash_into(
        &(owner_key, members, grants, roster, rows, revocations),
        &mut hasher,
        usize::MAX,
    )
    .map_err(|_| BasisError::Malformed)?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
fn strict_map(
    doc: &impl automerge::ReadDoc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<automerge::ObjId, BasisError> {
    read::strict_map(doc, parent, key).map_err(policy_error)
}
#[cfg(test)]
fn strict_string(
    doc: &impl automerge::ReadDoc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<String, BasisError> {
    read::strict_string(doc, parent, key).map_err(policy_error)
}
#[cfg(test)]
fn strict_bytes(
    doc: &impl automerge::ReadDoc,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<Vec<u8>, BasisError> {
    read::strict_bytes(doc, parent, key).map_err(policy_error)
}

#[cfg(test)]
mod tests;
