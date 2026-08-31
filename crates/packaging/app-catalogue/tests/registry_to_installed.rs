//! The whole path, on real code: a signed `.qpkg` published to a real
//! HTTP registry, listed as available, fetched, verified by
//! `qos-install`, and present in the registry the desktop enumerates
//! installed apps from.
//!
//! Nothing here is mocked: real socket, real signed package, real
//! `qos-install` child process. The assertion that matters:
//! `launcher::registry` (the compositor's own reader) returns the app.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use app_catalogue::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;
use pkg::pack::{SigningIdentity, pack};
use pkg::{Manifest, MemoryBlobStore};
use registry_types::{RegistryEntry, RegistryVersion, sign_entry};

/// The .qpkg-authoring the local test registry does, shared verbatim
/// so tests hold the served package to its real contract.
#[path = "../examples/support/authoring.rs"]
mod authoring;

/// Every test in this file repoints process-global environment
/// variables, so they cannot run concurrently.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Locate the `qos-install` binary this workspace builds. A missing
/// binary **fails** the test rather than skipping it — a check that
/// can't reach its subject proves nothing, and a silent skip would read as a pass.
fn qos_install_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("workspace root is three levels above this crate");
    for profile in ["debug", "release"] {
        let candidate = root.join("target").join(profile).join("qos-install");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!(
        "qos-install is not built under {}/target/{{debug,release}}. \
         This test installs a real package with the real installer; \
         build it first with `cargo build -p install`.",
        root.display()
    );
}

/// Author a signed .qpkg for `package_id`, declaring a GUI entrypoint so the desktop would treat it as launchable.
fn author_qpkg(package_id: &str, identity: &SigningIdentity) -> Vec<u8> {
    let pubhex = hex::encode(identity.public_key_bytes());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin").join("app"), b"#!/bin/true\nqos app\n").unwrap();
    let manifest_toml = format!(
        r#"
[package]
id = "{package_id}"
version = "1.4.0"
kind = "app"
name = "Sheets"
category = "Office"

[publisher]
identity = "ed25519:{pubhex}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "qos-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#
    );
    let manifest = Manifest::from_toml_str(&manifest_toml).unwrap();
    let mut store = MemoryBlobStore::new();
    let id = pack(&payload, &manifest, identity, &mut store).unwrap();
    app_registry::qpkg::write_bytes(id, &store).unwrap()
}

/// A signed registry entry pointing at `qpkg_url`.
fn signed_entry(package_id: &str, qpkg_url: &str, qpkg: &[u8]) -> RegistryEntry {
    let key = SigningKey::from_bytes(&[11u8; 32]);
    let mut entry = RegistryEntry {
        package_id: package_id.to_string(),
        name: "Sheets".into(),
        description: "A spreadsheet".into(),
        categories: vec!["Office".into()],
        icon_url: None,
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "1.4.0".into(),
            qpkg_url: qpkg_url.to_string(),
            qpkg_root_hash_hex: hex::encode([0u8; 32]),
            qpkg_size_bytes: qpkg.len() as u64,
            released_at_unix: 1,
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &key);
    entry
}

/// A minimal real HTTP server: GET /entries returns the catalogue JSON, GET /<name>.qpkg returns the package bytes.
struct Registry {
    port: u16,
    _thread: std::thread::JoinHandle<()>,
}

impl Registry {
    fn start(entries_json: String, qpkg: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let port = listener.local_addr().unwrap().port();
        let entries = Arc::new(entries_json);
        let bytes = Arc::new(qpkg);
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let entries = Arc::clone(&entries);
                let bytes = Arc::clone(&bytes);
                serve_one(stream, &entries, &bytes);
            }
        });
        Self {
            port,
            _thread: thread,
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn serve_one(mut stream: TcpStream, entries: &str, qpkg: &[u8]) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain headers so the client sees a clean response.
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let is_qpkg = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("qpkg"));
    let (status, content_type, body): (&str, &str, Vec<u8>) = if path.starts_with("/entries") {
        ("200 OK", "application/json", entries.as_bytes().to_vec())
    } else if is_qpkg {
        ("200 OK", "application/octet-stream", qpkg.to_vec())
    } else {
        ("404 Not Found", "text/plain", b"no".to_vec())
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// Point every relevant environment variable at scratch directories, restoring them on drop.
struct Env {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Env {
    fn set(vars: &[(&'static str, &Path)]) -> Self {
        let lock = env_lock();
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var_os(k)));
            // SAFETY: serialised by `env_lock`.
            unsafe { std::env::set_var(k, v) };
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

const PACKAGE_ID: &str = "dev.qos.catalogue-e2e";

#[test]
fn a_registry_entry_becomes_an_installed_app_the_desktop_can_see() {
    let publisher = SigningIdentity::generate();
    let qpkg = author_qpkg(PACKAGE_ID, &publisher);

    // Serve it.
    let port_probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let planned_port = port_probe.local_addr().unwrap().port();
    drop(port_probe);
    let url = format!("http://127.0.0.1:{planned_port}/sheets.qpkg");
    let entry = signed_entry(PACKAGE_ID, &url, &qpkg);
    let entries_json = serde_json::to_string(&vec![entry]).unwrap();
    // Bind for real; if the planned port was taken, fall back to whatever we get and rewrite the URL.
    let registry = Registry::start(entries_json, qpkg.clone());
    let registry = if registry.port == planned_port {
        registry
    } else {
        let url = format!("http://127.0.0.1:{}/sheets.qpkg", registry.port);
        let entry = signed_entry(PACKAGE_ID, &url, &qpkg);
        Registry::start(serde_json::to_string(&vec![entry]).unwrap(), qpkg)
    };

    let config_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let apps_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "local".into(),
                base_url: registry.base_url(),
                owner_pubkey_hex: String::new(),
                enabled: true,
            }],
        })
        .unwrap(),
    )
    .unwrap();

    let installer_bin = qos_install_bin();
    let _env = Env::set(&[
        ("QOS_STORE_CONFIG_DIR", config_dir.path()),
        ("QOS_STORE_CACHE_DIR", cache_dir.path()),
        ("QOS_APPS_DATA_DIR", apps_dir.path()),
        ("QOS_INSTALL_BIN", &installer_bin),
    ]);

    // 1. The registry's entry shows up as available.
    let listing = app_catalogue::available();
    assert!(
        listing.unreachable.is_empty(),
        "the local registry must be reachable: {:?}",
        listing.unreachable
    );
    let app = listing
        .apps
        .iter()
        .find(|a| a.package_id == PACKAGE_ID)
        .unwrap_or_else(|| panic!("the published app must be offered: {:?}", listing.apps));
    assert_eq!(app.display_name, "Sheets");
    assert_eq!(app.version, "1.4.0");
    assert_eq!(app.category.as_deref(), Some("Office"));
    assert_eq!(
        app.registry, "local",
        "the row names the registry it came from"
    );

    // 2. Nothing is installed yet — this is what makes step 3 mean something.
    assert!(
        launcher::registry::list_installed().unwrap().is_empty(),
        "the scratch install root must start empty"
    );

    // 3. Install it through the real installer.
    let installed = app_catalogue::install::install(PACKAGE_ID)
        .unwrap_or_else(|e| panic!("install must succeed: {}", e.user_message()));
    assert_eq!(installed.package_id, PACKAGE_ID);

    // 4. The desktop's own reader now returns it — the compositor
    //    enumerates installed apps through exactly this call.
    let now_installed = launcher::registry::list_installed().unwrap();
    let found = now_installed
        .iter()
        .find(|e| e.id == PACKAGE_ID)
        .unwrap_or_else(|| panic!("the desktop must now see it: {now_installed:?}"));
    assert!(
        found.manifest_path.exists(),
        "the manifest the launcher will read was really extracted: {:?}",
        found.manifest_path
    );
    assert!(
        found.payload_root.join("bin/app").exists(),
        "the payload was really extracted: {:?}",
        found.payload_root
    );

    // 5. And it stops being offered as available, because it is here.
    let after = app_catalogue::available();
    assert!(
        !after.apps.iter().any(|a| a.package_id == PACKAGE_ID),
        "an installed app must not still be offered: {:?}",
        after.apps
    );
}

/// Bytes that don't match the package they claim must not become an
/// installed app — the case of a compromised or buggy registry
/// handing over something other than what its signed entry described.
#[test]
fn a_corrupted_download_never_becomes_an_installed_app() {
    let publisher = SigningIdentity::generate();
    let good = author_qpkg("dev.qos.catalogue-corrupt", &publisher);
    // Serve a truncated archive under the same entry.
    let corrupted = good[..good.len() / 2].to_vec();

    let registry = Registry::start(String::new(), corrupted.clone());
    let url = format!("http://127.0.0.1:{}/sheets.qpkg", registry.port);
    let entry = signed_entry("dev.qos.catalogue-corrupt", &url, &good);
    let registry = Registry::start(serde_json::to_string(&vec![entry]).unwrap(), corrupted);

    let config_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let apps_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("registries.toml"),
        format!(
            "[[registry]]\nname = \"local\"\nbase_url = \"{}\"\n",
            registry.base_url()
        ),
    )
    .unwrap();

    let installer = qos_install_bin();
    let _env = Env::set(&[
        ("QOS_STORE_CONFIG_DIR", config_dir.path()),
        ("QOS_STORE_CACHE_DIR", cache_dir.path()),
        ("QOS_APPS_DATA_DIR", apps_dir.path()),
        ("QOS_INSTALL_BIN", &installer),
    ]);

    // The entry has to actually be on offer, or the refusal below would prove nothing about the bytes.
    let listing = app_catalogue::available();
    assert!(
        listing
            .apps
            .iter()
            .any(|a| a.package_id == "dev.qos.catalogue-corrupt"),
        "the entry must be offered before its download can be the thing \
         that fails: {listing:?}"
    );

    let err = app_catalogue::install::install("dev.qos.catalogue-corrupt")
        .expect_err("truncated bytes must not install");
    assert!(
        !matches!(err, app_catalogue::install::InstallError::NotOffered { .. }),
        "the install must fail on the bytes, not on resolution: {err:?}"
    );
    assert!(
        !err.user_message().trim().is_empty(),
        "the refusal has to say something a person can read"
    );
    assert!(
        launcher::registry::list_installed().unwrap().is_empty(),
        "nothing may be registered after a refused install"
    );
}

// Installing an app and opening it is the ordinary path.

/// A payload root shaped exactly like what `qos-install` leaves
/// behind: manifest.toml beside a bin/ — what every harness points a
/// local registry's --payload at.
fn packaged_payload_root(dir: &Path, package_id: &str) -> PathBuf {
    let root = dir.join("payload-root");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin").join("word-processor"), b"#!/bin/true\n").unwrap();
    std::fs::write(
        root.join("manifest.toml"),
        format!(
            r#"
[package]
id = "{package_id}"
version = "0.9.0"
kind = "app"
name = "Word Processor"
category = "Office"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "qos-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "optional@1"
qos-fs = "optional@1"

[entrypoint]
bin = "bin/word-processor"
type = "gui"
"#,
            hex::encode([7u8; 32])
        ),
    )
    .unwrap();
    root
}

/// Serve `qpkg` under `package_id` and install it with the real installer, returning what the desktop's own reader then sees.
fn publish_and_install(
    package_id: &str,
    qpkg: Vec<u8>,
    version: &str,
    apps_dir: &Path,
    runtime_dir: &Path,
) -> launcher::registry::InstalledPackage {
    let registry = Registry::start(String::new(), qpkg.clone());
    let url = format!("http://127.0.0.1:{}/app.qpkg", registry.port);
    let mut entry = signed_entry(package_id, &url, &qpkg);
    entry.versions[0].version = version.to_string();
    let key = SigningKey::from_bytes(&[11u8; 32]);
    entry.signature_hex = String::new();
    sign_entry(&mut entry, &key);
    let registry = Registry::start(serde_json::to_string(&vec![entry]).unwrap(), qpkg);

    let config_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("registries.toml"),
        format!(
            "[[registry]]\nname = \"local\"\nbase_url = \"{}\"\n",
            registry.base_url()
        ),
    )
    .unwrap();

    let installer = qos_install_bin();
    let _env = Env::set(&[
        ("QOS_STORE_CONFIG_DIR", config_dir.path()),
        ("QOS_STORE_CACHE_DIR", cache_dir.path()),
        ("QOS_APPS_DATA_DIR", apps_dir),
        ("QOS_INSTALL_BIN", &installer),
        ("XDG_RUNTIME_DIR", runtime_dir),
    ]);

    app_catalogue::install::install(package_id)
        .unwrap_or_else(|e| panic!("install must succeed: {}", e.user_message()));
    launcher::registry::list_installed()
        .unwrap()
        .into_iter()
        .find(|e| e.id == package_id)
        .unwrap_or_else(|| panic!("the desktop must see {package_id} after installing it"))
}

/// Installing an app and then opening it must reach the launcher with
/// the services its publisher declared: what a *newly installed* app
/// gets must equal what the package declared, service for service.
#[test]
fn an_app_installed_from_a_registry_keeps_the_services_it_declared() {
    const PKG: &str = "dev.qos.catalogue-collab";
    let scratch = tempfile::tempdir().unwrap();
    let root = packaged_payload_root(scratch.path(), PKG);

    // What the publisher declared, read with the launcher's own reader, so "declared" means what the launcher would act on.
    let declared = launcher::manifest::read_manifest(&root.join("manifest.toml"))
        .expect("the fixture manifest parses");
    assert!(
        declared.sandbox.services.contains_key("collab"),
        "the fixture has to declare collab or this test proves nothing"
    );

    let publisher = SigningIdentity::generate();
    let authored = authoring::author(
        PKG,
        "Word Processor",
        &root.join("bin/word-processor"),
        None,
        &publisher,
    )
    .unwrap();
    assert_eq!(
        authored.source,
        authoring::ManifestSource::Package(root.join("manifest.toml")),
        "the payload's own manifest is what gets published"
    );

    let apps_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let installed = publish_and_install(
        PKG,
        authored.qpkg,
        &authored.version,
        apps_dir.path(),
        runtime_dir.path(),
    );

    let reached = launcher::manifest::read_manifest(&installed.manifest_path)
        .expect("the installed manifest parses");
    assert_eq!(
        reached.sandbox.services, declared.sandbox.services,
        "an installed app must reach the launcher with the services its \
         package declared; it arrived with {:?} instead of {:?}",
        reached.sandbox.services, declared.sandbox.services
    );

    // Assert on the launcher's own resolution against a live socket, not
    // the map — a manifest that parses but resolves to nothing is the
    // same silent failure wearing a different hat.
    let _env = Env::set(&[("XDG_RUNTIME_DIR", runtime_dir.path())]);
    let services_root =
        launcher::services::ensure_services_root().expect("create the services dir");
    let _listener = std::os::unix::net::UnixListener::bind(services_root.join("collab.sock"))
        .expect("bind a collab socket for the launcher to dial");
    let binds = launcher::services::resolve_service_binds(&reached.sandbox.services, false)
        .expect("resolution must not fail on optional services");
    assert!(
        binds.iter().any(|b| b.name == "collab"),
        "the installed app must be handed a collab descriptor; \
         the launcher resolved {:?}",
        binds.iter().map(|b| &b.name).collect::<Vec<_>>()
    );
}

/// A payload that is not part of a package still installs (the local
/// registry accepts --payload /bin/true), but is published as
/// declaring no services rather than silently passing for a real package.
#[test]
fn a_payload_with_no_manifest_is_published_as_declaring_no_services() {
    let bare = tempfile::tempdir().unwrap();
    let bin = bare.path().join("true");
    std::fs::write(&bin, b"#!/bin/true\n").unwrap();

    let publisher = SigningIdentity::generate();
    let authored = authoring::author("dev.qos.bare", "Bare", &bin, None, &publisher).unwrap();
    assert_eq!(authored.source, authoring::ManifestSource::Synthesised);
}
