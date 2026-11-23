//! The `device-owner` helper, which a step runs as an account's user: the account's identity
//! records open only for it, never for root.

use std::path::Path;
use std::process::ExitCode;

use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{IdentityStorage, ensure_collaboration_identity};

fn owns(identity: &Path, device_root: &Path) -> ExitCode {
    tilo_device_steps::step::main(vec![
        "device-owner".into(),
        "--identity".into(),
        identity.into(),
        "--device-root".into(),
        device_root.into(),
    ])
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
    assert_eq!(owns(&owner, &device), ExitCode::SUCCESS);
    assert_eq!(owns(&other, &device), ExitCode::FAILURE);
    assert_eq!(owns(&owner, &other_device), ExitCode::FAILURE);
    assert_eq!(owns(&dir.path().join("absent"), &device), ExitCode::FAILURE);
}
