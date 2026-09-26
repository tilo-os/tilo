use super::{
    DaemonStart, MILESTONES, Observation, StartingProcess, StartupLimits, wait_until_ready,
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

/// A process that progresses until `progress_for`, then goes quiet.
struct Fake {
    started: Instant,
    progress_for: Duration,
    ready_after: Option<Duration>,
    exit_after: Option<Duration>,
    stopped: bool,
}

impl Fake {
    fn new(progress_for: Duration) -> Self {
        Self {
            started: Instant::now(),
            progress_for,
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
        if elapsed < self.progress_for {
            Observation {
                stage: "warming up",
                progress: u64::try_from(elapsed.as_millis() / 10).unwrap(),
            }
        } else {
            Observation {
                stage: "waiting on the lock",
                progress: u64::MAX,
            }
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
    assert!(elapsed >= Duration::from_millis(500), "{elapsed:?}");
    assert!(elapsed < LIMITS.cap, "{elapsed:?}");
    assert!(fake.stopped);
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
            .contains("within 0.8 s (still warming up)"),
        "{error}"
    );
    assert!(fake.stopped);
}

fn daemon_start<'a>(profile: &'a std::path::Path, script: &str) -> DaemonStart<'a> {
    let log = profile.join("daemon.log");
    let child = Command::new("/bin/sh")
        .args(["-c", script])
        .current_dir(profile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    DaemonStart {
        profile,
        log,
        child,
        before: MILESTONES.map(|_| None),
    }
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
fn real_child_progress_and_stall_are_observed() {
    let profile = tempfile::tempdir().unwrap();
    // Reaches two milestones, then blocks without using CPU or writing.
    let mut start = daemon_start(
        profile.path(),
        "sleep 0.2; : > runtime.lock; sleep 0.2; : > workspace.json; exec sleep 60",
    );
    let pid = start.child.id();
    let error = wait_until_ready(&mut start, LIMITS).unwrap_err();
    let message = error.to_string();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        message.contains("stalled while starting the identity service"),
        "{message}"
    );
    assert!(message.contains("wrote nothing"), "{message}");
    // The stalled child was killed and reaped.
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn cpu_time_of_a_live_process_is_readable() {
    assert!(super::cpu_ticks(std::process::id()).is_some());
}
