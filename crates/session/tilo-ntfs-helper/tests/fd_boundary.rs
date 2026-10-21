#![cfg(unix)]
use std::process::Command;
#[test]
fn production_rejects_regular_file_and_caller_device_assertions() {
    for args in [
        vec!["--image", "/tmp/image"],
        vec![
            "inspect-selected",
            "--inventory-id",
            "/tmp/image",
            "--root-mft",
            "5",
        ],
        vec![
            "inspect-selected",
            "--major-minor",
            "8:0",
            "--exclusive",
            "--health",
            "clean",
        ],
    ] {
        let out = Command::new(tilo_test_env::package_binary!("tilo-ntfs-probe"))
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "caller asserted authority accepted");
        assert!(out.stdout.is_empty(), "refused request emitted evidence");
    }
}
#[cfg(target_os = "linux")]
#[path = "support/linux_boundary.rs"]
mod linux;
