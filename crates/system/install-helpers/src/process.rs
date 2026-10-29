//! `subprocess.run(argv, capture_output=True, timeout=…)`: a child with
//! both outputs captured, killed when it outlives its timeout.

use std::process::Command;
use std::time::Duration;

use crate::{Error, Result};

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
    let Some((program, rest)) = argv.split_first() else {
        return Err(Error::new("empty command line"));
    };
    let mut command = Command::new(program);
    command.args(rest);
    let output = bounded_command::run_prepared_with_input_limit(
        command,
        input,
        usize::MAX,
        usize::MAX,
        usize::MAX,
        timeout,
    )
    .map_err(|error| match error {
        bounded_command::Error::Timeout(_) => Error::new(format!(
            "Command '{}' timed out after {} seconds",
            argv.join(" "),
            timeout.as_secs()
        )),
        bounded_command::Error::Process(error) => Error::new(format!("{error}: '{program}'")),
        other => Error::new(other.to_string()),
    })?;
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
