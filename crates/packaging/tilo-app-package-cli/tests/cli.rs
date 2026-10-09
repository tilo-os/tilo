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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn cli_pack_inspect_unpack_and_verify_round_trip() {
    // phase: cli_pack_then_verify
    {
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
    // phase: cli_pack_rejects_oversized_app_atomically
    {
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
    // phase: cli_inspect_lists_payload
    {
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
    // phase: cli_unpack_round_trips
    {
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

fn leb(mut value: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn custom_section(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = leb(name.len());
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(payload);
    let mut out = vec![0];
    out.extend(leb(body.len()));
    out.extend(body);
    out
}

/// A side module built against toolkit-core ABI `abi`.
fn side_module(abi: &str) -> Vec<u8> {
    let mut module = b"\0asm\x01\0\0\0".to_vec();
    module.extend(custom_section("tilo.runtime_abi", abi.as_bytes()));
    let mut features = leb(2);
    for name in ["sign-ext", "bulk-memory"] {
        features.push(b'+');
        features.extend(leb(name.len()));
        features.extend_from_slice(name.as_bytes());
    }
    module.extend(custom_section("target_features", &features));
    module
}

fn run(args: &[&str]) -> std::process::Output {
    let out = Command::new(bin_path()).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn cli_packs_a_web_build_and_emits_its_registry_request() {
    let abi = "tilo-runtime/2025.06+wasm.00112233445566778899aabbccddeeff";
    let workdir = tempfile::tempdir().unwrap();
    let path = |name: &str| workdir.path().join(name).to_str().unwrap().to_string();
    run(&["keygen", "--out", &path("publisher.json")]);
    std::fs::write(
        path("manifest.toml"),
        sample_manifest_with(&"00".repeat(32)),
    )
    .unwrap();
    std::fs::write(path("example.wasm"), side_module(abi)).unwrap();
    std::fs::create_dir_all(workdir.path().join("fonts/latin")).unwrap();
    std::fs::write(workdir.path().join("fonts/latin/body.ttf"), [1; 64]).unwrap();
    std::fs::write(path("icon.svg"), b"<svg/>").unwrap();

    run(&[
        "pack-web",
        "--manifest",
        &path("manifest.toml"),
        "--module",
        &path("example.wasm"),
        "--asset-dir",
        &path("fonts"),
        "--asset",
        &path("icon.svg"),
        "--identity",
        &path("publisher.json"),
        "-o",
        &path("example-web.tapp"),
    ]);
    let inspected = run(&["inspect", &path("example-web.tapp")]);
    let inspected = String::from_utf8_lossy(&inspected.stdout);
    assert!(inspected.contains("platform = \"web\""), "{inspected}");
    assert!(inspected.contains(abi), "{inspected}");
    assert!(inspected.contains("assets/latin/body.ttf"), "{inspected}");
    assert!(inspected.contains("assets/icon.svg"), "{inspected}");
    run(&["verify", &path("example-web.tapp")]);

    let emitted = run(&[
        "entry",
        "--url",
        "https://apps.tilo.so/artifacts/so.tilo.example-web.tapp",
        "--identity",
        &path("publisher.json"),
        "--platform",
        "web",
        "--arch",
        "wasm32",
        "--action",
        "add",
        &path("example-web.tapp"),
    ]);
    let request: registry_types::EntryChangeRequest =
        serde_json::from_slice(&emitted.stdout).unwrap();
    let key = hex::decode(&request.entry.publisher_pubkey_hex).unwrap();
    registry_types::verify_change_request(&request, &[key.try_into().unwrap()]).unwrap();
    let target = &request.entry.versions[0].target;
    assert_eq!(target.platform, registry_types::WEB_PLATFORM);
    assert_eq!(target.architecture, registry_types::WASM32_ARCHITECTURE);
    assert_eq!(target.isa, "wasm32-mvp+bulk-memory+sign-ext");
    assert_eq!(target.runtime.as_deref(), Some(abi));

    let native_as_web = Command::new(bin_path())
        .args([
            "entry",
            "--url",
            "https://apps.tilo.so/artifacts/x.tapp",
            "--identity",
            &path("publisher.json"),
            "--platform",
            "web",
            "--arch",
            "x86_64",
            &path("example-web.tapp"),
        ])
        .output()
        .unwrap();
    assert!(!native_as_web.status.success());
}

#[test]
fn cli_packs_a_macos_build_and_emits_its_registry_request() {
    let workdir = tempfile::tempdir().unwrap();
    let path = |name: &str| workdir.path().join(name).to_str().unwrap().to_string();
    run(&["keygen", "--out", &path("publisher.json")]);
    std::fs::write(
        path("manifest.toml"),
        sample_manifest_with(&"00".repeat(32)),
    )
    .unwrap();
    std::fs::write(path("hello"), b"\xcf\xfa\xed\xfe").unwrap();

    run(&[
        "pack-desktop",
        "--manifest",
        &path("manifest.toml"),
        "--executable",
        &path("hello"),
        "--platform",
        "macos",
        "--arch",
        "aarch64",
        "--identity",
        &path("publisher.json"),
        "-o",
        &path("hello-macos.tapp"),
    ]);
    let inspected = run(&["inspect", &path("hello-macos.tapp")]);
    let inspected = String::from_utf8_lossy(&inspected.stdout);
    assert!(inspected.contains("platform = \"macos\""), "{inspected}");
    assert!(inspected.contains("module = \"bin/hello\""), "{inspected}");
    run(&["verify", &path("hello-macos.tapp")]);

    let entry = |platform: &str, arch: &str| {
        Command::new(bin_path())
            .args([
                "entry",
                "--url",
                "https://apps.tilo.so/artifacts/hello-macos.tapp",
                "--identity",
                &path("publisher.json"),
                "--platform",
                platform,
                "--arch",
                arch,
                "--action",
                "add",
                &path("hello-macos.tapp"),
            ])
            .output()
            .unwrap()
    };
    let emitted = entry("macos", "aarch64");
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let request: registry_types::EntryChangeRequest =
        serde_json::from_slice(&emitted.stdout).unwrap();
    let key = hex::decode(&request.entry.publisher_pubkey_hex).unwrap();
    registry_types::verify_change_request(&request, &[key.try_into().unwrap()]).unwrap();
    let target = &request.entry.versions[0].target;
    assert_eq!(target.platform, registry_types::MACOS_PLATFORM);
    assert_eq!(target.architecture, "aarch64");
    assert_eq!(target.isa, "aarch64");
    assert_eq!(target.runtime.as_deref(), Some("tilo-runtime/2025.06"));

    assert!(!entry("windows", "aarch64").status.success());
    assert!(!entry("macos", "x86_64").status.success());
    assert!(!entry("tilo-os", "aarch64").status.success());
}

#[test]
fn cli_splits_a_runtime_and_describes_its_thin_form_with_the_objects() {
    let workdir = tempfile::tempdir().unwrap();
    let path = |name: &str| workdir.path().join(name).to_str().unwrap().to_string();
    run(&["keygen", "--out", &path("publisher.json")]);
    std::fs::write(
        path("manifest.toml"),
        format!(
            r#"
[package]
id = "so.tilo.runtime"
version = "2026.7.0"
kind = "runtime"
provides = "tilo-runtime/2026.07"

[package.isa]
x86_64 = "v3"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
            "00".repeat(32)
        ),
    )
    .unwrap();
    let chunk = usize::try_from(tilo_app_package::chunks::CHUNK_BYTES).unwrap();
    std::fs::create_dir_all(workdir.path().join("payload/lib")).unwrap();
    let library: Vec<u8> = (0..chunk + 4096)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    std::fs::write(workdir.path().join("payload/lib/libruntime.so"), &library).unwrap();
    std::fs::write(workdir.path().join("payload/bundle.toml"), b"abi = 1\n").unwrap();
    run(&[
        "pack",
        "--manifest",
        &path("manifest.toml"),
        "--payload",
        &path("payload"),
        "--identity",
        &path("publisher.json"),
        "-o",
        &path("runtime.tapp"),
    ]);
    run(&["split", &path("runtime.tapp"), "--out", &path("release")]);
    let hash = tilo_app_package::BlobHash::of(&library);
    let objects = workdir.path().join("release");
    assert!(objects.join(format!("{hash}.chunks")).exists());
    assert!(!objects.join(format!("{hash}.blob")).exists());
    let thin = std::fs::read(objects.join("runtime.tapp")).unwrap();
    assert!(thin.len() < 64 * 1024, "thin form is {} bytes", thin.len());

    let entry_args = |with_objects: bool| {
        let mut args = vec![
            "entry".to_string(),
            "--url".into(),
            "https://apps.tilo.so/artifacts/tilo/generic/tilo-app-runtime/2026.7.0/runtime.tapp"
                .into(),
            "--identity".into(),
            path("publisher.json"),
            "--arch".into(),
            "x86_64".into(),
        ];
        if with_objects {
            args.extend(["--objects".into(), path("release")]);
        }
        args.push(path("release/runtime.tapp"));
        Command::new(bin_path()).args(&args).output().unwrap()
    };
    let emitted = entry_args(true);
    assert!(emitted.status.success(), "entry: {emitted:?}");
    let entry: registry_types::RegistryEntry = serde_json::from_slice(&emitted.stdout).unwrap();
    let version = &entry.versions[0];
    assert_eq!(version.tapp_size_bytes, thin.len() as u64);
    assert_eq!(
        version.target.runtime.as_deref(),
        Some("tilo-runtime/2026.07")
    );

    assert!(
        !entry_args(false).status.success(),
        "a thin tapp needs its objects"
    );
    let list: tilo_app_package::chunks::ChunkList =
        serde_json::from_slice(&std::fs::read(objects.join(format!("{hash}.chunks"))).unwrap())
            .unwrap();
    std::fs::write(
        objects.join(format!("{}.blob", list.chunks[1].hash)),
        b"tampered",
    )
    .unwrap();
    let refused = entry_args(true);
    assert!(!refused.status.success(), "a tampered chunk must fail");
}

struct AttestFixture {
    dir: tempfile::TempDir,
    publisher: PathBuf,
    attestor: PathBuf,
    attestor_public: String,
    original: PathBuf,
}

impl AttestFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let publisher = dir.path().join("publisher.json");
        let attestor = dir.path().join("attestor.json");
        let generate = |path: &std::path::Path| {
            let output = Command::new(bin_path())
                .arg("keygen")
                .arg("--out")
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let public = generate(&publisher);
        let attestor_public = generate(&attestor);
        let manifest = dir.path().join("manifest.toml");
        std::fs::write(&manifest, sample_manifest_with(&public)).unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(
            payload.join("bin/hello"),
            b"#!/bin/sh\necho owned-fixture\n",
        )
        .unwrap();
        let original = dir.path().join("original.tapp");
        let output = Command::new(bin_path())
            .arg("pack")
            .arg("--manifest")
            .arg(manifest)
            .arg("--payload")
            .arg(payload)
            .arg("--identity")
            .arg(&publisher)
            .arg("--output")
            .arg(&original)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            dir,
            publisher,
            attestor,
            attestor_public,
            original,
        }
    }

    fn command(&self, input: &std::path::Path, output: &std::path::Path, scope: &str) -> Command {
        let mut command = Command::new(bin_path());
        command
            .arg("attest")
            .arg("--publisher-identity")
            .arg(&self.publisher)
            .arg("--attestor-identity")
            .arg(&self.attestor)
            .arg("--attestor")
            .arg("owned-fixture")
            .arg("--scope")
            .arg(scope)
            .arg(input)
            .arg("--output")
            .arg(output);
        command
    }
}

fn decoded(
    path: &std::path::Path,
) -> (
    tilo_app_package::Tapp,
    std::collections::BTreeMap<tilo_app_package::BlobHash, Vec<u8>>,
) {
    let mut blobs = std::collections::BTreeMap::new();
    let package =
        tilo_app_package::tapp::read(std::fs::File::open(path).unwrap(), &mut blobs).unwrap();
    (package, blobs)
}

#[test]
fn cli_attest_preserves_payload_and_original_publisher_with_scoped_recipient_trust() {
    use tilo_app_package::trust::{AttestorScope, RevocationList, TrustedAttestor};
    let fixture = AttestFixture::new();
    let destination = fixture.dir.path().join("attested.tapp");
    let before = std::fs::read(&fixture.original).unwrap();
    let output = fixture
        .command(&fixture.original, &destination, "prefix:so.tilo.example")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (original, original_blobs) = decoded(&fixture.original);
    let (attested, attested_blobs) = decoded(&destination);
    assert_eq!(std::fs::read(&fixture.original).unwrap(), before);
    assert_eq!(original_blobs, attested_blobs);
    assert_eq!(
        original.meta.collection_bytes,
        attested.meta.collection_bytes
    );
    assert_eq!(
        original.meta.root.payload_root_hash,
        attested.meta.root.payload_root_hash
    );
    assert_eq!(
        original.meta.root.publisher_identity,
        attested.meta.root.publisher_identity
    );
    assert_ne!(original.meta.id, attested.meta.id);
    assert!(attested.is_complete());
    let trust = TrustedAttestor {
        id: "owned-fixture".into(),
        pubkey_hex: fixture.attestor_public,
        scope: AttestorScope::parse("prefix:so.tilo.example").unwrap(),
    };
    assert!(matches!(
        tilo_app_package::verify_with_trust(
            &attested.meta,
            &[],
            std::slice::from_ref(&trust),
            &RevocationList::empty()
        ),
        tilo_app_package::Verification::AttestedBy { .. }
    ));
    let revoked = tilo_app_package::trust::RevocationList {
        revoked: vec![tilo_app_package::trust::RevokedKey {
            pubkey_hex: hex::encode(attested.meta.root.publisher_identity),
            reason: "withdrawn publisher".into(),
        }],
    };
    assert!(matches!(
        tilo_app_package::verify_with_trust(
            &attested.meta,
            &[],
            std::slice::from_ref(&trust),
            &revoked
        ),
        tilo_app_package::Verification::Revoked { .. }
    ));
    let mut unknown = trust.clone();
    unknown.pubkey_hex = hex::encode(original.meta.root.publisher_identity);
    assert!(matches!(
        tilo_app_package::verify_with_trust(
            &attested.meta,
            &[],
            &[unknown],
            &RevocationList::empty()
        ),
        tilo_app_package::Verification::UntrustedPublisher { .. }
    ));
    let foreign = TrustedAttestor {
        scope: AttestorScope::parse("prefix:foreign.").unwrap(),
        ..trust
    };
    assert!(matches!(
        tilo_app_package::verify_with_trust(
            &attested.meta,
            &[],
            &[foreign],
            &RevocationList::empty()
        ),
        tilo_app_package::Verification::UntrustedPublisher { .. }
    ));
}

#[test]
fn cli_attest_refuses_foreign_publisher_scope_partial_and_existing_output() {
    let mut fixture = AttestFixture::new();
    let destination = fixture.dir.path().join("attested.tapp");
    let output = fixture
        .command(&fixture.original, &destination, "prefix:foreign.")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!destination.exists());
    let publisher = fixture.publisher.clone();
    fixture.publisher = fixture.attestor.clone();
    assert!(
        !fixture
            .command(&fixture.original, &destination, "all")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!destination.exists());
    fixture.publisher = publisher;
    let thin = fixture.dir.path().join("thin.tapp");
    assert!(
        Command::new(bin_path())
            .arg("thin")
            .arg(&fixture.original)
            .arg("--output")
            .arg(&thin)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !fixture
            .command(&thin, &destination, "all")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!destination.exists());
    std::fs::write(&destination, b"preserved").unwrap();
    assert!(
        !fixture
            .command(&fixture.original, &destination, "all")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"preserved");
    assert!(
        !fixture
            .command(&fixture.original, &fixture.original, "all")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(decoded(&fixture.original).0.is_complete());
}
