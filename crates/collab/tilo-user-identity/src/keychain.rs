//! A Tilo runtime's device key in the user's login Keychain (macOS).
//!
//! The Tilo OS layout keeps the device key in a group-private file because
//! two uids share it there. On macOS only the user's own runtime reads it,
//! so it is a Keychain generic password, stored through `tilo-secrets` like
//! every other Tilo secret. A key file an earlier build left behind moves in
//! once: it is stored, read back, and only then removed. A Keychain that
//! refuses or fails fails the load; nothing falls back to a file.

use std::{fs, io};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_secrets::{PutOptions, SecretError, SecretStore};
use zeroize::Zeroizing;

use crate::persist::{DeviceKeyOutcome, IdentityPaths, PersistError, read_device_key};

/// The namespace `runtime-identity` keeps personal identity keys in.
const SECRETS_APP: &str = "so.tilo.identity";
const DEVICE_KEY: &str = "tilo-device-signing-v1";

fn store() -> SecretStore {
    SecretStore::for_app(SECRETS_APP)
}

/// The device key, moving a key file an earlier build left into the
/// Keychain first; `None` when neither holds one.
pub(crate) fn load(paths: &IdentityPaths) -> Result<Option<SigningKey>, PersistError> {
    let store = store();
    let stored = get(&store)?;
    let Some(file) = legacy_file(paths)? else {
        return Ok(stored);
    };
    match stored {
        Some(key) if key.as_bytes() != file.as_bytes() => Err(PersistError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the login Keychain and {} hold different device keys; restore one \
                 and remove the other",
                paths.device_privkey.display()
            ),
        ))),
        Some(key) => {
            remove_legacy_file(paths)?;
            Ok(Some(key))
        }
        None => {
            put(&store, &file)?;
            remove_legacy_file(paths)?;
            Ok(Some(file))
        }
    }
}

/// [`load`], minting and storing a key when there is none.
pub(crate) fn load_or_create(
    paths: &IdentityPaths,
) -> Result<(SigningKey, DeviceKeyOutcome), PersistError> {
    if let Some(key) = load(paths)? {
        return Ok((key, DeviceKeyOutcome::Loaded));
    }
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    put(&store(), &key)?;
    Ok((key, DeviceKeyOutcome::Created))
}

fn get(store: &SecretStore) -> Result<Option<SigningKey>, PersistError> {
    let Some(secret) = store
        .get_blocking(DEVICE_KEY)
        .map_err(|error| keychain_error(&error))?
    else {
        return Ok(None);
    };
    let bytes = secret.expose();
    if bytes.len() != 32 {
        return Err(PersistError::WrongKeyLength(bytes.len()));
    }
    let mut seed = Zeroizing::new([0_u8; 32]);
    seed.copy_from_slice(bytes);
    Ok(Some(SigningKey::from_bytes(&seed)))
}

/// Store `key` and read it back: a key the Keychain did not keep is not
/// this device's key.
fn put(store: &SecretStore, key: &SigningKey) -> Result<(), PersistError> {
    store
        .put_blocking(
            DEVICE_KEY,
            key.as_bytes(),
            PutOptions {
                label: Some("Tilo device key".into()),
            },
        )
        .map_err(|error| keychain_error(&error))?;
    match get(store)? {
        Some(stored) if stored.as_bytes() == key.as_bytes() => Ok(()),
        _ => Err(PersistError::Io(io::Error::other(
            "the login Keychain did not return the stored device key",
        ))),
    }
}

fn legacy_file(paths: &IdentityPaths) -> Result<Option<SigningKey>, PersistError> {
    match fs::symlink_metadata(&paths.device_privkey) {
        Ok(_) => read_device_key(&paths.device_privkey).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PersistError::Io(e)),
    }
}

fn remove_legacy_file(paths: &IdentityPaths) -> Result<(), PersistError> {
    fs::remove_file(&paths.device_privkey)?;
    if let Some(parent) = paths.device_privkey.parent() {
        tilo_files::native::atomic::sync_directory(parent)?;
    }
    Ok(())
}

fn keychain_error(error: &SecretError) -> PersistError {
    let kind = match error {
        SecretError::Denied => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    };
    PersistError::Io(io::Error::new(
        kind,
        format!("login Keychain, device key: {error}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::{DeviceKeyCustody, load_device_key, load_or_create_device_key};
    use std::sync::Arc;
    use tilo_secrets::{Protection, Secret, SecretBackend};

    /// A layout below a resolved temp dir: key files refuse symlinked
    /// paths, and macOS's `/var` is one.
    fn layout() -> (tempfile::TempDir, IdentityPaths) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let paths = IdentityPaths::under(root).with_device_key_custody(DeviceKeyCustody::Keychain);
        (dir, paths)
    }

    fn file_layout(paths: &IdentityPaths) -> IdentityPaths {
        paths
            .clone()
            .with_device_key_custody(DeviceKeyCustody::File)
    }

    struct Broken;

    impl SecretBackend for Broken {
        fn protection(&self) -> Protection {
            Protection::OsKeychain
        }
        fn get(&self, _: &str, _: &str) -> Result<Option<Secret>, SecretError> {
            Err(SecretError::Unavailable("locked".into()))
        }
        fn put(&self, _: &str, _: &str, _: &[u8], _: &str) -> Result<(), SecretError> {
            Err(SecretError::Unavailable("locked".into()))
        }
        fn delete(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Err(SecretError::Unavailable("locked".into()))
        }
    }

    #[test]
    fn the_device_key_is_minted_once_into_the_keychain_and_never_a_file() {
        let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let (_dir, paths) = layout();
        assert!(load_device_key(&paths).is_err(), "no key yet");

        let (created, outcome) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(outcome, DeviceKeyOutcome::Created);
        let (loaded, outcome) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(outcome, DeviceKeyOutcome::Loaded);
        assert_eq!(loaded.as_bytes(), created.as_bytes());
        assert_eq!(
            load_device_key(&paths).unwrap().as_bytes(),
            created.as_bytes()
        );
        assert!(!paths.device_privkey.exists());
        assert!(!paths.device_privkey.parent().unwrap().exists());
    }

    #[test]
    fn a_key_file_moves_into_the_keychain_once() {
        let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let (_dir, paths) = layout();
        let (file_key, _) = load_or_create_device_key(&file_layout(&paths)).unwrap();
        assert!(paths.device_privkey.is_file());

        let moved = load_device_key(&paths).unwrap();
        assert_eq!(moved.as_bytes(), file_key.as_bytes());
        assert!(!paths.device_privkey.exists(), "the key file stayed behind");
        assert_eq!(
            get(&store()).unwrap().unwrap().as_bytes(),
            file_key.as_bytes()
        );

        // A copy restored from a backup that matches is tidied away.
        let restored = file_layout(&paths);
        crate::persist::write_device_key(&restored.device_privkey, &file_key).unwrap();
        assert_eq!(
            load_device_key(&paths).unwrap().as_bytes(),
            file_key.as_bytes()
        );
        assert!(!paths.device_privkey.exists());
    }

    #[test]
    fn two_different_keys_or_a_failing_keychain_fail_closed_and_keep_the_file() {
        let (_dir, paths) = layout();
        let file = file_layout(&paths);
        {
            let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
            load_or_create_device_key(&paths).unwrap();
            let other = SigningKey::from_bytes(&[7; 32]);
            file.ensure_directories().unwrap();
            crate::persist::write_device_key(&file.device_privkey, &other).unwrap();
            let error = load_device_key(&paths).unwrap_err();
            assert!(
                error.to_string().contains("different device keys"),
                "{error}"
            );
            assert!(load_or_create_device_key(&paths).is_err());
            assert!(
                file.device_privkey.is_file(),
                "a conflicting key was removed"
            );
        }
        {
            let _keychain = tilo_secrets::scope_thread(Arc::new(Broken));
            assert!(load_device_key(&paths).is_err());
            assert!(
                load_or_create_device_key(&paths).is_err(),
                "a failing Keychain minted a key anyway"
            );
            assert_eq!(
                crate::persist::read_device_key(&file.device_privkey)
                    .unwrap()
                    .as_bytes(),
                &[7; 32],
                "the key file must survive a failed move"
            );
        }
    }
}
