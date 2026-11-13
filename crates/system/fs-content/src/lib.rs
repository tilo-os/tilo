//! The content format of the storage daemon.
//!
//! A file version's bytes are split into content-defined chunks with normalised
//! `FastCDC`, at sizes that scale with the body ([`ChunkTier`]): small files get
//! small chunks, so an edit re-sends little, and large files large chunks, so a
//! gigabyte is a few thousand chunks rather than a hundred thousand. A chunk's
//! BLAKE3 hash is also the root of its BAO tree, so a peer can fetch and verify
//! any byte range of any chunk.
//!
//! A version is described by a [`Manifest`], encoded as an iroh-blobs `HashSeq`
//! whose first entry is a header rather than a hash, followed by the chunk hashes
//! and the packed chunk lengths:
//!
//! ```text
//! [ header | c0 | c1 | … | cN-1 | lengths… ]      32 bytes per entry
//! header  = "tilocf" · format (2) · tier · total_len u64 · chunk count u64 · 0u64
//! lengths = u32 little endian, eight per entry, zero padded
//! content id = BLAKE3(encoding)
//! ```
//!
//! One `GET` of the content id with a `RangeSpecSeq` fetches exactly the missing
//! chunks, or byte ranges within them ([`Manifest::child_offset`]), BAO-verified
//! on one stream; the header and length entries are never requested.

mod chunker;
mod manifest;
pub mod ranges;
mod tier;

pub use chunker::{ChunkStats, Cutter, build, build_in_tier, build_reader, rebuild};
pub use manifest::{
    ChunkRef, DecodeError, FORMAT, HEADER_MAGIC, MAX_CHUNK_LEN, Manifest, is_manifest,
};
pub use ranges::RangeSet;
pub use tier::ChunkTier;
