//! Running external tools with secrets kept off `argv`: one place that knows how to avoid a PIN
//! or passphrase in `/proc/<pid>/cmdline`, and a [`Runner`] trait so the decision logic above it
//! stays testable without hardware.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    pub fn stdout_utf8(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    /// The most informative stderr line: the last one, unless it is tpm2-tools' generic
    /// `ERROR: Unable to run tpm2_foo` epilogue, which is skipped to keep the diagnosis.
    pub fn last_error_line(&self) -> String {
        let lines: Vec<&str> = self
            .stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        lines
            .iter()
            .rev()
            .find(|l| !l.starts_with("ERROR: Unable to run "))
            .or_else(|| lines.last())
            .unwrap_or(&"(no diagnostic)")
            .to_string()
    }
}

/// Runs external commands. A trait so tests can record instead.
pub trait Runner {
    /// # Errors
    ///
    /// `program` cannot be spawned, its `stdin` cannot be written, or waiting fails. A
    /// non-zero exit is not itself an error; callers inspect [`Output::ok`].
    fn run(&self, program: &str, args: &[&OsStr], stdin: Option<&[u8]>) -> Result<Output>;
}

/// Runs real processes, optionally with extra environment (chiefly the TCTI).
#[derive(Debug, Clone, Default)]
pub struct Real {
    env: Vec<(String, String)>,
    /// Directory to run in; scratch files (session contexts, policy digests) land here.
    cwd: Option<PathBuf>,
}

impl Real {
    pub fn new() -> Self {
        Self::default()
    }

    /// TCTI for tpm2-tools. `None` leaves their default, `/dev/tpmrm0`, which is what you want
    /// on real hardware: the kernel resource manager virtualises transient object and session
    /// slots, so nothing has to manage a TPM with three object slots by hand.
    #[must_use]
    pub fn with_tcti(mut self, tcti: Option<&str>) -> Self {
        if let Some(t) = tcti {
            self.env.push(("TPM2TOOLS_TCTI".into(), t.into()));
        }
        self
    }

    #[must_use]
    pub fn with_cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }
}

impl Runner for Real {
    fn run(&self, program: &str, args: &[&OsStr], stdin: Option<&[u8]>) -> Result<Output> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        let out =
            bounded_command::run_prepared(cmd, stdin, 8 * 1024 * 1024, Duration::from_secs(120))
                .with_context(|| {
                    format!("running bounded {program}: is it installed in the image?")
                })?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Is diagnostic tracing on? `TILO_ROLLBACK_DEBUG=1` turns it on for a boot that cannot
/// unseal, where the tool's own stderr is all there is. It prints tool arguments, which never
/// carry a secret: the PIN and passphrases are always passed by file reference.
pub fn debug_enabled() -> bool {
    std::env::var_os("TILO_ROLLBACK_DEBUG").is_some_and(|v| v != "0" && !v.is_empty())
}

/// Report a step that failed in a way the caller treats as an expected outcome, so the reason
/// is not lost.
pub fn trace_failure(program: &str, args: &[&OsStr], out: &Output) {
    if !debug_enabled() {
        return;
    }
    let joined: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    eprintln!(
        "tilo-rollback: {program} {} -> exit {}\n{}",
        joined.join(" "),
        out.code,
        out.stderr.trim_end()
    );
}

/// Run and fail with the tool's own last diagnostic line.
///
/// # Errors
///
/// `runner.run` itself fails, or `program` exits non-zero (with `what` and the tool's
/// [`Output::last_error_line`] in the message).
/// Returns an error if `runner.run` itself fails, or if `program` exits non-zero (with
/// `what` and the tool's [`Output::last_error_line`] in the message).
pub fn require(runner: &dyn Runner, program: &str, args: &[&OsStr], what: &str) -> Result<Output> {
    let out = runner.run(program, args, None)?;
    if !out.ok() {
        bail!(
            "{what} failed ({program} exit {}): {}",
            out.code,
            out.last_error_line()
        );
    }
    Ok(out)
}

/// Build an `&[&OsStr]` from string-ish pieces.
#[macro_export]
macro_rules! argv {
    ($($a:expr),* $(,)?) => {{
        let v: Vec<&std::ffi::OsStr> = vec![$(std::ffi::OsStr::new($a)),*];
        v
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_runs_capture_streams_forward_stdin_and_report_the_last_error() {
        {
            let o = Output {
                code: 1,
                stdout: vec![],
                stderr: "WARNING:esys: noise\nERROR: real problem\n\n".into(),
            };
            assert_eq!(o.last_error_line(), "ERROR: real problem");
        }
        {
            let o = Output {
                code: 1,
                stdout: vec![],
                stderr: "WARNING:esys: noise\nERROR: value is out of range\n\
                         ERROR: Unable to run tpm2_changeauth\n"
                    .into(),
            };
            assert_eq!(o.last_error_line(), "ERROR: value is out of range");
        }
        {
            let o = Output {
                code: 1,
                stdout: vec![],
                stderr: "ERROR: Unable to run tpm2_changeauth\n".into(),
            };
            assert_eq!(o.last_error_line(), "ERROR: Unable to run tpm2_changeauth");
        }
        {
            let r = Real::new();
            let out = r
                .run(
                    "/bin/sh",
                    &argv!["-c", "printf hi; printf bad >&2; exit 3"],
                    None,
                )
                .unwrap();
            assert_eq!(out.code, 3);
            assert_eq!(out.stdout_utf8(), "hi");
            assert_eq!(out.last_error_line(), "bad");
        }
        {
            let r = Real::new();
            let out = r.run("/bin/cat", &argv![], Some(b"secret")).unwrap();
            assert_eq!(out.stdout_utf8(), "secret");
        }
    }
}
