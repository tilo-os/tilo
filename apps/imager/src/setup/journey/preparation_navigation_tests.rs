//! The visible stop action must reach the held preparation worker, not reset choices.
use super::*;
use crate::resource::GuidedInstallerShell;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use ui::framework::testing::TestApp;

pub(super) struct WorkerControl {
    pub probe: Sender<()>,
    pub cancelled: Receiver<std::io::ErrorKind>,
    pub finish: Sender<()>,
}

fn status(app: &mut TestApp<GuidedInstallerShell>) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}

fn wait_for_cancelled(app: &mut TestApp<GuidedInstallerShell>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.settle();
        if status(app)["media_preparation"]["state"] == "cancelled" {
            break;
        }
        assert!(Instant::now() < deadline, "worker did not retire");
        std::thread::yield_now();
    }
}

#[test]
fn cancel_on_each_preparation_page_waits_for_worker_cleanup_and_keeps_choices() {
    for (page, destination) in [
        (Page::Devices, Destination::Computer),
        (Page::Belongings, Destination::Computer),
        (Page::Accounts, Destination::Computer),
        (Page::Review, Destination::Computer),
        (Page::Storage, Destination::Computer),
        (Page::Review, Destination::Desktop),
        (Page::Belongings, Destination::Another),
    ] {
        let private = tempfile::tempdir().unwrap();
        let scratch = private.path().join("preparation-partial");
        std::fs::write(&scratch, b"private unfinished preparation").unwrap();
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        let worker = app
            .with_widget::<InstallerPreview, _>(|view| {
                view.live = true;
                view.restart = restart_controller::Restart::default();
                view.journey.page = page;
                view.journey.destination = Some(destination);
                view.journey.migrate = Some(true);
                view.journey.name = "Ada".into();
                view.journey.device_name = "Ada's computer".into();
                view.journey.categories = [true, false, true];
                view.direct.belongings_pending = true;
                view.direct.belongings_continue = true;
                view.dirty = true;
                view.media_preparation
                    .native
                    .get_mut()
                    .unwrap()
                    .as_any_mut()
                    .downcast_mut::<prepared_media_native::Controller>()
                    .unwrap()
                    .start_cancellation_fixture(scratch.clone())
            })
            .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|view| {
            assert!(view.effect_page_admitted("cancel-media-preparation"));
            let cancel = visual_tests::control(view, "installer.cancel-preparation").unwrap();
            assert!(!cancel.accessibility().unwrap().state.disabled);
            let banner = visual_tests::control(view, "installer.preparation-status")
                .unwrap()
                .accessibility()
                .unwrap();
            assert_eq!(
                banner.name.as_deref(),
                Some(t("installer-media-phase-archive").as_str())
            );
            assert_eq!(banner.description, view.media_preparation.progress_label());
            assert_eq!(
                view.media_preparation.progress_label().unwrap(),
                super::super::strings::t_args(
                    "installer-media-progress-bytes-total",
                    &[
                        ("size", ui::format::size(4096).into()),
                        ("total", ui::format::size(8192).into()),
                    ]
                )
            );
        })
        .unwrap();
        let before = status(&mut app);
        assert_eq!(before["media_preparation"]["completed"], 4096);
        assert_eq!(before["media_preparation"]["total"], 8192);
        assert!(!before.to_string().contains(&scratch.display().to_string()));
        assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        app.effect(&InstallerPreviewEffect::CancelMediaPreparation)
            .unwrap();
        worker.probe.send(()).unwrap();
        assert_eq!(
            worker
                .cancelled
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            std::io::ErrorKind::Interrupted
        );
        let stopping = status(&mut app);
        assert_eq!(stopping["media_preparation"]["cancelling"], true);
        assert_eq!(
            stopping["media_preparation"]["phase"],
            "installer-media-phase-cancelling"
        );
        app.with_widget::<InstallerPreview, _>(|view| {
            let banner = visual_tests::control(view, "installer.preparation-status")
                .unwrap()
                .accessibility()
                .unwrap();
            assert_eq!(
                banner.name.as_deref(),
                Some(t("installer-media-phase-cancelling").as_str())
            );
        })
        .unwrap();
        assert!(scratch.exists(), "cleanup ownership stays with the worker");
        assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
        worker.finish.send(()).unwrap();
        wait_for_cancelled(&mut app);
        assert!(!scratch.exists());
        let after = status(&mut app);
        for key in [
            "page",
            "name",
            "device_name",
            "categories",
            "destination",
            "prepared_identity",
            "accounts",
        ] {
            assert_eq!(after[key], before[key], "{page:?}: {key}");
        }
        app.with_widget::<InstallerPreview, _>(|view| {
            assert!(!view.media_preparation.busy());
            assert!(!view.direct.belongings_pending);
            assert!(!view.direct.belongings_continue);
            assert!(!view.effect_page_admitted("cancel-media-preparation"));
            assert!(visual_tests::control(view, "installer.preparation-status").is_none());
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::CancelMediaPreparation)
                .is_err()
        );
    }
}

#[test]
fn busy_preparation_does_not_admit_cancel_in_privileged_or_finished_views() {
    let private = tempfile::tempdir().unwrap();
    let scratch = private.path().join("preparation-partial");
    std::fs::write(&scratch, b"unfinished").unwrap();
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    let worker = app
        .with_widget::<InstallerPreview, _>(|view| {
            view.live = true;
            view.restart = restart_controller::Restart::default();
            view.media_preparation
                .native
                .get_mut()
                .unwrap()
                .as_any_mut()
                .downcast_mut::<prepared_media_native::Controller>()
                .unwrap()
                .start_cancellation_fixture(scratch)
        })
        .unwrap();
    for (page, active, live) in [
        (Page::Review, true, true),
        (Page::Ready, false, true),
        (Page::Destination, false, true),
        (Page::Review, false, false),
    ] {
        app.with_widget::<InstallerPreview, _>(|view| {
            view.journey.page = page;
            view.direct.active = active;
            view.live = live;
            assert!(view.media_preparation.busy());
            assert!(!view.effect_page_admitted("cancel-media-preparation"));
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::CancelMediaPreparation)
                .is_err()
        );
    }
    app.with_widget::<InstallerPreview, _>(|view| {
        view.journey.page = Page::Review;
        view.direct.active = false;
        view.live = true;
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::CancelMediaPreparation)
        .unwrap();
    worker.probe.send(()).unwrap();
    assert_eq!(
        worker
            .cancelled
            .recv_timeout(Duration::from_secs(5))
            .unwrap(),
        std::io::ErrorKind::Interrupted
    );
    worker.finish.send(()).unwrap();
    wait_for_cancelled(&mut app);
}

#[test]
fn cancel_is_not_admitted_without_preparation() {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|view| {
        view.live = true;
        view.restart = restart_controller::Restart::default();
        view.journey.page = Page::Review;
        assert!(!view.effect_page_admitted("cancel-media-preparation"));
        assert!(visual_tests::control(view, "installer.cancel-preparation").is_none());
    })
    .unwrap();
    assert!(
        app.effect(&InstallerPreviewEffect::CancelMediaPreparation)
            .is_err()
    );
}
