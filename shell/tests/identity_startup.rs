//! Production-binary coverage for identity startup policy.
//!
//! Unit tests compile the shell library with `cfg(test)`, where identity
//! imports intentionally use the broker-file provider. This test executes the
//! shell binary itself and makes a schema-2 Secret Service record unavailable,
//! matching the failure seen on a fresh display-manager login.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn provision_identity_does_not_replace_metadata_when_secret_service_is_unavailable() {
    let root = TempDir::new().expect("test root");
    let config = root.path().join("config");
    let bin = root.path().join("bin");
    fs::create_dir_all(config.join("qos/identity")).expect("identity directory");
    fs::create_dir_all(&bin).expect("fake PATH");
    let secret_tool = bin.join("secret-tool");
    fs::write(&secret_tool, b"#!/bin/sh\nexit 1\n").expect("fake secret-tool");
    fs::set_permissions(&secret_tool, fs::Permissions::from_mode(0o755))
        .expect("fake secret-tool mode");

    let metadata = serde_json::json!({
        "display_name": "existing",
        "avatar_color": [1, 2, 3],
        "threshold_identity": {
            "single_device": true,
            "group_public_key": vec![0u8; 32],
            "threshold": 1,
            "total_shares": 1,
            "share_index": 1,
            "secret_bytes": [],
        },
        "created_at": "2026-01-01T00:00:00Z",
        "schema_version": 2,
        "secret_ref": {
            "backend": "secret-service-v1",
            "locator": "00".repeat(32),
        },
    });
    let identity = config.join("qos/identity/primary.json");
    let original = serde_json::to_vec_pretty(&metadata).expect("metadata JSON");
    fs::write(&identity, &original).expect("metadata");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).expect("metadata mode");

    let status = Command::new(env!("CARGO_BIN_EXE_shell"))
        .arg("--provision-identity")
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", &config)
        .env("QOS_STARTUP_WARM", "off")
        .env("PATH", &bin)
        .output()
        .expect("run production shell binary");
    assert!(
        status.status.success(),
        "unavailable Secret Service must defer to trusted recovery, stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(fs::read(&identity).expect("read metadata"), original);
}
