use super::*;
use crate::backend::test_support::isolated;
use crate::setup::internal_bundle::tests::manifest;
use test_http::Responder;

tilo_test_env::scenario! {
    fn an_internal_download_accepts_only_authenticated_resumable_bytes;

    phase manifest_and_selected_context_require_exact_authenticated_bytes;
    {
        isolated(
            "selected_context_requires_exact_original_authenticated_bytes_and_asset_url",
            || {
                let bytes = manifest();
                let release = InternalRelease::transport_fixture(
                    &bytes,
                    "https://code.tilo.so/releases/manifest.json",
                );
                assert!(SelectedManifest::from_bytes(&release, bytes.clone()).is_ok());
                let mut changed = bytes.clone();
                changed.push(b' ');
                assert!(SelectedManifest::from_bytes(&release, changed).is_err());
                for url in [
                    "https://code.tilo.so/releases/",
                    "https://code.tilo.so",
                    "http://code.tilo.so/m",
                    "https://evil.test/m",
                    "https://code.tilo.so:444/m",
                    "https://user@code.tilo.so/m",
                    "https://code.tilo.so/m?q=1",
                    "https://code.tilo.so/m#f",
                ] {
                    let release = InternalRelease::transport_fixture(&bytes, url);
                    assert!(
                        SelectedManifest::from_bytes(&release, bytes.clone()).is_err(),
                        "{url}"
                    );
                }
            },
        );

        isolated(
            "a_manifest_fetch_is_exact_bounded_and_authenticated",
            || {
                let mut bytes = manifest();
                bytes.resize(65536, b' ');
                let server = Server::new(vec![response(200, "Content-Length: 65536\r\n", &bytes)]);
                assert_eq!(
                    server
                        .transport()
                        .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                        .unwrap()
                        .bytes()
                        .len(),
                    65536
                );
                let mut excess = bytes.clone();
                excess.push(b' ');
                let server = Server::new(vec![response(200, "", &excess)]);
                assert!(
                    server
                        .transport()
                        .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                        .is_err()
                );
                let mut value = super::super::internal_bundle::tests::fixture();
                value["release_id"] = serde_json::json!("different");
                let bytes = serde_json::to_vec(&value).unwrap();
                let release = InternalRelease::transport_fixture(&bytes, "https://code.tilo.so/m");
                assert!(SelectedManifest::from_bytes(&release, bytes).is_err());
            },
        );
    }
}

use std::{
    io::{Read, Seek, Write},
    sync::{Arc, Mutex},
    thread,
};
pub(in crate::setup) struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    _served: Responder,
}
impl Server {
    pub(in crate::setup) fn new(responses: Vec<Vec<u8>>) -> Self {
        Self::on_request(responses, || {})
    }
    pub(in crate::setup) fn on_request(
        responses: Vec<Vec<u8>>,
        on_request: impl FnMut() + Send + 'static,
    ) -> Self {
        let responses = Arc::new(Mutex::new(
            responses
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let on_request = Arc::new(Mutex::new(on_request));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let served = Responder::serve(move |peer| {
            let mut head = format!(
                "{} {} HTTP/1.1\r\n",
                peer.request().method,
                peer.request().target
            );
            for (name, value) in &peer.request().headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            seen.lock().unwrap().push(head);
            (on_request.lock().unwrap())();
            let answer = responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| response(500, "", b""));
            let _ = peer.write(&answer);
        });
        let url = format!("{}/releases/manifest.json", served.url());
        Self {
            url,
            requests,
            _served: served,
        }
    }
    pub(in crate::setup) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
    /// A release server that answers by what was asked for rather than by
    /// request order: the manifest, either component, every time. A download
    /// that is paused, cancelled or retried abandons requests, and the next
    /// one must still find its own bytes.
    pub(in crate::setup) fn serve_release(
        manifest: Vec<u8>,
        esp: Vec<u8>,
        root: Vec<u8>,
        on_request: impl FnMut(&str) + Send + 'static,
    ) -> Self {
        let bodies = Arc::new((manifest, esp, root));
        let on_request = Arc::new(Mutex::new(on_request));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let served = Responder::serve(move |peer| {
            let target = peer.request().target.clone();
            seen.lock().unwrap().push(target.clone());
            (on_request.lock().unwrap())(&target);
            let (manifest, esp, root) = &*bodies;
            let body = if target.ends_with("staging-esp.img") {
                esp
            } else if target.ends_with("staging-root.img") {
                root
            } else {
                manifest
            };
            let _ = peer.write(&response(200, "", body));
        });
        let url = format!("{}/releases/manifest.json", served.url());
        Self {
            url,
            requests,
            _served: served,
        }
    }
    pub(in crate::setup) fn transport(&self) -> InternalTransport {
        InternalTransport::client(Url::parse(&self.url).unwrap(), tilo_test_env::hang_bound())
            .unwrap()
    }
    pub(in crate::setup) fn release(&self, bytes: &[u8]) -> InternalRelease {
        InternalRelease::transport_fixture(bytes, &self.url)
    }
}
pub(in crate::setup) fn response(status: u16, headers: &str, bytes: &[u8]) -> Vec<u8> {
    let mut result =
        format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    result.extend_from_slice(bytes);
    result
}
/// The fixture release as a server answers it in order: `manifest`, then its two components.
pub(in crate::setup) fn release_responses(manifest: &[u8]) -> Vec<Vec<u8>> {
    vec![
        response(200, "", manifest),
        response(200, "", &vec![b'E'; 4096]),
        response(200, "", &vec![b'R'; 8192]),
    ]
}
fn context(server: &Server) -> SelectedManifest {
    let bytes = manifest();
    SelectedManifest::checked(
        &server.release(&bytes),
        bytes,
        &Url::parse(&server.url).unwrap(),
    )
    .unwrap()
}
fn contents(file: &mut File) -> Vec<u8> {
    file.rewind().unwrap();
    let mut b = Vec::new();
    file.read_to_end(&mut b).unwrap();
    b
}
type Hook = Box<dyn FnMut(&str, u64)>;
thread_local! { static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) }; }
pub(super) fn event(stage: &str, count: u64) {
    HOOK.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            h(stage, count);
        }
    });
}
pub(in crate::setup) fn with_hook(hook: Hook, test: impl FnOnce()) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }
    HOOK.with(|h| *h.borrow_mut() = Some(hook));
    let _reset = Reset;
    test();
}

thread_local! { static WRITE_REMAINING: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
pub(super) struct FaultWriter<'a>(pub(super) &'a mut File);
impl Write for FaultWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        WRITE_REMAINING.with(|remaining| match remaining.get() {
            Some(0) => Err(std::io::Error::other("injected after real partial write")),
            Some(limit) => {
                let count = self.0.write(&bytes[..bytes.len().min(limit)])?;
                remaining.set(Some(limit - count));
                Ok(count)
            }
            None => self.0.write(bytes),
        })
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
fn with_write_failure(after: usize, test: impl FnOnce()) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            WRITE_REMAINING.with(|r| r.set(None));
        }
    }
    WRITE_REMAINING.with(|r| r.set(Some(after)));
    let _reset = Reset;
    test();
}

thread_local! { static CHECKPOINT_INTERVAL: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) }; }
pub(super) fn checkpoint_interval_override() -> Option<u64> {
    CHECKPOINT_INTERVAL.get()
}

#[cfg(unix)]
use super::super::internal_cache::tests::{
    DiskProvider, clear_reads, fail, read_totals, start_prefix, test_prepared,
};
#[cfg(unix)]
use super::super::internal_cache::{Provider, Session};
tilo_test_env::scenario! {
    #[cfg(unix)]
    fn a_persistent_internal_download_commits_only_what_it_validated;

    phase persistent_download_commits_only_verified_tails;
    {
        isolated(
            "persistent_success_has_linear_readback_final_verification_and_reopened_candidate",
            || {
                let payload = vec![b'R'; 512 * 1024];
                let mut value = super::super::internal_bundle::tests::fixture();
                value["components"][1]["byte_size"] = serde_json::json!(payload.len());
                value["components"][1]["sha256"] =
                    serde_json::json!(hex::encode(Sha256::digest(&payload)));
                value["source_image_bytes"] = serde_json::json!(payload.len() + 21504);
                let bytes = serde_json::to_vec(&value).unwrap();
                let server = Server::new(vec![response(200, "", &payload)]);
                let selected = SelectedManifest::checked(
                    &server.release(&bytes),
                    bytes,
                    &Url::parse(&server.url).unwrap(),
                )
                .unwrap();
                let dir = tempfile::tempdir().unwrap();
                let (p, mut f) = DiskProvider::new(dir.path());
                let own = p.ownership();
                let cancel = AtomicBool::new(false);
                let pause = AtomicBool::new(false);
                clear_reads();
                CHECKPOINT_INTERVAL.set(Some(32768));
                {
                    let mut session = Session::new(&selected, 1, 41, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    assert!(matches!(
                        server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &pause, |_| {})
                            .unwrap(),
                        PersistentOutcome::Complete(_)
                    ));
                }
                CHECKPOINT_INTERVAL.set(None);
                assert_eq!(contents(&mut f), payload);
                assert_eq!(read_totals("periodic"), (524288, 524288));
                assert_eq!(
                    std::fs::read_dir(dir.path().join("journal"))
                        .unwrap()
                        .count(),
                    2
                );
                drop(f);
                clear_reads();
                let (p, mut f) = DiskProvider::reopen(dir.path(), own);
                let mut session = Session::new(&selected, 1, 42, p).unwrap();
                let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                assert_eq!(read_totals("restart"), (524288, 524288));
                assert!(matches!(
                    server
                        .transport()
                        .transfer_persistent(prepared, &cancel, &pause, |_| {})
                        .unwrap(),
                    PersistentOutcome::Complete(_)
                ));
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            },
        );

        isolated(
            "persistent_pause_retains_tail_but_full_count_before_eof_is_not_committed",
            || {
                for full in [false, true] {
                    let data = vec![b'E'; if full { 4096 } else { 1024 }];
                    let server = Server::new(vec![response(200, "Content-Length: 4096\r\n", &data)]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (p, mut f) = DiskProvider::new(dir.path());
                    let own = p.ownership();
                    let cancel = AtomicBool::new(false);
                    let pause = Arc::new(AtomicBool::new(false));
                    let signal = pause.clone();
                    let mut session = Session::new(&selected, 0, 1, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    with_hook(
                        Box::new(move |stage, _| {
                            if stage == "write" {
                                signal.store(true, Ordering::Release);
                            }
                        }),
                        || {
                            assert!(
                                matches!(server.transport().transfer_persistent(prepared,&cancel,&pause,|_|{}).unwrap(),PersistentOutcome::Paused{committed} if committed==if full {0}else{1024})
                            );
                        },
                    );
                    drop(session);
                    drop(f);
                    let (p, mut f) = DiskProvider::reopen(dir.path(), own);
                    let mut session = Session::new(&selected, 0, 2, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    let committed = prepared.committed();
                    assert_eq!(committed, if full { 0 } else { 1024 });
                    drop(prepared);
                    assert_eq!(f.metadata().unwrap().len(), committed);
                }
            },
        );

        isolated(
            "persistent_failed_reset_ack_never_truncates_and_failed_full_eof_never_commits",
            || {
                for full_eof in [false, true] {
                    let body = vec![b'E'; 4096];
                    let raw = if full_eof {
                        response(
                            200,
                            "Transfer-Encoding: chunked\r\n",
                            format!("1000\r\n{}\r\n", String::from_utf8_lossy(&body)).as_bytes(),
                        )
                    } else {
                        response(416, "", b"")
                    };
                    let server = Server::new(vec![raw]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (p, mut f) = DiskProvider::new(dir.path());
                    let own = p.ownership();
                    let cancel = AtomicBool::new(false);
                    let mut session = Session::new(&selected, 0, 1, p).unwrap();
                    let mut prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    if !full_eof {
                        start_prefix(&mut prepared, &vec![b'E'; 1024]);
                        drop(prepared);
                        drop(session);
                        drop(f);
                        let reopened = DiskProvider::reopen(dir.path(), own.clone());
                        f = reopened.1;
                        session = Session::new(&selected, 0, 2, reopened.0).unwrap();
                        prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    }
                    fail(Some("record-create"));
                    let error = server
                        .transport()
                        .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |_| {})
                        .unwrap_err();
                    fail(None);
                    assert!(error.message.contains("record-create"));
                    assert!(error.uncertain);
                    assert_eq!(error.committed, if full_eof { 0 } else { 1024 });
                    assert_eq!(
                        f.metadata().unwrap().len(),
                        if full_eof { 4096 } else { 1024 }
                    );
                    drop(session);
                    drop(f);
                    let (p, mut f) = DiskProvider::reopen(dir.path(), own);
                    let mut session = Session::new(&selected, 0, 3, p).unwrap();
                    assert_eq!(
                        session
                            .prepare(&selected, &mut f, &cancel)
                            .unwrap()
                            .committed(),
                        if full_eof { 0 } else { 1024 }
                    );
                    assert_eq!(server.requests.lock().unwrap().len(), 1);
                }
            },
        );

        isolated(
            "persistent_cancel_wins_pause_and_final_callback_and_partial_write_retains_prefix",
            || {
                for mode in [0, 1, 2] {
                    let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (p, mut f) = DiskProvider::new(dir.path());
                    let cancel = Arc::new(AtomicBool::new(false));
                    let pause = Arc::new(AtomicBool::new(false));
                    let mut session = Session::new(&selected, 0, 1, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    if mode == 0 {
                        let c = cancel.clone();
                        let p = pause.clone();
                        with_hook(
                            Box::new(move |stage, _| {
                                if stage == "write" {
                                    c.store(true, Ordering::Release);
                                    p.store(true, Ordering::Release);
                                }
                            }),
                            || {
                                assert!(matches!(
                                    server
                                        .transport()
                                        .transfer_persistent(prepared, &cancel, &pause, |_| {})
                                        .unwrap(),
                                    PersistentOutcome::Cancelled { committed: 0, .. }
                                ));
                            },
                        );
                        assert!(cancel.load(Ordering::Acquire));
                    } else if mode == 1 {
                        assert!(matches!(
                            server
                                .transport()
                                .transfer_persistent(prepared, &cancel, &pause, |p| {
                                    if p.phase == Phase::Complete {
                                        cancel.store(true, Ordering::Release)
                                    }
                                })
                                .unwrap(),
                            PersistentOutcome::Cancelled { committed: 0, .. }
                        ));
                    } else {
                        with_write_failure(17, || {
                            let error = server
                                .transport()
                                .transfer_persistent(prepared, &cancel, &pause, |_| {})
                                .unwrap_err();
                            assert_eq!(error.committed, 0);
                        });
                        assert_eq!(contents(&mut f), vec![b'E'; 17]);
                    }
                }
            },
        );

        isolated(
            "persistent_retry_revalidates_frozen_prefix_after_advisory_callback",
            || {
                let server = Server::new(vec![response(
                    200,
                    "Content-Length: 4096\r\n",
                    &vec![b'E'; 1024],
                )]);
                let selected = context(&server);
                let dir = tempfile::tempdir().unwrap();
                let (p, mut f) = DiskProvider::new(dir.path());
                let own = p.ownership();
                let cancel = AtomicBool::new(false);
                let path = dir.path().join(own.leaf);
                let mut session = Session::new(&selected, 0, 1, p).unwrap();
                let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                let error = server
                    .transport()
                    .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |event| {
                        if event.phase == Phase::Retrying {
                            let mut corrupt = File::options().write(true).open(&path).unwrap();
                            corrupt.write_all(b"X").unwrap();
                        }
                    })
                    .unwrap_err();
                assert!(error.message.contains("digest mismatch"));
                assert_eq!(error.committed, 1024);
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            },
        );
    }

    phase persistent_pause_and_stale_acks_preserve_one_validated_prefix;
    {
        isolated(
            "persistent_stale_ack_refuses_truncation_and_final_ack_cancel_stays_terminal",
            || {
                for stale in [false, true] {
                    let server = Server::new(vec![if stale {
                        response(416, "", b"")
                    } else {
                        response(200, "", &vec![b'E'; 4096])
                    }]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (p, mut f) = DiskProvider::new(dir.path());
                    let own = p.ownership();
                    let cancel = AtomicBool::new(false);
                    let mut session = Session::new(&selected, 0, 1, p).unwrap();
                    let mut prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    if stale {
                        start_prefix(&mut prepared, &vec![b'E'; 1024]);
                        drop(prepared);
                        drop(session);
                        drop(f);
                        let (p, file) = DiskProvider::reopen(dir.path(), own);
                        f = file;
                        session = Session::new(&selected, 0, 2, p).unwrap();
                        prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    }
                    let parts = prepared.into_parts();
                    let mut observer = AckAdapter {
                        inner: parts.observer,
                        stale,
                        cancel: if stale { None } else { Some(&cancel) },
                    };
                    let prepared = test_prepared(cache::TransferParts {
                        observer: &mut observer,
                        ..parts
                    });
                    let result = server.transport().transfer_persistent(
                        prepared,
                        &cancel,
                        &AtomicBool::new(false),
                        |_| {},
                    );
                    if stale {
                        let error = result.unwrap_err();
                        assert!(error.message.contains("Stale durable"));
                        assert_eq!(error.committed, 1024);
                        assert!(error.uncertain);
                        assert_eq!(f.metadata().unwrap().len(), 1024);
                    } else {
                        assert!(matches!(
                            result.unwrap(),
                            PersistentOutcome::Cancelled {
                                committed: 4096,
                                error: None,
                                uncertain: false
                            }
                        ));
                        assert!(cancel.load(Ordering::Acquire));
                    }
                }
            },
        );

        isolated(
            "persistent_failed_full_eof_reset_allows_only_a_new_successful_response",
            || {
                let payload = vec![b'E'; 4096];
                let broken = response(
                    200,
                    "Transfer-Encoding: chunked\r\n",
                    format!("1000\r\n{}\r\n", String::from_utf8_lossy(&payload)).as_bytes(),
                );
                let server = Server::new(vec![broken, response(200, "", &payload)]);
                let selected = context(&server);
                let dir = tempfile::tempdir().unwrap();
                let (p, mut f) = DiskProvider::new(dir.path());
                let cancel = AtomicBool::new(false);
                let mut session = Session::new(&selected, 0, 1, p).unwrap();
                let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                let mut resets = 0;
                assert!(matches!(
                    server
                        .transport()
                        .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |p| {
                            if p.phase == Phase::Resetting {
                                resets += 1
                            }
                        })
                        .unwrap(),
                    PersistentOutcome::Complete(_)
                ));
                assert_eq!(resets, 1);
                assert_eq!(server.requests.lock().unwrap().len(), 2);
                assert_eq!(contents(&mut f), payload);
            },
        );

        isolated(
            "persistent_pause_checkpoint_failure_reports_prior_commit_and_uncertainty",
            || {
                let server = Server::new(vec![response(
                    200,
                    "Content-Length: 4096\r\n",
                    &vec![b'E'; 1024],
                )]);
                let selected = context(&server);
                let dir = tempfile::tempdir().unwrap();
                let (p, mut f) = DiskProvider::new(dir.path());
                let cancel = AtomicBool::new(false);
                let pause = Arc::new(AtomicBool::new(false));
                let signal = pause.clone();
                let mut session = Session::new(&selected, 0, 1, p).unwrap();
                let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                fail(Some("lost-ack"));
                with_hook(
                    Box::new(move |stage, _| {
                        if stage == "write" {
                            signal.store(true, Ordering::Release)
                        }
                    }),
                    || {
                        let error = server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &pause, |_| {})
                            .unwrap_err();
                        assert_eq!(error.committed, 0);
                        assert!(error.uncertain);
                        assert!(error.message.contains("lost-ack"));
                    },
                );
                fail(None);
                assert_eq!(contents(&mut f), vec![b'E'; 1024]);
            },
        );

        isolated(
            "persistent_partial_pause_close_reauthenticate_reopen_resumes_one_validated_prefix",
            || {
                let server = Server::new(vec![
                    response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                    response(
                        206,
                        "Content-Range: bytes 1024-4095/4096\r\nContent-Length: 3072\r\n",
                        &vec![b'E'; 3072],
                    ),
                ]);
                let dir = tempfile::tempdir().unwrap();
                let (p, mut f) = DiskProvider::new(dir.path());
                let own = p.ownership();
                let cancel = AtomicBool::new(false);
                let pause = Arc::new(AtomicBool::new(false));
                let signal = pause.clone();
                {
                    let selected = context(&server);
                    let mut session = Session::new(&selected, 0, 101, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    with_hook(
                        Box::new(move |stage, _| {
                            if stage == "write" {
                                signal.store(true, Ordering::Release)
                            }
                        }),
                        || {
                            assert!(matches!(
                                server
                                    .transport()
                                    .transfer_persistent(prepared, &cancel, &pause, |_| {})
                                    .unwrap(),
                                PersistentOutcome::Paused { committed: 1024 }
                            ));
                        },
                    );
                }
                drop(f);
                clear_reads();
                let selected = context(&server); // Reauthenticate exact manifest bytes in a new operation.
                let (p, mut f) = DiskProvider::reopen(dir.path(), own);
                let mut session = Session::new(&selected, 0, 102, p).unwrap();
                let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                assert_eq!(read_totals("restart"), (1024, 1024));
                let outcome = server
                    .transport()
                    .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |_| {})
                    .unwrap();
                match outcome {
                    PersistentOutcome::Complete(done) => assert_eq!(done.byte_size(), 4096),
                    other => panic!("{other:?}"),
                }
                assert_eq!(contents(&mut f), vec![b'E'; 4096]);
                assert_eq!(read_totals("restart"), (1024, 1024));
                assert_eq!(read_totals("periodic"), (3072, 3072));
                assert_eq!(read_totals("retry"), (0, 0));
                let requests = server.requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                assert!(!requests[0].contains("range:"));
                assert!(requests[1].contains("range: bytes=1024-"));
                println!(
                    "restart bytes requested/actual=1024/1024; new-tail readback=3072/3072; resumed HTTP body=3072; saved network bytes=1024"
                );
            },
        );

        isolated(
            "persistent_pause_at_entry_or_request_boundary_never_starts_network_or_completes",
            || {
                for at_request in [false, true] {
                    let server = Server::new(vec![]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (p, mut f) = DiskProvider::new(dir.path());
                    let cancel = AtomicBool::new(false);
                    let pause = AtomicBool::new(!at_request);
                    let mut session = Session::new(&selected, 0, 1, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    assert!(matches!(
                        server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &pause, |p| {
                                if at_request && p.phase == Phase::Requesting {
                                    pause.store(true, Ordering::Release)
                                }
                            })
                            .unwrap(),
                        PersistentOutcome::Paused { committed: 0 }
                    ));
                    assert!(server.requests.lock().unwrap().is_empty());
                }
                let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                let selected = context(&server);
                let dir = tempfile::tempdir().unwrap();
                let (p, mut f) = DiskProvider::new(dir.path());
                let own = p.ownership();
                let cancel = AtomicBool::new(false);
                {
                    let mut session = Session::new(&selected, 0, 1, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    assert!(matches!(
                        server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |_| {})
                            .unwrap(),
                        PersistentOutcome::Complete(_)
                    ));
                }
                drop(f);
                let (p, mut f) = DiskProvider::reopen(dir.path(), own);
                let mut session = Session::new(&selected, 0, 2, p).unwrap();
                let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                assert!(matches!(
                    server
                        .transport()
                        .transfer_persistent(prepared, &cancel, &AtomicBool::new(true), |_| {})
                        .unwrap(),
                    PersistentOutcome::Paused { committed: 4096 }
                ));
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            },
        );
    }

    phase returned_request_boundaries_observe_pause_cancel_and_reset;
    {
        isolated(
            "returned_request_boundary_preserves_prefix_before_status_redirect_or_error",
            || {
                for raw in [
                    response(200, "", b""),
                    response(416, "", b""),
                    response(503, "", b""),
                    response(302, "Location: /redirected\r\n", b""),
                    b"invalid HTTP response\r\n\r\n".to_vec(),
                ] {
                    for (request_number, cancel_too) in [(1, false), (1, true), (3, false), (3, true)] {
                        let pause = Arc::new(AtomicBool::new(false));
                        let cancel = Arc::new(AtomicBool::new(false));
                        let p = pause.clone();
                        let c = cancel.clone();
                        let mut responses = vec![response(503, "", b""); request_number - 1];
                        responses.push(raw.clone());
                        let mut count = 0;
                        let server = Server::on_request(responses, move || {
                            count += 1;
                            if count == request_number {
                                p.store(true, Ordering::Release);
                                if cancel_too {
                                    c.store(true, Ordering::Release);
                                }
                            }
                        });
                        let selected = context(&server);
                        let dir = tempfile::tempdir().unwrap();
                        let (provider, mut file) = DiskProvider::new(dir.path());
                        let own = provider.ownership();
                        {
                            let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                            let mut prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                            start_prefix(&mut prepared, &vec![b'E'; 1024]);
                        }
                        drop(file);
                        let (provider, mut file) = DiskProvider::reopen(dir.path(), own);
                        let mut session = Session::new(&selected, 0, 2, provider).unwrap();
                        let prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                        let record = std::fs::read(dir.path().join("journal/record-2")).unwrap();
                        let result = server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &pause, |_| {})
                            .unwrap();
                        if cancel_too {
                            assert!(matches!(
                                result,
                                PersistentOutcome::Cancelled {
                                    committed: 1024,
                                    ..
                                }
                            ));
                        } else {
                            assert!(
                                matches!(result, PersistentOutcome::Paused { committed: 1024 }),
                                "{result:?}"
                            );
                        }
                        assert_eq!(contents(&mut file), vec![b'E'; 1024]);
                        assert_eq!(
                            std::fs::read(dir.path().join("journal/record-2")).unwrap(),
                            record
                        );
                        assert_eq!(
                            std::fs::read_dir(dir.path().join("journal"))
                                .unwrap()
                                .count(),
                            2
                        );
                        assert_eq!(server.requests.lock().unwrap().len(), request_number);
                    }
                }
            },
        );

        isolated(
            "returned_request_without_pause_still_resets_and_finishes",
            || {
                for status in [200, 416] {
                    let server = Server::on_request(
                        vec![
                            response(status, "", b""),
                            response(200, "", &vec![b'E'; 4096]),
                        ],
                        || {},
                    );
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (provider, mut file) = DiskProvider::new(dir.path());
                    let own = provider.ownership();
                    let cancel = AtomicBool::new(false);
                    {
                        let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                        let mut prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                        start_prefix(&mut prepared, &vec![b'E'; 1024]);
                    }
                    drop(file);
                    let (provider, mut file) = DiskProvider::reopen(dir.path(), own);
                    let mut session = Session::new(&selected, 0, 2, provider).unwrap();
                    let prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                    let mut resets = 0;
                    assert!(matches!(
                        server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |p| {
                                if p.phase == Phase::Resetting {
                                    resets += 1
                                }
                            })
                            .unwrap(),
                        PersistentOutcome::Complete(_)
                    ));
                    assert_eq!(resets, 1);
                    assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                    assert_eq!(server.requests.lock().unwrap().len(), 2);
                }
            },
        );

        isolated(
            "returned_eof_or_read_error_observes_pause_and_cancel_before_final_or_reset",
            || {
                for (valid_eof, mode) in [(true, 0), (true, 1), (true, 2), (false, 1), (false, 2)] {
                    let payload = vec![b'E'; 4096];
                    let mut encoded =
                        format!("1000\r\n{}\r\n", String::from_utf8_lossy(&payload)).into_bytes();
                    if valid_eof {
                        encoded.extend_from_slice(b"0\r\n\r\n");
                    }
                    let server = Server::new(vec![response(
                        200,
                        "Transfer-Encoding: chunked\r\n",
                        &encoded,
                    )]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (provider, mut file) = DiskProvider::new(dir.path());
                    let pause = Arc::new(AtomicBool::new(false));
                    let cancel = Arc::new(AtomicBool::new(false));
                    let p = pause.clone();
                    let c = cancel.clone();
                    let (entered, waiting) = std::sync::mpsc::sync_channel(0);
                    let (release, released) = std::sync::mpsc::sync_channel(0);
                    let controller = thread::spawn(move || {
                        tilo_test_env::recv("the body read to block", &waiting);
                        if mode > 0 {
                            p.store(true, Ordering::Release);
                        }
                        if mode == 2 {
                            c.store(true, Ordering::Release);
                        }
                        release.send(()).unwrap();
                    });
                    let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                    let prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                    let record = std::fs::read(dir.path().join("journal/record-1")).unwrap();
                    let mut result = None;
                    with_hook(
                        Box::new(move |stage, n| {
                            if stage == "write" && n == 4096 {
                                entered.send(()).unwrap();
                                tilo_test_env::recv("the body read release", &released);
                            }
                        }),
                        || {
                            result = Some(server.transport().transfer_persistent(
                                prepared,
                                &cancel,
                                &pause,
                                |_| {},
                            ));
                        },
                    );
                    controller.join().unwrap();
                    let result = result.unwrap().unwrap();
                    match mode {
                        0 => assert!(matches!(result, PersistentOutcome::Complete(_))),
                        1 => assert!(
                            matches!(result, PersistentOutcome::Paused { committed: 0 }),
                            "{result:?}"
                        ),
                        2 => assert!(matches!(
                            result,
                            PersistentOutcome::Cancelled { committed: 0, .. }
                        )),
                        _ => unreachable!(),
                    }
                    assert_eq!(contents(&mut file), payload);
                    if mode > 0 {
                        assert_eq!(
                            std::fs::read_dir(dir.path().join("journal"))
                                .unwrap()
                                .count(),
                            1
                        );
                        assert_eq!(
                            std::fs::read(dir.path().join("journal/record-1")).unwrap(),
                            record
                        );
                    } else {
                        assert_eq!(
                            std::fs::read(dir.path().join("journal/record-2")).unwrap()[8],
                            3
                        );
                    }
                    assert_eq!(server.requests.lock().unwrap().len(), 1);
                }
            },
        );
    }

    phase pause_near_final_publication_reports_the_actual_committed_state;
    {
        isolated(
            "pause_during_final_verification_or_complete_callback_retains_prior_prefix",
            || {
                for stage in ["Verifying", "Complete"] {
                    let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (provider, mut file) = DiskProvider::new(dir.path());
                    let cancel = AtomicBool::new(false);
                    let pause = Arc::new(AtomicBool::new(false));
                    let signal = pause.clone();
                    let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                    let prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                    let record = std::fs::read(dir.path().join("journal/record-1")).unwrap();
                    let mut result = None;
                    let mut complete = false;
                    with_hook(
                        Box::new(move |event, _| {
                            if event == stage {
                                signal.store(true, Ordering::Release);
                            }
                        }),
                        || {
                            result = Some(server.transport().transfer_persistent(
                                prepared,
                                &cancel,
                                &pause,
                                |p| {
                                    if p.phase == Phase::Complete {
                                        complete = true;
                                    }
                                    if (stage == "Complete" && p.phase == Phase::Complete)
                                        || (stage == "Verifying" && p.phase == Phase::Verifying)
                                    {
                                        pause.store(true, Ordering::Release);
                                    }
                                },
                            ));
                        },
                    );
                    assert!(
                        matches!(
                            result.unwrap().unwrap(),
                            PersistentOutcome::Paused { committed: 0 }
                        ),
                        "{stage}"
                    );
                    assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                    assert_eq!(
                        std::fs::read_dir(dir.path().join("journal"))
                            .unwrap()
                            .count(),
                        1
                    );
                    assert_eq!(
                        std::fs::read(dir.path().join("journal/record-1")).unwrap(),
                        record
                    );
                    assert_eq!(complete, stage == "Complete");
                }
            },
        );

        isolated(
            "pause_arriving_during_final_ack_reports_the_actual_committed_candidate",
            || {
                for cancel_too in [false, true] {
                    let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (provider, mut file) = DiskProvider::new(dir.path());
                    let cancel = AtomicBool::new(false);
                    let pause = AtomicBool::new(false);
                    let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                    let parts = session
                        .prepare(&selected, &mut file, &cancel)
                        .unwrap()
                        .into_parts();
                    let mut observer = PauseAtFinalAck {
                        inner: parts.observer,
                        pause: &pause,
                        cancel: if cancel_too { Some(&cancel) } else { None },
                    };
                    let prepared = test_prepared(cache::TransferParts {
                        observer: &mut observer,
                        ..parts
                    });
                    let result = server
                        .transport()
                        .transfer_persistent(prepared, &cancel, &pause, |_| {})
                        .unwrap();
                    if cancel_too {
                        assert!(matches!(
                            result,
                            PersistentOutcome::Cancelled {
                                committed: 4096,
                                ..
                            }
                        ));
                    } else {
                        assert!(
                            matches!(result, PersistentOutcome::Paused { committed: 4096 }),
                            "{result:?}"
                        );
                    }
                    assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                    assert_eq!(
                        std::fs::read(dir.path().join("journal/record-2")).unwrap()[8],
                        3
                    );
                    assert_eq!(server.requests.lock().unwrap().len(), 1);
                }
            },
        );

        isolated(
            "pause_cannot_hide_failed_final_publication_or_lost_ack_uncertainty",
            || {
                for cancel_too in [false, true] {
                    let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                    let selected = context(&server);
                    let dir = tempfile::tempdir().unwrap();
                    let (provider, mut file) = DiskProvider::new(dir.path());
                    let own = provider.ownership();
                    let cancel = AtomicBool::new(false);
                    let pause = AtomicBool::new(false);
                    let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                    let parts = session
                        .prepare(&selected, &mut file, &cancel)
                        .unwrap()
                        .into_parts();
                    let mut observer = PauseAtFinalAck {
                        inner: parts.observer,
                        pause: &pause,
                        cancel: if cancel_too { Some(&cancel) } else { None },
                    };
                    let prepared = test_prepared(cache::TransferParts {
                        observer: &mut observer,
                        ..parts
                    });
                    fail(Some("lost-ack"));
                    let result =
                        server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &pause, |_| {});
                    fail(None);
                    if cancel_too {
                        match result.unwrap() {
                            PersistentOutcome::Cancelled {
                                committed: 0,
                                error: Some(error),
                                uncertain: true,
                            } => assert!(error.contains("lost-ack")),
                            other => panic!("{other:?}"),
                        }
                    } else {
                        let error = result.unwrap_err();
                        assert!(error.uncertain);
                        assert_eq!(error.committed, 0);
                        assert!(error.message.contains("lost-ack"));
                    }
                    drop(observer);
                    assert!(pause.load(Ordering::Acquire));
                    assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                    assert_eq!(
                        std::fs::read(dir.path().join("journal/record-2")).unwrap()[8],
                        3
                    );
                    drop(session);
                    drop(file);
                    let (provider, mut file) = DiskProvider::reopen(dir.path(), own);
                    let mut session = Session::new(&selected, 0, 2, provider).unwrap();
                    assert_eq!(
                        session
                            .prepare(&selected, &mut file, &AtomicBool::new(false))
                            .unwrap()
                            .committed(),
                        4096
                    );
                }
            },
        );

        isolated(
            "pause_at_last_retry_boundary_precedes_terminal_failure",
            || {
                let server = Server::new(vec![response(503, "", b""); 3]);
                let selected = context(&server);
                let dir = tempfile::tempdir().unwrap();
                let (provider, mut file) = DiskProvider::new(dir.path());
                let own = provider.ownership();
                let cancel = AtomicBool::new(false);
                let pause = AtomicBool::new(false);
                {
                    let mut session = Session::new(&selected, 0, 1, provider).unwrap();
                    let mut prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                    start_prefix(&mut prepared, &vec![b'E'; 1024]);
                }
                drop(file);
                let (provider, mut file) = DiskProvider::reopen(dir.path(), own);
                let mut session = Session::new(&selected, 0, 2, provider).unwrap();
                let prepared = session.prepare(&selected, &mut file, &cancel).unwrap();
                let record = std::fs::read(dir.path().join("journal/record-2")).unwrap();
                let mut retries = 0;
                let result = server
                    .transport()
                    .transfer_persistent(prepared, &cancel, &pause, |p| {
                        if p.phase == Phase::Retrying {
                            retries += 1;
                            if retries == 3 {
                                pause.store(true, Ordering::Release);
                            }
                        }
                    })
                    .unwrap();
                assert!(matches!(
                    result,
                    PersistentOutcome::Paused { committed: 1024 }
                ));
                assert_eq!(contents(&mut file), vec![b'E'; 1024]);
                assert_eq!(
                    std::fs::read(dir.path().join("journal/record-2")).unwrap(),
                    record
                );
                assert_eq!(
                    std::fs::read_dir(dir.path().join("journal"))
                        .unwrap()
                        .count(),
                    2
                );
                assert_eq!(server.requests.lock().unwrap().len(), 3);
            },
        );
    }
}

#[cfg(unix)]
struct AckAdapter<'a> {
    inner: &'a mut dyn cache::Observer,
    stale: bool,
    cancel: Option<&'a AtomicBool>,
}
#[cfg(unix)]
impl cache::Observer for AckAdapter<'_> {
    fn begin_attempt(&mut self, id: cache::BoundaryId, file: &File) -> Result<(), String> {
        self.inner.begin_attempt(id, file)
    }
    fn boundary(
        &mut self,
        id: cache::BoundaryId,
        file: &mut File,
        c: cache::Candidate,
    ) -> Result<cache::Ack, String> {
        let mut ack = self.inner.boundary(id, file, c)?;
        if self.stale {
            ack.id.epoch += 1;
        }
        if let Some(cancel) = self.cancel {
            cancel.store(true, Ordering::Release);
        }
        Ok(ack)
    }
}

#[cfg(unix)]
struct PauseAtFinalAck<'a> {
    inner: &'a mut dyn cache::Observer,
    pause: &'a AtomicBool,
    cancel: Option<&'a AtomicBool>,
}
#[cfg(unix)]
impl cache::Observer for PauseAtFinalAck<'_> {
    fn begin_attempt(&mut self, id: cache::BoundaryId, file: &File) -> Result<(), String> {
        self.inner.begin_attempt(id, file)
    }
    fn boundary(
        &mut self,
        id: cache::BoundaryId,
        file: &mut File,
        candidate: cache::Candidate,
    ) -> Result<cache::Ack, String> {
        let final_candidate = candidate.kind == cache::Kind::Final;
        let result = self.inner.boundary(id, file, candidate);
        if final_candidate {
            self.pause.store(true, Ordering::Release);
            if let Some(cancel) = self.cancel {
                cancel.store(true, Ordering::Release);
            }
        }
        result
    }
}
pub(in crate::setup) fn with_checkpoint_interval<T>(value: u64, run: impl FnOnce() -> T) -> T {
    assert!(value > 0);
    struct Reset(Option<u64>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CHECKPOINT_INTERVAL.set(self.0);
        }
    }
    let _reset = Reset(CHECKPOINT_INTERVAL.replace(Some(value)));
    run()
}
