//! Typst typesetting as the device's work: a source compiled, its pages laid out and drawn, its
//! headings read and its PDF written.
//!
//! An app runs sandboxed and typesets nothing itself, so `so.tilo.typst-editor` asks this crate
//! through `engine.call` and `engine.picture`. The app crate (`apps/tl/typst-editor`) reads every
//! answer as JSON into its own records; the process that runs it (`crates/lang/tilolang-process`)
//! keeps the pictures and the PDF in the app's content store and names them as files.
//!
//! The ops the app asks, each documented where it is answered:
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `compile` | `{source, today, now}` | `{ok, elapsed_ms, pages, diagnostics, pdf}` |
//! | `picture` | `{source, page, width, today, now}` | a page drawn as a PNG file |
//! | `outline` | `{source}` | `{headings}` |
//! | `text` | `{source, page, today, now}` | `{cells}` |
//!
//! A picture and a PDF are files the caller keeps: this crate makes the bytes and the caller
//! names them in the content store, so nothing here holds a path or a store.
//!
//! # Rendering and memory
//!
//! The compiled pages are kept between calls, by the source they came from, so a window drawing
//! a document's pages and reading their words typesets it once. [`Engine::compile`] answers from
//! a fresh compile and replaces what is kept; the other ops reuse it while the source stands.
//! Typst's own memo cache is evicted after each compile, so repeated typing does not grow it.

// Page geometry is floats the layout engine states within a page's box, and the rasteriser
// indexes pixels of a bitmap it just made at the page's size. The arithmetic cannot wrap in a
// way a page shows (the box is a positive size well inside i32), so the app-level arithmetic
// lints stand down here as they do in the app this replaces. A panic still ends nothing else:
// `unwrap`, `expect` and indexing stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "page geometry and the pixels of a page's bitmap, all bounded by the page's own box"
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::Serialize;
use typst::diag::{FileError, FileResult, Severity, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{DiagSpanKind, FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_layout::PagedDocument;

mod outline;
mod pages;
mod text;

pub use outline::Heading;
pub use pages::{PageSize, Pages};
pub use text::Cell;

/// The app whose typesetting this is: the one that asks these ops, and no other.
pub const APP: &str = "so.tilo.typst-editor";

/// How much a problem found in a document matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagKind {
    Error,
    Warning,
}

/// One problem the compiler reported, its span resolved to the source's lines and columns,
/// counted from 1.
#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    pub kind: DiagKind,
    pub message: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub end_line: Option<usize>,
    pub end_column: Option<usize>,
    pub hints: Vec<String>,
}

/// What one compile answered: whether it typeset, how long it took, the size and identity of
/// each page, everything the compiler reported, and the PDF a clean compile writes.
#[derive(Clone, Debug, Serialize)]
pub struct Compiled {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub pages: Vec<PageSize>,
    pub diagnostics: Vec<Diagnostic>,
    /// The PDF's bytes, on a clean compile; none when the source does not typeset.
    #[serde(skip)]
    pub pdf: Option<Vec<u8>>,
    /// Why a clean compile could not write its PDF; none when it did.
    pub pdf_trouble: Option<String>,
}

/// The day a document reads as today, and the instant its offset dates are shifted from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    /// The person's date: year, month and day.
    pub today: (i32, u8, u8),
    /// The Unix time now, in seconds.
    pub unix: i64,
}

impl Clock {
    /// The clock `today` (`YYYY-MM-DD`) and `unix` state; `None` when the date is not one.
    #[must_use]
    pub fn at(today: &str, unix: i64) -> Option<Self> {
        let mut parts = today.split('-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        Some(Self {
            today: (year, month, day),
            unix,
        })
    }
}

/// The parts of a [`World`] that do not change: the standard library, the font book and the
/// parsed fonts, built once and shared.
pub struct Parts {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
}

impl Parts {
    /// The shared parts, built once per process.
    pub fn new() -> Arc<Self> {
        static PARTS: OnceLock<Arc<Parts>> = OnceLock::new();
        Arc::clone(PARTS.get_or_init(|| Arc::new(Self::build())))
    }

    fn build() -> Self {
        let mut fonts: Vec<Font> = tilo_typst_fonts::embedded()
            .into_iter()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .collect();
        fonts.extend(system_emoji());
        let book = FontBook::from_fonts(fonts.iter());
        Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
        }
    }
}

/// The system's colour-emoji face, where the platform ships one Typst cannot carry (Windows).
fn system_emoji() -> Vec<Font> {
    #[cfg(windows)]
    {
        if tilo_typst_fonts::BORROW_SYSTEM_EMOJI {
            let mut database = fontdb::Database::new();
            database.load_system_fonts();
            let face = database.faces().find(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name == "Noto Color Emoji")
            });
            if let Some(face) = face
                && let Some(Some(font)) = database.with_face_data(face.id, |data, index| {
                    Font::new(Bytes::new(data.to_vec()), index)
                })
            {
                return vec![font];
            }
        }
        Vec::new()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// One source typeset for one clock: the one file this world serves, at `/main.typ`.
struct TypstWorld {
    parts: Arc<Parts>,
    main_id: FileId,
    main: Source,
    clock: Clock,
}

impl TypstWorld {
    fn new(parts: Arc<Parts>, src: &str, clock: Clock) -> Self {
        let main = Source::detached(src);
        let main_id = main.id();
        Self {
            parts,
            main_id,
            main,
            clock,
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
                let (year, month, day) = self.clock.today;
                Datetime::from_ymd(year, month, day)
            }
            // An offset asks for the UTC date shifted by it.
            Some(offset) => {
                let shifted = self.clock.unix.checked_add(offset.seconds() as i64)?;
                let (year, month, day) = civil_from_days(shifted.div_euclid(86_400));
                Datetime::from_ymd(year, month, day)
            }
        }
    }
}

/// The proleptic Gregorian date of a day count from 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (
        i32::try_from(year).unwrap_or(1970),
        u8::try_from(month).unwrap_or(1),
        u8::try_from(day).unwrap_or(1),
    )
}

/// The last source compiled here, with the pages it laid out: a window drawing a document's
/// pages and reading their words typesets the document once.
struct Last {
    source: String,
    clock: Clock,
    pages: Option<Arc<Pages>>,
    problems: Vec<Diagnostic>,
}

/// The device's typesetting: the shared parts, and the last document compiled.
pub struct Engine {
    parts: Arc<Parts>,
    last: Mutex<Option<Last>>,
}

impl Engine {
    /// The one engine of this process.
    pub fn shared() -> &'static Self {
        static ENGINE: OnceLock<Engine> = OnceLock::new();
        ENGINE.get_or_init(|| Self {
            parts: Parts::new(),
            last: Mutex::new(None),
        })
    }

    /// The source compiled against `clock`, fresh: its pages, everything the compiler reported,
    /// and the PDF a clean compile writes.
    pub fn compile(&self, source: &str, clock: Clock) -> Compiled {
        let started = std::time::Instant::now();
        let world = TypstWorld::new(Arc::clone(&self.parts), source, clock);
        let result = typst::compile::<PagedDocument>(&world);
        let warnings = result.warnings;
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let (pages, pdf, pdf_trouble, ok) = match result.output {
            Ok(document) => {
                let (pdf, pdf_trouble) =
                    match typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default()) {
                        Ok(bytes) => (Some(bytes), None),
                        Err(errors) => (
                            None,
                            Some(if errors.is_empty() {
                                "this device could not write the PDF".to_owned()
                            } else {
                                errors
                                    .iter()
                                    .map(|d| flatten(&world, d).message)
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            }),
                        ),
                    };
                (
                    Some(Arc::new(Pages::new(document.pages().to_vec()))),
                    pdf,
                    pdf_trouble,
                    true,
                )
            }
            Err(errors) => {
                for d in &errors {
                    diagnostics.push(flatten(&world, d));
                }
                (None, None, None, false)
            }
        };
        // Warnings come after errors so a person leads with the blocking issue.
        for d in &warnings {
            diagnostics.push(flatten(&world, d));
        }
        comemo::evict(8);
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let sizes = pages
            .as_ref()
            .map(|pages| pages.sizes().to_vec())
            .unwrap_or_default();
        *held(&self.last) = Some(Last {
            source: source.to_owned(),
            clock,
            pages,
            problems: diagnostics.clone(),
        });
        Compiled {
            ok,
            elapsed_ms,
            pages: sizes,
            diagnostics,
            pdf,
            pdf_trouble,
        }
    }

    /// The size and look of each page this source lays out, from the kept compile.
    pub fn page_sizes(&self, source: &str, clock: Clock) -> Result<Vec<PageSize>, String> {
        let pages = self.pages(source, clock)?;
        Ok(pages.sizes().to_vec())
    }

    /// What the compiler said of this source, blocking problems first: its errors when it does
    /// not typeset, else its warnings.
    pub fn problems(&self, source: &str, clock: Clock) -> Vec<Diagnostic> {
        // Whether it typesets or not, the kept compile holds what the compiler said of it.
        let _ = self.pages(source, clock);
        held(&self.last)
            .as_ref()
            .map_or_else(Vec::new, |last| last.problems.clone())
    }

    /// The pages this source lays out; the kept ones while the source stands, else a fresh
    /// layout that is kept for the next op.
    fn pages(&self, source: &str, clock: Clock) -> Result<Arc<Pages>, String> {
        {
            let held = held(&self.last);
            if let Some(last) = held.as_ref()
                && last.source == source
                && last.clock == clock
            {
                return match &last.pages {
                    Some(pages) => Ok(Arc::clone(pages)),
                    None => Err(refusal(&last.problems)),
                };
            }
        }
        match Self::layout(&self.parts, source, clock) {
            Ok((pages, warnings)) => {
                *held(&self.last) = Some(Last {
                    source: source.to_owned(),
                    clock,
                    pages: Some(Arc::clone(&pages)),
                    problems: warnings,
                });
                Ok(pages)
            }
            Err(problems) => {
                let why = refusal(&problems);
                *held(&self.last) = Some(Last {
                    source: source.to_owned(),
                    clock,
                    pages: None,
                    problems,
                });
                Err(why)
            }
        }
    }

    fn layout(
        parts: &Arc<Parts>,
        source: &str,
        clock: Clock,
    ) -> Result<(Arc<Pages>, Vec<Diagnostic>), Vec<Diagnostic>> {
        let world = TypstWorld::new(Arc::clone(parts), source, clock);
        let result = typst::compile::<PagedDocument>(&world);
        match result.output {
            Ok(document) => {
                let warnings = result.warnings.iter().map(|d| flatten(&world, d)).collect();
                comemo::evict(8);
                Ok((Arc::new(Pages::new(document.pages().to_vec())), warnings))
            }
            Err(errors) => {
                comemo::evict(8);
                Err(errors.iter().map(|d| flatten(&world, d)).collect())
            }
        }
    }

    /// The headings this source writes, read from its syntax without typesetting it.
    #[must_use]
    pub fn outline(&self, source: &str) -> Vec<Heading> {
        outline::headings(source)
    }

    /// One page drawn as a PNG `width` pixels wide, its height following the page's shape.
    pub fn picture(
        &self,
        source: &str,
        page: usize,
        width: u32,
        clock: Clock,
    ) -> Result<Vec<u8>, String> {
        let pages = self.pages(source, clock)?;
        pages.png(page, width)
    }

    /// One page's placed glyphs, in reading order, in the page's points.
    pub fn text(&self, source: &str, page: usize, clock: Clock) -> Result<Vec<Cell>, String> {
        let pages = self.pages(source, clock)?;
        pages
            .cells(page)
            .map(<[Cell]>::to_vec)
            .ok_or_else(|| format!("the document has no page {page}"))
    }
}

fn held(last: &Mutex<Option<Last>>) -> MutexGuard<'_, Option<Last>> {
    last.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Why a document does not typeset, in the first blocking problem's words.
fn refusal(problems: &[Diagnostic]) -> String {
    problems
        .iter()
        .find(|problem| problem.kind == DiagKind::Error)
        .or_else(|| problems.first())
        .map_or_else(
            || "the document does not typeset".to_owned(),
            |problem| problem.message.clone(),
        )
}

/// Flatten a `typst` diagnostic into this crate's plain record, resolving its span to a 1-based
/// line and column in the main source when it has one.
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
    let (line, column, end_line, end_column) = match range {
        Some(r) => {
            let (sl, sc) = byte_to_line_col(text, r.start);
            // Mark at least one column so a zero-width span (an insertion point) still shows.
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
        column,
        end_line,
        end_column,
        hints: d.hints.iter().map(|hint| hint.v.to_string()).collect(),
    }
}

/// A byte offset as a (1-based line, 1-based column) pair, floored to a char boundary so a span
/// end inside a multi-byte code point never slices mid-character.
fn byte_to_line_col(text: &str, byte: usize) -> (usize, usize) {
    let mut byte = byte.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let before = text.get(..byte).unwrap_or(text);
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |s| s.chars().count()) + 1;
    (line, column)
}

#[cfg(test)]
mod tests;
