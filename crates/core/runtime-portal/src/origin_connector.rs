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
//! Before any reader, the session hands the connector the connection that is its own: the
//! device's one dispatch, which answers the app's action frames as the app it launched. A
//! connector reaches the workspace's accounts and their sign-ins over that, and over nothing
//! else: it opens no file of them.
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

/// A connection handed to the connector: a reader's, or the connection to the device's
/// dispatch that answers the app's action frames.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Attach {
    #[serde(with = "crate::packet::fd")]
    connection: Descriptor,
    #[serde(default)]
    actions: bool,
}

/// What the session handed the connector.
#[cfg(any(unix, windows))]
#[derive(Debug)]
pub enum Handed {
    /// A reader's connection.
    Reader(tilo_socket::Stream),
    /// The connector's connection to the device's dispatch, which answers the app's action
    /// frames as the app it was launched as.
    Actions(tilo_socket::Stream),
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
    Ok((
        crate::packet::descriptor_of(instance),
        crate::packet::descriptor_of(connector),
    ))
}

/// A connection the session handed, to read and write.
#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a handle handed on Windows may be no pipe"
)]
fn stream(connection: Descriptor) -> io::Result<tilo_socket::Stream> {
    Ok(connection.into())
}

/// A connection the session handed, to read and write: refused when it is no pipe.
#[cfg(windows)]
fn stream(connection: Descriptor) -> io::Result<tilo_socket::Stream> {
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
        connection: crate::process_link::try_clone(connection)?,
        actions: false,
    }
    .send(socket)
}

/// Hand the connector behind `socket` its connection to the device's dispatch, `connection`.
///
/// # Errors
/// The socket's error.
pub fn send_actions(socket: &Descriptor, connection: Descriptor) -> io::Result<()> {
    Attach {
        connection,
        actions: true,
    }
    .send(socket)
}

/// The next connection the session handed the connector on `socket`; `None` at end of file,
/// when the session closed its end.
///
/// # Errors
/// The socket's error; a message that is no attachment with its one descriptor;
/// [`io::ErrorKind::Unsupported`] off Linux and Windows.
#[cfg(any(unix, windows))]
pub fn receive(socket: &Descriptor) -> io::Result<Option<Handed>> {
    let Some(attach) = Attach::receive(socket)? else {
        return Ok(None);
    };
    let connection = stream(attach.connection)?;
    Ok(Some(if attach.actions {
        Handed::Actions(connection)
    } else {
        Handed::Reader(connection)
    }))
}

/// The next reader's connection on `socket`; `None` at end of file, when the session closed its
/// end.
///
/// # Errors
/// The socket's error; a message that is no reader's attachment with its one descriptor;
/// [`io::ErrorKind::Unsupported`] off Linux and Windows.
#[cfg(any(unix, windows))]
pub fn receive_attachment(socket: &Descriptor) -> io::Result<Option<tilo_socket::Stream>> {
    match receive(socket)? {
        Some(Handed::Reader(connection)) => Ok(Some(connection)),
        Some(Handed::Actions(_)) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the session handed the connector a connection, not a reader's",
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
    std::iter::from_fn(move || {
        loop {
            // A connection the session handed the connector for its own use, before any reader's,
            // is no reader's: the readers are the ones that follow.
            match receive(&socket) {
                Ok(Some(Handed::Reader(connection))) => return Some(connection),
                Ok(Some(Handed::Actions(_))) => {}
                Ok(None) => {
                    ended(None);
                    return None;
                }
                Err(error) => {
                    ended(Some(error));
                    return None;
                }
            }
        }
    })
}
