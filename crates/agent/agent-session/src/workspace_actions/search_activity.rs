//! What search read of the documents and the folder, counted for the timing
//! lines and for the tests that hold a query to the index instead of to what
//! it is built from.

/// What search read of the documents and the folder, counted for the timing lines and for the
/// tests that hold a query to the index instead of to what it is built from.
#[derive(Default)]
pub(super) struct SearchActivity {
    /// Documents opened to read what their apps publish to search while a query waited.
    pub(super) opened: std::sync::atomic::AtomicU64,
    /// Documents opened to read the same between requests, so that a query never has to.
    pub(super) warmed: std::sync::atomic::AtomicU64,
    /// Folder chunks read while a find waited.
    pub(super) files_chunks: std::sync::atomic::AtomicU64,
    /// Files whose words were read while a find waited, from a chunk or the content store.
    pub(super) files_read: std::sync::atomic::AtomicU64,
    /// Files whose words were read between requests, so that a find never has to.
    pub(super) files_warmed: std::sync::atomic::AtomicU64,
    /// Permission reads made for a find's caller while it waited.
    pub(super) permissions: std::sync::atomic::AtomicU64,
    /// Requests served while the index was reading a document: what says the reading happens
    /// beside the serving, not on the thread the requests arrive on, where a write's landing
    /// held every request of the service's share for the seconds a notebook takes to read.
    pub(super) beside_reading: std::sync::atomic::AtomicU64,
    /// Documents the index's thread is reading now.
    pub(super) reading: std::sync::atomic::AtomicU64,
}
