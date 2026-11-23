//! The hold between a host and a child the host admits by a
//! launch record.
//!
//! A host records each launch (pid, start time, app, workspace) so the
//! broker can attest the process when it connects. `Command::spawn` returns
//! only after the child exists, and a fast child could connect before the
//! host recorded it. The hold closes that race at the source: the host hands
//! the child one end of a private socket pair, the child blocks on it before
//! it reaches the session, and the host releases it only after the record
//! exists. The broker can then attest once, with no retry, and an unrecorded
//! process is refused at once.
//!
//! Two kinds of host start such a child: a session starting `tilo-launcher`
//! for an app it opens ([`install`]), and a session starting its own surface
//! apps directly — the bar, the shell, the panels, the views it binds to a
//! slot of the shell — which call [`wait_on_stdin`] before they connect their
//! services.
//!
//! The child's end arrives as its **stdin**. Any other inherited descriptor
//! would need a `pre_exec` hook to clear its close-on-exec flag, and a
//! `pre_exec` hook makes `Command::spawn` `fork()` the host — for the
//! compositor, a copy of every page table of a large multi-threaded process
//! and a copy-on-write fault on each page its own threads touch next — where
//! without one it uses `posix_spawn`, whose cost does not grow with the host.
//! The child puts `/dev/null` on its stdin once released ([`wait_on_stdin`]),
//! so nothing it runs inherits the hold.

#[cfg(not(target_os = "linux"))]
use std::io::Read as _;
use std::io::{self, Write as _};
#[cfg(not(target_os = "linux"))]
use std::os::fd::FromRawFd as _;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

pub const LAUNCH_REGISTRATION_STDIN_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_STDIN";

/// The variable an earlier host set instead, naming an inherited descriptor
/// (at least 3) the child waits on. Still honoured, so a child accepts a host
/// from before [`LAUNCH_REGISTRATION_STDIN_ENV`]; [`install`] never sets it.
pub const LAUNCH_REGISTRATION_FD_ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_FD";

#[derive(Debug)]
pub struct LaunchHold(UnixStream);

impl LaunchHold {
    /// Let the child go on. Call only once the launch is recorded.
    pub fn release(mut self) -> io::Result<()> {
        self.0.write_all(&[1])
    }

    #[cfg(target_os = "linux")]
    pub fn release_with_window_link(self, link: std::os::fd::BorrowedFd<'_>) -> io::Result<()> {
        use std::os::fd::AsFd as _;
        crate::message::send(self.0.as_fd(), &[&[1]], &[link])
    }
}

/// The window link the shell handed over with the release, until the launch
/// takes it ([`take_window_link`]).
#[cfg(target_os = "linux")]
static WINDOW_LINK: std::sync::Mutex<Option<OwnedFd>> = std::sync::Mutex::new(None);

#[cfg(target_os = "linux")]
pub fn take_window_link() -> Option<OwnedFd> {
    WINDOW_LINK.lock().map_or_else(
        |poisoned| poisoned.into_inner().take(),
        |mut link| link.take(),
    )
}

/// Make `command` start held: its child's stdin is a private socket, named by
/// [`LAUNCH_REGISTRATION_STDIN_ENV`], that it waits on before it reaches the
/// session, until the returned hold is released. This replaces any stdin set
/// before.
///
/// Drop `command` once it has spawned: it holds the child's end until then.
pub fn install(command: &mut Command) -> io::Result<LaunchHold> {
    let (release, wait) = UnixStream::pair()?;
    command
        .env_remove(LAUNCH_REGISTRATION_FD_ENV)
        .env(LAUNCH_REGISTRATION_STDIN_ENV, "1")
        .stdin(Stdio::from(OwnedFd::from(wait)));
    Ok(LaunchHold(release))
}

pub fn install_external(command: &mut Command) -> io::Result<(LaunchHold, OwnedFd)> {
    let (release, wait) = UnixStream::pair()?;
    command
        .env_remove(LAUNCH_REGISTRATION_FD_ENV)
        .env(LAUNCH_REGISTRATION_STDIN_ENV, "1");
    Ok((LaunchHold(release), OwnedFd::from(wait)))
}

/// The child's side of [`install`]: wait for the host's release on stdin,
/// then put `/dev/null` there. Call it first in `main`, while the process has
/// one thread and nothing has read stdin, or in an app before it connects its
/// session's services.
///
/// # Errors
///
/// End-of-file when the host dropped the hold unreleased, or the error of
/// reading stdin or of replacing it. The child must not go on after any.
pub fn wait_on_stdin() -> io::Result<()> {
    read_release()?;
    let null = std::fs::File::open("/dev/null")?;
    // SAFETY: both are descriptors this process owns. Replacing fd 0 closes
    // the hold in one step, so no later child can inherit it.
    if unsafe { libc::dup2(null.as_raw_fd(), 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_release() -> io::Result<()> {
    // SAFETY: fd 0 is the socket the host handed over, open until `dup2`
    // replaces it after this returns.
    let hold = unsafe { std::os::fd::BorrowedFd::borrow_raw(0) };
    if let Some(link) = read_release_from(hold)? {
        *WINDOW_LINK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(link);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_release_from(hold: std::os::fd::BorrowedFd<'_>) -> io::Result<Option<OwnedFd>> {
    let mut release = [0_u8; 1];
    let Some((1, fds)) = crate::message::receive(hold, &mut release)? else {
        return Err(io::ErrorKind::UnexpectedEof.into());
    };
    Ok(fds.into_iter().next())
}

#[cfg(not(target_os = "linux"))]
fn read_release() -> io::Result<()> {
    // SAFETY: fd 0 is the socket the host handed over. The `File` is never
    // dropped, so the descriptor stays open until `dup2` replaces it below.
    let mut hold = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
    let mut release = [0_u8; 1];
    hold.read_exact(&mut release)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::os::fd::FromRawFd as _;
    use tilo_test_env::SpawnUnderEnvLock as _;

    use super::*;

    /// Set only in the child this module's tests start.
    const CHILD_ENV: &str = "TILO_LAUNCH_HOLD_TEST_CHILD";
    const CHILD_TEST: &str = "launch_hold::tests::held_child";

    /// The child side, run in a copy of this test binary: announce that it
    /// is waiting, wait on the hold as the launcher does, report what it
    /// read and what its stdin is afterwards. Without [`CHILD_ENV`] it is not
    /// the child and has nothing to do.
    #[test]
    fn held_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        assert_eq!(
            std::env::var(LAUNCH_REGISTRATION_STDIN_ENV).as_deref(),
            Ok("1"),
            "the hold is named"
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
        // `/dev/null`: end-of-file at once, where the hold would have
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
                "hold-still-stdin"
            }
        );
    }

    fn spawn_held() -> (
        std::process::Child,
        BufReader<std::process::ChildStdout>,
        LaunchHold,
    ) {
        let mut command = Command::new(std::env::current_exe().expect("this test binary"));
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .env(LAUNCH_REGISTRATION_FD_ENV, "9")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let hold = install(&mut command).expect("install the hold");
        let mut child = command.spawn_locked().expect("spawn the held child");
        drop(command);
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        (child, stdout, hold)
    }

    fn next_line(stdout: &mut BufReader<std::process::ChildStdout>) -> String {
        loop {
            let mut line = String::new();
            assert_ne!(
                stdout.read_line(&mut line).expect("child output"),
                0,
                "the child ended without reporting"
            );
            let line = line.trim();
            if let Some(report) = [
                "waiting",
                "released",
                "closed",
                "null-stdin",
                "hold-still-stdin",
            ]
            .into_iter()
            .find(|report| line.ends_with(report))
            {
                return report.to_owned();
            }
        }
    }

    /// the held child goes no further until the host
    /// releases it — the host records the launch in between — its stdin is
    /// `/dev/null` from then on, and a hold dropped unreleased refuses the
    /// child instead of letting it on.
    #[test]
    fn a_held_child_waits_for_its_release() {
        let (mut child, mut stdout, hold) = spawn_held();
        assert_eq!(next_line(&mut stdout), "waiting");
        assert!(
            child.try_wait().expect("child status").is_none(),
            "the child is held"
        );
        hold.release().expect("release the child");
        assert_eq!(next_line(&mut stdout), "released");
        assert_eq!(next_line(&mut stdout), "null-stdin");
        assert!(child.wait().expect("child exit").success());

        let (mut child, mut stdout, hold) = spawn_held();
        assert_eq!(next_line(&mut stdout), "waiting");
        drop(hold);
        assert_eq!(next_line(&mut stdout), "closed");
        assert!(child.wait().expect("child exit").success());
    }

    /// The shell's window link rides on the release, and a release without
    /// one is still a release.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_window_link_comes_with_the_release() {
        use std::os::fd::AsFd as _;
        let (release, wait) = UnixStream::pair().expect("socket pair");
        let (link, _app) = UnixStream::pair().expect("link");
        LaunchHold(release)
            .release_with_window_link(link.as_fd())
            .expect("release");
        assert!(read_release_from(wait.as_fd()).expect("released").is_some());

        let (release, wait) = UnixStream::pair().expect("socket pair");
        LaunchHold(release).release().expect("release");
        assert!(read_release_from(wait.as_fd()).expect("released").is_none());

        let (release, wait) = UnixStream::pair().expect("socket pair");
        drop(LaunchHold(release));
        assert!(
            read_release_from(wait.as_fd()).is_err(),
            "an unreleased hold refuses"
        );
    }
}
