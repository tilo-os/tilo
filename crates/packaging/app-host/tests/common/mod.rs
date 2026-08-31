//! Test helper: build a real .qpkg-tar in memory so integration tests
//! can round-trip through `install::install_qpkg`.
//!
//! Mirrors `pkg-cli::write_qpkg`, kept local so this test crate does
//! not depend on the `pkg-cli` binary crate.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{Cursor, Write};

use ed25519_dalek::SigningKey;
use pkg::blobs::{BlobHash, BlobStore, MemoryBlobStore};
use pkg::signed::SignedRoot;
use pkg::{Manifest, PackageId, SigningIdentity, pack};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

const QPKG_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct QpkgHeader {
    package_id: String,
    format: u32,
}

/// Same TOML shape as the pkg crate's fixtures.
pub const SAMPLE_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "dev.qos.editor"
version     = "1.4.2"
kind        = "app"
runtime     = "qos-runtime/2025.06"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[[trust.attested_by]]
attestor    = "qos-curated/2025.06"
claim       = "deadbeef"

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "qos-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[sandbox.portals]
file-chooser = "via-host"

[sandbox.services]
collab      = "required@1"
qos-fs      = "required@1"
spellcheck  = "optional@1"

[entrypoint]
bin             = "bin/editor"
splash-segments = ["bin/editor[0x0:0x300000]", "share/splash.png"]
splash-budget   = 100

[providers]
mime = ["application/vnd.oasis.opendocument.text"]
ext  = ["odt"]
"#;

pub fn render_template(template: &str, identity_hex: &str) -> String {
    template.replace("<HEX>", identity_hex)
}

pub fn write_app_payload(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("bin"))?;
    std::fs::create_dir_all(dir.join("share"))?;
    std::fs::write(dir.join("bin/editor"), b"#!/notreal")?;
    std::fs::write(dir.join("share/splash.png"), b"PNG\x00\x00\x00")?;
    Ok(())
}

pub fn write_app_payload_with_bulk(dir: &std::path::Path, bulk_kib: usize) -> std::io::Result<()> {
    write_app_payload(dir)?;
    // Make the payload measurably-sized without stalling on IO —
    // fill a single "data" blob deterministically. Content is
    // non-repeating (so blob-store dedup can't skip the bytes).
    let bulk_bytes = bulk_kib.saturating_mul(1024);
    let mut bulk = Vec::with_capacity(bulk_bytes);
    for i in 0..bulk_bytes {
        // `i % 256` is always in 0..256, so this never falls back.
        let low_byte = u8::try_from(i % 256).unwrap_or(0);
        bulk.push(low_byte.wrapping_mul(31));
    }
    std::fs::write(dir.join("share/data.bin"), bulk)?;
    Ok(())
}

pub fn build_qpkg(payload_kib: usize) -> (Vec<u8>, PackageId) {
    let mut rng = OsRng;
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    let src = render_template(SAMPLE_APP_MANIFEST_TEMPLATE, &pub_hex);
    let manifest = Manifest::from_toml_str(&src).expect("decode manifest");

    let payload_dir = tempfile::tempdir().unwrap();
    write_app_payload_with_bulk(payload_dir.path(), payload_kib).unwrap();

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk);
    let id = pack(payload_dir.path(), &manifest, &identity, &mut store).expect("pack");

    (write_qpkg_bytes(id, &store), id)
}

pub fn write_qpkg_bytes(id: PackageId, store: &MemoryBlobStore) -> Vec<u8> {
    let buf = Cursor::new(Vec::new());
    let mut tar = tar::Builder::new(buf);

    let header = QpkgHeader {
        package_id: id.to_string(),
        format: QPKG_FORMAT,
    };
    let header_toml = toml::to_string_pretty(&header).unwrap();
    tar_append(&mut tar, "qpkg.toml", header_toml.as_bytes());

    let signed = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    tar_append(&mut tar, "signed-root", &signed);
    let signed_dec = SignedRoot::decode(&signed).unwrap();
    let manifest = store.get(&signed_dec.manifest_hash).unwrap().unwrap();
    tar_append(&mut tar, "manifest.toml", &manifest);
    let collection = store.get(&signed_dec.payload_root_hash).unwrap().unwrap();
    tar_append(&mut tar, "collection.qcl", &collection);
    for (h, b) in store.iter() {
        let name = format!("blobs/{h}");
        tar_append(&mut tar, &name, b);
    }
    let inner = tar.into_inner().unwrap();
    inner.into_inner()
}

fn tar_append<W: Write>(tar: &mut tar::Builder<W>, name: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .expect("write tar entry");
}
