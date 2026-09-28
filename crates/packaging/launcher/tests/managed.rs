//! Explicit Linux prerequisite/kernel checks. They fail on missing facilities;
//! passing these alone does not qualify actual tilo-agent/model/provider work.
#[cfg(all(target_os = "linux", feature = "managed-tree-test"))]
#[path = "managed/tree.rs"]
mod tree;

#[cfg(target_os = "linux")]
#[test]
fn managed_kernel_identity_tracks_exact_child_lifetime() {
    use launcher::managed::ProcessIdentity;
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut owned = Reap(
        Command::new("/bin/sh")
            .args(["-c", "read -r line"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("real child required"),
    );
    let other = Reap(
        Command::new("/bin/sh")
            .args(["-c", "read -r line"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("independent child required"),
    );
    let owned_identity =
        ProcessIdentity::read(owned.0.id()).expect("Linux process start identity required");
    let other_identity = ProcessIdentity::read(other.0.id()).unwrap();
    assert_ne!(owned_identity, other_identity);
    assert!(owned_identity.is_current());
    let wrong_start = ProcessIdentity {
        pid: owned_identity.pid,
        start: owned_identity.start + 1,
    };
    assert!(!wrong_start.is_current());
    owned
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"done\n")
        .unwrap();
    assert!(owned.0.wait().unwrap().success());
    assert!(!owned_identity.is_current());
    assert!(
        other_identity.is_current(),
        "owned reap must not affect another launch"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn managed_linux_namespace_prerequisites_are_mandatory() {
    let bwrap =
        std::env::var_os("TILO_MANAGED_TEST_BWRAP").unwrap_or_else(|| "/usr/bin/bwrap".into());
    let result = std::process::Command::new(bwrap)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "/usr/bin/true",
        ])
        .output()
        .expect("bwrap is required for explicit managed Linux qualification");
    assert!(
        result.status.success(),
        "user/PID/mount namespaces required: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn managed_linux_qualification_refuses_unsupported_platform() {
    panic!(
        "managed process qualification requires Linux; Darwin checks cannot qualify kernel/sandbox execution"
    );
}
