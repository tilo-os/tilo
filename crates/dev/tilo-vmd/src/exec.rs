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

/// The variables that place a process in a user's session. `runuser` sets
/// only `HOME`, `SHELL`, `USER` and `LOGNAME`, so without these the command
/// would keep the daemon's (root's `XDG_RUNTIME_DIR=/run/user/0`, no bus).
pub const SESSION_ENV: &[&str] = &[
    "XDG_RUNTIME_DIR",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_SEAT",
    "XDG_SESSION_ID",
    "XDG_SESSION_TYPE",
    "TILO_TEST_CONTROL",
    "TILO_IDENTITY_ROOT",
];

/// The [`SESSION_ENV`] entries of a `/proc/PID/environ` image.
pub fn session_env(environ: &[u8]) -> Vec<(String, String)> {
    environ
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let (key, value) = std::str::from_utf8(entry).ok()?.split_once('=')?;
            SESSION_ENV
                .contains(&key)
                .then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Add the compositor's `WAYLAND_DISPLAY` to a session's `env`. The display is
/// named after the session process started, so that process's environment
/// never has it; the compositor publishes it under the runtime directory.
pub fn add_published_wayland_display(env: &mut Vec<(String, String)>) {
    if env.iter().any(|(key, _)| key == "WAYLAND_DISPLAY") {
        return;
    }
    let Some(runtime) = env
        .iter()
        .find(|(key, _)| key == "XDG_RUNTIME_DIR")
        .map(|(_, value)| std::path::PathBuf::from(value))
    else {
        return;
    };
    let Ok(published) = std::fs::read_to_string(runtime.join("tilo-session/wayland-display"))
    else {
        return;
    };
    let name = published.trim();
    if !name.is_empty() {
        env.push(("WAYLAND_DISPLAY".to_owned(), name.to_owned()));
    }
}

/// The command that runs `args` as `user` inside the mount namespace of
/// process `pid`, with that session's `env` (see [`session_env`]) in place
/// of the daemon's.
pub fn in_session(pid: u32, user: &str, env: &[(String, String)], args: &[String]) -> Command {
    let mut command = Command::new("nsenter");
    command
        .arg(format!("--target={pid}"))
        .arg("--mount")
        .args(["--", "runuser", "-u", user, "--", "env"]);
    for key in SESSION_ENV {
        command.arg(format!("--unset={key}"));
    }
    // `env` ends its options at the first assignment, so `--` must precede them.
    command.arg("--");
    for (key, value) in env {
        command.arg(format!("{key}={value}"));
    }
    command.args(args);
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

    fn env_runs_the_command_with_the_session_environment() {
        // Through the real `env`: an argv's shape cannot show where `env` stops parsing options.
        let command = in_session(
            4242,
            "tds-a",
            &[("XDG_RUNTIME_DIR".to_owned(), "/run/user/1001".to_owned())],
            &[
                "sh".into(),
                "-c".into(),
                "printf %s \"$XDG_RUNTIME_DIR\"".into(),
            ],
        );
        let args: Vec<_> = command.get_args().collect();
        let after_env = args
            .iter()
            .position(|arg| *arg == "env")
            .and_then(|at| at.checked_add(1))
            .expect("in_session runs env");
        let output = Command::new("env")
            .args(args.get(after_env..).expect("env has arguments"))
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .output()
            .expect("run env");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"/run/user/1001");
    }

    #[test]
    fn a_session_without_wayland_display_takes_the_published_one() {
        let runtime = tempfile::tempdir().unwrap();
        std::fs::create_dir(runtime.path().join("tilo-session")).unwrap();
        std::fs::write(
            runtime.path().join("tilo-session/wayland-display"),
            "wayland-1\n",
        )
        .unwrap();
        let runtime_env = (
            "XDG_RUNTIME_DIR".to_owned(),
            runtime.path().display().to_string(),
        );
        // Phase `the_published_name_fills_the_gap`.
        {
            let mut env = vec![runtime_env.clone()];
            add_published_wayland_display(&mut env);
            assert_eq!(
                env,
                [
                    runtime_env.clone(),
                    ("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned())
                ]
            );
        }
        // Phase `a_display_already_in_the_environment_is_kept`.
        {
            let own = ("WAYLAND_DISPLAY".to_owned(), "wayland-7".to_owned());
            let mut env = vec![runtime_env.clone(), own.clone()];
            add_published_wayland_display(&mut env);
            assert_eq!(env, [runtime_env, own]);
        }
        // Phase `nothing_published_adds_nothing`.
        {
            let empty = tempfile::tempdir().unwrap();
            let bare = (
                "XDG_RUNTIME_DIR".to_owned(),
                empty.path().display().to_string(),
            );
            let mut env = vec![bare.clone()];
            add_published_wayland_display(&mut env);
            assert_eq!(env, [bare]);
        }
    }

    #[test]
    fn exec_runs_in_the_session_as_the_slot_user_bounded_and_streamed() {
        // Phase `the_command_joins_the_session_namespace_as_the_slot_user`.
        {
            let env = session_env(
                b"HOME=/home/tds-a\0XDG_RUNTIME_DIR=/run/user/1001\0SECRET=x\0\
                  WAYLAND_DISPLAY=wayland-1\0TILO_IDENTITY_ROOT=/home/tds-a/.local/share/tilo/test-identity\0",
            );
            assert_eq!(
                env,
                [
                    ("XDG_RUNTIME_DIR".to_owned(), "/run/user/1001".to_owned()),
                    ("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned()),
                    (
                        "TILO_IDENTITY_ROOT".to_owned(),
                        "/home/tds-a/.local/share/tilo/test-identity".to_owned()
                    ),
                ]
            );
            let command = in_session(
                4242,
                "tds-a",
                &env,
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
                    "env"
                ]
                .into_iter()
                .map(str::to_owned)
                .chain(SESSION_ENV.iter().map(|key| format!("--unset={key}")))
                .chain([
                    "--".to_owned(),
                    "XDG_RUNTIME_DIR=/run/user/1001".to_owned(),
                    "WAYLAND_DISPLAY=wayland-1".to_owned(),
                    "TILO_IDENTITY_ROOT=/home/tds-a/.local/share/tilo/test-identity".to_owned(),
                    "sha256sum".to_owned(),
                    "/usr/bin/shell".to_owned(),
                ])
                .collect::<Vec<_>>()
            );
        }
        // Phase `env_runs_the_command_with_the_session_environment`.
        env_runs_the_command_with_the_session_environment();
        // Phase `output_streams_and_the_exit_status_passes_through`.
        {
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
        // Phase `runs_are_bounded_in_time_and_output`.
        {
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

            // A child that never ends on its own: the run ends only by
            // killing it once its time is up.
            let (done, ended) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let ended = run_bounded(
                    sh("exec sleep infinity"),
                    Duration::from_millis(300),
                    1000,
                    Sink::default(),
                    Sink::default(),
                );
                let _ = done.send(ended);
            });
            let ended = tilo_test_env::recv("the timed-out run to end", &ended).unwrap();
            assert_eq!(ended, Ended::TimedOut);
            assert_eq!(ended.code(), 124);
        }
    }
}
