//! End-to-end test of the `tilo-pkg` CLI binary.
//!
//! Drives the compiled binary via `std::process::Command` (the
//! cargo test harness exposes the binary under
//! `target/debug/tilo-pkg`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::process::Command;

fn bin_path() -> PathBuf {
    // CARGO_BIN_EXE_<name> is injected by cargo at build time
    // for integration tests. The bin name is `tilo-pkg`.
    PathBuf::from(env!("CARGO_BIN_EXE_tilo-pkg"))
}

/// Tiny manifest fixture — same shape as the tilo-pkg fixtures
/// module but inlined here so the CLI test stays self-contained.
fn sample_manifest_with(identity_hex: &str) -> String {
    format!(
        r#"
[package]
id          = "dev.tilo.example"
version     = "0.1.0"
kind        = "app"
runtime     = "tilo-runtime/2025.06"

[package.isa]
x86_64 = "v3"

[publisher]
identity    = "ed25519:{identity_hex}"
signature   = ""

[sandbox]
namespaces  = ["user", "pid"]
seccomp     = "tilo-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin             = "bin/hello"
splash-budget   = 100

[providers]
mime = []
ext  = []
"#
    )
}

#[test]
fn cli_pack_then_verify() {
    let workdir = tempfile::tempdir().unwrap();
    let payload_dir = workdir.path().join("payload");
    std::fs::create_dir_all(payload_dir.join("bin")).unwrap();
    std::fs::write(payload_dir.join("bin/hello"), b"#!/bin/sh\necho hi\n").unwrap();

    // Manifest with a placeholder identity — pack will patch
    // it to the generated key's public hex.
    let manifest_path = workdir.path().join("manifest.toml");
    std::fs::write(&manifest_path, sample_manifest_with(&"00".repeat(32))).unwrap();

    let qpkg = workdir.path().join("example.qpkg");
    let out = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            qpkg.to_str().unwrap(),
        ])
        .output()
        .expect("run tilo-pkg pack");
    assert!(
        out.status.success(),
        "pack failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(qpkg.exists(), "qpkg was not written");

    let out = Command::new(bin_path())
        .args(["verify", qpkg.to_str().unwrap()])
        .output()
        .expect("run tilo-pkg verify");
    assert!(
        out.status.success(),
        "verify failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("verifies and publisher is trusted"),
        "verify output: {stdout}"
    );
}

#[test]
fn cli_pack_rejects_oversized_app_atomically() {
    let workdir = tempfile::tempdir().unwrap();
    let payload_dir = workdir.path().join("payload");
    std::fs::create_dir_all(payload_dir.join("bin")).unwrap();
    std::fs::write(
        payload_dir.join("bin/hello"),
        vec![0x5a; usize::try_from(pkg::MAX_APP_UNPACKED_BYTES - 1024).unwrap()],
    )
    .unwrap();
    let manifest_path = workdir.path().join("manifest.toml");
    std::fs::write(&manifest_path, sample_manifest_with(&"00".repeat(32))).unwrap();
    let qpkg = workdir.path().join("example.qpkg");
    std::fs::write(&qpkg, b"previous valid release").unwrap();

    let out = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            qpkg.to_str().unwrap(),
        ])
        .output()
        .expect("run tilo-pkg pack");
    assert!(!out.status.success(), "oversized qpkg unexpectedly packed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("application qpkg is"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&qpkg).unwrap(),
        b"previous valid release",
        "a rejected build must not replace the prior release artifact"
    );
}

#[test]
fn cli_inspect_lists_payload() {
    let workdir = tempfile::tempdir().unwrap();
    let payload_dir = workdir.path().join("payload");
    std::fs::create_dir_all(payload_dir.join("bin")).unwrap();
    std::fs::write(payload_dir.join("bin/hello"), b"hi\n").unwrap();
    let manifest_path = workdir.path().join("manifest.toml");
    std::fs::write(&manifest_path, sample_manifest_with(&"00".repeat(32))).unwrap();

    let qpkg = workdir.path().join("example.qpkg");
    Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            qpkg.to_str().unwrap(),
        ])
        .status()
        .expect("pack");

    let out = Command::new(bin_path())
        .args(["inspect", qpkg.to_str().unwrap()])
        .output()
        .expect("inspect");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("package-id: qpkg:"), "inspect: {stdout}");
    assert!(stdout.contains("archive-size:"), "inspect: {stdout}");
    assert!(
        stdout.contains("payload-size: 3 bytes"),
        "inspect: {stdout}"
    );
    assert!(stdout.contains("package-overhead:"), "inspect: {stdout}");
    assert!(stdout.contains("bin/hello"), "inspect: {stdout}");
}

#[test]
fn cli_unpack_round_trips() {
    let workdir = tempfile::tempdir().unwrap();
    let payload_dir = workdir.path().join("payload");
    std::fs::create_dir_all(payload_dir.join("bin")).unwrap();
    std::fs::write(payload_dir.join("bin/hello"), b"hi\n").unwrap();
    let manifest_path = workdir.path().join("manifest.toml");
    std::fs::write(&manifest_path, sample_manifest_with(&"00".repeat(32))).unwrap();
    let qpkg = workdir.path().join("example.qpkg");
    Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            qpkg.to_str().unwrap(),
        ])
        .status()
        .unwrap();

    let dest = workdir.path().join("dest");
    let out = Command::new(bin_path())
        .args([
            "unpack",
            "--dest",
            dest.to_str().unwrap(),
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "unpack: {out:?}");
    assert!(dest.join("bin/hello").exists());
    assert!(dest.join("manifest.toml").exists());
    let body = std::fs::read(dest.join("bin/hello")).unwrap();
    assert_eq!(body, b"hi\n");
}

#[test]
fn cli_entry_emits_a_server_acceptable_signed_change() {
    let workdir = tempfile::tempdir().unwrap();
    let identity = workdir.path().join("publisher.json");
    let keygen = Command::new(bin_path())
        .args(["keygen", "--out", identity.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(keygen.status.success(), "keygen: {keygen:?}");

    let payload_dir = workdir.path().join("payload");
    std::fs::create_dir_all(payload_dir.join("bin")).unwrap();
    std::fs::write(payload_dir.join("bin/hello"), b"hi\n").unwrap();
    let manifest_path = workdir.path().join("manifest.toml");
    std::fs::write(&manifest_path, sample_manifest_with(&"00".repeat(32))).unwrap();
    let qpkg = workdir.path().join("example.qpkg");
    let packed = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "--identity",
            identity.to_str().unwrap(),
            "-o",
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(packed.status.success(), "pack: {packed:?}");

    let emitted = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://packages.example/dev.qos.example.qpkg",
            "--identity",
            identity.to_str().unwrap(),
            "--action",
            "add",
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(emitted.status.success(), "entry: {emitted:?}");
    let request: registry_types::EntryChangeRequest =
        serde_json::from_slice(&emitted.stdout).unwrap();
    let key = hex::decode(&request.entry.publisher_pubkey_hex).unwrap();
    let mut publisher = [0_u8; 32];
    publisher.copy_from_slice(&key);
    registry_types::verify_change_request(&request, &[publisher]).unwrap();
    assert_eq!(request.action, registry_types::EntryAction::Add);
    assert_eq!(request.entry.versions.len(), 1);
    let version = &request.entry.versions[0];
    assert_eq!(version.qpkg_blake3_hex.as_ref().map(String::len), Some(64));
    assert_eq!(
        version
            .target
            .as_ref()
            .map(|target| target.architecture.as_str()),
        Some("x86_64")
    );

    let previous = workdir.path().join("previous-entry.json");
    std::fs::write(
        &previous,
        serde_json::to_vec_pretty(&request.entry).unwrap(),
    )
    .unwrap();
    let retry = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://packages.example/dev.qos.example.qpkg",
            "--identity",
            identity.to_str().unwrap(),
            "--action",
            "update",
            "--previous-entry",
            previous.to_str().unwrap(),
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(retry.status.success(), "idempotent update: {retry:?}");
    let retried: registry_types::EntryChangeRequest =
        serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retried.entry, request.entry);

    // Continuous versions advance numerically; hash metadata never ranks them.
    for dev_version in ["0.1.1-dev.9+gffff", "0.1.1-dev.10+gaaaa"] {
        std::fs::write(
            &manifest_path,
            sample_manifest_with(&"00".repeat(32)).replace(
                "version     = \"0.1.0\"",
                &format!("version     = \"{dev_version}\""),
            ),
        )
        .unwrap();
        let packed = Command::new(bin_path())
            .args([
                "pack",
                "--manifest",
                manifest_path.to_str().unwrap(),
                "--payload",
                payload_dir.to_str().unwrap(),
                "--identity",
                identity.to_str().unwrap(),
                "-o",
                qpkg.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(packed.status.success(), "{packed:?}");
        let updated = Command::new(bin_path())
            .args([
                "entry",
                "--url",
                "https://packages.example/dev.qpkg",
                "--identity",
                identity.to_str().unwrap(),
                "--action",
                "update",
                "--previous-entry",
                previous.to_str().unwrap(),
                qpkg.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(updated.status.success(), "{updated:?}");
        let updated: registry_types::EntryChangeRequest =
            serde_json::from_slice(&updated.stdout).unwrap();
        registry_types::verify_change_request(&updated, &[publisher]).unwrap();
        assert_eq!(updated.entry.versions[0].version, dev_version);
        std::fs::write(&previous, serde_json::to_vec(&updated.entry).unwrap()).unwrap();
    }
    // A new hash at the same semantic sequence is a collision, not an update.
    std::fs::write(
        &manifest_path,
        sample_manifest_with(&"00".repeat(32)).replace(
            "version     = \"0.1.0\"",
            "version     = \"0.1.1-dev.10+gzzzz\"",
        ),
    )
    .unwrap();
    let packed = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "--identity",
            identity.to_str().unwrap(),
            "-o",
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(packed.status.success(), "{packed:?}");
    let collision = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://packages.example/dev.qpkg",
            "--identity",
            identity.to_str().unwrap(),
            "--action",
            "update",
            "--previous-entry",
            previous.to_str().unwrap(),
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!collision.status.success());
    assert!(String::from_utf8_lossy(&collision.stderr).contains("different package bytes"));

    let missing_previous = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://packages.example/dev.qos.example.qpkg",
            "--identity",
            identity.to_str().unwrap(),
            "--action",
            "update",
            qpkg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!missing_previous.status.success());
}
