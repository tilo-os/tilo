//! Loading real PDF files — and writing markup back into them.
//!
//! Every page is rendered on demand by Tilo's PDF engine ([`tilo_pdf`]) to an
//! RGBA bitmap at the current zoom and painted directly, so the viewer shows
//! the document exactly as authored (fonts, figures, vector art and all). The
//! engine parses documents in its sandboxed helper process; this module never
//! touches PDF bytes beyond handing them over. Bitmaps are cached per page +
//! zoom bucket so scrolling and panning never re-render. The backend also
//! fetches the engine's analysed **text layer** ([`crate::text_layer`]) per
//! page, so search finds text hits and text can be selected, and it can
//! **export** the reader's markup as real PDF annotations into a saved copy
//! reopenable in any viewer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use ui::time::Instant;

/// Emit a per-stage open timing line to stderr when `TILO_PDF_PROFILE` is set.
/// Zero cost when the env var is unset (the format args are only built then).
#[inline]
fn prof(stage: &str, since: Instant) {
    if std::env::var_os("TILO_PDF_PROFILE").is_some() {
        eprintln!(
            "[pdf-profile] {stage}: {:.1}ms",
            since.elapsed().as_secs_f64() * 1000.0
        );
    }
}
fn profiling() -> bool {
    std::env::var_os("TILO_PDF_PROFILE").is_some()
}

use anyhow::{Context, anyhow};
use tilo_content::ContentStore;
use vello::peniko::{Blob, ImageData as PenikoImage};

use crate::markup::{ImportedPage, PagePlanEntry};
use crate::model::{
    AnnoKind, Annotation, Comment, Document, FormEntry, MarkupStyle, OutlineItem, PtRect,
    Signature, SignatureInk,
};
use crate::text_layer::PageText;
use ui::lock::MutexRecover as _;

/// Opening is intentionally bounded before either PDF parser sees the bytes.
/// PDFs can expand dramatically while parsing, but rejecting unexpectedly
/// large inputs up front prevents a chooser selection (or a path-based action)
/// from first allocating an arbitrary file in the UI process.
pub const MAX_OPEN_BYTES: usize = 128 * 1024 * 1024;
/// Import bytes are independently capped: imports are retained in the local
/// content store and can otherwise multiply a reader's working set.
pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;
/// A document with more pages is not practical to materialise as an
/// interactive reader session; the cap also bounds the page-plan projection.
const MAX_SOURCE_PAGES: usize = 2_048;
/// PDF page boxes are supplied by an untrusted file. These bounds leave room
/// for large-format drawings while excluding pathological raster dimensions.
const MAX_PAGE_EDGE_POINTS: f64 = 144_000.0;
const MAX_PAGE_AREA_POINTS: f64 = 2_000_000_000.0;
/// A single page image is never allowed to consume more than this many pixels.
const MAX_RASTER_PIXELS: f64 = 32.0 * 1024.0 * 1024.0;
/// Keep document imagery responsive without letting a long read of a large
/// file turn the process into an unbounded bitmap cache.
const MAX_RASTER_CACHE_BYTES: usize = 192 * 1024 * 1024;

fn read_file_bounded(path: &Path, limit: usize) -> anyhow::Result<Vec<u8>> {
    let (dir, name) =
        tilo_files::file_at(path).with_context(|| format!("reading {}", path.display()))?;
    tilo_files::blocking::read_bounded(&dir, &name, limit)
        .with_context(|| format!("reading {}", path.display()))?
        .ok_or_else(|| {
            anyhow!(
                "PDF Viewer opens files up to {}",
                ui::format::memory(limit as u64)
            )
        })
}

type ImportInspection = (String, String, Vec<(f64, f64)>);

fn inspect_import_for_store(
    backend: &RasterBackend,
    path: &Path,
) -> anyhow::Result<ImportInspection> {
    let bytes = read_file_bounded(path, MAX_IMPORT_BYTES)?;
    if bytes.is_empty() {
        return Err(anyhow!("the import file is empty"));
    }
    let (kind, sizes) = backend.inspect_import(&bytes)?;
    let hash = backend
        .content
        .put(&bytes)
        .map_err(|error| anyhow!("the local content store refused the import: {error}"))?;
    Ok((hash, kind, sizes))
}

/// An exported document waiting for the UI to commit it. `temporary`
/// identifies the prepared save; nothing is written until the commit, which
/// replaces the destination atomically ([`tilo_files::blocking::save_document`]:
/// a private sibling, flushed, renamed over the destination).
struct PrivateSave {
    temporary: PathBuf,
    bytes: Vec<u8>,
}

fn private_save(destination: &Path, request_id: u64, bytes: Vec<u8>) -> PrivateSave {
    let name = destination
        .file_name()
        .map_or_else(|| "document.pdf".into(), |name| name.to_string_lossy());
    PrivateSave {
        temporary: destination.with_file_name(format!(".{name}.tilo-{request_id}.pending")),
        bytes,
    }
}

fn flush_private_save(private: &PrivateSave, destination: &Path) -> anyhow::Result<()> {
    let (dir, name) = tilo_files::file_at(destination)
        .with_context(|| format!("publishing the saved PDF to {}", destination.display()))?;
    tilo_files::blocking::save_document(&dir, &name, &private.bytes)
        .with_context(|| format!("publishing the saved PDF to {}", destination.display()))?;
    Ok(())
}

fn valid_page_size(width: f64, height: f64) -> bool {
    width.is_finite()
        && height.is_finite()
        && width > 0.0
        && height > 0.0
        && width <= MAX_PAGE_EDGE_POINTS
        && height <= MAX_PAGE_EDGE_POINTS
        && width * height <= MAX_PAGE_AREA_POINTS
}

/// A document loaded from disk plus the engine-backed rasteriser for it.
pub struct LoadedDoc {
    pub doc: Document,
    pub raster: RasterBackend,
    /// The file's display name (for the window chrome / status bar).
    pub name: String,
}

/// Load a PDF from `path` through the shared PDF engine. Pages the shared
/// page plan imports are read from `content` by hash.
///
/// # Errors
///
/// Returns an error if `path` cannot be read, if the PDF engine is not
/// available on this host, or if it cannot parse the bytes as a PDF within
/// the reader's page and geometry limits.
pub fn open(path: &Path, content: &ContentStore) -> anyhow::Result<LoadedDoc> {
    let t = ui::time::now();
    let bytes = read_file_bounded(path, MAX_OPEN_BYTES)?;
    prof("fs::read", t);
    open_bytes_at(path, &bytes, content)
}

/// Open a PDF the host handed over as bytes (a browser page has no paths).
///
/// # Errors
///
/// As [`open`], less the file-system read.
pub fn open_contents(
    name: &str,
    bytes: &[u8],
    content: &ContentStore,
) -> anyhow::Result<LoadedDoc> {
    if bytes.len() > MAX_OPEN_BYTES {
        anyhow::bail!("the document is too large");
    }
    open_bytes_at(Path::new(name), bytes, content)
}

fn open_bytes_at(path: &Path, bytes: &[u8], content: &ContentStore) -> anyhow::Result<LoadedDoc> {
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

    let t = ui::time::now();
    let r = RasterBackend::open(bytes, content.clone())?;
    prof("RasterBackend::open (engine parse + page sizes)", t);
    let (mut title, author, subject) = r.metadata();
    if title.trim().is_empty() {
        title = stem;
    }
    let (pw, ph) = r.first_page_size();
    let mut doc = Document::blank_pages(&title, &author, r.page_count(), pw, ph);
    doc.subject = subject;
    let t = ui::time::now();
    doc.outline = r.outline();
    prof("outline (bookmarks)", t);
    Ok(LoadedDoc {
        doc,
        raster: r,
        name,
    })
}

/// One document open in the PDF engine backing some of the displayed pages.
/// Source 0 is always the originally-opened file; imports (other PDFs, or
/// images wrapped as a one-page document) are appended as further sources.
struct Source {
    doc: tilo_pdf::Document,
    page_sizes: Vec<(f64, f64)>,
    content_hash: Option<String>,
}

impl Source {
    fn geometry(&self, page: usize) -> Option<tilo_pdf::PageGeometry> {
        self.doc.page(page)
    }
}

/// A displayed page: which loaded [`Source`] it comes from and the page index
/// within that source. The ordered list of these — the *page plan* — is the
/// editable model: reordering, deleting, duplicating and importing pages all
/// permute this vector, and a save exports the source pages in plan order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PageSlot {
    source: usize,
    page: usize,
}

/// Engine-backed implementation. It lives on the source worker thread, which
/// serialises page work and keeps bitmap bookkeeping off the UI thread; the
/// PDF itself is parsed in the engine's sandboxed helper process.
pub struct RasterBackend {
    engine: tilo_pdf::Engine,
    /// Loaded documents; index 0 is the original file.
    sources: Vec<Source>,
    /// The ordered, editable list of displayed pages.
    plan: Vec<Option<PageSlot>>,
    /// Dimensions supplied by the durable plan for a not-yet-downloaded page.
    /// This lets UI layout remain stable while the content store fetches it.
    pending_sizes: Vec<Option<(f64, f64)>>,
    /// Recorded rotation (degrees) of each displayed page occurrence.
    rotations: Vec<i32>,
    /// Rendered bitmaps keyed by (source, source-page, target-width bucket,
    /// rotation).
    cache: HashMap<RasterKey, PenikoImage>,
    /// Byte size and recency bookkeeping for the bounded raster cache.
    cache_sizes: HashMap<RasterKey, usize>,
    cache_lru: VecDeque<RasterKey>,
    cache_bytes: usize,
    /// Per-source-page extracted text layer, filled lazily on first request.
    text_cache: HashMap<(usize, usize), Arc<PageText>>,
    /// Rendered tiles of zoomed pages, per zoom level (see [`crate::tiles`]).
    tiles: crate::tiles::TileCache<TileKey>,
    /// Whether the original document's own markup is drawn into its page
    /// images. Off once the record has taken that markup over (see
    /// [`crate::adopt`]): the reader then draws it from the records.
    reader_markup: bool,
    /// Per-source-page link areas, filled lazily on first request.
    link_cache: HashMap<(usize, usize), Arc<Vec<PageLink>>>,
    /// Content hash and length of the opened bytes, which are kept in the
    /// content-addressed blob store.
    source_hash: String,
    source_len: u64,
    /// Where imported pages' bytes live, addressed by the hash in the plan.
    content: ContentStore,
}

/// A cached bitmap's identity: source, source page, width bucket, rotation.
type RasterKey = (usize, usize, u32, i32);

/// A cached tile's identity: source, source page, zoom level, rotation,
/// column and row. Keyed by source page like whole-page bitmaps, so a
/// duplicated or moved page reuses its tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TileKey {
    source: usize,
    page: usize,
    level: crate::tiles::Level,
    rotation: i32,
    col: u32,
    row: u32,
}

impl crate::tiles::LevelKey for TileKey {
    fn level(&self) -> crate::tiles::Level {
        self.level
    }
}

/// A page size after an added rotation.
fn rotated_size((w, h): (f64, f64), degrees: i32) -> (f64, f64) {
    if degrees.rem_euclid(180) == 90 {
        (h, w)
    } else {
        (w, h)
    }
}

/// The process-wide PDF engine.
fn engine() -> anyhow::Result<tilo_pdf::Engine> {
    tilo_pdf::Engine::shared().map_err(|e| anyhow!("{e}"))
}

fn page_sizes_of(doc: &tilo_pdf::Document) -> Option<Vec<(f64, f64)>> {
    (0..doc.page_count())
        .map(|i| {
            let size = doc.page_size(i)?;
            let size = (f64::from(size.width), f64::from(size.height));
            valid_page_size(size.0, size.1).then_some(size)
        })
        .collect()
}

impl RasterBackend {
    const MAX_IMPORT_PAGES: usize = 512;

    /// Validate an import before it is offered to the content store or turned
    /// into an action. This is deliberately separate from materialisation: it
    /// never mutates the live page plan. Images are decoded inside the engine,
    /// never in this process.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes are neither a readable PDF nor a
    /// decodable image, or when a PDF exceeds the bounded page limit.
    pub fn inspect_import(&self, bytes: &[u8]) -> anyhow::Result<(String, Vec<(f64, f64)>)> {
        let (kind, doc) = if bytes.starts_with(b"%PDF-") {
            let doc = self
                .engine
                .open_bytes(bytes)
                .map_err(|_| anyhow!("the file is not a readable PDF"))?;
            if doc.page_count() > Self::MAX_IMPORT_PAGES {
                return Err(anyhow!(
                    "the PDF exceeds the {}-page import limit",
                    Self::MAX_IMPORT_PAGES
                ));
            }
            ("pdf", doc)
        } else {
            let doc = self
                .engine
                .open_image_bytes(bytes)
                .map_err(|e| anyhow!("decoding the image: {e}"))?;
            ("image", doc)
        };
        let sizes =
            page_sizes_of(&doc).ok_or_else(|| anyhow!("the file has an unsupported page size"))?;
        (!sizes.is_empty())
            .then(|| (kind.to_string(), sizes))
            .ok_or_else(|| anyhow!("the file has no pages"))
    }

    fn source_from(doc: tilo_pdf::Document) -> Option<Source> {
        let page_count = doc.page_count();
        if page_count == 0 || page_count > MAX_SOURCE_PAGES {
            return None;
        }
        let page_sizes = page_sizes_of(&doc)?;
        Some(Source {
            doc,
            page_sizes,
            content_hash: None,
        })
    }

    fn load_source(engine: &tilo_pdf::Engine, bytes: &[u8]) -> Option<Source> {
        let t = ui::time::now();
        let doc = engine.open_bytes(bytes).ok()?;
        prof("  engine open", t);
        Self::source_from(doc)
    }

    fn image_source(engine: &tilo_pdf::Engine, bytes: &[u8]) -> anyhow::Result<Source> {
        let doc = engine
            .open_image_bytes(bytes)
            .map_err(|e| anyhow!("decoding the image: {e}"))?;
        Self::source_from(doc).ok_or_else(|| anyhow!("the image page has unsupported dimensions"))
    }

    fn open(bytes: &[u8], content: ContentStore) -> anyhow::Result<Self> {
        let engine = engine()?;
        // The document is an immutable blob: keep the bytes in the app's
        // content store (deduplicated by hash) so the markup record can pin
        // exactly what it annotates. A store that refuses (size limits) still
        // leaves the hash as the identity.
        let source_hash = content
            .put(bytes)
            .unwrap_or_else(|_| tilo_content::hash_bytes(bytes));
        let byte_len = bytes.len() as u64;
        let source = match engine.open_bytes(bytes) {
            Ok(doc) => Self::source_from(doc)
                .ok_or_else(|| anyhow!("the PDF has no pages or unsupported page sizes"))?,
            Err(tilo_pdf::Error::Password) => {
                return Err(anyhow!("the PDF is password protected"));
            }
            Err(e @ (tilo_pdf::Error::Unavailable(_) | tilo_pdf::Error::Unconfined(_))) => {
                return Err(anyhow!("{e}"));
            }
            Err(e) => return Err(anyhow!("the file could not be opened as a PDF ({e})")),
        };
        let source_len = source.page_sizes.len();
        let plan = (0..source_len)
            .map(|page| Some(PageSlot { source: 0, page }))
            .collect();
        Ok(RasterBackend {
            engine,
            sources: vec![source],
            plan,
            pending_sizes: vec![None; source_len],
            rotations: vec![0; source_len],
            cache: HashMap::new(),
            cache_sizes: HashMap::new(),
            cache_lru: VecDeque::new(),
            cache_bytes: 0,
            text_cache: HashMap::new(),
            tiles: crate::tiles::TileCache::new(crate::tiles::MAX_TILE_CACHE_BYTES),
            reader_markup: true,
            link_cache: HashMap::new(),
            source_hash,
            source_len: byte_len,
            content,
        })
    }

    /// Content hash of the opened bytes.
    pub fn source_hash(&self) -> &str {
        &self.source_hash
    }

    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    /// The opened document's form fields, their widgets mapped to displayed
    /// pages of the original plan.
    pub fn form_fields(&self) -> Vec<tilo_pdf::FormField> {
        self.sources
            .first()
            .and_then(|s| s.doc.form_fields().ok())
            .unwrap_or_default()
    }

    /// Draw (or leave out) the opened document's own markup in its page
    /// images. Changing it drops every cached image, which showed the other
    /// choice.
    pub fn set_reader_markup(&mut self, visible: bool) {
        if self.reader_markup == visible {
            return;
        }
        self.reader_markup = visible;
        self.cache.clear();
        self.cache_sizes.clear();
        self.cache_lru.clear();
        self.cache_bytes = 0;
        self.tiles = crate::tiles::TileCache::new(crate::tiles::MAX_TILE_CACHE_BYTES);
    }

    /// Whether page images include the document's own markup.
    pub fn reader_markup(&self) -> bool {
        self.reader_markup
    }

    /// The markup and form values already in the opened document, for the
    /// record to take over.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine cannot read a page's annotations.
    pub fn existing(&self) -> anyhow::Result<crate::adopt::Existing> {
        let source = self
            .sources
            .first()
            .ok_or_else(|| anyhow!("no document is open"))?;
        let mut pages = Vec::new();
        for page in 0..source.doc.page_count() {
            let Some(geometry) = source.doc.page(page) else {
                continue;
            };
            let annotations = source
                .doc
                .annotations(page)
                .map_err(|e| anyhow!("reading the annotations of page {}: {e}", page + 1))?;
            if annotations.iter().any(|a| a.kind.is_reader_markup()) {
                pages.push(crate::adopt::ExistingPage {
                    page,
                    geometry,
                    annotations,
                });
            }
        }
        Ok(crate::adopt::Existing {
            pages,
            form_fields: source.doc.form_fields().unwrap_or_default(),
        })
    }

    /// Number of displayed pages (the length of the editable page plan).
    pub fn page_count(&self) -> usize {
        self.plan.len()
    }

    /// Hydrate a content-addressed imported source on demand. A miss is not an
    /// error: the content store fetches it and the next sync/render retries.
    fn imported_source(&mut self, imported: &ImportedPage) -> Option<usize> {
        if let Some(index) = self
            .sources
            .iter()
            .position(|source| source.content_hash.as_deref() == Some(&imported.hash))
        {
            return Some(index);
        }
        // `read` verifies the bytes against the hash that named them.
        let bytes = self.content.read(&imported.hash)?;
        let mut source = if imported.kind == "pdf" {
            Self::load_source(&self.engine, &bytes)?
        } else {
            Self::image_source(&self.engine, &bytes).ok()?
        };
        source.content_hash = Some(imported.hash.clone());
        let index = self.sources.len();
        self.sources.push(source);
        Some(index)
    }

    /// Materialise a shared plan. Imported payloads may be unavailable on this
    /// peer; their occurrence remains in `plan` as `None` rather than being
    /// silently replaced by an original page.
    pub fn reconcile_page_plan(&mut self, entries: &[PagePlanEntry]) -> bool {
        let source_len = self
            .sources
            .first()
            .map_or(0, |source| source.page_sizes.len());
        if entries.is_empty()
            || entries.iter().any(|entry| {
                entry.imported.is_none()
                    && (entry.source_page == 0 || entry.source_page > source_len)
            })
        {
            return false;
        }
        self.pending_sizes = entries
            .iter()
            .map(|entry| {
                entry.imported.as_ref().map(|p| {
                    (
                        f64::from(p.width_milli) / 1000.0,
                        f64::from(p.height_milli) / 1000.0,
                    )
                })
            })
            .collect();
        self.rotations = entries
            .iter()
            .map(|entry| entry.rotation.rem_euclid(360))
            .collect();
        self.plan = entries
            .iter()
            .map(|entry| match &entry.imported {
                None => Some(PageSlot {
                    source: 0,
                    page: entry.source_page - 1,
                }),
                Some(imported) => self.imported_source(imported).and_then(|source| {
                    let page = imported.page.checked_sub(1)?;
                    (page < self.sources.get(source)?.page_sizes.len())
                        .then_some(PageSlot { source, page })
                }),
            })
            .collect();
        true
    }

    /// The source [`Source`] and source-page index backing displayed page
    /// `display_idx`.
    fn slot(&self, display_idx: usize) -> Option<PageSlot> {
        self.plan.get(display_idx).copied().flatten()
    }

    pub fn first_page_size(&self) -> (f64, f64) {
        self.plan
            .first()
            .and_then(|s| {
                s.and_then(|s| self.sources.get(s.source)?.page_sizes.get(s.page).copied())
            })
            .unwrap_or((612.0, 792.0))
    }

    /// Size of displayed page `display_idx`, in PDF points, with its recorded
    /// rotation applied.
    pub fn page_size(&self, display_idx: usize) -> (f64, f64) {
        let size = self
            .slot(display_idx)
            .and_then(|s| self.sources.get(s.source)?.page_sizes.get(s.page).copied())
            .or_else(|| self.pending_sizes.get(display_idx).copied().flatten())
            .unwrap_or_else(|| self.first_page_size());
        rotated_size(size, self.page_rotation(display_idx))
    }

    /// The source page's own geometry for displayed page `display_idx` (its
    /// `/Rotate`, not the occurrence's recorded rotation).
    pub fn page_geometry(&self, display_idx: usize) -> Option<tilo_pdf::PageGeometry> {
        let slot = self.slot(display_idx)?;
        self.sources.get(slot.source)?.geometry(slot.page)
    }

    /// The recorded rotation (degrees) of displayed page `display_idx`.
    pub fn page_rotation(&self, display_idx: usize) -> i32 {
        self.rotations.get(display_idx).copied().unwrap_or(0)
    }

    /// Own geometry and recorded rotation of every displayed page.
    pub fn page_layout(&self) -> (Vec<Option<tilo_pdf::PageGeometry>>, Vec<i32>) {
        (
            (0..self.page_count())
                .map(|i| self.page_geometry(i))
                .collect(),
            (0..self.page_count())
                .map(|i| self.page_rotation(i))
                .collect(),
        )
    }

    fn metadata(&self) -> (String, String, String) {
        let Some(src) = self.sources.first() else {
            return (String::new(), String::new(), String::new());
        };
        let meta = src.doc.metadata();
        (
            meta.title.clone(),
            meta.author.clone(),
            meta.subject.clone(),
        )
    }

    /// The document outline, depth-first, as the reader's two-level list.
    /// Every entry navigates to its page.
    fn outline(&self) -> Vec<OutlineItem> {
        let Some(src) = self.sources.first() else {
            return Vec::new();
        };
        let Ok(entries) = src.doc.outline() else {
            return Vec::new();
        };
        tilo_pdf::OutlineEntry::flatten(&entries)
            .into_iter()
            .filter(|(_, entry)| !entry.title.trim().is_empty())
            .take(200)
            .map(|(depth, entry)| OutlineItem {
                title: entry.title.clone(),
                page: match entry.destination {
                    Some(tilo_pdf::Destination::Page { page, .. }) => page + 1,
                    _ => 1,
                },
                level: u8::from(depth > 0),
            })
            .collect()
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

    /// The link areas on displayed page `display_idx`, cached per source
    /// page. Rectangles and destination points are in the page's display
    /// space (the text layer's space); the reader maps them into card space.
    /// A page whose links cannot be read has none.
    pub fn page_links(&mut self, display_idx: usize) -> Arc<Vec<PageLink>> {
        let Some(slot) = self.slot(display_idx) else {
            return Arc::default();
        };
        let key = (slot.source, slot.page);
        if let Some(links) = self.link_cache.get(&key) {
            return links.clone();
        }
        let links = Arc::new(
            self.sources
                .get(slot.source)
                .and_then(|source| source.doc.links(slot.page).ok())
                .map(|links| {
                    links
                        .into_iter()
                        .take(MAX_PAGE_LINKS)
                        .filter_map(PageLink::from_engine)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
        self.link_cache.insert(key, links.clone());
        links
    }

    /// The engine's analysed text layer for a source page, as the reader's
    /// page text. A page whose text is too large to analyse honestly yields
    /// `None` (reported as unavailable, never as a truncated layer).
    fn extract_page_text(&self, slot: PageSlot) -> Option<PageText> {
        let layer = self.sources.get(slot.source)?.doc.text(slot.page).ok()?;
        if layer.len() > MAX_TEXT_LAYER_GLYPHS {
            return None;
        }
        Some(PageText::from_layer(layer))
    }

    /// The width bucket `render` snaps `target_w` to. Exposed so the reader
    /// keys the worker's results with the exact bucket `render` uses.
    pub(crate) fn bucket_for(target_w: u32) -> u32 {
        ((target_w / 24).max(1) * 24).clamp(96, 4096)
    }

    fn touch_cache(&mut self, key: RasterKey) {
        self.cache_lru.retain(|candidate| *candidate != key);
        self.cache_lru.push_back(key);
    }

    fn evict_cache(&mut self, key: RasterKey) {
        self.cache.remove(&key);
        self.cache_bytes = self
            .cache_bytes
            .saturating_sub(self.cache_sizes.remove(&key).unwrap_or(0));
        self.cache_lru.retain(|candidate| *candidate != key);
    }

    fn cache_image(&mut self, key: RasterKey, image: PenikoImage) {
        let bytes = usize::try_from(image.width)
            .ok()
            .and_then(|width| {
                usize::try_from(image.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4));
        let Some(bytes) = bytes else {
            return;
        };
        // Rendering is already per-page bounded, but keep this guard local to
        // the cache too so a backend change cannot create one uncacheable giant
        // allocation followed by eviction churn.
        if bytes > MAX_RASTER_CACHE_BYTES {
            return;
        }
        self.evict_cache(key);
        while self.cache_bytes.saturating_add(bytes) > MAX_RASTER_CACHE_BYTES {
            let Some(oldest) = self.cache_lru.pop_front() else {
                break;
            };
            self.cache.remove(&oldest);
            self.cache_bytes = self
                .cache_bytes
                .saturating_sub(self.cache_sizes.remove(&oldest).unwrap_or(0));
        }
        self.cache.insert(key, image);
        self.cache_sizes.insert(key, bytes);
        self.cache_bytes = self.cache_bytes.saturating_add(bytes);
        self.cache_lru.push_back(key);
    }

    /// Rasterise displayed page `display_idx` at roughly `target_w` device
    /// pixels wide, returning a cached bitmap. Widths are snapped to a bucket so
    /// a small zoom nudge reuses the existing render instead of re-rasterising.
    /// The cache is keyed by source page, so a duplicated or reordered page
    /// never re-rasterises. The bitmap's pixels are the engine's shared
    /// buffer, wrapped without a copy.
    pub fn render(&mut self, display_idx: usize, target_w: u32) -> Option<PenikoImage> {
        let slot = self.slot(display_idx)?;
        let rotation = self.page_rotation(display_idx);
        let size = rotated_size(
            *self.sources.get(slot.source)?.page_sizes.get(slot.page)?,
            rotation,
        );
        let bucket = Self::bucket_for_page(target_w, size)?;
        let key = (slot.source, slot.page, bucket, rotation);
        if let Some(img) = self.cache.get(&key).cloned() {
            self.touch_cache(key);
            return Some(img);
        }
        let t = ui::time::now();
        let source = self.sources.get(slot.source)?;
        let extra = tilo_pdf::Rotation::from_degrees(rotation).unwrap_or_default();
        let options = tilo_pdf::RenderOptions {
            rotation: extra,
            reader_markup: slot.source != 0 || self.reader_markup,
            ..tilo_pdf::RenderOptions::fit_width(
                extra.apply_size(source.doc.page_size(slot.page)?),
                bucket,
            )
        };
        let bitmap = source.doc.render(slot.page, &options).ok()?;
        if profiling() {
            eprintln!(
                "[pdf-profile]   render page {} @ {}px: {:.1}ms",
                display_idx + 1,
                bucket,
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        let (w, h) = (bitmap.width(), bitmap.height());
        if w == 0 || h == 0 || bitmap.pixels().len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        let img = ui::raster::rgba8(Blob::new(bitmap.shared_pixels()), w, h);
        self.cache_image(key, img.clone());
        Some(img)
    }

    /// Render tile `(col, row)` of displayed page `display_idx` at zoom
    /// `level` (see [`crate::tiles`]), with the page's recorded rotation,
    /// returning a cached tile when this level already has it. Unlike
    /// [`Self::render`] there is no page-size cap: every tile is bounded by
    /// the grid, however large the page is at this level.
    pub fn render_tile(
        &mut self,
        display_idx: usize,
        level: crate::tiles::Level,
        col: u32,
        row: u32,
    ) -> Option<PenikoImage> {
        let slot = self.slot(display_idx)?;
        let rotation = self.page_rotation(display_idx);
        let key = TileKey {
            source: slot.source,
            page: slot.page,
            level,
            rotation,
            col,
            row,
        };
        if let Some(image) = self.tiles.get(&key) {
            return Some(image);
        }
        let size = rotated_size(
            *self.sources.get(slot.source)?.page_sizes.get(slot.page)?,
            rotation,
        );
        if !valid_page_size(size.0, size.1) {
            return None;
        }
        let region = crate::tiles::tile_region(size, level, col, row)?;
        let extra = tilo_pdf::Rotation::from_degrees(rotation).unwrap_or_default();
        let source = self.sources.get(slot.source)?;
        let tile = tilo_pdf::Tile { col, row, region };
        let t = ui::time::now();
        let bitmap = source
            .doc
            .render(
                slot.page,
                &tilo_pdf::RenderOptions {
                    scale: crate::tiles::level_scale(level),
                    rotation: extra,
                    region: Some(tile.region),
                    reader_markup: slot.source != 0 || self.reader_markup,
                    ..tilo_pdf::RenderOptions::default()
                },
            )
            .ok()?;
        if profiling() {
            eprintln!(
                "[pdf-profile]   render tile {}/{col},{row} @ level {level}: {:.1}ms",
                display_idx + 1,
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        let (w, h) = (bitmap.width(), bitmap.height());
        if w == 0 || h == 0 || bitmap.pixels().len() != (w as usize) * (h as usize) * 4 {
            return None;
        }
        let image = ui::raster::rgba8(Blob::new(bitmap.shared_pixels()), w, h);
        self.tiles.insert(key, image.clone());
        Some(image)
    }

    /// Width bucket adjusted to keep the decoded raster within the per-page
    /// pixel budget. A malformed, needle-shaped media box therefore produces
    /// an honest unavailable-page state instead of asking the engine for a
    /// multi-gigabyte bitmap.
    fn bucket_for_page(target_w: u32, (width, height): (f64, f64)) -> Option<u32> {
        if !valid_page_size(width, height) {
            return None;
        }
        let ratio = height / width;
        let max_width = (MAX_RASTER_PIXELS / ratio).sqrt().floor() as u32;
        // The engine does not produce useful document imagery below this
        // width; decline instead of silently returning a blurry or unsafe
        // bitmap.
        (max_width >= 24).then(|| Self::bucket_for(target_w).min(max_width))
    }

    /// The annotations the engine reads back from displayed page
    /// `display_idx` of the backing document. Used by the save round-trip
    /// tests to check that exported markup lands as real PDF annotations.
    #[cfg(test)]
    pub fn page_annotations(&self, display_idx: usize) -> Vec<tilo_pdf::AnnotationInfo> {
        self.slot(display_idx)
            .and_then(|slot| {
                self.sources
                    .get(slot.source)?
                    .doc
                    .annotations(slot.page)
                    .ok()
            })
            .unwrap_or_default()
    }

    /// Export the edited document — the pages in the current plan order,
    /// including imported PDF/image pages and each page's recorded rotation —
    /// with the reader's markup written as standard PDF annotations and the
    /// recorded form values as the form's field values, and write it to
    /// `out`. Highlights, underlines and strike-outs become text-markup
    /// annotations, ink becomes ink annotations, sticky notes become text
    /// annotations carrying their comment thread, and signatures become
    /// stamps whose appearance is the signer's real ink (or typed name) plus
    /// the signer name and date. The original bytes are kept as they are and
    /// the edits appended as an incremental update; the same markup on the
    /// same bytes always exports to the same file.
    ///
    /// `annotations` and `signatures` must be in PDF user space.
    ///
    /// # Errors
    ///
    /// Returns an error if a planned page is still awaiting imported content,
    /// if the engine fails to export, or if writing `out` fails.
    pub fn save_annotated(&self, markup: &ExportMarkup, out: &Path) -> anyhow::Result<()> {
        let bytes = self.export_bytes(markup)?;
        tilo_files::file_at(out)
            .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes))
            .with_context(|| format!("writing {}", out.display()))?;
        Ok(())
    }

    /// The exported PDF bytes for `markup` (see [`Self::save_annotated`]).
    ///
    /// # Errors
    ///
    /// As [`Self::save_annotated`], without the write.
    pub fn export_bytes(&self, markup: &ExportMarkup) -> anyhow::Result<Vec<u8>> {
        let edits = self.edits_for(markup)?;
        let base = self
            .sources
            .first()
            .ok_or_else(|| anyhow!("no document is open"))?;
        let imports: Vec<&tilo_pdf::Document> =
            self.sources.iter().skip(1).map(|s| &s.doc).collect();
        base.doc
            .export(&edits, &imports)
            .map_err(|e| anyhow!("exporting the PDF: {e}"))
    }

    /// The engine edits for the current plan and markup.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass from the records to the engine's edits, page by page"
    )]
    fn edits_for(&self, markup: &ExportMarkup) -> anyhow::Result<tilo_pdf::Edits> {
        let mut edits = tilo_pdf::Edits::default();
        for (dest, slot) in self.plan.iter().enumerate() {
            let slot = slot
                .ok_or_else(|| anyhow!("page {} is still awaiting imported content", dest + 1))?;
            // The occurrence that keeps an original page's own markup is the
            // original one; duplicates copy page content only.
            let primary = slot.source != 0
                || markup
                    .page_ids
                    .get(dest)
                    .is_none_or(|id| *id == format!("source-{}", slot.page + 1));
            edits.pages.push(tilo_pdf::PageEdit {
                source: if slot.source == 0 {
                    tilo_pdf::PageSource::Original(slot.page)
                } else {
                    tilo_pdf::PageSource::Imported {
                        import: slot.source - 1,
                        page: slot.page,
                    }
                },
                rotation: tilo_pdf::Rotation::from_degrees(self.page_rotation(dest))
                    .unwrap_or_default(),
                primary,
            });
        }
        let identity = edits.pages.iter().enumerate().all(|(i, p)| {
            p.source == tilo_pdf::PageSource::Original(i)
                && p.rotation == tilo_pdf::Rotation::None
                && p.primary
        }) && self.plan.len()
            == self.sources.first().map_or(0, |s| s.page_sizes.len());
        if identity {
            edits.pages.clear();
        }
        let point = |(x, y): (f64, f64)| tilo_pdf::Point::new(x as f32, y as f32);
        let rect = |r: &PtRect| {
            tilo_pdf::Rect::from_corners(point((r.x, r.y)), point((r.x + r.w, r.y + r.h)))
        };
        // Threads: every comment pinned to an annotation, oldest first. The
        // first comment's body is the annotation's text; its replies, and
        // any later comment, are written as replies to it.
        let mut threads: HashMap<&str, Vec<&Comment>> = HashMap::new();
        for c in &markup.comments {
            if let Some(anno) = c.anno_id.as_deref() {
                threads.entry(anno).or_default().push(c);
            }
        }
        let adopted: HashMap<&str, &crate::adopt::AdoptedEntry> =
            markup.adopted.iter().map(|e| (e.id.as_str(), e)).collect();
        // A taken-over thread whose record still says what the original
        // says keeps its original objects.
        let unchanged: HashSet<&str> = markup
            .annotations
            .iter()
            .filter(|a| {
                adopted.get(a.id.as_str()).is_some_and(|entry| {
                    entry.parent.is_none()
                        && crate::adopt::thread_print(
                            a,
                            threads.get(a.id.as_str()).and_then(|t| t.first().copied()),
                        ) == entry.print
                        && threads.get(a.id.as_str()).is_none_or(|t| t.len() <= 1)
                })
            })
            .map(|a| a.id.as_str())
            .collect();
        edits.adopted = markup
            .adopted
            .iter()
            .filter_map(|entry| {
                let owner = entry.parent.as_deref().unwrap_or(&entry.id);
                Some(tilo_pdf::AdoptedAnnotation {
                    target: entry.target()?,
                    unchanged: unchanged.contains(owner),
                })
            })
            .collect();
        // What the PDF itself should call a record: its original `/NM` when
        // it was taken over from the file, otherwise the record id.
        let pdf_name = |id: &str| {
            adopted
                .get(id)
                .map(|e| e.name.as_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(id)
                .to_string()
        };
        for idx in 0..self.plan.len() {
            let page_no = idx + 1;
            for a in markup.annotations.iter().filter(|a| a.page == page_no) {
                if unchanged.contains(a.id.as_str()) {
                    continue;
                }
                let (r, g, b) = a.color.rgb();
                let thread: &[&Comment] = threads.get(a.id.as_str()).map_or(&[], Vec::as_slice);
                let reply_at = match &a.kind {
                    AnnoKind::Markup { quads, .. } => {
                        quads.first().map_or((0.0, 0.0), |q| (q.x, q.y + q.h))
                    }
                    AnnoKind::Ink { strokes } => strokes
                        .iter()
                        .find_map(|s| s.first().copied())
                        .unwrap_or((0.0, 0.0)),
                    AnnoKind::Note { x, y } => (*x, *y),
                    AnnoKind::FreeText { rect, .. } => (rect.x, rect.y + rect.h),
                };
                let (kind, contents) = match &a.kind {
                    AnnoKind::Markup { style, quads } => {
                        let quads = quads.iter().map(rect).collect();
                        let kind = match style {
                            MarkupStyle::Highlight => {
                                tilo_pdf::NewAnnotationKind::Highlight { quads }
                            }
                            MarkupStyle::Underline => {
                                tilo_pdf::NewAnnotationKind::Underline { quads }
                            }
                            MarkupStyle::StrikeOut => {
                                tilo_pdf::NewAnnotationKind::StrikeOut { quads }
                            }
                            MarkupStyle::Squiggly => {
                                tilo_pdf::NewAnnotationKind::Squiggly { quads }
                            }
                        };
                        (kind, None)
                    }
                    AnnoKind::Ink { strokes } => {
                        let strokes: Vec<Vec<tilo_pdf::Point>> = strokes
                            .iter()
                            .filter(|s| s.len() >= 2)
                            .map(|s| s.iter().copied().map(point).collect())
                            .collect();
                        if strokes.is_empty() {
                            continue;
                        }
                        (
                            tilo_pdf::NewAnnotationKind::Ink {
                                strokes,
                                width: 2.0,
                            },
                            None,
                        )
                    }
                    AnnoKind::Note { x, y } => (
                        tilo_pdf::NewAnnotationKind::Note {
                            at: point((*x, *y)),
                        },
                        None,
                    ),
                    AnnoKind::FreeText {
                        rect: box_rect,
                        text,
                        size,
                    } => (
                        tilo_pdf::NewAnnotationKind::FreeText {
                            rect: rect(box_rect),
                            font_size: *size as f32,
                        },
                        Some(text.clone()),
                    ),
                };
                let name = pdf_name(&a.id);
                let (first, later_comments) = thread
                    .split_first()
                    .map_or((None, &[][..]), |(f, r)| (Some(*f), r));
                edits.annotations.push(tilo_pdf::NewAnnotation {
                    page: idx,
                    kind,
                    color: tilo_pdf::Rgba::new(r, g, b, 255),
                    author: a.author.clone(),
                    contents: contents
                        .or_else(|| first.map(|c| c.body.clone()))
                        .unwrap_or_default(),
                    name: name.clone(),
                    modified: pdf_date(a.created_ms),
                    in_reply_to: None,
                    reply_type: tilo_pdf::ReplyType::Reply,
                });
                // Replies, as reply annotations anchored where the thread is.
                let mut replies: Vec<(String, &str, &str)> = Vec::new();
                if let Some(first) = first {
                    replies.extend(
                        first
                            .replies
                            .iter()
                            .map(|r| (r.id.clone(), r.author.as_str(), r.body.as_str())),
                    );
                }
                for later in later_comments {
                    replies.push((later.id.clone(), later.author.as_str(), later.body.as_str()));
                    replies.extend(
                        later
                            .replies
                            .iter()
                            .map(|r| (r.id.clone(), r.author.as_str(), r.body.as_str())),
                    );
                }
                for (id, author, body) in replies {
                    edits.annotations.push(tilo_pdf::NewAnnotation {
                        page: idx,
                        kind: tilo_pdf::NewAnnotationKind::Note {
                            at: point(reply_at),
                        },
                        color: tilo_pdf::Rgba::new(r, g, b, 255),
                        author: author.to_string(),
                        contents: body.to_string(),
                        name: pdf_name(&id),
                        modified: String::new(),
                        in_reply_to: Some(name.clone()),
                        reply_type: tilo_pdf::ReplyType::Reply,
                    });
                }
            }
            for s in markup.signatures.iter().filter(|s| s.page == page_no) {
                edits.annotations.push(signature_stamp(idx, s));
            }
        }
        // Values the file already holds are left as they are.
        let in_file: HashMap<String, FormEntry> = self
            .sources
            .first()
            .and_then(|s| s.doc.form_fields().ok())
            .map(|fields| crate::adopt::form_values_of(&fields).into_iter().collect())
            .unwrap_or_default();
        edits.form_values = markup
            .form_values
            .iter()
            .filter(|(name, value)| in_file.get(name) != Some(value))
            .map(|(name, value)| tilo_pdf::FormValue {
                name: name.clone(),
                value: match value {
                    FormEntry::Text(t) => tilo_pdf::FieldValue::Text(t.clone()),
                    FormEntry::Choice(c) => tilo_pdf::FieldValue::Choice(c.clone()),
                    FormEntry::Checked { on, export } => tilo_pdf::FieldValue::Checked {
                        on: *on,
                        export: export.clone(),
                    },
                },
            })
            .collect();
        Ok(edits)
    }
}

/// Everything an export writes on top of the page plan: markup in PDF user
/// space, the comment threads (written as the annotation's text and reply
/// annotations), recorded form values, and where taken-over markup came
/// from in the source bytes.
#[derive(Clone, Debug, Default)]
pub struct ExportMarkup {
    pub annotations: Vec<Annotation>,
    pub comments: Vec<Comment>,
    pub signatures: Vec<Signature>,
    pub form_values: Vec<(String, FormEntry)>,
    /// The durable occurrence id of each displayed page, in order.
    pub page_ids: Vec<String>,
    pub adopted: Vec<crate::adopt::AdoptedEntry>,
}

/// A PDF date (`D:YYYYMMDDHHmmSSZ`) for unix milliseconds; empty for 0.
fn pdf_date(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let secs = ms / 1000;
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = ui::date::civil_from_days(secs.div_euclid(86_400));
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// A signature (its box in PDF user space) as a stamp: its real ink (drawn
/// strokes, or the typed name), a baseline, and the signer name and date as
/// text, laid out top-down inside the box.
fn signature_stamp(page: usize, s: &Signature) -> tilo_pdf::NewAnnotation {
    let r = &s.rect;
    // Local layout coordinates run down from the box's top-left corner.
    let top = r.y + r.h;
    let at = |u: f64, v: f64| tilo_pdf::Point::new((r.x + u) as f32, (top - v) as f32);
    let area = (r.w * 0.06, r.h * 0.10, r.w * 0.88, r.h * 0.46);
    let base = r.h * 0.62;
    let mut strokes: Vec<Vec<tilo_pdf::Point>> = Vec::new();
    let mut text = Vec::new();
    match &s.ink {
        SignatureInk::Drawn { strokes: drawn } => {
            for stroke in drawn.iter().filter(|s| s.len() >= 2) {
                strokes.push(
                    stroke
                        .iter()
                        .map(|(nx, ny)| {
                            at(
                                area.0 + nx.clamp(0.0, 1.0) * area.2,
                                area.1 + ny.clamp(0.0, 1.0) * area.3,
                            )
                        })
                        .collect(),
                );
            }
        }
        SignatureInk::Typed => {
            if !s.signer.trim().is_empty() {
                text.push(tilo_pdf::StampText {
                    at: at(area.0, area.1 + area.3 * 0.85),
                    size: (area.3 * 0.72).clamp(10.0, 22.0) as f32,
                    text: s.signer.trim().to_string(),
                });
            }
        }
    }
    strokes.push(vec![at(8.0, base), at(r.w - 8.0, base)]);
    if !s.signer.trim().is_empty() {
        text.push(tilo_pdf::StampText {
            at: at(8.0, base + 12.0),
            size: 10.0,
            text: s.signer.trim().to_string(),
        });
    }
    text.push(tilo_pdf::StampText {
        at: at(8.0, base + 24.0),
        size: 7.5,
        text: format!("Signed {}", s.date),
    });
    tilo_pdf::NewAnnotation {
        page,
        kind: tilo_pdf::NewAnnotationKind::Stamp {
            rect: tilo_pdf::Rect::from_xywh(r.x as f32, r.y as f32, r.w as f32, r.h as f32),
            strokes,
            stroke_width: 1.6,
            text,
        },
        color: tilo_pdf::Rgba::new(28, 33, 47, 255),
        author: s.signer.clone(),
        contents: format!("Signed by {} on {}", s.signer.trim(), s.date),
        name: s.id.clone(),
        modified: String::new(),
        in_reply_to: None,
        reply_type: tilo_pdf::ReplyType::Reply,
    }
}

/// A request to open a path on the PDF owner worker.
///
/// `id` is monotonically increasing per handle and also becomes the opened
/// raster document id. A newer open supersedes an open still waiting in the
/// bounded queue; callers can therefore ignore an event whose id is no longer
/// their current request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub id: u64,
    pub path: PathBuf,
}

/// A request for one page image. `target_width` is bucketed by the worker, so
/// repeated requests for nearby widths coalesce before they can fill its queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderRequest {
    pub document_id: u64,
    pub page: usize,
    pub target_width: u32,
}

/// A request for one tile of a zoomed page (see [`crate::tiles`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileRequest {
    pub document_id: u64,
    pub page: usize,
    pub level: crate::tiles::Level,
    pub col: u32,
    pub row: u32,
}

/// The tiles a viewer wants now, most important first. A batch *replaces*
/// the viewer's queued tiles: after a scroll or zoom, tiles that left the
/// viewport are never rendered ahead of the ones that entered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileBatch {
    pub document_id: u64,
    pub tiles: Vec<TileRequest>,
}

/// A request for one page's selectable text layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRequest {
    pub document_id: u64,
    pub page: usize,
}

/// A request for one page's link areas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinksRequest {
    pub document_id: u64,
    pub page: usize,
}

/// Where a link on a page leads.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// Page `page` (0-based) of the document the link's page comes from,
    /// optionally at a point in that page's display space.
    Page { page: usize, at: Option<(f64, f64)> },
    /// An external URI.
    Uri(String),
    /// Another file, by the path the PDF names.
    File(String),
}

/// One clickable link area on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLink {
    /// The link area, in the page's display space until the reader maps it
    /// into card space.
    pub rect: PtRect,
    pub target: LinkTarget,
}

impl PageLink {
    fn from_engine(link: tilo_pdf::Link) -> Option<Self> {
        let r = link.rect;
        let rect = PtRect::new(
            f64::from(r.x0.min(r.x1)),
            f64::from(r.y0.min(r.y1)),
            f64::from((r.x1 - r.x0).abs()),
            f64::from((r.y1 - r.y0).abs()),
        );
        if ![rect.x, rect.y, rect.w, rect.h]
            .iter()
            .all(|v| v.is_finite())
        {
            return None;
        }
        let target = match link.destination {
            tilo_pdf::Destination::Page { page, at } => LinkTarget::Page {
                page,
                at: at
                    .filter(|p| p.x.is_finite() && p.y.is_finite())
                    .map(|p| (f64::from(p.x), f64::from(p.y))),
            },
            tilo_pdf::Destination::Uri(uri) => LinkTarget::Uri(uri),
            tilo_pdf::Destination::File(path) => LinkTarget::File(path),
        };
        Some(Self { rect, target })
    }
}

/// Adopt a collaboratively-authored page plan on the owner thread.  The plan
/// carries only durable identifiers/content hashes, never `PDFium` handles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileRequest {
    pub document_id: u64,
    pub plan: Vec<PagePlanEntry>,
}

/// A fully-owned save payload. It carries durable markup and a destination,
/// never an owner-thread `PDFium` handle.
#[derive(Clone, Debug)]
pub struct SaveRequest {
    /// A non-reusable operation id. It makes cancellation and completion
    /// exact even when a user selects the same destination twice.
    pub id: u64,
    pub document_id: u64,
    /// The markup to export, in PDF user space.
    pub markup: ExportMarkup,
    pub path: PathBuf,
}

impl PartialEq for SaveRequest {
    fn eq(&self, other: &Self) -> bool {
        // The protocol id is globally non-reusable; document/path are retained
        // as defensive checks when a stale event is observed during teardown.
        self.id == other.id && self.document_id == other.document_id && self.path == other.path
    }
}

/// A bounded import inspection. The worker owns reading and decoding the
/// untrusted file; the UI only receives the small, durable result needed to
/// dispatch the collaborative page-plan action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRequest {
    pub id: u64,
    pub document_id: u64,
    pub path: PathBuf,
    pub at: usize,
    pub after: Option<String>,
}

/// Work accepted by the dedicated PDF owner thread.
///
/// The public enum makes the protocol explicit. [`RasterHandle`] exposes
/// typed submission helpers so callers normally do not need to construct it.
#[derive(Clone, Debug, PartialEq)]
pub enum RasterCommand {
    OpenPath(OpenRequest),
    /// Make a successfully-opened source the session's active document and
    /// release every older source only after the UI accepted its completion.
    Activate {
        document_id: u64,
    },
    /// Release an unaccepted/replaced source. This is also how cancellation
    /// preserves the still-readable predecessor while a native parse finishes.
    Discard {
        document_id: u64,
    },
    /// Read the markup and form values already in the document, for the
    /// record to take over.
    ReadExisting {
        document_id: u64,
    },
    /// Draw (or leave out) the document's own markup in page images.
    SetReaderMarkup {
        document_id: u64,
        visible: bool,
    },
    Render(RenderRequest),
    /// Replace the session's queued tiles with this batch.
    Tiles(TileBatch),
    /// One tile taken off a batch by the worker.
    Tile(TileRequest),
    Text(TextRequest),
    Links(LinksRequest),
    Reconcile(ReconcileRequest),
    Save(SaveRequest),
    Import(ImportRequest),
    /// Publish a fully-written private temporary file to the user-selected
    /// destination.  This separate phase is what makes cancellation exact.
    CommitSave {
        request: SaveRequest,
        temporary: PathBuf,
    },
    /// Forget a queued/prepared save without ever touching its destination.
    CancelSave {
        request: SaveRequest,
    },
    Shutdown,
}

/// Immutable raster-side facts projected to the UI after an open succeeds.
/// No engine handles are exposed here, so this value is `Send`.
#[derive(Clone, Debug)]
pub struct RasterDocument {
    pub id: u64,
    /// Displayed page sizes, recorded rotation applied.
    pub page_sizes: Vec<(f64, f64)>,
    /// Each displayed page's own geometry (`None` while its bytes are
    /// unavailable).
    pub geometries: Vec<Option<tilo_pdf::PageGeometry>>,
    /// Each displayed page's recorded rotation, in degrees.
    pub rotations: Vec<i32>,
    /// The document's form fields.
    pub form_fields: Vec<tilo_pdf::FormField>,
    /// Content hash and length of the opened bytes.
    pub source_hash: String,
    pub source_len: u64,
}

/// Results delivered back to the UI by [`RasterHandle::drain_events`].
///
/// Images and text layers are ordinary owned UI data. In particular, this enum
/// never contains an engine document handle.
#[derive(Clone, Debug)]
pub enum RasterEvent {
    /// Bounded result storage discarded paint work. Retry any outstanding
    /// page requests after consuming the results that are still available.
    VisualWorkEvicted,
    Opened {
        request: OpenRequest,
        document: Document,
        name: String,
        raster: RasterDocument,
    },
    OpenFailed {
        request: OpenRequest,
        error: String,
    },
    /// A queued open was replaced by a newer user selection before the worker
    /// began parsing it. Unlike paint work, accepted opens are always settled.
    OpenSuperseded {
        request: OpenRequest,
    },
    Rendered {
        request: RenderRequest,
        image: Option<PenikoImage>,
    },
    TextExtracted {
        request: TextRequest,
        text: Arc<PageText>,
    },
    TileRendered {
        request: TileRequest,
        image: Option<PenikoImage>,
    },
    TileFailed {
        request: TileRequest,
    },
    RenderFailed {
        request: RenderRequest,
        error: String,
    },
    LinksRead {
        request: LinksRequest,
        links: Arc<Vec<PageLink>>,
    },
    LinksFailed {
        request: LinksRequest,
    },
    TextFailed {
        request: TextRequest,
        error: String,
    },
    Reconciled {
        request: ReconcileRequest,
        page_sizes: Vec<(f64, f64)>,
        geometries: Vec<Option<tilo_pdf::PageGeometry>>,
        rotations: Vec<i32>,
    },
    ReconcileFailed {
        request: ReconcileRequest,
        error: String,
    },
    Imported {
        request: ImportRequest,
        hash: String,
        kind: String,
        sizes: Vec<(f64, f64)>,
    },
    ImportFailed {
        request: ImportRequest,
        error: String,
    },
    ExistingRead {
        document_id: u64,
        existing: crate::adopt::Existing,
    },
    ExistingFailed {
        document_id: u64,
        error: String,
    },
    /// The private temporary file is ready. The UI must explicitly commit it
    /// after confirming that the originating action is still current.
    SavePrepared {
        request: SaveRequest,
        temporary: PathBuf,
    },
    Saved {
        request: SaveRequest,
    },
    SaveFailed {
        request: SaveRequest,
        error: String,
    },
}

/// A failed attempt to submit owner-worker work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RasterSubmitError {
    Stopped,
    QueueFull,
    Exhausted,
    Unavailable(String),
}

impl std::fmt::Display for RasterSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("the PDF source worker has stopped"),
            Self::QueueFull => f.write_str("the PDF source worker queue is full"),
            Self::Exhausted => f.write_str("the PDF source worker has exhausted request ids"),
            Self::Unavailable(error) => write!(f, "the PDF source worker is unavailable: {error}"),
        }
    }
}

impl std::error::Error for RasterSubmitError {}

const MAX_PENDING_PAGE_REQUESTS: usize = 32;
const MAX_PENDING_EVENTS: usize = 64;
const MAX_PENDING_OPEN_SETTLEMENTS: usize = 64;
/// Images are shared with the backend cache, but queued events keep their
/// blobs alive after cache eviction. Bound those references independently.
const MAX_PENDING_RESULT_BYTES: usize = 32 * 1024 * 1024;
/// A pathological page can expose millions of glyphs. Returning a partial text
/// layer would make search/selection lie, so extraction declines the whole
/// layer once it crosses this honest transport/cache limit.
const MAX_TEXT_LAYER_GLYPHS: usize = 100_000;
/// Link areas read from one page. Real documents carry a handful; a page
/// claiming thousands is hostile or degenerate, and the reader need not
/// hit-test more.
const MAX_PAGE_LINKS: usize = 2_000;

struct WorkerQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

struct QueuedCommand {
    session_id: u64,
    command: RasterCommand,
}

struct QueuedEvent {
    session_id: u64,
    event: RasterEvent,
}

#[derive(Default)]
struct QueueState {
    /// Bytes for opens the host answered with contents, by open request id,
    /// with the session that asked (at most one per session).
    open_contents: HashMap<u64, (u64, String, Vec<u8>)>,
    /// Only the most recent queued open matters. The owner may already be
    /// reading an older one, but it cannot mutate the active document unless
    /// it succeeds; the request id lets the UI disregard that stale result.
    open: HashMap<u64, OpenRequest>,
    open_order: VecDeque<u64>,
    controls: VecDeque<QueuedCommand>,
    renders: VecDeque<QueuedCommand>,
    /// Each session's current tile batch, most important first, and the
    /// round-robin order sessions are served in.
    tiles: HashMap<u64, VecDeque<TileRequest>>,
    tile_order: VecDeque<u64>,
    texts: VecDeque<QueuedCommand>,
    reconciles: HashMap<u64, ReconcileRequest>,
    reconcile_order: VecDeque<u64>,
    saves: HashMap<u64, SaveRequest>,
    save_order: VecDeque<u64>,
    imports: HashMap<u64, ImportRequest>,
    import_order: VecDeque<u64>,
    shutdowns: VecDeque<u64>,
    /// Every accepted open receives exactly one settlement. These are separate
    /// from lossy/coalesced paint results, so a full image queue can never
    /// strand the UI in its loading state.
    open_events: VecDeque<QueuedEvent>,
    events: VecDeque<QueuedEvent>,
    event_bytes: usize,
    /// One loss notification per session, outside the lossy payload queue.
    evicted_sessions: HashSet<u64>,
    closing_sessions: HashSet<u64>,
    /// Each session's content store, from which its documents read imported
    /// pages and into which its imports are stored.
    content: HashMap<u64, ContentStore>,
    start_error: Option<String>,
    prefer_text: bool,
}

fn session_settlement_count(state: &QueueState, session_id: u64) -> usize {
    state
        .open_events
        .iter()
        .filter(|queued| queued.session_id == session_id)
        .count()
}

/// Thread-safe UI endpoint for the page-rendering backend.
///
/// Construction creates exactly one long-lived owner thread. It serialises all
/// engine access, reads paths off the UI thread, keeps a bounded/coalescing
/// work queue, and returns only UI-safe results. The old active document stays
/// active when a replacement open fails.
#[derive(Clone)]
pub struct RasterHandle {
    session: Arc<SessionGuard>,
}

struct SessionGuard {
    queue: Arc<WorkerQueue>,
    id: u64,
    closed: std::sync::atomic::AtomicBool,
}

impl SessionGuard {
    fn close(&self) {
        if !self.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            close_session(&self.queue, self.id);
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.close();
    }
}

impl RasterHandle {
    /// Connect a new reader session to the dedicated PDF owner thread. Its
    /// documents read imported pages from `content`, and its imports are
    /// stored there.
    ///
    /// # Errors
    ///
    /// Returns [`RasterSubmitError::Exhausted`] rather than reusing an id if
    /// the process has exhausted its non-reusable session-id space.
    pub fn new(content: ContentStore) -> Result<Self, RasterSubmitError> {
        let queue = shared_worker_queue();
        let id = next_session_id()?;
        queue.state.lock_recover().content.insert(id, content);
        Ok(Self {
            session: Arc::new(SessionGuard {
                queue,
                id,
                closed: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }

    /// Queue the newest path to open. Queued opens are deliberately coalesced:
    /// selecting A, then B, then C never makes the worker parse all three.
    ///
    /// # Errors
    ///
    /// Returns an error after shutdown, when worker startup failed, or when
    /// the non-reusable request-id space is exhausted.
    pub fn open_path(&self, path: impl AsRef<Path>) -> Result<OpenRequest, RasterSubmitError> {
        let request = OpenRequest {
            id: next_open_request_id(&NEXT_OPEN_REQUEST_ID)?,
            path: path.as_ref().to_path_buf(),
        };
        self.enqueue(RasterCommand::OpenPath(request.clone()))?;
        Ok(request)
    }

    /// Queue a document the host handed over as bytes; otherwise as
    /// [`Self::open_path`].
    ///
    /// # Errors
    ///
    /// As [`Self::open_path`].
    pub fn open_contents(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<OpenRequest, RasterSubmitError> {
        let request = OpenRequest {
            id: next_open_request_id(&NEXT_OPEN_REQUEST_ID)?,
            path: Path::new("picked").join(name),
        };
        {
            let mut state = self.session.queue.state.lock_recover();
            let session = self.session.id;
            state
                .open_contents
                .retain(|_, (owner, _, _)| *owner != session);
            state
                .open_contents
                .insert(request.id, (session, name.to_owned(), bytes));
        }
        self.enqueue(RasterCommand::OpenPath(request.clone()))?;
        Ok(request)
    }

    /// Commit a matching `Opened` event after the UI has installed its plain
    /// document projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped, is unavailable, or its
    /// bounded control queue is full.
    pub fn activate_document(&self, document_id: u64) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Activate { document_id })
    }

    /// Discard a result the UI did not accept (for example after cancellation
    /// or a newer selection). It is safe before the parse completes.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped, is unavailable, or its
    /// bounded control queue is full.
    pub fn discard_document(&self, document_id: u64) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Discard { document_id })
    }

    /// Read the markup already in the document; answered with
    /// [`RasterEvent::ExistingRead`].
    ///
    /// # Errors
    ///
    /// As [`Self::activate_document`].
    pub fn read_existing(&self, document_id: u64) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::ReadExisting { document_id })
    }

    /// Draw (or leave out) the document's own markup in page images. Images
    /// already delivered showed the other choice; the caller drops them.
    ///
    /// # Errors
    ///
    /// As [`Self::activate_document`].
    pub fn set_reader_markup(
        &self,
        document_id: u64,
        visible: bool,
    ) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::SetReaderMarkup {
            document_id,
            visible,
        })
    }

    /// Queue a page render, coalescing the same document/page/zoom bucket.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped or its bounded render
    /// queue is occupied by distinct work.
    pub fn request_render(&self, request: RenderRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Render(request))
    }

    /// Replace this session's queued tiles with `batch` (most important
    /// first; at most [`crate::tiles::MAX_TILE_BATCH`] are kept). An empty
    /// batch cancels every queued tile.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped or is unavailable.
    pub fn request_tiles(&self, batch: TileBatch) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Tiles(batch))
    }

    /// Queue extraction of one page text layer, coalescing duplicate pages.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped or its bounded text queue
    /// is occupied by distinct work.
    pub fn request_text(&self, request: TextRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Text(request))
    }

    /// Queue reading one page's link areas, coalescing duplicate pages. Link
    /// reads share the bounded page-work queue with text extraction.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped or its bounded page
    /// queue is occupied by distinct work.
    pub fn request_links(&self, request: LinksRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Links(request))
    }

    /// Queue the newest durable page plan.  Reorders/duplicates/deletes are
    /// collaborative state transitions, so unlike paint requests the worker
    /// keeps the newest plan per viewer session and reports its settlement.
    ///
    /// # Errors
    ///
    /// Returns an error if the owner worker is stopped or unavailable.
    pub fn reconcile_page_plan(&self, request: ReconcileRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Reconcile(request))
    }

    /// Queue the latest export for this session.
    ///
    /// # Errors
    ///
    /// Returns an error if the owner worker is stopped, unavailable, or its
    /// bounded control queue is full.
    pub fn save_annotated(&self, request: SaveRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Save(request))
    }

    /// Queue an import inspection without reading, parsing, or retaining the
    /// file on the UI thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped, is unavailable, or the
    /// per-session settlement capacity has been reached.
    pub fn import_path(
        &self,
        document_id: u64,
        path: impl AsRef<Path>,
        at: usize,
        after: Option<String>,
    ) -> Result<ImportRequest, RasterSubmitError> {
        let request = ImportRequest {
            id: next_open_request_id(&NEXT_OPEN_REQUEST_ID)?,
            document_id,
            path: path.as_ref().to_path_buf(),
            at,
            after,
        };
        self.enqueue(RasterCommand::Import(request.clone()))?;
        Ok(request)
    }

    /// Atomically publish a prepared private save after the UI has verified
    /// the action has not been cancelled or superseded.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped, is unavailable, or its
    /// bounded control queue is full.
    pub fn commit_save(
        &self,
        request: SaveRequest,
        temporary: PathBuf,
    ) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::CommitSave { request, temporary })
    }

    /// Cancel a save. A queued request is removed; a prepared temporary file
    /// is deleted on the owner thread. The selected destination is untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped, is unavailable, or its
    /// bounded control queue is full.
    pub fn cancel_save(&self, request: SaveRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::CancelSave { request })
    }

    /// Drain completed work without waiting. UI code should call this once per
    /// frame/event turn and match only its current document/request id.
    pub fn drain_events(&self) -> Vec<RasterEvent> {
        pump_inline(&self.session.queue);
        let mut state = self.session.queue.state.lock_recover();
        let mut events = drain_session_events(&mut state.open_events, self.session.id);
        events.extend(drain_session_events(&mut state.events, self.session.id));
        if state.evicted_sessions.remove(&self.session.id) {
            events.push(RasterEvent::VisualWorkEvicted);
        }
        state.event_bytes = state.events.iter().fold(0usize, |bytes, queued| {
            bytes.saturating_add(page_event_bytes(&queued.event))
        });
        drop(state);
        self.session.queue.ready.notify_all();
        events
    }

    /// Whether the UI thread serves the PDF owner (no thread could start)
    /// and has work queued or results waiting, so the window must keep
    /// producing frames until they are drained.
    #[must_use]
    pub fn wants_frames(&self) -> bool {
        let inline = INLINE_WORKER.lock_recover().is_some();
        if !inline {
            return false;
        }
        let state = self.session.queue.state.lock_recover();
        !(state.shutdowns.is_empty()
            && state.controls.is_empty()
            && state.open_order.is_empty()
            && state.reconcile_order.is_empty()
            && state.save_order.is_empty()
            && state.import_order.is_empty()
            && state.renders.is_empty()
            && state.tile_order.is_empty()
            && state.texts.is_empty()
            && state.open_events.is_empty()
            && state.events.is_empty())
    }

    /// Ask the owner thread to stop after its current operation. This is mainly
    /// useful for tests and explicit app teardown; dropping a handle does not
    /// block the UI thread waiting for a native PDF parse to finish.
    pub fn shutdown(&self) {
        self.session.close();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "centralizing queue admission makes its bounded/coalescing protocol auditable"
    )]
    fn enqueue(&self, command: RasterCommand) -> Result<(), RasterSubmitError> {
        let mut state = self.session.queue.state.lock_recover();
        if let Some(error) = &state.start_error {
            return Err(RasterSubmitError::Unavailable(error.clone()));
        }
        if self
            .session
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
            || state.closing_sessions.contains(&self.session.id)
        {
            return Err(RasterSubmitError::Stopped);
        }
        match command {
            RasterCommand::OpenPath(request) => {
                // Never let an unpolled window hold the shared PDF owner
                // hostage. Keeping two slots accounts for an in-flight parse
                // and a superseded-open settlement; events stay exact per
                // session while unrelated windows continue to make progress.
                if session_settlement_count(&state, self.session.id)
                    >= MAX_PENDING_OPEN_SETTLEMENTS.saturating_sub(2)
                {
                    return Err(RasterSubmitError::QueueFull);
                }
                let was_pending = state.open.contains_key(&self.session.id);
                if let Some(superseded) = state.open.insert(self.session.id, request) {
                    state.open_events.push_back(QueuedEvent {
                        session_id: self.session.id,
                        event: RasterEvent::OpenSuperseded {
                            request: superseded,
                        },
                    });
                }
                if !was_pending {
                    state.open_order.push_back(self.session.id);
                }
                // Page work from the prior document has no value once an open
                // is pending. Clearing it also prevents a rapid file switch
                // from retaining old bitmap/text payloads in the queue.
                state
                    .renders
                    .retain(|queued| queued.session_id != self.session.id);
                drop_session_tiles(&mut state, self.session.id);
                state
                    .texts
                    .retain(|queued| queued.session_id != self.session.id);
                state.reconciles.remove(&self.session.id);
                state
                    .reconcile_order
                    .retain(|queued| *queued != self.session.id);
            }
            command @ (RasterCommand::Activate { .. }
            | RasterCommand::Discard { .. }
            | RasterCommand::ReadExisting { .. }
            | RasterCommand::SetReaderMarkup { .. }) => {
                if state.controls.len() >= MAX_PENDING_PAGE_REQUESTS {
                    return Err(RasterSubmitError::QueueFull);
                }
                state.controls.push_back(QueuedCommand {
                    session_id: self.session.id,
                    command,
                });
            }
            RasterCommand::Render(mut request) => {
                // Snap prior to comparison: a continuous pinch gesture can
                // submit hundreds of widths but only one bitmap per bucket is
                // useful to the UI.
                request.target_width = RasterBackend::bucket_for(request.target_width);
                if !state.renders.iter().any(|queued| {
                    queued.session_id == self.session.id
                        && matches!(&queued.command, RasterCommand::Render(old) if *old == request)
                }) {
                    if state.renders.len() >= MAX_PENDING_PAGE_REQUESTS {
                        return Err(RasterSubmitError::QueueFull);
                    }
                    state.renders.push_back(QueuedCommand {
                        session_id: self.session.id,
                        command: RasterCommand::Render(request),
                    });
                }
            }
            RasterCommand::Tiles(batch) => {
                let mut seen = HashSet::new();
                let tiles: VecDeque<TileRequest> = batch
                    .tiles
                    .into_iter()
                    .filter(|tile| tile.document_id == batch.document_id && seen.insert(*tile))
                    .take(crate::tiles::MAX_TILE_BATCH)
                    .collect();
                if tiles.is_empty() {
                    state.tiles.remove(&self.session.id);
                    state.tile_order.retain(|queued| *queued != self.session.id);
                } else {
                    if !state.tiles.contains_key(&self.session.id) {
                        state.tile_order.push_back(self.session.id);
                    }
                    state.tiles.insert(self.session.id, tiles);
                }
            }
            // A single tile is only ever taken off a batch by the worker.
            RasterCommand::Tile(request) => {
                if !state.tiles.contains_key(&self.session.id) {
                    state.tile_order.push_back(self.session.id);
                }
                let queue = state.tiles.entry(self.session.id).or_default();
                if !queue.contains(&request) && queue.len() < crate::tiles::MAX_TILE_BATCH {
                    queue.push_back(request);
                }
            }
            RasterCommand::Text(request) => {
                if !state.texts.iter().any(|queued| {
                    queued.session_id == self.session.id
                        && matches!(&queued.command, RasterCommand::Text(old) if *old == request)
                }) {
                    if state.texts.len() >= MAX_PENDING_PAGE_REQUESTS {
                        return Err(RasterSubmitError::QueueFull);
                    }
                    state.texts.push_back(QueuedCommand {
                        session_id: self.session.id,
                        command: RasterCommand::Text(request),
                    });
                }
            }
            RasterCommand::Links(request) => {
                if !state.texts.iter().any(|queued| {
                    queued.session_id == self.session.id
                        && matches!(&queued.command, RasterCommand::Links(old) if *old == request)
                }) {
                    if state.texts.len() >= MAX_PENDING_PAGE_REQUESTS {
                        return Err(RasterSubmitError::QueueFull);
                    }
                    state.texts.push_back(QueuedCommand {
                        session_id: self.session.id,
                        command: RasterCommand::Links(request),
                    });
                }
            }
            RasterCommand::Reconcile(request) => {
                let was_pending = state.reconciles.contains_key(&self.session.id);
                state.reconciles.insert(self.session.id, request);
                if !was_pending {
                    state.reconcile_order.push_back(self.session.id);
                }
                // Old pixels/text are no longer valid after a page-plan edit.
                state
                    .renders
                    .retain(|queued| queued.session_id != self.session.id);
                drop_session_tiles(&mut state, self.session.id);
                state
                    .texts
                    .retain(|queued| queued.session_id != self.session.id);
            }
            RasterCommand::Save(request) => {
                if session_settlement_count(&state, self.session.id)
                    >= MAX_PENDING_OPEN_SETTLEMENTS.saturating_sub(2)
                {
                    return Err(RasterSubmitError::QueueFull);
                }
                let was_pending = state.saves.contains_key(&self.session.id);
                state.saves.insert(self.session.id, request);
                if !was_pending {
                    state.save_order.push_back(self.session.id);
                }
            }
            RasterCommand::Import(request) => {
                if session_settlement_count(&state, self.session.id)
                    >= MAX_PENDING_OPEN_SETTLEMENTS.saturating_sub(2)
                {
                    return Err(RasterSubmitError::QueueFull);
                }
                let was_pending = state.imports.contains_key(&self.session.id);
                state.imports.insert(self.session.id, request);
                if !was_pending {
                    state.import_order.push_back(self.session.id);
                }
            }
            command @ (RasterCommand::CommitSave { .. } | RasterCommand::CancelSave { .. }) => {
                if state.controls.len() >= MAX_PENDING_PAGE_REQUESTS {
                    return Err(RasterSubmitError::QueueFull);
                }
                if let RasterCommand::CancelSave { request } = &command
                    && state
                        .saves
                        .get(&self.session.id)
                        .is_some_and(|queued| queued.id == request.id)
                {
                    state.saves.remove(&self.session.id);
                    state.save_order.retain(|queued| *queued != self.session.id);
                }
                state.controls.push_back(QueuedCommand {
                    session_id: self.session.id,
                    command,
                });
            }
            RasterCommand::Shutdown => {
                close_session_locked(&mut state, self.session.id);
            }
        }
        drop(state);
        self.session.queue.ready.notify_one();
        Ok(())
    }
}

static NEXT_OPEN_REQUEST_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static NEXT_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_open_request_id(counter: &std::sync::atomic::AtomicU64) -> Result<u64, RasterSubmitError> {
    counter
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| id.checked_add(1),
        )
        .map_err(|_| RasterSubmitError::Exhausted)
}

/// Allocate a protocol operation id for a caller-owned request payload.
/// IDs are never reused, which makes late worker events inert by comparison.
///
/// # Errors
///
/// Returns [`RasterSubmitError::Exhausted`] once the non-reusable id space is
/// exhausted.
pub fn next_worker_request_id() -> Result<u64, RasterSubmitError> {
    next_open_request_id(&NEXT_OPEN_REQUEST_ID)
}

fn next_session_id() -> Result<u64, RasterSubmitError> {
    next_open_request_id(&NEXT_SESSION_ID)
}

fn shared_worker_queue() -> Arc<WorkerQueue> {
    // The engine serialises requests to its helper anyway; one owner thread
    // keeps the bitmap caches and page plans of every viewer window in one
    // place. Every RasterHandle shares this one worker.
    static QUEUE: OnceLock<Arc<WorkerQueue>> = OnceLock::new();
    Arc::clone(QUEUE.get_or_init(|| {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        if !ui::rt::available(ui::rt::Feature::Threads) {
            // A browser page: the UI thread serves the queue between frames.
            *INLINE_WORKER.lock_recover() = Some(SourceWorker::default());
            return queue;
        }
        let worker_queue = Arc::clone(&queue);
        if let Err(error) = ui::rt::spawn_thread("pdf-source", move || source_worker(&worker_queue))
        {
            let mut state = queue.state.lock_recover();
            state.start_error = Some(error.to_string());
        }
        queue
    }))
}

/// The owner where no thread can start: [`RasterHandle::drain_events`] serves
/// the shared queue with it, a bounded slice of work per frame.
static INLINE_WORKER: Mutex<Option<SourceWorker>> = Mutex::new(None);

/// How long one frame may spend serving queued work on the UI thread.
const INLINE_BUDGET: Duration = Duration::from_millis(12);

#[allow(
    clippy::significant_drop_tightening,
    reason = "the inline worker stays borrowed for every command this frame serves"
)]
fn pump_inline(queue: &Arc<WorkerQueue>) {
    let mut inline = INLINE_WORKER.lock_recover();
    let Some(worker) = inline.as_mut() else {
        return;
    };
    let started = ui::time::now();
    loop {
        let command = next_command(&mut queue.state.lock_recover());
        let Some(command) = command else {
            break;
        };
        worker.handle(queue, command);
        if ui::time::now().duration_since(started) >= INLINE_BUDGET {
            break;
        }
    }
}

fn close_session(queue: &WorkerQueue, session_id: u64) {
    let mut state = queue.state.lock_recover();
    close_session_locked(&mut state, session_id);
    drop(state);
    queue.ready.notify_one();
}

/// The next page-level job: whole-page renders, tiles and text layers.
/// Rendering and text work alternate so a long visible render burst cannot
/// starve selection/search indefinitely. Whole-page renders (placeholders,
/// thumbnails) come before tiles, so a zoomed page is never blank while its
/// tiles arrive.
fn next_page_work(state: &mut QueueState) -> Option<QueuedCommand> {
    let command = if state.prefer_text {
        state
            .texts
            .pop_front()
            .or_else(|| state.renders.pop_front())
            .or_else(|| pop_tile(state))
    } else {
        state
            .renders
            .pop_front()
            .or_else(|| pop_tile(state))
            .or_else(|| state.texts.pop_front())
    };
    if command.is_some() {
        state.prefer_text = !state.prefer_text;
    }
    command
}

fn drop_session_tiles(state: &mut QueueState, session_id: u64) {
    state.tiles.remove(&session_id);
    state.tile_order.retain(|queued| *queued != session_id);
}

/// The next queued tile, serving sessions round-robin so one viewer's large
/// batch cannot starve another window.
fn pop_tile(state: &mut QueueState) -> Option<QueuedCommand> {
    while let Some(session_id) = state.tile_order.pop_front() {
        let Some(queue) = state.tiles.get_mut(&session_id) else {
            continue;
        };
        let Some(tile) = queue.pop_front() else {
            state.tiles.remove(&session_id);
            continue;
        };
        if queue.is_empty() {
            state.tiles.remove(&session_id);
        } else {
            state.tile_order.push_back(session_id);
        }
        return Some(QueuedCommand {
            session_id,
            command: RasterCommand::Tile(tile),
        });
    }
    None
}

fn close_session_locked(state: &mut QueueState, session_id: u64) {
    if state.start_error.is_some() {
        return;
    }
    state.closing_sessions.insert(session_id);
    state.evicted_sessions.remove(&session_id);
    state.open.remove(&session_id);
    state
        .open_contents
        .retain(|_, (owner, _, _)| *owner != session_id);
    state.open_order.retain(|queued| *queued != session_id);
    state
        .controls
        .retain(|queued| queued.session_id != session_id);
    state
        .renders
        .retain(|queued| queued.session_id != session_id);
    drop_session_tiles(state, session_id);
    state.texts.retain(|queued| queued.session_id != session_id);
    state.reconciles.remove(&session_id);
    state.reconcile_order.retain(|queued| *queued != session_id);
    state.saves.remove(&session_id);
    state.save_order.retain(|queued| *queued != session_id);
    state.imports.remove(&session_id);
    state.import_order.retain(|queued| *queued != session_id);
    state
        .open_events
        .retain(|queued| queued.session_id != session_id);
    state
        .events
        .retain(|queued| queued.session_id != session_id);
    state.event_bytes = state.events.iter().fold(0usize, |bytes, queued| {
        bytes.saturating_add(page_event_bytes(&queued.event))
    });
    if !state.shutdowns.contains(&session_id) {
        state.shutdowns.push_back(session_id);
    }
}

fn source_worker(queue: &Arc<WorkerQueue>) {
    let mut worker = SourceWorker::default();
    loop {
        let command = {
            let mut state = queue.state.lock_recover();
            loop {
                if let Some(command) = next_command(&mut state) {
                    break command;
                }
                state = queue
                    .ready
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        worker.handle(queue, command);
    }
}

/// The next command the owner serves, in priority order, or `None` when the
/// queue is idle.
fn next_command(state: &mut QueueState) -> Option<QueuedCommand> {
    if let Some(session_id) = state.shutdowns.pop_front() {
        return Some(QueuedCommand {
            session_id,
            command: RasterCommand::Shutdown,
        });
    }
    if let Some(control) = state.controls.pop_front() {
        return Some(control);
    }
    if let Some(session_id) = state.open_order.pop_front()
        && let Some(request) = state.open.remove(&session_id)
    {
        return Some(QueuedCommand {
            session_id,
            command: RasterCommand::OpenPath(request),
        });
    }
    if let Some(session_id) = state.reconcile_order.pop_front()
        && let Some(request) = state.reconciles.remove(&session_id)
    {
        return Some(QueuedCommand {
            session_id,
            command: RasterCommand::Reconcile(request),
        });
    }
    if let Some(session_id) = state.save_order.pop_front()
        && let Some(request) = state.saves.remove(&session_id)
    {
        return Some(QueuedCommand {
            session_id,
            command: RasterCommand::Save(request),
        });
    }
    if let Some(session_id) = state.import_order.pop_front()
        && let Some(request) = state.imports.remove(&session_id)
    {
        return Some(QueuedCommand {
            session_id,
            command: RasterCommand::Import(request),
        });
    }
    if let Some(command) = next_page_work(state) {
        return Some(command);
    }
    None
}

/// The owner's state: the documents each session has open and the saves it
/// has prepared. Served by the owner thread, or pumped on the UI thread where
/// no thread can start (a browser page).
#[derive(Default)]
struct SourceWorker {
    // A session may briefly own both the visible predecessor and a parsed
    // replacement.  We retain the former until the UI explicitly activates
    // the latter, so a cancellation or stale result cannot turn the readable
    // document into an unusable document-id mismatch.
    active: HashMap<u64, HashMap<u64, RasterBackend>>,
    // PDFium writes only here until the UI explicitly commits the operation.
    // Keeping these paths worker-local prevents an abandoned action from ever
    // racing a later save selection on the UI thread.
    prepared_saves: HashMap<(u64, u64), (SaveRequest, PrivateSave)>,
}

impl SourceWorker {
    #[allow(
        clippy::too_many_lines,
        reason = "the worker keeps its serial ownership protocol auditable in one place"
    )]
    fn handle(&mut self, queue: &Arc<WorkerQueue>, command: QueuedCommand) {
        let Self {
            active,
            prepared_saves,
        } = self;
        let session_id = command.session_id;
        let event = match command.command {
            RasterCommand::OpenPath(request) => {
                let contents = queue.state.lock_recover().open_contents.remove(&request.id);
                let content = session_content(queue, session_id);
                let opened = match contents {
                    Some((_, name, bytes)) => open_contents(&name, &bytes, &content),
                    None => open(&request.path, &content),
                };
                match opened {
                    Ok(LoadedDoc { doc, raster, name }) => {
                        let page_sizes = (0..raster.page_count())
                            .map(|page| raster.page_size(page))
                            .collect();
                        let (geometries, rotations) = raster.page_layout();
                        let form_fields = raster.form_fields();
                        let source_hash = raster.source_hash().to_string();
                        let source_len = raster.source_len();
                        active
                            .entry(session_id)
                            .or_default()
                            .insert(request.id, raster);
                        let raster = RasterDocument {
                            id: request.id,
                            page_sizes,
                            geometries,
                            rotations,
                            form_fields,
                            source_hash,
                            source_len,
                        };
                        RasterEvent::Opened {
                            request,
                            document: doc,
                            name,
                            raster,
                        }
                    }
                    Err(error) => RasterEvent::OpenFailed {
                        request,
                        error: error.to_string(),
                    },
                }
            }
            RasterCommand::Activate { document_id } => {
                if let Some(documents) = active.get_mut(&session_id) {
                    documents.retain(|id, _| *id == document_id);
                }
                return;
            }
            RasterCommand::Discard { document_id } => {
                if let Some(documents) = active.get_mut(&session_id) {
                    documents.remove(&document_id);
                }
                return;
            }
            RasterCommand::ReadExisting { document_id } => match active
                .get(&session_id)
                .and_then(|documents| documents.get(&document_id))
            {
                Some(backend) => match backend.existing() {
                    Ok(existing) => RasterEvent::ExistingRead {
                        document_id,
                        existing,
                    },
                    Err(error) => RasterEvent::ExistingFailed {
                        document_id,
                        error: error.to_string(),
                    },
                },
                None => RasterEvent::ExistingFailed {
                    document_id,
                    error: "the requested raster document is no longer active".to_string(),
                },
            },
            RasterCommand::SetReaderMarkup {
                document_id,
                visible,
            } => {
                if let Some(backend) = active
                    .get_mut(&session_id)
                    .and_then(|documents| documents.get_mut(&document_id))
                {
                    backend.set_reader_markup(visible);
                }
                return;
            }
            RasterCommand::Render(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => RasterEvent::Rendered {
                    request,
                    image: backend.render(request.page, request.target_width),
                },
                _ => RasterEvent::RenderFailed {
                    request,
                    error: "the requested raster document is no longer active".to_string(),
                },
            },
            RasterCommand::Tile(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => RasterEvent::TileRendered {
                    request,
                    image: backend.render_tile(
                        request.page,
                        request.level,
                        request.col,
                        request.row,
                    ),
                },
                _ => RasterEvent::TileFailed { request },
            },
            // Batches are unpacked at admission and never reach the worker.
            RasterCommand::Tiles(_) => return,
            RasterCommand::Text(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => RasterEvent::TextExtracted {
                    request,
                    text: backend.page_text(request.page),
                },
                _ => RasterEvent::TextFailed {
                    request,
                    error: "the requested raster document is no longer active".to_string(),
                },
            },
            RasterCommand::Links(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => RasterEvent::LinksRead {
                    request,
                    links: backend.page_links(request.page),
                },
                _ => RasterEvent::LinksFailed { request },
            },
            RasterCommand::Reconcile(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => {
                    if backend.reconcile_page_plan(&request.plan) {
                        let page_sizes = (0..backend.page_count())
                            .map(|page| backend.page_size(page))
                            .collect();
                        let (geometries, rotations) = backend.page_layout();
                        RasterEvent::Reconciled {
                            request,
                            page_sizes,
                            geometries,
                            rotations,
                        }
                    } else {
                        RasterEvent::ReconcileFailed {
                            request,
                            error:
                                "the shared page plan needs source bytes unavailable on this device"
                                    .to_string(),
                        }
                    }
                }
                _ => RasterEvent::ReconcileFailed {
                    request,
                    error: "the requested raster document is no longer active".to_string(),
                },
            },
            RasterCommand::Save(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => match backend.export_bytes(&request.markup) {
                    Ok(bytes) => {
                        let private = private_save(&request.path, request.id, bytes);
                        let temporary = private.temporary.clone();
                        prepared_saves.insert((session_id, request.id), (request.clone(), private));
                        RasterEvent::SavePrepared { request, temporary }
                    }
                    Err(error) => RasterEvent::SaveFailed {
                        request,
                        error: error.to_string(),
                    },
                },
                None => RasterEvent::SaveFailed {
                    request,
                    error: "the requested raster document is no longer active".to_string(),
                },
            },
            RasterCommand::Import(request) => match active
                .get_mut(&session_id)
                .and_then(|documents| documents.get_mut(&request.document_id))
            {
                Some(backend) => match inspect_import_for_store(backend, &request.path) {
                    Ok((hash, kind, sizes)) => RasterEvent::Imported {
                        request,
                        hash,
                        kind,
                        sizes,
                    },
                    Err(error) => RasterEvent::ImportFailed {
                        request,
                        error: error.to_string(),
                    },
                },
                None => RasterEvent::ImportFailed {
                    request,
                    error: "the requested raster document is no longer active".to_string(),
                },
            },
            RasterCommand::CommitSave { request, temporary } => {
                if let Some((prepared, private)) = prepared_saves.remove(&(session_id, request.id))
                    && prepared == request
                    && private.temporary == temporary
                {
                    match flush_private_save(&private, &request.path) {
                        Ok(()) => RasterEvent::Saved { request },
                        Err(error) => RasterEvent::SaveFailed {
                            request,
                            error: error.to_string(),
                        },
                    }
                } else {
                    RasterEvent::SaveFailed {
                        request,
                        error: "the prepared save is no longer available".to_string(),
                    }
                }
            }
            RasterCommand::CancelSave { request } => {
                if let Some((_, temporary)) = prepared_saves.remove(&(session_id, request.id)) {
                    drop(temporary);
                }
                return;
            }
            RasterCommand::Shutdown => {
                active.remove(&session_id);
                prepared_saves.retain(|(owner, _), _| *owner != session_id);
                let mut state = queue.state.lock_recover();
                state.closing_sessions.remove(&session_id);
                state.content.remove(&session_id);
                drop(state);
                return;
            }
        };
        push_event(queue, session_id, event);
    }
}

/// The content store `session_id` registered. A session always registers one
/// before it can queue work; the empty fallback only covers a closed session.
fn session_content(queue: &WorkerQueue, session_id: u64) -> ContentStore {
    queue
        .state
        .lock_recover()
        .content
        .get(&session_id)
        .cloned()
        .unwrap_or_else(ContentStore::memory)
}

fn push_event(queue: &WorkerQueue, session_id: u64, event: RasterEvent) {
    let mut state = queue.state.lock_recover();
    if matches!(
        &event,
        RasterEvent::Opened { .. }
            | RasterEvent::OpenFailed { .. }
            | RasterEvent::OpenSuperseded { .. }
            | RasterEvent::Reconciled { .. }
            | RasterEvent::ReconcileFailed { .. }
            | RasterEvent::Imported { .. }
            | RasterEvent::ImportFailed { .. }
            | RasterEvent::ExistingRead { .. }
            | RasterEvent::ExistingFailed { .. }
            | RasterEvent::SavePrepared { .. }
            | RasterEvent::Saved { .. }
            | RasterEvent::SaveFailed { .. }
    ) {
        // Terminal work is a state transition, never best-effort paint. The
        // per-session admission limit above reserves room for every accepted
        // completion, so this enqueue never blocks the sole owner thread just
        // because another window has stopped draining its own events.
        if !state.closing_sessions.contains(&session_id) {
            state
                .open_events
                .push_back(QueuedEvent { session_id, event });
        }
        return;
    }

    let event = bound_page_event(event);
    // Results coalesce too. A superseded bitmap must not make the UI process a
    // long tail of stale scroll frames after it catches up.
    let mut index = 0;
    while index < state.events.len() {
        let is_same = state.events.get(index).is_some_and(|queued| {
            queued.session_id == session_id && same_page_result(&queued.event, &event)
        });
        if is_same {
            remove_page_event(&mut state, index);
        } else {
            index += 1;
        }
    }
    let bytes = page_event_bytes(&event);
    while state.events.len() >= MAX_PENDING_EVENTS
        || state.event_bytes.saturating_add(bytes) > MAX_PENDING_RESULT_BYTES
    {
        if state.events.is_empty() {
            // `bound_page_event` converts any individually oversized payload
            // into an explicit failure, so this cannot spin forever.
            break;
        }
        if let Some(discarded) = state.events.front() {
            let owner = discarded.session_id;
            state.evicted_sessions.insert(owner);
        }
        remove_page_event(&mut state, 0);
    }
    state.event_bytes = state.event_bytes.saturating_add(bytes);
    state.events.push_back(QueuedEvent { session_id, event });
}

fn same_page_result(left: &RasterEvent, right: &RasterEvent) -> bool {
    match (left, right) {
        (
            RasterEvent::Rendered { request: old, .. }
            | RasterEvent::RenderFailed { request: old, .. },
            RasterEvent::Rendered { request, .. } | RasterEvent::RenderFailed { request, .. },
        ) => {
            old.document_id == request.document_id
                && old.page == request.page
                && RasterBackend::bucket_for(old.target_width)
                    == RasterBackend::bucket_for(request.target_width)
        }
        (
            RasterEvent::TextExtracted { request: old, .. }
            | RasterEvent::TextFailed { request: old, .. },
            RasterEvent::TextExtracted { request, .. } | RasterEvent::TextFailed { request, .. },
        ) => old.document_id == request.document_id && old.page == request.page,
        (
            RasterEvent::TileRendered { request: old, .. }
            | RasterEvent::TileFailed { request: old, .. },
            RasterEvent::TileRendered { request, .. } | RasterEvent::TileFailed { request, .. },
        ) => old == request,
        (
            RasterEvent::LinksRead { request: old, .. }
            | RasterEvent::LinksFailed { request: old, .. },
            RasterEvent::LinksRead { request, .. } | RasterEvent::LinksFailed { request, .. },
        ) => old.document_id == request.document_id && old.page == request.page,
        _ => false,
    }
}

fn page_event_bytes(event: &RasterEvent) -> usize {
    match event {
        RasterEvent::Rendered {
            image: Some(image), ..
        }
        | RasterEvent::TileRendered {
            image: Some(image), ..
        } => usize::try_from(image.width)
            .ok()
            .and_then(|width| {
                usize::try_from(image.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .unwrap_or(usize::MAX),
        RasterEvent::TextExtracted { text, .. } => text.text.len().saturating_add(
            text.chars
                .len()
                .saturating_mul(std::mem::size_of::<crate::text_layer::CharBox>()),
        ),
        RasterEvent::LinksRead { links, .. } => links
            .iter()
            .map(|link| match &link.target {
                LinkTarget::Uri(s) | LinkTarget::File(s) => s.len(),
                LinkTarget::Page { .. } => 0,
            })
            .sum::<usize>()
            .saturating_add(links.len().saturating_mul(std::mem::size_of::<PageLink>())),
        _ => 0,
    }
}

fn bound_page_event(event: RasterEvent) -> RasterEvent {
    if page_event_bytes(&event) <= MAX_PENDING_RESULT_BYTES {
        return event;
    }
    match event {
        RasterEvent::Rendered { request, .. } => RasterEvent::RenderFailed {
            request,
            error: "the rendered page exceeds the UI result memory limit".to_string(),
        },
        RasterEvent::TextExtracted { request, .. } => RasterEvent::TextFailed {
            request,
            error: "the extracted text layer exceeds the UI result memory limit".to_string(),
        },
        RasterEvent::TileRendered { request, .. } => RasterEvent::TileFailed { request },
        _ => event,
    }
}

fn remove_page_event(state: &mut QueueState, index: usize) {
    if let Some(event) = state.events.remove(index) {
        state.event_bytes = state
            .event_bytes
            .saturating_sub(page_event_bytes(&event.event));
    }
}

fn drain_session_events(events: &mut VecDeque<QueuedEvent>, session_id: u64) -> Vec<RasterEvent> {
    let mut mine = Vec::new();
    let pending = std::mem::take(events);
    for queued in pending {
        if queued.session_id == session_id {
            mine.push(queued.event);
        } else {
            events.push_back(queued);
        }
    }
    mine
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicted_page_results_notify_only_their_session_once() {
        let shared = queue();
        let first = test_handle(Arc::clone(&shared), 1);
        let second = test_handle(Arc::clone(&shared), 2);
        // Share one allocation, but account each queued image's full size.
        let image = ui::raster::rgba8(vec![0; 4 * 1024 * 1024], 1024, 1024);
        for page in 0..12 {
            push_event(
                &shared,
                1,
                RasterEvent::Rendered {
                    request: RenderRequest {
                        document_id: 1,
                        page,
                        target_width: 1024,
                    },
                    image: Some(image.clone()),
                },
            );
        }
        let state = shared.state.lock().expect("queue lock");
        assert!(state.event_bytes <= MAX_PENDING_RESULT_BYTES);
        assert_eq!(state.evicted_sessions, HashSet::from([1]));
        drop(state);
        assert!(
            second.drain_events().is_empty(),
            "another viewer is unaffected"
        );
        let events = first.drain_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, RasterEvent::VisualWorkEvicted,))
                .count(),
            1,
            "loss is coalesced without losing its completion signal"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RasterEvent::Rendered { image: Some(_), .. },)),
            "the remaining images still reach the viewer"
        );
        assert!(
            first.drain_events().is_empty(),
            "the notification was consumed"
        );
    }

    fn test_handle(queue: Arc<WorkerQueue>, id: u64) -> RasterHandle {
        RasterHandle {
            session: Arc::new(SessionGuard {
                queue,
                id,
                closed: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    fn tile(document_id: u64, level: i32, col: u32, row: u32) -> TileRequest {
        TileRequest {
            document_id,
            page: 0,
            level,
            col,
            row,
        }
    }

    fn queue() -> Arc<WorkerQueue> {
        Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        })
    }

    fn open_request(id: u64, path: &str) -> OpenRequest {
        OpenRequest {
            id,
            path: path.into(),
        }
    }

    fn open(id: u64, path: &str) -> RasterCommand {
        RasterCommand::OpenPath(open_request(id, path))
    }

    fn text(document_id: u64, page: usize) -> TextRequest {
        TextRequest { document_id, page }
    }

    #[test]
    fn inputs_and_results_are_bounded_before_allocation() {
        let file = tempfile::NamedTempFile::new().expect("temporary source");
        std::fs::write(file.path(), b"12345").expect("write source");
        let error = read_file_bounded(file.path(), 4).expect_err("source exceeds cap");
        assert!(error.to_string().contains("opens files up to"));

        // (width, height, valid)
        for (w, h, valid) in [
            (612.0, 792.0, true),
            (f64::NAN, 792.0, false),
            (0.0, 792.0, false),
            (MAX_PAGE_EDGE_POINTS + 1.0, 792.0, false),
        ] {
            assert_eq!(valid_page_size(w, h), valid, "page size {w}x{h}");
        }

        let bucket = RasterBackend::bucket_for_page(4_096, (612.0, 20_000.0))
            .expect("a tall but valid page still gets a bounded raster");
        let pixels = f64::from(bucket) * f64::from(bucket) * 20_000.0 / 612.0;
        assert!(pixels <= MAX_RASTER_PIXELS * 1.01);

        let counter = std::sync::atomic::AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_open_request_id(&counter), Ok(u64::MAX - 1));
        assert_eq!(
            next_open_request_id(&counter),
            Err(RasterSubmitError::Exhausted)
        );

        // Result payloads are byte bounded.
        let request = RenderRequest {
            document_id: 1,
            page: 0,
            target_width: 4_096,
        };
        let oversized_image =
            ui::raster::rgba8(vec![0; MAX_PENDING_RESULT_BYTES + 1], 4_097, 2_048);
        assert!(matches!(
            bound_page_event(RasterEvent::Rendered {
                request,
                image: Some(oversized_image),
            }),
            RasterEvent::RenderFailed { .. }
        ));
        let oversized_text = Arc::new(PageText {
            text: "x".repeat(MAX_PENDING_RESULT_BYTES + 1),
            chars: Vec::new(),
            layer: None,
            map: None,
        });
        assert!(matches!(
            bound_page_event(RasterEvent::TextExtracted {
                request: text(1, 0),
                text: oversized_text,
            }),
            RasterEvent::TextFailed { .. }
        ));
    }

    #[test]
    fn a_tile_batch_replaces_stale_tiles_and_keeps_its_priority_order() {
        let queue = queue();
        let handle = test_handle(Arc::clone(&queue), 1);
        let other = test_handle(Arc::clone(&queue), 2);
        let batch = |document_id, tiles| TileBatch { document_id, tiles };
        // The first viewport, then a scroll: the new batch names what is on
        // screen now, nearest the centre first.
        handle
            .request_tiles(batch(
                7,
                vec![tile(7, 12, 0, 0), tile(7, 12, 1, 0), tile(7, 12, 2, 0)],
            ))
            .expect("first batch");
        handle
            .request_tiles(batch(
                7,
                vec![tile(7, 12, 1, 5), tile(7, 12, 1, 0), tile(7, 12, 1, 5)],
            ))
            .expect("second batch");
        other
            .request_tiles(batch(9, vec![tile(9, 8, 0, 0)]))
            .expect("another window");
        // A whole-page placeholder is served before any tile.
        handle
            .request_render(RenderRequest {
                document_id: 7,
                page: 0,
                target_width: 1024,
            })
            .expect("placeholder");
        let mut state = queue.state.lock().unwrap();
        let order: Vec<RasterCommand> = std::iter::from_fn(|| next_page_work(&mut state))
            .map(|queued| queued.command)
            .collect();
        assert!(matches!(order[0], RasterCommand::Render(_)));
        // Stale tiles (0,0) and (2,0) were dropped, the duplicate collapsed,
        // and the two windows are served in turn.
        assert_eq!(
            order[1..].to_vec(),
            vec![
                RasterCommand::Tile(tile(7, 12, 1, 5)),
                RasterCommand::Tile(tile(9, 8, 0, 0)),
                RasterCommand::Tile(tile(7, 12, 1, 0)),
            ]
        );
        drop(state);
        // An empty batch cancels whatever is still queued.
        handle
            .request_tiles(batch(7, vec![tile(7, 12, 3, 3)]))
            .expect("third batch");
        handle.request_tiles(batch(7, Vec::new())).expect("cancel");
        let mut state = queue.state.lock().unwrap();
        assert!(next_page_work(&mut state).is_none());
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn owner_queue_coalesces_requests_and_keeps_only_the_latest_open_and_plan() {
        // Build the endpoint without starting its worker so the test can
        // inspect exactly what a busy owner will receive.
        let handle = test_handle(queue(), 1);
        let first = open_request(1, "first.pdf");
        let latest = open_request(2, "latest.pdf");
        handle
            .enqueue(RasterCommand::OpenPath(first.clone()))
            .expect("queue first open");
        for (width, why) in [
            (101, "queue first render"),
            (110, "coalesce nearby zoom bucket"),
        ] {
            handle
                .request_render(RenderRequest {
                    document_id: first.id,
                    page: 3,
                    target_width: width,
                })
                .expect(why);
        }
        handle
            .request_text(text(first.id, 3))
            .expect("queue stale text");
        handle
            .enqueue(RasterCommand::OpenPath(latest.clone()))
            .expect("replace queued open");
        let state = handle.session.queue.state.lock().expect("queue lock");
        assert_ne!(first.id, latest.id);
        assert_eq!(state.open.get(&1), Some(&latest));
        assert_eq!(state.open_events.len(), 1);
        assert!(matches!(
            state.open_events.front(),
            Some(QueuedEvent {
                session_id: 1,
                event: RasterEvent::OpenSuperseded { request }
            }) if *request == first
        ));
        assert!(state.renders.is_empty(), "new open clears stale paints");
        assert!(state.texts.is_empty(), "new open clears stale text");
        drop(state);

        // Nearby page requests coalesce without an open in between.
        let handle = test_handle(queue(), 1);
        let request = RenderRequest {
            document_id: 2,
            page: 3,
            target_width: 101,
        };
        handle.request_render(request).expect("queue first render");
        handle
            .request_render(RenderRequest {
                target_width: 110,
                ..request
            })
            .expect("coalesce nearby zoom bucket");
        handle.request_text(text(2, 3)).expect("queue text");
        handle.request_text(text(2, 3)).expect("coalesce text");
        let state = handle.session.queue.state.lock().expect("queue lock");
        assert_eq!(state.renders.len(), 1);
        assert!(matches!(
            &state.renders[0].command,
            RasterCommand::Render(RenderRequest { target_width, .. })
                if *target_width == RasterBackend::bucket_for(101)
        ));
        assert_eq!(state.texts.len(), 1);
        drop(state);

        // Link reads share the page queue, coalesce, and are cleared by a plan edit.
        let handle = test_handle(queue(), 1);
        let request = LinksRequest {
            document_id: 2,
            page: 0,
        };
        handle.request_links(request).expect("queue links");
        handle.request_links(request).expect("coalesce links");
        handle
            .request_text(text(2, 0))
            .expect("text for the same page is distinct work");
        let state = handle.session.queue.state.lock().expect("queue lock");
        assert_eq!(state.texts.len(), 2);
        assert!(matches!(
            &state.texts[0].command,
            RasterCommand::Links(queued) if *queued == request
        ));
        drop(state);
        // Results for one page coalesce; text and links for it do not.
        let links = |n: usize| RasterEvent::LinksRead {
            request,
            links: Arc::new(
                (0..n)
                    .map(|_| PageLink {
                        rect: PtRect::new(0.0, 0.0, 1.0, 1.0),
                        target: LinkTarget::Page { page: 0, at: None },
                    })
                    .collect(),
            ),
        };
        assert!(same_page_result(&links(1), &links(2)));
        assert!(!same_page_result(
            &links(1),
            &RasterEvent::TextFailed {
                request: text(2, 0),
                error: String::new(),
            }
        ));
        handle
            .reconcile_page_plan(ReconcileRequest {
                document_id: 2,
                plan: Vec::new(),
            })
            .expect("queue plan");
        let state = handle.session.queue.state.lock().expect("queue lock");
        assert!(state.texts.is_empty(), "a plan edit drops stale link reads");
        drop(state);

        // Only the latest collaborative page plan is kept.
        let handle = test_handle(queue(), 1);
        let plan = |id: &str| ReconcileRequest {
            document_id: 7,
            plan: vec![PagePlanEntry {
                id: id.into(),
                source_page: 1,
                imported: None,
                rotation: 0,
            }],
        };
        handle
            .reconcile_page_plan(plan("first"))
            .expect("queue first plan");
        handle
            .reconcile_page_plan(plan("latest"))
            .expect("replace queued plan");
        let state = handle.session.queue.state.lock().expect("queue lock");
        assert_eq!(state.reconcile_order.len(), 1);
        assert_eq!(state.reconciles.get(&1), Some(&plan("latest")));
        drop(state);
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn sessions_are_isolated_and_settlements_bounded_per_session() {
        // An unavailable worker surfaces its startup error.
        let failed = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState {
                closing_sessions: HashSet::from([1]),
                start_error: Some("thread creation denied".to_string()),
                ..QueueState::default()
            }),
            ready: Condvar::new(),
        });
        assert_eq!(
            test_handle(failed, 1).request_text(text(1, 0)),
            Err(RasterSubmitError::Unavailable(
                "thread creation denied".to_string()
            ))
        );

        // Closing one session does not block another.
        let shared = queue();
        let first = test_handle(Arc::clone(&shared), 1);
        let second = test_handle(Arc::clone(&shared), 2);
        first
            .enqueue(open(1, "first.pdf"))
            .expect("first session queues open");
        second
            .enqueue(open(2, "second.pdf"))
            .expect("second session queues open");
        first.shutdown();
        second
            .request_text(text(2, 0))
            .expect("second session remains usable");
        let state = shared.state.lock().expect("queue lock");
        assert!(!state.open.contains_key(&1));
        assert!(state.open.contains_key(&2));
        assert!(state.texts.iter().any(|queued| queued.session_id == 2));
        drop(state);

        // A dropped session purges its undrained open settlements.
        let shared = queue();
        {
            let first = test_handle(Arc::clone(&shared), 1);
            for id in 1..=8 {
                first
                    .enqueue(open(id, &format!("{id}.pdf")))
                    .expect("queue transient open");
            }
            assert!(!first.drain_events().is_empty());
            // Queue one more settlement and intentionally do not drain it.
            first
                .enqueue(open(9, "undrained.pdf"))
                .expect("queue undrained open");
        }
        test_handle(Arc::clone(&shared), 2)
            .enqueue(open(10, "next.pdf"))
            .expect("dropped session cannot consume another session's capacity");
        let state = shared.state.lock().expect("queue lock");
        assert!(
            state
                .open_events
                .iter()
                .all(|queued| queued.session_id != 1)
        );
        drop(state);

        // Open settlements are bounded per session without losing the current completion.
        let shared = queue();
        let handle = test_handle(Arc::clone(&shared), 1);
        for id in 1..=63 {
            handle
                .enqueue(open(id, &format!("{id}.pdf")))
                .expect("settlement capacity reserves the current open");
        }
        assert!(matches!(
            handle.enqueue(open(64, "overflow.pdf")),
            Err(RasterSubmitError::QueueFull)
        ));
        let current = {
            let mut state = shared.state.lock().expect("queue lock");
            assert_eq!(
                session_settlement_count(&state, 1),
                MAX_PENDING_OPEN_SETTLEMENTS.saturating_sub(2)
            );
            let current = state.open.get(&1).cloned().expect("current open retained");
            let _ = state.open_events.pop_front();
            current
        };
        push_event(
            &shared,
            1,
            RasterEvent::OpenFailed {
                request: current.clone(),
                error: "fixture failure".to_string(),
            },
        );
        let events = handle.drain_events();
        assert!(matches!(
            events.last(),
            Some(RasterEvent::OpenFailed { request, .. }) if request == &current
        ));

        // A full hidden session cannot block another session's settlement.
        let shared = queue();
        shared.state.lock().expect("queue lock").open_events.extend(
            (0..MAX_PENDING_OPEN_SETTLEMENTS).map(|id| QueuedEvent {
                session_id: 1,
                event: RasterEvent::OpenFailed {
                    request: open_request(id as u64, "hidden.pdf"),
                    error: "fixture".into(),
                },
            }),
        );
        let visible = test_handle(Arc::clone(&shared), 2);
        visible
            .enqueue(open(900, "visible.pdf"))
            .expect("another session remains admissible");
        push_event(
            &shared,
            2,
            RasterEvent::OpenFailed {
                request: open_request(900, "visible.pdf"),
                error: "fixture".into(),
            },
        );
        assert!(matches!(
            visible.drain_events().as_slice(),
            [RasterEvent::OpenFailed { request, .. }] if request.id == 900
        ));
    }

    #[cfg(unix)]
    #[test]
    fn private_save_directory_does_not_follow_a_predictable_sibling_symlink() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("parent");
        let destination = parent.path().join("export.pdf");
        let victim = parent.path().join("victim.pdf");
        std::fs::write(&victim, b"keep").expect("victim");
        let predictable = parent.path().join(".export.pdf.tilo-7.saving");
        symlink(&victim, &predictable).expect("attacker symlink");

        let private = private_save(&destination, 7, b"safe export".to_vec());
        assert_ne!(private.temporary, predictable);
        assert!(
            !private.temporary.exists(),
            "nothing is written before the commit"
        );
        flush_private_save(&private, &destination).expect("atomic publish");

        assert_eq!(std::fs::read(&victim).expect("victim contents"), b"keep");
        assert_eq!(
            std::fs::read(&destination).expect("destination contents"),
            b"safe export"
        );
    }
}
