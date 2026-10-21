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
//!
//! The child's end arrives as its **stdin**. Any other inherited descriptor
//! would need a `pre_exec` hook to clear its close-on-exec flag, and a
//! `pre_exec` hook makes `Command::spawn` `fork()` the host — for the
//! compositor, a copy of every page table of a large multi-threaded process
//! and a copy-on-write fault on each page its own threads touch next — where
//! without one it uses `posix_spawn`, whose cost does not grow with the host.
//! The launcher puts `/dev/null` on its stdin once released
//! ([`wait_on_stdin`]), so nothing it runs inherits the gate.

#[cfg(not(target_os = "linux"))]
use std::io::Read as _;
use std::io::{self, Write as _};
#[cfg(not(target_os = "linux"))]
use std::os::fd::FromRawFd as _;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

/// The variable a host sets when the gate is the launcher's stdin. Internal to
/// a host and the launcher it starts; the launcher removes it before anything
/// it runs could inherit it.
pub const LAUNCH_REGISTRATION_STDIN_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_STDIN";

/// The variable an earlier host set instead, naming an inherited descriptor
/// (at least 3) the launcher waits on. Still honoured, so a launcher accepts a
/// host from before [`LAUNCH_REGISTRATION_STDIN_ENV`]; [`install`] never sets
/// it.
pub const LAUNCH_REGISTRATION_FD_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_FD";

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

    /// [`Self::release`], handing the launcher the shell's window door for
    /// the app (`runtime_portal::windows`) with the release: the launcher
    /// passes it on to the app it starts.
    ///
    /// # Errors
    ///
    /// The send error when the launcher is already gone.
    #[cfg(target_os = "linux")]
    pub fn release_with_window_door(self, door: std::os::fd::BorrowedFd<'_>) -> io::Result<()> {
        use std::os::fd::AsFd as _;
        runtime_portal::jobs::send(self.0.as_fd(), &[&[1]], &[door])
    }
}

/// The window door the shell handed over with the release, until the launch
/// takes it ([`take_window_door`]).
#[cfg(target_os = "linux")]
static WINDOW_DOOR: std::sync::Mutex<Option<OwnedFd>> = std::sync::Mutex::new(None);

/// The window door the shell released this launcher with, once; `None` when
/// it sent none.
#[cfg(target_os = "linux")]
pub fn take_window_door() -> Option<OwnedFd> {
    WINDOW_DOOR.lock().map_or_else(
        |poisoned| poisoned.into_inner().take(),
        |mut door| door.take(),
    )
}

/// Make `command` start gated: its child's stdin is a private socket, named by
/// [`LAUNCH_REGISTRATION_STDIN_ENV`], that it waits on before any launch work,
/// until the returned gate is released. This replaces any stdin set before.
///
/// Drop `command` once it has spawned: it holds the child's end until then.
///
/// # Errors
///
/// When the socket pair cannot be created.
pub fn install(command: &mut Command) -> io::Result<RegistrationGate> {
    let (release, wait) = UnixStream::pair()?;
    command
        .env_remove(LAUNCH_REGISTRATION_FD_ENV)
        .env(LAUNCH_REGISTRATION_STDIN_ENV, "1")
        .stdin(Stdio::from(OwnedFd::from(wait)));
    Ok(RegistrationGate(release))
}

/// [`install`] for a host that starts the launcher some other way than
/// `command.spawn()` (the app spawner, [`crate::spawner`]): `command` gets the
/// gate's variable, and the child's end comes back to be placed as the
/// launcher's stdin.
///
/// # Errors
///
/// When the socket pair cannot be created.
pub fn install_external(command: &mut Command) -> io::Result<(RegistrationGate, OwnedFd)> {
    let (release, wait) = UnixStream::pair()?;
    command
        .env_remove(LAUNCH_REGISTRATION_FD_ENV)
        .env(LAUNCH_REGISTRATION_STDIN_ENV, "1");
    Ok((RegistrationGate(release), OwnedFd::from(wait)))
}

/// The launcher's side of [`install`]: wait for the release on stdin, then put
/// `/dev/null` there. Call it first in `main`, while the process has one
/// thread and nothing has read stdin.
///
/// # Errors
///
/// End-of-file when the host dropped the gate unreleased, or the error of
/// reading stdin or of replacing it. The launcher must not go on after any.
pub fn wait_on_stdin() -> io::Result<()> {
    read_release()?;
    let null = std::fs::File::open("/dev/null")?;
    // SAFETY: both are descriptors this process owns. Replacing fd 0 closes
    // the gate in one step, so no later child can inherit it.
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read the host's release off stdin, keeping the window door that came with
/// it.
#[cfg(target_os = "linux")]
fn read_release() -> io::Result<()> {
    // SAFETY: fd 0 is the gate the host handed over, open until `dup2`
    // replaces it after this returns.
    let gate = unsafe { std::os::fd::BorrowedFd::borrow_raw(0) };
    if let Some(door) = read_release_from(gate)? {
        *WINDOW_DOOR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(door);
    }
    Ok(())
}

/// The release on `gate`, and the window door that came with it.
#[cfg(target_os = "linux")]
fn read_release_from(gate: std::os::fd::BorrowedFd<'_>) -> io::Result<Option<OwnedFd>> {
    let mut release = [0_u8; 1];
    let Some((1, fds)) = runtime_portal::jobs::receive(gate, &mut release)? else {
        return Err(io::ErrorKind::UnexpectedEof.into());
    };
    Ok(fds.into_iter().next())
}

#[cfg(not(target_os = "linux"))]
fn read_release() -> io::Result<()> {
    // SAFETY: fd 0 is the gate the host handed over. The `File` is never
    // dropped, so the descriptor stays open until `dup2` replaces it below.
    let mut gate = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
    let mut release = [0_u8; 1];
    gate.read_exact(&mut release)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::os::fd::FromRawFd as _;

    use super::*;

    /// Set only in the child this module's tests start.
    const CHILD_ENV: &str = "TILO_REGISTRATION_GATE_TEST_CHILD";
    const CHILD_TEST: &str = "registration_gate::tests::gated_child";

    /// The child side, run in a copy of this test binary: announce that it
    /// is waiting, wait on the gate as the launcher does, report what it
    /// read and what its stdin is afterwards. Without [`CHILD_ENV`] it is not
    /// the child and has nothing to do.
    #[test]
    fn gated_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        assert_eq!(
            std::env::var(LAUNCH_REGISTRATION_STDIN_ENV).as_deref(),
            Ok("1"),
            "the gate is named"
        );
        assert!(
            std::env::var_os(LAUNCH_REGISTRATION_FD_ENV).is_none(),
            "the descriptor-number handoff is never set alongside"
        );
        println!("waiting");
        if wait_on_stdin().is_err() {
            println!("closed");
            return;
        }
        println!("released");
        // `/dev/null`: end-of-file at once, where the gate would have
        // blocked or handed over another byte.
        let mut rest = Vec::new();
        // SAFETY: fd 0 is this process's stdin; the `File` is not dropped.
        let mut stdin = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
        let read = stdin
            .read_to_end(&mut rest)
            .expect("read the replaced stdin");
        let null = std::fs::metadata("/dev/null").expect("/dev/null");
        let current =
            std::fs::metadata("/proc/self/fd/0").or_else(|_| std::fs::metadata("/dev/fd/0"));
        let same = current.is_ok_and(|current| {
            use std::os::unix::fs::MetadataExt as _;
            current.rdev() == null.rdev() && current.ino() == null.ino()
        });
        println!(
            "{}",
            if read == 0 && same {
                "null-stdin"
            } else {
                "gate-still-stdin"
            }
        );
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
            // A host that once used the descriptor handoff: `install` drops it.
            .env(LAUNCH_REGISTRATION_FD_ENV, "9")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let gate = install(&mut command).expect("install the gate");
        let mut child = command.spawn().expect("spawn the gated child");
        drop(command);
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
            if let Some(report) = [
                "waiting",
                "released",
                "closed",
                "null-stdin",
                "gate-still-stdin",
            ]
            .into_iter()
            .find(|report| line.ends_with(report))
            {
                return report.to_owned();
            }
        }
    }

    /// The gated child goes no further than the gate until the host
    /// releases it — the host records the launch in between — its stdin is
    /// `/dev/null` from then on, and a gate dropped unreleased refuses the
    /// child instead of letting it on.
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
        assert_eq!(next_line(&mut stdout), "null-stdin");
        assert!(child.wait().expect("child exit").success());

        let (mut child, mut stdout, gate) = spawn_gated();
        assert_eq!(next_line(&mut stdout), "waiting");
        drop(gate);
        assert_eq!(next_line(&mut stdout), "closed");
        assert!(child.wait().expect("child exit").success());
    }

    /// The shell's window door rides on the release, and a release without
    /// one is still a release.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_window_door_comes_with_the_release() {
        use std::os::fd::AsFd as _;
        let (release, wait) = UnixStream::pair().expect("gate");
        let (door, _app) = UnixStream::pair().expect("door");
        RegistrationGate(release)
            .release_with_window_door(door.as_fd())
            .expect("release");
        assert!(read_release_from(wait.as_fd()).expect("released").is_some());

        let (release, wait) = UnixStream::pair().expect("gate");
        RegistrationGate(release).release().expect("release");
        assert!(read_release_from(wait.as_fd()).expect("released").is_none());

        let (release, wait) = UnixStream::pair().expect("gate");
        drop(RegistrationGate(release));
        assert!(
            read_release_from(wait.as_fd()).is_err(),
            "an unreleased gate refuses"
        );
    }
}
