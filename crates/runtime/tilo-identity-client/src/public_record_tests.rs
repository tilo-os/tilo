use super::*;
use std::os::unix::fs::symlink;

fn record(schema: u32) -> serde_json::Value {
    serde_json::json!({
        "schema_version": schema,
        "display_name": "Ada",
        "avatar_color": [10, 20, 30],
        "profile_photo": {"width": 1, "height": 1, "rgba": [1, 2, 3, 255]},
        "threshold_identity": {
            "single_device": false,
            "group_public_key": vec![7; 32],
            "threshold": 1,
            "total_shares": 1,
            "share_index": 0,
            "secret_bytes": {"not": "a decoded signing share"}
        },
        "secret_ref": {"backend": "unavailable", "locator": "/not/a/custody/path"}
    })
}

fn private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn public_fields_parse_without_decoding_shares_or_resolving_providers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identity.json");
    for schema in [1, 2] {
        let bytes = serde_json::to_vec(&record(schema)).unwrap();
        private(&path, &bytes);
        let metadata = load_metadata_from_path(&path).unwrap();
        assert_eq!(metadata.display_name, "Ada");
        assert_eq!(metadata.avatar_color, [10, 20, 30]);
        assert_eq!(metadata.group_public_key, [7; 32]);
        assert_eq!(
            (
                metadata.threshold,
                metadata.total_shares,
                metadata.share_index
            ),
            (1, 1, 0)
        );
        assert!(!metadata.single_device);
        assert_eq!(metadata.profile_photo.unwrap().rgba(), &[1, 2, 3, 255]);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

#[test]
fn public_record_rejects_invalid_schema_key_photo_and_size() {
    let mut invalid = record(3);
    assert!(load_metadata_from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
    invalid = record(1);
    invalid["threshold_identity"]["group_public_key"] = serde_json::json!([7]);
    assert!(load_metadata_from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
    invalid = record(2);
    invalid["profile_photo"]["rgba"] = serde_json::json!([1]);
    assert!(load_metadata_from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
    assert!(load_metadata_from_bytes(b"{}").is_err());
    assert!(load_metadata_from_bytes(&vec![b' '; 1024 * 1024 + 1]).is_err());
}

#[test]
fn explicit_public_record_read_refuses_symlinks_permissions_and_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record.json");
    private(&path, &serde_json::to_vec(&record(2)).unwrap());
    let link = dir.path().join("link.json");
    symlink(&path, &link).unwrap();
    assert!(load_metadata_from_path(&link).is_err());
    assert!(load_metadata_from_path(dir.path()).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load_metadata_from_path(&path).is_err());
    private(&path, &vec![b' '; 1024 * 1024 + 1]);
    assert!(load_metadata_from_path(&path).is_err());
}

#[test]
fn active_selector_is_confined_and_preserves_legacy_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("primary.json");
    let selected = dir.path().join("identity-ada.json");
    let selector = dir.path().join("active");
    private(&primary, &serde_json::to_vec(&record(1)).unwrap());
    private(&selected, &serde_json::to_vec(&record(2)).unwrap());
    assert_eq!(resolve_active_identity(dir.path()), Some(primary.clone()));
    private(&selector, b"identity-ada\n");
    assert_eq!(resolve_active_identity(dir.path()), Some(selected.clone()));
    for invalid in [
        b"../outside".as_slice(),
        b"/outside",
        b"",
        b"Upper",
        &[b'a'; 129],
        &[0xff],
    ] {
        private(&selector, invalid);
        assert_eq!(resolve_active_identity(dir.path()), Some(primary.clone()));
    }
    private(&selector, b"identity-ada");
    fs::set_permissions(&selector, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(resolve_active_identity(dir.path()), Some(primary.clone()));
    fs::remove_file(&selector).unwrap();
    symlink(&selected, &selector).unwrap();
    assert_eq!(resolve_active_identity(dir.path()), Some(primary));
    fs::remove_file(&selector).unwrap();
    private(&selector, b"identity-ada");
    fs::remove_file(&selected).unwrap();
    symlink(dir.path().join("primary.json"), &selected).unwrap();
    assert_eq!(resolve_active_identity(dir.path()), None);
}
