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
        // This process stands in for each device's daemon while its identity is provisioned: the
        // key is made and taken first, as the daemon takes it when it starts, and the certificate
        // is issued for that one key. A process that is not the daemon never makes the key.
        let paths = IdentityPaths::under(root);
        tilo_device_identity::persist::load_or_create_device_key(&paths).unwrap();
        let taken = tilo_device_identity::persist::take_device_key(&paths).unwrap();
        ensure_collaboration_identity(identity, &paths, IdentityStorage::Files).unwrap();
        drop(taken);
    }
    // The device's daemon holds its key while a session asks who owns the device: the check
    // reads the device's key from the one process that took it, and this one takes it as the
    // daemon does when it starts.
    let _daemon =
        tilo_device_identity::persist::take_device_key(&IdentityPaths::under(&device)).unwrap();
    assert_eq!(owns(&owner, &device), ExitCode::SUCCESS);
    assert_eq!(owns(&other, &device), ExitCode::FAILURE);
    assert_eq!(owns(&owner, &other_device), ExitCode::FAILURE);
    assert_eq!(owns(&dir.path().join("absent"), &device), ExitCode::FAILURE);
}
