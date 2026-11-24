//! Secrets in owner-only files, for a machine with no keychain: a headless node or
//! server whose sessions have no Secret Service.
//!
//! The store is one mode-0700 directory. `store.key`, a random 256-bit key written once
//! with an exclusive create, seals each secret as a `tilo-seal` envelope bound to its app
//! and key, in a file named by a digest of those, so a listing shows no name. The store
//! is exactly as private as the disk it is on — unless the platform holds a key that
//! wraps `store.key` ([`KeyWrap`]), as Android's Keystore does: then the files on disk
//! open only on this device, in this app.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tilo_seal::{Context, Label, SymmetricKey, envelope};

use crate::{Protection, Secret, SecretBackend, SecretError};

const KEY_FILE: &str = "store.key";
/// `store.key` wrapped by a key the platform holds: never the plain key's name, so a store
/// never mistakes one for the other.
#[cfg(any(target_os = "android", test))]
const WRAPPED_KEY_FILE: &str = "store.key.wrapped";
const SUFFIX: &str = "secret";
const MAX_FILE_BYTES: u64 = 1 << 20;

pub(crate) struct FileStore {
    dir: PathBuf,
    key: SymmetricKey,
}

/// A key the platform holds, which wraps the store's key before it is written and unwraps
/// it when it is read: the store's key never reaches the disk as it is.
pub(crate) trait KeyWrap {
    fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, SecretError>;
    fn unwrap(&self, wrapped: &[u8]) -> Result<zeroize::Zeroizing<Vec<u8>>, SecretError>;
}

fn failed(what: &str, error: impl std::fmt::Display) -> SecretError {
    SecretError::Failed(format!("{what}: {error}"))
}

impl FileStore {
    pub(crate) fn open(dir: &Path) -> Result<Self, SecretError> {
        prepare_dir(dir)?;
        let key = load_or_create_key(&dir.join(KEY_FILE), None)?;
        Ok(Self {
            dir: dir.to_owned(),
            key,
        })
    }

    /// The store in `dir` whose key is kept wrapped by `wrap`, minted on first open.
    #[cfg(any(target_os = "android", test))]
    pub(crate) fn open_wrapped(dir: &Path, wrap: &dyn KeyWrap) -> Result<Self, SecretError> {
        prepare_dir(dir)?;
        let key = load_or_create_key(&dir.join(WRAPPED_KEY_FILE), Some(wrap))?;
        Ok(Self {
            dir: dir.to_owned(),
            key,
        })
    }

    /// The store in `dir` under `key`, which is kept elsewhere: no key file is read or written.
    #[cfg(target_os = "linux")]
    pub(crate) fn with_key(dir: &Path, key: SymmetricKey) -> Result<Self, SecretError> {
        prepare_dir(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            key,
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
            .join(format!("{}.{SUFFIX}", hex::encode(Sha256::digest(record))))
    }
}

/// Create `dir` owner-only, or make an existing directory so.
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
        Protection::LocalFile
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
        tilo_files::native::atomic::write_with_mode(&self.path(&record), &sealed, 0o600)
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

/// The bytes of `path`, a regular file this user owns that nobody else may read, or
/// `None` when there is none.
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
    let uid = rustix::process::geteuid().as_raw();
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

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// The store's key at `path`, minted with an exclusive create when there is none: as it is,
/// or wrapped by `wrap`.
fn load_or_create_key(
    path: &Path,
    wrap: Option<&dyn KeyWrap>,
) -> Result<SymmetricKey, SecretError> {
    let from = |kept: &[u8]| {
        let unwrapped;
        let bytes = match wrap {
            Some(wrap) => {
                unwrapped = wrap.unwrap(kept)?;
                unwrapped.as_slice()
            }
            None => kept,
        };
        SymmetricKey::from_slice(bytes).map_err(|error| failed("secret store key", error))
    };
    if let Some(bytes) = read_private(path)? {
        return from(&bytes);
    }
    let key = SymmetricKey::generate().map_err(|error| failed("secret store key", error))?;
    let kept = match wrap {
        Some(wrap) => zeroize::Zeroizing::new(wrap.wrap(key.expose_secret())?),
        None => zeroize::Zeroizing::new(key.expose_secret().to_vec()),
    };
    let created = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc_flags())
        .open(path)
        .and_then(|mut file| {
            file.write_all(&kept)?;
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

        let moved = FileStore::open(&dir).unwrap();
        let from = moved.path(&FileStore::record("so.tilo.secrets-test", "kept"));
        let to = moved.path(&FileStore::record("so.tilo.secrets-test", "stolen"));
        fs::copy(&from, &to).unwrap();
        assert!(reopened.get_blocking("stolen").is_err());

        fs::set_permissions(&from, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(reopened.get_blocking("kept").is_err());
    }

    /// A platform key that wraps the store's key: a `tilo-seal` envelope under a key only
    /// this test holds, as the Keystore's key is held by the device alone.
    struct Platform(SymmetricKey);

    impl KeyWrap for Platform {
        fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, SecretError> {
            envelope::seal(&self.0, Context::new(Label::FileSecretStore, b"test"), key)
                .map_err(|error| failed("wrap", error))
        }
        fn unwrap(&self, wrapped: &[u8]) -> Result<zeroize::Zeroizing<Vec<u8>>, SecretError> {
            envelope::open(
                &self.0,
                Context::new(Label::FileSecretStore, b"test"),
                wrapped,
            )
            .map_err(|error| failed("unwrap", error))
        }
    }

    #[test]
    fn a_store_whose_key_the_platform_wraps_opens_only_under_that_platform_key() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("secrets");
        let device = Platform(SymmetricKey::generate().unwrap());
        let store = SecretStore::with_backend(
            "so.tilo.device",
            Arc::new(FileStore::open_wrapped(&dir, &device).unwrap()),
        );
        conformance::check_store_blocking(&store);
        store
            .put_blocking(
                "seed",
                b"thirty-two bytes of device seed!",
                crate::PutOptions::default(),
            )
            .unwrap();

        let reopened = SecretStore::with_backend(
            "so.tilo.device",
            Arc::new(FileStore::open_wrapped(&dir, &device).unwrap()),
        );
        assert_eq!(
            reopened.get_blocking("seed").unwrap().unwrap().expose(),
            b"thirty-two bytes of device seed!"
        );
        assert!(
            !dir.join(KEY_FILE).exists(),
            "a wrapped store keeps no plain key file"
        );
        assert!(
            !FileStore::open(&dir).is_ok_and(|plain| plain.get("so.tilo.device", "seed").is_ok()),
            "the plain store opened what the platform's key guards"
        );

        let copied_elsewhere = Platform(SymmetricKey::generate().unwrap());
        assert!(
            FileStore::open_wrapped(&dir, &copied_elsewhere).is_err(),
            "the files opened under another device's platform key"
        );
    }
}
