use super::*;
use std::os::unix::fs::PermissionsExt;
use tilo_cloud_files::cloud::{CLOUD_FRAMING, CloudRequest, CloudResponse};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one sequential metadata, resource and destination-chooser scenario verifies the same mounted files throughout"
)]
fn synced_native_rows_describe_sources_and_choose_independent_editable_copies() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let root = tempfile::tempdir().unwrap();
    let runtime = tilo_test_env::runtime_tempdir().unwrap();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let mount = root.path().join("Drive");
    let destination = root.path().join("Copies");
    std::fs::create_dir(&mount).unwrap();
    std::fs::create_dir(&destination).unwrap();
    let sources = [
        ("Document.docx", "document", "Google Docs"),
        ("Table.xlsx", "spreadsheet", "Google Sheets"),
        ("Deck.pptx", "presentation", "Google Slides"),
    ];
    for (name, _, _) in sources {
        let path = mount.join(name);
        std::fs::write(&path, b"converted bytes").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    std::fs::write(mount.join("Uploaded.docx"), b"uploaded bytes").unwrap();
    let socket = tilo_cloud_files::cloud::files_socket_in(runtime.path());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    let described = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&described);
    let server = std::thread::spawn(move || {
        // The window's own status follower shares this socket: answer its listing, and serve
        // the descriptions the mounted rows ask for, each client on its own thread.
        let mut served = 0;
        while served < 2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            served += 1;
            let counted = std::sync::Arc::clone(&counted);
            std::thread::spawn(move || {
                while let Ok(Some(request)) =
                    CLOUD_FRAMING.read_value::<_, CloudRequest>(&mut stream)
                {
                    let reply = match request {
                        CloudRequest::List => CloudResponse::Mounts(Vec::new()),
                        CloudRequest::File {
                            account,
                            request: FileRequest::Describe { path },
                        } => {
                            assert_eq!(account, "account");
                            let native = sources.iter().find(|(name, _, _)| *name == path);
                            let description = FileDescription {
                                source_mime: Some(native.map_or_else(
                                    || {
                                        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                                            .into()
                                    },
                                    |(_, kind, _)| format!("application/vnd.google-apps.{kind}"),
                                )),
                                converted: native.is_some(),
                                content_writable: native.is_none(),
                            };
                            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            CloudResponse::File(FileReply::Description(description))
                        }
                        other => panic!("unexpected mounted request: {other:?}"),
                    };
                    if CLOUD_FRAMING.write_value(&mut stream, &reply).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let (services, recorder) = ui::process::ProcessServices::recording();
    let _host = ui::process::enter(&services);
    let mut app = ui::prelude::TestApp::<crate::FileExplorerShell>::new().unwrap();
    app.with_surface(|files: &mut FileExplorerWidget| {
        files.state().home_path = Some(mount.clone());
        files.state().startup_accounts = Some(vec![crate::remote_accounts::RemoteAccount {
            id: "account".into(),
            kind: crate::remote_accounts::RemoteAccountKind::GoogleDrive,
            label: "Drive".into(),
            host: "drive.google.com".into(),
            health: crate::remote_accounts::RemoteAccountHealth::Healthy,
            files_mode: accounts::cloud_types::FilesMode::Sync,
            mount: Some(mount.clone()),
        }]);
        files.state().navigate(mount.clone());
        files.state().poll_mounted_cloud();
        let replies = files
            .state()
            .mounted_cloud
            .pending
            .take()
            .unwrap()
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let (tx, rx) = channel();
        tx.send(replies).unwrap();
        files.state().mounted_cloud.pending = Some(rx);
        assert!(files.state().poll_mounted_cloud());
        server.join().unwrap();
        for (name, _, source) in sources {
            let entry = files
                .state()
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .clone();
            let row = files.state().entry_resource(&entry);
            let mut row = row.into_resource();
            ui::framework::apply_row_statuses(std::slice::from_mut(&mut row), &|id| {
                files.state().row_status(id)
            });
            let presentation = row.presentation.unwrap();
            assert!(presentation.detail.unwrap().contains(source));
            assert!(presentation.preview.unwrap().contains("independent"));
            assert_eq!(
                row.actions
                    .iter()
                    .find(|action| action.id == "open")
                    .unwrap()
                    .label,
                t("files-mounted-open")
            );
            assert!(
                row.actions
                    .iter()
                    .any(|action| action.id == "import-converted")
            );
        }
        let uploaded = files
            .state()
            .entries
            .iter()
            .find(|entry| entry.name == "Uploaded.docx")
            .unwrap()
            .clone();
        assert!(
            files
                .state()
                .entry_resource(&uploaded)
                .into_resource()
                .actions
                .iter()
                .all(|a| a.id != "import-converted")
        );
    })
    .unwrap();
    let id = crate::search_provider::entry_resource_id(&mount.join("Document.docx"));
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    app.call_on(&id, "import-converted", serde_json::json!({}))
        .unwrap();
    assert!(recorder.pick_requests()[0].is_writable_directory());
    tilo_test_env::wait_until("the converted import to land", || {
        app.settle();
        destination.join("Document.docx").exists() && app.state().ops.pending().next().is_none()
    });
    app.with_surface(|files: &mut FileExplorerWidget| {
        assert!(
            destination.join("Document.docx").exists(),
            "status: {:?}; outcomes: {:?}",
            files.state().status,
            files.state().local_outcomes
        );
    })
    .unwrap();
    assert_eq!(
        std::fs::read(destination.join("Document.docx")).unwrap(),
        b"converted bytes"
    );
    assert_ne!(
        std::fs::metadata(destination.join("Document.docx"))
            .unwrap()
            .permissions()
            .mode()
            & 0o200,
        0
    );
    assert_eq!(
        std::fs::metadata(mount.join("Document.docx"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222,
        0
    );
}
