#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "fixture setup and production assertions fail the test"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tilo_identity_host::provisioning::{IdentityStorage, ensure_collaboration_identity};
use tilo_user_identity::IdentityPaths;

fn chown_tree(path: &Path, uid: u32, gid: u32) {
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: valid terminated path; only the owned fixture's metadata changes.
    assert_eq!(unsafe { libc::chown(name.as_ptr(), uid, gid) }, 0);
    if path.is_dir() {
        for child in std::fs::read_dir(path).unwrap() {
            chown_tree(&child.unwrap().path(), uid, gid);
        }
    }
}

struct OwnerFixture {
    dir: tempfile::TempDir,
    identity: PathBuf,
    paths: IdentityPaths,
}

impl OwnerFixture {
    fn new() -> Self {
        // This proof needs an actual root parent and an actual different UID,
        // not a skipped assertion or an identity-loader mock.
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "cannot observe root-to-owner credentials"
        );
        let dir = tempfile::Builder::new()
            .prefix("vmd-owner-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let identity = dir.path().join("owner/tilo/identity");
        let paths = IdentityPaths::under(dir.path().join("system"));
        ensure_collaboration_identity(&identity, &paths, IdentityStorage::Files).unwrap();
        chown_tree(&dir.path().join("owner"), 65534, 65534);
        chown_tree(&dir.path().join("system"), 0, 65534);
        std::fs::set_permissions(
            &paths.device_privkey,
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        Self {
            dir,
            identity,
            paths,
        }
    }

    fn inspect(&self, uid: u32) -> std::io::Result<bool> {
        tilo_vmd::owner_identity::inspect_as_user(
            Path::new(env!("CARGO_BIN_EXE_tilo-vmd")),
            "nobody",
            uid,
            &self.identity,
            &self.dir.path().join("system"),
        )
    }
}

#[test]
fn root_guard_accepts_actual_account_owned_identity_through_credential_dropped_inspector() {
    let fixture = OwnerFixture::new();
    assert!(fixture.inspect(65534).expect("actual owner inspection"));
    assert_eq!(unsafe { libc::geteuid() }, 0, "parent credentials changed");
}

#[test]
fn credential_dropped_owner_inspection_refuses_wrong_account_and_tampered_certificate() {
    let fixture = OwnerFixture::new();
    assert!(
        fixture.inspect(0).is_err(),
        "wrong expected account was admitted"
    );
    let mut certificate =
        tilo_user_identity::persist::read_device_cert(&fixture.paths.device_cert).unwrap();
    certificate.device_label = "tampered".into();
    tilo_user_identity::persist::write_device_cert(&fixture.paths.device_cert, &certificate)
        .unwrap();
    assert!(
        fixture.inspect(65534).is_err(),
        "invalid owner certificate was admitted"
    );
    assert_eq!(unsafe { libc::geteuid() }, 0, "parent credentials changed");
}

#[test]
fn owner_inspector_reads_actual_account_selector_and_refuses_shared_identity() {
    let fixture = OwnerFixture::new();
    let shared = fixture.dir.path().join("shared/tilo/identity");
    ensure_collaboration_identity(&shared, &fixture.paths, IdentityStorage::Files).unwrap();
    std::fs::copy(
        shared.join("primary.json"),
        fixture.identity.join("shared.json"),
    )
    .unwrap();
    let active = fixture.identity.join("active");
    std::fs::write(&active, "shared\n").unwrap();
    std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o600)).unwrap();
    chown_tree(&fixture.identity, 65534, 65534);
    assert!(
        !fixture.inspect(65534).unwrap(),
        "shared identity admitted as device owner"
    );
    std::fs::write(&active, "primary\n").unwrap();
    assert!(
        fixture.inspect(65534).unwrap(),
        "account selector did not restore owner"
    );
    chown_tree(&fixture.identity.join("primary.json"), 0, 0);
    assert!(
        fixture.inspect(65534).is_err(),
        "foreign-owned record was admitted"
    );
}

#[test]
fn owner_inspector_binds_certificate_to_actual_protected_device_key() {
    let fixture = OwnerFixture::new();
    std::fs::write(&fixture.paths.device_privkey, [42_u8; 32]).unwrap();
    assert!(
        !fixture.inspect(65534).unwrap(),
        "certificate for another device was admitted"
    );
}
