//! The follower and a registry: a workspace records the version a registry offers, with no file,
//! and the device fetches it, checks it against the root, publisher and surface the workspace
//! recorded, and installs it into its cache. A download that is not the signed package, and a
//! publisher the workspace refuses, install nothing.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_app_package::pack::SigningIdentity;
use tilo_peer_state::workspace_membership::{AppPayload, AppRelease};
use tilo_test_env::SpawnUnderEnvLock as _;

use super::{FOLLOWED_FILE, PackageCache, TrustLists};
use crate::device::control::DeviceContent;

/// The publisher's key, and the registry owner's.
const PUBLISHER: [u8; 32] = [11; 32];
const OWNER: [u8; 32] = [12; 32];

fn native_target() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_arch = "x86_64")]
    {
        ("x86_64", "baseline", "x86_64")
    }
    #[cfg(target_arch = "aarch64")]
    {
        ("aarch64", "armv8-a", "aarch64")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!("the follower tests need a supported package architecture");
}

/// A signed package of `app` at `version`, a GUI app the desktop would start.
fn author(app: &str, version: &str) -> Vec<u8> {
    author_carrying(app, version, None)
}

/// The surface a book of sheets publishes: one resource and its two actions.
const SURFACE: &str = r#"{"resources":[{"fields":[],"kind":"sheets.book","label":"Book","verbs":[{"id":"recalculate","kind":"undoable"},{"destructive":true,"id":"clear","kind":"undoable"}],"version":1}]}"#;

/// A signed package of `app` at `version` that carries `surface`, when it publishes one.
fn author_carrying(app: &str, version: &str, surface: Option<&str>) -> Vec<u8> {
    author_bound_to(app, version, surface, None, &[])
}

/// A signed package of `app` at `version` that runs on `runtime`, the Tilo runtime its manifest
/// binds to, and needs `optional` beside it.
fn author_bound_to(
    app: &str,
    version: &str,
    surface: Option<&str>,
    runtime: Option<&str>,
    optional: &[&str],
) -> Vec<u8> {
    let identity = SigningIdentity::from_signing_key(SigningKey::from_bytes(&PUBLISHER));
    let publisher = hex::encode(identity.public_key_bytes());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), b"#!/bin/true\ntilo app\n").unwrap();
    if let Some(surface) = surface {
        std::fs::write(payload.join("surface.json"), surface).unwrap();
    }
    let (architecture, isa, _) = native_target();
    let mut bound = String::new();
    if let Some(runtime) = runtime {
        let _ = writeln!(bound, "runtime = \"{runtime}\"");
    }
    if !optional.is_empty() {
        let listed: Vec<String> = optional
            .iter()
            .map(|runtime| format!("\"{runtime}\""))
            .collect();
        let _ = writeln!(bound, "optional-runtimes = [{}]", listed.join(", "));
    }
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{app}"
version = "{version}"
kind = "app"
name = "Sheets"
category = "Office"
{bound}

[package.isa]
{architecture} = "{isa}"

[publisher]
identity = "ed25519:{publisher}"
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
"#
    ))
    .unwrap();
    let packed = tilo_app_package::pack::pack(&payload, &manifest, &identity).unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// The owner-signed catalogue offering `tapp` as `app` at `version`, downloaded from `url`, its
/// version naming `surface` (empty for none).
fn catalogue(app: &str, version: &str, url: &str, tapp: &[u8], surface: &str) -> String {
    snapshot_of(vec![entry_of(app, version, url, tapp, surface, None)])
}

/// The registry entry of a package offering `tapp` at `url`; `runtime` names the build a runtime
/// package provides, and no app names one.
fn entry_of(
    app: &str,
    version: &str,
    url: &str,
    tapp: &[u8],
    surface: &str,
    runtime: Option<&str>,
) -> RegistryEntry {
    let publisher = SigningKey::from_bytes(&PUBLISHER);
    let (architecture, _, isa) = native_target();
    let mut entry = RegistryEntry {
        package_id: app.into(),
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
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: version.into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: url.into(),
                tapp_root_hash_hex: app_registry::tapp::load(tapp).unwrap().package_id.to_hex(),
                tapp_size_bytes: tapp.len() as u64,
                tapp_blake3_hex: blake3::hash(tapp).to_hex().to_string(),
                tapp_surface_hex: surface.into(),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: architecture.into(),
                    isa: isa.into(),
                    runtime: runtime.map(str::to_owned),
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher);
    entry
}

/// The owner-signed snapshot listing `entries`.
fn snapshot_of(entries: Vec<RegistryEntry>) -> String {
    let now = tilo_clock::try_unix_now().unwrap();
    let mut publishers: Vec<PublisherAuthorization> = Vec::new();
    for entry in &entries {
        if publishers
            .iter()
            .any(|listed| listed.pubkey_hex == entry.publisher_pubkey_hex)
        {
            continue;
        }
        publishers.push(PublisherAuthorization {
            pubkey_hex: entry.publisher_pubkey_hex.clone(),
            can_add_new: true,
        });
    }
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: 1,
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: String::new(),
        moderation_head_hex: String::new(),
        publishers,
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, &SigningKey::from_bytes(&OWNER)).unwrap();
    serde_json::to_string(&snapshot).unwrap()
}

/// A registry on loopback: `/entries` answers its catalogue, any `.tapp` the bytes `served`.
struct Registry {
    port: u16,
}

impl Registry {
    fn start(app: &str, version: &str, tapp: &[u8], served: Vec<u8>) -> Self {
        let surface = app_registry::publisher::entry::surface_hex(tapp).unwrap();
        Self::naming(app, version, tapp, served, &surface)
    }

    /// A registry whose catalogue names `surface` for the version, whatever `tapp` carries.
    fn naming(app: &str, version: &str, tapp: &[u8], served: Vec<u8>, surface: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let entries = Arc::new(catalogue(
            app,
            version,
            &format!("http://127.0.0.1:{port}/app.tapp"),
            tapp,
            surface,
        ));
        let served = Arc::new(vec![("/app.tapp".to_owned(), served)]);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let tapps: Vec<(&str, Vec<u8>)> = served
                    .iter()
                    .map(|(at, bytes)| (at.as_str(), bytes.clone()))
                    .collect();
                serve(stream, &entries, &tapps);
            }
        });
        Self { port }
    }

    /// The registry as the workspace's `registry.list` names it.
    fn listed(&self) -> app_cli::config::RegistriesFile {
        app_cli::config::registries_from_rows(&serde_json::json!({"registries": [{
            "name": "local",
            "base_url": format!("http://127.0.0.1:{}", self.port),
            "owner_pubkey_hex": hex::encode(SigningKey::from_bytes(&OWNER).verifying_key().to_bytes()),
            "enabled": true,
            "channel": "stable",
        }]}))
        .unwrap()
    }
}

fn serve(mut stream: TcpStream, entries: &str, tapps: &[(&str, Vec<u8>)]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let served = tapps.iter().find(|(at, _)| *at == path);
    let (status, body): (&str, &[u8]) = if path.starts_with("/entries") {
        ("200 OK", entries.as_bytes())
    } else if let Some((_, bytes)) = served {
        ("200 OK", bytes)
    } else {
        ("404 Not Found", b"no".as_slice())
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body);
}

/// The release a workspace records of `tapp` as a registry offers it: no file it keeps.
fn release_of(app: &str, version: &str, tapp: &[u8]) -> AppRelease {
    AppRelease {
        app: app.into(),
        version: version.into(),
        surface: app_registry::publisher::entry::surface_hex(tapp).unwrap(),
        package: app_registry::tapp::load(tapp).unwrap().package_id.to_hex(),
        publisher: hex::encode(
            SigningKey::from_bytes(&PUBLISHER)
                .verifying_key()
                .to_bytes(),
        ),
        file: None,
        dev: false,
        pinned: false,
        payload: AppPayload::Tapp,
    }
}

/// A device's empty cache, with its own store and configuration, and the scope that names them.
fn device() -> (tempfile::TempDir, PackageCache, tilo_test_env::ScopedEnv) {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["apps", "config", "cache"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", dir.path().join("apps"))
        .set("TILO_STORE_CONFIG_DIR", dir.path().join("config"))
        .set("TILO_STORE_CACHE_DIR", dir.path().join("cache"));
    let root = dir.path().join("apps");
    let cache = PackageCache::new(
        &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        true,
        &root,
        Arc::new(DeviceContent::at(&dir.path().join("content"))),
    );
    (dir, cache, env)
}

/// A package installed through the cache's own change — the Store's way, or a command's — is
/// named here as it installs: its surface's types and actions are known before anything opens
/// it, as they are for one a workspace's follower installs, and as a daemon's start names every
/// package it already holds.
#[test]
fn an_install_through_the_cache_names_the_apps_actions_now() {
    const APP: &str = "so.tilo.cache-installed-surface";
    let (dir, cache, _env) = device();
    let tapp = author_carrying(APP, "1.0.0", Some(SURFACE));
    let hash = blake3::hash(&tapp).to_hex().to_string();
    let content = dir.path().join("content");
    std::fs::create_dir_all(&content).unwrap();
    std::fs::write(content.join(&hash), &tapp).unwrap();
    assert!(
        collab_types::app_surface::bytes_of(APP).is_none(),
        "this process knows nothing of the app before it installs"
    );
    let applied = cache.writer.apply(package_actions::Change::Install {
        file: package_actions::Uploaded {
            source: format!("tilo.content:{hash}"),
        },
        trust: package_actions::Trust {
            require_package_root: None,
            require_publisher: None,
            require_attestor: None,
            allow_self_signed: false,
            attestors: Vec::new(),
            revoked: Vec::new(),
        },
        pinned: false,
        replacement: package_actions::Replacement::Replace,
        ignore_missing_runtime: false,
        dry_run: false,
        payload_from: None,
    });
    assert!(applied.is_ok(), "the package installs: {applied:?}");
    let surface = collab_types::app_surface::bytes_of(APP)
        .expect("the app's surface is known here as soon as it installs");
    assert!(
        std::str::from_utf8(surface)
            .unwrap()
            .contains("sheets.book"),
        "its types and actions are the ones its package ships: {}",
        String::from_utf8_lossy(surface)
    );
}

#[test]
fn a_release_a_registry_offers_is_fetched_checked_and_installed() {
    const APP: &str = "so.tilo.follower-e2e";
    let (dir, cache, _env) = device();
    let tapp = author(APP, "1.4.0");
    let release = release_of(APP, "1.4.0", &tapp);
    let registries = Registry::start(APP, "1.4.0", &tapp, tapp.clone()).listed();

    assert!(
        !cache.runs(&release.package),
        "the cache starts without the version"
    );
    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    assert!(
        cache.runs(&release.package),
        "the device fetched the version the workspace named and installed it"
    );
    let installed = package_store::Catalog::at(dir.path().join("apps"), None)
        .find(APP)
        .unwrap()
        .expect("the desktop now finds the app");
    assert_eq!(installed.version, "1.4.0");
    assert!(
        installed.payload_root.join("bin/app").exists(),
        "the payload was really extracted: {}",
        installed.payload_root.display()
    );
    assert!(
        installed
            .publisher_key_hex
            .eq_ignore_ascii_case(&release.publisher),
        "installed under the publisher the workspace recorded"
    );
}

#[test]
fn a_download_that_is_not_the_signed_package_installs_nothing() {
    const APP: &str = "so.tilo.follower-corrupt";
    let (_dir, cache, _env) = device();
    let tapp = author(APP, "1.4.0");
    let release = release_of(APP, "1.4.0", &tapp);
    let mut corrupt = tapp.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0xff;
    let registries = Registry::start(APP, "1.4.0", &tapp, corrupt).listed();

    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    assert!(
        !cache.runs(&release.package),
        "bytes that do not hash to the signed offer are never installed"
    );
}

#[test]
fn a_publisher_the_workspace_refuses_installs_nothing() {
    const APP: &str = "so.tilo.follower-revoked";
    let (_dir, cache, _env) = device();
    let tapp = author(APP, "1.4.0");
    let release = release_of(APP, "1.4.0", &tapp);
    let registries = Registry::start(APP, "1.4.0", &tapp, tapp.clone()).listed();

    let refused: TrustLists = (
        Vec::new(),
        vec![tilo_app_package::trust::RevokedKey {
            pubkey_hex: release.publisher.clone(),
            reason: "test".into(),
        }],
    );
    cache.fetch_offered(&release, &refused, &registries);
    assert!(
        !cache.runs(&release.package),
        "a publisher on the workspace's refused list is never installed"
    );
}

#[test]
fn a_release_whose_surface_is_not_the_recorded_one_installs_nothing() {
    const APP: &str = "so.tilo.follower-surface";
    let (_dir, cache, _env) = device();
    let tapp = author(APP, "1.4.0");
    let mut release = release_of(APP, "1.4.0", &tapp);
    release.surface = "5e".repeat(32);
    let registries = Registry::start(APP, "1.4.0", &tapp, tapp.clone()).listed();

    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    assert!(
        !cache.runs(&release.package),
        "a package decided against another surface than the workspace's is never installed"
    );
}

#[test]
fn an_app_no_workspace_names_any_more_goes_only_when_the_follower_installed_it() {
    const FOLLOWED: &str = "so.tilo.follower-followed";
    const OTHERWISE: &str = "so.tilo.follower-otherwise";
    let (dir, cache, _env) = device();
    let followed = author(FOLLOWED, "1.0.0");
    let registries = Registry::start(FOLLOWED, "1.0.0", &followed, followed.clone()).listed();
    cache.fetch_offered(
        &release_of(FOLLOWED, "1.0.0", &followed),
        &TrustLists::default(),
        &registries,
    );
    // Installed some other way than for a workspace: an image build's, a developer's.
    let otherwise = author(OTHERWISE, "1.0.0");
    assert!(cache.install_package(
        &release_of(OTHERWISE, "1.0.0", &otherwise),
        &otherwise,
        &TrustLists::default(),
        None,
    ));
    let found = |app: &str| {
        package_store::Catalog::at(dir.path().join("apps"), None)
            .find(app)
            .unwrap()
            .is_some()
    };
    assert!(found(FOLLOWED) && found(OTHERWISE));

    let named: std::collections::BTreeSet<String> = [FOLLOWED.to_owned()].into();
    cache.drop_followed_not_named(&named);
    assert!(found(FOLLOWED), "an app a workspace still names stays");

    cache.drop_followed_not_named(&std::collections::BTreeSet::new());
    assert!(
        !found(FOLLOWED),
        "the app the follower installed for a workspace goes once none names it"
    );
    assert!(
        found(OTHERWISE),
        "an app installed some other way is never the follower's to remove"
    );
}

/// A device that goes back to a build reading only the workspace's map of app versions, after a
/// build that stopped writing that map and its follower's list: the map it reads names what it
/// named when the newer build took over, its list names what it installed then, and an app the
/// newer build installed is on neither. Its removal rule removes nothing: not what the map still
/// names, not the newer build's app, and nothing at all once its list is gone.
#[test]
fn a_device_back_on_a_build_reading_the_stale_map_removes_nothing() {
    const FOLLOWED: &str = "so.tilo.follower-before-update";
    const NEWER: &str = "so.tilo.follower-after-update";
    let (dir, cache, _env) = device();
    let followed = author(FOLLOWED, "1.0.0");
    let registries = Registry::start(FOLLOWED, "1.0.0", &followed, followed.clone()).listed();
    cache.fetch_offered(
        &release_of(FOLLOWED, "1.0.0", &followed),
        &TrustLists::default(),
        &registries,
    );
    // Installed by the newer build, which keeps no list of what it installed.
    let newer = author(NEWER, "1.0.0");
    assert!(cache.install_package(
        &release_of(NEWER, "1.0.0", &newer),
        &newer,
        &TrustLists::default(),
        None,
    ));
    let found = |app: &str| {
        package_store::Catalog::at(dir.path().join("apps"), None)
            .find(app)
            .unwrap()
            .is_some()
    };
    assert!(found(FOLLOWED) && found(NEWER));

    // The map as the newer build left it: it names what the follower installed before.
    let stale: std::collections::BTreeSet<String> = [FOLLOWED.to_owned()].into();
    cache.drop_followed_not_named(&stale);
    assert!(found(FOLLOWED), "what the stale map names stays");
    assert!(found(NEWER), "an app only the newer build installed stays");

    // Without its list, the rule removes nothing, whatever the map names.
    std::fs::remove_file(dir.path().join("apps").join(FOLLOWED_FILE)).unwrap();
    cache.drop_followed_not_named(&std::collections::BTreeSet::new());
    assert!(found(FOLLOWED) && found(NEWER), "no list, nothing removed");
}

/// The actions the surface `app` publishes on this device names, by resource kind and id.
fn published(app: &str) -> Vec<String> {
    let kept =
        collab_types::app_surface::bytes_of(app).expect("the device knows what the app publishes");
    let file =
        collab_types::surface::SurfaceFile::from_json(std::str::from_utf8(kept).unwrap()).unwrap();
    file.resources
        .iter()
        .flat_map(|resource| {
            resource
                .verbs
                .iter()
                .map(move |verb| format!("{}#{}", resource.kind, verb.id))
        })
        .collect()
}

#[test]
fn an_offer_that_names_no_surface_installs_with_every_action_its_bytes_carry() {
    const APP: &str = "so.tilo.follower-unnamed-surface";
    let (_dir, cache, _env) = device();
    let tapp = author_carrying(APP, "1.4.0", Some(SURFACE));
    // The registry names no surface, so the release made from its offer names none either.
    let mut release = release_of(APP, "1.4.0", &tapp);
    release.surface.clear();
    let registries = Registry::naming(APP, "1.4.0", &tapp, tapp.clone(), "").listed();

    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    assert!(
        cache.runs(&release.package),
        "a surface the offer does not name is unknown, never a reason to refuse"
    );
    assert_eq!(
        published(APP),
        ["sheets.book#recalculate", "sheets.book#clear"],
        "every action the package carries is published, as its own bytes say"
    );
}

#[test]
fn a_release_recorded_without_a_surface_still_installs_with_every_action() {
    const APP: &str = "so.tilo.follower-old-release";
    let (_dir, cache, _env) = device();
    let tapp = author_carrying(APP, "1.4.0", Some(SURFACE));
    // Recorded before releases named their surface; the registry names it now.
    let mut release = release_of(APP, "1.4.0", &tapp);
    release.surface.clear();
    let registries = Registry::start(APP, "1.4.0", &tapp, tapp.clone()).listed();

    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    assert!(cache.runs(&release.package), "the old release installs");
    assert_eq!(
        published(APP),
        ["sheets.book#recalculate", "sheets.book#clear"],
        "with every action its package carries"
    );
}

#[test]
fn a_package_carrying_another_surface_than_its_release_names_installs_nothing() {
    const APP: &str = "so.tilo.follower-carried-surface";
    let (_dir, cache, _env) = device();
    let tapp = author_carrying(APP, "1.4.0", Some(SURFACE));
    let mut release = release_of(APP, "1.4.0", &tapp);
    release.surface = "5e".repeat(32);
    // The offer names none, so only the bytes can say the package is not that version.
    let registries = Registry::naming(APP, "1.4.0", &tapp, tapp.clone(), "").listed();

    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    assert!(
        !cache.runs(&release.package),
        "the bytes' surface decides, and it is not the one the release names"
    );
    assert!(
        collab_types::app_surface::bytes_of(APP).is_none(),
        "nothing of a refused package is published"
    );
}

/// The exact Tilo runtime build the test's host links, and the tilolang runtime it was built
/// against: what a phone's own signed manifest names.
const HOST_TILO_RUNTIME: &str = "tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef";
fn host_tilolang_runtime() -> String {
    tilolang_cli::package::default_tilolang_runtime()
}

/// Declare the runtimes this test binary's host carries in itself, once for the binary, as the
/// phone declares the ones its APK carries: the process declares them once, as its manifest is
/// fixed for its life.
fn host_carries() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let host = author_carrying("so.tilo.phone", "0.1.0", None);
        let read = tilo_app_package::tapp::read(
            std::io::Cursor::new(host),
            &mut tilo_app_package::DiscardBlobs,
        )
        .unwrap();
        let mut manifest = read.meta.manifest;
        manifest.package.runtime = Some(HOST_TILO_RUNTIME.into());
        manifest.package.required_runtimes = vec![host_tilolang_runtime()];
        manifest
            .sign(&SigningIdentity::from_signing_key(SigningKey::from_bytes(
                &PUBLISHER,
            )))
            .expect("the host signs the manifest it carries");
        super::carry_runtimes(&manifest).expect("the host declares the runtimes it carries");
    });
}

#[test]
fn an_app_running_on_a_build_the_host_carries_installs_without_its_runtime_being_fetched() {
    const APP: &str = "so.tilo.follower-host-runtime";
    host_carries();
    let (dir, cache, _env) = device();
    let tapp = author_bound_to(
        APP,
        "1.0.0",
        None,
        Some("tilo-runtime/2026.07"),
        &[host_tilolang_runtime().as_str()],
    );
    let mut release = release_of(APP, "1.0.0", &tapp);
    release.file = Some(blake3::hash(&tapp).to_hex().to_string());

    cache.install_named(
        &release,
        &tapp,
        &TrustLists::default(),
        &app_cli::config::RegistriesFile::default(),
    );
    assert!(
        cache.runs(&release.package),
        "the app installed although its runtimes are not bundles under a bundle root"
    );
    let installed = package_store::Catalog::at(dir.path().join("apps"), None)
        .find(APP)
        .unwrap()
        .expect("the device holds the app the workspace runs");
    assert!(
        installed.payload_root.join("bin/app").exists(),
        "the app's own package was installed, verified as any device verifies it"
    );
    assert!(
        !package_store::runtime_installed("tilo-runtime/2026.07"),
        "the runtime the host carries in itself is not fetched into the device's bundle root"
    );
}

#[test]
fn an_app_needing_a_runtime_this_host_does_not_carry_is_refused_legibly() {
    const APP: &str = "so.tilo.follower-other-runtime";
    host_carries();
    let (_dir, cache, _env) = device();
    let tapp = author_bound_to(APP, "1.0.0", None, Some("tilo-runtime/2027.01"), &[]);
    let mut release = release_of(APP, "1.0.0", &tapp);
    release.file = Some(blake3::hash(&tapp).to_hex().to_string());

    let refused = cache
        .keep_runtimes(
            &release,
            &tapp,
            &TrustLists::default(),
            &app_cli::config::RegistriesFile::default(),
        )
        .unwrap_err();
    assert!(
        refused.contains("tilo-runtime/2027.01") && refused.contains(HOST_TILO_RUNTIME),
        "the refusal names the runtime the app needs and the one this device carries: {refused}"
    );
    cache.install_named(
        &release,
        &tapp,
        &TrustLists::default(),
        &app_cli::config::RegistriesFile::default(),
    );
    assert!(
        !cache.runs(&release.package),
        "an app this device cannot run is not installed"
    );
}

#[test]
fn a_host_speaks_for_the_runtimes_it_carries_only_through_its_signed_manifest() {
    let unsigned = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.phone"
version = "0.1.0"
kind = "app"
runtime = "{HOST_TILO_RUNTIME}"
optional-runtimes = ["{tilolang}"]

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/tilo-phone"
type = "gui"
"#,
        tilolang = host_tilolang_runtime(),
        publisher = hex::encode(
            SigningKey::from_bytes(&PUBLISHER)
                .verifying_key()
                .to_bytes()
        ),
    ))
    .unwrap();
    let refused = super::carry_runtimes(&unsigned)
        .expect_err("an unsigned manifest claims no runtime this device may run an app on");
    assert!(
        refused.contains("unsigned"),
        "the refusal says the manifest is unsigned: {refused}"
    );
}
/// A signed package of `app` at `version` whose payload is a tilolang app's: `app.wasm`, the
/// ids it locks and one imported module.
fn author_tilolang(app: &str, version: &str) -> Vec<u8> {
    let identity = SigningIdentity::from_signing_key(SigningKey::from_bytes(&PUBLISHER));
    let publisher = hex::encode(identity.public_key_bytes());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("imports")).unwrap();
    // A module with nothing in it, and one exported function in the import: what a compile
    // makes of either is all this test reads.
    std::fs::write(payload.join("app.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(
        payload.join("imports/so.tilo.notes.wasm"),
        b"\0asm\x01\0\0\0",
    )
    .unwrap();
    std::fs::write(payload.join("app.ids"), "[]").unwrap();
    let (architecture, isa, _) = native_target();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{app}"
version = "{version}"
kind = "app"
name = "Notes"
category = "Productivity"

[package.isa]
{architecture} = "{isa}"

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "app.wasm"
type = "gui"
"#
    ))
    .unwrap();
    let packed = tilo_app_package::pack::pack(&payload, &manifest, &identity).unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// Installing an app compiles the modules it runs, so its first open reads the machine code
/// instead of compiling it: the handover after the install finds an entry for every part.
#[test]
fn an_install_compiles_the_modules_the_app_runs() {
    const APP: &str = "so.tilo.follower-tilolang";
    let dir = tempfile::tempdir().unwrap();
    for sub in ["apps", "config", "cache", "data"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", dir.path().join("apps"))
        .set("TILO_STORE_CONFIG_DIR", dir.path().join("config"))
        .set("TILO_STORE_CACHE_DIR", dir.path().join("cache"))
        // The device's data home: where its compiled modules are kept, and what the launcher
        // grants the processes that run them read-only.
        .set("XDG_DATA_HOME", dir.path().join("data"));
    let root = dir.path().join("apps");
    let cache = PackageCache::new(
        &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
        true,
        &root,
        Arc::new(DeviceContent::at(&dir.path().join("content"))),
    );
    let tapp = author_tilolang(APP, "1.0.0");
    let release = release_of(APP, "1.0.0", &tapp);
    let registries = Registry::start(APP, "1.0.0", &tapp, tapp.clone()).listed();

    cache.fetch_offered(&release, &TrustLists::default(), &registries);
    let installed = package_store::Catalog::at(&root, None)
        .find(APP)
        .unwrap()
        .expect("the app installed");
    let modules = super::super::compiled::payload_modules(&installed.payload_root);
    assert_eq!(modules.len(), 2, "its module and its import");
    let store = dir.path().join("data").join(super::super::compiled::DIR);
    for module in &modules {
        assert!(
            tilo_wasm_aot::kept_in(&store, module).is_some(),
            "the install compiled every module the app runs, so its first open reads it"
        );
    }
}

/// The runtime package of `runtime`, sealed as `pack-runtime-package.sh` seals one: a signed
/// bundle of the program a runtime is, in a `kind = "runtime"` package named by its root.
fn runtime_package(dir: &std::path::Path, runtime: &str) -> Vec<u8> {
    let key = SigningKey::from_bytes(&PUBLISHER);
    let identity = dir.join("runtime-identity.json");
    std::fs::write(&identity, registry_types::key_file::encode(&key)).unwrap();
    let binary = dir.join("tilo-runtime");
    std::fs::write(&binary, b"#!/bin/true\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (_, version) = runtime.split_once('/').expect("a runtime names its build");
    let bundle = dir.join("runtime-bundle");
    runtime_bundle::build_binaries_bundle(
        &bundle,
        version,
        Some(runtime_bundle::host_arch()),
        Some(&identity),
        &[binary],
        &[],
    )
    .expect("the runtime bundle bakes");
    let (architecture, isa, _) = native_target();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.runtime"
name = "Tilo runtime"
category = "System"
version = "0.1.0"
kind = "runtime"
provides = "{runtime}"

[package.isa]
{architecture} = "{isa}"

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
        publisher = hex::encode(key.verifying_key().to_bytes()),
    ))
    .expect("the runtime package manifest parses");
    let packed = tilo_app_package::pack::pack(
        &bundle,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(key),
    )
    .expect("the runtime package packs");
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// A file install whose app names a runtime this device lacks fetches it from the registry the
/// workspace's registries offer it in, then installs the app beside it.
#[test]
fn an_app_whose_runtime_a_registry_offers_is_installed_with_it() {
    const APP: &str = "so.tilo.follower-fetches-runtime";
    // Not the build the host-carried tests below declare this process has: a runtime this device
    // lacks is the one a registry's offer must bring.
    const RUNTIME: &str = "tilo-runtime/2026.09";
    // The host-carried tests in this binary declare, once for its life, the runtimes the process
    // carries, and a host that carries any never fetches one. This install needs a process that
    // carries none, so it runs this test alone in a child of the same binary.
    if std::env::var_os("TILO_FOLLOWER_RUNTIME_FETCH_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "device::packages::follower_tests::an_app_whose_runtime_a_registry_offers_is_installed_with_it",
                "--nocapture",
            ])
            .env("TILO_FOLLOWER_RUNTIME_FETCH_CHILD", "1")
            .status_locked()
            .expect("the child runs this test");
        assert!(
            status.success(),
            "the runtime fetch test failed in its own process"
        );
        return;
    }
    let (dir, cache, mut env) = device();
    // The runtime bundle's publisher is the one this device trusts.
    let trusted = dir.path().join("trusted-runtimes.txt");
    std::fs::write(
        &trusted,
        format!(
            "{}\n",
            hex::encode(
                SigningKey::from_bytes(&PUBLISHER)
                    .verifying_key()
                    .to_bytes()
            )
        ),
    )
    .unwrap();
    env.set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trusted);

    let runtime_bytes = runtime_package(dir.path(), RUNTIME);
    let app = author_bound_to(APP, "1.0.0", None, Some(RUNTIME), &[]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let surface = app_registry::publisher::entry::surface_hex(&app).unwrap();
    let entries = Arc::new(snapshot_of(vec![
        entry_of(
            APP,
            "1.0.0",
            &format!("http://127.0.0.1:{port}/app.tapp"),
            &app,
            &surface,
            None,
        ),
        entry_of(
            "so.tilo.runtime",
            "0.1.0",
            &format!("http://127.0.0.1:{port}/runtime.tapp"),
            &runtime_bytes,
            "",
            Some(RUNTIME),
        ),
    ]));
    let served = Arc::new(vec![
        ("/app.tapp".to_owned(), app.clone()),
        ("/runtime.tapp".to_owned(), runtime_bytes),
    ]);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let tapps: Vec<(&str, Vec<u8>)> = served
                .iter()
                .map(|(at, bytes)| (at.as_str(), bytes.clone()))
                .collect();
            serve(stream, &entries, &tapps);
        }
    });
    let registries = app_cli::config::registries_from_rows(&serde_json::json!({"registries": [{
        "name": "local",
        "base_url": format!("http://127.0.0.1:{port}"),
        "owner_pubkey_hex": hex::encode(SigningKey::from_bytes(&OWNER).verifying_key().to_bytes()),
        "enabled": true,
        "channel": "stable",
    }]}))
    .unwrap();

    let mut release = release_of(APP, "1.0.0", &app);
    release.file = Some(blake3::hash(&app).to_hex().to_string());
    cache.install_named(&release, &app, &TrustLists::default(), &registries);
    assert!(
        package_store::runtime_installed(RUNTIME),
        "the runtime the app names was fetched from the registry the workspace offers"
    );
    assert!(
        cache.runs(&release.package),
        "the app installed beside the runtime it needs"
    );
}

// USER REQUIREMENT: an app whose runtime no repository offers is refused once, its row says why, and it is not retried until the repositories change.
#[test]
fn an_app_no_repository_has_a_runtime_for_is_refused_once_and_tried_again_when_one_offers_it() {
    const APP: &str = "so.tilo.follower-without-runtime";
    const RUNTIME: &str = "tilo-runtime/2026.05";
    let (dir, cache, _env) = device();
    let app = author_bound_to(APP, "1.0.0", None, Some(RUNTIME), &[]);
    let mut old = release_of(APP, "1.0.0", &app);
    old.file = Some(blake3::hash(&app).to_hex().to_string());
    let registry = Registry::start(APP, "1.0.0", &app, app.clone());
    let registries = registry.listed();
    cache.install_named(&old, &app, &TrustLists::default(), &registries);
    assert!(!cache.runs(&old.package));
    let rows = cache.rows_for_test().unwrap();
    let row = rows.into_iter().find(|r| r.app == APP).unwrap();
    let said = row.failed().unwrap_or_default();
    assert!(said.contains(RUNTIME), "{said}");
    let bundle = dir.path().join("apps/runtimes/2026.05");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("bundle.toml"), "").unwrap();
    cache.install_named(&old, &app, &TrustLists::default(), &registries);
    assert!(
        !cache.runs(&old.package),
        "nothing is retried on the same repositories"
    );
    // A changed repository is the event that can offer the runtime: the package is tried again.
    let mut grown = registries;
    grown.registries[0].name = "another".into();
    cache.install_named(&old, &app, &TrustLists::default(), &grown);
    assert!(
        cache.runs(&old.package),
        "the repositories changed, so it installed"
    );
}

/// The workspace's governance action `verb`, asked as its device's person: it completes.
fn ask_completes(
    workspaces: &crate::device::workspaces::ServiceWorkspaces,
    workspace: &str,
    verb: &str,
    arguments: serde_json::Value,
) {
    let asked = crate::device::dev_builds::ask_as_person(workspaces, workspace, verb, arguments);
    assert!(
        matches!(
            asked,
            Ok(tilo_actions::remote::ReceiptStatus::Completed { .. })
        ),
        "the workspace takes {verb}: {asked:?}"
    );
}

// USER REQUIREMENT: a workspace's apps follow the store without anyone typing a command. The
// release a workspace runs moves to the store's newest one on the regular check; one the
// workspace went back to stays, until its runtime is gone and it no longer runs here, when the
// workspace moves to the store's current release.
#[test]
fn a_workspace_whose_app_release_runtime_is_gone_moves_to_the_stores_current_release() {
    use crate::device::app_updates::{Stuck, follow_store_in};

    const APP: &str = "so.tilo.follows-the-store";
    let (_dir, _cache, _env) = device();
    let old = author(APP, "0.1.0");
    let current = author(APP, "0.2.0");
    let registry = Registry::start(APP, "0.2.0", &current, current.clone()).listed();
    let id = tilo_desktop_model::WorkspaceId::new();
    let (workspaces, _directory, _key, _cert, _lock) = tilo_rt::app()
        .block_on(crate::device::workspaces::ServiceWorkspaces::test_hosting_person(id))
        .unwrap();
    let workspace = id.to_string();
    let local = &registry.registries[0];
    ask_completes(
        &workspaces,
        &workspace,
        "registry.add",
        serde_json::json!({
            "name": local.name,
            "url": local.base_url,
            "owner_key": local.owner_pubkey_hex,
        }),
    );
    let install = |tapp: &[u8], version: &str| {
        let release = release_of(APP, version, tapp);
        ask_completes(
            &workspaces,
            &workspace,
            tilo_action_policy::roles::governance::INSTALL_APP,
            serde_json::json!({
                "app": release.app,
                "version": release.version,
                "surface": release.surface,
                "package": release.package,
                "publisher": release.publisher,
            }),
        );
    };
    let runs = |stuck: bool| {
        let stuck: Arc<Stuck> = Arc::new(move |_, _| stuck);
        tilo_rt::app()
            .block_on(follow_store_in(&workspaces, &workspace, &stuck))
            .unwrap();
        let release = tilo_rt::app()
            .block_on(workspaces.copies().app_versions(&workspace))
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|installed| installed.release)
            .find(|release| release.app == APP)
            .expect("the workspace runs the app");
        (release.version, release.package)
    };
    let current_package = release_of(APP, "0.2.0", &current).package;

    install(&old, "0.1.0");
    assert_eq!(
        runs(false),
        ("0.2.0".to_owned(), current_package.clone()),
        "the regular check moves the workspace to the store's newest release"
    );

    install(&old, "0.1.0");
    assert_eq!(
        runs(false).0,
        "0.1.0",
        "a workspace that went back to an earlier release stays there while it runs"
    );

    assert_eq!(
        runs(true),
        ("0.2.0".to_owned(), current_package),
        "a release that no longer runs here moves to the store's current one"
    );
}
