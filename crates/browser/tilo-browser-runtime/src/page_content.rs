//! The content the page's windows hold: the pictures and attachments their
//! records reference, by hash, in the page's memory. Every window of the
//! page shares it, so content one fetched is there for the others; the
//! framework joins it to each window's record route, so content a record
//! references and the page lacks is asked of the room, and its pieces are
//! verified before they are kept here.

use tilo_content::{ContentLimits, ContentStore, MemoryBackend};

/// Most the page keeps: a tab is not a disk.
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    static PAGE_CONTENT: ContentStore = ContentStore::new(MemoryBackend::new(ContentLimits {
        max_total_bytes: MAX_TOTAL_BYTES,
        ..ContentLimits::default()
    }));
}

/// The page's content store, lent to each window it opens.
pub(crate) fn page_content() -> ContentStore {
    PAGE_CONTENT.with(Clone::clone)
}
