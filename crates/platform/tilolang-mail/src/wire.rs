//! What an instance's mail origin and the mail connector process say to each other over the
//! socket the session hands both: the connector's requests ([`MailConnector`]) one way, the
//! accounts and what the servers answered the other. Each message is a frame: its length as four
//! big-endian bytes, then its JSON.
//!
//! [`MailConnector`]: crate::connector::MailConnector

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::connector::{AccountRef, Event, Outgoing, Request};

/// The largest frame either side accepts: a fetch of the newest messages of a mailbox, bodies
/// included, fits many times over.
pub const MAX_FRAME: usize = 64 << 20;

/// What the origin asks of the connector. Each request carries the id the origin knows it by;
/// what the server answered names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToConnector {
    Changes {
        request: Request,
        account: String,
        mailbox: String,
    },
    Flag {
        request: Request,
        account: String,
        key: String,
        unread: bool,
        flagged: bool,
    },
    Move {
        request: Request,
        account: String,
        key: String,
        mailbox: String,
    },
    Search {
        request: Request,
        account: String,
        text: String,
    },
    Send {
        request: Request,
        account: String,
        message: Outgoing,
    },
    Watch {
        account: String,
    },
}

/// What the connector tells the origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromConnector {
    /// The settings' mail accounts now: sent when the origin attaches and whenever they change.
    Accounts(Vec<AccountRef>),
    /// What a server answered, or said on its own (new mail).
    Event(Event),
    /// The device came online again: what could not reach its server is tried again.
    Online,
}

/// Write `message` as one frame.
///
/// # Errors
/// The socket's error, or a message that does not encode or is larger than [`MAX_FRAME`].
pub fn write<T: Serialize>(to: &mut impl Write, message: &T) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a mail connector message larger than a frame",
        ));
    }
    let length = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(body.len().saturating_add(4));
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    to.write_all(&frame)?;
    to.flush()
}

/// The next frame, waiting for it; `None` at the end of the stream.
///
/// # Errors
/// The socket's error, a frame larger than [`MAX_FRAME`], or one that does not decode.
pub fn read<T: for<'de> Deserialize<'de>>(from: &mut impl Read) -> io::Result<Option<T>> {
    let mut length = [0_u8; 4];
    match from.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(io::Error::other)?;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a mail connector frame larger than allowed",
        ));
    }
    let mut body = vec![0_u8; length];
    from.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reads_back_as_written_and_the_end_reads_as_none() {
        let sent = ToConnector::Send {
            request: uuid::Uuid::new_v4(),
            account: "sam@rivera.example".into(),
            message: Outgoing {
                to: "ada@analytical.example".into(),
                subject: "Re: Launch plan".into(),
                ..Outgoing::default()
            },
        };
        let mut bytes = Vec::new();
        write(&mut bytes, &sent).unwrap();
        write(
            &mut bytes,
            &ToConnector::Watch {
                account: "sam@rivera.example".into(),
            },
        )
        .unwrap();
        let mut reading = bytes.as_slice();
        assert_eq!(read::<ToConnector>(&mut reading).unwrap(), Some(sent));
        assert!(matches!(
            read::<ToConnector>(&mut reading).unwrap(),
            Some(ToConnector::Watch { .. })
        ));
        assert_eq!(read::<ToConnector>(&mut reading).unwrap(), None);
    }

    #[test]
    fn an_oversized_frame_is_refused_before_it_is_read() {
        let mut bytes = u32::MAX.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"{}");
        let error = read::<FromConnector>(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
