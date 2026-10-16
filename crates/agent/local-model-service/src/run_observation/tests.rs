use super::*;

#[test]
fn counters_distinguish_missing_malformed_and_actual_zero() {
    assert_eq!(numbers("0\n", true).unwrap().get("bytes"), Some(&0));
    assert!(numbers("max\n", true).is_err());
    assert!(numbers("pgfault 1\npgfault 2\n", false).is_err());
    assert!(numbers("pgfault\n", false).is_err());
    assert!(numbers("\n", false).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        metric(&dir.path().join("absent"), true),
        Observed::Unavailable { .. }
    ));
    fs::write(dir.path().join("limit"), "max\n").unwrap();
    assert_eq!(
        limit(&dir.path().join("limit")),
        Observed::Actual {
            value: "max".into()
        }
    );
    fs::write(dir.path().join("limit"), "garbage\n").unwrap();
    assert!(matches!(
        limit(&dir.path().join("limit")),
        Observed::Unavailable { .. }
    ));
}

#[test]
fn io_records_preserve_devices_and_refuse_duplicate_or_malformed_fields() {
    assert_eq!(
        io_numbers("8:0 rbytes=10 wbytes=0\n")
            .unwrap()
            .get("8:0/wbytes"),
        Some(&0)
    );
    assert!(io_numbers("8:0 rbytes=1 rbytes=2\n").is_err());
    assert!(io_numbers("8:x rbytes=1\n").is_err());
    assert!(io_numbers("8:0 rbytes=max\n").is_err());
    assert!(io_numbers("").unwrap().is_empty());
}

#[test]
fn read_refuses_symlink_and_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("file"), "0").unwrap();
    std::os::unix::fs::symlink(dir.path().join("file"), dir.path().join("link")).unwrap();
    assert!(read(&dir.path().join("link")).is_err());
    fs::write(dir.path().join("huge"), vec![b'0'; 65_537]).unwrap();
    assert!(read(&dir.path().join("huge")).is_err());
    assert!(sample_process(0).is_err());
}

fn checkpoint() -> RunCheckpoint {
    let model_id = "generic-owned-fixture".to_owned();
    let container = crate::ownership::ContainerOwnership {
        container_id: "a".repeat(64),
        run_id: "b".repeat(64),
        image: format!("registry.example.invalid/runtime@sha256:{}", "c".repeat(64)),
        model_id_sha256: hex::encode(Sha256::digest(model_id.as_bytes())),
        arguments_sha256: "d".repeat(64),
    };
    RunCheckpoint {
        schema: 1,
        phase: "ready".into(),
        model_id,
        model_sha256: "e".repeat(64),
        runtime_id: Some("signed-fixture-runtime".into()),
        process: ProcessHandle {
            id: container.unit(),
            container: Some(container),
        },
        sample: Observed::unavailable("fixture supervisor deliberately has no numeric observation"),
    }
}

#[test]
fn receipts_are_exclusive_bound_and_private_and_survive_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let record = checkpoint();
    retain(root.path(), &record).unwrap();
    assert!(retain(root.path(), &record).is_err());
    let Observed::Actual { value } = retained(root.path()) else {
        panic!("receipt unavailable");
    };
    assert_eq!(value.as_slice(), std::slice::from_ref(&record));
    let directory = root.path().join("run-observations");
    let mut foreign = record.clone();
    foreign.model_id = "different-model".into();
    assert!(retain(root.path(), &foreign).is_err());
    let file = directory.join(validate_checkpoint(&record).unwrap());
    fs::rename(&file, directory.join("foreign.json")).unwrap();
    assert!(matches!(
        retained(root.path()),
        Observed::Unavailable { .. }
    ));
    fs::remove_file(directory.join("foreign.json")).unwrap();
    fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    assert!(retain(root.path(), &record).is_err());
}

#[test]
fn mount_resolution_respects_visible_root_and_refuses_ambiguity_or_escape() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("leaf")).unwrap();
    let mount = format!(
        "1 0 0:1 /visible {} rw - cgroup2 cgroup rw\n",
        dir.path().display()
    );
    assert_eq!(
        cgroup_path("0::/visible/leaf\n", &mount).unwrap(),
        dir.path().join("leaf")
    );
    assert!(cgroup_path("0::/outside/leaf\n", &mount).is_err());
    assert!(cgroup_path("0::/visible/../escape\n", &mount).is_err());
    assert!(cgroup_path("0::/visible/leaf\n", &format!("{mount}{mount}")).is_err());
    assert!(cgroup_path("0::/visible/leaf\n0::/other\n", &mount).is_err());
}
