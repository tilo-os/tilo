//! The CI seccomp filter refuses what builds never need and kernel exploits
//! do, while leaving the ordinary calls of a build working.

use crate::needs_sandbox;
use crate::support::{run, shell};

#[test]
fn exploit_primitives_are_refused_with_eperm() {
    needs_sandbox!();
    // Each line runs a tiny program that makes one refused syscall and
    // prints its errno name. `python3` is in the host root the tests use.
    let script = r#"
for probe in \
  'import ctypes,os; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(321,0,0,0,0); print("bpf", os.strerror(ctypes.get_errno()))' \
  'import ctypes,os; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(425,0,0,0); print("io_uring", os.strerror(ctypes.get_errno()))' \
  'import ctypes,os; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(323,0,0,0); print("userfaultfd", os.strerror(ctypes.get_errno()))' \
  'import ctypes,os; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(308,0,-1,0); print("setns", os.strerror(ctypes.get_errno()))' \
  'import ctypes,os; libc=ctypes.CDLL(None,use_errno=True); libc.unshare(0x10000000); print("unshare_newuser", os.strerror(ctypes.get_errno()))' ; do
  python3 -c "$probe"
done
"#;
    let (status, out) = run(|s| shell(s, script));
    if out.contains("No such file or directory") && out.lines().count() < 3 {
        eprintln!("skipped: no python3 in the host root: {out}");
        return;
    }
    assert!(status.success(), "{out}");
    for name in ["bpf", "io_uring", "userfaultfd", "setns", "unshare_newuser"] {
        let line = out.lines().find(|l| l.starts_with(name)).unwrap_or("");
        assert!(
            line.contains("Operation not permitted"),
            "{name} was not refused with EPERM: {out}"
        );
    }
}

#[test]
fn mount_is_refused() {
    needs_sandbox!();
    // A job's filesystem is final: even as root inside, mount(2) is EPERM
    // from the filter (before the kernel's own capability check).
    let (status, out) = run(|s| {
        shell(
            s,
            "mkdir -p /tmp/m && mount -t tmpfs none /tmp/m 2>&1; echo done",
        );
    });
    assert!(status.success(), "{out}");
    assert!(out.contains("done"), "{out}");
    assert!(
        out.contains("Operation not permitted")
            || out.contains("not permitted")
            || out.contains("Permission denied")
            || out.contains("denied"),
        "mount was not refused: {out}"
    );
}

#[test]
fn ordinary_build_syscalls_still_work() {
    needs_sandbox!();
    // Threads, fork, pipes, file I/O: the filter must leave all of it alone,
    // or no compiler runs. A shell that forks, pipes and writes proves it.
    let (status, out) = run(|s| {
        shell(
            s,
            "set -e; echo hi | tr a-z A-Z > /tmp/f; cat /tmp/f; \
             for i in 1 2 3; do (echo child $i) & done; wait; echo ok",
        );
    });
    assert!(status.success(), "{out}");
    assert!(out.contains("HI") && out.contains("ok"), "{out}");
}
