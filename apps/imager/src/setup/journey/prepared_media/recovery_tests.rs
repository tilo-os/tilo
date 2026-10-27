use super::*;

#[test]
#[ignore = "needs the owned-GPT native harness as root (os/scripts/installer-source-fixture/native_journey_tests.py)"]
fn prepared_stage_rechecks_recovery_custody_at_actual_write_admission() {
    if !native_retained::enter_owned_journey(
        "setup::journey::prepared_media_native::recovery_tests::prepared_stage_rechecks_recovery_custody_at_actual_write_admission",
    ) {
        return;
    }
    crate::setup::test_support::isolated("recovery-prepared-write-admission", || {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        // Both trees live on the owned GPT filesystem, not the builder's tmpfs.
        // Distinct sibling paths must not imply independent physical recovery.
        let temp = native_retained::journey_directory();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("save"), b"unique local save").unwrap();
        let inventory = crate::setup::windows_inventory::Inventory::discover(&[
            crate::setup::windows_inventory::SelectedRoot {
                path: source.clone(),
                role: crate::setup::windows_inventory::RootRole::KnownFolder,
            },
        ])
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let records = temp.path().join("records");
        std::fs::create_dir(&records).unwrap();
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut recovery = crate::setup::migration_recovery::Flow::default();
        recovery
            .review(
                &inventory,
                &inventory.roots[0].source_id,
                &destination,
                &records.join("journal"),
            )
            .unwrap();
        recovery.start(false).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        while recovery.busy() {
            recovery.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let evidence = recovery.evidence().unwrap();
        assert_eq!(evidence.len(), 1);
        use std::os::unix::fs::MetadataExt;
        assert!(!destination.starts_with(&source));
        assert!(!source.starts_with(&destination));
        assert_eq!(
            std::fs::metadata(&source).unwrap().dev(),
            std::fs::metadata(&destination).unwrap().dev(),
            "Sibling recovery trees are on the same owned GPT filesystem"
        );
        assert_eq!(recovery.report()["reviewed"]["independent_storage"], false);
        let identity = PreparedIdentity::create(
            &temp.path().join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        let prepared = PreparedMedia::prepare_for_install(
            &identity,
            None,
            None,
            Zeroizing::new("a sufficiently long installation password".into()),
            "recovery-admission".into(),
            None,
        )
        .unwrap();
        assert!(prepared.staging_artifact().is_ok());
        let mut storage = crate::setup::journey::storage::Storage::default();
        storage.stage();
        let mut plan = storage.plan.as_ref().unwrap().as_ref().unwrap().clone();
        let device = std::fs::metadata(&source).unwrap().dev();
        let partition = std::fs::canonicalize(format!(
            "/sys/dev/block/{}:{}",
            libc::major(device),
            libc::minor(device)
        ))
        .unwrap();
        assert!(partition.join("partition").is_file());
        let disk = partition.parent().unwrap().file_name().unwrap();
        plan.basis.disk.id = std::path::Path::new("/dev")
            .join(disk)
            .to_string_lossy()
            .into_owned();
        let target = std::fs::metadata(&plan.basis.disk.id).unwrap().rdev();
        assert_eq!(
            std::fs::canonicalize(format!(
                "/sys/dev/block/{}:{}",
                libc::major(target),
                libc::minor(target)
            ))
            .unwrap(),
            partition.parent().unwrap(),
            "Recovery fixture must address the actual owned GPT disk"
        );
        let plan = &plan;
        // A completed journal does not prove that these filesystem paths are
        // outside the raw target. The production binding path refuses it.
        assert!(prepared.bind_recovery(evidence.clone(), plan).is_err());
        // Exercise the write gate itself against a stale/unsafe bound witness,
        // not merely the selection-time check. Such a witness cannot yield a
        // staging artifact or pass the media writer's final admission.
        *prepared.recovery.lock().unwrap() =
            evidence.into_iter().map(|e| (e, plan.clone())).collect();
        assert!(prepared.staging_artifact().is_err());
        assert!(
            prepared
                .validate_for_write(&AtomicBool::new(false))
                .is_err()
        );
        assert!(
            prepared
                .validate_recovery_for_stage(plan, &AtomicBool::new(false))
                .is_err()
        );
    });
}
