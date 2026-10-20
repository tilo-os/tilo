use super::super::{InstallerPreview, InstallerPreviewEffect as E, Page};
use crate::setup::{
    journey::belongings::Belongings,
    windows_inventory::{RootRole, SelectedRoot},
};
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

fn wait(preview: &mut InstallerPreview) {
    let deadline = tilo_test_env::hang_deadline();
    while preview.recovery.busy() {
        preview.recovery.poll();
        assert!(Instant::now() < deadline, "Recovery worker did not finish");
        std::thread::yield_now();
    }
}
fn make_preview(source: &Path) -> InstallerPreview {
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.page = Page::Storage;
    preview.belongings = Belongings::from_windows_roots(&[SelectedRoot {
        path: source.into(),
        role: RootRole::KnownFolder,
    }])
    .unwrap();
    preview.storage.stage();
    preview.storage.set_application_minimum(600 << 30);
    preview
}
fn review(preview: &mut InstallerPreview, directory: &Path) {
    let source_id = preview.belongings.windows.as_ref().unwrap().roots[0]
        .source_id
        .clone();
    preview
        .apply(E::SelectRecoveryDirectory {
            source_id,
            directory: directory.to_string_lossy().into(),
        })
        .unwrap();
}

#[test]
fn semantic_shortage_destination_verified_copy_never_waives_review_or_source_custody() {
    crate::setup::test_support::isolated("semantic-recovery", || {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("external-folder");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("save"), b"irreplaceable save").unwrap();
        let mut preview = make_preview(&source);
        // The Windows belongings root does not use Linux page navigation. Its
        // recovery actions must still be published and routed to this flow.
        preview.windows.enabled = true;
        preview.journey.page = Page::Destination;
        assert!(preview.effect_page_admitted("select-recovery-directory"));
        assert!(preview.effect_page_admitted("retry-recovery-copy"));
        assert!(preview.storage.admit().is_err());
        let source_id = preview.belongings.windows.as_ref().unwrap().roots[0]
            .source_id
            .clone();
        assert!(
            preview
                .apply(E::SelectRecoveryDirectory {
                    source_id,
                    directory: source.to_string_lossy().into()
                })
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(&source).unwrap().count(),
            1,
            "Refused destination selection must not create journal state inside the source"
        );
        review(&mut preview, &destination);
        assert!(preview.reviewed_belongings().is_err());
        preview.apply(E::StartRecoveryCopy).unwrap();
        wait(&mut preview);
        assert_eq!(preview.recovery.report()["state"], "verified");
        let output = preview.recovery.report()["reviewed"]["output"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(Path::new(&output).starts_with(&destination));
        assert_eq!(
            std::fs::read(Path::new(&output).join("save")).unwrap(),
            b"irreplaceable save"
        );
        assert_eq!(
            preview.recovery.report()["reviewed"]["independent_storage"],
            false
        );
        assert!(
            preview.storage.admit().is_err(),
            "Verified copying must not waive the actual capacity planner"
        );
        let evidence = preview.recovery.evidence().unwrap();
        evidence[0].validate(&AtomicBool::new(false)).unwrap();
        // Unknown write-range mappings are refused by the same guard bound into
        // PreparedMedia, not upgraded into destructive authority by byte budget.
        preview.storage.set_application_minimum(0);
        let plan = preview.storage.plan.as_ref().unwrap().as_ref().unwrap();
        assert!(evidence[0].validate_plan(plan).is_err());
        std::fs::write(source.join("save"), b"changed authoritative source").unwrap();
        assert!(evidence[0].validate(&AtomicBool::new(false)).is_err());
        assert_eq!(
            std::fs::read(source.join("save")).unwrap(),
            b"changed authoritative source"
        );
    });
}

#[test]
fn semantic_cancel_retry_rechecks_hashes_and_keeps_source() {
    crate::setup::test_support::isolated("semantic-recovery-cancel", || {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("external-folder");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let file = std::fs::File::create(source.join("large-save")).unwrap();
        file.set_len(32 << 20).unwrap();
        drop(file);
        let mut preview = make_preview(&source);
        review(&mut preview, &destination);
        preview.apply(E::StartRecoveryCopy).unwrap();
        preview.apply(E::CancelRecoveryCopy).unwrap();
        wait(&mut preview);
        assert_ne!(preview.recovery.report()["state"], "verified");
        assert!(preview.recovery.evidence().is_err());
        preview.apply(E::RetryRecoveryCopy).unwrap();
        wait(&mut preview);
        assert_eq!(
            preview.recovery.report()["state"],
            "verified",
            "{}",
            preview.recovery.report()
        );
        assert_eq!(
            std::fs::metadata(source.join("large-save")).unwrap().len(),
            32 << 20
        );
        let output = preview.recovery.report()["reviewed"]["output"]
            .as_str()
            .unwrap()
            .to_owned();
        let journal = preview.recovery.report()["reviewed"]["journal"]
            .as_str()
            .unwrap()
            .to_owned();
        // Restart has no in-memory success authority: regrant the exact source,
        // output and journal, then explicitly retry the production verifier.
        preview = make_preview(&source);
        let source_id = preview.belongings.windows.as_ref().unwrap().roots[0]
            .source_id
            .clone();
        preview
            .apply(E::ReviewExistingRecoveryCopy {
                source_id,
                output: output.clone(),
                journal,
            })
            .unwrap();
        assert!(preview.recovery.evidence().is_err());
        preview.apply(E::RetryRecoveryCopy).unwrap();
        wait(&mut preview);
        assert_eq!(preview.recovery.report()["state"], "verified");
        std::fs::write(Path::new(&output).join("large-save"), b"corrupted").unwrap();
        preview.apply(E::RetryRecoveryCopy).unwrap();
        wait(&mut preview);
        assert_ne!(preview.recovery.report()["state"], "verified");
        assert!(preview.recovery.evidence().is_err());
        assert_eq!(
            std::fs::metadata(source.join("large-save")).unwrap().len(),
            32 << 20
        );
    });
}
