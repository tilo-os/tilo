//! A store app published at package format 1, byte for byte.
//!
//! The store serves every app at package format 1: format 2 changed only the
//! registry catalogue, so the archive, the signed root and the collection of a
//! format-1 app are what this build writes. The fixture is the newest
//! `so.tilo.chat` build for aarch64 (released 2026-10-15), the archive
//! `<...>/chat-0.1.118846+g000193032440e4743c4f78852c5e4be9773ed420-aarch64.tapp`
//! from `https://apps.tilo.so/artifacts/tilo/generic/tilo-app-chat/`, stored as
//! text: the repository holds no binaries. Its payload blobs are checked
//! against the collection wherever they are fetched.

use std::path::Path;

use tilo_app_package::{
    BlobHash, DiscardBlobs, Kind, PackageMeta, SignedRoot, Verification, tapp, verify,
};

fn fixture_text(name: &str) -> String {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shipped/app-chat-0.1.118846-format-1");
    std::fs::read_to_string(dir.join(name)).unwrap()
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/shipped/app-chat-0.1.118846-format-1")
            .join(name),
    )
    .unwrap()
}

fn unhex(name: &str) -> Vec<u8> {
    hex::decode(fixture_text(name).trim()).unwrap()
}

#[test]
fn the_store_app_published_at_format_1_verifies_and_installs() {
    let header = fixture_text("tapp.toml");
    assert_eq!(
        header,
        "package_id = \"tapp:4429e27a912ce4e8d61bf87f953690c46bd89981a6e3e59c86c86e4a55d6b044\"\n\
         format = 1\n"
    );

    let root_bytes = unhex("signed-root.hex");
    let root = SignedRoot::decode(&root_bytes).unwrap();
    assert_eq!(root.format, 1);
    assert_eq!(root.encode().as_slice(), root_bytes.as_slice());
    assert_eq!(
        root.package_id().to_hex(),
        "4429e27a912ce4e8d61bf87f953690c46bd89981a6e3e59c86c86e4a55d6b044"
    );

    let manifest = fixture_bytes("manifest.toml");
    assert_eq!(BlobHash::of(&manifest), root.manifest_hash);
    let collection = unhex("collection.hex");
    let meta = PackageMeta::from_root(root.clone(), manifest, collection).unwrap();
    assert_eq!(meta.id, root.package_id());
    assert_eq!(meta.manifest.package.id, "so.tilo.chat");
    assert_eq!(meta.manifest.package.kind, Kind::App);
    assert_eq!(
        meta.manifest.package.version,
        "0.1.118846+g000193032440e4743c4f78852c5e4be9773ed420"
    );
    assert!(!meta.manifest.package.kind.is_image());

    // The install path reads a thin archive before fetching its payload: the
    // format-1 app is read as it is, under its own id.
    let archived = tapp::to_bytes(&meta, None).unwrap();
    let read = tapp::read(archived.as_slice(), &mut DiscardBlobs).unwrap();
    assert_eq!(read.meta, meta);
    assert_eq!(read.meta.root.format, 1);

    assert!(matches!(
        verify(&meta, &[root.publisher_identity]),
        Verification::Trusted { .. }
    ));
}
