//! Disposable reads on the already admitted channel; no route or identity on the wire.
use serde::{Deserialize, Serialize};
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_WIRE_BYTES: usize = CHUNK_BYTES + 128;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Open {
        known_revision: Option<[u8; 32]>,
    },
    Next {
        capture: u64,
        offset: u32,
        token: [u8; 16],
    },
    Cancel {
        capture: u64,
        token: [u8; 16],
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Legacy,
    Unavailable,
    Unchanged {
        revision: [u8; 32],
    },
    Chunk {
        capture: u64,
        revision: [u8; 32],
        total: u32,
        offset: u32,
        bytes: Vec<u8>,
        token: [u8; 16],
    },
    Cancelled,
}
impl Request {
    /// Encode a valid request for the wire.
    ///
    /// # Errors
    ///
    /// Refuses an invalid request or a postcard encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("invalid committed read request");
        }
        postcard::to_stdvec(self).map_err(|_| "committed read encoding")
    }
    /// Decode exactly one bounded, valid request.
    ///
    /// # Errors
    ///
    /// Refuses oversized, trailing, malformed or invalid input.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 64 {
            return Err("committed read request too large");
        }
        let (value, tail): (Self, _) =
            postcard::take_from_bytes(bytes).map_err(|_| "committed read request encoding")?;
        if !tail.is_empty() || !value.valid() {
            return Err("invalid committed read request");
        }
        Ok(value)
    }
    fn valid(&self) -> bool {
        match self {
            Self::Open { .. } => true,
            Self::Next {
                capture,
                offset,
                token,
            } => *capture != 0 && *token != [0; 16] && (*offset as usize) < MAX_DOCUMENT_BYTES,
            Self::Cancel { capture, token } => *capture != 0 && *token != [0; 16],
        }
    }
}
impl Reply {
    /// Encode a valid reply for the wire.
    ///
    /// # Errors
    ///
    /// Refuses an invalid reply or a postcard encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("invalid committed read reply");
        }
        postcard::to_stdvec(self).map_err(|_| "committed read encoding")
    }
    /// Decode exactly one bounded, valid reply.
    ///
    /// # Errors
    ///
    /// Refuses oversized, trailing, malformed or invalid input.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_WIRE_BYTES {
            return Err("committed read reply too large");
        }
        let (value, tail): (Self, _) =
            postcard::take_from_bytes(bytes).map_err(|_| "committed read reply encoding")?;
        if !tail.is_empty() || !value.valid() {
            return Err("invalid committed read reply");
        }
        Ok(value)
    }
    fn valid(&self) -> bool {
        match self {
            Self::Chunk {
                capture,
                total,
                offset,
                bytes,
                token,
                ..
            } => {
                *capture != 0
                    && *token != [0; 16]
                    && *total != 0
                    && (*total as usize) <= MAX_DOCUMENT_BYTES
                    && !bytes.is_empty()
                    && bytes.len() <= CHUNK_BYTES
                    && (*offset as usize)
                        .checked_add(bytes.len())
                        .is_some_and(|end| end <= *total as usize)
            }
            _ => true,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_read_codec_roundtrip_and_bounds() {
        let request = Request::Open {
            known_revision: None,
        };
        assert_eq!(
            Request::decode(&request.encode().unwrap()).unwrap(),
            request
        );
        let reply = Reply::Chunk {
            capture: 1,
            revision: [7; 32],
            total: u32::try_from(CHUNK_BYTES).unwrap(),
            offset: 0,
            bytes: vec![9; CHUNK_BYTES],
            token: [3; 16],
        };
        assert_eq!(Reply::decode(&reply.encode().unwrap()).unwrap(), reply);
        let mut extra = request.encode().unwrap();
        extra.push(0);
        assert!(Request::decode(&extra).is_err());
        for (total, offset, len, capture) in [
            (0, 0, 0, 1),
            (1, 0, 2, 1),
            (u32::try_from(MAX_DOCUMENT_BYTES).unwrap() + 1, 0, 1, 1),
            (1, 0, 1, 0),
            (
                u32::try_from(CHUNK_BYTES).unwrap() + 1,
                0,
                CHUNK_BYTES + 1,
                1,
            ),
        ] {
            assert!(
                Reply::Chunk {
                    capture,
                    revision: [0; 32],
                    total,
                    offset,
                    bytes: vec![0; len],
                    token: [3; 16]
                }
                .encode()
                .is_err()
            );
        }
    }
}
