#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory, provision_directory};
#[cfg(target_os = "linux")]
#[path = "support/posix_acl.rs"]
mod posix_acl;

#[test]
fn namespace_control_is_distinct_from_directory_privacy() {
    for mode in [0o755, 0o750, 0o711, 0o700, 0o775, 0o757, 0o777] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(mode)).unwrap();
        fs::create_dir(root.path().join("child")).unwrap(); // Directory link count is not file privacy.
        let held = StorageDirectory::open(root.path()).unwrap();
        assert_eq!(held.validate_namespace_control().is_ok(), mode & 0o022 == 0);
        assert_eq!(
            held.validate_privacy(Privacy::OwnerOnly).is_ok(),
            mode == 0o700
        );
        if mode != 0o700 {
            assert!(provision_directory(root.path(), Privacy::OwnerOnly).is_err());
        }
        assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o777, mode);
    }
}

#[test]
fn namespace_control_rejects_foreign_owned_directory_without_mutation() {
    // An ordinary principal meets a foreign owner at `/`. Root owns `/`, so
    // as root the foreign owner is a directory handed to `nobody`, which root
    // without CAP_CHOWN (a job sandbox) cannot make.
    let effective = rustix::process::geteuid();
    let foreign = if effective.is_root() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::chown_to(65534),
            "root needs a directory owned by nobody"
        );
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        rustix::fs::chown(
            temp.path(),
            Some(rustix::process::Uid::from_raw(65534)),
            None,
        )
        .unwrap();
        Some(temp)
    } else {
        None
    };
    let root = foreign
        .as_ref()
        .map_or_else(|| std::path::Path::new("/"), tempfile::TempDir::path);
    let before = fs::metadata(root).unwrap();
    assert_ne!(
        before.uid(),
        effective.as_raw(),
        "the fixture directory must have a foreign owner"
    );
    assert_eq!(before.mode() & 0o022, 0, "only ownership may be foreign");
    assert!(
        StorageDirectory::open(root)
            .unwrap()
            .validate_namespace_control()
            .is_err()
    );
    let after = fs::metadata(root).unwrap();
    assert_eq!(
        (after.uid(), after.mode(), after.ino()),
        (before.uid(), before.mode(), before.ino())
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_namespace_acl_profile_preserves_independent_private_creation() {
    use std::process::Command;
    let user = Command::new("/usr/bin/id").arg("-un").output().unwrap();
    assert!(user.status.success());
    let user = String::from_utf8(user.stdout).unwrap();
    for (rule, accepted) in [
        (
            format!("user:{} allow read,write,directory_inherit", user.trim()),
            true,
        ),
        ("everyone deny delete".to_owned(), true),
        ("everyone allow read".to_owned(), false),
        (
            "everyone allow read,file_inherit,only_inherit".to_owned(),
            false,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            Command::new("/bin/chmod")
                .args(["+a", &rule])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        let acl = || {
            let output = Command::new("/bin/ls")
                .args(["-lde"])
                .arg(root.path())
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .skip(1)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let before = acl();
        let held = StorageDirectory::open(root.path()).unwrap();
        assert_eq!(
            held.validate_namespace_control().is_ok(),
            accepted,
            "{rule}"
        );
        assert!(held.validate_privacy(Privacy::OwnerOnly).is_err());
        let result = provision_directory(&root.path().join("private"), Privacy::OwnerOnly);
        assert_eq!(result.is_ok(), accepted, "{rule}");
        if !accepted {
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        }
        assert_eq!(acl(), before);
        assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o777, 0o755);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_named_acl_namespace_write_mask_is_rejected() {
    let owner = rustix::process::geteuid().as_raw();
    let foreign = if owner == 65534 { 65533 } else { 65534 };
    for (access, accepted) in [("r-x", true), ("rwx", false)] {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        posix_acl::modify(root.path(), &format!("u:{foreign}:{access}"));
        let before = fs::metadata(root.path()).unwrap();
        assert_eq!(
            StorageDirectory::open(root.path())
                .unwrap()
                .validate_namespace_control()
                .is_ok(),
            accepted
        );
        assert_eq!(fs::metadata(root.path()).unwrap().mode(), before.mode());
    }
}
