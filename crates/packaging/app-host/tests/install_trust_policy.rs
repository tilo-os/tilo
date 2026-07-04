//! `install_qpkg_with_policy` gates ingest on the publisher-trust
//! policy — matches the `qos-install` behaviour so a single trust
//! config governs both entry points.

mod common;

use app_host::install::{TrustPolicy, install_qpkg, install_qpkg_with_policy};
use app_host::registry::Pinning;
use app_host::store::Store;
use pkg::trust::{RevocationList, RevokedKey, TrustedAttestors};

#[test]
fn self_signed_install_still_succeeds_without_require_attestor() {
    // Default policy = no attestors, no require_attestor — matches the
    // out-of-the-box warn-then-install behaviour.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, expected_id) = common::build_qpkg(1);

    let policy = TrustPolicy::empty();
    let id = install_qpkg_with_policy(&store, &bytes, Pinning::Tried, &policy)
        .expect("self-signed install must succeed with empty policy");
    assert_eq!(id, expected_id);
    assert!(store.resolve(id).is_ok());
}

#[test]
fn require_attestor_refuses_self_signed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, _) = common::build_qpkg(1);

    let policy = TrustPolicy {
        attestors: TrustedAttestors::empty(),
        revocation: RevocationList::empty(),
        require_attestor: Some("qos-curated/2026.07".into()),
    };
    let err = install_qpkg_with_policy(&store, &bytes, Pinning::Tried, &policy)
        .expect_err("policy must refuse self-signed install when attestor is required");
    let msg = format!("{err}");
    assert!(
        msg.contains("qos-curated/2026.07") && msg.contains("self-signed"),
        "error should surface the missing attestor + self-signed diagnosis, got: {msg}"
    );
}

#[test]
fn revoked_publisher_refuses_install() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, expected_id) = common::build_qpkg(1);

    // Extract the publisher pubkey from the qpkg via the same
    // `MemoryBlobStore` reconstruction the daemon uses. Simpler:
    // decode the signed-root from a fresh scratch store.
    let publisher_hex = publisher_hex_of(&bytes);
    let policy = TrustPolicy {
        attestors: TrustedAttestors::empty(),
        revocation: RevocationList {
            revoked: vec![RevokedKey {
                pubkey_hex: publisher_hex,
                reason: "leaked key".into(),
            }],
        },
        require_attestor: None,
    };

    let err = install_qpkg_with_policy(&store, &bytes, Pinning::Tried, &policy)
        .expect_err("revoked publisher must refuse install");
    let msg = format!("{err}");
    assert!(
        msg.contains("revoked") && msg.contains("leaked key"),
        "expected 'revoked' + reason in error, got: {msg}"
    );

    // The store must not have persisted the entry.
    assert!(
        store.resolve(expected_id).is_err(),
        "revoked install should not leave a registry entry"
    );
}

#[test]
fn install_qpkg_convenience_matches_empty_policy() {
    // The zero-config `install_qpkg` shim must behave identically to
    // `install_qpkg_with_policy(&TrustPolicy::empty())`.
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let store_a = Store::open(dir_a.path()).unwrap();
    let store_b = Store::open(dir_b.path()).unwrap();
    let (bytes, id) = common::build_qpkg(1);

    let a = install_qpkg(&store_a, &bytes, Pinning::Tried).unwrap();
    let b = install_qpkg_with_policy(
        &store_b,
        &bytes,
        Pinning::Tried,
        &TrustPolicy::empty(),
    )
    .unwrap();
    assert_eq!(a, id);
    assert_eq!(b, id);
}

/// Ingest the qpkg into a scratch `MemoryBlobStore` just so we can
/// grab the signed-root's `publisher_identity` field — enough to feed
/// the revocation-list test above without spinning up a full parser.
fn publisher_hex_of(qpkg_bytes: &[u8]) -> String {
    use pkg::blobs::{BlobHash, BlobStore, MemoryBlobStore};
    use std::io::Read;

    let mut mem = MemoryBlobStore::new();
    let mut header_id: Option<pkg::PackageId> = None;
    let mut ar = tar::Archive::new(qpkg_bytes);
    for entry in ar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().to_string();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        if name == "qpkg.toml" {
            let s = String::from_utf8(buf).unwrap();
            #[derive(serde::Deserialize)]
            struct H {
                package_id: String,
            }
            let h: H = toml::from_str(&s).unwrap();
            header_id = Some(h.package_id.parse().unwrap());
        } else if name.starts_with("blobs/")
            || name == "signed-root"
            || name == "manifest.toml"
            || name == "collection.qcl"
        {
            mem.put(&buf).unwrap();
        }
    }
    let id = header_id.unwrap();
    let signed_bytes = mem.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    let signed = pkg::signed::SignedRoot::decode(&signed_bytes).unwrap();
    hex::encode(signed.publisher_identity)
}
