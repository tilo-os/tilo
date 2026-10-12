//! What an out-of-band join publishes stays readable through the device-trust
//! group, whatever umask `tilo-enroll install-cert` runs under.
//!
//! On an installed system the device-trust directories are set-group-ID and
//! shared with the session account through their group. `install-cert` is run
//! as root by `sudo` from the person's shell, and `sudo` keeps that shell's
//! umask: a restrictive one must not leave a certificate only root can read.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::process::Command;

use tilo_user_identity::IdentityPaths;

const ENROLL: &str = env!("CARGO_BIN_EXE_tilo-enroll");

/// Run `tilo-enroll` with `umask`, returning its standard output.
fn enroll(umask: &str, args: &[&std::ffi::OsStr]) -> String {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("umask {umask} && exec \"$0\" \"$@\""))
        .arg(ENROLL)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tilo-enroll {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn shared_directory(path: &Path, mode: u32) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

#[test]
fn an_out_of_band_join_under_a_private_umask_stays_group_readable() {
    let scope = tempfile::tempdir().unwrap();
    let identity_root = scope.path().join("tilo");
    let identity_dir = scope.path().join("identity");
    let work = scope.path().join("work");
    // The installed device-trust layout: group-shared, set-group-ID.
    shared_directory(&identity_root, 0o755);
    shared_directory(&identity_root.join("user-identity"), 0o2770);
    shared_directory(&identity_root.join("device"), 0o2770);
    shared_directory(&identity_root.join("device-private"), 0o2750);
    shared_directory(&identity_dir, 0o700);
    shared_directory(&work, 0o700);
    let paths = IdentityPaths::under(&identity_root);

    let device_key = enroll(
        "0027",
        &[
            "device-key".as_ref(),
            "--identity-root".as_ref(),
            identity_root.as_os_str(),
        ],
    );
    let device: serde_json::Value = serde_json::from_str(device_key.trim()).unwrap();
    let device = device["device_public_key"].as_str().unwrap().to_owned();

    let root = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
    let root_key = work.join("root.key");
    tilo_user_identity::persist::write_signing_key(&root_key, &root).unwrap();
    let member_cert = work.join("member.cert");
    enroll(
        "077",
        &[
            "certify".as_ref(),
            "--root-key".as_ref(),
            root_key.as_os_str(),
            "--device".as_ref(),
            device.as_ref(),
            "--label".as_ref(),
            "Paired laptop".as_ref(),
            "--capability".as_ref(),
            "own".as_ref(),
            "--out".as_ref(),
            member_cert.as_os_str(),
        ],
    );
    let installed = enroll(
        "077",
        &[
            "install-cert".as_ref(),
            "--identity-root".as_ref(),
            identity_root.as_os_str(),
            "--identity-dir".as_ref(),
            identity_dir.as_os_str(),
            "--cert".as_ref(),
            member_cert.as_os_str(),
        ],
    );
    assert!(installed.starts_with("installed "), "{installed}");

    let per_user_cert = paths.device_cert_for_user(&root.verifying_key().to_bytes());
    let certs = per_user_cert.parent().unwrap();
    let device_dir = identity_root.join("device");
    assert_eq!(
        mode(certs),
        0o2770,
        "the per-identity certificate directory is shared like its parent"
    );
    for file in [&paths.device_cert, &per_user_cert, &paths.user_pubkey] {
        assert_eq!(
            mode(file),
            0o644,
            "{} is readable through its directory's group",
            file.display()
        );
    }
    let group = std::fs::metadata(&device_dir).unwrap().gid();
    for path in [certs, per_user_cert.as_path(), paths.device_cert.as_path()] {
        assert_eq!(
            std::fs::metadata(path).unwrap().gid(),
            group,
            "{} keeps the device-trust group",
            path.display()
        );
    }
    assert_eq!(
        std::fs::metadata(&paths.user_pubkey).unwrap().gid(),
        std::fs::metadata(identity_root.join("user-identity"))
            .unwrap()
            .gid()
    );
}
