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
use std::sync::Arc;

use anyhow::{anyhow, Context};
use pdfium_render::prelude::*;
use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat};

use crate::model::{AnnoKind, Annotation, Comment, Document, OutlineItem, PtRect, Signature};
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

/// A retained pdfium document that rasterises pages on demand, extracts their
/// text layer, and can flatten markup back onto a saved copy.
///
/// The `Pdfium` handle is leaked to `'static` so the borrowed [`PdfDocument`]
/// can be stored alongside it; there is exactly one per opened file and it
/// lives for the process, which is the intended lifetime. The original bytes
/// are retained so a save can re-open a pristine copy and flatten onto it
/// without disturbing the on-screen render.
pub struct Rasterizer {
    pdfium: &'static Pdfium,
    doc: PdfDocument<'static>,
    original_bytes: Arc<Vec<u8>>,
    page_sizes: Vec<(f64, f64)>,
    /// Rendered bitmaps keyed by (page index, target-width bucket in px).
    cache: HashMap<(usize, u32), PenikoImage>,
    /// Per-page extracted text layer, filled lazily on first request.
    text_cache: HashMap<usize, Arc<PageText>>,
}

impl Rasterizer {
    fn open(bytes: Vec<u8>) -> Option<Self> {
        let bindings = find_pdfium()?;
        // One process-lifetime handle; leaking avoids a self-referential struct.
        let pdfium: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
        let original = Arc::new(bytes.clone());
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
        Some(Rasterizer {
            pdfium,
            doc,
            original_bytes: original,
            page_sizes,
            cache: HashMap::new(),
            text_cache: HashMap::new(),
        })
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn first_page_size(&self) -> (f64, f64) {
        self.page_sizes.first().copied().unwrap_or((612.0, 792.0))
    }

    /// The page's height in PDF points (needed to flip pdfium's bottom-left,
    /// y-up coordinates into the reader's top-left, y-down space).
    fn page_height(&self, page_idx: usize) -> f64 {
        self.page_sizes.get(page_idx).map(|s| s.1).unwrap_or(792.0)
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

    /// The extracted text layer for page `page_idx` (0-based), cached. Empty
    /// when the page has no extractable text (e.g. a scanned image).
    pub fn page_text(&mut self, page_idx: usize) -> Arc<PageText> {
        if let Some(pt) = self.text_cache.get(&page_idx) {
            return pt.clone();
        }
        let pt = Arc::new(self.extract_page_text(page_idx).unwrap_or_default());
        self.text_cache.insert(page_idx, pt.clone());
        pt
    }

    /// Pull every glyph on `page_idx` out of pdfium as a (character,
    /// page-point box) pair, flipping into the reader's top-left space.
    fn extract_page_text(&self, page_idx: usize) -> Option<PageText> {
        if page_idx >= self.page_sizes.len() {
            return None;
        }
        let page_h = self.page_height(page_idx);
        let page = self.doc.pages().get(page_idx as u16).ok()?;
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
                    let left = b.left().value as f64;
                    let right = b.right().value as f64;
                    let top_pdf = b.top().value as f64;
                    let bottom_pdf = b.bottom().value as f64;
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

    /// The number of content objects on `page_idx` in the *saved-on-disk*
    /// pdfium document. Used by the save-back round-trip test to prove
    /// flattened markup lands as real page content.
    pub fn page_object_count(&self, page_idx: usize) -> Option<usize> {
        let page = self.doc.pages().get(page_idx as u16).ok()?;
        Some(page.objects().len())
    }

    /// Flatten the reader's markup onto a **fresh** copy of the document and
    /// write it to `out` as a valid PDF. Highlights become translucent filled
    /// rectangles, ink becomes stroked paths, sticky notes become a coloured
    /// marker plus their comment text, and signatures become a handwriting
    /// stroke with the signer's name + date — all as ordinary page content, so
    /// the saved file reopens with the markup baked in for any viewer.
    ///
    /// Operates on a re-opened copy of the original bytes, leaving the
    /// on-screen render untouched.
    pub fn save_annotated(
        &self,
        annotations: &[Annotation],
        comments: &[Comment],
        signatures: &[Signature],
        out: &Path,
    ) -> anyhow::Result<()> {
        let mut doc = self
            .pdfium
            .load_pdf_from_byte_vec((*self.original_bytes).clone(), None)
            .map_err(|e| anyhow!("re-opening the document to flatten markup: {e:?}"))?;

        let font = doc.fonts_mut().helvetica();
        let font_italic = doc.fonts_mut().helvetica_oblique();

        let n = self.page_sizes.len();
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
                            path.line_to(x, y).map_err(|e| anyhow!("ink line_to: {e:?}"))?;
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
        // Handwriting stroke sampled from the on-screen signature glyph so the
        // saved mark echoes what the reader drew.
        let stroke_area = PtRect::new(
            r.x + r.w * 0.06,
            r.y + r.h * 0.12,
            r.w * 0.88,
            r.h * 0.46,
        );
        let poly = signature_polyline(s.style, &stroke_area);
        if poly.len() >= 2 {
            let (x0, y0) = pt_tl_to_pdf(poly[0].0, poly[0].1, page_h);
            let mut path = PdfPagePathObject::new(
                doc,
                x0,
                y0,
                Some(PdfColor::new(28, 33, 47, 255)),
                Some(PdfPoints::new(1.6)),
                None,
            )
            .map_err(|e| anyhow!("starting signature stroke: {e:?}"))?;
            for p in &poly[1..] {
                let (x, y) = pt_tl_to_pdf(p.0, p.1, page_h);
                path.line_to(x, y).map_err(|e| anyhow!("signature line_to: {e:?}"))?;
            }
            page.objects_mut()
                .add_path_object(path)
                .map_err(|e| anyhow!("adding signature stroke: {e:?}"))?;
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
    (PdfPoints::new(x as f32), PdfPoints::new((page_h - y) as f32))
}

/// Sample the styled signature "handwriting" curve into a polyline in
/// page-point (top-left) space. Mirrors the four bundled bezier styles drawn
/// on screen in `render::draw_signature_glyph`, so a flattened signature looks
/// like the placed one. Sampling to line segments keeps the flatten path free
/// of coordinate-order pitfalls.
fn signature_polyline(style: u8, area: &PtRect) -> Vec<(f64, f64)> {
    // Control geometry in a normalised [0,1]×[0,1] box (y increases downward),
    // as cubic segments `[(c1),(c2),(end)]` following a start point.
    let (start, segs): ((f64, f64), Vec<[(f64, f64); 3]>) = match style % 4 {
        0 => (
            (0.02, 0.7),
            vec![
                [(0.10, 0.0), (0.18, 1.0), (0.26, 0.4)],
                [(0.34, -0.1), (0.40, 0.9), (0.50, 0.5)],
                [(0.62, 0.05), (0.72, 1.0), (0.86, 0.35)],
                [(0.92, 0.15), (0.98, 0.6), (1.0, 0.45)],
            ],
        ),
        1 => (
            (0.0, 0.55),
            vec![
                [(0.15, -0.2), (0.30, 1.1), (0.45, 0.5)],
                [(0.55, 0.1), (0.60, 0.95), (0.70, 0.55)],
                [(0.80, 0.2), (0.95, 0.8), (1.0, 0.4)],
            ],
        ),
        2 => (
            (0.02, 0.5),
            // Quadratics from the on-screen style, promoted to cubics.
            vec![
                quad_to_cubic((0.02, 0.5), (0.2, -0.05), (0.38, 0.6)),
                quad_to_cubic((0.38, 0.6), (0.52, 1.05), (0.66, 0.45)),
                quad_to_cubic((0.66, 0.45), (0.82, -0.05), (1.0, 0.5)),
            ],
        ),
        _ => (
            (0.0, 0.6),
            vec![
                [(0.2, 0.0), (0.3, 0.9), (0.5, 0.45)],
                [(0.7, 0.0), (0.85, 1.0), (1.0, 0.5)],
            ],
        ),
    };

    let map = |p: (f64, f64)| (area.x + p.0 * area.w, area.y + p.1 * area.h);
    let mut out = vec![map(start)];
    let mut p0 = start;
    for seg in segs {
        let [c1, c2, end] = seg;
        // Sample the cubic at a handful of steps for a smooth-enough stroke.
        for k in 1..=10 {
            let t = k as f64 / 10.0;
            out.push(map(cubic(p0, c1, c2, end, t)));
        }
        p0 = end;
    }
    out
}

/// Promote a quadratic Bézier (control `q`) from `p0` to `p1` into an
/// equivalent cubic `[(c1),(c2),(end)]`.
fn quad_to_cubic(p0: (f64, f64), q: (f64, f64), p1: (f64, f64)) -> [(f64, f64); 3] {
    let c1 = (p0.0 + 2.0 / 3.0 * (q.0 - p0.0), p0.1 + 2.0 / 3.0 * (q.1 - p0.1));
    let c2 = (p1.0 + 2.0 / 3.0 * (q.0 - p1.0), p1.1 + 2.0 / 3.0 * (q.1 - p1.1));
    [c1, c2, p1]
}

/// Evaluate a cubic Bézier at parameter `t`.
fn cubic(p0: (f64, f64), c1: (f64, f64), c2: (f64, f64), p1: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let c = 3.0 * u * t * t;
    let d = t * t * t;
    (
        a * p0.0 + b * c1.0 + c * c2.0 + d * p1.0,
        a * p0.1 + b * c1.1 + c * c2.1 + d * p1.1,
    )
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
        if Path::new(&cand).exists() {
            if let Ok(b) = Pdfium::bind_to_library(&cand) {
                return Some(b);
            }
        }
    }
    Pdfium::bind_to_system_library().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_to_cubic_preserves_endpoints() {
        let seg = quad_to_cubic((0.0, 0.0), (0.5, 1.0), (1.0, 0.0));
        assert_eq!(seg[2], (1.0, 0.0), "the cubic ends where the quad ends");
    }

    #[test]
    fn signature_polyline_stays_within_a_padded_area() {
        let area = PtRect::new(100.0, 200.0, 180.0, 40.0);
        for style in 0u8..4 {
            let poly = signature_polyline(style, &area);
            assert!(poly.len() > 10, "style {style} samples a real stroke");
            // Points stay near the area (curves overshoot a little vertically).
            for (x, y) in poly {
                assert!(x >= area.x - 1.0 && x <= area.x + area.w + 1.0, "x in range");
                assert!(y >= area.y - area.h && y <= area.y + 2.0 * area.h, "y near range");
            }
        }
    }

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
