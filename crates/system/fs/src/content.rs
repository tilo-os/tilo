//! The content format (`fs-content`), as the daemon uses it.
//!
//! A file version is identified by the hash of its manifest (its content
//! id). Versions are written as size-tiered `FastCDC` chunks
//! described by a self-contained `HashSeq` manifest.

pub use fs_content::{
    ChunkRef, ChunkStats, ChunkTier, DecodeError, MAX_CHUNK_LEN, Manifest, RangeSet, build,
    build_reader, rebuild,
};

/// Content larger than this lives in the store's file-backed area; smaller
/// content is kept as loose blobs (its manifest and a few chunks), which is
/// what many small files transfer and store best as.
pub const FILE_BACKED_MIN: u64 = 1024 * 1024;

/// Whether `manifest`'s content lives in the file-backed area.
pub fn is_file_backed(manifest: &Manifest) -> bool {
    manifest.total_len() > FILE_BACKED_MIN
}

/// `(hash, offset, len)` of every chunk, for the area's chunk index.
pub fn chunk_locations(manifest: &Manifest) -> Vec<([u8; 32], u64, u32)> {
    manifest
        .chunks()
        .iter()
        .enumerate()
        .filter_map(|(index, chunk)| {
            let range = manifest.chunk_range(index)?;
            Some((chunk.hash, range.start, chunk.len))
        })
        .collect()
}
