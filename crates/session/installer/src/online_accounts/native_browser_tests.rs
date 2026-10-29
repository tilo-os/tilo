//! Explicit VM qualification, never a host-desktop test. Run this test binary
//! under a private headless Cage in an ordinary user's read-only development
//! slot containing the matching signed Browser, SDK and CEF packages.

use super::*;
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tilo_oauth::fake::FakeAuthorizationServer;

struct Page {
    port: u16,
    painted: Arc<AtomicBool>,
    callback_port: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Page {
    fn start(provider: String, proceed: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let painted = Arc::new(AtomicBool::new(false));
        let callback_port = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let signals = (painted.clone(), callback_port.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !signals.2.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        serve(&mut stream, &provider, proceed, &signals.0, &signals.1);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(10));
                    }
                    Err(error) => panic!("private provider listener failed: {error}"),
                }
            }
        });
        Self {
            port,
            painted,
            callback_port,
            stop,
            thread: Some(thread),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/authorize", self.port)
    }

    fn assert_callback_closed(&self) {
        let port = u16::try_from(self.callback_port.load(Ordering::Acquire)).unwrap();
        assert_ne!(port, 0, "Browser never requested the authorization page");
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "OAuth listener leaked"
        );
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            thread.join().unwrap();
        }
    }
}

fn serve(
    stream: &mut TcpStream,
    provider: &str,
    proceed: bool,
    painted: &AtomicBool,
    callback_port: &AtomicUsize,
) {
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    stream
        .set_write_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 2048];
    while !bytes.windows(4).any(|end| end == b"\r\n\r\n") {
        let Ok(count) = stream.read(&mut chunk) else {
            return;
        };
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 32 * 1024, "oversized fixture request");
    }
    let request = String::from_utf8(bytes).unwrap();
    let target = request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let (status, body) = if target == "/painted-visible-layout" {
        painted.store(true, Ordering::Release);
        ("200 OK", "ok".to_owned())
    } else if let Some(query) = target.strip_prefix("/authorize?") {
        let parameters: std::collections::BTreeMap<_, _> =
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
        assert_eq!(
            parameters.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(!parameters["state"].is_empty());
        assert!(!parameters["code_challenge"].is_empty());
        let callback = url::Url::parse(&parameters["redirect_uri"]).unwrap();
        assert_eq!(callback.host_str(), Some("127.0.0.1"));
        callback_port.store(usize::from(callback.port().unwrap()), Ordering::Release);
        let next = serde_json::to_string(&format!("{provider}?{query}")).unwrap();
        // The actor cannot receive a code until the actual Browser executes this
        // document, lays out visible content, crosses two animation frames
        // and reports Chromium's first-contentful-paint performance entry.
        // No HTTP fixture client follows this page on the Browser's behalf.
        let navigation = if proceed {
            format!("location.replace({next});")
        } else {
            String::new()
        };
        (
            "200 OK",
            format!(
                r#"<!doctype html><html><head><title>Local authorization fixture</title></head>
<body><h1 id="receipt">Local authorization fixture</h1><p>No real provider account is used.</p>
<script>async function painted() {{
const node = document.getElementById('receipt');
const box = node.getBoundingClientRect();
if (document.visibilityState !== 'visible' || box.width <= 0 || box.height <= 0 || getComputedStyle(node).visibility !== 'visible') return;
if (!performance.getEntriesByType('paint').some(entry => entry.name === 'first-contentful-paint' && entry.startTime > 0)) {{ requestAnimationFrame(painted); return; }}
const receipt = await fetch('/painted-visible-layout', {{cache:'no-store'}});
if (receipt.ok) {{ {navigation} }}
}}
requestAnimationFrame(() => requestAnimationFrame(painted));</script></body></html>"#
            ),
        )
    } else {
        ("404 Not Found", String::new())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn wait(predicate: impl FnMut() -> bool, failure: &str) {
    tilo_test_env::wait_until(failure, predicate);
}

fn engines() -> BTreeSet<u32> {
    let uid = rustix::process::geteuid().as_raw();
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            if entry.metadata().ok()?.uid() != uid {
                return None;
            }
            // The slot is exclusive to this qualification. Count every process
            // owned by its user, including engine subprocesses with generic or
            // truncated names, rather than guessing CEF's process titles.
            Some(pid)
        })
        .collect()
}

fn assert_cage_ancestor() {
    let mut pid = std::process::id();
    for _ in 0..32 {
        let path = format!("/proc/{pid}");
        if std::fs::read_to_string(format!("{path}/comm")).is_ok_and(|name| name.trim() == "cage") {
            return;
        }
        let status = std::fs::read_to_string(format!("{path}/status")).unwrap();
        pid = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:\t"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        if pid == 0 {
            break;
        }
    }
    panic!("qualification binary must be a child of its owned Cage, not the desktop");
}

fn fixture_controller(
    root: &std::path::Path,
    server: &FakeAuthorizationServer,
    page: &Page,
) -> (AccountsController, Arc<LiveBrowserOpener>) {
    let mut provider = server.provider();
    provider.authorization_endpoint = page.endpoint();
    let opener = Arc::new(LiveBrowserOpener::new().unwrap());
    let controller = AccountsController::fixture(root, opener.clone(), |custody| {
        custody.with_provider(
            OAuthCloud::GoogleDrive,
            provider,
            account_custody::Registration::public("native-browser-fixture"),
        )
    })
    .unwrap();
    (controller, opener)
}

#[test]
#[ignore = "requires an owned disposable VM and matching signed Browser/SDK/CEF slot layers"]
fn live_browser_renders_and_completes_canonical_pkce_under_cage() {
    assert_eq!(
        std::env::var("TILO_INSTALLER_BROWSER_NATIVE").as_deref(),
        Ok("1")
    );
    assert!(!rustix::process::getuid().is_root());
    assert!(!rustix::process::geteuid().is_root());
    assert_cage_ancestor();
    let baseline = engines();
    for outcome in ["success", "cancel", "forged-state"] {
        let directory = tempfile::tempdir().unwrap();
        let server = FakeAuthorizationServer::start();
        if outcome == "forged-state" {
            server.forge_state("deliberately-wrong");
        }
        let page = Page::start(
            server.provider().authorization_endpoint,
            outcome != "cancel",
        );
        let (mut controller, opener) = fixture_controller(directory.path(), &server, &page);
        controller
            .command(AccountsCommand::Add(view::AccountRequest {
                provider: view::Provider::Google,
                email: String::new(),
                server: String::new(),
                files: true,
                mail: false,
                calendar: false,
            }))
            .unwrap();
        wait(
            || page.painted.load(Ordering::Acquire),
            "Browser did not produce a visible DOM/layout receipt",
        );
        assert!(
            !engines().is_subset(&baseline),
            "visible Browser receipt had no live native child processes"
        );
        if outcome == "cancel" {
            controller.command(AccountsCommand::Cancel).unwrap();
            wait(
                || !controller.worker_alive.load(Ordering::Acquire),
                "cancelled OAuth actor leaked",
            );
            assert!(controller.poll() == AccountsStatus::Ready);
        } else {
            wait(
                || controller.poll() != AccountsStatus::SigningIn,
                "canonical callback did not complete",
            );
            assert!(
                controller.status()
                    == if outcome == "success" {
                        AccountsStatus::Added
                    } else {
                        AccountsStatus::Failed
                    }
            );
        }
        if outcome == "success" {
            assert_eq!(
                server.verifiers().len(),
                1,
                "canonical PKCE exchange was not observed"
            );
            assert_eq!(controller.selected().len(), 1);
            assert_eq!(AccountStore::load_in(&controller.root).len(), 1);
            assert!(
                SecretVault::open_read_only_in(&controller.root)
                    .get(&controller.selected()[0])
                    .is_some()
            );
        } else {
            assert!(server.verifiers().is_empty());
            assert!(controller.selected().is_empty());
            assert!(!accounts::store_path_in(&controller.root).exists());
            assert!(!accounts::vault_dir_in(&controller.root).exists());
        }
        page.assert_callback_closed();
        let root = controller.root.clone();
        drop(controller);
        opener.close().unwrap();
        drop(opener);
        assert!(!root.exists());
        wait(
            || engines().is_subset(&baseline),
            "Browser/CEF/launcher processes leaked after actor cleanup",
        );
        // Fixed, non-secret receipts only. URLs, state, verifier, tokens and
        // account addresses must never appear in qualification output.
        eprintln!(
            "native-browser: {outcome}; first-contentful-paint=true; visible-layout=true; callback-closed=true; engines-clean=true"
        );
    }
}
