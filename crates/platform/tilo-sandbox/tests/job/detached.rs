//! Detached jobs: a job's init outlives the thread and process that started it, is its
//! starter's parent's child, and writes its command's wait status to a file; a plain job dies
//! with the thread that started it.

use std::time::{Duration, Instant};

use tilo_sandbox::job::{JobSpec, Launch, Root};

use crate::needs_sandbox;
use crate::support::{TestCgroup, reap, start_in_child, status_file};

fn describe(dir: &std::path::Path, script: &str, cgroup: Option<&std::path::Path>) {
    std::fs::write(dir.join("kind"), "job").unwrap();
    std::fs::write(dir.join("script"), script).unwrap();
    if let Some(cg) = cgroup {
        std::fs::write(dir.join("cgroup"), cg.as_os_str().as_encoded_bytes()).unwrap();
    }
}

/// The fact detaching exists for: the parent-death signal follows the thread that started the
/// job, not its process.
#[test]
fn attached_init_dies_with_its_thread() {
    needs_sandbox!();
    let mut job = std::thread::spawn(|| {
        let spec = JobSpec::new(Root::Host { extra: vec![] }, ["sleep", "30"]);
        Launch::new(&spec).spawn().expect("start the job")
    })
    .join()
    .unwrap();
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = job.try_wait().unwrap() {
            break s;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "the job outlived its thread"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.signal, Some(9), "{status:?}");
}

#[test]
fn detached_init_outlives_its_starter_and_writes_status() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path(), "sleep 1; echo done; exit 7", None);
    let pid = start_in_child(dir.path());
    // The starter and its thread are gone; the init is ours.
    let proc_status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let parent = proc_status
        .lines()
        .find_map(|l| l.strip_prefix("PPid:"))
        .map(str::trim)
        .unwrap();
    assert_eq!(parent, std::process::id().to_string());
    reap(pid);
    let st = status_file(&dir.path().join("status")).expect("a 4-byte status");
    assert!(libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 7, "{st:#x}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out")).unwrap(),
        "done\n"
    );
}

#[test]
fn detached_status_empty_until_end() {
    needs_sandbox!();
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path(), "sleep 2", None);
    let pid = start_in_child(dir.path());
    assert_eq!(
        std::fs::metadata(dir.path().join("status")).unwrap().len(),
        0
    );
    reap(pid);
    assert_eq!(
        std::fs::metadata(dir.path().join("status")).unwrap().len(),
        4
    );
}

#[test]
fn killed_detached_job_writes_no_status() {
    needs_sandbox!();
    let cg = match TestCgroup::new() {
        Ok(cg) => cg,
        Err(why) => {
            eprintln!("skipped: no delegated cgroup here: {why}");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    describe(dir.path(), "sleep 30", Some(&cg.path));
    let pid = start_in_child(dir.path());
    // SAFETY: pidfd_open of our own child, a new descriptor we close below.
    let pidfd = i32::try_from(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) }).unwrap();
    assert!(
        pidfd >= 0,
        "pidfd_open: {}",
        std::io::Error::last_os_error()
    );
    cg.kill();
    let mut pfd = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd on the stack.
    let ready = unsafe { libc::poll(&raw mut pfd, 1, 5000) };
    assert_eq!(ready, 1, "the pidfd did not become readable");
    // SAFETY: closing the descriptor opened above.
    unsafe { libc::close(pidfd) };
    reap(pid);
    assert_eq!(
        std::fs::metadata(dir.path().join("status")).unwrap().len(),
        0
    );
}
