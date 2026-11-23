//! The `tilo-account-credential` binary through the real process boundary: what it says of the
//! protected-grant protocol it speaks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilo_test_env::SpawnUnderEnvLock as _;

fn helper() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-account-credential")
}

#[test]
fn capability_probe_runs_without_creating_ownership_and_rejects_extra_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(helper())
        .arg("capabilities")
        .env("HOME", directory.path())
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .output_locked()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"schema":1,"protected_grant_protocol":1}),
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    let refused = std::process::Command::new(helper())
        .args(["capabilities", "--config-root", "ignored"])
        .output_locked()
        .unwrap();
    assert_eq!(refused.status.code(), Some(64));
    assert!(refused.stdout.is_empty());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
