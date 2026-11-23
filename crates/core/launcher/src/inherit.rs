//! Descriptors a started program inherits at fixed numbers, and the game performance hints it
//! starts with: how a planned Windows sandbox is started, whether the compatibility service starts
//! it itself or the session starts it as one of its own launches.

use std::os::fd::RawFd;
use std::os::unix::process::CommandExt as _;
use std::process::Command;

/// Make the program `command` starts find each `(source, target)` descriptor of `handed` at
/// `target`, and start with the game performance hints when `latency_sensitive`.
///
/// In the child each source is first copied above every target, so handing one down never
/// overwrites the source of the next, whatever numbers they have; dup2 clears close-on-exec on
/// each target. The sources must stay open until the command is spawned.
pub fn hand_down(command: &mut Command, mut handed: Vec<(RawFd, RawFd)>, latency_sensitive: bool) {
    if handed.is_empty() && !latency_sensitive {
        return;
    }
    let above = handed
        .iter()
        .map(|&(_, target)| target.saturating_add(1))
        .max()
        .unwrap_or(0);
    // SAFETY: the closure only changes a vector allocated before the fork, in the child's own
    // copy, and calls async-signal-safe syscalls (the performance hints, fcntl and dup2).
    unsafe {
        command.pre_exec(move || {
            if latency_sensitive {
                crate::performance::apply_game_performance_hints()?;
            }
            for (source, _) in &mut handed {
                *source = libc::fcntl(*source, libc::F_DUPFD_CLOEXEC, above);
                if *source < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            for &(source, target) in &handed {
                if libc::dup2(source, target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use runtime_portal::process_link::{self, CHILD_FD, FromProcess, Packet as _};
    use std::os::fd::AsRawFd as _;
    use std::process::{Command, Stdio};

    /// Set in the process [`a_link_handed_as_stdin_reaches_the_sandbox`] starts.
    const TAKER: &str = "TILO_LAUNCHER_LINK_TAKER";

    /// A launcher started as the session starts it ([`process_link::spawn`], the link on its
    /// stdin) takes the link and hands it to the program it starts with stdin `/dev/null`, as the
    /// tilolang host's sandbox is: the program finds the link at [`CHILD_FD`], and the link
    /// answers the session.
    #[test]
    fn a_link_handed_as_stdin_reaches_the_sandbox() {
        if std::env::var_os(TAKER).is_some() {
            let link = process_link::take().unwrap();
            let mut sandbox = Command::new("sh");
            sandbox
                .args([
                    "-c",
                    &format!("[ -S /proc/self/fd/{CHILD_FD} ] && ! [ -S /proc/self/fd/0 ]"),
                ])
                .stdin(Stdio::null());
            super::hand_down(&mut sandbox, vec![(link.as_raw_fd(), CHILD_FD)], false);
            assert!(
                sandbox.status().unwrap().success(),
                "the link is at its slot, not stdin"
            );
            FromProcess::Serves.send(&link).unwrap();
            return;
        }
        let mut launcher = Command::new(std::env::current_exe().unwrap());
        launcher
            .args([
                "--exact",
                "inherit::tests::a_link_handed_as_stdin_reaches_the_sandbox",
            ])
            .env(TAKER, "1");
        let (link, mut child) = process_link::spawn(launcher).unwrap();
        assert!(
            child.wait().unwrap().success(),
            "the launcher handed its link on"
        );
        assert!(
            process_link::take_serves(&link).unwrap(),
            "the taken link answers"
        );
    }
}
