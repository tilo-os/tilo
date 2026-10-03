use super::*;
use crate::setup::{
    internal_bundle,
    internal_download::tests::{Server, response},
};
use std::io::Read;

fn manifest() -> Vec<u8> {
    internal_bundle::tests::bytes(&internal_bundle::tests::fixture())
}

/// Held roles, reauthentication and per-role resume.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn authenticated_roles_stay_held_and_resume_their_own_ranges() {
    isolated(
        "both_authenticated_roles_remain_held_until_matching_consume",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let consumed = seen.clone();
            let mut worker = fixture_worker(
                root.path(),
                server.transport(),
                move |_, _, first, second| {
                    let mut a = Vec::new();
                    let mut b = Vec::new();
                    first.read_to_end(&mut a).map_err(|e| e.to_string())?;
                    second.read_to_end(&mut b).map_err(|e| e.to_string())?;
                    *consumed.lock().unwrap() = vec![a, b];
                    Ok(())
                },
            );
            let token = worker.start(server.release(&bytes)).unwrap();
            let status = wait(&worker, Phase::Ready);
            assert_eq!(status.token, token);
            assert!(seen.lock().unwrap().is_empty());
            assert_eq!(status.finals, [true, true]);
            worker.consume(&token).unwrap();
            wait(&worker, Phase::Consumed);
            assert_eq!(
                *seen.lock().unwrap(),
                vec![vec![b'E'; 4096], vec![b'R'; 8192]]
            );
            worker.shutdown();
        },
    );

    isolated(
        "new_worker_reauthenticates_and_resumes_only_second_remaining_range",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "Content-Length: 8192\r\n", &vec![b'R'; 1024]),
                response(200, "", &bytes),
                ranged(1024, 8192, b'R'),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut first = transfer_hook_worker(
                root.path(),
                server.transport(),
                |s, n, shared| {
                    if s == "write" && n == 1024 {
                        active_flags(shared).pause.store(true, Ordering::Release);
                    }
                },
                None,
            );
            let old = first.start(server.release(&bytes)).unwrap();
            let paused = wait(&first, Phase::Paused);
            assert_eq!(paused.committed, [4096, 1024]);
            assert_eq!(paused.finals, [true, false]);
            first.shutdown();
            drop(first);
            assert_eq!(
                fs::read(root.path().join("0/final")).unwrap(),
                vec![b'E'; 4096]
            );
            let counts = Arc::new(Mutex::new([(0, 0); 2]));
            let mut second = transfer_hook_worker(
                root.path(),
                server.transport(),
                |_, _, _| {},
                Some(counts.clone()),
            );
            let new = second.start(server.release(&bytes)).unwrap();
            assert_ne!(old, new);
            wait(&second, Phase::Ready);
            second.consume(&new).unwrap();
            wait(&second, Phase::Consumed);
            second.shutdown();
            assert_eq!(
                fs::read(root.path().join("1/final")).unwrap(),
                vec![b'R'; 8192]
            );
            assert_eq!(*counts.lock().unwrap(), [(1024, 1024), (7168, 7168)]);
            let requests = server.requests();
            assert_eq!(requests.len(), 5);
            assert!(requests[3].starts_with("GET /releases/manifest.json"));
            assert!(requests[4].starts_with("GET /releases/staging-root.img"));
            assert!(requests[4].contains("range: bytes=1024-"));
        },
    );

    isolated(
        "both_partials_are_prepared_before_first_payload_and_each_resumes_own_range",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(200, "", &bytes),
                ranged(1024, 4096, b'E'),
                ranged(2048, 8192, b'R'),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut first = transfer_hook_worker(
                root.path(),
                server.transport(),
                |s, n, shared| {
                    if s == "write" && n == 1024 {
                        active_flags(shared).pause.store(true, Ordering::Release);
                    }
                },
                None,
            );
            first.start(server.release(&bytes)).unwrap();
            wait(&first, Phase::Paused);
            first.shutdown();
            // Both zero-prefix ownerships existed before first payload transfer. Seed the
            // second role via the existing real Session+prefix fixture on this thread.
            assert!(root.path().join("1/journal/record-1").exists());
            let release = InternalRelease::transport_fixture(
                &bytes,
                "https://code.tilo.so/releases/manifest.json",
            );
            let selected = SelectedManifest::from_bytes(&release, bytes.clone()).unwrap();
            ROOT.with(|r| *r.borrow_mut() = root.path().to_owned());
            let components = fixture::CacheRoot::acquire(&selected)
                .unwrap()
                .bootstrap(&selected, &AtomicBool::new(false))
                .unwrap();
            let [_, second] = components;
            if let fixture::Component::Partial(mut p) = second {
                let mut s = Session::new(&selected, 1, 999, &mut p.provider).unwrap();
                let mut prepared = s
                    .prepare(&selected, &mut p.file, &AtomicBool::new(false))
                    .unwrap();
                super::super::tests::start_prefix(&mut prepared, &vec![b'R'; 2048]);
            } else {
                panic!("expected partial");
            }
            let mut worker = fixture_worker(root.path(), server.transport(), noop);
            let token = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            worker.consume(&token).unwrap();
            wait(&worker, Phase::Consumed);
            worker.shutdown();
            let requests = server.requests();
            assert_eq!(requests.len(), 5);
            assert!(requests[3].contains("range: bytes=1024-"));
            assert!(requests[4].contains("range: bytes=2048-"));
        },
    );

    isolated(
        "invalid_second_prefix_prevents_any_first_payload_transfer",
        || {
            let bytes = manifest();
            let server = Server::new(vec![response(200, "", &bytes), response(200, "", &bytes)]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = fixture_worker_hook(root.path(), server.transport(), noop, |shared| {
                SETUP_HOOK.with(|h| {
                    *h.borrow_mut() = Some(Box::new(move |index, stage| {
                        if index == 1 && stage == "after-initialize" {
                            active_flags(&shared).pause.store(true, Ordering::Release);
                        }
                        Ok(())
                    }))
                });
            });
            worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Paused);
            worker.shutdown();
            fs::write(root.path().join("1/journal/record-1"), b"bad journal").unwrap();
            let mut next = fixture_worker(root.path(), server.transport(), noop);
            next.start(server.release(&bytes)).unwrap();
            wait(&next, Phase::Failed);
            next.shutdown();
            assert_eq!(server.requests().len(), 2);
            assert_eq!(
                fs::read(
                    root.path()
                        .join("0/.tilo-part-0123456789abcdef0123456789abcdef")
                )
                .unwrap()
                .len(),
                0
            );
        },
    );
}

use super::super::{Ownership, Provider, initialize_fresh};
use crate::setup::internal_cache::tests::{FreshDisk, clear_reads, fail, read_totals};
use crate::setup::internal_download::tests::{with_checkpoint_interval, with_hook};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};

thread_local! {
    static ROOT: RefCell<PathBuf> = RefCell::new(PathBuf::new());
    static SETUP_HOOK: RefCell<Option<Box<dyn FnMut(usize, &str) -> Result<(), String>>>> = RefCell::new(None);
}
fn setup_event(index: usize, stage: &str) -> Result<(), String> {
    SETUP_HOOK.with(|h| match h.borrow_mut().as_mut() {
        Some(h) => h(index, stage),
        None => Ok(()),
    })
}
fn fixture_worker(
    root: &Path,
    transport: InternalTransport,
    consumer: impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>
    + Send
    + 'static,
) -> Worker {
    fixture_worker_hook(root, transport, consumer, |_| {})
}
impl Worker {
    pub(in crate::setup) fn fixture_for_test(
        root: &Path,
        transport: InternalTransport,
        consumer: impl FnMut(
            &SelectedManifest,
            &Cancellation<'_>,
            &mut dyn HeldReadSeek,
            &mut dyn HeldReadSeek,
        ) -> Result<(), String>
        + Send
        + 'static,
    ) -> Self {
        fixture_worker(root, transport, consumer)
    }
}
fn fixture_worker_hook(
    root: &Path,
    transport: InternalTransport,
    consumer: impl FnMut(
        &SelectedManifest,
        &Cancellation<'_>,
        &mut dyn HeldReadSeek,
        &mut dyn HeldReadSeek,
    ) -> Result<(), String>
    + Send
    + 'static,
    install: impl FnOnce(Arc<Shared>) + Send + 'static,
) -> Worker {
    let root = root.to_owned();
    Worker::spawn_runner(move |shared| {
        ROOT.with(|r| *r.borrow_mut() = root);
        install(shared.clone());
        with_checkpoint_interval(1024, || worker_loop(shared, transport, consumer));
        SETUP_HOOK.with(|h| *h.borrow_mut() = None);
        fail(None);
    })
    .unwrap()
}
fn wait(worker: &Worker, phase: Phase) -> Status {
    let deadline = tilo_test_env::hang_deadline();
    let mut m = worker.shared.mailbox.lock().unwrap();
    loop {
        if let Some(s) = &m.status {
            if s.phase == phase {
                return s.clone();
            }
            if matches!(
                s.phase,
                Phase::Failed
                    | Phase::Cancelled
                    | Phase::Paused
                    | Phase::Consumed
                    | Phase::Discarded
                    | Phase::Released
            ) {
                let status = s.clone();
                drop(m);
                panic!("wanted {phase:?}, got {status:?}");
            }
        }
        assert!(
            ui::time::now() < deadline,
            "timeout waiting for {phase:?}: {:?}",
            m.status
        );
        m = worker
            .shared
            .wake
            .wait_timeout(m, Duration::from_millis(50))
            .unwrap()
            .0;
    }
}

/// Test-only ordinary files, existing FreshDisk/Session, real HTTP transport.
/// NOT a native security/locking/no-replace publication/durability substitute.
pub(in crate::setup::internal_cache::coordinator) mod fixture {
    use super::*;
    pub(in crate::setup::internal_cache::coordinator) struct CacheRoot {
        path: PathBuf,
    }
    pub(in crate::setup::internal_cache::coordinator) struct Partial<'s> {
        pub provider: FreshDisk<'s>,
        pub file: File,
        selected: &'s SelectedManifest,
        index: usize,
        root: PathBuf,
    }
    pub(in crate::setup::internal_cache::coordinator) struct FinalHandle<'s> {
        file: File,
        selected: &'s SelectedManifest,
        index: usize,
    }
    pub(in crate::setup::internal_cache::coordinator) enum Component<'s> {
        Partial(Partial<'s>),
        Final(FinalHandle<'s>),
    }
    fn id(file: &File) -> [u8; 32] {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata().unwrap();
        let mut h = Sha256::new();
        h.update(m.dev().to_le_bytes());
        h.update(m.ino().to_le_bytes());
        h.finalize().into()
    }
    fn check(cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::Acquire) {
            Err("Internal transfer cancelled".into())
        } else {
            Ok(())
        }
    }
    fn verify(
        file: &mut File,
        selected: &SelectedManifest,
        index: usize,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        check(cancel)?;
        let c = selected.bundle().component(index).unwrap();
        if file.metadata().unwrap().len() != c.byte_size {
            return Err("fixture final size mismatch".into());
        }
        file.rewind().unwrap();
        let mut h = Sha256::new();
        let mut buffer = [0; 8192];
        loop {
            check(cancel)?;
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(&buffer[..n]);
        }
        if hex::encode(h.finalize()) != c.sha256 {
            return Err("fixture final digest mismatch".into());
        }
        check(cancel)
    }
    impl CacheRoot {
        pub(in crate::setup::internal_cache::coordinator) fn acquire(
            _: &SelectedManifest,
        ) -> Result<Self, String> {
            Ok(Self {
                path: ROOT.with(|r| r.borrow().clone()),
            })
        }
        pub(in crate::setup::internal_cache::coordinator) fn bootstrap<'s>(
            self,
            selected: &'s SelectedManifest,
            cancel: &AtomicBool,
        ) -> Result<[Component<'s>; 2], String> {
            let mut components = Vec::new();
            for index in 0..2 {
                check(cancel)?;
                let root = self.path.join(index.to_string());
                if !root.exists() {
                    fs::create_dir(&root).unwrap();
                }
                if root.join("final").exists() {
                    let mut file = File::open(root.join("final")).map_err(|e| e.to_string())?;
                    verify(&mut file, selected, index, cancel)?;
                    components.push(Component::Final(FinalHandle {
                        file,
                        selected,
                        index,
                    }));
                    continue;
                }
                setup_event(index, "before-create")?;
                check(cancel)?;
                let leaf = ".tilo-part-0123456789abcdef0123456789abcdef";
                let (mut provider, mut file) = if root.join(leaf).exists() {
                    if !root.join("journal").exists()
                        || fs::read_dir(root.join("journal")).unwrap().next().is_none()
                    {
                        return Err("unknown fixture residue".into());
                    }
                    let own = Ownership {
                        parent: id(&File::open(&root).unwrap()),
                        file: id(&File::open(root.join(leaf)).unwrap()),
                        leaf: leaf.into(),
                    };
                    FreshDisk::reopen(&root, selected, index, own)
                } else {
                    let (mut provider, mut file) = FreshDisk::create(&root, selected, index);
                    setup_event(index, "after-create")?;
                    initialize_fresh(selected, index, &mut &mut provider, &mut file)?;
                    setup_event(index, "after-initialize")?;
                    check(cancel)?;
                    (provider, file)
                };
                // Provider owns all history interpretation; no fixture prefix adoption.
                let own = (&mut provider).ownership();
                (&mut provider).verify(&file, &own)?;
                file.rewind().unwrap();
                components.push(Component::Partial(Partial {
                    provider,
                    file,
                    selected,
                    index,
                    root,
                }));
            }
            components
                .try_into()
                .map_err(|_| "fixture component count".into())
        }
    }
    impl<'s> Partial<'s> {
        pub(in crate::setup::internal_cache::coordinator) fn publish(
            mut self,
            cancel: &AtomicBool,
        ) -> Result<FinalHandle<'s>, String> {
            check(cancel)?;
            setup_event(self.index, "publish")?;
            verify(&mut self.file, self.selected, self.index, cancel)?;
            let own = (&mut self.provider).ownership();
            (&mut self.provider).verify(&self.file, &own)?;
            let final_path = self.root.join("final");
            if final_path.exists() {
                return Err("fixture final collision retained".into());
            }
            fs::rename(self.root.join(own.leaf), &final_path).map_err(|e| e.to_string())?;
            drop(self.file);
            let mut file = File::open(final_path).map_err(|e| e.to_string())?;
            verify(&mut file, self.selected, self.index, cancel)?;
            Ok(FinalHandle {
                file,
                selected: self.selected,
                index: self.index,
            })
        }
        pub(in crate::setup::internal_cache::coordinator) fn discard(
            mut self,
            operation: u64,
            cancel: &AtomicBool,
        ) -> Result<(), String> {
            {
                let mut s = Session::new(self.selected, self.index, operation, &mut self.provider)?;
                drop(s.prepare(self.selected, &mut self.file, cancel)?);
            }
            check(cancel)?;
            let own = (&mut self.provider).ownership();
            (&mut self.provider).verify(&self.file, &own)?;
            fs::remove_file(self.root.join(own.leaf)).map_err(|e| e.to_string())?;
            (&mut self.provider).retain(&[])
        }
    }
    impl FinalHandle<'_> {
        pub(in crate::setup::internal_cache::coordinator) fn has_retained_residue(&self) -> bool {
            true
        } // journal retained by this fixture
        pub(in crate::setup::internal_cache::coordinator) fn with_verified_reader<T>(
            &mut self,
            cancel: &AtomicBool,
            consume: impl FnOnce(&mut dyn HeldReadSeek) -> Result<T, String>,
        ) -> Result<T, String> {
            let pos = self.file.stream_position().unwrap();
            let result = (|| {
                verify(&mut self.file, self.selected, self.index, cancel)?;
                self.file.rewind().unwrap();
                consume(&mut self.file)
            })();
            let restored = self
                .file
                .seek(SeekFrom::Start(pos))
                .map_err(|e| e.to_string())
                .and_then(|position| setup_event(self.index, "reader-restored").map(|_| position));
            let held = setup_event(self.index, "reader-held");
            let root = setup_event(self.index, "reader-root");
            let stopped = check(cancel);
            // This is the same result composer called by both native readers;
            // these injected guard results are not native guard execution.
            super::super::super::finish_held_read(result, restored, held, root, stopped)
        }
    }
}

fn active_flags(shared: &Shared) -> Arc<Flags> {
    shared
        .mailbox
        .lock()
        .unwrap()
        .active
        .as_ref()
        .unwrap()
        .1
        .clone()
}
fn ranged(offset: usize, total: usize, byte: u8) -> Vec<u8> {
    response(
        206,
        &format!(
            "Content-Range: bytes {offset}-{}/{total}\r\nContent-Length: {}\r\n",
            total - 1,
            total - offset
        ),
        &vec![byte; total - offset],
    )
}
fn noop(
    _: &SelectedManifest,
    _: &Cancellation<'_>,
    _: &mut dyn HeldReadSeek,
    _: &mut dyn HeldReadSeek,
) -> Result<(), String> {
    Ok(())
}
fn transfer_hook_worker(
    root: &Path,
    transport: InternalTransport,
    hook: impl FnMut(&str, u64, &Shared) + Send + 'static,
    counts: Option<Arc<Mutex<[(u64, u64); 2]>>>,
) -> Worker {
    let root = root.to_owned();
    Worker::spawn_runner(move |shared| {
        ROOT.with(|r| *r.borrow_mut() = root);
        clear_reads();
        let h = shared.clone();
        let mut hook = hook;
        with_hook(Box::new(move |s, n| hook(s, n, &h)), || {
            with_checkpoint_interval(1024, || worker_loop(shared, transport, noop))
        });
        if let Some(counts) = counts {
            *counts.lock().unwrap() = [read_totals("restart"), read_totals("periodic")];
        }
        fail(None);
    })
    .unwrap()
}

/// Failures and cancellation during creation, initialization, preparation and publication.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn creation_initialization_and_publication_failures_retain_exact_residue() {
    isolated(
        "cancellation_during_each_creation_settles_zero_ownership_before_return",
        || {
            for index in 0..2 {
                let bytes = manifest();
                let server = Server::new(vec![response(200, "", &bytes)]);
                let root = tempfile::tempdir().unwrap();
                let mut worker =
                    fixture_worker_hook(root.path(), server.transport(), noop, move |shared| {
                        SETUP_HOOK.with(|h| {
                            *h.borrow_mut() = Some(Box::new(move |i, stage| {
                                if i == index && stage == "after-create" {
                                    active_flags(&shared).cancel.store(true, Ordering::Release);
                                }
                                Ok(())
                            }))
                        });
                    });
                worker.start(server.release(&bytes)).unwrap();
                let s = wait(&worker, Phase::Cancelled);
                assert!(s.residue);
                worker.shutdown();
                assert!(
                    root.path()
                        .join(format!("{index}/journal/record-1"))
                        .exists()
                );
                assert!(
                    !root
                        .path()
                        .join(format!("{}/journal/record-1", index + 1))
                        .exists()
                );
                assert_eq!(server.requests().len(), 1);
            }
        },
    );

    isolated(
        "failures_before_and_after_each_initialization_retain_exact_residue",
        || {
            for index in 0..2 {
                for stage in ["before-create", "after-create", "after-initialize"] {
                    let bytes = manifest();
                    let server = Server::new(vec![response(200, "", &bytes)]);
                    let root = tempfile::tempdir().unwrap();
                    let mut worker =
                        fixture_worker_hook(root.path(), server.transport(), noop, move |_| {
                            SETUP_HOOK.with(|h| {
                                *h.borrow_mut() = Some(Box::new(move |i, s| {
                                    if i == index && s == stage {
                                        Err("injected acquisition boundary".into())
                                    } else {
                                        Ok(())
                                    }
                                }))
                            });
                        });
                    worker.start(server.release(&bytes)).unwrap();
                    let failed = wait(&worker, Phase::Failed);
                    assert!(failed.uncertain);
                    worker.shutdown();
                    assert_eq!(
                        root.path()
                            .join(format!(
                                "{index}/.tilo-part-0123456789abcdef0123456789abcdef"
                            ))
                            .exists(),
                        stage != "before-create"
                    );
                    assert_eq!(
                        root.path()
                            .join(format!("{index}/journal/record-1"))
                            .exists(),
                        stage == "after-initialize"
                    );
                    if index == 1 {
                        assert!(root.path().join("0/journal/record-1").exists());
                    }
                    assert_eq!(server.requests().len(), 1);
                }
            }
        },
    );

    isolated(
        "failed_second_publication_retains_first_final_and_reacquires_candidate",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &bytes),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = fixture_worker_hook(root.path(), server.transport(), noop, |_| {
                SETUP_HOOK.with(|h| {
                    *h.borrow_mut() = Some(Box::new(|i, s| {
                        if i == 1 && s == "publish" {
                            Err("injected second publication".into())
                        } else {
                            Ok(())
                        }
                    }))
                })
            });
            worker.start(server.release(&bytes)).unwrap();
            let failed = wait(&worker, Phase::Failed);
            assert_eq!(failed.finals, [true, false]);
            assert!(failed.uncertain);
            worker.shutdown();
            let mut next = fixture_worker(root.path(), server.transport(), noop);
            let t = next.start(server.release(&bytes)).unwrap();
            wait(&next, Phase::Ready);
            next.consume(&t).unwrap();
            wait(&next, Phase::Consumed);
            next.shutdown();
            assert_eq!(server.requests().len(), 4);
        },
    );

    isolated(
        "actual_initialization_failures_retire_no_unknown_files_and_cancel_wins",
        || {
            for index in 0..2 {
                for fault in ["initialize-flush", "record-write", "lost-ack"] {
                    let bytes = manifest();
                    let server = Server::new(vec![response(200, "", &bytes)]);
                    let root = tempfile::tempdir().unwrap();
                    let mut worker =
                        fixture_worker_hook(root.path(), server.transport(), noop, move |shared| {
                            SETUP_HOOK.with(|h| {
                                *h.borrow_mut() = Some(Box::new(move |i, stage| {
                                    if i == index && stage == "after-create" {
                                        fail(Some(fault));
                                        active_flags(&shared).cancel.store(true, Ordering::Release);
                                    }
                                    Ok(())
                                }))
                            });
                        });
                    worker.start(server.release(&bytes)).unwrap();
                    let cancelled = wait(&worker, Phase::Cancelled);
                    assert!(cancelled.uncertain);
                    assert!(cancelled.error.is_some());
                    worker.shutdown();
                    assert!(
                        root.path()
                            .join(format!(
                                "{index}/.tilo-part-0123456789abcdef0123456789abcdef"
                            ))
                            .exists()
                    );
                    assert_eq!(
                        root.path()
                            .join(format!("{index}/journal/record-1"))
                            .exists(),
                        fault == "lost-ack"
                    );
                }
            }
        },
    );

    isolated(
        "failed_second_preparation_preserves_first_acknowledged_prefix_in_status",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(200, "", &bytes),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut first = transfer_hook_worker(
                root.path(),
                server.transport(),
                |s, n, shared| {
                    if s == "write" && n == 1024 {
                        active_flags(shared).pause.store(true, Ordering::Release);
                    }
                },
                None,
            );
            first.start(server.release(&bytes)).unwrap();
            wait(&first, Phase::Paused);
            first.shutdown();
            fs::write(root.path().join("1/journal/record-1"), b"bad journal").unwrap();
            let mut next = fixture_worker(root.path(), server.transport(), noop);
            next.start(server.release(&bytes)).unwrap();
            let failed = wait(&next, Phase::Failed);
            next.shutdown();
            assert_eq!(failed.committed[0], 1024);
            assert_eq!(server.requests().len(), 3);
        },
    );
}

/// Stale commands, corruption, manifest changes, collisions and identifier exhaustion.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn stale_corrupt_or_colliding_state_cannot_become_ready() {
    isolated(
        "stale_ready_consume_discard_stop_and_progress_cannot_replace_new_selection",
        || {
            let bytes = manifest();
            let mut other = bytes.clone();
            other.push(b' ');
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &other),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = fixture_worker(root.path(), server.transport(), noop);
            let old = worker.start(server.release(&bytes)).unwrap();
            let stale = wait(&worker, Phase::Ready);
            let new = worker.start(server.release(&other)).unwrap();
            assert!(worker.consume(&old).is_err());
            assert!(worker.discard(&old, server.release(&bytes)).is_err());
            assert!(worker.cancel(&old).is_err());
            assert!(worker.release(&old).is_err());
            report(&worker.shared, &stale);
            assert_eq!(worker.status().unwrap().token, new);
            wait(&worker, Phase::Ready);
            worker.release(&new).unwrap();
            wait(&worker, Phase::Released);
            worker.shutdown();
        },
    );

    isolated(
        "ready_corruption_with_same_file_identity_and_length_refuses_consumer",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let called = Arc::new(AtomicBool::new(false));
            let c = called.clone();
            let mut worker = fixture_worker(root.path(), server.transport(), move |_, _, _, _| {
                c.store(true, Ordering::Release);
                Ok(())
            });
            let t = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            let mut f = File::options()
                .write(true)
                .open(root.path().join("1/final"))
                .unwrap();
            f.write_all(&vec![b'X'; 8192]).unwrap();
            f.sync_all().unwrap();
            drop(f);
            worker.consume(&t).unwrap();
            let failed = wait(&worker, Phase::Failed);
            assert!(failed.error.unwrap().contains("digest"));
            assert!(!called.load(Ordering::Acquire));
            worker.shutdown();
        },
    );

    isolated(
        "changed_manifest_bytes_never_reuse_saved_cache_without_current_authentication",
        || {
            let bytes = manifest();
            let mut changed = bytes.clone();
            changed.push(b' ');
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &changed),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = fixture_worker(root.path(), server.transport(), noop);
            let t = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            worker.release(&t).unwrap();
            wait(&worker, Phase::Released);
            worker.start(server.release(&bytes)).unwrap();
            let s = wait(&worker, Phase::Failed);
            assert!(!s.finals[0]);
            assert!(!s.finals[1]);
            worker.shutdown();
            assert_eq!(server.requests().len(), 4);
        },
    );

    isolated(
        "publication_collision_is_retained_and_cannot_produce_ready",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let collision = root.path().join("0/final");
            let path = collision.clone();
            let mut worker =
                fixture_worker_hook(root.path(), server.transport(), noop, move |_| {
                    SETUP_HOOK.with(|h| {
                        *h.borrow_mut() = Some(Box::new(move |i, s| {
                            if i == 0 && s == "publish" {
                                fs::write(&path, b"unknown collision").unwrap();
                            }
                            Ok(())
                        }))
                    })
                });
            worker.start(server.release(&bytes)).unwrap();
            let failed = wait(&worker, Phase::Failed);
            assert!(failed.uncertain);
            assert_eq!(failed.finals, [false, false]);
            worker.shutdown();
            assert_eq!(fs::read(collision).unwrap(), b"unknown collision");
            assert_eq!(server.requests().len(), 2);
        },
    );

    isolated("identifier_exhaustion_refuses_without_wrap_or_zero", || {
        let ids = AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_operation(&ids).unwrap(), u64::MAX - 1);
        assert!(next_operation(&ids).is_err());
        assert_eq!(ids.load(Ordering::Acquire), u64::MAX);
        assert!(next_operation(&AtomicU64::new(0)).is_err());
    });
}

/// Cancellation, discard and blocked-request interruption.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn cancel_and_discard_win_and_disclose_retained_state() {
    isolated(
        "cancel_retains_partial_and_separate_authenticated_discard_uses_fresh_flag",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(200, "", &bytes),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = transfer_hook_worker(
                root.path(),
                server.transport(),
                |s, n, shared| {
                    if s == "write" && n == 1024 {
                        active_flags(shared).cancel.store(true, Ordering::Release);
                    }
                },
                None,
            );
            let t = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Cancelled);
            let old_flags = active_flags(&worker.shared);
            assert!(
                root.path()
                    .join("0/.tilo-part-0123456789abcdef0123456789abcdef")
                    .exists()
            );
            let cleanup = worker.discard(&t, server.release(&bytes)).unwrap();
            assert_ne!(t, cleanup);
            let done = wait(&worker, Phase::Discarded);
            assert!(!done.residue);
            assert!(old_flags.cancel.load(Ordering::Acquire));
            assert!(
                !root
                    .path()
                    .join("0/.tilo-part-0123456789abcdef0123456789abcdef")
                    .exists()
            );
            worker.shutdown();
        },
    );

    isolated(
        "discard_discloses_retained_finals_and_unknown_residue_is_never_removed",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &bytes),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = fixture_worker(root.path(), server.transport(), noop);
            let t = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            worker.discard(&t, server.release(&bytes)).unwrap();
            let s = wait(&worker, Phase::Discarded);
            assert_eq!(s.finals, [true, true]);
            assert!(s.residue);
            worker.shutdown();
            assert!(root.path().join("0/final").exists());
            let server = Server::new(vec![response(200, "", &bytes)]);
            let unknown = tempfile::tempdir().unwrap();
            fs::create_dir(unknown.path().join("0")).unwrap();
            let leaf = unknown
                .path()
                .join("0/.tilo-part-0123456789abcdef0123456789abcdef");
            fs::write(&leaf, b"unknown").unwrap();
            let mut worker = fixture_worker(unknown.path(), server.transport(), noop);
            worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Failed);
            worker.shutdown();
            assert_eq!(fs::read(leaf).unwrap(), b"unknown");
        },
    );

    isolated(
        "pause_and_cancel_interrupt_blocked_request_without_command_queue_capacity",
        || {
            for cancel in [false, true] {
                let bytes = manifest();
                let gate = Arc::new((Mutex::new(false), Condvar::new()));
                let server_gate = gate.clone();
                let requested = Arc::new(AtomicU64::new(0));
                let seen = requested.clone();
                let server = Server::on_request(
                    vec![
                        response(200, "", &bytes),
                        response(200, "", &vec![b'E'; 4096]),
                    ],
                    move || {
                        if seen.fetch_add(1, Ordering::AcqRel) == 1 {
                            let (m, c) = &*server_gate;
                            let mut released = m.lock().unwrap();
                            while !*released {
                                released =
                                    c.wait_timeout(released, Duration::from_secs(2)).unwrap().0;
                                if !*released {
                                    break;
                                }
                            }
                        }
                    },
                );
                let root = tempfile::tempdir().unwrap();
                let mut worker = fixture_worker(root.path(), server.transport(), noop);
                let t = worker.start(server.release(&bytes)).unwrap();
                let deadline = tilo_test_env::hang_deadline();
                while requested.load(Ordering::Acquire) < 2 {
                    assert!(ui::time::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                worker.pause(&t).unwrap();
                if cancel {
                    worker.cancel(&t).unwrap();
                }
                *gate.0.lock().unwrap() = true;
                gate.1.notify_all();
                let stopped = wait(
                    &worker,
                    if cancel {
                        Phase::Cancelled
                    } else {
                        Phase::Paused
                    },
                );
                assert_eq!(stopped.committed, [0, 0]);
                assert_eq!(server.requests().len(), 2);
                worker.shutdown();
            }
        },
    );

    isolated(
        "cancel_during_scoped_consumer_wins_over_its_success_or_error",
        || {
            for error in [false, true] {
                let bytes = manifest();
                let server = Server::new(vec![
                    response(200, "", &bytes),
                    response(200, "", &vec![b'E'; 4096]),
                    response(200, "", &vec![b'R'; 8192]),
                ]);
                let root = tempfile::tempdir().unwrap();
                let entered = Arc::new((Mutex::new(false), Condvar::new()));
                let released = Arc::new((Mutex::new(false), Condvar::new()));
                let e = entered.clone();
                let r = released.clone();
                let mut worker =
                    fixture_worker(root.path(), server.transport(), move |_, _, _, _| {
                        *e.0.lock().unwrap() = true;
                        e.1.notify_all();
                        let mut done = r.0.lock().unwrap();
                        while !*done {
                            done = r.1.wait(done).unwrap();
                        }
                        if error {
                            Err("consumer error".into())
                        } else {
                            Ok(())
                        }
                    });
                let t = worker.start(server.release(&bytes)).unwrap();
                wait(&worker, Phase::Ready);
                worker.consume(&t).unwrap();
                let deadline = tilo_test_env::hang_deadline();
                let mut in_consumer = entered.0.lock().unwrap();
                while !*in_consumer {
                    let now = ui::time::now();
                    assert!(now < deadline, "the worker never entered the consumer");
                    in_consumer = entered
                        .1
                        .wait_timeout(in_consumer, deadline - now)
                        .unwrap()
                        .0;
                }
                drop(in_consumer);
                worker.cancel(&t).unwrap();
                *released.0.lock().unwrap() = true;
                released.1.notify_all();
                let cancelled = wait(&worker, Phase::Cancelled);
                assert!(cancelled.uncertain);
                assert_eq!(cancelled.committed, [4096, 8192]);
                assert_eq!(cancelled.finals, [true, true]);
                if error {
                    assert!(cancelled.error.as_ref().unwrap().contains("consumer error"));
                }
                worker.shutdown();
            }
        },
    );
}

/// Disconnection and paused scope release.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn disconnects_and_pauses_release_scopes_without_consuming() {
    isolated(
        "disconnect_from_held_ready_cancels_and_joins_without_consuming",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let worker = fixture_worker(root.path(), server.transport(), noop);
            worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            let shared = worker.shared.clone();
            let now = ui::time::now();
            drop(worker);
            assert!(now.elapsed() < Duration::from_secs(3));
            assert_eq!(
                shared
                    .mailbox
                    .lock()
                    .unwrap()
                    .status
                    .as_ref()
                    .unwrap()
                    .phase,
                Phase::Cancelled
            );
        },
    );

    isolated(
        "worker_disconnect_during_queued_authentication_has_terminal_status",
        || {
            let bytes = manifest();
            let server = Server::new(vec![response(200, "", &bytes)]);
            let root = tempfile::tempdir().unwrap();
            let worker = fixture_worker(root.path(), server.transport(), noop);
            worker.start(server.release(&bytes)).unwrap();
            let shared = worker.shared.clone();
            drop(worker);
            assert_eq!(
                shared
                    .mailbox
                    .lock()
                    .unwrap()
                    .status
                    .as_ref()
                    .unwrap()
                    .phase,
                Phase::Cancelled
            );
        },
    );

    isolated(
        "disconnect_signals_scoped_consumer_and_waits_for_its_release",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let entered = Arc::new(AtomicBool::new(false));
            let returned = Arc::new(AtomicBool::new(false));
            let e = entered.clone();
            let r = returned.clone();
            let worker = fixture_worker(root.path(), server.transport(), move |_, cancel, _, _| {
                e.store(true, Ordering::Release);
                let limit = tilo_test_env::hang_deadline();
                while !cancel.is_cancelled() {
                    if ui::time::now() > limit {
                        return Err("consumer did not receive cancellation".into());
                    }
                    thread::yield_now();
                }
                r.store(true, Ordering::Release);
                Ok(())
            });
            let token = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            worker.consume(&token).unwrap();
            let deadline = tilo_test_env::hang_deadline();
            while !entered.load(Ordering::Acquire) {
                assert!(ui::time::now() < deadline);
                thread::yield_now();
            }
            let shared = worker.shared.clone();
            drop(worker);
            assert!(returned.load(Ordering::Acquire));
            assert_eq!(
                shared
                    .mailbox
                    .lock()
                    .unwrap()
                    .status
                    .as_ref()
                    .unwrap()
                    .phase,
                Phase::Cancelled
            );
        },
    );

    isolated(
        "paused_ready_releases_scope_and_new_operation_reuses_both_finals",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
                response(200, "", &bytes),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut worker = fixture_worker(root.path(), server.transport(), noop);
            let old = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            worker.pause(&old).unwrap();
            wait(&worker, Phase::Paused);
            let new = worker.start(server.release(&bytes)).unwrap();
            wait(&worker, Phase::Ready);
            worker.consume(&new).unwrap();
            wait(&worker, Phase::Consumed);
            worker.shutdown();
            assert_eq!(server.requests().len(), 4);
        },
    );
}

/// Lost acks.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn lost_acks_stay_uncertain_until_fresh_recovery() {
    isolated(
        "lost_ack_is_uncertain_until_fresh_operation_recovers_surviving_record",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(200, "", &bytes),
                ranged(1024, 4096, b'E'),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut first = transfer_hook_worker(
                root.path(),
                server.transport(),
                |s, n, _| {
                    if s == "write" && n == 1024 {
                        fail(Some("lost-ack"));
                    }
                },
                None,
            );
            first.start(server.release(&bytes)).unwrap();
            let failed = wait(&first, Phase::Failed);
            assert!(failed.uncertain);
            assert_eq!(failed.committed, [0, 0]);
            first.shutdown();
            let mut next = fixture_worker(root.path(), server.transport(), noop);
            let t = next.start(server.release(&bytes)).unwrap();
            wait(&next, Phase::Ready);
            next.consume(&t).unwrap();
            wait(&next, Phase::Consumed);
            next.shutdown();
            assert!(server.requests()[3].contains("range: bytes=1024-"));
        },
    );

    isolated(
        "lost_reset_ack_does_not_truncate_until_fresh_reacquisition",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(200, "", &bytes),
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
                response(200, "", &vec![b'R'; 8192]),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut first = transfer_hook_worker(
                root.path(),
                server.transport(),
                |s, n, shared| {
                    if s == "write" && n == 1024 {
                        active_flags(shared).pause.store(true, Ordering::Release);
                    }
                },
                None,
            );
            first.start(server.release(&bytes)).unwrap();
            wait(&first, Phase::Paused);
            first.shutdown();
            let mut reset = fixture_worker_hook(root.path(), server.transport(), noop, |_| {
                fail(Some("lost-ack"))
            });
            reset.start(server.release(&bytes)).unwrap();
            let failed = wait(&reset, Phase::Failed);
            assert!(failed.uncertain);
            assert_eq!(failed.committed[0], 1024);
            reset.shutdown();
            assert_eq!(
                fs::read(
                    root.path()
                        .join("0/.tilo-part-0123456789abcdef0123456789abcdef")
                )
                .unwrap(),
                vec![b'E'; 1024]
            );
            let mut next = fixture_worker(root.path(), server.transport(), noop);
            let t = next.start(server.release(&bytes)).unwrap();
            wait(&next, Phase::Ready);
            next.consume(&t).unwrap();
            wait(&next, Phase::Consumed);
            next.shutdown();
            let requests = server.requests();
            assert!(requests[3].contains("range: bytes=1024-"));
            assert!(!requests[5].contains("range:"));
        },
    );
}

fn preparation_mutation_failure(cancel: bool) {
    for (index, first_final) in [(0, false), (1, false), (1, true)] {
        let bytes = manifest();
        let release = InternalRelease::transport_fixture(
            &bytes,
            "https://code.tilo.so/releases/manifest.json",
        );
        let selected = SelectedManifest::from_bytes(&release, bytes.clone()).unwrap();
        let root = tempfile::tempdir().unwrap();
        // Real zero/prefix records. Lost acknowledgement leaves three valid
        // survivors, then append a real uncommitted tail for preparation to trim.
        for i in 0..2 {
            let dir = root.path().join(i.to_string());
            fs::create_dir(&dir).unwrap();
            if i == 0 && first_final {
                fs::write(dir.join("final"), vec![b'E'; 4096]).unwrap();
                continue;
            }
            let (mut provider, mut file) = FreshDisk::create(&dir, &selected, i);
            initialize_fresh(&selected, i, &mut &mut provider, &mut file).unwrap();
            let mut session = Session::new(&selected, i, 999, &mut provider).unwrap();
            let mut prepared = session
                .prepare(&selected, &mut file, &AtomicBool::new(false))
                .unwrap();
            let byte = if i == 0 { b'E' } else { b'R' };
            super::super::tests::start_prefix(&mut prepared, &vec![byte; 512]);
            if i == index {
                fail(Some("lost-ack"));
                assert!(super::super::tests::prefix(&mut prepared, &vec![byte; 512]).is_err());
                fail(None);
            }
            drop(prepared);
            drop(session);
            if i == index {
                file.seek(SeekFrom::End(0)).unwrap();
                file.write_all(b"uncommitted tail").unwrap();
                file.sync_all().unwrap();
            }
        }
        drop(selected);
        let dir = root.path().join(index.to_string());
        let leaf = dir.join(".tilo-part-0123456789abcdef0123456789abcdef");
        assert_eq!(fs::metadata(&leaf).unwrap().len(), 1040);
        let record2 = fs::read(dir.join("journal/record-2")).unwrap();
        let record3 = fs::read(dir.join("journal/record-3")).unwrap();
        assert!(dir.join("journal/record-1").exists());
        let server = Server::new(vec![response(200, "", &bytes)]);
        let transport = server.transport();
        let path = root.path().to_owned();
        let mut worker = Worker::spawn_runner(move |shared| {
            ROOT.with(|r| *r.borrow_mut() = path);
            let control = shared.clone();
            super::super::tests::with_preparation_hook(
                Box::new(move |i| {
                    if i == index {
                        if cancel {
                            active_flags(&control).cancel.store(true, Ordering::Release);
                        }
                        return Err("injected refusal after preparation mutations".into());
                    }
                    Ok(())
                }),
                || worker_loop(shared, transport, noop),
            );
        })
        .unwrap();
        worker.start(server.release(&bytes)).unwrap();
        let flags = active_flags(&worker.shared);
        let status = wait(
            &worker,
            if cancel {
                Phase::Cancelled
            } else {
                Phase::Failed
            },
        );
        assert_eq!(flags.cancel.load(Ordering::Acquire), cancel);
        worker.shutdown();
        // Prove mutation happened before the refusal, and exact good survivors
        // remain. No HTTP payload was requested after preparation failed.
        assert_eq!(
            fs::read(&leaf).unwrap(),
            vec![if index == 0 { b'E' } else { b'R' }; 1024]
        );
        assert!(!dir.join("journal/record-1").exists());
        assert_eq!(fs::read(dir.join("journal/record-2")).unwrap(), record2);
        assert_eq!(fs::read(dir.join("journal/record-3")).unwrap(), record3);
        assert_eq!(fs::read_dir(dir.join("journal")).unwrap().count(), 2);
        assert_eq!(server.requests().len(), 1);
        assert!(
            status
                .error
                .as_ref()
                .unwrap()
                .contains("after preparation mutations")
        );
        if first_final {
            assert_eq!(status.finals, [true, false]);
            assert_eq!(
                fs::read(root.path().join("0/final")).unwrap(),
                vec![b'E'; 4096]
            );
        } else if index == 1 {
            assert_eq!(status.committed[0], 512);
        }
        assert!(
            status.uncertain,
            "mutating preparation refusal must remain uncertain: {status:?}"
        );
    }
}

/// Preparation mutation and held reader failure facts.
#[test]
fn failure_facts_keep_errors_and_uncertainty_despite_cancellation() {
    isolated(
        "preparation_mutation_failure_retains_uncertainty_and_other_component_facts",
        || {
            preparation_mutation_failure(false);
        },
    );

    isolated(
        "cancelled_preparation_mutation_retains_uncertainty_and_error_facts",
        || {
            preparation_mutation_failure(true);
        },
    );

    isolated(
        "held_reader_composition_keeps_all_observed_errors_despite_cancellation",
        || {
            let result: Result<(), String> = super::super::finish_held_read(
                Err("verification digest failure".into()),
                Err("restoration failure".into()),
                Err("held identity failure".into()),
                Err("root security failure".into()),
                Err("cancelled".into()),
            );
            let error = result.unwrap_err();
            for fact in [
                "verification digest failure",
                "restoration failure",
                "held identity failure",
                "root security failure",
                "cancelled",
            ] {
                assert!(error.contains(fact), "missing {fact}: {error}");
            }
            assert_eq!(
                super::super::finish_held_read(Ok(42), Ok(17), Ok(()), Ok(()), Ok(())).unwrap(),
                42
            );
        },
    );

    isolated(
        "cancelled_reader_boundary_after_success_keeps_error_and_uncertainty",
        || {
            cancelled_reader_failure(false);
        },
    );

    isolated(
        "cancelled_reader_verification_failure_keeps_error_and_uncertainty",
        || {
            cancelled_reader_failure(true);
        },
    );
}

fn cancelled_reader_failure(verification: bool) {
    for boundary in ["reader-restored", "reader-held", "reader-root"] {
        let bytes = manifest();
        let server = Server::new(vec![
            response(200, "", &bytes),
            response(200, "", &vec![b'E'; 4096]),
            response(200, "", &vec![b'R'; 8192]),
        ]);
        let root = tempfile::tempdir().unwrap();
        let called = Arc::new(AtomicBool::new(false));
        let c = called.clone();
        let checks = Arc::new(Mutex::new(Vec::new()));
        let observed = checks.clone();
        let mut worker = fixture_worker_hook(
            root.path(),
            server.transport(),
            move |_, _, _, _| {
                c.store(true, Ordering::Release);
                Ok(())
            },
            move |shared| {
                SETUP_HOOK.with(|h| {
                    *h.borrow_mut() = Some(Box::new(move |i, stage| {
                        if stage.starts_with("reader-") {
                            observed.lock().unwrap().push((i, stage.to_owned()));
                        }
                        if i == 1 && stage == boundary {
                            active_flags(&shared).cancel.store(true, Ordering::Release);
                            if !verification {
                                return Err(format!("injected {boundary} failure"));
                            }
                        }
                        Ok(())
                    }))
                });
            },
        );
        let token = worker.start(server.release(&bytes)).unwrap();
        wait(&worker, Phase::Ready);
        if verification {
            // Preserve the inode and length, but corrupt bytes before fresh
            // consumer verification. Cancellation comes only in later checks.
            let mut file = File::options()
                .write(true)
                .open(root.path().join("1/final"))
                .unwrap();
            file.write_all(&vec![b'X'; 8192]).unwrap();
            file.sync_all().unwrap();
            drop(file);
        }
        let flags = active_flags(&worker.shared);
        worker.consume(&token).unwrap();
        let status = wait(&worker, Phase::Cancelled);
        assert!(flags.cancel.load(Ordering::Acquire));
        worker.shutdown();
        assert_eq!(called.load(Ordering::Acquire), !verification);
        assert_eq!(status.finals, [true, true]);
        assert_eq!(
            fs::read(root.path().join("1/final")).unwrap(),
            vec![if verification { b'X' } else { b'R' }; 8192]
        );
        // Every postcheck runs on both held wrappers, including after a failed
        // inner verification, restoration or identity/root boundary.
        assert_eq!(
            *checks.lock().unwrap(),
            vec![
                (1, "reader-restored".into()),
                (1, "reader-held".into()),
                (1, "reader-root".into()),
                (0, "reader-restored".into()),
                (0, "reader-held".into()),
                (0, "reader-root".into())
            ]
        );
        let fact = if verification {
            "fixture final digest mismatch".into()
        } else {
            format!("injected {boundary} failure")
        };
        assert!(
            status.error.as_ref().unwrap().contains(&fact),
            "lost observed reader error: {status:?}"
        );
        assert!(
            status.uncertain,
            "failed reader boundary must preserve uncertainty: {status:?}"
        );
    }
}

/// Runs one merged phase on a fresh thread so the thread-local fault hooks,
/// scripts and counters start clean, exactly as when the phase was its own
/// test; a failing phase re-raises its original panic payload.
fn isolated(phase: &str, body: impl FnOnce() + Send) {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name(phase.to_owned())
            .spawn_scoped(scope, body)
            .expect("spawn isolated test phase");
        if let Err(panic) = handle.join() {
            std::panic::resume_unwind(panic);
        }
    });
}
