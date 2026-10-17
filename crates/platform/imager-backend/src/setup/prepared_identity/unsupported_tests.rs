//! Unsupported providers refuse before creating files or adopting public claims.
use super::*;
use std::path::Path;
use tilo_identity_host::provisioning::IdentityStorage;
use zeroize::Zeroizing;

fn refused<T>(result: std::io::Result<T>) {
    assert_eq!(
        result.err().expect("must refuse custody").kind(),
        std::io::ErrorKind::Unsupported
    );
}

#[test]
fn identity_operations_never_create_or_restore_custody() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("identity");
    refused(PreparedIdentity::create(
        &directory,
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    ));
    refused(PreparedIdentity::resume(&directory, [0; 32]));
    refused(PreparedIdentity::recover_created(&directory));
    refused(PreparedIdentity::restore_backup(
        &directory,
        Path::new("not-an-admitted-backup"),
        Zeroizing::new("password".into()),
    ));
    refused(PreparedIdentity::review_current());
    assert!(!directory.exists());
}

#[test]
fn selected_avatar_and_private_tree_have_no_unprotected_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("custody");
    refused(ensure_private_tree(&directory));
    refused(read_avatar(Path::new("not-an-admitted-photo")));
    assert!(!directory.exists());
}

#[test]
fn decoded_current_device_claim_is_not_authority() {
    let declaration: CurrentDeviceDeclaration = serde_json::from_value(serde_json::json!({
        "schema": 1,
        "user_public_key": vec![0u8; 32],
        "device_public_key": vec![1u8; 32],
        "record_name": "primary.json",
        "record_sha256": vec![2u8; 32],
        "certificate_sha256": vec![3u8; 32],
        "fleet_sha256": vec![4u8; 32],
    }))
    .unwrap();
    refused(PreparedIdentity::resume_current(
        Path::new("identity"),
        &declaration,
    ));
}
