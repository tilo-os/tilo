use super::*;
use crate::{
    home_transfer,
    migration_worker::{WorkerEvent, WorkerOutcome},
};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt, symlink};
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for path in [
        "original",
        "quarantine-parent",
        "target/.config/tilo",
        "target/.local/state/tilo",
        "journals",
    ] {
        std::fs::create_dir_all(root.path().join(path)).unwrap();
    }
    for path in [
        "quarantine-parent",
        "target",
        "journals",
        "target/.config/tilo",
        "target/.local/state/tilo",
    ] {
        std::fs::set_permissions(
            root.path().join(path),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    root
}
fn copy(root: &Path) -> PathBuf {
    let destination = root.join("quarantine-parent/home");
    let review = home_transfer::review(
        &root.join("original"),
        &destination,
        &root.join("journals/copy"),
    )
    .unwrap();
    let mut worker = review.start().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        assert!(std::time::Instant::now() < deadline);
        match worker.try_next() {
            Some(WorkerEvent::Finished(WorkerOutcome::Complete(_))) => break,
            Some(WorkerEvent::Finished(other)) => panic!("{other:?}"),
            _ => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    destination
}
#[test]
fn real_copied_home_adopts_files_modes_links_and_keeps_selected_identity_and_collisions() {
    let root = fixture();
    let original = root.path().join("original");
    let target = root.path().join("target");
    std::fs::create_dir_all(original.join(".config/tilo/identity")).unwrap();
    std::fs::write(
        original.join(".config/tilo/identity/primary.json"),
        b"old custody",
    )
    .unwrap();
    std::fs::write(
        target.join(".config/tilo/primary.json"),
        b"selected custody",
    )
    .unwrap();
    std::fs::write(original.join("keep"), b"old document").unwrap();
    std::fs::write(target.join("keep"), b"new document").unwrap();
    std::fs::write(original.join("launch"), b"executable").unwrap();
    std::fs::set_permissions(
        original.join("launch"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    std::fs::hard_link(original.join("launch"), original.join("alias")).unwrap();
    symlink("launch", original.join("link")).unwrap();
    let quarantine = copy(root.path());
    let journal = root.path().join("journals/adoption");
    let review = PromotionReview::review(&quarantine, &target, &journal).unwrap();
    let report = review.run(false, &AtomicBool::new(false)).unwrap();
    assert!(report.verified);
    assert!(!report.complete());
    assert!(report.collisions.contains(&PathBuf::from("keep")));
    assert!(
        report
            .inactive_custody
            .contains(&PathBuf::from(".config/tilo/identity/primary.json"))
    );
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"new document");
    assert_eq!(
        std::fs::read(quarantine.join("keep")).unwrap(),
        b"old document"
    );
    assert_eq!(
        std::fs::read(target.join(".config/tilo/primary.json")).unwrap(),
        b"selected custody"
    );
    assert_eq!(
        std::fs::read(quarantine.join(".config/tilo/identity/primary.json")).unwrap(),
        b"old custody"
    );
    assert_eq!(
        std::fs::metadata(target.join("launch"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        std::fs::metadata(target.join("launch")).unwrap().ino(),
        std::fs::metadata(target.join("alias")).unwrap().ino()
    );
    assert_eq!(
        std::fs::read_link(target.join("link")).unwrap(),
        Path::new("launch")
    );
    assert_eq!(
        std::fs::read(original.join("launch")).unwrap(),
        b"executable"
    );
    let recovered = PromotionReview::recover(&quarantine, &target, &journal).unwrap();
    let retried = recovered.run(true, &AtomicBool::new(false)).unwrap();
    assert!(retried.verified);
    assert_eq!(retried.installed, report.installed);
}
#[test]
fn appeared_target_and_replaced_parent_are_never_overwritten_or_followed() {
    let root = fixture();
    std::fs::write(root.path().join("original/new"), b"source").unwrap();
    let quarantine = copy(root.path());
    let target = root.path().join("target");
    let review =
        PromotionReview::review(&quarantine, &target, &root.path().join("journals/adoption"))
            .unwrap();
    std::fs::write(target.join("new"), b"appeared").unwrap();
    let report = review.run(false, &AtomicBool::new(false)).unwrap();
    assert!(!report.complete());
    assert!(!report.failed.is_empty());
    assert_eq!(std::fs::read(target.join("new")).unwrap(), b"appeared");
    assert_eq!(std::fs::read(quarantine.join("new")).unwrap(), b"source");
    std::fs::rename(target.join(".config"), target.join("saved-config")).unwrap();
    symlink(root.path().join("original"), target.join(".config")).unwrap();
    assert!(review.run(true, &AtomicBool::new(false)).is_err());
    assert_eq!(
        std::fs::read(root.path().join("original/new")).unwrap(),
        b"source"
    );
}
#[test]
fn recovery_proves_post_rename_receipt_and_refuses_missing_target_or_extra_quarantine_data() {
    let root = fixture();
    std::fs::write(root.path().join("original/document"), b"content").unwrap();
    let quarantine = copy(root.path());
    let target = root.path().join("target");
    let journal = root.path().join("journals/adoption");
    let review = PromotionReview::review(&quarantine, &target, &journal).unwrap();
    let (_q, _h, parent) = roots(&quarantine, &target, &journal).unwrap();
    let log = Log::create(&parent, journal.file_name().unwrap(), review.plan).unwrap();
    drop(log);
    std::fs::rename(quarantine.join("document"), target.join("document")).unwrap();
    let recovered = PromotionReview::recover(&quarantine, &target, &journal).unwrap();
    let report = recovered.run(true, &AtomicBool::new(false)).unwrap();
    assert!(report.complete());
    std::fs::write(quarantine.join("unexpected"), b"not reviewed").unwrap();
    let report = recovered.run(true, &AtomicBool::new(false)).unwrap();
    assert!(!report.complete());
    assert!(!report.failed.is_empty());
    std::fs::remove_file(target.join("document")).unwrap();
    let report = recovered.run(true, &AtomicBool::new(false)).unwrap();
    assert!(!report.complete());
    assert!(!report.failed.is_empty());
}

#[test]
fn interrupted_readonly_merge_parent_is_restored_before_receipt_replay() {
    let root = fixture();
    let original = root.path().join("original");
    std::fs::create_dir(original.join(".config")).unwrap();
    std::fs::write(original.join(".config/old-settings"), b"settings").unwrap();
    std::fs::set_permissions(
        original.join(".config"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let quarantine = copy(root.path());
    let target = root.path().join("target");
    let journal = root.path().join("journals/adoption");
    let review = PromotionReview::review(&quarantine, &target, &journal).unwrap();
    let (_q, _h, journal_parent) = roots(&quarantine, &target, &journal).unwrap();
    let mut log = Log::create(
        &journal_parent,
        journal.file_name().unwrap(),
        review.plan.clone(),
    )
    .unwrap();
    let merged = review
        .plan
        .entries
        .iter()
        .map(Result::unwrap)
        .position(|entry| matches!(entry.kind, Kind::MergedDirectory { .. }))
        .unwrap();
    log.checkpoint(merged).unwrap();
    drop(log);
    std::fs::set_permissions(
        quarantine.join(".config"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::rename(
        quarantine.join(".config/old-settings"),
        target.join(".config/old-settings"),
    )
    .unwrap();
    let recovered = PromotionReview::recover(&quarantine, &target, &journal).unwrap();
    assert!(
        recovered
            .run(true, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    assert_eq!(
        std::fs::metadata(quarantine.join(".config"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        std::fs::read(target.join(".config/old-settings")).unwrap(),
        b"settings"
    );
    assert_eq!(
        std::fs::read(original.join(".config/old-settings")).unwrap(),
        b"settings"
    );
    assert_eq!(
        std::fs::metadata(original.join(".config"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    // Leave temporary fixture roots removable after asserting preserved modes.
    for path in [original.join(".config"), quarantine.join(".config")] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn recovery_journal_cannot_bless_new_files_outside_completed_home_receipts() {
    let root = fixture();
    let source = root.path().join("original");
    std::fs::write(source.join("document"), b"original document").unwrap();
    let quarantine = copy(root.path());
    let completed = home_transfer::HomeReview::recover(
        &source,
        &quarantine,
        &root.path().join("journals/copy"),
    )
    .unwrap();
    std::fs::write(quarantine.join("unreviewed"), b"added after copy").unwrap();
    let journal = root.path().join("journals/adoption");
    let promotion =
        PromotionReview::review(&quarantine, &root.path().join("target"), &journal).unwrap();
    promotion.move_one_for_test(Path::new("document")).unwrap();
    let renewed =
        PromotionReview::recover(&quarantine, &root.path().join("target"), &journal).unwrap();
    assert!(
        completed
            .verify_adoption(&renewed.witness().unwrap())
            .is_err()
    );
    assert!(
        completed
            .verify_adoption_after_applications(&renewed.witness().unwrap())
            .is_err()
    );
    assert_eq!(
        std::fs::read(source.join("document")).unwrap(),
        b"original document"
    );
    assert_eq!(
        std::fs::read(quarantine.join("unreviewed")).unwrap(),
        b"added after copy"
    );
}

#[test]
fn mapped_application_root_refuses_a_split_existing_repository_collision() {
    let root = fixture();
    let relative = Path::new(".local/share/flatpak/repo");
    let source_repo = root.path().join("original").join(relative);
    let target_repo = root.path().join("target").join(relative);
    std::fs::create_dir_all(&source_repo).unwrap();
    std::fs::create_dir_all(&target_repo).unwrap();
    std::fs::write(source_repo.join("keep"), b"original repository object").unwrap();
    std::fs::write(source_repo.join("new"), b"another repository object").unwrap();
    std::fs::write(target_repo.join("keep"), b"existing target object").unwrap();
    let quarantine = copy(root.path());
    let journal = root.path().join("journals/adoption");
    let promotion =
        PromotionReview::review(&quarantine, &root.path().join("target"), &journal).unwrap();
    promotion.move_one_for_test(&relative.join("new")).unwrap();
    let witness = promotion.witness().unwrap();
    assert!(witness.application_home().is_err());
    assert!(
        witness
            .uniform_target(Path::new(".local/share/flatpak"))
            .is_err()
    );
    assert_eq!(
        std::fs::read(target_repo.join("keep")).unwrap(),
        b"existing target object"
    );
    assert_eq!(
        std::fs::read(quarantine.join(relative).join("keep")).unwrap(),
        b"original repository object"
    );
}

#[test]
fn completed_application_witness_defers_only_repository_digest_and_keeps_move_identity() {
    let root = fixture();
    let original = root.path().join("original");
    let target = root.path().join("target");
    std::fs::create_dir_all(original.join(".local/share/flatpak/repo")).unwrap();
    std::fs::write(
        original.join(".local/share/flatpak/repo/ref"),
        b"original ref",
    )
    .unwrap();
    std::fs::write(original.join("document"), b"original document").unwrap();
    std::fs::write(original.join(".local/personal"), b"personal original").unwrap();
    let quarantine = copy(root.path());
    let journal = root.path().join("journals/application-witness");
    let review = PromotionReview::review(&quarantine, &target, &journal).unwrap();
    assert!(
        review
            .run(false, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    review.verify_completed().unwrap();
    std::fs::write(
        target.join(".local/share/flatpak/repo/ref"),
        b"approved output",
    )
    .unwrap();
    assert!(review.verify_completed().is_err());
    let recovered = PromotionReview::recover(&quarantine, &target, &journal).unwrap();
    let witness = recovered.completed_application_witness().unwrap();
    assert_eq!(witness.application_home().unwrap(), target);
    std::fs::write(
        target.join(".local/share/flatpak/repo/new-approved-output"),
        b"independently verified output",
    )
    .unwrap();
    recovered.completed_application_witness().unwrap();
    std::fs::write(
        target.join(".local/personal"),
        b"unapproved personal change",
    )
    .unwrap();
    assert!(recovered.completed_application_witness().is_err());
    std::fs::write(target.join(".local/personal"), b"personal original").unwrap();
    recovered.completed_application_witness().unwrap();

    // This structural witness deliberately leaves repository bytes to the
    // independently authenticated original/archive and signed-output verifier.
    std::fs::write(target.join("document"), b"unapproved change").unwrap();
    assert!(recovered.completed_application_witness().is_err());
    std::fs::write(target.join("document"), b"original document").unwrap();
    recovered.completed_application_witness().unwrap();
    let installation = target.join(".local/share/flatpak");
    let mode = std::fs::metadata(&installation)
        .unwrap()
        .permissions()
        .mode();
    std::fs::set_permissions(&installation, std::fs::Permissions::from_mode(mode ^ 0o100)).unwrap();
    assert!(recovered.completed_application_witness().is_err());
    std::fs::set_permissions(&installation, std::fs::Permissions::from_mode(mode)).unwrap();
    recovered.completed_application_witness().unwrap();
    let retained_installation = installation.with_extension("original-held");
    std::fs::rename(&installation, &retained_installation).unwrap();
    std::fs::create_dir(&installation).unwrap();
    std::fs::set_permissions(&installation, std::fs::Permissions::from_mode(mode)).unwrap();
    assert!(recovered.completed_application_witness().is_err());
    std::fs::remove_dir(&installation).unwrap();
    std::fs::rename(&retained_installation, &installation).unwrap();
    recovered.completed_application_witness().unwrap();
    crate::portable_transfer::acl::set_test_acl(
        &installation,
        true,
        b"user::rwx\ngroup::r-x\nother::---",
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&installation)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        mode & 0o777,
        "default ACL change must leave access mode unchanged"
    );
    assert!(recovered.completed_application_witness().is_err());

    assert_eq!(
        std::fs::read(original.join("document")).unwrap(),
        b"original document"
    );
    assert_eq!(
        std::fs::read(original.join(".local/share/flatpak/repo/ref")).unwrap(),
        b"original ref"
    );
}

#[test]
fn imported_startup_consent_survives_partial_adoption_and_keeps_target_collisions() {
    for allowed in [false, true] {
        let root = fixture();
        let original = root.path().join("original");
        let target = root.path().join("target");
        for home in [&original, &target] {
            std::fs::create_dir_all(home.join(".config/autostart")).unwrap();
        }
        let imported = Path::new(".config/autostart/imported.desktop");
        let existing = Path::new(".config/autostart/existing.desktop");
        std::fs::write(
            original.join(imported),
            b"[Desktop Entry]\nType=Application\nName=Imported\nExec=imported\n",
        )
        .unwrap();
        std::fs::write(original.join(existing), b"old collision").unwrap();
        std::fs::write(target.join(existing), b"existing target startup").unwrap();
        let quarantine = copy(root.path());
        let journal = root.path().join("journals/adoption");
        let review = PromotionReview::review(&quarantine, &target, &journal)
            .unwrap()
            .with_application_consent(allowed)
            .unwrap();
        review.move_one_for_test(imported).unwrap();
        // Protection is already durable when the first imported inode appears.
        let paths = [target.join(imported), target.join(existing)];
        let permitted = desktop_entry::migration_startup::filter(&target, &paths).unwrap();
        assert_eq!(permitted.contains(&paths[0]), allowed);
        assert!(permitted.contains(&paths[1]));
        let recovered = PromotionReview::recover(&quarantine, &target, &journal)
            .unwrap()
            .with_application_consent(allowed)
            .unwrap();
        let report = recovered.run(true, &AtomicBool::new(false)).unwrap();
        assert!(report.verified && report.failed.is_empty() && !report.cancelled);
        assert_eq!(report.collisions, vec![existing.to_path_buf()]);
        assert!(
            !report.complete(),
            "retained collisions require truthful partial status"
        );
        assert_eq!(
            std::fs::read(original.join(imported)).unwrap(),
            std::fs::read(target.join(imported)).unwrap()
        );
        assert_eq!(
            std::fs::read(target.join(existing)).unwrap(),
            b"existing target startup"
        );
        assert_eq!(
            std::fs::read(quarantine.join(existing)).unwrap(),
            b"old collision"
        );
        assert_eq!(
            desktop_entry::migration_startup::filter(&target, &paths).unwrap(),
            permitted
        );
        assert!(
            PromotionReview::recover(&quarantine, &target, &journal)
                .unwrap()
                .with_application_consent(!allowed)
                .is_err()
        );
        std::fs::remove_file(target.join(desktop_entry::migration_startup::RECORD_PATH)).unwrap();
        assert!(desktop_entry::migration_startup::filter(&target, &paths).is_err());
    }
}

#[test]
fn old_adoption_without_structural_evidence_refuses_application_deferral() {
    let root = fixture();
    let original = root.path().join("original");
    let target = root.path().join("target");
    std::fs::create_dir_all(original.join(".local/share/flatpak/repo")).unwrap();
    std::fs::write(
        original.join(".local/share/flatpak/repo/ref"),
        b"original ref",
    )
    .unwrap();
    let quarantine = copy(root.path());
    let journal = root.path().join("journals/old-application-witness");
    let review = PromotionReview::review(&quarantine, &target, &journal).unwrap();
    assert!(
        review
            .run(false, &AtomicBool::new(false))
            .unwrap()
            .complete()
    );
    let bytes = std::fs::read(&journal).unwrap();
    let split = bytes.iter().position(|byte| *byte == b'\n').unwrap();
    let mut plan: serde_json::Value = serde_json::from_slice(&bytes[..split]).unwrap();
    for entry in plan["entries"].as_array_mut().unwrap() {
        entry.as_object_mut().unwrap().remove("structure");
    }
    let mut old = serde_json::to_vec(&plan).unwrap();
    old.push(b'\n');
    old.extend(&bytes[split + 1..]);
    std::fs::write(&journal, old).unwrap();
    let recovered = PromotionReview::recover(&quarantine, &target, &journal).unwrap();
    recovered.verify_completed().unwrap();
    assert!(recovered.completed_application_witness().is_err());
    let mut incomplete: serde_json::Value = serde_json::from_slice(&bytes[..split]).unwrap();
    let proof = incomplete["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find_map(|entry| {
            entry
                .get_mut("structure")
                .and_then(serde_json::Value::as_array_mut)
        })
        .unwrap();
    assert!(proof.len() > 1);
    proof.pop();
    let mut truncated = serde_json::to_vec(&incomplete).unwrap();
    truncated.push(b'\n');
    truncated.extend(&bytes[split + 1..]);
    std::fs::write(&journal, truncated).unwrap();
    assert!(PromotionReview::recover(&quarantine, &target, &journal).is_err());
}
