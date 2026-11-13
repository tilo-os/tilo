//! How a served session's guest setup reports to the supervisor, apart from the person's
//! terminal.
//!
//! The supervisor starts `ush run` with the session's pty as standard input and output and
//! gives it the pty again as [`TERMINAL_FD`]; its standard error is a pipe back to the
//! supervisor, so everything said while setting the guest up — its own log, a library's
//! warning, `bwrap`'s refusal — is the supervisor's to log. The guest's first process puts
//! the terminal on standard error just before it starts the shell.
//!
//! When setup fails, one [`SetupReport`] line tells the supervisor, which tells the app on
//! the session's lease; the app shows that the environment could not be set up, never the
//! error text in a shell.

use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

pub const TERMINAL_FD: RawFd = 3;

pub const TERMINAL_FD_ARG: &str = "--terminal-fd";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "setup", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SetupReport {
    Failed { reason: String },
}

impl SetupReport {
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }

    #[must_use]
    pub fn line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }
}

/// Adopt the inherited terminal descriptor named by `--terminal-fd`, closed
/// on exec from here on: only the guest's first process is handed it again.
pub fn adopt_terminal(raw: &str) -> anyhow::Result<OwnedFd> {
    let fd: RawFd = raw.parse()?;
    // SAFETY: fcntl only probes the supplied integer; ownership begins below.
    if fd < 3 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        anyhow::bail!("invalid inherited terminal descriptor");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
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
    let saved = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if saved < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let saved = unsafe { OwnedFd::from_raw_fd(saved) };
    if unsafe { libc::dup2(terminal.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(saved)
}

pub fn restore_stderr(saved: BorrowedFd<'_>) {
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
