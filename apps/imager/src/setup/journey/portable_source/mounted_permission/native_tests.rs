//! Actual shared-picker permission continuation, never a source epoch or receipt.
use super::*;
use std::{
    ffi::CString,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};
const SELECTOR: &str = "setup::journey::portable_source_native::mounted_permission::native_tests::actual_mounted_permission_controller_preserves_original_grant_and_refuses_stale_and_wrong_answers";
struct Binding(CString);
impl Binding {
    fn new(source: &Path, target: &Path) -> Self {
        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let target = CString::new(target.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0
        );
        Self(target)
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        assert_eq!(
            unsafe { libc::umount2(self.0.as_ptr(), 0) },
            0,
            "close actual permission custody before normal unmount"
        );
    }
}
struct Picker {
    path: PathBuf,
    answer: bool,
}
impl ui::host::FilePicker for Picker {
    fn pick(&self, request: ui::PickRequest) -> ui::PortalChannel {
        assert!(request.is_directory());
        let (tx, rx) = mpsc::channel();
        if self.answer {
            tx.send(ui::PortalOutcome::Opened(self.path.clone()))
                .unwrap();
        } else {
            tx.send(ui::PortalOutcome::Cancelled).unwrap();
            tx.send(ui::PortalOutcome::Opened(self.path.clone()))
                .unwrap();
        }
        rx
    }
    fn open_file(&self, _: ui::PickRequest) -> ui::portal::OpenFileChannel {
        panic!("this fixture grants a real directory");
    }
    fn pick_multiple(&self, _: ui::PickRequest) -> ui::portal::MultiChannel {
        panic!("no multiple picker");
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        panic!("no external launch");
    }
}
struct WaitingPicker(Mutex<Option<ui::PortalChannel>>);
impl ui::host::FilePicker for WaitingPicker {
    fn pick(&self, request: ui::PickRequest) -> ui::PortalChannel {
        assert!(request.is_directory());
        self.0.lock().unwrap().take().unwrap()
    }
    fn open_file(&self, _: ui::PickRequest) -> ui::portal::OpenFileChannel {
        panic!("directory fixture")
    }
    fn pick_multiple(&self, _: ui::PickRequest) -> ui::portal::MultiChannel {
        panic!("single location")
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        panic!("no external launch")
    }
}

struct RetiringPicker {
    picker: Picker,
    retiring: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
impl ui::host::FilePicker for RetiringPicker {
    fn pick(&self, request: ui::PickRequest) -> ui::PortalChannel {
        self.picker.pick(request)
    }
    fn open_file(&self, request: ui::PickRequest) -> ui::portal::OpenFileChannel {
        self.picker.open_file(request)
    }
    fn pick_multiple(&self, request: ui::PickRequest) -> ui::portal::MultiChannel {
        self.picker.pick_multiple(request)
    }
    fn launch(&self, request: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        self.picker.launch(request)
    }
}
impl Drop for RetiringPicker {
    fn drop(&mut self) {
        self.retiring.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }
}
fn wait(controller: &mut Controller) -> Result<Option<Reviewed>, ReviewFailure> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(reply) = controller.poll() {
            if !controller.busy() {
                return reply;
            }
        }
        assert!(
            Instant::now() < deadline,
            "actual permission worker failed to terminate"
        );
        std::thread::yield_now();
    }
}
fn request(controller: &mut Controller, id: &str, path: &Path, answer: bool) {
    let services = ui::host::HostServices::null().with_files(Arc::new(Picker {
        path: path.into(),
        answer,
    }));
    let _scope = ui::host::enter(&services);
    controller
        .request(
            id.into(),
            "Reviewed mounted location".into(),
            "Allow".into(),
        )
        .unwrap();
}
#[test]
fn actual_mounted_permission_controller_preserves_original_grant_and_refuses_stale_and_wrong_answers()
 {
    const CHILD: &str = "TILO_GUIDED_MOUNT_GRANT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = temp.path().join("complete");
        let mut child = std::process::Command::new("/usr/bin/unshare")
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", SELECTOR, "--nocapture"])
            .env(CHILD, &marker)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("actual mounted permission qualification timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read(marker).unwrap(),
            b"actual permission continuation complete; no export invented"
        );
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "isolated native root required"
    );
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let external = temp.path().join("external");
    let mounted = home.join("mounted");
    let wrong = temp.path().join("wrong");
    for path in [&home, &external, &mounted, &wrong] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(home.join("document"), b"original Home bytes").unwrap();
    std::fs::write(external.join("hidden-data"), b"original mounted bytes").unwrap();
    let bind = Binding::new(&external, &mounted);
    let grant =
        validate_grant(&home, &home).unwrap_or_else(|_| panic!("matching actual Home grant"));
    let selected_pin = crate::setup::migration::PinnedDirectory::open_absolute(&home).unwrap();
    let locations =
        MountedHomeLocations::discover(&selected_pin, &home, &AtomicBool::new(false)).unwrap();
    assert_eq!(locations.display_names(), vec!["mounted"]);
    let context = MountedContext {
        grant,
        choices: Choices {
            home: true,
            ..Choices::default()
        },
    };
    // Actual discovered custody is presented by the existing live effect/widget
    // surface; no content epoch or granted child is seeded into the UI.
    {
        use crate::setup::journey::{Destination, InstallerPreview, InstallerPreviewEffect, Page};
        use ui::framework::testing::TestApp;
        // Capture mode intentionally publishes no mutable effect runner.
        // Use the same live entry as --installer, on private scratch state.
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home);
        environment.set("XDG_CONFIG_HOME", temp.path().join("config"));
        environment.set("XDG_STATE_HOME", temp.path().join("state"));
        let mut app =
            TestApp::<crate::resource::GuidedInstallerShell>::launched(&["--installer"]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.settle();
            let state = app
                .effect(&InstallerPreviewEffect::ReadPreview)
                .unwrap_or_else(|error| panic!("live installer read effect refused: {error:?}"))
                .value
                .unwrap();
            if state["restart"]["state"] == "missing" {
                break;
            }
            assert!(
                state["restart"]["state"] == "checking" && Instant::now() < deadline,
                "fresh scratch installer preparation unavailable: {state}"
            );
            std::thread::yield_now();
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(
                preview.live,
                "the real guided installer attached its live surface"
            );
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.categories[0] = true;
            preview.portable_source.mounted_permission =
                Some(Controller::new(locations.clone(), context.clone()));
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("installer.mounted-location.current.grant"));
        assert!(
            !inspection.contains("installer.details"),
            "focused permission replaces the dense belongings overview"
        );
        assert!(
            !inspection.contains("installer.next"),
            "ordinary advance cannot bypass mounted permissions"
        );
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        let stale_request = app.effect(&InstallerPreviewEffect::GrantMountedHomeLocation {
            location: "not-this-generation".into(),
        });
        assert!(
            stale_request.is_ok(),
            "request is asynchronous; native worker must reject the opaque stale ID; public effect returned {stale_request:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.settle();
            let done = app
                .with_widget::<InstallerPreview, _>(|preview| !preview.portable_source.busy())
                .unwrap();
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "public stale-ID worker did not terminate"
            );
            std::thread::yield_now();
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.portable_source.mounted_choices()[0].2);
        })
        .unwrap();
        // Start a genuine platform request and verify the rendered cancel control
        // follows exactly the same busy availability as the semantic effect.
        let actual_id = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.portable_source.mounted_choices()[0].0.clone()
            })
            .unwrap();
        let (answer_tx, answer_rx) = mpsc::channel();
        let services = ui::host::HostServices::null()
            .with_files(Arc::new(WaitingPicker(Mutex::new(Some(answer_rx)))));
        let scope = ui::host::enter(&services);
        app.effect(&InstallerPreviewEffect::GrantMountedHomeLocation {
            location: actual_id,
        })
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.settle();
            let choosing = app
                .with_widget::<InstallerPreview, _>(|preview| {
                    preview.portable_source.mounted_choosing()
                })
                .unwrap();
            if choosing {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual scoped chooser was not published"
            );
            std::thread::yield_now();
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(
                crate::setup::journey::visual_tests::control(
                    preview,
                    "installer.mounted-locations.cancel"
                )
                .unwrap()
                .is_enabled()
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::CancelMountedHomePermission)
            .unwrap();
        assert!(
            answer_tx
                .send(ui::PortalOutcome::Opened(mounted.clone()))
                .is_err(),
            "late platform answer has no live receiver after actual cancellation"
        );
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.portable_source.busy());
            assert!(!preview.portable_source.mounted_choices()[0].2);
        })
        .unwrap();
        let details = app
            .with_widget::<InstallerPreview, _>(|preview| preview.journey.details)
            .unwrap();
        if !details {
            app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
                .unwrap();
        }
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: false,
        })
        .unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.journey.categories[0]);
            assert!(
                !preview.portable_source.mounted_permission_active(),
                "declining Home revokes idle mounted permission context"
            );
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::RestartMountedHomeReview)
                .is_err(),
            "restart must not revive declined Home consent"
        );
        drop(scope);
        drop(app);
    }
    let mut controller = Controller::new(locations.clone(), context.clone());
    let id = controller.locations()[0].0.clone();
    assert_eq!(
        controller.finish().unwrap_err(),
        "installer-mounted-home-grants-required"
    );
    // The actual HostServices destructor blocks AFTER reply transmission. A
    // channel reply must not clear busy before the production worker has exited.
    let (retiring_tx, retiring_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    {
        let services = ui::host::HostServices::null().with_files(Arc::new(RetiringPicker {
            picker: Picker {
                path: mounted.clone(),
                answer: true,
            },
            retiring: retiring_tx,
            release: Mutex::new(release_rx),
        }));
        let _scope = ui::host::enter(&services);
        controller
            .request(
                id.clone(),
                "Reviewed mounted location".into(),
                "Allow".into(),
            )
            .unwrap();
    }
    retiring_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        controller.poll().is_none(),
        "received reply is held until actual worker exit"
    );
    assert!(controller.busy());
    assert_eq!(
        controller
            .request(id.clone(), "Location".into(), "Allow".into())
            .unwrap_err(),
        "installer-portable-source-busy"
    );
    controller.cancel_request();
    assert!(
        controller.busy(),
        "cancellation retains the still-live thread"
    );
    release_tx.send(()).unwrap();
    assert!(
        wait(&mut controller)
            .unwrap_or_else(|_| panic!("joined cancelled worker"))
            .is_none()
    );
    assert!(!controller.busy());
    assert!(!controller.locations()[0].2);
    request(&mut controller, &id, &wrong, true);
    assert!(matches!(
        wait(&mut controller),
        Err(ReviewFailure::Permission)
    ));
    assert!(!controller.locations()[0].2);
    request(&mut controller, &id, &mounted, false);
    assert!(
        wait(&mut controller)
            .unwrap_or_else(|_| panic!("actual picker cancellation"))
            .is_none()
    );
    assert!(
        !controller.locations()[0].2,
        "late answer after cancellation cannot grant"
    );
    request(&mut controller, &id, &mounted, true);
    assert!(
        wait(&mut controller)
            .unwrap_or_else(|_| panic!("actual matching grant"))
            .is_none()
    );
    assert!(controller.locations()[0].2);
    controller.finish().unwrap();
    let reviewed = wait(&mut controller)
        .unwrap_or_else(|_| panic!("actual complete permission review"))
        .unwrap();
    assert!(Arc::ptr_eq(&reviewed.grant, &context.grant));
    let source = reviewed.home.as_ref().unwrap();
    assert!(source.original_only());
    assert!(
        source.summary().is_err(),
        "original scope has no fabricated content inventory"
    );
    assert_eq!(source.capture_status(), "awaiting-original-boot");
    assert!(reviewed.original.is_some());
    assert!(reviewed.applications.is_none());
    source
        .validate_with_cancel(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        std::fs::read(home.join("document")).unwrap(),
        b"original Home bytes"
    );
    assert_eq!(
        std::fs::read(external.join("hidden-data")).unwrap(),
        b"original mounted bytes"
    );
    let mut prior_choices = context.clone();
    prior_choices.choices.applications = true;
    let mut cancelled_review = Controller::new(locations.clone(), prior_choices);
    let cancelled_id = cancelled_review.locations()[0].0.clone();
    request(&mut cancelled_review, &cancelled_id, &mounted, true);
    assert!(
        wait(&mut cancelled_review)
            .unwrap_or_else(|_| panic!("actual grant before cancellation"))
            .is_none()
    );
    cancelled_review.finish().unwrap();
    cancelled_review.cancel_request();
    assert!(
        cancelled_review.busy(),
        "consumed RootSet worker remains owned through exit"
    );
    assert!(
        wait(&mut cancelled_review)
            .unwrap_or_else(|_| panic!("cancelled final review"))
            .is_none()
    );
    assert!(cancelled_review.needs_restart());
    assert!(
        cancelled_review.locations().is_empty(),
        "retired grants must not look approved"
    );
    assert!(Arc::ptr_eq(
        &cancelled_review.retained_context().grant,
        &context.grant
    ));
    assert!(cancelled_review.finish().is_err());
    let mut selection = Selection::default();
    selection.mounted_permission = Some(cancelled_review);
    assert_eq!(
        selection
            .refresh_mounted(
                wrong.clone(),
                Choices {
                    home: true,
                    ..Choices::default()
                }
            )
            .unwrap_err(),
        "installer-source-changed",
        "a different recognized Home cannot reuse the old grant"
    );
    selection
        .refresh_mounted(
            home.clone(),
            Choices {
                home: true,
                ..Choices::default()
            },
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = selection.poll() {
            assert!(
                result.is_err(),
                "new review must still report the actual mounted-root boundary"
            );
            assert!(selection.mounted_permission_active());
            assert!(!selection.mounted_needs_restart());
            assert!(
                !selection
                    .mounted_permission
                    .as_ref()
                    .unwrap()
                    .retained_context()
                    .choices
                    .applications,
                "renewal preserves current declined Apps consent rather than the previous review choices"
            );
            assert_eq!(selection.mounted_choices().len(), 1);
            assert!(
                !selection.mounted_choices()[0].2,
                "new generation needs a fresh matching grant"
            );
            assert_ne!(selection.mounted_choices()[0].0, cancelled_id);
            assert!(
                Arc::ptr_eq(
                    &selection
                        .mounted_permission
                        .as_ref()
                        .unwrap()
                        .retained_context()
                        .grant,
                    &context.grant
                ),
                "restart retains the exact approved original Home capability"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "retained-grant recovery review did not finish"
        );
        std::thread::yield_now();
    }
    drop(selection);
    let mut retired = Controller::new(locations.clone(), context.clone());
    let retired_id = retired.locations()[0].0.clone();
    request(&mut retired, &retired_id, &mounted, true);
    retired.cancel();
    assert!(
        wait(&mut retired)
            .unwrap_or_else(|_| panic!("actual retired chooser worker"))
            .is_none()
    );
    assert!(retired.locations().is_empty());
    drop((retired, reviewed, controller, context, locations));
    drop(bind);
    temp.close().unwrap();
    std::fs::write(
        std::env::var_os(CHILD).unwrap(),
        b"actual permission continuation complete; no export invented",
    )
    .unwrap();
}
