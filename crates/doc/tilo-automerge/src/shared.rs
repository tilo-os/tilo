//! A large document a reader on this thread already holds, handed to the next reader of the
//! same bytes as a copy instead of loaded again.
//!
//! Loading a document rebuilds and hashes every change it holds: seconds for a tracker of
//! thousands of issues on a page. A page reads such a record several times as it opens it —
//! its workspace replica captures it as bytes, the window showing it loads those, the activity
//! reads the list of records the same document keeps — and each of them loaded the same bytes
//! whole. Whoever saved or loaded the bytes holds the document they make, so it is kept here
//! by the bytes' digest, and the next load of exactly those bytes is a copy of it: the same
//! changes, heads and values, under an actor of its own, as a load would give.
//!
//! Only documents that load whole are kept, so a reader that would have refused a torn tail
//! never receives one, and only documents large enough to be slow to load. What is kept is
//! bounded, and the least recently used goes first.

use std::cell::RefCell;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, AutomergeError};

/// Below this a document loads in a few milliseconds and is not kept.
pub const MIN_BYTES: usize = 64 * 1024;

/// Most saved bytes the documents kept on one thread come to.
const MAX_BYTES: usize = 32 * 1024 * 1024;

/// Most documents kept on one thread.
const MAX_DOCUMENTS: usize = 8;

thread_local! {
    static HELD: RefCell<Held> = const { RefCell::new(Held { kept: Vec::new(), uses: 0 }) };
}

struct Held {
    kept: Vec<Kept>,
    uses: u64,
}

struct Kept {
    len: usize,
    digest: [u8; 32],
    doc: AutoCommit,
    used: u64,
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Keep `doc` as the document `bytes` load to: `bytes` are what it saved, or what it was
/// loaded from whole. A document with changes not yet committed is not kept, nor one too
/// small to be slow to load.
pub fn hold(bytes: &[u8], doc: &AutoCommit) {
    if bytes.len() < MIN_BYTES || bytes.len() > MAX_BYTES || doc.pending_ops() > 0 {
        return;
    }
    let digest = digest(bytes);
    // Handed out as a fork: no transaction, no patches, an actor of its own.
    let kept = doc.clone();
    HELD.with_borrow_mut(|held| {
        held.uses = held.uses.wrapping_add(1);
        let used = held.uses;
        held.kept
            .retain(|other| !(other.len == bytes.len() && other.digest == digest));
        held.kept.push(Kept {
            len: bytes.len(),
            digest,
            doc: kept,
            used,
        });
        while held.kept.len() > MAX_DOCUMENTS
            || held.kept.iter().map(|kept| kept.len).sum::<usize>() > MAX_BYTES
        {
            let Some(oldest) = held
                .kept
                .iter()
                .enumerate()
                .min_by_key(|(_, kept)| kept.used)
                .map(|(at, _)| at)
            else {
                break;
            };
            held.kept.swap_remove(oldest);
        }
    });
}

/// The document `bytes` load to, when a reader on this thread holds it: a copy under an actor
/// of its own, as a load of them would give.
#[must_use]
pub fn held(bytes: &[u8]) -> Option<AutoCommit> {
    if bytes.len() < MIN_BYTES || bytes.len() > MAX_BYTES {
        return None;
    }
    let digest = digest(bytes);
    HELD.with_borrow_mut(|held| {
        held.uses = held.uses.wrapping_add(1);
        let used = held.uses;
        let kept = held
            .kept
            .iter_mut()
            .find(|kept| kept.len == bytes.len() && kept.digest == digest)?;
        kept.used = used;
        Some(kept.doc.fork())
    })
}

/// The document `bytes` hold, as [`AutoCommit::load`] reads them: a copy of the one a reader
/// on this thread holds, else loaded and kept for the next reader.
///
/// # Errors
/// As [`AutoCommit::load`].
pub fn load(bytes: &[u8]) -> Result<AutoCommit, AutomergeError> {
    if let Some(doc) = held(bytes) {
        return Ok(doc);
    }
    let doc = AutoCommit::load(bytes)?;
    hold(bytes, &doc);
    Ok(doc)
}

/// How many documents are kept on this thread.
#[must_use]
pub fn kept() -> usize {
    HELD.with_borrow(|held| held.kept.len())
}

/// Let every document kept on this thread go.
pub fn clear() {
    HELD.with_borrow_mut(|held| held.kept.clear());
}
