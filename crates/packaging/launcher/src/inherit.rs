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
