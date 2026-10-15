//! A job sees only itself: not its neighbours' processes or files, not the
//! host's loopback, not another process's environment.

use std::io::Read;
use std::net::TcpListener;

use tilo_sandbox::job::{JobSpec, Launch, Mount, Root};

use crate::needs_sandbox;
use crate::support::{pipe, run, shell};

/// Fallback tunnel devices the kernel puts, down and leading nowhere, into
/// every new network namespace on a host with those tunnel modules loaded.
const FALLBACK_TUNNELS: &[&str] = &[
    "tunl0", "sit0", "ip6tnl0", "gre0", "gretap0", "erspan0", "ip6gre0", "ip_vti0", "ip6_vti0",
];

#[test]
fn a_job_is_pid_1_and_sees_no_other_process() {
    needs_sandbox!();
    // Its own PID namespace: pid 1 is the job's init, and nothing with the
    // worker's pid exists.
    let (status, out) = run(|s| {
        shell(
            s,
            "echo pid=$$; ls /proc | grep -E '^[0-9]+$' | sort -n | tr '\\n' ' '; echo",
        );
    });
    assert!(status.success(), "{out}");
    let pids: Vec<u32> = out
        .lines()
        .find(|l| l.chars().all(|c| c.is_ascii_digit() || c == ' ') && l.contains(' '))
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    // Its own PID namespace: pid 1 is the job's init, and every pid is a low
    // one the job itself started — never the worker's (a large host pid).
    assert!(pids.contains(&1), "no pid 1 in the job: {out}");
    assert!(
        pids.iter().max().copied().unwrap_or(0) < 1000,
        "a job saw a host pid: {out}"
    );
}

#[test]
fn a_job_cannot_see_a_neighbours_files() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("secret"), "a's secret\n").unwrap();

    // Job B gets only its own workspace; A's is not mounted into it.
    let (status, out) = run(|s| {
        s.mount(Mount::read_write(&b, "/work"));
        shell(s, "ls /work; cat /work/../a/secret 2>&1 || true; ls / ");
    });
    assert!(status.success(), "{out}");
    assert!(
        !out.contains("a's secret"),
        "a job read a neighbour's file: {out}"
    );
}

#[test]
fn a_job_cannot_reach_the_hosts_loopback() {
    needs_sandbox!();
    // A server on the host's loopback, like another job's sccache.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let (status, out) = run(|s| {
        // /dev/tcp is bash's; the job's loopback is its own and empty.
        s.argv = ["/bin/bash", "-c", &format!(
            "if (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then echo reached; else echo unreachable; fi; \
             sed -n '3,$ s/^ *\\([^:]*\\):.*/iface \\1/p' /proc/net/dev"
        )].map(Into::into).to_vec();
    });
    assert!(status.success(), "{out}");
    assert!(
        out.contains("unreachable"),
        "a job reached the host loopback: {out}"
    );
    assert!(!out.lines().any(|l| l == "reached"), "{out}");
    // Only loopback exists in the job's netns (no egress attached here),
    // besides the kernel's fallback tunnel devices.
    let ifaces: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("iface "))
        .filter(|i| !FALLBACK_TUNNELS.contains(i))
        .collect();
    assert_eq!(ifaces, ["lo"], "{out}");
    drop(listener);
}

#[test]
fn a_job_reads_only_its_own_environ_from_proc() {
    needs_sandbox!();
    // The init's /proc/1/environ must not carry the worker's environment
    // (which holds tokens): it is zeroed, and pid 1 is non-dumpable.
    let (status, out) = run(|s| {
        s.env("JOB_MARKER", "only-the-job");
        // `cat` opens the file itself, so /proc/self is the reader. The init's
        // environ is a different process; reading it must be refused.
        shell(
            s,
            "echo -- self --; cat /proc/self/environ | tr '\\0' '\\n'; \
             echo -- init --; cat /proc/1/environ 2>&1 | tr '\\0' '\\n'",
        );
    });
    assert!(status.success(), "{out}");
    let self_part = out.split("-- init --").next().unwrap_or("");
    let init_part = out.split("-- init --").nth(1).unwrap_or("");
    // The command reads its own environment.
    assert!(self_part.contains("JOB_MARKER=only-the-job"), "{out}");
    // Pid 1's environ is refused, and the worker's secrets never appear.
    assert!(
        init_part.contains("Permission denied") || init_part.trim().is_empty(),
        "the job read pid 1's environment: {out}"
    );
    assert!(!out.contains("T_TOKEN"), "{out}");
}

#[test]
fn two_jobs_each_have_their_own_network_namespace() {
    needs_sandbox!();
    // Two jobs' loopbacks can't collide because their network namespaces
    // differ; a server one starts on 127.0.0.1 is the other's business never.
    let one = run(|s| shell(s, "readlink /proc/self/ns/net"));
    let two = run(|s| shell(s, "readlink /proc/self/ns/net"));
    assert!(one.0.success() && two.0.success(), "{} {}", one.1, two.1);
    assert_ne!(
        one.1.trim(),
        two.1.trim(),
        "two jobs shared a network namespace"
    );
}

#[test]
fn a_job_runs_as_root_inside_mapped_to_an_unprivileged_id_outside() {
    needs_sandbox!();
    let (status, out) = run(|s| shell(s, "id -u; id -g; cat /proc/self/uid_map"));
    assert!(status.success(), "{out}");
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("0"), "not root inside: {out}");
    assert_eq!(lines.next(), Some("0"), "{out}");
    // Exactly one uid is mapped.
    let map = lines.next().unwrap_or_default();
    let fields: Vec<&str> = map.split_whitespace().collect();
    assert_eq!(fields.first(), Some(&"0"), "inside uid not 0: {out}");
    assert_eq!(fields.get(2), Some(&"1"), "more than one uid mapped: {out}");
}

#[test]
fn the_setup_hook_sees_the_jobs_network_namespace_before_it_runs() {
    needs_sandbox!();
    // What t uses to attach pasta: the hook runs with the job's pid, whose
    // /proc/<pid>/ns/net names the namespace, before any of the job runs.
    let (reader, writer) = pipe();
    let spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen2 = std::sync::Arc::clone(&seen);
    let mut job = Launch::new(&spec)
        .stdout(writer)
        .before_start(move |started| {
            let ns = std::fs::read_link(format!("/proc/{}/ns/net", started.pid))
                .map_err(|e| e.to_string())?;
            *seen2.lock().unwrap() = Some(ns);
            Ok(())
        })
        .spawn()
        .expect("spawn with a hook");
    assert!(job.wait().unwrap().success());
    drop(reader);
    let ns = seen.lock().unwrap().clone().expect("the hook ran");
    // The job's netns, not the test's.
    let mine = std::fs::read_link("/proc/self/ns/net").unwrap();
    assert_ne!(ns, mine, "the hook saw the worker's network namespace");
}

#[test]
fn a_bad_mount_source_fails_before_the_job_runs() {
    needs_sandbox!();
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/true"]);
    spec.mount(Mount::read_only("/no/such/path/at/all", "/x"));
    let err = Launch::new(&spec)
        .spawn()
        .expect_err("a missing source is refused");
    assert!(
        err.to_string().contains("/no/such/path"),
        "the error should name the missing source: {err}"
    );
}

#[test]
fn an_exit_code_is_reported() {
    needs_sandbox!();
    let (status, _) = run(|s| shell(s, "exit 7"));
    assert_eq!(status.code, Some(7));
    assert!(!status.success());

    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["/bin/sh"]);
    spec.argv = ["/bin/sh", "-c", "kill -9 $$"].map(Into::into).to_vec();
    let (out_r, out_w) = pipe();
    let mut job = Launch::new(&spec).stdout(out_w).spawn().unwrap();
    let status = job.wait().unwrap();
    assert_eq!(
        status.signal,
        Some(9),
        "a killed command reports its signal"
    );
    let mut s = String::new();
    let _ = {
        let mut f = out_r;
        f.read_to_string(&mut s)
    };
}
