//! Typst compilation + raster rendering.
//!
//! This module owns the entire interface to the `typst` crate. The rest of
//! the app never touches a `typst::*` type directly — it sees only the plain
//! [`CompileOutput`] (decoded page bitmaps + flattened diagnostics), which is
//! `Send` and trivially cacheable in widget state.
//!
//! A minimal [`World`] implementation backs compilation. We need only one
//! in-memory source file (`/main.typ`) plus the fonts embedded by
//! `typst-assets` (plus the shared Windows emoji face). In the browser the
//! same fonts arrive as host data after the editor opens, keeping 9 MiB out
//! of the page's first download. There is no package resolution, no
//! filesystem, and the only clock is the toolkit's (`ui::clock`), so a capture
//! shows the same date on every run.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use typst::diag::{FileError, FileResult, Severity, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::layout::{Frame, FrameItem, Point as TypstPoint, Transform};
use typst::syntax::{DiagSpanKind, FileId, Source};
use typst::text::{Font, FontBook, TextItem};
use typst::utils::{LazyHash, Scalar};
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;
use vello::kurbo::{Affine, Point};

/// Severity of a flattened diagnostic — decoupled from `typst::Severity` so
/// downstream widgets need no typst dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagKind {
    Error,
    Warning,
}

/// One compiler message, already resolved to a 1-based line/column in the
/// main source (when the span points into it).
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub kind: DiagKind,
    pub message: String,
    /// 1-based line number of the span start in the main source, if resolvable.
    pub line: Option<usize>,
    /// 1-based column (in chars) of the span start, if resolvable.
    pub col: Option<usize>,
    /// 1-based line number of the span end (inclusive of the marked range), if
    /// resolvable. Equals `line` for a single-line span.
    pub end_line: Option<usize>,
    /// 1-based column (in chars) just past the last marked character, if
    /// resolvable. Always `>= col` on the same line so the inline squiggle has
    /// a positive width.
    pub end_col: Option<usize>,
    pub hints: Vec<String>,
}

/// A single rendered page: straight-alpha RGBA8, composited over white so it
/// reads as paper regardless of the document's page fill.
#[derive(Clone)]
pub struct PageImage {
    pub rgba: Arc<[u8]>,
    /// Bitmap size in pixels (intrinsic to `pixel_per_pt`).
    pub width: u32,
    pub height: u32,
    /// Page size in typographic points — the resolution-independent unit the
    /// preview lays pages out in, so zoom scales display without re-rendering.
    pub width_pt: f64,
    pub height_pt: f64,
}

/// A run of shaped text on a page, in **page points**, carrying one selectable
/// cell per glyph so the preview can hit-test a drag to a character boundary
/// and copy the covered text. Extracted from the compiled `typst` frame — the
/// same layout the raster is rendered from — so positions line up exactly.
#[derive(Clone, Debug)]
pub struct TextRun {
    /// Top / bottom of the run's line box in page points (y grows downward).
    pub top: f64,
    pub bottom: f64,
    pub cells: Vec<TextCell>,
}

/// One selectable glyph cell within a [`TextRun`], in page points.
#[derive(Clone, Debug)]
pub struct TextCell {
    pub x0: f64,
    pub x1: f64,
    /// The source characters this glyph renders (may be empty for a ligature
    /// continuation, or several chars for a combined glyph).
    pub text: String,
}

/// The full result of one compile pass.
#[derive(Clone)]
pub struct CompileOutput {
    pub pages: Vec<PageImage>,
    /// Extracted selectable text, one entry per page (parallel to `pages`).
    pub page_text: Vec<Vec<TextRun>>,
    pub diagnostics: Vec<Diagnostic>,
    /// Wall-clock compile+render time in milliseconds.
    pub elapsed_ms: u128,
    /// True when compilation produced a document (even with warnings); false
    /// when a fatal error stopped layout.
    pub ok: bool,
}

impl CompileOutput {
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.kind == DiagKind::Error)
            .count()
    }
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.kind == DiagKind::Warning)
            .count()
    }
}

/// Whether a [`WorldParts`] has every font it will ever have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fonts {
    Ready,
    /// The host is still fetching some; build again once it relayouts.
    Loading,
    /// The host could not supply some; compile with the rest.
    Failed(String),
}

/// Typst's bundled fonts: embedded in native builds, and in a page fetched
/// from the host by name when the editor opens (`tilo_typst_fonts`).
mod fonts {
    use typst::foundations::Bytes;
    use typst::text::Font;

    use super::Fonts;

    /// The bundled fonts available now.
    pub fn bundled() -> (Vec<Font>, Fonts) {
        if tilo_typst_fonts::EMBEDDED {
            (embedded(), Fonts::Ready)
        } else {
            from_host()
        }
    }

    #[cfg(feature = "thin")]
    fn embedded() -> Vec<Font> {
        runtime_dylib::typst_fonts()
    }

    #[cfg(not(feature = "thin"))]
    fn embedded() -> Vec<Font> {
        tilo_typst_fonts::embedded()
            .into_iter()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect()
    }

    fn from_host() -> (Vec<Font>, Fonts) {
        let mut fonts = Vec::new();
        let mut state = Fonts::Ready;
        for name in tilo_typst_fonts::HOST_FONTS {
            match ui::host_data::request(name) {
                ui::host_data::Status::Ready(bytes) => fonts.extend(Font::iter(Bytes::new(bytes))),
                ui::host_data::Status::Pending => state = Fonts::Loading,
                ui::host_data::Status::Failed(reason) => {
                    if state == Fonts::Ready {
                        state = Fonts::Failed(format!("{name}: {reason}"));
                    }
                }
            }
        }
        (fonts, state)
    }

    pub fn loading() -> bool {
        !tilo_typst_fonts::EMBEDDED
            && tilo_typst_fonts::HOST_FONTS
                .iter()
                .any(|name| ui::host_data::request(name) == ui::host_data::Status::Pending)
    }
}

/// Immutable, reusable pieces of a [`World`]: the standard library, the font
/// book, and the parsed fonts. Built once (parsing every embedded font is the
/// expensive part) and shared by `Arc` across every compile.
pub struct WorldParts {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    state: Fonts,
}

impl WorldParts {
    /// Build the default library and font book once per process, then share
    /// their immutable data across documents and compilation workers. While
    /// the host is still supplying fonts the parts are rebuilt on each call
    /// and kept only once complete.
    pub fn new() -> Arc<Self> {
        static PARTS: OnceLock<Arc<WorldParts>> = OnceLock::new();
        if let Some(parts) = PARTS.get() {
            return Arc::clone(parts);
        }
        let parts = Arc::new(Self::build());
        if parts.state == Fonts::Loading {
            return parts;
        }
        Arc::clone(PARTS.get_or_init(|| parts))
    }

    pub fn fonts(&self) -> &Fonts {
        &self.state
    }

    /// Whether the host is still fetching bundled fonts: cheap, so a caller
    /// can wait for it before building again.
    pub fn fonts_loading() -> bool {
        fonts::loading()
    }

    fn build() -> Self {
        let (mut fonts, state) = fonts::bundled();
        if tilo_typst_fonts::BORROW_SYSTEM_EMOJI {
            // The toolkit already validates and shares the installed font
            // catalog. Reuse its emoji face instead of bundling a second
            // copy or relying on machine-specific system font discovery.
            let system = ui::text_render::configured_font_system();
            let database = system.db();
            if let Some(face) = database.faces().find(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name == "Noto Color Emoji")
            }) && let Some(Some(font)) = database.with_face_data(face.id, |data, index| {
                Font::new(Bytes::new(data.to_vec()), index)
            }) {
                fonts.push(font);
            }
        }
        let book = FontBook::from_fonts(fonts.iter());
        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            state,
        }
    }
}

/// Per-compile world: shares the heavy [`WorldParts`] by `Arc`, owns only the
/// one in-memory source file.
struct TypstWorld {
    parts: Arc<WorldParts>,
    main_id: FileId,
    main: Source,
}

impl TypstWorld {
    fn new(parts: Arc<WorldParts>, src: &str) -> Self {
        // `/main.typ` in the project root: the one file this world serves.
        let main = Source::detached(src);
        let main_id = main.id();
        Self {
            parts,
            main_id,
            main,
        }
    }
}

impl World for TypstWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.parts.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.parts.book
    }
    fn main(&self) -> FileId {
        self.main_id
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main_id {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotFound(PathBuf::from(
                id.vpath().get_without_slash(),
            )))
        }
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main_id {
            Ok(Bytes::new(self.main.text().to_string().into_bytes()))
        } else {
            Err(FileError::NotFound(PathBuf::from(
                id.vpath().get_without_slash(),
            )))
        }
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.parts.fonts.get(index).cloned()
    }
    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        match offset {
            // No offset asks for the date on the person's own clock.
            None => {
                let local = ui::clock::local_now().format("%Y-%m-%d").to_string();
                let mut parts = local.split('-').filter_map(|part| part.parse::<i32>().ok());
                let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
                Datetime::from_ymd(year, u8::try_from(month).ok()?, u8::try_from(day).ok()?)
            }
            // An offset asks for the UTC date shifted by it.
            Some(offset) => {
                let seconds = i64::try_from(ui::time::try_unix_now()?).ok()?;
                let shifted = seconds.checked_add(offset.seconds() as i64)?;
                let (year, month, day) = civil_from_days(shifted.div_euclid(86_400));
                Datetime::from_ymd(year, month, day)
            }
        }
    }
}

/// The proleptic Gregorian `(year, month, day)` of `days` since 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        i32::try_from(year).unwrap_or(1970),
        u8::try_from(month).unwrap_or(1),
        u8::try_from(day).unwrap_or(1),
    )
}

/// Compile `src` and rasterise every page at `pixel_per_pt`. Never panics on
/// bad input — compiler errors are returned as [`Diagnostic`]s with an empty
/// page list.
pub fn compile_source(parts: &Arc<WorldParts>, src: &str, pixel_per_pt: f32) -> CompileOutput {
    let started = ui::time::now();
    let world = TypstWorld::new(parts.clone(), src);

    let result = typst::compile::<PagedDocument>(&world);
    let warnings = result.warnings;

    let render_options = typst_render::RenderOptions {
        pixel_per_pt: Scalar::new(f64::from(pixel_per_pt)),
        render_bleed: false,
    };
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let (pages, page_text, ok) = match result.output {
        Ok(document) => {
            let mut imgs = Vec::with_capacity(document.pages().len());
            let mut texts = Vec::with_capacity(document.pages().len());
            for page in document.pages() {
                let pixmap = typst_render::render(page, &render_options);
                let size = page.frame.size();
                let mut img = pixmap_to_rgba(&pixmap);
                img.width_pt = size.x.to_pt();
                img.height_pt = size.y.to_pt();
                imgs.push(img);
                texts.push(extract_page_text(&page.frame));
            }
            (imgs, texts, true)
        }
        Err(errors) => {
            for d in &errors {
                diagnostics.push(flatten(&world, d));
            }
            (Vec::new(), Vec::new(), false)
        }
    };

    // Warnings come after errors so the panel leads with the blocking issue.
    for d in &warnings {
        diagnostics.push(flatten(&world, d));
    }

    // Bound comemo's global memo cache so repeated compiles don't grow RSS
    // unbounded — typst's own CLI does the same after each compile.
    comemo::evict(8);

    CompileOutput {
        pages,
        page_text,
        diagnostics,
        elapsed_ms: started.elapsed().as_millis(),
        ok,
    }
}

/// Compile `src` to a PDF byte buffer, reusing the same [`World`] the raster
/// preview is built from so the exported file is identical to what's on screen.
/// On a clean compile the returned bytes are a complete PDF (they begin with the
/// `%PDF` magic). A fatal compile error — or a failure inside the PDF backend —
/// comes back as the flattened [`Diagnostic`]s so the caller can surface them.
///
/// # Errors
/// Returns the flattened compiler [`Diagnostic`]s when `src` fails to compile,
/// or when the PDF backend fails on an otherwise-valid document.
pub fn compile_pdf(parts: &Arc<WorldParts>, src: &str) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let world = TypstWorld::new(parts.clone(), src);
    let flatten_all = |errors: &[SourceDiagnostic]| -> Vec<Diagnostic> {
        errors.iter().map(|d| flatten(&world, d)).collect()
    };
    let document = typst::compile::<PagedDocument>(&world)
        .output
        .map_err(|errors| flatten_all(&errors))?;
    let options = typst_pdf::PdfOptions::default();
    let bytes = typst_pdf::pdf(&document, &options).map_err(|errors| flatten_all(&errors))?;
    comemo::evict(8);
    Ok(bytes)
}

/// Walk a compiled page frame and pull out every run of shaped text with its
/// per-glyph geometry, in page points. Nested groups are followed with their
/// transform composed in, so text placed inside `#align`, `#box`, tables, etc.
/// still lands at the right spot.
fn extract_page_text(frame: &Frame) -> Vec<TextRun> {
    let mut runs = Vec::new();
    walk_frame(frame, Affine::IDENTITY, &mut runs);
    runs
}

fn walk_frame(frame: &Frame, cur: Affine, out: &mut Vec<TextRun>) {
    for (pos, item) in frame.items() {
        match item {
            FrameItem::Group(group) => {
                let child = cur
                    * Affine::translate((pos.x.to_pt(), pos.y.to_pt()))
                    * transform_to_affine(&group.transform);
                walk_frame(&group.frame, child, out);
            }
            FrameItem::Text(text) => {
                if let Some(run) = build_run(*pos, text, cur) {
                    out.push(run);
                }
            }
            _ => {}
        }
    }
}

/// A typst affine transform → a kurbo one (same row-major layout typst-render
/// uses to feed tiny-skia, so geometry matches the raster).
fn transform_to_affine(t: &Transform) -> Affine {
    Affine::new([
        t.sx.get(),
        t.ky.get(),
        t.kx.get(),
        t.sy.get(),
        t.tx.to_pt(),
        t.ty.to_pt(),
    ])
}

/// Build a [`TextRun`] for one shaped `TextItem` placed at `pos` inside a frame
/// whose local→page transform is `cur`. Glyph advances accumulate along the
/// baseline; the line box comes from the font's ascender/descender so cells on
/// adjacent lines don't vertically overlap. Rotation/shear is not modelled
/// (documents lay text axis-aligned); the x/y scale of `cur` is honoured.
fn build_run(pos: TypstPoint, item: &TextItem, cur: Affine) -> Option<TextRun> {
    if item.glyphs.is_empty() {
        return None;
    }
    let size = item.size;
    let metrics = item.font.metrics();
    let ascent = metrics.ascender.at(size).to_pt();
    let descent = metrics.descender.at(size).to_pt(); // negative (below baseline)

    let coeffs = cur.as_coeffs();
    let sx = coeffs[0].hypot(coeffs[1]).max(1e-6); // |x-basis| of `cur`
    let sy = coeffs[2].hypot(coeffs[3]).max(1e-6); // |y-basis| of `cur`
    let origin = cur * Point::new(pos.x.to_pt(), pos.y.to_pt());

    let mut cells = Vec::with_capacity(item.glyphs.len());
    let mut cum = 0.0f64;
    for g in &item.glyphs {
        let adv = g.x_advance.at(size).to_pt();
        let x0 = origin.x + cum * sx;
        let x1 = origin.x + (cum + adv) * sx;
        cum += adv;
        let range = g.range.start as usize..g.range.end as usize;
        let text = item.text.get(range).unwrap_or("").to_string();
        cells.push(TextCell { x0, x1, text });
    }

    Some(TextRun {
        top: origin.y - ascent * sy,
        bottom: origin.y - descent * sy,
        cells,
    })
}

/// Flatten a `typst` diagnostic into our plain struct, resolving the span to a
/// 1-based line/column in the main source when possible.
fn flatten(world: &TypstWorld, d: &SourceDiagnostic) -> Diagnostic {
    let kind = match d.severity {
        Severity::Error => DiagKind::Error,
        Severity::Warning => DiagKind::Warning,
    };
    let text = world.main.text();
    let range = match d.span.get() {
        DiagSpanKind::Number { id, num, sub_range } if id == world.main_id => {
            world.main.range(num, sub_range)
        }
        DiagSpanKind::Range { id, range } if id == world.main_id => Some(range),
        _ => None,
    };
    let (line, col, end_line, end_col) = match range {
        Some(r) => {
            let (sl, sc) = byte_to_line_col(text, r.start);
            // Mark at least one column so a zero-width span (e.g. an insertion
            // point) still gets a visible squiggle.
            let end = r.end.max(r.start + 1);
            let (el, ec) = byte_to_line_col(text, end);
            (Some(sl), Some(sc), Some(el), Some(ec))
        }
        None => (None, None, None, None),
    };
    Diagnostic {
        kind,
        message: d.message.to_string(),
        line,
        col,
        end_line,
        end_col,
        hints: d.hints.iter().map(|hint| hint.v.to_string()).collect(),
    }
}

/// Convert a byte offset into a (1-based line, 1-based column) pair. The offset
/// is floored to the nearest char boundary first so a span end that lands
/// inside a multi-byte code point never slices mid-character.
fn byte_to_line_col(text: &str, byte: usize) -> (usize, usize) {
    let mut byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let before = text.get(..byte).unwrap_or(text);
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |s| s.chars().count()) + 1;
    (line, col)
}

/// tiny-skia stores premultiplied RGBA8. Un-premultiply and composite over
/// white so the page reads as opaque paper. `pixmap.data()` is RGBA order.
///
/// `tiny-skia` is pinned to the same `0.12` series typst-render uses, so the
/// `Pixmap` type returned by `typst_render::render` unifies with this one.
fn pixmap_to_rgba(pixmap: &tiny_skia::Pixmap) -> PageImage {
    let width = pixmap.width();
    let height = pixmap.height();
    let src = pixmap.data();
    let mut out = vec![0u8; (width as usize) * (height as usize) * 4];
    for (dst, px) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let (&[r, g, b, a], [d0, d1, d2, d3]) = (px, dst) else {
            // `chunks_exact(4)` guarantees length 4 on both sides; this branch
            // is unreachable, but skipping the pixel beats indexing blind.
            continue;
        };
        // Composite premultiplied colour over white: out = premul + (255 - a).
        let inv = 255 - a;
        *d0 = r.saturating_add(inv);
        *d1 = g.saturating_add(inv);
        *d2 = b.saturating_add(inv);
        *d3 = 255;
    }
    PageImage {
        rgba: Arc::from(out),
        width,
        height,
        // Filled in by the caller from the page frame; a sane default keeps the
        // aspect correct if ever constructed without a frame.
        width_pt: f64::from(width),
        height_pt: f64::from(height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_extracts_text_glyphs_and_pdf_bytes() {
        // Case `windows_compilation_uses_real_emoji_glyphs`.
        #[cfg(target_os = "windows")]
        {
            let parts = WorldParts::new();
            let world = TypstWorld::new(parts, "= Native Grün 🌍");
            let output = typst::compile::<PagedDocument>(&world)
                .output
                .expect("emoji document compiles");
            let mut frames: Vec<_> = output.pages.iter().map(|page| &page.frame).collect();
            let mut glyph_count = 0;
            while let Some(frame) = frames.pop() {
                for (_, item) in frame.items() {
                    match item {
                        FrameItem::Group(group) => frames.push(&group.frame),
                        FrameItem::Text(text) => {
                            glyph_count += text.glyphs.len();
                            assert!(
                                text.glyphs.iter().all(|glyph| glyph.id != 0),
                                "compiled text contains a missing glyph"
                            );
                        }
                        _ => {}
                    }
                }
            }
            assert!(glyph_count > 0, "the production compiler emitted no text");
        }
        // Case `compile_extracts_positioned_text`:
        // A clean compile renders a page, records its point size, and extracts
        // selectable glyph cells whose left-to-right x positions increase — the
        // geometry the preview's text selection hit-tests against.
        {
            let parts = WorldParts::new();
            let out = compile_source(&parts, "= Hello\n\nSome body text here.\n", 2.0);
            assert!(out.ok, "clean source compiles");
            assert_eq!(out.error_count(), 0);
            assert!(!out.pages.is_empty(), "produced a page");
            assert!(
                out.pages[0].width_pt > 0.0 && out.pages[0].height_pt > 0.0,
                "point size set"
            );

            let cells: usize = out
                .page_text
                .iter()
                .flat_map(|p| p.iter())
                .map(|r| r.cells.len())
                .sum();
            assert!(cells > 0, "extracted glyph cells for the text layer");

            // Within any run, glyph cells advance left-to-right.
            for run in out.page_text.iter().flat_map(|p| p.iter()) {
                for w in run.cells.windows(2) {
                    assert!(w[1].x0 + 1e-6 >= w[0].x0, "cells advance rightward");
                }
                assert!(run.bottom > run.top, "run line box has positive height");
            }
        }
        // Case `compile_pdf_produces_pdf_bytes`:
        // Exporting a clean document yields a real PDF: the byte stream opens with
        // the `%PDF` magic and carries substantive content.
        {
            let parts = WorldParts::new();
            let bytes = compile_pdf(&parts, "= Export me\n\nA body paragraph to typeset.\n")
                .expect("a clean source exports to PDF");
            assert!(
                bytes.starts_with(b"%PDF"),
                "exported bytes begin with the %PDF magic (got {:?})",
                &bytes[..bytes.len().min(8)]
            );
            assert!(
                bytes.len() > 400,
                "a real single-page PDF is more than a stub"
            );
        }
    }

    #[test]
    fn the_document_can_read_today_from_the_toolkit_clock() {
        // Case `datetime_today_compiles`:
        // A document asking for today's date (the bundled letter does) compiles
        // cleanly instead of failing on a world without a clock.
        {
            let parts = WorldParts::new();
            let out = compile_source(&parts, "Dated #datetime.today().display()\n", 1.0);
            assert_eq!(out.error_count(), 0, "{:?}", out.diagnostics);
            assert!(!out.pages.is_empty());
        }
        // Case `civil_date_from_days`:
        // The shifted UTC date of an explicit offset is the proleptic
        // Gregorian date of the day count.
        {
            assert_eq!(civil_from_days(0), (1970, 1, 1));
            assert_eq!(civil_from_days(-1), (1969, 12, 31));
            assert_eq!(civil_from_days(11_016), (2000, 2, 29), "a leap day");
            assert_eq!(civil_from_days(20_634), (2026, 6, 30));
        }
    }

    #[test]
    fn compile_errors_carry_spans_and_both_severities() {
        // Case `error_carries_resolvable_span`:
        // A reference to an undefined variable is surfaced as an error with a
        // resolvable span whose end column is strictly past the start.
        {
            let parts = WorldParts::new();
            let out = compile_source(&parts, "Text #missing_var more.\n", 2.0);
            assert!(out.error_count() >= 1, "unknown variable is an error");
            let d = out
                .diagnostics
                .iter()
                .find(|d| d.kind == DiagKind::Error)
                .expect("an error diagnostic");
            assert!(d.line.is_some() && d.col.is_some(), "start resolved");
            assert!(d.end_line.is_some() && d.end_col.is_some(), "end resolved");
            assert!(
                d.end_col.unwrap() > d.col.unwrap(),
                "span has positive width: {:?}..{:?}",
                d.col,
                d.end_col
            );
            assert_eq!(d.line, Some(1), "on the first line");
        }
        // Case `compile_pdf_reports_errors_instead_of_writing_garbage`:
        // A source that can't compile can't export — the PDF path reports the same
        // blocking error the preview would show, not a truncated/empty file.
        {
            let parts = WorldParts::new();
            let err = compile_pdf(&parts, "Body #undefined_var end.\n")
                .expect_err("a broken source must not produce PDF bytes");
            assert!(
                err.iter().any(|d| d.kind == DiagKind::Error),
                "the blocking error is surfaced to the caller"
            );
        }
        // Case `error_and_warning_fixture_surfaces_both_severities`:
        // The bundled error+warning fixture drives the whole diagnostics surface:
        // the real compiler returns **both** an error and a warning, each resolved
        // to a distinct source line so the panel, the inline squiggles, and
        // click-to-jump all have real spans to act on.
        {
            let parts = WorldParts::new();
            let out = compile_source(&parts, crate::samples::ERROR_WARNING, 2.0);
            assert!(out.error_count() >= 1, "fixture carries a blocking error");
            assert!(out.warning_count() >= 1, "fixture carries a warning");

            let err = out
                .diagnostics
                .iter()
                .find(|d| d.kind == DiagKind::Error)
                .expect("an error diagnostic");
            let warn = out
                .diagnostics
                .iter()
                .find(|d| d.kind == DiagKind::Warning)
                .expect("a warning diagnostic");
            assert!(
                err.line.is_some() && err.col.is_some(),
                "error span resolved"
            );
            assert!(
                warn.line.is_some() && warn.col.is_some(),
                "warning span resolved"
            );
            assert_ne!(
                err.line, warn.line,
                "the two diagnostics sit on different lines so a jump is observable"
            );
            assert_eq!(warn.line, Some(1), "the font warning is on line 1");
        }
    }
}
