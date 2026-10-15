//! Private, bounded transport for live-session accounts. Only the installed user
//! opens the sealed bundle; the privileged installer merely carries its bytes.
use account_custody::portable::SealedAccounts;
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
    /// # Errors
    /// Propagates pipe failures or an oversized envelope.
    pub fn write_to(&self, output: &mut impl Write) -> io::Result<()> {
        let length = u32::try_from(self.ciphertext.len()).map_err(io::Error::other)?;
        output.write_all(MAGIC)?;
        output.write_all(&length.to_be_bytes())?;
        output.write_all(self.key.expose_secret())?;
        output.write_all(&self.ciphertext)
    }

    /// Read exactly one bounded frame, refusing truncation and trailing bytes.
    /// # Errors
    /// Refuses malformed, oversized or non-key-based artifacts.
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

    /// The caller must already have dropped real/effective IDs and supplementary
    /// groups to the destination user. Authentication precedes all vault writes.
    /// # Errors
    /// Refuses wrong user, invalid binding/key, or conflicting destination state.
    pub fn import_for_user(
        &self,
        root: &std::path::Path,
        binding: &account_custody::portable::PortableBinding,
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
        binding: &account_custody::portable::PortableBinding,
    ) -> Result<(), Box<dyn std::error::Error>> {
        validate_destination(root)?;
        let sealed = SealedAccounts::from_bytes(&self.ciphertext)?;
        account_custody::portable::import_sealed_accounts(
            root,
            &sealed,
            &self.key,
            binding.as_bytes(),
        )?;
        Ok(())
    }
}

// Refuse pre-existing custody aliases, including aliases left in a migrated
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
                return Err(io::Error::other("redirected or foreign account custody"));
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
    use account_custody::portable::{self, PortableBinding};
    use accounts::{Account, AccountId, AccountStore, AccountTypeId, Secret, SecretVault};

    #[test]
    fn framed_selection_authenticates_before_writes_and_retries_without_touching_source() {
        let uid = rustix::process::getuid().as_raw().max(1000);
        let directory = tempfile::tempdir().unwrap();
        // The production consumer uses a canonical home. TMPDIR itself may
        // contain an alias (for example /home -> /var/home on Fedora).
        let root = directory.path().canonicalize().unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        let id = AccountId::new("selected");
        let mut vault = SecretVault::open_in(&source);
        vault.put(&id, &Secret::password("private fixture credential"));
        vault.try_save().unwrap();
        let mut store = AccountStore::new();
        store.upsert(Account::new(id.clone(), AccountTypeId::new("webdav")));
        store.save_in(&source).unwrap();
        let source_key = std::fs::read(accounts::vault_dir_in(&source).join("vault.key")).unwrap();
        let source_cipher =
            std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
        let binding = PortableBinding::new("operation", "disk", "person", uid).unwrap();
        let key = SymmetricKey::generate().unwrap();
        let sealed = portable::seal_selected_accounts(
            &source,
            std::slice::from_ref(&id),
            &key,
            binding.as_bytes(),
        )
        .unwrap();
        let accounts = InstallAccounts::new(&sealed, key);
        let mut frame = zeroize::Zeroizing::new(Vec::new());
        accounts.write_to(&mut *frame).unwrap();
        let received = InstallAccounts::read_from(&mut frame.as_slice()).unwrap();
        assert_eq!(received, accounts);
        let wrong_user =
            PortableBinding::new("operation", "disk", "person", uid.saturating_add(1)).unwrap();
        assert!(received.import_for_user(&destination, &wrong_user).is_err());
        assert!(!destination.exists());
        if rustix::process::getuid().is_root() {
            assert!(received.import_for_user(&destination, &binding).is_err());
            assert!(!destination.exists());
        }
        for wrong in [
            PortableBinding::new("other", "disk", "person", uid).unwrap(),
            PortableBinding::new("operation", "other", "person", uid).unwrap(),
            PortableBinding::new("operation", "disk", "other", uid).unwrap(),
            PortableBinding::new("operation", "disk", "person", uid.saturating_add(1)).unwrap(),
        ] {
            assert!(received.import_authenticated(&destination, &wrong).is_err());
            assert!(!destination.exists());
        }
        let wrong_key = InstallAccounts {
            ciphertext: received.ciphertext.clone(),
            key: SymmetricKey::generate().unwrap(),
        };
        assert!(
            wrong_key
                .import_authenticated(&destination, &binding)
                .is_err()
        );
        assert!(!destination.exists());
        received
            .import_authenticated(&destination, &binding)
            .unwrap();
        received
            .import_authenticated(&destination, &binding)
            .unwrap();
        assert_eq!(AccountStore::load_in(&destination).len(), 1);
        assert_eq!(
            SecretVault::open_read_only_in(&destination)
                .get(&id)
                .unwrap()
                .password,
            "private fixture credential"
        );
        assert_eq!(
            std::fs::read(accounts::vault_dir_in(&source).join("vault.key")).unwrap(),
            source_key
        );
        assert_eq!(
            std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
            source_cipher
        );
        assert_ne!(
            std::fs::read(accounts::vault_dir_in(&destination).join("vault.key")).unwrap(),
            source_key
        );
        assert_destination_rejects_alias_and_conflict(
            &root,
            &source,
            &destination,
            &received,
            &binding,
            &id,
            &source_cipher,
        );
        assert!(InstallAccounts::read_from(&mut &frame[..frame.len().saturating_sub(1)]).is_err());
        frame.push(0);
        assert!(InstallAccounts::read_from(&mut frame.as_slice()).is_err());
    }

    fn assert_destination_rejects_alias_and_conflict(
        directory: &std::path::Path,
        source: &std::path::Path,
        destination: &std::path::Path,
        received: &InstallAccounts,
        binding: &PortableBinding,
        id: &AccountId,
        source_cipher: &[u8],
    ) {
        let redirected = directory.join("redirected");
        std::os::unix::fs::symlink(source, &redirected).unwrap();
        assert!(received.import_authenticated(&redirected, binding).is_err());
        assert_eq!(
            std::fs::read(accounts::vault_dir_in(source).join("vault.enc")).unwrap(),
            source_cipher
        );
        let mut changed = SecretVault::open_in(destination);
        changed.put(id, &Secret::password("new destination credential"));
        changed.try_save().unwrap();
        assert!(received.import_authenticated(destination, binding).is_err());
        assert_eq!(
            SecretVault::open_read_only_in(destination)
                .get(id)
                .unwrap()
                .password,
            "new destination credential"
        );
    }

    #[test]
    fn oversized_frame_and_ambiguous_identity_are_rejected() {
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(InstallAccounts::read_from(&mut frame.as_slice()).is_err());
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
