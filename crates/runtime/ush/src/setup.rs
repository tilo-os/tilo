//! How a served session's guest setup reports to the supervisor, apart from
//! the person's terminal.
//!
//! The supervisor starts `ush run` for a session on this device with the
//! session's pty as standard input and output, and gives it the pty again as
//! [`TERMINAL_FD`] (`--terminal-fd`). Its standard error is a pipe back to
//! the supervisor. Everything `ush run` and the processes it starts while
//! setting the guest up say on standard error — its own log, a library's
//! warning, `bwrap`'s refusal, the reason setup failed — goes to the
//! supervisor, which logs it. The guest's first process puts the terminal on
//! standard error just before it starts the shell, so from the shell on,
//! everything is the person's.
//!
//! When setup fails, `ush run` says so with one [`SetupReport`] line; the
//! supervisor tells the app on the session's lease, and the app shows that
//! the environment could not be set up, never the error text in the shell.

use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// The descriptor a served `ush run`, and its guest's first process, hold
/// the session's terminal at.
pub const TERMINAL_FD: RawFd = 3;

/// The argument that names [`TERMINAL_FD`] to `ush run` and `guest-init`.
pub const TERMINAL_FD_ARG: &str = "--terminal-fd";

/// What `ush run` tells the supervisor about the guest it set up.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "setup", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SetupReport {
    /// No shell started; `reason` is the error, for the log.
    Failed { reason: String },
}

impl SetupReport {
    /// The report a line of `ush run`'s standard error is, if it is one.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }

    /// The report as the one line `ush run` writes.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }
}

/// Adopt the inherited terminal descriptor named by `--terminal-fd`, closed
/// on exec from here on: only the guest's first process is handed it again.
///
/// # Errors
/// Returns why `raw` is not an open descriptor this process may take.
pub fn adopt_terminal(raw: &str) -> anyhow::Result<OwnedFd> {
    let fd: RawFd = raw.parse()?;
    // SAFETY: fcntl only probes the supplied integer; ownership begins below.
    if fd < 3 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        anyhow::bail!("invalid inherited terminal descriptor");
    }
    // SAFETY: this explicit startup argument transfers one inherited fd.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: sets a flag on the descriptor just adopted.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd)
}

/// Make `terminal` this process's standard error: what the guest's first
/// process does just before it starts the shell, closing `terminal` after. Returns the standard error it replaced, to put back when the
/// shell could not be started.
///
/// # Errors
/// Returns `dup`'s or `dup2`'s error.
pub fn attach_stderr(terminal: &OwnedFd) -> std::io::Result<OwnedFd> {
    // SAFETY: duplicates this process's standard error into a new descriptor.
    let saved = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if saved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `saved` is the new descriptor just made.
    let saved = unsafe { OwnedFd::from_raw_fd(saved) };
    // SAFETY: replaces standard error with an open descriptor this process owns.
    if unsafe { libc::dup2(terminal.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(saved)
}

/// Put back the standard error [`attach_stderr`] replaced.
pub fn restore_stderr(saved: BorrowedFd<'_>) {
    // SAFETY: replaces standard error with an open descriptor this process owns.
    unsafe { libc::dup2(saved.as_raw_fd(), libc::STDERR_FILENO) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_report_is_one_line_and_nothing_else_reads_as_one() {
        let report = SetupReport::Failed {
            reason: "its network namespace: denied".into(),
        };
        let line = report.line();
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(SetupReport::parse(&line), Some(report));
        assert_eq!(SetupReport::parse("ush: something went wrong"), None);
        assert_eq!(SetupReport::parse(r#"{"setup":"unknown"}"#), None);
        assert_eq!(SetupReport::parse(""), None);
    }
}
