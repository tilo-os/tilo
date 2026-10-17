//! Ciphertext-only input to the existing staging transaction.
#[cfg(unix)]
use cap_fs_ext::OpenOptionsFollowExt;
#[cfg(unix)]
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::Path};

#[cfg(all(test, unix))]
#[path = "browser_tests.rs"]
mod tests;

pub const SLOT_BYTES: u64 = 130 * 1024 * 1024;
pub const RECEIPT_BYTES: u64 = 4096;
pub const SELECTION_BYTES: u64 = 4096;
pub const PACKAGE_PATH: &str = "/.tilo-installer/browser.sealed";
pub const SELECTION_PATH: &str = "/.tilo-installer/browser-selection.json";
pub const RECEIPT_PATH: &str = "/.tilo-installer/browser-receipt.json";

/// Read-only view of the caller's validated, bounded ciphertext receipt.
/// It carries no source directory or elevated I/O authority.
pub trait Expectation {
    fn validate(&self) -> io::Result<()>;
    fn filename(&self) -> &str;
    fn sealed_bytes(&self) -> u64;
    fn ciphertext_sha256(&self) -> [u8; 32];
}

impl<E: Expectation + ?Sized> Expectation for &E {
    fn validate(&self) -> io::Result<()> {
        E::validate(self)
    }
    fn filename(&self) -> &str {
        E::filename(self)
    }
    fn sealed_bytes(&self) -> u64 {
        E::sealed_bytes(self)
    }
    fn ciphertext_sha256(&self) -> [u8; 32] {
        E::ciphertext_sha256(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserArtifact<E> {
    pub source_path: String,
    pub expectation: E,
}
impl<E: Expectation> BrowserArtifact<E> {
    pub fn valid(&self) -> bool {
        Path::new(&self.source_path).is_absolute()
            && self.expectation.validate().is_ok()
            && Path::new(&self.source_path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some(self.expectation.filename())
            && self.expectation.sealed_bytes() <= SLOT_BYTES
    }
    /// Read from a pinned private caller-owned parent without following links.
    /// The exact receipt is checked before the elevated transaction mutates.
    #[cfg(unix)]
    pub fn read(&self, caller: u32) -> io::Result<Vec<u8>> {
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
        let pin = crate::browser_selection::pinned::PinnedDirectory::open_absolute(parent)?;
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
            || before.len() != self.expectation.sealed_bytes()
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
    pub fn read(&self, _caller: u32) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Browser direct staging requires supported private caller custody",
        ))
    }
}
pub fn verify(bytes: &[u8], expected: &impl Expectation) -> io::Result<()> {
    if expected.validate().is_err()
        || expected.sealed_bytes() > SLOT_BYTES
        || u64::try_from(bytes.len()).ok() != Some(expected.sealed_bytes())
        || !bytes.starts_with(b"TILOMIG1")
        || <[u8; 32]>::from(Sha256::digest(bytes)) != expected.ciphertext_sha256()
    {
        return Err(io::Error::other(
            "Browser ciphertext differs from its exact reviewed receipt",
        ));
    }
    Ok(())
}
