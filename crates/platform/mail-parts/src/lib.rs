//! A mail message whose large parts are kept apart from it.
//!
//! A message's attachments can make it far larger than one piece of content a workspace keeps,
//! and larger than anything worth reading whole to show its words. So each part whose body is
//! large (an attachment, a forwarded message, a big HTML body) is cut out of the message as it
//! was sent, still in its transfer encoding, and kept in pieces of its own ([`split`]). What
//! remains is the message with those bodies empty: its headers, its structure and its small
//! parts, which every MIME reader still reads, each cut part named where its body was.
//! [`join`] puts the bodies back where they were cut, byte for byte, so the message is the one
//! that was sent.

use mail_parser::{MessageParser, PartType};

/// A part whose body is larger than this is kept apart from its message.
pub const APART_BYTES: usize = 256 * 1024;

/// The most bytes one piece of a part kept apart holds: well under what one request and one
/// piece of content carry.
pub const PIECE_BYTES: usize = 8 * 1024 * 1024;

/// One part's body, cut out of its message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Apart<'a> {
    /// Where the body goes back: its offset in the rest of the message.
    pub at: u64,
    /// The size of the part's content once decoded: what its file holds.
    pub size: u64,
    /// The body as it was sent, in its transfer encoding.
    pub body: &'a [u8],
}

impl Apart<'_> {
    /// The body in the pieces it is kept in, in order.
    pub fn pieces(&self) -> std::slice::Chunks<'_, u8> {
        self.body.chunks(PIECE_BYTES)
    }
}

/// A message with its large parts kept apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split<'a> {
    /// The message with the bodies of the parts kept apart left empty.
    pub rest: Vec<u8>,
    /// The bodies cut out of it, in the order they were in.
    pub apart: Vec<Apart<'a>>,
}

/// `message` with the bodies of its large parts kept apart. A message that is no MIME message,
/// has no large part, or would not read the same with its parts apart is kept whole: `rest` is
/// all of it and nothing is apart.
#[must_use]
pub fn split(message: &[u8]) -> Split<'_> {
    let whole = || Split {
        rest: message.to_vec(),
        apart: Vec::new(),
    };
    let Some(parsed) = MessageParser::default().parse(message) else {
        return whole();
    };
    // Each large leaf: its body's start and end in the message, and its decoded size. A nested
    // message is one leaf, its own parts kept with it.
    let mut cut: Vec<(usize, usize, u64)> = parsed
        .parts
        .iter()
        .filter(|part| !matches!(part.body, PartType::Multipart(_)))
        .filter_map(|part| {
            let start = usize::try_from(part.offset_body).ok()?;
            let end = usize::try_from(part.offset_end).ok()?;
            (end > start && end.saturating_sub(start) > APART_BYTES)
                .then(|| (start, end, u64::try_from(part.len()).unwrap_or(u64::MAX)))
        })
        .collect();
    cut.sort_unstable();
    let mut rest = Vec::with_capacity(message.len());
    let mut apart = Vec::with_capacity(cut.len());
    let mut from = 0_usize;
    for (start, end, size) in cut {
        // Overlapping or out of order: not a message these offsets describe.
        let (Some(kept), Some(body)) = (message.get(from..start), message.get(start..end)) else {
            return whole();
        };
        rest.extend_from_slice(kept);
        apart.push(Apart {
            at: u64::try_from(rest.len()).unwrap_or(u64::MAX),
            size,
            body,
        });
        from = end;
    }
    if apart.is_empty() {
        return whole();
    }
    rest.extend_from_slice(message.get(from..).unwrap_or_default());
    let split = Split { rest, apart };
    if reads_the_same(message, &split) {
        split
    } else {
        whole()
    }
}

/// Whether `split` is `message`: put back together it is the same bytes, and its rest reads as
/// the same parts, each one kept apart where it says, empty.
fn reads_the_same(message: &[u8], split: &Split<'_>) -> bool {
    let bodies: Vec<(u64, &[u8])> = split
        .apart
        .iter()
        .map(|part| (part.at, part.body))
        .collect();
    if join(&split.rest, &bodies).as_deref() != Some(message) {
        return false;
    }
    let (Some(whole), Some(rest)) = (
        MessageParser::default().parse(message),
        MessageParser::default().parse(&split.rest),
    ) else {
        return false;
    };
    whole.parts.len() == rest.parts.len()
        && whole.attachments == rest.attachments
        && whole.text_body == rest.text_body
        && whole.html_body == rest.html_body
        && split.apart.iter().all(|part| {
            rest.parts.iter().any(|kept| {
                u64::from(kept.offset_body) == part.at && u64::from(kept.offset_end) == part.at
            })
        })
}

/// The message `rest` was, with each body of `bodies` (where it goes in `rest`, and its bytes)
/// put back. `None` when one would go past the end of `rest` or before the one ahead of it.
#[must_use]
pub fn join(rest: &[u8], bodies: &[(u64, &[u8])]) -> Option<Vec<u8>> {
    let some: Vec<(u64, Option<&[u8]>)> =
        bodies.iter().map(|(at, body)| (*at, Some(*body))).collect();
    join_some(rest, &some).map(|(message, _)| message)
}

/// `rest` with the bodies `bodies` brings (`Some`) put back where they go, and the others left
/// apart: answered with where each of those now goes in the message (`None` for each put back).
/// `None` when one would go past the end of `rest` or before the one ahead of it.
#[must_use]
pub fn join_some(
    rest: &[u8],
    bodies: &[(u64, Option<&[u8]>)],
) -> Option<(Vec<u8>, Vec<Option<u64>>)> {
    let length = bodies.iter().try_fold(rest.len(), |total, (_, body)| {
        total.checked_add(body.map_or(0, <[u8]>::len))
    })?;
    let mut message = Vec::with_capacity(length);
    let mut left = Vec::with_capacity(bodies.len());
    let mut from = 0_usize;
    for (at, body) in bodies {
        let at = usize::try_from(*at).ok()?;
        message.extend_from_slice(rest.get(from..at)?);
        match body {
            Some(body) => {
                message.extend_from_slice(body);
                left.push(None);
            }
            None => left.push(Some(u64::try_from(message.len()).ok()?)),
        }
        from = at;
    }
    message.extend_from_slice(rest.get(from..)?);
    Some((message, left))
}

/// A part's body decoded as it arrives, a piece at a time: what its file holds, without the
/// whole body or the whole file in memory at once.
#[derive(Debug)]
pub struct Decoder {
    encoding: mail_parser::Encoding,
    /// What arrived and is not decoded yet: base64 short of a group of four, quoted-printable
    /// short of its line's end.
    carry: Vec<u8>,
}

impl Decoder {
    /// The decoder of the part whose body goes back into `rest` at `at`, in the transfer
    /// encoding that part names; `None` when no part's body is there.
    #[must_use]
    pub fn of_part(rest: &[u8], at: u64) -> Option<Self> {
        let parsed = MessageParser::default().parse(rest)?;
        let part = parsed
            .parts
            .iter()
            .find(|part| u64::from(part.offset_body) == at)?;
        Some(Self {
            encoding: part.encoding,
            carry: Vec::new(),
        })
    }

    /// The bytes `piece`, the next of the body, decodes to so far. `None` for bytes that are
    /// not in the part's encoding.
    pub fn push(&mut self, piece: &[u8]) -> Option<Vec<u8>> {
        use mail_parser::Encoding;
        match self.encoding {
            Encoding::None => Some(piece.to_vec()),
            Encoding::Base64 => {
                self.carry
                    .extend(piece.iter().filter(|byte| !byte.is_ascii_whitespace()));
                // A group of four characters decodes alone; a group cut by the piece's end
                // waits for the rest of it, as does one after which padding may follow.
                let whole = self.carry.len().saturating_sub(self.carry.len() % 4);
                let ready: Vec<u8> = self.carry.drain(..whole).collect();
                mail_parser::decoders::base64::base64_decode(&ready)
            }
            Encoding::QuotedPrintable => {
                self.carry.extend_from_slice(piece);
                let Some(line_end) = self.carry.iter().rposition(|byte| *byte == b'\n') else {
                    return Some(Vec::new());
                };
                let ready: Vec<u8> = self.carry.drain(..=line_end).collect();
                mail_parser::decoders::quoted_printable::quoted_printable_decode(&ready)
            }
        }
    }

    /// The rest of the body decoded, once its last piece came.
    #[must_use]
    pub fn finish(self) -> Option<Vec<u8>> {
        use mail_parser::Encoding;
        match self.encoding {
            Encoding::None => Some(self.carry),
            Encoding::Base64 => mail_parser::decoders::base64::base64_decode(&self.carry),
            Encoding::QuotedPrintable => {
                mail_parser::decoders::quoted_printable::quoted_printable_decode(&self.carry)
            }
        }
    }
}

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

#[cfg(test)]
mod tests;
