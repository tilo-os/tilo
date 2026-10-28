//! The thread that wakes an app when the shell has written to its floating
//! surface connection, and the rearm the app gives it once it has read.

use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

/// Tells a readable watcher ([`spawn_readable_watcher`]) that the app has
/// read what woke it; dropping it stops the watcher at once.
pub(crate) struct Rearm {
    pipe: OwnedFd,
    /// The watcher woke the app and waits for this rearm: only then does
    /// a read rearm it, so an app reading while the watcher is armed (every
    /// frame pumps the connection) never wakes it.
    parked: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Rearm {
    /// The app has read: the next arrival wakes it again.
    pub(crate) fn rearm(&self) {
        use std::os::fd::AsRawFd as _;
        if !self.parked.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        // A full pipe already holds a rearm the watcher has not taken.
        // SAFETY: one byte from a live buffer to a descriptor this owns.
        let _ = unsafe { libc::write(self.pipe.as_raw_fd(), [1_u8].as_ptr().cast(), 1) };
    }
}

/// Watch `stream` for what the shell writes, calling `wake` once each time
/// it has written something the app has not read yet; the app rearms the
/// watch through the returned [`Rearm`] after reading, and dropping it stops
/// the watcher. `parked` is called each time the watcher, having woken the
/// app, starts waiting for that rearm.
///
/// The watcher sleeps in one `poll` on the socket and the rearm pipe with no
/// timeout: an idle app's watcher is never scheduled.
pub(super) fn spawn_readable_watcher(
    stream: &UnixStream,
    wake: impl Fn() + Send + 'static,
    parked: impl Fn() + Send + 'static,
) -> std::io::Result<Rearm> {
    use std::os::fd::AsRawFd as _;
    // Its own descriptor for the same socket: closing the client's never
    // leaves the watcher polling a number the process reused.
    let watched = stream.try_clone()?;
    let mut ends = [0 as libc::c_int; 2];
    // SAFETY: `ends` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded, so both are fresh descriptors owned here alone.
    let (rearms, rearm) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    let parked_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiting = std::sync::Arc::clone(&parked_flag);
    tilo_rt::spawn_thread("tilo-shell-ui-wake", move || {
        let mut armed = true;
        loop {
            let mut fds = [
                libc::pollfd {
                    // A negative descriptor is skipped: parked, unread bytes
                    // must not wake the watcher again and again.
                    fd: if armed { watched.as_raw_fd() } else { -1 },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: rearms.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two valid pollfds for descriptors this thread owns.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if fds[1].revents != 0 {
                let mut drained = [0_u8; 64];
                loop {
                    // SAFETY: reads into a live buffer of its own length.
                    let read = unsafe {
                        libc::read(
                            rearms.as_raw_fd(),
                            drained.as_mut_ptr().cast(),
                            drained.len(),
                        )
                    };
                    match read {
                        // The client dropped its end: it is gone.
                        0 => return,
                        n if n > 0 => armed = true,
                        _ if std::io::Error::last_os_error().kind()
                            == std::io::ErrorKind::Interrupted => {}
                        _ if std::io::Error::last_os_error().kind()
                            == std::io::ErrorKind::WouldBlock =>
                        {
                            break;
                        }
                        _ => return,
                    }
                }
            }
            if armed && fds[0].revents != 0 {
                // Parked before the app is woken, so the read the wake
                // brings rearms the watcher; a read before it at most rearms
                // it early, for one more wake.
                waiting.store(true, std::sync::atomic::Ordering::Release);
                armed = false;
                wake();
                if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    return;
                }
                parked();
            }
        }
    })?;
    Ok(Rearm {
        pipe: rearm,
        parked: parked_flag,
    })
}
