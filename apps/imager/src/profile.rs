//! Secret-free installation media profile codec and verified I/O.
//! Shared verbatim by the graphical imager and the standalone headless tool.
use crate::model::InstallProfile;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Fixed block at the end of a flashed device carrying a secret-free profile.
/// Keeping it outside the image partitions makes the handoff identical on
/// Linux, macOS and Windows and lets the live medium read it before touching
/// any target disk.
pub const PROFILE_BLOCK_SIZE: usize = 64 * 1024;
pub const PROFILE_MAGIC: &str = "TILO_INSTALL_PROFILE_V1";

/// Encode the on-media profile block. The compact JSON contains no secrets;
/// SHA-256 detects incomplete writes and accidental corruption. It is not an
/// authenticity claim: the installer treats every field as untrusted input,
/// validates it, and displays the result again on Review.
///
/// # Errors
///
/// Returns an error if the profile cannot be serialized or does not fit in the
/// reserved block.
pub fn encode_profile_block(profile: &InstallProfile) -> io::Result<Vec<u8>> {
    profile
        .validate_media_profile()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let payload = profile
        .to_media_profile_json()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let digest = hex::encode(Sha256::digest(&payload));
    let header = format!("{PROFILE_MAGIC}\n{digest}\n{}\n", payload.len());
    if header.len() + payload.len() > PROFILE_BLOCK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "installation profile exceeds reserved media block",
        ));
    }
    let mut block = vec![0u8; PROFILE_BLOCK_SIZE];
    let payload_at = header.len();
    block
        .get_mut(..payload_at)
        .ok_or_else(|| io::Error::other("installation profile header is out of bounds"))?
        .copy_from_slice(header.as_bytes());
    block
        .get_mut(payload_at..payload_at + payload.len())
        .ok_or_else(|| io::Error::other("installation profile payload is out of bounds"))?
        .copy_from_slice(&payload);
    Ok(block)
}

/// Write and read back the complete profile block at the device tail.
///
/// # Errors
///
/// Returns an error if the target is too small, cannot be opened or written,
/// or if the complete read-back differs from the encoded profile block.
pub fn write_profile_block(
    dst: &Path,
    target_size: u64,
    profile: &InstallProfile,
) -> io::Result<()> {
    write_profile_block_bound(dst, target_size, profile, &|| false)
}

pub(crate) fn write_profile_block_bound(
    dst: &Path,
    target_size: u64,
    profile: &InstallProfile,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    if should_cancel() {
        return Err(cancelled_error());
    }
    let block = encode_profile_block(profile)?;
    let mut device = OpenOptions::new().read(true).write(true).open(dst)?;
    write_encoded_profile_block(&mut device, target_size, &block, should_cancel)
}

/// Encode, write, sync, and verify using the caller's already-open file.
/// The caller retains file identity and any regular-file/device admission.
///
/// # Errors
/// Returns an error for an invalid profile, insufficient capacity, cancellation,
/// I/O failure, or read-back mismatch.
pub fn write_profile_block_to_file(
    device: &mut std::fs::File,
    target_size: u64,
    profile: &InstallProfile,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    if should_cancel() {
        return Err(cancelled_error());
    }
    let block = encode_profile_block(profile)?;
    write_encoded_profile_block(device, target_size, &block, should_cancel)
}

fn write_encoded_profile_block(
    device: &mut std::fs::File,
    target_size: u64,
    block: &[u8],
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    let offset = target_size
        .checked_sub(PROFILE_BLOCK_SIZE as u64)
        .ok_or_else(|| io::Error::other("target is too small for installation profile"))?;
    if should_cancel() {
        return Err(cancelled_error());
    }
    device.seek(SeekFrom::Start(offset))?;
    device.write_all(block)?;
    device.flush()?;
    device.sync_all()?;
    if should_cancel() {
        return Err(cancelled_error());
    }
    let mut actual = vec![0u8; PROFILE_BLOCK_SIZE];
    device.seek(SeekFrom::Start(offset))?;
    device.read_exact(&mut actual)?;
    if actual != block {
        return Err(io::Error::other(
            "installation profile read-back did not match",
        ));
    }
    if should_cancel() {
        return Err(cancelled_error());
    }
    Ok(())
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "image write cancelled")
}
