#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn fixture() -> tempfile::TempDir {
    let temp = match std::env::var_os("QOS_PRIVATE_ACTIONS_TEST_ROOT") {
        Some(parent) => {
            let parent = std::path::PathBuf::from(parent);
            assert!(
                parent.is_absolute(),
                "explicit fixture root must be absolute"
            );
            assert!(parent.is_dir(), "explicit fixture root must already exist");
            tempfile::tempdir_in(parent).unwrap()
        }
        None => tempfile::tempdir().unwrap(),
    };
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let opened = std::fs::File::open(temp.path()).unwrap();
    assert_eq!(
        opened.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    #[cfg(target_os = "linux")]
    if std::env::var_os("QOS_PRIVATE_ACTIONS_TEST_ROOT").is_some() {
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
    let output = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
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

    let output = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
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
    let ordinary = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"));
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
    let run = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
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
    let output = Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
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
        Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
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
