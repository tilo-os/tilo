//! Moving a document's content between peers, and noticing when it has not
//! moved.
//!
//! Documents hold a reference to their binaries rather than the bytes (see
//! `qdoc::blobstore`), which is what keeps a photograph from becoming a CRDT
//! change too large for the transport. The bytes still have to reach the other
//! machine, and this is what carries them.
//!
//! # Why chunking is safe here and was not safe for changes
//!
//! The obvious repair for "a frame is too big" is to split it. That is the
//! wrong answer for a CRDT change and the right one for a blob, and the
//! difference is worth stating because it is the difference between the bug
//! and the system:
//!
//! - A **change** is ordered and ancestor-linked. Losing one chunk of it means
//!   losing the change, and a lost change is undetectable from the far side —
//!   Automerge queues every later change naming it, and the document simply
//!   stops, forever, with nothing to read that says so.
//! - A **blob** is self-verifying: the key *is* the blake3 hash of the content,
//!   so a missing or corrupted chunk is detectable by construction, the
//!   transfer is retryable, and the worst case degrades one image to a
//!   placeholder while the document itself stays live and converged.
//!
//! So the same mechanism that would have hidden the defect one order of
//! magnitude further out is fine once the thing being chunked can prove itself.
//!
//! # What is deliberately not here
//!
//! A direct peer-to-peer stream. `crate::network::surface_stream` looks like
//! the natural carrier, and its own doc comment recommends it, but it is
//! framing plus an in-process **drop-oldest** queue with no wire binding —
//! `flatpak_share` states plainly that there is no dedicated substream, and
//! nothing drains the sender to a socket. Routing content onto it would put
//! the bytes in a lossy queue nobody empties: the same silent loss at a new
//! address. Drop-oldest is also exactly the wrong policy for content that a
//! document is referencing.

use std::collections::{HashMap, HashSet};

use automerge::AutoCommit;

/// Payload budget for one chunk, in bytes.
///
/// The same figure the gossip snapshot cap uses, for the same reason and with
/// the same arithmetic behind it: a `NeighbourMsg` is encoded with `serde_json`,
/// which writes a `Vec<u8>` as an array of decimal numbers at up to four
/// characters per byte, against gossipsub's 64 KiB RPC ceiling. 14 KiB of
/// payload expands to at most 56 KiB, leaving room for the envelope — the room
/// uuid, the hash, the indices, and gossipsub's own protobuf framing.
pub const CHUNK_PAYLOAD_BYTES: usize = 14 * 1024;

/// Largest blob this relay will carry or accept.
///
/// A ceiling is needed on the receive side regardless of what senders do: the
/// chunk count arrives from the wire, and a peer claiming a very large one
/// would otherwise have this process allocate against it. 8 MiB is comfortably
/// above a photograph and far below anything that threatens the session.
pub const MAX_BLOB_BYTES: usize = 8 * 1024 * 1024;

/// Most partially-received blobs to hold at once, across all peers.
const MAX_PARTIALS: usize = 16;

/// How long to wait for an answer before asking the room again.
///
/// Short enough that a picture missed by a mesh that was still forming appears
/// within a beat rather than a minute; long enough that a 146 KB transfer
/// already in flight completes before a duplicate is asked for.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_millis(1500);

/// How many times to ask before declaring the content unobtainable and saying so.
const MAX_ASKS: u32 = 5;

/// Most per-record mirrors to keep. Mirrors are small now that content lives
/// outside the document, but the count still wants a bound.
const MAX_MIRRORS: usize = 64;

/// What observing a document frame revealed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// Referenced content this machine holds and can therefore serve.
    pub have: Vec<String>,
    /// Referenced content this machine does not hold. A renderer shows these
    /// as placeholders; this is the knowable state that replaced silence.
    pub missing: Vec<String>,
    /// Ancestor changes this record is waiting on.
    ///
    /// Non-zero means the document has a hole: Automerge is queueing changes it
    /// cannot apply, and the record reads as the page *before* the gap however
    /// many later changes have arrived. This is the one honest way to tell
    /// "there is nothing more" from "there is more, and I was never sent what
    /// it stands on".
    pub missing_ancestors: usize,
}

/// A blob being reassembled from chunks.
struct Partial {
    count: u32,
    received: HashMap<u32, Vec<u8>>,
    bytes: usize,
}

/// Per-room content relay: tracks what each shared document refers to, what
/// this machine holds, and reassembles what arrives.
///
/// The store is *held* rather than looked up per call. `qdoc::blobstore`'s is
/// a process-wide singleton, and a singleton cannot express the one thing this
/// code exists to get right: two peers are two stores. Holding a handle is
/// what lets a test stand two machines up in one process and actually prove a
/// transfer, rather than have both ends read the same map and prove nothing.
pub struct BlobRelay {
    /// What this machine holds. Defaults to the process-wide store.
    store: std::sync::Arc<dyn qdoc::blobstore::BlobStore>,
    /// A replica of each shared document, fed by every frame this shell sends
    /// or receives for it. Only used to answer two questions the raw frame
    /// bytes cannot: what content does this document reference, and is its
    /// history whole.
    ///
    /// Keyed by `(cluster, app, record)` — the whole address, not part of it.
    /// It was keyed by `(cluster, record)`, and an application announces the
    /// same empty record until it adopts records, so one replica was fed the
    /// word processor's document, the file manager's, the chat's and the photo
    /// editor's at once. They all build the same schema at the same root, so
    /// their containers collide; Automerge keeps every one and returns a
    /// winner, and the losers' contents are simply not there to be read.
    ///
    /// What that costs is the whole job of this replica. Content referenced by
    /// a document whose containers lost the merge is never noticed, so it is
    /// never offered to the room and never asked for — the picture stays a
    /// placeholder on every machine, and nothing anywhere reports a reason.
    /// Which document wins is decided by op id, so it is also a race nobody
    /// chose.
    mirrors: HashMap<(String, String, String), AutoCommit>,
    /// Content already offered to a room, so a document that is edited a
    /// hundred times does not re-send its photograph a hundred times.
    announced: HashSet<(String, String)>,
    /// Content this machine has asked a room for and not yet received, with
    /// when it last asked and how many times. Kept so a request can be
    /// repeated — the first ask can be missed for reasons the asker cannot
    /// see — and so an ask that is never answered can be *reported* instead of
    /// leaving a placeholder that silently never resolves.
    wanted: HashMap<(String, String), Want>,
    partials: HashMap<(u64, String), Partial>,
}

/// An outstanding request for content.
struct Want {
    asked_at: std::time::Instant,
    /// When the want was first raised, so an unanswered one can say how long
    /// it has gone unanswered rather than just that it is outstanding.
    since: std::time::Instant,
    attempts: u32,
    /// Whether the "nobody answered" report has already been made, so it is
    /// said once per request rather than on every sweep.
    reported: bool,
}

impl Default for BlobRelay {
    fn default() -> Self {
        Self::with_store(qdoc::blobstore::store())
    }
}

impl BlobRelay {
    pub fn new() -> Self {
        Self::default()
    }

    /// A relay backed by `store` rather than the process-wide one.
    pub fn with_store(store: std::sync::Arc<dyn qdoc::blobstore::BlobStore>) -> Self {
        Self {
            store,
            mirrors: HashMap::new(),
            announced: HashSet::new(),
            wanted: HashMap::new(),
            partials: HashMap::new(),
        }
    }

    /// What this relay reads and writes.
    pub fn store(&self) -> std::sync::Arc<dyn qdoc::blobstore::BlobStore> {
        std::sync::Arc::clone(&self.store)
    }

    /// Fold a document frame into the mirror for `record` and report what it
    /// implies about content and history.
    ///
    /// Called for frames in both directions: an outbound frame is what tells
    /// this side it now holds content worth offering, and an inbound one is
    /// what tells it something is referenced that has not arrived.
    pub fn observe_frame(
        &mut self,
        cluster: &str,
        app: &str,
        record: &str,
        frame: &[u8],
    ) -> Observation {
        let key = (cluster.to_string(), app.to_string(), record.to_string());
        if !self.mirrors.contains_key(&key) && self.mirrors.len() >= MAX_MIRRORS {
            // Refusing to grow without bound is correct, but it does mean this
            // record's content is not tracked, so say so rather than behave as
            // though the document referenced nothing.
            tracing::warn!(
                room = %cluster, app = %app, record = %record, tracked = self.mirrors.len(),
                "blob_relay: too many shared records to track; content for this one \
                 will not be offered or requested",
            );
            return Observation::default();
        }
        let mirror = self.mirrors.entry(key).or_default();
        // A frame that will not apply is not a reason to stop: it is queued,
        // which is precisely the condition `missing_ancestors` reports.
        if let Err(e) = mirror.load_incremental(frame) {
            tracing::warn!(
                room = %cluster, app = %app, record = %record, error = %e,
                "blob_relay: a document frame did not parse; it is lost for this replica",
            );
            return Observation::default();
        }

        let missing_ancestors = mirror.get_missing_deps(&[]).len();
        let mut obs = Observation { missing_ancestors, ..Default::default() };

        let c = match qdoc::model::open(mirror, &automerge::ROOT) {
            Ok(c) => c,
            // Not a qdoc document — nothing to say about its content.
            Err(_) => return obs,
        };
        for hash in qdoc::model::blob_hashes(mirror, &c) {
            if self.store.has(&hash) {
                obs.have.push(hash);
            } else {
                obs.missing.push(hash);
            }
        }
        obs
    }

    /// Whether this room has already been offered `hash`.
    pub fn already_announced(&self, cluster: &str, hash: &str) -> bool {
        self.announced.contains(&(cluster.to_string(), hash.to_string()))
    }

    /// Mark `hash` as offered to `cluster`.
    pub fn mark_announced(&mut self, cluster: &str, hash: &str) {
        self.announced.insert((cluster.to_string(), hash.to_string()));
    }

    /// Whether to ask `cluster` for `hash` right now.
    ///
    /// True the first time, and again once `RETRY_AFTER` has passed with no
    /// answer, up to [`MAX_ASKS`]. The retry is the point: the first ask can be
    /// lost to a mesh that has not finished forming, and without a second one
    /// a picture that was merely early stays missing forever.
    ///
    /// Giving up after a bounded number of tries is deliberate too — an ask
    /// nobody in the room can answer must stop costing the room traffic, and
    /// must be *said*, which is what [`Self::unanswered`] is for.
    pub fn should_ask(&mut self, cluster: &str, hash: &str) -> bool {
        let key = (cluster.to_string(), hash.to_string());
        let now = std::time::Instant::now();
        match self.wanted.get_mut(&key) {
            None => {
                self.wanted.insert(
                    key,
                    Want { asked_at: now, since: now, attempts: 1, reported: false },
                );
                true
            }
            Some(w) => {
                if w.attempts >= MAX_ASKS || now.duration_since(w.asked_at) < RETRY_AFTER {
                    return false;
                }
                w.asked_at = now;
                w.attempts += 1;
                true
            }
        }
    }

    /// Forget any outstanding request for `hash` — the content is here.
    pub fn note_arrived(&mut self, hash: &str) {
        self.wanted.retain(|(_, h), _| h != hash);
    }

    /// Requests that have run out of attempts and are still unanswered, as
    /// `(room, hash, seconds waiting)`. Reported once each.
    ///
    /// This exists because the defect this whole area came from was a failure
    /// that said nothing. A fetch that nobody answers is a picture that will
    /// not appear, and the log is the only place that can say so before a
    /// person notices a grey box.
    pub fn unanswered(&mut self) -> Vec<(String, String, u64)> {
        let now = std::time::Instant::now();
        let mut out = Vec::new();
        for ((room, hash), w) in self.wanted.iter_mut() {
            if w.reported || w.attempts < MAX_ASKS {
                continue;
            }
            if now.duration_since(w.asked_at) < RETRY_AFTER {
                continue;
            }
            w.reported = true;
            out.push((room.clone(), hash.clone(), now.duration_since(w.since).as_secs()));
        }
        out
    }

    /// Whether this machine holds `hash` and can therefore answer for it.
    pub fn holds(&self, hash: &str) -> bool {
        self.store.has(hash)
    }

    /// Store content that arrived, and clear any request for it.
    ///
    /// Returns false when the store refused it, which is a real failure with a
    /// user-visible consequence, so callers report it rather than dropping it.
    pub fn store_arrived(&mut self, hash: &str, bytes: &[u8]) -> bool {
        let ok = self.store.put(hash, bytes);
        if ok {
            self.note_arrived(hash);
        }
        ok
    }

    /// Split `bytes` into publishable chunks.
    ///
    /// `None` when the content is larger than this relay will carry — the
    /// caller reports that rather than sending a transfer that cannot complete.
    pub fn split(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
        if bytes.len() > MAX_BLOB_BYTES {
            return None;
        }
        if bytes.is_empty() {
            return Some(vec![Vec::new()]);
        }
        Some(bytes.chunks(CHUNK_PAYLOAD_BYTES).map(|c| c.to_vec()).collect())
    }

    /// Take one received chunk. Returns the whole content once the last piece
    /// lands and it verifies against `hash`.
    ///
    /// Verification is the reason chunking is acceptable for content at all:
    /// bytes that do not hash to the name they arrived under are rejected here,
    /// so a truncated, reordered or tampered transfer cannot enter the store.
    pub fn accept_chunk(
        &mut self,
        peer: u64,
        hash: &str,
        index: u32,
        count: u32,
        data: Vec<u8>,
    ) -> Option<Vec<u8>> {
        if count == 0 || index >= count {
            return None;
        }
        // The count comes from the wire; bound the allocation it can imply.
        if count as usize > MAX_BLOB_BYTES.div_ceil(CHUNK_PAYLOAD_BYTES) {
            tracing::warn!(peer, hash, count, "blob_relay: refusing an implausible chunk count");
            return None;
        }
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) || hash.len() < 4 {
            return None;
        }

        let key = (peer, hash.to_string());
        if !self.partials.contains_key(&key) && self.partials.len() >= MAX_PARTIALS {
            // Drop the transfer holding the fewest pieces: it is the one that
            // loses least, and holding all of them unbounded is not an option.
            if let Some(victim) = self
                .partials
                .iter()
                .min_by_key(|(_, p)| p.received.len())
                .map(|(k, _)| k.clone())
            {
                tracing::warn!(
                    hash = %victim.1,
                    "blob_relay: too many transfers in flight; abandoning one. \
                     Its image stays a placeholder until the content is offered again",
                );
                self.partials.remove(&victim);
            }
        }

        let entry = self.partials.entry(key.clone()).or_insert_with(|| Partial {
            count,
            received: HashMap::new(),
            bytes: 0,
        });
        if entry.count != count {
            // The sender changed its mind about the shape of this transfer;
            // the pieces cannot be combined.
            return None;
        }
        if entry.bytes + data.len() > MAX_BLOB_BYTES {
            self.partials.remove(&key);
            tracing::warn!(peer, hash, "blob_relay: transfer exceeded the size ceiling; dropped");
            return None;
        }
        entry.bytes += data.len();
        entry.received.insert(index, data);
        if entry.received.len() < count as usize {
            return None;
        }

        let Some(part) = self.partials.remove(&key) else {
            return None;
        };
        let mut whole = Vec::with_capacity(part.bytes);
        for i in 0..count {
            match part.received.get(&i) {
                Some(c) => whole.extend_from_slice(c),
                None => return None,
            }
        }
        if !qdoc::blobstore::verify(hash, &whole) {
            tracing::warn!(
                peer, hash,
                bytes = whole.len(),
                "blob_relay: reassembled content does not match the hash it arrived under; \
                 discarded rather than stored",
            );
            return None;
        }
        Some(whole)
    }

    /// Transfers currently part-received, for diagnostics.
    pub fn in_flight(&self) -> usize {
        self.partials.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use qdoc::model::{self, DocMut, IdGen};

    fn doc_with_blob(bytes: &[u8]) -> (Vec<u8>, String) {
        let mut doc = AutoCommit::new();
        let c = model::init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let hash = {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let h = dm.put_blob(bytes);
            let img = dm.insert_block(None, "image");
            dm.set_attr(&img, "blob", h.as_str());
            h
        };
        (doc.save_incremental(), hash)
    }

    #[test]
    fn a_frame_reveals_the_content_it_references() {
        let (frame, hash) = doc_with_blob(b"the content of a picture");
        let mut relay = BlobRelay::new();
        let obs = relay.observe_frame("room", "dev.qos.word-processor", "rec", &frame);
        assert_eq!(obs.have, vec![hash], "the authoring side holds what it just stored");
        assert!(obs.missing.is_empty());
        assert_eq!(obs.missing_ancestors, 0, "a complete frame leaves no hole");
    }

    #[test]
    fn a_transfer_round_trips_through_chunks() {
        // Large enough to need several chunks, so reassembly is actually
        // exercised rather than trivially satisfied by a single piece.
        let content: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let hash = qdoc::blobstore::hash_bytes(&content);
        let chunks = BlobRelay::split(&content).expect("within the ceiling");
        assert!(chunks.len() > 1, "the fixture must span more than one chunk");

        let mut relay = BlobRelay::new();
        let count = chunks.len() as u32;
        let mut out = None;
        for (i, c) in chunks.into_iter().enumerate() {
            out = relay.accept_chunk(7, &hash, i as u32, count, c);
        }
        assert_eq!(out.as_deref(), Some(content.as_slice()), "the content reassembles exactly");
        assert_eq!(relay.in_flight(), 0, "a completed transfer is not left holding memory");
    }

    #[test]
    fn chunks_that_do_not_add_up_to_their_hash_are_refused() {
        // This is what makes chunking safe for content: a transfer that was
        // truncated, reordered or tampered with cannot enter the store, because
        // the key is the content's own hash.
        let content: Vec<u8> = (0..30_000u32).map(|i| (i % 97) as u8).collect();
        let honest = qdoc::blobstore::hash_bytes(&content);
        let chunks = BlobRelay::split(&content).unwrap();
        let count = chunks.len() as u32;

        let mut relay = BlobRelay::new();
        let mut out = None;
        for (i, mut c) in chunks.into_iter().enumerate() {
            if i == 0 {
                c[0] ^= 0xFF; // one flipped byte, anywhere
            }
            out = relay.accept_chunk(7, &honest, i as u32, count, c);
        }
        assert!(out.is_none(), "content that does not match its name must not be accepted");
    }

    #[test]
    fn content_above_the_ceiling_is_refused_rather_than_half_sent() {
        let huge = vec![0u8; MAX_BLOB_BYTES + 1];
        assert!(
            BlobRelay::split(&huge).is_none(),
            "a transfer that cannot complete must be refused up front, so the caller \
             can say so rather than starting one that stalls"
        );
    }

    #[test]
    fn an_implausible_chunk_count_is_refused() {
        let mut relay = BlobRelay::new();
        let h = qdoc::blobstore::hash_bytes(b"x");
        assert!(relay.accept_chunk(1, &h, 0, u32::MAX, vec![0; 8]).is_none());
        assert_eq!(relay.in_flight(), 0, "a refused transfer allocates nothing");
    }

    /// A replica that is missing an ancestor must be able to say so.
    ///
    /// This is the state the original defect left every peer in, undetectably:
    /// the document reads as the page before the gap, with later changes queued
    /// behind it and nothing to indicate they exist.
    #[test]
    fn a_hole_in_a_records_history_is_detectable() {
        let mut doc = AutoCommit::new();
        let c = model::init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let first = {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "before");
            p
        };
        let f1 = doc.save_incremental();
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let p = dm.insert_block(Some(&first), "paragraph");
            dm.splice_text(&p, 0, 0, "the change that never arrives");
        }
        let _withheld = doc.save_incremental();
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "after");
        }
        let f3 = doc.save_incremental();

        let mut relay = BlobRelay::new();
        assert_eq!(relay.observe_frame("room", "dev.qos.word-processor", "rec", &f1).missing_ancestors, 0);
        // Deliver the third frame without the second, exactly as a dropped
        // publish would have.
        let obs = relay.observe_frame("room", "dev.qos.word-processor", "rec", &f3);
        assert!(
            obs.missing_ancestors > 0,
            "a record missing a change in the middle must report it, not read as complete"
        );
    }

    /// One application's picture must not be hidden by another application
    /// sharing its room.
    ///
    /// An application announces the same empty record until it adopts records,
    /// so a room routinely carries the word processor, the file manager, the
    /// chat and the photo editor all under `record=""`. Keyed on the room and
    /// the record alone, every one of those documents was folded into a single
    /// replica — and they all build the same schema at the same root, so their
    /// containers collide. Automerge keeps every one and hands back a winner,
    /// which means the blobs map of one document wins and the others are
    /// simply not there to be read.
    ///
    /// The consequence is silent and total for the losers: content they
    /// reference is never noticed, so it is never offered to the room and
    /// never asked for, and the picture stays a placeholder on every machine
    /// with nothing anywhere reporting a reason. This asserts both directions,
    /// because which container wins a merge is decided by op id — a race
    /// nobody chose, and one that would let a single-direction test pass on
    /// half the runs.
    #[test]
    fn one_applications_picture_is_not_hidden_by_another_sharing_the_room() {
        let (page, page_picture) = doc_with_blob(b"the photograph on the page");
        let (message, message_picture) = doc_with_blob(b"a picture posted in the chat");
        assert_ne!(page_picture, message_picture);

        for (first, first_hash, second, second_hash) in [
            (
                ("dev.qos.word-processor", &page),
                &page_picture,
                ("dev.qos.chat", &message),
                &message_picture,
            ),
            (
                ("dev.qos.chat", &message),
                &message_picture,
                ("dev.qos.word-processor", &page),
                &page_picture,
            ),
        ] {
            let mut relay = BlobRelay::new();
            let (first_app, first_frame) = first;
            let (second_app, second_frame) = second;

            let obs = relay.observe_frame("room", first_app, "", first_frame);
            assert!(
                obs.have.contains(first_hash),
                "{first_app} referenced a picture this machine holds and it was \
                 not noticed",
            );

            let obs = relay.observe_frame("room", second_app, "", second_frame);
            assert!(
                obs.have.contains(second_hash),
                "{second_app}'s picture went unnoticed because {first_app} is \
                 in the same room: content nobody notices is never offered and \
                 never asked for, and the placeholder never resolves",
            );
            assert!(
                !obs.have.contains(first_hash),
                "{second_app}'s document was credited with {first_app}'s \
                 picture, so the two are being read as one document",
            );
        }
    }
}
