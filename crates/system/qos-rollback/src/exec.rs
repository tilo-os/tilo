//! Running external tools, with the secrets kept off `argv`.
//!
//! Everything that talks to the TPM or to cryptsetup goes through here so that
//! (a) the decision logic above it stays testable without hardware, and (b)
//! there is exactly one place that knows how to avoid leaking a PIN or a
//! passphrase into `/proc/<pid>/cmdline`.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Result of one external invocation.
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
    /// Last non-empty stderr line — the useful part of a tpm2-tools failure,
    /// which prints several layers of ESYS scaffolding above it.
    pub fn last_error_line(&self) -> String {
        self.stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .next_back()
            .unwrap_or("(no diagnostic)")
            .to_string()
    }
}

/// One external command runner. A trait so tests can substitute a recorder.
pub trait Runner {
    fn run(&self, program: &str, args: &[&OsStr], stdin: Option<&[u8]>) -> Result<Output>;
}

/// Runs real processes, optionally with extra environment (the TCTI, chiefly).
#[derive(Debug, Clone, Default)]
pub struct Real {
    env: Vec<(String, String)>,
    /// Directory to run in; scratch files (session contexts, policy digests)
    /// land here.
    cwd: Option<PathBuf>,
}

impl Real {
    pub fn new() -> Self {
        Self::default()
    }

    /// TCTI for tpm2-tools. `None` leaves the tools' own default, which resolves
    /// to `/dev/tpmrm0` — and that is the one you want on real hardware: the
    /// in-kernel resource manager virtualises transient object and session
    /// slots, so we do not have to hand-manage a TPM with three object slots
    /// (which is what a bare `swtpm` socket gives you, and what makes the
    /// integration test flush so aggressively).
    pub fn with_tcti(mut self, tcti: Option<&str>) -> Self {
        if let Some(t) = tcti {
            self.env.push(("TPM2TOOLS_TCTI".into(), t.into()));
        }
        self
    }

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
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {program}: is it installed in the image?"))?;
        if let Some(bytes) = stdin {
            child
                .stdin
                .as_mut()
                .expect("stdin piped")
                .write_all(bytes)
                .with_context(|| format!("writing stdin to {program}"))?;
        }
        let out = child
            .wait_with_output()
            .with_context(|| format!("waiting for {program}"))?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Convenience: run and fail with the tool's own last diagnostic line.
pub fn require(runner: &dyn Runner, program: &str, args: &[&OsStr], what: &str) -> Result<Output> {
    let out = runner.run(program, args, None)?;
    if !out.ok() {
        bail!("{what} failed ({program} exit {}): {}", out.code, out.last_error_line());
    }
    Ok(out)
}

/// Build an `&[&OsStr]` from string-ish pieces without repeating the ceremony.
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
    fn last_error_line_picks_the_bottom_line() {
        let o = Output {
            code: 1,
            stdout: vec![],
            stderr: "WARNING:esys: noise\nERROR: real problem\n\n".into(),
        };
        assert_eq!(o.last_error_line(), "ERROR: real problem");
    }

    #[test]
    fn real_runner_captures_status_and_streams() {
        let r = Real::new();
        let out = r.run("/bin/sh", &argv!["-c", "printf hi; printf bad >&2; exit 3"], None).unwrap();
        assert_eq!(out.code, 3);
        assert_eq!(out.stdout_utf8(), "hi");
        assert_eq!(out.last_error_line(), "bad");
    }

    #[test]
    fn stdin_is_forwarded() {
        let r = Real::new();
        let out = r.run("/bin/cat", &argv![], Some(b"secret")).unwrap();
        assert_eq!(out.stdout_utf8(), "secret");
    }
}
