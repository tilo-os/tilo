//! Exact delivery projection, distinct from strict private policy semantics.
use super::read::LegacyRead;
use crate::resources::{PeerResourceError, json::JsonLimits};
use automerge::AutoCommit;
use tilo_user_identity::{DeviceCert, RevocationEntry};
pub(crate) const POLICY_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug)]
pub(crate) enum Error {
    Decode(PeerResourceError),
    Unavailable,
    CurrentDenied,
    UnsupportedPolicy,
    Capacity,
}
pub(crate) struct Metadata<'a, I, J> {
    pub(crate) room: &'a str,
    pub(crate) workspace: u64,
    pub(crate) authority: &'a DeviceCert,
    pub(crate) host: &'a DeviceCert,
    pub(crate) device: &'a DeviceCert,
    pub(crate) certificates: I,
    pub(crate) revocations: J,
}
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) collections: usize,
    pub(crate) json: JsonLimits,
}

use super::read::{ReadError, RoomRead, RoomValue, Selection, collections::Collections};
use crate::resources::json;
use collab_types::caps::{CapOverride, Principal, Scope, Subject};
/// Whether a grant's subject can address this consumer's person (whose
/// document-read actor is `Person(hex(root))`). Role, room, app, agent and
/// token subjects are kept; person-addressed grants only for this person.
/// Allocation-free.
pub(super) fn consumer_subject(subject: &Subject, root: &[u8; 32]) -> bool {
    match subject {
        Subject::User(key) => key.0 == *root,
        Subject::Principal(Principal::Person(id)) => {
            let mut text = [0u8; 64];
            hex::encode_to_slice(root, &mut text).is_ok() && id.as_bytes() == text
        }
        _ => true,
    }
}
use serde::Serialize;
fn read_error(error: ReadError<Error>) -> Error {
    match error {
        ReadError::Malformed => Error::Unavailable,
        ReadError::Capacity => Error::Capacity,
        ReadError::Visitor(error) => error,
    }
}
fn add(total: &mut usize, bytes: usize) -> Result<(), Error> {
    *total = total
        .checked_add(bytes)
        .filter(|n| *n <= POLICY_BYTES)
        .ok_or(Error::Capacity)?;
    Ok(())
}
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
            .filter(|n| *n <= POLICY_BYTES)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.0)
    }
}
fn counted<T: Serialize>(value: &T) -> Result<usize, Error> {
    postcard::serialize_with_flavor(value, Count(0)).map_err(|_| Error::Capacity)
}
fn encode<T: Serialize>(value: &T, work: &mut Collections) -> Result<Vec<u8>, Error> {
    let length = counted(value)?;
    let mut output = work.zeroed(length).map_err(|_| Error::Capacity)?;
    let written = postcard::to_slice(value, &mut output)
        .map_err(|_| Error::Capacity)?
        .len();
    if written != length {
        return Err(Error::Capacity);
    }
    Ok(output)
}
struct Hash {
    value: blake3::Hasher,
    length: usize,
}
impl postcard::ser_flavors::Flavor for Hash {
    type Output = ([u8; 32], usize);
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.try_extend(&[byte])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.length = self
            .length
            .checked_add(bytes.len())
            .filter(|n| *n <= POLICY_BYTES)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        self.value.update(bytes);
        Ok(())
    }
    fn finalize(self) -> postcard::Result<Self::Output> {
        Ok((*self.value.finalize().as_bytes(), self.length))
    }
}
trait Decode {
    fn cap(&self, bytes: &[u8]) -> Result<CapOverride, Error>;
}
struct LegacyDecode;
impl Decode for LegacyDecode {
    fn cap(&self, bytes: &[u8]) -> Result<CapOverride, Error> {
        serde_json::from_slice(bytes).map_err(|_| Error::Unavailable)
    }
}
struct NativeDecode {
    limits: JsonLimits,
}
impl Decode for NativeDecode {
    fn cap(&self, bytes: &[u8]) -> Result<CapOverride, Error> {
        json::decode(bytes, self.limits).map_err(|e| match e {
            PeerResourceError::Malformed => Error::Unavailable,
            PeerResourceError::Capacity => Error::Decode(PeerResourceError::Capacity),
        })
    }
}
fn rows<'a, T: Serialize + 'a>(
    rows: impl Iterator<Item = &'a T>,
    total: &mut usize,
    work: &mut Collections,
) -> Result<Vec<Vec<u8>>, Error> {
    // Preserve count-all-before-encode-all ordering.
    let mut references = Vec::new();
    for row in rows {
        add(total, counted(row)?)?;
        work.push(&mut references, row)
            .map_err(|_| Error::Capacity)?;
    }
    let mut output = Vec::new();
    for row in references {
        let bytes = encode(row, work)?;
        work.push(&mut output, bytes).map_err(|_| Error::Capacity)?;
    }
    output.sort_unstable();
    Ok(output)
}
#[expect(
    clippy::too_many_lines,
    reason = "one canonical digest projection, in wire order"
)]
fn project<'a, R: RoomRead, D: Decode, I, J>(
    read: &R,
    meta: Metadata<'a, I, J>,
    decode: &D,
    work: &mut Collections,
    check_routes: impl FnOnce() -> Result<(), Error>,
) -> Result<[u8; 32], Error>
where
    I: Iterator<Item = &'a DeviceCert>,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let mut total = 0;
    let mut governor = None;
    // The consumer's authorization view: its own person, device, host and
    // governor. Other members' certificates, grants and device revocations
    // cannot change what this device may read, so admitting or removing
    // another device no longer resets every other consumer's session.
    let person = meta.device.user_public_key;
    let roots = [
        meta.device.user_public_key,
        meta.authority.user_public_key,
        meta.host.user_public_key,
    ];
    let devices = [
        meta.device.device_public_key,
        meta.authority.device_public_key,
        meta.host.device_public_key,
    ];
    read.visit_property(&read.root(), "owner", Selection::Winner, |value| {
        let RoomValue::Str(text) = value else {
            return Err(Error::Unavailable);
        };
        add(&mut total, text.len())?;
        governor = Some(work.text(text).map_err(|_| Error::Capacity)?);
        Ok(())
    })
    .map_err(read_error)?;
    let governor = governor.ok_or(Error::Unavailable)?;
    let mut caps = Vec::new();
    let mut map = None;
    read.visit_property(&read.root(), "capabilities", Selection::Winner, |value| {
        let RoomValue::Object {
            kind: automerge::ObjType::Map,
            object,
        } = value
        else {
            return Err(Error::Unavailable);
        };
        map = Some(object);
        Ok(())
    })
    .map_err(read_error)?;
    if let Some(map) = map {
        read.visit_keys(&map, |key| {
            add(&mut total, key.len())?;
            let count = read
                .visit_property(&map, key, Selection::Winner, |value| {
                    let RoomValue::Bytes(bytes) = value else {
                        return Err(Error::Unavailable);
                    };
                    if bytes.len() > POLICY_BYTES {
                        return Err(Error::Capacity);
                    }
                    let cap = &decode.cap(bytes)?;
                    match &cap.scope {
                        Scope::Workspace => (),
                        Scope::Room(room) if room == meta.room => (),
                        // Enforced only where files are opened, or where an
                        // action is invoked; neither decides a document read.
                        Scope::FsPath(_) | Scope::Action { .. } => return Ok(()),
                        _ => return Err(Error::UnsupportedPolicy),
                    }
                    // A grant addressed to another person can never match this
                    // consumer's document-read actor; it is not part of its view.
                    if !consumer_subject(&cap.subject, &person) {
                        return Ok(());
                    }
                    add(&mut total, counted(cap)?)?;
                    let encoded = encode(cap, work)?;
                    let key = work.text(key).map_err(|_| Error::Capacity)?;
                    work.push(&mut caps, (key, encoded))
                        .map_err(|_| Error::Capacity)
                })
                .map_err(read_error)?;
            if count != 1 {
                return Err(Error::Unavailable);
            }
            Ok(())
        })
        .map_err(read_error)?;
    }
    caps.sort_unstable();
    let certificates = rows(
        meta.certificates
            .filter(|cert| roots.contains(&cert.user_public_key)),
        &mut total,
        work,
    )?;
    let revocations = rows(
        meta.revocations
            .filter(|entry| devices.contains(&entry.revoked_device_public_key)),
        &mut total,
        work,
    )?;
    // Authorization follows all metadata work but precedes final tuple quota.
    check_routes()?;
    let tuple = (
        2_u8,
        meta.room,
        meta.workspace,
        meta.authority,
        meta.host,
        meta.device,
        governor,
        certificates,
        revocations,
        caps,
    );
    let expected = counted(&tuple)?;
    let mut value = blake3::Hasher::new();
    value.update(b"tilo-delivery-v3-room-policy-v2\0");
    let (digest, length) = postcard::serialize_with_flavor(&tuple, Hash { value, length: 0 })
        .map_err(|_| Error::Capacity)?;
    if length != expected {
        return Err(Error::Capacity);
    }
    Ok(digest)
}
pub(crate) fn legacy<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
    check_routes: impl FnOnce() -> Result<(), Error>,
) -> Result<[u8; 32], Error>
where
    I: Iterator<Item = &'a DeviceCert>,
    J: Iterator<Item = &'a RevocationEntry>,
{
    project(
        read,
        meta,
        &LegacyDecode,
        &mut Collections::unqualified(),
        check_routes,
    )
}
pub(crate) fn native<'a, I, J>(
    read: &LegacyRead<'_, AutoCommit>,
    meta: Metadata<'a, I, J>,
    limits: Limits,
    check_routes: impl FnOnce() -> Result<(), Error>,
) -> Result<[u8; 32], Error>
where
    I: Iterator<Item = &'a DeviceCert>,
    J: Iterator<Item = &'a RevocationEntry>,
{
    let mut work = Collections::limited(limits.collections);
    project(
        read,
        meta,
        &NativeDecode {
            limits: limits.json,
        },
        &mut work,
        check_routes,
    )
}
#[cfg(test)]
mod tests;
