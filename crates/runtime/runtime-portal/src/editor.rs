//! Bounded native editor frames carried by an already admitted session FD.
//!
//! These values declare an edit, never authority. The session supervisor must
//! bind the executing producer to its original launch; Terminal's existing
//! resource/actor checks still admit each invocation. No ambient socket, PTY
//! input, clipboard or durable text store is provided here.

use serde::{Deserialize, Serialize};
use std::io;

pub const VERSION: u8 = 1;
pub const HEADER_BYTES: usize = 80;
pub const TEXT_BYTES: usize = 4096;
pub const FRAME_BYTES: usize = HEADER_BYTES + TEXT_BYTES;
const MAGIC: &[u8; 4] = b"TBE1";

#[cfg(target_os = "linux")]
mod socket;
#[cfg(target_os = "linux")]
pub use socket::Socket;

/// Original producer field and selection. Deserializing this grants nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub incarnation: [u8; 16],
    pub generation: u64,
    pub revision: u64,
    pub expires_ns: u64,
    pub anchor: u32,
    pub focus: u32,
}

impl Lease {
    fn valid(&self) -> bool {
        self.incarnation != [0; 16]
            && self.generation > 0
            && self.generation < u64::MAX
            && self.revision > 0
            && self.revision < u64::MAX
            && self.expires_ns > 0
            && usize::try_from(self.anchor).is_ok_and(|value| value <= TEXT_BYTES)
            && usize::try_from(self.focus).is_ok_and(|value| value <= TEXT_BYTES)
    }
}

/// Request IDs correlate delivery; transaction IDs correlate the original edit.
/// A query is not permission to retry a lost edit or redirect it to a new field.
pub enum Request {
    Inspect,
    Replace {
        lease: Lease,
        transaction: u64,
        text: String,
    },
    Undo {
        lease: Lease,
        transaction: u64,
    },
    Redo {
        lease: Lease,
        transaction: u64,
    },
    Query {
        lease: Lease,
        transaction: u64,
    },
    Revoke,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Unavailable = 1,
    Ready = 2,
    Applied = 3,
    Undone = 4,
    Redone = 5,
    Stale = 6,
    Invalid = 7,
    Unknown = 8,
    Revoked = 9,
}

/// Private observation: intentionally not Debug or Serialize. Publish only
/// explicit lease/availability metadata through the resource action door.
pub struct Reply {
    pub request: u64,
    pub status: Status,
    pub lease: Option<Lease>,
    pub transaction: u64,
    pub can_undo: bool,
    pub can_redo: bool,
    pub text: String,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid native editor frame")
}

fn write(frame: &mut [u8], offset: usize, bytes: &[u8]) -> io::Result<()> {
    let end = offset.checked_add(bytes.len()).ok_or_else(invalid)?;
    frame
        .get_mut(offset..end)
        .ok_or_else(invalid)?
        .copy_from_slice(bytes);
    Ok(())
}

fn read<const N: usize>(frame: &[u8], offset: usize) -> io::Result<[u8; N]> {
    let end = offset.checked_add(N).ok_or_else(invalid)?;
    frame
        .get(offset..end)
        .ok_or_else(invalid)?
        .try_into()
        .map_err(|_| invalid())
}

fn put_lease(frame: &mut [u8], lease: &Lease) -> io::Result<()> {
    if !lease.valid() {
        return Err(invalid());
    }
    write(frame, 16, &lease.incarnation)?;
    write(frame, 32, &lease.generation.to_le_bytes())?;
    write(frame, 40, &lease.revision.to_le_bytes())?;
    write(frame, 48, &lease.expires_ns.to_le_bytes())?;
    write(frame, 56, &lease.anchor.to_le_bytes())?;
    write(frame, 60, &lease.focus.to_le_bytes())
}

/// Encode one bounded request. Replacement is literal text and cannot press Enter.
/// # Errors
/// Rejects invalid identities, zero request/transaction IDs and control payloads.
pub fn encode(request: u64, operation: &Request) -> io::Result<Vec<u8>> {
    if request == 0 {
        return Err(invalid());
    }
    let (op, lease, transaction, text) = match operation {
        Request::Inspect => (1, None, 0, ""),
        Request::Replace {
            lease,
            transaction,
            text,
        } => (2, Some(lease), *transaction, text.as_str()),
        Request::Undo { lease, transaction } => (3, Some(lease), *transaction, ""),
        Request::Redo { lease, transaction } => (4, Some(lease), *transaction, ""),
        Request::Query { lease, transaction } => (5, Some(lease), *transaction, ""),
        Request::Revoke => (6, None, 0, ""),
    };
    if text.len() > TEXT_BYTES
        || text.chars().any(char::is_control)
        || (lease.is_some() && transaction == 0)
    {
        return Err(invalid());
    }
    let mut frame = vec![0; HEADER_BYTES];
    write(&mut frame, 0, MAGIC)?;
    write(&mut frame, 4, &[VERSION, op, 0, 0])?;
    write(&mut frame, 8, &request.to_le_bytes())?;
    if let Some(lease) = lease {
        put_lease(&mut frame, lease)?;
    }
    write(&mut frame, 64, &transaction.to_le_bytes())?;
    let bytes = u32::try_from(text.len()).map_err(|_| invalid())?;
    write(&mut frame, 72, &bytes.to_le_bytes())?;
    frame.extend_from_slice(text.as_bytes());
    Ok(frame)
}

/// Decode an exact response or unsolicited field change (request zero).
/// # Errors
/// Rejects unknown flags, trailing/truncated bytes, malformed UTF-8, protected
/// replies carrying text, and selections outside Unicode scalar boundaries.
pub fn decode(frame: &[u8]) -> io::Result<Reply> {
    if frame.len() < HEADER_BYTES
        || frame.len() > FRAME_BYTES
        || read::<4>(frame, 0)? != *MAGIC
        || read::<1>(frame, 4)? != [VERSION]
        || read::<1>(frame, 5)? != [0]
        || read::<4>(frame, 76)? != [0; 4]
    {
        return Err(invalid());
    }
    let status = match read::<1>(frame, 6)? {
        [1] => Status::Unavailable,
        [2] => Status::Ready,
        [3] => Status::Applied,
        [4] => Status::Undone,
        [5] => Status::Redone,
        [6] => Status::Stale,
        [7] => Status::Invalid,
        [8] => Status::Unknown,
        [9] => Status::Revoked,
        _ => return Err(invalid()),
    };
    let [flags] = read::<1>(frame, 7)?;
    if flags & !7 != 0 {
        return Err(invalid());
    }
    let bytes = usize::try_from(u32::from_le_bytes(read(frame, 72)?)).map_err(|_| invalid())?;
    if HEADER_BYTES.checked_add(bytes) != Some(frame.len()) {
        return Err(invalid());
    }
    let text = std::str::from_utf8(frame.get(HEADER_BYTES..).ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    let lease = if flags & 4 != 0 {
        let lease = Lease {
            incarnation: read(frame, 16)?,
            generation: u64::from_le_bytes(read(frame, 32)?),
            revision: u64::from_le_bytes(read(frame, 40)?),
            expires_ns: u64::from_le_bytes(read(frame, 48)?),
            anchor: u32::from_le_bytes(read(frame, 56)?),
            focus: u32::from_le_bytes(read(frame, 60)?),
        };
        if !lease.valid()
            || status != Status::Ready
            || text.chars().any(char::is_control)
            || !text.is_char_boundary(usize::try_from(lease.anchor).map_err(|_| invalid())?)
            || !text.is_char_boundary(usize::try_from(lease.focus).map_err(|_| invalid())?)
        {
            return Err(invalid());
        }
        Some(lease)
    } else {
        if !text.is_empty()
            || flags != 0
            || status == Status::Ready
            || frame
                .get(16..64)
                .ok_or_else(invalid)?
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(invalid());
        }
        None
    };
    Ok(Reply {
        request: u64::from_le_bytes(read(frame, 8)?),
        status,
        lease,
        transaction: u64::from_le_bytes(read(frame, 64)?),
        can_undo: flags & 1 != 0,
        can_redo: flags & 2 != 0,
        text: text.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease() -> Lease {
        Lease {
            incarnation: [1; 16],
            generation: 1,
            revision: 1,
            expires_ns: 1,
            anchor: 0,
            focus: 0,
        }
    }

    fn ready(text: &str) -> Vec<u8> {
        let mut frame = encode(
            1,
            &Request::Replace {
                lease: lease(),
                transaction: 1,
                text: text.into(),
            },
        )
        .unwrap();
        frame[5] = 0;
        frame[6] = 2;
        frame[7] = 4;
        frame
    }

    #[test]
    fn protected_replies_cannot_disclose_a_buffer_or_lease() {
        let ready = ready("private text");
        assert_eq!(decode(&ready).unwrap().text, "private text");
        for status in [1, 3, 4, 5, 6, 7, 8, 9] {
            let mut frame = ready.clone();
            frame[6] = status;
            assert!(
                decode(&frame).is_err(),
                "status {status} disclosed private text"
            );
            frame[7] = 0;
            assert!(
                decode(&frame).is_err(),
                "status {status} disclosed an unmarked buffer"
            );
        }
    }

    #[test]
    fn native_selection_and_packet_bounds_reject_malformed_input() {
        let mut frame = ready("界");
        frame[60..64].copy_from_slice(&1u32.to_le_bytes());
        assert!(decode(&frame).is_err(), "selection splits a UTF-8 scalar");
        frame[60..64].copy_from_slice(&3u32.to_le_bytes());
        assert!(decode(&frame).is_ok());
        assert!(decode(&frame[..frame.len() - 1]).is_err());
        frame.push(0);
        assert!(decode(&frame).is_err(), "trailing bytes accepted");
        for text in ["execute\n", "\r", "\0", "\u{1b}[A"] {
            assert!(
                encode(
                    1,
                    &Request::Replace {
                        lease: lease(),
                        transaction: 1,
                        text: text.into(),
                    }
                )
                .is_err(),
                "control text could enter the command channel"
            );
        }
        assert!(
            encode(
                1,
                &Request::Replace {
                    lease: lease(),
                    transaction: 1,
                    text: "x".repeat(TEXT_BYTES + 1),
                }
            )
            .is_err()
        );
        assert!(encode(0, &Request::Inspect).is_err());
    }
}
