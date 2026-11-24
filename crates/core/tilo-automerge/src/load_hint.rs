//! What a saved document holds, kept beside it so loading it again needs neither to rebuild and
//! hash every change nor to find the reader who holds it by comparing the saved bytes.
//!
//! A compacted Automerge document stores each change's metadata but not its hash, so a plain
//! load re-encodes every change from the op set and hashes it: most of the cost of opening a
//! large document. The writer has those hashes in memory when it saves, so it makes a hint for
//! exactly the bytes it saved, and hands it out with them; the next load passes the hint's
//! hashes to [`LoadOptions::known_change_hashes`](automerge::LoadOptions::known_change_hashes).
//! The same save knows the document's heads, and a reader that holds the document they name
//! ([`crate::shared::held_at`]: a page's replica that captured the document, a window showing it)
//! hands over a copy instead of a load.
//!
//! A hint is trusted like the document bytes it names, and only for them:
//! - its change hashes are used for the document chunk they were made for, named by BLAKE3
//!   digest, so they are used for those bytes or not at all (changes appended after the chunk
//!   keep them valid; a replacement makes them stale and ignored);
//! - its heads are used only for exactly the bytes they were written for, named by length and
//!   BLAKE3 digest of the whole stored value, so an append or a replacement makes them stale and
//!   ignored: a copy of a document that is not those bytes' is never handed out;
//! - only a holder of the workspace's own store makes one, when it saves a document it rebuilt
//!   from writes whose changes it verified as it admitted them;
//! - it travels with the bytes it names, from this device's own daemon, which is the store they
//!   come from: a peer never sends one and nothing syncs it, so changes from peers are always
//!   rebuilt and verified in full.
//!
//! Automerge still checks a hash list is one distinct hash per change whose dependency-free tips
//! are the document's heads, and ignores it otherwise.

use automerge::ChangeHash;

/// Format tag; a hint in any other format is ignored.
const MAGIC: &[u8; 8] = b"TILOCH02";

/// A Blake3 digest's length.
const DIGEST: usize = 32;

/// An Automerge change hash's length.
const HASH: usize = 32;

/// The stored value's length as the hint names it, in bytes.
const LEN: usize = 8;

/// A count of hashes or heads, in bytes.
const COUNT: usize = 4;

/// Below this a document loads in about a millisecond, and the hint (32
/// bytes per change) would not pay for itself beside the document...
pub const MIN_HINTED_BYTES: usize = 64 * 1024;

/// ...unless it has at least this many changes. Loading costs per change as
/// well as per byte, and a document of many small changes compresses to
/// about a byte per change: the desktop document, one change per workspace
/// switch, is 37 KB at 33,000 changes and takes 25 ms to load, 8 ms with a
/// hint (M5 Max). At this count a load takes about 3 ms.
pub const MIN_HINTED_CHANGES: usize = 4096;

/// The hint for `saved` (bytes [`automerge::AutoCommit::save`] returned) listing `hashes` (its
/// `change_hashes()` at that save) and the `heads` it stood at, or `None` for a document too
/// small to need one.
#[must_use]
pub fn encode(saved: &[u8], hashes: &[ChangeHash], heads: &[ChangeHash]) -> Option<Vec<u8>> {
    if saved.len() < MIN_HINTED_BYTES && hashes.len() < MIN_HINTED_CHANGES {
        return None;
    }
    let chunk = leading_document_chunk(saved)?;
    let capacity = MAGIC
        .len()
        .saturating_add(DIGEST)
        .saturating_add(LEN)
        .saturating_add(DIGEST)
        .saturating_add(COUNT)
        .saturating_add(HASH.saturating_mul(hashes.len()))
        .saturating_add(COUNT)
        .saturating_add(HASH.saturating_mul(heads.len()));
    let mut hint = Vec::with_capacity(capacity);
    hint.extend_from_slice(MAGIC);
    hint.extend_from_slice(blake3::hash(chunk).as_bytes());
    hint.extend_from_slice(&u64::try_from(saved.len()).ok()?.to_le_bytes());
    hint.extend_from_slice(blake3::hash(saved).as_bytes());
    hint.extend_from_slice(&u32::try_from(hashes.len()).ok()?.to_le_bytes());
    for hash in hashes {
        hint.extend_from_slice(&hash.0);
    }
    hint.extend_from_slice(&u32::try_from(heads.len()).ok()?.to_le_bytes());
    for head in heads {
        hint.extend_from_slice(&head.0);
    }
    Some(hint)
}

/// The change hashes `hint` lists, when it was made for the document chunk `stored` starts with;
/// `None` for any other bytes, a malformed hint, or one listing no hashes.
#[must_use]
pub fn hashes_for(hint: &[u8], stored: &[u8]) -> Option<Vec<ChangeHash>> {
    let fields = Fields::read(hint)?;
    let chunk = leading_document_chunk(stored)?;
    if blake3::hash(chunk).as_bytes().as_slice() != fields.chunk {
        return None;
    }
    if fields.hashes.is_empty() {
        return None;
    }
    fields
        .hashes
        .chunks_exact(HASH)
        .map(|hash| ChangeHash::try_from(hash).ok())
        .collect()
}

/// The heads `hint` names, when it was made for exactly `stored`: its length and whole digest are
/// those the hint was written for. `None` for any other bytes, a malformed hint, or one naming no
/// heads.
#[must_use]
pub fn heads_for(hint: &[u8], stored: &[u8]) -> Option<Vec<ChangeHash>> {
    let fields = Fields::read(hint)?;
    if fields.len != u64::try_from(stored.len()).ok()?
        || blake3::hash(stored).as_bytes().as_slice() != fields.whole
    {
        return None;
    }
    if fields.heads.is_empty() {
        return None;
    }
    fields
        .heads
        .chunks_exact(HASH)
        .map(|head| ChangeHash::try_from(head).ok())
        .collect()
}

/// A hint's parts, as byte slices into it.
struct Fields<'a> {
    chunk: &'a [u8],
    len: u64,
    whole: &'a [u8],
    hashes: &'a [u8],
    heads: &'a [u8],
}

impl<'a> Fields<'a> {
    /// `hint` read, or `None` when it is not one this format wrote.
    fn read(hint: &'a [u8]) -> Option<Self> {
        let rest = hint.strip_prefix(MAGIC.as_slice())?;
        let (chunk, rest) = rest.split_at_checked(DIGEST)?;
        let (len, rest) = rest.split_at_checked(LEN)?;
        let len = u64::from_le_bytes(len.try_into().ok()?);
        let (whole, rest) = rest.split_at_checked(DIGEST)?;
        let (hashes, rest) = count_and_items(rest)?;
        let (heads, rest) = count_and_items(rest)?;
        if !rest.is_empty()
            || !hashes.len().is_multiple_of(HASH)
            || !heads.len().is_multiple_of(HASH)
        {
            return None;
        }
        Some(Self {
            chunk,
            len,
            whole,
            hashes,
            heads,
        })
    }
}

/// The items `bytes` lists after a count of them.
fn count_and_items(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (count, rest) = bytes.split_at_checked(COUNT)?;
    let count = usize::try_from(u32::from_le_bytes(count.try_into().ok()?)).ok()?;
    let len = count.checked_mul(HASH)?;
    rest.split_at_checked(len)
}

/// The chunk `bytes` starts with, header included, when it is a document
/// chunk: Automerge's magic bytes, a 4-byte checksum, chunk type 0 and the
/// body's length as unsigned LEB128.
fn leading_document_chunk(bytes: &[u8]) -> Option<&[u8]> {
    const CHUNK_MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
    const DOCUMENT: u8 = 0;
    let rest = bytes.strip_prefix(CHUNK_MAGIC.as_slice())?.get(4..)?;
    let (&kind, rest) = rest.split_first()?;
    if kind != DOCUMENT {
        return None;
    }
    let mut body = 0u64;
    let mut used = 0usize;
    for (index, byte) in rest.iter().take(10).enumerate() {
        let shift = u32::try_from(index).ok()?.checked_mul(7)?;
        body |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            used = index.checked_add(1)?;
            break;
        }
    }
    if used == 0 {
        return None;
    }
    let header = CHUNK_MAGIC
        .len()
        .checked_add(4)?
        .checked_add(1)?
        .checked_add(used)?;
    bytes.get(..header.checked_add(usize::try_from(body).ok()?)?)
}
