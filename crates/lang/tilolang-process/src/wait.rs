//! What the host's loop waits on between its frames: its api, its own wake, a stop signal, its
//! task's connection, and its instances' windows.
//!
//! Each descriptor is watched only for what the loop takes from it, so the wait ends only when
//! the loop has something to do. A running instance's window is watched for what the window
//! sends, which that instance's next frame reads. The window of an instance still being opened
//! is watched only for hanging up: what it sends meanwhile waits in the connection until the
//! instance runs and reads it, and a wait that ended on it would end again at once, forever.
//!
//! On Windows the loop's connections are pipes and its wake and stop are events: one wait
//! (`tilo_socket::link::wait`) watches them all. A pipe is watched for anything to read, which a
//! window being opened would end every wait on, so there those windows are not watched: one that
//! closes ends its opening at the loop's next pass, which its module and join wake it for.

#[cfg(unix)]
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use runtime_portal::packet::Descriptor;
use tilolang_tilo::background::Waker;

use crate::TilolangProcess;
use crate::stop::Stop;

/// The host's own wake: work off the frame (a module read, a join, a document's read, a
/// publication) writes it, and the loop's wait ends. It owns both ends, so what woke the loop is
/// taken out here ([`Self::consume`]) and nowhere else. A socket pair on Unix, an event on Windows.
pub struct Wake {
    #[cfg(unix)]
    read: UnixStream,
    #[cfg(unix)]
    write: Arc<UnixStream>,
    #[cfg(windows)]
    event: Arc<std::os::windows::io::OwnedHandle>,
}

#[cfg(unix)]
impl Wake {
    /// A wake, unwritten.
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

    /// Wait until it is written, or `timeout` passes: whether the wait ended before its time
    /// ran out (a wait cut short by a signal counts as ended, as it may have been written).
    pub fn wait_for(&self, timeout: Duration) -> bool {
        let millis = i32::try_from(timeout.as_millis())
            .unwrap_or(i32::MAX)
            .max(1);
        let mut fds = [libc::pollfd {
            fd: self.read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `fds` is one valid pollfd for the duration of the call.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, millis) != 0 }
    }

    /// Take out everything written to it: the wait ends again only when it is written again.
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

#[cfg(windows)]
impl Wake {
    /// A wake, unwritten: a manual-reset event, set until it is taken out.
    pub fn new() -> std::io::Result<Self> {
        use std::os::windows::io::FromRawHandle as _;
        // SAFETY: no attributes and no name; manual reset, unset.
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                1,
                0,
                std::ptr::null(),
            )
        };
        if event.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            // SAFETY: an event CreateEventW just returned, owned from here.
            event: Arc::new(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(event) }),
        })
    }

    /// What writes this wake, from any thread.
    #[must_use]
    pub fn waker(&self) -> Waker {
        use std::os::windows::io::AsRawHandle as _;
        let event = Arc::clone(&self.event);
        Arc::new(move || {
            // SAFETY: the event the wake holds open for as long as this waker lives.
            unsafe { windows_sys::Win32::System::Threading::SetEvent(event.as_raw_handle()) };
        })
    }

    /// The event to wait on.
    #[must_use]
    pub fn handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        use std::os::windows::io::AsHandle as _;
        self.event.as_handle()
    }

    /// Wait until it is written, or `timeout` passes: whether the wait ended before its time
    /// ran out.
    pub fn wait_for(&self, timeout: Duration) -> bool {
        use std::os::windows::io::AsRawHandle as _;
        /// What the wait answers when its time ran out.
        const WAIT_TIMEOUT: u32 = 0x102;
        let millis = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX - 1)
            .max(1);
        // SAFETY: the event this wake owns.
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                self.event.as_raw_handle(),
                millis,
            ) != WAIT_TIMEOUT
        }
    }

    /// Whether it was written since it was last taken out.
    #[must_use]
    pub fn written(&self) -> bool {
        signalled(self.handle())
    }

    /// Take out what was written: the wait ends again only when it is written again.
    pub fn consume(&self) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: the event this wake owns.
        if unsafe { windows_sys::Win32::System::Threading::ResetEvent(self.event.as_raw_handle()) }
            == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Whether `event` is set, without waiting.
#[cfg(windows)]
pub(crate) fn signalled(event: std::os::windows::io::BorrowedHandle<'_>) -> bool {
    use std::os::windows::io::AsRawHandle as _;
    // SAFETY: a live event, asked without waiting.
    unsafe {
        windows_sys::Win32::System::Threading::WaitForSingleObject(event.as_raw_handle(), 0)
            == windows_sys::Win32::Foundation::WAIT_OBJECT_0
    }
}

/// What ended a wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one readiness for each source the wait watches, each taken out by its owner"
)]
pub struct Ready {
    /// The API has something, or closed.
    pub api: bool,
    /// The host's wake was written: work off the frame has something for it.
    pub wake: bool,
    /// The host was asked to stop ([`crate::stop`]).
    pub stop: bool,
    /// The task's connection has something.
    pub task: bool,
    /// A window sent something or closed, or an opening instance's window closed.
    pub windows: bool,
}

impl Ready {
    /// Whether anything ended the wait.
    #[must_use]
    pub fn any(self) -> bool {
        self.api || self.wake || self.stop || self.task || self.windows
    }
}

/// Wait until the API, the wake, a stop, the task or a window has something for the host, for at
/// most `timeout_ms` (`-1`: as long as that takes; `0`: whether one has, without waiting).
///
/// # Errors
/// The wait itself failed (a signal's interruption is no failure: nothing is ready).
#[cfg(unix)]
pub fn wait(
    api: Option<&Descriptor>,
    wake: &Wake,
    stop: Option<&Stop>,
    host: &TilolangProcess,
    timeout_ms: i32,
) -> std::io::Result<Ready> {
    let watch = |fd: BorrowedFd<'_>, events: libc::c_short| libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    let mut fds = vec![watch(wake.fd(), libc::POLLIN)];
    let stop_at = stop.map(|stop| {
        let at = fds.len();
        fds.push(watch(stop.fd(), libc::POLLIN));
        at
    });
    let api_at = api.map(|api| {
        let at = fds.len();
        fds.push(watch(api.as_fd(), libc::POLLIN));
        at
    });
    let task_at = host.task_link().map(|task| {
        let at = fds.len();
        fds.push(watch(task.as_fd(), libc::POLLIN));
        at
    });
    // Held for the wait's lifetime: a guest's connection is polled, not owned by the wait.
    let guests = host.task_guests();
    for guest in &guests {
        fds.push(watch(guest.as_fd(), libc::POLLIN));
    }
    let windows_at = fds.len();
    for window in host.windows() {
        fds.push(watch(window.as_fd(), libc::POLLIN));
    }
    // No events asked: poll reports a hang-up, an error or a closed descriptor regardless.
    for window in host.opening_windows() {
        fds.push(watch(window.as_fd(), 0));
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
        stop: at(stop_at),
        api: at(api_at),
        task: at(task_at),
        windows: fds
            .get(windows_at..)
            .is_some_and(|windows| windows.iter().any(|fd| fd.revents != 0)),
    })
}

/// Wait until the API, the wake, a stop, the task or a window has something for the host, for at
/// most `timeout_ms` (`-1`: as long as that takes; `0`: whether one has, without waiting).
///
/// # Errors
/// A connection that is no pipe, or the wait itself failed.
#[cfg(windows)]
pub fn wait(
    api: Option<&Descriptor>,
    wake: &Wake,
    stop: Option<&Stop>,
    host: &TilolangProcess,
    timeout_ms: i32,
) -> std::io::Result<Ready> {
    let api = api.map(Descriptor::stream).transpose()?;
    let task = host.task_link();
    let guests = host.task_guests();
    // A window being opened is not waited on here: what it sends before its instance runs would
    // end every wait, where Unix's poll asks such a window for its hang-up alone.
    let windows: Vec<&tilo_socket::Stream> = host
        .windows()
        .filter_map(|window| window.stream().ok())
        .collect();
    let mut streams: Vec<&tilo_socket::Stream> = api.into_iter().chain(task).collect();
    streams.extend(guests.iter());
    streams.extend(windows.iter().copied());
    let mut signals = vec![wake.handle()];
    signals.extend(stop.map(Stop::handle));
    let timeout = u64::try_from(timeout_ms).ok().map(Duration::from_millis);
    tilo_socket::link::wait(&streams, &signals, timeout)?;
    let ready = |stream: &tilo_socket::Stream| stream.ready().unwrap_or(true);
    Ok(Ready {
        wake: wake.written(),
        stop: stop.is_some_and(Stop::asked),
        api: api.is_some_and(ready),
        task: task.is_some_and(ready) || guests.iter().any(ready),
        windows: windows.iter().any(|window| ready(window)),
    })
}
