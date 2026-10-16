//! A job that may start sandboxes of its own, with a range of subordinate
//! ids: its command is root over the whole range in a user namespace of its
//! own, and still can't touch what the library set up for it.

use std::os::unix::fs::MetadataExt;

use tilo_sandbox::job::{IdMap, IdRange, JobSpec, Mount};

use crate::needs_sandbox;
use crate::support::{run, shell};

/// The caller's subordinate ids, or why a test can't use them here.
fn range() -> Option<IdRange> {
    let range = IdRange::subordinate();
    if range.is_none() {
        eprintln!("skipped: no entry for this user in /etc/subuid and /etc/subgid");
    }
    range
}

fn nested(spec: &mut JobSpec, range: IdRange) {
    spec.allow_nested_sandboxes();
    spec.ids.range = Some(range);
}

#[test]
fn the_command_is_root_over_the_whole_range() {
    needs_sandbox!();
    let Some(range) = range() else { return };
    let dir = tempfile::tempdir().unwrap();
    let ids = IdMap {
        range: Some(range),
        ..IdMap::current()
    };
    let (status, out) = run(|s| {
        nested(s, range);
        s.mount(Mount::read_write(dir.path(), "/w"));
        shell(
            s,
            "echo uid=$(id -u); cat /proc/self/uid_map; \
             mkdir /w/d && touch /w/d/f && chown -R 65532:65532 /w/d && chmod 700 /w/d && echo chowned; \
             echo runtime=$XDG_RUNTIME_DIR $(stat -c %a:%u $XDG_RUNTIME_DIR); touch $XDG_RUNTIME_DIR/bus && echo bus",
        );
    });
    let owner = std::fs::metadata(dir.path().join("d")).map(|m| (m.uid(), m.gid()));
    // Removed as the job's ids: the worker can't enter a 0700 directory of
    // a subordinate id.
    assert!(
        std::fs::remove_dir_all(dir.path().join("d")).is_err() || owner.is_err(),
        "the caller removed a directory of a subordinate id"
    );
    tilo_sandbox::job::remove_tree(dir.path(), ids).unwrap();
    assert!(!dir.path().exists());
    assert!(status.success(), "{out}");
    assert!(out.contains("uid=0"), "{out}");
    assert!(
        out.lines().any(|l| l
            .split_whitespace()
            .eq(["1", "1", &range.count.to_string()])),
        "the command's namespace does not map the whole range: {out}"
    );
    assert!(out.contains("chowned"), "{out}");
    assert_eq!(
        owner.unwrap(),
        (range.uid + 65531, range.gid + 65531),
        "id 65532 inside is not the range's"
    );
    assert!(out.contains("runtime=/run/user/0 700:0"), "{out}");
    assert!(out.contains("\nbus"), "{out}");
}

#[test]
fn setgroups_works_and_bubblewrap_style_sandboxes_start() {
    needs_sandbox!();
    let Some(range) = range() else { return };
    let (status, out) = run(|s| {
        nested(s, range);
        shell(
            s,
            "setpriv --groups 10,65532 id -G; \
             unshare --user --map-root-user --mount --net sh -c 'mount -t tmpfs none /tmp && echo nested' 2>&1; \
             unshare --mount --pid --fork sh -c 'mount -t tmpfs none /tmp && echo rootful' 2>&1",
        );
    });
    assert!(status.success(), "{out}");
    assert!(out.contains("10 65532"), "setgroups failed: {out}");
    assert!(out.contains("nested"), "{out}");
    assert!(out.contains("rootful"), "{out}");
}

#[test]
fn the_jobs_own_mounts_and_network_stay_out_of_reach() {
    needs_sandbox!();
    let Some(range) = range() else { return };
    // Root in its own namespace, the command still can't make the job's
    // read-only mounts writable, uncover what is under them, or reach
    // nftables and XFRM.
    let probe = r#"
mount -o remount,rw / 2>&1 && echo remounted-root
mount -o remount,bind,rw /usr 2>&1 && echo remounted-usr
umount /proc 2>&1 && echo unmounted-proc
ip link add dummy0 type dummy 2>&1 && echo added-link
python3 -c '
import socket
for name, proto in (("route", 0), ("netfilter", 12), ("xfrm", 6)):
    try:
        socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, proto).close(); print(name, "opened")
    except OSError as e:
        print(name, e.strerror)
' 2>&1
echo done"#;
    let (status, out) = run(|s| {
        nested(s, range);
        shell(s, probe);
    });
    assert!(status.success() && out.contains("done"), "{out}");
    for escaped in [
        "remounted-root",
        "remounted-usr",
        "unmounted-proc",
        "added-link",
    ] {
        assert!(!out.contains(escaped), "{escaped}: {out}");
    }
    if out.contains("python3: not found") || out.contains("No such file") {
        eprintln!("skipped the netlink probe: no python3 in the host root: {out}");
        return;
    }
    assert!(out.contains("netfilter Operation not permitted"), "{out}");
    assert!(out.contains("xfrm Operation not permitted"), "{out}");
}

#[test]
fn an_id_without_an_account_gets_one() {
    needs_sandbox!();
    // dbus-daemon and ssh refuse to run as an id without a name.
    let (status, out) = run(|s| {
        s.allow_nested_sandboxes();
        s.ids.inside_uid = 4242;
        s.ids.inside_gid = 4242;
        s.env("HOME", "/tmp");
        shell(
            s,
            "getent passwd 4242; getent group 4242; echo runtime=$XDG_RUNTIME_DIR",
        );
    });
    assert!(status.success(), "{out}");
    assert!(out.contains("job:x:4242:4242:job:/tmp:/bin/sh"), "{out}");
    assert!(out.contains("job:x:4242:"), "{out}");
    assert!(out.contains("runtime=/run/user/4242"), "{out}");
}

#[test]
fn a_standard_job_keeps_one_id_and_no_capability() {
    needs_sandbox!();
    let Some(range) = range() else { return };
    // A range alone changes nothing for a job that may not nest.
    let (status, out) = run(|s| {
        s.ids.range = Some(range);
        shell(
            s,
            "cat /proc/self/uid_map; grep CapEff /proc/self/status; echo runtime=$XDG_RUNTIME_DIR",
        );
    });
    assert!(status.success(), "{out}");
    let uid = rustix::process::geteuid().as_raw().to_string();
    assert!(
        out.lines()
            .any(|l| l.split_whitespace().eq(["0", &uid, "1"])),
        "{out}"
    );
    assert!(out.contains("CapEff:\t0000000000000000"), "{out}");
    assert!(out.contains("runtime=\n"), "{out}");
}
