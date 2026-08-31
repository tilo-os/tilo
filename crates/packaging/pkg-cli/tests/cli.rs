//! End-to-end test of the `qos-pkg` CLI binary.
//!
//! Drives the compiled binary via `std::process::Command` (the
//! cargo test harness exposes the binary under
//! `target/debug/qos-pkg`).

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
    // for integration tests. The bin name is `qos-pkg`.
    PathBuf::from(env!("CARGO_BIN_EXE_qos-pkg"))
}

/// Tiny manifest fixture — same shape as the qos-pkg fixtures
/// module but inlined here so the CLI test stays self-contained.
fn sample_manifest_with(identity_hex: &str) -> String {
    format!(
        r#"
[package]
id          = "dev.qos.example"
version     = "0.1.0"
kind        = "app"
runtime     = "qos-runtime/2025.06"

[publisher]
identity    = "ed25519:{identity_hex}"
signature   = ""

[sandbox]
namespaces  = ["user", "pid"]
seccomp     = "qos-default"
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
        .expect("run qos-pkg pack");
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
        .expect("run qos-pkg verify");
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
