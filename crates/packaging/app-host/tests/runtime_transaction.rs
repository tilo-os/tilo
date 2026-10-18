//! Optional-runtime review and consent through the production Cap'n Proto service.
//! Signed HTTP fixtures mirror the native runtime release format.

#![allow(
    clippy::future_not_send,
    reason = "Cap'n Proto clients and RPC drivers are confined to the test's single-threaded LocalSet"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use app_cli::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_app_package::Manifest;
use tilo_app_package::pack::{SigningIdentity, pack};

const APP_ID: &str = "so.tilo.linked-e2e";

/// What the stand-in registry serves, by request path.
#[derive(Clone, Default)]
struct Served(
    Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Option<MetadataGate>>>,
);

struct MetadataGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Served {
    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.0.lock().unwrap().insert(path.to_string(), bytes);
    }

    fn start(&self) -> HttpFixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = self.clone();
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = Arc::clone(&stopping);
        let driver = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                serve(stream, &served);
            }
        });
        HttpFixture {
            port,
            stopping,
            driver: Some(driver),
        }
    }
}

fn serve(mut stream: TcpStream, served: &Served) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or(path);
    served.1.lock().unwrap().push(path.to_string());
    if path == "/entries" {
        let gate = served.2.lock().unwrap().take();
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            gate.release
                .recv_timeout(std::time::Duration::from_secs(15))
                .unwrap();
        }
    }
    let body = served.0.lock().unwrap().get(path).cloned();
    let (status, body) = match body {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", b"not found".to_vec()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn publisher() -> SigningKey {
    SigningKey::from_bytes(&[21_u8; 32])
}

fn manifest_isa() -> String {
    format!("{} = \"{}\"", native_target().0, native_target().1)
}

/// A signed runtime build of 2026.07 holding `toolkit` as its Rust dylib.
fn runtime_bundle(root: &Path, name: &str, toolkit: &[u8], identity: &Path) -> (String, PathBuf) {
    let host = root.join(format!("host-{name}"));
    let loader = runtime_bundle::host_minimal_paths_for(runtime_bundle::host_arch())
        .unwrap()
        .first()
        .unwrap()
        .trim_start_matches('/');
    std::fs::create_dir_all(host.join(loader).parent().unwrap()).unwrap();
    std::fs::write(host.join(loader), b"loader").unwrap();
    let dylib = root.join(format!("toolkit-{name}/libruntime_dylib.so"));
    std::fs::create_dir_all(dylib.parent().unwrap()).unwrap();
    std::fs::write(&dylib, toolkit).unwrap();
    let (manifest, dir) = runtime_bundle::build_abi_bundle(
        &root.join(format!("bundles-{name}")),
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: Some(&host),
            arch: None,
            identity: Some(identity),
            extra_libs: &[dylib],
            extra_bins: &[],
        },
    )
    .unwrap();
    (manifest.version, dir)
}

fn tapp(manifest: &str, payload: &Path) -> Vec<u8> {
    let manifest = Manifest::from_toml_str(manifest).unwrap();
    let packed = pack(
        payload,
        &manifest,
        &SigningIdentity::from_signing_key(publisher()),
    )
    .unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

fn app_tapp(root: &Path, runtime: &str, optional: &[&str]) -> Vec<u8> {
    let payload = root.join("app-payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), b"#!/bin/true\n").unwrap();
    tapp(
        &format!(
            r#"
[package]
id = "{APP_ID}"
version = "2.0.0"
kind = "app"
name = "Linked"
runtime = "{runtime}"
optional-runtimes = {optional:?}

[package.isa]
{isa}

[publisher]
identity = "ed25519:{key}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            isa = manifest_isa(),
            key = hex::encode(publisher().verifying_key().to_bytes()),
        ),
        &payload,
    )
}

fn runtime_tapp(bundle: &Path, provides: &str, version: &str) -> Vec<u8> {
    tapp(
        &format!(
            r#"
[package]
id = "so.tilo.runtime"
version = "{version}"
kind = "runtime"
name = "Tilo runtime"
provides = "{provides}"

[package.isa]
{isa}

[publisher]
identity = "ed25519:{key}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
            isa = manifest_isa(),
            key = hex::encode(publisher().verifying_key().to_bytes()),
        ),
        bundle,
    )
}

fn entry(package_id: &str, version: &str, url: &str, tapp: &[u8], runtime: &str) -> RegistryEntry {
    let root = app_registry::tapp::load(tapp).unwrap().package_id;
    let mut entry = RegistryEntry {
        package_id: package_id.into(),
        kind: registry_types::EntryKind::App,
        name: package_id.into(),
        description: "end to end".into(),
        tagline: "end to end".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["Utility".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher().verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: version.into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: url.into(),
                tapp_root_hash_hex: root.to_hex(),
                tapp_size_bytes: tapp.len() as u64,
                tapp_blake3_hex: blake3::hash(tapp).to_hex().to_string(),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: native_target().0.into(),
                    isa: native_target().2.into(),
                    runtime: Some(runtime.into()),
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher());
    entry
}

fn snapshot(
    entries: Vec<RegistryEntry>,
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
            pubkey_hex: hex::encode(publisher().verifying_key().to_bytes()),
            can_add_new: true,
        }],
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
        bots_signature_hex: String::new(),
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

struct HttpFixture {
    port: u16,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    driver: Option<std::thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn stop(&mut self) {
        if let Some(driver) = self.driver.take() {
            self.stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            driver.join().unwrap();
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn native_target() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_arch = "x86_64")]
    {
        ("x86_64", "baseline", "x86_64")
    }
    #[cfg(target_arch = "aarch64")]
    {
        ("aarch64", "armv8-a", "aarch64")
    }
}

fn installer() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tilo-install");
    assert!(
        path.is_file(),
        "build the actual tilo-install before running this test"
    );
    path
}

use app_host::app_host_capnp::app_host as rpc;
use app_host::runtime_transaction::RuntimeReview;
use package_store::{Catalog, PackageStore, Registry, SystemStore};
use tilo_app_package::PackageId;

struct HostConnection {
    client: rpc::Client,
    driver: tokio::task::JoinHandle<Result<(), capnp::Error>>,
}

impl HostConnection {
    fn new(root: &Path) -> Self {
        let system = root.join("empty-image");
        std::fs::create_dir_all(&system).unwrap();
        let store = Arc::new(PackageStore::at(root.join("apps")));
        let catalog = Arc::new(Catalog::at(
            root.join("apps"),
            Some(SystemStore {
                data_dir: system.clone(),
                payload_dir: system,
                interfaces: None,
            }),
        ));
        let server = app_host::service::AppHostServer::new(store, catalog, 0);
        let service: rpc::Client = capnp_rpc::new_client(server);
        let (peer, connection) = tokio::net::UnixStream::pair().unwrap();
        let driver = tokio::task::spawn_local(runtime_portal::serve_connected_io(
            connection,
            service.client,
        ));
        let client = runtime_portal::CapnpClient::from_connected_io(
            peer,
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
        )
        .bootstrap();
        Self { client, driver }
    }

    async fn close(self) {
        drop(self.client);
        self.driver.abort();
        match self.driver.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(result) => assert!(result.is_ok()),
        }
    }
}

fn inspect(
    client: &rpc::Client,
    root: PackageId,
    runtime: &str,
) -> capnp::capability::RemotePromise<rpc::inspect_optional_runtime_results::Owned> {
    let mut request = client.inspect_optional_runtime_request();
    request.get().set_app_id(APP_ID);
    request.get().set_consumer_root(root.as_bytes());
    request.get().set_runtime(runtime);
    request.send()
}

async fn reviewed(client: &rpc::Client, root: PackageId, runtime: &str) -> RuntimeReview {
    let response = inspect(client, root, runtime).promise.await.unwrap();
    serde_json::from_str(
        response
            .get()
            .unwrap()
            .get_review_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap()
}

type ProgressEvents = Arc<Mutex<Vec<(bool, u64)>>>;
struct ProgressObserver(ProgressEvents);

impl app_host::app_host_capnp::runtime_progress::Server for ProgressObserver {
    fn update(
        self: capnp::capability::Rc<Self>,
        params: app_host::app_host_capnp::runtime_progress::UpdateParams,
        _results: app_host::app_host_capnp::runtime_progress::UpdateResults,
    ) -> impl core::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let result = params.get().map(|p| {
            self.0
                .lock()
                .unwrap()
                .push((p.get_installing(), p.get_received()));
        });
        std::future::ready(result)
    }
}

async fn acquire_observed(
    client: &rpc::Client,
    review: &RuntimeReview,
    events: ProgressEvents,
) -> Result<(), capnp::Error> {
    let mut request = client.acquire_optional_runtime_request();
    let observer: app_host::app_host_capnp::runtime_progress::Client =
        capnp_rpc::new_client(ProgressObserver(events));
    request.get().set_progress(observer);
    request
        .get()
        .set_review_json(serde_json::to_string(review).unwrap());
    request.send().promise.await.map(|_| ())
}

async fn acquire(client: &rpc::Client, review: &RuntimeReview) -> Result<(), capnp::Error> {
    let mut request = client.acquire_optional_runtime_request();
    request
        .get()
        .set_review_json(serde_json::to_string(review).unwrap());
    request.send().promise.await.map(|_| ())
}

fn assert_no_payload(served: &Served) {
    let requested = served.1.lock().unwrap().clone();
    assert!(
        requested.iter().all(|path| path.starts_with("/entries")),
        "review and rejected consent must request metadata only: {requested:?}"
    );
}

struct Device {
    scratch_dir: tempfile::TempDir,
    _env: tilo_test_env::ScopedEnv,
    served: Served,
    http: HttpFixture,
    runtime: Vec<u8>,
    optional_ref: String,
    baseline_ref: String,
    root: PackageId,
    trust: PathBuf,
    signer: runtime_bundle::Identity,
    catalog: Catalog,
}

impl Device {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = root.path().join("identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let trust = root.path().join("trusted.txt");
        std::fs::write(&trust, format!("{}\n", identity.public_key_hex())).unwrap();
        let (device_version, device_bundle) =
            runtime_bundle(root.path(), "device", b"base toolkit", &identity_path);
        let (optional_version, optional_bundle) =
            runtime_bundle(root.path(), "optional", b"optional toolkit", &identity_path);
        let device_ref = format!("tilo-runtime/{device_version}");
        let optional_ref = format!("tilo-runtime/{optional_version}");
        let runtimes = root.path().join("runtimes");
        std::fs::create_dir(&runtimes).unwrap();
        std::fs::rename(device_bundle, runtimes.join(&device_version)).unwrap();
        runtime_bundle::gc::BundleRoot::new(&runtimes)
            .pin(&device_version)
            .unwrap();

        let served = Served::default();
        let http = served.start();
        let base = format!("http://127.0.0.1:{}", http.port);
        let app = app_tapp(root.path(), &device_ref, &[&optional_ref]);
        let runtime = runtime_tapp(&optional_bundle, &optional_ref, "0.1.7");
        served.put("/optional.tapp", runtime.clone());
        let owner = SigningKey::from_bytes(&[22_u8; 32]);
        let offer = entry(
            registry_types::NATIVE_RUNTIME_PACKAGE_ID,
            "0.1.7",
            &format!("{base}/optional.tapp"),
            &runtime,
            &optional_ref,
        );
        served.put(
            "/entries",
            serde_json::to_vec(&snapshot(vec![offer], &owner, None)).unwrap(),
        );

        let config = root.path().join("config");
        std::fs::create_dir(&config).unwrap();
        std::fs::write(
            config.join("registries.toml"),
            toml::to_string_pretty(&RegistriesFile {
                registries: vec![RegistryConfig {
                    name: "local".into(),
                    base_url: base,
                    owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                    enabled: true,
                    channel: tilo_app_package::version::ReleaseChannel::Stable,
                }],
            })
            .unwrap(),
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        for (key, value) in [
            ("TILO_STORE_CONFIG_DIR", config),
            ("TILO_STORE_CACHE_DIR", root.path().join("cache")),
            ("TILO_APPS_DATA_DIR", root.path().join("apps")),
            ("TILO_INSTALL_BIN", installer()),
            ("TILO_RUNTIME_BUNDLE_DIR", runtimes),
            ("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", trust.clone()),
            ("XDG_DATA_HOME", root.path().join("data")),
        ] {
            env.set(key, value);
        }

        // Seed only the consumer through the existing signed-package harness.
        // Every optional-runtime operation below travels over the real RPC.
        let store = PackageStore::at(root.path().join("apps"));
        let installed =
            app_host::install::install_tapp(&store, &app, package_store::Pinning::Pinned, None)
                .unwrap();
        let catalog = Catalog::at(
            root.path().join("apps"),
            Some(SystemStore {
                data_dir: root.path().join("empty-image"),
                payload_dir: root.path().join("empty-image"),
                interfaces: None,
            }),
        );
        Self {
            scratch_dir: root,
            _env: env,
            served,
            http,
            runtime,
            optional_ref,
            baseline_ref: device_ref,
            root: installed.package_id().unwrap(),
            trust,
            signer: identity,
            catalog,
        }
    }
}

#[test]
fn signed_runtime_transaction_over_real_rpc() {
    let mut device = Device::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let host = HostConnection::new(device.scratch_dir.path());
        runtime_gate_rejects_overlap(&device, &host.client).await;
        let review = reviewed(&host.client, device.root, &device.optional_ref).await;
        let offer = review.offer.as_ref().unwrap();
        assert_eq!(review.consumer_app, APP_ID);
        assert_eq!(review.consumer_root, device.root.to_hex());
        assert_eq!(review.runtime, device.optional_ref);
        assert_eq!(offer.package_id, registry_types::NATIVE_RUNTIME_PACKAGE_ID);
        assert_eq!(offer.version, "0.1.7");
        assert_eq!(
            offer.root,
            app_registry::tapp::load(&device.runtime)
                .unwrap()
                .package_id
                .to_hex()
        );
        assert_eq!(
            offer.publisher,
            hex::encode(publisher().verifying_key().to_bytes())
        );
        assert_eq!(
            offer.archive_hash,
            blake3::hash(&device.runtime).to_hex().to_string()
        );
        assert_eq!(offer.size_bytes, device.runtime.len() as u64);
        assert_no_payload(&device.served);
        assert!(!app_cli::runtime_installed(&device.optional_ref));

        reject_unadmitted(&device, &host.client, &review).await;
        assert_no_payload(&device.served);

        // Corrupt artifact bytes fail their catalogue hash and publish no runtime.
        let mut corrupted = device.runtime.clone();
        let last = corrupted.last_mut().unwrap();
        *last ^= 1;
        device.served.put("/optional.tapp", corrupted);
        let error = acquire(&host.client, &review)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("hash"), "{error}");
        assert!(!app_cli::runtime_installed(&device.optional_ref));

        // The bundle signer is independent of the registry package publisher.
        std::fs::write(
            &device.trust,
            format!(
                "{}\n",
                runtime_bundle::Identity::generate().public_key_hex()
            ),
        )
        .unwrap();
        device.served.put("/optional.tapp", device.runtime.clone());
        let error = acquire(&host.client, &review)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("trust") || error.contains("signer") || error.contains("publisher"),
            "{error}"
        );
        assert!(!app_cli::runtime_installed(&device.optional_ref));

        std::fs::write(
            &device.trust,
            format!("{}\n", device.signer.public_key_hex()),
        )
        .unwrap();
        let events = ProgressEvents::default();
        acquire_observed(&host.client, &review, events.clone())
            .await
            .unwrap();
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|(installing, received)| *installing && *received > 0),
            "the connection-owned observer must receive actual installer progress"
        );
        assert!(app_cli::runtime_installed(&device.optional_ref));
        assert!(app_cli::runtime_installed(&device.baseline_ref));

        // No metadata or payload request is allowed once verified bytes exist.
        device.http.stop();
        assert!(TcpStream::connect(("127.0.0.1", device.http.port)).is_err());
        device.served.0.lock().unwrap().clear();
        let before = device.served.1.lock().unwrap().len();
        let present = reviewed(&host.client, device.root, &device.optional_ref).await;
        assert!(present.offer.is_none());
        acquire(&host.client, &present).await.unwrap();
        acquire(&host.client, &review).await.unwrap();
        assert_eq!(device.served.1.lock().unwrap().len(), before);
        host.close().await;
    });
}

async fn reject_unadmitted(device: &Device, client: &rpc::Client, review: &RuntimeReview) {
    for runtime in [
        "tilo-runtime/not-declared",
        "tilo-runtime/../escape",
        "other-runtime/1",
    ] {
        assert!(
            inspect(client, device.root, runtime).promise.await.is_err(),
            "{runtime}"
        );
        let mut forged = review.clone();
        forged.runtime = runtime.into();
        assert!(acquire(client, &forged).await.is_err(), "{runtime}");
    }
    let wrong = PackageId::from_bytes([0x55; 32]);
    assert!(
        inspect(client, wrong, &device.optional_ref)
            .promise
            .await
            .is_err()
    );
    let mut replaced = review.clone();
    replaced.consumer_root = wrong.to_hex();
    assert!(acquire(client, &replaced).await.is_err());

    let original = device.catalog.find(APP_ID).unwrap().unwrap();
    let manifest = std::fs::read(&original.manifest_path).unwrap();
    std::fs::write(
        &original.manifest_path,
        String::from_utf8(manifest.clone())
            .unwrap()
            .replace(&device.optional_ref, "tilo-runtime/fake"),
    )
    .unwrap();
    assert!(
        inspect(client, device.root, &device.optional_ref)
            .promise
            .await
            .is_err()
    );
    assert!(acquire(client, review).await.is_err());
    std::fs::write(&original.manifest_path, manifest).unwrap();

    let path = device.catalog.user().registry();
    let mut registry = Registry::load(&path).unwrap();
    let mut replaced = original.clone();
    replaced.package_id_hex = wrong.to_hex();
    registry.upsert(replaced);
    registry.save(&path).unwrap();
    assert!(
        inspect(client, device.root, &device.optional_ref)
            .promise
            .await
            .is_err()
    );
    assert!(acquire(client, review).await.is_err());

    let mut forged = original.clone();
    forged.publisher_key_hex = hex::encode([0x77; 32]);
    registry.upsert(forged);
    registry.save(&path).unwrap();
    assert!(
        inspect(client, device.root, &device.optional_ref)
            .promise
            .await
            .is_err()
    );
    assert!(acquire(client, review).await.is_err());
    registry.upsert(original);
    registry.save(&path).unwrap();

    let mut offer = review.clone();
    offer.offer.as_mut().unwrap().archive_hash = hex::encode([0x99; 32]);
    assert!(acquire(client, &offer).await.is_err());
}

#[allow(
    clippy::panic,
    reason = "completion before the held metadata response is an explicit asynchronous test failure"
)]
async fn runtime_gate_rejects_overlap(device: &Device, client: &rpc::Client) {
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, resumed) = std::sync::mpsc::channel();
    *device.served.2.lock().unwrap() = Some(MetadataGate {
        entered,
        release: resumed,
    });
    let first = inspect(client, device.root, &device.optional_ref).promise;
    tokio::pin!(first);
    tokio::select! {
        _result = &mut first => panic!("inspection completed before the held metadata response"),
        result = tokio::time::timeout(std::time::Duration::from_secs(10), observed) => {
            result.expect("metadata RPC entered within hang bound").unwrap();
        }
    }
    let second = inspect(client, device.root, &device.optional_ref).promise;
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), second).await;
    // Always release the real HTTP operation before asserting so failure cannot
    // strand the server thread or the service's blocking worker.
    release.send(()).unwrap();
    let result = result.expect("overlapping RPC must reject without waiting");
    assert!(result.is_err(), "overlapping RPC was admitted");
    let error = result.err().expect("RPC refusal").to_string();
    assert!(error.contains("already active"), "{error}");
    first.await.unwrap();
}
