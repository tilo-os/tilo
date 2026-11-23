//! What an instance's calendar origin and the calendar connector process say to each other over
//! the socket the session hands both: the connector's requests ([`CalDavConnector`]) one way,
//! the accounts and what the servers answered the other. Each message is a frame: its length as
//! four big-endian bytes, then its JSON.
//!
//! [`CalDavConnector`]: crate::connector::CalDavConnector

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::connector::{AccountRef, CalendarKnown, Entry, Event, Request};

/// The largest frame either side accepts: a sync of a calendar whose entries each hold their
/// text fits many times over.
pub const MAX_FRAME: usize = 64 << 20;

/// What the origin asks of the connector. Each request carries the id the origin knows it by;
/// what the server answered names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToConnector {
    Sync {
        request: Request,
        account: String,
        /// Where each calendar's last pull ended, as the workspace kept it.
        known: Vec<CalendarKnown>,
    },
    Put {
        request: Request,
        account: String,
        entry: Box<Entry>,
        /// Where its server keeps it now, empty for a new entry.
        href: String,
        /// The entry to take out first, where it moves from another calendar.
        from: String,
    },
    Remove {
        request: Request,
        account: String,
        calendar: String,
        href: String,
    },
    Answer {
        request: Request,
        account: String,
        calendar: String,
        href: String,
        attendee: String,
        answer: String,
    },
    Import {
        request: Request,
        account: String,
        calendar: String,
        text: String,
    },
    Calendar {
        request: Request,
        account: String,
        calendar: String,
        name: String,
        color: String,
    },
}

/// What the connector tells the origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromConnector {
    /// The accounts' settings now: sent when the origin attaches and whenever they change.
    Accounts(Vec<AccountRef>),
    /// What a server answered, or said on its own.
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
            "a calendar connector message larger than a frame",
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
            "a calendar connector frame larger than allowed",
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
        let sent = ToConnector::Sync {
            request: uuid::Uuid::new_v4(),
            account: "caldav:ada@calendar.example".into(),
            known: Vec::new(),
        };
        let mut bytes = Vec::new();
        write(&mut bytes, &sent).unwrap();
        write(
            &mut bytes,
            &ToConnector::Remove {
                request: uuid::Uuid::new_v4(),
                account: "caldav:ada@calendar.example".into(),
                calendar: "https://calendar.example/dav/calendars/ada/work/".into(),
                href: "https://calendar.example/dav/calendars/ada/work/a.ics".into(),
            },
        )
        .unwrap();
        let mut reading = bytes.as_slice();
        assert_eq!(read::<ToConnector>(&mut reading).unwrap(), Some(sent));
        assert!(matches!(
            read::<ToConnector>(&mut reading).unwrap(),
            Some(ToConnector::Remove { .. })
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
