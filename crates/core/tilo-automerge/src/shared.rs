//! A large document a reader on this thread already holds, handed to the next reader of it as a
//! copy instead of loaded again.
//!
//! Loading a document rebuilds and hashes every change it holds: seconds for a tracker of
//! thousands of issues on a page. A page reads such a record several times as it opens it —
//! its workspace replica captures it as bytes, the window showing it loads those, the activity
//! reads the list of documents the same one keeps — and each of them loaded the same document
//! whole. Whoever saved or loaded it holds it here, and the next reader takes a copy: the same
//! changes, heads and values, under an actor of its own, as a load would give.
//!
//! A document is found by the bytes it was kept from ([`held`]) and by its heads ([`held_at`]):
//! the same document saved twice is two byte strings, and a reader that knows which heads the
//! stored bytes stand at ([`load_hint`](crate::load_hint)) takes the copy without comparing
//! bytes at all.
//!
//! Only documents that load whole are kept, so a reader that would have refused a torn tail
//! never receives one, and only documents large enough to be slow to load. What is kept is
//! bounded by the bytes it was kept from, and the least recently used goes first. It is never
//! bounded by how many documents there are: a document kept in parts (a tracker of thousands of
//! issues is a dozen of them) is read part after part by each reader in turn, and a bound below
//! the number of parts let each reader's pass push out the parts the next reader was about to
//! read, so every reader loaded every part again.

use std::cell::{Cell, RefCell};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, AutomergeError, ChangeHash, LoadOptions};

/// Below this a document loads in a few milliseconds and is not kept.
pub const MIN_BYTES: usize = 64 * 1024;

/// Most saved bytes the documents kept on one thread come to.
const MAX_BYTES: usize = 32 * 1024 * 1024;

thread_local! {
    static HELD: RefCell<Held> = const { RefCell::new(Held { kept: Vec::new(), uses: 0 }) };
    /// How many documents this thread loaded from bytes ([`note_decoded`]), so a test can count
    /// what opening a document costs.
    static DECODED: Cell<usize> = const { Cell::new(0) };
}

struct Held {
    kept: Vec<Kept>,
    uses: u64,
}

struct Kept {
    len: usize,
    /// The bytes it was kept from, or none for one kept by its heads alone ([`hold_at`]).
    digest: Option<[u8; 32]>,
    /// The heads it stands at, sorted: the same document saved twice is found by them.
    heads: Vec<ChangeHash>,
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
    if bytes.len() < MIN_BYTES {
        return;
    }
    keep(bytes.len(), Some(digest(bytes)), doc);
}

/// Keep `doc` for the next reader that asks for it by its heads ([`held_at`]), counted as
/// `len` bytes: a reader that brought it up to date by changes it read (a page's replica
/// opening the writes that landed), for the window it hands those changes to, which takes a
/// copy instead of applying them again.
pub fn hold_at(len: usize, doc: &AutoCommit) {
    keep(len, None, doc);
}

fn keep(len: usize, digest: Option<[u8; 32]>, doc: &AutoCommit) {
    if len > MAX_BYTES || doc.pending_ops() > 0 {
        return;
    }
    let mut heads = doc.committed_heads();
    heads.sort_unstable();
    // Handed out as a fork: no transaction, no patches, an actor of its own.
    let kept = doc.clone();
    HELD.with_borrow_mut(|held| {
        held.uses = held.uses.wrapping_add(1);
        let used = held.uses;
        held.kept.retain(|other| match digest {
            Some(_) => !(other.len == len && other.digest == digest),
            None => other.heads != heads,
        });
        held.kept.push(Kept {
            len,
            digest,
            heads,
            doc: kept,
            used,
        });
        while held.kept.iter().map(|kept| kept.len).sum::<usize>() > MAX_BYTES {
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
            .find(|kept| kept.len == bytes.len() && kept.digest == Some(digest))?;
        kept.used = used;
        Some(kept.doc.fork())
    })
}

/// The document at `heads`, when a reader on this thread holds it: a copy under an
/// actor of its own, as a load of its bytes would give. A document is found by its heads when
/// the caller knows them without loading the bytes ([`crate::load_hint`]).
#[must_use]
pub fn held_at(heads: &[ChangeHash]) -> Option<AutoCommit> {
    if heads.is_empty() {
        return None;
    }
    let mut heads = heads.to_vec();
    heads.sort_unstable();
    HELD.with_borrow_mut(|held| {
        held.uses = held.uses.wrapping_add(1);
        let used = held.uses;
        let kept = held.kept.iter_mut().find(|kept| kept.heads == heads)?;
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
    load_hinted(bytes, None)
}

/// [`load`], given the load hint kept for exactly `bytes` ([`crate::load_hint`]): the change
/// hashes it names are taken instead of being rebuilt from the document, which is most of the
/// cost of loading a large one. A hint made for other bytes, or none at all, loads them as
/// [`load`] does.
///
/// # Errors
/// As [`AutoCommit::load`].
pub fn load_hinted(bytes: &[u8], hint: Option<&[u8]>) -> Result<AutoCommit, AutomergeError> {
    if let Some(doc) = held(bytes) {
        return Ok(doc);
    }
    note_decoded();
    let known = hint.and_then(|hint| crate::load_hint::hashes_for(hint, bytes));
    let doc = match known {
        Some(hashes) => {
            AutoCommit::load_with_options(bytes, LoadOptions::new().known_change_hashes(&hashes))?
        }
        None => AutoCommit::load(bytes)?,
    };
    hold(bytes, &doc);
    Ok(doc)
}

/// Note that a document was loaded from bytes on this thread: what [`decoded`] counts, so a
/// test can say how many times opening a document loaded it.
pub fn note_decoded() {
    DECODED.with(|decoded| decoded.set(decoded.get().saturating_add(1)));
}

/// How many documents this thread loaded from bytes since [`clear`].
#[must_use]
pub fn decoded() -> usize {
    DECODED.with(Cell::get)
}

/// How many documents are kept on this thread.
#[must_use]
pub fn kept() -> usize {
    HELD.with_borrow(|held| held.kept.len())
}

/// Let every document kept on this thread go, and forget the loads counted since the last
/// one.
pub fn clear() {
    HELD.with_borrow_mut(|held| held.kept.clear());
    DECODED.with(|decoded| decoded.set(0));
}
