//! What the host's loop waits on between its frames: its door, its own wake, its task's
//! connection, and its instances' windows.
//!
//! Each descriptor is watched only for what the loop takes from it, so the wait ends only when
//! the loop has something to do. A running instance's window is watched for what the window
//! sends, which that instance's next frame reads. The window of an instance still being opened
//! is watched only for hanging up: what it sends meanwhile waits in the connection until the
//! instance runs and reads it, and a wait that ended on it would end again at once, forever.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use tilolang_tilo::background::Waker;

use crate::TilolangHost;

/// The host's own wake: work off the frame (a module read, a join, a record's read, a
/// publication) writes it, and the loop's wait ends. It owns both ends, so what woke the loop is
/// taken out here ([`Self::consume`]) and nowhere else.
pub struct Wake {
    read: UnixStream,
    write: Arc<UnixStream>,
}

impl Wake {
    /// A wake, unwritten.
    ///
    /// # Errors
    /// The socket pair cannot be made, or made non-blocking.
    pub fn new() -> std::io::Result<Self> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(Self {
            read,
            write: Arc::new(write),
        })
    }

    /// What writes this wake, from any thread.
    #[must_use]
    pub fn waker(&self) -> Waker {
        let write = Arc::clone(&self.write);
        Arc::new(move || {
            // A full socket already wakes the loop; nothing else can fail while the wake
            // lives, as it holds the reading end.
            let _ = (&*write).write(&[1]);
        })
    }

    /// The end to wait on.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }

    /// Take out everything written to it: the wait ends again only when it is written again.
    ///
    /// # Errors
    /// The socket failed.
    pub fn consume(&self) -> std::io::Result<()> {
        let mut drained = [0_u8; 256];
        loop {
            match (&self.read).read(&mut drained) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the wake's writing end closed",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

/// What ended a wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one readiness for each source the wait watches, each taken out by its owner"
)]
pub struct Ready {
    /// The door has something, or closed.
    pub door: bool,
    /// The host's wake was written: work off the frame has something for it.
    pub wake: bool,
    /// The task's connection has something.
    pub task: bool,
    /// A window sent something or closed, or an opening instance's window closed.
    pub windows: bool,
}

impl Ready {
    /// Whether anything ended the wait.
    #[must_use]
    pub fn any(self) -> bool {
        self.door || self.wake || self.task || self.windows
    }
}

/// Wait until the door, the wake, the task or a window has something for the host, for at most
/// `timeout_ms` (`-1`: as long as that takes; `0`: whether one has, without waiting).
///
/// # Errors
/// The wait itself failed (a signal's interruption is no failure: nothing is ready).
pub fn wait(
    door: Option<BorrowedFd<'_>>,
    wake: BorrowedFd<'_>,
    host: &TilolangHost,
    timeout_ms: i32,
) -> std::io::Result<Ready> {
    let watch = |fd: BorrowedFd<'_>, events: libc::c_short| libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    let mut fds = vec![watch(wake, libc::POLLIN)];
    let door_at = door.map(|door| {
        let at = fds.len();
        fds.push(watch(door, libc::POLLIN));
        at
    });
    let task_at = host.task_fd().map(|task| {
        let at = fds.len();
        fds.push(watch(task, libc::POLLIN));
        at
    });
    let windows_at = fds.len();
    for window in host.windows() {
        fds.push(watch(window, libc::POLLIN));
    }
    // No events asked: poll reports a hang-up, an error or a closed descriptor regardless.
    for window in host.opening_windows() {
        fds.push(watch(window, 0));
    }
    let count = libc::nfds_t::try_from(fds.len()).unwrap_or(libc::nfds_t::MAX);
    // SAFETY: `fds` is a live array of initialized pollfds naming open descriptors.
    let polled = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout_ms) };
    if polled < 0 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::Interrupted {
            Ok(Ready::default())
        } else {
            Err(error)
        };
    }
    let at = |index: Option<usize>| {
        index
            .and_then(|index| fds.get(index))
            .is_some_and(|fd| fd.revents != 0)
    };
    Ok(Ready {
        wake: at(Some(0)),
        door: at(door_at),
        task: at(task_at),
        windows: fds
            .get(windows_at..)
            .is_some_and(|windows| windows.iter().any(|fd| fd.revents != 0)),
    })
}
