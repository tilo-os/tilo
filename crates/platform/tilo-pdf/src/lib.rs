//! `tilo-pdf` — Tilo's one PDF engine API.
//!
//! Every PDF consumer (the PDF viewer, thumbnails, previews) goes through
//! this crate. The API is engine independent: documents, pages, bitmaps,
//! text layers, links, outlines, form fields, annotations and edits are
//! this crate's own types, and no engine type is visible outside the crate.
//!
//! ## Engines
//!
//! | | pdfium (Linux, macOS) | hayro (every target) |
//! |---|---|---|
//! | where | the sandboxed helper | in process |
//! | render, text, search, links, outline, annotations | ✅ | ✅ |
//! | annotation appearances, form fields | ✅ | not drawn / none |
//! | export, images as pages, encrypted files | ✅ | `Unavailable` |
//!
//! On Linux and macOS the engine is pdfium in the sandboxed helper when a
//! pdfium library is installed. hayro ([`Engine::portable`]) is a memory-safe
//! Rust rasteriser, so parsing in process does not carry the risk that makes
//! pdfium a separate, confined process: it is the only engine on the web and
//! the fallback wherever pdfium or its helper is unavailable.
//!
//! ## Isolation
//!
//! PDFs are hostile input and pdfium is a large C++ parser, so documents are
//! parsed in a separate helper process (`tilo-pdf-helper`) that `tilo-sandbox`
//! confines to read-only access to the pdfium library, the system fonts and
//! Tilo's pinned font catalog, with no network and no exec (Landlock and
//! seccomp on Linux, Seatbelt on macOS).
//! Documents reach the helper as read-only descriptors; pages are rendered
//! straight into pooled shared pixel buffers and exports come back as sealed
//! memory files, so no pixel or page is copied through the socket. If the helper crashes or hangs, the request fails
//! with [`Error::Crashed`] or [`Error::Timeout`] and the next request starts
//! a fresh helper, re-opening the documents it needs.
//!
//! ### When the sandbox cannot be enforced
//!
//! Tilo's kernel always has Landlock (ABI 5 and later), but a foreign
//! distribution, an old kernel or a container may not. The helper then
//! refuses to start and the engine reports [`Error::Unconfined`]; it never
//! falls back to seccomp alone. Seccomp keeps the parser off the network and
//! away from `exec`, but only Landlock keeps a compromised parser from
//! opening the user's files, and that is the property the helper exists for.
//! A partially enforced ruleset (a kernel whose Landlock ABI is older than
//! the one Tilo targets) counts as not enforced.
//!
//! Release builds then parse with hayro, never with an unconfined pdfium. A
//! development build instead parses with pdfium in this process, saying so on
//! stderr, exactly as it does when no helper binary is installed — unless [`REQUIRE_SANDBOX_ENV`] is set, which
//! an environment that promises a sandbox-capable kernel (a CI runner with
//! Landlock) does so that an unconfined helper is a failure there. Tests
//! whose assertions need the helper itself go through [`sandboxed_for_test`]:
//! they stand down on a host that cannot confine it, and fail when the
//! variable is set.
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
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(unix)]
mod appearance;
#[cfg(unix)]
mod client;
pub mod edits;
mod error;
#[cfg(unix)]
mod font;
pub mod geom;
#[cfg(unix)]
#[doc(hidden)]
pub mod helper;
pub mod model;
#[cfg(unix)]
mod patch;
#[cfg(unix)]
mod pdfium;
mod portable;
mod proto;
pub mod text;

use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub use edits::{
    AdoptedAnnotation, AnnotationRef, Edits, FieldValue, FormValue, NewAnnotation,
    NewAnnotationKind, PageEdit, PageSource, StampText,
};
pub use error::{Error, Result};
pub use geom::{PageGeometry, PageSize, Point, Rect, Rotation};
pub use model::{
    ANNOT_FLAG_HIDDEN, ANNOT_FLAG_NOVIEW, ANNOT_FLAG_PRINT, AnnotationInfo, AnnotationKind,
    Destination, DocumentInfo, FieldKind, FieldWidget, FormField, Link, Metadata, OutlineEntry,
    PixelRect, RenderOptions, ReplyType, Rgba, Tile, TileGrid,
};
pub use proto::ProbeReport;
pub use text::{SearchOptions, Selection, TextChar, TextLayer, TextMatch};

use proto::OpenKind;
use tilo_lock::MutexRecover as _;
#[cfg(unix)]
use tilo_sandbox::shm;

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

/// The in-process pdfium engine, used when no helper is available in
/// development builds and for measuring the helper's overhead.
#[cfg(unix)]
struct Local(pdfium::Engine);

// SAFETY: pdfium is not thread-safe but is thread-agnostic: every call into
// it goes through the `Mutex<Local>` below, so calls never overlap, and no
// handle it returns escapes that lock.
#[cfg(unix)]
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the raw pdfium handles are only touched under the engine mutex"
)]
unsafe impl Send for Local {}

enum Backend {
    #[cfg(unix)]
    Sandboxed(client::Sandboxed),
    #[cfg(unix)]
    InProcess(Mutex<Local>),
    Portable(Mutex<portable::Engine>),
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
#[cfg(unix)]
const DEV_OVERRIDES: bool = cfg!(debug_assertions);

/// Set by an environment that promises a kernel able to confine the helper;
/// see [When the sandbox cannot be enforced](crate#when-the-sandbox-cannot-be-enforced).
pub const REQUIRE_SANDBOX_ENV: &str = "TILO_REQUIRE_PDF_SANDBOX";

/// Whether this environment promises that the helper can confine itself
/// ([`REQUIRE_SANDBOX_ENV`] is set), so [`Error::Unconfined`] is a failure
/// rather than a property of the host.
#[cfg(unix)]
fn sandbox_required() -> bool {
    std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| !v.is_empty())
}

/// The gate for every test whose assertions need the sandboxed helper
/// itself — its confinement, or the pinned appearance fonts compiled into it
/// — rather than any engine. Tests that only need documents opened use
/// [`Engine::shared`] and run everywhere.
///
/// The engine is [`HelperGate::Sandboxed`] when it is the helper. Otherwise
/// the test stands down, saying on stderr that `what` went unchecked: on a
/// host that cannot confine the helper, and where no helper binary is built
/// (the in-process engine of a development build).
///
/// # Panics
///
/// When the host cannot confine the helper and [`REQUIRE_SANDBOX_ENV`] is
/// set: the environment promised a sandbox.
pub fn sandboxed_for_test(engine: Result<Engine>, what: &str) -> HelperGate {
    match engine {
        Ok(engine) if engine.is_sandboxed() => HelperGate::Sandboxed(engine),
        Ok(_) => {
            eprintln!(
                "SKIPPING {what}: the PDF engine here is not the sandboxed helper \
                 (none is built next to this binary, or this host cannot confine it)"
            );
            HelperGate::StandDown
        }
        Err(Error::Unconfined(reason)) => {
            #[cfg(unix)]
            assert!(
                !sandbox_required(),
                "{REQUIRE_SANDBOX_ENV} is set, but this host cannot confine the PDF helper: {reason}"
            );
            eprintln!(
                "SKIPPING {what}: this host cannot confine the PDF helper ({reason}); \
                 set {REQUIRE_SANDBOX_ENV} to make this a failure"
            );
            HelperGate::StandDown
        }
        Err(e) => HelperGate::Failed(e),
    }
}

/// The first page of `pdf`, rasterised through [`Engine::shared`] to fit a
/// `width` x `height` box: straight-alpha RGBA pixels and their size. `None`
/// when no engine is available or the document is unreadable.
///
/// Its signature is a file preview's (`tilo_thumbnail::PdfRasteriser`): an
/// app that links this engine hands it to the thumbnailer once, and every
/// other app's previews stay free of the engine.
pub fn first_page_preview(pdf: &[u8], width: u32, height: u32) -> Option<(Vec<u8>, u32, u32)> {
    let doc = Engine::shared().ok()?.open_bytes(pdf).ok()?;
    let page = doc.page_size(0)?;
    if !(page.width.is_finite() && page.height.is_finite() && page.width > 0.0 && page.height > 0.0)
    {
        return None;
    }
    // Render straight at the size that fits the box, so the helper never
    // rasterises more pixels than the preview keeps.
    #[allow(
        clippy::cast_precision_loss,
        reason = "preview boxes are a few hundred pixels"
    )]
    let scale = (width.max(1) as f32 / page.width).min(height.max(1) as f32 / page.height);
    let options = RenderOptions {
        scale,
        rotation: Rotation::None,
        region: None,
        annotations: true,
        reader_markup: true,
    };
    let bitmap = doc.render(0, &options).ok()?;
    Some((bitmap.pixels().to_vec(), bitmap.width(), bitmap.height()))
}

/// What [`sandboxed_for_test`] decided.
#[derive(Debug)]
pub enum HelperGate {
    /// The helper runs confined here: run the test against it.
    Sandboxed(Engine),
    /// It cannot, and nothing promised it could: the test stands down.
    StandDown,
    /// The engine failed for a reason unrelated to the sandbox.
    Failed(Error),
}

/// Where the helper binary is looked for, in order.
#[cfg(unix)]
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
    /// The engine every consumer should use: pdfium in the sandboxed helper
    /// where a pdfium library is installed, otherwise the portable engine.
    /// In development builds without an installed helper, or on a host that
    /// cannot confine it, pdfium runs in process (with a warning on stderr);
    /// release builds never parse with pdfium in process and use the portable
    /// engine instead. See
    /// [When the sandbox cannot be enforced](crate#when-the-sandbox-cannot-be-enforced).
    ///
    /// # Errors
    ///
    /// [`Error::Unconfined`] when the host cannot confine the helper and
    /// [`REQUIRE_SANDBOX_ENV`] is set, [`Error::Unavailable`] when the helper
    /// cannot start.
    pub fn new() -> Result<Self> {
        #[cfg(not(unix))]
        return Ok(Self::portable());
        #[cfg(unix)]
        {
            if !pdfium::library_candidates().iter().any(|p| p.is_file()) {
                return Ok(Self::portable());
            }
            match helper_candidates().into_iter().find(|p| p.is_file()) {
                Some(helper) => match Self::with_helper(&helper) {
                    Err(Error::Unconfined(reason)) if DEV_OVERRIDES && !sandbox_required() => {
                        eprintln!(
                            "tilo-pdf: {HELPER_NAME} cannot confine itself here ({reason}); \
                             parsing PDFs in-process (development build; set \
                             {REQUIRE_SANDBOX_ENV} to refuse instead)"
                        );
                        Self::in_process()
                    }
                    Err(Error::Unconfined(reason)) if !sandbox_required() => {
                        eprintln!(
                            "tilo-pdf: {HELPER_NAME} cannot confine itself here ({reason}); \
                             using the portable engine"
                        );
                        Ok(Self::portable())
                    }
                    started => started,
                },
                None if DEV_OVERRIDES => {
                    eprintln!(
                        "tilo-pdf: {HELPER_NAME} not found; parsing PDFs in-process (development build)"
                    );
                    Self::in_process()
                }
                None => Ok(Self::portable()),
            }
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
        let mut guard = slot.lock_recover();
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
    /// [`Error::Unconfined`] when the helper cannot confine itself,
    /// [`Error::Unavailable`] when it cannot start.
    pub fn with_helper(helper: &Path) -> Result<Self> {
        #[cfg(unix)]
        return Ok(Self::from_backend(Backend::Sandboxed(
            client::Sandboxed::start(helper)?,
        )));
        #[cfg(not(unix))]
        Err(Error::Unavailable(format!(
            "the sandboxed helper ({}) runs on Unix only",
            helper.display()
        )))
    }

    /// An engine that parses in this process. For development and
    /// measurement only: a malicious document can compromise the caller.
    ///
    /// # Errors
    ///
    /// [`Error::Unavailable`] when no pdfium library can be loaded.
    pub fn in_process() -> Result<Self> {
        #[cfg(unix)]
        return match pdfium::Library::load() {
            Ok(library) => Ok(Self::from_backend(Backend::InProcess(Mutex::new(Local(
                pdfium::Engine::new(library),
            ))))),
            Err(reason) if DEV_OVERRIDES => {
                eprintln!("tilo-pdf: no pdfium library ({reason}); using the portable engine");
                Ok(Self::portable())
            }
            Err(reason) => Err(Error::Unavailable(reason)),
        };
        #[cfg(not(unix))]
        Ok(Self::portable())
    }

    /// The portable engine: hayro, a memory-safe rasteriser, in this
    /// process. Available on every target, and the default wherever pdfium is
    /// not installed; see
    /// [Engines](crate#engines) for what it does not do yet.
    #[must_use]
    pub fn portable() -> Self {
        Self::from_backend(Backend::Portable(Mutex::new(portable::Engine::default())))
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
        #[cfg(unix)]
        return matches!(self.inner.backend, Backend::Sandboxed(_));
        #[cfg(not(unix))]
        false
    }

    #[cfg(unix)]
    fn local(&self) -> Option<std::sync::MutexGuard<'_, Local>> {
        match &self.inner.backend {
            Backend::InProcess(m) => Some(m.lock_recover()),
            _ => None,
        }
    }

    #[cfg_attr(
        not(unix),
        allow(
            clippy::unnecessary_wraps,
            reason = "Unix has engines that are not portable"
        )
    )]
    fn portable_engine(&self) -> Option<std::sync::MutexGuard<'_, portable::Engine>> {
        match &self.inner.backend {
            Backend::Portable(m) => Some(m.lock_recover()),
            #[cfg(unix)]
            _ => None,
        }
    }

    fn document(&self, id: u64, info: DocumentInfo) -> Document {
        Document {
            engine: self.clone(),
            id,
            info: Arc::new(info),
            text_cache: Mutex::new(TextCache::default()),
        }
    }

    /// Open `bytes` in whichever engine this is.
    fn open_owned(&self, bytes: &[u8], kind: OpenKind, password: Option<&str>) -> Result<Document> {
        if let Some(mut engine) = self.portable_engine() {
            if kind == OpenKind::Image {
                return Err(Error::Unavailable(
                    "the portable PDF engine does not open images as pages yet".into(),
                ));
            }
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            let info = engine.open(id, bytes.to_vec(), password)?;
            drop(engine);
            return Ok(self.document(id, info));
        }
        #[cfg(unix)]
        {
            let name = match kind {
                OpenKind::Pdf => "tilo-pdf-document",
                OpenKind::Image => "tilo-pdf-image",
            };
            let fd = shm::from_bytes(name, bytes)?;
            let len = Some(bytes.len() as u64);
            self.open_kind(fd, len, kind, password.map(str::to_string))
        }
        #[cfg(not(unix))]
        {
            Err(Error::Engine("backend".into()))
        }
    }

    /// `len` is the document's length when the descriptor is a memory file,
    /// whose size can exceed its content; `None` reads the whole file.
    #[cfg(unix)]
    fn open_kind(
        &self,
        fd: OwnedFd,
        len: Option<u64>,
        kind: OpenKind,
        password: Option<String>,
    ) -> Result<Document> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let info = match &self.inner.backend {
            Backend::Sandboxed(s) => s.open(id, fd, len, kind, password)?,
            Backend::InProcess(_) => {
                let file = File::from(fd);
                let mut local = self
                    .local()
                    .ok_or_else(|| Error::Engine("backend".into()))?;
                local
                    .0
                    .open(id, &file, len, kind, password)
                    .map_err(|f| Error::from_failure(f.kind, f.message))?
            }
            Backend::Portable(_) => return Err(Error::Engine("backend".into())),
        };
        Ok(self.document(id, info))
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
        self.open_owned(bytes, OpenKind::Pdf, password)
    }

    /// Open a document from a descriptor. A regular file is re-opened
    /// read-only so the engine cannot write through it; anything else (a
    /// pipe, a socket) is read into a sealed memory file first.
    ///
    /// # Errors
    ///
    /// As [`Self::open_bytes`].
    #[cfg(unix)]
    pub fn open_fd(&self, fd: impl AsFd) -> Result<Document> {
        #[cfg(unix)]
        if self.portable_engine().is_none() {
            let (fd, len) = readonly_document_fd(fd.as_fd())?;
            return self.open_kind(fd, len, OpenKind::Pdf, None);
        }
        let mut bytes = Vec::new();
        (&std::fs::File::from(fd.as_fd().try_clone_to_owned()?))
            .take(MAX_DOCUMENT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        self.open_bytes(&bytes)
    }

    /// Open a file by path (read-only).
    ///
    /// # Errors
    ///
    /// As [`Self::open_bytes`], plus I/O errors opening the path.
    pub fn open_path(&self, path: &Path) -> Result<Document> {
        #[cfg(unix)]
        return self.open_fd(&std::fs::File::open(path)?);
        #[cfg(not(unix))]
        self.open_bytes(&std::fs::read(path)?)
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
        self.open_owned(bytes, OpenKind::Image, None)
    }

    /// Ask the helper to attempt what its sandbox must refuse. Returns
    /// `None` for the in-process engine.
    ///
    /// # Errors
    ///
    /// Helper failures.
    pub fn probe_sandbox(&self, path: &str) -> Result<Option<ProbeReport>> {
        match &self.inner.backend {
            #[cfg(unix)]
            Backend::Sandboxed(s) => s.probe(path).map(Some),
            _ => {
                let _ = path;
                Ok(None)
            }
        }
    }

    /// Make a development build of the helper crash in the middle of a
    /// request, returning the error that request produced. For crash
    /// recovery tests.
    #[doc(hidden)]
    pub fn crash_helper_for_test(&self) -> Option<Error> {
        match &self.inner.backend {
            #[cfg(unix)]
            Backend::Sandboxed(s) => Some(s.abort_helper()),
            _ => None,
        }
    }

    /// Process id of the running helper (sandboxed engine only).
    #[must_use]
    pub fn helper_pid(&self) -> Option<u32> {
        match &self.inner.backend {
            #[cfg(unix)]
            Backend::Sandboxed(s) => s.helper_pid(),
            _ => None,
        }
    }

    /// How many helper processes this engine has started so far.
    #[must_use]
    pub fn helpers_started(&self) -> u64 {
        match &self.inner.backend {
            #[cfg(unix)]
            Backend::Sandboxed(s) => s.helpers_started(),
            _ => 0,
        }
    }
}

/// A read-only descriptor for the document behind `fd`, and its length when
/// it had to be copied into a memory file.
#[cfg(unix)]
fn readonly_document_fd(fd: std::os::fd::BorrowedFd<'_>) -> Result<(OwnedFd, Option<u64>)> {
    let file = File::from(fd.try_clone_to_owned()?);
    let meta = file.metadata()?;
    if meta.is_file() {
        if meta.len() > MAX_DOCUMENT_BYTES {
            return Err(Error::Limit("the document is too large".into()));
        }
        if let Some(reopened) = shm::reopen_read_only(&file) {
            return Ok((reopened.into(), None));
        }
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_DOCUMENT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(Error::Limit("the document is too large".into()));
    }
    let len = Some(bytes.len() as u64);
    Ok((shm::from_bytes("tilo-pdf-document", &bytes)?, len))
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
    ($self:ident, $sandboxed:ident => $s:expr, $local:ident => $l:expr, $portable:ident => $p:expr) => {
        match &$self.engine.inner.backend {
            #[cfg(unix)]
            Backend::Sandboxed($sandboxed) => $s,
            #[cfg(unix)]
            Backend::InProcess(_) => {
                let $local = $self
                    .engine
                    .local()
                    .ok_or_else(|| Error::Engine("backend".into()))?;
                $l.map_err(|f| Error::from_failure(f.kind, f.message))
            }
            Backend::Portable(m) => {
                #[allow(unused_mut, reason = "rendering mutates the portable engine's cache")]
                let mut $portable = m.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                $p
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
            },
            p => p.render(self.id, page, options)
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
                ..RenderOptions::default()
            },
        )
    }

    /// The raw characters the engine extracted from a page, in engine order.
    ///
    /// # Errors
    ///
    /// Out-of-range pages, engine failures.
    pub fn raw_text(&self, page: usize) -> Result<Vec<text::RawChar>> {
        dispatch!(self, s => s.text(self.id, page), local => local.0.text(self.id, page), p => p.text(self.id, page))
    }

    /// The analysed text layer of a page (cached).
    ///
    /// # Errors
    ///
    /// As [`Self::raw_text`], or [`Error::Limit`] for a page with too much
    /// text to analyse honestly.
    pub fn text(&self, page: usize) -> Result<Arc<TextLayer>> {
        {
            let cache = self.text_cache.lock_recover();
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
        let mut cache = self.text_cache.lock_recover();
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
        dispatch!(self, s => s.links(self.id, page), local => local.0.links(self.id, page), p => p.links(self.id, page))
    }

    /// Annotations present in the file on a page.
    ///
    /// # Errors
    ///
    /// Out-of-range pages, engine failures.
    pub fn annotations(&self, page: usize) -> Result<Vec<AnnotationInfo>> {
        dispatch!(self, s => s.annotations(self.id, page), local => local.0.annotations(self.id, page), p => p.annotations(self.id, page))
    }

    /// The interactive form's fields (empty without a form).
    ///
    /// # Errors
    ///
    /// Engine failures.
    pub fn form_fields(&self) -> Result<Vec<FormField>> {
        dispatch!(self, s => s.form_fields(self.id), local => local.0.form_fields(self.id), p => p.form_fields(self.id))
    }

    /// The outline (bookmarks).
    ///
    /// # Errors
    ///
    /// Engine failures.
    pub fn outline(&self) -> Result<Vec<OutlineEntry>> {
        dispatch!(self, s => s.outline(self.id), local => local.0.outline(self.id), p => p.outline(self.id))
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
        dispatch!(self,
            s => s.export(self.id, edits, &ids),
            local => local.0.export(self.id, edits, &ids),
            p => {
                let _ = (&p, edits, ids);
                Err(Error::Unavailable("the portable PDF engine cannot save edits yet".into()))
            }
        )
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        match &self.engine.inner.backend {
            #[cfg(unix)]
            Backend::Sandboxed(s) => s.close(self.id),
            #[cfg(unix)]
            Backend::InProcess(m) => {
                m.lock_recover().0.close(self.id);
            }
            Backend::Portable(m) => {
                m.lock_recover().close(self.id);
            }
        }
    }
}
