//! A signed `.tapp` published to a local registry, and a device's configuration naming it: the
//! fixtures the catalogue tests read registries with.

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
use std::path::Path;
use std::sync::Arc;

use app_cli::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;
use registry_types::{RegistryEntry, RegistryVersion, sign_entry};
use tilo_app_package::Manifest;
use tilo_app_package::pack::{SigningIdentity, pack};

pub(crate) fn author_tapp_version(
    package_id: &str,
    version: &str,
    identity: &SigningIdentity,
) -> Vec<u8> {
    author_tapp_with_files(package_id, version, identity, &[])
}

pub(crate) fn author_tapp_with_files(
    package_id: &str,
    version: &str,
    identity: &SigningIdentity,
    files: &[(String, Vec<u8>)],
) -> Vec<u8> {
    let pubhex = hex::encode(identity.public_key_bytes());
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin").join("app"), b"#!/bin/true\ntilo app\n").unwrap();
    for (path, bytes) in files {
        let path = payload.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
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
                tapp_surface_hex: String::new(),
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

/// A minimal HTTP server: GET /entries returns the catalogue JSON, GET /<name>.tapp returns the package bytes.
///
/// Shared with [`crate::registry_recovery`], whose registry is the same
/// service after a lost disk.
pub(crate) struct Registry {
    port: u16,
    _thread: std::thread::JoinHandle<()>,
}

impl Registry {
    pub(crate) fn start(entries_json: String, tapp: Vec<u8>) -> Self {
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

    pub(crate) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn serve_one(mut stream: TcpStream, entries: &str, tapp: &[u8]) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
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

pub(crate) fn registry_config(name: &str, base_url: &str, owner: &SigningKey) -> RegistryConfig {
    RegistryConfig {
        name: name.into(),
        base_url: base_url.into(),
        owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
        enabled: true,
        channel: tilo_app_package::version::ReleaseChannel::Stable,
    }
}

#[allow(
    dead_code,
    reason = "the cache and app-data directories only need to outlive the device"
)]
pub(crate) struct Device {
    pub config: tempfile::TempDir,
    pub cache: tempfile::TempDir,
    pub apps: tempfile::TempDir,
    _env: tilo_test_env::ScopedEnv,
}

impl Device {
    pub(crate) fn new(registry: Option<RegistryConfig>, extra: &[(&'static str, &Path)]) -> Self {
        let config = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let apps = tempfile::tempdir().unwrap();
        if let Some(registry) = registry {
            std::fs::write(
                config.path().join("registries.toml"),
                toml::to_string_pretty(&RegistriesFile {
                    registries: vec![registry],
                })
                .unwrap(),
            )
            .unwrap();
        }
        let mut env = scoped_env(&[
            ("TILO_STORE_CONFIG_DIR", config.path()),
            ("TILO_STORE_CACHE_DIR", cache.path()),
            ("TILO_APPS_DATA_DIR", apps.path()),
        ]);
        for (key, value) in extra {
            env.set(key, value);
        }
        Self {
            config,
            cache,
            apps,
            _env: env,
        }
    }

    pub(crate) fn point_at(&self, registry: RegistryConfig) {
        std::fs::write(
            self.config.path().join("registries.toml"),
            toml::to_string_pretty(&RegistriesFile {
                registries: vec![registry],
            })
            .unwrap(),
        )
        .unwrap();
    }
}
