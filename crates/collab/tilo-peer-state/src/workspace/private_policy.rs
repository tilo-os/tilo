//! Shared private policy data. These values confer no caller or owner authority.
use super::read::{LegacyRead, collections::Collections};
use automerge::{AutoCommit, AutomergeError};
use tilo_user_identity::{DeviceCert, RevocationEntry};

#[derive(Debug)]
pub(crate) enum Error {
    Legacy(AutomergeError),
    Capacity,
    Rule(Rule),
    Encoding(postcard::Error),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    Nonpersistent,
    OwnerAmbiguous,
    OwnerInvalid,
    OwnerRoot,
    MediaUnreadable,
    Read,
}
impl Error {
    pub(crate) fn legacy(self) -> String {
        match self {
            Self::Legacy(e) => e.to_string(),
            Self::Encoding(e) => e.to_string(),
            Self::Rule(rule) => match rule {
                Rule::Nonpersistent => "nonpersistent governed private policy grant",
                Rule::OwnerAmbiguous => "ambiguous canonical workspace/owner",
                Rule::OwnerInvalid => "invalid canonical owner",
                Rule::OwnerRoot => "owner differs from the workspace's owner root",
                Rule::MediaUnreadable => "unreadable governed media policy",
                Rule::Read => "private policy read failed",
            }
            .into(),
            Self::Capacity => "private policy resource refusal: Capacity".into(),
        }
    }
}
pub(crate) struct Metadata<'a, I, J> {
    pub(crate) workspace: &'a str,
    pub(crate) owner: &'a [u8; 32],
    pub(crate) certificates: I,
    pub(crate) revocations: J,
}
pub(crate) struct LegacyBasis {
    pub(crate) revision: String,
    pub(crate) expiries: Vec<u64>,
}
use super::media::fingerprint::FingerprintRead;
use super::read::{self, Capacity, ReadError, WorkspaceValue};
use automerge::ReadDoc;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::collections::BTreeMap;
use tilo_action_policy::Lifetime;

fn resource(_: Capacity) -> Error {
    Error::Capacity
}
fn rule(value: Rule) -> Error {
    Error::Rule(value)
}
fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(e) => e,
        ReadError::Malformed => rule(Rule::Read),
    }
}
// Reads keep the original get_all diagnostics.
trait PrivateRead: FingerprintRead {
    fn all(
        &self,
        object: &Self::Object,
        key: &str,
        visitor: impl for<'a> FnMut(WorkspaceValue<'a, Self::Object>) -> Result<(), Error>,
    ) -> Result<usize, Error>;
}
impl PrivateRead for LegacyRead<'_, AutoCommit> {
    fn all(
        &self,
        object: &Self::Object,
        key: &str,
        mut visitor: impl for<'a> FnMut(WorkspaceValue<'a, Self::Object>) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        let values = self.0.get_all(object, key).map_err(Error::Legacy)?;
        let count = values.len();
        for (value, id) in values {
            read::visit_legacy_value(value, id, &mut visitor).map_err(read_error)?;
        }
        Ok(count)
    }
}
/// The ascending capability expiries of the grants `policy` holds, in
/// seconds; a grant the workspace keeps must outlast the session it was
/// given in.
fn grants(
    policy: &tilo_action_policy::WorkspacePolicy,
    work: &mut Collections,
) -> Result<Vec<u64>, Error> {
    let mut expiries = Vec::new();
    for grant in &policy.grants {
        match grant.lifetime {
            Lifetime::Once | Lifetime::Session(_) => return Err(rule(Rule::Nonpersistent)),
            // Expiries are compared in seconds, rounded up so a grant is never
            // reported gone before it is.
            Lifetime::Until(at) => work
                .push(&mut expiries, at.div_ceil(1000))
                .map_err(resource)?,
            Lifetime::UntilRevoked => {}
        }
    }
    expiries.sort_unstable();
    expiries.dedup();
    Ok(expiries)
}
fn canonical_owner<R: PrivateRead>(
    read: &R,
    workspace: &str,
    owner: &[u8; 32],
) -> Result<String, Error> {
    let expected = hex::encode(owner);
    let mut is_string = false;
    let mut owner_is_root = false;
    let owners = read.all(&read.root(), "owner", |value| {
        if let WorkspaceValue::Str(value) = value {
            is_string = true;
            owner_is_root = value.as_bytes() == expected.as_bytes();
        }
        Ok(())
    })?;
    let mut matches_workspace = false;
    let workspace_count = read.all(&read.root(), "room", |value| {
        matches_workspace = matches!(value,WorkspaceValue::Str(value) if value==workspace);
        Ok(())
    })?;
    if owners != 1 || workspace_count != 1 || !matches_workspace {
        return Err(rule(Rule::OwnerAmbiguous));
    }
    if !is_string {
        return Err(rule(Rule::OwnerInvalid));
    }
    if !owner_is_root {
        return Err(rule(Rule::OwnerRoot));
    }
    let mut owner = None;
    read.all(&read.root(), "owner", |value| {
        let WorkspaceValue::Str(value) = value else {
            return Err(rule(Rule::OwnerInvalid));
        };
        owner = Some(value.to_owned());
        Ok(())
    })?;
    owner.ok_or_else(|| rule(Rule::OwnerInvalid))
}
struct CertificateRows<I>(I);
impl<'a, I> Serialize for CertificateRows<I>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for cert in self.0.clone() {
            seq.serialize_element(cert)?;
        }
        seq.end()
    }
}
fn revision<'a, R, I, J>(
    read: &R,
    ops: &BTreeMap<String, Vec<u8>>,
    meta: Metadata<'a, I, J>,
    work: &mut Collections,
) -> Result<String, Error>
where
    R: PrivateRead,
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let owner = canonical_owner(read, meta.workspace, meta.owner)?;
    let mut peers = BTreeMap::new();
    let mut identities = BTreeMap::new();
    read::visit_governance_entries(read, |entry| {
        match entry {
            read::GovernanceEntry::Owner(_) | read::GovernanceEntry::Sandbox { .. } => (),
            read::GovernanceEntry::Peer { key, name } => {
                work.tree_entry::<String, String>().map_err(resource)?;
                peers.insert(
                    work.text(key).map_err(resource)?,
                    work.text(name).map_err(resource)?,
                );
            }
            read::GovernanceEntry::Identity { key, bytes } => {
                work.tree_entry::<String, Vec<u8>>().map_err(resource)?;
                identities.insert(
                    work.text(key).map_err(resource)?,
                    work.bytes(bytes).map_err(resource)?,
                );
            }
        }
        Ok(())
    })
    .map_err(read_error)?;
    let mut media = Vec::new();
    let count = read
        .visit_media_fingerprints(|hash| work.push(&mut media, hash).map_err(resource))
        .map_err(|e| match e {
            ReadError::Malformed => rule(Rule::MediaUnreadable),
            other => read_error(other),
        })?;
    media.sort_unstable();
    media.dedup();
    let mut revocations = Vec::new();
    for entry in meta.revocations {
        work.push(&mut revocations, entry).map_err(resource)?;
    }
    // Verified union targets are unique, so this order is total.
    revocations.sort_unstable_by_key(|entry| entry.revoked_device_public_key);
    let mut hash = blake3::Hasher::new_derive_key("Tilo agent governance revision v1");
    crate::postcard_sink::hash_into(
        &(
            meta.workspace,
            meta.owner,
            owner.as_str(),
            ops,
            peers,
            identities,
            (u8::from(count != 0), media),
            CertificateRows(meta.certificates),
            revocations,
        ),
        &mut hash,
        usize::MAX,
    )
    .map_err(Error::Encoding)?;
    let text = hex::encode(hash.finalize().as_bytes());
    work.text(&text).map_err(resource)
}
pub(crate) fn legacy_owner(
    read: &LegacyRead<'_, AutoCommit>,
    workspace: &str,
    owner: &[u8; 32],
) -> Result<String, Error> {
    canonical_owner(read, workspace, owner)
}
pub(crate) fn legacy_revision<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
) -> Result<String, Error>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    revision(
        read,
        &crate::members::replay::raw(read.0),
        meta,
        &mut Collections::unqualified(),
    )
}
pub(crate) fn legacy<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
) -> Result<LegacyBasis, Error>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let mut work = Collections::unqualified();
    let expiries = grants(&crate::members::replay(read.0, meta.workspace), &mut work)?;
    let revision = revision(read, &crate::members::replay::raw(read.0), meta, &mut work)?;
    Ok(LegacyBasis { revision, expiries })
}
#[cfg(test)]
mod tests;
