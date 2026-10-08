// Production surface and worker protocol, with no package or host transaction.
thread_local! {
    static JOURNEY_DOWNLOAD: RefCell<Option<mpsc::Sender<Result<PathBuf, String>>>> = const { RefCell::new(None) };
    static JOURNEY_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static JOURNEY_INSTALL: RefCell<Option<mpsc::Sender<super::super::desktop_install::DesktopInstallEvent>>> = const { RefCell::new(None) };
}
fn journey_probe() -> Result<super::super::desktop_flow::ProbeReceiver, String> {
    use super::super::desktop_probe as p;
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(super::super::desktop_flow::DesktopProbeSnapshot {
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
) -> Result<super::super::desktop_flow::DownloadReceiver, String> {
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
    request: super::super::desktop_install::DesktopInstallRequest,
) -> Result<super::super::desktop_flow::InstallReceiver, String> {
    if request.release_id != fixture_desktop_release().id
        || request.package_input != Path::new("/fixture/package.rpm")
    {
        return Err("wrong package request".into());
    }
    let (tx, rx) = mpsc::channel();
    JOURNEY_INSTALL.with(|slot| *slot.borrow_mut() = Some(tx));
    Ok(rx)
}
fn journey_fixture() -> SetupApp {
    let mut setup = supported_fixture();
    setup.review_saver = |folders| {
        assert!(folders.is_empty());
        Ok(())
    };
    setup.desktop_flow = DesktopFlow::with_runtime_and_state(
        super::super::desktop_flow::DesktopRuntime {
            launch_probe: journey_probe,
            launch_download: journey_download,
            launch_install: journey_install,
        },
        DesktopFlowState::Ready(fixture_desktop_selection(false)),
    );
    setup
}
fn journey_phase(app: &mut TestApp<SetupFlow>) -> serde_json::Value {
    app.effect(&SetupEffect::ReadDesktopStatus)
        .unwrap()
        .value
        .unwrap()
}
fn journey_review(app: &mut TestApp<SetupFlow>) {
    app.effect(&SetupEffect::ChooseExperience {
        experience: "desktop".into(),
    })
    .unwrap();
    app.effect(&SetupEffect::ContinueSetup).unwrap();
    app.effect(&SetupEffect::SetCommonMode {
        mode: "later".into(),
    })
    .unwrap();
    app.effect(&SetupEffect::ContinueSetup).unwrap();
}
#[test]
fn desktop_journey_ui_cancel_recheck_and_verified_completion() {
    use super::super::desktop_install::DesktopInstallEvent as E;
    let mut app = launch(journey_fixture(), ImportApp::new);
    assert!(
        !journey_phase(&mut app)["actions"]["install"]
            .as_bool()
            .unwrap()
    );
    assert!(app.effect(&SetupEffect::InstallDesktop).is_err());
    journey_review(&mut app);
    app.harness()
        .perform_named("setup.install", &Action::Activate)
        .unwrap();
    app.settle();
    assert_eq!(journey_phase(&mut app)["phase"], "downloading");
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
    assert_eq!(journey_phase(&mut app)["phase"], "failed");
    app.effect(&SetupEffect::RecheckDesktop).unwrap();
    app.settle();
    assert_eq!(journey_phase(&mut app)["phase"], "ready");
    app.effect(&SetupEffect::InstallDesktop).unwrap();
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
        assert_eq!(journey_phase(&mut app)["phase"], phase);
    }
    JOURNEY_INSTALL.with(|slot| {
        slot.borrow_mut()
            .take()
            .unwrap()
            .send(E::Complete(
                super::super::desktop_install::InstalledDesktop {
                    version: "0.1.9-1.fc44".into(),
                    session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
                },
            ))
            .unwrap()
    });
    app.settle();
    assert_eq!(journey_phase(&mut app)["phase"], "installed");
    assert!(app.effect(&SetupEffect::InstallDesktop).is_err());
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.desktop.status")
                .name_contains("choose Tilo on the login screen"),
        )
        .unwrap();
}
#[test]
fn desktop_journey_usb_opens_existing_writer_without_alongside_authority() {
    let unsupported = SetupApp::with_capabilities(SetupCapabilities {
        platform: Capabilities {
            host: HostOs::Other,
            desktop_session: false,
            internal_dual_boot: false,
        },
        connected_folders: false,
    });
    let mut unsupported = launch(unsupported, ImportApp::new);
    let status = unsupported
        .effect(&SetupEffect::ReadSetupStatus)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(status["methods"]["usb"], false);
    assert!(unsupported.effect(&SetupEffect::OpenUsbWizard).is_err());
    drop(unsupported);
    let setup = SetupApp::with_capabilities(SetupCapabilities {
        platform: Capabilities {
            host: HostOs::Linux,
            desktop_session: false,
            internal_dual_boot: false,
        },
        connected_folders: false,
    });
    let mut app = launch(setup, ImportApp::new);
    let status = app
        .effect(&SetupEffect::ReadSetupStatus)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(status["methods"]["alongside"], false);
    assert_eq!(status["methods"]["usb"], true);
    assert!(
        app.effect(&SetupEffect::InstallInternal {
            reviewed: "not-authority".into()
        })
        .is_err()
    );
    app.harness()
        .perform_named("setup.method.usb", &Action::Activate)
        .unwrap();
    app.settle();
    let resources = app.harness().resources();
    assert!(find_row(&resources, "imager.setup").is_none());
    assert!(
        resources
            .iter()
            .any(|r| r.actions.iter().any(|a| a.id == "write")),
        "existing writer owns review and write"
    );
}
#[test]
fn desktop_journey_rejects_stale_pending_folder_review() {
    for pending in [PendingFolderSelection::Scan, PendingFolderSelection::Picker] {
        let (mut setup, _picker) = stale_ready_desktop_review(pending);
        let phase = setup.answer(SetupEffect::ReadDesktopStatus).unwrap();
        assert_eq!(phase["actions"]["install"], false);
        assert!(setup.answer(SetupEffect::InstallDesktop).is_err());
        assert!(!setup.desktop_flow.has_pending_operation());
    }
}

fn journey_shot_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/onboarding-native-shots");
    std::fs::create_dir_all(&root).unwrap();
    root
}
#[test]
fn desktop_journey_screenshots_preserve_live_fixture_availability_across_themes() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut unavailable = launch(SetupApp::for_shot("setup-experience"), ImportApp::new);
        unavailable.harness().set_theme(theme.clone());
        unavailable
            .harness()
            .expect(&SemanticExpectation::named("setup.method.usb").enabled(true))
            .unwrap();
        assert_eq!(
            unavailable
                .effect(&SetupEffect::ReadDesktopStatus)
                .unwrap()
                .value
                .unwrap()["phase"],
            "unavailable"
        );
        let image = journey_shot_root().join(format!("setup-{name}.png"));
        unavailable.harness().render_png(&image);
        assert!(std::fs::metadata(&image).unwrap().len() > 0);
        let mut installed = launch(
            SetupApp::for_shot("setup-desktop-installed"),
            ImportApp::new,
        );
        installed.harness().set_theme(theme);
        installed
            .harness()
            .expect(
                &SemanticExpectation::named("setup.desktop.status")
                    .name_contains("choose Tilo on the login screen"),
            )
            .unwrap();
        let image = journey_shot_root().join(format!("installed-{name}.png"));
        installed.harness().render_png(&image);
        assert!(std::fs::metadata(&image).unwrap().len() > 0);
    }
}
