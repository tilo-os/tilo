//! What `app update` fetches and costs: only what changed, over a registry that answers slowly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic,
    reason = "a benchmark reports on stderr and fails loudly"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_test_env::SpawnUnderEnvLock as _;

use ed25519_dalek::SigningKey;
use registry_types::{PublisherAuthorization, SignedRegistrySnapshot, sign_snapshot};

use crate::daemon_fixture::Daemon;
use crate::registry_fixture::{
    Device, author_tapp_version, registry_config, signed_entry_version, test_publisher,
};

const HANDSHAKE: Duration = Duration::from_millis(150);
const ROUND_TRIP: Duration = Duration::from_millis(150);
/// Installed apps the registry offers an update for.
const APPS: usize = 12;

/// A loopback registry with a far-away registry's latency, keeping
/// connections alive between requests as an HTTP/1.1 or HTTP/2 server does.
#[derive(Clone, Default)]
pub(crate) struct FarRegistry {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
    seen: Arc<Mutex<Vec<String>>>,
    etags: Arc<Mutex<HashMap<String, String>>>,
    not_modified: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    far: Arc<AtomicBool>,
}

impl FarRegistry {
    pub(crate) fn start(&self) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let registry = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let registry = registry.clone();
                std::thread::spawn(move || registry.serve(stream));
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    pub(crate) fn put(&self, path: &str, bytes: Vec<u8>) {
        self.objects.lock().unwrap().insert(path.to_owned(), bytes);
    }

    /// Serve `snapshot` as the catalogue with its hash as the `ETag`, as the
    /// registry does, answering a request that names it with a `304`.
    pub(crate) fn put_catalogue(&self, snapshot: &SignedRegistrySnapshot) {
        self.put("/entries", serde_json::to_vec(snapshot).unwrap());
        self.etags.lock().unwrap().insert(
            "/entries".to_owned(),
            format!("\"{}\"", registry_types::snapshot_hash(snapshot).unwrap()),
        );
    }

    pub(crate) fn not_modified(&self) -> usize {
        self.not_modified.load(Ordering::SeqCst)
    }

    fn serve(&self, mut stream: TcpStream) {
        // The handshake is one per connection, when the connection is the far one's.
        if self.far.load(Ordering::SeqCst) {
            self.connections.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(HANDSHAKE);
        }
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut request = String::new();
            if reader.read_line(&mut request).unwrap_or(0) == 0 {
                return;
            }
            let mut if_none_match = None;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) if line.trim().is_empty() => break,
                    Ok(_) => {
                        if let Some((name, value)) = line.split_once(':')
                            && name.trim().eq_ignore_ascii_case("if-none-match")
                        {
                            if_none_match = Some(value.trim().to_owned());
                        }
                    }
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let path = path.split('?').next().unwrap_or(path).to_owned();
            self.seen.lock().unwrap().push(path.clone());
            // Read per request: a connection kept alive from before the registry turned far
            // still carries a request the run is measuring.
            if self.far.load(Ordering::SeqCst) {
                self.requests.lock().unwrap().push(path.clone());
                let open = self.open.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                self.peak.fetch_max(open, Ordering::SeqCst);
                std::thread::sleep(ROUND_TRIP);
                self.open.fetch_sub(1, Ordering::SeqCst);
            }
            let body = self.objects.lock().unwrap().get(&path).cloned();
            let etag = self.etags.lock().unwrap().get(&path).cloned();
            let unchanged = etag.is_some() && etag == if_none_match;
            let (status, body) = match body {
                Some(_) if unchanged => {
                    self.not_modified.fetch_add(1, Ordering::SeqCst);
                    ("304 Not Modified", Vec::new())
                }
                Some(body) => ("200 OK", body),
                None => ("404 Not Found", b"not found".to_vec()),
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            if stream.write_all(header.as_bytes()).is_err()
                || stream.write_all(&body).is_err()
                || stream.flush().is_err()
            {
                return;
            }
        }
    }

    pub(crate) fn take_seen(&self) -> Vec<String> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    pub(crate) fn requests_for(&self, prefix: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.starts_with(prefix))
            .count()
    }
}

pub(crate) fn snapshot(
    entries: Vec<registry_types::RegistryEntry>,
    owner: &SigningKey,
    previous: Option<&SignedRegistrySnapshot>,
) -> SignedRegistrySnapshot {
    let now = tilo_clock::unix_now();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: previous.map_or(1, |previous| previous.sequence.saturating_add(1)),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: previous
            .map(|previous| registry_types::snapshot_hash(previous).unwrap())
            .unwrap_or_default(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: hex::encode(test_publisher().public_key_bytes()),
            can_add_new: true,
        }],
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

/// Publish the given version of every app, each with the compressed twin the
/// store keeps beside a package, and return the registry entries.
fn publish(
    registry: &FarRegistry,
    base: &str,
    versions: &[&str],
) -> Vec<registry_types::RegistryEntry> {
    versions
        .iter()
        .enumerate()
        .map(|(index, version)| {
            let id = app_id(index);
            let tapp = author_tapp_version(&id, version, &test_publisher());
            let path = format!("/artifacts/{id}-{version}.tapp");
            registry.put(
                &format!("{path}{}", tilo_app_package::compressed::SUFFIX),
                tilo_app_package::compressed::compress(&tapp).unwrap(),
            );
            registry.put(&path, tapp.clone());
            signed_entry_version(&id, version, &format!("{base}{path}"), &tapp)
        })
        .collect()
}

fn app_id(index: usize) -> String {
    format!("so.tilo.update-latency-{index}")
}

fn app_update() -> std::process::Output {
    std::process::Command::new(tilo_test_env::package_binary!("app"))
        .arg("update")
        .output_locked()
        .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one run installs, revalidates and updates over a far registry, each step asserted"
)]
fn an_update_fetches_only_what_changed_one_app_at_a_time() {
    let registry = FarRegistry::default();
    let base = registry.start();
    let owner = SigningKey::from_bytes(&[23_u8; 32]);

    let data = tempfile::tempdir().unwrap();
    let device = Device::new(
        Some(registry_config("far", &base, &owner)),
        &[("XDG_DATA_HOME", data.path())],
    );
    let _daemon = Daemon::start(device.apps.path());

    let versions = vec!["1.0.0"; APPS];
    let first = snapshot(publish(&registry, &base, &versions), &owner, None);
    registry.put_catalogue(&first);
    let started = Instant::now();
    for offer in app_cli::available().apps {
        app_cli::install::install_app(&offer).unwrap();
    }
    eprintln!(
        "perf installing {APPS} apps one by one: {:?}",
        started.elapsed()
    );
    assert_eq!(package_store::list_installed().unwrap().len(), APPS);
    registry.take_seen();

    // Nothing changed: the catalogue is asked once and answers not-modified.
    let not_modified = registry.not_modified();
    let nothing = app_update();
    assert!(
        nothing.status.success(),
        "app update: {}\n{}",
        String::from_utf8_lossy(&nothing.stdout),
        String::from_utf8_lossy(&nothing.stderr)
    );
    assert_eq!(registry.take_seen(), ["/entries"]);
    assert_eq!(registry.not_modified() - not_modified, 1);

    // One app changed: exactly its package is fetched, and only it moves.
    let mut one_changed = versions.clone();
    one_changed[1] = "1.0.1";
    let second = snapshot(
        publish(&registry, &base, &one_changed),
        &owner,
        Some(&first),
    );
    registry.put_catalogue(&second);
    let one = app_update();
    assert!(
        one.status.success(),
        "app update: {}\n{}",
        String::from_utf8_lossy(&one.stdout),
        String::from_utf8_lossy(&one.stderr)
    );
    let seen = registry.take_seen();
    let fetched = artifacts(&seen);
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    assert!(
        fetched
            .first()
            .is_some_and(|path| path.contains(&format!("{}-1.0.1", app_id(1)))),
        "{fetched:?}"
    );
    let rows = package_store::list_installed().unwrap();
    assert_eq!(rows.len(), APPS);
    for row in &rows {
        let expected = if row.id == app_id(1) {
            "1.0.1"
        } else {
            "1.0.0"
        };
        assert_eq!(row.version, expected, "{}", row.id);
    }

    // Every app changed, over a far registry: each app's update fetches its package, the
    // catalogue is revalidated rather than downloaded again, and the CLI waits for each one.
    let all_new = vec!["2.0.0"; APPS];
    let third = snapshot(publish(&registry, &base, &all_new), &owner, Some(&second));
    registry.put_catalogue(&third);
    registry.far.store(true, Ordering::SeqCst);
    let entries_before = registry.requests_for("/entries");
    let revalidated_before = registry.not_modified();

    let started = Instant::now();
    let update = app_update();
    let elapsed = started.elapsed();
    assert!(
        update.status.success(),
        "app update: {}\n{}",
        String::from_utf8_lossy(&update.stdout),
        String::from_utf8_lossy(&update.stderr)
    );
    let rows = package_store::list_installed().unwrap();
    assert_eq!(rows.len(), APPS);
    assert!(rows.iter().all(|row| row.version == "2.0.0"), "{rows:?}");

    let requests = registry.requests.lock().unwrap().len();
    let connections = registry.connections.load(Ordering::SeqCst);
    let peak = registry.peak.load(Ordering::SeqCst);
    eprintln!(
        "perf app update ({APPS} apps, {HANDSHAKE:?} handshake, {ROUND_TRIP:?} round trip): \
         {elapsed:?}, {requests} requests on {connections} connections, at most {peak} at once"
    );

    // Each package is one request: its compressed twin.
    assert_eq!(registry.requests_for("/artifacts/"), APPS);
    // The catalogue body is served once: each app's read of it is a conditional 304.
    let entries = registry.requests_for("/entries") - entries_before;
    let revalidated = registry.not_modified() - revalidated_before;
    assert!(entries >= 1, "the catalogue was never read");
    assert!(
        revalidated + 1 >= entries,
        "{entries} catalogue reads, only {revalidated} answered 304: the body was served again"
    );
    // The CLI waits for each app's update, so one download runs at a time.
    assert_eq!(peak, 1, "downloads overlapped");
    // Each app's update opens a few connections of its own — the catalogue's, its version
    // index's and its download's — never one per request.
    assert!(
        connections <= 3 * APPS + 1,
        "{connections} connections for {requests} requests"
    );
}

fn artifacts(seen: &[String]) -> Vec<&str> {
    seen.iter()
        .map(String::as_str)
        .filter(|path| path.starts_with("/artifacts/"))
        .collect()
}
