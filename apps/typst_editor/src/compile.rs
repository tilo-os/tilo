//! Typst compilation + raster rendering.
//!
//! This module owns the entire interface to the `typst` crate. The rest of
//! the app never touches a `typst::*` type directly — it sees only the plain
//! [`CompileOutput`] (decoded page bitmaps + flattened diagnostics), which is
//! `Send` and trivially cacheable in widget state.
//!
//! A minimal [`World`] implementation backs compilation. We need only one
//! in-memory source file (`/main.typ`) plus the fonts embedded by
//! `typst-assets`, so the implementation is deliberately tiny: no package
//! resolution, no filesystem, no clock.

use std::sync::Arc;

use typst::diag::{FileError, FileResult, Severity, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime};
use typst::layout::PagedDocument;
use typst::syntax::{FileId, Source, VirtualPath};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};

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
    /// 1-based line number in the main source, if resolvable.
    pub line: Option<usize>,
    /// 1-based column (in chars), if resolvable.
    pub col: Option<usize>,
    pub hints: Vec<String>,
}

/// A single rendered page: straight-alpha RGBA8, composited over white so it
/// reads as paper regardless of the document's page fill.
#[derive(Clone)]
pub struct PageImage {
    pub rgba: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
}

/// The full result of one compile pass.
#[derive(Clone)]
pub struct CompileOutput {
    pub pages: Vec<PageImage>,
    pub diagnostics: Vec<Diagnostic>,
    /// Wall-clock compile+render time in milliseconds.
    pub elapsed_ms: u128,
    /// True when compilation produced a document (even with warnings); false
    /// when a fatal error stopped layout.
    pub ok: bool,
}

impl CompileOutput {
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.kind == DiagKind::Error).count()
    }
    pub fn warning_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.kind == DiagKind::Warning).count()
    }
}

/// Immutable, reusable pieces of a [`World`]: the standard library, the font
/// book, and the parsed fonts. Built once (parsing every embedded font is the
/// expensive part) and shared by `Arc` across every compile.
pub struct WorldParts {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

impl WorldParts {
    /// Parse every font embedded by `typst-assets` and build the standard
    /// library + font book. Cheap to share afterwards (`Arc`).
    pub fn new() -> Arc<Self> {
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        let book = FontBook::from_fonts(fonts.iter());
        Arc::new(Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
        })
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
        let main_id = FileId::new(None, VirtualPath::new("/main.typ"));
        let main = Source::new(main_id, src.to_string());
        Self { parts, main_id, main }
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
            Err(FileError::NotFound(
                id.vpath().as_rootless_path().to_path_buf(),
            ))
        }
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.main_id {
            Ok(Bytes::new(self.main.text().to_string().into_bytes()))
        } else {
            Err(FileError::NotFound(
                id.vpath().as_rootless_path().to_path_buf(),
            ))
        }
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.parts.fonts.get(index).cloned()
    }
    fn today(&self, _offset: Option<i64>) -> Option<Datetime> {
        None
    }
}

/// Compile `src` and rasterise every page at `pixel_per_pt`. Never panics on
/// bad input — compiler errors are returned as [`Diagnostic`]s with an empty
/// page list.
pub fn compile_source(parts: &Arc<WorldParts>, src: &str, pixel_per_pt: f32) -> CompileOutput {
    let started = std::time::Instant::now();
    let world = TypstWorld::new(parts.clone(), src);

    let result = typst::compile::<PagedDocument>(&world);
    let warnings = result.warnings;

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let (pages, ok) = match result.output {
        Ok(document) => {
            let mut imgs = Vec::with_capacity(document.pages.len());
            for page in &document.pages {
                let pixmap = typst_render::render(page, pixel_per_pt);
                imgs.push(pixmap_to_rgba(&pixmap));
            }
            (imgs, true)
        }
        Err(errors) => {
            for d in &errors {
                diagnostics.push(flatten(&world, d));
            }
            (Vec::new(), false)
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
        diagnostics,
        elapsed_ms: started.elapsed().as_millis(),
        ok,
    }
}

/// Flatten a `typst` diagnostic into our plain struct, resolving the span to a
/// 1-based line/column in the main source when possible.
fn flatten(world: &TypstWorld, d: &SourceDiagnostic) -> Diagnostic {
    let kind = match d.severity {
        Severity::Error => DiagKind::Error,
        Severity::Warning => DiagKind::Warning,
    };
    let (line, col) = world
        .main
        .range(d.span)
        .map(|r| byte_to_line_col(world.main.text(), r.start))
        .map(|(l, c)| (Some(l), Some(c)))
        .unwrap_or((None, None));
    Diagnostic {
        kind,
        message: d.message.to_string(),
        line,
        col,
        hints: d.hints.iter().map(|h| h.to_string()).collect(),
    }
}

/// Convert a byte offset into a (1-based line, 1-based column) pair.
fn byte_to_line_col(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let before = &text[..byte];
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = before.rsplit('\n').next().map(|s| s.chars().count()).unwrap_or(0) + 1;
    (line, col)
}

/// tiny-skia stores premultiplied RGBA8. Un-premultiply and composite over
/// white so the page reads as opaque paper. `pixmap.data()` is RGBA order.
///
/// `tiny-skia` is pinned to the same `0.11` series typst-render uses, so the
/// `Pixmap` type returned by `typst_render::render` unifies with this one.
fn pixmap_to_rgba(pixmap: &tiny_skia::Pixmap) -> PageImage {
    let w = pixmap.width();
    let h = pixmap.height();
    let src = pixmap.data();
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for (i, px) in src.chunks_exact(4).enumerate() {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        // Composite premultiplied colour over white: out = premul + (255 - a).
        let inv = 255 - a;
        out[i * 4] = r.saturating_add(inv);
        out[i * 4 + 1] = g.saturating_add(inv);
        out[i * 4 + 2] = b.saturating_add(inv);
        out[i * 4 + 3] = 255;
    }
    PageImage {
        rgba: Arc::from(out),
        width: w,
        height: h,
    }
}
