//! End-to-end registry protocol coverage:
//! author a `.tapp`, publish it to a self-hosted `file://` registry
//! served by a live iroh node, then resolve → download → verify →
//! install it on a fresh client — plus the fail-closed negatives.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use app_host::install::install_tapp;
use app_host::{PackageStore, Pinning};

use app_registry::config::{RegistriesConfig, RegistryRef, RegistryTrust};
use app_registry::index::HostProfile;
use app_registry::resolve::fetch_tapp;
use app_registry::transport::BlobNode;
use app_registry::{publish_to_dir, registry_dir};

use tilo_app_package::pack::{SigningIdentity, pack};
use tilo_app_package::{Manifest, PackageId};

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
    let packed = pack(&payload, &manifest, identity).unwrap();
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    (bytes, packed.id())
}

fn file_url(dir: &std::path::Path) -> String {
    format!("file://{}", dir.display())
}

#[tokio::test(flavor = "multi_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn registry_packages_publish_resolve_and_install_only_when_trusted_and_intact() {
    // Phase `publish_resolve_fetch_and_install`.
    // The happy path, on the production resolve/verify/install code.
    {
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
        assert!(
            outcome.bytes.len() < 16 * 1024 && outcome.bytes.len() < tapp_bytes.len(),
            "the registry serves the thin package: {} bytes against {} full",
            outcome.bytes.len(),
            tapp_bytes.len()
        );

        // Without a source for the payload, a thin package cannot install.
        let store = PackageStore::at(tmp.path().join("store"));
        assert!(install_tapp(&store, &outcome.bytes, Pinning::Tried, None).is_err());

        // Install on the production path, fetching the payload from the
        // registry's node, and confirm the payload landed.
        let installed = install_tapp(
            &store,
            &outcome.bytes,
            Pinning::Tried,
            Some(&outcome.payload),
        )
        .unwrap();
        assert_eq!(installed.package_id(), Some(pkg_id));
        assert!(
            installed.payload_root.join("bin/app").exists(),
            "payload was materialised"
        );
    }
    // Phase `untrusted_registry_key_is_skipped`.
    // A registry whose operator key the client does NOT trust is skipped:
    // the index signature fails to verify, so nothing is served.
    {
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
    // Phase `tampered_entry_is_rejected_after_index_resign`.
    // Even a registry operator (who can re-sign the index) cannot forge an
    // entry's binding: tampering the advertised tapp-hash invalidates the
    // publisher's per-entry signature, so the entry is rejected.
    {
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
}
