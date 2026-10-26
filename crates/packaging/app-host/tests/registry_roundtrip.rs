//! The catalogue a Store request resolves through: an owner-signed snapshot,
//! the publisher-signed entry, the exact package bytes, and the fail-closed
//! refusals a wrong owner key, a rewritten entry or tampered bytes produce.
//!
//! The fixture is an HTTP registry because that is the production transport;
//! the client is the same `app_cli` catalogue and download `app update` uses,
//! and the install is the app host's.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use app_host::install::install_tapp;
use app_host::{PackageStore, Pinning};
use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryTarget, RegistryVersion, SignedRegistrySnapshot,
    TappArtifact, VersionArtifact, sign_entry, sign_snapshot,
};
use test_http::{Reply, Responder};
use tilo_app_package::pack::{SigningIdentity, pack};
use tilo_app_package::{DiscardBlobs, Manifest, PackageId};

/// What the fake registry serves. The snapshot is filled once the responder
/// has a port, because its package URL names that port.
#[derive(Default)]
struct Served {
    snapshot: Option<Vec<u8>>,
    package: Vec<u8>,
    blobs: BTreeMap<String, Vec<u8>>,
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the handler answers every route from one consistent view of the served state"
)]
fn served_registry() -> (Responder, Arc<Mutex<Served>>) {
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
        if path == "/package.tapp" {
            return Some(
                Reply::status(200)
                    .header("content-type", "application/octet-stream")
                    .body(state.package.clone()),
            );
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

/// Author a tiny signed `.tapp` for `so.tilo.editor@1.0.0`.
fn author_tapp(identity: &SigningIdentity) -> (Vec<u8>, PackageId) {
    let pubhex = hex::encode(identity.public_key_bytes());
    let arch = std::env::consts::ARCH;
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin").join("app"), b"#!/bin/true\ntilo app\n").unwrap();
    let manifest_toml = format!(
        r#"
[package]
id = "so.tilo.editor"
version = "1.0.0"
kind = "app"
name = "Editor"

[package.isa]
{arch} = "baseline"

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
"#
    );
    let manifest = Manifest::from_toml_str(&manifest_toml).unwrap();
    let packed = pack(&payload, &manifest, identity).unwrap();
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    (bytes, packed.id())
}

/// The thin form and the payload blobs a registry keeps for a full package.
fn split_package(full: &[u8]) -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let mut payload = BTreeMap::new();
    let decoded = tilo_app_package::tapp::read(full, &mut payload).unwrap();
    let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None).unwrap();
    (
        thin,
        payload
            .into_iter()
            .map(|(hash, bytes)| (hash.to_hex(), bytes))
            .collect(),
    )
}

fn native_isa() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64"
    }
}

/// The publisher-signed entry a registry serves for `thin`.
fn entry_for(
    package_id: &PackageId,
    thin: &[u8],
    publisher: &SigningKey,
    url: &str,
) -> RegistryEntry {
    let decoded = tilo_app_package::tapp::read(thin, &mut DiscardBlobs).unwrap();
    let manifest = &decoded.meta.manifest;
    let mut entry = RegistryEntry {
        package_id: manifest.package.id.clone(),
        kind: registry_types::EntryKind::App,
        name: "Editor".into(),
        description: "CRDT-native editor".into(),
        tagline: "An editor".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["Office".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: manifest.package.version.clone(),
            released_at_unix: 1,
            artifact: VersionArtifact::Tapp(TappArtifact {
                tapp_url: url.into(),
                tapp_root_hash_hex: package_id.to_hex(),
                tapp_size_bytes: thin.len() as u64,
                tapp_blake3_hex: blake3::hash(thin).to_hex().to_string(),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: std::env::consts::ARCH.into(),
                    isa: native_isa().into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, publisher);
    entry
}

fn signed_snapshot(entry: RegistryEntry, owner: &SigningKey) -> SignedRegistrySnapshot {
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

/// A scratch Store config naming `url` and pinning `owner`.
fn store_config(owner: &SigningKey, url: &str) -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("registries.toml"),
        toml::to_string_pretty(&app_cli::config::RegistriesFile {
            registries: vec![app_cli::config::RegistryConfig {
                name: "test".into(),
                base_url: url.to_owned(),
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CONFIG_DIR", dir.path());
    (dir, env)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn registry_packages_resolve_and_install_only_when_trusted_and_intact() {
    // Phase `publish_resolve_fetch_and_install`.
    // The happy path, on the production catalogue/download/install code.
    {
        let publisher_key = SigningKey::from_bytes(&[7; 32]);
        let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
        let (full, pkg_id) = author_tapp(&publisher);
        let (thin, blobs) = split_package(&full);
        assert!(thin.len() < full.len(), "the registry serves the thin form");

        let (responder, state) = served_registry();
        let entry = entry_for(
            &pkg_id,
            &thin,
            &publisher_key,
            &format!("{}/package.tapp", responder.url()),
        );
        let snapshot = signed_snapshot(entry, &SigningKey::from_bytes(&[12; 32]));
        {
            let mut served = state.lock().unwrap();
            served.package = thin;
            served.blobs = blobs;
            served.snapshot = Some(serde_json::to_vec(&snapshot).unwrap());
        }
        let (_config, _env) = store_config(&SigningKey::from_bytes(&[12; 32]), &responder.url());

        let offers = app_cli::version_offers("so.tilo.editor", "test").unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].version, "1.0.0");
        let bytes = app_cli::install::verified_bytes(&offers[0], &mut |_| {}).unwrap();

        let source = app_registry::HttpBlobs::new(responder.url());
        let store = PackageStore::at(tempfile::tempdir().unwrap().path().join("store"));
        assert!(
            install_tapp(&store, &bytes, Pinning::Tried, None).is_err(),
            "a thin package without its payload cannot install"
        );
        let installed = install_tapp(&store, &bytes, Pinning::Tried, Some(&source)).unwrap();
        assert_eq!(installed.package_id(), Some(pkg_id));
        assert!(
            installed.payload_root.join("bin/app").exists(),
            "payload was materialised"
        );
    }
    // Phase `untrusted_registry_key_is_skipped`.
    // A registry whose owner key the client does NOT pin is not trusted: the
    // snapshot signature fails against the pin, so nothing is offered.
    {
        let publisher_key = SigningKey::from_bytes(&[7; 32]);
        let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
        let (full, pkg_id) = author_tapp(&publisher);
        let (thin, blobs) = split_package(&full);
        let (responder, state) = served_registry();
        let entry = entry_for(
            &pkg_id,
            &thin,
            &publisher_key,
            &format!("{}/package.tapp", responder.url()),
        );
        let snapshot = signed_snapshot(entry, &SigningKey::from_bytes(&[12; 32]));
        {
            let mut served = state.lock().unwrap();
            served.package = thin;
            served.blobs = blobs;
            served.snapshot = Some(serde_json::to_vec(&snapshot).unwrap());
        }
        let (_config, _env) = store_config(&SigningKey::from_bytes(&[13; 32]), &responder.url());
        assert!(
            app_cli::version_offers("so.tilo.editor", "test").is_err(),
            "an untrusted registry must offer nothing"
        );
    }
    // Phase `tampered_entry_is_rejected_after_snapshot_resign`.
    // Even the registry owner cannot forge an entry's binding: changing the
    // advertised artifact leaves the publisher's signature stale.
    {
        let publisher_key = SigningKey::from_bytes(&[7; 32]);
        let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
        let (full, pkg_id) = author_tapp(&publisher);
        let (thin, blobs) = split_package(&full);
        let (responder, state) = served_registry();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let mut entry = entry_for(
            &pkg_id,
            &thin,
            &publisher_key,
            &format!("{}/package.tapp", responder.url()),
        );
        if let Some(tapp) = entry.versions[0].tapp_mut() {
            tapp.tapp_blake3_hex = hex::encode([0x22; 32]);
        }
        let snapshot = signed_snapshot(entry, &owner);
        {
            let mut served = state.lock().unwrap();
            served.package = thin;
            served.blobs = blobs;
            served.snapshot = Some(serde_json::to_vec(&snapshot).unwrap());
        }
        let (_config, _env) = store_config(&owner, &responder.url());
        assert!(
            app_cli::version_offers("so.tilo.editor", "test").is_err(),
            "a publisher-unsigned entry must not be offered"
        );
    }
    // Phase `tampered_bytes_never_install`.
    // The entry is authentic, but the server answers with different bytes.
    {
        let publisher_key = SigningKey::from_bytes(&[7; 32]);
        let publisher = SigningIdentity::from_signing_key(publisher_key.clone());
        let (full, pkg_id) = author_tapp(&publisher);
        let (thin, blobs) = split_package(&full);
        let (responder, state) = served_registry();
        let owner = SigningKey::from_bytes(&[12; 32]);
        let entry = entry_for(
            &pkg_id,
            &thin,
            &publisher_key,
            &format!("{}/package.tapp", responder.url()),
        );
        let snapshot = signed_snapshot(entry, &owner);
        {
            let mut served = state.lock().unwrap();
            served.package = full;
            served.blobs = blobs;
            served.snapshot = Some(serde_json::to_vec(&snapshot).unwrap());
        }
        let (_config, _env) = store_config(&owner, &responder.url());
        let offers = app_cli::version_offers("so.tilo.editor", "test").unwrap();
        assert!(
            app_cli::install::verified_bytes(&offers[0], &mut |_| {}).is_err(),
            "downloads that do not match the signed entry never reach an installer"
        );
    }
}
