//! Original launch binding and disposable reads on the admitted channel; no caller-selected identity.
use serde::{Deserialize, Serialize};
/// Inert recovery provenance returned by the authenticated channel, never an
/// application's claimed invoker or authority to execute/publish.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    pub root: [u8; 32],
    pub device: [u8; 32],
    pub agent: Option<String>,
    pub app_id: String,
    /// This workspace's id. Its JSON name, and the name every other crate
    /// that builds or reads a snapshot uses, is `room`: kept as it was.
    pub room: String,
    pub document: String,
}
impl ConnectionSnapshot {
    pub fn valid(&self) -> bool {
        self.root != [0; 32]
            && self.device != [0; 32]
            && [&self.app_id, &self.room]
                .into_iter()
                .all(|s| !s.is_empty() && valid_snapshot_text(s))
            && valid_snapshot_text(&self.document)
            && self
                .agent
                .as_ref()
                .is_none_or(|s| !s.is_empty() && valid_snapshot_text(s))
    }
}
fn valid_snapshot_text(text: &str) -> bool {
    text.len() <= 4096 && !text.chars().any(char::is_control)
}
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_WIRE_BYTES: usize = CHUNK_BYTES + 128;
/// The most heads a [`Reply::Changes`] names as its base. A document edited
/// concurrently by more writers than this is read whole instead.
pub const MAX_CHANGES_BASE: usize = 8;
/// The most change bytes the first piece of a [`Reply::Changes`] carries, so
/// that piece with its base stays within [`MAX_WIRE_BYTES`] as a
/// [`Reply::Chunk`] does. The rest follows through [`Request::Next`].
pub const CHANGES_FIRST_BYTES: usize = CHUNK_BYTES - MAX_CHANGES_BASE * 32;
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
    /// Retired: a record is made by its first write, never by an
    /// initialization request. Kept in place so later requests keep their
    /// tags.
    RetiredInitialize,
    /// Read only the host-retained window instance for this live connection.
    PrivateWindowBinding,
    /// Hold until an `Open { known_revision }` would answer anything but
    /// what the caller already holds (`Unchanged` for that revision, or
    /// for `None` the `Unavailable` or `Absent` this channel last
    /// answered), then answer as that Open does (as `OpenChanges` for a
    /// revision this channel handed out): the changed record comes with the
    /// wake, read on and acknowledged as any Open's capture. Refused once
    /// the caller may no longer read. A caller that gives up the wait
    /// before its answer came just Opens: the capture it never saw is
    /// dropped.
    Await {
        known_revision: Option<[u8; 32]>,
    },
    /// An `Open { Some(known_revision) }` that may answer with only the
    /// changes since: [`Reply::Changes`] when this channel last handed out
    /// `known_revision` (a capture whose last piece it acknowledged), and
    /// otherwise exactly what that `Open` answers. Same checks, same
    /// acknowledgement.
    OpenChanges {
        known_revision: [u8; 32],
    },
    /// Original authenticated principal and exact channel route; no caller claims.
    ConnectionSnapshot,
    /// The caller reads no more on this channel: give up its place in the
    /// broker's byte budget queue (after a [`Reply::Busy`]), and the place
    /// it was granted, so reads behind it are served. Answered
    /// [`Reply::Cancelled`]; an active capture is untouched.
    Leave,
    /// How this channel's writes travel: answered [`Reply::Writes`].
    Writes,
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
    /// Retired: the creation receipt of a record's first materialization,
    /// whose first write makes it whole. Kept in place so later replies keep
    /// their tags.
    RetiredCreated,
    /// Retired: the answer to an initialization request, which no record
    /// takes. Kept in place so later replies keep their tags.
    RetiredInitialized,
    /// No instance means this connection has no native private control binding.
    PrivateWindowBinding {
        instance: Option<u64>,
    },
    /// Retired: an [`Request::Await`] answers with the read itself. Kept in
    /// place so later variants keep their tags.
    RetiredStale,
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
    ConnectionSnapshot(ConnectionSnapshot),
    /// The record reaches a device only once it asks
    /// (`neighbour_wire::record_sync`), and the workspace's hosts sent
    /// everything they hold of it without it, or no host of the workspace
    /// is connected: not readable, and not on its way either. Like
    /// [`Self::Unavailable`] an [`Request::Await`] for `None` holds until
    /// that changes (a host connects, the record arrives).
    Absent,
    /// An Open the broker cannot capture yet: its byte budget is held by
    /// other reads. The read is queued in order, and nothing is refused: an
    /// [`Request::Await`] on this channel answers with the Open's capture
    /// once the read's turn has come. A caller that
    /// stops reading on the channel sends [`Request::Leave`].
    Busy,
    /// The answer to [`Request::Writes`]. `sealed`: the record's workspace
    /// seals every write under its channel's keys, so the app encrypts each
    /// one and a write it cannot encrypt is refused, never sent as written.
    /// Otherwise the workspace's owner commits the writes as they are.
    Writes {
        sealed: bool,
    },
}
impl Request {
    /// Encode a valid request for the wire.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("invalid committed read request");
        }
        postcard::to_stdvec(self).map_err(|_| "committed read encoding")
    }
    /// Decode exactly one bounded, valid request.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_WIRE_BYTES {
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
            | Self::ConnectionSnapshot
            | Self::Writes
            | Self::PrivateWindowBinding
            | Self::Await { .. }
            | Self::OpenChanges { .. }
            | Self::RetiredInitialize
            | Self::Leave => true,
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
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("invalid committed read reply");
        }
        postcard::to_stdvec(self).map_err(|_| "committed read encoding")
    }
    /// Decode exactly one bounded, valid reply.
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
            Self::ConnectionSnapshot(snapshot) => snapshot.valid(),
            Self::PrivateWindowBinding { instance } => {
                instance.is_none_or(|instance| instance != 0)
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
    fn busy_is_a_tag_of_its_own_after_every_earlier_reply() {
        assert_eq!(Reply::Absent.encode().unwrap(), [11]);
        assert_eq!(Reply::Busy.encode().unwrap(), [12]);
        assert_eq!(Reply::decode(&[12]).unwrap(), Reply::Busy);
        assert_eq!(Request::Leave.encode().unwrap(), [8]);
        assert_eq!(Request::decode(&[8]).unwrap(), Request::Leave);
    }

    #[test]
    fn connection_snapshot_is_bounded_and_keeps_original_principal_and_route() {
        assert_eq!(Request::ConnectionSnapshot.encode().unwrap(), [7]);
        let snapshot = ConnectionSnapshot {
            root: [1; 32],
            device: [2; 32],
            agent: Some("agent".into()),
            app_id: "notes".into(),
            room: "workspace".into(),
            document: "document".into(),
        };
        let reply = Reply::ConnectionSnapshot(snapshot.clone());
        assert_eq!(Reply::decode(&reply.encode().unwrap()).unwrap(), reply);
        for field in 0..5 {
            let mut invalid = snapshot.clone();
            match field {
                0 => invalid.root = [0; 32],
                1 => invalid.device = [0; 32],
                2 => invalid.agent = Some(String::new()),
                3 => invalid.document = "x".repeat(4097),
                _ => invalid.room = "bad\nworkspace".into(),
            }
            assert!(Reply::ConnectionSnapshot(invalid).encode().is_err());
        }
        let mut trailing = reply.encode().unwrap();
        trailing.push(0);
        assert!(Reply::decode(&trailing).is_err());
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
        assert_eq!(Reply::RetiredStale.encode().unwrap(), [8]);
        assert_eq!(Reply::decode(&[8]).unwrap(), Reply::RetiredStale);
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
