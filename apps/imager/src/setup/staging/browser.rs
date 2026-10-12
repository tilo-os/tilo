//! Ciphertext-only input to the existing staging transaction.
use crate::setup::browser_transfer::Expectation;
#[cfg(unix)]
use cap_fs_ext::OpenOptionsFollowExt;
#[cfg(unix)]
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::Path};

pub const SLOT_BYTES: u64 = 130 * 1024 * 1024;
pub const RECEIPT_BYTES: u64 = 4096;
pub const SELECTION_BYTES: u64 = 4096;
pub const PACKAGE_PATH: &str = "/.tilo-installer/browser.sealed";
pub const SELECTION_PATH: &str = "/.tilo-installer/browser-selection.json";
pub const RECEIPT_PATH: &str = "/.tilo-installer/browser-receipt.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserArtifact {
    pub source_path: String,
    pub expectation: Expectation,
}
impl BrowserArtifact {
    pub fn valid(&self) -> bool {
        Path::new(&self.source_path).is_absolute()
            && self.expectation.validate().is_ok()
            && Path::new(&self.source_path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(self.expectation.filename.as_str())
            && self.expectation.sealed_bytes <= SLOT_BYTES
    }
    /// Read from a pinned private caller-owned parent without following links.
    /// The exact receipt is checked before the elevated transaction mutates.
    #[cfg(unix)]
    pub(in crate::setup) fn read(&self, caller: u32) -> io::Result<Vec<u8>> {
        use std::{io::Read, os::unix::fs::MetadataExt};
        if !self.valid() {
            return Err(io::Error::other("Invalid bounded browser data descriptor"));
        }
        let path = Path::new(&self.source_path);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Browser data has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("Browser data has no filename"))?;
        let pin = crate::setup::migration::PinnedDirectory::open_absolute(parent)?;
        let facts = pin.file.metadata()?;
        if facts.uid() != caller || facts.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "Browser data source parent is not caller-private",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
        let before = file.metadata()?;
        if !before.is_file()
            || before.uid() != caller
            || before.mode() & 0o077 != 0
            || before.nlink() != 1
            || before.len() != self.expectation.sealed_bytes
        {
            return Err(io::Error::other(
                "Browser data is not the exact private caller artifact",
            ));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(SLOT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) {
            return Err(io::Error::other("Browser data changed while being read"));
        }
        let current = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
        let current = current.metadata()?;
        if (
            current.dev(),
            current.ino(),
            current.len(),
            current.mtime(),
            current.mtime_nsec(),
            current.ctime(),
            current.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) {
            return Err(io::Error::other("Browser source publication changed"));
        }
        pin.validate()?;
        verify(&bytes, &self.expectation)?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    pub(in crate::setup) fn read(&self, _caller: u32) -> io::Result<Vec<u8>> {
        Err(io::Error::other(
            "Browser direct staging requires supported private caller custody",
        ))
    }
}
pub(super) fn verify(bytes: &[u8], expected: &Expectation) -> io::Result<()> {
    if expected.validate().is_err()
        || expected.sealed_bytes > SLOT_BYTES
        || u64::try_from(bytes.len()).ok() != Some(expected.sealed_bytes)
        || !bytes.starts_with(b"TILOMIG1")
        || <[u8; 32]>::from(Sha256::digest(bytes)) != expected.ciphertext_sha256
    {
        return Err(io::Error::other(
            "Browser ciphertext differs from its exact reviewed receipt",
        ));
    }
    Ok(())
}
