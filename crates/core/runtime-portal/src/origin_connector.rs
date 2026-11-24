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
//!
//! On Windows the socket is the connector's process link (`crate::process_link::take`), which
//! carries the connections the session duplicates into the connector it started; each connection
//! is a pipe (`tilo_socket::pair`), read and written as a Unix one is.

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
/// The socket error, or [`io::ErrorKind::Unsupported`] off Unix and Windows.
pub fn pair() -> io::Result<(Descriptor, Descriptor)> {
    crate::packet::pair()
}

/// A connection between an instance and the connector: the session hands one end to each.
///
/// # Errors
/// The system's error making the pair.
#[cfg(any(unix, windows))]
pub fn connection() -> io::Result<(Descriptor, Descriptor)> {
    let (instance, connector) = tilo_socket::pair()?;
    Ok((descriptor(instance), descriptor(connector)))
}

#[cfg(unix)]
fn descriptor(stream: tilo_socket::Stream) -> Descriptor {
    stream.into()
}

#[cfg(windows)]
fn descriptor(stream: tilo_socket::Stream) -> Descriptor {
    Descriptor::new(stream)
}

/// A connection the session handed, to read and write.
#[cfg(unix)]
#[must_use]
pub fn stream(connection: Descriptor) -> tilo_socket::Stream {
    connection.into()
}

/// A connection the session handed, to read and write.
#[cfg(windows)]
#[must_use]
pub fn stream(connection: Descriptor) -> tilo_socket::Stream {
    connection.into_stream()
}

/// The socket the launcher handed this connector, taken once: on Linux the service descriptor it
/// names, on macOS and Windows the process link.
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
    #[cfg(any(target_os = "macos", windows))]
    {
        crate::process_link::take()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    None
}

/// Hand the connector behind `socket` an instance's connection `connection`.
///
/// # Errors
/// The socket's error.
pub fn send_attachment(socket: &Descriptor, connection: &Descriptor) -> io::Result<()> {
    Attach {
        connection: connection.try_clone()?,
        sign_ins: false,
    }
    .send(socket)
}

/// Hand the connector behind `socket` its account sign-ins, `connection`.
///
/// # Errors
/// The socket's error.
pub fn send_sign_ins(socket: &Descriptor, connection: Descriptor) -> io::Result<()> {
    Attach {
        connection,
        sign_ins: true,
    }
    .send(socket)
}

/// The next connection the session handed the connector on `socket`; `None` at end of file,
/// when the session closed its end.
///
/// # Errors
/// The socket's error; a message that is no attachment with its one descriptor;
/// [`io::ErrorKind::Unsupported`] off Linux and Windows.
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
/// account sign-ins the app is not granted; [`io::ErrorKind::Unsupported`] off Linux and Windows.
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
#[cfg(any(unix, windows))]
pub fn attachments(
    socket: Descriptor,
    mut ended: impl FnMut(Option<io::Error>) + Send + 'static,
) -> impl Iterator<Item = tilo_socket::Stream> + Send + 'static {
    std::iter::from_fn(move || match receive_attachment(&socket) {
        Ok(Some(connection)) => Some(stream(connection)),
        next => {
            ended(next.err());
            None
        }
    })
}
