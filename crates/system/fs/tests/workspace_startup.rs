//! The recovery guard must reject before any production identity or transport
//! startup can create persistent state. These tests invoke the shipped CLI.
use std::process::Command;

#[test]
fn recovery_only_launch_does_not_create_a_workspace_or_identity() {
    let dir = tempfile::tempdir().expect("isolated fixture");
    let cache = dir.path().join("cache");
    let mount = dir.path().join("mount");
    let identity = dir.path().join("identity");
    let output = Command::new(env!("CARGO_BIN_EXE_fs"))
        .env("TILO_IDENTITY_ROOT", &identity)
        .args(["serve", "--require-existing-workspace", "--cache-dir"])
        .arg(&cache)
        .arg("--mount-point")
        .arg(&mount)
        .output()
        .expect("run production Files CLI");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no saved Files namespace"));
    assert!(!cache.exists());
    assert!(!mount.exists());
    assert!(!identity.exists());
}

#[test]
fn status_is_read_only_and_does_not_require_identity() {
    let dir = tempfile::tempdir().expect("isolated fixture");
    let cache = dir.path().join("cache");
    let output = Command::new(env!("CARGO_BIN_EXE_fs"))
        .env("TILO_IDENTITY_ROOT", dir.path().join("identity"))
        .args(["workspace-status", "--cache-dir"])
        .arg(&cache)
        .output()
        .expect("run production Files CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("public status JSON");
    assert_eq!(status["can_resume_existing"], false);
    assert_eq!(status["saved_namespace"], false);
    assert!(!cache.exists());
    assert!(!dir.path().join("identity").exists());
}
