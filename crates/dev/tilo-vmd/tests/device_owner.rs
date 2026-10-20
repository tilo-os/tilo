//! `tilo-vmd device-owner`, which the daemon runs as a slot's account: the
//! account's identity records open only for it, never for root.
#![allow(clippy::unwrap_used, reason = "fixture setup fails the test")]

use std::path::Path;
use std::process::Command;

use tilo_identity_host::provisioning::{IdentityStorage, ensure_collaboration_identity};
use tilo_user_identity::IdentityPaths;

fn owns(identity: &Path, device_root: &Path) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_tilo-vmd"))
        .arg("device-owner")
        .arg("--identity")
        .arg(identity)
        .arg("--device-root")
        .arg(device_root)
        .status()
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
