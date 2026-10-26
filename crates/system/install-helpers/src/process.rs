//! `subprocess.run(argv, capture_output=True, timeout=…)`: a child with
//! both outputs captured, ended — with the processes it started — when it
//! outlives its timeout or floods its output.
//!
//! The execution itself is `bounded-command`'s, the one contract every
//! privileged helper in the workspace runs an external tool under. What stays
//! here is the shape the helpers' Python originals had: bytes on standard
//! input, both streams captured, and `subprocess.run`'s two error messages.

use std::ffi::OsStr;
use std::time::Duration;

use crate::{Error, Result};

/// The most one tool may write on either stream. The tools here answer with a
/// device list, a mount table or a manifest — kilobytes; anything near this is
/// not an answer.
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// What a finished child left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The exit status, or `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// Whether it exited 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The exit status as Python's `returncode` reports it: negative for a
    /// signal.
    #[must_use]
    pub fn returncode(&self) -> i32 {
        self.code.unwrap_or(-1)
    }
}

/// The program's name, as `Path(argv[0]).name`.
#[must_use]
pub fn name(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Runs `argv` with stdin from `input` (empty for none), both outputs
/// captured, and kills it after `timeout`.
///
/// # Errors
/// The program missing (as `OSError`), or the timeout (as `TimeoutExpired`).
pub fn run(argv: &[&str], input: Option<&[u8]>, timeout: Duration) -> Result<Output> {
    let argv: Vec<&OsStr> = argv.iter().map(OsStr::new).collect();
    run_os(&argv, input, timeout)
}

/// [`run`] for arguments that may not be UTF-8, as a Python script's paths
/// were.
///
/// # Errors
/// As [`run`].
pub fn run_os(argv: &[&OsStr], input: Option<&[u8]>, timeout: Duration) -> Result<Output> {
    let Some((program, rest)) = argv.split_first() else {
        return Err(Error::new("empty command line"));
    };
    let shown = argv
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let output = bounded_command::run_os(program, rest, input, OUTPUT_LIMIT, timeout).map_err(
        |failure| {
            match failure {
                bounded_command::Error::Process(error) => {
                    Error::new(format!("{error}: '{}'", program.to_string_lossy()))
                }
                // A tool that outlives its deadline or floods its output is not
                // answering the request it was given: both read as one failure, in
                // the words `subprocess.run` used for the deadline.
                bounded_command::Error::Timeout(_) => Error::new(format!(
                    "Command '{shown}' timed out after {} seconds",
                    timeout.as_secs()
                )),
                // Not a timeout, and not worded as one: a caller that reads a
                // timeout from the message must not mistake a flood for it.
                other => Error::new(format!("{shown}: {other}")),
            }
        },
    )?;
    Ok(Output {
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// `subprocess.run(argv, check=True, capture_output=True, timeout=…).stdout`.
///
/// # Errors
/// As [`run`], or a nonzero exit (as `CalledProcessError`).
pub fn checked(argv: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let output = run(argv, None, timeout)?;
    if output.success() {
        Ok(output.stdout)
    } else {
        Err(Error::new(format!(
            "Command '{}' returned non-zero exit status {}.",
            argv.join(" "),
            output.returncode()
        )))
    }
}

/// [`checked`], decoded and stripped as `text=True` and `.strip()` left it.
///
/// # Errors
/// As [`checked`].
pub fn checked_text(argv: &[&str], timeout: Duration) -> Result<String> {
    let stdout = checked(argv, timeout)?;
    Ok(crate::py::strip(&String::from_utf8_lossy(&stdout)).to_owned())
}

#[cfg(test)]
mod tests {
    use super::{OUTPUT_LIMIT, run};
    use std::time::Duration;

    /// Long enough that a loaded machine never turns the expected result into
    /// a timeout.
    const UNTESTED_TIMEOUT: Duration = Duration::from_secs(60);

    #[test]
    fn a_tool_reads_its_input_and_leaves_both_streams_and_its_status() {
        let output = run(
            &[
                "sh",
                "-c",
                "read line; printf '%s' \"$line\"; printf e >&2; exit 3",
            ],
            Some(b"start=2048, size=4096\n"),
            UNTESTED_TIMEOUT,
        )
        .expect("sh runs");
        assert_eq!(output.code, Some(3));
        assert!(!output.success());
        assert_eq!(output.returncode(), 3);
        assert_eq!(output.stdout, b"start=2048, size=4096");
        assert_eq!(output.stderr, b"e");
    }

    #[test]
    fn a_missing_program_is_the_os_error_python_reported() {
        let error = run(&["tilo-no-such-tool", "--version"], None, UNTESTED_TIMEOUT)
            .expect_err("a missing program cannot run");
        assert!(
            error.0.contains("'tilo-no-such-tool'"),
            "the message names the program: {error}"
        );
    }

    #[test]
    fn a_wedged_tool_is_ended_at_its_deadline() {
        let started = std::time::Instant::now();
        let error = run(&["sleep", "30"], None, Duration::from_millis(100))
            .expect_err("a wedged tool is not waited for");
        assert_eq!(
            error.0, "Command 'sleep 30' timed out after 0 seconds",
            "the deadline is reported as `subprocess.run` reported it"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the call returned instead of waiting on the child"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_wedged_tool_is_killed_with_the_child_it_started() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("helper");
        let marker = directory.path().join("child.pid");
        fs_write(&helper, "#!/bin/sh\nsleep 3600 & echo $! > \"$1\"\nwait\n");
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = super::run_os(
            &[helper.as_os_str(), marker.as_os_str()],
            None,
            Duration::from_millis(100),
        )
        .expect_err("a wedged helper is ended");
        assert!(error.0.contains("timed out"), "{error}");
        let pid: i32 = std::fs::read_to_string(&marker)
            .expect("the helper started its child")
            .trim()
            .parse()
            .expect("a pid");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: signal 0 only probes whether the pid exists.
            if unsafe { libc::kill(pid, 0) } != 0 {
                let gone = std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                assert!(gone, "the probe failed for another reason");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the helper's child {pid} survived the deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn fs_write(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_flooding_tool_fails_as_a_flood_not_as_a_timeout() {
        let error = run(
            &["sh", "-c", "head -c 16777216 /dev/zero"],
            None,
            UNTESTED_TIMEOUT,
        )
        .expect_err("a tool that floods its output is not answering");
        assert!(
            !error.0.starts_with("Command '"),
            "a flood is not worded as a timeout: {error}"
        );
        assert!(
            error.0.contains(&OUTPUT_LIMIT.to_string()),
            "the message names the bound: {error}"
        );
    }
}
