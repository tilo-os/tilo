//! Helpers shared by the test modules of this binary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture: what it cannot set up is the failure it reports"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::{Command, Output};

use registry_server::{AppState, Storage};
use tilo_test_env::SpawnUnderEnvLock as _;

pub fn server_command(data_dir: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(tilo_test_env::package_binary!("tilo-registry-server"));
    command.arg("--data-dir").arg(data_dir).args(arguments);
    command
}

pub fn server(data_dir: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    server_command(data_dir, arguments).output_locked()
}

/// [`server`] for a command that has to succeed, quoting what it reported.
pub fn server_ok(data_dir: &Path, arguments: &[&str]) -> Output {
    let output = server(data_dir, arguments).unwrap();
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub fn identity(
    owner_pubkey_hex: impl Into<String>,
    base_url: &str,
) -> registry_server::ServerIdentity {
    registry_server::ServerIdentity {
        owner_pubkey_hex: owner_pubkey_hex.into(),
        name: "test".into(),
        description: "test".into(),
        base_url: base_url.into(),
    }
}

pub fn serve(storage: &Storage) -> SocketAddr {
    let storage = storage.clone();
    let identity = storage.load_identity().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
                let bound = registry_server::bind_and_run(addr, AppState::new(storage, identity))
                    .await
                    .unwrap();
                sender.send(bound).unwrap();
                std::future::pending::<()>().await;
            });
    });
    receiver.recv().unwrap()
}

pub fn target(runtime: Option<String>) -> registry_types::RegistryTarget {
    registry_types::RegistryTarget {
        platform: registry_types::TILO_OS_PLATFORM.into(),
        architecture: "x86_64".into(),
        isa: "x86_64-v3".into(),
        runtime,
        interfaces: Vec::new(),
    }
}

pub fn tapp(
    url: impl Into<String>,
    root_hash_hex: impl Into<String>,
    size: u64,
    blake3_hex: impl Into<String>,
    runtime: Option<String>,
) -> registry_types::TappArtifact {
    registry_types::TappArtifact {
        tapp_url: url.into(),
        tapp_root_hash_hex: root_hash_hex.into(),
        tapp_size_bytes: size,
        tapp_blake3_hex: blake3_hex.into(),
        target: target(runtime),
    }
}

pub fn version(
    version: impl Into<String>,
    released_at_unix: u64,
    artifact: registry_types::TappArtifact,
) -> registry_types::RegistryVersion {
    registry_types::RegistryVersion {
        version: version.into(),
        released_at_unix,
        artifact: registry_types::VersionArtifact::Tapp(artifact),
    }
}

pub fn entry(
    package_id: impl Into<String>,
    publisher_pubkey_hex: impl Into<String>,
    versions: Vec<registry_types::RegistryVersion>,
) -> registry_types::RegistryEntry {
    registry_types::RegistryEntry {
        package_id: package_id.into(),
        kind: registry_types::EntryKind::App,
        name: "Test app".into(),
        description: "An app the registry tests publish".into(),
        tagline: "Test on Tilo OS".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: Vec::new(),
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: publisher_pubkey_hex.into(),
        versions,
        updated_at_unix: 1,
        signature_hex: String::new(),
    }
}

pub fn publisher(
    pubkey_hex: impl Into<String>,
    can_add_new: bool,
) -> registry_server::PublisherRow {
    registry_server::PublisherRow {
        pubkey_hex: pubkey_hex.into(),
        can_add_new,
        note: String::new(),
        namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
    }
}

pub fn allowlist(
    publishers: Vec<registry_server::PublisherRow>,
) -> registry_server::PublisherAllowlist {
    registry_server::PublisherAllowlist { publishers }
}
