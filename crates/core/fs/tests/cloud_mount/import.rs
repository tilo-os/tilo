use super::*;
use accounts::cloud_types::{FilesMode, set_files_mode};
use runtime_fs::cloud::{
    IMPORT_CHUNK_LIMIT, ImportErrorKind, ImportKind, ImportReadState, ImportReply, ImportRequest,
};

/// The accounts a test's cloud service follows, with the fake drive's token as their sign-in:
/// the accounts the service would otherwise hear from the session.
struct DriveAccounts {
    kept: Mutex<Vec<accounts::Account>>,
    token: Arc<StaticCredential>,
}

impl DriveAccounts {
    fn new(accounts: Vec<accounts::Account>, token: &str) -> Arc<Self> {
        Arc::new(Self {
            kept: Mutex::new(accounts),
            token: bearer(token),
        })
    }
}

impl tilo_fs::cloud::AccountSource for DriveAccounts {
    fn accounts(&self) -> Vec<accounts::Account> {
        self.kept.lock().unwrap().clone()
    }

    fn watch(
        &self,
        _changed: Box<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<tilo_fs::cloud::AccountsWatch> {
        Ok(tilo_fs::cloud::AccountsWatch::still())
    }

    fn credential(&self, _account: &accounts::Account) -> Arc<dyn cloud_source::CredentialSource> {
        Arc::clone(&self.token) as Arc<dyn cloud_source::CredentialSource>
    }
}

fn request(service: &mut CloudService, account: &str, request: ImportRequest) -> ImportReply {
    match service.request(
        CloudRequest::Import {
            account: account.into(),
            request,
        },
        Duration::from_secs(5),
    ) {
        CloudResponse::Import(reply) => reply,
        other => panic!("unexpected response: {other:?}"),
    }
}
fn prepare(service: &mut CloudService, id: &str, revision: Option<String>) -> String {
    match request(
        service,
        "files-1",
        ImportRequest::StartRead {
            id: id.into(),
            revision,
        },
    ) {
        ImportReply::Preparing { session } => session,
        other => panic!("unexpected prepare: {other:?}"),
    }
}
fn ready(service: &mut CloudService, session: &str) -> ImportReadState {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let ImportReply::ReadState(state) = request(
            service,
            "files-1",
            ImportRequest::ReadStatus {
                session: session.into(),
            },
        ) else {
            panic!("missing read state")
        };
        if state != ImportReadState::Preparing {
            return state;
        }
        assert!(std::time::Instant::now() < deadline, "read did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn verify_export(service: &mut CloudService, entry: &runtime_fs::cloud::ImportEntry, bytes: &[u8]) {
    let session = prepare(service, &entry.id, entry.revision.clone());
    assert_eq!(
        ready(service, &session),
        ImportReadState::Ready {
            size: bytes.len() as u64
        }
    );
    assert_eq!(
        request(
            service,
            "files-1",
            ImportRequest::ReadChunk {
                session: session.clone(),
                offset: 0,
                length: IMPORT_CHUNK_LIMIT
            }
        ),
        ImportReply::Chunk {
            bytes: bytes.to_vec(),
            eof: true
        }
    );
    assert!(
        matches!(request(service, "files-1", ImportRequest::ReadChunk { session: session.clone(), offset: 0, length: IMPORT_CHUNK_LIMIT + 1 }), ImportReply::Error(e) if e.kind == ImportErrorKind::InvalidRequest)
    );
    assert_eq!(
        request(
            service,
            "files-1",
            ImportRequest::CloseRead {
                session: session.clone()
            }
        ),
        ImportReply::Closed
    );
    assert!(
        matches!(request(service, "files-1", ImportRequest::ReadStatus { session }), ImportReply::Error(e) if e.kind == ImportErrorKind::Cancelled)
    );
}

#[test]
fn import_mode_browses_and_exports_office_without_a_mount() {
    use cloud_source_gdrive::fake::{FakeDrive, SOURCE_TOKEN};
    let server = FakeDrive::start();
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut config = serde_json::json!({});
    // The shared configuration key is not a protocol detail of the source.
    config[accounts::cloud_types::MOUNT_CONFIG_KEY] =
        serde_json::to_value(server.config()).unwrap();
    set_files_mode(&mut config, FilesMode::Import);
    let mut account = accounts::Account::new("files-1", accounts::cloud_types::GOOGLE_DRIVE);
    account.display_name = "Drive".into();
    account.config = config;
    let accounts = DriveAccounts::new(vec![account], SOURCE_TOKEN);
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        ServiceConfig {
            config_root: Some(root.path().to_path_buf()),
            mounts_root: home.path().join("Remote"),
            state_root: home.path().join("state"),
            accounts,
            control_socket: None,
            file_socket: None,
        },
        Box::new(launcher.clone()),
    );
    service.reconcile();
    assert!(service.mounted().is_empty());
    assert!(launcher.volume("files-1").is_none());
    assert!(!home.path().join("Remote").exists());
    let mut fixtures = Vec::new();
    for (kind, suffix, bytes) in [
        ("document", "docx", OFFICE_DOCX),
        ("spreadsheet", "xlsx", OFFICE_XLSX),
        ("presentation", "pptx", OFFICE_PPTX),
        (
            "drawing",
            "svg",
            include_bytes!("../../../../integrations/cloud-source-gdrive/tests/fixtures/empty.svg")
                .as_slice(),
        ),
    ] {
        let id = server.put_native(
            &server.root_id(),
            "Native",
            &format!("application/vnd.google-apps.{kind}"),
        );
        fixtures.push((id, format!("Native.{suffix}"), bytes));
    }
    let unsupported = server.put_native(
        &server.root_id(),
        "Form",
        "application/vnd.google-apps.form",
    );
    let source = cloud_source_gdrive::GDrive::new(server.config(), bearer(SOURCE_TOKEN)).unwrap();
    let folder = source
        .create_folder(&cloud_source::ItemId::new(server.root_id()), "Folder")
        .unwrap()
        .id;
    let ImportReply::Page { entries, .. } = request(
        &mut service,
        "files-1",
        ImportRequest::Browse {
            folder: None,
            offset: 0,
        },
    ) else {
        panic!("missing page")
    };
    assert_eq!(entries.len(), 6);
    assert!(
        entries
            .iter()
            .any(|e| e.id == folder.0 && e.kind == ImportKind::Folder)
    );
    for (id, name, bytes) in fixtures {
        let entry = entries.iter().find(|e| e.id == id).unwrap();
        assert_eq!(entry.name, name);
        assert_eq!(entry.kind, ImportKind::NativeExport);
        assert!(!entry.content_writable);
        verify_export(&mut service, entry, bytes);
    }
    let session = prepare(&mut service, &unsupported, None);
    assert!(
        matches!(ready(&mut service, &session), ImportReadState::Failed(e) if e.kind == ImportErrorKind::Unsupported)
    );
    assert!(!CloudSocket::Accounts.serves(&CloudRequest::Import {
        account: "files-1".into(),
        request: ImportRequest::Browse {
            folder: None,
            offset: 0
        }
    }));
    assert!(service.mounted().is_empty());
    service.shutdown();
}

fn verify_stale(
    service: &mut CloudService,
    server: &FakeDav,
    file: &runtime_fs::cloud::ImportEntry,
) {
    server.put("/plain.txt", b"changed remotely");
    let stale = prepare(service, &file.id, file.revision.clone());
    assert!(
        matches!(ready(service, &stale), ImportReadState::Failed(e) if e.kind == ImportErrorKind::SourceChanged)
    );
    assert_eq!(
        request(
            service,
            "files-1",
            ImportRequest::CloseRead { session: stale }
        ),
        ImportReply::Closed
    );
}

fn verify_sequential_read(service: &mut CloudService, session: &str) {
    // One sequential provider body, not a random-access RAM snapshot.
    assert!(
        matches!(request(service, "files-1", ImportRequest::ReadChunk {
        session: session.into(), offset: 6, length: 20
    }), ImportReply::Error(e) if e.kind == ImportErrorKind::InvalidRequest)
    );
    assert_eq!(
        request(
            service,
            "files-1",
            ImportRequest::ReadChunk {
                session: session.into(),
                offset: 0,
                length: 6
            }
        ),
        ImportReply::Chunk {
            bytes: b"hello ".to_vec(),
            eof: false
        }
    );
    assert_eq!(
        request(
            service,
            "files-1",
            ImportRequest::ReadChunk {
                session: session.into(),
                offset: 6,
                length: 20
            }
        ),
        ImportReply::Chunk {
            bytes: b"import".to_vec(),
            eof: true
        }
    );
}

#[test]
fn standalone_reads_are_bounded_scoped_and_revoked_on_removal() {
    let server = FakeDav::generic();
    server.put("/plain.txt", b"hello import");
    for i in 0..105 {
        server.put(&format!("/page-{i:03}"), b"x");
    }
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut config = server.account_config();
    set_files_mode(&mut config, FilesMode::Import);
    account_of_type(root.path(), "webdav", config.clone(), true);
    publish(root.path(), "files-1", PASSWORD);
    let fixture = session(root.path());
    let launcher = InProcessLauncher::default();
    let mut service =
        CloudService::new(service_config(root.path(), home.path()), Box::new(launcher));
    service.reconcile();
    let ImportReply::Page {
        entries,
        next_offset,
        ..
    } = request(
        &mut service,
        "files-1",
        ImportRequest::Browse {
            folder: None,
            offset: 0,
        },
    )
    else {
        panic!("missing page")
    };
    assert_eq!(entries.len(), 100);
    assert_eq!(next_offset, Some(100));
    let ImportReply::Page {
        entries: rest,
        next_offset,
        ..
    } = request(
        &mut service,
        "files-1",
        ImportRequest::Browse {
            folder: None,
            offset: 100,
        },
    )
    else {
        panic!("missing page")
    };
    assert_eq!(rest.len(), 6);
    assert_eq!(next_offset, None);
    assert!(entries.last().unwrap().id < rest.first().unwrap().id);
    let all: Vec<_> = entries.into_iter().chain(rest).collect();
    let file = all.iter().find(|e| e.name == "plain.txt").unwrap();
    assert!(file.content_writable);
    let session = prepare(&mut service, &file.id, file.revision.clone());
    assert_eq!(
        ready(&mut service, &session),
        ImportReadState::Ready { size: 12 }
    );
    let mut other = accounts::Account::new("other", "webdav");
    other.config = config;
    fixture.keep_account(other);
    assert!(
        matches!(request(&mut service, "other", ImportRequest::ReadChunk { session: session.clone(), offset: 0, length: 20 }), ImportReply::Error(e) if e.kind == ImportErrorKind::Cancelled)
    );
    verify_sequential_read(&mut service, &session);
    verify_stale(&mut service, &server, file);
    let second = prepare(&mut service, &file.id, None);
    assert!(
        matches!(request(&mut service, "files-1", ImportRequest::StartRead { id: file.id.clone(), revision: None }), ImportReply::Error(e) if e.kind == ImportErrorKind::QuotaExceeded)
    );
    fixture.remove_account("files-1");
    service.reconcile();
    assert!(
        matches!(request(&mut service, "files-1", ImportRequest::ReadStatus { session }), ImportReply::Error(e) if e.kind == ImportErrorKind::PermissionDenied)
    );
    let _ = second;
    service.shutdown();
}
