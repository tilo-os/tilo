//! Read Windows hints from the exact media block selected at boot.
//!
//! Boot extraction copies opaque ciphertext, never unlocks it. The Accounts
//! actor owns the expectation and a fresh operation/selection generation, and
//! invokes `unlock` only in response to explicit private password input.
use account_custody::hints::{
    AccountHint,
    media::{self, Expectation, Prepared},
};
use std::{
    fs::File,
    io::{self, Read, Seek},
    path::Path,
};

pub const BOOT_BLOCK: &str = "/run/tilo-installer/windows-media.block";

pub struct Source {
    prepared: Prepared,
    operation: String,
}
impl Source {
    /// Reader also used against a held real medium descriptor by native tests.
    /// # Errors
    /// Rejects invalid operations, unreadable media or a mismatched expectation.
    pub fn read(
        source: &mut (impl Read + Seek),
        size: u64,
        expected: &Expectation,
        operation: &str,
    ) -> io::Result<Self> {
        if operation.is_empty() || operation.len() > 2048 || operation.chars().any(char::is_control)
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            prepared: media::read(source, size, expected)?,
            operation: operation.into(),
        })
    }
    /// Loads only the block copied alongside the accepted boot profile. There
    /// is no removable-disk scan and no fallback to another attached export.
    /// # Errors
    /// Rejects missing, malformed or mismatched boot blocks and invalid operations.
    pub fn from_boot(expected: &Expectation, operation: &str) -> io::Result<Self> {
        Self::from_extracted(Path::new(BOOT_BLOCK), expected, operation)
    }
    fn from_extracted(path: &Path, expected: &Expectation, operation: &str) -> io::Result<Self> {
        let mut file = File::open(path)?;
        if file.metadata()?.len() != media::BLOCK_BYTES as u64 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Self::read(&mut file, media::BLOCK_BYTES as u64, expected, operation)
    }
    /// Return review-only suggestions, never tokens, consent or sign-in grants.
    /// The current private operation must match even for the same media digest.
    /// # Errors
    /// Rejects stale operations, wrong passwords and mismatched media expectations.
    pub fn unlock(
        &self,
        password: &[u8],
        expected: &Expectation,
        operation: &str,
    ) -> io::Result<Vec<AccountHint>> {
        if self.operation != operation {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.prepared.unlock(password, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boot_reader_requires_explicit_unlock_and_current_private_operation() {
        let prepared = Prepared::seal(
            b"private-password",
            "12345678-1234-1234-1234-123456789abc",
            &[AccountHint {
                provider: account_custody::hints::Provider::Microsoft,
                email: "private@example.org".into(),
                server: String::new(),
                sources: account_custody::hints::HintSources::default(),
            }],
            b"verified copies",
        )
        .unwrap();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut block = vec![0; media::BLOCK_BYTES];
        prepared.embed(&mut block).unwrap();
        std::io::Write::write_all(&mut file, &block).unwrap();
        let source = Source::from_extracted(file.path(), &prepared.expectation, "read-7").unwrap();
        assert!(
            source
                .unlock(b"private-password", &prepared.expectation, "read-8")
                .is_err()
        );
        assert!(
            source
                .unlock(b"wrong-password", &prepared.expectation, "read-7")
                .is_err()
        );
        assert_eq!(
            source
                .unlock(b"private-password", &prepared.expectation, "read-7")
                .unwrap()[0]
                .email,
            "private@example.org"
        );
        block[media::PRIVATE_OFFSET + 20] ^= 1;
        std::fs::write(file.path(), &block).unwrap();
        assert!(Source::from_extracted(file.path(), &prepared.expectation, "read-9").is_err());
    }
}
