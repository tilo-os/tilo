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

const FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(MAX_FRAME);

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
    Purge {
        request: Request,
        account: String,
        key: String,
    },
    SaveDraft {
        request: Request,
        account: String,
        /// Boxed, as a send's message is: a draft with its files is far larger than the rest.
        draft: Box<Outgoing>,
    },
    Search {
        request: Request,
        account: String,
        text: String,
    },
    Send {
        request: Request,
        account: String,
        /// Boxed: a message with its files is far larger than any other request.
        message: Box<Outgoing>,
    },
    Watch {
        account: String,
    },
    /// The messages older than the oldest one held in a mailbox.
    Older {
        request: Request,
        account: String,
        mailbox: String,
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
    tilolang_tilo::origin::write_json_frame(FRAMING, to, message)
}

/// The next frame, waiting for it; `None` at a clean end of the stream.
///
/// # Errors
/// The socket's error, a truncated or oversized frame (past [`MAX_FRAME`]), or one that does not
/// decode.
pub fn read<T: for<'de> Deserialize<'de>>(from: &mut impl Read) -> io::Result<Option<T>> {
    tilolang_tilo::origin::read_json_frame(FRAMING, from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_wire_fixture<T>(value: &T, expected: &[u8])
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let mut encoded = Vec::new();
        write(&mut encoded, value).unwrap();
        assert_eq!(encoded, expected);

        let mut reading = expected;
        assert_eq!(read::<T>(&mut reading).unwrap().as_ref(), Some(value));
        assert_eq!(read::<T>(&mut reading).unwrap(), None);
    }

    #[test]
    fn a_frame_reads_back_as_written_and_the_end_reads_as_none() {
        let sent = ToConnector::Send {
            request: uuid::Uuid::new_v4(),
            account: "sam@rivera.example".into(),
            message: Box::new(Outgoing {
                to: "ada@analytical.example".into(),
                subject: "Re: Launch plan".into(),
                ..Outgoing::default()
            }),
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

    #[test]
    fn wire_bytes_match_frozen_fixtures() {
        // Literal bytes captured from main before migrating wire.rs to wire-frame.
        const SEND_WIRE: &[u8] = b"\x00\x00\x00\xe5{\"Send\":{\"request\":\"01234567-89ab-4cde-8f01-23456789abcd\",\"account\":\"sam@rivera.example\",\"message\":{\"to\":\"ada@analytical.example\",\"cc\":\"\",\"subject\":\"Re: Launch plan\",\"body\":\"\",\"in_reply_to\":\"\",\"references\":\"\",\"calendar_ics\":\"\"}}}";
        const WATCH_WIRE: &[u8] = b"\x00\x00\x00*{\"Watch\":{\"account\":\"sam@rivera.example\"}}";
        const ACCOUNTS_WIRE: &[u8] = b"\x00\x00\x00P{\"Accounts\":[{\"id\":\"acc-1\",\"address\":\"sam@rivera.example\",\"name\":\"Sam Rivera\"}]}";
        const ONLINE_WIRE: &[u8] = b"\x00\x00\x00\x08\"Online\"";
        const ACCEPTED_WIRE: &[u8] =
            b"\x00\x00\x00={\"Event\":{\"Accepted\":\"01234567-89ab-4cde-8f01-23456789abcd\"}}";

        let id = uuid::Uuid::from_bytes([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x4c, 0xde, 0x8f, 0x01, 0x23, 0x45, 0x67, 0x89,
            0xab, 0xcd,
        ]);
        assert_wire_fixture(
            &ToConnector::Send {
                request: id,
                account: "sam@rivera.example".into(),
                message: Box::new(Outgoing {
                    to: "ada@analytical.example".into(),
                    subject: "Re: Launch plan".into(),
                    ..Outgoing::default()
                }),
            },
            SEND_WIRE,
        );
        assert_wire_fixture(
            &ToConnector::Watch {
                account: "sam@rivera.example".into(),
            },
            WATCH_WIRE,
        );
        assert_wire_fixture(
            &FromConnector::Accounts(vec![AccountRef {
                id: "acc-1".into(),
                address: "sam@rivera.example".into(),
                name: "Sam Rivera".into(),
                identities: Vec::new(),
            }]),
            ACCOUNTS_WIRE,
        );
        assert_wire_fixture(&FromConnector::Online, ONLINE_WIRE);
        assert_wire_fixture(&FromConnector::Event(Event::Accepted(id)), ACCEPTED_WIRE);
    }

    #[test]
    fn write_flushes_and_partial_prefix_or_body_errors() {
        let mut writer = io::BufWriter::new(Vec::new());
        write(&mut writer, &FromConnector::Online).unwrap();
        assert!(writer.buffer().is_empty(), "write must flush the writer");

        let mut empty = &b""[..];
        assert_eq!(read::<FromConnector>(&mut empty).unwrap(), None);

        for partial in [&[0_u8][..], &[0, 0][..], &[0, 0, 0][..]] {
            let mut slice = partial;
            let error = read::<FromConnector>(&mut slice).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }

        let mut truncated_body = &b"\x00\x00\x00\x08\"On"[..];
        let error = read::<FromConnector>(&mut truncated_body).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

        let mut invalid_json = &b"\x00\x00\x00\x02{}"[..];
        let error = read::<FromConnector>(&mut invalid_json).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
