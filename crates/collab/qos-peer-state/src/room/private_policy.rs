//! Shared private policy data. These values confer no caller or owner authority.
use super::read::{LegacyRead, collections::Collections};
use crate::resources::{PeerResourceError, json::JsonLimits};
use automerge::resource_limits::{NativeReadSession, NativeResourceError, NativeWorkTurn};
use automerge::{AutoCommit, AutomergeError};
use tilo_user_identity::{DeviceCert, RevocationEntry};

#[derive(Debug)]
pub(crate) enum Error {
    Legacy(AutomergeError),
    Resource(NativeResourceError),
    Decode(PeerResourceError),
    Rule(Rule),
    Encoding(postcard::Error),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    CapabilitiesAmbiguous,
    CapabilitiesInvalid,
    CapabilityAmbiguous,
    CapabilityMissing,
    CapabilityInvalid,
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
                Rule::CapabilitiesAmbiguous => "ambiguous private policy capabilities",
                Rule::CapabilitiesInvalid => "invalid private policy capabilities",
                Rule::CapabilityAmbiguous => "ambiguous private policy capability",
                Rule::CapabilityMissing => "missing private policy capability",
                Rule::CapabilityInvalid => "invalid private policy capability",
                Rule::Nonpersistent => "nonpersistent governed private policy capability",
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
    _collections: Collections,
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
use super::read::{
    self, CopyValue, LegacyCopy, ReadError, RoomRead, RoomValue, Selection, TurnCopy,
};
use crate::resources::json::JsonHeld;
use automerge::{ObjType, ReadDoc};
use collab_types::caps::{CapOverride, Expiry};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::collections::BTreeMap;

fn resource(error: NativeResourceError) -> Error {
    Error::Resource(error)
}
fn rule(value: Rule) -> Error {
    Error::Rule(value)
}
fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Resource(e) => resource(e),
        ReadError::Visitor(e) => e,
        ReadError::Malformed => rule(Rule::Read),
    }
}
// Only this strict legacy adapter retains the original get_all diagnostics.
// The shared decisions below are unchanged for the native borrowed reader.
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
impl PrivateRead for NativeReadSession<'_, '_> {
    fn all(
        &self,
        object: &Self::Object,
        key: &str,
        visitor: impl for<'a> FnMut(RoomValue<'a, Self::Object>) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        RoomRead::visit_property(self, object, key, Selection::All, visitor).map_err(read_error)
    }
}
trait CapValue {
    fn cap(&self) -> &CapOverride;
}
impl CapValue for CapOverride {
    fn cap(&self) -> &CapOverride {
        self
    }
}
impl CapValue for JsonHeld<CapOverride> {
    fn cap(&self) -> &CapOverride {
        self.value()
    }
}
trait Decode {
    type Cap: CapValue;
    fn capability(&self, bytes: &[u8]) -> Result<Self::Cap, Error>;
}
struct LegacyDecode;
impl Decode for LegacyDecode {
    type Cap = CapOverride;
    fn capability(&self, bytes: &[u8]) -> Result<Self::Cap, Error> {
        serde_json::from_slice(bytes).map_err(|_| rule(Rule::CapabilityInvalid))
    }
}
struct NativeDecode<'a> {
    turn: &'a NativeWorkTurn,
    limits: JsonLimits,
}
impl Decode for NativeDecode<'_> {
    type Cap = JsonHeld<CapOverride>;
    fn capability(&self, bytes: &[u8]) -> Result<Self::Cap, Error> {
        JsonHeld::decode(bytes, self.limits, self.turn).map_err(|error| match error {
            PeerResourceError::Malformed => rule(Rule::CapabilityInvalid),
            error => Error::Decode(error),
        })
    }
}
fn capabilities<R: PrivateRead, D: Decode>(
    read: &R,
    decode: &D,
    work: &mut Collections,
) -> Result<Vec<u64>, Error> {
    let mut object = None;
    let count = read.all(&read.root(), "capabilities", |value| {
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
        return Err(rule(Rule::CapabilitiesAmbiguous));
    }
    let mut expiries = Vec::new();
    if count == 1 {
        let object = object.ok_or_else(|| rule(Rule::CapabilitiesInvalid))?;
        read.visit_keys(&object, |key| {
            // Count and scalar shape precede JSON/persistence, even when the
            // first conflicting value happens to contain malformed bytes.
            let mut bytes_shape = false;
            let count = read.all(&object, key, |value| {
                bytes_shape = matches!(value, RoomValue::Bytes(_));
                Ok(())
            })?;
            if count != 1 {
                return Err(rule(Rule::CapabilityAmbiguous));
            }
            if !bytes_shape {
                return Err(rule(Rule::CapabilityInvalid));
            }
            let mut seen = false;
            read.all(&object, key, |value| {
                let RoomValue::Bytes(bytes) = value else {
                    return Err(rule(Rule::CapabilityInvalid));
                };
                seen = true;
                let held = decode.capability(bytes)?;
                let cap = held.cap();
                if !cap.persists_across_restart() {
                    return Err(rule(Rule::Nonpersistent));
                }
                if let Expiry::AtUnix(at) = cap.expiry {
                    work.push(&mut expiries, at).map_err(resource)?;
                }
                Ok(())
            })?;
            if !seen {
                return Err(rule(Rule::CapabilityMissing));
            }
            Ok(())
        })
        .map_err(read_error)?;
    }
    expiries.sort_unstable();
    expiries.dedup();
    Ok(expiries)
}
fn canonical_owner<R: PrivateRead, C: CopyValue>(
    read: &R,
    room: &str,
    authority: &DeviceCert,
    copies: &C,
) -> Result<C::Text, Error> {
    let mut expected = [0; 64];
    hex::encode_to_slice(authority.user_public_key, &mut expected).expect("fixed hex root");
    let mut is_string = false;
    let mut matches_root = false;
    let owners = read.all(&read.root(), "owner", |value| {
        if let RoomValue::Str(value) = value {
            is_string = true;
            matches_root = value.as_bytes() == expected;
        }
        Ok(())
    })?;
    let mut matches_room = false;
    let rooms = read.all(&read.root(), "room", |value| {
        matches_room = matches!(value,RoomValue::Str(value) if value==room);
        Ok(())
    })?;
    if owners != 1 || rooms != 1 || !matches_room {
        return Err(rule(Rule::OwnerAmbiguous));
    }
    if !is_string {
        return Err(rule(Rule::OwnerInvalid));
    }
    if !matches_root {
        return Err(rule(Rule::OwnerRoot));
    }
    let mut owner = None;
    read.all(&read.root(), "owner", |value| {
        let RoomValue::Str(value) = value else {
            return Err(rule(Rule::OwnerInvalid));
        };
        owner = Some(copies.text(value).map_err(resource)?);
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
fn revision<'a, R, C, I, J>(
    read: &R,
    meta: Metadata<'a, I, J>,
    copies: &C,
    work: &mut Collections,
) -> Result<String, Error>
where
    R: PrivateRead,
    C: CopyValue,
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let owner = canonical_owner(read, meta.room, meta.authority, copies)?;
    let mut caps = BTreeMap::new();
    let mut peers = BTreeMap::new();
    let mut identities = BTreeMap::new();
    read::visit_governance_entries(read, |entry| {
        match entry {
            read::GovernanceEntry::Owner(_) => (),
            read::GovernanceEntry::Capability { key, bytes } => {
                work.tree_entry::<String, Vec<u8>>().map_err(resource)?;
                caps.insert(
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
    // Verified union targets are unique. This order is identical to the legacy
    // sorted owned Vec and needs no cloning of row backing or sort workspace.
    revocations.sort_unstable_by_key(|entry| entry.revoked_device_public_key);
    let mut hash = blake3::Hasher::new_derive_key("qOS agent governance revision v1");
    postcard::serialize_with_flavor(
        &(
            meta.room,
            meta.authority,
            owner.as_ref(),
            caps,
            peers,
            identities,
            (u8::from(count != 0), media),
            CertificateRows(meta.certificates),
            revocations,
        ),
        Hash(&mut hash),
    )
    .map_err(Error::Encoding)?;
    let mut text = [0; 64];
    hex::encode_to_slice(hash.finalize().as_bytes(), &mut text).expect("fixed digest hex");
    work.text(std::str::from_utf8(&text).expect("ASCII hex"))
        .map_err(resource)
}
pub(crate) fn legacy_owner(
    read: &LegacyRead<'_, AutoCommit>,
    room: &str,
    authority: &DeviceCert,
) -> Result<String, Error> {
    canonical_owner(read, room, authority, &LegacyCopy)
}
pub(crate) fn legacy_revision<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
) -> Result<String, Error>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    revision(read, meta, &LegacyCopy, &mut Collections::unqualified())
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
    let expiries = capabilities(read, &LegacyDecode, &mut work)?;
    let revision = revision(read, meta, &LegacyCopy, &mut work)?;
    Ok(LegacyBasis { revision, expiries })
}
pub(crate) fn native<'a, I, J>(
    read: &NativeReadSession<'_, '_>,
    meta: Metadata<'a, I, J>,
    turn: &NativeWorkTurn,
    limits: Limits,
) -> Result<NativeBasis, Error>
where
    I: ExactSizeIterator<Item = &'a DeviceCert> + Clone,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let mut work = Collections::native(turn, limits.collections).map_err(resource)?;
    let expiries = capabilities(
        read,
        &NativeDecode {
            turn,
            limits: limits.json,
        },
        &mut work,
    )?;
    let revision = revision(read, meta, &TurnCopy(turn), &mut work)?;
    Ok(NativeBasis {
        revision,
        expiries,
        _collections: work,
    })
}
#[cfg(test)]
mod tests;
