//! Settings asking the host for the person's languages.
//!
//! Settings may ask which language packs the device's repositories offer
//! and to install one of them, which is all the host answers it
//! ([`Request::LanguagePacks`], [`Request::InstallLanguagePack`]). The pack,
//! its signature and where it lands are the host's decision; Settings
//! supplies no URL, key or path. The launcher stamped the connection with
//! the calling app's id before the app held it, and the host answers no
//! other app.
//!
//! An app's runtimes are never asked for here: they install and update with
//! the app, in the same step or not at all.
//!
//! Wire format: a big-endian `u32` length, then JSON, per message; one
//! [`Response`] per [`Request`], on a connection the app keeps.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// The longest message either side accepts.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// What the app asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Which language packs the repositories offer, by locale, and whether
    /// each is installed. Settings only.
    LanguagePacks,
    /// Make the language pack of `locale` (`de-DE`) installed, starting the
    /// install when none runs, and report where it stands. Asking again is
    /// how Settings follows the install; it never starts a second one.
    /// Settings only.
    InstallLanguagePack { locale: String },
}

/// A language the repositories offer a pack for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedLanguage {
    /// The locale, a BCP 47 tag (`de-DE`).
    pub locale: String,
    pub installed: bool,
}

/// Where an install is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Choosing the repository and the build to fetch.
    Preparing,
    /// Fetching and checking the signed package.
    Downloading,
    /// Putting the verified pack in place.
    Installing,
}

/// The host's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Response {
    /// The pack is installed.
    Installed,
    /// An install is running.
    Installing {
        phase: Phase,
        received: u64,
        /// Bytes expected, once known.
        total: Option<u64>,
    },
    /// The language packs the repositories offer.
    LanguagePacks { offered: Vec<OfferedLanguage> },
    /// The install stopped; asking again retries it.
    Failed { message: String },
    /// The host will not answer this app or this request.
    Refused { message: String },
}

/// Every message is a big-endian length prefix and a JSON body; none is empty.
const FRAMING: wire_frame::Framing =
    wire_frame::Framing::big_endian(MAX_MESSAGE_BYTES).reject_empty();

pub fn write_message<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    FRAMING.write(writer, &bytes).map_err(framing)?;
    writer.flush()
}

/// Read one message; `None` when the peer closed before it began.
pub fn read_message<R: Read, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> io::Result<Option<T>> {
    let Some(bytes) = FRAMING.read(reader).map_err(framing)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// A frame too long or cut short reads as the malformed message it is.
fn framing(error: wire_frame::FrameError) -> io::Error {
    let kind = match error {
        wire_frame::FrameError::Io(io) => return io,
        wire_frame::FrameError::Truncated => io::ErrorKind::UnexpectedEof,
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, error)
}

/// The app side: one connection to the host's `app-host` service. The type
/// exists on every target so callers need no target selection; where the
/// launcher hands no service sockets (off unix), `from_service` finds none.
pub struct Client(Connection);

#[cfg(unix)]
type Connection = std::os::unix::net::UnixStream;

/// No transport carries the service off unix, so no client is ever made there.
#[cfg(not(unix))]
enum Connection {}

impl Client {
    /// Claim the connection the launcher handed this app, if it did.
    #[must_use]
    pub fn from_service() -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::FromRawFd as _;
            let fd = crate::service_fd(crate::runtime_request::SERVICE)?;
            // SAFETY: `service_fd` verified the descriptor is open, and the
            // launcher gave it to this process alone; the client owns it.
            Some(Self(unsafe {
                std::os::unix::net::UnixStream::from_raw_fd(fd)
            }))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// A client on an explicit connection (a test host).
    #[cfg(unix)]
    #[must_use]
    pub fn from_stream(stream: std::os::unix::net::UnixStream) -> Self {
        Self(stream)
    }

    /// Ask the host which language packs the repositories offer.
    ///
    /// # Errors
    ///
    /// Returns a transport error, or the host hanging up before it answered.
    pub fn language_packs(&mut self) -> io::Result<Response> {
        self.exchange(&Request::LanguagePacks)
    }

    /// Ask the host to install the language pack of `locale`; asking again
    /// follows the install.
    ///
    /// # Errors
    ///
    /// As [`Self::language_packs`].
    pub fn install_language_pack(&mut self, locale: &str) -> io::Result<Response> {
        self.exchange(&Request::InstallLanguagePack {
            locale: locale.to_owned(),
        })
    }

    #[cfg(unix)]
    fn exchange(&mut self, request: &Request) -> io::Result<Response> {
        write_message(&mut self.0, request)?;
        read_message(&mut self.0)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the host closed the connection",
            )
        })
    }

    #[cfg(not(unix))]
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "shares the signature of the Unix exchange"
    )]
    fn exchange(&mut self, _request: &Request) -> io::Result<Response> {
        match self.0 {}
    }
}

pub const SERVICE: &str = "app-host";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_and_bound_their_length() {
        let mut wire = Vec::new();
        let sent = Response::Installing {
            phase: Phase::Downloading,
            received: 5,
            total: Some(9),
        };
        write_message(&mut wire, &sent).unwrap();
        let got: Response = read_message(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(got, sent);
        let none: Option<Response> = read_message(&mut [].as_slice()).unwrap();
        assert!(none.is_none());
        let huge = u32::try_from(MAX_MESSAGE_BYTES + 1).unwrap().to_be_bytes();
        assert!(read_message::<_, Response>(&mut huge.as_slice()).is_err());
    }
}
