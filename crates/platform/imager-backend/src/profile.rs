//! Secret-free installation media profile codec and verified I/O.
//! Shared verbatim by the graphical imager and the standalone headless tool.
use crate::imager_model::InstallProfile;
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
    encode_profile_block_with_windows(profile, None)
}

pub fn encode_profile_block_with_windows(
    profile: &InstallProfile,
    windows: Option<&account_custody::hints::media::Prepared>,
) -> io::Result<Vec<u8>> {
    profile
        .validate_media_profile()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut payload = profile
        .to_media_profile_json()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Some(prepared) = windows {
        prepared.verify()?;
        if profile.prepared_setup.is_some() || profile.enroll_local_identity {
            return Err(io::Error::other(
                "Windows suggestions cannot claim a prepared Linux identity",
            ));
        }
        let mut json: serde_json::Value = serde_json::from_slice(&payload)?;
        json["windows_preparation"] = serde_json::to_value(&prepared.expectation)?;
        payload = serde_json::to_vec(&json)?;
    }
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
    if let Some(prepared) = windows {
        prepared.embed(&mut block)?;
    }
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

pub fn write_profile_block_bound(
    dst: &Path,
    target_size: u64,
    profile: &InstallProfile,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    if should_cancel() {
        return Err(cancelled_error());
    }
    write_windows_profile_block_bound(dst, target_size, profile, None, should_cancel)
}

pub fn write_windows_profile_block_bound(
    dst: &Path,
    target_size: u64,
    profile: &InstallProfile,
    windows: Option<&account_custody::hints::media::Prepared>,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    let block = encode_profile_block_with_windows(profile, windows)?;
    let mut device = OpenOptions::new().read(true).write(true).open(dst)?;
    write_encoded_profile_block(&mut device, target_size, &block, should_cancel)
}

/// Encode, write, sync, and verify using the caller's already-open file.
/// The caller retains file identity and any regular-file/device admission.
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

#[cfg(test)]
mod windows_tests {
    use super::*;
    use account_custody::hints::{AccountHint, HintSources, Provider, media};

    #[test]
    fn production_writer_and_reader_bind_exact_private_hints_and_recovery() {
        let prepared = media::Prepared::seal(
            b"private-password",
            "12345678-1234-1234-1234-123456789abc",
            &[AccountHint {
                provider: Provider::Microsoft,
                email: "private@example.org".into(),
                server: String::new(),
                sources: HintSources::default(),
            }],
            b"independently verified recovery copies",
        )
        .unwrap();
        let profile = InstallProfile::default();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let size = 3 * PROFILE_BLOCK_SIZE as u64;
        file.as_file_mut().set_len(size).unwrap();
        write_windows_profile_block_bound(file.path(), size, &profile, Some(&prepared), &|| false)
            .unwrap();
        let opened = media::read(file.as_file_mut(), size, &prepared.expectation).unwrap();
        assert_eq!(
            opened
                .unlock(b"private-password", &prepared.expectation)
                .unwrap()[0]
                .email,
            "private@example.org"
        );
        assert!(
            opened
                .unlock(b"wrong-password", &prepared.expectation)
                .is_err()
        );
        file.seek(SeekFrom::Start(size - PROFILE_BLOCK_SIZE as u64))
            .unwrap();
        let mut block = vec![0; PROFILE_BLOCK_SIZE];
        file.read_exact(&mut block).unwrap();
        let prefix = std::str::from_utf8(&block[..media::PRIVATE_OFFSET]).unwrap();
        let mut lines = prefix.splitn(4, '\n');
        assert_eq!(lines.next(), Some(PROFILE_MAGIC));
        let _digest = lines.next().unwrap();
        let length: usize = lines.next().unwrap().parse().unwrap();
        let payload = &lines.next().unwrap().as_bytes()[..length];
        let public: serde_json::Value = serde_json::from_slice(payload).unwrap();
        assert!(
            !std::str::from_utf8(payload)
                .unwrap()
                .contains("private@example.org")
        );
        assert!(public.get("prepared_setup").is_none());
        assert_eq!(
            public["windows_preparation"],
            serde_json::to_value(&prepared.expectation).unwrap()
        );
        let expected: media::Expectation =
            serde_json::from_value(public["windows_preparation"].clone()).unwrap();
        file.seek(SeekFrom::Start(
            size - (PROFILE_BLOCK_SIZE - media::PRIVATE_OFFSET) as u64 + 20,
        ))
        .unwrap();
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 1;
        file.seek(SeekFrom::Current(-1)).unwrap();
        file.write_all(&byte).unwrap();
        assert!(media::read(file.as_file_mut(), size, &expected).is_err());
        assert!(
            write_windows_profile_block_bound(
                file.path(),
                size,
                &profile,
                Some(&prepared),
                &|| true
            )
            .is_err()
        );
    }
}
