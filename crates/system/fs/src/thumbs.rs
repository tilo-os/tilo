//! Host-side thumbnail generation + caching for the FUSE mount.
//!
//! A thumbnail is metadata generated ahead of (or independent of) the content
//! bytes: a small PNG preview kept in the shared preview cache under
//! `<cache-dir>/thumbs`, served through the `user.tilo.thumb` xattr and
//! gossiped via the namespace node.
//!
//! Only light, pure-Rust preview types are host-generated (raster images,
//! EPUB covers) — `thumbnail` uses default features so the daemon never
//! pulls pdfium or gstreamer; PDF/video previews are the client's job.

use std::path::Path;

use thumbnail::{FileCategory, ThumbCache, ThumbKey};

/// Longest-edge target for host-generated previews — smaller than the file
/// manager's grid, on purpose: it must stay under [`MAX_XATTR_THUMB`] to ride the xattr.
pub const HOST_EDGE: u32 = 160;

/// The largest thumbnail servable via the xattr — the kernel caps a single
/// xattr at 64 KiB (`XATTR_SIZE_MAX`); larger reports absent (`ENODATA`).
pub const MAX_XATTR_THUMB: usize = 60 * 1024;

/// The host's thumbnail generator + on-disk cache for one mount.
pub struct ThumbProvider {
    cache: ThumbCache,
    edge: u32,
}

impl ThumbProvider {
    /// Open a provider whose cache lives under `dir` (created if missing; typically `<cache-dir>/thumbs`).
    ///
    /// # Errors
    ///
    /// Returns an error when `dir` cannot be created or the on-disk cache cannot be opened.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self {
            cache: ThumbCache::open(dir)?,
            edge: HOST_EDGE,
        })
    }

    pub fn edge(&self) -> u32 {
        self.edge
    }

    fn key(&self, content_hash: [u8; 32]) -> ThumbKey {
        ThumbKey::of_hash(content_hash, self.edge)
    }

    /// The cached PNG for a content hash, without generating anything.
    pub fn cached(&self, content_hash: [u8; 32]) -> Option<Vec<u8>> {
        self.cache
            .get(&self.key(content_hash))
            .map(|a| a.as_ref().clone())
    }

    /// Generate a PNG thumbnail from `bytes`, cache it under `content_hash`. `None` for non-previewable, oversized, or undecodable input.
    pub fn generate(
        &self,
        content_hash: [u8; 32],
        category: FileCategory,
        bytes: &[u8],
    ) -> Option<Vec<u8>> {
        if !category.has_preview() || bytes.len() > thumbnail::MAX_SOURCE_BYTES {
            return None;
        }
        let thumb = thumbnail::generate(&thumbnail::Source::Bytes(bytes), category, self.edge)?;
        let png = thumbnail::encode_png(&thumb)?;
        self.cache.put(&self.key(content_hash), png.clone());
        Some(png)
    }

    /// Adopt an already-encoded PNG (e.g. hydrated from a peer), validating it decodes before caching. `None` if not decodable.
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
    fn thumbnails_generate_cache_reopen_and_decline_junk() {
        {
            let dir = tempfile::tempdir().unwrap();
            let src = solid_png(400, 300, [30, 120, 210]);
            let content_hash = *blake3::hash(&src).as_bytes();

            let png = {
                let provider = ThumbProvider::open(dir.path()).unwrap();
                assert!(provider.cached(content_hash).is_none(), "cold");
                let png = provider
                    .generate(content_hash, FileCategory::Image, &src)
                    .expect("generates a preview");
                let decoded = thumbnail::decode_png(&png).unwrap();
                assert!(decoded.is_valid());
                assert_eq!(decoded.max_edge(), HOST_EDGE);
                assert!(
                    png.len() <= MAX_XATTR_THUMB,
                    "host preview fits the xattr: {} bytes",
                    png.len()
                );
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
        {
            let dir = tempfile::tempdir().unwrap();
            let provider = ThumbProvider::open(dir.path()).unwrap();
            assert!(
                provider
                    .generate([1u8; 32], FileCategory::Archive, b"PK\x03\x04")
                    .is_none(),
                "a non-previewable type is never generated"
            );
            assert!(
                provider
                    .generate([2u8; 32], FileCategory::Image, b"not an image")
                    .is_none(),
                "undecodable bytes yield no thumbnail"
            );
        }
    }
}
