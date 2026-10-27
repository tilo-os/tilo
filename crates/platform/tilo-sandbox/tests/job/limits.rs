//! Limits: an OOM kills only the job, a fork bomb is contained, `cgroup.kill`
//! removes daemons, quotas apply where the filesystem enforces them. These
//! need a delegated cgroup; without one each prints why and passes.

use std::io::Read;
use std::os::fd::AsFd;

use tilo_sandbox::job::cgroup::{CgroupProvider, Direct, DirectOptions, Limits, QuotaOutcome};
use tilo_sandbox::job::{JobSpec, Launch, Mount, Root, Tmp};

use crate::needs_sandbox;
use crate::support::pipe;

/// A `Direct` provider in a sub-cgroup of this process's own, or why the
/// host won't allow one.
pub fn provider() -> Result<(Direct, tempfile::TempDir), String> {
    let mount = std::path::Path::new("/sys/fs/cgroup");
    let own = tilo_sandbox::job::cgroup::cgroup_of("self", mount).map_err(|e| e.to_string())?;
    // A private sub-tree so the test doesn't disturb its own cgroup.
    let base = own.join(format!("job-sandbox-test-{}", std::process::id()));
    std::fs::create_dir_all(&base).map_err(|e| format!("creating {}: {e}", base.display()))?;
    // Move this process in, so the base becomes an inner node we may give
    // controllers to (v2 allows processes only in leaves).
    let leaf = base.join("runner");
    std::fs::create_dir_all(&leaf).map_err(|e| e.to_string())?;
    std::fs::write(leaf.join("cgroup.procs"), std::process::id().to_string())
        .map_err(|e| format!("joining the test leaf: {e}"))?;
    let options = DirectOptions {
        root: Some(base.clone()),
        worker_leaf: "runner".into(),
        jobs: "jobs".into(),
        reserve: 0,
    };
    let direct = Direct::init(&options).map_err(|e| e.to_string())?;
    // Keep the base alive and clean it up at the end.
    let guard = scopeguard(base);
    Ok((direct, guard))
}

/// A tempdir-like guard that removes a cgroup subtree on drop.
fn scopeguard(path: std::path::PathBuf) -> tempfile::TempDir {
    // Reuse TempDir only as a drop guard is awkward; instead leak a cleanup
    // thread-free guard via a real tempdir we ignore, and remove by hand in
    // tests. Simplest: return a TempDir rooted elsewhere, and remove `path`
    // through a Drop wrapper below.
    CLEANUP.with(|c| c.borrow_mut().push(path));
    tempfile::tempdir().expect("a scratch tempdir")
}

thread_local! {
    static CLEANUP: std::cell::RefCell<Vec<std::path::PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Remove the cgroup subtrees a test made.
pub fn cleanup() {
    CLEANUP.with(|c| {
        for path in c.borrow_mut().drain(..) {
            for leaf in ["jobs", "runner"] {
                let _ = std::fs::remove_dir(path.join(leaf));
            }
            let _ = std::fs::remove_dir(&path);
        }
    });
}

macro_rules! needs_cgroup {
    ($name:ident) => {
        match provider() {
            Ok(p) => p,
            Err(why) => {
                eprintln!(
                    "skipped {}: no delegated cgroup here: {why}",
                    stringify!($name)
                );
                return;
            }
        }
    };
}

#[test]
fn an_oom_kills_only_the_job() {
    needs_sandbox!();
    let (direct, _g) = needs_cgroup!(an_oom_kills_only_the_job);
    let before = std::fs::read_to_string("/proc/self/status").unwrap();

    let limits = Limits {
        memory_max: Some(64 * 1024 * 1024),
        ..Limits::default()
    };
    let cg = match direct.create("oom", &limits) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: could not make a job cgroup: {e}");
            cleanup();
            return;
        }
    };
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // Touch far more than the limit; memory.oom.group ends the whole job.
    // Grow until the kernel kills the job for memory; shell arithmetic in a
    // string keeps a huge value live without needing python.
    spec.argv = [
        "/bin/sh",
        "-c",
        "s=x; while true; do s=$s$s$s$s$s$s$s$s; done",
    ]
    .map(Into::into)
    .to_vec();
    let mut job = Launch::new(&spec)
        .cgroup(&cg)
        .spawn()
        .expect("spawn under a memory cap");
    let status = job.wait().expect("wait");
    // The job died; the kernel killed it for memory.
    assert!(!status.success(), "the job survived its memory cap");
    assert!(
        cg.oom_killed(),
        "the kernel did not record an OOM kill: {:?}",
        cg.usage()
    );

    // The test process is untouched: it survived, and so would the worker.
    let after = std::fs::read_to_string("/proc/self/status").unwrap();
    assert!(before.contains("VmRSS") && after.contains("VmRSS"));
    drop(cg);
    cleanup();
}

#[test]
fn a_fork_bomb_is_contained_and_the_runner_survives() {
    needs_sandbox!();
    let (direct, _g) = needs_cgroup!(a_fork_bomb_is_contained_and_the_runner_survives);
    let limits = Limits {
        pids_max: Some(64),
        memory_max: Some(256 * 1024 * 1024),
        ..Limits::default()
    };
    let cg = match direct.create("forkbomb", &limits) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: {e}");
            cleanup();
            return;
        }
    };
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // A classic fork bomb, bounded by pids.max; stops itself after a moment.
    spec.argv = ["/bin/sh", "-c", ":(){ :|:& };: & sleep 1; echo survived"]
        .map(Into::into)
        .to_vec();
    let (mut out_r, out_w) = pipe();
    let mut job = Launch::new(&spec)
        .cgroup(&cg)
        .stdout(out_w)
        .spawn()
        .expect("spawn the fork bomb");
    // The job is killed or ends; cgroup.kill removes whatever is left.
    let _ = job.wait();
    assert!(cg.kill().is_ok());
    let peak = cg.usage().pids_peak;
    // The runner (this process) is still here to assert.
    assert!(
        peak.is_none_or(|p| p <= 64 + 2),
        "pids.max was exceeded: {peak:?}"
    );
    let mut out = String::new();
    let _ = out_r.read_to_string(&mut out);
    drop(cg);
    cleanup();
}

/// Wait up to a second for `cg` to hold at least `n` processes, or none when `n` is 0.
fn await_pids(cg: &tilo_sandbox::job::cgroup::JobCgroup, n: usize) -> Vec<u32> {
    let mut pids = cg.pids();
    for _ in 0..500 {
        if (n == 0 && pids.is_empty()) || (n > 0 && pids.len() >= n) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        pids = cg.pids();
    }
    pids
}

#[test]
fn cgroup_kill_removes_a_daemon_the_job_started() {
    needs_sandbox!();
    let (direct, _g) = needs_cgroup!(cgroup_kill_removes_a_daemon_the_job_started);
    let cg = match direct.create("daemon", &Limits::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: {e}");
            cleanup();
            return;
        }
    };
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // setsid a sleeper, the sort of listener a test leaves behind, and keep the
    // command running: a job's daemons die with its init (the end of its PID
    // namespace), so cgroup.kill is what stops a job that is still running.
    spec.argv = [
        "/bin/sh",
        "-c",
        "setsid sh -c 'sleep 300' </dev/null >/dev/null 2>&1 & echo started; exec sleep 300",
    ]
    .map(Into::into)
    .to_vec();
    let (out_r, out_w) = pipe();
    let mut job = Launch::new(&spec)
        .cgroup(&cg)
        .stdout(out_w)
        .spawn()
        .expect("spawn a daemon starter");
    let mut line = String::new();
    let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(out_r), &mut line);
    assert_eq!(line, "started\n");

    // The init, the command and the daemon.
    let before = await_pids(&cg, 3);
    assert!(
        before.len() >= 3,
        "the daemon is not in the cgroup: {before:?}"
    );
    cg.kill().expect("cgroup.kill");
    assert!(!job.wait().unwrap().success());
    let after = await_pids(&cg, 0);
    assert!(after.is_empty(), "cgroup.kill left {after:?} running");
    drop(cg);
    cleanup();
}

#[test]
fn a_daemon_the_job_left_dies_with_it() {
    needs_sandbox!();
    let (direct, _g) = needs_cgroup!(a_daemon_the_job_left_dies_with_it);
    let cg = match direct.create("left", &Limits::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: {e}");
            cleanup();
            return;
        }
    };
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // The command returns once the daemon is in a session of its own (setsid
    // execs sh in the same process: it is no group leader to fork for).
    spec.argv = [
        "/bin/sh",
        "-c",
        "setsid sh -c 'sleep 300' </dev/null >/dev/null 2>&1 & d=$! tries=0
         until [ \"$(cut -d' ' -f6 /proc/$d/stat 2>/dev/null)\" = $d ]; do
           tries=$((tries + 1))
           [ $tries -lt 500 ] || { echo \"daemon $d: no session of its own after 5 s\"; exit 1; }
           sleep 0.01
         done
         echo \"daemon $d in its session\"",
    ]
    .map(Into::into)
    .to_vec();
    let (mut out_r, out_w) = pipe();
    let mut job = Launch::new(&spec)
        .cgroup(&cg)
        .stdout(out_w)
        .spawn()
        .expect("spawn a daemon starter");
    let status = job.wait().unwrap();
    let mut out = String::new();
    let _ = out_r.read_to_string(&mut out);
    assert!(status.success(), "{status:?}: {out}");
    assert!(
        out.contains("in its session"),
        "the daemon never ran: {out}"
    );
    // The init's exit ends the job's PID namespace, and the kernel kills the
    // daemon with it: nothing of the job is left once it has been waited for.
    let left = await_pids(&cg, 0);
    assert!(left.is_empty(), "the daemon outlived its job: {left:?}");
    drop(cg);
    cleanup();
}

#[test]
fn a_disk_quota_stops_writes_where_the_filesystem_enforces_it() {
    needs_sandbox!();
    let Some(xfs) = std::env::var_os("JOB_SANDBOX_TEST_XFS") else {
        eprintln!("skipped: set JOB_SANDBOX_TEST_XFS to an XFS dir mounted with prjquota");
        return;
    };
    let (direct, _g) = needs_cgroup!(a_disk_quota_stops_writes_where_the_filesystem_enforces_it);
    let scratch = std::path::Path::new(&xfs).join(format!("job-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("make the scratch dir");
    let dir = tilo_sandbox::job::quota::open_dir(&scratch).expect("open the scratch dir");
    let outcome = direct
        .set_quota(dir.as_fd(), 7777, 8 * 1024 * 1024)
        .expect("set the quota");
    if let QuotaOutcome::Unsupported(why) = outcome {
        eprintln!("skipped: {why}");
        let _ = std::fs::remove_dir_all(&scratch);
        cleanup();
        return;
    }
    let cg = direct
        .create("quota", &Limits::default())
        .expect("job cgroup");
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    spec.tmp = Tmp::Dir(scratch.clone());
    spec.mount(Mount::read_write(&scratch, "/scratch"));
    // Writing past 8 MiB must fail with EDQUOT ("Disk quota exceeded").
    spec.argv = [
        "/bin/sh",
        "-c",
        "dd if=/dev/zero of=/scratch/big bs=1M count=64 2>&1; echo done",
    ]
    .map(Into::into)
    .to_vec();
    let (mut out_r, out_w) = pipe();
    let mut job = Launch::new(&spec)
        .cgroup(&cg)
        .stdout(out_w)
        .spawn()
        .expect("spawn under a quota");
    let _ = job.wait();
    let mut out = String::new();
    let _ = out_r.read_to_string(&mut out);
    assert!(
        out.contains("quota") || out.contains("No space"),
        "a write past the quota was not refused: {out}"
    );
    drop(cg);
    let _ = std::fs::remove_dir_all(&scratch);
    cleanup();
}
