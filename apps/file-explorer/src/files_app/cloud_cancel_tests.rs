use super::*;
use tilo_cloud_files::cloud::{CLOUD_FRAMING, CloudRequest, CloudResponse, ImportReadState};

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one sequential action, portal and protocol scenario keeps the observable transfer lifecycle together"
)]
fn cancel_action_closes_preparing_export_and_retry_records_only_failed_copy() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let root = tempfile::tempdir().unwrap();
    let runtime = tilo_test_env::runtime_tempdir().unwrap();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let destination = root.path().join("Chosen");
    std::fs::create_dir(&destination).unwrap();
    let socket = tilo_cloud_files::cloud::files_socket_in(runtime.path());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let entry = ImportEntry {
        id: "native".into(),
        name: "Copy.docx".into(),
        kind: ImportKind::NativeExport,
        revision: None,
        size: None,
        content_writable: false,
    };
    let projected = entry.clone();
    let (ready_tx, ready_rx) = channel();
    let server = std::thread::spawn(move || {
        let mut closed = 0;
        for attempt in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            while let Some(request) = CLOUD_FRAMING
                .read_value::<_, CloudRequest>(&mut stream)
                .unwrap()
            {
                let CloudRequest::Import { request, .. } = request else {
                    panic!("not import")
                };
                let reply = match request {
                    ImportRequest::Browse { .. } => ImportReply::Page {
                        folder: "root".into(),
                        entries: vec![projected.clone()],
                        next_offset: None,
                    },
                    ImportRequest::StartRead { .. } => ImportReply::Preparing {
                        session: "session".into(),
                    },
                    ImportRequest::ReadStatus { .. } if attempt == 1 => {
                        let _ = ready_tx.send(());
                        ImportReply::ReadState(ImportReadState::Preparing)
                    }
                    ImportRequest::ReadStatus { .. } => {
                        ImportReply::ReadState(ImportReadState::Ready { size: 5 })
                    }
                    ImportRequest::ReadChunk { .. } => ImportReply::Chunk {
                        bytes: b"bytes".to_vec(),
                        eof: true,
                    },
                    ImportRequest::CloseRead { .. } => {
                        closed += 1;
                        ImportReply::Closed
                    }
                };
                CLOUD_FRAMING
                    .write_value(&mut stream, &CloudResponse::Import(reply))
                    .unwrap();
            }
        }
        closed
    });
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let files = FileExplorerWidget::with_persistence(None, Some(root.path().to_path_buf()));
    files.state().cloud_fixture_consent = true;
    files.state().startup_accounts = Some(vec![crate::remote_accounts::RemoteAccount {
        id: "account".into(),
        kind: crate::remote_accounts::RemoteAccountKind::GoogleDrive,
        label: "Drive".into(),
        host: "drive.google.com".into(),
        health: crate::remote_accounts::RemoteAccountHealth::Healthy,
        mount: None,
        files_mode: accounts::cloud_types::FilesMode::Import,
    }]);
    files
        .state()
        .run_effect(FilesEffect::OpenAccount {
            account: "account".into(),
        })
        .unwrap();
    tilo_test_env::wait_until("the cloud listing to settle", || {
        files.state().poll_cloud();
        files.state().cloud.pending.is_none()
    });
    files
        .state()
        .run_effect(FilesEffect::SelectCloud { entry: entry.id })
        .unwrap();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    files
        .state()
        .run_effect(FilesEffect::ImportCloud { folder: None })
        .unwrap();
    files.state().poll_cloud();
    tilo_test_env::recv("the transfer to start", &ready_rx);
    files.state().run_effect(FilesEffect::CancelCloud).unwrap();
    tilo_test_env::wait_until("the cloud transfer to settle", || {
        files.state().poll_cloud();
        files.state().cloud.transfer.is_none()
    });
    assert!(!destination.join("Copy.docx").exists());
    assert!(files.state().cloud.retry.is_some());
    assert_eq!(files.state().cloud.progress.phase, "cancelled");
    let progress = files
        .state()
        .run_effect(FilesEffect::CloudProgress)
        .unwrap();
    assert!(matches!(progress,Ran::Done(Some(value)) if value["phase"] == "cancelled"));
    files.state().run_effect(FilesEffect::RetryCloud).unwrap();
    tilo_test_env::wait_until("the cloud transfer to settle", || {
        files.state().poll_cloud();
        files.state().cloud.transfer.is_none()
    });
    assert_eq!(server.join().unwrap(), 2);
    assert_eq!(
        std::fs::read(destination.join("Copy.docx")).unwrap(),
        b"bytes"
    );
    assert!(files.state().cloud.retry.is_none());
    assert_eq!(files.state().local_outcomes.len(), 2);
    assert_eq!(
        files
            .state()
            .local_outcomes
            .values()
            .filter(|outcome| outcome.is_done())
            .count(),
        1
    );
}
