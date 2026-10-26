//! What Windows Apps does on this machine, as the operations its typed
//! effects name: which picker, service call or appliance command each one
//! reaches, and which the catalogue or the appliance refuses.

use super::*;

/// A window whose sample service reports every maintenance operation as
/// available, as it does while it is still following an installation.
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

/// Showing an app selects its row and asks the service for nothing;
/// launching one goes through the route the catalogue reports, including
/// the separately authorized appliance fallback.
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

/// Maintenance effects reach the service with the id or private capsule
/// they name, and leave the service to report the outcome.
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

/// Uninstall names the capsule of a Wine app and the entry itself of an
/// app in the appliance, as the window offers it.
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

/// Install, ISO, file, publish and the pending install's two
/// destinations reach the picker or the service that owns each; the
/// service's own answer is what comes back, never a claim made here.
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
        path: Some(installer.display().to_string()),
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
            path: Some(document.display().to_string()),
        }),
        Err("Opening portal files is unavailable for this Windows route.".to_owned())
    );
    let (services, recorder) = ui::host::HostServices::recording();
    for _ in 0..3 {
        recorder.respond_to_open_file(OpenFileOutcome::Cancelled);
    }
    ui::host::scoped(&services, || {
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

/// Every operation an effect can name is refused with the catalogue's own
/// reason when it does not offer it now: an effect re-resolves what it
/// asks for instead of trusting its invoker.
#[test]
fn operations_the_catalogue_reports_unavailable_are_refused() {
    use actions::WindowsEffect as E;
    const REFUSED: &str = "Windows operation is no longer available";
    const GONE: &str = "Windows application is no longer available";
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
        assert_eq!(app.perform_effect(&effect), Err(REFUSED.to_owned()));
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
        assert_eq!(app.perform_effect(&effect), Err(GONE.to_owned()));
    }
    assert_eq!(
        app.perform_effect(&E::Show { app: "gone".into() }),
        Err("the application is no longer in the catalogue".to_owned())
    );
}

/// The appliance's own status and this session's ownership decide its
/// operations; a window that only reports a status runs none of them.
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
            path: Some(iso.display().to_string()),
        }),
        Err(APPLIANCE_OFFLINE.to_owned())
    );
}
