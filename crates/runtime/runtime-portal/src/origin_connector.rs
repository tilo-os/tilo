//! The socket of an app's origin connector: the process the session's launcher starts for a
//! tilolang app whose types hold an outside service's resources (a mail server), in the app's
//! sandbox with the network and ownership its manifest declares for that origin. It runs whether
//! or not a window of the app is open.
//!
//! Each instance of the app a tilolang process runs reaches the connector over a connection of its
//! own: the session makes a pair, hands one end to the instance with the instance's other
//! connections (`crate::process_link::InstanceFds::origin`) and the other to the connector on
//! this socket, as one packet (`crate::packet`) carrying that one descriptor.
//!
//! Before any reader, the session hands the connector of an app whose signed manifest grants them
//! its account sign-ins on the same socket: a connection that speaks `accounts::sign_ins`'s lines,
//! scoped to the connector's workspace and app. A connector reaches the workspace's accounts and
//! their sign-ins over it, and over nothing else: it opens no file of them.

use std::io;

pub use crate::packet::Descriptor;
use crate::packet::Packet as _;

pub const SERVICE: &str = "tilolang-origin";

pub const CHILD_FD: i32 = 37;

/// The variable the launcher names the origin the process connects with (`imap`). Set, the
/// tilolang runtime runs that origin's connector instead of a window.
pub const ORIGIN_ENV: &str = "TILO_TILOLANG_CONNECTOR";

/// A connection handed to the connector: a reader's, or the connector's account sign-ins.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Attach {
    #[serde(with = "crate::packet::fd")]
    connection: Descriptor,
    #[serde(default)]
    sign_ins: bool,
}

/// What the session handed the connector.
#[derive(Debug)]
pub enum Handed {
    /// A reader's connection.
    Reader(Descriptor),
    /// The connector's account sign-ins.
    SignIns(Descriptor),
}

impl crate::packet::Packet for Attach {}

/// A connected pair of socket ends: the session keeps one and hands the other to the connector
/// it starts.
///
/// # Errors
/// The socket error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    crate::packet::pair()
}

#[cfg(unix)]
pub fn connection() -> io::Result<(Descriptor, Descriptor)> {
    let (instance, connector) = std::os::unix::net::UnixStream::pair()?;
    Ok((instance.into(), connector.into()))
}

#[must_use]
pub fn take_socket() -> Option<Descriptor> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd = crate::service_fd(SERVICE)?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the launcher handed this descriptor to the process for the socket alone, and
        // the flag above lets it be owned exactly once.
        Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Hand the connector behind `socket` an instance's connection `connection`.
///
/// # Errors
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_attachment(socket: &Descriptor, connection: &Descriptor) -> io::Result<()> {
    #[cfg(unix)]
    {
        Attach {
            connection: connection.try_clone()?,
            sign_ins: false,
        }
        .send(socket)
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, connection);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only the Linux session runs origin connectors",
        ))
    }
}

/// Hand the connector behind `socket` its account sign-ins, `connection`.
///
/// # Errors
/// The socket's error, or [`io::ErrorKind::Unsupported`] off Linux.
pub fn send_sign_ins(socket: &Descriptor, connection: Descriptor) -> io::Result<()> {
    #[cfg(unix)]
    {
        Attach {
            connection,
            sign_ins: true,
        }
        .send(socket)
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, connection);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "only the Linux session runs origin connectors",
        ))
    }
}

/// The next connection the session handed the connector on `socket`; `None` at end of file,
/// when the session closed its end.
///
/// # Errors
/// The socket's error; a message that is no attachment with its one descriptor;
/// [`io::ErrorKind::Unsupported`] off Linux.
pub fn receive(socket: &Descriptor) -> io::Result<Option<Handed>> {
    Ok(Attach::receive(socket)?.map(|attach| {
        if attach.sign_ins {
            Handed::SignIns(attach.connection)
        } else {
            Handed::Reader(attach.connection)
        }
    }))
}

/// The next reader's connection on `socket`; `None` at end of file, when the session closed its
/// end.
///
/// # Errors
/// The socket's error; a message that is no reader's attachment with its one descriptor, such as
/// account sign-ins the app is not granted; [`io::ErrorKind::Unsupported`] off Linux.
pub fn receive_attachment(socket: &Descriptor) -> io::Result<Option<Descriptor>> {
    match receive(socket)? {
        Some(Handed::Reader(connection)) => Ok(Some(connection)),
        Some(Handed::SignIns(_)) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the session handed account sign-ins to a connector not granted them",
        )),
        None => Ok(None),
    }
}

/// The readers' connections the session hands the connector on `socket`, until it closes its end
/// or the socket fails: `ended` is told which, with the failure.
#[cfg(unix)]
pub fn attachments(
    socket: Descriptor,
    mut ended: impl FnMut(Option<io::Error>) + Send + 'static,
) -> impl Iterator<Item = std::os::unix::net::UnixStream> + Send + 'static {
    std::iter::from_fn(move || match receive_attachment(&socket) {
        Ok(Some(connection)) => Some(connection.into()),
        next => {
            ended(next.err());
            None
        }
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn an_attachment_reaches_the_connector_with_its_connection() {
        use std::io::{Read as _, Write as _};
        let (session, connector) = pair().unwrap();
        let (instance, theirs) = connection().unwrap();
        send_attachment(&session, &theirs).unwrap();
        let received = receive_attachment(&connector)
            .unwrap()
            .expect("an attachment");
        let mut instance = std::os::unix::net::UnixStream::from(instance);
        let mut received = std::os::unix::net::UnixStream::from(received);
        instance.write_all(b"hello").unwrap();
        let mut read = [0_u8; 5];
        received.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hello");
        let (_kept, sign_ins) = connection().unwrap();
        send_sign_ins(&session, sign_ins).unwrap();
        let refused = receive_attachment(&connector).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
        drop(session);
        assert!(receive_attachment(&connector).unwrap().is_none());
    }
}
