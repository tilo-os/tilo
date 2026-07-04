//! Loading real PDF files.
//!
//! Two backends, chosen at open time with a graceful fallback:
//!
//!  * **Raster** — when a `pdfium` native library can be located, each page is
//!    rasterised on demand to an RGBA bitmap at the current zoom and painted
//!    directly, so the viewer shows the document exactly as authored (fonts,
//!    figures, vector art and all). Bitmaps are cached per page + zoom bucket
//!    so scrolling and panning never re-rasterise.
//!  * **Text** — when no native library is available, the page text is
//!    extracted in pure Rust and reflowed onto the page card. The words are the
//!    document's own, so search, selection and markup still operate on real
//!    content; only the exact visual layout is lost.
//!
//! The library is discovered via `QOS_PDFIUM_LIB`, a few well-known system
//! paths, or the platform loader — never bundled, so the binary runs whether
//! or not pdfium is present on the host.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context};
use pdfium_render::prelude::*;
use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat};

use crate::model::{Document, OutlineItem};

/// A document loaded from disk plus its optional raster backend.
pub struct LoadedDoc {
    pub doc: Document,
    /// Present only in raster mode; `None` means the text backend is in use.
    pub raster: Option<Rasterizer>,
    /// The file's display name (for the window chrome / status bar).
    pub name: String,
}

/// Load a PDF from `path`, preferring pixel-accurate rasterisation and falling
/// back to pure-Rust text extraction.
pub fn open(path: &Path) -> anyhow::Result<LoadedDoc> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Document")
        .to_string();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_string();

    if let Some(r) = Rasterizer::open(bytes.clone()) {
        let (mut title, author, subject) = r.metadata();
        if title.trim().is_empty() {
            title = stem.clone();
        }
        let (pw, ph) = r.first_page_size();
        let mut doc = Document::blank_pages(&title, &author, r.page_count(), pw, ph);
        doc.subject = subject;
        doc.outline = r.outline();
        return Ok(LoadedDoc { doc, raster: Some(r), name });
    }

    // No native library — extract the text so the words are still real.
    let pages = pdf_extract::extract_text_from_mem_by_pages(&bytes)
        .map_err(|e| anyhow!("pdf text extraction failed: {e:?}"))?;
    let doc = Document::from_text_pages(&stem, "", &pages);
    Ok(LoadedDoc { doc, raster: None, name })
}

/// A retained pdfium document that rasterises pages on demand.
///
/// The `Pdfium` handle is leaked to `'static` so the borrowed [`PdfDocument`]
/// can be stored alongside it; there is exactly one per opened file and it
/// lives for the process, which is the intended lifetime.
pub struct Rasterizer {
    doc: PdfDocument<'static>,
    page_sizes: Vec<(f64, f64)>,
    /// Rendered bitmaps keyed by (page index, target-width bucket in px).
    cache: HashMap<(usize, u32), PenikoImage>,
}

impl Rasterizer {
    fn open(bytes: Vec<u8>) -> Option<Self> {
        let bindings = find_pdfium()?;
        // One process-lifetime handle; leaking avoids a self-referential struct.
        let pdfium: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
        let doc = pdfium.load_pdf_from_byte_vec(bytes, None).ok()?;
        let mut page_sizes = Vec::new();
        let pages = doc.pages();
        for i in 0..pages.len() {
            if let Ok(rect) = pages.page_size(i) {
                page_sizes.push((rect.width().value as f64, rect.height().value as f64));
            } else {
                page_sizes.push((612.0, 792.0));
            }
        }
        if page_sizes.is_empty() {
            return None;
        }
        Some(Rasterizer { doc, page_sizes, cache: HashMap::new() })
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn first_page_size(&self) -> (f64, f64) {
        self.page_sizes.first().copied().unwrap_or((612.0, 792.0))
    }

    fn metadata(&self) -> (String, String, String) {
        let meta = self.doc.metadata();
        let get = |tag| meta.get(tag).map(|t| t.value().to_string()).unwrap_or_default();
        (
            get(PdfDocumentMetadataTagType::Title),
            get(PdfDocumentMetadataTagType::Author),
            get(PdfDocumentMetadataTagType::Subject),
        )
    }

    /// Best-effort table of contents from the document's bookmarks. A flat
    /// list in document order — every entry still navigates to its page.
    fn outline(&self) -> Vec<OutlineItem> {
        let mut out = Vec::new();
        for bm in self.doc.bookmarks().iter() {
            let Some(title) = bm.title() else { continue };
            let page = bm
                .destination()
                .and_then(|d| d.page_index().ok())
                .map(|i| i as usize + 1)
                .unwrap_or(1);
            if !title.trim().is_empty() {
                out.push(OutlineItem { title, page, level: 0 });
            }
            if out.len() >= 200 {
                break;
            }
        }
        out
    }

    /// Rasterise `page_idx` (0-based) at roughly `target_w` device pixels wide,
    /// returning a cached bitmap. Widths are snapped to a bucket so a small
    /// zoom nudge reuses the existing render instead of re-rasterising.
    pub fn render(&mut self, page_idx: usize, target_w: u32) -> Option<PenikoImage> {
        if page_idx >= self.page_sizes.len() {
            return None;
        }
        let bucket = ((target_w / 24).max(1) * 24).clamp(96, 4096);
        let key = (page_idx, bucket);
        if let Some(img) = self.cache.get(&key) {
            return Some(img.clone());
        }
        let page = self.doc.pages().get(page_idx as u16).ok()?;
        let config = PdfRenderConfig::new().set_target_width(bucket as Pixels);
        let bitmap = page.render_with_config(&config).ok()?;
        let w = bitmap.width().max(0) as u32;
        let h = bitmap.height().max(0) as u32;
        if w == 0 || h == 0 {
            return None;
        }
        let rgba = bitmap.as_rgba_bytes();
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        let img = PenikoImage {
            data: Blob::new(Arc::new(rgba)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: w,
            height: h,
        };
        self.cache.insert(key, img.clone());
        Some(img)
    }

    /// Drop cached page bitmaps for stale zoom levels, keeping the current
    /// page bucket plus the small thumbnail renders (which are cheap to hold
    /// and expensive to redo every scroll). Called once per frame so a full
    /// zoom level's worth of large bitmaps never accumulates.
    pub fn prune(&mut self, keep_bucket: u32) {
        const THUMB_MAX: u32 = 600;
        self.cache
            .retain(|(_, b), _| *b == keep_bucket || *b <= THUMB_MAX);
    }
}

/// Locate a usable pdfium library without ever bundling one.
fn find_pdfium() -> Option<Box<dyn PdfiumLibraryBindings>> {
    if let Ok(path) = std::env::var("QOS_PDFIUM_LIB") {
        if !path.is_empty() {
            if let Ok(b) = Pdfium::bind_to_library(&path) {
                return Some(b);
            }
        }
    }
    for cand in [
        "/usr/lib64/libpdfium.so",
        "/usr/lib/libpdfium.so",
        "/usr/lib/x86_64-linux-gnu/libpdfium.so",
        "/app/lib/libpdfium.so",
    ] {
        if Path::new(cand).exists() {
            if let Ok(b) = Pdfium::bind_to_library(cand) {
                return Some(b);
            }
        }
    }
    Pdfium::bind_to_system_library().ok()
}
