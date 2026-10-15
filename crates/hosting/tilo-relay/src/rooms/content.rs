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

use std::sync::{Arc, PoisonError, RwLock};

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::peer_handle;

use super::Rooms;

/// The bytes of one piece, as members answer.
const PIECE_BYTES: usize = 192 * 1024;
/// Content the host answers with at most.
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// Where a host's content comes from.
pub trait RoomContent: Send + Sync {
    /// The bytes named `hash` (lowercase BLAKE3 hex) that a record of `app`
    /// in `room` may reference, when this host keeps them.
    fn read(&self, room: &str, app: &str, hash: &str) -> Option<Vec<u8>>;
}

/// The content source a host's rooms answer from, once it has one.
#[derive(Default)]
pub(super) struct ContentSlot(RwLock<Option<Arc<dyn RoomContent>>>);

impl ContentSlot {
    fn get(&self) -> Option<Arc<dyn RoomContent>> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Rooms {
    /// Answer members' requests for content from `content`.
    pub fn serve_content(&self, content: Arc<dyn RoomContent>) {
        *self
            .inner
            .content
            .0
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(content);
    }

    /// The pieces answering `body`, a presence `room`'s member published,
    /// when it asks for content this host keeps: frames for the member's
    /// device alone. Empty for any other presence.
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
        let Some(bytes) = self
            .inner
            .content
            .get()
            .and_then(|content| content.read(room, &app_id, &hash))
            .filter(|bytes| bytes.len() <= MAX_CONTENT_BYTES)
        else {
            return Vec::new();
        };
        let responder = peer_handle(&self.inner.host.cert.device_public_key);
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
    async fn a_member_asking_for_kept_content_gets_it_in_pieces_and_nothing_else() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = crate::rooms::tests::governor();
        let relay = crate::rooms::tests::view_read_relay(scratch.path(), &key, &cert).await;
        let rooms = relay.rooms().unwrap().clone();
        let avatar = vec![7_u8; PIECE_BYTES + 10];
        let hash = blake3::hash(&avatar).to_hex().to_string();
        assert!(
            rooms.content_answer(ROOM, &want(ROOM, &hash)).is_empty(),
            "nothing before the host serves content"
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
        assert!(
            rooms
                .content_answer(ROOM, &want(ROOM, &"0".repeat(64)))
                .is_empty(),
            "not kept"
        );
        assert!(rooms.content_answer(ROOM, b"not a request").is_empty());
    }
}
