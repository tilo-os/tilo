//! End-to-end registry-to-install coverage: a signed `.tapp` published to an
//! HTTP registry, listed as available, fetched, verified by
//! `tilo-install`, and present in the registry the desktop enumerates
//! installed apps from.
//!
//! The test uses a socket, signed package, and `tilo-install` child process;
//! the final assertion is that `launcher::registry` (the compositor's own
//! reader) returns the app.

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
use std::sync::Arc;

use app_cli::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_app_package::Manifest;
use tilo_app_package::pack::{SigningIdentity, pack};

/// The .tapp-authoring the local test registry does, shared verbatim
/// so tests use the same manifest contract as the served package.
#[path = "../examples/support/authoring.rs"]
mod authoring;

/// Locate the `tilo-install` binary this workspace builds. A missing
/// binary **fails** the test rather than skipping it; without the binary the
/// installation path cannot be exercised.
pub(crate) fn tilo_install_bin() -> PathBuf {
    // Remote builds can remap CARGO_MANIFEST_DIR to the caller's checkout.
    // The running test still lives in the actual Cargo profile's deps dir.
    let executable = std::env::current_exe().expect("running test executable");
    let profile = executable
        .parent()
        .and_then(Path::parent)
        .expect("Cargo test executable lives under a profile's deps directory");
    let sibling = profile.join("tilo-install");
    if sibling.is_file() {
        return sibling;
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .ancestors()
        .nth(3)
        .expect("workspace root is three levels above this crate");
    let target = root.join("target");
    let native_triple = match std::env::consts::ARCH {
        "x86_64" => Some("x86_64-unknown-linux-gnu"),
        "aarch64" => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    };
    for profile in ["debug", "release"] {
        for candidate in std::iter::once(target.join(profile).join("tilo-install")).chain(
            native_triple.map(|triple| target.join(triple).join(profile).join("tilo-install")),
        ) {
            if candidate.exists() {
                return candidate;
            }
        }
    }
    panic!(
        "tilo-install is not built under {}/target/[<native-triple>/]{{debug,release}}. \
         This test installs a real package with the real installer; \
         build it first with `cargo build -p install`.",
        root.display()
    );
}

/// Author a signed .tapp for `package_id`, declaring a GUI entrypoint so the desktop would treat it as launchable.
fn author_tapp(package_id: &str, identity: &SigningIdentity) -> Vec<u8> {
    author_tapp_version(package_id, "1.4.0", identity)
}

pub(crate) fn author_tapp_version(
    package_id: &str,
    version: &str,
    identity: &SigningIdentity,
) -> Vec<u8> {
    let pubhex = hex::encode(identity.public_key_bytes());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin").join("app"), b"#!/bin/true\ntilo app\n").unwrap();
    let manifest_toml = format!(
        r#"
[package]
id = "{package_id}"
version = "{version}"
kind = "app"
name = "Sheets"
category = "Office"

[package.isa]
{} = "{}"

[publisher]
identity = "ed25519:{pubhex}"
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
        native_target().0,
        native_target().1,
    );
    let manifest = Manifest::from_toml_str(&manifest_toml).unwrap();
    let packed = pack(&payload, &manifest, identity).unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// Manifest key/value and registry wire label for the runner's architecture.
pub(crate) fn native_target() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_arch = "x86_64")]
    {
        ("x86_64", "baseline", "x86_64")
    }
    #[cfg(target_arch = "aarch64")]
    {
        ("aarch64", "armv8-a", "aarch64")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!("registry installation tests require a supported package architecture");
}

pub(crate) fn test_publisher() -> SigningIdentity {
    SigningIdentity::from_signing_key(SigningKey::from_bytes(&[11_u8; 32]))
}

/// A signed registry entry pointing at `tapp_url`.
fn signed_entry(package_id: &str, tapp_url: &str, tapp: &[u8]) -> RegistryEntry {
    signed_entry_version(package_id, "1.4.0", tapp_url, tapp)
}

pub(crate) fn signed_entry_version(
    package_id: &str,
    version: &str,
    tapp_url: &str,
    tapp: &[u8],
) -> RegistryEntry {
    let key = SigningKey::from_bytes(&[11u8; 32]);
    let root = app_registry::tapp::load(tapp)
        .expect("fixture tapp must parse")
        .package_id;
    let mut entry = RegistryEntry {
        package_id: package_id.to_string(),
        kind: registry_types::EntryKind::App,
        name: "Sheets".into(),
        description: "A spreadsheet".into(),
        tagline: "Native Tilo OS app".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Office".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: version.into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: tapp_url.to_string(),
                tapp_root_hash_hex: root.to_hex(),
                tapp_size_bytes: tapp.len() as u64,
                tapp_blake3_hex: blake3::hash(tapp).to_hex().to_string(),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: native_target().0.into(),
                    isa: native_target().2.into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &key);
    entry
}

fn signed_snapshot_json(entry: RegistryEntry, owner: &SigningKey) -> String {
    serde_json::to_string(&signed_snapshot(entry, owner, None)).unwrap()
}

fn signed_snapshot(
    entry: RegistryEntry,
    owner: &SigningKey,
    previous: Option<&SignedRegistrySnapshot>,
) -> SignedRegistrySnapshot {
    let now = tilo_clock::try_unix_now().unwrap();
    let publisher = entry.publisher_pubkey_hex.clone();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: previous.map_or(1, |snapshot| {
            snapshot
                .sequence
                .checked_add(1)
                .expect("test snapshot sequence must not overflow")
        }),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: previous
            .map(|snapshot| registry_types::snapshot_hash(snapshot).unwrap())
            .unwrap_or_default(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: publisher,
            can_add_new: true,
        }],
        entries: vec![entry],
        owner_signature_hex: String::new(),
        channel_authorization: None,
        bots_signature_hex: String::new(),
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

/// A minimal HTTP server: GET /entries returns the catalogue JSON, GET /<name>.tapp returns the package bytes.
struct Registry {
    port: u16,
    _thread: std::thread::JoinHandle<()>,
}

impl Registry {
    fn start(entries_json: String, tapp: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let port = listener.local_addr().unwrap().port();
        let entries = Arc::new(entries_json);
        let bytes = Arc::new(tapp);
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

fn serve_one(mut stream: TcpStream, entries: &str, tapp: &[u8]) {
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
    let is_tapp = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tapp"));
    let (status, content_type, body): (&str, &str, Vec<u8>) = if path.starts_with("/entries") {
        ("200 OK", "application/json", entries.as_bytes().to_vec())
    } else if is_tapp {
        ("200 OK", "application/octet-stream", tapp.to_vec())
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

/// Point every relevant environment variable at scratch directories, restoring them when the
/// returned scope drops.
pub(crate) fn scoped_env(vars: &[(&'static str, &Path)]) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    for (k, v) in vars {
        env.set(k, v);
    }
    env
}

const PACKAGE_ID: &str = "so.tilo.catalogue-e2e";

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn registry_entries_install_update_and_refuse_corrupted_downloads() {
    // Phase `a_registry_entry_becomes_an_installed_app_the_desktop_can_see`.
    {
        let publisher = test_publisher();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let tapp = author_tapp(PACKAGE_ID, &publisher);

        // Serve it.
        let port_probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let planned_port = port_probe.local_addr().unwrap().port();
        drop(port_probe);
        let url = format!("http://127.0.0.1:{planned_port}/sheets.tapp");
        let entry = signed_entry(PACKAGE_ID, &url, &tapp);
        let entries_json = signed_snapshot_json(entry, &owner);
        // Bind the server; if the planned port was taken, fall back to whatever we get and rewrite the URL.
        let registry = Registry::start(entries_json, tapp.clone());
        let registry = if registry.port == planned_port {
            registry
        } else {
            let url = format!("http://127.0.0.1:{}/sheets.tapp", registry.port);
            let entry = signed_entry(PACKAGE_ID, &url, &tapp);
            Registry::start(signed_snapshot_json(entry, &owner), tapp)
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
                    owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                    enabled: true,
                    channel: tilo_app_package::version::ReleaseChannel::Stable,
                }],
            })
            .unwrap(),
        )
        .unwrap();

        let installer_bin = tilo_install_bin();
        let _env = scoped_env(&[
            ("TILO_STORE_CONFIG_DIR", config_dir.path()),
            ("TILO_STORE_CACHE_DIR", cache_dir.path()),
            ("TILO_APPS_DATA_DIR", apps_dir.path()),
            ("TILO_INSTALL_BIN", &installer_bin),
        ]);

        // 1. The registry's entry shows up as available.
        let listing = app_cli::available();
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
            package_store::list_installed().unwrap().is_empty(),
            "the scratch install root must start empty"
        );

        // 3. Install it through the installer.
        let installed = app_cli::install::install(PACKAGE_ID)
            .unwrap_or_else(|e| panic!("install must succeed: {}", e.user_message()));
        assert_eq!(installed.package_id, PACKAGE_ID);

        // 4. The desktop's own reader now returns it — the compositor
        //    enumerates installed apps through exactly this call.
        let now_installed = package_store::list_installed().unwrap();
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
        let after = app_cli::available();
        assert!(
            !after.apps.iter().any(|a| a.package_id == PACKAGE_ID),
            "an installed app must not still be offered: {:?}",
            after.apps
        );
    }
    // Phase `an_installed_app_updates_from_a_newer_signed_registry_offer`.
    {
        const UPDATE_ID: &str = "so.tilo.app-update-e2e";
        let publisher = test_publisher();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let config_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let apps_dir = tempfile::tempdir().unwrap();
        let installer = tilo_install_bin();
        let _env = scoped_env(&[
            ("TILO_STORE_CONFIG_DIR", config_dir.path()),
            ("TILO_STORE_CACHE_DIR", cache_dir.path()),
            ("TILO_APPS_DATA_DIR", apps_dir.path()),
            ("TILO_INSTALL_BIN", &installer),
        ]);

        let publish = |version: &str, previous: Option<&SignedRegistrySnapshot>| {
            let tapp = author_tapp_version(UPDATE_ID, version, &publisher);
            let artifact = Registry::start(String::new(), tapp.clone());
            let url = format!("http://127.0.0.1:{}/app.tapp", artifact.port);
            let entry = signed_entry_version(UPDATE_ID, version, &url, &tapp);
            let snapshot = signed_snapshot(entry, &owner, previous);
            let registry_server = Registry::start(serde_json::to_string(&snapshot).unwrap(), tapp);
            (artifact, registry_server, snapshot)
        };
        let configure = |registry: &Registry| {
            std::fs::write(
                config_dir.path().join("registries.toml"),
                toml::to_string_pretty(&RegistriesFile {
                    registries: vec![RegistryConfig {
                        name: "local".into(),
                        base_url: registry.base_url(),
                        owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                        enabled: true,
                        channel: tilo_app_package::version::ReleaseChannel::Stable,
                    }],
                })
                .unwrap(),
            )
            .unwrap();
        };

        let (_v1_artifact, v1_registry, v1_snapshot) = publish("1.4.0", None);
        configure(&v1_registry);
        app_cli::install::install(UPDATE_ID).unwrap_or_else(|error| {
            panic!("initial install must succeed: {}", error.user_message())
        });
        assert_eq!(
            app_cli::installed_version(UPDATE_ID).unwrap().as_deref(),
            Some("1.4.0")
        );

        let (_v2_artifact, v2_registry, _v2_snapshot) = publish("1.5.0", Some(&v1_snapshot));
        configure(&v2_registry);
        let listing = app_cli::all_offers();
        assert!(
            listing.unreachable.is_empty(),
            "the update registry must be reachable: {:?}",
            listing.unreachable
        );
        let update = listing
            .apps
            .iter()
            .find(|app| app.package_id == UPDATE_ID)
            .expect("the newer signed version must be offered");
        assert_eq!(update.version, "1.5.0");
        app_cli::install::update_app_reporting(update, &mut |_| {})
            .unwrap_or_else(|error| panic!("signed update must succeed: {}", error.user_message()));

        assert_eq!(
            app_cli::installed_version(UPDATE_ID).unwrap().as_deref(),
            Some("1.5.0"),
            "the launcher's registry reader must observe the updated manifest"
        );
    }
    // Phase `a_corrupted_download_never_becomes_an_installed_app`.
    // Bytes that don't match the package they claim must not become an
    // installed app — the case of a compromised or buggy registry
    // handing over something other than what its signed entry described.
    {
        let publisher = test_publisher();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let good = author_tapp("so.tilo.catalogue-corrupt", &publisher);
        // Serve a truncated archive under the same entry.
        let corrupted = good[..good.len() / 2].to_vec();

        let registry = Registry::start(String::new(), corrupted.clone());
        let url = format!("http://127.0.0.1:{}/sheets.tapp", registry.port);
        let entry = signed_entry("so.tilo.catalogue-corrupt", &url, &good);
        let registry = Registry::start(signed_snapshot_json(entry, &owner), corrupted);

        let config_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let apps_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config_dir.path().join("registries.toml"),
            toml::to_string_pretty(&RegistriesFile {
                registries: vec![RegistryConfig {
                    name: "local".into(),
                    base_url: registry.base_url(),
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
            ("TILO_STORE_CONFIG_DIR", config_dir.path()),
            ("TILO_STORE_CACHE_DIR", cache_dir.path()),
            ("TILO_APPS_DATA_DIR", apps_dir.path()),
            ("TILO_INSTALL_BIN", &installer),
        ]);

        // The entry has to actually be on offer, or the refusal below would prove nothing about the bytes.
        let listing = app_cli::available();
        assert!(
            listing
                .apps
                .iter()
                .any(|a| a.package_id == "so.tilo.catalogue-corrupt"),
            "the entry must be offered before its download can be the thing \
             that fails: {listing:?}"
        );

        let err = app_cli::install::install("so.tilo.catalogue-corrupt")
            .expect_err("truncated bytes must not install");
        assert!(
            !matches!(err, app_cli::install::InstallError::NotOffered { .. }),
            "the install must fail on the bytes, not on resolution: {err:?}"
        );
        assert!(
            !err.user_message().trim().is_empty(),
            "the refusal has to say something a person can read"
        );
        assert!(
            package_store::list_installed().unwrap().is_empty(),
            "nothing may be registered after a refused install"
        );
    }
}

// Installing an app and opening it is the ordinary path.

/// A payload root shaped exactly like what `tilo-install` leaves
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
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "optional@1"
tilo-fs = "optional@1"

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

/// Serve `tapp` under `package_id` and install it with the installer, returning what the desktop's own reader then sees.
fn publish_and_install(
    package_id: &str,
    tapp: Vec<u8>,
    version: &str,
    apps_dir: &Path,
    runtime_dir: &Path,
) -> package_store::InstalledPackage {
    let owner = SigningKey::from_bytes(&[12; 32]);
    let registry = Registry::start(String::new(), tapp.clone());
    let url = format!("http://127.0.0.1:{}/app.tapp", registry.port);
    let mut entry = signed_entry(package_id, &url, &tapp);
    entry.versions[0].version = version.to_string();
    let key = SigningKey::from_bytes(&[11u8; 32]);
    entry.signature_hex = String::new();
    sign_entry(&mut entry, &key);
    let registry = Registry::start(signed_snapshot_json(entry, &owner), tapp);

    let config_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "local".into(),
                base_url: registry.base_url(),
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
        ("TILO_STORE_CONFIG_DIR", config_dir.path()),
        ("TILO_STORE_CACHE_DIR", cache_dir.path()),
        ("TILO_APPS_DATA_DIR", apps_dir),
        ("TILO_INSTALL_BIN", &installer),
        ("XDG_RUNTIME_DIR", runtime_dir),
    ]);

    app_cli::install::install(package_id)
        .unwrap_or_else(|e| panic!("install must succeed: {}", e.user_message()));
    package_store::list_installed()
        .unwrap()
        .into_iter()
        .find(|e| e.id == package_id)
        .unwrap_or_else(|| panic!("the desktop must see {package_id} after installing it"))
}

#[test]
fn installed_apps_keep_exactly_the_services_they_declared() {
    // Phase `a_payload_with_no_manifest_is_published_as_declaring_no_services`.
    // A payload that is not part of a package still installs (the local
    // registry accepts --payload /bin/true), but is published as
    // declaring no services rather than silently passing for a package.
    {
        let bare = tempfile::tempdir().unwrap();
        let bin = bare.path().join("true");
        std::fs::write(&bin, b"#!/bin/true\n").unwrap();

        let publisher = test_publisher();
        let authored = authoring::author("so.tilo.bare", "Bare", &bin, None, &publisher).unwrap();
        assert_eq!(authored.source, authoring::ManifestSource::Synthesised);
    }
    // Phase `an_app_installed_from_a_registry_keeps_the_services_it_declared`.
    // Installing an app and then opening it must reach the launcher with
    // the services its publisher declared: what a *newly installed* app
    // gets must equal what the package declared, service for service.
    {
        const PKG: &str = "so.tilo.catalogue-collab";
        let scratch = tempfile::tempdir().unwrap();
        let root = packaged_payload_root(scratch.path(), PKG);

        // What the publisher declared, read with the launcher's own reader, so "declared" means what the launcher would act on.
        let declared = tilo_app_package::view::read_manifest(&root.join("manifest.toml"))
            .expect("the fixture manifest parses");
        assert!(
            declared.sandbox.services.contains_key("collab"),
            "the fixture has to declare collab or this test proves nothing"
        );

        let publisher = test_publisher();
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
            authored.tapp,
            &authored.version,
            apps_dir.path(),
            runtime_dir.path(),
        );

        let reached = tilo_app_package::view::read_manifest(&installed.manifest_path)
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
        let _env = scoped_env(&[("XDG_RUNTIME_DIR", runtime_dir.path())]);
        let services_root =
            launcher::services::ensure_services_root().expect("create the services dir");
        let _listener = std::os::unix::net::UnixListener::bind(services_root.join("collab.sock"))
            .expect("bind a collab socket for the launcher to dial");
        let binds = launcher::services::resolve_service_binds_for_launch(
            &reached.sandbox.services,
            false,
            launcher::services::ServiceLaunchContext {
                app_id: PKG,
                workspace_id: Some("catalogue-e2e-workspace"),
            },
        )
        .expect("resolution must not fail with authenticated launch context");
        assert!(
            binds.iter().any(|b| b.name == "collab"),
            "the installed app must be handed a collab descriptor; \
             the launcher resolved {:?}",
            binds.iter().map(|b| &b.name).collect::<Vec<_>>()
        );
    }
}

#[test]
fn explicit_version_updates_an_existing_app_to_the_requested_signed_offer() {
    const ID: &str = "so.tilo.explicit-version-e2e";
    let publisher = test_publisher();
    let owner = SigningKey::from_bytes(&[12; 32]);
    let config_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let apps_dir = tempfile::tempdir().unwrap();
    let installer = tilo_install_bin();
    let _env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", config_dir.path()),
        ("TILO_STORE_CACHE_DIR", cache_dir.path()),
        ("TILO_APPS_DATA_DIR", apps_dir.path()),
        ("TILO_INSTALL_BIN", &installer),
    ]);
    let mut artifacts = Vec::new();
    let mut versions = Vec::new();
    let mut latest = None;
    for version in ["1.4.0", "1.5.0", "1.6.0"] {
        let tapp = author_tapp_version(ID, version, &publisher);
        artifacts.push(Registry::start(String::new(), tapp.clone()));
        let url = format!(
            "http://127.0.0.1:{}/app.tapp",
            artifacts.last().unwrap().port
        );
        let entry = signed_entry_version(ID, version, &url, &tapp);
        versions.extend(entry.versions.clone());
        latest = Some(entry);
    }
    let mut entry = latest.unwrap();
    entry.versions = versions;
    sign_entry(&mut entry, &SigningKey::from_bytes(&[11; 32]));
    let registry = Registry::start(signed_snapshot_json(entry, &owner), Vec::new());
    std::fs::write(
        config_dir.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "local".into(),
                base_url: registry.base_url(),
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let install = |version: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_app"))
            .args(["install", ID, "--version", version])
            .output()
            .unwrap()
    };
    for version in ["1.4.0", "1.5.0", "1.5.0"] {
        let result = install(version);
        assert!(
            result.status.success(),
            "explicit version {version} must succeed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            app_cli::installed_version(ID).unwrap().as_deref(),
            Some(version),
            "install must select the requested signed version, not the latest 1.6.0"
        );
    }
    let downgrade = install("1.4.0");
    assert!(!downgrade.status.success(), "downgrades still use rollback");
    assert!(String::from_utf8_lossy(&downgrade.stderr).contains("app rollback"));
    assert_eq!(
        app_cli::installed_version(ID).unwrap().as_deref(),
        Some("1.5.0")
    );
}
