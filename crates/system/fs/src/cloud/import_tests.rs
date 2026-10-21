use super::*;
use cloud_source::{
    Capabilities, CloudSource, Download, ExportFormat, Item, ItemId, ItemKind, Revision,
    UploadSession, UploadTarget,
};
use std::io::Read;
use std::ops::Range;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const SIZE: u64 = (33 << 20) + 13;
struct Generated {
    remaining: u64,
    changed: Arc<AtomicBool>,
    change_at_eof: bool,
}
impl Read for Generated {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = usize::try_from(self.remaining.min(buffer.len() as u64))
            .expect("buffer length fits usize");
        buffer
            .get_mut(..count)
            .expect("generated body fits buffer")
            .fill(42);
        self.remaining = self.remaining.saturating_sub(count as u64);
        if count == 0 && self.change_at_eof {
            self.changed.store(true, Ordering::Release);
        }
        Ok(count)
    }
}
struct Source {
    changed: Arc<AtomicBool>,
    change_at_eof: bool,
    unknown: bool,
}
impl CloudSource for Source {
    fn capabilities(&self) -> Capabilities {
        Capabilities::webdav()
    }
    fn root(&self) -> ItemId {
        ItemId::new("root")
    }
    fn stat(&self, id: &ItemId) -> cloud_source::Result<Item> {
        Ok(Item {
            id: id.clone(),
            parents: vec![],
            name: "video".into(),
            kind: ItemKind::File,
            size: (!self.unknown).then_some(SIZE),
            modified: None,
            revision: Some(Revision::new(if self.changed.load(Ordering::Acquire) {
                "new"
            } else {
                "old"
            })),
            hashes: vec![],
            mime: None,
        })
    }
    fn read(
        &self,
        _: &ItemId,
        _: Option<Range<u64>>,
        _: Option<&Revision>,
    ) -> cloud_source::Result<Download> {
        Ok(Download {
            body: Box::new(Generated {
                remaining: SIZE,
                changed: self.changed.clone(),
                change_at_eof: self.change_at_eof,
            }),
            range: 0..SIZE,
            total: (!self.unknown).then_some(SIZE),
            revision: Some(Revision::new("old")),
        })
    }
    fn list(&self, _: &ItemId) -> cloud_source::Result<Vec<Item>> {
        Err(Error::Unsupported("unused test operation"))
    }
    fn export(&self, _: &ItemId, _: &ExportFormat) -> cloud_source::Result<Download> {
        Err(Error::Unsupported("unused test operation"))
    }
    fn begin_upload(
        &self,
        _: UploadTarget,
        _: u64,
    ) -> cloud_source::Result<Box<dyn UploadSession>> {
        Err(Error::Unsupported("unused test operation"))
    }
    fn create_folder(&self, _: &ItemId, _: &str) -> cloud_source::Result<Item> {
        Err(Error::Unsupported("unused test operation"))
    }
    fn move_item(
        &self,
        _: &ItemId,
        _: &ItemId,
        _: &str,
        _: Option<&Revision>,
    ) -> cloud_source::Result<Item> {
        Err(Error::Unsupported("unused test operation"))
    }
    fn delete(&self, _: &ItemId, _: Option<&Revision>) -> cloud_source::Result<()> {
        Err(Error::Unsupported("unused test operation"))
    }
}

#[test]
fn generated_large_body_is_streamed_and_eof_is_revision_checked() {
    for change_at_eof in [false, true] {
        let source = Arc::new(Source {
            changed: Arc::new(AtomicBool::new(false)),
            change_at_eof,
            unknown: false,
        });
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let mut transfer = Transfer::prepare(
            source,
            &ItemId::new("video"),
            Some(&Revision::new("old")),
            &|| false,
            &staging,
        )
        .unwrap();
        assert_eq!(transfer.size, SIZE);
        loop {
            let offset = transfer.offset;
            let result = transfer.chunk(offset, IMPORT_CHUNK_LIMIT, &|| false);
            if change_at_eof && SIZE.saturating_sub(offset) <= u64::from(IMPORT_CHUNK_LIMIT) {
                assert!(matches!(result, Err(Error::Conflict { .. })));
                break;
            }
            let ImportReply::Chunk { bytes, eof } = result.unwrap() else {
                panic!("expected chunk")
            };
            assert!(bytes.len() <= IMPORT_CHUNK_LIMIT as usize);
            assert!(bytes.iter().all(|byte| *byte == 42));
            if eof {
                assert_eq!(transfer.offset, SIZE);
                break;
            }
        }
    }
}

#[test]
fn cancellation_does_not_release_body_or_final_chunk() {
    let source = Arc::new(Source {
        changed: Arc::new(AtomicBool::new(false)),
        change_at_eof: false,
        unknown: false,
    });
    let dir = tempfile::tempdir().unwrap();
    let staging = Staging::new(dir.path().to_path_buf());
    let mut transfer = Transfer::prepare(
        source.clone(),
        &ItemId::new("video"),
        None,
        &|| false,
        &staging,
    )
    .unwrap();
    assert!(matches!(
        transfer.chunk(0, IMPORT_CHUNK_LIMIT, &|| true),
        Err(Error::SessionExpired)
    ));
    assert_eq!(transfer.offset, 0);
    while SIZE.saturating_sub(transfer.offset) > u64::from(IMPORT_CHUNK_LIMIT) {
        transfer
            .chunk(transfer.offset, IMPORT_CHUNK_LIMIT, &|| false)
            .unwrap();
    }
    let offset = transfer.offset;
    let checks = std::sync::atomic::AtomicUsize::new(0);
    assert!(matches!(
        transfer.chunk(offset, IMPORT_CHUNK_LIMIT, &|| checks
            .fetch_add(1, Ordering::AcqRel)
            >= 2),
        Err(Error::SessionExpired)
    ));
    assert_eq!(transfer.offset, offset);
    assert!(matches!(
        Transfer::prepare(source, &ItemId::new("video"), None, &|| true, &staging),
        Err(Error::SessionExpired)
    ));
}

#[test]
fn unknown_length_staging_has_per_file_and_aggregate_limits_and_releases_quota() {
    let dir = tempfile::tempdir().unwrap();
    let source = Arc::new(Source {
        changed: Arc::new(AtomicBool::new(false)),
        change_at_eof: false,
        unknown: true,
    });
    let mut staging = Staging::new(dir.path().join("state-staging"));
    staging.per_file = SIZE - 1;
    assert!(matches!(
        Transfer::prepare(
            source.clone(),
            &ItemId::new("video"),
            None,
            &|| false,
            &staging
        ),
        Err(Error::TooLarge)
    ));
    assert_eq!(staging.used.load(Ordering::Acquire), 0);
    assert_eq!(std::fs::read_dir(&staging.directory).unwrap().count(), 0);

    staging.per_file = SIZE;
    staging.aggregate = SIZE * 2;
    let first = Transfer::prepare(
        source.clone(),
        &ItemId::new("video"),
        None,
        &|| false,
        &staging,
    )
    .unwrap();
    let second = Transfer::prepare(
        source.clone(),
        &ItemId::new("video"),
        None,
        &|| false,
        &staging,
    )
    .unwrap();
    assert_eq!(staging.used.load(Ordering::Acquire), SIZE * 2);
    assert!(matches!(
        Transfer::prepare(
            source.clone(),
            &ItemId::new("video"),
            None,
            &|| false,
            &staging
        ),
        Err(Error::TooLarge)
    ));
    assert_eq!(staging.used.load(Ordering::Acquire), SIZE * 2);
    drop(first);
    drop(second);
    assert_eq!(staging.used.load(Ordering::Acquire), 0);
    let checks = AtomicUsize::new(0);
    assert!(matches!(
        Transfer::prepare(
            source,
            &ItemId::new("video"),
            None,
            &|| checks.fetch_add(1, Ordering::AcqRel) >= 5,
            &staging
        ),
        Err(Error::SessionExpired)
    ));
    assert_eq!(staging.used.load(Ordering::Acquire), 0);
    assert_eq!(std::fs::read_dir(&staging.directory).unwrap().count(), 0);

    // A known-length body larger than either disk budget still streams.
    staging.per_file = 1;
    staging.aggregate = 1;
    let known = Arc::new(Source {
        changed: Arc::new(AtomicBool::new(false)),
        change_at_eof: false,
        unknown: false,
    });
    let mut transfer =
        Transfer::prepare(known, &ItemId::new("video"), None, &|| false, &staging).unwrap();
    assert_eq!(staging.used.load(Ordering::Acquire), 0);
    assert!(matches!(
        transfer.chunk(0, IMPORT_CHUNK_LIMIT, &|| false),
        Ok(ImportReply::Chunk { eof: false, .. })
    ));
}

fn import_request(imports: &Imports, account: &str, request: ImportRequest) -> ImportReply {
    let credentials = Arc::new(cloud_source::StaticCredential(
        cloud_source::Credential::Bearer {
            token: zeroize::Zeroizing::new("test".into()),
            expires: None,
        },
    ));
    let (reply, incoming) = mpsc::channel();
    imports.request(account.into(), request, credentials, reply);
    let CloudResponse::Import(answer) = tilo_test_env::recv("the import reply", &incoming) else {
        panic!("expected import reply")
    };
    answer
}

fn leased_imports(state_root: &Path) -> (Imports, Arc<AtomicU64>, Arc<AtomicBool>) {
    let mut imports = Imports::new(state_root);
    let seconds = Arc::new(AtomicU64::new(0));
    let time = seconds.clone();
    let start = Instant::now();
    imports.clock = Arc::new(move || {
        start
            .checked_add(Duration::from_secs(time.load(Ordering::Acquire)))
            .unwrap()
    });
    let spec = MountSpec {
        account_id: "owner".into(),
        provider: "webdav".into(),
        config: serde_json::Value::Null,
        cache_budget: 0,
        paused: false,
    };
    let mut other = spec.clone();
    other.account_id = "other".into();
    imports.reconcile(vec![spec, other]);
    let cancelled = Arc::new(AtomicBool::new(false));
    imports.sessions.lock().unwrap().insert(
        "read".into(),
        Session {
            account: "owner".into(),
            expires: start.checked_add(SESSION_TTL).unwrap(),
            cancelled: cancelled.clone(),
            account_cancelled: imports.accounts.get("owner").unwrap().cancelled.clone(),
            content: Content::Preparing,
        },
    );
    (imports, seconds, cancelled)
}

#[test]
fn production_requests_renew_idle_leases_but_invalid_or_foreign_requests_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let (imports, seconds, cancelled) = leased_imports(dir.path());
    // Active status polls keep even preparation alive beyond the initial TTL.
    for now in [240, 480] {
        seconds.store(now, Ordering::Release);
        assert_eq!(
            import_request(
                &imports,
                "owner",
                ImportRequest::ReadStatus {
                    session: "read".into()
                }
            ),
            ImportReply::ReadState(ImportReadState::Preparing)
        );
    }
    let source = Arc::new(Source {
        changed: Arc::new(AtomicBool::new(false)),
        change_at_eof: false,
        unknown: false,
    });
    let mut transfer = Transfer::prepare(
        source,
        &ItemId::new("video"),
        None,
        &|| false,
        &imports.staging,
    )
    .unwrap();
    let (requests, incoming) = mpsc::sync_channel(1);
    imports
        .sessions
        .lock()
        .unwrap()
        .get_mut("read")
        .unwrap()
        .content = Content::Ready {
        size: SIZE,
        requests,
    };
    let sessions = imports.sessions.clone();
    let clock = imports.clock.clone();
    let revoked = cancelled.clone();
    let worker = std::thread::spawn(move || {
        run_chunks(
            &mut transfer,
            &incoming,
            &sessions,
            "read",
            &|| revoked.load(Ordering::Acquire),
            &clock,
        );
    });
    seconds.store(720, Ordering::Release);
    assert!(matches!(
        import_request(
            &imports,
            "owner",
            ImportRequest::ReadChunk {
                session: "read".into(),
                offset: 0,
                length: 10,
            }
        ),
        ImportReply::Chunk { eof: false, .. }
    ));
    seconds.store(960, Ordering::Release);
    assert!(
        matches!(import_request(&imports, "other", ImportRequest::ReadStatus { session: "read".into() }),
        ImportReply::Error(e) if e.kind == ImportErrorKind::Cancelled)
    );
    for (offset, length) in [(0, 10), (10, 0), (10, IMPORT_CHUNK_LIMIT + 1)] {
        assert!(
            matches!(import_request(&imports, "owner", ImportRequest::ReadChunk {
            session: "read".into(), offset, length,
        }), ImportReply::Error(e) if e.kind == ImportErrorKind::InvalidRequest)
        );
    }
    seconds.store(1080, Ordering::Release);
    assert!(
        matches!(import_request(&imports, "owner", ImportRequest::ReadStatus { session: "read".into() }),
        ImportReply::Error(e) if e.kind == ImportErrorKind::Cancelled)
    );
    assert!(cancelled.load(Ordering::Acquire));
    worker.join().unwrap();
}
