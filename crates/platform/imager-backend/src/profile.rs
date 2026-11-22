//! The installation-media profile's Windows preparation: private account
//! hints sealed beside the codec shared with the standalone media tool.
//!
//! Everything the codec itself does lives in `install_profile_model::profile`;
//! this module adds the Windows suggestion an image written from a Windows
//! machine carries, which needs the account-credentials readers.

pub use install_profile_model::profile::{
    PROFILE_BLOCK_SIZE, PROFILE_MAGIC, encode_profile_block, encode_profile_block_bytes,
    encode_profile_payload, write_encoded_profile_block, write_profile_block,
    write_profile_block_bound, write_profile_block_to_file,
};

use crate::imager_model::InstallProfile;
use std::io;
use std::path::Path;

/// Encode the on-media profile block, with `windows`' private hints in it.
///
/// # Errors
/// The profile is not a valid media profile, or the hints cannot be sealed
/// into its block.
pub fn encode_profile_block_with_windows(
    profile: &InstallProfile,
    windows: Option<&account_credentials::hints::media::Prepared>,
) -> io::Result<Vec<u8>> {
    let mut payload = encode_profile_payload(profile)?;
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
    let mut block = encode_profile_block_bytes(&payload)?;
    if let Some(prepared) = windows {
        prepared.embed(&mut block)?;
    }
    Ok(block)
}

/// [`write_profile_block_bound`], with `windows`' private hints in the block.
///
/// # Errors
/// The hints cannot be sealed, or the target cannot be written or read back.
pub fn write_windows_profile_block_bound(
    dst: &Path,
    target_size: u64,
    profile: &InstallProfile,
    windows: Option<&account_credentials::hints::media::Prepared>,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    let block = encode_profile_block_with_windows(profile, windows)?;
    let mut device = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dst)?;
    write_encoded_profile_block(&mut device, target_size, &block, should_cancel)
}

#[cfg(test)]
mod windows_tests {
    use super::*;
    use account_credentials::hints::{AccountHint, HintSources, Provider, media};
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

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
