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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Instant;

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
use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat};

use crate::doc::{ImportedPage, PagePlanEntry};
use crate::model::{
    AnnoKind, Annotation, Comment, Document, FormEntry, MarkupStyle, OutlineItem, PtRect,
    Signature, SignatureInk,
};
use crate::text_layer::PageText;

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
    let metadata =
        std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if metadata.len() > limit as u64 {
        return Err(anyhow!(
            "file is {} MiB; PDF Viewer opens files up to {} MiB",
            metadata.len() / (1024 * 1024),
            limit / (1024 * 1024)
        ));
    }
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len().min(limit as u64) as usize);
    // Metadata may race with a writer. Reading one byte beyond the cap keeps
    // that race bounded too, rather than trusting the earlier size check.
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() > limit {
        return Err(anyhow!(
            "file grew beyond the {} MiB PDF Viewer open limit",
            limit / (1024 * 1024)
        ));
    }
    Ok(bytes)
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
    let hash = qdoc::blobstore::put(&bytes)
        .ok_or_else(|| anyhow!("the local content store refused the import"))?;
    qdoc::blobstore::store()
        .has(&hash)
        .then_some((hash, kind, sizes))
        .ok_or_else(|| anyhow!("the local content store refused the import"))
}

struct PrivateSave {
    temporary: PathBuf,
    directory: tempfile::TempDir,
}

fn private_save_path(destination: &Path, request_id: u64) -> anyhow::Result<PrivateSave> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    // A random, owner-only directory is deliberately used instead of a
    // predictable sibling filename. PDFium saves by pathname, so `create_new`
    // on a file alone would still leave an unlink-and-replace race in a shared
    // directory. The private directory prevents a local symlink/collision from
    // redirecting the export before the final atomic rename.
    let directory = tempfile::Builder::new()
        .prefix(&format!(".tilo-pdf-{request_id}-"))
        .tempdir_in(parent)
        .with_context(|| format!("creating a private save directory in {}", parent.display()))?;
    Ok(PrivateSave {
        temporary: directory.path().join("document.pdf"),
        directory,
    })
}

fn flush_private_save(private: &PrivateSave, destination: &Path) -> anyhow::Result<()> {
    std::fs::File::open(&private.temporary)
        .with_context(|| format!("opening private save {}", private.temporary.display()))?
        .sync_all()
        .with_context(|| format!("flushing private save {}", private.temporary.display()))?;
    std::fs::File::open(private.directory.path())
        .with_context(|| {
            format!(
                "opening private save directory {}",
                private.directory.path().display()
            )
        })?
        .sync_all()
        .with_context(|| {
            format!(
                "flushing private save directory {}",
                private.directory.path().display()
            )
        })?;
    std::fs::rename(&private.temporary, destination)
        .with_context(|| format!("publishing the saved PDF to {}", destination.display()))?;
    if let Some(parent) = destination.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        // A directory fsync is not available on every target. Where the host
        // supports it, it makes the rename durable before reporting success.
        let _ = directory.sync_all();
    }
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
    pub raster: Rasterizer,
    /// The file's display name (for the window chrome / status bar).
    pub name: String,
    /// The file's durable record identity ([`ui::RecordId::identify_file`]):
    /// the same across renames and moves, so its markup follows it.
    pub file: ui::RecordId,
}

/// Load a PDF from `path` through the shared PDF engine.
///
/// # Errors
///
/// Returns an error if `path` cannot be read, if the PDF engine is not
/// available on this host, or if it cannot parse the bytes as a PDF within
/// the reader's page and geometry limits.
pub fn open(path: &Path) -> anyhow::Result<LoadedDoc> {
    let t = Instant::now();
    let bytes = read_file_bounded(path, MAX_OPEN_BYTES)?;
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

    let t = Instant::now();
    let r = Rasterizer::open(&bytes)?;
    prof("Rasterizer::open (engine parse + page sizes)", t);
    let (mut title, author, subject) = r.metadata();
    if title.trim().is_empty() {
        title = stem;
    }
    let (pw, ph) = r.first_page_size();
    let mut doc = Document::blank_pages(&title, &author, r.page_count(), pw, ph);
    doc.subject = subject;
    let t = Instant::now();
    doc.outline = r.outline();
    prof("outline (bookmarks)", t);
    // Read (or mint) the durable identity here, on the thread that opens
    // the file, rather than on the UI thread.
    let file = ui::RecordId::identify_file(path);
    Ok(LoadedDoc {
        doc,
        raster: r,
        name,
        file,
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
    /// This lets UI layout remain stable while `blobstore::get()` asks peers.
    pending_sizes: Vec<Option<(f64, f64)>>,
    pending_hashes: Vec<Option<String>>,
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
    /// Content hash and length of the opened bytes, which are kept in the
    /// content-addressed blob store.
    source_hash: String,
    source_len: u64,
}

/// A cached bitmap's identity: source, source page, width bucket, rotation.
type RasterKey = (usize, usize, u32, i32);

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
        let t = Instant::now();
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

    fn open(bytes: &[u8]) -> anyhow::Result<Self> {
        let engine = engine()?;
        // The document is an immutable blob: keep the bytes in the
        // content-addressed store (deduplicated by hash) so the markup record
        // can pin exactly what it annotates. A store that refuses (size
        // limits) still leaves the hash as the identity.
        let source_hash =
            qdoc::blobstore::put(bytes).unwrap_or_else(|| qdoc::blobstore::hash_bytes(bytes));
        let byte_len = bytes.len() as u64;
        let source = match engine.open_bytes(bytes) {
            Ok(doc) => Self::source_from(doc)
                .ok_or_else(|| anyhow!("the PDF has no pages or unsupported page sizes"))?,
            Err(tilo_pdf::Error::Password) => {
                return Err(anyhow!("the PDF is password protected"));
            }
            Err(e @ tilo_pdf::Error::Unavailable(_)) => return Err(anyhow!("{e}")),
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
            pending_hashes: vec![None; source_len],
            rotations: vec![0; source_len],
            cache: HashMap::new(),
            cache_sizes: HashMap::new(),
            cache_lru: VecDeque::new(),
            cache_bytes: 0,
            text_cache: HashMap::new(),
            source_hash,
            source_len: byte_len,
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

    /// Number of displayed pages (the length of the editable page plan).
    pub fn page_count(&self) -> usize {
        self.plan.len()
    }

    /// Hydrate a content-addressed imported source on demand. A miss is not an
    /// error: qdoc asks the room and the next sync/render retries lazily.
    fn imported_source(&mut self, imported: &ImportedPage) -> Option<usize> {
        if let Some(index) = self
            .sources
            .iter()
            .position(|source| source.content_hash.as_deref() == Some(&imported.hash))
        {
            return Some(index);
        }
        let bytes = qdoc::blobstore::get(&imported.hash)?;
        if !qdoc::blobstore::verify(&imported.hash, &bytes) {
            return None;
        }
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
        self.pending_hashes = entries
            .iter()
            .map(|entry| {
                entry
                    .imported
                    .as_ref()
                    .map(|imported| imported.hash.clone())
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

    /// `None` for a normal/original page; otherwise the truthful transfer
    /// state for an imported occurrence whose bytes have not materialised yet.
    pub fn page_blob_state(&self, display_idx: usize) -> Option<qdoc::blobstore::BlobState> {
        self.slot(display_idx).is_none().then(|| {
            self.pending_hashes
                .get(display_idx)
                .and_then(Option::as_deref)
                .map(qdoc::blobstore::state)
        })?
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

    // ---- page editing -----------------------------------------------

    /// Move displayed page `from` to final index `to` (matching a
    /// [`ui::widget::page_sidebar::move_item`]-style move). Content, text and
    /// bitmap caches are keyed by source page, so no re-render is needed.
    pub fn reorder_page(&mut self, from: usize, to: usize) {
        if from == to || from >= self.plan.len() || to >= self.plan.len() {
            return;
        }
        let slot = self.plan.remove(from);
        let size = self.pending_sizes.remove(from);
        let hash = self.pending_hashes.remove(from);
        let rotation = if from < self.rotations.len() {
            self.rotations.remove(from)
        } else {
            0
        };
        self.plan.insert(to, slot);
        self.pending_sizes.insert(to, size);
        self.pending_hashes.insert(to, hash);
        self.rotations
            .insert(to.min(self.rotations.len()), rotation);
    }

    /// Remove displayed page `idx`. Keeps at least one page.
    pub fn delete_page(&mut self, idx: usize) {
        if self.plan.len() > 1 && idx < self.plan.len() {
            self.plan.remove(idx);
            self.pending_sizes.remove(idx);
            self.pending_hashes.remove(idx);
            if idx < self.rotations.len() {
                self.rotations.remove(idx);
            }
        }
    }

    /// Duplicate displayed page `idx`, inserting the copy right after it. The
    /// duplicate points at the same source page (cheap; shares the render cache).
    pub fn duplicate_page(&mut self, idx: usize) {
        if let Some(slot) = self.slot(idx) {
            self.plan.insert(idx + 1, Some(slot));
            self.pending_sizes
                .insert(idx + 1, self.pending_sizes.get(idx).copied().flatten());
            self.pending_hashes
                .insert(idx + 1, self.pending_hashes.get(idx).cloned().flatten());
            let rotation = self.page_rotation(idx);
            self.rotations
                .insert((idx + 1).min(self.rotations.len()), rotation);
        }
    }

    /// Import every page of the PDF in `bytes`, inserting them starting at
    /// displayed index `at`. Returns the number of pages added.
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` is not a PDF the engine can parse (corrupt,
    /// encrypted without a supplied password, or zero pages).
    pub fn import_pdf(&mut self, bytes: &[u8], at: usize) -> anyhow::Result<usize> {
        let source = Self::load_source(&self.engine, bytes)
            .ok_or_else(|| anyhow!("the file is not a readable PDF"))?;
        let n = source.page_sizes.len();
        let src_idx = self.sources.len();
        self.sources.push(source);
        let at = at.min(self.plan.len());
        for page in 0..n {
            self.plan.insert(
                at + page,
                Some(PageSlot {
                    source: src_idx,
                    page,
                }),
            );
            self.pending_sizes.insert(at + page, None);
            self.pending_hashes.insert(at + page, None);
            self.rotations
                .insert((at + page).min(self.rotations.len()), 0);
        }
        Ok(n)
    }

    /// Import an image (`bytes` = PNG/JPEG/… file contents) as a new page,
    /// inserted at displayed index `at`. The image is placed to fill a page
    /// sized to its own aspect ratio.
    ///
    /// # Errors
    ///
    /// Returns an error if the engine cannot decode `bytes` as an image.
    pub fn import_image(&mut self, bytes: &[u8], at: usize) -> anyhow::Result<()> {
        let source = Self::image_source(&self.engine, bytes)?;
        let src_idx = self.sources.len();
        self.sources.push(source);
        let at = at.min(self.plan.len());
        self.plan.insert(
            at,
            Some(PageSlot {
                source: src_idx,
                page: 0,
            }),
        );
        self.pending_sizes.insert(at, None);
        self.pending_hashes.insert(at, None);
        self.rotations.insert(at.min(self.rotations.len()), 0);
        Ok(())
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

    /// The width bucket `render` snaps `target_w` to. Exposed so the draw loop
    /// can probe the cache (`is_page_cached`) with the exact key `render` uses.
    pub(crate) fn bucket_for(target_w: u32) -> u32 {
        ((target_w / 24).max(1) * 24).clamp(96, 4096)
    }

    /// Whether displayed page `display_idx` already has a cached bitmap at the
    /// bucket `render(display_idx, target_w)` would use — i.e. drawing it this
    /// frame is a free cache hit rather than a fresh (expensive) rasterisation.
    pub fn is_page_cached(&self, display_idx: usize, target_w: u32) -> bool {
        match self.slot(display_idx) {
            Some(slot) => {
                let Some(size) = self
                    .sources
                    .get(slot.source)
                    .and_then(|source| source.page_sizes.get(slot.page))
                else {
                    return false;
                };
                let rotation = self.page_rotation(display_idx);
                let Some(bucket) = Self::bucket_for_page(target_w, rotated_size(*size, rotation))
                else {
                    return false;
                };
                self.cache
                    .contains_key(&(slot.source, slot.page, bucket, rotation))
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
        self.cache
            .keys()
            .filter(|(_, _, b, _)| *b > THUMB_MAX)
            .count()
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
        let t = Instant::now();
        let source = self.sources.get(slot.source)?;
        let extra = tilo_pdf::Rotation::from_degrees(rotation).unwrap_or_default();
        let options = tilo_pdf::RenderOptions {
            rotation: extra,
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
        let img = PenikoImage {
            data: Blob::new(bitmap.shared_pixels()),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: w,
            height: h,
        };
        self.cache_image(key, img.clone());
        Some(img)
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

    /// Drop oversized stale zoom levels. Smaller safe buckets (including a
    /// tall page capped by [`MAX_RASTER_PIXELS`]) and thumbnails may stay until
    /// the LRU byte budget evicts them, so this never churns a freshly rendered
    /// page. Called once per frame.
    pub fn prune(&mut self, keep_bucket: u32) {
        const THUMB_MAX: u32 = 600;
        // Cache keys store the *snapped* bucket (`bucket_for`), so snap the
        // keep-width the same way. Comparing against the raw width would match
        // nothing — silently evicting the current page every frame and forcing
        // a full re-rasterise on the very next one.
        let keep = Self::bucket_for(keep_bucket);
        let stale: Vec<_> = self
            .cache
            .keys()
            .copied()
            .filter(|(_, _, bucket, _)| *bucket > keep && *bucket > THUMB_MAX)
            .collect();
        for key in stale {
            self.evict_cache(key);
        }
    }

    /// The annotations the engine reads back from displayed page
    /// `display_idx` of the backing document. Used by the save round-trip
    /// tests to check that exported markup lands as real PDF annotations.
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
        std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
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
    fn edits_for(&self, markup: &ExportMarkup) -> anyhow::Result<tilo_pdf::Edits> {
        let mut edits = tilo_pdf::Edits::default();
        for (dest, slot) in self.plan.iter().enumerate() {
            let slot = slot
                .ok_or_else(|| anyhow!("page {} is still awaiting imported content", dest + 1))?;
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
            });
        }
        let identity = edits.pages.iter().enumerate().all(|(i, p)| {
            p.source == tilo_pdf::PageSource::Original(i) && p.rotation == tilo_pdf::Rotation::None
        }) && self.plan.len()
            == self.sources.first().map_or(0, |s| s.page_sizes.len());
        if identity {
            edits.pages.clear();
        }
        let point = |(x, y): (f64, f64)| tilo_pdf::Point::new(x as f32, y as f32);
        let rect = |r: &PtRect| {
            tilo_pdf::Rect::from_corners(point((r.x, r.y)), point((r.x + r.w, r.y + r.h)))
        };
        for idx in 0..self.plan.len() {
            let page_no = idx + 1;
            for a in markup.annotations.iter().filter(|a| a.page == page_no) {
                let (r, g, b) = a.color.rgb();
                let thread = markup
                    .comments
                    .iter()
                    .find(|c| c.anno_id.as_deref() == Some(a.id.as_str()));
                let kind = match &a.kind {
                    AnnoKind::Markup { style, quads } => {
                        let quads = quads.iter().map(rect).collect();
                        match style {
                            MarkupStyle::Highlight => {
                                tilo_pdf::NewAnnotationKind::Highlight { quads }
                            }
                            MarkupStyle::Underline => {
                                tilo_pdf::NewAnnotationKind::Underline { quads }
                            }
                            MarkupStyle::StrikeOut => {
                                tilo_pdf::NewAnnotationKind::StrikeOut { quads }
                            }
                        }
                    }
                    AnnoKind::Ink { points } if points.len() >= 2 => {
                        tilo_pdf::NewAnnotationKind::Ink {
                            strokes: vec![points.iter().copied().map(point).collect()],
                            width: 2.0,
                        }
                    }
                    AnnoKind::Ink { .. } => continue,
                    AnnoKind::Note { x, y } => tilo_pdf::NewAnnotationKind::Note {
                        at: point((*x, *y)),
                    },
                };
                edits.annotations.push(tilo_pdf::NewAnnotation {
                    page: idx,
                    kind,
                    color: tilo_pdf::Rgba::new(r, g, b, 255),
                    author: a.author.clone(),
                    contents: thread.map(thread_text).unwrap_or_default(),
                    name: a.id.clone(),
                    modified: pdf_date(a.created_ms),
                });
            }
            for s in markup.signatures.iter().filter(|s| s.page == page_no) {
                edits.annotations.push(signature_stamp(idx, s));
            }
        }
        edits.form_values = markup
            .form_values
            .iter()
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
/// space, the comment threads notes carry, and recorded form values.
#[derive(Clone, Debug, Default)]
pub struct ExportMarkup {
    pub annotations: Vec<Annotation>,
    pub comments: Vec<Comment>,
    pub signatures: Vec<Signature>,
    pub form_values: Vec<(String, FormEntry)>,
}

/// A PDF date (`D:YYYYMMDDHHmmSSZ`) for unix milliseconds; empty for 0.
fn pdf_date(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm), proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// A comment thread as annotation text: the body, then each reply.
fn thread_text(c: &Comment) -> String {
    let mut text = c.body.trim().to_string();
    for r in &c.replies {
        text.push('\n');
        text.push_str(&r.author);
        text.push_str(": ");
        text.push_str(r.body.trim());
    }
    text
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
    }
}

/// Compatibility name for the synchronous, owner-thread-only backend.
///
/// New UI code must use [`RasterHandle`]. This alias is retained temporarily
/// while the reader application is migrated in a separate change; it must not
/// cross a thread boundary.
pub type Rasterizer = RasterBackend;

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

/// A request for one page's selectable text layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRequest {
    pub document_id: u64,
    pub page: usize,
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
    Render(RenderRequest),
    Text(TextRequest),
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
    Opened {
        request: OpenRequest,
        document: Document,
        name: String,
        raster: RasterDocument,
        /// The file's durable record identity.
        file: ui::RecordId,
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
    RenderFailed {
        request: RenderRequest,
        error: String,
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
    /// Only the most recent queued open matters. The owner may already be
    /// reading an older one, but it cannot mutate the active document unless
    /// it succeeds; the request id lets the UI disregard that stale result.
    open: HashMap<u64, OpenRequest>,
    open_order: VecDeque<u64>,
    controls: VecDeque<QueuedCommand>,
    renders: VecDeque<QueuedCommand>,
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
    closing_sessions: HashSet<u64>,
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
    /// Connect a new reader session to the dedicated PDF owner thread.
    ///
    /// # Errors
    ///
    /// Returns [`RasterSubmitError::Exhausted`] rather than reusing an id if
    /// the process has exhausted its non-reusable session-id space.
    pub fn new() -> Result<Self, RasterSubmitError> {
        Ok(Self {
            session: Arc::new(SessionGuard {
                queue: shared_worker_queue(),
                id: next_session_id()?,
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

    /// Queue a page render, coalescing the same document/page/zoom bucket.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker has stopped or its bounded render
    /// queue is occupied by distinct work.
    pub fn request_render(&self, request: RenderRequest) -> Result<(), RasterSubmitError> {
        self.enqueue(RasterCommand::Render(request))
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
        let mut state = self
            .session
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut events = drain_session_events(&mut state.open_events, self.session.id);
        events.extend(drain_session_events(&mut state.events, self.session.id));
        state.event_bytes = state.events.iter().fold(0usize, |bytes, queued| {
            bytes.saturating_add(page_event_bytes(&queued.event))
        });
        drop(state);
        self.session.queue.ready.notify_all();
        events
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
        let mut state = self
            .session
            .queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                state
                    .texts
                    .retain(|queued| queued.session_id != self.session.id);
                state.reconciles.remove(&self.session.id);
                state
                    .reconcile_order
                    .retain(|queued| *queued != self.session.id);
            }
            command @ (RasterCommand::Activate { .. } | RasterCommand::Discard { .. }) => {
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
        let worker_queue = Arc::clone(&queue);
        if let Err(error) = thread::Builder::new()
            .name("pdf-source".to_string())
            .spawn(move || source_worker(&worker_queue))
        {
            let mut state = queue
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.start_error = Some(error.to_string());
        }
        queue
    }))
}

fn close_session(queue: &WorkerQueue, session_id: u64) {
    let mut state = queue
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    close_session_locked(&mut state, session_id);
    drop(state);
    queue.ready.notify_one();
}

fn close_session_locked(state: &mut QueueState, session_id: u64) {
    if state.start_error.is_some() {
        return;
    }
    state.closing_sessions.insert(session_id);
    state.open.remove(&session_id);
    state.open_order.retain(|queued| *queued != session_id);
    state
        .controls
        .retain(|queued| queued.session_id != session_id);
    state
        .renders
        .retain(|queued| queued.session_id != session_id);
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

#[allow(
    clippy::too_many_lines,
    reason = "the worker loop keeps its serial ownership protocol auditable in one place"
)]
fn source_worker(queue: &Arc<WorkerQueue>) {
    // A session may briefly own both the visible predecessor and a parsed
    // replacement.  We retain the former until the UI explicitly activates
    // the latter, so a cancellation or stale result cannot turn the readable
    // document into an unusable document-id mismatch.
    let mut active: HashMap<u64, HashMap<u64, RasterBackend>> = HashMap::new();
    // PDFium writes only here until the UI explicitly commits the operation.
    // Keeping these paths worker-local prevents an abandoned action from ever
    // racing a later save selection on the UI thread.
    let mut prepared_saves: HashMap<(u64, u64), (SaveRequest, PrivateSave)> = HashMap::new();
    loop {
        let command = {
            let mut state = queue
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(session_id) = state.shutdowns.pop_front() {
                    break QueuedCommand {
                        session_id,
                        command: RasterCommand::Shutdown,
                    };
                }
                if let Some(control) = state.controls.pop_front() {
                    break control;
                }
                if let Some(session_id) = state.open_order.pop_front()
                    && let Some(request) = state.open.remove(&session_id)
                {
                    break QueuedCommand {
                        session_id,
                        command: RasterCommand::OpenPath(request),
                    };
                }
                if let Some(session_id) = state.reconcile_order.pop_front()
                    && let Some(request) = state.reconciles.remove(&session_id)
                {
                    break QueuedCommand {
                        session_id,
                        command: RasterCommand::Reconcile(request),
                    };
                }
                if let Some(session_id) = state.save_order.pop_front()
                    && let Some(request) = state.saves.remove(&session_id)
                {
                    break QueuedCommand {
                        session_id,
                        command: RasterCommand::Save(request),
                    };
                }
                if let Some(session_id) = state.import_order.pop_front()
                    && let Some(request) = state.imports.remove(&session_id)
                {
                    break QueuedCommand {
                        session_id,
                        command: RasterCommand::Import(request),
                    };
                }
                // Alternate between rendering and text work so a long visible
                // render burst cannot starve selection/search indefinitely.
                let command = if state.prefer_text {
                    state
                        .texts
                        .pop_front()
                        .or_else(|| state.renders.pop_front())
                } else {
                    state
                        .renders
                        .pop_front()
                        .or_else(|| state.texts.pop_front())
                };
                if let Some(command) = command {
                    state.prefer_text = !state.prefer_text;
                    break command;
                }
                state = queue
                    .ready
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };

        let session_id = command.session_id;
        let event = match command.command {
            RasterCommand::OpenPath(request) => match open(&request.path) {
                Ok(LoadedDoc {
                    doc,
                    raster,
                    name,
                    file,
                }) => {
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
                        file,
                    }
                }
                Err(error) => RasterEvent::OpenFailed {
                    request,
                    error: error.to_string(),
                },
            },
            RasterCommand::Activate { document_id } => {
                if let Some(documents) = active.get_mut(&session_id) {
                    documents.retain(|id, _| *id == document_id);
                }
                continue;
            }
            RasterCommand::Discard { document_id } => {
                if let Some(documents) = active.get_mut(&session_id) {
                    documents.remove(&document_id);
                }
                continue;
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
                Some(backend) => match private_save_path(&request.path, request.id) {
                    Ok(private) => {
                        let temporary = private.temporary.clone();
                        match backend.save_annotated(&request.markup, &temporary) {
                            Ok(()) => {
                                prepared_saves
                                    .insert((session_id, request.id), (request.clone(), private));
                                RasterEvent::SavePrepared { request, temporary }
                            }
                            Err(error) => RasterEvent::SaveFailed {
                                request,
                                error: error.to_string(),
                            },
                        }
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
                continue;
            }
            RasterCommand::Shutdown => {
                active.remove(&session_id);
                prepared_saves.retain(|(owner, _), (_, temporary)| {
                    if *owner == session_id {
                        let _ = std::fs::remove_file(&temporary.temporary);
                        false
                    } else {
                        true
                    }
                });
                let mut state = queue
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.closing_sessions.remove(&session_id);
                drop(state);
                continue;
            }
        };
        push_event(queue, session_id, event);
    }
}

fn push_event(queue: &WorkerQueue, session_id: u64, event: RasterEvent) {
    let mut state = queue
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(
        &event,
        RasterEvent::Opened { .. }
            | RasterEvent::OpenFailed { .. }
            | RasterEvent::OpenSuperseded { .. }
            | RasterEvent::Reconciled { .. }
            | RasterEvent::ReconcileFailed { .. }
            | RasterEvent::Imported { .. }
            | RasterEvent::ImportFailed { .. }
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
        _ => false,
    }
}

fn page_event_bytes(event: &RasterEvent) -> usize {
    match event {
        RasterEvent::Rendered {
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

    fn test_handle(queue: Arc<WorkerQueue>, id: u64) -> RasterHandle {
        RasterHandle {
            session: Arc::new(SessionGuard {
                queue,
                id,
                closed: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    #[test]
    fn bounded_reader_rejects_growth_past_its_cap() {
        let file = tempfile::NamedTempFile::new().expect("temporary source");
        std::fs::write(file.path(), b"12345").expect("write source");
        let error = read_file_bounded(file.path(), 4).expect_err("source exceeds cap");
        assert!(error.to_string().contains("opens files up to"));
    }

    #[test]
    fn page_size_validation_rejects_non_finite_and_extreme_boxes() {
        assert!(valid_page_size(612.0, 792.0));
        assert!(!valid_page_size(f64::NAN, 792.0));
        assert!(!valid_page_size(0.0, 792.0));
        assert!(!valid_page_size(MAX_PAGE_EDGE_POINTS + 1.0, 792.0));
    }

    #[test]
    fn raster_bucket_respects_page_pixel_budget() {
        let bucket = Rasterizer::bucket_for_page(4_096, (612.0, 20_000.0))
            .expect("a tall but valid page still gets a bounded raster");
        let pixels = f64::from(bucket) * f64::from(bucket) * 20_000.0 / 612.0;
        assert!(pixels <= MAX_RASTER_PIXELS * 1.01);
    }

    #[test]
    fn owner_queue_coalesces_latest_open_and_page_requests() {
        // Build the endpoint without starting its worker so this test can
        // inspect exactly what a busy owner will receive.
        let handle = test_handle(
            Arc::new(WorkerQueue {
                state: Mutex::new(QueueState::default()),
                ready: Condvar::new(),
            }),
            1,
        );
        let first = OpenRequest {
            id: 1,
            path: "first.pdf".into(),
        };
        let latest = OpenRequest {
            id: 2,
            path: "latest.pdf".into(),
        };
        handle
            .enqueue(RasterCommand::OpenPath(first.clone()))
            .expect("queue first open");
        handle
            .request_render(RenderRequest {
                document_id: first.id,
                page: 3,
                target_width: 101,
            })
            .expect("queue first render");
        handle
            .request_render(RenderRequest {
                document_id: first.id,
                page: 3,
                target_width: 110,
            })
            .expect("coalesce nearby zoom bucket");
        handle
            .request_text(TextRequest {
                document_id: first.id,
                page: 3,
            })
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
    }

    #[test]
    fn open_ids_fail_closed_at_the_counter_boundary() {
        let counter = std::sync::atomic::AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_open_request_id(&counter), Ok(u64::MAX - 1));
        assert_eq!(
            next_open_request_id(&counter),
            Err(RasterSubmitError::Exhausted)
        );
    }

    #[test]
    fn unavailable_worker_surfaces_its_startup_error() {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState {
                closing_sessions: HashSet::from([1]),
                start_error: Some("thread creation denied".to_string()),
                ..QueueState::default()
            }),
            ready: Condvar::new(),
        });
        let handle = test_handle(queue, 1);
        assert_eq!(
            handle.request_text(TextRequest {
                document_id: 1,
                page: 0,
            }),
            Err(RasterSubmitError::Unavailable(
                "thread creation denied".to_string()
            ))
        );
    }

    #[test]
    fn closing_one_session_does_not_block_another() {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        let first = test_handle(Arc::clone(&queue), 1);
        let second = test_handle(Arc::clone(&queue), 2);
        first
            .enqueue(RasterCommand::OpenPath(OpenRequest {
                id: 1,
                path: "first.pdf".into(),
            }))
            .expect("first session queues open");
        second
            .enqueue(RasterCommand::OpenPath(OpenRequest {
                id: 2,
                path: "second.pdf".into(),
            }))
            .expect("second session queues open");
        first.shutdown();
        second
            .request_text(TextRequest {
                document_id: 2,
                page: 0,
            })
            .expect("second session remains usable");
        let state = queue.state.lock().expect("queue lock");
        assert!(!state.open.contains_key(&1));
        assert!(state.open.contains_key(&2));
        assert!(state.texts.iter().any(|queued| queued.session_id == 2));
        drop(state);
    }

    #[test]
    fn dropped_session_purges_undrained_open_settlements() {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        {
            let first = test_handle(Arc::clone(&queue), 1);
            for id in 1..=8 {
                first
                    .enqueue(RasterCommand::OpenPath(OpenRequest {
                        id,
                        path: format!("{id}.pdf").into(),
                    }))
                    .expect("queue transient open");
            }
            assert!(!first.drain_events().is_empty());
            // Queue one more settlement and intentionally do not drain it.
            first
                .enqueue(RasterCommand::OpenPath(OpenRequest {
                    id: 9,
                    path: "undrained.pdf".into(),
                }))
                .expect("queue undrained open");
        }
        let second = test_handle(Arc::clone(&queue), 2);
        second
            .enqueue(RasterCommand::OpenPath(OpenRequest {
                id: 10,
                path: "next.pdf".into(),
            }))
            .expect("dropped session cannot consume another session's capacity");
        let state = queue.state.lock().expect("queue lock");
        assert!(
            state
                .open_events
                .iter()
                .all(|queued| queued.session_id != 1)
        );
        drop(state);
    }

    #[test]
    fn open_settlements_are_bounded_per_session_without_losing_the_current_completion() {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        let handle = test_handle(Arc::clone(&queue), 1);
        for id in 1..=63 {
            handle
                .enqueue(RasterCommand::OpenPath(OpenRequest {
                    id,
                    path: format!("{id}.pdf").into(),
                }))
                .expect("settlement capacity reserves the current open");
        }
        assert!(matches!(
            handle.enqueue(RasterCommand::OpenPath(OpenRequest {
                id: 64,
                path: "overflow.pdf".into(),
            })),
            Err(RasterSubmitError::QueueFull)
        ));
        let current = {
            let mut state = queue.state.lock().expect("queue lock");
            assert_eq!(
                session_settlement_count(&state, 1),
                MAX_PENDING_OPEN_SETTLEMENTS.saturating_sub(2)
            );
            let current = state.open.get(&1).cloned().expect("current open retained");
            let _ = state.open_events.pop_front();
            current
        };
        push_event(
            &queue,
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
    }

    #[test]
    fn a_full_hidden_session_cannot_block_another_sessions_settlement() {
        let queue = Arc::new(WorkerQueue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        {
            let mut state = queue.state.lock().expect("queue lock");
            state
                .open_events
                .extend((0..MAX_PENDING_OPEN_SETTLEMENTS).map(|id| QueuedEvent {
                    session_id: 1,
                    event: RasterEvent::OpenFailed {
                        request: OpenRequest {
                            id: id as u64,
                            path: "hidden.pdf".into(),
                        },
                        error: "fixture".into(),
                    },
                }));
        }
        let visible = test_handle(Arc::clone(&queue), 2);
        visible
            .enqueue(RasterCommand::OpenPath(OpenRequest {
                id: 900,
                path: "visible.pdf".into(),
            }))
            .expect("another session remains admissible");
        push_event(
            &queue,
            2,
            RasterEvent::OpenFailed {
                request: OpenRequest {
                    id: 900,
                    path: "visible.pdf".into(),
                },
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

        let private = private_save_path(&destination, 7).expect("private directory");
        assert_ne!(private.temporary, predictable);
        assert!(private.temporary.starts_with(private.directory.path()));
        std::fs::write(&private.temporary, b"safe export").expect("private output");
        flush_private_save(&private, &destination).expect("atomic publish");

        assert_eq!(std::fs::read(&victim).expect("victim contents"), b"keep");
        assert_eq!(
            std::fs::read(&destination).expect("destination contents"),
            b"safe export"
        );
    }

    #[test]
    fn result_payloads_are_byte_bounded() {
        let request = RenderRequest {
            document_id: 1,
            page: 0,
            target_width: 4_096,
        };
        let oversized_image = PenikoImage {
            data: Blob::new(Arc::new(vec![0; MAX_PENDING_RESULT_BYTES + 1])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 4_097,
            height: 2_048,
        };
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
                request: TextRequest {
                    document_id: 1,
                    page: 0,
                },
                text: oversized_text,
            }),
            RasterEvent::TextFailed { .. }
        ));
    }

    #[test]
    fn owner_queue_coalesces_nearby_page_requests() {
        let handle = test_handle(
            Arc::new(WorkerQueue {
                state: Mutex::new(QueueState::default()),
                ready: Condvar::new(),
            }),
            1,
        );
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
        handle
            .request_text(TextRequest {
                document_id: request.document_id,
                page: 3,
            })
            .expect("queue text");
        handle
            .request_text(TextRequest {
                document_id: request.document_id,
                page: 3,
            })
            .expect("coalesce text");

        let state = handle.session.queue.state.lock().expect("queue lock");
        assert_eq!(state.renders.len(), 1);
        assert!(matches!(
            &state.renders[0].command,
            RasterCommand::Render(RenderRequest { target_width, .. })
                if *target_width == RasterBackend::bucket_for(101)
        ));
        assert_eq!(state.texts.len(), 1);
        drop(state);
    }

    #[test]
    fn owner_queue_keeps_only_the_latest_collaborative_page_plan() {
        let handle = test_handle(
            Arc::new(WorkerQueue {
                state: Mutex::new(QueueState::default()),
                ready: Condvar::new(),
            }),
            1,
        );
        let first = ReconcileRequest {
            document_id: 7,
            plan: vec![PagePlanEntry {
                id: "first".into(),
                source_page: 1,
                imported: None,
                rotation: 0,
            }],
        };
        let latest = ReconcileRequest {
            document_id: 7,
            plan: vec![PagePlanEntry {
                id: "latest".into(),
                source_page: 1,
                imported: None,
                rotation: 0,
            }],
        };
        handle.reconcile_page_plan(first).expect("queue first plan");
        handle
            .reconcile_page_plan(latest.clone())
            .expect("replace queued plan");
        let state = handle.session.queue.state.lock().expect("queue lock");
        assert_eq!(state.reconcile_order.len(), 1);
        assert_eq!(state.reconciles.get(&1), Some(&latest));
        drop(state);
    }
}
