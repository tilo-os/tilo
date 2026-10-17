//! App transfer recovery across a deliberately interrupted backend adoption.
use super::promotion::PromotionEvent;
use std::{os::unix::fs::PermissionsExt, path::Path};

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
#[test]
fn cross_process_partial_adoption_renews_completed_copy_and_move_witnesses() {
    use crate::setup::{
        flatpak_transfer::Runner,
        user_transfer::{self, TransferEvent, TransferRequest},
    };
    struct NoApps;
    impl Runner for NoApps {
        fn run(&self, _: &Path, _: &[String]) -> Result<String, String> {
            panic!("Home-only adoption must not run applications")
        }
    }
    let root = fixture();
    let source = root.path().join("original");
    let quarantine = root.path().join("quarantine-parent/home");
    let target = root.path().join("target");
    let journal = root.path().join("journals/adoption");
    std::fs::write(source.join("first"), b"first document").unwrap();
    std::fs::write(source.join("second"), b"second document").unwrap();
    std::fs::create_dir_all(source.join(".config/tilo")).unwrap();
    std::fs::write(source.join(".config/tilo/primary.json"), b"old custody").unwrap();
    std::fs::write(
        target.join(".config/tilo/primary.json"),
        b"selected custody",
    )
    .unwrap();
    let request = TransferRequest {
        source_home: source.clone(),
        target_home: quarantine.clone(),
        private_journals: root.path().join("journals"),
        copy_home: true,
        copy_flatpak: false,
        system_refs: Vec::new(),
    };
    let transfer = user_transfer::review(&NoApps, request.clone()).unwrap();
    let mut worker = transfer.start(false).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        assert!(std::time::Instant::now() < deadline);
        if let Some(TransferEvent::Finished(result)) = worker.try_next() {
            assert!(result.unwrap().complete());
            worker.join_finished().unwrap();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let promotion = transfer.review_promotion(&target, &journal).unwrap();
    promotion
        .move_one_without_checkpoint_for_test(Path::new("first"))
        .unwrap();
    drop(promotion);
    drop(transfer);
    assert!(!quarantine.join("first").exists());
    assert!(quarantine.join("second").exists());
    let (_, resumed) =
        user_transfer::recover_promotion(request.clone(), &target, &journal).unwrap();
    let mut worker = resumed.start(true).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let report = loop {
        assert!(std::time::Instant::now() < deadline);
        match worker.try_next() {
            Some(PromotionEvent::Finished(report)) => break report,
            Some(PromotionEvent::Failed(error)) => panic!("{error}"),
            None => std::thread::yield_now(),
        }
    };
    assert!(report.complete(), "{report:?}");
    assert_eq!(
        std::fs::read(target.join("second")).unwrap(),
        b"second document"
    );
    assert_eq!(
        std::fs::read(source.join("first")).unwrap(),
        b"first document"
    );
    assert_eq!(
        std::fs::read(target.join(".config/tilo/primary.json")).unwrap(),
        b"selected custody"
    );
    assert_eq!(
        std::fs::read(quarantine.join(".config/tilo/primary.json")).unwrap(),
        b"old custody"
    );
    std::fs::write(target.join("first"), b"changed after adoption").unwrap();
    assert!(user_transfer::recover_promotion(request.clone(), &target, &journal).is_err());
    std::fs::write(target.join("first"), b"first document").unwrap();
    std::fs::write(source.join("second"), b"changed original").unwrap();
    assert!(user_transfer::recover_promotion(request, &target, &journal).is_err());
}
