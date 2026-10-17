use super::*;

#[test]
fn review_navigation_keeps_the_journey_and_writer_in_sync() {
    let navigation = Navigation {
        artifact: ui::LocalState::new(None),
        showing_media: ui::LocalState::new(true),
        reason: ui::LocalState::new(None),
    };
    navigation.review_home(ReturnReason::ReviewedHomeChanged);
    assert!(!navigation.showing_media.get());
    assert_eq!(
        navigation.reason.get(),
        Some(ReturnReason::ReviewedHomeChanged)
    );
    assert!(navigation.artifact.get().is_none());
}

#[test]
fn source_review_is_presentation_state_even_without_a_native_actor() {
    let mut controller = Controller {
        native: Actor::default(),
        review: None,
    };
    controller.require_source_review(ReturnReason::ReviewedHomeUnavailable);
    assert!(controller.needs_source_review());
    assert_eq!(
        controller.review_reason(),
        Some(ReturnReason::ReviewedHomeUnavailable)
    );
    assert_eq!(controller.projection()["state"], "source-unavailable");
    assert!(!controller.busy());
    assert!(controller.poll().is_none());
    assert!(controller.cancel().is_err());
}

#[test]
fn absent_delivery_actor_never_reports_native_completion() {
    let mut delivery = Delivery {
        native: Actor::default(),
    };
    assert!(delivery.start_automatic().is_err());
    assert!(!delivery.pending());
    assert!(!delivery.busy());
    assert!(!delivery.poll());
    assert_eq!(delivery.projection()["complete"], false);
    assert!(delivery.projection()["receipt"].is_null());
    assert!(
        delivery
            .start(std::path::PathBuf::from("unavailable-medium"))
            .is_err()
    );
    assert!(delivery.cancel().is_err());
    assert!(!delivery.awaiting_original_boot());
    assert!(delivery.original_boot_drive().is_none());
    assert!(delivery.artifact().is_none());
    assert!(!delivery.requires_data_partition());
    assert!(!delivery.requires_offline_export());
    assert!(delivery.finish_close().is_ok());
}

#[test]
fn ordinary_transfer_failure_does_not_request_a_source_review() {
    for kind in [
        std::io::ErrorKind::Interrupted,
        std::io::ErrorKind::Unsupported,
        std::io::ErrorKind::PermissionDenied,
    ] {
        assert_eq!(source_review_reason(&std::io::Error::from(kind)), None);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn write_refresh_preserves_native_source_review_refusals() {
    use std::os::unix::fs::PermissionsExt;
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let prepared = home_media_fixture(&root);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let refreshed = refresh_for_write(&prepared, &cancel).unwrap();
    assert!(Arc::ptr_eq(&prepared, &refreshed));

    std::fs::write(
        root.join("original-home/document"),
        b"changed after source review",
    )
    .unwrap();
    let error = refresh_for_write(&prepared, &cancel).err().unwrap();
    assert_eq!(
        source_review_reason(&error),
        Some(ReturnReason::ReviewedHomeChanged)
    );
}
