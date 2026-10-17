use super::*;

fn retained_witness() -> Witness {
    serde_json::from_value(serde_json::json!({
        "proof_sha256": "a".repeat(64),
        "generated": {
            "path": ".bashrc",
            "stamp": {
                "inode": 7, "uid": 1000, "mode": 420, "bytes": 12,
                "mtime_ns": 0, "ctime_ns": 0
            },
            "sha256": "b".repeat(64)
        },
        "target": {
            "identity": { "volume": [1], "inode": 7 },
            "digest": vec![0; 32],
            "mode": 420,
            "directory": false
        }
    }))
    .unwrap()
}

#[test]
fn retained_seed_witness_binds_path_and_regular_file_policy() {
    let mut witness = retained_witness();
    witness.validate(Path::new(".bashrc")).unwrap();
    assert!(witness.validate(Path::new(".profile")).is_err());
    witness.target.directory = true;
    assert!(witness.validate(Path::new(".bashrc")).is_err());
    witness.target.directory = false;
    witness.target.mode = 0o4755;
    assert!(witness.validate(Path::new(".bashrc")).is_err());
}

#[test]
fn generated_paths_cannot_name_active_custody_or_escape_home() {
    for path in [
        "",
        "../.bashrc",
        "/etc/passwd",
        ".config/tilo/key",
        ".local/state/tilo/journal",
    ] {
        assert!(relative(path).is_err(), "{path}");
    }
    assert_eq!(
        relative(".config/app/settings").unwrap(),
        Path::new(".config/app/settings")
    );
}

#[test]
fn retained_seed_records_preserve_journal_shape_and_reject_extensions() {
    let witness = retained_witness();
    let record = serde_json::to_value(&witness).unwrap();
    let recovered: Witness = serde_json::from_value(record.clone()).unwrap();
    assert_eq!(serde_json::to_value(recovered).unwrap(), record);
    let mut extended = record;
    extended["authority"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Witness>(extended).is_err());
}
