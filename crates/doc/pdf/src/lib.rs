//! `tilo-pdf` — Tilo's one PDF engine API.
//!
//! Every PDF consumer (the PDF viewer, thumbnails, previews) goes through
//! this crate. The API is engine independent: documents, pages, bitmaps,
//! text layers, links, outlines, form fields, annotations and edits are
//! this crate's own types. The engine behind it today is pdfium, and no
//! pdfium type is visible outside the crate.
//!
//! ## Isolation
//!
//! PDFs are hostile input and pdfium is a large C++ parser, so documents are
//! parsed in a separate helper process (`tilo-pdf-helper`) confined by
//! Landlock (read-only access to the pdfium library and system fonts, no
//! network) and a seccomp policy (no sockets, no exec, write-xor-execute).
//! Documents reach the helper as read-only descriptors; pages are rendered
//! straight into pooled shared pixel buffers and exports come back as sealed
//! memory files, so no pixel or page is copied through the socket. If the helper crashes or hangs, the request fails
//! with [`Error::Crashed`] or [`Error::Timeout`] and the next request starts
//! a fresh helper, re-opening the documents it needs.
//!
//! ## Shape
//!
//! - [`Engine`] — a handle to the engine (cheap to clone). [`Engine::shared`]
//!   is the process-wide instance.
//! - [`Document`] — an open document: page geometry ([`PageGeometry`]),
//!   rendering ([`Document::render`], [`Document::render_tile`]), the text
//!   layer ([`Document::text`] → [`TextLayer`]) with selection and copy,
//!   search, links, outline, form fields, annotations, and
//!   [`Document::export`], which writes [`Edits`] as real PDF objects.
//!
//! A second engine would slot in as another backend behind [`Engine`]; the
//! text analysis in [`text`] only needs the engine's raw characters.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod client;
pub mod edits;
mod error;
pub mod geom;
#[doc(hidden)]
pub mod helper;
pub mod model;
mod pdfium;
mod proto;
mod shm;
pub mod text;

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub use edits::{
    Edits, FieldValue, FormValue, NewAnnotation, NewAnnotationKind, PageEdit, PageSource, StampText,
};
pub use error::{Error, Result};
pub use geom::{PageGeometry, PageSize, Point, Rect, Rotation};
pub use model::{
    AnnotationInfo, AnnotationKind, Destination, DocumentInfo, FieldKind, FieldWidget, FormField,
    Link, Metadata, OutlineEntry, PixelRect, RenderOptions, Rgba, Tile, TileGrid,
};
pub use proto::ProbeReport;
pub use text::{SearchOptions, Selection, TextChar, TextLayer, TextMatch};

use proto::OpenKind;

/// Documents larger than this are refused before any parsing.
pub const MAX_DOCUMENT_BYTES: u64 = 512 * 1024 * 1024;

/// Name of the helper executable.
pub const HELPER_NAME: &str = "tilo-pdf-helper";

/// A rendered image: RGBA8, straight alpha (opaque on white), rows packed
/// without padding.
#[derive(Clone)]
pub struct Bitmap {
    width: u32,
    height: u32,
    pixels: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bitmap")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Bitmap {
    fn from_parts(width: u32, height: u32, pixels: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }

    fn from_vec(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
        }
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The RGBA pixels, `width * height * 4` bytes.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        (*self.pixels).as_ref()
    }

    /// The pixel storage itself, for wrapping into another image type
    /// without copying (for example `peniko::Blob::new`).
    #[must_use]
    pub fn shared_pixels(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        Arc::clone(&self.pixels)
    }
}

/// The in-process engine, used when no helper is available in development
/// builds and for measuring the helper's overhead.
struct Local(pdfium::Engine);

// SAFETY: pdfium is not thread-safe but is thread-agnostic: every call into
// it goes through the `Mutex<Local>` below, so calls never overlap, and no
// handle it returns escapes that lock.
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the raw pdfium handles are only touched under the engine mutex"
)]
unsafe impl Send for Local {}

enum Backend {
    Sandboxed(client::Sandboxed),
    InProcess(Mutex<Local>),
}

struct Inner {
    backend: Backend,
    next_id: AtomicU64,
}

/// A handle to the PDF engine. Cloning shares the engine.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("sandboxed", &self.is_sandboxed())
            .finish()
    }
}

/// Whether development-only overrides are honoured (debug builds).
const DEV_OVERRIDES: bool = cfg!(debug_assertions);

/// Where the helper binary is looked for, in order.
fn helper_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if DEV_OVERRIDES
        && let Some(path) = std::env::var_os("TILO_PDF_HELPER").filter(|p| !p.is_empty())
    {
        out.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        out.push(dir.join(HELPER_NAME));
        // Test binaries live in `target/<profile>/deps`, next to the bins.
        if DEV_OVERRIDES
            && dir.file_name().is_some_and(|n| n == "deps")
            && let Some(profile) = dir.parent()
        {
            out.push(profile.join(HELPER_NAME));
        }
    }
    out.push(PathBuf::from("/usr/libexec").join(HELPER_NAME));
    out
}

impl Engine {
    /// The engine every consumer should use: the sandboxed helper. In
    /// development builds without an installed helper it falls back to the
    /// in-process engine (with a warning on stderr); release builds never
    /// parse a PDF in-process.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when neither is possible.
    pub fn new() -> Result<Self> {
        match helper_candidates().into_iter().find(|p| p.is_file()) {
            Some(helper) => Self::with_helper(&helper),
            None if DEV_OVERRIDES => {
                eprintln!(
                    "tilo-pdf: {HELPER_NAME} not found; parsing PDFs in-process (development build)"
                );
                Self::in_process()
            }
            None => Err(Error::Unavailable(format!(
                "{HELPER_NAME} is not installed"
            ))),
        }
    }

    /// The process-wide engine, created on first use. A failed start is
    /// retried on the next call.
    ///
    /// # Errors
    ///
    /// As [`Engine::new`].
    pub fn shared() -> Result<Self> {
        static SHARED: OnceLock<Mutex<Option<Engine>>> = OnceLock::new();
        let slot = SHARED.get_or_init(|| Mutex::new(None));
        let mut guard = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(engine) = guard.as_ref() {
            return Ok(engine.clone());
        }
        let engine = Self::new()?;
        *guard = Some(engine.clone());
        drop(guard);
        Ok(engine)
    }

    /// An engine running the helper at `helper`.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when the helper cannot start or confine itself.
    pub fn with_helper(helper: &Path) -> Result<Self> {
        Ok(Self::from_backend(Backend::Sandboxed(
            client::Sandboxed::start(helper)?,
        )))
    }

    /// An engine that parses in this process. For development and
    /// measurement only: a malicious document can compromise the caller.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when no pdfium library can be loaded.
    pub fn in_process() -> Result<Self> {
        let library = pdfium::Library::load().map_err(Error::Unavailable)?;
        Ok(Self::from_backend(Backend::InProcess(Mutex::new(Local(
            pdfium::Engine::new(library),
        )))))
    }

    fn from_backend(backend: Backend) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend,
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// Whether documents are parsed in the sandboxed helper.
    #[must_use]
    pub fn is_sandboxed(&self) -> bool {
        matches!(self.inner.backend, Backend::Sandboxed(_))
    }

    fn local(&self) -> Option<std::sync::MutexGuard<'_, Local>> {
        match &self.inner.backend {
            Backend::InProcess(m) => {
                Some(m.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            }
            Backend::Sandboxed(_) => None,
        }
    }

    fn open_kind(&self, fd: OwnedFd, kind: OpenKind, password: Option<String>) -> Result<Document> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let info = match &self.inner.backend {
            Backend::Sandboxed(s) => s.open(id, fd, kind, password)?,
            Backend::InProcess(_) => {
                let file = File::from(fd);
                let mut local = self
                    .local()
                    .ok_or_else(|| Error::Engine("backend".into()))?;
                local
                    .0
                    .open(id, &file, kind, password)
                    .map_err(|f| Error::from_failure(f.kind, f.message))?
            }
        };
        Ok(Document {
            engine: self.clone(),
            id,
            info: Arc::new(info),
            text_cache: Mutex::new(TextCache::default()),
        })
    }

    /// Open a document from bytes. The bytes are copied once into a sealed
    /// memory file the engine reads from.
    ///
    /// # Errors
    ///
    /// Unreadable or protected documents, or an unavailable engine.
    pub fn open_bytes(&self, bytes: &[u8]) -> Result<Document> {
        self.open_bytes_with_password(bytes, None)
    }

    /// As [`Self::open_bytes`], for an encrypted document.
    ///
    /// # Errors
    ///
    /// As [`Self::open_bytes`]; [`Error::Password`] for a wrong password.
    pub fn open_bytes_with_password(
        &self,
        bytes: &[u8],
        password: Option<&str>,
    ) -> Result<Document> {
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(Error::Limit("the document is too large".into()));
        }
        let fd = shm::from_bytes("tilo-pdf-document", bytes)?;
        self.open_kind(fd, OpenKind::Pdf, password.map(str::to_string))
    }

    /// Open a document from a descriptor. A regular file is re-opened
    /// read-only so the engine cannot write through it; anything else (a
    /// pipe, a socket) is read into a sealed memory file first.
    ///
    /// # Errors
    ///
    /// As [`Self::open_bytes`].
    pub fn open_fd(&self, fd: impl AsFd) -> Result<Document> {
        let fd = readonly_document_fd(fd.as_fd())?;
        self.open_kind(fd, OpenKind::Pdf, None)
    }

    /// Open a file by path (read-only).
    ///
    /// # Errors
    ///
    /// As [`Self::open_bytes`], plus I/O errors opening the path.
    pub fn open_path(&self, path: &Path) -> Result<Document> {
        let file = File::open(path)?;
        self.open_fd(&file)
    }

    /// Open a blob from the content-addressed store by its hash.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] when the blob is not (yet) available locally or
    /// does not match its hash; otherwise as [`Self::open_bytes`].
    #[cfg(feature = "blobstore")]
    pub fn open_blob(&self, hash: &str) -> Result<Document> {
        let bytes = qdoc::blobstore::get(hash).ok_or_else(|| {
            Error::Invalid("the document's bytes are not available on this device yet".into())
        })?;
        if !qdoc::blobstore::verify(hash, &bytes) {
            return Err(Error::Invalid(
                "the stored document does not match its hash".into(),
            ));
        }
        self.open_bytes(&bytes)
    }

    /// Wrap an image (PNG, JPEG, WebP, …) as a one-page document, 612 pt
    /// wide. The image is decoded inside the engine.
    ///
    /// # Errors
    ///
    /// Undecodable images.
    pub fn open_image_bytes(&self, bytes: &[u8]) -> Result<Document> {
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err(Error::Limit("the image is too large".into()));
        }
        let fd = shm::from_bytes("tilo-pdf-image", bytes)?;
        self.open_kind(fd, OpenKind::Image, None)
    }

    /// Ask the helper to attempt what its sandbox must refuse. Returns
    /// `None` for the in-process engine.
    ///
    /// # Errors
    ///
    /// Helper failures.
    pub fn probe_sandbox(&self, path: &str) -> Result<Option<ProbeReport>> {
        match &self.inner.backend {
            Backend::Sandboxed(s) => s.probe(path).map(Some),
            Backend::InProcess(_) => Ok(None),
        }
    }

    /// Make a development build of the helper crash in the middle of a
    /// request, returning the error that request produced. For crash
    /// recovery tests.
    #[doc(hidden)]
    pub fn crash_helper_for_test(&self) -> Option<Error> {
        match &self.inner.backend {
            Backend::Sandboxed(s) => Some(s.abort_helper()),
            Backend::InProcess(_) => None,
        }
    }

    /// Process id of the running helper (sandboxed engine only).
    #[must_use]
    pub fn helper_pid(&self) -> Option<u32> {
        match &self.inner.backend {
            Backend::Sandboxed(s) => s.helper_pid(),
            Backend::InProcess(_) => None,
        }
    }

    /// How many helper processes this engine has started so far.
    #[must_use]
    pub fn helpers_started(&self) -> u64 {
        match &self.inner.backend {
            Backend::Sandboxed(s) => s.helpers_started(),
            Backend::InProcess(_) => 0,
        }
    }
}

fn readonly_document_fd(fd: std::os::fd::BorrowedFd<'_>) -> Result<OwnedFd> {
    let file = File::from(fd.try_clone_to_owned()?);
    let meta = file.metadata()?;
    if meta.is_file() {
        if meta.len() > MAX_DOCUMENT_BYTES {
            return Err(Error::Limit("the document is too large".into()));
        }
        let proc_path = format!("/proc/self/fd/{}", file.as_raw_fd());
        if let Ok(reopened) = File::open(&proc_path) {
            return Ok(reopened.into());
        }
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_DOCUMENT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(Error::Limit("the document is too large".into()));
    }
    Ok(shm::from_bytes("tilo-pdf-document", &bytes)?)
}

/// A bounded per-document cache of analysed text layers.
#[derive(Default)]
struct TextCache {
    layers: HashMap<usize, Arc<TextLayer>>,
    order: VecDeque<usize>,
}

const TEXT_CACHE_PAGES: usize = 64;

/// One search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// 0-based page.
    pub page: usize,
    pub hit: TextMatch,
}

/// An open document. Closed when dropped.
pub struct Document {
    engine: Engine,
    id: u64,
    info: Arc<DocumentInfo>,
    text_cache: Mutex<TextCache>,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("id", &self.id)
            .field("pages", &self.info.pages.len())
            .finish_non_exhaustive()
    }
}

macro_rules! dispatch {
    ($self:ident, $sandboxed:ident => $s:expr, $local:ident => $l:expr) => {
        match &$self.engine.inner.backend {
            Backend::Sandboxed($sandboxed) => $s,
            Backend::InProcess(_) => {
                let $local = $self
                    .engine
                    .local()
                    .ok_or_else(|| Error::Engine("backend".into()))?;
                $l.map_err(|f| Error::from_failure(f.kind, f.message))
            }
        }
    };
}

impl Document {
    /// Facts fetched at open.
    #[must_use]
    pub fn info(&self) -> &DocumentInfo {
        &self.info
    }

    #[must_use]
    pub fn page_count(&self) -> usize {
        self.info.pages.len()
    }

    /// Geometry of page `page` (0-based).
    #[must_use]
    pub fn page(&self, page: usize) -> Option<PageGeometry> {
        self.info.pages.get(page).copied()
    }

    /// Display size of page `page`.
    #[must_use]
    pub fn page_size(&self, page: usize) -> Option<PageSize> {
        self.page(page).map(|g| g.size())
    }

    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.info.metadata
    }

    /// The engine this document is open in.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Render a page (or a region of it) at a scale.
    ///
    /// # Errors
    ///
    /// Out-of-range pages, renders over the pixel budget, engine failures.
    pub fn render(&self, page: usize, options: &RenderOptions) -> Result<Bitmap> {
        let size = self
            .page_size(page)
            .ok_or_else(|| Error::Invalid("page out of range".into()))?;
        let (w, h) = options.output_pixels(size);
        let pixels = u64::from(w).saturating_mul(u64::from(h));
        if pixels > model::MAX_RENDER_PIXELS {
            return Err(Error::Limit("the render exceeds the pixel budget".into()));
        }
        dispatch!(self,
            s => s.render(self.id, page, options, pixels.saturating_mul(4)),
            local => {
                local.0.render_size(self.id, page, options).and_then(|(w, h)| {
                    let mut pixels = vec![0u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)];
                    local.0.render_into(self.id, page, options, &mut pixels)?;
                    Ok(Bitmap::from_vec(w, h, pixels))
                })
            }
        )
    }

    /// Render one tile of a [`TileGrid`] at `scale` and `rotation`.
    ///
    /// # Errors
    ///
    /// As [`Self::render`].
    pub fn render_tile(
        &self,
        page: usize,
        scale: f32,
        rotation: Rotation,
        tile: &Tile,
    ) -> Result<Bitmap> {
        self.render(
            page,
            &RenderOptions {
                scale,
                rotation,
                region: Some(tile.region),
                annotations: true,
            },
        )
    }

    /// The raw characters the engine extracted from a page, in engine order.
    ///
    /// # Errors
    ///
    /// Out-of-range pages, engine failures.
    pub fn raw_text(&self, page: usize) -> Result<Vec<text::RawChar>> {
        dispatch!(self, s => s.text(self.id, page), local => local.0.text(self.id, page))
    }

    /// The analysed text layer of a page (cached).
    ///
    /// # Errors
    ///
    /// As [`Self::raw_text`], or [`Error::Limit`] for a page with too much
    /// text to analyse honestly.
    pub fn text(&self, page: usize) -> Result<Arc<TextLayer>> {
        {
            let cache = self
                .text_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(layer) = cache.layers.get(&page) {
                return Ok(Arc::clone(layer));
            }
        }
        let raw = self.raw_text(page)?;
        let geometry = self
            .page(page)
            .ok_or_else(|| Error::Invalid("page out of range".into()))?;
        let unrotated = PageGeometry {
            rotation: Rotation::None,
            ..geometry
        }
        .size();
        let layer = Arc::new(
            TextLayer::from_raw_rotated(&raw, geometry.rotation, unrotated)
                .ok_or_else(|| Error::Limit("the page has too much text".into()))?,
        );
        let mut cache = self
            .text_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.layers.insert(page, Arc::clone(&layer)).is_none() {
            cache.order.push_back(page);
        }
        while cache.order.len() > TEXT_CACHE_PAGES {
            if let Some(old) = cache.order.pop_front() {
                cache.layers.remove(&old);
            }
        }
        drop(cache);
        Ok(layer)
    }

    /// Search the whole document, stopping after `limit` hits.
    ///
    /// # Errors
    ///
    /// Engine failures while extracting text.
    pub fn search(
        &self,
        query: &str,
        options: SearchOptions,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut out = Vec::new();
        for page in 0..self.page_count() {
            if out.len() >= limit {
                break;
            }
            let layer = match self.text(page) {
                Ok(layer) => layer,
                Err(Error::Limit(_)) => continue,
                Err(e) => return Err(e),
            };
            for hit in layer.search(query, options) {
                if out.len() >= limit {
                    break;
                }
                out.push(SearchHit { page, hit });
            }
        }
        Ok(out)
    }

    /// Link areas on a page.
    ///
    /// # Errors
    ///
    /// Out-of-range pages, engine failures.
    pub fn links(&self, page: usize) -> Result<Vec<Link>> {
        dispatch!(self, s => s.links(self.id, page), local => local.0.links(self.id, page))
    }

    /// Annotations present in the file on a page.
    ///
    /// # Errors
    ///
    /// Out-of-range pages, engine failures.
    pub fn annotations(&self, page: usize) -> Result<Vec<AnnotationInfo>> {
        dispatch!(self, s => s.annotations(self.id, page), local => local.0.annotations(self.id, page))
    }

    /// The interactive form's fields (empty without a form).
    ///
    /// # Errors
    ///
    /// Engine failures.
    pub fn form_fields(&self) -> Result<Vec<FormField>> {
        dispatch!(self, s => s.form_fields(self.id), local => local.0.form_fields(self.id))
    }

    /// The outline (bookmarks).
    ///
    /// # Errors
    ///
    /// Engine failures.
    pub fn outline(&self) -> Result<Vec<OutlineEntry>> {
        dispatch!(self, s => s.outline(self.id), local => local.0.outline(self.id))
    }

    /// Write `edits` into a new PDF: an incremental update of this
    /// document's bytes with pages from it and `imports`, annotations as
    /// standard annotation objects and form values as field values. The
    /// same edits on the same bytes always give the same output.
    ///
    /// # Errors
    ///
    /// Out-of-range references (pages, imports), documents from another
    /// engine, engine failures.
    pub fn export(&self, edits: &Edits, imports: &[&Document]) -> Result<Vec<u8>> {
        if imports
            .iter()
            .any(|d| !Arc::ptr_eq(&d.engine.inner, &self.engine.inner))
        {
            return Err(Error::Invalid(
                "imports must be open in the same engine".into(),
            ));
        }
        let ids: Vec<u64> = imports.iter().map(|d| d.id).collect();
        dispatch!(self, s => s.export(self.id, edits, &ids), local => local.0.export(self.id, edits, &ids))
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        match &self.engine.inner.backend {
            Backend::Sandboxed(s) => s.close(self.id),
            Backend::InProcess(m) => {
                m.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0
                    .close(self.id);
            }
        }
    }
}
