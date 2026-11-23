//! Content a record references and a device lacks (a picture, by its hash),
//! asked of the workspace on its presence as every device asks. The host and
//! any member keeping it answer the asker alone, in pieces. Only pieces of
//! content the device asked for are kept, and the assembled bytes are handed
//! on only when they hash to what was asked for.
//!
//! Every key holder answers such requests ([`answer`]), host or page, from
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
pub const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes one piece carries.
pub const PIECE_BYTES: usize = 192 * 1024;
/// How long an unanswered request keeps collecting pieces, and how long
/// before the same content is asked of the workspace again.
pub const WANT_TTL_MS: u64 = 60_000;
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
/// How long content a device said it lacks waits for that device to hand it
/// over before it is asked again, at first; each silence doubles it up to
/// [`LACKED_CAP_MS`]. Asking again finds a holder whose answer was lost.
pub const LACKED_PATIENCE_MS: u64 = 2_000;
/// The longest content a device said it lacks waits between asks.
const LACKED_CAP_MS: u64 = 30_000;
/// Pieces one answer carries: 64 pieces is 12 MiB, a quarter of the 256 frames
/// a session's outbound queue holds, so a burst of content leaves room for
/// the presence, deltas and answers travelling beside it.
pub const PIECES_PER_ANSWER: usize = 64;

/// How long a want that got nothing waits before it is asked again, after
/// `reasks` asks that brought no piece: the first wait is
/// [`PIECE_PATIENCE_MS`], and each failed ask doubles it up to
/// [`REASK_CAP_MS`]. An asker that waits for content any holder said it
/// lacks asks again after this, naming what it holds, because a holder may
/// keep the bytes a moment later.
#[must_use]
pub fn patience_ms(reasks: u32) -> u64 {
    PIECE_PATIENCE_MS
        .saturating_mul(1_u64 << reasks.min(4))
        .min(REASK_CAP_MS)
}

/// The pieces an answer to an ask carries: at most [`PIECES_PER_ANSWER`] of
/// the content's [`PIECE_BYTES`]-long pieces, in order, leaving out what
/// `have`, the stretches of bytes the asker holds, already covers. A piece is
/// a stretch of `bytes` an answer sends whole.
#[must_use]
pub fn answer_pieces(bytes: &[u8], have: &[Span]) -> Vec<Span> {
    let mut pieces = Vec::new();
    for (index, chunk) in bytes.chunks(PIECE_BYTES).enumerate() {
        if pieces.len() >= PIECES_PER_ANSWER {
            break;
        }
        let offset = u64::try_from(index.saturating_mul(PIECE_BYTES)).unwrap_or(u64::MAX);
        let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if holds(have, offset, len) {
            continue;
        }
        pieces.push(Span { offset, len });
    }
    pieces
}

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
    /// A device asked said it keeps none of the content, and the asker
    /// counts on what was lacking following once a device keeps it
    /// ([`ContentWants::lacks`]): such a want is asked again at once on word
    /// that a holder may have it ([`ContentWants::wake`]), and otherwise only
    /// after a long silence ([`LACKED_PATIENCE_MS`]), in case the holder's
    /// answer was lost.
    lacked: bool,
    /// Woken: asked again by the next [`ContentWants::reask`], however
    /// recently it was asked.
    woken: bool,
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
        patience_ms(self.reasks)
    }

    /// Whether the want went unanswered past [`WANT_TTL_MS`] at `now_ms`:
    /// neither a piece nor word that a device lacks it arrived, so the ask or
    /// its answer was lost on the way.
    fn lapsed(&self, now_ms: u64) -> bool {
        !self.lacked && !self.woken && now_ms.saturating_sub(self.asked_ms) >= WANT_TTL_MS
    }

    /// Whether the want waits for what was lacking to follow: a device said
    /// it keeps none and nothing of it arrived.
    fn waits_for_holder(&self) -> bool {
        self.lacked && self.pieces.is_empty() && !self.woken
    }

    /// How long a silence lasts before this want is asked again: a want a
    /// device said it lacks waits for that device to hand it over, so only a
    /// holder's answer that was lost needs asking again, and that is rare.
    fn silence_ms(&self) -> u64 {
        if self.waits_for_holder() {
            LACKED_PATIENCE_MS
                .saturating_mul(1_u64 << self.reasks.min(4))
                .min(LACKED_CAP_MS)
        } else {
            self.patience_ms()
        }
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
    /// Requests open at once; past it, the oldest goes.
    room: usize,
}

impl<T> Default for ContentWants<T> {
    fn default() -> Self {
        Self::with_room(MAX_OPEN_WANTS)
    }
}

impl<T> ContentWants<T> {
    /// Requests that keep at most `room` open at once: an asker that asks
    /// for a whole tree's content at once keeps room for all of it, so an
    /// answer that follows later still finds its request.
    #[must_use]
    pub fn with_room(room: usize) -> Self {
        Self {
            wants: BTreeMap::new(),
            room: room.max(1),
        }
    }

    /// Whether one more request fits without the oldest going: an asker that
    /// must not lose a request it made asks for more only while this holds.
    #[must_use]
    pub fn has_room(&self) -> bool {
        self.wants.len() < self.room
    }

    /// Take the requests that went unanswered past [`WANT_TTL_MS`] at
    /// `now_ms`, their tags: the ask or its answer was lost on the way, and
    /// [`Self::reask`] drops them too, so an asker that still needs the
    /// content asks afresh.
    pub fn lapsed(&mut self, now_ms: u64) -> Vec<T> {
        let (lapsed, open): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.wants)
            .into_iter()
            .partition(|(_, want)| want.lapsed(now_ms) && !want.whole());
        self.wants = open;
        lapsed.into_values().map(|want| want.tag).collect()
    }

    /// The request device `device` sends on the workspace's presence for content
    /// `hash` a record of `app_id` in workspace `cluster_id` references, named
    /// `tag`; `None` when it was asked for within `WANT_TTL_MS`, or when a
    /// device said it lacks it and the asker waits for it to follow
    /// ([`Self::lacks`]).
    pub fn ask(
        &mut self,
        device: &[u8; 32],
        tag: T,
        (app_id, cluster_id): (&str, &str),
        hash: &str,
        now_ms: u64,
    ) -> Option<Vec<u8>> {
        let hash = hash.to_ascii_lowercase();
        if self.wants.get(&hash).is_some_and(|want| {
            now_ms.saturating_sub(want.asked_ms) < WANT_TTL_MS || want.waits_for_holder()
        }) {
            return None;
        }
        if self.wants.len() >= self.room
            && !self.wants.contains_key(&hash)
            && let Some(oldest) = self
                .wants
                .iter()
                .min_by_key(|(_, want)| want.asked_ms)
                .map(|(hash, _)| hash.clone())
        {
            self.wants.remove(&oldest);
        }
        let request = postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentWant {
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
                lacked: false,
                woken: false,
            },
        );
        Some(request)
    }

    /// Take `frame` when it is word, addressed to device `device`, that the
    /// responder keeps none of content this device asked for; whether it
    /// was. The device that said so hands the content over once it keeps
    /// it, so from here on the want is asked again at once only when
    /// [`Self::wake`] says a holder may have it, and otherwise after a long
    /// silence ([`LACKED_PATIENCE_MS`]). Only an asker whose responders hand
    /// over what they lacked takes these.
    pub fn lacks(&mut self, device: &[u8; 32], frame: &[u8]) -> bool {
        let Some(hash) = self.lacking(device, frame) else {
            return false;
        };
        if let Some(want) = self.wants.get_mut(&hash) {
            want.lacked = true;
        }
        true
    }

    /// Whether `frame` is word, addressed to device `device`, that the
    /// responder keeps none of content this device asked for
    /// ([`Self::lacks`]).
    #[must_use]
    pub fn awaits(&self, device: &[u8; 32], frame: &[u8]) -> bool {
        self.lacking(device, frame).is_some()
    }

    /// The hash of the want `frame`, word addressed to `device` that the
    /// responder lacks it, is about.
    fn lacking(&self, device: &[u8; 32], frame: &[u8]) -> Option<String> {
        let Ok(crate::NeighbourMsg::DocumentContentLacks {
            requester,
            app_id,
            cluster_id,
            hash,
            ..
        }) = crate::NeighbourMsg::decode(frame)
        else {
            return None;
        };
        let hash = hash.to_ascii_lowercase();
        (requester == crate::peer_handle(device)
            && self
                .wants
                .get(&hash)
                .is_some_and(|want| want.app_id == app_id && want.cluster_id == cluster_id))
        .then_some(hash)
    }

    /// Ask again for what the devices of workspace `cluster_id` said they
    /// lack, through the next [`Self::reask`]s: a device that may keep it
    /// was reached, or joined. Each want is asked once more, not on a clock.
    pub fn wake(&mut self, cluster_id: &str) {
        for want in self.wants.values_mut() {
            if want.cluster_id == cluster_id && want.lacked && want.pieces.is_empty() {
                want.woken = true;
            }
        }
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
        self.reask_at_most(device, now_ms, usize::MAX)
    }

    /// [`Self::reask`], `limit` asks at most: what is left is asked by the
    /// next call, so a burst of wants woken at once ([`Self::wake`]) goes
    /// out at a pace the links carry.
    ///
    /// A want a device said it lacks waits for that device to hand it over
    /// ([`Self::lacks`]): it is not dropped by the clock, and asked again
    /// only after a long silence, or once woken.
    pub fn reask_at_most(
        &mut self,
        device: &[u8; 32],
        now_ms: u64,
        limit: usize,
    ) -> Vec<(T, Vec<u8>)>
    where
        T: Clone,
    {
        self.wants
            .retain(|_, want| !want.lapsed(now_ms) && !want.whole());
        let mut asks = Vec::new();
        for (hash, want) in &mut self.wants {
            if asks.len() >= limit {
                break;
            }
            let silent = now_ms.saturating_sub(want.last_piece_ms.max(want.asked_ms));
            if !want.woken && silent < want.silence_ms() {
                continue;
            }
            let Ok(request) = postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentWant {
                requester: crate::peer_handle(device),
                app_id: want.app_id.clone(),
                cluster_id: want.cluster_id.clone(),
                hash: hash.clone(),
                have: want.have(),
            }) else {
                continue;
            };
            if want.woken {
                // Asked afresh, of a holder that may have it now; the devices
                // that said they lack it still owe it.
                want.woken = false;
                want.reasks = 0;
                want.asked_ms = now_ms;
                want.last_piece_ms = now_ms;
            } else {
                // The silence counts from this ask: without it, a want silent
                // past its patience is asked again on every call.
                want.reasks = want.reasks.saturating_add(1);
                want.last_piece_ms = now_ms;
            }
            asks.push((want.tag.clone(), request));
        }
        asks
    }

    /// Whether `frame` is a piece of content device `device` asked for and
    /// is still collecting: what a workspace's host takes as the answer to
    /// its own request.
    #[must_use]
    pub fn expects(&self, device: &[u8; 32], frame: &[u8]) -> bool {
        let Ok(crate::NeighbourMsg::DocumentContentPiece {
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            ..
        }) = crate::NeighbourMsg::decode(frame)
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
        let Ok(crate::NeighbourMsg::DocumentContentPiece {
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            offset,
            bytes,
            ..
        }) = crate::NeighbourMsg::decode(frame)
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
        // Pieces arriving say the transfer moves: it is asked again for its
        // next window by the clock, whoever said they lack it.
        want.lacked = false;
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

/// The frames that answer `request`, a member's `DocumentContentWant` that
/// device `asker` published, from device `device`, addressed to `asker`: the
/// `NeighbourMsg::DocumentContentPiece`s of what `read` (given the request's
/// app, workspace and hash) keeps under it that the asker does not hold yet,
/// or one `DocumentContentLacks` when it keeps nothing, so the asker stops
/// counting on this responder. A request that names another device than the
/// one that sent it is answered with one `DocumentContentRefused`, addressed
/// to the sender: the ask can never be answered as addressed, and the device
/// that asked logs it instead of waiting on this responder for ever. Empty
/// when the request is not one this responder answers. What `read` returns is
/// sent only when it hashes to what was asked for.
///
/// `device` may be the asker: a server's own agents join its workspaces as the
/// server's device, with nothing kept from before it started, and the server's
/// store is where they get what its history names. A device answering requests
/// other members fanned out to it leaves its own aside itself.
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
    let Ok(crate::NeighbourMsg::DocumentContentWant {
        requester,
        app_id,
        cluster_id,
        hash,
        have,
    }) = crate::NeighbourMsg::decode(request)
    else {
        return Vec::new();
    };
    let hash = hash.to_ascii_lowercase();
    let responder = crate::peer_handle(device);
    let sender = crate::peer_handle(asker);
    if requester != sender {
        return postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentRefused {
            responder,
            requester: sender,
            app_id,
            cluster_id,
            hash,
        })
        .into_iter()
        .collect();
    }
    let valid = hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid {
        return Vec::new();
    }
    let Some(bytes) = read(&app_id, &cluster_id, &hash).filter(|bytes| {
        u64::try_from(bytes.len()).is_ok_and(|total| total <= MAX_CONTENT_BYTES)
            && blake3::hash(bytes).to_hex().as_str() == hash
    }) else {
        return postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentLacks {
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
        postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentPiece {
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
    answer_pieces(&bytes, &have)
        .into_iter()
        .filter_map(|span| {
            let start = usize::try_from(span.offset).ok()?;
            let len = usize::try_from(span.len).ok()?;
            let chunk = bytes.get(start..start.checked_add(len)?)?;
            piece(span.offset, chunk)
        })
        .collect()
}

#[cfg(test)]
mod tests {

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
            lacked: false,
            woken: false,
        }
    }

    /// What `assembled` gives, by the name of the test each row was: what is wanted with the
    /// pieces that came, and the whole content once they cover its total (none before that).
    #[test]
    fn wanted_content_is_whole_only_once_its_pieces_cover_its_total() {
        let cases = [
            (
                "content_is_whole_once_its_pieces_cover_it_in_any_order",
                vec![
                    (want(6, &[(3, b"def"), (0, b"abc")]), Some(&b"abcdef"[..])),
                    (want(0, &[(0, b"")]), Some(&b""[..])),
                ],
            ),
            (
                "content_with_a_gap_or_short_of_its_total_is_not_whole",
                vec![
                    (want(9, &[(0, b"abc"), (6, b"ghi")]), None),
                    (want(9, &[(0, b"abc"), (3, b"def")]), None),
                    (
                        Want {
                            total: None,
                            ..want(3, &[(0, b"abc")])
                        },
                        None,
                    ),
                ],
            ),
        ];
        for (case, wants) in cases {
            for (row, (wanted, whole)) in wants.into_iter().enumerate() {
                assert_eq!(wanted.assembled().as_deref(), whole, "{case}: row {row}");
            }
        }
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
            postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentPiece {
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
            refused(
                &answer(&page, &[3; 32], &request, kept),
                &page,
                &[3; 32],
                &hash
            ),
            "a request naming another device is refused to the one that sent it"
        );
        assert_eq!(
            answer(&host, &host, &request, kept).len(),
            3,
            "a server's store answers its own device, which keeps none of it"
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
                Ok(crate::NeighbourMsg::DocumentContentLacks { .. })
            )
        )
    }

    /// Whether `frames` is one refusal that `responder` refuses the ask
    /// `asker` sent for `hash`, addressed to the one that sent it.
    fn refused(frames: &[Vec<u8>], responder: &[u8; 32], asker: &[u8; 32], hash: &str) -> bool {
        matches!(
            frames,
            [frame] if matches!(
                postcard::from_bytes(frame),
                Ok(crate::NeighbourMsg::DocumentContentRefused {
                    responder: refused_by,
                    requester,
                    hash: refused_hash,
                    ..
                }) if refused_by == crate::peer_handle(responder)
                    && requester == crate::peer_handle(asker)
                    && refused_hash == hash
            )
        )
    }

    #[test]
    fn a_request_that_names_another_device_is_refused_visibly() {
        let (named, page, stranger) = ([1; 32], [2; 32], [3; 32]);
        let bytes: Vec<u8> = (0..=u8::MAX).cycle().take(PIECE_BYTES + 7).collect();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let workspace = ("so.tilo.chat", "workspace");
        // The ask was made as `named` would ask it, but `page` sent it: no
        // answer can go where the ask names, so the one that sent it hears
        // why and stops waiting.
        let request = postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentWant {
            requester: crate::peer_handle(&named),
            app_id: workspace.0.into(),
            cluster_id: workspace.1.into(),
            hash: hash.clone(),
            have: Vec::new(),
        })
        .unwrap();
        let kept = |_: &str, _: &str, _: &str| Some(bytes.clone());
        let frames = answer(&page, &stranger, &request, kept);
        assert!(
            refused(&frames, &page, &stranger, &hash),
            "the refusal names the device that sent the ask and is addressed to it"
        );
        assert!(
            answer(&page, &stranger, b"not a request", kept).is_empty(),
            "a frame that is no ask is nothing to refuse"
        );
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
        let windows = 2;
        let bytes = vec![0x83; PIECE_BYTES * PIECES_PER_ANSWER * windows];
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

    /// Asking again is spaced by the silence since the last ask, however long the content takes:
    /// a host looking every 100 ms asks a want it got no word on every few seconds, and one a
    /// device said it lacks (and owes it) every half minute at most, until word that a holder may
    /// have it asks each once more, paced. What it lacks is held until it arrives, so the
    /// device that owed it finds the request still open when it hands it over.
    #[test]
    fn content_is_asked_for_again_after_a_silence_not_on_every_look() {
        let (host, member) = ([1; 32], [2; 32]);
        let workspace = ("so.tilo.files", "workspace");
        let mut wants = ContentWants::default();
        let bytes = b"a file written a moment ago".to_vec();
        let lacked = blake3::hash(&bytes).to_hex().to_string();
        let silent = blake3::hash(b"nobody answered").to_hex().to_string();
        let mut now_ms = 1_000;
        let ask = wants.ask(&host, (), workspace, &lacked, now_ms).unwrap();
        wants.ask(&host, (), workspace, &silent, now_ms).unwrap();
        let lacking = answer(&member, &host, &ask, |_, _, _| None);
        assert!(wants.lacks(&host, &lacking[0]));
        assert!(
            !wants.lacks(
                &host,
                &answer(
                    &member,
                    &[3; 32],
                    &ask_frame(&[3; 32], workspace, &lacked),
                    |_, _, _| None
                )[0]
            ),
            "word addressed to another device is not this one's"
        );
        let mut asked: BTreeMap<String, usize> = BTreeMap::new();
        let looks = 1_200;
        for _ in 0..looks {
            now_ms += 100;
            for ((), again) in wants.reask(&host, now_ms) {
                let Ok(crate::NeighbourMsg::DocumentContentWant { hash, .. }) =
                    crate::NeighbourMsg::decode(&again)
                else {
                    panic!("an ask");
                };
                *asked.entry(hash).or_default() += 1;
            }
        }
        let (lacked_asks, silent_asks) = (asked.get(&lacked).copied(), asked.get(&silent).copied());
        assert!(
            lacked_asks.is_some_and(|asks| (1..=8).contains(&asks)),
            "in {looks} looks, content a device lacks was asked again {lacked_asks:?} times"
        );
        assert!(
            silent_asks.is_some_and(|asks| (1..=(WANT_TTL_MS / REASK_CAP_MS) as usize + 4).contains(&asks)),
            "in {looks} looks, content nobody answered was asked again {silent_asks:?} times"
        );
        assert!(
            wants.ask(&host, (), workspace, &lacked, now_ms).is_none(),
            "reading the record again asks nothing while the device owes it"
        );
        wants.wake("another workspace");
        assert!(
            wants.reask(&host, now_ms).is_empty(),
            "word from another workspace wakes none"
        );
        wants.wake(workspace.1);
        assert_eq!(
            wants.reask(&host, now_ms).len(),
            1,
            "asked once more at once"
        );
        assert!(wants.reask(&host, now_ms).is_empty(), "and only once");
        let repaid = answer(&member, &host, &ask, |_, _, _| Some(bytes.clone()));
        let arrived = wants
            .piece(&host, &repaid[0], now_ms)
            .map(|arrived| arrived.bytes);
        assert_eq!(
            arrived,
            Some(bytes),
            "what was owed still finds its request"
        );
    }

    /// An ask whose answer was lost hands its request back once it lapses, so the asker that
    /// still needs the content asks afresh; content a device said it lacks waits for that device
    /// instead, and an asker short of room asks for no more than it keeps open.
    #[test]
    fn an_unanswered_request_lapses_back_to_its_asker() {
        let (host, member) = ([1; 32], [2; 32]);
        let workspace = ("so.tilo.files", "workspace");
        let mut wants = ContentWants::with_room(3);
        let hash = |name: &str| blake3::hash(name.as_bytes()).to_hex().to_string();
        let now_ms = 1_000;
        wants
            .ask(&host, "lost", workspace, &hash("lost"), now_ms)
            .unwrap();
        let owed = wants
            .ask(&host, "owed", workspace, &hash("owed"), now_ms)
            .unwrap();
        assert!(wants.lacks(&host, &answer(&member, &host, &owed, |_, _, _| None)[0]));
        assert!(wants.has_room(), "two of three requests open");
        wants
            .ask(&host, "late", workspace, &hash("late"), now_ms + 30_000)
            .unwrap();
        assert!(!wants.has_room(), "three of three requests open");
        assert!(
            wants.lapsed(now_ms + WANT_TTL_MS - 1).is_empty(),
            "nothing lapses within {WANT_TTL_MS} ms"
        );
        assert_eq!(
            wants.lapsed(now_ms + WANT_TTL_MS),
            ["lost"],
            "only the request nothing answered lapses"
        );
        assert!(wants.has_room(), "a lapsed request leaves room");
        assert!(
            wants
                .ask(
                    &host,
                    "lost",
                    workspace,
                    &hash("lost"),
                    now_ms + WANT_TTL_MS
                )
                .is_some(),
            "and is asked afresh"
        );
    }

    fn ask_frame(device: &[u8; 32], (app_id, cluster_id): (&str, &str), hash: &str) -> Vec<u8> {
        postcard::to_allocvec(&crate::NeighbourMsg::DocumentContentWant {
            requester: crate::peer_handle(device),
            app_id: app_id.into(),
            cluster_id: cluster_id.into(),
            hash: hash.into(),
            have: Vec::new(),
        })
        .unwrap()
    }

    /// A holder reached wakes everything its workspace's devices said they lack at once, which
    /// for a tree is thousands of blobs: each look asks at most its share, and every one is asked
    /// by the looks that follow.
    #[test]
    fn woken_content_is_asked_for_at_a_pace() {
        let (host, member) = ([1; 32], [2; 32]);
        let workspace = ("so.tilo.files", "workspace");
        let mut wants = ContentWants::default();
        let now_ms = 1_000;
        let blobs = 200_u32;
        for blob in 0..blobs {
            let hash = blake3::hash(&blob.to_le_bytes()).to_hex().to_string();
            let ask = wants.ask(&host, (), workspace, &hash, now_ms).unwrap();
            assert!(wants.lacks(&host, &answer(&member, &host, &ask, |_, _, _| None)[0]));
        }
        wants.wake(workspace.1);
        let mut looks = Vec::new();
        loop {
            let asked = wants.reask_at_most(&host, now_ms, 64).len();
            if asked == 0 {
                break;
            }
            looks.push(asked);
        }
        assert_eq!(looks, [64, 64, 64, 8], "{blobs} woken blobs, 64 a look");
    }
}
