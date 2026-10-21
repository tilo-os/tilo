//! End to end: a registry build linked against a runtime build this device
//! does not have.
//!
//! The device holds one runtime build (as an image holds the one it was
//! built with); the registry offers an app linked against another. Without
//! that build on offer, `app install` refuses and changes nothing. Once the
//! registry offers the build as `so.tilo.runtime` — a thin runtime package
//! whose libraries arrive as content-addressed, chunked payload objects —
//! the install fetches the runtime first, verifies it as the launcher does,
//! and then installs the app. The served objects are produced by the same
//! `split` the release pipeline runs, and the registry stand-in serves them
//! the way the registry does (`/blobs/<hex>` and `/blobs/<hex>.chunks`).

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
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

use crate::registry_to_installed::{native_target, scoped_env, tilo_install_bin};

const APP_ID: &str = "so.tilo.linked-e2e";

/// What the stand-in registry serves, by request path.
#[derive(Clone, Default)]
struct Served(
    Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    Arc<Mutex<Vec<String>>>,
);

impl Served {
    /// How many requests have asked for `path` so far.
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

fn app_tapp(root: &Path, runtime: &str, optional: &[&str]) -> Vec<u8> {
    app_tapp_with_optional(root, APP_ID, runtime, optional)
}

/// An app on `runtime` that also pins `optional` (an engine) by exact version.
fn app_tapp_pinning(root: &Path, id: &str, runtime: &str, optional: Option<&str>) -> Vec<u8> {
    let optional: Vec<&str> = optional.into_iter().collect();
    app_tapp_with_optional(root, id, runtime, &optional)
}

fn app_tapp_with_optional(root: &Path, id: &str, runtime: &str, optional: &[&str]) -> Vec<u8> {
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

    // The runtime publisher the device trusts, as an image does.
    let runtime_identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-publisher.json");
    std::fs::write(&identity_path, runtime_identity.to_json_bytes()).unwrap();
    let trust = root.join("trusted-runtime-publishers.txt");
    std::fs::write(&trust, format!("{}\n", runtime_identity.public_key_hex())).unwrap();

    // The device's runtime: the build its system apps were linked with.
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

    // The registry's runtime: another build, large enough to travel chunked.
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

    let config = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "local".into(),
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
        ("TILO_RUNTIME_BUNDLE_DIR", &runtimes),
        ("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust),
        ("XDG_DATA_HOME", data.path()),
    ]);
    assert!(app_cli::runtime_installed(&format!(
        "tilo-runtime/{device_version}"
    )));
    assert!(!app_cli::runtime_installed(&needed));

    // Phase `without_its_runtime_on_offer_the_app_is_refused_and_nothing_changes`.
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
        .output()
        .unwrap();
    assert!(
        !absent.status.success(),
        "unoffered runtime cannot be installed"
    );
    assert!(package_store::list_installed().unwrap().is_empty());

    // Phase `with_its_runtime_on_offer_the_runtime_is_fetched_first`.
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
        .output()
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
        .output()
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
    // Finding the runtime's offer verifies one snapshot, however many runtime
    // packages it lists; it is not downloaded again per package.
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

    // Phase `an_app_pinning_an_engine_installs_with_it_or_not_at_all`.
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
        matches!(
            error,
            app_cli::install::InstallError::RuntimeUnavailable { .. }
        ),
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

    // The registry offers the engine: installing the Browser fetches it first.
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

    // A subsequent runtime package update must materialize its provided bundle,
    // not merely replace the package row while leaving its libraries absent.
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
        .output()
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

    // A catalogued package plus a damaged retained bundle is not installed
    // successfully: the real same-version update must refuse, not trust presence.
    std::fs::write(
        runtimes.join(&new_version).join("lib/libruntime_dylib.so"),
        b"tampered",
    )
    .unwrap();
    let damaged = std::process::Command::new(tilo_test_env::package_binary!("app"))
        .args(["runtime", "install", &new_needed, "--repo", "local"])
        .output()
        .unwrap();
    assert!(
        !damaged.status.success(),
        "damaged retained runtime must not count as a completed install"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one device and registry exercise automatic acquisition, verification failure and retry together"
)]
fn pinned_engine_is_automatic_and_failures_preserve_the_working_app() {
    let root = tempfile::tempdir().unwrap();
    let owner = SigningKey::from_bytes(&[22_u8; 32]);
    let identity = runtime_bundle::Identity::generate();
    let identity_path = root.path().join("runtime-publisher.json");
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
    let base = format!("http://127.0.0.1:{}", served.start());
    let baseline = app_tapp(root.path(), &device_ref, &[]);
    let app = app_tapp(root.path(), &device_ref, &[&optional_ref]);
    let runtime = runtime_tapp(&optional_bundle, &optional_ref, "0.1.7");
    served.put("/app.tapp", app.clone());
    served.put("/optional.tapp", runtime.clone());
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
            &format!("{base}/optional.tapp"),
            &runtime,
            &optional_ref,
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
    let config = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("registries.toml"),
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
    let installer = tilo_install_bin();
    let _env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", config.path()),
        ("TILO_STORE_CACHE_DIR", cache.path()),
        ("TILO_APPS_DATA_DIR", apps.path()),
        ("TILO_INSTALL_BIN", &installer),
        ("TILO_RUNTIME_BUNDLE_DIR", &runtimes),
        ("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust),
        ("XDG_DATA_HOME", data.path()),
    ]);
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

    // Missing pinned engines refuse both replacement paths without changing
    // the working app, even though its replacement archive is available.
    let without_engine = snapshot(vec![entries[0].clone()], &owner, Some(&first));
    served.put("/entries", serde_json::to_vec(&without_engine).unwrap());
    // Update an installed app using the complete catalogue, not the listing
    // that deliberately excludes already-installed apps.
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
            matches!(
                refused,
                Err(app_cli::install::InstallError::RuntimeUnavailable { .. })
            ),
            "missing pinned engine must refuse replacement: {refused:?}"
        );
        assert_working();
    }
    let with_engine = snapshot(entries, &owner, Some(&without_engine));
    served.put("/entries", serde_json::to_vec(&with_engine).unwrap());

    let missing = app_cli::install::acquire_optional_runtime_reporting(
        &offer,
        "tilo-runtime/not-offered",
        &mut |_| {},
    )
    .expect_err("a missing offer is an actionable error");
    assert!(matches!(
        missing,
        app_cli::install::InstallError::OptionalRuntimeUnavailable { .. }
    ));
    assert!(missing.user_message().contains("tilo-runtime/not-offered"));

    // A signed offer is not enough: corrupt artifact bytes must still fail.
    served.put("/optional.tapp", b"corrupt archive".to_vec());
    assert!(
        app_cli::install::acquire_optional_runtime_reporting(&offer, &optional_ref, &mut |_| {},)
            .is_err()
    );
    assert!(!app_cli::runtime_installed(&optional_ref));
    assert!(app_cli::install::update_app_reporting(&offer, &mut |_| {}).is_err());
    assert!(app_cli::install::rollback_app_reporting(&offer, &mut |_| {}).is_err());
    assert_working();

    // The runtime's signer is checked independently of the registry publisher.
    std::fs::write(
        &trust,
        format!(
            "{}\n",
            runtime_bundle::Identity::generate().public_key_hex()
        ),
    )
    .unwrap();
    served.put("/optional.tapp", runtime);
    assert!(
        app_cli::install::acquire_optional_runtime_reporting(&offer, &optional_ref, &mut |_| {},)
            .is_err()
    );
    assert!(!app_cli::runtime_installed(&optional_ref));
    assert!(app_cli::install::update_app_reporting(&offer, &mut |_| {}).is_err());
    assert!(app_cli::install::rollback_app_reporting(&offer, &mut |_| {}).is_err());
    assert_working();
    std::fs::write(&trust, format!("{}\n", identity.public_key_hex())).unwrap();
    let mut progress = Vec::new();
    let mut engine_ready_before_replacement = false;
    app_cli::install::install_app_reporting(&offer, &mut |event| {
        // Progress is delivered before each installer invocation. Observing
        // the verified engine while the old app is intact proves ordering,
        // rather than only checking that both exist after the operation.
        if !app_cli::runtime_installed(&optional_ref) {
            assert_working();
        } else if matches!(
            event.phase,
            app_cli::install::InstallPhase::Installing { elapsed } if elapsed.is_zero()
        ) {
            assert_working();
            engine_ready_before_replacement = true;
        }
        progress.push(event);
    })
    .unwrap();
    assert!(engine_ready_before_replacement);
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
    assert!(app_cli::runtime_installed(&optional_ref));
    assert!(app_cli::runtime_installed(&device_ref));
    let requested = served.1.lock().unwrap().len();
    app_cli::install::acquire_optional_runtime_reporting(&offer, &optional_ref, &mut |_| {})
        .unwrap();
    assert_eq!(
        served.1.lock().unwrap().len(),
        requested,
        "retry reuses verified installation"
    );
}
