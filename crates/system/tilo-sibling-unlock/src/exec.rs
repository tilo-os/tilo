//! External tools (cryptsetup, systemd-creds, systemctl), with secrets kept on
//! stdin and off `argv`.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

/// Runs `program` with `args`, feeding `stdin`, and returns its stdout.
///
/// # Errors
/// When the program cannot run or exits unsuccessfully; the error carries the
/// last line of its stderr.
pub fn run<I, S>(program: &str, args: I, stdin: Option<&[u8]>) -> Result<Zeroizing<Vec<u8>>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input)
            .with_context(|| format!("could not write to {program}"))?;
    }
    let output = child
        .wait_with_output()
        .with_context(|| format!("{program} did not finish"))?;
    let stdout = Zeroizing::new(output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        bail!("{program} failed ({}): {reason}", output.status);
    }
    Ok(stdout)
}

/// Like [`run`], for a program whose success is all that matters.
///
/// # Errors
/// As [`run`].
pub fn check<I, S>(program: &str, args: I, stdin: Option<&[u8]>) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run(program, args, stdin).map(drop)
}
