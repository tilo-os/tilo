//! End-to-end test of the `tilo-app-package` CLI binary.
//!
//! Drives the compiled binary via `std::process::Command` (the
//! cargo test harness exposes the binary under
//! `target/debug/tilo-app-package`).

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
    // for integration tests. The bin name is `tilo-app-package`.
    PathBuf::from(env!("CARGO_BIN_EXE_tilo-app-package"))
}

/// Tiny manifest fixture — same shape as the tilo-app-package fixtures
/// module but inlined here so the CLI test stays self-contained.
fn sample_manifest_with(identity_hex: &str) -> String {
    format!(
        r#"
[package]
id          = "so.tilo.example"
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

    let tapp = workdir.path().join("example.tapp");
    let out = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            tapp.to_str().unwrap(),
        ])
        .output()
        .expect("run tilo-app-package pack");
    assert!(
        out.status.success(),
        "pack failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tapp.exists(), "tapp was not written");

    let out = Command::new(bin_path())
        .args(["verify", tapp.to_str().unwrap()])
        .output()
        .expect("run tilo-app-package verify");
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
        vec![0x5a; usize::try_from(tilo_app_package::MAX_APP_UNPACKED_BYTES - 1024).unwrap()],
    )
    .unwrap();
    let manifest_path = workdir.path().join("manifest.toml");
    std::fs::write(&manifest_path, sample_manifest_with(&"00".repeat(32))).unwrap();
    let tapp = workdir.path().join("example.tapp");
    std::fs::write(&tapp, b"previous valid release").unwrap();

    let out = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            tapp.to_str().unwrap(),
        ])
        .output()
        .expect("run tilo-app-package pack");
    assert!(!out.status.success(), "oversized tapp unexpectedly packed");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("application tapp is"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(&tapp).unwrap(),
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

    let tapp = workdir.path().join("example.tapp");
    Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            tapp.to_str().unwrap(),
        ])
        .status()
        .expect("pack");

    let out = Command::new(bin_path())
        .args(["inspect", tapp.to_str().unwrap()])
        .output()
        .expect("inspect");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("package-id: tapp:"), "inspect: {stdout}");
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
    let tapp = workdir.path().join("example.tapp");
    Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            manifest_path.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "-o",
            tapp.to_str().unwrap(),
        ])
        .status()
        .unwrap();

    let dest = workdir.path().join("dest");
    let out = Command::new(bin_path())
        .args([
            "unpack",
            "--dest",
            dest.to_str().unwrap(),
            tapp.to_str().unwrap(),
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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario covers add, target, and retry semantics"
)]
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
    let tapp = workdir.path().join("example.tapp");
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
            tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(packed.status.success(), "pack: {packed:?}");

    let emitted = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/so.tilo.example.tapp",
            "--identity",
            identity.to_str().unwrap(),
            "--arch",
            "x86_64",
            "--action",
            "add",
            tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(emitted.status.success(), "entry: {emitted:?}");
    let x86_request: registry_types::EntryChangeRequest =
        serde_json::from_slice(&emitted.stdout).unwrap();
    let key = hex::decode(&x86_request.entry.publisher_pubkey_hex).unwrap();
    let mut publisher = [0_u8; 32];
    publisher.copy_from_slice(&key);
    registry_types::verify_change_request(&x86_request, &[publisher]).unwrap();
    assert_eq!(x86_request.action, registry_types::EntryAction::Add);
    assert_eq!(x86_request.entry.versions.len(), 1);
    let version = &x86_request.entry.versions[0];
    assert_eq!(version.tapp_blake3_hex.len(), 64);
    assert_eq!(version.target.platform, registry_types::TILO_OS_PLATFORM);
    assert_eq!(version.target.architecture, "x86_64");

    let aarch_manifest = workdir.path().join("manifest-aarch64.toml");
    std::fs::write(
        &aarch_manifest,
        sample_manifest_with(&"00".repeat(32)).replace("x86_64 = \"v3\"", "aarch64 = \"v8.2-a\""),
    )
    .unwrap();
    let aarch_tapp = workdir.path().join("example-aarch64.tapp");
    let packed = Command::new(bin_path())
        .args([
            "pack",
            "--manifest",
            aarch_manifest.to_str().unwrap(),
            "--payload",
            payload_dir.to_str().unwrap(),
            "--identity",
            identity.to_str().unwrap(),
            "-o",
            aarch_tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(packed.status.success(), "aarch64 pack: {packed:?}");
    let emitted = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/so.tilo.example-aarch64.tapp",
            "--identity",
            identity.to_str().unwrap(),
            "--arch",
            "aarch64",
            "--action",
            "add",
            aarch_tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(emitted.status.success(), "aarch64 entry: {emitted:?}");
    let aarch_request: registry_types::EntryChangeRequest =
        serde_json::from_slice(&emitted.stdout).unwrap();
    assert_eq!(
        aarch_request.entry.versions[0].target.architecture,
        "aarch64"
    );

    let previous = workdir.path().join("previous-entry.json");
    std::fs::write(
        &previous,
        serde_json::to_vec_pretty(&x86_request.entry).unwrap(),
    )
    .unwrap();
    let retry = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/so.tilo.example.tapp",
            "--identity",
            identity.to_str().unwrap(),
            "--action",
            "update",
            "--previous-entry",
            previous.to_str().unwrap(),
            tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(retry.status.success(), "idempotent update: {retry:?}");
    let retried: registry_types::EntryChangeRequest =
        serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retried.entry, x86_request.entry);

    let metadata = workdir.path().join("metadata.json");
    std::fs::write(
        &metadata,
        serde_json::to_vec_pretty(&serde_json::json!({
            "name": {"en": "Example", "de": "Beispiel"},
            "description": {"en": "A complete example app", "de": "Eine Beispiel-App"},
            "tagline": {"en": "Learn by example", "de": "Am Beispiel lernen"},
            "images": {
                "en": [{
                    "kind": "icon",
                    "url": "https://apps.tilo.so/images/example-icon.png",
                    "alt": "Example icon"
                }]
            },
            "supported_languages": ["de", "en"]
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata_update = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/so.tilo.example.tapp",
            "--identity",
            identity.to_str().unwrap(),
            "--metadata",
            metadata.to_str().unwrap(),
            "--action",
            "update",
            "--previous-entry",
            previous.to_str().unwrap(),
            tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        metadata_update.status.success(),
        "metadata update: {metadata_update:?}"
    );
    let localized: registry_types::EntryChangeRequest =
        serde_json::from_slice(&metadata_update.stdout).unwrap();
    registry_types::verify_change_request(&localized, &[publisher]).unwrap();
    assert_eq!(localized.entry.versions.len(), 1);
    assert_eq!(localized.entry.name.resolve("de-DE"), Some("Beispiel"));
    assert_eq!(localized.entry.supported_languages, ["de", "en"]);

    std::fs::write(
        &metadata,
        serde_json::to_vec(&serde_json::json!({
            "name": {"de": "Beispiel"},
            "description": {"de": "Eine Beispiel-App"},
            "tagline": {"de": "Am Beispiel lernen"},
            "supported_languages": ["de"]
        }))
        .unwrap(),
    )
    .unwrap();
    let invalid_metadata = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/so.tilo.example.tapp",
            "--identity",
            identity.to_str().unwrap(),
            "--metadata",
            metadata.to_str().unwrap(),
            "--action",
            "update",
            "--previous-entry",
            previous.to_str().unwrap(),
            tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!invalid_metadata.status.success());

    let missing_previous = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/so.tilo.example.tapp",
            "--identity",
            identity.to_str().unwrap(),
            "--action",
            "update",
            tapp.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!missing_previous.status.success());
}
