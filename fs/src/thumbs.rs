//! Host-side thumbnail generation + caching for the FUSE mount.
//!
//! A thumbnail is **metadata the host generates ahead of (or independent of)
//! the content bytes**. This module owns the filesystem side of that: it wraps
//! the shared [`thumbnail`] generator + content-addressed cache, generating a
//! small PNG preview from a file's *local* content and caching it under
//! `<cache-dir>/thumbs` (so it survives a remount and the read-only root stays
//! read-only). [`crate::fs::QuickFS`] drives it: it serves the PNG through the
//! `user.qos.thumb` xattr and records the thumbnail blob's hash on the namespace
//! node so the pointer gossips and a peer sees a preview before it fetches the
//! file.
//!
//! Only the light, pure-Rust preview types are host-generated here (raster
//! images and EPUB covers) — the `thumbnail` crate is linked with default
//! features so the OS daemon never pulls pdfium or the gstreamer stack. PDF and
//! video previews are the client's job.

use std::path::Path;

use thumbnail::{FileCategory, ThumbCache, ThumbKey};

/// A file larger than this is never read whole just to make a thumbnail — a
/// pathological source must not stall the FUSE thread or blow up memory.
const MAX_SOURCE_BYTES: usize = 96 * 1024 * 1024;

/// The host's thumbnail generator + on-disk cache for one mount.
pub struct ThumbProvider {
    cache: ThumbCache,
    edge: u32,
}

impl ThumbProvider {
    /// Open a provider whose cache lives under `dir` (created if missing).
    /// Typically `<cache-dir>/thumbs`.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self {
            cache: ThumbCache::open(dir)?,
            edge: thumbnail::DEFAULT_EDGE,
        })
    }

    /// The target longest edge of generated thumbnails.
    pub fn edge(&self) -> u32 {
        self.edge
    }

    fn key(&self, content_hash: [u8; 32]) -> ThumbKey {
        ThumbKey::of_hash(content_hash, self.edge)
    }

    /// The cached PNG for a content hash, without generating anything.
    pub fn cached(&self, content_hash: [u8; 32]) -> Option<Vec<u8>> {
        self.cache.get(&self.key(content_hash)).map(|a| a.as_ref().clone())
    }

    /// Generate a PNG thumbnail from a file's `bytes` (category chosen by the
    /// caller), cache it content-addressed under `content_hash`, and return it.
    /// `None` for a non-previewable category, an oversized source, or a decode
    /// failure.
    pub fn generate(
        &self,
        content_hash: [u8; 32],
        category: FileCategory,
        bytes: &[u8],
    ) -> Option<Vec<u8>> {
        if !category.has_preview() || bytes.len() > MAX_SOURCE_BYTES {
            return None;
        }
        let thumb = thumbnail::generate(&thumbnail::Source::Bytes(bytes), category, self.edge)?;
        let png = thumbnail::encode_png(&thumb)?;
        self.cache.put(&self.key(content_hash), png.clone());
        Some(png)
    }

    /// Adopt an already-encoded PNG thumbnail (e.g. a blob hydrated from a peer
    /// that generated it), validating it decodes before caching it under
    /// `content_hash`. `None` if the bytes are not a decodable image.
    pub fn adopt(&self, content_hash: [u8; 32], png: Vec<u8>) -> Option<Vec<u8>> {
        thumbnail::decode_png(&png)?;
        self.cache.put(&self.key(content_hash), png.clone());
        Some(png)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = image::RgbaImage::new(w, h);
        for px in buf.pixels_mut() {
            *px = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn generates_caches_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let src = solid_png(400, 300, [30, 120, 210]);
        let content_hash = *blake3::hash(&src).as_bytes();

        let png = {
            let provider = ThumbProvider::open(dir.path()).unwrap();
            assert!(provider.cached(content_hash).is_none(), "cold");
            let png = provider
                .generate(content_hash, FileCategory::Image, &src)
                .expect("generates a preview");
            // It is a real, smaller PNG.
            let decoded = thumbnail::decode_png(&png).unwrap();
            assert!(decoded.is_valid());
            assert_eq!(decoded.max_edge(), thumbnail::DEFAULT_EDGE);
            // Second lookup hits the cache.
            assert_eq!(provider.cached(content_hash).unwrap(), png, "cache hit");
            png
        };

        // A fresh provider on the same dir finds it on disk (survives a remount).
        let reopened = ThumbProvider::open(dir.path()).unwrap();
        assert_eq!(
            reopened.cached(content_hash).unwrap(),
            png,
            "thumbnail survives a reopen via the on-disk cache"
        );
    }

    #[test]
    fn declines_badge_types_and_junk() {
        let dir = tempfile::tempdir().unwrap();
        let provider = ThumbProvider::open(dir.path()).unwrap();
        assert!(
            provider.generate([1u8; 32], FileCategory::Archive, b"PK\x03\x04").is_none(),
            "a non-previewable type is never generated"
        );
        assert!(
            provider.generate([2u8; 32], FileCategory::Image, b"not an image").is_none(),
            "undecodable bytes yield no thumbnail"
        );
    }
}
