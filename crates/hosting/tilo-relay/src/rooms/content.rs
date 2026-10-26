//! Content a governed room's records reference that this host keeps: what
//! its bots fetched from outside (a bridged person's avatar, a file posted
//! elsewhere) and the surfaces of the apps it runs, which members decide
//! the host's changes against.
//!
//! A member asks the room for content its record lacks with the room's
//! presence ([`NeighbourMsg::RecordContentWant`]), as it asks any member.
//! The host answers from what it keeps, to that member's device alone, in
//! the same bounded pieces a member answers with; the member keeps the
//! content once every piece arrived and the bytes hash to what it asked.
//!
//! The host asks too: it keeps a copy of the files members post
//! ([`ContentKeeper`]), so they stay in the room after the member who
//! posted them leaves. It asks the room on its own presence, and the
//! pieces members answer it with come to it, not to another device.

use std::sync::{Arc, RwLock};

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::peer_handle;

use super::Rooms;
use tilo_lock::RwLockRecover as _;

/// The bytes of one piece, as members answer.
const PIECE_BYTES: usize = 192 * 1024;
/// Content the host answers with at most.
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// Word from the host `host` that it keeps none of what `body`, a
/// member's `NeighbourMsg::RecordContentWant`, asks for, as a frame for the
/// member's device alone: the member stops counting on this host for it.
/// `None` for any other presence.
pub(crate) fn lacks_answer(host: &[u8; 32], body: &[u8]) -> Option<Vec<u8>> {
    let Ok(NeighbourMsg::RecordContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
    }) = postcard::from_bytes(body)
    else {
        return None;
    };
    let hash = hash.to_ascii_lowercase();
    if !tilo_content::valid_hash(&hash) {
        return None;
    }
    postcard::to_allocvec(&NeighbourMsg::RecordContentLacks {
        responder: peer_handle(host),
        requester,
        app_id,
        cluster_id,
        hash,
    })
    .ok()
}

/// Where a host's content comes from.
pub trait RoomContent: Send + Sync {
    /// The bytes named `hash` (lowercase BLAKE3 hex) that a record of `app`
    /// in `room` may reference, when this host keeps them.
    fn read(&self, room: &str, app: &str, hash: &str) -> Option<Vec<u8>>;
}

/// Where content members answer the host with goes.
pub trait ContentKeeper: Send + Sync {
    /// `frame`, a `NeighbourMsg::RecordContentPiece` the member device
    /// `from` sent this host in `room`.
    fn piece(&self, room: &str, from: [u8; 32], frame: &[u8]);

    /// Whether `frame` answers a request this host made in `room` and
    /// would keep: such a piece counts against what the keeper keeps, not
    /// the member's general content budget.
    fn expects(&self, room: &str, frame: &[u8]) -> bool {
        let _ = (room, frame);
        false
    }
}

/// The content source a host's rooms answer from, and the keeper of what
/// members answer the host with, once it has them.
#[derive(Default)]
pub(super) struct ContentSlot {
    source: RwLock<Option<Arc<dyn RoomContent>>>,
    keeper: RwLock<Option<Arc<dyn ContentKeeper>>>,
}

impl ContentSlot {
    fn get(&self) -> Option<Arc<dyn RoomContent>> {
        self.source.read_recover().clone()
    }
}

impl Rooms {
    /// Answer members' requests for content from `content`.
    pub fn serve_content(&self, content: Arc<dyn RoomContent>) {
        *self.inner.content.source.write_recover() = Some(content);
    }

    /// The bytes named `hash` this host keeps for the room of `workspace`
    /// governed here, checked against the hash: what its bots fetched and
    /// the files it keeps for members. `None` for a room not governed here
    /// or content it does not keep.
    pub async fn kept_content(&self, workspace: u64, hash: &str) -> Option<Vec<u8>> {
        let source = self.inner.content.get()?;
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .iter()
            .map(|(room, governed)| (room.clone(), governed.clone()))
            .collect::<Vec<_>>();
        for (room, governed) in rooms {
            if governed.lock().await.workspace_id() == workspace {
                return source
                    .read(&room, "", hash)
                    .filter(|bytes| tilo_content::verify(hash, bytes));
            }
        }
        None
    }

    /// Hand the pieces of content members send this host to `keeper`.
    pub fn keep_content(&self, keeper: Arc<dyn ContentKeeper>) {
        *self.inner.content.keeper.write_recover() = Some(keeper);
    }

    /// `frame`, a piece of content the member device `from` sent this host
    /// in `room`, to the keeper; dropped when there is none.
    pub(crate) fn content_for_host(&self, room: &str, from: [u8; 32], frame: &[u8]) {
        let keeper = self.inner.content.keeper.read_recover().clone();
        if let Some(keeper) = keeper {
            keeper.piece(room, from, frame);
        }
    }

    /// Whether `frame`, a piece of content sent this host in `room`,
    /// answers a request its keeper made ([`ContentKeeper::expects`]).
    pub(crate) fn host_expects(&self, room: &str, frame: &[u8]) -> bool {
        let keeper = self.inner.content.keeper.read_recover().clone();
        keeper.is_some_and(|keeper| keeper.expects(room, frame))
    }

    /// Publish `body`, a `NeighbourMsg::RecordContentWant` naming this
    /// host, on `room`'s presence: the members keeping the content answer
    /// the host. The number of devices it reached.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or its workspace not hosted here.
    pub async fn ask_members(&self, room: &str, body: Vec<u8>) -> Result<usize, String> {
        let workspace_id = self
            .room(room)
            .await
            .map_err(|error| error.to_string())?
            .lock()
            .await
            .workspace_id();
        self.inner
            .host
            .records
            .host_presence(workspace_id, body)
            .await
    }

    /// The answer to `body`, a presence `room`'s member published, when it
    /// asks for content: the pieces of what this host keeps, or word that
    /// it keeps none, as frames for the member's device alone. Empty for
    /// any other presence.
    pub(crate) fn content_answer(&self, room: &str, body: &[u8]) -> Vec<Vec<u8>> {
        let Ok(NeighbourMsg::RecordContentWant {
            requester,
            app_id,
            cluster_id,
            hash,
        }) = postcard::from_bytes(body)
        else {
            return Vec::new();
        };
        let hash = hash.to_ascii_lowercase();
        if cluster_id != room || !tilo_content::valid_hash(&hash) {
            return Vec::new();
        }
        let responder = peer_handle(&self.inner.host.cert.device_public_key);
        let Some(bytes) = self
            .app_file(room, &hash)
            .or_else(|| {
                self.inner
                    .content
                    .get()
                    .and_then(|content| content.read(room, &app_id, &hash))
            })
            .filter(|bytes| bytes.len() <= MAX_CONTENT_BYTES)
        else {
            return lacks_answer(&self.inner.host.cert.device_public_key, body)
                .into_iter()
                .collect();
        };
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let piece = |offset: u64, chunk: &[u8]| NeighbourMsg::RecordContentPiece {
            responder,
            requester,
            app_id: app_id.clone(),
            cluster_id: cluster_id.clone(),
            hash: hash.clone(),
            total,
            offset,
            bytes: chunk.to_vec(),
        };
        let pieces = if bytes.is_empty() {
            vec![piece(0, &[])]
        } else {
            let mut offset = 0_u64;
            bytes
                .chunks(PIECE_BYTES)
                .map(|chunk| {
                    let message = piece(offset, chunk);
                    offset = offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(0));
                    message
                })
                .collect()
        };
        pieces
            .iter()
            .filter_map(|message| postcard::to_allocvec(message).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    struct One(Vec<u8>);

    impl RoomContent for One {
        fn read(&self, room: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
            (room == ROOM
                && app == "so.tilo.chat"
                && hash == blake3::hash(&self.0).to_hex().as_str())
            .then(|| self.0.clone())
        }
    }

    const ROOM: &str = "2f10f463-8f4c-43a3-b9dd-18df422eb0bd";

    fn want(room: &str, hash: &str) -> Vec<u8> {
        postcard::to_allocvec(&NeighbourMsg::RecordContentWant {
            requester: 42,
            app_id: "so.tilo.chat".into(),
            cluster_id: room.into(),
            hash: hash.into(),
        })
        .unwrap()
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the relay stays live until its consuming shutdown"
    )]
    async fn a_member_asking_for_content_gets_it_in_pieces_or_word_that_it_is_not_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::rooms::tests::governor();
        let relay = crate::rooms::tests::view_read_relay(scratch.path(), &key, &cert).await;
        let rooms = relay.rooms().unwrap().clone();
        let avatar = vec![7_u8; PIECE_BYTES + 10];
        let hash = blake3::hash(&avatar).to_hex().to_string();
        assert_eq!(
            lacks(&rooms.content_answer(ROOM, &want(ROOM, &hash))),
            Some((peer_handle(&cert.device_public_key), 42)),
            "a host that serves no content keeps none"
        );
        rooms.serve_content(Arc::new(One(avatar.clone())));

        let frames = rooms.content_answer(ROOM, &want(ROOM, &hash));
        let mut gathered = Vec::new();
        for frame in &frames {
            let NeighbourMsg::RecordContentPiece {
                responder,
                requester,
                total,
                offset,
                bytes,
                ..
            } = postcard::from_bytes(frame).unwrap()
            else {
                panic!("a piece");
            };
            assert_eq!(responder, peer_handle(&cert.device_public_key));
            assert_eq!(requester, 42, "addressed to whoever asked");
            assert_eq!(total, u64::try_from(avatar.len()).unwrap());
            assert_eq!(offset, u64::try_from(gathered.len()).unwrap());
            gathered.extend(bytes);
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(gathered, avatar);

        let other = "0b7e4c2a-9d1f-4e3b-8a6c-5f2d1e0c9b8a";
        assert!(
            rooms.content_answer(other, &want(ROOM, &hash)).is_empty(),
            "another room's request"
        );
        assert_eq!(
            lacks(&rooms.content_answer(ROOM, &want(ROOM, &"0".repeat(64)))),
            Some((peer_handle(&cert.device_public_key), 42)),
            "not kept: the member is told so"
        );
        assert!(rooms.content_answer(ROOM, b"not a request").is_empty());
    }

    /// Who said, to whom, that it keeps none of what was asked: the one
    /// frame `frames` holds when it is such word.
    fn lacks(frames: &[Vec<u8>]) -> Option<(u64, u64)> {
        let [frame] = frames else {
            return None;
        };
        match postcard::from_bytes(frame).ok()? {
            NeighbourMsg::RecordContentLacks {
                responder,
                requester,
                ..
            } => Some((responder, requester)),
            _ => None,
        }
    }
}
