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
