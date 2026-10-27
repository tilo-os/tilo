//! The CI seccomp filter refuses what builds never need and kernel exploits
//! do, while leaving the ordinary calls of a build working.

use crate::needs_sandbox;
use crate::support::{run, shell};
use tilo_sandbox::job::NESTING_ID;

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
fn only_a_job_allowed_nested_sandboxes_starts_one_and_netfilter_stays_closed() {
    needs_sandbox!();
    // What bubblewrap and `unshare` do: a user namespace of their own, and
    // mounts in it.
    let nest = "unshare --user --map-root-user --mount --net sh -c 'mount -t tmpfs none /tmp && echo nested' 2>&1; echo done";
    let (status, out) = run(|s| shell(s, nest));
    assert!(status.success() && out.contains("done"), "{out}");
    assert!(
        !out.contains("nested"),
        "a standard job started a sandbox: {out}"
    );

    // Not root inside: the job's root holds no capability, and only another
    // id may map itself in a namespace of its own without `CAP_SETFCAP`, so
    // a nesting job is NESTING_ID unless its caller picks another id.
    let (status, out) = run(|s| {
        shell(s, &format!("id -u; {nest}"));
        s.allow_nested_sandboxes();
    });
    assert!(status.success() && out.contains("nested"), "{out}");
    assert_eq!(
        out.lines().next(),
        Some(NESTING_ID.to_string().as_str()),
        "{out}"
    );

    // Holding CAP_NET_ADMIN over its own network namespace, a nested
    // sandbox still can't reach nftables or XFRM; route netlink works.
    let probe = r#"unshare --user --map-root-user --net python3 -c '
import socket
for name, proto in (("route", 0), ("netfilter", 12), ("xfrm", 6)):
    try:
        socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, proto).close(); print(name, "opened")
    except OSError as e:
        print(name, e.strerror)
'"#;
    let (status, out) = run(|s| {
        shell(s, probe);
        s.allow_nested_sandboxes();
    });
    if out.contains("python3: not found") || out.contains("No such file") {
        eprintln!("skipped the netlink probe: no python3 in the host root: {out}");
        return;
    }
    assert!(status.success(), "{out}");
    assert!(out.contains("route opened"), "{out}");
    assert!(out.contains("netfilter Operation not permitted"), "{out}");
    assert!(out.contains("xfrm Operation not permitted"), "{out}");
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

#[test]
fn only_a_job_allowed_file_handle_names_gets_them_and_open_by_handle_stays_refused() {
    needs_sandbox!();
    // name_to_handle_at with no handle: the kernel answers "Bad address", the
    // filter "Operation not permitted" before it. open_by_handle_at stays
    // refused either way, and setns with it.
    let script = r#"
for probe in \
  'import ctypes,os; nr={"x86_64":303,"aarch64":264}[os.uname().machine]; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(nr,-100,b"/",None,None,0); print("name_to_handle_at", os.strerror(ctypes.get_errno()))' \
  'import ctypes,os; nr={"x86_64":304,"aarch64":265}[os.uname().machine]; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(nr,-1,None,0); print("open_by_handle_at", os.strerror(ctypes.get_errno()))' \
  'import ctypes,os; nr={"x86_64":308,"aarch64":268}[os.uname().machine]; libc=ctypes.CDLL(None,use_errno=True); libc.syscall(nr,-1,0); print("setns", os.strerror(ctypes.get_errno()))' ; do
  python3 -c "$probe"
done
"#;
    let line = |out: &str, name: &str| {
        out.lines()
            .find(|l| l.starts_with(&format!("{name} ")))
            .unwrap_or("")
            .to_owned()
    };
    let (status, out) = run(|s| {
        shell(s, script);
        s.allow_nested_sandboxes();
    });
    if out.contains("No such file or directory") && out.lines().count() < 3 {
        eprintln!("skipped: no python3 in the host root: {out}");
        return;
    }
    assert!(status.success(), "{out}");
    for name in ["name_to_handle_at", "open_by_handle_at", "setns"] {
        assert!(
            line(&out, name).contains("Operation not permitted"),
            "{name} was not refused unasked: {out}"
        );
    }

    let (status, out) = run(|s| {
        shell(s, script);
        s.allow_nested_sandboxes();
        s.allow(tilo_sandbox::job::CiAllowance::FileHandleNames);
    });
    assert!(status.success(), "{out}");
    // Past the filter the kernel's answer depends on the file system (no
    // handles on some), so only the filter's refusal is looked for.
    let name = line(&out, "name_to_handle_at");
    assert!(
        !name.is_empty() && !name.contains("Operation not permitted"),
        "name_to_handle_at was refused when allowed: {out}"
    );
    for name in ["open_by_handle_at", "setns"] {
        assert!(
            line(&out, name).contains("Operation not permitted"),
            "the allowance let {name} through: {out}"
        );
    }
}
