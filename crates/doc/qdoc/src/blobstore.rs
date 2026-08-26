//! Content-addressed storage for document binaries, and the sniffer that
//! derives a blob's descriptor from its bytes.
//!
//! The bytes are deliberately not in the document. Storing them inline made a
//! 146 KB image a 146 KB automerge change, and a change is what the sync path
//! carries: gossipsub caps one RPC at 64 KiB, so the transport rejected it
//! without a word. Automerge queues a change whose ancestors it lacks rather
//! than applying it, so every keystroke after the image named a change nobody
//! downstream would ever receive, and the far replica froze at the line before
//! it while both ends still reported themselves connected.
//!
//! So the document holds `{hash, mime, len, w, h}` and the bytes live here —
//! the shape `fs/src/hydration.rs` already uses. A reference is a few dozen
//! bytes and cannot hit that ceiling, dedup is free because the key *is* the
//! content hash, and history stops carrying megabytes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

/// Metadata a document carries *instead of* a blob's bytes.
///
/// Enough for a renderer to lay out a placeholder occupying exactly the space
/// the image will, so nothing reflows when the bytes arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobMeta {
    /// Media type, sniffed from the content (`image/png`, …). Empty if unknown.
    pub mime: String,
    pub len: u64,
    /// Intrinsic pixel width, 0 when not an image we can read a header from.
    pub w: u32,
    /// Intrinsic pixel height, 0 when unknown.
    pub h: u32,
}

/// A content-addressed byte store keyed by blake3 hex.
///
/// Implementations must treat the key as authoritative and verify that the bytes
/// hash to it. A store that trusts the key would let one peer poison every
/// replica that later reads that hash, which is the point of addressing by
/// content rather than by name.
pub trait BlobStore: Send + Sync {
    fn get(&self, hash: &str) -> Option<Vec<u8>>;
    fn put(&self, hash: &str, bytes: &[u8]) -> bool;
    fn has(&self, hash: &str) -> bool {
        self.get(hash).is_some()
    }
}

/// Hash `bytes` the way every blob key in this system is derived.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// True when `bytes` really are the content named by `hash`.
pub fn verify(hash: &str, bytes: &[u8]) -> bool {
    hash_bytes(bytes) == hash
}

/// In-process store: the default, so a unit test or a headless codec run works
/// with no filesystem at all.
#[derive(Default)]
pub struct MemoryBlobStore {
    inner: RwLock<BTreeMap<String, Vec<u8>>>,
}

impl BlobStore for MemoryBlobStore {
    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        self.inner.read().ok()?.get(hash).cloned()
    }
    fn put(&self, hash: &str, bytes: &[u8]) -> bool {
        if !verify(hash, bytes) {
            return false;
        }
        match self.inner.write() {
            Ok(mut m) => {
                m.insert(hash.to_string(), bytes.to_vec());
                true
            }
            Err(_) => false,
        }
    }
}

/// Filesystem store, sharded one level on the first two hex characters so a
/// directory does not accumulate every blob the machine has ever seen.
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store this machine uses unless told otherwise. `QOS_BLOB_STORE`
    /// overrides it, which is what lets two sessions on one host be two peers
    /// with genuinely separate stores rather than one store they both hit.
    pub fn default_root() -> PathBuf {
        if let Ok(v) = std::env::var("QOS_BLOB_STORE")
            && !v.is_empty()
        {
            return PathBuf::from(v);
        }
        let base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        base.join("qos").join("blobs")
    }

    fn path_for(&self, hash: &str) -> Option<PathBuf> {
        // The hash reaches this function from the wire, so anything that is not
        // plain hex is refused: a key with a separator in it would otherwise
        // address a file outside the store.
        if hash.len() < 4 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.root.join(&hash[..2]).join(hash))
    }
}

impl BlobStore for FsBlobStore {
    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        let p = self.path_for(hash)?;
        let bytes = std::fs::read(p).ok()?;
        // A blob that does not match its own name is corruption, not content.
        if verify(hash, &bytes) {
            Some(bytes)
        } else {
            None
        }
    }

    fn put(&self, hash: &str, bytes: &[u8]) -> bool {
        if !verify(hash, bytes) {
            return false;
        }
        let Some(p) = self.path_for(hash) else {
            return false;
        };
        if p.exists() {
            return true; // content-addressed: identical bytes, nothing to do
        }
        let Some(dir) = p.parent() else { return false };
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        // Write beside the target and rename, so a reader never observes a
        // partially written blob under a name that promises whole content.
        let tmp = dir.join(format!(".{hash}.partial"));
        if std::fs::write(&tmp, bytes).is_err() {
            return false;
        }
        if std::fs::rename(&tmp, &p).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        true
    }

    fn has(&self, hash: &str) -> bool {
        self.path_for(hash).map(|p| p.exists()).unwrap_or(false)
    }
}

static STORE: OnceLock<RwLock<Arc<dyn BlobStore>>> = OnceLock::new();

fn cell() -> &'static RwLock<Arc<dyn BlobStore>> {
    STORE.get_or_init(|| RwLock::new(Arc::new(MemoryBlobStore::default())))
}

/// Install the process-wide store.
///
/// A global rather than a handle threaded through `DocMut`, because otherwise
/// every codec and every caller of `put_blob` needs the parameter, and the store
/// is a property of the machine rather than of a document.
pub fn set_store(store: Arc<dyn BlobStore>) {
    if let Ok(mut s) = cell().write() {
        *s = store;
    }
}

/// The process-wide store (in-memory until [`set_store`] says otherwise).
pub fn store() -> Arc<dyn BlobStore> {
    match cell().read() {
        Ok(s) => s.clone(),
        Err(_) => Arc::new(MemoryBlobStore::default()),
    }
}

/// How this process reaches the rest of the system for content.
///
/// A content-addressed store makes a blob's *identity* portable; it does not
/// move the bytes, and nothing else here was going to. An app holds its blobs in
/// its own sandbox — the launcher gives every app a private `HOME` and
/// `XDG_CACHE_HOME`, deliberately — so there is no directory an app and its
/// shell can both read, and there must not be one. Without this door an app
/// could put an image in a document that the shell then had to send to the room
/// without ever holding its bytes, and both ends would read a store that was
/// simply not the store the content was in.
///
/// Whatever owns this process's link to the rest of the system installs one;
/// then [`put`] announces and a missed [`get`] asks.
pub trait BlobTransport: Send + Sync {
    /// This process now holds `hash`. Anyone who might need it should hear.
    fn offer(&self, hash: &str, bytes: &[u8]);
    /// This process needs `hash` and does not have it. Called on a miss,
    /// rate-limited by [`ASK_AGAIN_AFTER`].
    fn want(&self, hash: &str);
}

static TRANSPORT: OnceLock<RwLock<Option<Arc<dyn BlobTransport>>>> = OnceLock::new();

fn transport_cell() -> &'static RwLock<Option<Arc<dyn BlobTransport>>> {
    TRANSPORT.get_or_init(|| RwLock::new(None))
}

/// Content stored before a transport existed, waiting to be offered.
///
/// A document is opened — and its images stored — while the app is still
/// starting, which is before anything has had a chance to install a transport.
/// Dropping those offers meant the case that matters most, an image that was in
/// the document from the moment it opened, was the one that never travelled.
///
/// Bounded, because this holds real content in memory. A document opening with
/// more images than this offers the first few immediately and the rest when a
/// peer asks, which is the path a late joiner uses anyway.
/// Hash and bytes of a blob stored before any transport existed.
type PendingOffer = (String, Vec<u8>);

static PENDING_OFFERS: OnceLock<RwLock<Vec<PendingOffer>>> = OnceLock::new();

const MAX_PENDING_OFFERS: usize = 8;

fn pending_offers() -> &'static RwLock<Vec<PendingOffer>> {
    PENDING_OFFERS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Install the process-wide transport, and hand it anything stored before it
/// existed.
pub fn set_transport(t: Arc<dyn BlobTransport>) {
    if let Ok(mut s) = transport_cell().write() {
        *s = Some(Arc::clone(&t));
    }
    let queued = match pending_offers().write() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    };
    for (hash, bytes) in queued {
        t.offer(&hash, &bytes);
    }
}

/// Take the transport away again — the collaboration socket closed, or the app
/// left the room.
///
/// Content already here still reads; content that is not stops claiming to be on
/// its way, because with nothing able to fetch it that is no longer true (see
/// [`state`]).
pub fn clear_transport() {
    if let Ok(mut s) = transport_cell().write() {
        *s = None;
    }
}

/// The process-wide transport, if one has been installed.
pub fn transport() -> Option<Arc<dyn BlobTransport>> {
    transport_cell().read().ok()?.clone()
}

/// How long to wait before asking again for the same content. A renderer calls
/// [`get`] for its image every frame, so without this one missing image would
/// put sixty requests a second on the room's topic.
const ASK_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_millis(1000);

static ASKED: OnceLock<RwLock<BTreeMap<String, std::time::Instant>>> = OnceLock::new();

fn may_ask(hash: &str) -> bool {
    let cell = ASKED.get_or_init(|| RwLock::new(BTreeMap::new()));
    let now = std::time::Instant::now();
    let Ok(mut m) = cell.write() else { return false };
    match m.get(hash) {
        Some(t) if now.duration_since(*t) < ASK_AGAIN_AFTER => false,
        _ => {
            // Bounded: a document referencing thousands of blobs must not turn
            // this into a leak. The worst case of evicting oldest-first is one
            // extra request for whatever was evicted.
            if m.len() > 512
                && let Some(k) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())
            {
                m.remove(&k);
            }
            m.insert(hash.to_string(), now);
            true
        }
    }
}

/// Where a reference's content has got to.
///
/// A renderer that only knows "the bytes are not here" draws the same grey box
/// for a transfer in flight and for one that will never complete, and a reader
/// cannot tell a slow link from a lost image. The two have to be
/// distinguishable before anything can be drawn honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobState {
    /// The content is here; [`get`] returns it.
    Resolved,
    /// Not here, and expected: this process has asked and the answer has had
    /// less than [`GIVE_UP_AFTER`] to arrive.
    Pending,
    /// Not here, and not coming — either nothing in this process can fetch it
    /// (no transport, so a miss is permanent by construction), or it has been
    /// asked for long enough that the honest answer is that nobody has it.
    Unavailable,
}

/// How long a missing blob stays [`BlobState::Pending`] before this process
/// stops calling it "arriving".
///
/// Sized against the layer that does the fetching: the shell asks the room and
/// retries a handful of times, taking well under a minute. Declaring failure
/// sooner would call an image lost while it was still transferring; much later
/// leaves a reader watching a box that says "arriving" about something that
/// stopped arriving a minute ago.
pub const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(45);

static FIRST_WANTED: OnceLock<RwLock<BTreeMap<String, std::time::Instant>>> = OnceLock::new();

/// When this process first went looking for `hash`, recording now if it never
/// has. The [`BlobState::Pending`] clock starts here rather than when the
/// reference arrived, because a document can hold a reference for a long time
/// before anything draws it.
fn first_wanted(hash: &str) -> std::time::Instant {
    let cell = FIRST_WANTED.get_or_init(|| RwLock::new(BTreeMap::new()));
    let now = std::time::Instant::now();
    let Ok(mut m) = cell.write() else { return now };
    if m.len() > 512
        && let Some(k) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())
    {
        m.remove(&k);
    }
    *m.entry(hash.to_string()).or_insert(now)
}

/// Forget that `hash` was ever waited on, so a re-reference after it landed
/// starts its clock fresh rather than inheriting an expired one.
fn clear_wait(hash: &str) {
    if let Some(cell) = FIRST_WANTED.get()
        && let Ok(mut m) = cell.write()
    {
        m.remove(hash);
    }
}

/// Whether the content behind `hash` is here, on its way, or not coming.
///
/// Reports; it does not ask. [`get`] is what asks, and a renderer calls that
/// every frame anyway — a second request raised from a state query would be the
/// same flood arriving by another route.
pub fn state(hash: &str) -> BlobState {
    if store().has(hash) {
        clear_wait(hash);
        return BlobState::Resolved;
    }
    // No door out of this process, so a miss is permanent however long anyone
    // waits and "arriving" would be a lie that never resolves.
    if transport().is_none() {
        return BlobState::Unavailable;
    }
    if first_wanted(hash).elapsed() < GIVE_UP_AFTER {
        BlobState::Pending
    } else {
        BlobState::Unavailable
    }
}

/// Read a blob's bytes from the process-wide store.
///
/// A miss asks the transport and still returns `None`: the caller renders its
/// placeholder now, the bytes arrive out of band, and the next frame draws the
/// image. A blocking fetch here would stall a compositor frame on a peer.
pub fn get(hash: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = store().get(hash) {
        clear_wait(hash);
        return Some(bytes);
    }
    if let Some(t) = transport() {
        // Start the clock on the first miss, so `state` can tell a transfer in
        // flight from one nobody is going to answer.
        let _ = first_wanted(hash);
        if may_ask(hash) {
            t.want(hash);
        }
    }
    None
}

/// Write bytes to the process-wide store, returning their hash.
///
/// Also *offers* them to the transport, which is what carries an image out of
/// the process that produced it — the document only ever gains the reference.
pub fn put(bytes: &[u8]) -> String {
    let hash = hash_bytes(bytes);
    if !store().put(&hash, bytes) {
        // The document is about to reference content this machine then does not
        // hold, and nothing downstream can detect that from the reference.
        tracing::error!(
            hash = %hash, bytes = bytes.len(),
            "blobstore: could not store this content; a document referencing it will \
             show a placeholder that never resolves",
        );
        return hash;
    }
    match transport() {
        Some(t) => t.offer(&hash, bytes),
        // A document's images are stored while the app is still starting, so
        // having nowhere to offer them *yet* is the common case, not the odd
        // one. `set_transport` flushes these.
        None => {
            if let Ok(mut q) = pending_offers().write()
                && q.len() < MAX_PENDING_OFFERS
                && !q.iter().any(|(h, _)| h == &hash)
            {
                q.push((hash.clone(), bytes.to_vec()));
            }
        }
    }
    hash
}

/// Derive a descriptor from content: media type plus intrinsic dimensions.
///
/// Dimensions come from the container header rather than a full decode — the
/// point is to know how much space to reserve, which every one of these formats
/// states in its first few dozen bytes. An unrecognised format still gets a
/// correct `len`, and simply reserves no particular size.
pub fn sniff(bytes: &[u8]) -> BlobMeta {
    let mut m = BlobMeta { len: bytes.len() as u64, ..Default::default() };
    if let Some((mime, w, h)) = sniff_image(bytes) {
        m.mime = mime.to_string();
        m.w = w;
        m.h = h;
    }
    m
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn sniff_image(b: &[u8]) -> Option<(&'static str, u32, u32)> {
    // PNG: 8-byte signature, then an IHDR chunk whose width/height are the two
    // big-endian u32s at offset 16.
    if b.len() >= 24 && b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(("image/png", be32(&b[16..20]), be32(&b[20..24])));
    }
    // GIF: logical screen descriptor at offset 6, little-endian u16s.
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        let w = u16::from_le_bytes([b[6], b[7]]) as u32;
        let h = u16::from_le_bytes([b[8], b[9]]) as u32;
        return Some(("image/gif", w, h));
    }
    // WebP: a RIFF container whose lossy/lossless/extended chunks each state the
    // canvas size in a different place.
    if b.len() >= 30 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        match &b[12..16] {
            b"VP8X" => {
                let w = 1 + (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16);
                let h = 1 + (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16);
                return Some(("image/webp", w, h));
            }
            b"VP8 " => {
                let w = (u16::from_le_bytes([b[26], b[27]]) & 0x3FFF) as u32;
                let h = (u16::from_le_bytes([b[28], b[29]]) & 0x3FFF) as u32;
                return Some(("image/webp", w, h));
            }
            b"VP8L" => {
                let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
                return Some(("image/webp", (bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1));
            }
            _ => return Some(("image/webp", 0, 0)),
        }
    }
    // JPEG: walk the marker segments to the start-of-frame, the only one
    // carrying the dimensions. Honouring each segment's declared length is what
    // skips the entropy-coded scan, which a scan for the next 0xFF would not.
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        let mut i = 2usize;
        while i + 3 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            // Standalone markers carry no payload.
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                i += 2;
                continue;
            }
            if marker == 0xD9 || marker == 0xDA {
                break; // end of image, or the start of the scan data
            }
            if i + 3 >= b.len() {
                break;
            }
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
            if is_sof && i + 9 < b.len() {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some(("image/jpeg", w, h));
            }
            if len < 2 {
                break;
            }
            i += 2 + len;
        }
        return Some(("image/jpeg", 0, 0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 PNG, the fixture the codec tests already use.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn png_dimensions_come_from_the_header() {
        let m = sniff(TINY_PNG);
        assert_eq!(m.mime, "image/png");
        assert_eq!((m.w, m.h), (1, 1));
        assert_eq!(m.len, TINY_PNG.len() as u64);
    }

    #[test]
    fn unknown_content_still_reports_its_length() {
        let m = sniff(b"not an image at all");
        assert_eq!(m.mime, "");
        assert_eq!((m.w, m.h), (0, 0));
        assert_eq!(m.len, 19, "length is knowable even when the format is not");
    }

    #[test]
    fn a_store_refuses_bytes_that_do_not_match_their_name() {
        // Accepting bytes that hash to something else would let one peer poison
        // every replica that later reads that hash.
        let s = MemoryBlobStore::default();
        assert!(!s.put(&hash_bytes(b"honest"), b"tampered"));
        assert!(s.get(&hash_bytes(b"honest")).is_none());
        assert!(s.put(&hash_bytes(b"honest"), b"honest"));
    }

    #[test]
    fn a_filesystem_store_round_trips_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsBlobStore::new(dir.path());
        let h = hash_bytes(TINY_PNG);
        assert!(s.put(&h, TINY_PNG));
        assert_eq!(s.get(&h).as_deref(), Some(TINY_PNG));
        assert!(s.has(&h));
        // The hash arrives from the wire, so a key that is not plain hex must
        // not be able to name a path outside the store.
        assert!(s.get("../../etc/passwd").is_none());
        assert!(!s.put("../escape", b"x"));
    }
}
