use super::*;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn the_channel_state_key_is_created_once_owner_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(CHANNEL_STATE_KEY);
    vault_key(&path).unwrap();
    let first = std::fs::read(&path).unwrap();
    assert_eq!(first.len(), 32);
    vault_key(&path).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        first,
        "an existing key is kept"
    );
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "the key is owner-only");
    std::fs::write(&path, b"short").unwrap();
    assert!(
        vault_key(&path).is_err(),
        "a damaged key is refused, never replaced"
    );
}
