//! Content a record references and this page lacks (a picture, by its
//! hash), asked of the room on its presence as every device asks. The host
//! and any member keeping it answer this device alone, in pieces. Only
//! pieces of content this page asked for are kept, and the assembled bytes
//! are handed on only when they hash to what was asked for.

use std::collections::BTreeMap;

/// Largest content one reference names, as the room's hosts keep it.
const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes one piece carries.
const PIECE_BYTES: usize = 192 * 1024;
/// How long an unanswered request keeps collecting pieces, and how long
/// before the same content is asked of the room again.
const WANT_TTL_MS: u64 = 60_000;
/// Requests open at once; past it, the oldest goes.
const MAX_OPEN_WANTS: usize = 256;

/// Content this page asked a room for, and the pieces of it that came.
struct Want<T> {
    /// What the asker names the request by (the record it is for).
    tag: T,
    app_id: String,
    cluster_id: String,
    asked_ms: u64,
    total: Option<u64>,
    pieces: BTreeMap<u64, Vec<u8>>,
}

impl<T> Want<T> {
    /// The whole content once its pieces cover it, end to end.
    fn assembled(&self) -> Option<Vec<u8>> {
        let total = self.total?;
        let mut bytes = Vec::with_capacity(usize::try_from(total).ok()?);
        for (offset, piece) in &self.pieces {
            if *offset != u64::try_from(bytes.len()).ok()? {
                return None;
            }
            bytes.extend_from_slice(piece);
        }
        (u64::try_from(bytes.len()).ok()? == total).then_some(bytes)
    }
}

/// Content whose whole, verified bytes arrived.
pub struct Arrived<T> {
    /// What the request was named by.
    pub tag: T,
    /// The content's address, lowercase BLAKE3 hex.
    pub hash: String,
    /// The content itself.
    pub bytes: Vec<u8>,
}

/// The content requests this page has open, by hash.
pub struct ContentWants<T> {
    wants: BTreeMap<String, Want<T>>,
}

impl<T> Default for ContentWants<T> {
    fn default() -> Self {
        Self {
            wants: BTreeMap::new(),
        }
    }
}

impl<T> ContentWants<T> {
    /// The request device `device` sends on the room's presence for content
    /// `hash` a record of `app_id` in room `cluster_id` references, named
    /// `tag`; `None` when it was asked for within [`WANT_TTL_MS`].
    pub fn ask(
        &mut self,
        device: &[u8; 32],
        tag: T,
        (app_id, cluster_id): (&str, &str),
        hash: &str,
        now_ms: u64,
    ) -> Option<Vec<u8>> {
        let hash = hash.to_ascii_lowercase();
        if self
            .wants
            .get(&hash)
            .is_some_and(|want| now_ms.saturating_sub(want.asked_ms) < WANT_TTL_MS)
        {
            return None;
        }
        if self.wants.len() >= MAX_OPEN_WANTS
            && !self.wants.contains_key(&hash)
            && let Some(oldest) = self
                .wants
                .iter()
                .min_by_key(|(_, want)| want.asked_ms)
                .map(|(hash, _)| hash.clone())
        {
            self.wants.remove(&oldest);
        }
        let request = postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentWant {
            requester: neighbour_wire::peer_handle(device),
            app_id: app_id.to_owned(),
            cluster_id: cluster_id.to_owned(),
            hash: hash.clone(),
        })
        .ok()?;
        self.wants.insert(
            hash,
            Want {
                tag,
                app_id: app_id.to_owned(),
                cluster_id: cluster_id.to_owned(),
                asked_ms: now_ms,
                total: None,
                pieces: BTreeMap::new(),
            },
        );
        Some(request)
    }

    /// One piece of content a member sent device `device`: kept when it
    /// belongs to content this page asked for; the content once whole and
    /// verified.
    pub fn piece(&mut self, device: &[u8; 32], frame: &[u8]) -> Option<Arrived<T>> {
        let Ok(neighbour_wire::NeighbourMsg::RecordContentPiece {
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            offset,
            bytes,
            ..
        }) = postcard::from_bytes(frame)
        else {
            return None;
        };
        let hash = hash.to_ascii_lowercase();
        if requester != neighbour_wire::peer_handle(device)
            || total > MAX_CONTENT_BYTES
            || bytes.len() > PIECE_BYTES
        {
            return None;
        }
        let want = self.wants.get_mut(&hash)?;
        if want.app_id != app_id || want.cluster_id != cluster_id {
            return None;
        }
        if want.total.is_some_and(|known| known != total) {
            return None;
        }
        let fits = u64::try_from(bytes.len())
            .ok()
            .and_then(|len| offset.checked_add(len))
            .is_some_and(|end| end <= total);
        if !fits {
            return None;
        }
        want.total = Some(total);
        want.pieces.entry(offset).or_insert(bytes);
        let assembled = want.assembled()?;
        let want = self.wants.remove(&hash)?;
        (blake3::hash(&assembled).to_hex().as_str() == hash).then_some(Arrived {
            tag: want.tag,
            hash,
            bytes: assembled,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    fn want(total: u64, pieces: &[(u64, &[u8])]) -> Want<()> {
        Want {
            tag: (),
            app_id: "so.tilo.chat".into(),
            cluster_id: String::new(),
            asked_ms: 0,
            total: Some(total),
            pieces: pieces
                .iter()
                .map(|(at, bytes)| (*at, bytes.to_vec()))
                .collect(),
        }
    }

    #[test]
    fn content_is_whole_once_its_pieces_cover_it_in_any_order() {
        assert_eq!(
            want(6, &[(3, b"def"), (0, b"abc")]).assembled().as_deref(),
            Some(&b"abcdef"[..])
        );
        assert_eq!(want(0, &[(0, b"")]).assembled().as_deref(), Some(&b""[..]));
    }

    #[test]
    fn content_with_a_gap_or_short_of_its_total_is_not_whole() {
        assert!(want(9, &[(0, b"abc"), (6, b"ghi")]).assembled().is_none());
        assert!(want(9, &[(0, b"abc"), (3, b"def")]).assembled().is_none());
        assert!(
            Want {
                total: None,
                ..want(3, &[(0, b"abc")])
            }
            .assembled()
            .is_none()
        );
    }

    #[test]
    fn asked_content_comes_back_whole_and_verified_once() {
        let device = [7; 32];
        let bytes = b"a picture someone posted".to_vec();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let mut wants = ContentWants::default();
        let room = ("so.tilo.chat", "room");
        assert!(wants.ask(&device, "general", room, &hash, 0).is_some());
        assert!(
            wants.ask(&device, "general", room, &hash, 1).is_none(),
            "asked once while the request is open"
        );
        let piece = |offset: usize, chunk: &[u8]| {
            postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentPiece {
                responder: neighbour_wire::peer_handle(&[1; 32]),
                requester: neighbour_wire::peer_handle(&device),
                app_id: room.0.into(),
                cluster_id: room.1.into(),
                hash: hash.clone(),
                total: bytes.len() as u64,
                offset: offset as u64,
                bytes: chunk.to_vec(),
            })
            .unwrap()
        };
        assert!(wants.piece(&device, &piece(10, &bytes[10..])).is_none());
        let arrived = wants.piece(&device, &piece(0, &bytes[..10])).unwrap();
        assert_eq!(
            (arrived.tag, arrived.hash.as_str(), arrived.bytes.as_slice()),
            ("general", hash.as_str(), bytes.as_slice())
        );
        assert!(
            wants.piece(&device, &piece(0, &bytes[..10])).is_none(),
            "content arrives once"
        );
    }
}
