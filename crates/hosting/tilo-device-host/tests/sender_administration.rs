#![cfg(any(target_os = "macos", target_os = "linux"))]

tilo_automerge_isolate::test_worker!();

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

/// A fresh temporary directory on a filesystem the private owner profile
/// admits on Linux (ext4, btrfs, XFS or tmpfs) with the free space the
/// session Store requires.
///
/// CI's test job runs in a container whose root filesystem, and so its
/// default temporary directory, is overlayfs, which the bounded owner profile
/// refuses by design (`unsupported`). There the fixture falls back to the
/// build's target directory on the host-backed checkout (as main's qualified
/// imager fixtures do), then to `/dev/shm`. The explicit XFS qualification
/// root remains `TILO_PRIVATE_ACTIONS_TEST_ROOT`.
fn admitted_temp_dir() -> tempfile::TempDir {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let admitted = |path: &std::path::Path| {
            let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
                return false;
            };
            let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: `path` is NUL-terminated and `stat` is a writable buffer.
            if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: statfs returned 0, so it initialised the buffer.
            let stat = unsafe { stat.assume_init() };
            // The session Store also refuses to commit below its free-space
            // floor (a twentieth of the filesystem, at least 1 GiB).
            let fragment = u64::try_from(stat.f_frsize).unwrap_or(0);
            #[allow(
                clippy::useless_conversion,
                reason = "fsblkcnt_t is u64 on 64-bit Linux but narrower on other targets"
            )]
            let (total, available) = (
                u64::try_from(stat.f_blocks).unwrap_or(0) * fragment,
                u64::try_from(stat.f_bavail).unwrap_or(0) * fragment,
            );
            u64::try_from(stat.f_type).is_ok_and(|magic| {
                matches!(magic, 0xef53 | 0x9123_683e | 0x5846_5342 | 0x0102_1994)
            }) && available > (total / 20).max(1 << 30) + (64 << 20)
        };
        // Cargo provides integration tests a temporary directory below the target.
        let target: Option<std::path::PathBuf> = Some(env!("CARGO_TARGET_TMPDIR").into());
        std::iter::once(std::env::temp_dir())
            .chain(target)
            .chain(std::iter::once(std::path::PathBuf::from("/dev/shm")))
            .find(|base| std::fs::create_dir_all(base).is_ok() && admitted(base))
            .map(|base| tempfile::tempdir_in(base).unwrap())
            .expect(
                "sender administration fixtures need an ext4, btrfs, XFS or tmpfs \
                 temporary directory with free space (or TILO_PRIVATE_ACTIONS_TEST_ROOT)",
            )
    }
    #[cfg(not(target_os = "linux"))]
    tempfile::tempdir().unwrap()
}

fn fixture() -> tempfile::TempDir {
    let temp = match std::env::var_os("TILO_PRIVATE_ACTIONS_TEST_ROOT") {
        Some(parent) => {
            let parent = std::path::PathBuf::from(parent);
            assert!(
                parent.is_absolute(),
                "explicit fixture root must be absolute"
            );
            assert!(parent.is_dir(), "explicit fixture root must already exist");
            tempfile::tempdir_in(parent).unwrap()
        }
        None => admitted_temp_dir(),
    };
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let opened = std::fs::File::open(temp.path()).unwrap();
    assert_eq!(
        opened.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    #[cfg(target_os = "linux")]
    if std::env::var_os("TILO_PRIVATE_ACTIONS_TEST_ROOT").is_some() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(
            unsafe { libc::fstatfs(opened.as_raw_fd(), stat.as_mut_ptr()) },
            0
        );
        let stat = unsafe { stat.assume_init() };
        assert_eq!(stat.f_type as u64, 0x5846_5342, "actual XFS required");
        let metadata = opened.metadata().unwrap();
        eprintln!(
            "native-sender-admin-cli XFS fixture={} device={} inode={} magic={:#x}",
            temp.path().display(),
            metadata.dev(),
            metadata.ino(),
            stat.f_type
        );
    }
    temp
}

#[test]
fn sender_admin_cli_managed_init_and_explicit_status() {
    let container = fixture();
    let root = container.path().join("owner");
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
        .arg("init-managed")
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        root.join("session.bin").is_file(),
        "real initializer was not reached"
    );
    assert!(
        output.status.success(),
        "managed initializer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let initialized: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(initialized["phase"], "active");
    assert_eq!(
        std::fs::metadata(root.join("private-actions-v1/private-actions-v1.bin"))
            .unwrap()
            .len(),
        439
    );

    let output = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
        .args(["sender-storage", "status"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "explicit status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reopened: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(initialized, reopened);

    let before = std::fs::read(root.join("session.bin")).unwrap();
    let ordinary = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
        .arg("status")
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        !ordinary.status.success(),
        "ordinary owner must not escape the administrative boundary"
    );
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
}

#[test]
fn sender_admin_cli_explicit_select_resume_and_runtime_refusal() {
    let container = fixture();
    let root = container.path().join("existing");
    let invoke = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"));
        command.args(args).arg(&root).output().unwrap()
    };
    assert!(invoke(&["init"]).status.success());
    assert!(!root.join("private-actions-v1").exists());
    let selected = invoke(&["sender-storage", "select"]);
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let resumed = invoke(&["sender-storage", "resume"]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(selected.stdout, resumed.stdout);
    let before = std::fs::read(root.join("session.bin")).unwrap();
    let ready = container.path().join("must-not-be-ready");
    let run = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
        .arg("run")
        .arg(&root)
        .arg(&ready)
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(!ready.exists());
    assert_eq!(before, std::fs::read(root.join("session.bin")).unwrap());
}

#[test]
fn sender_admin_cli_named_managed_initializer() {
    let container = fixture();
    let root = container.path().join("named");
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
        .arg("init-managed-workspace")
        .arg(&root)
        .arg("Named private room")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["phase"], "active");
    assert_eq!(
        std::fs::metadata(root.join("private-actions-v1/private-actions-v1.bin"))
            .unwrap()
            .len(),
        439
    );
}

#[test]
fn sender_admin_cli_owned_physical_error_refuses_and_releases_original_locks() {
    let container = fixture();
    let root = container.path().join("owner");
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
            .args(args)
            .arg(&root)
            .output()
            .unwrap()
    };
    assert!(invoke(&["init-managed"]).status.success());
    let file = root.join("private-actions-v1/private-actions-v1.bin");
    let saved = std::fs::read(&file).unwrap();
    let aggregate = std::fs::read(root.join("session.bin")).unwrap();
    std::fs::remove_file(&file).unwrap();
    let refused = invoke(&["sender-storage", "status"]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("administrative physical storage refused")
    );
    assert!(
        !file.exists(),
        "Active absence must not initialize a new file"
    );
    assert_eq!(std::fs::read(root.join("session.bin")).unwrap(), aggregate);
    std::fs::write(&file, saved).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let recovered = invoke(&["sender-storage", "status"]);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
}
