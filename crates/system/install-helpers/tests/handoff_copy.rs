//! The setup handoff copy keeps installed directory metadata, starts new files
//! private and refuses every link.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_install_helpers::handoff_copy::copy_tree;

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().mode() & 0o7777
}

fn mkdir(path: &Path, mode: u32) {
    DirBuilder::new().mode(mode).create(path).unwrap();
}

#[cfg(target_os = "linux")]
fn mark(path: &Path, value: &[u8]) {
    rustix::fs::setxattr(
        path,
        "user.test_context",
        value,
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
}

#[cfg(target_os = "linux")]
fn context(path: &Path) -> Vec<u8> {
    let mut buffer = vec![0u8; 64];
    let length = rustix::fs::getxattr(path, "user.test_context", &mut buffer).unwrap();
    buffer.truncate(length);
    buffer
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey through one command reads as one flow"
)]
fn the_setup_handoff_copy() {
    {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let (source, target) = (root.join("staging"), root.join("installed"));
        mkdir(&source, 0o700);
        mkdir(&target, 0o755);
        fs::set_permissions(&target, Permissions::from_mode(0o755)).unwrap();
        mkdir(&source.join("NetworkManager"), 0o700);
        mkdir(&target.join("NetworkManager"), 0o755);
        fs::write(target.join("profile"), "existing shell setup").unwrap();
        fs::set_permissions(target.join("profile"), Permissions::from_mode(0o644)).unwrap();
        fs::write(
            source.join("NetworkManager/profile"),
            "private network fixture",
        )
        .unwrap();
        let before = fs::metadata(&target).unwrap();
        #[cfg(target_os = "linux")]
        {
            mark(&source, b"staging_var_run");
            mark(&target, b"installed_etc");
        }
        copy_tree(&source, &target).unwrap();
        let after = fs::metadata(&target).unwrap();
        assert_eq!(mode(&target), 0o755);
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
        #[cfg(target_os = "linux")]
        assert_eq!(context(&target), b"installed_etc");
        assert_eq!(mode(&target.join("NetworkManager")), 0o755);
        assert_eq!(mode(&target.join("profile")), 0o644);
        assert_eq!(
            fs::read_to_string(target.join("profile")).unwrap(),
            "existing shell setup"
        );
        assert_eq!(mode(&target.join("NetworkManager/profile")), 0o600);
        assert_eq!(
            fs::read_to_string(target.join("NetworkManager/profile")).unwrap(),
            "private network fixture"
        );
    }

    {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let (source, target) = (root.join("source"), root.join("target"));
        mkdir(&source, 0o700);
        mkdir(&source.join("tilo"), 0o700);
        mkdir(&target, 0o755);
        fs::write(source.join("tilo/request"), "fixture").unwrap();
        // The installer runs with a private umask; new directories still open up. The
        // umask is the process's, so the copy runs in its own.
        let status = Command::new("sh")
            .arg("-c")
            .arg("umask 077 && exec \"$0\" \"$@\"")
            .arg(tilo_test_env::package_binary!("tilo-install-handoff-copy"))
            .arg(&source)
            .arg(&target)
            .status_locked()
            .unwrap();
        assert!(status.success());
        assert_eq!(mode(&target.join("tilo")), 0o755);
        assert_eq!(mode(&target.join("tilo/request")), 0o600);
    }

    {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let (source, target) = (root.join("source"), root.join("target"));
        for path in [&source, &target] {
            fs::create_dir(path).unwrap();
        }
        fs::write(source.join("conf"), "new").unwrap();
        fs::write(target.join("conf"), "old and longer").unwrap();
        fs::set_permissions(target.join("conf"), Permissions::from_mode(0o644)).unwrap();
        copy_tree(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(target.join("conf")).unwrap(), "new");
        assert_eq!(mode(&target.join("conf")), 0o600);
    }

    {
        for linked_source in [true, false] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path();
            let (source, target, outside) = (
                root.join("source"),
                root.join("target"),
                root.join("outside"),
            );
            for path in [&source, &target, &outside] {
                fs::create_dir(path).unwrap();
            }
            fs::write(outside.join("private"), "unchanged").unwrap();
            if linked_source {
                std::os::unix::fs::symlink(outside.join("private"), source.join("private"))
                    .unwrap();
            } else {
                fs::write(source.join("private"), "new").unwrap();
                std::os::unix::fs::symlink(outside.join("private"), target.join("private"))
                    .unwrap();
            }
            let error = copy_tree(&source, &target).unwrap_err();
            assert!(error.0.contains("symlinks"), "{error}");
            assert_eq!(
                fs::read_to_string(outside.join("private")).unwrap(),
                "unchanged"
            );
        }
    }

    {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let (source, target) = (root.join("source"), root.join("target"));
        for path in [&source, &target] {
            fs::create_dir(path).unwrap();
        }
        assert_eq!(
            copy_tree(&root.join("missing"), &target).unwrap_err().0,
            "handoff source must be a real directory"
        );
        assert_eq!(
            copy_tree(&source, &root.join("missing")).unwrap_err().0,
            "installed destination must be a real directory"
        );
        fs::write(source.join("entry"), "file").unwrap();
        fs::create_dir(target.join("entry")).unwrap();
        assert_eq!(
            copy_tree(&source, &target).unwrap_err().0,
            "handoff destination is not a regular file"
        );
        fs::remove_file(source.join("entry")).unwrap();
        fs::remove_dir(target.join("entry")).unwrap();
        fs::create_dir(source.join("entry")).unwrap();
        fs::write(target.join("entry"), "file").unwrap();
        assert_eq!(
            copy_tree(&source, &target).unwrap_err().0,
            "installed destination must be a real directory"
        );
    }

    {
        let binary = tilo_test_env::package_binary!("tilo-install-handoff-copy");
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let (source, target) = (root.join("source"), root.join("target"));
        for path in [&source, &target] {
            fs::create_dir(path).unwrap();
        }
        fs::write(source.join("file"), "x").unwrap();
        let status = Command::new(&binary)
            .arg(&source)
            .arg(&target)
            .status_locked()
            .unwrap();
        assert_eq!(status.code(), Some(0));
        assert_eq!(fs::read_to_string(target.join("file")).unwrap(), "x");

        let refused = Command::new(binary)
            .arg(root.join("missing"))
            .arg(&target)
            .output_locked()
            .unwrap();
        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&refused.stderr),
            "tilo-install-handoff-copy: handoff source must be a real directory\n"
        );
    }
}
