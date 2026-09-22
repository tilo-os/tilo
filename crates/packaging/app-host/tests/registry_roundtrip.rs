//! End-to-end registry protocol coverage:
//! author a `.tapp`, publish it to a self-hosted `file://` registry
//! served by a live iroh node, then resolve → download → verify →
//! install it on a fresh client — plus the fail-closed negatives.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use app_host::install::install_tapp;
use app_host::registry::Pinning;
use app_host::store::Store;

use app_registry::config::{RegistriesConfig, RegistryRef, RegistryTrust};
use app_registry::index::HostProfile;
use app_registry::resolve::fetch_tapp;
use app_registry::transport::BlobNode;
use app_registry::{publish_to_dir, registry_dir};

use tilo_app_package::pack::{SigningIdentity, pack};
use tilo_app_package::{Manifest, MemoryBlobStore, PackageId};

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
    let mut store = MemoryBlobStore::new();
    let id = pack(&payload, &manifest, identity, &mut store).unwrap();
    let bytes = app_registry::tapp::write_bytes(id, &store).unwrap();
    (bytes, id)
}

fn file_url(dir: &std::path::Path) -> String {
    format!("file://{}", dir.display())
}

/// The happy path, on the production resolve/verify/install code.
#[tokio::test(flavor = "multi_thread")]
async fn publish_resolve_fetch_and_install() {
    let publisher = SigningIdentity::generate();
    let (tapp_bytes, pkg_id) = author_tapp(&publisher);

    let tmp = tempfile::tempdir().unwrap();
    let regdir = tmp.path().join("registry");

    // A live provider node serving the registry's blob store.
    let node = BlobNode::persistent(registry_dir::blobs_dir(&regdir))
        .await
        .unwrap();
    let registry_key = SigningIdentity::generate();
    let receipt = publish_to_dir(
        &regdir,
        &node,
        &tapp_bytes,
        &publisher,
        &registry_key,
        "test.reg",
        None,
    )
    .await
    .unwrap();
    assert_eq!(receipt.app_id, "so.tilo.editor");
    assert_eq!(receipt.version, "1.0.0");

    // Client trusts the registry operator key.
    let config = RegistriesConfig::from_registries(vec![RegistryRef {
        name: "test".into(),
        base_url: file_url(&regdir),
        trust: RegistryTrust::Key(registry_key.public_key_bytes()),
        priority: 0,
    }]);
    let host = HostProfile::detect();

    let outcome = fetch_tapp(&config, "so.tilo.editor", None, &host)
        .await
        .expect("fetch + verify");
    assert_eq!(outcome.package_id, pkg_id);
    assert_eq!(
        outcome.bytes, tapp_bytes,
        "fetched bytes are byte-identical"
    );

    // Install on the production path and confirm the payload landed.
    let store = Store::open(&tmp.path().join("store")).unwrap();
    let installed = install_tapp(&store, &outcome.bytes, Pinning::Tried).unwrap();
    assert_eq!(installed, pkg_id);
    let entry = store.resolve(installed).unwrap();
    assert!(
        entry.payload_root.join("bin/app").exists(),
        "payload was extracted"
    );
}

/// A registry whose operator key the client does NOT trust is skipped:
/// the index signature fails to verify, so nothing is served.
#[tokio::test(flavor = "multi_thread")]
async fn untrusted_registry_key_is_skipped() {
    let publisher = SigningIdentity::generate();
    let (tapp_bytes, _pkg_id) = author_tapp(&publisher);

    let tmp = tempfile::tempdir().unwrap();
    let regdir = tmp.path().join("registry");
    let node = BlobNode::persistent(registry_dir::blobs_dir(&regdir))
        .await
        .unwrap();
    let registry_key = SigningIdentity::generate();
    publish_to_dir(
        &regdir,
        &node,
        &tapp_bytes,
        &publisher,
        &registry_key,
        "test.reg",
        None,
    )
    .await
    .unwrap();

    // Pin a DIFFERENT operator key → index signature will not verify.
    let wrong_key = SigningIdentity::generate().public_key_bytes();
    let config = RegistriesConfig::from_registries(vec![RegistryRef {
        name: "test".into(),
        base_url: file_url(&regdir),
        trust: RegistryTrust::Key(wrong_key),
        priority: 0,
    }]);
    let host = HostProfile::detect();

    assert!(
        fetch_tapp(&config, "so.tilo.editor", None, &host)
            .await
            .is_err(),
        "an untrusted registry must serve nothing"
    );
}

/// Even a registry operator (who can re-sign the index) cannot forge an
/// entry's binding: tampering the advertised tapp-hash invalidates the
/// publisher's per-entry signature, so the entry is rejected.
#[tokio::test(flavor = "multi_thread")]
async fn tampered_entry_is_rejected_after_index_resign() {
    let publisher = SigningIdentity::generate();
    let (tapp_bytes, _pkg_id) = author_tapp(&publisher);

    let tmp = tempfile::tempdir().unwrap();
    let regdir = tmp.path().join("registry");
    let node = BlobNode::persistent(registry_dir::blobs_dir(&regdir))
        .await
        .unwrap();
    let registry_key = SigningIdentity::generate();
    publish_to_dir(
        &regdir,
        &node,
        &tapp_bytes,
        &publisher,
        &registry_key,
        "test.reg",
        None,
    )
    .await
    .unwrap();

    // Operator rewrites the advertised hash and re-signs the index.
    let mut index = registry_dir::read_index(&regdir).unwrap();
    index.packages[0].tapp_hash = hex::encode([0x22u8; 32]);
    index.sign(&registry_key).unwrap();
    registry_dir::write_index(&regdir, &index).unwrap();

    // Client trusts the (valid) index signature — but the entry's own
    // publisher signature is now stale, so it must be rejected.
    let config = RegistriesConfig::from_registries(vec![RegistryRef {
        name: "test".into(),
        base_url: file_url(&regdir),
        trust: RegistryTrust::Key(registry_key.public_key_bytes()),
        priority: 0,
    }]);
    let host = HostProfile::detect();

    assert!(
        fetch_tapp(&config, "so.tilo.editor", None, &host)
            .await
            .is_err(),
        "a publisher-unsigned entry must not install"
    );
}
