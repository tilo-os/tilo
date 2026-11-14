//! Portable, verified media bytes for collaborative Camera captures.
//!
//! A document stores only the `media:blake3:<reference>` identity. The bytes live in
//! the app's content store, which verifies them against that reference on every
//! read; no received document can direct Camera to open an arbitrary local
//! path.

use std::path::Path;

use tilo_content::ContentStore;

pub const MEDIA_PREFIX: &str = "media:blake3:";
pub const MAX_MEDIA_BYTES: usize = 64 * 1024 * 1024;

pub fn is_media_id(id: &str) -> bool {
    id.strip_prefix(MEDIA_PREFIX)
        .is_some_and(tilo_content::valid_reference)
}

/// The content address `id` names, which a thumbnail cache keys on.
pub fn address(id: &str) -> Option<String> {
    tilo_content::content_address(id.strip_prefix(MEDIA_PREFIX)?)
}

pub fn store(content: &ContentStore, bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_MEDIA_BYTES {
        return None;
    }
    // A refused write surfaces to the caller as `None`, which reports it.
    let reference = content.put(bytes).ok()?;
    let stored = content.read(&reference)?;
    tilo_content::names(&reference, &stored).then(|| format!("{MEDIA_PREFIX}{reference}"))
}

pub fn read(content: &ContentStore, id: &str) -> Option<Vec<u8>> {
    let reference = id.strip_prefix(MEDIA_PREFIX)?;
    let bytes = content.read(reference)?;
    (bytes.len() <= MAX_MEDIA_BYTES && tilo_content::names(reference, &bytes)).then_some(bytes)
}

pub fn store_file(content: &ContentStore, path: &Path) -> Option<String> {
    let bytes = crate::files::read_bounded(path, MAX_MEDIA_BYTES)?;
    store(content, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_bytes_have_a_verified_portable_identity() {
        let content = ContentStore::memory();
        let id = store(&content, b"camera media").expect("store");
        assert!(is_media_id(&id));
        assert_eq!(read(&content, &id).as_deref(), Some(&b"camera media"[..]));
        assert!(!is_media_id("/tmp/not-a-shared-media-file"));
    }

    #[test]
    fn a_sealed_reference_names_its_media() {
        let content = ContentStore::memory().sealing();
        let id = store(&content, b"camera media").expect("store");
        let reference = id.strip_prefix(MEDIA_PREFIX).expect("the media prefix");
        assert!(tilo_content::is_sealed(reference), "{id}");
        assert!(is_media_id(&id));
        assert_eq!(read(&content, &id).as_deref(), Some(&b"camera media"[..]));
        assert_eq!(
            address(&id).as_deref(),
            tilo_content::content_address(reference).as_deref()
        );
    }
}
