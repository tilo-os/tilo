//! Shared private policy data. These values confer no caller or owner authority.
use super::read::{LegacyRead, collections::Collections};
use crate::resources::{PeerResourceError, json::JsonLimits};
use automerge::{AutoCommit, AutomergeError};
use tilo_user_identity::{DeviceCert, RevocationEntry};

#[derive(Debug)]
pub(crate) enum Error {
    Legacy(AutomergeError),
    Capacity,
    Decode(PeerResourceError),
    Rule(Rule),
    Encoding(postcard::Error),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    GrantsAmbiguous,
    GrantsInvalid,
    GrantAmbiguous,
    GrantMissing,
    GrantInvalid,
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
                Rule::GrantsAmbiguous => "ambiguous private policy grants",
                Rule::GrantsInvalid => "invalid private policy grants",
                Rule::GrantAmbiguous => "ambiguous private policy grant",
                Rule::GrantMissing => "missing private policy grant",
                Rule::GrantInvalid => "invalid private policy grant",
                Rule::Nonpersistent => "nonpersistent governed private policy grant",
                Rule::OwnerAmbiguous => "ambiguous canonical room/owner",
                Rule::OwnerInvalid => "invalid canonical owner",
                Rule::OwnerRoot => "owner differs from pinned authority root",
                Rule::MediaUnreadable => "unreadable governed media policy",
                Rule::Read => "private policy read failed",
            }
            .into(),
            error => format!("private policy resource refusal: {error:?}"),
        }
    }
}
pub(crate) struct Metadata<'a, I, J> {
    pub(crate) room: &'a str,
    pub(crate) authority: &'a DeviceCert,
    pub(crate) certificates: I,
    pub(crate) revocations: J,
}
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) collections: usize,
    pub(crate) json: JsonLimits,
}
pub(crate) struct LegacyBasis {
    pub(crate) revision: String,
    pub(crate) expiries: Vec<u64>,
}
pub(crate) struct NativeBasis {
    revision: String,
    expiries: Vec<u64>,
}
impl NativeBasis {
    pub(crate) fn revision(&self) -> &str {
        &self.revision
    }
    pub(crate) fn next_expiry_after(&self, at: u64) -> Option<u64> {
        self.expiries.iter().copied().find(|v| *v > at)
    }
}
use super::media::fingerprint::FingerprintRead;
use super::read::{self, Capacity, ReadError, RoomValue};
use crate::resources::json;
use automerge::{ObjType, ReadDoc};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::collections::BTreeMap;
use tilo_action_policy::{Grant, Lifetime};

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
        visitor: impl for<'a> FnMut(RoomValue<'a, Self::Object>) -> Result<(), Error>,
    ) -> Result<usize, Error>;
}
impl PrivateRead for LegacyRead<'_, AutoCommit> {
    fn all(
        &self,
        object: &Self::Object,
        key: &str,
        mut visitor: impl for<'a> FnMut(RoomValue<'a, Self::Object>) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        let values = self.0.get_all(object, key).map_err(Error::Legacy)?;
        let count = values.len();
        for (value, id) in values {
            read::visit_legacy_value(value, id, &mut visitor).map_err(read_error)?;
        }
        Ok(count)
    }
}
trait Decode {
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error>;
}
struct LegacyDecode;
impl Decode for LegacyDecode {
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error> {
        serde_json::from_slice(bytes).map_err(|_| rule(Rule::GrantInvalid))
    }
}
struct NativeDecode {
    limits: JsonLimits,
}
impl Decode for NativeDecode {
    fn grant(&self, bytes: &[u8]) -> Result<Grant, Error> {
        json::decode(bytes, self.limits).map_err(|error| match error {
            PeerResourceError::Malformed => rule(Rule::GrantInvalid),
            PeerResourceError::Capacity => Error::Decode(PeerResourceError::Capacity),
        })
    }
}
fn grants<R: PrivateRead, D: Decode>(
    read: &R,
    decode: &D,
    work: &mut Collections,
) -> Result<Vec<u64>, Error> {
    let mut object = None;
    let count = read.all(&read.root(), "grants", |value| {
        if let RoomValue::Object {
            kind: ObjType::Map,
            object: found,
        } = value
        {
            object = Some(found);
        }
        Ok(())
    })?;
    if count > 1 {
        return Err(rule(Rule::GrantsAmbiguous));
    }
    let mut expiries = Vec::new();
    if count == 1 {
        let object = object.ok_or_else(|| rule(Rule::GrantsInvalid))?;
        read.visit_keys(&object, |key| {
            // Count and scalar shape precede JSON/persistence, even when the
            // first conflicting value happens to contain malformed bytes.
            let mut bytes_shape = false;
            let count = read.all(&object, key, |value| {
                bytes_shape = matches!(value, RoomValue::Bytes(_));
                Ok(())
            })?;
            if count != 1 {
                return Err(rule(Rule::GrantAmbiguous));
            }
            if !bytes_shape {
                return Err(rule(Rule::GrantInvalid));
            }
            let mut seen = false;
            read.all(&object, key, |value| {
                let RoomValue::Bytes(bytes) = value else {
                    return Err(rule(Rule::GrantInvalid));
                };
                seen = true;
                let grant = decode.grant(bytes)?;
                match grant.lifetime {
                    Lifetime::Once | Lifetime::Session(_) => {
                        return Err(rule(Rule::Nonpersistent));
                    }
                    // Expiries are compared in seconds, rounded up so a grant is
                    // never reported gone before it is.
                    Lifetime::Until(at) => work
                        .push(&mut expiries, at.div_ceil(1000))
                        .map_err(resource)?,
                    Lifetime::UntilRevoked => {}
                }
                Ok(())
            })?;
            if !seen {
                return Err(rule(Rule::GrantMissing));
            }
            Ok(())
        })
        .map_err(read_error)?;
    }
    expiries.sort_unstable();
    expiries.dedup();
    Ok(expiries)
}
fn canonical_owner<R: PrivateRead>(
    read: &R,
    room: &str,
    authority: &DeviceCert,
) -> Result<String, Error> {
    let expected = crate::room::key_hex::KeyHex::new(&authority.user_public_key);
    let mut is_string = false;
    let mut owner_is_root = false;
    let owners = read.all(&read.root(), "owner", |value| {
        if let RoomValue::Str(value) = value {
            is_string = true;
            owner_is_root = value.as_bytes() == expected.as_bytes();
        }
        Ok(())
    })?;
    let mut matches_room = false;
    let room_count = read.all(&read.root(), "room", |value| {
        matches_room = matches!(value,RoomValue::Str(value) if value==room);
        Ok(())
    })?;
    if owners != 1 || room_count != 1 || !matches_room {
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
        let RoomValue::Str(value) = value else {
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
    meta: Metadata<'a, I, J>,
    work: &mut Collections,
) -> Result<String, Error>
where
    R: PrivateRead,
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let owner = canonical_owner(read, meta.room, meta.authority)?;
    let mut grants = BTreeMap::new();
    let mut members = BTreeMap::new();
    let mut peers = BTreeMap::new();
    let mut identities = BTreeMap::new();
    read::visit_governance_entries(read, |entry| {
        match entry {
            read::GovernanceEntry::Owner(_)
            | read::GovernanceEntry::Sandbox { .. }
            | read::GovernanceEntry::MemberJoined { .. } => (),
            read::GovernanceEntry::Grant { key, bytes } => {
                work.tree_entry::<String, Vec<u8>>().map_err(resource)?;
                grants.insert(
                    work.text(key).map_err(resource)?,
                    work.bytes(bytes).map_err(resource)?,
                );
            }
            read::GovernanceEntry::Member { key, bytes } => {
                work.tree_entry::<String, Vec<u8>>().map_err(resource)?;
                members.insert(
                    work.text(key).map_err(resource)?,
                    work.bytes(bytes).map_err(resource)?,
                );
            }
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
            meta.room,
            meta.authority,
            owner.as_str(),
            members,
            grants,
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
    let text = crate::room::key_hex::KeyHex::new(hash.finalize().as_bytes());
    work.text(text.as_str()).map_err(resource)
}
pub(crate) fn legacy_owner(
    read: &LegacyRead<'_, AutoCommit>,
    room: &str,
    authority: &DeviceCert,
) -> Result<String, Error> {
    canonical_owner(read, room, authority)
}
pub(crate) fn legacy_revision<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
) -> Result<String, Error>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    revision(read, meta, &mut Collections::unqualified())
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
    let expiries = grants(read, &LegacyDecode, &mut work)?;
    let revision = revision(read, meta, &mut work)?;
    Ok(LegacyBasis { revision, expiries })
}
pub(crate) fn native<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
    limits: Limits,
) -> Result<NativeBasis, Error>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let mut work = Collections::limited(limits.collections);
    let expiries = grants(
        read,
        &NativeDecode {
            limits: limits.json,
        },
        &mut work,
    )?;
    let revision = revision(read, meta, &mut work)?;
    Ok(NativeBasis { revision, expiries })
}
#[cfg(test)]
mod tests;
