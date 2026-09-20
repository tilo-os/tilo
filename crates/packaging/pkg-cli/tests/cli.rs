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

struct EntryFixture {
    workdir: tempfile::TempDir,
}

impl EntryFixture {
    fn new() -> Self {
        let fixture = Self {
            workdir: tempfile::tempdir().unwrap(),
        };
        let keygen = Command::new(bin_path())
            .arg("keygen")
            .arg("--out")
            .arg(fixture.path("publisher.json"))
            .output()
            .unwrap();
        assert!(keygen.status.success(), "keygen: {keygen:?}");
        std::fs::create_dir_all(fixture.path("payload/bin")).unwrap();
        std::fs::write(fixture.path("payload/bin/hello"), b"hi\n").unwrap();
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.workdir.path().join(name)
    }

    fn pack(&self, version: &str) {
        std::fs::write(
            self.path("manifest.toml"),
            sample_manifest_with(&"00".repeat(32)).replace(
                "version     = \"0.1.0\"",
                &format!("version     = \"{version}\""),
            ),
        )
        .unwrap();
        let packed = Command::new(bin_path())
            .arg("pack")
            .arg("--manifest")
            .arg(self.path("manifest.toml"))
            .arg("--payload")
            .arg(self.path("payload"))
            .arg("--identity")
            .arg(self.path("publisher.json"))
            .arg("-o")
            .arg(self.path("example.qpkg"))
            .output()
            .unwrap();
        assert!(packed.status.success(), "pack: {packed:?}");
    }

    fn entry(&self, action: &str, previous: bool) -> std::process::Output {
        let mut command = Command::new(bin_path());
        command
            .args([
                "entry",
                "--url",
                "https://packages.example/dev.qpkg",
                "--action",
                action,
            ])
            .arg("--identity")
            .arg(self.path("publisher.json"));
        if previous {
            command
                .arg("--previous-entry")
                .arg(self.path("previous-entry.json"));
        }
        command.arg(self.path("example.qpkg")).output().unwrap()
    }

    fn save_previous(&self, request: &registry_types::EntryChangeRequest) {
        std::fs::write(
            self.path("previous-entry.json"),
            serde_json::to_vec(&request.entry).unwrap(),
        )
        .unwrap();
    }
}

fn verified_entry(output: &std::process::Output) -> registry_types::EntryChangeRequest {
    assert!(output.status.success(), "entry: {output:?}");
    let request: registry_types::EntryChangeRequest =
        serde_json::from_slice(&output.stdout).unwrap();
    let key = hex::decode(&request.entry.publisher_pubkey_hex).unwrap();
    let mut publisher = [0_u8; 32];
    publisher.copy_from_slice(&key);
    registry_types::verify_change_request(&request, &[publisher]).unwrap();
    request
}

#[test]
fn cli_entry_emits_a_server_acceptable_signed_change() {
    let fixture = EntryFixture::new();
    fixture.pack("0.1.0");
    let request = verified_entry(&fixture.entry("add", false));
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
    fixture.save_previous(&request);
    let retry = verified_entry(&fixture.entry("update", true));
    assert_eq!(retry.entry, request.entry);

    // Continuous versions advance numerically; hash metadata never ranks them.
    for dev_version in ["0.1.1-dev.9+gffff", "0.1.1-dev.10+gaaaa"] {
        fixture.pack(dev_version);
        let updated = verified_entry(&fixture.entry("update", true));
        assert_eq!(updated.entry.versions[0].version, dev_version);
        fixture.save_previous(&updated);
    }
    // A new hash at the same semantic sequence is a collision, not an update.
    fixture.pack("0.1.1-dev.10+gzzzz");
    let collision = fixture.entry("update", true);
    assert!(!collision.status.success());
    assert!(String::from_utf8_lossy(&collision.stderr).contains("different package bytes"));
    assert!(!fixture.entry("update", false).status.success());
}
