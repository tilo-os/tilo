//! Holder startup through the MCP socket, with a separate process owning the real lock.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;

use advisory_lock::FileLock;
use rustix::process::{Pid, WaitOptions, waitpid};
use tilo_api::daemon::startup::{STAGES_FILE, StartupLimits};

use super::{DaemonPlace, Duration, Instant, ReachLimits, Started, reach_with_limits};

const FIXTURE: &str = "TILO_TEST_MCP_STARTUP_DIR";
const BEHAVIOR: &str = "TILO_TEST_MCP_STARTUP_BEHAVIOR";
const KEY: [u8; 32] = [0x52; 32];
const TICK: Duration = Duration::from_millis(100);

fn stages(path: &Path) -> File {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .unwrap()
}

fn claim(place: &DaemonPlace) -> FileLock {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(place.lock())
        .unwrap();
    let lock = FileLock::exclusive(file).unwrap();
    lock.resource_owner().unwrap();
    lock
}

/// Re-executed by the cases below so the lock's PID belongs to its kernel holder.
#[test]
fn holder_process() {
    let Some(dir) = std::env::var_os(FIXTURE).map(PathBuf::from) else {
        return;
    };
    let behavior = std::env::var(BEHAVIOR).unwrap();
    let place = DaemonPlace::at(&dir, &dir, &KEY);
    let path = dir.join(STAGES_FILE);
    let mut lock = (!matches!(behavior.as_str(), "idle" | "successor")).then(|| claim(&place));
    if lock.is_some() {
        writeln!(stages(&path), "opening the workspace").unwrap();
    }
    fs::write(dir.join(format!("ready-{behavior}")), b"").unwrap();
    let mut input = std::io::stdin().lock();
    if input.read_exact(&mut [0]).is_err() {
        return;
    }
    if behavior == "successor" {
        lock = Some(claim(&place));
    }
    for step in 1..=16 {
        thread::sleep(TICK);
        match behavior.as_str() {
            "advance" | "successor" | "die" => {
                writeln!(stages(&path), "opening service {step}").unwrap()
            }
            "stall" if step <= 3 => writeln!(stages(&path), "opening service {step}").unwrap(),
            "replace" => {
                let next = dir.join("next.stages");
                for row in 0..=step {
                    writeln!(stages(&next), "opening service {row}").unwrap();
                }
                fs::rename(next, &path).unwrap();
            }
            "corrupt" => stages(&path).write_all(b"\xff\0\n").unwrap(),
            _ => {}
        }
        if behavior == "die" && step == 3 {
            return;
        }
    }
    if behavior != "idle" {
        let _listener = UnixListener::bind(place.socket()).unwrap();
        let _ = input.read(&mut [0]);
    }
    drop(lock);
}

/// Keep the helper's stdin until the assertion ends. Reap it even after the reach loop took
/// its Child handle, so a failed assertion leaves no daemon behind.
struct Holder {
    process: RefCell<Option<Child>>,
    input: RefCell<Option<ChildStdin>>,
    pid: Pid,
}

impl Holder {
    fn spawn(dir: &Path, behavior: &str) -> Self {
        let mut process = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::held::startup_tests::holder_process",
                "--nocapture",
            ])
            .env(FIXTURE, dir)
            .env(BEHAVIOR, behavior)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = Pid::from_child(&process);
        let input = process.stdin.take();
        let ready = dir.join(format!("ready-{behavior}"));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            assert!(
                process.try_wait().unwrap().is_none(),
                "fixture exited before claiming its place"
            );
            assert!(Instant::now() < deadline, "fixture did not start");
            thread::sleep(Duration::from_millis(10));
        }
        Self {
            process: RefCell::new(Some(process)),
            input: RefCell::new(input),
            pid,
        }
    }

    fn go(&self) {
        self.input
            .borrow_mut()
            .as_mut()
            .unwrap()
            .write_all(&[1])
            .unwrap();
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        drop(self.input.get_mut().take());
        let _ = waitpid(Some(self.pid), WaitOptions::empty());
    }
}

fn reach(launched: &str, another: Option<&str>, has_profile: bool, cap: Duration) -> bool {
    let dir = tempfile::Builder::new()
        .prefix("mcp")
        .tempdir_in("/tmp")
        .unwrap();
    let place = DaemonPlace::at(dir.path(), dir.path(), &KEY);
    // Another service of the same login must never answer an MCP connect.
    let _api = UnixListener::bind(place.api()).unwrap();
    let started = Holder::spawn(dir.path(), launched);
    let other = another.map(|behavior| Holder::spawn(dir.path(), behavior));
    let limits = ReachLimits {
        first: Duration::from_millis(800),
        startup: StartupLimits {
            stall: Duration::from_millis(600),
            cap,
            poll: Duration::from_millis(15),
        },
        service: Duration::from_secs(8),
    };
    let began = Instant::now();
    let answer = reach_with_limits(
        &place,
        |_| {
            if let Some(child) = started.process.borrow_mut().take() {
                started.go();
                if let Some(other) = &other {
                    other.go();
                }
                Ok(Started::Process(child))
            } else {
                // The launched child ended. A replacement request has no verified child of its own.
                Ok(Started::Service(Instant::now()))
            }
        },
        has_profile.then(|| dir.path()),
        limits,
    );
    match answer {
        Ok(stream) => {
            assert_eq!(
                stream.peer_addr().unwrap().as_pathname(),
                Some(place.socket())
            );
            true
        }
        Err(why) => {
            assert!(why.contains("isn't answering"), "{why}");
            assert!(began.elapsed() >= limits.first);
            false
        }
    }
}

const CAP: Duration = Duration::from_secs(3);

#[test]
fn the_launched_holder_can_finish_after_the_first_deadline_while_its_stages_advance() {
    assert!(reach("advance", None, true, CAP));
}

#[test]
fn a_preexisting_stage_and_a_live_lock_do_not_extend_startup() {
    assert!(!reach("stale", None, true, CAP));
}

#[test]
fn an_advancing_holder_that_stalls_runs_out_of_time() {
    assert!(!reach("stall", None, true, CAP));
}

#[test]
fn replacing_the_stage_file_is_not_progress() {
    assert!(!reach("replace", None, true, CAP));
}

#[test]
fn corrupt_stage_records_do_not_extend_startup() {
    assert!(!reach("corrupt", None, true, CAP));
}

#[test]
fn a_live_holder_on_another_pid_cannot_extend_the_launched_process() {
    assert!(!reach("idle", Some("advance"), true, CAP));
}

#[test]
fn a_successor_cannot_continue_the_ended_childs_startup() {
    assert!(!reach("die", Some("successor"), true, CAP));
}

#[test]
fn even_an_advancing_child_has_a_total_deadline() {
    assert!(!reach("advance", None, true, Duration::from_millis(1150)));
}

#[test]
fn startup_without_a_login_profile_keeps_the_original_deadline() {
    assert!(!reach("advance", None, false, CAP));
}
