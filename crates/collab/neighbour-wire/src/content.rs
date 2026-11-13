//! Content a record references and a device lacks (a picture, by its hash),
//! asked of the workspace on its presence as every device asks. The host and
//! any member keeping it answer the asker alone, in pieces. Only pieces of
//! content the device asked for are kept, and the assembled bytes are handed
//! on only when they hash to what was asked for.
//!
//! Every participant answers such requests ([`answer`]), host or page, from
//! what it keeps: a file a visitor posted from a page is only there until the
//! workspace's host has asked for its own copy, and an app surface is one
//! device's until the workspace has it too.
//!
//! A piece can be lost on the way — an answer that does not fit a session's
//! outbound queue is dropped — so an answer carries at most
//! [`PIECES_PER_ANSWER`] of the pieces the asker lacks, and an asker that
//! stops receiving asks again within [`PIECE_PATIENCE_MS`], naming what it
//! holds so the next answer carries the pieces still missing
//! ([`ContentWants::reask`]). Large content therefore crosses the network in
//! windows that never fill a queue, and one dropped piece costs a re-ask
//! rather than the rest of the transfer.

use std::collections::BTreeMap;

/// Largest content one reference names, as the workspace's hosts keep it.
const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes one piece carries.
pub const PIECE_BYTES: usize = 192 * 1024;
/// How long an unanswered request keeps collecting pieces, and how long
/// before the same content is asked of the workspace again.
const WANT_TTL_MS: u64 = 60_000;
/// Requests open at once; past it, the oldest goes.
const MAX_OPEN_WANTS: usize = 256;
/// How long a piece of content being gathered waits for the next one before
/// the asker asks again: a piece a peer's outbound queue dropped leaves the
/// content short, and asking again within this bound of the last piece that
/// arrived is what completes it — and what brings the next window of a large
/// file once the window before it is in. It bounds a transfer that stopped,
/// so it is long enough for one piece still travelling to arrive, and far
/// shorter than [`WANT_TTL_MS`], which only bounds asking for content that
/// never started arriving.
pub const PIECE_PATIENCE_MS: u64 = 1_000;
/// The longest a re-ask waits between attempts, however long a transfer
/// keeps failing.
const REASK_CAP_MS: u64 = 5_000;
/// Pieces one answer carries: 64 pieces is 12 MiB, a quarter of the 256 frames
/// a session's outbound queue holds, so a burst of content leaves room for
/// the presence, deltas and answers travelling beside it.
pub const PIECES_PER_ANSWER: usize = 64;

/// A run of content bytes: `len` bytes from `offset`. What an asker already
/// holds of what it asks for, so an answer carries only what it lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub offset: u64,
    pub len: u64,
}

/// Whether `spans`, what an asker holds of content, carry the `len` bytes at
/// `offset` in full: what an answer leaves out.
#[must_use]
pub fn holds(spans: &[Span], offset: u64, len: u64) -> bool {
    Span::covering(spans, offset, len).is_some()
}

impl Span {
    const fn end(self) -> u64 {
        self.offset.saturating_add(self.len)
    }

    /// One piece's `len` bytes at `offset`, or `None` for a piece no span
    /// carries in full.
    fn covering(spans: &[Span], offset: u64, len: u64) -> Option<Self> {
        let end = offset.checked_add(len)?;
        spans
            .iter()
            .find(|span| span.offset <= offset && end <= span.end())
            .map(|_| Self {
                offset,
                len: end.saturating_sub(offset),
            })
    }
}

/// Content this device asked a workspace for, and the pieces of it that came.
struct Want<T> {
    /// What the asker names the request by (the record it is for).
    tag: T,
    app_id: String,
    cluster_id: String,
    asked_ms: u64,
    /// When the last piece arrived, and when the last ask went out: what says
    /// whether a transfer stalled and how long to wait before asking again.
    last_piece_ms: u64,
    reasks: u32,
    total: Option<u64>,
    pieces: BTreeMap<u64, Vec<u8>>,
}

impl<T> Want<T> {
    /// Whether the pieces cover the content end to end.
    fn whole(&self) -> bool {
        let Some(total) = self.total else {
            return false;
        };
        let mut end = 0_u64;
        for (offset, piece) in &self.pieces {
            if *offset != end {
                return false;
            }
            end = end.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
        }
        end == total
    }

    /// The whole content once its pieces cover it, end to end.
    fn assembled(&self) -> Option<Vec<u8>> {
        if !self.whole() {
            return None;
        }
        let total = self.total?;
        let mut bytes = Vec::with_capacity(usize::try_from(total).ok()?);
        for piece in self.pieces.values() {
            bytes.extend_from_slice(piece);
        }
        Some(bytes)
    }

    /// What the asker holds of the content, as the runs of bytes that arrived:
    /// the pieces it keeps, each with the offset it starts at.
    fn have(&self) -> Vec<Span> {
        let mut spans: Vec<Span> = Vec::with_capacity(self.pieces.len());
        for (offset, piece) in &self.pieces {
            let len = u64::try_from(piece.len()).unwrap_or(u64::MAX);
            match spans.last_mut() {
                Some(last) if last.end() == *offset => last.len = last.len.saturating_add(len),
                _ => spans.push(Span {
                    offset: *offset,
                    len,
                }),
            }
        }
        spans
    }

    /// How long this want waits for its next piece before asking again: the
    /// first wait is the patience of a transfer that started, and each failed
    /// ask doubles it up to [`REASK_CAP_MS`].
    fn patience_ms(&self) -> u64 {
        let waits = self.reasks.min(4);
        PIECE_PATIENCE_MS
            .saturating_mul(1_u64 << waits)
            .min(REASK_CAP_MS)
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

/// The content requests this device has open, by hash.
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
    /// The request device `device` sends on the workspace's presence for content
    /// `hash` a record of `app_id` in workspace `cluster_id` references, named
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
        let request = postcard::to_allocvec(&crate::NeighbourMsg::RecordContentWant {
            requester: crate::peer_handle(device),
            app_id: app_id.to_owned(),
            cluster_id: cluster_id.to_owned(),
            hash: hash.clone(),
            have: Vec::new(),
        })
        .ok()?;
        self.wants.insert(
            hash,
            Want {
                tag,
                app_id: app_id.to_owned(),
                cluster_id: cluster_id.to_owned(),
                asked_ms: now_ms,
                last_piece_ms: now_ms,
                reasks: 0,
                total: None,
                pieces: BTreeMap::new(),
            },
        );
        Some(request)
    }

    /// The requests device `device` sends the workspace again for the content
    /// it asked for and is still missing pieces of, `now_ms` being the
    /// current time: one per want whose transfer stalled, saying what arrived
    /// so an answer carries only what is missing. A want is asked again once
    /// its [`PIECE_PATIENCE_MS`] of silence has passed, doubling while the
    /// answers do not bring pieces, so content a peer cannot serve costs the
    /// workspace a handful of asks rather than one a second for ever.
    ///
    /// Nothing is asked again for a want past [`WANT_TTL_MS`]: whoever asked
    /// asks afresh by then ([`Self::ask`]). Each ask comes with the tag the
    /// want was named by, so the caller can send it where it belongs.
    pub fn reask(&mut self, device: &[u8; 32], now_ms: u64) -> Vec<(T, Vec<u8>)>
    where
        T: Clone,
    {
        self.wants
            .retain(|_, want| now_ms.saturating_sub(want.asked_ms) < WANT_TTL_MS && !want.whole());
        let mut asks = Vec::new();
        for (hash, want) in &mut self.wants {
            let silent = now_ms.saturating_sub(want.last_piece_ms.max(want.asked_ms));
            if silent < want.patience_ms() {
                continue;
            }
            let Ok(request) = postcard::to_allocvec(&crate::NeighbourMsg::RecordContentWant {
                requester: crate::peer_handle(device),
                app_id: want.app_id.clone(),
                cluster_id: want.cluster_id.clone(),
                hash: hash.clone(),
                have: want.have(),
            }) else {
                continue;
            };
            want.reasks = want.reasks.saturating_add(1);
            asks.push((want.tag.clone(), request));
        }
        asks
    }

    /// Whether `frame` is a piece of content device `device` asked for and
    /// is still collecting: what a workspace's host lets through outside a
    /// member's general content budget, since it asked for it.
    #[must_use]
    pub fn expects(&self, device: &[u8; 32], frame: &[u8]) -> bool {
        let Ok(crate::NeighbourMsg::RecordContentPiece {
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
        requester == crate::peer_handle(device)
            && total <= MAX_CONTENT_BYTES
            && self
                .wants
                .get(&hash.to_ascii_lowercase())
                .is_some_and(|want| want.app_id == app_id && want.cluster_id == cluster_id)
    }

    /// One piece of content a member sent device `device`: kept when it
    /// belongs to content a device asked for; the content once whole and
    /// verified.
    pub fn piece(&mut self, device: &[u8; 32], frame: &[u8], now_ms: u64) -> Option<Arrived<T>> {
        let Ok(crate::NeighbourMsg::RecordContentPiece {
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
        if requester != crate::peer_handle(device)
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
        // A piece the asker did not hold yet means the transfer is moving: the
        // next ask waits the patience of a fresh one rather than a failed
        // ask's. A repeated piece says nothing (another answer was in flight)
        // and leaves the backoff where it is.
        let fresh = !want.pieces.contains_key(&offset);
        want.last_piece_ms = now_ms;
        if fresh {
            want.reasks = 0;
        }
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
/// app, workspace and hash) keeps under it that the asker does not hold yet,
/// or one `RecordContentLacks` when it keeps nothing, so the asker stops
/// counting on this responder. Empty when the request is not one this
/// responder answers. What `read` returns is sent only when it hashes to what
/// was asked for.
///
/// At most [`PIECES_PER_ANSWER`] pieces go in one answer, the first of what
/// the asker lacks: the asker asks again for what is still missing, so a large
/// piece of content crosses a session's outbound queue in windows instead of
/// filling it and losing what it cannot carry.
pub fn answer(
    device: &[u8; 32],
    asker: &[u8; 32],
    request: &[u8],
    read: impl FnOnce(&str, &str, &str) -> Option<Vec<u8>>,
) -> Vec<Vec<u8>> {
    let Ok(crate::NeighbourMsg::RecordContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
        have,
    }) = postcard::from_bytes(request)
    else {
        return Vec::new();
    };
    let hash = hash.to_ascii_lowercase();
    let responder = crate::peer_handle(device);
    let valid = hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
    if requester != crate::peer_handle(asker) || requester == responder || !valid {
        return Vec::new();
    }
    let Some(bytes) = read(&app_id, &cluster_id, &hash).filter(|bytes| {
        u64::try_from(bytes.len()).is_ok_and(|total| total <= MAX_CONTENT_BYTES)
            && blake3::hash(bytes).to_hex().as_str() == hash
    }) else {
        return postcard::to_allocvec(&crate::NeighbourMsg::RecordContentLacks {
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
    let piece = |offset: u64, chunk: &[u8]| {
        postcard::to_allocvec(&crate::NeighbourMsg::RecordContentPiece {
            responder,
            requester,
            app_id: app_id.clone(),
            cluster_id: cluster_id.clone(),
            hash: hash.clone(),
            total,
            offset,
            bytes: chunk.to_vec(),
        })
        .ok()
    };
    if bytes.is_empty() {
        return piece(0, &[]).into_iter().collect();
    }
    let mut pieces = Vec::new();
    for (index, chunk) in bytes.chunks(PIECE_BYTES).enumerate() {
        if pieces.len() >= PIECES_PER_ANSWER {
            break;
        }
        let offset = u64::try_from(index.saturating_mul(PIECE_BYTES)).unwrap_or(u64::MAX);
        let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if holds(&have, offset, len) {
            continue;
        }
        if let Some(frame) = piece(offset, chunk) {
            pieces.push(frame);
        }
    }
    pieces
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
            last_piece_ms: 0,
            reasks: 0,
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
        let workspace = ("so.tilo.chat", "workspace");
        assert!(wants.ask(&device, "general", workspace, &hash, 0).is_some());
        assert!(
            wants.ask(&device, "general", workspace, &hash, 1).is_none(),
            "asked once while the request is open"
        );
        let piece = |offset: usize, chunk: &[u8]| {
            postcard::to_allocvec(&crate::NeighbourMsg::RecordContentPiece {
                responder: crate::peer_handle(&[1; 32]),
                requester: crate::peer_handle(&device),
                app_id: workspace.0.into(),
                cluster_id: workspace.1.into(),
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
        assert!(wants.piece(&device, &piece(10, &bytes[10..]), 0).is_none());
        let arrived = wants.piece(&device, &piece(0, &bytes[..10]), 0).unwrap();
        assert_eq!(
            (arrived.tag, arrived.hash.as_str(), arrived.bytes.as_slice()),
            ("general", hash.as_str(), bytes.as_slice())
        );
        assert!(
            wants.piece(&device, &piece(0, &bytes[..10]), 0).is_none(),
            "content arrives once"
        );
        assert!(
            !wants.expects(&device, &piece(0, &bytes[..10])),
            "nor expected once whole"
        );
    }

    #[test]
    fn a_member_answers_a_request_for_what_it_keeps_in_pieces_the_asker_takes() {
        let (host, page) = ([1; 32], [2; 32]);
        let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(PIECE_BYTES * 2 + 5).collect();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let mut wants = ContentWants::default();
        let workspace = ("so.tilo.chat", "workspace");
        let request = wants.ask(&host, (), workspace, &hash, 0).unwrap();
        let kept = |app: &str, cluster: &str, asked: &str| {
            ((app, cluster) == workspace && asked == hash).then(|| bytes.clone())
        };
        let pieces = answer(&page, &host, &request, kept);
        assert_eq!(pieces.len(), 3, "a piece per {PIECE_BYTES} bytes");
        let arrived = pieces
            .iter()
            .rev()
            .find_map(|piece| wants.piece(&host, piece, 0))
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
                Ok(crate::NeighbourMsg::RecordContentLacks { .. })
            )
        )
    }

    #[test]
    fn a_lost_piece_is_asked_again_for_and_only_that_piece_comes_back() {
        let (host, page) = ([1; 32], [2; 32]);
        let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(PIECE_BYTES * 3 + 7).collect();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let mut wants = ContentWants::default();
        let workspace = ("so.tilo.chat", "workspace");
        let request = wants.ask(&host, (), workspace, &hash, 1_000).unwrap();
        let kept = |app: &str, cluster: &str, asked: &str| {
            ((app, cluster) == workspace && asked == hash).then(|| bytes.clone())
        };
        let pieces = answer(&page, &host, &request, kept);
        assert_eq!(pieces.len(), 4);
        // The second piece is lost on the way: the asker never sees it.
        for (index, piece) in pieces.iter().enumerate() {
            if index != 1 {
                assert!(wants.piece(&host, piece, 1_000).is_none(), "not whole yet");
            }
        }
        assert!(
            wants.reask(&host, 1_000 + PIECE_PATIENCE_MS - 1).is_empty(),
            "a transfer whose pieces just arrived waits its patience"
        );
        let again = wants.reask(&host, 1_000 + PIECE_PATIENCE_MS);
        assert_eq!(again.len(), 1, "the content is asked for again");
        let recovered = answer(&page, &host, &again[0].1, kept);
        assert_eq!(
            recovered.len(),
            1,
            "only the piece the asker lacks comes back"
        );
        let arrived = wants.piece(&host, &recovered[0], 1_000 + PIECE_PATIENCE_MS);
        assert_eq!(
            arrived.map(|arrived| arrived.bytes),
            Some(bytes),
            "the piece that was lost completes it"
        );
    }

    #[test]
    fn content_crosses_in_windows_rather_than_all_at_once() {
        let (host, page) = ([1; 32], [2; 32]);
        let windows = 3;
        let bytes: Vec<u8> = (0..=u8::MAX)
            .cycle()
            .take(PIECE_BYTES * PIECES_PER_ANSWER * windows)
            .collect();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let mut wants = ContentWants::default();
        let workspace = ("so.tilo.chat", "workspace");
        let kept = |app: &str, cluster: &str, asked: &str| {
            ((app, cluster) == workspace && asked == hash).then(|| bytes.clone())
        };
        let mut now_ms = 1_000;
        let mut request = wants.ask(&host, (), workspace, &hash, now_ms).unwrap();
        let mut whole = None;
        let mut answered = 0;
        while whole.is_none() {
            let pieces = answer(&page, &host, &request, kept);
            assert!(
                pieces.len() <= PIECES_PER_ANSWER,
                "an answer never fills a session's outbound queue"
            );
            answered += pieces.len();
            for piece in &pieces {
                whole = wants.piece(&host, piece, now_ms).or(whole);
            }
            now_ms += PIECE_PATIENCE_MS;
            let again = wants.reask(&host, now_ms);
            if whole.is_some() {
                assert!(again.is_empty(), "nothing is asked for once whole");
                break;
            }
            assert_eq!(again.len(), 1, "the next window is asked for");
            request = again[0].1.clone();
        }
        assert_eq!(answered, windows * PIECES_PER_ANSWER, "each piece once");
        assert_eq!(whole.map(|whole| whole.bytes), Some(bytes));
    }
}
