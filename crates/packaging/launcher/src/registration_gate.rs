//! The registration gate between a host that launches `tilo-launcher` and
//! the launcher it starts.
//!
//! A host records each launch (pid, start time, app, room) so the broker can
//! attest the launcher when it connects. `Command::spawn` returns only after
//! the child exists, and a fast child could connect before the host recorded
//! it. The gate closes that race at the source: the host hands the child one
//! end of a private socket pair, the launcher blocks on it before doing any
//! launch work, and the host releases it only after the record exists. The
//! broker can then attest once, with no retry, and an unrecorded process is
//! refused at once.

use std::io::{self, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::process::Command;

/// The environment variable naming the inherited descriptor the launcher
/// waits on. Internal to a host and the launcher it starts; the launcher
/// removes it before anything it runs could inherit it.
pub use runtime_portal::registration_gate::ENV as LAUNCH_REGISTRATION_FD_ENV;

/// The host's end of a launch's registration gate.
///
/// Dropping it without [`Self::release`] closes the gate: the launcher reads
/// end-of-file and refuses to launch.
#[derive(Debug)]
pub struct RegistrationGate(UnixStream);

impl RegistrationGate {
    /// Let the launcher go on. Call only once the launch is recorded.
    ///
    /// # Errors
    ///
    /// The write error when the launcher is already gone.
    pub fn release(mut self) -> io::Result<()> {
        self.0.write_all(&[1])
    }
}

/// Make `command` start gated: its child inherits a private descriptor named
/// by [`LAUNCH_REGISTRATION_FD_ENV`] and waits on it before any launch work,
/// until the returned gate is released.
///
/// # Errors
///
/// When the socket pair cannot be created.
pub fn install(command: &mut Command) -> io::Result<RegistrationGate> {
    let (release, wait) = UnixStream::pair()?;
    command.env(LAUNCH_REGISTRATION_FD_ENV, wait.as_raw_fd().to_string());
    // SAFETY: the closure runs after fork and before exec and calls only the
    // async-signal-safe `fcntl(2)`. The closure retains the child copy through
    // exec; the launcher closes it immediately after reading release.
    unsafe {
        command.pre_exec(move || {
            let inherited_fd = wait.as_raw_fd();
            let flags = libc::fcntl(inherited_fd, libc::F_GETFD);
            if flags == -1
                || libc::fcntl(inherited_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(RegistrationGate(release))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::os::fd::{FromRawFd as _, RawFd};
    use std::process::Stdio;

    use super::*;

    /// Set only in the child this module's tests start.
    const CHILD_ENV: &str = "TILO_REGISTRATION_GATE_TEST_CHILD";
    const CHILD_TEST: &str = "registration_gate::tests::gated_child";

    /// The child side, run in a copy of this test binary: announce that it
    /// is waiting, wait on the gate as the launcher does, report what it
    /// read. Without [`CHILD_ENV`] it is not the child and has nothing to do.
    #[test]
    fn gated_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        let fd: RawFd = std::env::var(LAUNCH_REGISTRATION_FD_ENV)
            .expect("the gate descriptor is named")
            .parse()
            .expect("a descriptor number");
        println!("waiting");
        // SAFETY: the parent handed this descriptor to this process through
        // `LAUNCH_REGISTRATION_FD_ENV`; nothing else owns it.
        let mut gate = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut release = [0_u8; 1];
        match gate.read_exact(&mut release) {
            Ok(()) => println!("released"),
            Err(_) => println!("closed"),
        }
    }

    /// Start the child gated; its stdout and the gate.
    fn spawn_gated() -> (
        std::process::Child,
        BufReader<std::process::ChildStdout>,
        RegistrationGate,
    ) {
        let mut command = Command::new(std::env::current_exe().expect("this test binary"));
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let gate = install(&mut command).expect("install the gate");
        let mut child = command.spawn().expect("spawn the gated child");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        (child, stdout, gate)
    }

    fn next_line(stdout: &mut BufReader<std::process::ChildStdout>) -> String {
        loop {
            let mut line = String::new();
            assert_ne!(
                stdout.read_line(&mut line).expect("child output"),
                0,
                "the child ended without reporting"
            );
            // libtest may print the test's name on the same line first.
            let line = line.trim();
            if let Some(report) = ["waiting", "released", "closed"]
                .into_iter()
                .find(|report| line.ends_with(report))
            {
                return report.to_owned();
            }
        }
    }

    /// The gated child goes no further than the gate until the host
    /// releases it — the host records the launch in between — and a gate
    /// dropped unreleased refuses the child instead of letting it on.
    #[test]
    fn a_gated_child_waits_for_its_release() {
        let (mut child, mut stdout, gate) = spawn_gated();
        assert_eq!(next_line(&mut stdout), "waiting");
        assert!(
            child.try_wait().expect("child status").is_none(),
            "the child is held at the gate"
        );
        gate.release().expect("release the child");
        assert_eq!(next_line(&mut stdout), "released");
        assert!(child.wait().expect("child exit").success());

        let (mut child, mut stdout, gate) = spawn_gated();
        assert_eq!(next_line(&mut stdout), "waiting");
        drop(gate);
        assert_eq!(next_line(&mut stdout), "closed");
        assert!(child.wait().expect("child exit").success());
    }
}
