//! A Tilo runtime's device key in the platform's secret store.
//!
//! The Tilo OS layout keeps the device key in a group-private file because
//! two uids share it there. On macOS and Android only the user's own runtime
//! reads it, so it is kept through `tilo-secrets` under this device's own app
//! namespace — not the `so.tilo.identity` namespace person identity keys live
//! in: a login Keychain generic password on macOS, and on Android a sealed file
//! of the app's whose key the Android Keystore wraps. A store that refuses or
//! fails fails the load; nothing falls back to a file.

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

/// What a person calls the store, in what a failure says.
#[cfg(target_os = "macos")]
pub(crate) const NAME: &str = "login Keychain";
#[cfg(target_os = "android")]
pub(crate) const NAME: &str = "Android Keystore";
#[cfg(not(any(target_os = "macos", target_os = "android")))]
pub(crate) const NAME: &str = "platform secret store";

fn store() -> SecretStore {
    SecretStore::for_app(SECRETS_APP)
}

/// The device key the store holds, `None` when it holds none.
pub(crate) fn load() -> Result<Option<SigningKey>, PersistError> {
    get(&store())
}

/// [`load`], minting and storing a key when the store holds none.
pub(crate) fn load_or_create() -> Result<(SigningKey, DeviceKeyOutcome), PersistError> {
    if let Some(key) = load()? {
        return Ok((key, DeviceKeyOutcome::Loaded));
    }
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    put(&store(), &key)?;
    Ok((key, DeviceKeyOutcome::Created))
}

/// Store `key` in place of the one the store holds.
pub(crate) fn replace(key: &SigningKey) -> Result<(), PersistError> {
    put(&store(), key)
}

fn get(store: &SecretStore) -> Result<Option<SigningKey>, PersistError> {
    let Some(secret) = store
        .get_blocking(DEVICE_KEY)
        .map_err(|error| store_error(&error))?
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

/// Store `key` and read it back: a key the store did not keep is not this
/// device's key.
fn put(store: &SecretStore, key: &SigningKey) -> Result<(), PersistError> {
    store
        .put_blocking(
            DEVICE_KEY,
            key.as_bytes(),
            PutOptions {
                label: Some("Tilo device key".into()),
            },
        )
        .map_err(|error| store_error(&error))?;
    match get(store)? {
        Some(stored) if stored.as_bytes() == key.as_bytes() => Ok(()),
        _ => Err(PersistError::Io(io::Error::other(format!(
            "the {NAME} did not return the stored device key"
        )))),
    }
}

fn store_error(error: &SecretError) -> PersistError {
    let kind = match error {
        SecretError::Denied => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    };
    PersistError::Io(io::Error::new(kind, format!("{NAME}, device key: {error}")))
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
            .with_device_key_ownership(DeviceKeyOwnership::PlatformStore);
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
    fn the_device_key_is_minted_once_into_the_platform_store_and_never_a_file() {
        let _store = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
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
        let _store = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let (_dir, paths) = layout();
        let (key, _) = load_or_create_device_key(&paths).unwrap();
        assert_eq!(get(&store()).unwrap().unwrap().as_bytes(), key.as_bytes());
        let person = SecretStore::for_app("so.tilo.identity");
        assert!(
            person.get_blocking(DEVICE_KEY).unwrap().is_none(),
            "the device key leaked into the person identity store"
        );
    }

    /// A phone logged in with a ticket: its key is in the platform store, never in the file, and
    /// the certificate its person signed for that key enrolls it all the same.
    #[test]
    fn a_device_whose_key_the_platform_store_keeps_is_enrolled_by_its_certificate() {
        use ed25519_dalek::SigningKey;
        use tilo_user_identity::{DeviceCapabilities, DeviceCert};

        let _store = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let (_dir, paths) = layout();
        let (device, _) = load_or_create_device_key(&paths).unwrap();
        let _taken = crate::persist::take_device_key(&paths).unwrap();
        let root = SigningKey::from_bytes(&[5; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "phone".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1_000,
            expires_at_ns: Some(10_000),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        assert!(!crate::persist::joined_device_is_ready(&paths, 2_000).unwrap());

        crate::persist::install_device_cert(&paths, &cert, 2_000).unwrap();

        assert!(!paths.device_privkey.exists());
        assert!(
            crate::persist::joined_device_is_ready(&paths, 2_000).unwrap(),
            "a certified device whose key the platform store keeps was not enrolled"
        );
    }

    #[test]
    fn a_failing_platform_store_fails_closed_and_mints_nothing() {
        let _store = tilo_secrets::scope_thread(Arc::new(Broken));
        let (_dir, paths) = layout();
        assert!(load_device_key(&paths).is_err());
        assert!(
            load_or_create_device_key(&paths).is_err(),
            "a failing platform store minted a key anyway"
        );
        assert!(!paths.device_privkey.exists());
    }
}
