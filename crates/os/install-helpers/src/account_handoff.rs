//! Private, bounded transport for live-session accounts. Only the installed user
//! opens the sealed bundle; the privileged installer merely carries its bytes.
use account_credentials::portable::SealedAccounts;
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use tilo_seal::SymmetricKey;

const MAGIC: &[u8; 8] = b"TILOAH01";
const MAX_SEALED: usize = 8 * 1024 * 1024 + 256;

/// A sealed selection and one ephemeral transport key, never public UI state.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallAccounts {
    ciphertext: Vec<u8>,
    key: SymmetricKey,
}
impl std::fmt::Debug for InstallAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InstallAccounts([private])")
    }
}
impl InstallAccounts {
    pub fn new(sealed: &SealedAccounts, key: SymmetricKey) -> Self {
        Self {
            ciphertext: sealed.as_bytes().to_vec(),
            key,
        }
    }

    /// Write only to the installer's private stdin pipe, following password/PIN.
    pub fn write_to(&self, output: &mut impl Write) -> io::Result<()> {
        let length = u32::try_from(self.ciphertext.len()).map_err(io::Error::other)?;
        output.write_all(MAGIC)?;
        output.write_all(&length.to_be_bytes())?;
        output.write_all(self.key.expose_secret())?;
        output.write_all(&self.ciphertext)
    }

    /// Read exactly one bounded frame, refusing truncation and trailing bytes.
    pub fn read_from(input: &mut impl Read) -> io::Result<Self> {
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::other("invalid account transport"));
        }
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = usize::try_from(u32::from_be_bytes(length)).map_err(io::Error::other)?;
        if length == 0 || length > MAX_SEALED {
            return Err(io::Error::other("account transport exceeds bounds"));
        }
        let mut key = zeroize::Zeroizing::new([0; 32]);
        input.read_exact(key.as_mut())?;
        let mut ciphertext = vec![0; length];
        input.read_exact(&mut ciphertext)?;
        if input.read(&mut [0; 1])? != 0 {
            return Err(io::Error::other("trailing account transport bytes"));
        }
        let sealed = SealedAccounts::from_bytes(&ciphertext).map_err(io::Error::other)?;
        Ok(Self::new(
            &sealed,
            SymmetricKey::from_slice(key.as_ref()).map_err(io::Error::other)?,
        ))
    }

    /// The caller must already have dropped real and effective IDs and
    /// supplementary groups to the destination user; authentication precedes every
    /// vault write.
    pub fn import_for_user(
        &self,
        root: &std::path::Path,
        binding: &account_credentials::portable::PortableBinding,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if rustix::process::getuid().as_raw() != binding.target_uid()
            || rustix::process::geteuid().as_raw() != binding.target_uid()
            || binding.target_uid() == 0
        {
            return Err("wrong account consumer".into());
        }
        self.import_authenticated(root, binding)
    }

    fn import_authenticated(
        &self,
        root: &std::path::Path,
        binding: &account_credentials::portable::PortableBinding,
    ) -> Result<(), Box<dyn std::error::Error>> {
        validate_destination(root)?;
        let sealed = SealedAccounts::from_bytes(&self.ciphertext)?;
        account_credentials::portable::import_sealed_accounts(
            root,
            &sealed,
            &self.key,
            binding.as_bytes(),
        )?;
        Ok(())
    }
}

// Refuse pre-existing ownership aliases, including aliases left in a migrated
// home. Import must never follow a link back to a source account's credentials.
fn validate_destination(root: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !root.is_absolute() {
        return Err(io::Error::other("account configuration must be absolute"));
    }
    for path in root.ancestors() {
        match std::fs::symlink_metadata(path) {
            Ok(info) if !info.is_dir() || info.file_type().is_symlink() => {
                return Err(io::Error::other(
                    "redirected account configuration directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    for (relative, directory) in [
        ("", true),
        ("accounts", true),
        ("accounts/vault", true),
        ("accounts/store.json", false),
        ("accounts/vault/vault.key", false),
        ("accounts/vault/vault.enc", false),
    ] {
        match std::fs::symlink_metadata(root.join(relative)) {
            Ok(info)
                if info.file_type().is_symlink()
                    || info.uid() != rustix::process::getuid().as_raw()
                    || (directory && !info.is_dir())
                    || (!directory && (!info.is_file() || info.nlink() != 1)) =>
            {
                return Err(io::Error::other(
                    "redirected or foreign account credentials",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Identity of the revalidated disk, independent of mutable device path names.
pub fn target_fingerprint(size: u64, major_minor: &str, wwn: &str, serial: &str) -> String {
    let canonical = serde_json::json!([size, major_minor, wwn, serial]).to_string();
    Sha256::digest(canonical.as_bytes()).iter().fold(
        String::with_capacity(64),
        |mut output, byte| {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The micro-VM fixture (`tests/accounts_native.rs::production_stage_adopt_accounts`)
    /// seals, carries and adopts a selection through the shipped helper; these are the
    /// frame and fingerprint bounds it never reaches.
    #[test]
    fn the_frame_and_target_fingerprint_are_bounded_and_unambiguous() {
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(InstallAccounts::read_from(&mut frame.as_slice()).is_err());
        let mut truncated = MAGIC.to_vec();
        truncated.extend_from_slice(&1u32.to_be_bytes());
        assert!(InstallAccounts::read_from(&mut truncated.as_slice()).is_err());
        assert_ne!(
            target_fingerprint(1, "2:3", "ab", "c"),
            target_fingerprint(1, "2:3", "a", "bc")
        );
        assert_ne!(
            target_fingerprint(1, "2:3", "ab", "c"),
            target_fingerprint(2, "2:3", "ab", "c")
        );
    }
}
