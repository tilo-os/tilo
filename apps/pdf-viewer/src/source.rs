//! Loading real PDF files — and writing markup back into them.
//!
//! Every page is rasterised on demand by a `pdfium` native library to an RGBA
//! bitmap at the current zoom and painted directly, so the viewer shows the
//! document exactly as authored (fonts, figures, vector art and all). Bitmaps
//! are cached per page + zoom bucket so scrolling and panning never
//! re-rasterise. The backend also extracts a **text layer**
//! ([`crate::text_layer`]) per page from pdfium, so search finds text hits and
//! text can be selected, and it can **flatten** the reader's annotations +
//! signatures back onto the pages and save a valid `.pdf` reopenable in any
//! viewer.
//!
//! The library is discovered via `TILO_PDFIUM_LIB`, a few well-known system
//! paths, or the platform loader — never bundled into the binary. On a host
//! without it, opening a PDF fails with an error naming the missing library.

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
use pdfium_render::prelude::*;
use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat};

use crate::doc::{ImportedPage, PagePlanEntry};
use crate::model::{
    AnnoKind, Annotation, Comment, Document, OutlineItem, PtRect, Signature, SignatureInk,
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
/// Why a PDF cannot open on a host without the native engine, in the words
/// the operator needs.
const NO_PDFIUM: &str = "no usable pdfium library was found; install one \
     (e.g. ~/.local/lib/pdfium/libpdfium.so) or set TILO_PDFIUM_LIB";

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

/// A document loaded from disk plus the pdfium backend that renders it.
pub struct LoadedDoc {
    pub doc: Document,
    pub raster: Rasterizer,
    /// The file's display name (for the window chrome / status bar).
    pub name: String,
}

/// Load a PDF from `path` through pdfium.
///
/// # Errors
///
/// Returns an error if `path` cannot be read, if no pdfium library can be
/// located on this host, or if pdfium cannot parse the bytes as a PDF within
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

    // Hand the freshly-read bytes straight to pdfium (it takes ownership), so
    // a large file is never copied on the open→first-paint path.
    let t = Instant::now();
    let r = Rasterizer::open(bytes)?;
    prof("Rasterizer::open (pdfium parse + page sizes)", t);
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
    Ok(LoadedDoc {
        doc,
        raster: r,
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
    content_hash: Option<String>,
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

/// Pdfium-owned implementation. This type deliberately stays on the source
/// worker: pdfium documents contain raw handles and are not `Send`.
pub struct RasterBackend {
    pdfium: &'static Pdfium,
    /// Loaded pdfium documents; index 0 is the original file.
    sources: Vec<Source>,
    /// The ordered, editable list of displayed pages.
    plan: Vec<Option<PageSlot>>,
    /// Dimensions supplied by the durable plan for a not-yet-downloaded page.
    /// This lets UI layout remain stable while `blobstore::get()` asks peers.
    pending_sizes: Vec<Option<(f64, f64)>>,
    pending_hashes: Vec<Option<String>>,
    /// Rendered bitmaps keyed by (source, source-page, target-width bucket).
    cache: HashMap<(usize, usize, u32), PenikoImage>,
    /// Byte size and recency bookkeeping for the bounded raster cache.
    cache_sizes: HashMap<(usize, usize, u32), usize>,
    cache_lru: VecDeque<(usize, usize, u32)>,
    cache_bytes: usize,
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
    // pdfium available" so opening reports the missing library.
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

impl RasterBackend {
    const MAX_IMPORT_PAGES: usize = 512;

    /// Validate an import before it is offered to the content store or turned
    /// into an action. This is deliberately separate from materialisation: it
    /// never mutates the live page plan.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes are neither a readable PDF nor a
    /// decodable image, or when a PDF exceeds the bounded page limit.
    pub fn inspect_import(&self, bytes: &[u8]) -> anyhow::Result<(String, Vec<(f64, f64)>)> {
        if bytes.starts_with(b"%PDF-") {
            // Count before allocating a per-page descriptor/action vector.
            // Import must not turn an adversarial thousand-page PDF into a
            // large action just to discover it exceeds the product limit.
            let doc = self
                .pdfium
                .load_pdf_from_byte_vec(bytes.to_vec(), None)
                .map_err(|_| anyhow!("the file is not a readable PDF"))?;
            let pages = doc.pages();
            if usize::from(pages.len()) > Self::MAX_IMPORT_PAGES {
                return Err(anyhow!(
                    "the PDF exceeds the {}-page import limit",
                    Self::MAX_IMPORT_PAGES
                ));
            }
            let mut sizes = Vec::with_capacity(usize::from(pages.len()));
            for index in 0..pages.len() {
                let rect = pages
                    .page_size(index)
                    .map_err(|_| anyhow!("reading PDF page size"))?;
                let size = (
                    f64::from(rect.width().value),
                    f64::from(rect.height().value),
                );
                if !valid_page_size(size.0, size.1) {
                    return Err(anyhow!("the PDF has an unsupported page size"));
                }
                sizes.push(size);
            }
            return (!sizes.is_empty())
                .then_some(("pdf".to_string(), sizes))
                .ok_or_else(|| anyhow!("the PDF has no pages"));
        }
        let image =
            image::load_from_memory(bytes).map_err(|e| anyhow!("decoding the image: {e}"))?;
        let width = f64::from(image.width().max(1));
        let height = f64::from(image.height().max(1));
        Ok((
            "image".to_string(),
            vec![(612.0, (612.0 * height / width).clamp(72.0, 3000.0))],
        ))
    }
    fn load_source(pdfium: &'static Pdfium, bytes: Vec<u8>) -> Option<Source> {
        let t = Instant::now();
        let doc = pdfium.load_pdf_from_byte_vec(bytes, None).ok()?;
        prof("  load_pdf_from_byte_vec", t);
        let t = Instant::now();
        let page_count = usize::from(doc.pages().len());
        if page_count == 0 || page_count > MAX_SOURCE_PAGES {
            return None;
        }
        let mut page_sizes = Vec::with_capacity(page_count);
        let pages = doc.pages();
        for i in 0..pages.len() {
            let rect = pages.page_size(i).ok()?;
            let size = (
                f64::from(rect.width().value),
                f64::from(rect.height().value),
            );
            if !valid_page_size(size.0, size.1) {
                return None;
            }
            page_sizes.push(size);
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
        Some(Source {
            doc,
            page_sizes,
            content_hash: None,
        })
    }

    fn open(bytes: Vec<u8>) -> anyhow::Result<Self> {
        let pdfium = shared_pdfium().ok_or_else(|| anyhow!(NO_PDFIUM))?;
        let source = Self::load_source(pdfium, bytes)
            .ok_or_else(|| anyhow!("pdfium could not open this file as a PDF"))?;
        let source_len = source.page_sizes.len();
        let plan = (0..source.page_sizes.len())
            .map(|page| Some(PageSlot { source: 0, page }))
            .collect();
        Ok(RasterBackend {
            pdfium,
            sources: vec![source],
            plan,
            pending_sizes: vec![None; source_len],
            pending_hashes: vec![None; source_len],
            cache: HashMap::new(),
            cache_sizes: HashMap::new(),
            cache_lru: VecDeque::new(),
            cache_bytes: 0,
            text_cache: HashMap::new(),
        })
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
            Self::load_source(self.pdfium, bytes)?
        } else {
            Self::image_source(self.pdfium, &bytes).ok()?
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

    /// Size of displayed page `display_idx`, in PDF points.
    pub fn page_size(&self, display_idx: usize) -> (f64, f64) {
        self.slot(display_idx)
            .and_then(|s| self.sources.get(s.source)?.page_sizes.get(s.page).copied())
            .or_else(|| self.pending_sizes.get(display_idx).copied().flatten())
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
        let size = self.pending_sizes.remove(from);
        let hash = self.pending_hashes.remove(from);
        self.plan.insert(to, slot);
        self.pending_sizes.insert(to, size);
        self.pending_hashes.insert(to, hash);
    }

    /// Remove displayed page `idx`. Keeps at least one page.
    pub fn delete_page(&mut self, idx: usize) {
        if self.plan.len() > 1 && idx < self.plan.len() {
            self.plan.remove(idx);
            self.pending_sizes.remove(idx);
            self.pending_hashes.remove(idx);
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
        }
    }

    /// Import every page of the PDF in `bytes`, inserting them starting at
    /// displayed index `at`. Returns the number of pages added.
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` is not a PDF pdfium can parse (corrupt,
    /// encrypted without a supplied password, or zero pages).
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
                Some(PageSlot {
                    source: src_idx,
                    page,
                }),
            );
            self.pending_sizes.insert(at + page, None);
            self.pending_hashes.insert(at + page, None);
        }
        Ok(n)
    }

    /// Import an image (`bytes` = PNG/JPEG/… file contents) as a new page,
    /// inserted at displayed index `at`. The image is placed to fill a page
    /// sized to its own aspect ratio.
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` cannot be decoded as an image, or if
    /// pdfium fails to create the backing single-page document or place the
    /// image object on it.
    pub fn import_image(&mut self, bytes: &[u8], at: usize) -> anyhow::Result<()> {
        let source = Self::image_source(self.pdfium, bytes)?;
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
        Ok(())
    }

    fn image_source(pdfium: &'static Pdfium, bytes: &[u8]) -> anyhow::Result<Source> {
        let img = image::load_from_memory(bytes).map_err(|e| anyhow!("decoding the image: {e}"))?;
        let (iw, ih) = (
            f64::from(img.width().max(1)),
            f64::from(img.height().max(1)),
        );
        // A page 612pt wide (US-Letter width) with the image's aspect ratio.
        let pw = 612.0f64;
        let ph = (pw * ih / iw).clamp(72.0, 3000.0);
        let mut doc = pdfium
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
        Ok(Source {
            doc,
            page_sizes: vec![(pw, ph)],
            content_hash: None,
        })
    }

    fn metadata(&self) -> (String, String, String) {
        let Some(src) = self.sources.first() else {
            return (String::new(), String::new(), String::new());
        };
        let meta = src.doc.metadata();
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
        let Some(src) = self.sources.first() else {
            return out;
        };
        for bm in src.doc.bookmarks().iter() {
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
            if glyphs.len() >= MAX_TEXT_LAYER_GLYPHS {
                // A prefix looks superficially valid but makes searching and
                // selecting the rest of a page silently wrong. Report this
                // page as unavailable instead of caching a dishonest layer.
                return None;
            }
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
                let Some(bucket) = Self::bucket_for_page(target_w, *size) else {
                    return false;
                };
                self.cache.contains_key(&(slot.source, slot.page, bucket))
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

    fn touch_cache(&mut self, key: (usize, usize, u32)) {
        self.cache_lru.retain(|candidate| *candidate != key);
        self.cache_lru.push_back(key);
    }

    fn evict_cache(&mut self, key: (usize, usize, u32)) {
        self.cache.remove(&key);
        self.cache_bytes = self
            .cache_bytes
            .saturating_sub(self.cache_sizes.remove(&key).unwrap_or(0));
        self.cache_lru.retain(|candidate| *candidate != key);
    }

    fn cache_image(&mut self, key: (usize, usize, u32), image: PenikoImage) {
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

    pub fn render(&mut self, display_idx: usize, target_w: u32) -> Option<PenikoImage> {
        let slot = self.slot(display_idx)?;
        let size = *self.sources.get(slot.source)?.page_sizes.get(slot.page)?;
        let bucket = Self::bucket_for_page(target_w, size)?;
        let key = (slot.source, slot.page, bucket);
        if let Some(img) = self.cache.get(&key).cloned() {
            self.touch_cache(key);
            return Some(img);
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
        self.cache_image(key, img.clone());
        Some(img)
    }

    /// Width bucket adjusted to keep the decoded raster within the per-page
    /// pixel budget. A malformed, needle-shaped media box therefore produces
    /// an honest unavailable-page state instead of asking pdfium to allocate a
    /// multi-gigabyte bitmap.
    fn bucket_for_page(target_w: u32, (width, height): (f64, f64)) -> Option<u32> {
        if !valid_page_size(width, height) {
            return None;
        }
        let ratio = height / width;
        let max_width = (MAX_RASTER_PIXELS / ratio).sqrt().floor() as u32;
        // pdfium does not produce useful document imagery below this width;
        // decline instead of silently returning a blurry or unsafe bitmap.
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
        // a full re-rasterise on the very next one (the bug that made a page
        // re-render on every idle/scroll frame instead of staying cached).
        let keep = Self::bucket_for(keep_bucket);
        let stale: Vec<_> = self
            .cache
            .keys()
            .copied()
            // A tall but valid page may have received a smaller safe bucket
            // than the requested width. Keep that raster too; otherwise it
            // would be evicted in the very frame it was produced. The LRU byte
            // budget still bounds all retained older/smaller zoom levels.
            .filter(|(_, _, bucket)| *bucket > keep && *bucket > THUMB_MAX)
            .collect();
        for key in stale {
            self.evict_cache(key);
        }
    }

    /// The number of content objects on displayed page `display_idx` in the
    /// backing pdfium document. Used by the save-back round-trip test to check
    /// that flattened markup lands as page content.
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
    ///
    /// # Errors
    ///
    /// Returns an error if pdfium fails to create the output document, copy a
    /// planned page from its source, or flatten an annotation/signature onto
    /// a page, or if writing the finished PDF to `out` fails (e.g. an
    /// unwritable path).
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
            let slot = slot
                .ok_or_else(|| anyhow!("page {} is still awaiting imported content", dest + 1))?;
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
                        // The guard above guarantees at least two points.
                        let Some((first, rest)) = points.split_first() else {
                            continue;
                        };
                        let stroke = PdfColor::new(r8, g8, b8, 255);
                        let (x0, y0) = pt_tl_to_pdf(first.0, first.1, page_h);
                        let mut path = PdfPagePathObject::new(
                            &doc,
                            x0,
                            y0,
                            Some(stroke),
                            Some(PdfPoints::new(2.0)),
                            None,
                        )
                        .map_err(|e| anyhow!("starting ink path: {e:?}"))?;
                        for p in rest {
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
                    // A stray one- or zero-point ink stroke has nothing to draw.
                    AnnoKind::Ink { .. } => {}
                }
            }

            for s in signatures.iter().filter(|s| s.page == page_no) {
                Self::flatten_signature(&doc, &mut page, page_h, s, font, font_italic)?;
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
                    // The check above guarantees at least two points.
                    let Some((first, rest)) = stroke.split_first() else {
                        continue;
                    };
                    let map = |nx: f64, ny: f64| {
                        let x = stroke_area.x + nx.clamp(0.0, 1.0) * stroke_area.w;
                        let y = stroke_area.y + ny.clamp(0.0, 1.0) * stroke_area.h;
                        pt_tl_to_pdf(x, y, page_h)
                    };
                    let (x0, y0) = map(first.0, first.1);
                    let mut path = PdfPagePathObject::new(
                        doc,
                        x0,
                        y0,
                        Some(ink_col),
                        Some(PdfPoints::new(1.6)),
                        None,
                    )
                    .map_err(|e| anyhow!("starting signature stroke: {e:?}"))?;
                    for p in rest {
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
    pub annotations: Vec<Annotation>,
    pub comments: Vec<Comment>,
    pub signatures: Vec<Signature>,
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
/// No pdfium handles are exposed here, so this value is `Send`.
#[derive(Clone, Debug)]
pub struct RasterDocument {
    pub id: u64,
    pub page_sizes: Vec<(f64, f64)>,
}

/// Results delivered back to the UI by [`RasterHandle::drain_events`].
///
/// Images and text layers are ordinary owned UI data. In particular, this enum
/// never contains `Pdfium`, `PdfDocument`, or a borrowed pdfium value.
#[derive(Clone, Debug)]
pub enum RasterEvent {
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

/// Thread-safe UI endpoint for the non-`Send` pdfium backend.
///
/// Construction creates exactly one long-lived owner thread. It serialises all
/// pdfium access, reads paths off the UI thread, keeps a bounded/coalescing
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
    // pdfium's process-wide library handle is !Sync. More than one owner
    // thread would therefore merely move the old UI-thread hazard into a race
    // between viewer windows. Every RasterHandle shares this one worker.
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
                Ok(LoadedDoc { doc, raster, name }) => {
                    let page_sizes = (0..raster.page_count())
                        .map(|page| raster.page_size(page))
                        .collect();
                    active
                        .entry(session_id)
                        .or_default()
                        .insert(request.id, raster);
                    let raster = RasterDocument {
                        id: request.id,
                        page_sizes,
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
                        RasterEvent::Reconciled {
                            request,
                            page_sizes,
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
                        match backend.save_annotated(
                            &request.annotations,
                            &request.comments,
                            &request.signatures,
                            &temporary,
                        ) {
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
    if let Ok(path) = std::env::var("TILO_PDFIUM_LIB")
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
            }],
        };
        let latest = ReconcileRequest {
            document_id: 7,
            plan: vec![PagePlanEntry {
                id: "latest".into(),
                source_page: 1,
                imported: None,
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
