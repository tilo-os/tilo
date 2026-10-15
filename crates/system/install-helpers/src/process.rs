//! `subprocess.run(argv, capture_output=True, timeout=…)`: a child with
//! both outputs captured, killed when it outlives its timeout.

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    let mut child = Command::new(program)
        .args(rest)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::new(format!("{error}: '{program}'")))?;
    let writer = input.map(<[u8]>::to_vec).and_then(|data| {
        child.stdin.take().map(|mut stdin| {
            thread::spawn(move || {
                use std::io::Write as _;
                let _ = stdin.write_all(&data);
            })
        })
    });
    let reader = |stream: Option<Box<dyn std::io::Read + Send>>| {
        thread::spawn(move || {
            let mut data = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut data);
            }
            data
        })
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let deadline = Instant::now().checked_add(timeout);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(format!(
                "Command '{}' timed out after {} seconds",
                argv.join(" "),
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(5));
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    Ok(Output {
        code: status.code(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
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
