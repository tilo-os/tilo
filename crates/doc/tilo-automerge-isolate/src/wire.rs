//! Messages between the caller and its worker: little-endian, length-prefixed.
//!
//! A frame is `u64 length` then the payload. Requests:
//!
//! - `1 key deps prepare incoming…`: apply `incoming` to the worker's copy of
//!   the document at `key`. `prepare` is `0` (use the cached copy), `1 bytes`
//!   (a full save of the caller's document) or `2 from delta` (bring the
//!   cached copy at `from` up to `key` with the caller's newer changes).
//! - `2 incoming…`: load `incoming` as a document.
//! - `3 incoming…`: enumerate its exact change hashes in the confined worker.
//!
//! The worker's first frame is [`READY`], sent once it is confined; any other
//! first frame says why it could not be.
//!
//! Replies: `0 changed`, `1` (missing dependencies), `2 count keys…` (no copy
//! at `key`; the ones the worker holds), `3 message…` (Automerge refused it),
//! `4 count hashes…` (the exact incoming change set).

use std::io::{self, Read, Write};

use crate::engine::Key;
use crate::{Applied, Dependencies};
use automerge::ChangeHash;

/// The first frame a worker sends, once confined and before reading anything.
pub(crate) const READY: &[u8] = b"tilo-automerge-isolate 1";
/// Replies are a few keys at most; anything larger is not a worker speaking.
pub(crate) const MAX_REPLY_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) enum Prepare<'a> {
    Cached,
    Full(&'a [u8]),
    After { from: Key, delta: &'a [u8] },
}

pub(crate) enum Request<'a> {
    Apply {
        key: Key,
        dependencies: Dependencies,
        prepare: Prepare<'a>,
        incoming: &'a [u8],
    },
    Load {
        incoming: &'a [u8],
    },
    ChangeHashes {
        incoming: &'a [u8],
    },
    /// Make a syscall the sandbox must refuse, to show that it does.
    #[cfg(test)]
    Escape(Escape),
}

/// What [`Request::Escape`] tries.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Escape {
    /// `open` a file that exists and anyone may read.
    Open,
    /// Create a Unix socket.
    Socket,
}

pub(crate) enum Reply {
    Applied(Applied),
    Uncached(Vec<Key>),
    Invalid(String),
    ChangeHashes(Vec<ChangeHash>),
}

pub(crate) fn write_frame(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u64::try_from(payload.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    out.write_all(&length.to_le_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

/// `None` at a clean end of input.
pub(crate) fn read_frame(input: &mut impl Read, max: u64) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 8];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u64::from_le_bytes(length);
    if length > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = Vec::new();
    input.take(length).read_to_end(&mut payload)?;
    if u64::try_from(payload.len()).ok() != Some(length) {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(payload))
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    out.extend(length.to_le_bytes());
    out.extend(bytes);
}

fn take_bytes<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (length, rest) = input.split_first_chunk::<8>()?;
    let length = usize::try_from(u64::from_le_bytes(*length)).ok()?;
    let bytes = rest.get(..length)?;
    *input = rest.get(length..)?;
    Some(bytes)
}

fn take_byte(input: &mut &[u8]) -> Option<u8> {
    let (byte, rest) = input.split_first()?;
    *input = rest;
    Some(*byte)
}

impl Request<'_> {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Apply {
                key,
                dependencies,
                prepare,
                incoming,
            } => {
                out.push(1);
                push_bytes(&mut out, key.as_bytes());
                out.push(match dependencies {
                    Dependencies::RequireComplete => 0,
                    Dependencies::RetainOrphans => 1,
                });
                match prepare {
                    Prepare::Cached => out.push(0),
                    Prepare::Full(bytes) => {
                        out.push(1);
                        push_bytes(&mut out, bytes);
                    }
                    Prepare::After { from, delta } => {
                        out.push(2);
                        push_bytes(&mut out, from.as_bytes());
                        push_bytes(&mut out, delta);
                    }
                }
                out.extend(*incoming);
            }
            Self::Load { incoming } => {
                out.push(2);
                out.extend(*incoming);
            }
            Self::ChangeHashes { incoming } => {
                out.push(3);
                out.extend(*incoming);
            }
            #[cfg(test)]
            Self::Escape(escape) => {
                out.push(0xff);
                out.push(match escape {
                    Escape::Open => 0,
                    Escape::Socket => 1,
                });
            }
        }
        out
    }

    pub(crate) fn decode(mut input: &[u8]) -> Option<Request<'_>> {
        match take_byte(&mut input)? {
            1 => {
                let key = Key::from_bytes(take_bytes(&mut input)?.to_vec());
                let dependencies = match take_byte(&mut input)? {
                    0 => Dependencies::RequireComplete,
                    1 => Dependencies::RetainOrphans,
                    _ => return None,
                };
                let prepare = match take_byte(&mut input)? {
                    0 => Prepare::Cached,
                    1 => Prepare::Full(take_bytes(&mut input)?),
                    2 => Prepare::After {
                        from: Key::from_bytes(take_bytes(&mut input)?.to_vec()),
                        delta: take_bytes(&mut input)?,
                    },
                    _ => return None,
                };
                Some(Request::Apply {
                    key,
                    dependencies,
                    prepare,
                    incoming: input,
                })
            }
            2 => Some(Request::Load { incoming: input }),
            3 => Some(Request::ChangeHashes { incoming: input }),
            #[cfg(test)]
            0xff => Some(Request::Escape(match take_byte(&mut input)? {
                0 => Escape::Open,
                _ => Escape::Socket,
            })),
            _ => None,
        }
    }
}

impl Reply {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Applied(Applied::Changed(changed)) => {
                out.push(0);
                out.push(u8::from(*changed));
            }
            Self::Applied(Applied::MissingDependencies) => out.push(1),
            Self::Uncached(keys) => {
                out.push(2);
                out.extend(u64::try_from(keys.len()).unwrap_or(0).to_le_bytes());
                for key in keys {
                    push_bytes(&mut out, key.as_bytes());
                }
            }
            Self::ChangeHashes(hashes) => {
                out.push(4);
                out.extend(
                    u64::try_from(hashes.len())
                        .unwrap_or(u64::MAX)
                        .to_le_bytes(),
                );
                for hash in hashes {
                    out.extend(hash.0);
                }
            }
            Self::Invalid(message) => {
                out.push(3);
                out.extend(message.as_bytes());
            }
        }
        out
    }

    pub(crate) fn decode(mut input: &[u8]) -> Option<Self> {
        match take_byte(&mut input)? {
            0 => Some(Self::Applied(Applied::Changed(take_byte(&mut input)? != 0))),
            1 => Some(Self::Applied(Applied::MissingDependencies)),
            2 => {
                let (count, mut rest) = input.split_first_chunk::<8>()?;
                let count = u64::from_le_bytes(*count);
                let mut keys = Vec::new();
                for _ in 0..count {
                    keys.push(Key::from_bytes(take_bytes(&mut rest)?.to_vec()));
                }
                Some(Self::Uncached(keys))
            }
            3 => Some(Self::Invalid(String::from_utf8_lossy(input).into_owned())),
            4 => {
                let (count, rest) = input.split_first_chunk::<8>()?;
                let count = usize::try_from(u64::from_le_bytes(*count)).ok()?;
                if count.checked_mul(32)? != rest.len() {
                    return None;
                }
                let hashes = rest
                    .chunks_exact(32)
                    .map(|bytes| bytes.try_into().ok().map(ChangeHash))
                    .collect::<Option<Vec<_>>>()?;
                Some(Self::ChangeHashes(hashes))
            }
            _ => None,
        }
    }
}
