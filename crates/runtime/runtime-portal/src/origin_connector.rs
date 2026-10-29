//! The door of an app's origin connector: the process the session's launcher starts for a
//! tilolang app whose types hold an outside service's resources (a mail server), in the app's
//! sandbox with the network and custody its manifest declares for that origin. It runs whether
//! or not a window of the app is open.
//!
//! Each instance of the app a tilolang host runs reaches the connector over a connection of its
//! own: the session makes a pair, hands one end to the instance with the instance's other
//! connections (`crate::tilolang_host::InstanceFds::origin`) and the other to the connector on
//! this door, as one `SOCK_SEQPACKET` packet in the framing of `jobs`: the verb
//! [`ATTACH_VERB`] with the one descriptor.

use std::io;

pub use crate::windows::Descriptor;

/// The name the launcher hands the door under (`crate::service_fd`).
pub const SERVICE: &str = "tilolang-origin";

/// Where the door is in the connector's process.
pub const CHILD_FD: i32 = 37;

/// The variable the launcher names the origin the process connects with (`imap`). Set, the
/// tilolang runtime runs that origin's connector instead of a window.
pub const ORIGIN_ENV: &str = "TILO_TILOLANG_CONNECTOR";

/// The verb of an instance's connection.
pub const ATTACH_VERB: &[u8] = b"attach";

/// A connected pair of door ends: the session keeps one and hands the other to the connector
/// it starts.
///
/// # Errors
/// The socket error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    crate::windows::pair()
}

/// A connection between an instance and the connector: a stream, the instance's end and the
/// connector's.
///
/// # Errors
/// The socket error.
#[cfg(unix)]
pub fn connection() -> io::Result<(Descriptor, Descriptor)> {
    let (instance, connector) = std::os::unix::net::UnixStream::pair()?;
    Ok((instance.into(), connector.into()))
}

/// The door the launcher handed this process, once.
#[must_use]
pub fn take_door() -> Option<Descriptor> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd = crate::service_fd(SERVICE)?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the launcher handed this descriptor to the process for the door alone, and
        // the flag above lets it be owned exactly once.
        Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Hand the connector behind `door` an instance's connection `connection`.
///
/// # Errors
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_attachment(door: &Descriptor, connection: &Descriptor) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        crate::jobs::send(door.as_fd(), &[ATTACH_VERB], &[connection.as_fd()])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (door, connection);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only the Linux shell runs origin connectors",
        ))
    }
}

/// The next instance's connection on `door`; `None` at end of file, when the session closed
/// its end.
///
/// # Errors
/// The socket's error; a message that is no attachment with its one descriptor;
/// [`io::ErrorKind::Unsupported`] off Linux.
pub fn receive_attachment(door: &Descriptor) -> io::Result<Option<Descriptor>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsFd as _;
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let mut buffer = vec![0_u8; 64];
        let Some((length, fds)) = crate::jobs::receive(door.as_fd(), &mut buffer)? else {
            return Ok(None);
        };
        let message = buffer.get(..length).unwrap_or_default();
        if crate::jobs::decode(message).as_slice() != [ATTACH_VERB] {
            return Err(invalid(
                "the connector's door carried something other than an attachment",
            ));
        }
        let [connection] = <[Descriptor; 1]>::try_from(fds)
            .map_err(|_| invalid("an attachment without its connection"))?;
        Ok(Some(connection))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = door;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only the Linux shell runs origin connectors",
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn an_attachment_reaches_the_connector_with_its_connection() {
        let (session, connector) = pair().unwrap();
        let (instance, theirs) = connection().unwrap();
        send_attachment(&session, &theirs).unwrap();
        let received = receive_attachment(&connector)
            .unwrap()
            .expect("an attachment");
        // What the instance writes, the connector reads.
        use std::io::{Read as _, Write as _};
        let mut instance = std::os::unix::net::UnixStream::from(instance);
        let mut received = std::os::unix::net::UnixStream::from(received);
        instance.write_all(b"hello").unwrap();
        let mut read = [0_u8; 5];
        received.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hello");
        drop(session);
        assert!(receive_attachment(&connector).unwrap().is_none());
    }
}
