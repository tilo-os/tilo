//! Loading real PDF files — and writing markup back into them.
//!
//! Two backends, chosen at open time with a graceful fallback:
//!
//!  * **Raster** — when a `pdfium` native library can be located, each page is
//!    rasterised on demand to an RGBA bitmap at the current zoom and painted
//!    directly, so the viewer shows the document exactly as authored (fonts,
//!    figures, vector art and all). Bitmaps are cached per page + zoom bucket
//!    so scrolling and panning never re-rasterise. The raster backend also
//!    extracts a real **text layer** ([`crate::text_layer`]) per page from
//!    pdfium, so search finds genuine hits and text can be selected, and it can
//!    **flatten** the reader's annotations + signatures back onto the pages and
//!    save a valid `.pdf` reopenable in any viewer.
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
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Emit a per-stage open timing line to stderr when `QOS_PDF_PROFILE` is set.
/// Zero cost when the env var is unset (the format args are only built then).
#[inline]
fn prof(stage: &str, since: Instant) {
    if std::env::var_os("QOS_PDF_PROFILE").is_some() {
        eprintln!(
            "[pdf-profile] {stage}: {:.1}ms",
            since.elapsed().as_secs_f64() * 1000.0
        );
    }
}
fn profiling() -> bool {
    std::env::var_os("QOS_PDF_PROFILE").is_some()
}

use anyhow::{Context, anyhow};
use pdfium_render::prelude::*;
use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat};

use crate::model::{
    AnnoKind, Annotation, Comment, Document, OutlineItem, PtRect, Signature, SignatureInk,
};
use crate::text_layer::PageText;

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
    let t = Instant::now();
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    prof("fs::read", t);
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

    // Hand the freshly-read bytes straight to pdfium (it takes ownership). The
    // previous `bytes.clone()` here doubled memory and, on a large file, cost a
    // full-buffer memcpy on the open→first-paint path (~140ms for a 365MB PDF);
    // the pure-Rust text fallback below is rare (no pdfium, or an unreadable
    // file), so it re-reads the file rather than the raster path paying to keep
    // a spare copy every open.
    let t = Instant::now();
    let opened = Rasterizer::open(bytes);
    prof("Rasterizer::open (pdfium parse + page sizes)", t);
    if let Some(r) = opened {
        let t = Instant::now();
        let (mut title, author, subject) = r.metadata();
        prof("metadata", t);
        if title.trim().is_empty() {
            title = stem;
        }
        let (pw, ph) = r.first_page_size();
        let t = Instant::now();
        let mut doc = Document::blank_pages(&title, &author, r.page_count(), pw, ph);
        prof("Document::blank_pages", t);
        doc.subject = subject;
        let t = Instant::now();
        doc.outline = r.outline();
        prof("outline (bookmarks)", t);
        return Ok(LoadedDoc {
            doc,
            raster: Some(r),
            name,
        });
    }

    // No native library (or an unreadable-by-pdfium file) — extract the text so
    // the words are still real. `bytes` was moved into the raster attempt above,
    // so re-read the file for this rare path.
    let bytes = std::fs::read(path).with_context(|| format!("re-reading {}", path.display()))?;
    let pages = pdf_extract::extract_text_from_mem_by_pages(&bytes)
        .map_err(|e| anyhow!("pdf text extraction failed: {e:?}"))?;
    let doc = Document::from_text_pages(&stem, "", &pages);
    Ok(LoadedDoc {
        doc,
        raster: None,
        name,
    })
}

/// A retained pdfium document that rasterises pages on demand, extracts their
/// text layer, and can flatten markup back onto a saved copy.
///
/// The `Pdfium` handle is leaked to `'static` so the borrowed [`PdfDocument`]
/// can be stored alongside it; there is exactly one per opened file and it
/// lives for the process, which is the intended lifetime. The original bytes
/// are retained so a save can re-open a pristine copy and flatten onto it
/// without disturbing the on-screen render.
/// One loaded pdfium document backing some of the displayed pages. Source 0 is
/// always the originally-opened file; imports (other PDFs, or images promoted to
/// a one-page PDF) are appended as further sources.
struct Source {
    doc: PdfDocument<'static>,
    page_sizes: Vec<(f64, f64)>,
}

/// A displayed page: which loaded [`Source`] it comes from and the page index
/// within that source. The ordered list of these — the *page plan* — is the
/// editable model: reordering, deleting, duplicating and importing pages all
/// permute this vector, and a save copies the source pages in plan order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PageSlot {
    source: usize,
    page: usize,
}

pub struct Rasterizer {
    pdfium: &'static Pdfium,
    /// Loaded pdfium documents; index 0 is the original file.
    sources: Vec<Source>,
    /// The ordered, editable list of displayed pages.
    plan: Vec<PageSlot>,
    /// Rendered bitmaps keyed by (source, source-page, target-width bucket).
    cache: HashMap<(usize, usize, u32), PenikoImage>,
    /// Per-source-page extracted text layer, filled lazily on first request.
    text_cache: HashMap<(usize, usize), Arc<PageText>>,
}

/// The process-wide pdfium handle, bound exactly once.
///
/// pdfium's `FPDF_InitLibrary` must run once per process, and pdfium-render's
/// default thread-safety marshalling takes a global `Mutex` on that first init
/// and only releases it on `FPDF_DestroyLibrary`. A process-lifetime handle is
/// never destroyed, so a *second* `Pdfium` would call `FPDF_InitLibrary` again
/// and block forever waiting for that never-released lock. Binding once and
/// sharing the handle sidesteps this entirely — opening any number of PDFs (and
/// the save-back reopen) uses the same library. Our pdfium access is confined
/// to the UI thread, so the single shared handle is never touched concurrently.
fn shared_pdfium() -> Option<&'static Pdfium> {
    // `Pdfium` wraps a raw library handle and is `!Sync`, so it can't sit in a
    // `static` directly. Leak one behind the `OnceLock` (it lives for the whole
    // process anyway) and cache its address as a `usize`, which is `Sync`. The
    // `OnceLock` guarantees the leak happens exactly once; `0` records "no
    // pdfium available" so the fallback text backend still engages.
    static PDFIUM_ADDR: OnceLock<usize> = OnceLock::new();
    let addr = *PDFIUM_ADDR.get_or_init(|| match find_pdfium() {
        Some(bindings) => {
            let handle: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
            std::ptr::from_ref::<Pdfium>(handle) as usize
        }
        None => 0,
    });
    if addr == 0 {
        None
    } else {
        // SAFETY: `addr` is the address of a `Box::leak`'d `Pdfium` that lives
        // for the entire process, so the reference is valid for `'static`.
        Some(unsafe { &*(addr as *const Pdfium) })
    }
}

impl Rasterizer {
    fn load_source(pdfium: &'static Pdfium, bytes: Vec<u8>) -> Option<Source> {
        let t = Instant::now();
        let doc = pdfium.load_pdf_from_byte_vec(bytes, None).ok()?;
        prof("  load_pdf_from_byte_vec", t);
        let t = Instant::now();
        let mut page_sizes = Vec::new();
        let pages = doc.pages();
        for i in 0..pages.len() {
            if let Ok(rect) = pages.page_size(i) {
                page_sizes.push((
                    f64::from(rect.width().value),
                    f64::from(rect.height().value),
                ));
            } else {
                page_sizes.push((612.0, 792.0));
            }
        }
        if profiling() {
            eprintln!(
                "[pdf-profile]   page_size loop ({} pages): {:.1}ms",
                page_sizes.len(),
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        if page_sizes.is_empty() {
            return None;
        }
        Some(Source { doc, page_sizes })
    }

    fn open(bytes: Vec<u8>) -> Option<Self> {
        let pdfium = shared_pdfium()?;
        let source = Self::load_source(pdfium, bytes)?;
        let plan = (0..source.page_sizes.len())
            .map(|page| PageSlot { source: 0, page })
            .collect();
        Some(Rasterizer {
            pdfium,
            sources: vec![source],
            plan,
            cache: HashMap::new(),
            text_cache: HashMap::new(),
        })
    }

    /// Number of displayed pages (the length of the editable page plan).
    pub fn page_count(&self) -> usize {
        self.plan.len()
    }

    /// The source [`Source`] and source-page index backing displayed page
    /// `display_idx`.
    fn slot(&self, display_idx: usize) -> Option<PageSlot> {
        self.plan.get(display_idx).copied()
    }

    pub fn first_page_size(&self) -> (f64, f64) {
        self.plan
            .first()
            .and_then(|s| self.sources[s.source].page_sizes.get(s.page).copied())
            .unwrap_or((612.0, 792.0))
    }

    /// Size of displayed page `display_idx`, in PDF points.
    pub fn page_size(&self, display_idx: usize) -> (f64, f64) {
        self.slot(display_idx)
            .and_then(|s| self.sources[s.source].page_sizes.get(s.page).copied())
            .unwrap_or_else(|| self.first_page_size())
    }

    /// The displayed page's height in PDF points (needed to flip pdfium's
    /// bottom-left, y-up coordinates into the reader's top-left, y-down space).
    fn page_height(&self, display_idx: usize) -> f64 {
        self.page_size(display_idx).1
    }

    // ---- page editing -----------------------------------------------

    /// Move displayed page `from` to final index `to` (matching a
    /// [`ui::widget::page_sidebar::move_item`]-style move). Content, text and
    /// bitmap caches are keyed by source page, so no re-render is needed.
    pub fn reorder_page(&mut self, from: usize, to: usize) {
        if from == to || from >= self.plan.len() || to >= self.plan.len() {
            return;
        }
        let slot = self.plan.remove(from);
        self.plan.insert(to, slot);
    }

    /// Remove displayed page `idx`. Keeps at least one page.
    pub fn delete_page(&mut self, idx: usize) {
        if self.plan.len() > 1 && idx < self.plan.len() {
            self.plan.remove(idx);
        }
    }

    /// Duplicate displayed page `idx`, inserting the copy right after it. The
    /// duplicate points at the same source page (cheap; shares the render cache).
    pub fn duplicate_page(&mut self, idx: usize) {
        if let Some(slot) = self.slot(idx) {
            self.plan.insert(idx + 1, slot);
        }
    }

    /// Import every page of the PDF in `bytes`, inserting them starting at
    /// displayed index `at`. Returns the number of pages added.
    pub fn import_pdf(&mut self, bytes: Vec<u8>, at: usize) -> anyhow::Result<usize> {
        let source = Self::load_source(self.pdfium, bytes)
            .ok_or_else(|| anyhow!("the file is not a readable PDF"))?;
        let n = source.page_sizes.len();
        let src_idx = self.sources.len();
        self.sources.push(source);
        let at = at.min(self.plan.len());
        for page in 0..n {
            self.plan.insert(
                at + page,
                PageSlot {
                    source: src_idx,
                    page,
                },
            );
        }
        Ok(n)
    }

    /// Import an image (`bytes` = PNG/JPEG/… file contents) as a new page,
    /// inserted at displayed index `at`. The image is placed to fill a page
    /// sized to its own aspect ratio.
    pub fn import_image(&mut self, bytes: &[u8], at: usize) -> anyhow::Result<()> {
        let img = image::load_from_memory(bytes).map_err(|e| anyhow!("decoding the image: {e}"))?;
        let (iw, ih) = (
            f64::from(img.width().max(1)),
            f64::from(img.height().max(1)),
        );
        // A page 612pt wide (US-Letter width) with the image's aspect ratio.
        let pw = 612.0f64;
        let ph = (pw * ih / iw).clamp(72.0, 3000.0);
        let mut doc = self
            .pdfium
            .create_new_pdf()
            .map_err(|e| anyhow!("creating the image page document: {e:?}"))?;
        {
            let mut page = doc
                .pages_mut()
                .create_page_at_start(PdfPagePaperSize::Custom(
                    PdfPoints::new(pw as f32),
                    PdfPoints::new(ph as f32),
                ))
                .map_err(|e| anyhow!("creating the image page: {e:?}"))?;
            page.objects_mut()
                .create_image_object(
                    PdfPoints::new(0.0),
                    PdfPoints::new(0.0),
                    &img,
                    Some(PdfPoints::new(pw as f32)),
                    Some(PdfPoints::new(ph as f32)),
                )
                .map_err(|e| anyhow!("placing the image on the page: {e:?}"))?;
            let _ = page.regenerate_content();
        }
        let src_idx = self.sources.len();
        self.sources.push(Source {
            doc,
            page_sizes: vec![(pw, ph)],
        });
        let at = at.min(self.plan.len());
        self.plan.insert(
            at,
            PageSlot {
                source: src_idx,
                page: 0,
            },
        );
        Ok(())
    }

    fn metadata(&self) -> (String, String, String) {
        let meta = self.sources[0].doc.metadata();
        let get = |tag| {
            meta.get(tag)
                .map(|t| t.value().to_string())
                .unwrap_or_default()
        };
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
        for bm in self.sources[0].doc.bookmarks().iter() {
            let Some(title) = bm.title() else { continue };
            let page = bm
                .destination()
                .and_then(|d| d.page_index().ok())
                .map_or(1, |i| i as usize + 1);
            if !title.trim().is_empty() {
                out.push(OutlineItem {
                    title,
                    page,
                    level: 0,
                });
            }
            if out.len() >= 200 {
                break;
            }
        }
        out
    }

    /// The extracted text layer for displayed page `display_idx`, cached. Empty
    /// when the page has no extractable text (e.g. a scanned image).
    pub fn page_text(&mut self, display_idx: usize) -> Arc<PageText> {
        let Some(slot) = self.slot(display_idx) else {
            return Arc::new(PageText::default());
        };
        let key = (slot.source, slot.page);
        if let Some(pt) = self.text_cache.get(&key) {
            return pt.clone();
        }
        let pt = Arc::new(self.extract_page_text(slot).unwrap_or_default());
        self.text_cache.insert(key, pt.clone());
        pt
    }

    /// Pull every glyph on the given source page out of pdfium as a (character,
    /// page-point box) pair, flipping into the reader's top-left space.
    fn extract_page_text(&self, slot: PageSlot) -> Option<PageText> {
        let src = self.sources.get(slot.source)?;
        let page_h = src.page_sizes.get(slot.page).map_or(792.0, |s| s.1);
        let page = src.doc.pages().get(slot.page as u16).ok()?;
        let text = page.text().ok()?;
        // Bind `chars` to a local: `PdfPageTextChars::iter` borrows it, so it
        // must outlive the loop (iterating `text.chars().iter()` directly would
        // borrow a dropped temporary).
        let chars = text.chars();
        let mut glyphs: Vec<(char, PtRect)> = Vec::new();
        for ch in chars.iter() {
            let Some(c) = ch.unicode_char() else { continue };
            if c == '\0' {
                continue;
            }
            // `loose_bounds` covers the glyph's advance box (spaces included);
            // a failure (rare — some synthetic glyphs) yields a zero box, which
            // the text layer keeps for search continuity but skips when drawing
            // bands.
            let rect = match ch.loose_bounds() {
                Ok(b) => {
                    let left = f64::from(b.left().value);
                    let right = f64::from(b.right().value);
                    let top_pdf = f64::from(b.top().value);
                    let bottom_pdf = f64::from(b.bottom().value);
                    PtRect::new(
                        left,
                        page_h - top_pdf,
                        (right - left).max(0.0),
                        (top_pdf - bottom_pdf).max(0.0),
                    )
                }
                Err(_) => PtRect::new(0.0, 0.0, 0.0, 0.0),
            };
            glyphs.push((c, rect));
        }
        Some(PageText::from_glyphs(glyphs))
    }

    /// Rasterise displayed page `display_idx` at roughly `target_w` device
    /// pixels wide, returning a cached bitmap. Widths are snapped to a bucket so
    /// a small zoom nudge reuses the existing render instead of re-rasterising.
    /// The cache is keyed by source page, so a duplicated or reordered page
    /// never re-rasterises.
    /// The width bucket `render` snaps `target_w` to. Exposed so the draw loop
    /// can probe the cache (`is_page_cached`) with the exact key `render` uses.
    fn bucket_for(target_w: u32) -> u32 {
        ((target_w / 24).max(1) * 24).clamp(96, 4096)
    }

    /// Whether displayed page `display_idx` already has a cached bitmap at the
    /// bucket `render(display_idx, target_w)` would use — i.e. drawing it this
    /// frame is a free cache hit rather than a fresh (expensive) rasterisation.
    pub fn is_page_cached(&self, display_idx: usize, target_w: u32) -> bool {
        match self.slot(display_idx) {
            Some(slot) => {
                self.cache
                    .contains_key(&(slot.source, slot.page, Self::bucket_for(target_w)))
            }
            None => false,
        }
    }

    /// Test-only: how many *full-page* bitmaps are cached (buckets above the
    /// thumbnail ceiling). Lets a regression test assert the first paint only
    /// rasterises the budgeted number of pages instead of a screen-full.
    #[cfg(test)]
    pub fn cached_full_page_count(&self) -> usize {
        const THUMB_MAX: u32 = 600;
        self.cache.keys().filter(|(_, _, b)| *b > THUMB_MAX).count()
    }

    pub fn render(&mut self, display_idx: usize, target_w: u32) -> Option<PenikoImage> {
        let slot = self.slot(display_idx)?;
        let bucket = Self::bucket_for(target_w);
        let key = (slot.source, slot.page, bucket);
        if let Some(img) = self.cache.get(&key) {
            return Some(img.clone());
        }
        let t = Instant::now();
        let page = self
            .sources
            .get(slot.source)?
            .doc
            .pages()
            .get(slot.page as u16)
            .ok()?;
        let config = PdfRenderConfig::new().set_target_width(bucket as Pixels);
        let bitmap = page.render_with_config(&config).ok()?;
        if profiling() {
            eprintln!(
                "[pdf-profile]   render page {} @ {}px: {:.1}ms",
                display_idx + 1,
                bucket,
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
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
        // Cache keys store the *snapped* bucket (`bucket_for`), so snap the
        // keep-width the same way. Comparing against the raw width would match
        // nothing — silently evicting the current page every frame and forcing
        // a full re-rasterise on the very next one (the bug that made a page
        // re-render on every idle/scroll frame instead of staying cached).
        let keep = Self::bucket_for(keep_bucket);
        self.cache
            .retain(|(_, _, b), _| *b == keep || *b <= THUMB_MAX);
    }

    /// The number of content objects on displayed page `display_idx` in the
    /// backing pdfium document. Used by the save-back round-trip test to prove
    /// flattened markup lands as real page content.
    pub fn page_object_count(&self, display_idx: usize) -> Option<usize> {
        let slot = self.slot(display_idx)?;
        let page = self
            .sources
            .get(slot.source)?
            .doc
            .pages()
            .get(slot.page as u16)
            .ok()?;
        Some(page.objects().len())
    }

    /// Build the edited document — the pages in the current plan order,
    /// including imported PDF/image pages — flatten the reader's markup onto it,
    /// and write it to `out` as a valid PDF. Highlights become translucent
    /// filled rectangles, ink becomes stroked paths, sticky notes become a
    /// coloured marker plus their comment text, and signatures become their
    /// real ink — the drawn strokes, or the typed name as a script — plus the
    /// signer name + date, all as ordinary page content, so the saved file
    /// reopens with the edits + markup baked in for any viewer.
    ///
    /// Operates on a freshly-created document, leaving the on-screen render and
    /// the loaded sources untouched.
    pub fn save_annotated(
        &self,
        annotations: &[Annotation],
        comments: &[Comment],
        signatures: &[Signature],
        out: &Path,
    ) -> anyhow::Result<()> {
        let mut doc = self
            .pdfium
            .create_new_pdf()
            .map_err(|e| anyhow!("creating the output document: {e:?}"))?;

        // Copy every planned page, in order, from its source document. This is
        // what makes reorder / delete / duplicate / import persist on save.
        for (dest, slot) in self.plan.iter().enumerate() {
            let src = self
                .sources
                .get(slot.source)
                .ok_or_else(|| anyhow!("page plan references a missing source"))?;
            doc.pages_mut()
                .copy_page_from_document(&src.doc, slot.page as PdfPageIndex, dest as PdfPageIndex)
                .map_err(|e| anyhow!("copying page {} into the output: {e:?}", dest + 1))?;
        }

        let font = doc.fonts_mut().helvetica();
        let font_italic = doc.fonts_mut().helvetica_oblique();

        let n = self.plan.len();
        for idx in 0..n {
            let page_no = idx + 1;
            let page_h = self.page_height(idx);
            let mut page = doc
                .pages()
                .get(idx as u16)
                .map_err(|e| anyhow!("opening page {page_no} to flatten: {e:?}"))?;

            for a in annotations.iter().filter(|a| a.page == page_no) {
                let (r8, g8, b8) = a.color.rgb();
                match &a.kind {
                    AnnoKind::Highlight { rect } => {
                        let pr = rect_tl_to_pdf(rect, page_h);
                        page.objects_mut()
                            .create_path_object_rect(
                                pr,
                                None,
                                None,
                                Some(PdfColor::new(r8, g8, b8, 90)),
                            )
                            .map_err(|e| anyhow!("flattening highlight: {e:?}"))?;
                    }
                    AnnoKind::Ink { points } if points.len() >= 2 => {
                        let stroke = PdfColor::new(r8, g8, b8, 255);
                        let (x0, y0) = pt_tl_to_pdf(points[0].0, points[0].1, page_h);
                        let mut path = PdfPagePathObject::new(
                            &doc,
                            x0,
                            y0,
                            Some(stroke),
                            Some(PdfPoints::new(2.0)),
                            None,
                        )
                        .map_err(|e| anyhow!("starting ink path: {e:?}"))?;
                        for p in &points[1..] {
                            let (x, y) = pt_tl_to_pdf(p.0, p.1, page_h);
                            path.line_to(x, y)
                                .map_err(|e| anyhow!("ink line_to: {e:?}"))?;
                        }
                        page.objects_mut()
                            .add_path_object(path)
                            .map_err(|e| anyhow!("adding ink path: {e:?}"))?;
                    }
                    AnnoKind::Note { x, y } => {
                        // A coloured marker square...
                        let marker = PtRect::new(*x, *y, 16.0, 16.0);
                        page.objects_mut()
                            .create_path_object_rect(
                                rect_tl_to_pdf(&marker, page_h),
                                Some(PdfColor::new(60, 60, 60, 200)),
                                Some(PdfPoints::new(0.75)),
                                Some(PdfColor::new(r8, g8, b8, 235)),
                            )
                            .map_err(|e| anyhow!("flattening note marker: {e:?}"))?;
                        // ...plus the linked comment's text beside it, so the
                        // note's content survives into the saved file.
                        if let Some(body) = comments
                            .iter()
                            .find(|c| c.anno_id.as_deref() == Some(a.id.as_str()))
                            .map(|c| c.body.trim())
                            .filter(|b| !b.is_empty())
                        {
                            let (tx, ty) = pt_tl_to_pdf(*x + 20.0, *y + 12.0, page_h);
                            page.objects_mut()
                                .create_text_object(tx, ty, body, font, PdfPoints::new(9.0))
                                .map_err(|e| anyhow!("flattening note text: {e:?}"))?;
                        }
                    }
                    _ => {}
                }
            }

            for s in signatures.iter().filter(|s| s.page == page_no) {
                self.flatten_signature(&doc, &mut page, page_h, s, font, font_italic)?;
            }

            let _ = page.regenerate_content();
        }

        doc.save_to_file(out)
            .map_err(|e| anyhow!("saving flattened PDF to {}: {e:?}", out.display()))?;
        Ok(())
    }

    /// Flatten one signature: its handwriting stroke, a baseline, and the
    /// signer name + date as real (extractable) text.
    fn flatten_signature(
        &self,
        doc: &PdfDocument<'static>,
        page: &mut PdfPage<'static>,
        page_h: f64,
        s: &Signature,
        font: PdfFontToken,
        font_italic: PdfFontToken,
    ) -> anyhow::Result<()> {
        let r = &s.rect;
        let stroke_area = PtRect::new(r.x + r.w * 0.06, r.y + r.h * 0.10, r.w * 0.88, r.h * 0.46);
        // Flatten the signature's *real* ink onto the page: the freehand strokes
        // the user drew (scaled from normalised field space), or their typed name
        // as a large italic script. Both land as ordinary, extractable page
        // content so the saved PDF looks the same in any viewer.
        match &s.ink {
            SignatureInk::Drawn { strokes } => {
                let ink_col = PdfColor::new(28, 33, 47, 255);
                for stroke in strokes {
                    if stroke.len() < 2 {
                        continue;
                    }
                    let map = |nx: f64, ny: f64| {
                        let x = stroke_area.x + nx.clamp(0.0, 1.0) * stroke_area.w;
                        let y = stroke_area.y + ny.clamp(0.0, 1.0) * stroke_area.h;
                        pt_tl_to_pdf(x, y, page_h)
                    };
                    let (x0, y0) = map(stroke[0].0, stroke[0].1);
                    let mut path = PdfPagePathObject::new(
                        doc,
                        x0,
                        y0,
                        Some(ink_col),
                        Some(PdfPoints::new(1.6)),
                        None,
                    )
                    .map_err(|e| anyhow!("starting signature stroke: {e:?}"))?;
                    for p in &stroke[1..] {
                        let (x, y) = map(p.0, p.1);
                        path.line_to(x, y)
                            .map_err(|e| anyhow!("signature line_to: {e:?}"))?;
                    }
                    page.objects_mut()
                        .add_path_object(path)
                        .map_err(|e| anyhow!("adding signature stroke: {e:?}"))?;
                }
            }
            SignatureInk::Typed => {
                let name = s.signer.trim();
                if !name.is_empty() {
                    let size = (stroke_area.h * 0.72).clamp(10.0, 22.0);
                    let (tx, ty) =
                        pt_tl_to_pdf(stroke_area.x, stroke_area.y + stroke_area.h * 0.85, page_h);
                    page.objects_mut()
                        .create_text_object(tx, ty, name, font_italic, PdfPoints::new(size as f32))
                        .map_err(|e| anyhow!("signature script: {e:?}"))?;
                }
            }
        }

        // Baseline under the name.
        let base_y = r.y + r.h * 0.62;
        let (lx0, ly) = pt_tl_to_pdf(r.x + 8.0, base_y, page_h);
        let (lx1, _) = pt_tl_to_pdf(r.x + r.w - 8.0, base_y, page_h);
        page.objects_mut()
            .create_path_object_line(
                lx0,
                ly,
                lx1,
                ly,
                PdfColor::new(148, 163, 184, 255),
                PdfPoints::new(0.75),
            )
            .map_err(|e| anyhow!("signature baseline: {e:?}"))?;

        // Signer name (oblique, reads as a signature) + date, both extractable.
        if !s.signer.trim().is_empty() {
            let (nx, ny) = pt_tl_to_pdf(r.x + 8.0, base_y + 12.0, page_h);
            page.objects_mut()
                .create_text_object(nx, ny, s.signer.trim(), font_italic, PdfPoints::new(10.0))
                .map_err(|e| anyhow!("signature name: {e:?}"))?;
        }
        let date_label = format!("Signed {}", s.date);
        let (dx, dy) = pt_tl_to_pdf(r.x + 8.0, base_y + 24.0, page_h);
        page.objects_mut()
            .create_text_object(dx, dy, date_label, font, PdfPoints::new(7.5))
            .map_err(|e| anyhow!("signature date: {e:?}"))?;
        Ok(())
    }
}

/// Convert a page-point rectangle (origin top-left, y-down) into a pdfium
/// [`PdfRect`] (origin bottom-left, y-up).
fn rect_tl_to_pdf(r: &PtRect, page_h: f64) -> PdfRect {
    let left = r.x as f32;
    let right = (r.x + r.w) as f32;
    let top = (page_h - r.y) as f32;
    let bottom = (page_h - (r.y + r.h)) as f32;
    PdfRect::new(
        PdfPoints::new(bottom),
        PdfPoints::new(left),
        PdfPoints::new(top),
        PdfPoints::new(right),
    )
}

/// Convert a page-point coordinate (origin top-left, y-down) into pdfium's
/// bottom-left, y-up point pair.
fn pt_tl_to_pdf(x: f64, y: f64, page_h: f64) -> (PdfPoints, PdfPoints) {
    (
        PdfPoints::new(x as f32),
        PdfPoints::new((page_h - y) as f32),
    )
}

/// Locate a usable pdfium library without ever bundling one.
fn find_pdfium() -> Option<Box<dyn PdfiumLibraryBindings>> {
    if let Ok(path) = std::env::var("QOS_PDFIUM_LIB")
        && !path.is_empty()
        && let Ok(b) = Pdfium::bind_to_library(&path)
    {
        return Some(b);
    }
    let mut candidates: Vec<String> = vec![
        "/usr/lib64/libpdfium.so".into(),
        "/usr/lib/libpdfium.so".into(),
        "/usr/lib/x86_64-linux-gnu/libpdfium.so".into(),
        "/app/lib/libpdfium.so".into(),
    ];
    // A common per-user install location (`~/.local/lib/pdfium/libpdfium.so`).
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(format!("{home}/.local/lib/pdfium/libpdfium.so"));
    }
    for cand in candidates {
        if Path::new(&cand).exists()
            && let Ok(b) = Pdfium::bind_to_library(&cand)
        {
            return Some(b);
        }
    }
    Pdfium::bind_to_system_library().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_tl_to_pdf_flips_the_y_axis() {
        // A 10pt-tall box 5pt from the top of an 800pt page sits 785..795 in
        // pdfium's bottom-left space.
        let r = PtRect::new(20.0, 5.0, 30.0, 10.0);
        let pr = rect_tl_to_pdf(&r, 800.0);
        assert!((pr.left().value - 20.0).abs() < 0.01);
        assert!((pr.right().value - 50.0).abs() < 0.01);
        assert!((pr.top().value - 795.0).abs() < 0.01);
        assert!((pr.bottom().value - 785.0).abs() < 0.01);
    }
}
