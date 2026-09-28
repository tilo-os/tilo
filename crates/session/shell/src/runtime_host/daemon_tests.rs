use super::{
    DaemonStart, Observation, STAGES_FILE, StartingProcess, StartupLimits, current_stage,
    wait_until_ready,
};
use std::{
    io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const LIMITS: StartupLimits = StartupLimits {
    stall: Duration::from_millis(300),
    cap: Duration::from_secs(10),
    poll: Duration::from_millis(5),
};

/// A process that progresses until `progress_for`, then goes quiet. Only
/// the signal at `moving` changes; the others stay constant or unreadable.
struct Fake {
    started: Instant,
    progress_for: Duration,
    moving: usize,
    ready_after: Option<Duration>,
    exit_after: Option<Duration>,
    stopped: bool,
}

impl Fake {
    fn new(progress_for: Duration) -> Self {
        Self {
            started: Instant::now(),
            progress_for,
            moving: 0,
            ready_after: None,
            exit_after: None,
            stopped: false,
        }
    }
}

impl StartingProcess for Fake {
    type Ready = &'static str;
    fn ready(&mut self) -> Option<&'static str> {
        self.ready_after
            .is_some_and(|after| self.started.elapsed() >= after)
            .then_some("ready")
    }
    fn exited(&mut self) -> io::Result<Option<String>> {
        Ok(self
            .exit_after
            .is_some_and(|after| self.started.elapsed() >= after)
            .then(|| "exit status: 3".to_owned()))
    }
    fn observe(&mut self) -> Observation {
        let elapsed = self.started.elapsed();
        let (stage, value) = if elapsed < self.progress_for {
            (
                "warming up",
                u64::try_from(elapsed.as_millis() / 10).unwrap(),
            )
        } else {
            ("waiting on the lock", u64::MAX)
        };
        let mut signals = vec![("steady", Some(7)), ("absent", None), ("busy", Some(0))];
        signals[self.moving].1 = Some(value);
        Observation {
            stage: stage.to_owned(),
            signals,
        }
    }
    fn stop(&mut self) {
        self.stopped = true;
    }
    fn diagnostics(&self) -> String {
        "last output: fake tail".to_owned()
    }
}

#[test]
fn slow_start_that_keeps_progressing_is_waited_for() {
    let mut fake = Fake::new(Duration::from_secs(60));
    // Several stall windows long: only continued progress keeps it alive.
    fake.ready_after = Some(Duration::from_millis(1500));
    let started = Instant::now();
    assert_eq!(wait_until_ready(&mut fake, LIMITS).unwrap(), "ready");
    assert!(started.elapsed() >= Duration::from_millis(1500));
    assert!(!fake.stopped);
}

#[test]
fn stalled_start_fails_at_the_stall_timeout_naming_its_stage() {
    let mut fake = Fake::new(Duration::from_millis(200));
    let started = Instant::now();
    let error = wait_until_ready(&mut fake, LIMITS).unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let message = error.to_string();
    assert!(
        message.contains("stalled while waiting on the lock"),
        "{message}"
    );
    assert!(message.contains("fake tail"), "{message}");
    // Each signal says when it last moved, so a log shows which went quiet.
    assert!(message.contains("steady last changed"), "{message}");
    assert!(message.contains("absent unreadable"), "{message}");
    assert!(message.contains("busy unchanged since launch"), "{message}");
    assert!(elapsed >= Duration::from_millis(500), "{elapsed:?}");
    assert!(elapsed < LIMITS.cap, "{elapsed:?}");
    assert!(fake.stopped);
}

#[test]
fn progress_in_any_single_signal_keeps_the_start_alive() {
    let mut fake = Fake::new(Duration::from_secs(60));
    fake.moving = 2;
    fake.ready_after = Some(Duration::from_millis(1500));
    assert_eq!(wait_until_ready(&mut fake, LIMITS).unwrap(), "ready");
    assert!(!fake.stopped);
}

#[test]
fn exited_start_fails_immediately_with_its_output() {
    let mut fake = Fake::new(Duration::from_secs(60));
    fake.exit_after = Some(Duration::from_millis(20));
    let started = Instant::now();
    let message = wait_until_ready(&mut fake, LIMITS).unwrap_err().to_string();
    assert!(started.elapsed() < LIMITS.stall);
    assert!(
        message.contains("exited while warming up (exit status: 3)"),
        "{message}"
    );
    assert!(message.contains("fake tail"), "{message}");
}

#[test]
fn start_that_never_finishes_fails_at_the_cap() {
    let mut fake = Fake::new(Duration::from_secs(60));
    let limits = StartupLimits {
        cap: Duration::from_millis(800),
        ..LIMITS
    };
    let error = wait_until_ready(&mut fake, limits).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        error
            .to_string()
            .contains("within 0.8 s (still warming up; steady last changed"),
        "{error}"
    );
    assert!(fake.stopped);
}

fn daemon_start(profile: &std::path::Path, script: &str) -> DaemonStart {
    let child = Command::new("/bin/sh")
        .args(["-c", script])
        .current_dir(profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(profile.join("daemon.log")).unwrap())
        .spawn()
        .unwrap();
    DaemonStart::new(profile, child)
}

#[test]
fn real_child_exit_reports_its_stderr() {
    let profile = tempfile::tempdir().unwrap();
    let mut start = daemon_start(profile.path(), "echo 'identity: boom' >&2; exit 3");
    let message = wait_until_ready(&mut start, LIMITS)
        .unwrap_err()
        .to_string();
    assert!(message.contains("exit status: 3"), "{message}");
    assert!(message.contains("identity: boom"), "{message}");
}

#[test]
fn named_startup_steps_are_progress_and_a_stall_names_the_last_one() {
    let profile = tempfile::tempdir().unwrap();
    // Names a step every 0.2 s for several stall windows, without output,
    // then blocks without using CPU, reading, writing or naming a step.
    let mut start = daemon_start(
        profile.path(),
        &format!(
            "for step in 1 2 3 4 5 6 7 8; do sleep 0.2; echo \"step $step\" >> {STAGES_FILE}; \
             done; exec sleep 60"
        ),
    );
    let pid = start.child.id();
    let limits = StartupLimits {
        stall: Duration::from_secs(1),
        ..LIMITS
    };
    let started = Instant::now();
    let error = wait_until_ready(&mut start, limits).unwrap_err();
    let message = error.to_string();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(message.contains("stalled while step 8"), "{message}");
    assert!(
        started.elapsed() >= Duration::from_millis(2600),
        "{message}"
    );
    assert!(message.contains("startup step last changed"), "{message}");
    assert!(message.contains("wrote nothing"), "{message}");
    // What the stalled process was blocked on.
    assert!(message.contains("threads: sleep S"), "{message}");
    // The stalled child was killed and reaped.
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn the_current_stage_is_the_last_named_step() {
    assert_eq!(current_stage(b""), "loading the runtime");
    assert_eq!(current_stage(b"one\ntwo\n\n"), "two");
    assert_eq!(current_stage("x".repeat(500).as_bytes()).len(), 200);
}

#[cfg(target_os = "linux")]
#[test]
fn cpu_time_of_a_live_process_is_readable() {
    assert!(super::cpu_ticks(std::process::id()).is_some());
    assert!(super::io_bytes(std::process::id()).is_some());
}

/// A launcher that waits on a wrapper still sees the worker it started.
#[cfg(target_os = "linux")]
#[test]
fn a_process_tree_counts_its_descendants_cpu_time_and_runnable_threads() {
    use std::os::unix::process::CommandExt;
    let mut wrapper = Command::new("/bin/sh")
        .args(["-c", "sh -c 'while :; do :; done' & wait"])
        .process_group(0)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let pid = wrapper.id();
    let deadline = tilo_test_env::hang_deadline();
    let (mut sample, mut own) = (super::sample_tree(pid, false).unwrap(), 0);
    while sample.cpu_ticks <= own && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        own = super::cpu_ticks(pid).unwrap();
        sample = super::sample_tree(pid, false).unwrap();
    }
    let described = super::sample_tree(pid, true).unwrap();
    // SAFETY: the wrapper leads its own process group, which holds only
    // itself and the worker it started.
    unsafe {
        libc::killpg(i32::try_from(pid).unwrap(), libc::SIGKILL);
    }
    let _ = wrapper.wait();
    assert!(
        sample.cpu_ticks > own,
        "the tree never counted its busy descendant's CPU time within the hang bound: {sample:?}, wrapper {own}"
    );
    assert!(described.threads.len() >= 2, "{described:?}");
    assert!(
        described.held_by_machine || described.cpu_ticks > sample.cpu_ticks,
        "{described:?}"
    );
}

/// Without per-task child lists, children are found by their parent pid.
#[cfg(target_os = "linux")]
#[test]
fn parent_pids_find_a_spawned_child() {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let found = super::parent_pids().contains(&(child.id(), std::process::id()));
    let _ = child.kill();
    let _ = child.wait();
    assert!(found);
}
