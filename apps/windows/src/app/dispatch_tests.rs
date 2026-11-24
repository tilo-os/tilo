//! What Windows Apps does on this machine, as the operations its typed effects name: which
//! picker, service call or appliance command each one reaches, and which the catalogue or
//! the appliance refuses.

use super::*;

/// A window whose sample service reports every maintenance operation as available, as it
/// does while it is still following an installation.
fn window_offering_every_operation() -> WindowsApp {
    let mut app = WindowsApp::new_offline();
    for item in &mut app.catalogue.apps {
        item.can_retry_install = true;
        item.can_escalate_install_to_vm = true;
        item.can_remove_launcher = true;
        item.can_vendor_uninstall = true;
        item.can_delete_data = true;
        item.can_publish_vm_candidate = true;
    }
    app
}

/// Showing an app selects its row and asks the service for nothing; launching one goes
/// through the route the catalogue reports, including the separately authorized appliance
/// fallback.
#[test]
fn show_and_launch_effects_reach_the_selection_and_the_route() {
    use actions::WindowsEffect as E;
    let requested = Some(Notice::info(strings::t("windows-operation-requested")));
    let mut app = window_offering_every_operation();
    app.perform_effect(&E::Show {
        app: "visual-studio".into(),
    })
    .expect("showing an app selects its row");
    assert_eq!(
        app.selected_app().map(|item| item.id.as_str()),
        Some("visual-studio")
    );
    assert_eq!(app.notice, None, "a selection asks the service for nothing");
    app.perform_effect(&E::Launch {
        app: "adobe-reader".into(),
    })
    .expect("a launchable app launches");
    app.perform_effect(&E::LaunchVmFallback {
        app: "visual-studio".into(),
    })
    .expect("a VM fallback is its own operation");
    assert_eq!(app.notice, requested);
    for candidate in ["adobe-reader", "visual-studio"] {
        assert_eq!(
            app.provider
                .borrow_mut()
                .catalogue()
                .app(candidate)
                .unwrap()
                .status,
            AppStatus::Running,
            "{candidate} was launched through the service"
        );
    }
}

/// A click with no session to take its request is performed in this window instead of waiting on an answer
/// that never comes, and the banner says what happened rather than that a request is pending.
#[test]
fn a_click_with_nobody_to_ask_acts_where_it_is() {
    let mut app = WindowsApp::new_offline();
    app.use_provider(Box::new(
        crate::provider::SampleProvider::new().offering_native_controls(),
    ));
    app.apply_intent(Intent::SelectApp(0));
    app.notice = Some(Notice::info(strings::t("windows-request-sent")));
    app.apply_intent(Intent::Launch);
    assert_eq!(
        app.provider
            .borrow_mut()
            .catalogue()
            .app("adobe-reader")
            .map(|app| app.status),
        Some(AppStatus::Running),
        "the launch happened here: there is no dispatcher to hand it to"
    );
    assert_ne!(
        app.notice.as_ref().map(|notice| notice.text.clone()),
        Some(strings::t("windows-request-sent")),
        "and the banner is about what happened, not a request nobody takes"
    );
}

/// A banner belongs to the state it describes: the person's next action takes an old one down,
/// even when that action is refused.
#[test]
fn a_banner_goes_with_the_next_action() {
    let mut app = WindowsApp::new_offline();
    app.notice = Some(Notice::info(strings::t("windows-request-sent")));
    app.apply_intent(Intent::ConfirmCapsule);
    assert!(
        app.notice.is_none(),
        "nothing waits for approval once the next action is taken"
    );
}

/// Maintenance effects reach the service with the id or private capsule they name, and
/// leave the service to report the outcome.
#[test]
fn maintenance_effects_reach_the_service_by_id_and_capsule() {
    use actions::WindowsEffect as E;
    let mut app = window_offering_every_operation();
    app.perform_effect(&E::RetryInstall {
        capsule: "Adobe Reader".into(),
    })
    .expect("a retry reaches the capsule's app");
    app.perform_effect(&E::EscalateInstall {
        capsule: "Adobe Reader".into(),
    })
    .expect("an escalation reaches the capsule's app");
    assert_eq!(
        app.provider
            .borrow_mut()
            .catalogue()
            .app("adobe-reader")
            .unwrap()
            .fallback_detail,
        "The installer is opening in the Windows appliance."
    );
    app.perform_effect(&E::RemoveLauncher {
        app: "adobe-reader".into(),
    })
    .expect("a launcher removal reaches the service");
    assert!(
        app.provider
            .borrow_mut()
            .catalogue()
            .app("adobe-reader")
            .is_none(),
        "the service removed the launcher it was told to"
    );
    let mut data = window_offering_every_operation();
    data.perform_effect(&E::DeleteData {
        capsule: "Adobe Reader".into(),
    })
    .expect("deleting data reaches the capsule");
    assert!(
        data.provider
            .borrow_mut()
            .catalogue()
            .app("adobe-reader")
            .is_none()
    );
}

/// Uninstall names the capsule of a Wine app and the entry itself of an app in the
/// appliance, as the window offers it.
#[test]
fn uninstall_effects_reach_the_entry_their_route_owns() {
    use actions::WindowsEffect as E;
    let mut wine = window_offering_every_operation();
    wine.perform_effect(&E::Uninstall {
        app: "adobe-reader".into(),
    })
    .expect("a vendor uninstall reaches the capsule");
    assert_eq!(
        wine.provider
            .borrow_mut()
            .catalogue()
            .app("adobe-reader")
            .unwrap()
            .status,
        AppStatus::Uninstalled
    );
    let mut vm = window_offering_every_operation();
    vm.perform_effect(&E::Uninstall {
        app: "visual-studio".into(),
    })
    .expect("an appliance app is uninstalled by its entry");
    assert!(
        vm.provider
            .borrow_mut()
            .catalogue()
            .app("visual-studio")
            .is_none()
    );
}

/// Install, ISO, file, publish and the pending install's two destinations reach the picker
/// or the service that owns each; the service's own answer is what comes back, never a
/// claim made here.
#[test]
fn install_and_file_effects_reach_the_picker_or_the_service() {
    use actions::WindowsEffect as E;
    let mut app = window_offering_every_operation();
    assert_eq!(
        app.perform_effect(&E::Publish {
            app: "adobe-reader".into(),
        }),
        Err("No managed Windows installation is awaiting publication.".to_owned())
    );
    assert_eq!(
        app.perform_effect(&E::InstallPrivateCapsule),
        Err("Choose an installer first.".to_owned())
    );
    assert_eq!(
        app.perform_effect(&E::InstallWindowsAppliance),
        Err("Choose an installer first.".to_owned())
    );
    let directory = tempfile::tempdir().unwrap();
    let installer = directory.path().join("setup.msi");
    std::fs::write(&installer, b"installer bytes").unwrap();
    app.perform_effect(&E::Install {
        path: Some(ui::tilo_resource::AbsolutePath::new(&installer).unwrap()),
    })
    .expect("an installer path installs");
    assert!(
        app.provider
            .borrow_mut()
            .catalogue()
            .app("imported-sample")
            .is_some(),
        "the service took the installer it was handed"
    );
    let document = directory.path().join("document.bin");
    std::fs::write(&document, b"document bytes").unwrap();
    assert_eq!(
        app.perform_effect(&E::OpenFile {
            app: "visual-studio".into(),
            path: Some(ui::tilo_resource::AbsolutePath::new(&document).unwrap()),
        }),
        Err("Opening portal files is unavailable for this Windows route.".to_owned())
    );
    let (services, recorder) = ui::process::ProcessServices::recording();
    for _ in 0..3 {
        recorder.respond_to_open_file(OpenFileOutcome::Cancelled);
    }
    ui::process::scoped(&services, || {
        app.perform_effect(&E::Install { path: None })
            .expect("an install without a path asks for one");
        app.perform_effect(&E::ImportIso { path: None })
            .expect("an ISO import without a path asks for one");
        app.perform_effect(&E::OpenFile {
            app: "visual-studio".into(),
            path: None,
        })
        .expect("a file operation without a path asks for one");
    });
    let requests = recorder.pick_requests();
    let titles: Vec<&str> = requests.iter().map(PickRequest::title).collect();
    assert_eq!(
        titles,
        [
            "Add a Windows application",
            "Choose Windows installation media",
            "Open a file in Windows"
        ]
    );
    assert_eq!(app.document_target.as_deref(), Some("visual-studio"));
    app.drain_portal();
    app.drain_iso_portal();
    app.drain_document_portal();
}

/// Every operation an effect can name is refused with the catalogue's own reason when it
/// does not offer it now: an effect re-resolves what it asks for instead of trusting its
/// invoker.
#[test]
fn operations_the_catalogue_reports_unavailable_are_refused() {
    use actions::WindowsEffect as E;
    let refused = strings::t("windows-error-operation-gone");
    let gone = strings::t("windows-error-app-gone");
    let mut app = window_offering_every_operation();
    for (name, effect) in [
        (
            "adobe-reader",
            E::Launch {
                app: "adobe-reader".into(),
            },
        ),
        (
            "visual-studio",
            E::LaunchVmFallback {
                app: "visual-studio".into(),
            },
        ),
        (
            "adobe-reader",
            E::Publish {
                app: "adobe-reader".into(),
            },
        ),
        (
            "adobe-reader",
            E::RetryInstall {
                capsule: "Adobe Reader".into(),
            },
        ),
        (
            "adobe-reader",
            E::EscalateInstall {
                capsule: "Adobe Reader".into(),
            },
        ),
        (
            "adobe-reader",
            E::RemoveLauncher {
                app: "adobe-reader".into(),
            },
        ),
        (
            "adobe-reader",
            E::Uninstall {
                app: "adobe-reader".into(),
            },
        ),
        (
            "adobe-reader",
            E::DeleteData {
                capsule: "Adobe Reader".into(),
            },
        ),
    ] {
        let item = app.catalogue.app_mut(name).unwrap();
        item.launch_available = false;
        item.vm_fallback_available = false;
        item.can_publish_vm_candidate = false;
        item.can_retry_install = false;
        item.can_escalate_install_to_vm = false;
        item.can_remove_launcher = false;
        item.can_vendor_uninstall = false;
        item.can_delete_data = false;
        assert_eq!(app.perform_effect(&effect), Err(refused.clone()));
    }
    assert_eq!(
        app.provider
            .borrow_mut()
            .catalogue()
            .app("adobe-reader")
            .unwrap()
            .status,
        AppStatus::Ready,
        "a refused operation leaves the service's app untouched"
    );
    for effect in [
        E::Launch { app: "gone".into() },
        E::RetryInstall {
            capsule: "gone".into(),
        },
        E::DeleteData {
            capsule: "gone".into(),
        },
    ] {
        assert_eq!(app.perform_effect(&effect), Err(gone.clone()));
    }
    // Showing an application is looking it up, so a gone one is refused as
    // the catalogue refuses it everywhere else.
    assert_eq!(
        app.perform_effect(&E::Show { app: "gone".into() }),
        Err(gone)
    );
}

/// The appliance's own status and this session's ownership decide its operations; a window
/// that only reports a status runs none of them.
#[test]
fn appliance_operations_are_refused_when_this_session_cannot_manage_it() {
    use actions::WindowsEffect as E;
    const APPLIANCE_OFFLINE: &str = "The Windows appliance can't be managed in this session.";
    let mut app = window_offering_every_operation();
    for effect in [
        E::Recovery,
        E::Start,
        E::Restart,
        E::Suspend,
        E::Resume,
        E::Shutdown,
        E::TemporaryNetwork,
        E::UpdateNetwork,
        E::RevokeNetwork,
    ] {
        assert_eq!(
            app.perform_effect(&effect),
            Err(APPLIANCE_OFFLINE.to_owned())
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let iso = directory.path().join("windows.iso");
    std::fs::write(&iso, b"iso bytes").unwrap();
    assert_eq!(
        app.perform_effect(&E::ImportIso {
            path: Some(ui::tilo_resource::AbsolutePath::new(&iso).unwrap()),
        }),
        Err(APPLIANCE_OFFLINE.to_owned())
    );
}

/// Stands in for the session's compatibility service on `stream`, as the manager's own client
/// reaches it: it asks where an installer goes, installs it in a private capsule once told to,
/// and lists it from then on (or once the test sets `installed`, as a setup finishing by itself
/// does). Each request's method is noted in `asked`.
fn stand_in_service(
    mut stream: std::os::unix::net::UnixStream,
    installed: &std::sync::atomic::AtomicBool,
    asked: &std::sync::Mutex<Vec<String>>,
) {
    use compat_service::{
        InstallReceipt, InstallationStatus, ManagedInstallation, ProtocolRequest, ProtocolResponse,
        RouteKind, VmAvailability,
    };
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering;
    loop {
        let mut length = [0_u8; 4];
        if stream.read_exact(&mut length).is_err() {
            return;
        }
        let mut bytes = vec![0_u8; u32::from_le_bytes(length) as usize];
        if stream.read_exact(&mut bytes).is_err() {
            return;
        }
        let request: ProtocolRequest = serde_json::from_slice(&bytes).expect("a request");
        let answer = match &request {
            ProtocolRequest::RefreshVmInstallation { .. } => {
                ProtocolResponse::VmInstallationRefreshed { installation: None }
            }
            ProtocolRequest::RefreshVmUninstallation { .. } => {
                ProtocolResponse::VmUninstallationRefreshed {
                    uninstallation: None,
                }
            }
            ProtocolRequest::List { .. } => ProtocolResponse::Listed {
                applications: Vec::new(),
                installations: installed
                    .load(Ordering::SeqCst)
                    .then(|| ManagedInstallation {
                        capsule_id: "capsule".into(),
                        display_name: "Setup".into(),
                        route: RouteKind::Wine,
                        runner: "Wine".into(),
                        status: InstallationStatus::Installed,
                        program_count: 1,
                        can_retry: false,
                        can_vendor_uninstall: false,
                        can_delete_data: true,
                        detail: "Installed.".into(),
                    })
                    .into_iter()
                    .collect(),
                vm_availability: VmAvailability::unavailable("Not set up"),
                vm_installation: None,
                vm_uninstallation: None,
                ended: Vec::new(),
            },
            ProtocolRequest::Install { .. } => ProtocolResponse::VmInstallConfirmation {
                confirmation: compat_service::VmInstallConfirmation {
                    display_name: "Setup".into(),
                    detail: "Tilo recommends a private compatibility capsule.".into(),
                    recommended_route: RouteKind::Wine,
                },
            },
            ProtocolRequest::InstallInCapsule { .. } => {
                installed.store(true, Ordering::SeqCst);
                ProtocolResponse::Installed {
                    receipt: InstallReceipt {
                        capsule_id: "capsule".into(),
                        display_name: "Setup".into(),
                        route: RouteKind::Wine,
                        status: InstallationStatus::Installed,
                        discovered_programs: 1,
                        exit_code: Some(0),
                        detail: "Installed.".into(),
                    },
                }
            }
            other => panic!("the manager asked for nothing this flow needs: {other:?}"),
        };
        let method = serde_json::to_value(&request).expect("a request")["method"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        asked.lock().expect("the requests asked").push(method);
        let bytes = serde_json::to_vec(&answer).expect("an answer");
        let length = u32::try_from(bytes.len())
            .expect("a short answer")
            .to_le_bytes();
        if stream
            .write_all(&length)
            .and_then(|()| stream.write_all(&bytes))
            .is_err()
        {
            return;
        }
    }
}

/// An installer is taken, the window asks where it goes, and the private capsule the person then
/// approves is where the service installs it: the install answers once its destination is asked
/// for, so the approved choice is not refused as another operation still running.
#[test]
fn an_approved_private_capsule_installs_the_chosen_installer() {
    use actions::WindowsEffect as E;
    let (manager, service) = std::os::unix::net::UnixStream::pair().expect("a connection");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let asked = std::sync::Arc::clone(&asked);
        std::thread::spawn(move || {
            stand_in_service(service, &std::sync::atomic::AtomicBool::new(false), &asked);
        });
    }
    let mut app = tilo_app_test_host::TestApp::<crate::resource::WindowsShell>::new().unwrap();
    app.with_widget(|window: &mut WindowsWindow| {
        let provider = crate::provider::LiveProvider::over(
            compat_service::SessionClient::from_owned_fd(manager.into()),
            ui::WakeHandle::new(),
        )
        .expect("the worker starts");
        window.app.borrow_mut().use_provider(Box::new(provider));
    })
    .expect("the window over the stand-in service");
    let directory = tempfile::tempdir().unwrap();
    let installer = directory.path().join("setup.exe");
    std::fs::write(&installer, b"MZ installer bytes").unwrap();

    app.effect_confirmed(&E::Install {
        path: Some(ui::tilo_resource::AbsolutePath::new(&installer).unwrap()),
    })
    .expect("the install is answered once the window asks where it goes");
    assert!(
        app.effect_on(actions::PENDING_INSTALL_ROW, &E::InstallPrivateCapsule)
            .is_err(),
        "a private capsule needs the person's approval"
    );
    app.effect_on_confirmed(actions::PENDING_INSTALL_ROW, &E::InstallPrivateCapsule)
        .expect("the approved capsule installs");

    let methods = asked.lock().expect("the requests asked").clone();
    assert!(
        methods.iter().any(|method| method == "install_in_capsule"),
        "the service was asked to install in the capsule: {methods:?}"
    );
}

/// What the service answers reaches the window by itself: the worker wakes it with every answer,
/// and with a list that changed while nobody asked (a setup that finished), so the window shows it
/// without waiting for the person's next input.
#[test]
fn the_window_is_woken_by_what_the_service_answers() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (manager, service) = std::os::unix::net::UnixStream::pair().expect("a connection");
    let installed = Arc::new(AtomicBool::new(false));
    {
        let installed = Arc::clone(&installed);
        std::thread::spawn(move || {
            stand_in_service(service, &installed, &std::sync::Mutex::default());
        });
    }
    let scope = ui::WakeScope::new();
    let mut provider = crate::provider::LiveProvider::over(
        compat_service::SessionClient::from_owned_fd(manager.into()),
        scope.handle(),
    )
    .expect("the worker starts");
    let woken = |what: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !scope.is_pending() {
            assert!(
                std::time::Instant::now() < deadline,
                "the window was not woken by {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        scope.take_pending();
    };

    woken("the first list");
    assert_eq!(
        provider.catalogue().vm.detail,
        "Not set up",
        "the woken window takes in the list"
    );
    installed.store(true, Ordering::SeqCst);
    woken("a list that changed while nobody asked");
    assert!(
        provider
            .catalogue()
            .apps
            .iter()
            .any(|app| app.capsule == "capsule"),
        "the finished setup is shown"
    );
}
