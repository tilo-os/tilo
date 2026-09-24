//! `tilo-vmd exec`: run one command as a slot sees the system, streamed and
//! bounded.
//!
//! The command joins the mount namespace of the slot's running session (the
//! session unit's main process), where the slot's layers are stacked over
//! `/usr` with `ExtensionDirectories=`, and runs as the slot's user. So it
//! sees exactly the files the session does: nothing is re-assembled for it.

use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How a bounded run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited(i32),
    /// Killed after the time limit.
    TimedOut,
    /// Killed once its output passed the byte limit.
    TooMuchOutput,
    /// Killed by a signal from elsewhere.
    Signalled,
}

impl Ended {
    /// The exit status `tilo-vmd exec` passes on: the command's own, 124
    /// for a timeout (as `timeout(1)`), 125 for too much output.
    pub fn code(self) -> u8 {
        match self {
            Self::Exited(code) => u8::try_from(code).unwrap_or(1),
            Self::TimedOut => 124,
            Self::TooMuchOutput => 125,
            Self::Signalled => 1,
        }
    }
}

/// The command that runs `args` as `user` inside the mount namespace of
/// process `pid`.
pub fn in_session(pid: u32, user: &str, args: &[String]) -> Command {
    let mut command = Command::new("nsenter");
    command
        .arg(format!("--target={pid}"))
        .arg("--mount")
        .args(["--", "runuser", "-u", user, "--"])
        .args(args);
    command
}

/// Run `command`, copying its stdout and stderr to `out` and `err` as they
/// come, and kill it after `timeout` or once `max_bytes` of output passed.
///
/// # Errors
/// The command could not be started.
pub fn run_bounded(
    mut command: Command,
    timeout: Duration,
    max_bytes: u64,
    out: impl Write + Send + 'static,
    err: impl Write + Send + 'static,
) -> std::io::Result<Ended> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let total = Arc::new(AtomicU64::new(0));
    let overflow = Arc::new(AtomicBool::new(false));
    let copiers = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .zip([
        Box::new(out) as Box<dyn Write + Send>,
        Box::new(err) as Box<dyn Write + Send>,
    ])
    .filter_map(|(source, sink)| Some((source?, sink)))
    .map(|(mut source, mut sink)| {
        let total = Arc::clone(&total);
        let overflow = Arc::clone(&overflow);
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            while let Ok(read) = source.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                let before = total.fetch_add(read as u64, Ordering::SeqCst);
                let room = usize::try_from(max_bytes.saturating_sub(before)).unwrap_or(usize::MAX);
                let _ = sink.write_all(buffer.get(..read.min(room)).unwrap_or_default());
                let _ = sink.flush();
                if read > room {
                    overflow.store(true, Ordering::SeqCst);
                    break;
                }
            }
        })
    })
    .collect::<Vec<_>>();
    let started = Instant::now();
    let status: Option<ExitStatus> = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if overflow.load(Ordering::SeqCst) || started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // After a kill, grandchildren may still hold the pipes open: give the
    // copiers a moment to pass on what was read, but do not wait on them.
    let drained = Instant::now();
    while status.is_none()
        && drained.elapsed() < Duration::from_secs(1)
        && !copiers.iter().all(std::thread::JoinHandle::is_finished)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    for copier in copiers {
        if status.is_some() || copier.is_finished() {
            let _ = copier.join();
        }
    }
    // A copier that stops reading may end the command itself (SIGPIPE).
    Ok(match status {
        _ if overflow.load(Ordering::SeqCst) => Ended::TooMuchOutput,
        Some(status) => status.code().map_or(Ended::Signalled, Ended::Exited),
        None => Ended::TimedOut,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn the_command_joins_the_session_namespace_as_the_slot_user() {
        let command = in_session(
            4242,
            "tds-a",
            &["sha256sum".into(), "/usr/bin/shell".into()],
        );
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "nsenter");
        assert_eq!(
            args,
            [
                "--target=4242",
                "--mount",
                "--",
                "runuser",
                "-u",
                "tds-a",
                "--",
                "sha256sum",
                "/usr/bin/shell"
            ]
        );
    }

    #[test]
    fn output_streams_and_the_exit_status_passes_through() {
        let (out, err) = (Sink::default(), Sink::default());
        let ended = run_bounded(
            sh("echo out; echo err >&2; exit 3"),
            Duration::from_secs(10),
            1 << 20,
            out.clone(),
            err.clone(),
        )
        .unwrap();
        assert_eq!(ended, Ended::Exited(3));
        assert_eq!(ended.code(), 3);
        assert_eq!(out.0.lock().unwrap().as_slice(), b"out\n");
        assert_eq!(err.0.lock().unwrap().as_slice(), b"err\n");
    }

    #[test]
    fn runs_are_bounded_in_time_and_output() {
        let out = Sink::default();
        let ended = run_bounded(
            sh("while :; do echo yyyyyyyy; done"),
            Duration::from_secs(10),
            1000,
            out.clone(),
            Sink::default(),
        )
        .unwrap();
        assert_eq!(ended, Ended::TooMuchOutput);
        assert_eq!(ended.code(), 125);
        assert_eq!(out.0.lock().unwrap().len(), 1000);

        let started = Instant::now();
        let ended = run_bounded(
            sh("exec sleep 30"),
            Duration::from_millis(300),
            1000,
            Sink::default(),
            Sink::default(),
        )
        .unwrap();
        assert_eq!(ended, Ended::TimedOut);
        assert_eq!(ended.code(), 124);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
