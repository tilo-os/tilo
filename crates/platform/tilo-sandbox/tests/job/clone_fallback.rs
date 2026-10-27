//! A job started with `clone`, as where `clone3` answers `ENOSYS` (an old
//! kernel, or a seccomp filter that refuses it: a job sandbox's own, for jobs
//! that start jobs), is the job `clone3` starts: the same mounts, cgroup,
//! namespaces, ids and privileges, and born in its cgroup.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;

use tilo_sandbox::job::cgroup::{CgroupProvider, Limits};
use tilo_sandbox::job::{JobSpec, Launch, Root};

use crate::limits::{cleanup, provider};
use crate::needs_sandbox;
use crate::support::pipe;

/// The namespaces a job may have of its own.
const NAMESPACES: [&str; 7] = ["cgroup", "ipc", "mnt", "net", "pid", "user", "uts"];

/// What a job sees of itself, then waits for its stdin to close: its
/// mounts, its cgroup as it sees it, which namespaces are its own (`NS_<n>`
/// holds the starter's), its ids, privileges and host name.
const SEEN: &str = r#"
cat /proc/self/mounts
echo --- cgroup; cat /proc/self/cgroup
echo --- namespaces
for n in cgroup ipc mnt net pid user uts; do
    eval outside=\$NS_$n
    if [ "$(readlink /proc/self/ns/$n)" = "$outside" ]; then echo "$n shared"; else echo "$n own"; fi
done
echo --- ids; id; cat /proc/self/uid_map /proc/self/gid_map
echo --- status; grep -E '^(Cap(Inh|Prm|Eff|Bnd|Amb)|NoNewPrivs|Seccomp|Seccomp_filters):' /proc/self/status
echo --- host; hostname
read _
"#;

/// A job of [`SEEN`], in `cgroup` where there is one: what it printed, and
/// the cgroup its first process was in while it ran, from outside.
fn seen(cgroup: Option<&tilo_sandbox::job::cgroup::JobCgroup>) -> (String, Option<String>) {
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh", "-c", SEEN]);
    for n in NAMESPACES {
        let outside = std::fs::read_link(format!("/proc/self/ns/{n}")).unwrap();
        spec.env(format!("NS_{n}"), outside);
    }
    spec.env("PATH", "/usr/bin:/bin");
    let (mut out_r, out_w) = pipe();
    let err_w = out_w.try_clone().unwrap();
    let (in_r, mut in_w) = stdin_pipe();
    let mut launch = Launch::new(&spec).stdin(in_r).stdout(out_w).stderr(err_w);
    if let Some(cg) = cgroup {
        launch = launch.cgroup(cg);
    }
    let mut job = launch.spawn().expect("start the job");
    let placed =
        cgroup.map(|_| std::fs::read_to_string(format!("/proc/{}/cgroup", job.pid())).unwrap());
    in_w.write_all(b"\n").unwrap();
    drop(in_w);
    let status = job.wait().unwrap();
    let mut out = String::new();
    out_r.read_to_string(&mut out).unwrap();
    assert!(status.success(), "{status:?}: {out}");
    (out, placed)
}

/// A pipe for a job's stdin: its read end goes to the job, its write end
/// stays here.
fn stdin_pipe() -> (OwnedFd, std::fs::File) {
    let (r, w) = pipe();
    (OwnedFd::from(r), std::fs::File::from(w))
}

#[test]
fn a_job_started_with_clone_is_the_job_clone3_starts() {
    needs_sandbox!();
    // In a cgroup of its own where this host delegates one, as a worker's
    // jobs are: `clone` can't start a job in it, so the job joins it first.
    let direct = match provider() {
        Ok(p) => Some(p),
        Err(why) => {
            // "skipped" for what goes untested: the real-kernel check fails on it.
            eprintln!(
                "skipped joining a cgroup, comparing without one: no delegated cgroup here: {why}"
            );
            None
        }
    };
    let cgroup = |name: &str| {
        direct.as_ref().map(|(d, _)| {
            d.create(name, &Limits::default())
                .expect("make a job cgroup")
        })
    };
    let first = cgroup("clone3");
    let (by_clone3, in_clone3) = seen(first.as_ref());
    if tilo_sandbox::job::start_with_clone_from_now() {
        eprintln!(
            "skipped: clone3 answers ENOSYS here, so every job already starts with clone: there is no clone3 start to compare with"
        );
        drop(first);
        cleanup();
        return;
    }
    let second = cgroup("clone");
    let (by_clone, in_clone) = seen(second.as_ref());
    assert_eq!(by_clone3, by_clone);
    // Each in its own cgroup while it ran, seen from outside.
    if let (Some(a), Some(b)) = (&in_clone3, &in_clone) {
        assert!(a.trim_end().ends_with("/clone3"), "{a}");
        assert!(b.trim_end().ends_with("/clone"), "{b}");
    }
    drop((first, second));
    cleanup();
}
