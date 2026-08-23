//! A photograph in a shared document must not desynchronise the room.
//!
//! # The defect these tests pin
//!
//! One session dropped a 146 KB photograph into a shared document. `put_blob`
//! hashed it and then stored the whole payload *inline* in the CRDT, so the
//! image became a 146 KB automerge change — and a change is what the sync path
//! carries. The actor serialises a `NeighbourMsg` with `serde_json`, which
//! writes a `Vec<u8>` as an array of decimal numbers (up to four characters per
//! byte), and gossipsub caps one RPC at 64 KiB. The publish was rejected by the
//! transport and discarded by a `let _ =`.
//!
//! What made that catastrophic rather than merely lossy: **Automerge queues a
//! change whose ancestors it lacks rather than applying it.** Every keystroke
//! after the image named the image's change as an ancestor, so all of them sat
//! unapplied forever. The far replicas froze at the last line before the image
//! — no error, no warning — while both ends still reported themselves
//! connected. A replica in that state also spreads it: a snapshot saved from it
//! carries the queued changes along.
//!
//! The fix is not a bigger pipe. A document has no upper bound, so any ceiling
//! only moves where it breaks. The fix is that the bytes are not in the
//! document at all: it holds a reference (blake3 hash) plus the metadata a
//! renderer needs before the content arrives, and the bytes live in a
//! content-addressed store — the shape `fs/src/hydration.rs` already uses.
//!
//! # Why these tests model the transport instead of standing up libp2p
//!
//! `deliverable` below is not a stand-in for the transport's behaviour, it *is*
//! the transport's behaviour, reproduced from the two lines that produce it:
//! `serde_json::to_vec(&msg)` followed by a `publish` whose error is dropped
//! (`shell/src/network/actor.rs`, the `NetCmd::Publish` arm), against
//! gossipsub's default `max_transmit_size`. Modelling it keeps the test
//! deterministic and fast while still failing for exactly the production
//! reason.

use automerge::{AutoCommit, ROOT};
use qdoc::model::{self, DocMut, IdGen};
use qdoc::snapshot::AttrVal;
use shell::network::protocol::NeighbourMsg;

/// libp2p-gossipsub's default maximum RPC size. `NiriosBehaviour::new` does not
/// raise it, so this is the real ceiling a publish is measured against.
const GOSSIPSUB_MAX_TRANSMIT: usize = 65_536;

/// The size of the photograph in the incident this test exists because of.
const PHOTOGRAPH_BYTES: usize = 146_492;

// ---- a genuinely well-formed PNG of a realistic size -----------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for b in bytes {
        c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// A structurally valid PNG of `total` bytes declaring `w` x `h`.
///
/// The bulk rides in an ancillary `tEXt` chunk of incompressible-looking bytes.
/// Built with real chunk lengths and real CRCs rather than shaped to satisfy
/// this crate's own header reader — a fixture that only fools the code under
/// test proves nothing about the code under test.
fn large_png(w: u32, h: u32, total: usize) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace

    let mut png = Vec::with_capacity(total);
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));

    let end = chunk(b"IEND", &[]);
    let fill_len = total.saturating_sub(png.len() + end.len() + 12);
    let mut fill = Vec::with_capacity(fill_len);
    // A cheap PRNG: bytes that do not collapse under compression, so the
    // fixture's size on the wire is its size on disk.
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for _ in 0..fill_len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        fill.push((x >> 33) as u8);
    }
    png.extend_from_slice(&chunk(b"tEXt", &fill));
    png.extend_from_slice(&end);
    png
}

// ---- the transport, reproduced --------------------------------------------

/// Whether this frame would actually reach a peer.
///
/// Mirrors `shell/src/network/actor.rs`'s `NetCmd::Publish` arm exactly: encode
/// the `NeighbourMsg` with `serde_json`, hand it to gossipsub, and — because
/// the publish result is discarded there — get no signal when it does not fit.
fn deliverable(delta: &[u8]) -> bool {
    let msg = NeighbourMsg::AppCrdtIncremental {
        peer: u64::MAX,
        app_id: "dev.qos.word-processor".to_string(),
        cluster_id: "6b3f9a10-0000-4000-8000-00000000000c".to_string(),
        record_id: "file:/work/shared/notes.qdoc".to_string(),
        data: delta.to_vec(),
    };
    serde_json::to_vec(&msg).map(|b| b.len() <= GOSSIPSUB_MAX_TRANSMIT).unwrap_or(false)
}

// ---- the document under test ----------------------------------------------

struct Session {
    doc: AutoCommit,
    c: model::Containers,
    ids: IdGen,
}

impl Session {
    fn new() -> Self {
        let mut doc = AutoCommit::new();
        let c = model::init(&mut doc, &ROOT);
        let ids = IdGen::new(doc.get_actor().to_hex_string());
        Self { doc, c, ids }
    }

    fn edit<R>(&mut self, f: impl FnOnce(&mut DocMut<'_>) -> R) -> R {
        let mut dm = DocMut { doc: &mut self.doc, c: &self.c, ids: &mut self.ids };
        f(&mut dm)
    }

    /// The changes since the last call — what the broker publishes per edit.
    fn delta(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }

}

/// Build a document the way the word processor does: a line, a photograph, then
/// more typing after it. Returns the deltas in the order they were produced.
fn author_a_document_with_a_photograph() -> (Vec<Vec<u8>>, String) {
    let photo = large_png(1200, 800, PHOTOGRAPH_BYTES);
    assert_eq!(photo.len(), PHOTOGRAPH_BYTES, "fixture must be the size the incident was");

    let mut s = Session::new();
    let mut deltas = Vec::new();

    let first = s.edit(|dm| {
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, "Before the photograph.");
        p
    });
    deltas.push(s.delta());

    // The photograph. This is the change that was 146 KB and never arrived.
    let hash = s.edit(|dm| {
        let hash = dm.put_blob(&photo);
        let img = dm.insert_block(Some(&first), "image");
        dm.set_attr(&img, "blob", hash.as_str());
        hash
    });
    deltas.push(s.delta());

    // Everything typed after it. Each of these names the image's change as an
    // ancestor, which is why losing one frame lost all of them.
    for i in 0..25 {
        s.edit(|dm| {
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, &format!("Line {i} after the photograph."));
        });
        deltas.push(s.delta());
    }

    (deltas, hash)
}

// ---- the tests -------------------------------------------------------------

/// Every frame a document produces must fit the carrier that has to move it.
///
/// This is the assertion the old code cannot satisfy: with the bytes inline,
/// the photograph's delta is ~146 KB, which `serde_json` expands to roughly
/// half a megabyte, against a 64 KiB ceiling.
#[test]
fn every_frame_of_a_document_with_a_photograph_fits_one_gossip_message() {
    let (deltas, _) = author_a_document_with_a_photograph();

    let undeliverable: Vec<usize> = deltas
        .iter()
        .enumerate()
        .filter(|(_, d)| !deliverable(d))
        .map(|(i, _)| i)
        .collect();

    assert!(
        undeliverable.is_empty(),
        "frames {undeliverable:?} of {} cannot be published: a document's own content \
         must not exceed the transport that carries it. Largest frame was {} bytes.",
        deltas.len(),
        deltas.iter().map(|d| d.len()).max().unwrap_or(0),
    );
}

/// The whole product symptom, end to end: the far replica converges, and the
/// typing that happened *after* the image is on it.
///
/// Against the old code this fails twice over — the far replica is provably
/// missing an ancestor, and its text stops at the line before the photograph
/// even though 25 later frames arrived.
#[test]
fn a_far_replica_converges_and_keeps_receiving_after_a_photograph() {
    let (deltas, _) = author_a_document_with_a_photograph();

    // The far session. It receives what the transport will actually carry —
    // which today silently excludes the photograph's frame.
    let mut far = AutoCommit::new();
    let mut delivered = 0usize;
    for d in &deltas {
        if deliverable(d) {
            far.load_incremental(d).expect("a delivered frame applies");
            delivered += 1;
        }
    }

    let missing = far.get_missing_deps(&[]);
    assert!(
        missing.is_empty(),
        "the far replica is missing {} ancestor(s) having received {delivered} of {} \
         frames: every later change naming one of them stays queued and unapplied, and \
         nothing about reading the document afterwards says so — it simply materialises \
         as the page before the gap",
        missing.len(),
        deltas.len(),
    );

    let c = model::open(&far, &ROOT).expect("the far replica carries the document schema");
    let text = model::materialize(&far, &c)
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("Before the photograph."),
        "the far replica must have the line before the image, got: {text:?}"
    );
    // The real symptom: the document froze at the last line *before* the image.
    assert!(
        text.contains("Line 0 after the photograph."),
        "typing after the photograph must keep arriving — this is the line the far \
         session never saw. Got: {text:?}"
    );
    assert!(
        text.contains("Line 24 after the photograph."),
        "the far replica must be current, not frozen part-way. Got: {text:?}"
    );
}

/// A referenced image whose bytes have not arrived is a *knowable* state, and
/// the document says how much room to leave for it.
///
/// Without the intrinsic size the renderer falls back to a fixed guess and
/// every paragraph after the image moves when the bytes land — on a live shared
/// document, while someone is typing into it.
#[test]
fn an_image_that_has_not_arrived_still_says_how_big_it_is() {
    let photo = large_png(1200, 800, PHOTOGRAPH_BYTES);
    let mut s = Session::new();
    let hash = s.edit(|dm| {
        let hash = dm.put_blob(&photo);
        let img = dm.insert_block(None, "image");
        dm.set_attr(&img, "blob", hash.as_str());
        hash
    });

    // A peer that has the document but not the content: the reference synced,
    // the bytes did not.
    let far_doc = {
        let mut far = AutoCommit::new();
        far.load_incremental(&s.doc.save()).expect("the document applies");
        far
    };
    let c = model::open(&far_doc, &ROOT).expect("schema present");
    let snap = model::materialize(&far_doc, &c);

    let img = snap.blocks.iter().find(|b| b.kind == "image").expect("the image block synced");
    assert_eq!(
        img.attrs.get("blob"),
        Some(&AttrVal::Str(hash.clone())),
        "the reference itself must survive the sync"
    );
    assert_eq!(
        img.attrs.get("blob_w"),
        Some(&AttrVal::Int(1200)),
        "the intrinsic width must be readable without the bytes: {:?}",
        img.attrs
    );
    assert_eq!(
        img.attrs.get("blob_h"),
        Some(&AttrVal::Int(800)),
        "the intrinsic height must be readable without the bytes"
    );
    assert_eq!(
        img.attrs.get("blob_bytes"),
        Some(&AttrVal::Int(PHOTOGRAPH_BYTES as i64)),
        "the size must be readable so a placeholder can say what is coming"
    );
    assert_eq!(
        img.attrs.get("blob_mime"),
        Some(&AttrVal::Str("image/png".to_string())),
        "the media type must be readable without the bytes"
    );
}
