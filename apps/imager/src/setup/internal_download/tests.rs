use super::*;
use crate::setup::test_support::isolated;
fn manifest() -> Vec<u8> {
    super::super::internal_bundle::tests::bytes(&super::super::internal_bundle::tests::fixture())
}
/// Selected context, manifest HTTP, size boundary and transfer encoding.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn manifest_and_selected_context_require_exact_authenticated_bytes() {
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
        "manifest_http_requires_exact_length_digest_status_and_eof",
        || {
            let bytes = manifest();
            let server = Server::new(vec![response(200, "", &bytes)]);
            let selected = server
                .transport()
                .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                .unwrap();
            assert_eq!(selected.bytes(), bytes);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
            let mut long = bytes.clone();
            long.push(b' ');
            let mut corrupt = bytes.clone();
            corrupt[0] = b' ';
            for raw in [
                response(200, "", &bytes[..bytes.len() - 1]),
                response(200, "", &long),
                response(200, "", &corrupt),
                response(201, "", &bytes),
                response(200, "Content-Encoding: gzip\r\n", &bytes),
                response(200, "Content-Length: 1\r\n", &bytes),
                response(
                    200,
                    "Transfer-Encoding: chunked\r\n",
                    format!(
                        "{:x}\r\n{}\r\n",
                        bytes.len(),
                        String::from_utf8_lossy(&bytes)
                    )
                    .as_bytes(),
                ),
            ] {
                let server = Server::new(vec![raw]);
                assert!(
                    server
                        .transport()
                        .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                        .is_err()
                );
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            }
        },
    );

    isolated(
        "manifest_maximum_boundary_and_selection_mismatch_are_authenticated",
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

    isolated(
        "framing_manifests_refuse_unsupported_transfer_encoding_with_authentic_payload",
        || {
            let bytes = manifest();
            for headers in [
                "Transfer-Encoding: chunked\r\n",
                "Transfer-Encoding: \tChUnKeD \t\r\n",
            ] {
                let server = Server::new(vec![response(200, headers, &chunked(&bytes))]);
                assert_eq!(
                    server
                        .transport()
                        .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                        .unwrap()
                        .bytes(),
                    bytes
                );
            }
            let mut accepted = Vec::new();
            for (name, headers, body) in unsupported_framings(&bytes) {
                let server = Server::new(vec![response(200, &headers, &body)]);
                if server
                    .transport()
                    .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                    .is_ok()
                {
                    accepted.push(name);
                }
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            }
            assert!(
                accepted.is_empty(),
                "unsupported framing accepted authentic manifest: {accepted:?}"
            );
        },
    );
}

use std::{
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};
pub(in crate::setup) struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub(in crate::setup) fn new(responses: Vec<Vec<u8>>) -> Self {
        Self::on_request(responses, || {})
    }
    pub(in crate::setup) fn on_request(
        responses: Vec<Vec<u8>>,
        mut on_request: impl FnMut() + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/releases/manifest.json",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = thread::spawn(move || {
            let mut responses = responses.into_iter();
            while !done.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // Darwin can inherit O_NONBLOCK from the listener. A
                        // temporary WouldBlock is not an empty HTTP request.
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut byte = [0];
                        while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                            if socket.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            request.push(byte[0]);
                        }
                        seen.lock()
                            .unwrap()
                            .push(String::from_utf8(request).unwrap());
                        on_request();
                        let response = responses.next().unwrap_or_else(|| response(500, "", b""));
                        let _ = socket.write_all(&response);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    pub(in crate::setup) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
    pub(in crate::setup) fn transport(&self) -> InternalTransport {
        InternalTransport::client(Url::parse(&self.url).unwrap(), Duration::from_secs(2)).unwrap()
    }
    pub(in crate::setup) fn release(&self, bytes: &[u8]) -> InternalRelease {
        InternalRelease::transport_fixture(bytes, &self.url)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
pub(in crate::setup) fn response(status: u16, headers: &str, bytes: &[u8]) -> Vec<u8> {
    let mut result =
        format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    result.extend_from_slice(bytes);
    result
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
/// Full component fetch and prefix resume.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn component_fetch_resumes_only_checkpointed_rehashed_prefixes() {
    isolated(
        "full_component_uses_selected_sibling_and_verifies_held_file",
        || {
            let server = Server::new(vec![response(
                200,
                "Content-Length: 4096\r\n",
                &vec![b'E'; 4096],
            )]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            let mut events = Vec::new();
            let done = server
                .transport()
                .transfer(
                    &selected,
                    0,
                    &mut file,
                    None,
                    &AtomicBool::new(false),
                    42,
                    |p| events.push(p),
                )
                .unwrap();
            assert_eq!(done.byte_size(), 4096);
            assert_eq!(contents(&mut file), vec![b'E'; 4096]);
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(
                requests[0].starts_with("GET /releases/staging-esp.img HTTP/1.1"),
                "request: {:?}",
                requests[0]
            );
            assert!(requests[0].contains("accept-encoding: identity"));
            assert!(events.iter().all(|p| p.operation == 42 && p.component == 0));
            assert!(
                events
                    .iter()
                    .filter(|p| p.phase != Phase::Complete)
                    .all(|p| p.verified == 0)
            );
            assert_eq!(events.last().unwrap().verified, 4096);
        },
    );

    isolated(
        "closed_reopened_prefix_requires_checkpoint_and_resumes_with_range",
        || {
            let server = Server::new(vec![response(
                206,
                "Content-Range: bytes 1024-4095/4096\r\nContent-Length: 3072\r\n",
                &vec![b'E'; 3072],
            )]);
            let selected = context(&server);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("working");
            let mut file = File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            file.write_all(&vec![b'E'; 1024]).unwrap();
            let checkpoint =
                capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            drop(file);
            let mut file = File::options().read(true).write(true).open(&path).unwrap();
            assert!(
                server
                    .transport()
                    .transfer(
                        &selected,
                        0,
                        &mut file,
                        None,
                        &AtomicBool::new(false),
                        1,
                        |_| {}
                    )
                    .is_err()
            );
            assert!(server.requests.lock().unwrap().is_empty());
            server
                .transport()
                .transfer(
                    &selected,
                    0,
                    &mut file,
                    Some(&checkpoint),
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                )
                .unwrap();
            assert_eq!(contents(&mut file), vec![b'E'; 4096]);
            assert!(server.requests.lock().unwrap()[0].contains("range: bytes=1024-"));
        },
    );

    isolated(
        "interrupted_and_partial_ranges_resume_only_bounded_rehashed_prefixes",
        || {
            let server = Server::new(vec![
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(
                    206,
                    "Content-Range: bytes 1024-2047/4096\r\n",
                    &vec![b'E'; 1024],
                ),
                response(
                    206,
                    "Content-Range: bytes 2048-4095/4096\r\n",
                    &vec![b'E'; 2048],
                ),
            ]);
            let mut file = tempfile::tempfile().unwrap();
            server
                .transport()
                .transfer(
                    &context(&server),
                    0,
                    &mut file,
                    None,
                    &AtomicBool::new(false),
                    7,
                    |_| {},
                )
                .unwrap();
            assert_eq!(contents(&mut file), vec![b'E'; 4096]);
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert!(requests[1].contains("range: bytes=1024-"));
            assert!(requests[2].contains("range: bytes=2048-"));
        },
    );

    isolated(
        "within_operation_retry_rehashes_its_captured_prefix",
        || {
            let server = Server::new(vec![
                response(200, "Content-Length: 4096\r\n", &vec![b'E'; 1024]),
                response(
                    206,
                    "Content-Range: bytes 1024-4095/4096\r\n",
                    &vec![b'E'; 3072],
                ),
            ]);
            let mut file = tempfile::tempfile().unwrap();
            let mut mutator = file.try_clone().unwrap();
            let result = server.transport().transfer(
                &context(&server),
                0,
                &mut file,
                None,
                &AtomicBool::new(false),
                1,
                |p| {
                    if p.phase == Phase::Retrying {
                        mutator.rewind().unwrap();
                        mutator.write_all(b"X").unwrap();
                    }
                },
            );
            assert!(result.unwrap_err().contains("prefix digest"));
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        },
    );
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

/// Checkpoint trust, range restarts and whole-component digest.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn untrusted_checkpoints_and_bad_ranges_restart_or_refuse_safely() {
    isolated(
        "untrusted_checkpoint_fields_and_changed_prefix_refuse_without_request_or_mutation",
        || {
            let server = Server::new(vec![]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'E'; 1024]).unwrap();
            let checkpoint =
                capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            for field in 0..8 {
                let mut bad = checkpoint.clone();
                match field {
                    0 => bad.manifest_sha256 = "a".repeat(64),
                    1 => bad.role = "staging-root".into(),
                    2 => bad.filename = "other.img".into(),
                    3 => bad.full_size = 8192,
                    4 => bad.full_sha256 = "a".repeat(64),
                    5 => bad.prefix_size = 1023,
                    6 => bad.prefix_sha256 = "a".repeat(64),
                    _ => bad.prefix_size = u64::MAX,
                }
                assert!(
                    server
                        .transport()
                        .transfer(
                            &selected,
                            0,
                            &mut file,
                            Some(&bad),
                            &AtomicBool::new(false),
                            1,
                            |_| {}
                        )
                        .is_err()
                );
                assert_eq!(contents(&mut file), vec![b'E'; 1024]);
            }
            file.rewind().unwrap();
            file.write_all(b"X").unwrap();
            let before = contents(&mut file);
            assert!(
                server
                    .transport()
                    .transfer(
                        &selected,
                        0,
                        &mut file,
                        Some(&checkpoint),
                        &AtomicBool::new(false),
                        1,
                        |_| {}
                    )
                    .is_err()
            );
            assert_eq!(contents(&mut file), before);
            assert!(server.requests.lock().unwrap().is_empty());
        },
    );

    isolated(
        "ignored_rejected_malformed_and_oversized_ranges_restart_held_handle",
        || {
            for raw in [
                response(200, "", b"ignored"),
                response(416, "", b""),
                response(206, "Content-Range: bytes 0-4095/4096\r\n", b""),
                response(206, "Content-Range: bytes 1024-4096/4096\r\n", b""),
                response(206, "Content-Range: bytes 1024-4095/4097\r\n", b""),
                response(
                    206,
                    "Content-Range: bytes 1024-4095/4096\r\nContent-Length: 1\r\n",
                    b"x",
                ),
                response(
                    206,
                    "Content-Range: bytes 1024-4095/4096\r\nContent-Range: bytes 1024-4095/4096\r\n",
                    b"",
                ),
            ] {
                let server = Server::new(vec![raw, response(200, "", &vec![b'E'; 4096])]);
                let selected = context(&server);
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&vec![b'E'; 1024]).unwrap();
                let cp =
                    capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
                let mut events = Vec::new();
                server
                    .transport()
                    .transfer(
                        &selected,
                        0,
                        &mut file,
                        Some(&cp),
                        &AtomicBool::new(false),
                        9,
                        |p| events.push(p),
                    )
                    .unwrap();
                assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                assert!(
                    events
                        .iter()
                        .any(|p| p.phase == Phase::Resetting && p.received == 0 && p.verified == 0)
                );
                let requests = server.requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                assert!(!requests[1].contains("range:"));
            }
        },
    );

    isolated(
        "complete_checkpoint_and_corrupt_resume_require_whole_component_digest",
        || {
            for bytes in [vec![b'E'; 4096], vec![b'X'; 4096]] {
                let server = Server::new(vec![]);
                let selected = context(&server);
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&bytes).unwrap();
                let cp =
                    capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
                let result = server.transport().transfer(
                    &selected,
                    0,
                    &mut file,
                    Some(&cp),
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                );
                assert_eq!(result.is_ok(), bytes[0] == b'E');
                assert!(server.requests.lock().unwrap().is_empty());
            }
            let server = Server::new(vec![response(
                206,
                "Content-Range: bytes 1024-4095/4096\r\n",
                &vec![b'E'; 3072],
            )]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'X'; 1024]).unwrap();
            let cp = capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            assert!(
                server
                    .transport()
                    .transfer(
                        &selected,
                        0,
                        &mut file,
                        Some(&cp),
                        &AtomicBool::new(false),
                        1,
                        |_| {}
                    )
                    .unwrap_err()
                    .contains("SHA-256")
            );
        },
    );
}
/// Transient retries, encoding/length/digest errors and EOF errors.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn network_errors_and_short_bodies_are_never_completion() {
    isolated(
        "transient_http_retries_are_bounded_and_preserve_prefix",
        || {
            let server = Server::new(vec![
                response(503, "", b""),
                response(
                    206,
                    "Content-Range: bytes 1024-4095/4096\r\n",
                    &vec![b'E'; 3072],
                ),
            ]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'E'; 1024]).unwrap();
            let cp = capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            server
                .transport()
                .transfer(
                    &selected,
                    0,
                    &mut file,
                    Some(&cp),
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                )
                .unwrap();
            assert!(
                server
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|r| r.contains("range: bytes=1024-"))
            );
            let server = Server::new(vec![response(503, "", b""); 4]);
            let mut file = tempfile::tempfile().unwrap();
            let error = server
                .transport()
                .transfer(
                    &context(&server),
                    0,
                    &mut file,
                    None,
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                )
                .unwrap_err();
            assert!(error.contains("3 attempts"));
            assert_eq!(server.requests.lock().unwrap().len(), 3);
        },
    );

    isolated(
        "component_encoding_length_excess_truncation_and_digest_are_not_completion",
        || {
            for raw in [
                response(200, "Content-Encoding: gzip\r\n", &vec![b'E'; 4096]),
                response(
                    200,
                    "Content-Encoding: identity\r\nContent-Encoding: identity\r\n",
                    &vec![b'E'; 4096],
                ),
                response(200, "Content-Length: 4097\r\n", &vec![b'E'; 4096]),
                response(200, "", &vec![b'E'; 4097]),
                response(200, "", &vec![b'E'; 4095]),
                response(200, "", &vec![b'X'; 4096]),
                response(
                    206,
                    "Content-Range: bytes 0-4095/4096\r\n",
                    &vec![b'E'; 4096],
                ),
            ] {
                let server = Server::new(vec![raw]);
                let mut file = tempfile::tempfile().unwrap();
                let mut events = Vec::new();
                assert!(
                    server
                        .transport()
                        .transfer(
                            &context(&server),
                            0,
                            &mut file,
                            None,
                            &AtomicBool::new(false),
                            1,
                            |p| events.push(p)
                        )
                        .is_err()
                );
                assert!(file.metadata().unwrap().len() <= 4096);
                assert_eq!(events.last().unwrap().phase, Phase::Failed);
                assert!(events.iter().all(|p| p.verified == 0));
            }
        },
    );

    isolated(
        "expected_network_bytes_followed_by_eof_error_require_new_complete_response",
        || {
            let malformed = format!("1000\r\n{}\r\n", "E".repeat(4096));
            let raw = response(200, "Transfer-Encoding: chunked\r\n", malformed.as_bytes());
            let server = Server::new(vec![raw.clone(), raw.clone(), raw]);
            let mut file = tempfile::tempfile().unwrap();
            assert!(
                server
                    .transport()
                    .transfer(
                        &context(&server),
                        0,
                        &mut file,
                        None,
                        &AtomicBool::new(false),
                        1,
                        |_| {}
                    )
                    .is_err()
            );
            assert_eq!(server.requests.lock().unwrap().len(), 3);
            assert_eq!(file.metadata().unwrap().len(), 0);
            let complete = format!("1000\r\n{}\r\n0\r\n\r\n", "E".repeat(4096));
            let server = Server::new(vec![response(
                200,
                "Transfer-Encoding: chunked\r\n",
                complete.as_bytes(),
            )]);
            server
                .transport()
                .transfer(
                    &context(&server),
                    0,
                    &mut file,
                    None,
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                )
                .unwrap();
        },
    );
}
/// Cancellation before requests, during hashing and at completion.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn download_cancellation_never_returns_success_at_any_boundary() {
    isolated(
        "output_type_invalid_index_and_cancellation_refuse_before_request",
        || {
            let server = Server::new(vec![]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            assert!(
                server
                    .transport()
                    .transfer(
                        &selected,
                        2,
                        &mut file,
                        None,
                        &AtomicBool::new(false),
                        1,
                        |_| {}
                    )
                    .is_err()
            );
            assert!(
                server
                    .transport()
                    .transfer(
                        &selected,
                        0,
                        &mut file,
                        None,
                        &AtomicBool::new(true),
                        1,
                        |_| {}
                    )
                    .is_err()
            );
            assert!(
                server
                    .transport()
                    .fetch_manifest(&server.release(&manifest()), &AtomicBool::new(true))
                    .is_err()
            );
            #[cfg(unix)]
            {
                let dir = tempfile::tempdir().unwrap();
                let mut file = File::open(dir.path()).unwrap();
                assert!(
                    server
                        .transport()
                        .transfer(
                            &selected,
                            0,
                            &mut file,
                            None,
                            &AtomicBool::new(false),
                            1,
                            |_| {}
                        )
                        .is_err()
                );
                assert!(
                    capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).is_err()
                );
            }
            assert!(server.requests.lock().unwrap().is_empty());
        },
    );

    isolated(
        "cancellation_at_network_final_hash_and_completion_never_returns_success",
        || {
            for phase in [Phase::Receiving, Phase::Verifying, Phase::Complete] {
                let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                let mut file = tempfile::tempfile().unwrap();
                let cancel = AtomicBool::new(false);
                let mut events = Vec::new();
                assert!(
                    server
                        .transport()
                        .transfer(&context(&server), 0, &mut file, None, &cancel, 1, |p| {
                            events.push(p);
                            if p.phase == phase {
                                cancel.store(true, Ordering::Release);
                            }
                        })
                        .is_err()
                );
                assert_eq!(events.last().unwrap().phase, Phase::Cancelled);
            }
            for stage in ["hash", "hash-eof"] {
                let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                let mut file = tempfile::tempfile().unwrap();
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                with_hook(
                    Box::new(move |s, _| {
                        if s == stage {
                            signal.store(true, Ordering::Release);
                        }
                    }),
                    || {
                        assert!(
                            server
                                .transport()
                                .transfer(&context(&server), 0, &mut file, None, &cancel, 1, |_| {})
                                .is_err()
                        );
                    },
                );
            }
        },
    );

    isolated(
        "cancellation_during_prefix_and_checkpoint_hash_stops_before_requests",
        || {
            for capture in [false, true] {
                let server = Server::new(vec![]);
                let selected = context(&server);
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&vec![b'E'; 1024]).unwrap();
                let cp =
                    capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                with_hook(
                    Box::new(move |stage, _| {
                        if stage == "hash" {
                            signal.store(true, Ordering::Release);
                        }
                    }),
                    || {
                        if capture {
                            assert!(capture_checkpoint(&selected, 0, &mut file, &cancel).is_err());
                        } else {
                            assert!(
                                server
                                    .transport()
                                    .transfer(
                                        &selected,
                                        0,
                                        &mut file,
                                        Some(&cp),
                                        &cancel,
                                        1,
                                        |_| {}
                                    )
                                    .is_err()
                            );
                        }
                    },
                );
                assert!(server.requests.lock().unwrap().is_empty());
                assert_eq!(contents(&mut file), vec![b'E'; 1024]);
            }
        },
    );

    isolated(
        "final_hash_cancellation_checks_first_middle_last_chunks_and_eof",
        || {
            let payload = vec![b'R'; 131584];
            let mut value = super::super::internal_bundle::tests::fixture();
            value["components"][1]["byte_size"] = serde_json::json!(131584);
            value["components"][1]["sha256"] =
                serde_json::json!(hex::encode(Sha256::digest(&payload)));
            value["source_image_bytes"] = serde_json::json!(153088);
            let bytes = serde_json::to_vec(&value).unwrap();
            for (stage, count) in [
                ("hash", 65536),
                ("hash", 131072),
                ("hash", 131584),
                ("hash-eof", 131584),
            ] {
                let server = Server::new(vec![response(200, "", &payload)]);
                let selected = SelectedManifest::checked(
                    &server.release(&bytes),
                    bytes.clone(),
                    &Url::parse(&server.url).unwrap(),
                )
                .unwrap();
                let mut file = tempfile::tempfile().unwrap();
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                with_hook(
                    Box::new(move |s, n| {
                        if s == stage && n == count {
                            signal.store(true, Ordering::Release);
                        }
                    }),
                    || {
                        assert!(
                            server
                                .transport()
                                .transfer(&selected, 1, &mut file, None, &cancel, 1, |_| {})
                                .is_err()
                        );
                    },
                );
                assert!(cancel.load(Ordering::Acquire));
                assert_eq!(file.metadata().unwrap().len(), 131584);
            }
        },
    );
}
/// Redirect bounds, stalled reads and multi-read timeouts.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn redirects_and_stream_timeouts_are_bounded() {
    isolated(
        "redirects_are_bounded_checked_and_keep_original_sibling_base",
        || {
            let bytes = manifest();
            let server = Server::new(vec![
                response(302, "Location: /elsewhere/manifest.json\r\n", b""),
                response(200, "", &bytes),
                response(200, "", &vec![b'E'; 4096]),
            ]);
            let transport = server.transport();
            let selected = transport
                .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                .unwrap();
            let mut file = tempfile::tempfile().unwrap();
            transport
                .transfer(
                    &selected,
                    0,
                    &mut file,
                    None,
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                )
                .unwrap();
            assert!(
                server.requests.lock().unwrap()[2].starts_with("GET /releases/staging-esp.img")
            );
            for location in [
                "http://127.0.0.1:1/escape",
                "https://evil.test/escape",
                "/escape?x=1",
                "/escape#x",
                "http://user@127.0.0.1/escape",
            ] {
                let server = Server::new(vec![response(
                    302,
                    &format!("Location: {location}\r\n"),
                    b"",
                )]);
                assert!(
                    server
                        .transport()
                        .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                        .is_err()
                );
                assert_eq!(
                    server.requests.lock().unwrap().len(),
                    1,
                    "location {location}: {:?}",
                    server.requests
                );
            }
            for hops in [5, 6] {
                let mut responses = vec![response(302, "Location: /again\r\n", b""); hops];
                responses.push(response(200, "", &bytes));
                let server = Server::new(responses);
                assert_eq!(
                    server
                        .transport()
                        .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                        .is_ok(),
                    hops == 5
                );
                assert_eq!(server.requests.lock().unwrap().len(), 6);
            }
        },
    );

    isolated("stalled_network_read_obeys_injected_timeout", || {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/manifest.json", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n")
                .unwrap();
            thread::sleep(Duration::from_millis(400));
        });
        let bytes = manifest();
        let selected = SelectedManifest::checked(
            &InternalRelease::transport_fixture(&bytes, &url),
            bytes,
            &Url::parse(&url).unwrap(),
        )
        .unwrap();
        let transport =
            InternalTransport::client(Url::parse(&url).unwrap(), Duration::from_millis(40))
                .unwrap();
        let mut file = tempfile::tempfile().unwrap();
        let start = ui::time::now();
        assert!(
            transport
                .transfer(
                    &selected,
                    0,
                    &mut file,
                    None,
                    &AtomicBool::new(false),
                    1,
                    |_| {}
                )
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        worker.join().unwrap();
    });

    isolated(
        "multiple_stream_reads_can_outlast_one_timeout_and_cancel_after_received_bytes",
        || {
            for should_cancel in [false, true] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let url = format!("http://{}/manifest.json", listener.local_addr().unwrap());
                let worker = thread::spawn(move || {
                    let (mut socket, _) = listener.accept().unwrap();
                    let mut request = [0; 4096];
                    socket.read(&mut request).unwrap();
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n")
                        .unwrap();
                    for i in 0..4 {
                        if i > 0 {
                            thread::sleep(Duration::from_millis(150));
                        }
                        if socket.write_all(&vec![b'E'; 1024]).is_err() {
                            break;
                        }
                    }
                });
                let bytes = manifest();
                let selected = SelectedManifest::checked(
                    &InternalRelease::transport_fixture(&bytes, &url),
                    bytes,
                    &Url::parse(&url).unwrap(),
                )
                .unwrap();
                let transport = InternalTransport::client(
                    Url::parse(&url).unwrap(),
                    Duration::from_millis(300),
                )
                .unwrap();
                let mut file = tempfile::tempfile().unwrap();
                let cancel = AtomicBool::new(false);
                let start = ui::time::now();
                let mut events = Vec::new();
                let result = transport.transfer(&selected, 0, &mut file, None, &cancel, 5, |p| {
                    events.push(p);
                    if should_cancel && p.phase == Phase::Receiving && p.received > 0 {
                        cancel.store(true, Ordering::Release);
                    }
                });
                assert_eq!(result.is_ok(), !should_cancel);
                if should_cancel {
                    assert!(file.metadata().unwrap().len() < 4096);
                    assert_eq!(events.last().unwrap().phase, Phase::Cancelled);
                } else {
                    assert!(start.elapsed() >= Duration::from_millis(400));
                    assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                }
                assert!(
                    events
                        .iter()
                        .any(|p| p.phase == Phase::Receiving && p.received > 0 && p.verified == 0)
                );
                worker.join().unwrap();
            }
        },
    );
}
/// Malformed framing and HTTP/1.0 transfer encoding.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn http_framing_errors_fail_before_retry_redirect_or_prefix_change() {
    isolated(
        "malformed_http_framing_fails_without_retrying_an_invalid_response",
        || {
            for headers in [
                "Content-Length: invalid\r\n",
                "Content-Length: 4096\r\nContent-Length: 4097\r\n",
            ] {
                let server = Server::new(vec![response(200, headers, b""); 3]);
                let mut file = tempfile::tempfile().unwrap();
                assert!(
                    server
                        .transport()
                        .transfer(
                            &context(&server),
                            0,
                            &mut file,
                            None,
                            &AtomicBool::new(false),
                            1,
                            |_| {}
                        )
                        .is_err()
                );
                assert_eq!(server.requests.lock().unwrap().len(), 1);
                assert_eq!(file.metadata().unwrap().len(), 0);
            }
        },
    );

    isolated(
        "framing_resumed_components_refuse_before_modifying_held_prefix",
        || {
            let mut accepted = Vec::new();
            for (name, headers, body) in unsupported_framings(&vec![b'E'; 3072]) {
                let server = Server::new(vec![response(
                    206,
                    &format!("{headers}Content-Range: bytes 1024-4095/4096\r\n"),
                    &body,
                )]);
                let selected = context(&server);
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&vec![b'E'; 1024]).unwrap();
                let cp =
                    capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
                let result = server.transport().transfer(
                    &selected,
                    0,
                    &mut file,
                    Some(&cp),
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                );
                if result.is_ok() || contents(&mut file) != vec![b'E'; 1024] {
                    accepted.push(name);
                }
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            }
            assert!(
                accepted.is_empty(),
                "unsupported framing accepted/mutated resumed component: {accepted:?}"
            );
        },
    );

    isolated(
        "framing_refuses_before_redirect_or_ignored_range_reset",
        || {
            let bytes = manifest();
            let mut violations = Vec::new();
            for (name, headers, _) in unsupported_framings(&[]) {
                let server = Server::new(vec![
                    response(302, &format!("{headers}Location: /next\r\n"), b""),
                    response(200, "", &bytes),
                ]);
                let result = server
                    .transport()
                    .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false));
                if result.is_ok() || server.requests.lock().unwrap().len() != 1 {
                    violations.push(format!("redirect {name}"));
                }
                let server = Server::new(vec![
                    response(200, &headers, b""),
                    response(200, "", &vec![b'E'; 4096]),
                ]);
                let selected = context(&server);
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(&vec![b'E'; 1024]).unwrap();
                let cp =
                    capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
                let result = server.transport().transfer(
                    &selected,
                    0,
                    &mut file,
                    Some(&cp),
                    &AtomicBool::new(false),
                    1,
                    |_| {},
                );
                if result.is_ok()
                    || server.requests.lock().unwrap().len() != 1
                    || contents(&mut file) != vec![b'E'; 1024]
                {
                    violations.push(format!("reset {name}"));
                }
            }
            assert!(
                violations.is_empty(),
                "unsupported framing was acted on: {violations:?}"
            );
        },
    );

    isolated("framing_http_10_requires_absent_transfer_encoding", || {
        let bytes = manifest();
        for (headers, body, accepted) in [
            ("", bytes.clone(), true),
            ("Transfer-Encoding: chunked\r\n", chunked(&bytes), false),
        ] {
            let raw = response(200, headers, &body);
            let raw = [b"HTTP/1.0".as_slice(), &raw[8..]].concat();
            let server = Server::new(vec![raw]);
            assert_eq!(
                server
                    .transport()
                    .fetch_manifest(&server.release(&bytes), &AtomicBool::new(false))
                    .is_ok(),
                accepted
            );
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        }
    });
}

fn chunked(bytes: &[u8]) -> Vec<u8> {
    let mut wire = format!("{:x}\r\n", bytes.len()).into_bytes();
    wire.extend_from_slice(bytes);
    wire.extend_from_slice(b"\r\n0\r\n\r\n");
    wire
}
fn unsupported_framings(bytes: &[u8]) -> Vec<(&'static str, String, Vec<u8>)> {
    vec![
        (
            "unknown",
            "Transfer-Encoding: yolo\r\n".into(),
            bytes.to_vec(),
        ),
        (
            "TE plus CL",
            format!(
                "Transfer-Encoding: chunked\r\nContent-Length: {}\r\n",
                bytes.len()
            ),
            chunked(bytes),
        ),
        (
            "repeated coding",
            "Transfer-Encoding: chunked, chunked\r\n".into(),
            chunked(bytes),
        ),
        (
            "additional coding",
            "Transfer-Encoding: gzip, chunked\r\n".into(),
            chunked(bytes),
        ),
        (
            "duplicate fields",
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n".into(),
            chunked(bytes),
        ),
    ]
}
thread_local! { static WRITE_REMAINING: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
pub(super) struct FaultWriter<'a>(pub(super) &'a mut File);
impl Write for FaultWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(result) = scripted_write(self.0, bytes) {
            return result;
        }
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
/// Terminal byte counters across cancellation, reset and errors.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn terminal_counter_reflects_actual_file_writes() {
    isolated(
        "terminal_counter_includes_successful_write_before_cancellation",
        || {
            let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
            let mut file = tempfile::tempfile().unwrap();
            let mut events = Vec::new();
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            with_hook(
                Box::new(move |stage, _| {
                    if stage == "write" {
                        signal.store(true, Ordering::Release);
                    }
                }),
                || {
                    assert!(
                        server
                            .transport()
                            .transfer(&context(&server), 0, &mut file, None, &cancel, 41, |p| {
                                events.push(p)
                            })
                            .is_err()
                    );
                },
            );
            let bytes = contents(&mut file);
            assert!(!bytes.is_empty());
            assert!(bytes.iter().all(|b| *b == b'E'));
            let terminal = events.last().unwrap();
            assert_eq!(terminal.phase, Phase::Cancelled);
            assert_eq!(terminal.verified, 0);
            assert_eq!(terminal.received, bytes.len() as u64);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        },
    );

    isolated(
        "terminal_counter_clears_successful_reset_before_cancellation",
        || {
            let server = Server::new(vec![response(416, "", b"")]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'E'; 1024]).unwrap();
            let cp = capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            let mut events = Vec::new();
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = cancel.clone();
            with_hook(
                Box::new(move |stage, _| {
                    if stage == "reset" {
                        signal.store(true, Ordering::Release);
                    }
                }),
                || {
                    assert!(
                        server
                            .transport()
                            .transfer(&selected, 0, &mut file, Some(&cp), &cancel, 42, |p| events
                                .push(p))
                            .is_err()
                    );
                },
            );
            assert!(contents(&mut file).is_empty());
            let terminal = events.last().unwrap();
            assert_eq!(terminal.phase, Phase::Cancelled);
            assert_eq!(terminal.verified, 0);
            assert_eq!(terminal.received, 0);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        },
    );

    isolated(
        "terminal_counter_includes_partial_real_file_write_before_error",
        || {
            let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
            let mut file = tempfile::tempfile().unwrap();
            let mut events = Vec::new();
            with_write_failure(1024, || {
                let error = server
                    .transport()
                    .transfer(
                        &context(&server),
                        0,
                        &mut file,
                        None,
                        &AtomicBool::new(false),
                        43,
                        |p| events.push(p),
                    )
                    .unwrap_err();
                assert!(error.contains("injected after real partial write"));
            });
            assert_eq!(contents(&mut file), vec![b'E'; 1024]);
            let terminal = events.last().unwrap();
            assert_eq!(terminal.phase, Phase::Failed);
            assert_eq!(terminal.verified, 0);
            assert_eq!(terminal.received, 1024);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        },
    );
}
#[derive(Clone, Copy)]
enum WriteStep {
    Interrupted,
    Write(usize),
}
struct WriteScript {
    steps: std::collections::VecDeque<WriteStep>,
    always_interrupted: bool,
    cancel_on_interrupt: Option<Arc<AtomicBool>>,
    attempts: usize,
    interrupted: usize,
}
thread_local! { static WRITE_SCRIPT: std::cell::RefCell<Option<WriteScript>> = const { std::cell::RefCell::new(None) }; }
fn scripted_write(file: &mut File, bytes: &[u8]) -> Option<std::io::Result<usize>> {
    WRITE_SCRIPT.with(|slot| {
        let mut slot = slot.borrow_mut();
        let script = slot.as_mut()?;
        script.attempts += 1;
        assert!(
            script.attempts <= 64,
            "interrupted writer exceeded test safety bound"
        );
        let step = script
            .steps
            .pop_front()
            .or(script.always_interrupted.then_some(WriteStep::Interrupted));
        Some(match step {
            Some(WriteStep::Interrupted) => {
                script.interrupted += 1;
                if let Some(cancel) = &script.cancel_on_interrupt {
                    cancel.store(true, Ordering::Release);
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "injected interrupted write",
                ))
            }
            Some(WriteStep::Write(limit)) => file.write(&bytes[..bytes.len().min(limit)]),
            None => file.write(bytes),
        })
    })
}
fn with_write_script(
    steps: Vec<WriteStep>,
    always_interrupted: bool,
    cancel: Option<Arc<AtomicBool>>,
    test: impl FnOnce(),
) -> (usize, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            WRITE_SCRIPT.with(|s| *s.borrow_mut() = None);
        }
    }
    WRITE_SCRIPT.with(|s| {
        *s.borrow_mut() = Some(WriteScript {
            steps: steps.into(),
            always_interrupted,
            cancel_on_interrupt: cancel,
            attempts: 0,
            interrupted: 0,
        })
    });
    let _reset = Reset;
    test();
    WRITE_SCRIPT.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().unwrap();
        (s.attempts, s.interrupted)
    })
}
/// Interrupted write recovery, cancellation and exhaustion.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn interrupted_writes_recover_within_a_cumulative_bound() {
    isolated(
        "interrupted_write_recovers_then_authenticates_exact_real_file_bytes",
        || {
            for interruptions in [1, 7] {
                let server = Server::new(vec![response(200, "", &vec![b'E'; 4096])]);
                let mut file = tempfile::tempfile().unwrap();
                let mut events = Vec::new();
                let (attempts, observed) = with_write_script(
                    vec![WriteStep::Interrupted; interruptions],
                    false,
                    None,
                    || {
                        let result = server
                            .transport()
                            .transfer(
                                &context(&server),
                                0,
                                &mut file,
                                None,
                                &AtomicBool::new(false),
                                51,
                                |p| events.push(p),
                            )
                            .unwrap();
                        assert_eq!(result.byte_size(), 4096);
                    },
                );
                assert_eq!(contents(&mut file), vec![b'E'; 4096]);
                assert_eq!(observed, interruptions);
                assert!(attempts > interruptions);
                let terminal = events.last().unwrap();
                assert_eq!(terminal.phase, Phase::Complete);
                assert_eq!(terminal.received, 4096);
                assert_eq!(terminal.verified, 4096);
                assert_eq!(server.requests.lock().unwrap().len(), 1);
            }
        },
    );

    isolated(
        "interrupted_write_cancellation_prevents_retry_and_preserves_prefix",
        || {
            let server = Server::new(vec![response(
                206,
                "Content-Range: bytes 1024-4095/4096\r\n",
                &vec![b'E'; 3072],
            )]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'E'; 1024]).unwrap();
            let cp = capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let mut events = Vec::new();
            let counts = with_write_script(
                vec![WriteStep::Interrupted],
                false,
                Some(cancel.clone()),
                || {
                    let error = server
                        .transport()
                        .transfer(&selected, 0, &mut file, Some(&cp), &cancel, 52, |p| {
                            events.push(p)
                        })
                        .unwrap_err();
                    assert!(error.contains("cancelled"), "{error}");
                },
            );
            assert_eq!(counts, (1, 1));
            assert_eq!(contents(&mut file), vec![b'E'; 1024]);
            let terminal = events.last().unwrap();
            assert_eq!(terminal.phase, Phase::Cancelled);
            assert_eq!(terminal.received, 1024);
            assert_eq!(terminal.verified, 0);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        },
    );

    isolated(
        "interrupted_write_exhaustion_stops_at_eight_and_preserves_prefix",
        || {
            let server = Server::new(vec![response(
                206,
                "Content-Range: bytes 1024-4095/4096\r\n",
                &vec![b'E'; 3072],
            )]);
            let selected = context(&server);
            let mut file = tempfile::tempfile().unwrap();
            file.write_all(&vec![b'E'; 1024]).unwrap();
            let cp = capture_checkpoint(&selected, 0, &mut file, &AtomicBool::new(false)).unwrap();
            let mut events = Vec::new();
            let counts = with_write_script(vec![], true, None, || {
                assert!(
                    server
                        .transport()
                        .transfer(
                            &selected,
                            0,
                            &mut file,
                            Some(&cp),
                            &AtomicBool::new(false),
                            53,
                            |p| events.push(p)
                        )
                        .is_err()
                );
            });
            assert_eq!(counts, (8, 8));
            assert_eq!(contents(&mut file), vec![b'E'; 1024]);
            let terminal = events.last().unwrap();
            assert_eq!(terminal.phase, Phase::Failed);
            assert_eq!(terminal.received, 1024);
            assert_eq!(terminal.verified, 0);
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        },
    );

    isolated(
        "interrupted_write_allowance_is_cumulative_across_successful_partial_writes",
        || {
            // Directly exercise one production chunk to avoid depending on TCP read boundaries.
            let mut steps = Vec::new();
            for _ in 0..7 {
                steps.extend([WriteStep::Interrupted, WriteStep::Write(1)]);
            }
            steps.push(WriteStep::Interrupted);
            let mut file = tempfile::tempfile().unwrap();
            let mut events = Events {
                callback: |_| {},
                operation: 54,
                component: 0,
                last: None,
                received: 0,
            };
            let counts = with_write_script(steps, false, None, || {
                assert!(
                    write_chunk(&mut file, &[b'E'; 32], &AtomicBool::new(false), &mut events)
                        .is_err()
                );
            });
            assert_eq!(counts, (15, 8));
            assert_eq!(contents(&mut file), vec![b'E'; 7]);
            assert_eq!(events.received, 7);
        },
    );
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
/// Persistent success, pause tail, failed resets, cancel and retry.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn persistent_download_commits_only_verified_tails() {
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
            let final_reads = Arc::new(Mutex::new(0));
            let count = final_reads.clone();
            with_hook(
                Box::new(move |stage, n| {
                    if stage == "hash-eof" {
                        *count.lock().unwrap() += n;
                    }
                }),
                || {
                    let mut session = Session::new(&selected, 1, 41, p).unwrap();
                    let prepared = session.prepare(&selected, &mut f, &cancel).unwrap();
                    assert!(matches!(
                        server
                            .transport()
                            .transfer_persistent(prepared, &cancel, &pause, |_| {})
                            .unwrap(),
                        PersistentOutcome::Complete(_)
                    ));
                },
            );
            CHECKPOINT_INTERVAL.set(None);
            assert_eq!(contents(&mut f), payload);
            assert_eq!(read_totals("periodic"), (524288, 524288));
            assert_eq!(*final_reads.lock().unwrap(), 524288);
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
/// Stale acks, failed EOF reset, pause checkpoints and reopen.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn persistent_pause_and_stale_acks_preserve_one_validated_prefix() {
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
            let final_reads = Arc::new(Mutex::new(0));
            let count = final_reads.clone();
            with_hook(
                Box::new(move |stage, n| {
                    if stage == "hash-eof" {
                        *count.lock().unwrap() += n
                    }
                }),
                || {
                    let outcome = server
                        .transport()
                        .transfer_persistent(prepared, &cancel, &AtomicBool::new(false), |_| {})
                        .unwrap();
                    match outcome {
                        PersistentOutcome::Complete(done) => assert_eq!(done.byte_size(), 4096),
                        other => panic!("{other:?}"),
                    }
                },
            );
            assert_eq!(contents(&mut f), vec![b'E'; 4096]);
            assert_eq!(read_totals("restart"), (1024, 1024));
            assert_eq!(read_totals("periodic"), (3072, 3072));
            assert_eq!(read_totals("retry"), (0, 0));
            assert_eq!(*final_reads.lock().unwrap(), 4096);
            assert_eq!(read_totals("full-file"), (4098, 4096));
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(!requests[0].contains("range:"));
            assert!(requests[1].contains("range: bytes=1024-"));
            println!(
                "restart bytes requested/actual=1024/1024; new-tail readback=3072/3072; final full requested/actual=4098/4096 (EOF sentinel included); resumed HTTP body=3072; saved network bytes=1024"
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
pub(super) struct HashReader<'a>(pub &'a mut File);
impl Read for HashReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(bytes)?;
        super::super::internal_cache::tests::read_event("full-file", bytes.len() as u64, n as u64);
        Ok(n)
    }
}

/// Exact reader seam: the event is inside Read::read, after the caller's
/// pre-read stop check. Tests block this call with channels, never timing sleeps.
pub(super) struct ResponseReader<'a> {
    pub response: &'a mut reqwest::blocking::Response,
    pub received: u64,
}
impl Read for ResponseReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        event("body-read-enter", self.received);
        let n = self.response.read(bytes)?;
        self.received += n as u64;
        Ok(n)
    }
}
/// Returned request boundaries.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn returned_request_boundaries_observe_pause_cancel_and_reset() {
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
                        if stage == "body-read-enter" && n == 4096 {
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
/// Pause during final verification, final ack, failed publication and last retry.
#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn pause_near_final_publication_reports_the_actual_committed_state() {
    isolated(
        "pause_during_final_verification_or_complete_callback_retains_prior_prefix",
        || {
            for stage in ["Verifying", "hash", "hash-eof", "Complete"] {
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
/// Scoped worker-thread interval override. The cell itself remains private.
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
