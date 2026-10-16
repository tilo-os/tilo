//! Retained relay frames: a four-byte length prefix, a relay `Envelope`,
//! and inside an `Envelope::Delta` the co-edit payload a device published.
//!
//! Authentication happens at ingress; these decoders only refuse anything
//! that is not exactly one complete frame of the expected workspace.
use crate::StoreError;
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};
use tilo_relay_client::coedit::CoeditPayload;
use tilo_relay_protocol::transport::{self, Envelope};

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

/// Parse framing separately from proof: metadata routing may preserve a known
/// prefix with unknown trailing bytes, but that prefix never grants coverage.
#[must_use]
pub fn framed_envelope(bytes: &[u8]) -> Option<(Envelope, &[u8])> {
    let prefix: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    if usize::try_from(u32::from_le_bytes(prefix)).ok()? != bytes.len().checked_sub(4)? {
        return None;
    }
    postcard::take_from_bytes(bytes.get(4..)?).ok()
}

/// Validate EVERY enclosing representation before granting proof authority.
/// Authentication happens at ingress, not in this disk decoder.
#[must_use]
pub fn payload(bytes: &[u8], workspace: u64) -> Option<([u8; 32], CoeditPayload)> {
    let (
        Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        },
        rest,
    ) = framed_envelope(bytes)?
    else {
        return None;
    };
    if workspace_id != workspace || !rest.is_empty() {
        return None;
    }
    Some((author_device_public_key, complete_coedit_payload(&delta)?))
}

/// The co-edit body of a retained frame of `workspace`.
#[must_use]
pub fn body_of(bytes: &[u8], workspace: u64) -> Option<Vec<u8>> {
    match payload(bytes, workspace)?.1 {
        CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. } => {
            Some(body)
        }
    }
}

/// Used with an independently validated complete outer frame during replay.
#[must_use]
pub fn complete_coedit_payload(delta: &[u8]) -> Option<CoeditPayload> {
    let (payload, rest) = postcard::take_from_bytes(delta).ok()?;
    rest.is_empty().then_some(payload)
}

/// Only reads complete legal transport frames. Invalid data remains untouched.
/// Prefix and file length are checked before any payload-sized allocation.
///
/// # Errors
///
/// Returns an error when `path` cannot be read or changes while it is read.
pub fn frame_bytes(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if !(4..=(transport::MAX_FRAME_BYTES as u64).saturating_add(4)).contains(&len) {
        return Ok(None);
    }
    let mut prefix = [0; 4];
    file.read_exact(&mut prefix)?;
    if u64::from(u32::from_le_bytes(prefix)) != len.saturating_sub(4) {
        return Ok(None);
    }
    let mut bytes = vec![0; usize::try_from(len).map_err(|_| invalid("oversized frame"))?];
    // The range check above guarantees at least the four prefix bytes.
    let (head, body) = bytes.split_at_mut(4);
    head.copy_from_slice(&prefix);
    file.read_exact(body)?;
    let mut extra = [0];
    if file.read(&mut extra)? != 0 {
        return Err(invalid("frame changed during read"));
    }
    Ok(Some(bytes))
}

/// Whether the files at `a` and `b` hold the same bytes.
///
/// # Errors
///
/// Returns an error when either file cannot be read.
pub fn equal_files(a: &Path, b: &Path) -> Result<bool, StoreError> {
    let mut a = File::open(a)?;
    let mut b = File::open(b)?;
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let mut left = [0; 16 * 1024];
    let mut right = [0; 16 * 1024];
    loop {
        let n = a.read(&mut left)?;
        if n == 0 {
            return Ok(true);
        }
        let (Some(read), Some(expected)) = (left.get(..n), right.get_mut(..n)) else {
            return Ok(false);
        };
        b.read_exact(expected)?;
        if read != expected {
            return Ok(false);
        }
    }
}

/// Identity of the authenticated retained input being classified. Transport time
/// is irrelevant, but another source/route/kind cannot inherit its disposition.
#[must_use]
pub fn input_identity(
    source: [u8; 32],
    channel: [u8; 32],
    checkpoint: bool,
    body: &[u8],
) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(&source);
    hash.update(&channel);
    hash.update(&[u8::from(checkpoint)]);
    hash.update(body);
    *hash.finalize().as_bytes()
}

/// The [`input_identity`] of the complete retained frame `bytes` of
/// `workspace`, or `None` when it is not one.
#[must_use]
pub fn incoming_identity(bytes: &[u8], workspace: u64) -> Option<[u8; 32]> {
    let (source, payload) = payload(bytes, workspace)?;
    Some(match payload {
        CoeditPayload::Checkpoint { channel, body } => input_identity(source, channel, true, &body),
        CoeditPayload::RetainedDelta { channel, body } => {
            input_identity(source, channel, false, &body)
        }
    })
}
