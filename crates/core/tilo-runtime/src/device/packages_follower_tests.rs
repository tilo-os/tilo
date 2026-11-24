//! The follower and a registry: a workspace records the version a registry offers, with no file,
//! and the device fetches it, checks it against the root, publisher and surface the workspace
//! recorded, and installs it into its cache. A download that is not the signed package, and a
//! publisher the workspace refuses, install nothing.

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

use super::{PackageCache, TrustLists};
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
const SURFACE: &str = r#"format = 2

[[resources]]
kind = "sheets.book"
label = "Book"
version = 1

[[resources.verbs]]
id = "recalculate"
kind = "undoable"

[[resources.verbs]]
id = "clear"
destructive = true
kind = "undoable"
"#;

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
        std::fs::write(payload.join("surface.toml"), surface).unwrap();
    }
    let (architecture, isa, _) = native_target();
    let mut bound = String::new();
    if let Some(runtime) = runtime {
        bound.push_str(&format!("runtime = \"{runtime}\"\n"));
    }
    if !optional.is_empty() {
        let listed: Vec<String> = optional.iter().map(|runtime| format!("\"{runtime}\"")).collect();
        bound.push_str(&format!("optional-runtimes = [{}]\n", listed.join(", ")));
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
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher);
    let now = tilo_clock::try_unix_now().unwrap();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: 1,
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: String::new(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: entry.publisher_pubkey_hex.clone(),
            can_add_new: true,
        }],
        entries: vec![entry],
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
        let served = Arc::new(served);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                serve(stream, &entries, &served);
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

fn serve(mut stream: TcpStream, entries: &str, tapp: &[u8]) {
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
    let (status, body) = if path.starts_with("/entries") {
        ("200 OK", entries.as_bytes())
    } else if path.ends_with(".tapp") {
        ("200 OK", tapp)
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
const HOST_TILOLANG_RUNTIME: &str = tilolang_cli::package::DEFAULT_TILOLANG_RUNTIME;

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
        manifest.package.required_runtimes = vec![HOST_TILOLANG_RUNTIME.into()];
        manifest
            .sign(&SigningIdentity::from_signing_key(SigningKey::from_bytes(&PUBLISHER)))
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
        &[HOST_TILOLANG_RUNTIME],
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
optional-runtimes = ["{HOST_TILOLANG_RUNTIME}"]

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
        publisher = hex::encode(SigningKey::from_bytes(&PUBLISHER).verifying_key().to_bytes()),
    ))
    .unwrap();
    let refused = super::carry_runtimes(&unsigned)
        .expect_err("an unsigned manifest claims no runtime this device may run an app on");
    assert!(
        refused.contains("unsigned"),
        "the refusal says the manifest is unsigned: {refused}"
    );
}
