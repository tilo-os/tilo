//! Trusted Windows installer intake and bounded format validation.
//!
//! The manager selects a file through the Tilo file portal and transfers the
//! already-open descriptor. No client path crosses the compatibility protocol.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::InstallerKind;

/// Large enough for current game installers while still putting a hard upper
/// bound on storage and hashing work performed from one UI request.
pub const MAX_INSTALLER_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_PE_HEADER_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallerInspection {
    pub source_name: String,
    pub display_name: String,
    pub sha256: String,
    pub size: u64,
    pub kind: InstallerKind,
    pub machine: Option<PeMachine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeMachine {
    X86,
    X86_64,
    Arm64,
}

/// Inspect the exact portal-selected bytes. Extension is only a UX hint: MSI
/// and PE signatures decide what the service will execute.
pub fn inspect_installer(
    source: &mut File,
    source_name: &str,
) -> Result<InstallerInspection, InstallerError> {
    validate_source_name(source_name)?;
    let metadata = source.metadata()?;
    if !metadata.is_file() {
        return Err(InstallerError::NotRegular);
    }
    if metadata.len() == 0 {
        return Err(InstallerError::Empty);
    }
    if metadata.len() > MAX_INSTALLER_BYTES {
        return Err(InstallerError::TooLarge(metadata.len()));
    }

    source.seek(SeekFrom::Start(0))?;
    let mut header = Vec::with_capacity(MAX_PE_HEADER_BYTES);
    Read::by_ref(source)
        .take(MAX_PE_HEADER_BYTES as u64)
        .read_to_end(&mut header)?;
    let (kind, machine) = if header.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        (InstallerKind::Msi, None)
    } else {
        (InstallerKind::Exe, Some(validate_pe(&header)?))
    };

    let extension = source_name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or_default();
    let extension_matches = match kind {
        InstallerKind::Exe => extension.eq_ignore_ascii_case("exe"),
        InstallerKind::Msi => extension.eq_ignore_ascii_case("msi"),
    };
    if !extension_matches {
        return Err(InstallerError::ExtensionMismatch);
    }

    source.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 128 * 1024].into_boxed_slice();
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| InstallerError::TooLarge(u64::MAX))?)
            .ok_or(InstallerError::TooLarge(u64::MAX))?;
        if total > MAX_INSTALLER_BYTES {
            return Err(InstallerError::TooLarge(total));
        }
        let chunk = buffer
            .get(..read)
            .ok_or(InstallerError::InvalidReadLength)?;
        digest.update(chunk);
    }
    if total == 0 {
        return Err(InstallerError::Empty);
    }
    Ok(InstallerInspection {
        source_name: source_name.to_string(),
        display_name: display_name(source_name),
        sha256: hex::encode(digest.finalize()),
        size: total,
        kind,
        machine,
    })
}

fn validate_pe(header: &[u8]) -> Result<PeMachine, InstallerError> {
    if header.len() < 64 || !header.starts_with(b"MZ") {
        return Err(InstallerError::UnsupportedFormat);
    }
    let offset_bytes: [u8; 4] = header
        .get(0x3c..0x40)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(InstallerError::InvalidPe)?;
    let offset =
        usize::try_from(u32::from_le_bytes(offset_bytes)).map_err(|_| InstallerError::InvalidPe)?;
    let signature_end = offset.checked_add(6).ok_or(InstallerError::InvalidPe)?;
    let signature = header
        .get(offset..signature_end)
        .ok_or(InstallerError::PeHeaderTooLarge)?;
    if signature.get(..4) != Some(b"PE\0\0") {
        return Err(InstallerError::InvalidPe);
    }
    let machine = u16::from_le_bytes(
        signature
            .get(4..6)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(InstallerError::InvalidPe)?,
    );
    match machine {
        0x014c => Ok(PeMachine::X86),
        0x8664 => Ok(PeMachine::X86_64),
        0xaa64 => Ok(PeMachine::Arm64),
        _ => Err(InstallerError::UnsupportedMachine(machine)),
    }
}

fn validate_source_name(name: &str) -> Result<(), InstallerError> {
    if name.is_empty()
        || name.len() > 255
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
        || matches!(name, "." | "..")
    {
        return Err(InstallerError::InvalidName);
    }
    Ok(())
}

fn display_name(source_name: &str) -> String {
    let stem = source_name
        .rsplit_once('.')
        .map_or(source_name, |(stem, _)| stem)
        .replace(['_', '-'], " ");
    let display = stem.split_whitespace().collect::<Vec<_>>().join(" ");
    if display.is_empty() {
        "Windows application".to_string()
    } else {
        display
    }
}

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("installer name is invalid")]
    InvalidName,
    #[error("selected installer is not a regular file")]
    NotRegular,
    #[error("selected installer is empty")]
    Empty,
    #[error("installer is {0} bytes, over the 16 GiB import limit")]
    TooLarge(u64),
    #[error("selected file is not a PE executable or MSI package")]
    UnsupportedFormat,
    #[error("PE header is malformed")]
    InvalidPe,
    #[error("PE header lies outside the bounded inspection window")]
    PeHeaderTooLarge,
    #[error("PE machine 0x{0:04x} is unsupported")]
    UnsupportedMachine(u16),
    #[error("file extension does not match its validated Windows format")]
    ExtensionMismatch,
    #[error("installer reader returned an invalid byte count")]
    InvalidReadLength,
    #[error("reading selected installer failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    fn pe(machine: u16) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let mut bytes = vec![0_u8; 0x200];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn installers_are_validated_hashed_and_honestly_typed() {
        // Phase `validates_and_hashes_pe_from_descriptor`.
        {
            let file = pe(0x8664);
            let mut source = file.reopen().unwrap();
            let inspected = inspect_installer(&mut source, "Favorite_App.exe").unwrap();
            assert_eq!(inspected.kind, InstallerKind::Exe);
            assert_eq!(inspected.machine, Some(PeMachine::X86_64));
            assert_eq!(inspected.display_name, "Favorite App");
            assert_eq!(inspected.sha256.len(), 64);
        }
        // Phase `msi_signature_wins_but_extension_must_be_honest`.
        {
            let mut file = NamedTempFile::new().unwrap();
            file.write_all(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1, 0, 0])
                .unwrap();
            assert_eq!(
                inspect_installer(&mut file.reopen().unwrap(), "setup.msi")
                    .unwrap()
                    .kind,
                InstallerKind::Msi
            );
            assert!(matches!(
                inspect_installer(&mut file.reopen().unwrap(), "setup.exe"),
                Err(InstallerError::ExtensionMismatch)
            ));
        }
        // Phase `rejects_paths_streams_and_fake_executables`.
        {
            let mut fake = NamedTempFile::new().unwrap();
            fake.write_all(b"MZ but not a PE").unwrap();
            assert!(inspect_installer(&mut fake.reopen().unwrap(), "../bad.exe").is_err());
            assert!(matches!(
                inspect_installer(&mut fake.reopen().unwrap(), "bad.exe"),
                Err(InstallerError::UnsupportedFormat | InstallerError::InvalidPe)
            ));
        }
    }
}
