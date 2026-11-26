use super::*;
use crate::setup::test_support::isolated;
use test_http::Responder;
fn manifest() -> Vec<u8> {
    super::super::internal_bundle::tests::bytes(&super::super::internal_bundle::tests::fixture())
}

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
    io::Write,
    sync::{Arc, Mutex},
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
type Hook = Box<dyn FnMut(&str, u64)>;
thread_local! { static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) }; }
pub(super) fn event(stage: &str, count: u64) {
    HOOK.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            h(stage, count);
        }
    });
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
thread_local! { static CHECKPOINT_INTERVAL: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) }; }
pub(super) fn checkpoint_interval_override() -> Option<u64> {
    CHECKPOINT_INTERVAL.get()
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
