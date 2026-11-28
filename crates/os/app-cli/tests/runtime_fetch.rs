//! A registry build linked against a runtime build this device lacks: the app installs once its
//! runtime does, and both are verified.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tilo_test_env::SpawnUnderEnvLock as _;

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_app_package::Manifest;
use tilo_app_package::pack::{SigningIdentity, pack};

use crate::daemon_fixture::Daemon;
use crate::registry_fixture::{Device, native_target, registry_config};

const APP_ID: &str = "so.tilo.linked-e2e";

#[derive(Clone, Default)]
struct Served(
    Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    Arc<Mutex<Vec<String>>>,
);

impl Served {
    fn hits(&self, path: &str) -> usize {
        self.1
            .lock()
            .unwrap()
            .iter()
            .filter(|hit| *hit == path)
            .count()
    }

    fn put(&self, path: &str, bytes: Vec<u8>) {
        self.0.lock().unwrap().insert(path.to_string(), bytes);
    }

    fn start(&self) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let served = served.clone();
                std::thread::spawn(move || serve(stream, &served));
            }
        });
        port
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

fn app_tapp(root: &Path, runtime: &str, required: &[&str]) -> Vec<u8> {
    app_tapp_with_runtimes(root, APP_ID, runtime, required)
}

/// An app on `runtime` that also requires `engine` by exact version.
fn app_tapp_pinning(root: &Path, id: &str, runtime: &str, engine: Option<&str>) -> Vec<u8> {
    let required: Vec<&str> = engine.into_iter().collect();
    app_tapp_with_runtimes(root, id, runtime, &required)
}

fn app_tapp_with_runtimes(root: &Path, id: &str, runtime: &str, required: &[&str]) -> Vec<u8> {
    let payload = root.join(format!("payload-{id}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), b"#!/bin/true\n").unwrap();
    tapp(
        &format!(
            r#"
[package]
id = "{id}"
version = "2.0.0"
kind = "app"
name = "Linked"
runtime = "{runtime}"
optional-runtimes = {required:?}

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
    runtime_package_tapp(bundle, "so.tilo.runtime", provides, version)
}

fn runtime_package_tapp(bundle: &Path, id: &str, provides: &str, version: &str) -> Vec<u8> {
    tapp(
        &format!(
            r#"
[package]
id = "{id}"
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
                tapp_surface_hex: String::new(),
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
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one device, one registry, the phases of one install"
)]
fn a_registry_build_for_another_runtime_fetches_that_runtime_or_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let owner = SigningKey::from_bytes(&[22_u8; 32]);

    let runtime_identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-publisher.json");
    std::fs::write(&identity_path, runtime_identity.to_json_bytes()).unwrap();
    let trust = root.join("trusted-runtime-publishers.txt");
    std::fs::write(&trust, format!("{}\n", runtime_identity.public_key_hex())).unwrap();

    let (device_version, device_bundle) = runtime_bundle(
        root,
        "device",
        b"toolkit the image was built with",
        &identity_path,
    );
    let runtimes = root.join("runtimes");
    std::fs::create_dir_all(&runtimes).unwrap();
    std::fs::rename(&device_bundle, runtimes.join(&device_version)).unwrap();
    // The image's apps keep it installed. This fixture has no image store,
    // so a pin stands in for them; without one, collection after the
    // install would rightly take a build nothing runs on.
    runtime_bundle::gc::BundleRoot::new(&runtimes)
        .pin(&device_version)
        .unwrap();

    let mut toolkit =
        vec![0_u8; usize::try_from(tilo_app_package::chunks::CHUNK_BYTES).unwrap() + 4096];
    for (index, byte) in toolkit.iter_mut().enumerate() {
        *byte = u8::try_from(index % 251).unwrap();
    }
    let (registry_version, registry_bundle) =
        runtime_bundle(root, "registry", &toolkit, &identity_path);
    assert_ne!(device_version, registry_version);
    let needed = format!("tilo-runtime/{registry_version}");

    let served = Served::default();
    let port = served.start();
    let base = format!("http://127.0.0.1:{port}");
    let app = app_tapp(root, &needed, &[]);
    served.put("/linked.tapp", app.clone());
    let app_entry = entry(
        APP_ID,
        "2.0.0",
        &format!("{base}/linked.tapp"),
        &app,
        &needed,
    );

    let data = tempfile::tempdir().unwrap();
    let device = Device::new(
        Some(registry_config("local", &base, &owner)),
        &[
            ("TILO_RUNTIME_BUNDLE_DIR", &runtimes),
            ("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust),
            ("XDG_DATA_HOME", data.path()),
        ],
    );
    let _daemon = Daemon::start(device.apps.path());
    assert!(app_cli::runtime_installed(&format!(
        "tilo-runtime/{device_version}"
    )));
    assert!(!app_cli::runtime_installed(&needed));

    let first = snapshot(vec![app_entry.clone()], &owner, None);
    served.put("/entries", serde_json::to_vec(&first).unwrap());
    let offer = app_cli::available()
        .apps
        .into_iter()
        .find(|app| app.package_id == APP_ID)
        .expect("the app is offered");
    assert_eq!(offer.runtime.as_deref(), Some(needed.as_str()));
    let error = app_cli::install::install_app(&offer).expect_err("no runtime, no install");
    assert!(
        matches!(
            error,
            app_cli::install::InstallError::RuntimeUnavailable { .. }
        ),
        "{error:?}"
    );
    let message = error.user_message();
    assert!(
        message.contains(&needed) && message.contains("nothing was changed"),
        "{message}"
    );
    assert!(package_store::list_installed().unwrap().is_empty());
    assert!(!app_cli::runtime_installed(&needed));
    let absent = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .args(["runtime", "install", &needed, "--repo", "local"])
        .output_locked()
        .unwrap();
    assert!(
        !absent.status.success(),
        "unoffered runtime cannot be installed"
    );
    assert!(package_store::list_installed().unwrap().is_empty());

    let runtime_full = runtime_tapp(&registry_bundle, &needed, "0.1.7");
    let objects = root.join("objects");
    std::fs::create_dir_all(&objects).unwrap();
    let split = tilo_app_package::chunks::split(runtime_full.as_slice(), &objects).unwrap();
    assert!(split.chunked_blobs >= 1, "the toolkit travels in chunks");
    let thin = tilo_app_package::tapp::to_bytes(&split.meta, None).unwrap();
    assert!(
        thin.len() < 1024 * 1024,
        "the runtime package itself is metadata"
    );
    for object in std::fs::read_dir(&objects).unwrap() {
        let object = object.unwrap().path();
        let name = object.file_name().unwrap().to_str().unwrap().to_string();
        let served_as = name
            .strip_suffix(".blob")
            .map_or_else(|| format!("/blobs/{name}"), |hex| format!("/blobs/{hex}"));
        served.put(&served_as, std::fs::read(&object).unwrap());
    }
    served.put("/runtime.tapp", thin.clone());
    let runtime_entry = entry(
        registry_types::NATIVE_RUNTIME_PACKAGE_ID,
        "0.1.7",
        &format!("{base}/runtime.tapp"),
        &thin,
        &needed,
    );
    let second = snapshot(vec![app_entry, runtime_entry], &owner, Some(&first));
    served.put("/entries", serde_json::to_vec(&second).unwrap());

    let explicit = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .args(["runtime", "install", &needed, "--repo", "local"])
        .output_locked()
        .unwrap();
    assert!(
        explicit.status.success(),
        "explicit runtime install: {}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(
        package_store::list_installed()
            .unwrap()
            .iter()
            .all(|row| row.id != APP_ID),
        "installing the engine must not replace or install its app"
    );

    let installed_runtime = package_store::list_installed()
        .unwrap()
        .into_iter()
        .find(|row| row.id == registry_types::NATIVE_RUNTIME_PACKAGE_ID)
        .expect("the verified runtime installation must publish its canonical package identity");
    assert_eq!(installed_runtime.version, "0.1.7");
    assert_eq!(installed_runtime.package_id_hex, split.meta.id.to_hex());
    assert_eq!(
        installed_runtime.publisher_key_hex,
        hex::encode(publisher().verifying_key().to_bytes())
    );
    assert!(installed_runtime.manifest_path.is_file());
    assert!(installed_runtime.payload_root.join("bundle.toml").is_file());

    assert!(
        package_store::integrity::verify(&installed_runtime, &installed_runtime.payload_root,)
            .unwrap()
            .is_intact()
    );
    let repeat = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .args(["runtime", "install", &needed, "--repo", "local"])
        .output_locked()
        .unwrap();
    assert!(
        repeat.status.success(),
        "{}",
        String::from_utf8_lossy(&repeat.stderr)
    );

    let listing = app_cli::available();
    assert!(
        !listing
            .apps
            .iter()
            .any(|app| registry_types::is_runtime_package_id(&app.package_id)),
        "a runtime is not listed as an app: {:?}",
        listing.apps
    );
    let offer = listing
        .apps
        .into_iter()
        .find(|app| app.package_id == APP_ID)
        .unwrap();
    let entries_before = served.hits("/entries");
    let found = app_cli::runtime_offer(&needed, "local")
        .expect("the offer is found")
        .expect("the registry offers the runtime");
    assert_eq!(found.package_id, registry_types::NATIVE_RUNTIME_PACKAGE_ID);
    assert_eq!(
        served.hits("/entries") - entries_before,
        1,
        "the registry snapshot is fetched once to find the runtime"
    );
    app_cli::install::install_app(&offer)
        .unwrap_or_else(|error| panic!("install must fetch the runtime: {}", error.user_message()));
    let fetched = runtimes.join(&registry_version);
    let verified = runtime_bundle::verify_bundle(&fetched).expect("fetched runtime verifies");
    assert_eq!(verified.version, registry_version);
    assert_eq!(
        std::fs::read(fetched.join("lib/libruntime_dylib.so")).unwrap(),
        toolkit
    );
    assert!(app_cli::runtime_installed(&needed));
    assert!(
        runtimes.join(&device_version).join("bundle.toml").exists(),
        "the device's own runtime stays for the apps that link it"
    );
    let apps_now = package_store::list_installed().unwrap();
    let linked = apps_now
        .iter()
        .find(|row| row.id == APP_ID)
        .expect("the app is installed");
    assert_eq!(linked.version, "2.0.0");

    // The Browser pins its engine runtime by exact version. Whatever installs
    // the Browser brings that engine first; where the registry offers none,
    // the install is refused and the working state stays.
    let engine_id = "so.tilo.runtime.engine";
    let browser_id = "so.tilo.engine-e2e";
    let (engine_version, engine_bundle) =
        runtime_bundle(root, "engine", b"the engine build", &identity_path);
    let engine = format!("tilo-runtime/{engine_version}");
    let browser = app_tapp_pinning(
        root,
        browser_id,
        &format!("tilo-runtime/{device_version}"),
        Some(&engine),
    );
    served.put("/browser.tapp", browser.clone());
    let browser_entry = entry(
        browser_id,
        "2.0.0",
        &format!("{base}/browser.tapp"),
        &browser,
        &format!("tilo-runtime/{device_version}"),
    );
    let current = &second;
    let mut entries = current.entries.clone();
    entries.push(browser_entry);
    let third = snapshot(entries.clone(), &owner, Some(current));
    served.put("/entries", serde_json::to_vec(&third).unwrap());
    let offer = app_cli::available()
        .apps
        .into_iter()
        .find(|app| app.package_id == browser_id)
        .expect("the browser is offered");
    let error = app_cli::install::install_app(&offer).expect_err("no engine on offer, no install");
    assert!(
        matches!(error, app_cli::install::InstallError::Refused { .. }),
        "{error:?}"
    );
    assert!(
        error.user_message().contains(&engine),
        "{}",
        error.user_message()
    );
    assert!(
        package_store::list_installed()
            .unwrap()
            .iter()
            .all(|row| row.id != browser_id),
        "a refused install leaves nothing behind"
    );
    assert!(!app_cli::runtime_installed(&engine));

    let engine_full = runtime_package_tapp(&engine_bundle, engine_id, &engine, "0.1.7");
    let engine_objects = root.join("engine-objects");
    std::fs::create_dir_all(&engine_objects).unwrap();
    let split = tilo_app_package::chunks::split(engine_full.as_slice(), &engine_objects).unwrap();
    let engine_thin = tilo_app_package::tapp::to_bytes(&split.meta, None).unwrap();
    for object in std::fs::read_dir(&engine_objects).unwrap() {
        let object = object.unwrap().path();
        let name = object.file_name().unwrap().to_str().unwrap().to_string();
        let served_as = name
            .strip_suffix(".blob")
            .map_or_else(|| format!("/blobs/{name}"), |hex| format!("/blobs/{hex}"));
        served.put(&served_as, std::fs::read(&object).unwrap());
    }
    served.put("/engine.tapp", engine_thin.clone());
    entries.push(entry(
        engine_id,
        "0.1.7",
        &format!("{base}/engine.tapp"),
        &engine_thin,
        &engine,
    ));
    let fourth = snapshot(entries, &owner, Some(&third));
    served.put("/entries", serde_json::to_vec(&fourth).unwrap());
    let offer = app_cli::available()
        .apps
        .into_iter()
        .find(|app| app.package_id == browser_id)
        .unwrap();
    app_cli::install::install_app(&offer).unwrap_or_else(|error| {
        panic!(
            "the Browser must bring its engine: {}",
            error.user_message()
        )
    });
    assert!(
        app_cli::runtime_installed(&engine),
        "the engine came with the Browser"
    );
    assert!(
        package_store::list_installed()
            .unwrap()
            .iter()
            .any(|row| row.id == browser_id)
    );

    let (new_version, new_bundle) = runtime_bundle(root, "newer", b"new toolkit", &identity_path);
    let new_needed = format!("tilo-runtime/{new_version}");
    let new_tapp = runtime_tapp(&new_bundle, &new_needed, "0.1.8");
    let new_root = app_registry::tapp::load(&new_tapp)
        .unwrap()
        .package_id
        .to_hex();
    served.put("/new-runtime.tapp", new_tapp.clone());
    let newer = snapshot(
        vec![entry(
            registry_types::NATIVE_RUNTIME_PACKAGE_ID,
            "0.1.8",
            &format!("{base}/new-runtime.tapp"),
            &new_tapp,
            &new_needed,
        )],
        &owner,
        Some(&fourth),
    );
    served.put("/entries", serde_json::to_vec(&newer).unwrap());
    let update = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .args(["runtime", "install", &new_needed, "--repo", "local"])
        .output_locked()
        .unwrap();
    assert!(
        update.status.success(),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
    let rows = package_store::list_installed().unwrap();
    let runtime = rows
        .iter()
        .find(|row| row.id == registry_types::NATIVE_RUNTIME_PACKAGE_ID)
        .unwrap();
    assert_eq!(runtime.version, "0.1.8");
    assert_eq!(runtime.package_id_hex, new_root);
    assert_eq!(
        runtime.publisher_key_hex,
        installed_runtime.publisher_key_hex
    );
    runtime_bundle::verify_bundle(&runtimes.join(&new_version)).unwrap();
    runtime_bundle::verify_bundle(&runtimes.join(&registry_version)).unwrap();
    assert_eq!(
        std::fs::read(runtimes.join(&new_version).join("lib/libruntime_dylib.so")).unwrap(),
        b"new toolkit"
    );

    std::fs::write(
        runtimes.join(&new_version).join("lib/libruntime_dylib.so"),
        b"tampered",
    )
    .unwrap();
    let damaged = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .args(["runtime", "install", &new_needed, "--repo", "local"])
        .output_locked()
        .unwrap();
    assert!(
        damaged.status.success(),
        "a damaged retained runtime is installed again: {}",
        String::from_utf8_lossy(&damaged.stderr)
    );
    runtime_bundle::verify_bundle(&runtimes.join(&new_version)).unwrap();
    assert_eq!(
        std::fs::read(runtimes.join(&new_version).join("lib/libruntime_dylib.so")).unwrap(),
        b"new toolkit",
        "the damaged file is the one the install replaced"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one device and registry exercise the engine's install with the app, its verification failures and the kept working app together"
)]
fn the_engine_installs_with_the_app_and_a_failed_fetch_keeps_the_working_app() {
    let root = tempfile::tempdir().unwrap();
    let owner = SigningKey::from_bytes(&[22_u8; 32]);
    let identity = runtime_bundle::Identity::generate();
    let identity_path = root.path().join("runtime-publisher.json");
    std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
    let trust = root.path().join("trusted.txt");
    std::fs::write(&trust, format!("{}\n", identity.public_key_hex())).unwrap();
    let (device_version, device_bundle) =
        runtime_bundle(root.path(), "device", b"base toolkit", &identity_path);
    let (engine_version, engine_bundle) =
        runtime_bundle(root.path(), "engine", b"engine toolkit", &identity_path);
    let device_ref = format!("tilo-runtime/{device_version}");
    let engine_ref = format!("tilo-runtime/{engine_version}");
    let runtimes = root.path().join("runtimes");
    std::fs::create_dir(&runtimes).unwrap();
    std::fs::rename(device_bundle, runtimes.join(&device_version)).unwrap();
    runtime_bundle::gc::BundleRoot::new(&runtimes)
        .pin(&device_version)
        .unwrap();

    let served = Served::default();
    let base = format!("http://127.0.0.1:{}", served.start());
    let baseline = app_tapp(root.path(), &device_ref, &[]);
    let app = app_tapp(root.path(), &device_ref, &[&engine_ref]);
    let runtime = runtime_tapp(&engine_bundle, &engine_ref, "0.1.7");
    served.put("/app.tapp", app.clone());
    served.put("/engine.tapp", runtime.clone());
    let entries = vec![
        entry(
            APP_ID,
            "2.0.0",
            &format!("{base}/app.tapp"),
            &app,
            &device_ref,
        ),
        entry(
            registry_types::NATIVE_RUNTIME_PACKAGE_ID,
            "0.1.7",
            &format!("{base}/engine.tapp"),
            &runtime,
            &engine_ref,
        ),
    ];
    served.put("/baseline.tapp", baseline.clone());
    let first = snapshot(
        vec![entry(
            APP_ID,
            "2.0.0",
            &format!("{base}/baseline.tapp"),
            &baseline,
            &device_ref,
        )],
        &owner,
        None,
    );
    served.put("/entries", serde_json::to_vec(&first).unwrap());
    let data = tempfile::tempdir().unwrap();
    let device = Device::new(
        Some(registry_config("local", &base, &owner)),
        &[
            ("TILO_RUNTIME_BUNDLE_DIR", &runtimes),
            ("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust),
            ("XDG_DATA_HOME", data.path()),
        ],
    );
    let _daemon = Daemon::start(device.apps.path());
    let offer = app_cli::available()
        .apps
        .into_iter()
        .find(|app| app.package_id == APP_ID)
        .unwrap();
    app_cli::install::install_app(&offer).unwrap();
    let working = package_store::list_installed()
        .unwrap()
        .into_iter()
        .find(|row| row.id == APP_ID)
        .unwrap();
    let assert_working = || {
        let current = package_store::list_installed()
            .unwrap()
            .into_iter()
            .find(|row| row.id == APP_ID)
            .unwrap();
        assert_eq!(current.version, working.version);
        assert_eq!(current.package_id_hex, working.package_id_hex);
        assert!(
            package_store::integrity::verify(&current, &current.payload_root)
                .unwrap()
                .is_intact()
        );
        assert!(app_cli::runtime_installed(&device_ref));
    };

    let without_engine = snapshot(vec![entries[0].clone()], &owner, Some(&first));
    served.put("/entries", serde_json::to_vec(&without_engine).unwrap());
    let offer = app_cli::all_offers()
        .apps
        .into_iter()
        .find(|app| app.package_id == APP_ID)
        .unwrap();
    for replace in [
        app_cli::install::update_app_reporting,
        app_cli::install::rollback_app_reporting,
    ] {
        let refused = replace(&offer, &mut |_| {});
        assert!(
            matches!(refused, Err(app_cli::install::InstallError::Refused { .. })),
            "missing pinned engine must refuse replacement: {refused:?}"
        );
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.user_message().contains(&engine_ref)),
            "the refusal names the engine: {refused:?}"
        );
        assert_working();
    }
    let with_engine = snapshot(entries, &owner, Some(&without_engine));
    served.put("/entries", serde_json::to_vec(&with_engine).unwrap());

    // A signed offer is not enough: corrupt artifact bytes must still fail.
    served.put("/engine.tapp", b"corrupt archive".to_vec());
    assert!(!app_cli::runtime_installed(&engine_ref));
    assert!(app_cli::install::update_app_reporting(&offer, &mut |_| {}).is_err());
    assert!(app_cli::install::rollback_app_reporting(&offer, &mut |_| {}).is_err());
    assert_working();

    std::fs::write(
        &trust,
        format!(
            "{}\n",
            runtime_bundle::Identity::generate().public_key_hex()
        ),
    )
    .unwrap();
    served.put("/engine.tapp", runtime);
    assert!(app_cli::install::update_app_reporting(&offer, &mut |_| {}).is_err());
    assert!(app_cli::install::rollback_app_reporting(&offer, &mut |_| {}).is_err());
    assert_working();
    std::fs::write(&trust, format!("{}\n", identity.public_key_hex())).unwrap();
    let mut progress = Vec::new();
    app_cli::install::install_app_reporting(&offer, &mut |event| {
        // Until the engine is there the app that works keeps working; the install brings both.
        if !app_cli::runtime_installed(&engine_ref) {
            assert_working();
        }
        progress.push(event);
    })
    .unwrap();
    assert!(
        app_cli::runtime_installed(&engine_ref),
        "the engine came with the app"
    );
    assert_ne!(
        package_store::list_installed()
            .unwrap()
            .into_iter()
            .find(|row| row.id == APP_ID)
            .unwrap()
            .package_id_hex,
        working.package_id_hex
    );
    app_cli::install::update_app_reporting(&offer, &mut |_| {}).unwrap();
    app_cli::install::rollback_app_reporting(&offer, &mut |_| {}).unwrap();
    assert!(
        !progress.is_empty(),
        "automatic engine acquisition reports real progress"
    );
    assert!(app_cli::runtime_installed(&engine_ref));
    assert!(app_cli::runtime_installed(&device_ref));
}

/// What a child of this test writes when it builds against the edited library: where the build
/// lands. The child is this test run again, so its process compiles with that library alone.
const EDITED_BUILD: &str = "TILO_TEST_EDITED_STD_BUILD";

const EDITED_APP_ID: &str = "so.tilo.edited";

/// A package that says what the standard library makes of a reaction.
const EDITED_APP: &str = r#"app so.tilo.edited "Edited library"

/// What the window shows.
type Board {
    name: Text = ""
}

root Board

default view board for Board

/// The board's window: the reactions the library says.
view board(b: Board) {
    ui.Text(std.reactions_said(["x:1f"]))
}
"#;

/// The standard library with an edit: its reactions say so. Built against it, a package records
/// another library than the one this build carries, and runs only on the runtime that carries it.
fn edit_library(dir: &Path) {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lang/tilolang/std");
    std::fs::create_dir_all(dir).unwrap();
    let source = std::fs::read_to_string(std.join("app.tl")).unwrap();
    let said = "    each_said(keys.map(k -> emoji_of(k)), 0, [])\n";
    assert!(source.contains(said), "the library says its reactions");
    std::fs::write(
        dir.join("app.tl"),
        source.replace(
            said,
            "    \"edited: {each_said(keys.map(k -> emoji_of(k)), 0, [])}\"\n",
        ),
    )
    .unwrap();
    std::fs::copy(std.join("app.ids"), dir.join("app.ids")).unwrap();
}

/// In the child: compile the edited library and the package against it, as `tilolang build` and
/// `tilolang-std` do in a checkout that holds the edit, into `out`: the package's payload, the
/// library's module and its digest.
fn build_against_edited_library(out: &Path) {
    tilolang::stdlib::load_from(&out.join("std")).unwrap();
    let source = out.join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("app.tl"), EDITED_APP).unwrap();
    let bundles = tilolang_tilo::bundle::compiled(&source, &[]).unwrap();
    let app = bundles.last().unwrap();
    assert_eq!(app.meta.std, tilolang::stdlib::digest());
    app.write_installed(&out.join("payload")).unwrap();
    std::fs::write(
        out.join("tilo.std.wasm"),
        &tilolang::stdlib::compiled().wasm,
    )
    .unwrap();
    std::fs::write(out.join("digest"), tilolang::stdlib::digest()).unwrap();
}

/// The signed package of the tilolang app whose payload is `payload`, as `tilolang build` writes
/// its manifest: it runs on the device's Tilo runtime `runtime` and executes the tilolang runtime
/// `tilolang`, which it requires.
fn tilolang_app_tapp(payload: &Path, runtime: &str, tilolang: &str) -> Vec<u8> {
    tapp(
        &format!(
            r#"
[package]
id = "{EDITED_APP_ID}"
version = "1.0.0"
kind = "app"
name = "Edited library"
runtime = "{runtime}"
optional-runtimes = ["{tilolang}"]

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
runtime = "{tilolang}"
bin = "bin/tilolang-process"
type = "gui"
"#,
            isa = manifest_isa(),
            key = hex::encode(publisher().verifying_key().to_bytes()),
        ),
        payload,
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one device and registry: the build, the install that fetches the runtime and the open"
)]
fn a_package_built_against_another_library_fetches_the_runtime_that_carries_it_and_opens() {
    if let Some(out) = std::env::var_os(EDITED_BUILD) {
        build_against_edited_library(Path::new(&out));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let owner = SigningKey::from_bytes(&[22_u8; 32]);

    // A checkout whose library has an edit builds the package, in a process of its own: this one
    // keeps the library the device's own tilolang runtime carries.
    let built = root.join("edited");
    edit_library(&built.join("std"));
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime_fetch::a_package_built_against_another_library_fetches_the_runtime_that_carries_it_and_opens",
            "--nocapture",
        ])
        .env(EDITED_BUILD, &built)
        .output_locked()
        .unwrap();
    assert!(
        child.status.success(),
        "the package builds against the edited library: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let edited = std::fs::read_to_string(built.join("digest")).unwrap();
    assert_ne!(
        edited,
        tilolang::stdlib::digest(),
        "the edit is another library"
    );
    let needed = tilolang_tilo::runtime_of(&edited);
    assert_ne!(needed, tilolang_tilo::runtime());
    let package_bytes = |dir: &Path| {
        let mut size = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            size += entry.metadata().unwrap().len();
        }
        size
    };
    assert!(
        !built.join("payload/imports").exists()
            && package_bytes(&built.join("payload")) < 64 * 1024,
        "the package carries no library of its own"
    );

    let runtime_identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-publisher.json");
    std::fs::write(&identity_path, runtime_identity.to_json_bytes()).unwrap();
    let trust = root.join("trusted-runtime-publishers.txt");
    std::fs::write(&trust, format!("{}\n", runtime_identity.public_key_hex())).unwrap();
    let (device_version, device_bundle) = runtime_bundle(
        root,
        "device",
        b"toolkit the image was built with",
        &identity_path,
    );
    let runtimes = root.join("runtimes");
    std::fs::create_dir_all(&runtimes).unwrap();
    std::fs::rename(&device_bundle, runtimes.join(&device_version)).unwrap();
    runtime_bundle::gc::BundleRoot::new(&runtimes)
        .pin(&device_version)
        .unwrap();
    let device_runtime = format!("tilo-runtime/{device_version}");

    // The tilolang runtime of the edited library, as the packing bakes it: the host binary and
    // the library, signed, offered by the registry as a runtime package.
    let host = root.join("tilolang-process");
    std::fs::write(&host, b"#!/bin/true\n").unwrap();
    std::fs::set_permissions(&host, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let edited_bundle = root.join("tilolang-edited");
    runtime_bundle::build_binaries_bundle(
        &edited_bundle,
        needed.split_once('/').unwrap().1,
        None,
        Some(&identity_path),
        &[host],
        &[built.join("tilo.std.wasm")],
    )
    .unwrap();
    let served = Served::default();
    let base = format!("http://127.0.0.1:{}", served.start());
    let app = tilolang_app_tapp(&built.join("payload"), &device_runtime, &needed);
    let runtime =
        runtime_package_tapp(&edited_bundle, "so.tilo.runtime.tilolang", &needed, "0.1.0");
    served.put("/edited.tapp", app.clone());
    served.put("/tilolang.tapp", runtime.clone());
    let entries = vec![
        entry(
            EDITED_APP_ID,
            "1.0.0",
            &format!("{base}/edited.tapp"),
            &app,
            &device_runtime,
        ),
        entry(
            "so.tilo.runtime.tilolang",
            "0.1.0",
            &format!("{base}/tilolang.tapp"),
            &runtime,
            &needed,
        ),
    ];
    served.put(
        "/entries",
        serde_json::to_vec(&snapshot(entries, &owner, None)).unwrap(),
    );
    let data = tempfile::tempdir().unwrap();
    let device = Device::new(
        Some(registry_config("local", &base, &owner)),
        &[
            ("TILO_RUNTIME_BUNDLE_DIR", &runtimes),
            ("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust),
            ("XDG_DATA_HOME", data.path()),
        ],
    );
    let _daemon = Daemon::start(device.apps.path());
    assert!(
        !app_cli::runtime_installed(&needed),
        "the device lacks the edited library"
    );

    // The install fetches the runtime the package names, as it fetches any it requires.
    let listing = app_cli::available();
    let offer = listing
        .apps
        .iter()
        .find(|app| app.package_id == EDITED_APP_ID)
        .unwrap_or_else(|| {
            panic!(
                "the app is offered: {:?}, unreachable {:?}",
                listing
                    .apps
                    .iter()
                    .map(|app| &app.package_id)
                    .collect::<Vec<_>>(),
                listing.unreachable
            )
        })
        .clone();
    app_cli::install::install_app(&offer).unwrap();
    assert!(
        app_cli::runtime_installed(&needed),
        "the runtime came with the app"
    );

    // Opened as the device opens it: the installation verified as a launch verifies it, its
    // module beside the library the runtime carries, and its window drawn on that library.
    let installed = package_store::find_installed(EDITED_APP_ID)
        .unwrap()
        .unwrap();
    let instance = launcher::run::tilolang_process::verified_instance(
        &installed,
        &installed.manifest_path,
        &installed.payload_root,
        &installed.package_id_hex,
        &tilo_app_package::RevocationList::default(),
    )
    .unwrap();
    assert_eq!(instance.runtime, needed);
    let module = instance.module;
    let bundle =
        tilolang_tilo::bundle::Bundle::from_parts(module.wasm, module.ids, &module.imports)
            .unwrap();
    let library = bundle
        .imports
        .first()
        .expect("the runtime's library comes first");
    assert_eq!(library.meta.app, tilolang::stdlib::APP);
    assert_eq!(library.meta.std, edited);
    let viewer =
        tilolang_tilo::runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    let window = tilolang_tilo::host::Host::new(viewer.clone(), || None).unwrap();
    window.load_bundle(&bundle).unwrap();
    window.root_of(EDITED_APP_ID).unwrap();
    let view = window.open_main(EDITED_APP_ID).unwrap();
    let shown = format!("{:?}", view.tree());
    assert!(
        shown.contains("edited: x 1"),
        "the window runs the edited library's code: {shown}"
    );

    // A host that has only the library this build carries refuses the package, and says why.
    let carried = tilolang_tilo::bundle::Bundle::read(&installed.payload_root).unwrap();
    let elsewhere = tilolang_tilo::host::Host::new(viewer, || None).unwrap();
    let refused = elsewhere
        .load_bundle(&carried)
        .map(drop)
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains(&format!("compiled against the standard library {edited}"))
            && refused.contains(tilolang::stdlib::digest()),
        "{refused}"
    );
}
