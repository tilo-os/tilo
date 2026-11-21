//! `tilo-vmd device-owner`, which the daemon runs as an account's account: the
//! account's identity records open only for it, never for root.

use std::path::Path;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{IdentityStorage, ensure_collaboration_identity};

fn owns(identity: &Path, device_root: &Path) -> Option<i32> {
    Command::new(tilo_test_env::package_binary!("tilo-vmd"))
        .arg("device-owner")
        .arg("--identity")
        .arg(identity)
        .arg("--device-root")
        .arg(device_root)
        .status_locked()
        .unwrap()
        .code()
}

#[test]
fn only_the_identity_the_device_is_enrolled_under_owns_it() {
    let dir = tempfile::tempdir().unwrap();
    let (owner, device) = (dir.path().join("owner"), dir.path().join("device"));
    let (other, other_device) = (dir.path().join("other"), dir.path().join("other-device"));
    for (identity, root) in [(&owner, &device), (&other, &other_device)] {
        ensure_collaboration_identity(
            identity,
            &IdentityPaths::under(root),
            IdentityStorage::Files,
        )
        .unwrap();
    }
    assert_eq!(owns(&owner, &device), Some(0));
    assert_eq!(owns(&other, &device), Some(1));
    assert_eq!(owns(&owner, &other_device), Some(1));
    assert_eq!(owns(&dir.path().join("absent"), &device), Some(1));
}
