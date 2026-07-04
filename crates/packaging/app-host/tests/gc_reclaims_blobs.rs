//! Blob-layer garbage collection: removing a package walks its
//! collection and deletes blobs that no surviving entry references,
//! while shared blobs stay put.

mod common;

use std::fs;
use std::io::Write;

use ed25519_dalek::SigningKey;
use pkg::blobs::{BlobHash, BlobStore, MemoryBlobStore};
use pkg::signed::SignedRoot;
use pkg::{Manifest, SigningIdentity, pack};
use rand::rngs::OsRng;
use serde::Serialize;

use app_host::install::install_qpkg;
use app_host::registry::Pinning;
use app_host::store::Store;

const MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "<ID>"
version     = "1.0.0"
kind        = "app"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "qos-default"
landlock    = "strict"

[sandbox.fs]
read  = []
write = []

[sandbox.net]
mode = "off"

[entrypoint]
bin  = "bin/app"

[providers]
mime = []
ext  = []
"#;

/// Build a .qpkg with three specific shared payload files (identical
/// bytes → identical BlobHashes) plus one unique bytes file so each
/// package has a distinguishing blob.
fn build_qpkg_with_shared(
    pkg_id: &str,
    shared_files: &[(&str, &[u8])],
    unique_file: (&str, &[u8]),
) -> (Vec<u8>, pkg::PackageId, Vec<BlobHash>) {
    let mut rng = OsRng;
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    let src = MANIFEST_TEMPLATE
        .replace("<ID>", pkg_id)
        .replace("<HEX>", &pub_hex);
    let manifest = Manifest::from_toml_str(&src).expect("decode");

    let payload_dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(payload_dir.path().join("bin")).unwrap();
    fs::write(payload_dir.path().join("bin/app"), b"#!/notreal").unwrap();
    for (name, bytes) in shared_files {
        let path = payload_dir.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }
    let up_path = payload_dir.path().join(unique_file.0);
    if let Some(parent) = up_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(up_path, unique_file.1).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk);
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    let shared_hashes: Vec<BlobHash> = shared_files
        .iter()
        .map(|(_, bytes)| BlobHash::of(bytes))
        .collect();

    (write_qpkg(id, &store), id, shared_hashes)
}

fn write_qpkg(id: pkg::PackageId, store: &MemoryBlobStore) -> Vec<u8> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut tar = tar::Builder::new(buf);
    #[derive(Serialize)]
    struct H {
        package_id: String,
        format: u32,
    }
    let h = H {
        package_id: id.to_string(),
        format: 1,
    };
    let text = toml::to_string_pretty(&h).unwrap();
    append(&mut tar, "qpkg.toml", text.as_bytes());

    let signed_bytes = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    append(&mut tar, "signed-root", &signed_bytes);
    let signed = SignedRoot::decode(&signed_bytes).unwrap();
    let manifest = store.get(&signed.manifest_hash).unwrap().unwrap();
    append(&mut tar, "manifest.toml", &manifest);
    let collection = store.get(&signed.payload_root_hash).unwrap().unwrap();
    append(&mut tar, "collection.qcl", &collection);
    for (h, b) in store.iter() {
        let name = format!("blobs/{}", h);
        append(&mut tar, &name, b);
    }
    let inner = tar.into_inner().unwrap();
    inner.into_inner()
}

fn append<W: Write>(tar: &mut tar::Builder<W>, name: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes).unwrap();
}

fn blob_path(store_root: &std::path::Path, hash: &BlobHash) -> std::path::PathBuf {
    store_root.join("store").join(hash.to_hex())
}

#[test]
fn remove_reclaims_unshared_blobs_and_keeps_shared_ones() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    // Three shared payload files — identical bytes in both packages,
    // so they hash to the same BlobHash. `pack` deduplicates blobs
    // via the content-addressed store; installing pkg B after pkg A
    // leaves the shared blobs at refcount ≥ 2.
    let shared: Vec<(&str, &[u8])> = vec![
        ("share/logo.png", b"PNG\x00\x00shared-1"),
        ("share/locale.mo", b"locale-shared-2"),
        ("share/theme.css", b"theme-shared-3"),
    ];

    let (bytes_a, id_a, shared_hashes) = build_qpkg_with_shared(
        "dev.qos.pkg-a",
        &shared,
        ("bin/pkg-a-unique", b"unique to A"),
    );
    let (bytes_b, id_b, shared_hashes_b) = build_qpkg_with_shared(
        "dev.qos.pkg-b",
        &shared,
        ("bin/pkg-b-unique", b"unique to B"),
    );
    assert_eq!(shared_hashes, shared_hashes_b, "shared bytes must hash equally");
    assert_ne!(id_a, id_b, "different package ids");

    install_qpkg(&store, &bytes_a, Pinning::Tried).unwrap();
    install_qpkg(&store, &bytes_b, Pinning::Tried).unwrap();

    // Shared blobs exist on disk.
    for h in &shared_hashes {
        assert!(
            blob_path(dir.path(), h).is_file(),
            "shared blob {} must be present after install",
            h
        );
    }
    // Unique blob for A exists.
    let unique_a = BlobHash::of(b"unique to A");
    assert!(
        blob_path(dir.path(), &unique_a).is_file(),
        "A's unique blob should exist before removal"
    );

    // Remove pkg-a. Its unique blob must go; the three shared blobs
    // must stay because pkg-b still references them.
    store.remove(id_a).expect("remove tried pkg-a");

    assert!(
        !blob_path(dir.path(), &unique_a).exists(),
        "A's unique blob should have been reclaimed after removal"
    );
    for h in &shared_hashes {
        assert!(
            blob_path(dir.path(), h).is_file(),
            "shared blob {} must still exist while pkg-b references it",
            h
        );
    }

    // Removing pkg-b should now reclaim the shared blobs too.
    store.remove(id_b).expect("remove tried pkg-b");
    for h in &shared_hashes {
        assert!(
            !blob_path(dir.path(), h).exists(),
            "shared blob {} must be reclaimed once no package references it",
            h
        );
    }
}
