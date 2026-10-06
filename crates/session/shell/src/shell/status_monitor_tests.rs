//! Real subprocess lifetime proof for the status monitor used by the shell.
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const TEST: &str = "shell::status::monitor_tests::status_monitor_exits_when_owner_exits";
const PHASE: &str = "TILO_STATUS_MONITOR_TEST_PHASE";
const MARKER: &str = "TILO_STATUS_MONITOR_TEST_MARKER";

fn fixture_command(phase: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(PHASE, phase);
    command
}

struct FixtureParent(Child);

impl Drop for FixtureParent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn monitor_fixture() {
    let marker = std::env::var(MARKER).expect("owned marker");
    let literal = serde_json::to_string(&marker).expect("marker literal");
    let script = format!(
        "import os,pathlib,time; pathlib.Path({literal}).write_text(str(os.getpid())); print('ready',flush=True); time.sleep(60)"
    );
    let (tx, _rx) = std::sync::mpsc::channel();
    let _ = super::monitor_until_exit("python3", &["-c", &script], &tx);
}

fn observed_pid(marker: &Path, parent: &mut Child) -> i32 {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("startup deadline");
    loop {
        if let Ok(text) = std::fs::read_to_string(marker)
            && let Ok(pid) = text.parse::<i32>()
        {
            assert!(pid > 1, "fixture monitor has a concrete PID");
            return pid;
        }
        assert!(
            parent.try_wait().expect("parent state").is_none(),
            "fixture parent exited before monitor startup"
        );
        assert!(Instant::now() < deadline, "fixture monitor never started");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn monitor_exit(pid: i32) -> Option<i32> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(2))
        .expect("exit deadline");
    loop {
        let mut status = 0;
        // SAFETY: status is writable; pid is the owned fixture's adopted child.
        let result = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        assert!(
            result >= 0,
            "owned monitor wait failed: {}",
            std::io::Error::last_os_error()
        );
        if result == pid {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn supervise_fixture() {
    // Only this separately exec'd fixture becomes a subreaper, never the suite.
    // SAFETY: prctl takes these integer arguments and no memory pointers.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let directory = tempfile::tempdir().expect("private fixture directory");
    let marker = directory.path().join("monitor.pid");
    let mut parent = FixtureParent(
        fixture_command("parent")
            .env(MARKER, &marker)
            .spawn()
            .expect("fixture parent"),
    );
    let pid = observed_pid(&marker, &mut parent.0);
    parent.0.kill().expect("end only the owned monitor parent");
    parent.0.wait().expect("reap fixture parent");
    let observed = monitor_exit(pid);
    if observed.is_none() {
        // Clean up the baseline's leaked child before reporting its failure.
        // SAFETY: pid names only the marker-identified owned fixture child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        assert!(
            monitor_exit(pid).is_some(),
            "fixture cleanup did not reap the monitor"
        );
    }
    let status = observed.expect("status monitor survives its shell owner");
    assert!(libc::WIFSIGNALED(status));
    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
}

#[test]
fn status_monitor_exits_when_owner_exits() {
    match std::env::var(PHASE).as_deref() {
        Ok("parent") => monitor_fixture(),
        Ok("supervisor") => supervise_fixture(),
        Err(std::env::VarError::NotPresent) => {
            let output = fixture_command("supervisor")
                .output()
                .expect("isolated supervisor");
            assert!(
                output.status.success(),
                "real monitor lifecycle failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        other => panic!("unexpected fixture phase: {other:?}"),
    }
}
