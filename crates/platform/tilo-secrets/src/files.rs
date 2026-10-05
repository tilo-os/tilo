//! Secrets in owner-only files, for a machine with no keychain: a headless
//! node or server whose sessions have no Secret Service.
//!
//! The store is one mode-0700 directory. `store.key`, a random 256-bit key
//! written once with an exclusive create, seals each secret as a
//! `tilo-seal` envelope bound to its app and key; the secret's file is named
//! by a digest of those, so a listing shows no app or key name. The key
//! lives beside the secrets, so the store is exactly as private as the disk
//! and the Unix account that own the directory: put it on an encrypted
//! volume. [`Protection::LocalFile`] says so.
//!
//! [`FileStore::open_machine_sealed`] keeps the same files, but never holds
//! the store key in the clear: `store.key.cred` is a systemd credential that
//! the machine's credential service sealed for this account, under the TPM
//! where the machine has one and otherwise under the root-only host key.
//! Only that service opens it again, and only for the same account on the
//! same machine; a copy of the directory, or the whole disk image, elsewhere
//! opens nothing. [`Protection::MachineSealed`] says so.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tilo_seal::{Context, Label, SymmetricKey, envelope};

use crate::{Protection, Secret, SecretBackend, SecretError};

const KEY_FILE: &str = "store.key";
const SEALED_KEY_FILE: &str = "store.key.cred";
/// The name sealed into the store key's credential: a credential sealed for
/// anything else does not open as this store's key.
const CREDENTIAL_NAME: &str = "tilo-secrets-store-key";
/// The machine's credential tool; the service it speaks to enforces that a
/// user-scoped credential opens only for the account that sealed it.
const SYSTEMD_CREDS: &str = "/usr/bin/systemd-creds";
const SUFFIX: &str = "secret";
/// Larger files are not secrets this store wrote.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// A file-backed store rooted at one directory.
pub(crate) struct FileStore {
    dir: PathBuf,
    key: SymmetricKey,
    protection: Protection,
}

/// Seals and opens a store key with a key this process never holds.
pub(crate) trait KeySealer {
    fn seal(&self, key: &[u8]) -> Result<Vec<u8>, SecretError>;
    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError>;
}

/// The machine's credential service, through `systemd-creds --user`.
pub(crate) struct SystemCredentials;

impl SystemCredentials {
    fn run(verb: &str, input: &[u8]) -> Result<Vec<u8>, SecretError> {
        use std::process::{Command, Stdio};
        let mut child = Command::new(SYSTEMD_CREDS)
            .args(["--user", verb, "--name", CREDENTIAL_NAME, "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                SecretError::Unavailable(format!("the machine credential service: {error}"))
            })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| SecretError::Failed("credential service input".into()))?;
        let written = stdin.write_all(input);
        drop(stdin);
        let output = child
            .wait_with_output()
            .map_err(|error| failed("the machine credential service", error))?;
        written.map_err(|error| failed("the machine credential service", error))?;
        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::Unavailable(format!(
                "the machine credential service refused to {verb} the store key: {}",
                reason.trim()
            )));
        }
        Ok(output.stdout)
    }
}

impl KeySealer for SystemCredentials {
    fn seal(&self, key: &[u8]) -> Result<Vec<u8>, SecretError> {
        Self::run("encrypt", key)
    }

    fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
        Self::run("decrypt", sealed).map(|plain| {
            let plain = zeroize::Zeroizing::new(plain);
            plain.to_vec()
        })
    }
}

fn failed(what: &str, error: impl std::fmt::Display) -> SecretError {
    SecretError::Failed(format!("{what}: {error}"))
}

impl FileStore {
    /// Open the store in `dir`, creating it and its key on first use.
    pub(crate) fn open(dir: &Path) -> Result<Self, SecretError> {
        prepare_dir(dir)?;
        let key = load_or_create_key(&dir.join(KEY_FILE))?;
        Ok(Self {
            dir: dir.to_owned(),
            key,
            protection: Protection::LocalFile,
        })
    }

    /// Open the store in `dir` with its key sealed by the machine's
    /// credential service, creating both on first use. There is no fallback:
    /// when the service does not answer, the store is unavailable.
    pub(crate) fn open_machine_sealed(dir: &Path) -> Result<Self, SecretError> {
        Self::open_sealed_with(dir, &SystemCredentials)
    }

    pub(crate) fn open_sealed_with(
        dir: &Path,
        sealer: &dyn KeySealer,
    ) -> Result<Self, SecretError> {
        prepare_dir(dir)?;
        let key = load_or_create_sealed_key(&dir.join(SEALED_KEY_FILE), sealer)?;
        Ok(Self {
            dir: dir.to_owned(),
            key,
            protection: Protection::MachineSealed,
        })
    }

    fn record(app: &str, key: &str) -> Vec<u8> {
        let mut record = Vec::with_capacity(app.len().saturating_add(key.len()).saturating_add(1));
        record.extend_from_slice(app.as_bytes());
        record.push(0);
        record.extend_from_slice(key.as_bytes());
        record
    }

    fn path(&self, record: &[u8]) -> PathBuf {
        self.dir
            .join(format!("{}.{SUFFIX}", hex(&Sha256::digest(record))))
    }
}

fn prepare_dir(dir: &Path) -> Result<(), SecretError> {
    fs::create_dir_all(dir).map_err(|error| failed("secret store directory", error))?;
    let meta = fs::symlink_metadata(dir).map_err(|error| failed("secret store", error))?;
    if !meta.is_dir() {
        return Err(SecretError::Unavailable(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| failed("secret store directory", error))
}

impl SecretBackend for FileStore {
    fn protection(&self) -> Protection {
        self.protection
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        let record = Self::record(app, key);
        let Some(sealed) = read_private(&self.path(&record))? else {
            return Ok(None);
        };
        let plain = envelope::open(
            &self.key,
            Context::new(Label::FileSecretStore, &record),
            &sealed,
        )
        .map_err(|error| failed("stored secret does not open", error))?;
        Ok(Some(Secret::new(plain.to_vec())))
    }

    fn put(&self, app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        let record = Self::record(app, key);
        let sealed = envelope::seal(
            &self.key,
            Context::new(Label::FileSecretStore, &record),
            value,
        )
        .map_err(|error| failed("sealing the secret", error))?;
        write_private(&self.path(&record), &sealed)
            .map_err(|error| failed("storing the secret", error))
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        match fs::remove_file(self.path(&Self::record(app, key))) {
            Ok(()) => sync_dir(&self.dir).map_err(|error| failed("deleting the secret", error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failed("deleting the secret", error)),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The bytes of `path`, a regular file this user owns that nobody else may
/// read, or `None` when there is none.
fn read_private(path: &Path) -> Result<Option<Vec<u8>>, SecretError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc_flags())
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed("reading the secret store", error)),
    };
    check_private(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| failed("reading the secret store", error))?;
    Ok(Some(bytes))
}

fn check_private(file: &File, path: &Path) -> Result<(), SecretError> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = file
        .metadata()
        .map_err(|error| failed("reading the secret store", error))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_file()
        || meta.uid() != uid
        || meta.mode() & 0o077 != 0
        || meta.len() > MAX_FILE_BYTES
    {
        return Err(SecretError::Failed(format!(
            "{} is not an owner-only file of this user",
            path.display()
        )));
    }
    Ok(())
}

const fn libc_flags() -> i32 {
    libc::O_NOFOLLOW | libc::O_CLOEXEC
}

/// Replace `path` with `bytes`, owner-only, atomically and durably.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    let mut nonce = [0_u8; 8];
    tilo_seal::fill_random(&mut nonce).map_err(io::Error::other)?;
    let temp = dir.join(format!(".{}.tmp", hex(&nonce)));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc_flags())
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// The store's key, created once. Concurrent first openers converge on
/// whichever key reached the disk first.
fn load_or_create_key(path: &Path) -> Result<SymmetricKey, SecretError> {
    let from = |bytes: &[u8]| {
        SymmetricKey::from_slice(bytes).map_err(|error| failed("secret store key", error))
    };
    if let Some(bytes) = read_private(path)? {
        return from(&bytes);
    }
    let key = SymmetricKey::generate().map_err(|error| failed("secret store key", error))?;
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc_flags())
        .open(path)
        .and_then(|mut file| {
            file.write_all(key.expose_secret())?;
            file.sync_all()
        });
    match created {
        Ok(()) => {
            sync_dir(path.parent().unwrap_or(path))
                .map_err(|error| failed("secret store key", error))?;
            Ok(key)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let bytes = read_private(path)?
                .ok_or_else(|| SecretError::Failed("secret store key vanished".into()))?;
            from(&bytes)
        }
        Err(error) => Err(failed("secret store key", error)),
    }
}

/// The store's key from its sealed credential, created once. The credential
/// is written whole under a temporary name and linked into place, so a
/// concurrent opener sees either no credential or a complete one, and both
/// converge on whichever was linked first.
fn load_or_create_sealed_key(
    path: &Path,
    machine: &dyn KeySealer,
) -> Result<SymmetricKey, SecretError> {
    let open = |credential: &[u8]| {
        let plain = zeroize::Zeroizing::new(machine.open(credential)?);
        SymmetricKey::from_slice(&plain).map_err(|error| failed("secret store key", error))
    };
    if let Some(credential) = read_private(path)? {
        return open(&credential);
    }
    let key = SymmetricKey::generate().map_err(|error| failed("secret store key", error))?;
    let fresh = machine.seal(key.expose_secret())?;
    let dir = path.parent().unwrap_or(path);
    let mut nonce = [0_u8; 8];
    tilo_seal::fill_random(&mut nonce).map_err(|error| failed("secret store key", error))?;
    let temp = dir.join(format!(".{}.tmp", hex(&nonce)));
    let linked = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc_flags())
            .open(&temp)?;
        file.write_all(&fresh)?;
        file.sync_all()?;
        fs::hard_link(&temp, path)
    })();
    let _ = fs::remove_file(&temp);
    match linked {
        Ok(()) => {
            sync_dir(dir).map_err(|error| failed("secret store key", error))?;
            Ok(key)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let winner = read_private(path)?
                .ok_or_else(|| SecretError::Failed("secret store key vanished".into()))?;
            open(&winner)
        }
        Err(error) => Err(failed("secret store key", error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SecretStore, conformance};
    use std::sync::Arc;

    #[test]
    fn the_file_store_conforms_and_keeps_secrets_across_opens() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("secrets");
        let store = SecretStore::with_backend(
            "so.tilo.secrets-test",
            Arc::new(FileStore::open(&dir).unwrap()),
        );
        assert_eq!(store.protection(), Protection::LocalFile);
        conformance::check_store_blocking(&store);
        store
            .put_blocking("kept", b"across opens", crate::PutOptions::default())
            .unwrap();
        let reopened = SecretStore::with_backend(
            "so.tilo.secrets-test",
            Arc::new(FileStore::open(&dir).unwrap()),
        );
        assert_eq!(
            reopened.get_blocking("kept").unwrap().unwrap().expose(),
            b"across opens"
        );
        let other =
            SecretStore::with_backend("so.tilo.other", Arc::new(FileStore::open(&dir).unwrap()));
        assert_eq!(other.get_blocking("kept").unwrap(), None);

        // Nothing on disk names the app or holds the secret in the clear.
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let mode = entry.metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{}", entry.path().display());
            let name = entry.file_name().into_string().unwrap();
            assert!(!name.contains("secrets-test") && !name.contains("kept"));
            let bytes = fs::read(entry.path()).unwrap();
            assert!(!bytes.windows(12).any(|window| window == b"across opens"));
        }
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        // A secret file moved under another key does not open.
        let moved = FileStore::open(&dir).unwrap();
        let from = moved.path(&FileStore::record("so.tilo.secrets-test", "kept"));
        let to = moved.path(&FileStore::record("so.tilo.secrets-test", "stolen"));
        fs::copy(&from, &to).unwrap();
        assert!(reopened.get_blocking("stolen").is_err());

        // A file others may read is refused rather than trusted.
        fs::set_permissions(&from, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(reopened.get_blocking("kept").is_err());
    }

    /// Seals with a fixed key, as the machine's credential service does with
    /// one this process never sees; counts how often it was asked.
    struct TestSealer {
        key: SymmetricKey,
        seals: std::sync::atomic::AtomicUsize,
    }

    impl TestSealer {
        fn new(byte: u8) -> Self {
            Self {
                key: SymmetricKey::from_slice(&[byte; 32]).unwrap(),
                seals: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl KeySealer for TestSealer {
        fn seal(&self, key: &[u8]) -> Result<Vec<u8>, SecretError> {
            self.seals
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            envelope::seal(&self.key, Context::new(Label::FileSecretStore, b"k"), key)
                .map_err(|error| failed("test seal", error))
        }
        fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, SecretError> {
            envelope::open(
                &self.key,
                Context::new(Label::FileSecretStore, b"k"),
                sealed,
            )
            .map(|plain| plain.to_vec())
            .map_err(|_| SecretError::Unavailable("not this machine".into()))
        }
    }

    #[test]
    fn a_machine_sealed_store_never_holds_its_key_in_the_clear() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("secrets");
        let machine = TestSealer::new(3);
        let opened = FileStore::open_sealed_with(&dir, &machine).unwrap();
        assert_eq!(opened.protection(), Protection::MachineSealed);
        let store = SecretStore::with_backend("so.tilo.secrets-test", Arc::new(opened));
        conformance::check_store_blocking(&store);
        store
            .put_blocking("kept", b"across opens", crate::PutOptions::default())
            .unwrap();

        // The same machine opens it again without sealing a new key.
        let again = SecretStore::with_backend(
            "so.tilo.secrets-test",
            Arc::new(FileStore::open_sealed_with(&dir, &machine).unwrap()),
        );
        assert_eq!(
            again.get_blocking("kept").unwrap().unwrap().expose(),
            b"across opens"
        );
        assert_eq!(machine.seals.load(std::sync::atomic::Ordering::Relaxed), 1);

        // No plain key file, and no file holds the key or the secret.
        assert!(!dir.join(KEY_FILE).exists());
        let sealed = fs::read(dir.join(SEALED_KEY_FILE)).unwrap();
        let key = machine.open(&sealed).map(zeroize::Zeroizing::new).unwrap();
        for entry in fs::read_dir(&dir).unwrap() {
            let bytes = fs::read(entry.unwrap().path()).unwrap();
            assert!(
                !bytes
                    .windows(key.len())
                    .any(|window| window == key.as_slice())
            );
            assert!(!bytes.windows(12).any(|window| window == b"across opens"));
        }

        // Another machine (or account) opens nothing, and never falls back
        // to a fresh key that would orphan what the store holds.
        let elsewhere = TestSealer::new(4);
        assert!(matches!(
            FileStore::open_sealed_with(&dir, &elsewhere),
            Err(SecretError::Unavailable(_))
        ));
        assert_eq!(
            elsewhere.seals.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
