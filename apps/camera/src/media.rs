//! Portable, verified media bytes for collaborative Camera captures.
//!
//! A document stores only the `media:blake3:<hash>` identity. qdoc's blob
//! transport owns the bytes and asks peers for a missing hash; no received
//! document can direct Camera to open an arbitrary local path.

use std::io::Read;
use std::path::Path;

pub const MEDIA_PREFIX: &str = "media:blake3:";
pub const MAX_MEDIA_BYTES: usize = 64 * 1024 * 1024;

pub fn is_media_id(id: &str) -> bool {
    id.strip_prefix(MEDIA_PREFIX)
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub fn store(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_MEDIA_BYTES {
        return None;
    }
    let hash = qdoc::blobstore::put(bytes)?;
    let stored = qdoc::blobstore::get(&hash)?;
    (stored == bytes && qdoc::blobstore::verify(&hash, &stored))
        .then(|| format!("{MEDIA_PREFIX}{hash}"))
}

pub fn read(id: &str) -> Option<Vec<u8>> {
    let hash = id.strip_prefix(MEDIA_PREFIX)?;
    let bytes = qdoc::blobstore::get(hash)?;
    (bytes.len() <= MAX_MEDIA_BYTES && qdoc::blobstore::verify(hash, &bytes)).then_some(bytes)
}

pub fn store_file(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(64 * 1024);
    file.take((MAX_MEDIA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    store(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_bytes_have_a_verified_portable_identity() {
        let id = store(b"camera media").expect("store");
        assert!(is_media_id(&id));
        assert_eq!(read(&id).as_deref(), Some(&b"camera media"[..]));
        assert!(!is_media_id("/tmp/not-a-shared-media-file"));
    }
}
