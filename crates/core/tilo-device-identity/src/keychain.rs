//! A Tilo runtime's device key in the user's login Keychain (macOS).
//!
//! The Tilo OS layout keeps the device key in a group-private file because
//! two uids share it there. On macOS only the user's own runtime reads it,
//! so it is a Keychain generic password, stored through `tilo-secrets` under
//! this device's own app namespace — not the `so.tilo.identity` namespace
//! person identity keys live in. A Keychain that refuses or fails fails the
//! load; nothing falls back to a file.

use std::io;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_secrets::{PutOptions, SecretError, SecretStore};
use zeroize::Zeroizing;

use crate::persist::{DeviceKeyOutcome, PersistError};

/// The namespace the device key's app owns. Person identity signing shares
/// live under `so.tilo.identity`, so neither store can answer for the other.
const SECRETS_APP: &str = "so.tilo.device";
const DEVICE_KEY: &str = "tilo-device-signing-v1";

fn store() -> SecretStore {
    SecretStore::for_app(SECRETS_APP)
}

/// The device key the login Keychain holds, `None` when it holds none.
pub(crate) fn load() -> Result<Option<SigningKey>, PersistError> {
    get(&store())
}

/// [`load`], minting and storing a key when the Keychain holds none.
pub(crate) fn load_or_create() -> Result<(SigningKey, DeviceKeyOutcome), PersistError> {
    if let Some(key) = load()? {
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
    use crate::persist::{
        DeviceKeyOwnership, IdentityPaths, load_device_key, load_or_create_device_key,
    };
    use std::sync::Arc;
    use tilo_secrets::{Protection, Secret, SecretBackend};

    fn layout() -> (tempfile::TempDir, IdentityPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path())
            .with_device_key_ownership(DeviceKeyOwnership::Keychain);
        (dir, paths)
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
    fn the_device_key_is_never_read_from_the_person_identity_namespace() {
        let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let (_dir, paths) = layout();
        let (key, _) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(get(&store()).unwrap().unwrap().as_bytes(), key.as_bytes());
        let person = SecretStore::for_app("so.tilo.identity");
        assert!(
            person.get_blocking(DEVICE_KEY).unwrap().is_none(),
            "the device key leaked into the person identity store"
        );
    }

    #[test]
    fn a_failing_keychain_fails_closed_and_mints_nothing() {
        let _keychain = tilo_secrets::scope_thread(Arc::new(Broken));
        let (_dir, paths) = layout();
        assert!(load_device_key(&paths).is_err());
        assert!(
            load_or_create_device_key(&paths).is_err(),
            "a failing Keychain minted a key anyway"
        );
        assert!(!paths.device_privkey.exists());
    }
}
