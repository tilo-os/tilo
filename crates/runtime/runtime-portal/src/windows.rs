//! The window door: where the shell opens another window of an app that is
//! already running.
//!
//! One process serves every window of an app in a room. The shell hands each
//! app it starts one end of a socket pair, through the launcher, as
//! `TILO_SVC_WINDOWS_FD`, and keeps the other. When the person opens another
//! window of the app in that room, the shell does not start a second
//! process: it writes the window's request on its end, with the window's own
//! connections to the session attached, and the app opens the window itself.
//!
//! A message is one `SOCK_SEQPACKET` packet in the framing of `jobs`: the
//! verb [`OPEN_VERB`] and the request (the toolkit's
//! `ui::window_door::WindowRequest`, JSON), with the descriptors the request
//! names. Only the Linux shell opens windows this way; elsewhere there is no
//! door and every call says so.

use std::io;

/// The service name of the door: the app reads its descriptor from
/// `TILO_SVC_WINDOWS_FD`.
pub const SERVICE: &str = "windows";

/// In-sandbox descriptor slot of the door.
pub const CHILD_FD: i32 = 36;

/// The verb of a request to open a window.
pub const OPEN_VERB: &[u8] = b"open";

/// The largest request: a few numbers and flags.
pub const MAX_MESSAGE: usize = 4096;

/// A descriptor the door carries, owned: the door itself, or a connection a
/// window request hands over.
#[cfg(unix)]
pub type Descriptor = std::os::fd::OwnedFd;
/// There are no window doors where there are no descriptors.
#[cfg(not(unix))]
pub enum Descriptor {}

/// The raw descriptor `descriptor` owned, for a client that takes ownership
/// of a raw one.
#[must_use]
pub fn into_raw(descriptor: Descriptor) -> crate::RawFd {
    #[cfg(unix)]
    {
        std::os::fd::IntoRawFd::into_raw_fd(descriptor)
    }
    #[cfg(not(unix))]
    match descriptor {}
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "only the Linux shell opens windows in a running app",
    )
}

/// The door the launcher handed this process, once: `None` outside the
/// shell, for a process that was handed none, and on every later call.
#[must_use]
pub fn take_door() -> Option<Descriptor> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd = crate::service_fd(SERVICE)?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the launcher handed this descriptor to the process for the
        // door alone, and the flag above lets it be owned exactly once.
        Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// A connected pair of door ends: the shell keeps one and hands the other
/// to the launch.
///
/// # Errors
///
/// The socket error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    #[cfg(target_os = "linux")]
    {
        use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
        Ok(socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?)
    }
    #[cfg(not(target_os = "linux"))]
    Err(unsupported())
}

/// Send the request `request` (encoded) on `door` with `fds`.
///
/// # Errors
///
/// The socket's error, a request that contains a NUL byte, or
/// [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_open(door: &Descriptor, request: &[u8], fds: &[&Descriptor]) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let fds: Vec<_> = fds.iter().map(|fd| fd.as_fd()).collect();
        crate::jobs::send(door.as_fd(), &[OPEN_VERB, request], &fds)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, request, fds);
        Err(unsupported())
    }
}

/// The next request on `door` and the descriptors it came with; `None` at end
/// of file, when the shell closed its end.
///
/// # Errors
///
/// The socket's error, or a message that is not a request to open a window.
pub fn receive_open(door: &Descriptor) -> io::Result<Option<(Vec<u8>, Vec<Descriptor>)>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let mut buffer = vec![0_u8; MAX_MESSAGE];
        let Some((length, fds)) = crate::jobs::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        match crate::jobs::decode(message).as_slice() {
            [verb, request] if *verb == OPEN_VERB => Ok(Some((request.to_vec(), fds))),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the window door carried something other than a window request",
            )),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn a_request_crosses_the_door_with_its_descriptors() {
        let (shell, app) = pair().unwrap();
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let theirs = Descriptor::from(theirs);
        send_open(&shell, br#"{"instance":7}"#, &[&theirs]).unwrap();
        drop(theirs);
        let (request, fds) = receive_open(&app).unwrap().unwrap();
        assert_eq!(request, br#"{"instance":7}"#);
        assert_eq!(fds.len(), 1, "the window's connection came with it");
        // The descriptor is the far end of `ours`.
        ours.write_all(b"x").unwrap();
        let mut received = std::os::unix::net::UnixStream::from(fds.into_iter().next().unwrap());
        let mut byte = [0_u8; 1];
        std::io::Read::read_exact(&mut received, &mut byte).unwrap();
        assert_eq!(&byte, b"x");
        drop(shell);
        assert!(receive_open(&app).unwrap().is_none(), "closed door ends");
    }

    #[test]
    fn anything_but_a_request_is_refused() {
        use std::os::fd::AsFd as _;
        let (shell, app) = pair().unwrap();
        crate::jobs::send(shell.as_fd(), &[b"job", b"label"], &[]).unwrap();
        assert!(receive_open(&app).is_err());
    }
}
