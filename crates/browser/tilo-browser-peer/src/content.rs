//! Content a record references and this page lacks (a picture, by its
//! hash), asked of the room on its presence as every device asks. The host
//! and any member keeping it answer this device alone, in pieces. Only
//! pieces of content this page asked for are kept, and the assembled bytes
//! are handed on only when they hash to what was asked for.
//!
//! A page answers such requests too ([`answer`]), from what it keeps: a file
//! a visitor posted from this page is only here until the room's host has
//! asked for its own copy.

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
    /// `tag`; `None` when it was asked for within `WANT_TTL_MS`.
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

    /// Whether `frame` is a piece of content device `device` asked for and
    /// is still collecting: what a room's host lets through outside a
    /// member's general content budget, since it asked for it.
    #[must_use]
    pub fn expects(&self, device: &[u8; 32], frame: &[u8]) -> bool {
        let Ok(neighbour_wire::NeighbourMsg::RecordContentPiece {
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            ..
        }) = postcard::from_bytes(frame)
        else {
            return false;
        };
        requester == neighbour_wire::peer_handle(device)
            && total <= MAX_CONTENT_BYTES
            && self
                .wants
                .get(&hash.to_ascii_lowercase())
                .is_some_and(|want| want.app_id == app_id && want.cluster_id == cluster_id)
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

/// The frames that answer `request`, a member's `RecordContentWant` that
/// device `asker` published, from device `device`, addressed to `asker`: the
/// `NeighbourMsg::RecordContentPiece`s of what `read` (given the request's
/// app, room and hash) keeps under it, or one `RecordContentLacks` when it
/// keeps nothing, so the asker stops counting on this page. Empty when the
/// request is not one this page answers. What `read` returns is sent only
/// when it hashes to what was asked for.
pub fn answer(
    device: &[u8; 32],
    asker: &[u8; 32],
    request: &[u8],
    read: impl FnOnce(&str, &str, &str) -> Option<Vec<u8>>,
) -> Vec<Vec<u8>> {
    let Ok(neighbour_wire::NeighbourMsg::RecordContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
    }) = postcard::from_bytes(request)
    else {
        return Vec::new();
    };
    let hash = hash.to_ascii_lowercase();
    let responder = neighbour_wire::peer_handle(device);
    let valid = hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
    if requester != neighbour_wire::peer_handle(asker) || requester == responder || !valid {
        return Vec::new();
    }
    let Some(bytes) = read(&app_id, &cluster_id, &hash).filter(|bytes| {
        u64::try_from(bytes.len()).is_ok_and(|total| total <= MAX_CONTENT_BYTES)
            && blake3::hash(bytes).to_hex().as_str() == hash
    }) else {
        return postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentLacks {
            responder,
            requester,
            app_id,
            cluster_id,
            hash,
        })
        .into_iter()
        .collect();
    };
    let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let piece = |offset: usize, chunk: &[u8]| {
        postcard::to_allocvec(&neighbour_wire::NeighbourMsg::RecordContentPiece {
            responder,
            requester,
            app_id: app_id.clone(),
            cluster_id: cluster_id.clone(),
            hash: hash.clone(),
            total,
            offset: u64::try_from(offset).unwrap_or(u64::MAX),
            bytes: chunk.to_vec(),
        })
        .ok()
    };
    if bytes.is_empty() {
        return piece(0, &[]).into_iter().collect();
    }
    bytes
        .chunks(PIECE_BYTES)
        .enumerate()
        .filter_map(|(index, chunk)| piece(index.saturating_mul(PIECE_BYTES), chunk))
        .collect()
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
        assert!(
            wants.expects(&device, &piece(0, &bytes[..10])),
            "what it asked for"
        );
        assert!(
            !wants.expects(&[8; 32], &piece(0, &bytes[..10])),
            "not what another device asked for"
        );
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
        assert!(
            !wants.expects(&device, &piece(0, &bytes[..10])),
            "nor expected once whole"
        );
    }

    #[test]
    fn a_page_answers_a_request_for_what_it_keeps_in_pieces_the_asker_takes() {
        let (host, page) = ([1; 32], [2; 32]);
        let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(PIECE_BYTES * 2 + 5).collect();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let mut wants = ContentWants::default();
        let room = ("so.tilo.chat", "room");
        let request = wants.ask(&host, (), room, &hash, 0).unwrap();
        let kept = |app: &str, cluster: &str, asked: &str| {
            ((app, cluster) == room && asked == hash).then(|| bytes.clone())
        };
        let pieces = answer(&page, &host, &request, kept);
        assert_eq!(pieces.len(), 3, "a piece per {PIECE_BYTES} bytes");
        let arrived = pieces
            .iter()
            .rev()
            .find_map(|piece| wants.piece(&host, piece))
            .unwrap();
        assert_eq!(arrived.bytes, bytes, "the asker assembles and verifies it");

        assert!(
            answer(&page, &[3; 32], &request, kept).is_empty(),
            "only the device that asked is answered"
        );
        assert!(
            answer(&host, &host, &request, kept).is_empty(),
            "a device never answers itself"
        );
        assert!(
            lacks(&answer(&page, &host, &request, |_, _, _| Some(
                b"other".to_vec()
            ))),
            "bytes under another address are never sent"
        );
        assert!(lacks(&answer(&page, &host, &request, |_, _, _| None)));
    }

    /// Whether `frames` is one word that the page keeps none of it.
    fn lacks(frames: &[Vec<u8>]) -> bool {
        matches!(
            frames,
            [frame] if matches!(
                postcard::from_bytes(frame),
                Ok(neighbour_wire::NeighbourMsg::RecordContentLacks { .. })
            )
        )
    }
}
