//! What `app update` costs on a network far away: a registry that answers slowly, many apps, and the
//! requests each update makes.

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

use crate::registry_to_installed::{
    Device, author_tapp_version, registry_config, signed_entry_version, test_publisher,
};

const HANDSHAKE: Duration = Duration::from_millis(150);
const ROUND_TRIP: Duration = Duration::from_millis(150);
/// Installed apps the registry offers an update for: more than are
/// downloaded at once, so later downloads can reuse a connection.
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
        let far = self.far.load(Ordering::SeqCst);
        if far {
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
            if far {
                self.requests.lock().unwrap().push(path.clone());
                let open = self.open.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                self.peak.fetch_max(open, Ordering::SeqCst);
                std::thread::sleep(ROUND_TRIP);
                // A download waits for a second one to be asked for, so
                // downloads that overlap always do, however slow the machine,
                // and ones that queue never do.
                let deadline = tilo_test_env::hang_deadline();
                while path.starts_with("/artifacts/")
                    && self.peak.load(Ordering::SeqCst) < 2
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
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

/// Publish `version` of every app, each with the compressed twin the store
/// keeps beside a package, and return the registry entries.
fn publish(
    registry: &FarRegistry,
    base: &str,
    version: &str,
) -> Vec<registry_types::RegistryEntry> {
    (0..APPS)
        .map(|index| {
            let id = format!("so.tilo.update-latency-{index}");
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

#[test]
fn updating_every_app_overlaps_downloads_and_reuses_connections() {
    let registry = FarRegistry::default();
    let base = registry.start();
    let owner = SigningKey::from_bytes(&[23_u8; 32]);

    let data = tempfile::tempdir().unwrap();
    let _device = Device::new(
        Some(registry_config("far", &base, &owner)),
        &[("XDG_DATA_HOME", data.path())],
    );

    let first = snapshot(publish(&registry, &base, "1.0.0"), &owner, None);
    registry.put("/entries", serde_json::to_vec(&first).unwrap());
    for offer in app_cli::available().apps {
        app_cli::install::install_app(&offer).unwrap();
    }
    assert_eq!(package_store::list_installed().unwrap().len(), APPS);

    let second = snapshot(publish(&registry, &base, "2.0.0"), &owner, Some(&first));
    registry.put("/entries", serde_json::to_vec(&second).unwrap());
    registry.far.store(true, Ordering::SeqCst);

    let started = Instant::now();
    let update = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .arg("update")
        .output_locked()
        .unwrap();
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

    // One catalogue for the whole run, however many apps it updates.
    assert_eq!(registry.requests_for("/entries"), 1);
    // Each package is one request: its compressed twin.
    assert_eq!(registry.requests_for("/artifacts/"), APPS);
    // The downloads overlap rather than waiting app after app.
    assert!(peak > 1, "downloads ran one at a time");
    // A connection, once open, carries the next download: the catalogue's,
    // and one per download running at once.
    assert!(
        connections <= app_cli::install::PARALLEL_DOWNLOADS + 1,
        "{connections} connections for {requests} requests"
    );
}
