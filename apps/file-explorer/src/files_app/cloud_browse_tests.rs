use super::*;

// Cloud import stages through descriptor-relative paths only Linux provides.
#[cfg(target_os = "linux")]
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one sequential action, portal and protocol scenario keeps the observable transfer lifecycle together"
)]
fn provider_actions_choose_destination_and_import_office_bytes_without_mount() {
    use std::io::Write;
    use tilo_cloud_files::cloud::{CLOUD_FRAMING, CloudRequest, CloudResponse, ImportReadState};
    let mut env = tilo_test_env::ScopedEnv::new();
    let root = tempfile::tempdir().unwrap();
    let runtime = tilo_test_env::runtime_tempdir().unwrap();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let destination = root.path().join("Chosen");
    std::fs::create_dir(&destination).unwrap();
    let socket = tilo_cloud_files::cloud::files_socket_in(runtime.path());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut office = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    office
        .start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    office
        .write_all(b"<document>Independent document</document>")
        .unwrap();
    let bytes = office.finish().unwrap().into_inner();
    let exported = bytes.clone();
    let source = ImportEntry {
        id: "native".into(),
        name: "Document.docx".into(),
        kind: ImportKind::NativeExport,
        revision: Some("v1".into()),
        size: None,
        content_writable: false,
    };
    let projected = source.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            while let Some(request) = CLOUD_FRAMING
                .read_value::<_, CloudRequest>(&mut stream)
                .unwrap()
            {
                let CloudRequest::Import { account, request } = request else {
                    panic!("not cloud import")
                };
                assert_eq!(account, "account");
                let reply = match request {
                    ImportRequest::Browse {
                        folder: None,
                        offset: 0,
                    } => ImportReply::Page {
                        folder: "root".into(),
                        entries: vec![projected.clone()],
                        next_offset: None,
                    },
                    ImportRequest::StartRead { id, revision } => {
                        assert_eq!(id, "native");
                        assert_eq!(revision.as_deref(), Some("v1"));
                        ImportReply::Preparing {
                            session: "read".into(),
                        }
                    }
                    ImportRequest::ReadStatus { .. } => {
                        ImportReply::ReadState(ImportReadState::Ready {
                            size: exported.len() as u64,
                        })
                    }
                    ImportRequest::ReadChunk { offset: 0, .. } => ImportReply::Chunk {
                        bytes: exported.clone(),
                        eof: true,
                    },
                    ImportRequest::CloseRead { .. } => ImportReply::Closed,
                    _ => panic!("unexpected request"),
                };
                CLOUD_FRAMING
                    .write_value(&mut stream, &CloudResponse::Import(reply))
                    .unwrap();
            }
        }
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
    assert_eq!(files.state().cloud.entries, vec![source]);
    assert!(!root.path().join("Remote").exists());
    assert_eq!(files.state().cloud_resources().len(), 1);
    files
        .state()
        .run_effect(FilesEffect::SelectCloud {
            entry: "native".into(),
        })
        .unwrap();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(destination.clone()));
    files
        .state()
        .run_effect(FilesEffect::ImportCloud { folder: None })
        .unwrap();
    assert!(recorder.pick_requests()[0].is_writable_directory());
    tilo_test_env::wait_until("the cloud import to settle", || {
        files.state().poll_cloud();
        let state = files.state();
        state.cloud.chooser.is_none() && state.cloud.transfer.is_none()
    });
    server.join().unwrap();
    assert_eq!(
        std::fs::read(destination.join("Document.docx")).unwrap(),
        bytes
    );
    assert_eq!(files.state().cloud.progress.bytes, bytes.len() as u64);
    std::fs::write(destination.join("Document.docx"), b"independent local edit").unwrap();
    let outcome = files
        .state()
        .local_outcomes
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(outcome.is_done());
    assert!(
        matches!(&outcome.result()[0], crate::file_ops::Change::CloudImported{from,to,..} if from == "cloud:account:native" && Path::new(to) == destination.join("Document.docx"))
    );
    recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
    files
        .state()
        .run_effect(FilesEffect::ImportCloud { folder: None })
        .unwrap();
    files.state().poll_cloud();
    assert!(files.state().cloud.transfer.is_none());
    assert_eq!(
        files.state().local_outcomes.len(),
        1,
        "cancelled chooser never queues a copy"
    );
    files.state().run_effect(FilesEffect::RevealCloud).unwrap();
    assert_eq!(files.state().current_path(), destination);
}

#[test]
fn explicit_import_folder_is_pinned_without_a_chooser_and_is_not_source_consent() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("Copies");
    std::fs::create_dir(&destination).unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
    files.state().cloud.active = true;
    files.state().cloud.account = "private-account".into();
    files.state().cloud.entries = vec![ImportEntry {
        id: "remote".into(),
        name: "Document.docx".into(),
        kind: ImportKind::NativeExport,
        revision: Some("revision".into()),
        size: None,
        content_writable: false,
    }];
    files.state().cloud.selected.insert("remote".into());
    files
        .state()
        .run_effect(FilesEffect::ImportCloud {
            folder: Some(destination.display().to_string()),
        })
        .unwrap();
    assert!(recorder.pick_requests().is_empty());
    assert!(files.state().cloud.chooser.is_none());
    let crate::file_ops::FileOp::CloudImport {
        folder,
        destination: pinned,
        ..
    } = files.state().cloud.retry.clone().unwrap()
    else {
        panic!("explicit destination must queue a cloud import")
    };
    assert_eq!(Path::new(&folder), destination);
    assert_eq!(
        pinned,
        crate::cloud_transfer::pin_destination(&folder, std::iter::empty()).unwrap()
    );
    tilo_test_env::wait_until("the cloud transfer to settle", || {
        files.state().poll_cloud();
        files.state().cloud.transfer.is_none()
    });
    assert!(
        files
            .state()
            .local_outcomes
            .values()
            .all(|outcome| matches!(outcome,
                ui::tilo_resource::Settled::Failed { reason, partial }
                    if reason.contains("host person") && partial.is_empty()
            ))
    );
    assert_eq!(files.state().local_outcomes.len(), 1);
    assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
}

fn browse_test_account(id: &str) -> crate::remote_accounts::RemoteAccount {
    crate::remote_accounts::RemoteAccount {
        id: id.into(),
        kind: crate::remote_accounts::RemoteAccountKind::GoogleDrive,
        label: "Drive".into(),
        host: "drive.google.com".into(),
        health: crate::remote_accounts::RemoteAccountHealth::Healthy,
        mount: None,
        files_mode: accounts::cloud_types::FilesMode::Import,
    }
}

fn browse_test_entry() -> ImportEntry {
    ImportEntry {
        id: "document".into(),
        name: "Document.docx".into(),
        kind: ImportKind::NativeExport,
        revision: None,
        size: None,
        content_writable: false,
    }
}

#[test]
fn replacing_folder_or_account_detaches_pending_pages_and_selection() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let root = tempfile::tempdir().unwrap();
    let runtime = tilo_test_env::runtime_tempdir().unwrap();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
    files.state().startup_accounts = Some(vec![
        browse_test_account("first"),
        browse_test_account("second"),
    ]);
    for (account, folder) in [("first", Some("child")), ("second", None)] {
        files.state().cloud.active = true;
        files.state().cloud.account = "first".into();
        files.state().cloud.entries = vec![browse_test_entry()];
        files.state().cloud.selected.insert("document".into());
        files.state().cloud.next = Some(20);
        let (old, pending) = channel();
        files.state().cloud.pending = Some(pending);
        files
            .state()
            .run_effect(FilesEffect::BrowseCloud {
                account: account.into(),
                folder: folder.map(str::to_owned),
            })
            .unwrap();
        assert!(files.state().cloud.entries.is_empty());
        assert!(files.state().cloud.selected.is_empty());
        assert_eq!(files.state().cloud.next, None);
        assert_eq!(files.state().cloud.account, account);
        assert_eq!(files.state().cloud.folder.as_deref(), folder);
        assert!(
            old.send(Ok(ImportReply::Page {
                folder: "old".into(),
                entries: vec![browse_test_entry()],
                next_offset: Some(20),
            }))
            .is_err()
        );
        assert!(files.state().run_effect(FilesEffect::MoreCloud).is_err());
        tilo_test_env::wait_until("the cloud listing to settle", || {
            files.state().poll_cloud();
            files.state().cloud.pending.is_none()
        });
        assert!(files.state().cloud.entries.is_empty());
    }
}

#[test]
fn local_navigation_invalidates_page_and_destination_chooser() {
    let root = tempfile::tempdir().unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
    files.state().cloud.active = true;
    files.state().cloud.account = "account".into();
    files.state().cloud.entries = vec![browse_test_entry()];
    files
        .state()
        .run_effect(FilesEffect::SelectCloud {
            entry: "document".into(),
        })
        .unwrap();
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(root.path().into()));
    files
        .state()
        .run_effect(FilesEffect::ImportCloud { folder: None })
        .unwrap();
    let (old, pending) = channel();
    files.state().cloud.pending = Some(pending);
    files.state().navigate(root.path().into());
    assert!(!files.state().cloud.active);
    assert!(files.state().cloud.chooser.is_none());
    assert!(files.state().cloud.proposal.is_empty());
    assert!(
        old.send(Ok(ImportReply::Page {
            folder: "old".into(),
            entries: vec![browse_test_entry()],
            next_offset: None,
        }))
        .is_err()
    );
    files.state().poll_cloud();
    assert!(files.state().cloud_resources().is_empty());
    assert!(files.state().cloud.transfer.is_none());
    assert!(files.state().awaiting_ops.is_empty());
    assert!(
        files
            .state()
            .run_effect(FilesEffect::ImportCloud { folder: None })
            .is_err()
    );
    assert!(
        files
            .state()
            .run_effect(FilesEffect::SelectCloud {
                entry: "document".into()
            })
            .is_err()
    );
}

#[test]
fn next_page_appends_only_to_current_folder_and_disconnect_finishes_loading() {
    let files = FileExplorerWidget::with_persistence(None, None);
    files.state().cloud.active = true;
    files.state().cloud.account = "account".into();
    files.state().cloud.folder = Some("folder".into());
    files.state().cloud.entries = vec![browse_test_entry()];
    files.state().cloud.selected.insert("document".into());
    let (page, pending) = channel();
    files.state().cloud.pending = Some(pending);
    let mut second = browse_test_entry();
    second.id = "second".into();
    page.send(Ok(ImportReply::Page {
        folder: "folder".into(),
        entries: vec![second],
        next_offset: Some(40),
    }))
    .unwrap();
    assert!(files.state().poll_cloud());
    assert_eq!(files.state().cloud.entries.len(), 2);
    assert!(files.state().cloud.selected.contains("document"));
    assert_eq!(files.state().cloud.next, Some(40));
    let (page, pending) = channel();
    files.state().cloud.pending = Some(pending);
    drop(page);
    assert!(files.state().poll_cloud());
    assert!(files.state().cloud.pending.is_none());
    assert!(files.state().cloud.error.is_some());
}

#[test]
fn legacy_cloud_links_do_not_offer_document_import() {
    let root = tempfile::tempdir().unwrap();
    let files = FileExplorerWidget::with_persistence(None, Some(root.path().into()));
    let mut account = browse_test_account("account");
    account.files_mode = accounts::cloud_types::FilesMode::Sync;
    account.mount = Some(root.path().into());
    files.state().startup_accounts = Some(vec![account]);
    for name in [
        "Document.gdoc",
        "Sheet.GSHEET",
        "Slides.gslides",
        "Link.url",
    ] {
        let path = root.path().join(name);
        std::fs::write(&path, b"browser link").unwrap();
        let entries = files.state().read_directory(root.path()).unwrap();
        let entry = entries.iter().find(|entry| entry.path == path).unwrap();
        assert!(
            !files
                .state()
                .entry_resource(entry)
                .actions
                .iter()
                .any(|action| action.id == "import-once")
        );
    }
    assert!(
        files
            .state()
            .importable_cloud_entry(&root.path().join("Document.docx"))
    );
}

/// A cloud folder is named by the window's bar as the account and the folder in
/// it, and the window draws no header of its own while the bar hosts it.
#[test]
fn the_window_bar_names_the_cloud_folder() {
    let root = tempfile::tempdir().unwrap();
    let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().to_path_buf()));
    files.state().header_in_bar = true;
    files.state().debug_show_cloud_folder();
    files.with_state(FilesState::refresh_cloud_header);
    assert!(
        !files.state().cloud.header_shown,
        "no cloud title in the window while the bar names the place"
    );
    let location = files
        .with_state(|state, chrome| state.window_toolbar(chrome))
        .location
        .expect("a location");
    assert_eq!(location.label, "Trip to Lisbon");
    assert_eq!(
        location
            .segments
            .iter()
            .map(|segment| segment.label.as_str())
            .collect::<Vec<_>>(),
        ["Drive", "Trip to Lisbon"]
    );
    assert!(!location.editable, "a cloud folder has no path to type");
}

/// A cloud folder the window named itself before the bar hosted it moves
/// into the bar: the bar is told the account and the folder, and the window
/// stops drawing its own title for it.
#[test]
fn a_cloud_folder_named_before_the_bar_hosts_the_window_moves_into_the_bar() {
    let root = tempfile::tempdir().unwrap();
    let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().to_path_buf()));
    ui::window_toolbar::set_hosted(false);
    files.state().debug_show_cloud_folder();
    files.with_state(FilesState::sync_window_toolbar);
    files.with_state(FilesState::refresh_cloud_header);
    assert!(
        files.state().cloud.header_shown,
        "outside a Tilo session the window names the cloud folder"
    );

    ui::window_toolbar::set_hosted(true);
    files.with_state(FilesState::sync_window_toolbar);
    files.with_state(FilesState::refresh_cloud_header);
    assert!(
        !files.state().cloud.header_shown,
        "the window draws no cloud title while the bar names the place"
    );
    let location = ui::window_toolbar::declared()
        .and_then(|toolbar| toolbar.location)
        .expect("the bar is told the place");
    assert_eq!(location.label, "Trip to Lisbon");
    assert_eq!(
        location
            .segments
            .iter()
            .map(|segment| segment.label.as_str())
            .collect::<Vec<_>>(),
        ["Drive", "Trip to Lisbon"]
    );
}

/// The cloud folder as a window shows it, its place in the window's bar,
/// in dark and light, for a person to look at: each picture is printed as
/// hex between `capture <name>` and `end capture` lines. Run on request
/// (`--ignored --nocapture`).
#[test]
#[ignore = "prints captures for review"]
fn a_cloud_folder_capture_shows_its_place_in_the_bar() {
    for (name, theme) in [("dark", ui::Theme::dark()), ("light", ui::Theme::light())] {
        let root = tempfile::tempdir().unwrap();
        let files = FileExplorerWidget::with_persistence(None, Some(root.path().to_path_buf()));
        let mut harness =
            ui::headless::FrameHarness::for_app(ui::State::new(), files, (1200, 720), theme);
        harness.present_in_window_bar();
        harness
            .root_mut::<FileExplorerWidget>()
            .state()
            .debug_show_cloud_folder();
        for _ in 0..3 {
            harness.recompose().unwrap();
        }
        assert_eq!(
            ui::window_toolbar::declared()
                .and_then(|toolbar| toolbar.location)
                .map(|location| location.label)
                .as_deref(),
            Some("Trip to Lisbon"),
            "the bar is told the cloud folder"
        );
        let out = root.path().join("capture.png");
        harness.render_window_png(&out);
        let png = std::fs::read(&out).unwrap();
        println!("capture cloud-folder-{name}");
        let hex = png.iter().fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        println!("{hex}");
        println!("end capture");
    }
}
