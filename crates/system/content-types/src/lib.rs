//! Content descriptors and image signatures, independent of storage and transport.
//!
//! Action and protocol definitions can inspect content without depending on a
//! device storage client that itself transports those definitions.

mod document;
mod sniff;
pub use document::{DocumentFormat, document_format, opened_by};
pub use sniff::{ImageFormat, image_format, image_size, sniff};

/// Whether `hash` has the shape of a content address (64 lower- or upper-case hex digits).
/// Addresses arrive from documents and peers, so anything else is refused before it can name a path.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// What a document records about content instead of its bytes, so placeholders
/// reserve the eventual content's dimensions before those bytes arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentMeta {
    /// Media type, sniffed from the content (`image/png`, …). Empty if unknown.
    pub mime: String,
    pub len: u64,
    /// Intrinsic pixel width, 0 when not an image we can read a header from.
    pub w: u32,
    /// Intrinsic pixel height, 0 when unknown.
    pub h: u32,
}
