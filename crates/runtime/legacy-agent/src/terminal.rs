//! A launch on a pseudo-terminal of its own ([`Stdio::Terminal`]).
//!
//! The app's three streams are the terminal's other end, and the terminal
//! is the controlling terminal of a session the app leads: it gets the
//! line discipline, `isatty`, job-control signals from the keys and
//! `SIGWINCH` on a resize, as it would in a terminal on the guest. The
//! agent holds the master: the host's keys are written to it and
//! everything the app prints is read from it.
//!
//! [`Stdio::Terminal`]: legacy_agent_protocol::Stdio::Terminal

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use legacy_agent_protocol::TerminalSize;

/// Both ends of a new pseudo-terminal.
pub struct Pty {
    pub master: File,
    pub slave: File,
}

impl Pty {
    /// A pseudo-terminal of `size`; both ends close on exec.
    pub fn open(size: TerminalSize) -> io::Result<Self> {
        let (mut master, mut slave) = (-1, -1);
        let mut winsize = winsize(size);
        // SAFETY: both descriptors are out-parameters `openpty` fills in; the
        // name may be null; `winsize` lives across the call.
        let opened = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &raw mut winsize,
            )
        };
        if opened != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openpty` succeeded: both are open descriptors only this
        // owns from here.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        for fd in [&master, &slave] {
            // SAFETY: `fcntl` on a descriptor this owns.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        Ok(Self {
            master: File::from(master),
            slave: File::from(slave),
        })
    }

    /// Make `command` run on the slave as the leader of a new session whose
    /// controlling terminal it is. `command` must not set a process group:
    /// a group leader cannot start a session. The new session is its own
    /// process group, so signalling the launch's group still reaches it.
    pub fn attach(&self, command: &mut Command) -> io::Result<()> {
        command
            .stdin(Stdio::from(self.slave.try_clone()?))
            .stdout(Stdio::from(self.slave.try_clone()?))
            .stderr(Stdio::from(self.slave.try_clone()?));
        // SAFETY: `setsid` and `ioctl` are async-signal-safe and allocate
        // nothing; standard input is the slave by now.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

/// Tell the terminal behind `master` it is now `size`; the kernel signals
/// its foreground process group.
pub fn resize(master: &File, size: TerminalSize) -> io::Result<()> {
    let winsize = winsize(size);
    // SAFETY: TIOCSWINSZ reads the struct, which lives across the call.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const winsize) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn winsize(size: TerminalSize) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows.max(1),
        ws_col: size.cols.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use tilo_test_env::SpawnUnderEnvLock as _;

    use super::*;

    #[test]
    fn an_app_on_the_terminal_sees_a_terminal_its_size_and_its_keys() {
        let pty = Pty::open(TerminalSize {
            cols: 100,
            rows: 30,
        })
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "test -t 0 && stty size; read line; echo \"got $line\"",
        ]);
        pty.attach(&mut command).unwrap();
        let mut child = command.spawn_locked().unwrap();
        // Nothing here may hold the slave, or the master never ends.
        drop(command);
        let Pty { mut master, slave } = pty;
        drop(slave);
        master.write_all(b"hello\n").unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0_u8; 1024];
        // The slave's last holder is gone once the shell exits: EIO.
        while let Ok(read) = master.read(&mut buffer) {
            if read == 0 {
                break;
            }
            seen.extend_from_slice(&buffer[..read]);
        }
        assert!(child.wait().unwrap().success());
        let seen = String::from_utf8_lossy(&seen);
        assert!(seen.contains("30 100"), "{seen}");
        assert!(seen.contains("got hello"), "{seen}");
    }
}
