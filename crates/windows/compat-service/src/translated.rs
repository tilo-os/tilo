//! Lifetime management for a translated command inside an existing capsule.
//!
//! This does not create or relax a sandbox. The caller must already have
//! entered the capsule, including its namespace and Landlock restrictions.

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub(crate) const ROOTFS: &str = "/opt/qos/x86";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Run a guest command while retaining a ready, monitored FEX server.
pub fn run_translated_command(arguments: &[OsString]) -> io::Result<i32> {
    let Some((mode, arguments)) = arguments.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing guest command",
        ));
    };
    if arguments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing Windows executable",
        ));
    }
    let (mut guest, wine_server) = match mode.to_str() {
        Some("wine32") => (
            vec![OsString::from("/usr/bin/wine32")],
            Some("/usr/bin/wineserver32"),
        ),
        Some("wine64") => (
            vec![OsString::from("/usr/bin/wine64")],
            Some("/usr/bin/wineserver64"),
        ),
        Some("proton") => (
            vec![
                OsString::from("/usr/bin/python3"),
                OsString::from("/usr/libexec/umu_run.py"),
            ],
            None,
        ),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown translated runtime",
            ));
        }
    };
    guest.extend_from_slice(arguments);
    let (read_pipe, write_pipe) = readiness_pipe()?;
    let write_fd = write_pipe.as_raw_fd();
    let mut server_command = Command::new("/usr/bin/FEXServer");
    server_command
        .args(["--foreground", "--persistent", "300", "--wait_pipe"])
        .arg(write_fd.to_string())
        .stdin(Stdio::null());
    configure_environment(&mut server_command);
    // SAFETY: fcntl is async-signal-safe. The owned write descriptor remains
    // live until spawn finishes; only this descriptor needs to survive exec.
    unsafe {
        server_command.pre_exec(move || {
            if libc::fcntl(write_fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut server = ManagedChild::new(server_command.spawn()?, false);
    drop(write_pipe);
    wait_for_ready(&read_pipe, &mut server.child, STARTUP_TIMEOUT)?;
    drop(read_pipe);

    let status = run_guest(&guest, &mut server)?;
    if let Some(wine_server) = wine_server {
        let cleanup = run_guest(
            &[OsString::from(wine_server), OsString::from("-w")],
            &mut server,
        )?;
        if cleanup != 0 {
            return Err(io::Error::other(format!(
                "Wine server wait failed: {cleanup}"
            )));
        }
    }
    Ok(status)
}

fn run_guest(arguments: &[OsString], server: &mut ManagedChild) -> io::Result<i32> {
    let mut command = Command::new("/usr/bin/FEX");
    command.args(arguments).process_group(0);
    configure_environment(&mut command);
    let mut application = ManagedChild::new(command.spawn()?, true);
    loop {
        // Check the server first: even a simultaneous command exit must not
        // turn an unexpectedly terminated server into successful acceptance.
        if let Some(status) = server.child.try_wait()? {
            return Err(io::Error::other(format!(
                "FEXServer exited during execution: {status}"
            )));
        }
        if let Some(status) = application.child.try_wait()? {
            return Ok(exit_code(status));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn configure_environment(command: &mut Command) {
    command
        .env("FEX_ROOTFS", ROOTFS)
        // FEX otherwise puts its filesystem socket below XDG_RUNTIME_DIR,
        // where the capsule grants access only to its brokered Wayland socket.
        // This absolute override is shared by the server and every client.
        // /tmp is the capsule's private tmpfs, already writable under Landlock.
        .env("FEX_SERVERSOCKETPATH", "/tmp/qos-fex-server.sock")
        .env("FEX_INTERPRETER_INSTALLED", "0");
}

fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128_i32.saturating_add(status.signal().unwrap_or(1)))
}

fn readiness_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    // SAFETY: pipe2 writes exactly two descriptors into the supplied array.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let [read, write] = descriptors;
    // SAFETY: both successful pipe2 results are new owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(read), OwnedFd::from_raw_fd(write)) })
}

fn wait_for_ready(pipe: &OwnedFd, server: &mut Child, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        if let Some(status) = server.try_wait()? {
            return Err(io::Error::other(format!(
                "FEXServer exited before readiness: {status}"
            )));
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "FEXServer readiness timed out",
            ));
        }
        let mut event = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the descriptor is live and event is a valid pollfd.
        let result = unsafe { libc::poll(&raw mut event, 1, 50) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            continue;
        }
        let mut byte = 0_u8;
        // SAFETY: poll signalled a live pipe; the buffer holds one byte.
        let count = unsafe { libc::read(pipe.as_raw_fd(), (&raw mut byte).cast(), 1) };
        if count == 0 && server.try_wait()?.is_none() {
            return Ok(());
        }
        if count == -1 {
            return Err(io::Error::last_os_error());
        }
        return Err(io::Error::other("invalid FEXServer readiness response"));
    }
}

/// Ensure every failure path reaps the exact process this invocation spawned.
struct ManagedChild {
    child: Child,
    process_group: bool,
}

impl ManagedChild {
    const fn new(child: Child, process_group: bool) -> Self {
        Self {
            child,
            process_group,
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        let Ok(pid) = i32::try_from(self.child.id()) else {
            return;
        };
        // SAFETY: this child is still live and unreaped. Application groups
        // are created with process_group(0), never inherited from the caller.
        unsafe {
            libc::kill(
                if self.process_group {
                    pid.saturating_neg()
                } else {
                    pid
                },
                libc::SIGTERM,
            );
        }
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: the leader remains our live unreaped child, so its process
        // group cannot have been reused for an unrelated application.
        if self.process_group {
            unsafe {
                libc::kill(pid.saturating_neg(), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn inherited_socket_and_rootfs_overrides_are_replaced() {
        let mut child = Command::new("/usr/bin/env");
        child
            .env_clear()
            .env("FEX_ROOTFS", "/attacker/rootfs")
            .env("FEX_SERVERSOCKETPATH", "/attacker/server.sock")
            .env("FEX_INTERPRETER_INSTALLED", "1")
            .env("XDG_RUNTIME_DIR", "/run/user/1000");
        configure_environment(&mut child);
        let output = child.output().unwrap();
        assert!(output.status.success());
        let environment = String::from_utf8(output.stdout).unwrap();
        assert!(
            environment
                .lines()
                .any(|line| line == "FEX_SERVERSOCKETPATH=/tmp/qos-fex-server.sock")
        );
        assert!(
            environment
                .lines()
                .any(|line| line == format!("FEX_ROOTFS={ROOTFS}"))
        );
        assert!(
            environment
                .lines()
                .any(|line| line == "FEX_INTERPRETER_INSTALLED=0")
        );
        assert!(
            environment
                .lines()
                .any(|line| line == "XDG_RUNTIME_DIR=/run/user/1000")
        );
        assert!(!environment.contains("/attacker/"));
    }

    fn sleeping_server() -> ManagedChild {
        ManagedChild::new(Command::new("/bin/sleep").arg("5").spawn().unwrap(), false)
    }

    #[test]
    fn readiness_requires_eof_from_a_live_server() {
        let (read, write) = readiness_pipe().unwrap();
        let mut server = sleeping_server();
        drop(write);
        wait_for_ready(&read, &mut server.child, Duration::from_millis(200)).unwrap();
        assert!(server.child.try_wait().unwrap().is_none());
    }

    #[test]
    fn readiness_is_bounded_when_server_keeps_pipe_open() {
        let (read, _write) = readiness_pipe().unwrap();
        let mut server = sleeping_server();
        let error =
            wait_for_ready(&read, &mut server.child, Duration::from_millis(10)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn unexpected_readiness_bytes_are_rejected() {
        let (read, write) = readiness_pipe().unwrap();
        File::from(write).write_all(b"unexpected").unwrap();
        let mut server = sleeping_server();
        let error =
            wait_for_ready(&read, &mut server.child, Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("invalid FEXServer readiness"));
    }

    #[test]
    fn server_exit_is_not_readiness() {
        let (read, write) = readiness_pipe().unwrap();
        drop(write);
        let mut server = Command::new("/bin/true").spawn().unwrap();
        server.wait().unwrap();
        let error = wait_for_ready(&read, &mut server, Duration::from_millis(200)).unwrap_err();
        assert!(error.to_string().contains("before readiness"));
    }

    #[test]
    fn command_mode_is_required_and_allow_listed() {
        for arguments in [
            vec![],
            vec![OsString::from("wine64")],
            vec![OsString::from("/bin/sh"), OsString::from("app.exe")],
        ] {
            assert_eq!(
                run_translated_command(&arguments).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn guest_status_preserves_exit_codes_and_signals() {
        assert_eq!(exit_code(ExitStatus::from_raw(23 << 8)), 23);
        assert_eq!(
            exit_code(ExitStatus::from_raw(libc::SIGTERM)),
            128 + libc::SIGTERM
        );
    }
}
