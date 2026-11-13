//! The window door: where the shell opens another window of an app that is
//! already running.
//!
//! Also the one switch a launch hands a window it opens: [`VISITOR_WINDOW_FLAG`].
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

pub const VISITOR_WINDOW_FLAG: &str = "--visitor";

use std::io;

pub const SERVICE: &str = "windows";

pub const CHILD_FD: i32 = 36;

pub const OPEN_VERB: &[u8] = b"open";

/// The verb an app says on its door once it reads it: it opens the windows
/// the shell asks for. An app built before the door never says it, and the
/// shell starts a process of its own for each of its windows, as before:
/// the shell and the apps are updated apart.
pub const SERVES_VERB: &[u8] = b"serves";

pub const PATCH_VERB: &[u8] = b"patch";

pub const MAX_MESSAGE: usize = 4096;

#[derive(Debug)]
pub enum Message {
    Open(Vec<u8>, Vec<Descriptor>),
    Patch(u64, Descriptor),
}

#[cfg(unix)]
pub type Descriptor = std::os::fd::OwnedFd;
/// There are no window doors where there are no descriptors. Nothing can
/// own a value of this type, so copying one owns nothing either.
#[cfg(not(unix))]
#[derive(Clone, Copy, Debug)]
pub enum Descriptor {}

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
        crate::door::send(door.as_fd(), &[OPEN_VERB, request], &fds)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, request, fds);
        Err(unsupported())
    }
}

pub fn send_serves(door: &Descriptor) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        crate::door::send(door.as_fd(), &[SERVES_VERB], &[])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

pub fn take_serves(door: &Descriptor) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        use rustix::net::{RecvAncillaryBuffer, RecvFlags, recvmsg};
        let mut serves = false;
        loop {
            let mut buffer = [0_u8; 64];
            let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
            let mut control = RecvAncillaryBuffer::new(&mut space);
            match recvmsg(
                door,
                &mut [io::IoSliceMut::new(&mut buffer)],
                &mut control,
                RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
            ) {
                Ok(received) if received.bytes == 0 => return Ok(serves),
                Ok(received) => {
                    serves |= buffer.get(..received.bytes) == Some(SERVES_VERB);
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(serves),
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

pub fn has_message(door: &Descriptor) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let mut buffer = [0_u8; 1];
        match rustix::net::recv(
            door,
            &mut buffer,
            rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::DONTWAIT,
        ) {
            Ok((_, length)) => Ok(length > 0),
            Err(rustix::io::Errno::AGAIN) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(unsupported())
    }
}

pub fn send_patch(door: &Descriptor, instance: u64, patch: &Descriptor) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let instance = instance.to_string();
        crate::door::send(
            door.as_fd(),
            &[PATCH_VERB, instance.as_bytes()],
            &[patch.as_fd()],
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, instance, patch);
        Err(unsupported())
    }
}

pub fn receive_message(door: &Descriptor) -> io::Result<Option<Message>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let mut buffer = vec![0_u8; MAX_MESSAGE];
        let Some((length, fds)) = crate::door::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        match crate::door::decode(message).as_slice() {
            [verb, request] if *verb == OPEN_VERB => Ok(Some(Message::Open(request.to_vec(), fds))),
            [verb, instance] if *verb == PATCH_VERB => {
                let instance = std::str::from_utf8(instance)
                    .ok()
                    .and_then(|instance| instance.parse().ok())
                    .ok_or_else(|| invalid("a patch connection names no window"))?;
                let [patch] = <[Descriptor; 1]>::try_from(fds)
                    .map_err(|_| invalid("a patch connection without its descriptor"))?;
                Ok(Some(Message::Patch(instance, patch)))
            }
            _ => Err(invalid(
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

pub fn receive_open(door: &Descriptor) -> io::Result<Option<(Vec<u8>, Vec<Descriptor>)>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let mut buffer = vec![0_u8; MAX_MESSAGE];
        let Some((length, fds)) = crate::door::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        match crate::door::decode(message).as_slice() {
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
        ours.write_all(b"x").unwrap();
        let mut received = std::os::unix::net::UnixStream::from(fds.into_iter().next().unwrap());
        let mut byte = [0_u8; 1];
        std::io::Read::read_exact(&mut received, &mut byte).unwrap();
        assert_eq!(&byte, b"x");
        drop(shell);
        assert!(receive_open(&app).unwrap().is_none(), "closed door ends");
    }

    #[test]
    fn an_app_that_reads_its_door_says_so_and_an_older_one_never_does() {
        let (shell, app) = pair().unwrap();
        assert!(
            !take_serves(&shell).unwrap(),
            "an app built before the door says nothing"
        );
        send_serves(&app).unwrap();
        assert!(take_serves(&shell).unwrap());
        assert!(!take_serves(&shell).unwrap(), "said once");
    }

    #[test]
    fn anything_but_a_request_is_refused() {
        use std::os::fd::AsFd as _;
        let (shell, app) = pair().unwrap();
        crate::door::send(shell.as_fd(), &[b"job", b"label"], &[]).unwrap();
        assert!(receive_open(&app).is_err());
    }
}
