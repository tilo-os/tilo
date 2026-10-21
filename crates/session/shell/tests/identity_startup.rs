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
fn provision_identity_fails_closed_without_replacing_metadata_when_secret_service_is_unavailable() {
    let root = TempDir::new().expect("test root");
    let config = root.path().join("config");
    let cache = root.path().join("cache");
    let bin = root.path().join("bin");
    fs::create_dir_all(config.join("tilo/identity")).expect("identity directory");
    fs::create_dir_all(&bin).expect("fake PATH");
    let shader_seed = cache.join("tilo/public-mesa-seed/mesa");
    fs::create_dir_all(&shader_seed).expect("private shader seed directory");
    fs::write(shader_seed.join("seed.json"), b"invalid shader manifest")
        .expect("shader seed negative control");
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
    let identity = config.join("tilo/identity/primary.json");
    let startup_metrics = root.path().join("startup.json");
    let original = serde_json::to_vec_pretty(&metadata).expect("metadata JSON");
    fs::write(&identity, &original).expect("metadata");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).expect("metadata mode");

    let status = Command::new(tilo_test_env::package_binary!("shell"))
        .arg("--provision-identity")
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", &cache)
        .env_remove("TILO_SHADER_SEED_PREPARED")
        .env_remove("MESA_SHADER_CACHE_DISABLE")
        .env("TILO_STARTUP_WARM", "1")
        .env("TILO_STARTUP_WARM_VELLO", "1")
        .env("TILO_STARTUP_METRICS_PATH", &startup_metrics)
        .env("PATH", &bin)
        .output()
        .expect("run production shell binary");
    assert!(
        !status.status.success(),
        "provision-only mode must fail closed when Secret Service is unavailable, stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(fs::read(&identity).expect("read metadata"), original);
    assert!(
        !String::from_utf8_lossy(&status.stderr).contains("session shader seed unavailable"),
        "a non-rendering identity helper must not inspect the shader seed"
    );
    let metrics: serde_json::Value =
        serde_json::from_slice(&fs::read(startup_metrics).expect("production startup metrics"))
            .expect("startup metrics JSON");
    let events = metrics["events"]
        .as_array()
        .expect("recorded startup events");
    assert!(events.iter().any(|event| event["name"] == "main-start"));
    assert!(
        !events.iter().any(|event| event["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("warmup-"))),
        "a non-rendering identity helper must not start font or GPU warmup: {metrics}"
    );
}
