//! Ciphertext-only input to the existing staging transaction.
use crate::imager_model::PreparedSetupExpectation;
#[cfg(unix)]
use cap_fs_ext::OpenOptionsFollowExt;
#[cfg(unix)]
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, path::Path};

pub const SLOT_BYTES: u64 = 32 * 1024 * 1024 + 16384;
pub const RECEIPT_BYTES: u64 = 1024;
pub const PACKAGE_PATH: &str = "/.tilo-installer/prepared-setup.sealed";
pub const RECEIPT_PATH: &str = "/.tilo-installer/receipt.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedArtifact<E> {
    pub source_path: String,
    pub expectation: PreparedSetupExpectation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<super::browser::BrowserArtifact<E>>,
}
impl<E: super::browser::Expectation> PreparedArtifact<E> {
    pub fn valid(&self) -> bool {
        Path::new(&self.source_path).is_absolute()
            && self.expectation.valid()
            && self.expectation.sealed_bytes <= SLOT_BYTES
            && self.browser_valid()
    }
    pub fn browser_expectation(&self) -> Option<&E> {
        self.browser.as_ref().map(|browser| &browser.expectation)
    }
    fn browser_valid(&self) -> bool {
        self.browser
            .as_ref()
            .is_none_or(super::browser::BrowserArtifact::valid)
    }
    /// Read from a pinned private caller-owned parent without following links.
    /// The exact receipt is checked before the elevated transaction mutates.
    #[cfg(unix)]
    pub fn read(&self, caller: u32) -> io::Result<Vec<u8>> {
        use std::{io::Read, os::unix::fs::MetadataExt};
        if !self.valid() {
            return Err(io::Error::other(
                "Invalid bounded prepared setup descriptor",
            ));
        }
        let path = Path::new(&self.source_path);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Prepared setup has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("Prepared setup has no filename"))?;
        let pin = crate::setup::migration::PinnedDirectory::open_absolute(parent)?;
        let facts = pin.file.metadata()?;
        if facts.uid() != caller || facts.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "Prepared setup source parent is not caller-private",
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
                "Prepared setup is not the exact private caller artifact",
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
            return Err(io::Error::other("Prepared setup changed while being read"));
        }
        pin.validate()?;
        verify(&bytes, &self.expectation)?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    pub fn read(&self, _caller: u32) -> io::Result<Vec<u8>> {
        Err(io::Error::other(
            "Prepared direct staging requires supported private caller custody",
        ))
    }
}
pub fn verify(bytes: &[u8], expected: &PreparedSetupExpectation) -> io::Result<()> {
    if !expected.valid()
        || expected.sealed_bytes > SLOT_BYTES
        || u64::try_from(bytes.len()).ok() != Some(expected.sealed_bytes)
        || !bytes.starts_with(b"TILOMIG1")
        || hex::encode(Sha256::digest(bytes)) != expected.ciphertext_sha256
    {
        return Err(io::Error::other(
            "Prepared ciphertext differs from its exact reviewed receipt",
        ));
    }
    Ok(())
}
pub fn caller_uid() -> io::Result<u32> {
    if let Some(value) = ["PKEXEC_UID", "SUDO_UID"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
    {
        return value
            .parse()
            .map_err(|_| io::Error::other("Invalid authenticated caller UID"));
    }
    #[cfg(unix)]
    // SAFETY: reads this helper's credentials, never changes ownership.
    let owner = rustix::process::geteuid().as_raw();
    #[cfg(not(unix))]
    let owner = 0;
    Ok(owner)
}
