//! Original launch binding and disposable reads on the admitted channel; no caller-selected identity.
use serde::{Deserialize, Serialize};
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_WIRE_BYTES: usize = CHUNK_BYTES + 128;
/// Bootstrap is deliberately bounded independently of ordinary document reads.
pub const MAX_INITIALIZATION_BYTES: usize = CHUNK_BYTES;
/// The most heads a [`Reply::Changes`] names as its base. A document edited
/// concurrently by more writers than this is read whole instead.
pub const MAX_CHANGES_BASE: usize = 8;
/// The most change bytes the first piece of a [`Reply::Changes`] carries, so
/// that piece with its base stays within [`MAX_WIRE_BYTES`] as a
/// [`Reply::Chunk`] does. The rest follows through [`Request::Next`].
pub const CHANGES_FIRST_BYTES: usize = CHUNK_BYTES - MAX_CHANGES_BASE * 32;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationWorkspace {
    pub id: String,
    pub name: String,
}
impl CreationWorkspace {
    pub fn valid(&self) -> bool {
        self.id.len() == 36
            && self.id.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
            && self.id.bytes().any(|byte| byte != b'0' && byte != b'-')
            && !self.name.is_empty()
            && self.name.len() <= 256
            && !self.name.chars().any(char::is_control)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationReceipt {
    pub token: [u8; 16],
    pub workspace: CreationWorkspace,
}
/// A first capture chunk. Kept nonrecursive so untrusted wire decoding has no
/// attacker-controlled nesting depth.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationChunk {
    pub capture: u64,
    pub revision: [u8; 32],
    pub total: u32,
    pub bytes: Vec<u8>,
    pub token: [u8; 16],
}
impl CreationChunk {
    pub fn into_reply(self) -> Reply {
        Reply::Chunk {
            capture: self.capture,
            revision: self.revision,
            total: self.total,
            offset: 0,
            bytes: self.bytes,
            token: self.token,
        }
    }
    /// Extract only the first bounded capture chunk.
    ///
    /// # Errors
    /// Refuses every other reply or a continuation offset.
    pub fn from_reply(reply: Reply) -> Result<Self, &'static str> {
        let Reply::Chunk {
            capture,
            revision,
            total,
            offset: 0,
            bytes,
            token,
        } = reply
        else {
            return Err("creation requires first capture chunk");
        };
        Ok(Self {
            capture,
            revision,
            total,
            bytes,
            token,
        })
    }
}
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
    Initialize {
        token: [u8; 16],
        revision: [u8; 32],
        kind: String,
        document: Vec<u8>,
    },
    /// Read only the host-retained window instance for this live connection.
    PrivateWindowBinding,
    /// Hold until an `Open { known_revision }` would answer anything but
    /// what the caller already holds (`Unchanged` for that revision, or
    /// `Unavailable` for `None`), then answer [`Reply::Stale`]. Refused once
    /// the caller may no longer read. Never carries document bytes and
    /// never publishes readiness: the caller re-`Open`s.
    Await {
        known_revision: Option<[u8; 32]>,
    },
    /// An `Open { Some(known_revision) }` that may answer with only the
    /// changes since: [`Reply::Changes`] when this channel last handed out
    /// `known_revision` (a capture whose last piece it acknowledged), and
    /// otherwise exactly what that `Open` answers. Same checks, same
    /// acknowledgement. An older broker refuses it, and the caller Opens.
    OpenChanges {
        known_revision: [u8; 32],
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
    /// Only the original first materialization on this admitted channel.
    Created {
        receipt: CreationReceipt,
        chunk: CreationChunk,
    },
    Initialized {
        revision: [u8; 32],
    },
    /// No instance means this connection has no native private control binding.
    PrivateWindowBinding {
        instance: Option<u64>,
    },
    /// An [`Request::Await`] ended: what the caller holds may be stale.
    Stale,
    /// The first piece of the changes that turn the document with heads
    /// `base` (the revision the caller asked from) into `revision`, as
    /// Automerge change bytes. Only an answer to [`Request::OpenChanges`].
    /// Like a [`Reply::Chunk`] at offset 0, `total` bytes follow through
    /// [`Request::Next`] (answered with `Chunk`s of this `capture` and
    /// `revision`) and [`Request::Cancel`] acknowledges the last piece. They
    /// apply only to a document holding every `base` head.
    Changes {
        capture: u64,
        revision: [u8; 32],
        base: Vec<[u8; 32]>,
        total: u32,
        bytes: Vec<u8>,
        token: [u8; 16],
    },
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
        if bytes.len() > MAX_INITIALIZATION_BYTES + 512 {
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
            Self::Open { .. }
            | Self::PrivateWindowBinding
            | Self::Await { .. }
            | Self::OpenChanges { .. } => true,
            Self::Next {
                capture,
                offset,
                token,
            } => *capture != 0 && *token != [0; 16] && (*offset as usize) < MAX_DOCUMENT_BYTES,
            Self::Cancel { capture, token } => *capture != 0 && *token != [0; 16],
            Self::Initialize {
                token,
                kind,
                document,
                ..
            } => {
                *token != [0; 16]
                    && !kind.is_empty()
                    && kind.len() <= 256
                    && !kind.chars().any(char::is_control)
                    && !document.is_empty()
                    && document.len() <= MAX_INITIALIZATION_BYTES
            }
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
        if bytes.len() > MAX_WIRE_BYTES + 512 {
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
            Self::PrivateWindowBinding { instance } => {
                instance.is_none_or(|instance| instance != 0)
            }
            Self::Created { receipt, chunk } => {
                receipt.token != [0; 16]
                    && receipt.workspace.valid()
                    && chunk.capture != 0
                    && chunk.token != [0; 16]
                    && chunk.total != 0
                    && chunk.total as usize <= MAX_DOCUMENT_BYTES
                    && !chunk.bytes.is_empty()
                    && chunk.bytes.len() <= CHUNK_BYTES
                    && chunk.bytes.len() <= chunk.total as usize
            }
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
            Self::Changes {
                capture,
                base,
                total,
                bytes,
                token,
                ..
            } => {
                *capture != 0
                    && *token != [0; 16]
                    && *total != 0
                    && (*total as usize) <= MAX_DOCUMENT_BYTES
                    && !bytes.is_empty()
                    && bytes.len() <= CHANGES_FIRST_BYTES
                    && bytes.len() <= *total as usize
                    && !base.is_empty()
                    && base.len() <= MAX_CHANGES_BASE
            }
            _ => true,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_wire_bounds_and_first_creation_receipt_are_explicit() {
        let workspace = CreationWorkspace {
            id: "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af".into(),
            name: "Owner room".into(),
        };
        let reply = Reply::Created {
            receipt: CreationReceipt {
                token: [7; 16],
                workspace,
            },
            chunk: CreationChunk {
                capture: 1,
                revision: [3; 32],
                total: 1,
                bytes: vec![1],
                token: [4; 16],
            },
        };
        assert_eq!(Reply::decode(&reply.encode().unwrap()).unwrap(), reply);
        let request = Request::Initialize {
            token: [7; 16],
            revision: [3; 32],
            kind: "chat.workspace".into(),
            document: vec![1; MAX_INITIALIZATION_BYTES],
        };
        assert_eq!(
            Request::decode(&request.encode().unwrap()).unwrap(),
            request
        );
        for (token, kind, document) in [
            ([0; 16], "chat.workspace", vec![1]),
            ([7; 16], "", vec![1]),
            ([7; 16], "chat.workspace", Vec::new()),
            (
                [7; 16],
                "chat.workspace",
                vec![1; MAX_INITIALIZATION_BYTES + 1],
            ),
        ] {
            assert!(
                Request::Initialize {
                    token,
                    revision: [3; 32],
                    kind: kind.into(),
                    document
                }
                .encode()
                .is_err()
            );
        }
        let mut trailing = reply.encode().unwrap();
        trailing.push(0);
        assert!(Reply::decode(&trailing).is_err());
        assert!(CreationChunk::from_reply(Reply::Initialized { revision: [3; 32] }).is_err());
    }
    #[test]
    fn private_binding_wire_is_bounded_append_only_and_has_no_selected_identity() {
        assert_eq!(
            Request::Open {
                known_revision: None
            }
            .encode()
            .unwrap(),
            [0, 0]
        );
        assert_eq!(Request::PrivateWindowBinding.encode().unwrap(), [4]);
        assert_eq!(Reply::Legacy.encode().unwrap(), [0]);
        assert_eq!(
            Reply::PrivateWindowBinding { instance: None }
                .encode()
                .unwrap(),
            [7, 0]
        );
        let reply = Reply::PrivateWindowBinding { instance: Some(42) };
        assert_eq!(Reply::decode(&reply.encode().unwrap()).unwrap(), reply);
        assert!(
            Reply::PrivateWindowBinding { instance: Some(0) }
                .encode()
                .is_err()
        );
        // A caller cannot append an instance or identity selector to the unit request.
        assert!(Request::decode(&[4, 42]).is_err());
        assert!(Reply::decode(&[7, 1, 0]).is_err());
    }

    #[test]
    fn await_and_stale_are_appended_and_carry_no_bytes() {
        // Appended after every existing variant: older peers' encodings keep
        // their tags, and an older broker refuses the unknown request.
        let request = Request::Await {
            known_revision: Some([5; 32]),
        };
        let encoded = request.encode().unwrap();
        assert_eq!(encoded[0], 5);
        assert_eq!(Request::decode(&encoded).unwrap(), request);
        assert_eq!(
            Request::Await {
                known_revision: None
            }
            .encode()
            .unwrap(),
            [5, 0]
        );
        assert_eq!(Reply::Stale.encode().unwrap(), [8]);
        assert_eq!(Reply::decode(&[8]).unwrap(), Reply::Stale);
        assert!(Reply::decode(&[8, 0]).is_err());
    }

    #[test]
    fn open_changes_and_changes_are_appended_and_bounded() {
        // Appended after every existing variant: older peers keep their
        // tags, and an older broker refuses the unknown request.
        let request = Request::OpenChanges {
            known_revision: [5; 32],
        };
        let encoded = request.encode().unwrap();
        assert_eq!(encoded[0], 6);
        assert_eq!(Request::decode(&encoded).unwrap(), request);
        let reply = |base: usize, first: usize, total: u32| Reply::Changes {
            capture: 1,
            revision: [7; 32],
            base: vec![[3; 32]; base],
            total,
            bytes: vec![9; first],
            token: [4; 16],
        };
        let largest = reply(
            MAX_CHANGES_BASE,
            CHANGES_FIRST_BYTES,
            u32::try_from(MAX_DOCUMENT_BYTES).unwrap(),
        );
        let encoded = largest.encode().unwrap();
        assert_eq!(encoded[0], 9);
        assert!(
            encoded.len() <= MAX_WIRE_BYTES,
            "the largest first piece fits the wire like a chunk: {} bytes",
            encoded.len()
        );
        assert_eq!(Reply::decode(&encoded).unwrap(), largest);
        for refused in [
            reply(0, 1, 1),
            reply(MAX_CHANGES_BASE + 1, 1, 1),
            reply(1, CHANGES_FIRST_BYTES + 1, u32::MAX),
            reply(1, 0, 1),
            reply(1, 2, 1),
            reply(1, 1, 0),
            reply(1, 1, u32::try_from(MAX_DOCUMENT_BYTES).unwrap() + 1),
        ] {
            assert!(refused.encode().is_err(), "{refused:?}");
        }
        let mut zero = reply(1, 1, 1);
        if let Reply::Changes { token, .. } = &mut zero {
            *token = [0; 16];
        }
        assert!(zero.encode().is_err());
    }

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
