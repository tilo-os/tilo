// The setup pages driven as a person or an agent drives them: every step of
// the Full Tilo install, the check before the guided pages and the Desktop
// install goes through the journey's published effects and rows, over the
// fake platform, a local release server and the staging helper's scripted
// runs. No disk, firmware or network.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

use tilo_actions::ApiError;
use tilo_resource::Effect;
use tilo_ui_base::a11y::Action;
use tilo_ui_base::widget::Widget;
use ui::headless::{Driver, SemanticExpectation};
use ui::time::Duration;

use super::super::{Commands, Destination, InstallerPreview, Page};
use super::*;
use crate::hosting::testing::TestHost;
use crate::hosting::{Hosted, Port};
use crate::setup::actions::SETUP_ROW_ID;
use crate::setup::desktop_flow::{DesktopFailureKind, DesktopRuntime};
use crate::setup::internal_bundle;
use crate::setup::internal_cache::coordinator::{Phase as InternalPhase, Worker};
use crate::setup::internal_download::tests::{Server, response};

fn run_inline(_: &str, task: WorkerTask) -> Result<(), String> {
    task();
    Ok(())
}

/// A setup page over the fake platform and its release server.
struct HeldFixture {
    setup: NativeSetup,
    fakes: install_flow::tests::Fakes,
    bytes: Vec<u8>,
    _server: Server,
    _root: tempfile::TempDir,
    _preference: tempfile::TempDir,
}

/// The `handoff` page over `server`, with `serves` released selections
/// offered in turn to each download the page starts, resumes or discards.
fn setup_fixture_in(
    handoff: Handoff,
    server: Server,
    fakes: install_flow::tests::Fakes,
    host: HostOs,
    serves: usize,
    runtime: Option<install_flow::StagingRuntime>,
) -> HeldFixture {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let root = tempfile::tempdir().unwrap();
    let preference = tempfile::tempdir().unwrap();
    let platform = Capabilities {
        host,
        desktop_session: true,
        internal_dual_boot: false,
    };
    let runtime = runtime.unwrap_or_else(|| fakes.runtime());
    let install = InternalInstall::new(runtime, true, host);
    let worker = Worker::fixture_for_test(
        root.path(),
        server.transport(),
        install_flow::staging_consumer(install.link()),
    );
    let mut setup = NativeSetup::with_parts(handoff, platform, run_inline, install);
    setup.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
        platform,
        preference.path().join("selection.json"),
        worker,
        (0..serves).map(|_| vec![server.release(&bytes)]).collect(),
        "x86_64",
    ));
    HeldFixture {
        setup,
        fakes,
        bytes,
        _server: server,
        _root: root,
        _preference: preference,
    }
}

fn setup_fixture(
    server: Server,
    fakes: install_flow::tests::Fakes,
    host: HostOs,
    serves: usize,
) -> HeldFixture {
    setup_fixture_in(Handoff::Install, server, fakes, host, serves, None)
}

/// The `handoff` page over the fixture release, three downloads deep.
fn guarded(handoff: Handoff, fakes: install_flow::tests::Fakes, host: HostOs) -> HeldFixture {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let server = Server::new(vec![
        response(200, "", &bytes),
        response(200, "", &vec![b'E'; 4096]),
        response(200, "", &vec![b'R'; 8192]),
    ]);
    setup_fixture_in(handoff, server, fakes, host, 3, None)
}

fn guarded_app(fakes: install_flow::tests::Fakes, host: HostOs) -> HeldFixture {
    guarded(Handoff::Install, fakes, host)
}

/// The Full Tilo page with the guarded installer and no release on offer.
fn supported_fixture(handoff: Handoff) -> NativeSetup {
    NativeSetup::with_parts(
        handoff,
        Capabilities {
            host: HostOs::Linux,
            desktop_session: true,
            internal_dual_boot: true,
        },
        launch_worker,
        InternalInstall::guarded(HostOs::Linux),
    )
}

fn open_full_review(setup: &mut NativeSetup) {
    setup.start_host_probe();
    setup.poll();
    assert!(setup.installs(), "{:?}", setup.notice);
}

fn wait_for(setup: &mut NativeSetup, what: &str, done: impl Fn(&NativeSetup) -> bool) {
    tilo_test_env::wait_until(what, || {
        setup.poll();
        done(setup)
    });
}

/// The journey showing `setup`, as a window shows it.
fn page(setup: NativeSetup) -> InstallerPreview {
    InstallerPreview {
        native: Some(setup),
        ..InstallerPreview::default()
    }
}

/// `setup`'s page mounted in the host, as a caller reaches it.
fn launch(setup: NativeSetup) -> TestHost {
    TestHost::hosting(|store, wake| {
        Box::new(Hosted(InstallerPreview {
            port: Port::new(store.clone(), wake.clone()),
            ..page(setup)
        }))
    })
}

fn with_setup<O>(app: &mut TestHost, visit: impl FnOnce(&mut NativeSetup) -> O) -> O {
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.dirty = true;
        visit(preview.native.as_mut().expect("a setup page is shown"))
    })
    .expect("the journey is shown")
}

/// The page's composition, as the window builds it.
fn composition(setup: &NativeSetup) -> Box<dyn Widget> {
    InstallerPreview::default().native_page(setup)
}

fn retained_widget_count(root: &dyn Widget, id: &str) -> usize {
    usize::from(root.test_id() == Some(id))
        + root
            .children()
            .into_iter()
            .map(|child| retained_widget_count(child, id))
            .sum::<usize>()
}

fn retained_name(root: &dyn Widget, id: &str) -> Option<String> {
    tilo_ui_base::widget::find_test_id(root, id)?
        .accessibility()?
        .name
}

fn find_row(
    resources: &[tilo_actions::ResourceSpec],
    id: &str,
) -> Option<tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find_row(&resource.children, id)
        }
    })
}

fn child_resource<'a>(
    resource: &'a tilo_actions::ResourceSpec,
    kind: &str,
) -> &'a tilo_actions::ResourceSpec {
    resource
        .children
        .iter()
        .find(|child| child.kind == kind)
        .unwrap_or_else(|| panic!("missing child resource kind {kind}"))
}

/// The internal row's action `id`, bound and refused as the page publishes it.
fn internal_action(setup: &NativeSetup, id: &str) -> tilo_actions::ActionSpec {
    let projection = setup.projection();
    let mut rows = vec![crate::setup::actions::row(&projection).into_resource()];
    let statuses = crate::setup::actions::row_statuses(&projection);
    tilo_resource::status::apply_row_statuses(&mut rows, &|row| {
        statuses
            .iter()
            .find(|(id, _)| id == row)
            .map(|(_, statuses)| statuses.clone())
            .unwrap_or_default()
    });
    child_resource(&rows[0], "imager.setup.internal")
        .actions
        .iter()
        .find(|action| action.id == id)
        .unwrap_or_else(|| panic!("missing internal action {id}"))
        .clone()
}

/// Invoke the internal row's `id` with what the row binds, past its
/// availability: the page's own answer refuses what it may not do.
fn invoke_internal(setup: &mut NativeSetup, id: &str) -> Result<(), String> {
    let encoded = internal_action(setup, id)
        .build_json(&serde_json::Map::new())
        .map_err(|error| error.to_string())?;
    assert_eq!(encoded.type_id, InstallerPreviewEffect::OPERATION);
    setup.apply(
        InstallerPreviewEffect::from_payload(&encoded.payload)
            .map_err(|error| error.to_string())?,
    )
}

/// What the setup publishes right now, read the way the palette or an agent
/// reads it.
fn setup_value(app: &mut TestHost) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .expect("the journey answers its status")
        .value
        .expect("the journey's status is a value")["setup"]
        .clone()
}

fn internal_value(app: &mut TestHost) -> serde_json::Value {
    setup_value(app)["internal"].clone()
}

fn download_phase(app: &mut TestHost) -> String {
    internal_value(app)["download"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn download_failure(app: &mut TestHost) -> Option<String> {
    with_setup(app, |setup| {
        setup
            .internal_download_flow
            .as_ref()
            .and_then(|flow| flow.presentation().failure.clone())
    })
}

fn wait_download(app: &mut TestHost, wanted: &[&str]) {
    tilo_test_env::wait_until(format!("the download to be one of {wanted:?}"), || {
        let phase = download_phase(app);
        assert!(
            phase != "failed" || wanted.contains(&"failed"),
            "the download failed instead of becoming {wanted:?}: {:?}",
            download_failure(app)
        );
        wanted.contains(&phase.as_str())
    });
}

fn wait_run(app: &mut TestHost, wanted: &str) {
    tilo_test_env::wait_until(format!("the installer run to be {wanted}"), || {
        internal_value(app)["run"].as_str() == Some(wanted)
    });
}

/// Start, pause, resume, release, discard and remove as the internal row
/// offers them: the row binds the reviewed digest and the pending operation,
/// so the caller names the command alone.
fn internal_effect(
    app: &mut TestHost,
    effect: &InstallerPreviewEffect,
) -> Result<tilo_actions::ActionOutput, ApiError> {
    app.effect_on("imager.setup.internal", effect)
}

/// The fixture release, served by what each request asks for.
fn release_server(bytes: &[u8]) -> Server {
    Server::serve_release(bytes.to_vec(), vec![b'E'; 4096], vec![b'R'; 8192], |_| {})
}

/// A release server whose first component request waits until the test lets
/// it go, so the download sits in the phase a person can pause or cancel.
fn held_release_server(bytes: &[u8]) -> (Server, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
    let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
    let (go_tx, go_rx) = mpsc::sync_channel(0);
    let mut held = false;
    let server = Server::serve_release(
        bytes.to_vec(),
        vec![b'E'; 4096],
        vec![b'R'; 8192],
        move |target| {
            if !held && target.ends_with("staging-esp.img") {
                held = true;
                blocked_tx.send(()).expect("the test waits for the block");
                go_rx.recv().expect("the test lets the request go");
            }
        },
    );
    (server, blocked_rx, go_tx)
}

fn internal_sentence(key: &str) -> String {
    t(key)
}

tilo_test_env::scenario! {
    #[cfg(target_os = "linux")]
    fn a_desktop_only_install_checks_its_packages_first;

    phase desktop_only_records_intent_without_inventing_support;
    {
        let setup = NativeSetup::new(Handoff::Desktop);
        assert_eq!(setup.handoff, Handoff::Desktop);
        assert!(!setup.platform.desktop_session);
        assert!(!setup.internal_install.is_probing());
        assert!(!setup.projection().desktop.actions.install);
    }

    phase desktop_only_install_checks_packages_and_blocks_navigation;
    {
        let mut setup = NativeSetup::new(Handoff::Desktop);
        setup.desktop_flow = DesktopFlow::with_runtime_and_state(
            DesktopRuntime {
                launch_probe: || {
                    let (_sender, receiver) = mpsc::channel();
                    Ok(receiver)
                },
                launch_download: |_, _| Err("unexpected download".into()),
                launch_install: |_| Err("unexpected native install".into()),
            },
            DesktopFlowState::Ready(fixture_desktop_selection(false)),
        );
        assert!(!setup.platform.desktop_session);
        assert!(setup.projection().desktop.actions.install);
        setup.apply(InstallerPreviewEffect::InstallDesktop).unwrap();
        assert!(setup.desktop_flow.has_pending_operation());
        assert!(matches!(setup.desktop_flow.state(), DesktopFlowState::Checking));
        assert!(!setup.navigation_allowed());
        assert!(setup.apply(InstallerPreviewEffect::InstallDesktop).is_err());
        assert!(!setup.internal_install.is_probing());
    }
}

tilo_test_env::scenario! {
    fn guided_setup_refuses_what_the_review_did_not_admit;

    phase guided_navigation_follows_actual_download_and_staging_with_one_way_back;
    {
        let mut fixture = guarded_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
        let setup = &mut fixture.setup;
        open_full_review(setup);
        assert!(setup.navigation_allowed());
        let page = composition(setup);
        assert_eq!(retained_widget_count(page.as_ref(), "installer.back"), 1);
        assert!(retained_name(page.as_ref(), "setup.back").is_none());
        invoke_internal(setup, "start-internal-download").unwrap();
        assert!(!setup.navigation_allowed());
        wait_for(setup, "download ready", NativeSetup::internal_download_ready);
        assert!(setup.navigation_allowed());
        invoke_internal(setup, "install-internal").unwrap();
        assert!(!setup.navigation_allowed());
        wait_for(setup, "staged", |setup| {
            matches!(
                setup.internal_install.run_state(),
                RunState::Succeeded {
                    action: HelperAction::Stage
                }
            )
        });
        assert!(!setup.navigation_allowed());
    }

    phase unsupported_host_keeps_internal_state_but_refuses_installation_actions;
    {
        let mut setup = NativeSetup::with_parts(
            Handoff::Install,
            Capabilities {
                host: HostOs::Other,
                desktop_session: false,
                internal_dual_boot: false,
            },
            launch_worker,
            InternalInstall::guarded(HostOs::Other),
        );
        let projection = setup.projection();
        assert_eq!(projection.internal, setup.internal_projection());
        assert!(!setup.internal_install.offers_full_os());
        assert!(setup.apply(InstallerPreviewEffect::StartInternalDownload).is_err());
        assert!(
            setup
                .apply(InstallerPreviewEffect::InstallInternal {
                    reviewed: String::new(),
                })
                .is_err()
        );
    }
}

/// A download fixture over `server` with `releases` offered in turn, for the
/// supported Full Tilo page.
fn download_fixture(
    server: &Server,
    bytes: &[u8],
    releases: usize,
) -> (NativeSetup, tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let preference = tempfile::tempdir().unwrap();
    let worker = Worker::fixture_for_test(root.path(), server.transport(), |_, _, _, _| {
        Err("Protected staging is unavailable.".into())
    });
    let mut setup = supported_fixture(Handoff::Install);
    setup.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
        setup.platform,
        preference.path().join("selection.json"),
        worker,
        (0..releases).map(|_| vec![server.release(bytes)]).collect(),
        "x86_64",
    ));
    (setup, root, preference)
}

fn wait_download_phase(setup: &mut NativeSetup, phase: InternalPhase, what: &str) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        setup.poll();
        if setup
            .internal_download_flow
            .as_ref()
            .is_some_and(|flow| flow.presentation().phase == Some(phase))
        {
            break;
        }
        assert!(ui::time::now() < deadline, "{what} timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn full_os_download_is_distinct_and_reaches_only_the_real_worker() {
    {
        let mut setup = supported_fixture(Handoff::Install);
        let page = composition(&setup);
        assert_eq!(
            retained_widget_count(page.as_ref(), "setup.internal.install"),
            0
        );
        assert!(
            !internal_action(&setup, "install-internal")
                .contract
                .availability
                .is_available()
        );
        assert!(invoke_internal(&mut setup, "install-internal").is_err());
        assert_eq!(setup.internal_install.run_state(), &RunState::Idle);
        let mut harness = Driver::new(
            ui::State::new(),
            Box::new(Hosted(page_of(setup))),
            (960, 720),
        );

        harness
            .expect(
                &SemanticExpectation::named("setup.internal.status")
                    .name_contains("No Tilo download is offered right now."),
            )
            .unwrap();
        harness
            .expect(
                &SemanticExpectation::named("setup.internal.install-unavailable")
                    .name_contains("Install Tilo")
                    .enabled(false),
            )
            .unwrap();
    }

    {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let (mut setup, _root, _preference) = download_fixture(&server, &bytes, 2);

        setup
            .apply(InstallerPreviewEffect::StartInternalDownload)
            .unwrap();
        assert!(matches!(
            setup.desktop_flow.state(),
            DesktopFlowState::Ready(_)
        ));
        wait_download_phase(&mut setup, InternalPhase::Ready, "internal download");
        assert!(!setup.needs_idle_sync());
        let mut ready = Driver::new(
            ui::State::new(),
            Box::new(setup.internal_download_card(&Commands::default())),
            (640, 320),
        );
        assert!(ready.a11y_node_named("Download ready").is_some());
        ready
            .expect(
                &SemanticExpectation::named("setup.internal.release")
                    .name_contains("Keep download for later")
                    .enabled(true),
            )
            .unwrap();

        setup
            .apply(InstallerPreviewEffect::ReleaseInternalDownload)
            .unwrap();
        wait_download_phase(&mut setup, InternalPhase::Released, "internal release");
        let kept = Driver::new(
            ui::State::new(),
            Box::new(setup.internal_download_card(&Commands::default())),
            (640, 320),
        );
        assert!(kept.a11y_node_named("Kept for later").is_some());
    }

    {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let (mut setup, _root, _preference) = download_fixture(&server, &bytes, 2);
        setup.handoff = Handoff::Desktop;

        assert_eq!(
            setup.apply(InstallerPreviewEffect::StartInternalDownload),
            Err(t("imager-setup-internal-error-not-review"))
        );
        assert_eq!(
            setup.notice.as_deref(),
            Some(t("imager-setup-internal-error-not-review").as_str())
        );
        assert!(
            setup
                .internal_download_flow
                .as_ref()
                .is_some_and(|flow| flow.presentation().phase.is_none())
        );
        assert!(server.requests().is_empty());
    }

    {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
        let (continue_tx, continue_rx) = mpsc::sync_channel(0);
        let mut request = 0;
        let server = Server::on_request(
            vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ],
            move || {
                request += 1;
                if request == 2 {
                    blocked_tx.send(()).unwrap();
                    continue_rx.recv().unwrap();
                }
            },
        );
        let (mut setup, _root, _preference) = download_fixture(&server, &bytes, 3);

        setup
            .apply(InstallerPreviewEffect::StartInternalDownload)
            .unwrap();
        tilo_test_env::recv("the download to block", &blocked_rx);
        let requests = server.requests().len();
        let duplicate = setup.apply(InstallerPreviewEffect::StartInternalDownload);
        let requests_after = server.requests().len();
        continue_tx.send(()).unwrap();

        assert_eq!(
            duplicate,
            Err(t("imager-setup-error-download-start-unavailable"))
        );
        assert_eq!(requests_after, requests);
        wait_download_phase(&mut setup, InternalPhase::Ready, "internal download");
        assert_eq!(server.requests().len(), 3);
    }
}

/// `setup`'s page as the journey hosts it, for a driver to draw.
fn page_of(setup: NativeSetup) -> InstallerPreview {
    page(setup)
}

fn disconnected_desktop_probe() -> Result<crate::setup::desktop_flow::ProbeReceiver, String> {
    let (_tx, rx) = mpsc::channel();
    Ok(rx)
}

fn disconnected_desktop_download(
    _: DesktopRelease,
    _: Arc<AtomicBool>,
) -> Result<crate::setup::desktop_flow::DownloadReceiver, String> {
    let (_tx, rx) = mpsc::channel();
    Ok(rx)
}

fn disconnected_desktop_install(
    _: crate::setup::desktop_install::DesktopInstallRequest,
) -> Result<crate::setup::desktop_flow::InstallReceiver, String> {
    let (_tx, rx) = mpsc::channel();
    Ok(rx)
}

static RECOVERY_PROBES: AtomicUsize = AtomicUsize::new(0);

fn recoverable_desktop_probe() -> Result<crate::setup::desktop_flow::ProbeReceiver, String> {
    use crate::setup::desktop_probe as p;
    let (tx, rx) = mpsc::channel();
    if RECOVERY_PROBES.fetch_add(1, Ordering::SeqCst) > 0 {
        tx.send(Ok(crate::setup::desktop_flow::DesktopProbeSnapshot {
            host: p::DesktopHost {
                operating_system: "linux".into(),
                os_release: Ok(p::OsRelease {
                    id: "fedora".into(),
                    version_id: Some("44".into()),
                    name: Some("Fedora Linux".into()),
                }),
                architecture: Ok("aarch64".into()),
                package_manager: Ok(p::PackageManager::Dnf),
                immutable: Ok(false),
                sandboxed: Ok(false),
                session: Ok(p::SessionSupport {
                    display_manager: "gdm.service".into(),
                    other_wayland_sessions: vec!["gnome.desktop".into()],
                }),
                installed_package: Ok(p::InstalledPackage::Absent),
            },
            releases: vec![fixture_desktop_release()],
        }))
        .unwrap();
    }
    drop(tx);
    Ok(rx)
}

#[test]
fn desktop_review_shows_exact_status_and_recovers_from_failures() {
    {
        RECOVERY_PROBES.store(0, Ordering::SeqCst);
        let mut setup = NativeSetup::with_parts(
            Handoff::Desktop,
            Capabilities {
                host: HostOs::Linux,
                desktop_session: false,
                internal_dual_boot: false,
            },
            launch_worker,
            InternalInstall::guarded(HostOs::Linux),
        );
        setup.desktop_flow = DesktopFlow::without_start(DesktopRuntime {
            launch_probe: recoverable_desktop_probe,
            launch_download: disconnected_desktop_download,
            launch_install: disconnected_desktop_install,
        });
        setup.desktop_flow.start_check().unwrap();
        assert!(setup.poll());
        let mut harness = Driver::new(ui::State::new(), Box::new(Hosted(page(setup))), (640, 640));

        harness
            .expect(&SemanticExpectation::named("setup.retry-desktop").enabled(true))
            .unwrap();
        harness
            .perform_named("setup.retry-desktop", &Action::Activate)
            .unwrap();
        assert_eq!(RECOVERY_PROBES.load(Ordering::SeqCst), 2);
    }

    {
        let setup = supported_fixture(Handoff::Desktop);
        let mut harness = Driver::new(ui::State::new(), Box::new(Hosted(page(setup))), (640, 640));

        harness
            .expect(
                &SemanticExpectation::named("setup.desktop.status").name_contains("0.1.9-1.fc44"),
            )
            .unwrap();
        harness
            .expect(
                &SemanticExpectation::named("setup.desktop.status")
                    .name_contains("Fedora Linux 44 (ARM64)"),
            )
            .unwrap();
        harness
            .expect(&SemanticExpectation::named("setup.install").enabled(true))
            .unwrap();
    }

    {
        let mut setup = supported_fixture(Handoff::Desktop);
        setup.desktop_flow = DesktopFlow::with_runtime_and_state(
            DesktopRuntime {
                launch_probe: disconnected_desktop_probe,
                launch_download: disconnected_desktop_download,
                launch_install: disconnected_desktop_install,
            },
            DesktopFlowState::Failed {
                selection: Some(fixture_desktop_selection(false)),
                kind: DesktopFailureKind::NativeTransaction,
                message: "package manager failed".into(),
            },
        );

        setup.apply(InstallerPreviewEffect::RecheckDesktop).unwrap();

        assert!(matches!(
            setup.desktop_flow.state(),
            DesktopFlowState::Checking
        ));
    }

    {
        let mut setup = supported_fixture(Handoff::Install);
        setup.desktop_flow = DesktopFlow::from_fixture_state(DesktopFlowState::Installed {
            selection: fixture_desktop_selection(false),
            result: crate::setup::desktop_install::InstalledDesktop {
                version: "0.1.9-1.fc44".into(),
                session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
            },
        });
        setup.poll();
        assert_eq!(setup.notice, None);
        let harness = Driver::new(ui::State::new(), Box::new(Hosted(page(setup))), (640, 640));
        assert!(
            harness
                .a11y_node_named("Tilo Desktop is installed")
                .is_none()
        );
    }
}

#[test]
fn native_full_os_review_keeps_probe_refusal_and_verified_consumption() {
    let setup = NativeSetup::new(Handoff::Install);
    assert!(setup.installs());
    assert!(!setup.platform.internal_dual_boot);
    assert!(
        !internal_action(&setup, "install-internal")
            .contract
            .availability
            .is_available()
    );

    let mut blocked = guarded_app(
        install_flow::tests::Fakes::new(vec![crate::setup::staging::probe::Blocker::NotGpt]),
        HostOs::Windows,
    );
    blocked.setup.start_host_probe();
    blocked.setup.poll();
    assert!(invoke_internal(&mut blocked.setup, "install-internal").is_err());
    assert!(blocked.fakes.launches.lock().unwrap().is_empty());

    let mut fixture = guarded_app(install_flow::tests::Fakes::new(Vec::new()), HostOs::Windows);
    let setup = &mut fixture.setup;
    assert!(invoke_internal(setup, "install-internal").is_err());
    setup.start_host_probe();
    setup.poll();
    assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 1);
    assert!(invoke_internal(setup, "install-internal").is_err());
    invoke_internal(setup, "start-internal-download").unwrap();
    wait_for(
        setup,
        "verified native download",
        NativeSetup::internal_download_ready,
    );
    assert!(setup.navigation_allowed());
    let mut window = launch(fixture.setup);
    assert!(
        window
            .effect(&InstallerPreviewEffect::InstallInternal {
                reviewed: "stale-review".into(),
            })
            .is_err()
    );
    assert!(fixture.fakes.launches.lock().unwrap().is_empty());
    let root = find_row(&[window.published()], SETUP_ROW_ID).unwrap();
    let internal = child_resource(&root, "imager.setup.internal");
    window
        .call_on(
            internal.id.as_str(),
            "install-internal",
            serde_json::json!({}),
        )
        .unwrap();
    assert!(!with_setup(&mut window, |setup| setup.navigation_allowed()));
    let deadline = tilo_test_env::hang_deadline();
    while with_setup(&mut window, |setup| setup.internal_install.is_running()) {
        assert!(ui::time::now() < deadline, "native staging must complete");
    }
    with_setup(&mut window, |setup| {
        assert!(matches!(
            setup.internal_install.run_state(),
            RunState::Succeeded {
                action: HelperAction::Stage
            }
        ));
    });
    let launches = fixture.fakes.launches.lock().unwrap();
    assert_eq!(launches.len(), 1);
    assert!(launches[0].profile.is_empty());
    assert_eq!(launches[0].plan_sha256, launches[0].plan.digest().unwrap());
    assert_eq!(fixture.fakes.manifests.lock().unwrap()[0], fixture.bytes);
    assert!(!with_setup(&mut window, |setup| setup.navigation_allowed()));
}

tilo_test_env::scenario! {
    fn the_full_os_check_changes_nothing_and_reads_fresh_facts;

    phase full_os_check_refuses_mutations_even_with_forged_public_actions;
    {
        let mut fixture = guarded(Handoff::Check, install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
        let setup = &mut fixture.setup;
        setup.start_host_probe();
        wait_for(setup, "the full OS check is ready", NativeSetup::check_ready);
        for effect in [
            InstallerPreviewEffect::StartInternalDownload,
            InstallerPreviewEffect::ResumeInternalDownload,
            InstallerPreviewEffect::ReleaseInternalDownload,
            InstallerPreviewEffect::DiscardInternalDownload,
            InstallerPreviewEffect::InstallInternal {
                reviewed: setup.internal_install.reviewed_digest().unwrap().unwrap(),
            },
            InstallerPreviewEffect::RetryInternal {
                operation_id: "forged-operation".into(),
            },
            InstallerPreviewEffect::RemoveInternal {
                operation_id: "forged-operation".into(),
            },
            InstallerPreviewEffect::RestartInternal,
            InstallerPreviewEffect::SetInternalTargetSize { gib: 120 },
            InstallerPreviewEffect::InstallDesktop,
        ] {
            assert_eq!(setup.apply(effect), Err(t("imager-setup-check-only")));
        }
        for id in [
            "start-internal-download",
            "install-internal",
            "remove-internal",
        ] {
            assert!(invoke_internal(setup, id).is_err(), "{id}");
        }
        assert!(!setup.internal_projection().availability.start_download);
        assert!(!setup.internal_projection().availability.install);
        assert!(!setup.internal_projection().availability.remove);
        assert!(!setup.internal_download_ready());
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
        assert!(setup.checked().is_ok());
        assert_eq!(fixture.fakes.probes.load(Ordering::SeqCst), 1);

        let mut pending = install_flow::tests::Fakes::new(Vec::new());
        pending
            .records
            .push(install_flow::tests::record("actual-operation"));
        let mut fixture = guarded(Handoff::Check, pending, HostOs::Linux);
        fixture.setup.start_host_probe();
        fixture.setup.poll();
        assert!(fixture.setup.internal_install.pending().is_some());
        for effect in [
            InstallerPreviewEffect::RemoveInternal {
                operation_id: "actual-operation".into(),
            },
            InstallerPreviewEffect::RetryInternal {
                operation_id: "actual-operation".into(),
            },
        ] {
            assert!(fixture.setup.apply(effect).is_err());
        }
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
    }

    phase full_os_check_rechecks_fresh_facts_and_does_not_reuse_what_it_found;
    {
        let fakes = install_flow::tests::Fakes::new(Vec::new());
        let facts = Arc::new(std::sync::Mutex::new(fakes.facts.clone()));
        let mut fixture = guarded(Handoff::Check, fakes, HostOs::Linux);
        let mut runtime = fixture.fakes.runtime();
        let current = Arc::clone(&facts);
        runtime.probe = Arc::new(move || current.lock().unwrap().clone());
        fixture.setup.internal_install = InternalInstall::new(runtime, true, HostOs::Linux);
        let setup = &mut fixture.setup;
        setup.start_host_probe();
        wait_for(setup, "the full OS check is ready", NativeSetup::check_ready);
        assert!(setup.checked().is_ok());
        facts
            .lock()
            .unwrap()
            .blockers
            .push(crate::setup::staging::probe::Blocker::NotGpt);
        setup.apply(InstallerPreviewEffect::RecheckInternal).unwrap();
        assert!(setup.checked().is_err(), "a recheck forgets what the last check found");
        wait_for(setup, "the recheck has read the facts", |setup| {
            !setup.internal_install.is_probing()
        });
        assert!(!setup.check_ready());
        assert_eq!(setup.checked(), Err(t("imager-setup-check-not-ready")));
        assert!(
            setup
                .internal_readiness_text()
                .contains(&install_flow::blocker_text(
                    crate::setup::staging::probe::Blocker::NotGpt,
                    HostOs::Linux
                ))
        );
        assert!(fixture.fakes.launches.lock().unwrap().is_empty());
    }

    phase full_os_check_closed_check_is_build_limitation_and_keeps_back_available;
    {
        let mut setup = NativeSetup::new(Handoff::Check);
        setup.internal_install = InternalInstall::guarded(HostOs::Linux);
        assert!(!setup.check_ready());
        assert!(setup.navigation_allowed());
        assert_eq!(
            setup.internal_readiness_text(),
            t("imager-setup-check-not-enabled")
        );
        assert!(!setup.internal_projection().availability.install);
    }

    phase full_os_check_facts_enter_the_normal_flow;
    {
        let mut fixture = guarded(Handoff::Check, install_flow::tests::Fakes::new(Vec::new()), HostOs::Linux);
        fixture.setup.start_host_probe();
        wait_for(&mut fixture.setup, "the full OS check is ready", NativeSetup::check_ready);
        // The check found the disk the guided pages' own storage fixture
        // describes, so its facts carry the bounds the size step needs.
        let mut staged = super::super::storage::Storage::default();
        staged.stage();
        fixture.setup.check_facts = staged.facts.clone();
        let (facts, _) = fixture.setup.checked().unwrap();
        let guid = facts.basis.as_ref().unwrap().disk.guid.clone();
        let mut preview = InstallerPreview {
            live: true,
            linux: true,
            full_os_host: false,
            ..InstallerPreview::default()
        };
        preview.journey.destination = Some(Destination::Computer);
        preview.open_native(fixture.setup, false);
        preview
            .apply_native(InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert!(preview.native.is_none());
        assert_eq!(preview.error, None);
        assert_eq!(preview.journey.page, Page::Migrate);
        assert_eq!(preview.journey.history, vec![Page::Destination]);
        assert_eq!(
            preview
                .storage
                .facts
                .as_ref()
                .unwrap()
                .basis
                .as_ref()
                .unwrap()
                .disk
                .guid,
            guid
        );
        assert!(preview.journey.password.is_empty());
    }
}

#[test]
fn journey_the_review_checks_this_computer_before_offering_the_full_os_install() {
    use crate::setup::staging::probe::Blocker;
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());

    let fixture = setup_fixture(
        release_server(&bytes),
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Linux,
        2,
    );
    let probes = Arc::clone(&fixture.fakes.probes);
    let mut app = launch(fixture.setup);
    assert_eq!(internal_value(&mut app)["readiness"], "checking");
    let text = with_setup(&mut app, |setup| setup.internal_readiness_text());
    assert!(
        text.starts_with("Checking whether Tilo can be installed"),
        "{text}"
    );

    internal_effect(&mut app, &InstallerPreviewEffect::RecheckInternal).unwrap();
    tilo_test_env::wait_until("the check to answer", || {
        app.settle();
        !with_setup(&mut app, |setup| setup.internal_install.is_probing())
    });
    assert_eq!(internal_value(&mut app)["readiness"], "ready");
    assert_eq!(probes.load(Ordering::SeqCst), 1);

    // An older MBR disk is not offered: the person is told why, and no
    // install is published.
    let older_disk = setup_fixture(
        release_server(&bytes),
        install_flow::tests::Fakes::new(vec![Blocker::NotGpt]),
        HostOs::Linux,
        2,
    );
    let mut app = launch(older_disk.setup);
    internal_effect(&mut app, &InstallerPreviewEffect::RecheckInternal).unwrap();
    tilo_test_env::wait_until("the refused check to answer", || {
        app.settle();
        !with_setup(&mut app, |setup| setup.internal_install.is_probing())
    });
    let refused = internal_value(&mut app);
    assert_eq!(refused["readiness"], "unsupported");
    assert_eq!(refused["availability"]["install"], false);
    assert_eq!(refused["availability"]["start_download"], false);
    let text = with_setup(&mut app, |setup| setup.internal_readiness_text());
    assert!(text.contains("MBR"), "{text}");
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.install-unavailable",
        ))
        .unwrap();
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named("setup.internal.install"))
            .is_err(),
        "no install is offered for a disk Tilo cannot share"
    );
}

/// A setup page mounted in the host, with the fake platform and its release
/// server.
struct Mounted {
    app: TestHost,
    fakes: install_flow::tests::Fakes,
    server: Server,
    bytes: Vec<u8>,
    _root: tempfile::TempDir,
    _preference: tempfile::TempDir,
}

fn mount(mut fixture: HeldFixture) -> Mounted {
    open_full_review(&mut fixture.setup);
    Mounted {
        app: launch(fixture.setup),
        fakes: fixture.fakes,
        server: fixture._server,
        bytes: fixture.bytes,
        _root: fixture._root,
        _preference: fixture._preference,
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey keeps each step and its fresh fixture visible"
)]
fn journey_a_full_os_download_pauses_resumes_and_installs_alongside_the_current_system() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let (server, blocked_rx, go_tx) = held_release_server(&bytes);
    let mut hosted = mount(setup_fixture(
        server,
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Linux,
        6,
    ));
    let app = &mut hosted.app;

    let status = internal_value(app);
    assert_eq!(status["download"], "none");
    assert_eq!(status["availability"]["start_download"], true);
    assert_eq!(status["availability"]["install"], false);
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.install").enabled(false))
        .unwrap();

    internal_effect(app, &InstallerPreviewEffect::StartInternalDownload).unwrap();
    tilo_test_env::recv("the download to reach its first component", &blocked_rx);
    wait_download(
        app,
        &["authenticating", "acquiring", "preparing", "downloading"],
    );
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.pause").enabled(true))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.cancel").enabled(true))
        .unwrap();

    internal_effect(app, &InstallerPreviewEffect::PauseInternalDownload).unwrap();
    go_tx
        .send(())
        .expect("the server finishes the held request");
    wait_download(app, &["paused"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-paused")),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.resume").enabled(true))
        .unwrap();
    assert_eq!(internal_value(app)["availability"]["install"], false);

    internal_effect(app, &InstallerPreviewEffect::ResumeInternalDownload).unwrap();
    wait_download(app, &["ready"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-ready")),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.release").enabled(true))
        .unwrap();
    assert_eq!(internal_value(app)["availability"]["install"], true);

    // A caller holding the review before a size change is refused: the plan
    // it reviewed is not the plan the installer would run.
    let stale = internal_value(app)["reviewed"]
        .as_str()
        .expect("the review is published")
        .to_owned();
    app.effect(&InstallerPreviewEffect::SetInternalTargetSize { gib: 100 })
        .unwrap();
    assert_eq!(internal_value(app)["target_gib"], 100);
    let out_of_range = app
        .effect(&InstallerPreviewEffect::SetInternalTargetSize { gib: 10 })
        .unwrap_err();
    assert!(
        format!("{out_of_range:?}").contains("Choose a Tilo size between"),
        "{out_of_range:?}"
    );
    assert_eq!(internal_value(app)["target_gib"], 100);
    let stale_refusal = app
        .effect(&InstallerPreviewEffect::InstallInternal { reviewed: stale })
        .unwrap_err();
    assert!(
        format!("{stale_refusal:?}").contains(&t("imager-setup-internal-error-plan-changed")),
        "{stale_refusal:?}"
    );
    assert!(hosted.fakes.launches.lock().unwrap().is_empty());

    // Installing through the row binds the digest the review published.
    internal_effect(
        app,
        &InstallerPreviewEffect::InstallInternal {
            reviewed: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "succeeded");
    wait_download(app, &["consumed"]);
    let installed = internal_value(app);
    assert_eq!(installed["run_action"], "stage");
    assert_eq!(installed["availability"]["restart"], true);
    assert_eq!(installed["availability"]["restart_later"], true);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run")
                .name_contains("stays the startup default"),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.restart").enabled(true))
        .unwrap();
    assert!(
        internal_effect(
            app,
            &InstallerPreviewEffect::InstallInternal {
                reviewed: String::new()
            }
        )
        .is_err(),
        "a staged installer cannot be staged again"
    );
    {
        let launches = hosted.fakes.launches.lock().unwrap();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].action, HelperAction::Stage);
        assert_eq!(launches[0].plan_sha256, launches[0].plan.digest().unwrap());
        assert_eq!(hosted.fakes.manifests.lock().unwrap()[0], hosted.bytes);
    }

    internal_effect(app, &InstallerPreviewEffect::RestartInternalLater).unwrap();
    assert_eq!(internal_value(app)["availability"]["restart_later"], false);
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named("setup.internal.restart-later"))
            .is_err(),
        "a deferred restart is not offered twice"
    );
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run")
                .name_contains(t("imager-setup-internal-restart-deferred")),
        )
        .unwrap();

    internal_effect(app, &InstallerPreviewEffect::RestartInternal).unwrap();
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![HostCommand::Restart]
    );
    assert!(
        hosted.server.requests().len() >= 5,
        "the resumed download refetched"
    );
}

#[test]
fn journey_an_internal_download_can_be_cancelled_and_its_data_discarded() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let (server, blocked_rx, go_tx) = held_release_server(&bytes);
    let mut hosted = mount(setup_fixture(
        server,
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Linux,
        4,
    ));
    let app = &mut hosted.app;

    internal_effect(app, &InstallerPreviewEffect::StartInternalDownload).unwrap();
    tilo_test_env::recv("the download to reach its first component", &blocked_rx);
    wait_download(app, &["downloading", "acquiring", "preparing"]);
    internal_effect(app, &InstallerPreviewEffect::CancelInternalDownload).unwrap();
    go_tx
        .send(())
        .expect("the server finishes the held request");
    wait_download(app, &["cancelled"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-cancelled")),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.discard").enabled(true))
        .unwrap();

    internal_effect(app, &InstallerPreviewEffect::DiscardInternalDownload).unwrap();
    wait_download(app, &["discarded"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-discarded")),
        )
        .unwrap();
    assert_eq!(
        internal_value(app)["availability"]["discard_download"],
        false
    );
    assert!(hosted.fakes.launches.lock().unwrap().is_empty());
}

#[test]
fn journey_a_verified_internal_download_is_kept_for_later_and_can_still_be_discarded() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let mut hosted = mount(setup_fixture(
        release_server(&bytes),
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Windows,
        4,
    ));
    let app = &mut hosted.app;

    internal_effect(app, &InstallerPreviewEffect::StartInternalDownload).unwrap();
    wait_download(app, &["ready"]);
    internal_effect(app, &InstallerPreviewEffect::ReleaseInternalDownload).unwrap();
    wait_download(app, &["released"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-released")),
        )
        .unwrap();
    let kept = internal_value(app);
    assert_eq!(kept["availability"]["resume_download"], true);
    assert_eq!(kept["availability"]["discard_download"], true);

    internal_effect(app, &InstallerPreviewEffect::DiscardInternalDownload).unwrap();
    wait_download(app, &["discarded"]);
    assert_eq!(internal_value(app)["availability"]["install"], false);
}

/// A Full Tilo review whose probe answers with `blockers`, and whose Fast
/// Startup fix clears them, as a person moving through firmware would find.
fn remedy_fixture(blockers: Vec<crate::setup::staging::probe::Blocker>) -> Mounted {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let fakes = install_flow::tests::Fakes::new(blockers);
    let facts = Arc::new(std::sync::Mutex::new(fakes.facts.clone()));
    let mut runtime = fakes.runtime();
    let probes = Arc::clone(&fakes.probes);
    let shared = Arc::clone(&facts);
    runtime.probe = Arc::new(move || {
        probes.fetch_add(1, Ordering::SeqCst);
        shared.lock().unwrap().clone()
    });
    // Turning Fast Startup off is the fix itself: once the command runs, the
    // computer's next check finds the blockers gone, as after a real change.
    let commands = Arc::clone(&fakes.commands);
    let shared = Arc::clone(&facts);
    runtime.host_command = Arc::new(move |command| {
        if command == HostCommand::DisableFastStartup {
            shared.lock().unwrap().blockers.clear();
        }
        commands.lock().unwrap().push(command);
        Ok(())
    });
    mount(setup_fixture_in(
        Handoff::Install,
        release_server(&bytes),
        fakes,
        HostOs::Windows,
        4,
        Some(runtime),
    ))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey keeps each remedy and the recheck after it visible"
)]
fn journey_a_refused_full_os_install_opens_firmware_settings_disables_fast_startup_and_rechecks() {
    use crate::setup::staging::probe::Blocker;
    let mut hosted = remedy_fixture(vec![Blocker::SecureBootOn, Blocker::Hibernation]);
    let app = &mut hosted.app;

    let blocked = internal_value(app);
    assert_eq!(blocked["availability"]["install"], false);
    assert_eq!(blocked["availability"]["firmware_settings"], true);
    assert_eq!(blocked["availability"]["disable_fast_startup"], true);
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.blocker.secure-boot-on",
        ))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.blocker.hibernation",
        ))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.firmware-settings").enabled(true))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.disable-fast-startup").enabled(true))
        .unwrap();
    assert!(
        internal_effect(
            app,
            &InstallerPreviewEffect::InstallInternal {
                reviewed: String::new()
            }
        )
        .is_err()
    );

    internal_effect(app, &InstallerPreviewEffect::InternalFirmwareSettings).unwrap();
    tilo_test_env::wait_until("firmware settings to open", || {
        app.settle();
        with_setup(app, |setup| setup.host_command_pending.is_none())
    });
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![HostCommand::FirmwareSettings {
            suspend_bitlocker: false
        }]
    );

    // The person turns Secure Boot off in firmware, then lets setup turn off
    // Fast Startup; its success rechecks this computer by itself.
    internal_effect(app, &InstallerPreviewEffect::InternalDisableFastStartup).unwrap();
    tilo_test_env::wait_until("the recheck after Fast Startup", || {
        app.settle();
        with_setup(app, |setup| {
            setup.host_command_pending.is_none() && !setup.internal_install.is_probing()
        })
    });
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![
            HostCommand::FirmwareSettings {
                suspend_bitlocker: false
            },
            HostCommand::DisableFastStartup
        ]
    );
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 2);
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.blockers"))
        .unwrap();
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named(
                "setup.internal.blocker.hibernation",
            ))
            .is_err()
    );

    internal_effect(app, &InstallerPreviewEffect::RecheckInternal).unwrap();
    tilo_test_env::wait_until("the recheck to answer", || {
        app.settle();
        !with_setup(app, |setup| setup.internal_install.is_probing())
    });
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 3);
    let ready = internal_value(app);
    assert_eq!(ready["readiness"], "ready");
    assert_eq!(ready["availability"]["firmware_settings"], false);
    assert_eq!(ready["availability"]["disable_fast_startup"], false);
    // Still no install: the verified download comes first.
    assert_eq!(ready["availability"]["install"], false);

    let mut battery = remedy_fixture(vec![Blocker::OnBattery]);
    let app = &mut battery.app;
    let state = internal_value(app);
    assert_eq!(
        state["availability"]["firmware_settings"], false,
        "battery is not a firmware problem"
    );
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.blocker.on-battery",
        ))
        .unwrap();
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named(
                "setup.internal.firmware-settings",
            ))
            .is_err()
    );
    assert!(internal_effect(app, &InstallerPreviewEffect::InternalFirmwareSettings).is_err());
    assert!(battery.fakes.commands.lock().unwrap().is_empty());
}

#[test]
fn journey_a_failed_internal_install_retries_the_unfinished_operation_to_a_staged_install() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let mut fakes = install_flow::tests::Fakes::new(vec![
        crate::setup::staging::probe::Blocker::PendingOperation,
    ]);
    fakes.records = vec![install_flow::tests::record("op-1")];
    let runtime = queued_runtime(
        &fakes,
        vec![
            vec![install_flow::tests::finished(false, Some("boot-next-busy"))],
            vec![install_flow::tests::finished(true, None)],
        ],
    );
    let mut hosted = mount(setup_fixture_in(
        Handoff::Install,
        release_server(&bytes),
        fakes,
        HostOs::Windows,
        4,
        Some(runtime),
    ));
    let app = &mut hosted.app;

    internal_effect(app, &InstallerPreviewEffect::StartInternalDownload).unwrap();
    wait_download(app, &["ready"]);
    assert_eq!(internal_value(app)["pending_operation"], "op-1");
    internal_effect(
        app,
        &InstallerPreviewEffect::InstallInternal {
            reviewed: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "failed");
    let failed = internal_value(app);
    assert_eq!(failed["failure"], "boot-next-busy");
    assert_eq!(failed["availability"]["retry"], true);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run").name_contains(internal_sentence(
                "imager-setup-internal-failure-boot-next-busy",
            )),
        )
        .unwrap();

    internal_effect(
        app,
        &InstallerPreviewEffect::RetryInternal {
            operation_id: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "succeeded");
    let staged = internal_value(app);
    assert_eq!(staged["run_action"], "arm");
    assert_eq!(staged["availability"]["restart"], true);
    {
        let launches = hosted.fakes.launches.lock().unwrap();
        assert_eq!(
            launches
                .iter()
                .map(|request| (request.action, request.operation_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(HelperAction::Stage, "op-1"), (HelperAction::Arm, "op-1")]
        );
    }

    internal_effect(app, &InstallerPreviewEffect::RestartInternalLater).unwrap();
    internal_effect(app, &InstallerPreviewEffect::RestartInternal).unwrap();
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![HostCommand::Restart]
    );
}

#[test]
fn journey_an_unfinished_internal_install_is_removed_and_its_space_returned() {
    use crate::setup::staging::probe::Blocker;
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let mut fakes = install_flow::tests::Fakes::new(vec![Blocker::PendingOperation]);
    fakes.records = vec![install_flow::tests::record("op-1")];
    let mut hosted = mount(setup_fixture(
        release_server(&bytes),
        fakes,
        HostOs::Linux,
        4,
    ));
    let app = &mut hosted.app;

    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.retry").enabled(true))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.remove").enabled(true))
        .unwrap();
    let stale = app
        .effect(&InstallerPreviewEffect::RemoveInternal {
            operation_id: "op-0".into(),
        })
        .unwrap_err();
    assert!(
        format!("{stale:?}").contains(&t("imager-setup-internal-error-pending-changed")),
        "{stale:?}"
    );
    assert!(hosted.fakes.launches.lock().unwrap().is_empty());
    internal_effect(
        app,
        &InstallerPreviewEffect::RemoveInternal {
            operation_id: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "succeeded");
    let removed = internal_value(app);
    assert_eq!(removed["run_action"], "remove");
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run")
                .name_contains(internal_sentence("imager-setup-internal-removed")),
        )
        .unwrap();
    {
        let launches = hosted.fakes.launches.lock().unwrap();
        assert_eq!(
            launches
                .iter()
                .map(|request| (request.action, request.operation_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(HelperAction::Remove, "op-1")]
        );
    }
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 2);
}

/// The staging helper's runs, one script per launch: the first attempt fails
/// the way a busy one-time startup choice does, the next finishes.
fn queued_runtime(
    fakes: &install_flow::tests::Fakes,
    scripts: Vec<Vec<crate::setup::staging::helper::Poll>>,
) -> install_flow::StagingRuntime {
    let mut runtime = fakes.runtime();
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        scripts,
    )));
    let launches = Arc::clone(&fakes.launches);
    let manifests = Arc::clone(&fakes.manifests);
    runtime.launch = Arc::new(move |request: &crate::setup::staging::helper::Request| {
        if let Ok(bytes) = std::fs::read(&request.manifest_path) {
            manifests.lock().unwrap().push(bytes);
        }
        launches.lock().unwrap().push(request.clone());
        let script = queue.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::new(install_flow::tests::ScriptedHelper(script.into()))
            as Box<dyn install_flow::HelperRun>)
    });
    runtime
}

// The Desktop page's production surface and worker protocol, with no package
// or host transaction.
thread_local! {
    static JOURNEY_DOWNLOAD: RefCell<Option<mpsc::Sender<Result<PathBuf, String>>>> = const { RefCell::new(None) };
    static JOURNEY_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static JOURNEY_INSTALL: RefCell<Option<mpsc::Sender<crate::setup::desktop_install::DesktopInstallEvent>>> = const { RefCell::new(None) };
}

fn journey_probe() -> Result<crate::setup::desktop_flow::ProbeReceiver, String> {
    use crate::setup::desktop_probe as p;
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(crate::setup::desktop_flow::DesktopProbeSnapshot {
        host: p::DesktopHost {
            operating_system: "linux".into(),
            os_release: Ok(p::OsRelease {
                id: "fedora".into(),
                version_id: Some("44".into()),
                name: Some("Fedora Linux".into()),
            }),
            architecture: Ok("aarch64".into()),
            package_manager: Ok(p::PackageManager::Dnf),
            immutable: Ok(false),
            sandboxed: Ok(false),
            session: Ok(p::SessionSupport {
                display_manager: "gdm.service".into(),
                other_wayland_sessions: vec!["gnome.desktop".into()],
            }),
            installed_package: Ok(p::InstalledPackage::Absent),
        },
        releases: vec![fixture_desktop_release()],
    }))
    .map_err(|error| error.to_string())?;
    Ok(rx)
}

fn journey_download(
    release: DesktopRelease,
    cancel: Arc<AtomicBool>,
) -> Result<crate::setup::desktop_flow::DownloadReceiver, String> {
    if release.id != fixture_desktop_release().id
        || release.sha256 != fixture_desktop_release().sha256
    {
        return Err("wrong reviewed release".into());
    }
    let (tx, rx) = mpsc::channel();
    JOURNEY_DOWNLOAD.with(|slot| *slot.borrow_mut() = Some(tx));
    JOURNEY_CANCEL.with(|slot| *slot.borrow_mut() = Some(cancel));
    Ok(rx)
}

fn journey_install(
    request: crate::setup::desktop_install::DesktopInstallRequest,
) -> Result<crate::setup::desktop_flow::InstallReceiver, String> {
    if request.release_id != fixture_desktop_release().id
        || request.package_input != Path::new("/fixture/package.rpm")
    {
        return Err("wrong package request".into());
    }
    let (tx, rx) = mpsc::channel();
    JOURNEY_INSTALL.with(|slot| *slot.borrow_mut() = Some(tx));
    Ok(rx)
}

fn desktop_phase(app: &mut TestHost) -> serde_json::Value {
    setup_value(app)["desktop"].clone()
}

#[test]
fn desktop_journey_ui_cancel_recheck_and_verified_completion() {
    use crate::setup::desktop_install::DesktopInstallEvent as E;
    // The Desktop package is installed from the Desktop page alone: the Full
    // Tilo page shows the same flow and offers no install.
    let mut setup = supported_fixture(Handoff::Install);
    setup.desktop_flow = DesktopFlow::with_runtime_and_state(
        DesktopRuntime {
            launch_probe: journey_probe,
            launch_download: journey_download,
            launch_install: journey_install,
        },
        DesktopFlowState::Ready(fixture_desktop_selection(false)),
    );
    let mut app = launch(setup);
    assert!(
        !desktop_phase(&mut app)["actions"]["install"]
            .as_bool()
            .unwrap()
    );
    assert!(app.effect(&InstallerPreviewEffect::InstallDesktop).is_err());
    with_setup(&mut app, |setup| setup.handoff = Handoff::Desktop);
    app.settle();
    app.harness()
        .perform_named("setup.install", &Action::Activate)
        .unwrap();
    app.settle();
    assert_eq!(desktop_phase(&mut app)["phase"], "downloading");
    app.harness()
        .perform_named("setup.cancel-download", &Action::Activate)
        .unwrap();
    app.settle();
    JOURNEY_CANCEL.with(|slot| assert!(slot.borrow().as_ref().unwrap().load(Ordering::Acquire)));
    JOURNEY_DOWNLOAD.with(|slot| {
        slot.borrow_mut()
            .take()
            .unwrap()
            .send(Err("Download was cancelled.".into()))
            .unwrap()
    });
    app.settle();
    assert_eq!(desktop_phase(&mut app)["phase"], "failed");
    app.effect(&InstallerPreviewEffect::RecheckDesktop).unwrap();
    app.settle();
    assert_eq!(desktop_phase(&mut app)["phase"], "ready");
    app.effect(&InstallerPreviewEffect::InstallDesktop).unwrap();
    app.settle();
    JOURNEY_DOWNLOAD.with(|slot| {
        slot.borrow_mut()
            .take()
            .unwrap()
            .send(Ok(PathBuf::from("/fixture/package.rpm")))
            .unwrap()
    });
    app.settle();
    for (event, phase) in [
        (E::Preparing, "preparing"),
        (E::Installing, "installing"),
        (E::Verifying, "verifying"),
    ] {
        JOURNEY_INSTALL.with(|slot| slot.borrow().as_ref().unwrap().send(event).unwrap());
        app.settle();
        assert_eq!(desktop_phase(&mut app)["phase"], phase);
    }
    JOURNEY_INSTALL.with(|slot| {
        slot.borrow_mut()
            .take()
            .unwrap()
            .send(E::Complete(
                crate::setup::desktop_install::InstalledDesktop {
                    version: "0.1.9-1.fc44".into(),
                    session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
                },
            ))
            .unwrap()
    });
    app.settle();
    assert_eq!(desktop_phase(&mut app)["phase"], "installed");
    assert!(app.effect(&InstallerPreviewEffect::InstallDesktop).is_err());
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.desktop.status")
                .name_contains("choose Tilo on the login screen"),
        )
        .unwrap();
}

tilo_test_env::scenario! {
    fn the_setup_buttons_read_from_the_catalogues;

    phase the_setup_buttons_name_messages_the_catalogues_carry;
    // Every key a setup button names is a message both shipped languages
    // carry, so no window paints a key.
    {
        let catalog = &crate::resource::CATALOG;
        let previous = catalog.current_locale();
        for locale in ["en-US", "de-DE"] {
            catalog.set_locale(locale);
            for key in [
                "installer-preview-continue",
                "installer-preview-back",
                "imager-setup-action-start-download",
                "imager-setup-action-pause-download",
                "imager-setup-action-cancel-download",
                "imager-setup-action-resume-download",
                "imager-setup-action-release-download",
                "imager-setup-action-discard-download",
                "imager-setup-internal-install",
                "imager-setup-internal-continue",
                "imager-setup-internal-installing",
                "imager-setup-desktop-install",
                "imager-setup-desktop-cancel",
                "imager-setup-desktop-recheck",
                "installer-desktop-retry",
                "imager-setup-internal-remedy-firmware",
                "imager-setup-internal-remedy-fast-startup",
                "imager-setup-internal-recheck",
                "imager-setup-internal-resume-retry",
                "imager-setup-internal-resume-remove",
                "imager-setup-internal-restart-later",
                "imager-setup-internal-restart-now",
            ] {
                assert_ne!(catalog.t(key), key, "{key} is missing from {locale}");
            }
        }
        catalog.set_locale(&previous);
    }

    phase the_setup_buttons_read_in_the_windows_language;
    // A German window reads the review's buttons in German.
    {
        let catalog = &crate::resource::CATALOG;
        let previous = catalog.current_locale();
        catalog.set_locale("de-DE");
        let setup = supported_fixture(Handoff::Install);
        let harness = Driver::new(ui::State::new(), Box::new(Hosted(page(setup))), (960, 720));
        let labels: Vec<String> = harness
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        catalog.set_locale(&previous);
        // The fixture offers no download, so the review's download card says so in
        // German instead of showing a start button.
        for german in [
            "Tilo installieren",
            "Zurzeit wird kein Tilo-Download angeboten.",
        ] {
            assert!(
                labels.iter().any(|label| label == german),
                "{german} is missing from {labels:?}"
            );
        }
        assert!(
            labels.iter().all(|label| !matches!(
                label.as_str(),
                "Install Tilo" | "Download Tilo" | "No Tilo download is offered right now." | "Back"
            )),
            "the source language is left in the German window: {labels:?}"
        );
    }
}

#[cfg(target_os = "linux")]
mod support_report {
    use super::*;
    use crate::setup::support_report_view::tests::fixture_support_report;

    fn has_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().into_iter().any(|child| has_id(child, id))
    }

    tilo_test_env::scenario! {
        fn the_support_report_shows_limitations_and_evidence;

        phase checking_report_stays_visible_until_the_probe_reports;
        {
            let setup = NativeSetup::new(Handoff::Check);
            assert!(setup.support_report.is_none());
            assert!(
                setup
                    .hardware_report_sections()
                    .iter()
                    .any(|widget| has_id(widget.as_ref(), "setup.hardware.checking"))
            );
        }

        phase the_page_draws_the_probe_report;
        {
            let mut setup = NativeSetup::new(Handoff::Check);
            setup.support_report = Some(fixture_support_report());
            let sections = setup.hardware_report_sections();
            for id in [
                "setup.hardware.model",
                "setup.hardware.summary",
                "setup.hardware.fingerprint",
                "setup.hardware.wifi",
                "setup.hardware.camera",
            ] {
                assert!(
                    sections
                        .iter()
                        .any(|widget| has_id(widget.as_ref(), id)),
                    "{id} is drawn"
                );
            }
        }

        phase semantic_projection_carries_the_identity_statuses_and_evidence;
        {
            let mut setup = NativeSetup::new(Handoff::Check);
            setup.support_report = Some(fixture_support_report());
            let hardware = setup.projection().hardware.unwrap();
            assert_eq!(hardware.source, "local-scan");
            assert_eq!(hardware.model.as_deref(), Some("Test Laptop 15"));
            assert_eq!(hardware.variant.as_deref(), Some("TESTBOARD"));
            assert!(hardware.supported >= 4);
            assert_eq!(hardware.limited, 1);
            assert!(!hardware.blocking);
            let graphics = hardware
                .components
                .iter()
                .find(|component| component.kind == "graphics")
                .unwrap();
            assert_eq!(graphics.status, "supported");
            let evidence = serde_json::to_value(&graphics.evidence).unwrap();
            assert!(evidence.to_string().contains("amdgpu"));
            let fingerprint = hardware
                .components
                .iter()
                .find(|component| component.kind == "fingerprint")
                .unwrap();
            assert_eq!(fingerprint.status, "limited");
            assert_eq!(
                serde_json::to_value(fingerprint.remedy).unwrap(),
                serde_json::json!("password-sign-in")
            );
        }
    }
}
