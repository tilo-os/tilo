use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn the_channel_state_key_file_is_created_once_owner_only() {
    let directory = tempfile::tempdir().unwrap();
    let room = RoomId::new();
    let path = directory.path().join(CHANNEL_STATE_KEY);
    vault_key(directory.path(), room, IdentityStorage::Files).unwrap();
    let first = std::fs::read(&path).unwrap();
    assert_eq!(first.len(), 32);
    vault_key(directory.path(), room, IdentityStorage::Files).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        first,
        "an existing key is kept"
    );
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the key is owner-only");
    std::fs::write(&path, b"short").unwrap();
    assert!(
        vault_key(directory.path(), room, IdentityStorage::Files).is_err(),
        "a damaged key is refused, never replaced"
    );
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::*;
    use tilo_secrets::{Protection, Secret, SecretBackend, SecretError};

    fn stored(room: RoomId) -> Option<Vec<u8>> {
        tilo_secrets::SecretStore::for_app("so.tilo.runtime")
            .get_blocking(&format!("channel-state-v1.{room}"))
            .unwrap()
            .map(|secret| secret.expose().to_vec())
    }

    #[test]
    fn a_key_file_moves_into_the_keychain_once_and_no_file_is_written() {
        let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CHANNEL_STATE_KEY);

        let fresh = RoomId::new();
        vault_key(directory.path(), fresh, IdentityStorage::Platform).unwrap();
        assert!(!path.exists(), "a new key was written to a file");
        let first = stored(fresh).unwrap();
        vault_key(directory.path(), fresh, IdentityStorage::Platform).unwrap();
        assert_eq!(stored(fresh).unwrap(), first, "an existing key is kept");

        let migrated = RoomId::new();
        vault_key(directory.path(), migrated, IdentityStorage::Files).unwrap();
        let file_key = std::fs::read(&path).unwrap();
        vault_key(directory.path(), migrated, IdentityStorage::Platform).unwrap();
        assert_eq!(stored(migrated).unwrap(), file_key);
        assert!(!path.exists(), "the key file stayed behind");
    }

    struct Locked;

    impl SecretBackend for Locked {
        fn protection(&self) -> Protection {
            Protection::OsKeychain
        }
        fn get(&self, _: &str, _: &str) -> Result<Option<Secret>, SecretError> {
            Err(SecretError::Denied)
        }
        fn put(&self, _: &str, _: &str, _: &[u8], _: &str) -> Result<(), SecretError> {
            Err(SecretError::Denied)
        }
        fn delete(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Err(SecretError::Denied)
        }
    }

    #[test]
    fn a_conflicting_or_refusing_keychain_fails_closed_and_keeps_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(CHANNEL_STATE_KEY);
        let room = RoomId::new();
        {
            let _keychain = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
            vault_key(directory.path(), room, IdentityStorage::Platform).unwrap();
            std::fs::write(&path, [9; 32]).unwrap();
            let error = vault_key(directory.path(), room, IdentityStorage::Platform).unwrap_err();
            assert!(error.to_string().contains("different"), "{error}");
        }
        {
            let _keychain = tilo_secrets::scope_thread(std::sync::Arc::new(Locked));
            let error = vault_key(directory.path(), room, IdentityStorage::Platform).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        assert_eq!(std::fs::read(&path).unwrap(), [9; 32]);
    }
}
