//! Integration test: build a bundle from a fake host root, verify it,
//! then flip a byte and confirm verify rejects it.

use std::fs;
use std::path::Path;

use runtime_bundle::{build_bundle, verify_bundle, BundleError};

fn fake_host_with_libc(root: &Path) {
    for rel in [
        "lib64/ld-linux-x86-64.so.2",
        "lib64/libc.so.6",
        "lib64/libm.so.6",
        "lib64/libdl.so.2",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        // Distinct contents per file so per-file hashes differ.
        fs::write(&p, format!("STUB: {rel}\n").as_bytes()).unwrap();
    }
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(root.join("etc/ld.so.cache"), b"stub-cache").unwrap();
}

#[test]
fn build_verify_roundtrip_over_fake_host() {
    let host = tempfile::tempdir().unwrap();
    fake_host_with_libc(host.path());

    let out = tempfile::tempdir().unwrap();
    let manifest =
        build_bundle(out.path(), "2026.07", Some(host.path())).expect("build");
    assert_eq!(manifest.version, "2026.07");
    assert!(!manifest.files.is_empty(), "bundle should include files");
    assert_eq!(manifest.root_hash.len(), 64);

    // ld.so.cache should be included when present.
    assert!(manifest.files.iter().any(|f| f.path == "etc/ld.so.cache"));
    // Sorted.
    assert!(manifest
        .files
        .windows(2)
        .all(|w| w[0].path <= w[1].path));

    let verified = verify_bundle(out.path()).expect("verify");
    assert_eq!(verified.root_hash, manifest.root_hash);
}

#[test]
fn verify_rejects_a_flipped_byte() {
    let host = tempfile::tempdir().unwrap();
    fake_host_with_libc(host.path());

    let out = tempfile::tempdir().unwrap();
    build_bundle(out.path(), "2026.07", Some(host.path())).expect("build");

    let libc = out.path().join("lib/lib64/libc.so.6");
    let mut bytes = fs::read(&libc).unwrap();
    bytes[0] ^= 0x5a;
    fs::write(&libc, bytes).unwrap();

    let err = verify_bundle(out.path()).unwrap_err();
    matches!(err, BundleError::HashMismatch { .. });
}

#[test]
fn build_is_deterministic_over_root_hash() {
    let host = tempfile::tempdir().unwrap();
    fake_host_with_libc(host.path());

    let out1 = tempfile::tempdir().unwrap();
    let out2 = tempfile::tempdir().unwrap();
    let a = build_bundle(out1.path(), "v", Some(host.path())).expect("build a");
    let b = build_bundle(out2.path(), "v", Some(host.path())).expect("build b");
    assert_eq!(a.root_hash, b.root_hash);
}
