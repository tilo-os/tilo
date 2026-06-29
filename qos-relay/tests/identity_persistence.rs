//! Persistent ed25519 keypair round-trip tests.

use qos_relay::load_or_create_keypair;

#[test]
fn fresh_path_generates_a_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    assert!(!path.exists());
    let _kp = load_or_create_keypair(&path).expect("creates");
    assert!(path.exists());
    let meta = std::fs::metadata(&path).unwrap();
    assert!(meta.len() > 0, "key file must be non-empty");
}

#[test]
fn same_path_yields_same_peer_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    let kp1 = load_or_create_keypair(&path).expect("first");
    let kp2 = load_or_create_keypair(&path).expect("second");
    assert_eq!(kp1.public().to_peer_id(), kp2.public().to_peer_id());
}

#[test]
fn key_file_is_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    let _ = load_or_create_keypair(&path).expect("creates");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "got mode {mode:o}");
}

#[test]
fn corrupted_file_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("identity.key");
    std::fs::write(&path, b"not a keypair").unwrap();
    let err = load_or_create_keypair(&path).expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("unreadable"), "got: {msg}");
}

#[test]
fn nested_parents_are_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a").join("b").join("c").join("identity.key");
    assert!(!path.parent().unwrap().exists());
    let _ = load_or_create_keypair(&path).expect("creates dirs + file");
    assert!(path.exists());
}
