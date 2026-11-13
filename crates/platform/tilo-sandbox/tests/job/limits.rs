//! Limits: an OOM kills only the job, a fork bomb is contained, a daemon never outlives its job,
//! quotas apply where the filesystem enforces them. These need a delegated cgroup; without one each
//! prints why and passes.

use std::io::Read;
use std::os::fd::AsFd;

use tilo_sandbox::job::cgroup::{CgroupProvider, Direct, DirectOptions, Limits, QuotaOutcome};
use tilo_sandbox::job::{JobSpec, Launch, Mount, Root, Tmp};

use crate::needs_sandbox;
use crate::support::pipe;

/// A `Direct` provider in a fresh cgroup of the test's own, or why the host won't allow one. A
/// cgroup hands its children controllers only while it holds no processes (the host's root aside),
/// so the test's own cgroup, which holds at least this process, rarely can. The fresh cgroup is
/// made instead under the closest cgroup at or above it that already hands its children the memory
/// and pids controllers: a delegated one.
fn provider() -> Result<(Direct, Tree), String> {
    let mount = std::path::Path::new("/sys/fs/cgroup");
    let own = tilo_sandbox::job::cgroup::cgroup_of("self", mount).map_err(|e| e.to_string())?;
    let delegating = own
        .ancestors()
        .take_while(|dir| dir.starts_with(mount))
        .find(|dir| {
            let enabled =
                std::fs::read_to_string(dir.join("cgroup.subtree_control")).unwrap_or_default();
            ["memory", "pids"]
                .iter()
                .all(|c| enabled.split_whitespace().any(|e| e == *c))
        })
        .ok_or_else(|| {
            format!(
                "no cgroup at or above {} hands its children the memory and pids controllers",
                own.display()
            )
        })?;
    let base = delegating.join(format!("job-sandbox-test-{}", std::process::id()));
    std::fs::create_dir(&base).map_err(|e| format!("creating {}: {e}", base.display()))?;
    // Removed at the end, whatever happens from here.
    let tree = Tree(base.clone());
    let options = DirectOptions {
        root: Some(base),
        worker_leaf: "runner".into(),
        jobs: "jobs".into(),
        reserve: 0,
    };
    let direct = Direct::init(&options).map_err(|e| e.to_string())?;
    Ok((direct, tree))
}

/// A test's cgroup subtree, removed when dropped (its job cgroups are removed by theirs).
struct Tree(std::path::PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        for leaf in ["jobs", "runner"] {
            let _ = std::fs::remove_dir(self.0.join(leaf));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
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
    let cg = direct
        .create("oom", &limits)
        .expect("a job cgroup where the controllers are delegated");
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // Touch far more than the limit; memory.oom.group ends the whole job. Grow until the kernel
    // kills the job for memory; shell arithmetic in a string keeps a huge value live without
    // needing python.
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
}

#[test]
fn a_job_of_a_worker_shielded_from_the_oom_killer_stays_killable() {
    needs_sandbox!();
    if !crate::support::is_root() {
        eprintln!("skipped: shielding the worker from the OOM killer takes root");
        return;
    }
    // As a host may shield a runner: then a job at its memory limit would hang there, as nothing in
    // its cgroup could be killed.
    std::fs::write("/proc/self/oom_score_adj", "-1000").unwrap();
    let (status, out) = crate::support::run(|s| {
        crate::support::shell(s, "cat /proc/self/oom_score_adj");
    });
    assert!(status.success(), "{out}");
    assert_eq!(out, "0\n");
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
    let cg = direct
        .create("forkbomb", &limits)
        .expect("a job cgroup where the controllers are delegated");
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
        peak.is_some_and(|p| p <= 64 + 2),
        "pids.max was exceeded: {peak:?}"
    );
    let mut out = String::new();
    let _ = out_r.read_to_string(&mut out);
}

fn command_line(pid: u32) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default()
}

#[test]
fn cgroup_kill_removes_a_daemon_the_job_left_running() {
    needs_sandbox!();
    let (direct, _g) = needs_cgroup!(cgroup_kill_removes_a_daemon_the_job_left_running);
    let cg = direct
        .create("daemon", &Limits::default())
        .expect("a job cgroup where the controllers are delegated");
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // A setsid sleeper in a session of its own, the sort of listener a test leaves behind, beside a
    // command that keeps running.
    spec.argv = [
        "/bin/sh",
        "-c",
        "setsid sleep 301 </dev/null >/dev/null 2>&1 & echo started; exec sleep 300",
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
    std::io::BufRead::read_line(&mut std::io::BufReader::new(out_r), &mut line).unwrap();
    assert_eq!(line, "started\n");

    // A new session leaves the job's cgroup unchanged: the daemon is in it.
    tilo_test_env::wait_until("the setsid daemon to show in the job's cgroup", || {
        cg.pids()
            .into_iter()
            .any(|pid| command_line(pid) == "sleep 301 ")
    });
    cg.kill().expect("cgroup.kill");
    tilo_test_env::wait_until("cgroup.kill to end the daemon", || cg.pids().is_empty());
    assert!(!job.wait().unwrap().success());
}

#[test]
fn a_daemon_ends_with_the_command_that_left_it() {
    needs_sandbox!();
    let (direct, _g) = needs_cgroup!(a_daemon_ends_with_the_command_that_left_it);
    let cg = direct
        .create("left", &Limits::default())
        .expect("a job cgroup where the controllers are delegated");
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    // The command returns at once, leaving a setsid sleeper behind: the job's PID namespace ends
    // with its init, and everything in it with that.
    spec.argv = [
        "/bin/sh",
        "-c",
        "setsid sleep 302 </dev/null >/dev/null 2>&1 & echo started",
    ]
    .map(Into::into)
    .to_vec();
    let (mut out_r, out_w) = pipe();
    let mut job = Launch::new(&spec)
        .cgroup(&cg)
        .stdout(out_w)
        .spawn()
        .expect("spawn a daemon starter");
    assert!(job.wait().unwrap().success());
    let mut out = String::new();
    let _ = out_r.read_to_string(&mut out);
    assert!(out.contains("started"), "{out}");
    tilo_test_env::wait_until("the job's end to take its daemon", || cg.pids().is_empty());
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
    let _ = std::fs::remove_dir_all(&scratch);
}
