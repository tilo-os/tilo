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
    env.set("XDG_RUNTIME_DIR", root.path());
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
    let socket = tilo_cloud_files::cloud::files_socket_in(root.path());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        for _ in 0..4 {
            let request = CLOUD_FRAMING
                .read_value::<_, CloudRequest>(&mut stream)
                .unwrap()
                .unwrap();
            let CloudRequest::File {
                account,
                request: FileRequest::Describe { path },
            } = request
            else {
                panic!("expected mounted metadata request")
            };
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
            CLOUD_FRAMING
                .write_value(
                    &mut stream,
                    &CloudResponse::File(FileReply::Description(description)),
                )
                .unwrap();
        }
    });
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = ui::prelude::TestApp::<crate::FileExplorerShell>::new().unwrap();
    app.with_surface(|files: &mut FileExplorerWidget| {
        files.home_path = Some(mount.clone());
        files.startup_accounts = Some(vec![crate::remote_accounts::RemoteAccount {
            id: "account".into(),
            kind: crate::remote_accounts::RemoteAccountKind::GoogleDrive,
            label: "Drive".into(),
            host: "drive.google.com".into(),
            health: crate::remote_accounts::RemoteAccountHealth::Healthy,
            files_mode: accounts::cloud_types::FilesMode::Sync,
            mount: Some(mount.clone()),
        }]);
        files.navigate(mount.clone());
        files.poll_mounted_cloud();
        let replies = files
            .mounted_cloud
            .pending
            .take()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (tx, rx) = channel();
        tx.send(replies).unwrap();
        files.mounted_cloud.pending = Some(rx);
        assert!(files.poll_mounted_cloud());
        server.join().unwrap();
        for (name, _, source) in sources {
            let entry = files
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap();
            let row = files.entry_resource(entry);
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
            .entries
            .iter()
            .find(|entry| entry.name == "Uploaded.docx")
            .unwrap();
        assert!(
            files
                .entry_resource(uploaded)
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
    tilo_test_env::wait_until("the converted import to settle", || {
        app.settle();
        destination.join("Document.docx").exists() && app.state().ops.pending().next().is_none()
    });
    app.with_surface(|files: &mut FileExplorerWidget| {
        assert!(
            destination.join("Document.docx").exists(),
            "status: {:?}; outcomes: {:?}",
            files.status,
            files.local_outcomes
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
