//! What `app update` costs on a network far away: a registry that answers
//! every request a round trip late and charges a handshake for every new
//! connection, as a store behind a CDN does (the public store measured
//! ~0.3 s to connect and ~0.4 s to the first byte from a laptop in Europe).
//!
//! The device has several apps installed and the registry offers each a
//! newer version. Updating them is network-bound: the installs themselves
//! are local work. So the test pins what decides the wait — one catalogue
//! request, downloads that overlap instead of queueing app after app, and
//! connections that are reused rather than opened per request — and reports
//! the wall time.
//!
//! `cargo test -p app-cli --test it update_latency -- --nocapture`

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

use app_cli::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;
use registry_types::{PublisherAuthorization, SignedRegistrySnapshot, sign_snapshot};

use crate::registry_to_installed::{
    author_tapp_version, scoped_env, signed_entry_version, test_publisher, tilo_install_bin,
};

/// What a new connection costs before its first request is read: TCP and TLS.
const HANDSHAKE: Duration = Duration::from_millis(150);
/// How long every request waits for its answer.
const ROUND_TRIP: Duration = Duration::from_millis(150);
/// Installed apps the registry offers an update for: more than are
/// downloaded at once, so later downloads can reuse a connection.
const APPS: usize = 12;

/// A loopback registry with a far-away registry's latency, keeping
/// connections alive between requests as an HTTP/1.1 or HTTP/2 server does.
#[derive(Clone, Default)]
struct FarRegistry {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
    connections: Arc<AtomicUsize>,
    open: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    /// Off while the fixture installs the old versions.
    far: Arc<AtomicBool>,
}

impl FarRegistry {
    fn start(&self) -> String {
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

    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.objects.lock().unwrap().insert(path.to_owned(), bytes);
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
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) if line.trim().is_empty() => break,
                    Ok(_) => {}
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let path = path.split('?').next().unwrap_or(path).to_owned();
            if far {
                self.requests.lock().unwrap().push(path.clone());
                let open = self.open.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                self.peak.fetch_max(open, Ordering::SeqCst);
                std::thread::sleep(ROUND_TRIP);
                self.open.fetch_sub(1, Ordering::SeqCst);
            }
            let body = self.objects.lock().unwrap().get(&path).cloned();
            let (status, body) = match body {
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

    fn requests_for(&self, prefix: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.starts_with(prefix))
            .count()
    }
}

fn snapshot(
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

    let config = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "far".into(),
                base_url: base.clone(),
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let installer = tilo_install_bin();
    let _env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", config.path()),
        ("TILO_STORE_CACHE_DIR", cache.path()),
        ("TILO_APPS_DATA_DIR", apps.path()),
        ("TILO_INSTALL_BIN", &installer),
        ("XDG_DATA_HOME", data.path()),
    ]);

    // The device: every app installed at 1.0.0, without the far latency.
    let first = snapshot(publish(&registry, &base, "1.0.0"), &owner, None);
    registry.put("/entries", serde_json::to_vec(&first).unwrap());
    for offer in app_cli::available().apps {
        app_cli::install::install_app(&offer).unwrap();
    }
    assert_eq!(package_store::list_installed().unwrap().len(), APPS);

    // The registry now offers 2.0.0 of each, and is far away.
    let second = snapshot(publish(&registry, &base, "2.0.0"), &owner, Some(&first));
    registry.put("/entries", serde_json::to_vec(&second).unwrap());
    registry.far.store(true, Ordering::SeqCst);

    let started = Instant::now();
    let update = std::process::Command::new(env!("CARGO_BIN_EXE_app"))
        .arg("update")
        .output()
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
    // On the network the run costs the catalogue and two overlapped waves of
    // downloads, the first on fresh connections; the installs are local
    // work. Budgets apply to optimised builds only.
    if !cfg!(debug_assertions) {
        let network = (HANDSHAKE + ROUND_TRIP) * 2 + ROUND_TRIP;
        let budget = network + Duration::from_secs(2);
        assert!(
            elapsed < budget,
            "updating {APPS} apps took {elapsed:?}, budget {budget:?}"
        );
    }
}
