//! `tilo-install update` resolving through the catalogue the Store lists.
//!
//! The same owner-signed snapshot, version index fields and publisher entry
//! the `app` command and the app host read decide which build this installer
//! fetches. These phases pin the parts that must not drift: the owner pin,
//! the channel and target selection, and the refusal of bytes that do not
//! match the signed entry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryTarget, RegistryVersion, SignedRegistrySnapshot,
    TappArtifact, VersionArtifact, sign_entry, sign_snapshot,
};
use test_http::{Reply, Responder};
use tilo_app_package::manifest::Arch;
use tilo_app_package::{Manifest, SigningIdentity, pack};

const PACKAGE: &str = "so.tilo.registry-update";

fn install_bin() -> PathBuf {
    tilo_test_env::package_binary!("tilo-install")
}

/// One package version as the registry serves it: thin bytes and blobs.
struct Version {
    release: String,
    arch: String,
    thin: Vec<u8>,
    blobs: BTreeMap<String, Vec<u8>>,
    root_hex: String,
}

fn author(
    publisher: &SigningKey,
    version: &str,
    arch: Arch,
    body: &[u8],
) -> (Vec<u8>, BTreeMap<String, Vec<u8>>, String) {
    let identity = SigningIdentity::from_signing_key(publisher.clone());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), body).unwrap();
    let manifest_toml = format!(
        r#"
[package]
id = "{PACKAGE}"
version = "{version}"
kind = "app"
name = "Registry Update"

[package.isa]
{} = "baseline"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
"#,
        match arch {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
        },
        hex::encode(publisher.verifying_key().to_bytes())
    );
    let manifest = Manifest::from_toml_str(&manifest_toml).unwrap();
    let packed = pack(&payload, &manifest, &identity).unwrap();
    let root_hex = packed.id().to_hex();
    let full = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let mut payload = BTreeMap::new();
    let decoded = tilo_app_package::tapp::read(full.as_slice(), &mut payload).unwrap();
    let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None).unwrap();
    (
        thin,
        payload
            .into_iter()
            .map(|(hash, bytes)| (hash.to_hex(), bytes))
            .collect(),
        root_hex,
    )
}

fn version(publisher: &SigningKey, version: &str, arch: Arch, body: &[u8]) -> Version {
    let (thin, blobs, root_hex) = author(publisher, version, arch, body);
    Version {
        release: version.into(),
        arch: match arch {
            Arch::X86_64 => "x86_64".into(),
            Arch::AArch64 => "aarch64".into(),
        },
        thin,
        blobs,
        root_hex,
    }
}

fn entry(publisher: &SigningKey, url_root: &str, versions: &[Version]) -> RegistryEntry {
    let mut entry = RegistryEntry {
        package_id: PACKAGE.into(),
        kind: registry_types::EntryKind::App,
        name: "Registry Update".into(),
        description: "An app the update path must select".into(),
        tagline: "Update selection".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["Office".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: versions
            .iter()
            .map(|version| RegistryVersion {
                version: version.release.clone(),
                released_at_unix: 1,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url: format!("{url_root}/pkg-{}.tapp", version.release),
                    tapp_root_hash_hex: version.root_hex.clone(),
                    tapp_size_bytes: version.thin.len() as u64,
                    tapp_blake3_hex: blake3::hash(&version.thin).to_hex().to_string(),
                    target: RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: version.arch.clone(),
                        isa: version.arch.clone(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            })
            .collect(),
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, publisher);
    entry
}

fn snapshot(entry: RegistryEntry, owner: &SigningKey) -> SignedRegistrySnapshot {
    let now = tilo_clock::unix_now();
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
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

#[derive(Default)]
struct Served {
    snapshot: Option<Vec<u8>>,
    packages: BTreeMap<String, Vec<u8>>,
    blobs: BTreeMap<String, Vec<u8>>,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the handler answers every route from one consistent view of the served state"
)]
fn registry() -> (Responder, Arc<Mutex<Served>>) {
    let state = Arc::new(Mutex::new(Served::default()));
    let answers = Arc::clone(&state);
    let responder = Responder::handle(move |request| {
        let state = answers.lock().unwrap();
        let path = request.target.split('?').next().unwrap_or("");
        if path.starts_with("/entries") {
            return state
                .snapshot
                .as_ref()
                .map(|snapshot| Reply::status(200).body(snapshot.clone()));
        }
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(std::ffi::OsStr::to_str);
        if extension.is_some_and(|extension| {
            extension.eq_ignore_ascii_case("zst") || extension.eq_ignore_ascii_case("chunks")
        }) {
            return Some(Reply::status(404));
        }
        if let Some(name) = path.strip_prefix("/pkg-") {
            return state.packages.get(name).map(|bytes| {
                Reply::status(200)
                    .header("content-type", "application/octet-stream")
                    .body(bytes.clone())
            });
        }
        if let Some(name) = path.strip_prefix("/blobs/") {
            return state.blobs.get(name).map(|bytes| {
                Reply::status(200)
                    .header("content-type", "application/octet-stream")
                    .body(bytes.clone())
            });
        }
        Some(Reply::status(404))
    });
    (responder, state)
}

fn publish(
    responder: &Responder,
    state: &Arc<Mutex<Served>>,
    owner: &SigningKey,
    versions: &[Version],
    publisher: &SigningKey,
) {
    let entry = entry(publisher, &responder.url(), versions);
    let snapshot = snapshot(entry, owner);
    let mut served = state.lock().unwrap();
    for version in versions {
        served
            .packages
            .insert(format!("{}.tapp", version.release), version.thin.clone());
        for (hash, bytes) in &version.blobs {
            served.blobs.insert(hash.clone(), bytes.clone());
        }
    }
    served.snapshot = Some(serde_json::to_vec(&snapshot).unwrap());
}

struct Machine {
    home: tempfile::TempDir,
    publisher: SigningKey,
}

impl Machine {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            publisher: SigningKey::from_bytes(&[7; 32]),
        }
    }

    fn tapp(&self, version: &str, body: &[u8]) -> PathBuf {
        let build = self.home.path().join(format!("build-{version}"));
        std::fs::create_dir_all(build.join("payload/bin")).unwrap();
        std::fs::write(build.join("payload/bin/app"), body).unwrap();
        let identity = SigningIdentity::from_signing_key(self.publisher.clone());
        let manifest_toml = format!(
            r#"
[package]
id = "{PACKAGE}"
version = "{version}"
kind = "app"
name = "Registry Update"

[package.isa]
{} = "baseline"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
"#,
            std::env::consts::ARCH,
            hex::encode(self.publisher.verifying_key().to_bytes())
        );
        let manifest = Manifest::from_toml_str(&manifest_toml).unwrap();
        let packed = pack(&build.join("payload"), &manifest, &identity).unwrap();
        let path = build.join("pkg.tapp");
        let file = std::fs::File::create(&path).unwrap();
        tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
        path
    }

    fn config(&self, owner: &SigningKey, url: &str, channel: &str) {
        let dir = self.home.path().join("store-config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("registries.toml"),
            format!(
                r#"
[[registry]]
name = "test"
base_url = "{url}"
owner_pubkey_hex = "{}"
enabled = true
channel = "{channel}"
"#,
                hex::encode(owner.verifying_key().to_bytes())
            ),
        )
        .unwrap();
    }

    fn cmd(&self, args: &[&str]) -> (bool, String, String) {
        let mut command = Command::new(install_bin());
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("XDG_DATA_HOME", self.home.path().join("data"))
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_CACHE_HOME", self.home.path().join("cache"))
            .env(
                "TILO_STORE_CONFIG_DIR",
                self.home.path().join("store-config"),
            )
            .env("TILO_STORE_CACHE_DIR", self.home.path().join("store-cache"))
            .args(args);
        let out = command.output().expect("spawn tilo-install");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn installed_version(&self) -> String {
        package_store::Registry::load(&self.home.path().join("data/tilo-apps/registry.toml"))
            .ok()
            .and_then(|registry| registry.find(PACKAGE).map(|row| row.version.clone()))
            .unwrap_or_default()
    }

    fn installed_body(&self) -> Vec<u8> {
        package_store::Registry::load(&self.home.path().join("data/tilo-apps/registry.toml"))
            .ok()
            .and_then(|registry| registry.find(PACKAGE).map(|row| row.payload_root.clone()))
            .and_then(|root| std::fs::read(root.join("bin/app")).ok())
            .unwrap_or_default()
    }
}

fn host_arch() -> Arch {
    if std::env::consts::ARCH == "aarch64" {
        Arch::AArch64
    } else {
        Arch::X86_64
    }
}

fn other_arch() -> Arch {
    if host_arch() == Arch::AArch64 {
        Arch::X86_64
    } else {
        Arch::AArch64
    }
}

#[test]
fn update_selects_the_catalogue_build_for_the_configured_channel_and_host() {
    let machine = Machine::new();
    let owner = SigningKey::from_bytes(&[12; 32]);
    let first = machine.tapp("1.0.0", b"version one");
    let (ok, out, err) = machine.cmd(&["install", first.to_str().unwrap()]);
    assert!(ok, "install failed: {out}{err}");
    assert_eq!(machine.installed_version(), "1.0.0");

    // A stable build this machine runs, a newer pre-release, and a newer
    // build for another architecture. Stable selects the runnable release;
    // the development channel selects the pre-release.
    let versions = vec![
        version(
            &machine.publisher,
            "1.5.0",
            host_arch(),
            b"version one five",
        ),
        version(
            &machine.publisher,
            "2.0.0-dev.1",
            host_arch(),
            b"version two dev",
        ),
        version(&machine.publisher, "9.0.0", other_arch(), b"other arch"),
    ];
    let (responder, state) = registry();
    publish(&responder, &state, &owner, &versions, &machine.publisher);

    machine.config(&owner, &responder.url(), "stable");
    let (ok, out, err) = machine.cmd(&["update", PACKAGE]);
    assert!(ok, "stable update failed: {out}{err}");
    assert!(
        out.contains("updated: so.tilo.registry-update 1.0.0 -> 1.5.0"),
        "{out}"
    );
    assert_eq!(machine.installed_body(), b"version one five");

    machine.config(&owner, &responder.url(), "development");
    let (ok, out, err) = machine.cmd(&["update", PACKAGE]);
    assert!(ok, "development update failed: {out}{err}");
    assert!(
        out.contains("updated: so.tilo.registry-update 1.5.0 -> 2.0.0-dev.1"),
        "{out}"
    );
    assert_eq!(machine.installed_body(), b"version two dev");
}

#[test]
fn update_refuses_an_untrusted_snapshot_or_tampered_bytes_and_keeps_the_installed_build() {
    let machine = Machine::new();
    let owner = SigningKey::from_bytes(&[12; 32]);
    let first = machine.tapp("1.0.0", b"version one");
    let (ok, out, err) = machine.cmd(&["install", first.to_str().unwrap()]);
    assert!(ok, "install failed: {out}{err}");

    let versions = vec![version(
        &machine.publisher,
        "1.5.0",
        host_arch(),
        b"version one five",
    )];

    // The directory pins an owner key the snapshot was not signed with.
    let (responder, state) = registry();
    publish(&responder, &state, &owner, &versions, &machine.publisher);
    machine.config(
        &SigningKey::from_bytes(&[13; 32]),
        &responder.url(),
        "stable",
    );
    let (ok, _out, err) = machine.cmd(&["update", PACKAGE]);
    assert!(!ok, "an untrusted snapshot must not update anything");
    assert!(
        err.contains("registry") || err.contains("no configured registry"),
        "{err}"
    );
    assert_eq!(machine.installed_version(), "1.0.0");
    assert_eq!(machine.installed_body(), b"version one");

    // An explicit publisher pin that disagrees with the signed entry is
    // refused, not replaced by the registry's own answer.
    machine.config(&owner, &responder.url(), "stable");
    let other = hex::encode(SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes());
    let (ok, _out, err) = machine.cmd(&["update", PACKAGE, "--require-publisher", &other]);
    assert!(
        !ok,
        "a mismatched --require-publisher must refuse the update"
    );
    assert!(err.contains("does not match --require-publisher"), "{err}");
    assert_eq!(machine.installed_version(), "1.0.0");

    // The entry is authentic, but the tapp the registry serves is not the
    // one it signed.
    {
        let mut served = state.lock().unwrap();
        served
            .packages
            .insert("1.5.0.tapp".into(), b"not the signed package".to_vec());
    }
    let (ok, _out, err) = machine.cmd(&["update", PACKAGE]);
    assert!(!ok, "tampered bytes must not update anything");
    assert!(
        err.contains("does not match") || err.contains("registry check"),
        "{err}"
    );
    assert_eq!(machine.installed_version(), "1.0.0");
    assert_eq!(machine.installed_body(), b"version one");
}

#[test]
fn update_reads_a_registry_directory_published_for_offline_use() {
    let machine = Machine::new();
    let owner = SigningKey::from_bytes(&[12; 32]);
    let first = machine.tapp("1.0.0", b"version one");
    let (ok, out, err) = machine.cmd(&["install", first.to_str().unwrap()]);
    assert!(ok, "install failed: {out}{err}");

    let next = machine.tapp("1.5.0", b"version one five");
    let bytes = std::fs::read(&next).unwrap();
    let directory = machine.home.path().join("registry");
    registry_client::local::publish_to_dir(
        &directory,
        "http://127.0.0.1:8443",
        &bytes,
        &machine.publisher,
        &owner,
        registry_client::Platform::TiloOs,
        None,
        None,
    )
    .unwrap();

    let dir = machine.home.path().join("store-config");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("registries.toml"),
        format!(
            r#"
[[registry]]
name = "offline"
base_url = "file://{}"
owner_pubkey_hex = "{}"
enabled = true
channel = "stable"
"#,
            directory.display(),
            hex::encode(owner.verifying_key().to_bytes())
        ),
    )
    .unwrap();

    let (ok, out, err) = machine.cmd(&["update", PACKAGE]);
    assert!(ok, "offline update failed: {out}{err}");
    assert!(
        out.contains("updated: so.tilo.registry-update 1.0.0 -> 1.5.0"),
        "{out}"
    );
    assert_eq!(machine.installed_body(), b"version one five");
}
