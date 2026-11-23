//! The security-basis algorithm.
use super::MediaPolicyError;
use crate::cached_document::CachedDocument;
use crate::state::GovernedIdentity;
use crate::workspace::read::{self, WorkspaceRead, policy_error};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tilo_user_identity::{RevocationEntry, RevocationList};

/// The security basis of `workspace` under the revocations `extra` adds.
pub(super) fn of_document(
    workspace: &automerge::AutoCommit,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    project(
        workspace,
        None,
        &crate::members::replay_declared(workspace),
        extra,
    )
}

/// [`of_document`] of a governance state whose identity rows are decoded
/// once for it.
pub(super) fn decoded(
    workspace: &CachedDocument,
    extra: &[RevocationEntry],
) -> Result<[u8; 32], MediaPolicyError> {
    project(
        &**workspace,
        Some(workspace),
        &crate::members::replay_declared(workspace),
        extra,
    )
}

fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>, MediaPolicyError> {
    postcard::to_allocvec(value).map_err(|_| MediaPolicyError::Malformed)
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
) -> Result<[u8; 32], MediaPolicyError> {
    let root = automerge::ROOT;
    let owner = read::strict_string(workspace, &root, "owner").map_err(policy_error)?;
    let owner_key = super::canonical_key(&owner)?;
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
            named.insert(super::canonical_key(key)?);
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
                None => {
                    GovernedIdentity::decode(&bytes).map_err(|_| MediaPolicyError::Malformed)?
                }
            };
            let device = super::canonical_key(key)?;
            if device != row.device_cert.device_public_key {
                return Err(MediaPolicyError::Identity);
            }
            tilo_user_identity::verify_device_cert(&row.device_cert)
                .map_err(|_| MediaPolicyError::Identity)?;
            let mut admissions = BTreeSet::new();
            for proof in row.admission.iter().chain(&row.admissions) {
                if proof.device_public_key != device
                    || proof.user_public_key != row.device_cert.user_public_key
                {
                    return Err(MediaPolicyError::Identity);
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
                    return Err(MediaPolicyError::Revocation);
                }
                revocations.push(entry.clone());
            }
            certificates.insert(device, row.device_cert.user_public_key);
            rows.insert(device, (row.device_cert.clone(), admissions, delegations));
            Ok(())
        })
        .map_err(policy_error)?;
    if !certificates.values().any(|root| *root == owner_key) {
        return Err(MediaPolicyError::OwnerUnadmitted);
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
            return Err(MediaPolicyError::NamesNobody(key));
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
            return Err(MediaPolicyError::Revocation);
        }
        by_bytes.insert(encoded(&entry)?, entry);
    }
    // Keep the existing verified union, including equal-timestamp behavior.
    // Its input order is canonical entry bytes.
    let mut verified = RevocationList::new();
    for entry in by_bytes.into_values() {
        verified
            .insert(entry)
            .map_err(|_| MediaPolicyError::Revocation)?;
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
    .map_err(|_| MediaPolicyError::Malformed)?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests;
