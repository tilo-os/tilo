//! Fixed-layout opaque envelope. `required_bytes` is the exact encoded size of a payload.
use super::{SenderScope, SenderStorageCode as Code};
pub(super) const MAX_PAYLOAD: usize = 4 * 1024 * 1024;
pub(super) const MAX_FILE: usize = MAX_PAYLOAD + 4096;
pub(super) const HEADER: usize = 399;
const DIGEST: usize = 32;
const MAGIC: &[u8; 8] = b"QSPA0001";
const DOMAIN: &str = "tilo private sender file envelope v1";
pub(super) struct Decoded<'a> {
    pub instance: [u8; 16],
    pub sequence: u64,
    pub revision: u64,
    pub payload: &'a [u8],
}
pub(super) fn required_bytes(_scope: &SenderScope, payload: usize) -> Result<usize, Code> {
    if payload > MAX_PAYLOAD {
        return Err(Code::Capacity);
    }
    payload.checked_add(HEADER + DIGEST).ok_or(Code::Capacity)
}
pub(super) fn encode(
    scope: &SenderScope,
    instance: [u8; 16],
    sequence: u64,
    revision: u64,
    payload: &[u8],
) -> Result<Vec<u8>, Code> {
    if instance == [0; 16] || sequence == 0 {
        return Err(Code::InvalidFile);
    }
    let size = required_bytes(scope, payload.len())?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| Code::Capacity)?;
    bytes.extend_from_slice(MAGIC);
    bytes.push(scope.profile);
    bytes.extend_from_slice(&scope.room);
    bytes.extend_from_slice(&scope.root);
    bytes.extend_from_slice(&scope.device);
    bytes.extend_from_slice(&scope.machine_len.to_le_bytes());
    bytes.extend_from_slice(&scope.machine);
    bytes.extend_from_slice(&scope.incarnation);
    bytes.extend_from_slice(&instance);
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&revision.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    let digest = blake3::derive_key(DOMAIN, &bytes);
    bytes.extend_from_slice(&digest);
    Ok(bytes)
}
pub(super) fn decode<'a>(scope: &SenderScope, bytes: &'a [u8]) -> Result<Decoded<'a>, Code> {
    if bytes.len() < HEADER + DIGEST || bytes.len() > MAX_FILE {
        return Err(Code::InvalidFile);
    }
    if &bytes[..8] != MAGIC
        || bytes[8] != scope.profile
        || bytes[9..25] != scope.room
        || bytes[25..57] != scope.root
        || bytes[57..89] != scope.device
        || bytes[89..91] != scope.machine_len.to_le_bytes()
        || bytes[91..347] != scope.machine
        || bytes[347..363] != scope.incarnation
    {
        return Err(Code::InvalidFile);
    }
    let instance = bytes[363..379].try_into().map_err(|_| Code::InvalidFile)?;
    let sequence = u64::from_le_bytes(bytes[379..387].try_into().map_err(|_| Code::InvalidFile)?);
    let revision = u64::from_le_bytes(bytes[387..395].try_into().map_err(|_| Code::InvalidFile)?);
    let length =
        u32::from_le_bytes(bytes[395..399].try_into().map_err(|_| Code::InvalidFile)?) as usize;
    if instance == [0; 16]
        || sequence == 0
        || required_bytes(scope, length).map_err(|_| Code::InvalidFile)? != bytes.len()
    {
        return Err(Code::InvalidFile);
    }
    let end = bytes.len() - DIGEST;
    if bytes[end..] != blake3::derive_key(DOMAIN, &bytes[..end]) {
        return Err(Code::InvalidFile);
    }
    Ok(Decoded {
        instance,
        sequence,
        revision,
        payload: &bytes[HEADER..end],
    })
}
