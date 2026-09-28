//! The pdfium engine: the one place that talks to the native library.
//!
//! Runs inside the sandboxed helper process in production, and in-process
//! only for development fallbacks and measurements. Everything it returns
//! is the crate's engine-independent model; no pdfium type leaves this
//! module.
//!
//! pdfium is driven through its raw C API (via `pdfium-render`'s dynamic
//! bindings) rather than the high-level wrapper: document lifetimes, form
//! environments and memory-mapped inputs are owned explicitly here.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "C API integer and float conversions; every value is range-checked where it matters"
)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{c_int, c_ulong, c_void};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::ptr::null_mut;

use pdfium_render::prelude::{
    FPDF_ACTION, FPDF_ANNOTATION, FPDF_BITMAP, FPDF_BOOKMARK, FPDF_DEST, FPDF_DOCUMENT,
    FPDF_FILEWRITE, FPDF_FORMFILLINFO, FPDF_FORMHANDLE, FPDF_PAGE, FPDF_WCHAR, FS_MATRIX,
    FS_POINTF, FS_QUADPOINTSF, FS_RECTF, Pdfium, PdfiumLibraryBindings,
};

use crate::edits::{
    AdoptedAnnotation, Edits, FieldValue, NewAnnotation, NewAnnotationKind, PageSource,
};
use crate::geom::{PageGeometry, Point, Rect, Rotation};
use crate::model::{
    AnnotationInfo, AnnotationKind, Destination, DocumentInfo, FieldKind, FieldWidget, FormField,
    Link, MAX_RENDER_PIXELS, Metadata, OutlineEntry, RenderOptions, ReplyType, Rgba,
    parse_default_appearance,
};
use crate::proto::{FailureKind, OpenKind};
use crate::text::{MAX_PAGE_CHARS, RawChar, raw_flags};

/// A failure inside the engine, classified for the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

type Result<T> = std::result::Result<T, Failure>;

/// Documents with more pages are refused as unreasonable.
pub const MAX_PAGES: usize = 20_000;
/// Page boxes come from untrusted files; refuse absurd geometry.
const MAX_PAGE_EDGE: f32 = 144_000.0;

// pdfium C constants (fpdfview.h, fpdf_annot.h, fpdf_formfill.h, fpdf_doc.h).
const FPDF_ERR_PASSWORD: c_ulong = 4;
const FPDF_ANNOT: c_int = 0x01;
const FPDF_REVERSE_BYTE_ORDER: c_int = 0x10;
const FPDF_BITMAP_BGRA: c_int = 4;
const FPDF_INCREMENTAL: c_ulong = 1;
const FPDF_NO_INCREMENTAL: c_ulong = 2;
const FORMTYPE_NONE: c_int = 0;
const COLORTYPE_COLOR: u32 = 0;
const ANNOT_FLAG_HIDDEN: c_int = 2;
const ANNOT_FLAG_PRINT: c_int = 4;
const PDFACTION_GOTO: c_ulong = 1;
const PDFACTION_REMOTEGOTO: c_ulong = 2;
const PDFACTION_URI: c_ulong = 3;
const PDFACTION_LAUNCH: c_ulong = 4;
const FIELD_FLAG_READONLY: c_int = 1;
const FIELD_FLAG_REQUIRED: c_int = 1 << 1;
const FIELD_FLAG_TEXT_MULTILINE: c_int = 1 << 12;

mod subtype {
    use std::ffi::c_int;
    pub const TEXT: c_int = 1;
    pub const LINK: c_int = 2;
    pub const FREETEXT: c_int = 3;
    pub const LINE: c_int = 4;
    pub const SQUARE: c_int = 5;
    pub const CIRCLE: c_int = 6;
    pub const POLYGON: c_int = 7;
    pub const POLYLINE: c_int = 8;
    pub const HIGHLIGHT: c_int = 9;
    pub const UNDERLINE: c_int = 10;
    pub const SQUIGGLY: c_int = 11;
    pub const STRIKEOUT: c_int = 12;
    pub const STAMP: c_int = 13;
    pub const CARET: c_int = 14;
    pub const INK: c_int = 15;
    pub const POPUP: c_int = 16;
    pub const FILEATTACHMENT: c_int = 17;
    pub const WIDGET: c_int = 20;
    pub const REDACT: c_int = 28;
}

mod field_type {
    use std::ffi::c_int;
    pub const PUSHBUTTON: c_int = 1;
    pub const CHECKBOX: c_int = 2;
    pub const RADIOBUTTON: c_int = 3;
    pub const COMBOBOX: c_int = 4;
    pub const LISTBOX: c_int = 5;
    pub const TEXTFIELD: c_int = 6;
    pub const SIGNATURE: c_int = 7;
}

/// Where to look for the pdfium shared library, in order. The library is
/// never bundled; `TILO_PDFIUM_LIB` overrides the search.
#[must_use]
pub fn library_candidates() -> Vec<PathBuf> {
    let name = format!(
        "{}pdfium{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("TILO_PDFIUM_LIB").filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(path));
    }
    for dir in [
        "/usr/lib64",
        "/usr/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/app/lib",
        "/opt/homebrew/lib",
        "/usr/local/lib",
    ] {
        out.push(Path::new(dir).join(&name));
    }
    if let Some(home) = tilo_paths::home() {
        out.push(home.join(".local/lib/pdfium").join(&name));
    }
    out
}

/// Directories pdfium reads system fonts from when a document uses a font
/// it does not embed.
#[must_use]
pub fn font_directories() -> Vec<PathBuf> {
    tilo_paths::system_font_dirs()
}

/// The loaded library.
pub struct Library {
    pdfium: Pdfium,
    path: PathBuf,
}

impl Library {
    /// Bind the first usable pdfium library and initialise it.
    ///
    /// # Errors
    ///
    /// When no candidate library loads.
    pub fn load() -> std::result::Result<Self, String> {
        for path in library_candidates() {
            if !path.exists() {
                continue;
            }
            if let Ok(bindings) = Pdfium::bind_to_library(&path) {
                return Ok(Self {
                    pdfium: Pdfium::new(bindings),
                    path,
                });
            }
        }
        Err(format!(
            "no usable pdfium library was found; install one (e.g. \
             ~/.local/lib/pdfium/{}pdfium{}) or set TILO_PDFIUM_LIB",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
    }

    /// Path of the loaded library file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn api(&self) -> &dyn PdfiumLibraryBindings {
        self.pdfium.bindings()
    }
}

/// Bytes a pdfium document reads from; must outlive the document.
enum Backing {
    /// The document's bytes: pdfium reads them in place, and the
    /// structure reader shares them.
    Mapped(std::sync::Arc<tilo_sandbox::shm::Bytes>),
    None,
}

/// A form-fill environment and the callback table pdfium keeps a pointer to.
struct Form {
    handle: FPDF_FORMHANDLE,
    _info: Pin<Box<FPDF_FORMFILLINFO>>,
}

struct Doc {
    raw: FPDF_DOCUMENT,
    form: Option<Form>,
    pages: Vec<PageGeometry>,
    password: Option<String>,
    backing: Backing,
    /// The file's object structure, read on first use for what pdfium's
    /// API does not expose (see [`Engine::annotation_colors`]).
    structure: std::cell::OnceCell<Option<hayro_syntax::Pdf>>,
}

/// The engine: a library and the documents opened in it.
pub struct Engine {
    lib: Library,
    docs: HashMap<u64, Doc>,
}

/// A page loaded for one operation; closed (with its form view) on drop.
struct PageGuard<'a> {
    api: &'a dyn PdfiumLibraryBindings,
    raw: FPDF_PAGE,
    form: Option<FPDF_FORMHANDLE>,
}

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        if let Some(form) = self.form {
            self.api.FORM_OnBeforeClosePage(self.raw, form);
        }
        self.api.FPDF_ClosePage(self.raw);
    }
}

fn empty_form_info() -> FPDF_FORMFILLINFO {
    FPDF_FORMFILLINFO {
        version: 2,
        Release: None,
        FFI_Invalidate: None,
        FFI_OutputSelectedRect: None,
        FFI_SetCursor: None,
        FFI_SetTimer: None,
        FFI_KillTimer: None,
        FFI_GetLocalTime: None,
        FFI_OnChange: None,
        FFI_GetPage: None,
        FFI_GetCurrentPage: None,
        FFI_GetRotation: None,
        FFI_ExecuteNamedAction: None,
        FFI_SetTextFieldFocus: None,
        FFI_DoURIAction: None,
        FFI_DoGoToAction: None,
        m_pJsPlatform: null_mut(),
        xfa_disabled: 1,
        FFI_DisplayCaret: None,
        FFI_GetCurrentPageIndex: None,
        FFI_SetCurrentPage: None,
        FFI_GotoURL: None,
        FFI_GetPageViewRect: None,
        FFI_PageEvent: None,
        FFI_PopupMenu: None,
        FFI_OpenFile: None,
        FFI_EmailTo: None,
        FFI_UploadTo: None,
        FFI_GetPlatform: None,
        FFI_GetLanguage: None,
        FFI_DownloadFromURL: None,
        FFI_PostRequestURL: None,
        FFI_PutRequestURL: None,
        FFI_OnFocusChange: None,
        FFI_DoURIActionWithKeyboardModifier: None,
    }
}

fn utf16_to_string(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(units.get(..end).unwrap_or_default())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a UTF-16LE string through pdfium's two-call byte-length protocol.
fn read_wide(mut call: impl FnMut(*mut FPDF_WCHAR, c_ulong) -> c_ulong) -> String {
    let bytes = call(null_mut(), 0);
    if !(2..=1 << 24).contains(&bytes) {
        return String::new();
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let written = call(buf.as_mut_ptr(), bytes);
    if written == 0 {
        return String::new();
    }
    utf16_to_string(&buf)
}

/// Same protocol for APIs typed with `void*` buffers.
fn read_wide_void(mut call: impl FnMut(*mut c_void, c_ulong) -> c_ulong) -> String {
    read_wide(|ptr, len| call(ptr.cast(), len))
}

/// Read a byte string (ASCII/UTF-8) through the two-call protocol.
fn read_bytes(mut call: impl FnMut(*mut c_void, c_ulong) -> c_ulong) -> String {
    let len = call(null_mut(), 0);
    if len == 0 || len > 1 << 24 {
        return String::new();
    }
    let mut buf = vec![0u8; len as usize];
    call(buf.as_mut_ptr().cast(), len);
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(end);
    String::from_utf8_lossy(&buf).into_owned()
}

fn fs_rect(r: &FS_RECTF) -> Rect {
    // pdfium user-space rects are left/top/right/bottom with top > bottom.
    Rect::from_corners(Point::new(r.left, r.bottom), Point::new(r.right, r.top))
}

fn to_fs_rect(r: Rect) -> FS_RECTF {
    FS_RECTF {
        left: r.x0.min(r.x1),
        top: r.y0.max(r.y1),
        right: r.x0.max(r.x1),
        bottom: r.y0.min(r.y1),
    }
}

fn valid_geometry(g: &PageGeometry) -> bool {
    let s = g.size();
    g.user_box.is_finite()
        && s.width > 0.0
        && s.height > 0.0
        && s.width <= MAX_PAGE_EDGE
        && s.height <= MAX_PAGE_EDGE
}

/// A pdfium `FPDF_FILEWRITE` that appends to a `Vec`.
#[repr(C)]
struct VecWriter {
    base: FPDF_FILEWRITE,
    out: *mut Vec<u8>,
}

unsafe extern "C" fn write_block(
    this: *mut FPDF_FILEWRITE,
    data: *const c_void,
    size: c_ulong,
) -> c_int {
    if this.is_null() || (data.is_null() && size > 0) {
        return 0;
    }
    // SAFETY: pdfium passes back the pointer we gave it; `VecWriter` is
    // `repr(C)` with the `FPDF_FILEWRITE` first, so the cast recovers it,
    // and `out` points at a live `Vec` for the duration of the save call.
    unsafe {
        let writer = this.cast::<VecWriter>();
        let out = &mut *(*writer).out;
        if size > 0 {
            out.extend_from_slice(std::slice::from_raw_parts(data.cast::<u8>(), size as usize));
        }
    }
    1
}

impl Engine {
    /// An engine over a loaded library.
    #[must_use]
    pub fn new(lib: Library) -> Self {
        Self {
            lib,
            docs: HashMap::new(),
        }
    }

    fn api(&self) -> &dyn PdfiumLibraryBindings {
        self.lib.api()
    }

    fn doc(&self, id: u64) -> Result<&Doc> {
        self.docs
            .get(&id)
            .ok_or_else(|| Failure::new(FailureKind::Invalid, "unknown document"))
    }

    fn load_page<'a>(&'a self, doc: &Doc, page: usize, with_form: bool) -> Result<PageGuard<'a>> {
        if page >= doc.pages.len() {
            return Err(Failure::new(FailureKind::Invalid, "page out of range"));
        }
        let api = self.api();
        let raw = api.FPDF_LoadPage(doc.raw, page as c_int);
        if raw.is_null() {
            return Err(Failure::new(
                FailureKind::Engine,
                "the page could not be loaded",
            ));
        }
        let form = if with_form {
            doc.form.as_ref().map(|f| {
                api.FORM_OnAfterLoadPage(raw, f.handle);
                f.handle
            })
        } else {
            None
        };
        Ok(PageGuard { api, raw, form })
    }

    fn init_form(&self, raw: FPDF_DOCUMENT) -> Option<Form> {
        let api = self.api();
        if api.FPDF_GetFormType(raw) == FORMTYPE_NONE {
            return None;
        }
        let mut info = Box::pin(empty_form_info());
        let handle = api.FPDFDOC_InitFormFillEnvironment(raw, &raw mut *info);
        (!handle.is_null()).then_some(Form {
            handle,
            _info: info,
        })
    }

    fn close_raw(&self, raw: FPDF_DOCUMENT, form: Option<Form>) {
        if let Some(form) = form {
            self.api().FPDFDOC_ExitFormFillEnvironment(form.handle);
        }
        self.api().FPDF_CloseDocument(raw);
    }

    fn load_failure(&self) -> Failure {
        if self.api().FPDF_GetLastError() == FPDF_ERR_PASSWORD {
            Failure::new(FailureKind::Password, "the document is password protected")
        } else {
            Failure::new(FailureKind::Format, "the file is not a readable PDF")
        }
    }

    fn page_geometries(&self, raw: FPDF_DOCUMENT) -> Result<Vec<PageGeometry>> {
        let api = self.api();
        let count = api.FPDF_GetPageCount(raw);
        let count = usize::try_from(count).unwrap_or(0);
        if count == 0 {
            return Err(Failure::new(
                FailureKind::Format,
                "the document has no pages",
            ));
        }
        if count > MAX_PAGES {
            return Err(Failure::new(
                FailureKind::Limit,
                format!("the document has more than {MAX_PAGES} pages"),
            ));
        }
        let mut pages = Vec::with_capacity(count);
        for index in 0..count {
            let page = api.FPDF_LoadPage(raw, index as c_int);
            if page.is_null() {
                return Err(Failure::new(
                    FailureKind::Format,
                    "a page could not be loaded",
                ));
            }
            let (mut l, mut b, mut r, mut t) = (0f32, 0f32, 0f32, 0f32);
            let has_crop =
                api.FPDFPage_GetCropBox(page, &raw mut l, &raw mut b, &raw mut r, &raw mut t) != 0;
            if !has_crop {
                let has_media = api
                    .FPDFPage_GetMediaBox(page, &raw mut l, &raw mut b, &raw mut r, &raw mut t)
                    != 0;
                if !has_media {
                    // pdfium falls back to US Letter for a page without boxes.
                    (l, b, r, t) = (0.0, 0.0, 612.0, 792.0);
                }
            }
            let rotation =
                Rotation::from_degrees(api.FPDFPage_GetRotation(page).saturating_mul(90))
                    .unwrap_or_default();
            api.FPDF_ClosePage(page);
            let geometry = PageGeometry {
                user_box: Rect::from_corners(Point::new(l, b), Point::new(r, t)),
                rotation,
            };
            if !valid_geometry(&geometry) {
                return Err(Failure::new(
                    FailureKind::Limit,
                    "a page has unsupported dimensions",
                ));
            }
            pages.push(geometry);
        }
        Ok(pages)
    }

    fn metadata(&self, raw: FPDF_DOCUMENT) -> Metadata {
        let api = self.api();
        let get = |tag: &str| read_wide_void(|buf, len| api.FPDF_GetMetaText(raw, tag, buf, len));
        Metadata {
            title: get("Title"),
            author: get("Author"),
            subject: get("Subject"),
            keywords: get("Keywords"),
            creator: get("Creator"),
            producer: get("Producer"),
        }
    }

    /// Open a document from `file` under `id`.
    ///
    /// # Errors
    ///
    /// Unreadable, encrypted (without the right password) or out-of-limits
    /// documents.
    pub fn open(
        &mut self,
        id: u64,
        file: &File,
        len: Option<u64>,
        kind: OpenKind,
        password: Option<String>,
    ) -> Result<DocumentInfo> {
        if self.docs.contains_key(&id) {
            return Err(Failure::new(FailureKind::Invalid, "document id in use"));
        }
        let (raw, backing) = match kind {
            OpenKind::Pdf => {
                let map = tilo_sandbox::shm::map_document(file, len)
                    .map_err(|e| Failure::new(FailureKind::Format, e.to_string()))?;
                let raw = self.api().FPDF_LoadMemDocument64(&map, password.as_deref());
                if raw.is_null() {
                    return Err(self.load_failure());
                }
                (raw, Backing::Mapped(std::sync::Arc::new(map)))
            }
            OpenKind::Image => (self.image_document(file, len)?, Backing::None),
        };
        let pages = match self.page_geometries(raw) {
            Ok(pages) => pages,
            Err(e) => {
                self.close_raw(raw, None);
                return Err(e);
            }
        };
        let form = self.init_form(raw);
        let info = DocumentInfo {
            pages: pages.clone(),
            metadata: self.metadata(raw),
            has_form: form.is_some(),
        };
        self.docs.insert(
            id,
            Doc {
                raw,
                form,
                pages,
                password,
                backing,
                structure: std::cell::OnceCell::new(),
            },
        );
        Ok(info)
    }

    /// Wrap an image file as a one-page document, 612 pt wide with the
    /// image's aspect ratio.
    fn image_document(&self, file: &File, len: Option<u64>) -> Result<FPDF_DOCUMENT> {
        const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
        // Mapped rather than read: the descriptor's offset is shared with
        // the sender and may sit anywhere.
        let bytes = tilo_sandbox::shm::map_document(file, len)
            .map_err(|e| Failure::new(FailureKind::Format, e.to_string()))?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(Failure::new(FailureKind::Limit, "the image is too large"));
        }
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes[..]))
            .with_guessed_format()
            .map_err(|e| Failure::new(FailureKind::Format, e.to_string()))?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(MAX_RENDER_PIXELS.saturating_mul(4));
        reader.limits(limits);
        let img = reader
            .decode()
            .map_err(|e| Failure::new(FailureKind::Format, format!("decoding the image: {e}")))?
            .to_rgba8();
        let (iw, ih) = img.dimensions();
        if iw == 0 || ih == 0 {
            return Err(Failure::new(FailureKind::Format, "the image is empty"));
        }
        // pdfium bitmaps are BGRA.
        let mut bgra = img.into_raw();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let pw = 612.0f64;
        let ph = (pw * f64::from(ih) / f64::from(iw)).clamp(72.0, 3000.0);
        let api = self.api();
        let raw = api.FPDF_CreateNewDocument();
        if raw.is_null() {
            return Err(Failure::new(
                FailureKind::Engine,
                "creating a document failed",
            ));
        }
        let page = api.FPDFPage_New(raw, 0, pw, ph);
        let fail = |msg: &str| {
            api.FPDF_CloseDocument(raw);
            Failure::new(FailureKind::Engine, msg)
        };
        if page.is_null() {
            return Err(fail("creating the image page failed"));
        }
        let stride = iw.saturating_mul(4);
        let bitmap = api.FPDFBitmap_CreateEx(
            iw as c_int,
            ih as c_int,
            FPDF_BITMAP_BGRA,
            bgra.as_mut_ptr().cast(),
            stride as c_int,
        );
        if bitmap.is_null() {
            api.FPDF_ClosePage(page);
            return Err(fail("allocating the image failed"));
        }
        let obj = api.FPDFPageObj_NewImageObj(raw);
        let matrix = FS_MATRIX {
            a: pw as f32,
            b: 0.0,
            c: 0.0,
            d: ph as f32,
            e: 0.0,
            f: 0.0,
        };
        let mut pages = [page];
        let ok = !obj.is_null()
            && api.FPDFImageObj_SetBitmap(pages.as_mut_ptr(), 1, obj, bitmap) != 0
            && api.FPDFPageObj_SetMatrix(obj, &raw const matrix) != 0;
        api.FPDFBitmap_Destroy(bitmap);
        if !ok {
            api.FPDF_ClosePage(page);
            return Err(fail("placing the image failed"));
        }
        api.FPDFPage_InsertObject(page, obj);
        let generated = api.FPDFPage_GenerateContent(page) != 0;
        api.FPDF_ClosePage(page);
        if !generated {
            return Err(fail("writing the image page failed"));
        }
        Ok(raw)
    }

    /// Close a document; unknown ids are ignored.
    pub fn close(&mut self, id: u64) {
        if let Some(doc) = self.docs.remove(&id) {
            self.close_raw(doc.raw, doc.form);
            drop(doc.backing);
        }
    }

    /// Output size of a render, validated against the pixel budget.
    ///
    /// # Errors
    ///
    /// Unknown document or page, or a render over the budget.
    pub fn render_size(&self, id: u64, page: usize, options: &RenderOptions) -> Result<(u32, u32)> {
        let doc = self.doc(id)?;
        let geometry = doc
            .pages
            .get(page)
            .ok_or_else(|| Failure::new(FailureKind::Invalid, "page out of range"))?;
        if !(options.scale.is_finite() && options.scale > 0.0) {
            return Err(Failure::new(FailureKind::Invalid, "invalid render scale"));
        }
        let (full_w, full_h) = options.page_pixels(geometry.size());
        let (w, h) = options.output_pixels(geometry.size());
        if let Some(region) = options.region
            && (region.x.saturating_add(region.width) > full_w
                || region.y.saturating_add(region.height) > full_h)
        {
            return Err(Failure::new(
                FailureKind::Invalid,
                "render region outside the page",
            ));
        }
        if w == 0 || h == 0 {
            return Err(Failure::new(FailureKind::Invalid, "empty render"));
        }
        if u64::from(w).saturating_mul(u64::from(h)) > MAX_RENDER_PIXELS
            || full_w > i32::MAX as u32 / 4
            || full_h > i32::MAX as u32 / 4
        {
            return Err(Failure::new(
                FailureKind::Limit,
                "the render exceeds the pixel budget",
            ));
        }
        Ok((w, h))
    }

    /// Render into `out`, which must hold exactly `w * h * 4` bytes for the
    /// size [`Self::render_size`] returned. Pixels are RGBA, opaque, on
    /// white.
    ///
    /// # Errors
    ///
    /// As [`Self::render_size`], or a wrongly sized buffer.
    pub fn render_into(
        &self,
        id: u64,
        page: usize,
        options: &RenderOptions,
        out: &mut [u8],
    ) -> Result<()> {
        let (w, h) = self.render_size(id, page, options)?;
        let expected = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if out.len() != expected {
            return Err(Failure::new(
                FailureKind::Invalid,
                "render buffer size mismatch",
            ));
        }
        let doc = self.doc(id)?;
        let geometry = doc
            .pages
            .get(page)
            .ok_or_else(|| Failure::new(FailureKind::Invalid, "page out of range"))?;
        let (full_w, full_h) = options.page_pixels(geometry.size());
        let (ox, oy) = options.region.map_or((0, 0), |r| (r.x, r.y));
        let guard = self.load_page(doc, page, options.annotations)?;
        let api = self.api();
        let bitmap: FPDF_BITMAP = api.FPDFBitmap_CreateEx(
            w as c_int,
            h as c_int,
            FPDF_BITMAP_BGRA,
            out.as_mut_ptr().cast(),
            w.saturating_mul(4) as c_int,
        );
        if bitmap.is_null() {
            return Err(Failure::new(
                FailureKind::Engine,
                "allocating the bitmap failed",
            ));
        }
        api.FPDFBitmap_FillRect(bitmap, 0, 0, w as c_int, h as c_int, 0xFFFF_FFFF);
        let mut flags = FPDF_REVERSE_BYTE_ORDER;
        if options.annotations {
            flags |= FPDF_ANNOT;
        }
        let rotate = c_int::from(options.rotation.quarter_turns());
        let (start_x, start_y) = (
            0i32.saturating_sub(ox as c_int),
            0i32.saturating_sub(oy as c_int),
        );
        // Reader markup is hidden for this render only: the flags are put
        // back before the page is closed, so exports never see the change.
        let hidden = if options.annotations && !options.reader_markup {
            self.hide_reader_markup(guard.raw)
        } else {
            Vec::new()
        };
        api.FPDF_RenderPageBitmap(
            bitmap,
            guard.raw,
            start_x,
            start_y,
            full_w as c_int,
            full_h as c_int,
            rotate,
            flags,
        );
        if let Some(form) = guard.form {
            api.FPDF_FFLDraw(
                form,
                bitmap,
                guard.raw,
                start_x,
                start_y,
                full_w as c_int,
                full_h as c_int,
                rotate,
                flags,
            );
        }
        api.FPDFBitmap_Destroy(bitmap);
        self.restore_flags(guard.raw, &hidden);
        drop(guard);
        Ok(())
    }

    /// Set the Hidden flag on every reader-markup annotation of `page`;
    /// returns what to restore.
    fn hide_reader_markup(&self, page: FPDF_PAGE) -> Vec<(c_int, c_int)> {
        let api = self.api();
        let count = api.FPDFPage_GetAnnotCount(page).clamp(0, 10_000);
        let mut changed = Vec::new();
        for i in 0..count {
            let annot = api.FPDFPage_GetAnnot(page, i);
            if annot.is_null() {
                continue;
            }
            if Self::annotation_kind(api.FPDFAnnot_GetSubtype(annot)).is_reader_markup() {
                let flags = api.FPDFAnnot_GetFlags(annot);
                if api.FPDFAnnot_SetFlags(annot, flags | ANNOT_FLAG_HIDDEN) != 0 {
                    changed.push((i, flags));
                }
            }
            api.FPDFPage_CloseAnnot(annot);
        }
        changed
    }

    fn restore_flags(&self, page: FPDF_PAGE, changed: &[(c_int, c_int)]) {
        let api = self.api();
        for &(i, flags) in changed {
            let annot = api.FPDFPage_GetAnnot(page, i);
            if !annot.is_null() {
                api.FPDFAnnot_SetFlags(annot, flags);
                api.FPDFPage_CloseAnnot(annot);
            }
        }
    }

    /// The characters of a page in engine order, in the page's display
    /// space before its own rotation.
    ///
    /// # Errors
    ///
    /// Unknown document or page, or a page over [`MAX_PAGE_CHARS`].
    pub fn text(&self, id: u64, page: usize) -> Result<Vec<RawChar>> {
        let doc = self.doc(id)?;
        // Characters are reported before the page's own rotation, where its
        // lines run horizontally; the text layer maps them to display space.
        let geometry = PageGeometry {
            rotation: Rotation::None,
            ..*doc
                .pages
                .get(page)
                .ok_or_else(|| Failure::new(FailureKind::Invalid, "page out of range"))?
        };
        let guard = self.load_page(doc, page, false)?;
        let api = self.api();
        let text = api.FPDFText_LoadPage(guard.raw);
        if text.is_null() {
            return Ok(Vec::new());
        }
        let count = usize::try_from(api.FPDFText_CountChars(text)).unwrap_or(0);
        if count > MAX_PAGE_CHARS {
            api.FPDFText_ClosePage(text);
            return Err(Failure::new(
                FailureKind::Limit,
                "the page has too much text",
            ));
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let index = i as c_int;
            let unicode = api.FPDFText_GetUnicode(text, index);
            let mut flags = 0u8;
            if api.FPDFText_IsGenerated(text, index) == 1 {
                flags |= raw_flags::GENERATED;
            }
            if api.FPDFText_IsHyphen(text, index) == 1 {
                flags |= raw_flags::HYPHEN;
            }
            if api.FPDFText_HasUnicodeMapError(text, index) == 1 {
                flags |= raw_flags::MAP_ERROR;
            }
            let (mut l, mut r, mut b, mut t) = (0f64, 0f64, 0f64, 0f64);
            let bbox = if api
                .FPDFText_GetCharBox(text, index, &raw mut l, &raw mut r, &raw mut b, &raw mut t)
                != 0
            {
                geometry.rect_to_display(Rect::from_corners(
                    Point::new(l as f32, b as f32),
                    Point::new(r as f32, t as f32),
                ))
            } else {
                Rect::default()
            };
            let mut loose_rect = FS_RECTF {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            };
            let loose = if api.FPDFText_GetLooseCharBox(text, index, &raw mut loose_rect) != 0 {
                geometry.rect_to_display(fs_rect(&loose_rect))
            } else {
                Rect::default()
            };
            out.push(RawChar {
                unicode,
                index: i as u32,
                bbox,
                loose,
                font_size: api.FPDFText_GetFontSize(text, index) as f32,
                flags,
            });
        }
        api.FPDFText_ClosePage(text);
        drop(guard);
        Ok(out)
    }

    fn destination(&self, doc: &Doc, dest: FPDF_DEST) -> Option<Destination> {
        if dest.is_null() {
            return None;
        }
        let api = self.api();
        let page = usize::try_from(api.FPDFDest_GetDestPageIndex(doc.raw, dest)).ok()?;
        let geometry = doc.pages.get(page)?;
        let (mut has_x, mut has_y, mut has_zoom) = (0, 0, 0);
        let (mut x, mut y, mut zoom) = (0f32, 0f32, 0f32);
        let at = (api.FPDFDest_GetLocationInPage(
            dest,
            &raw mut has_x,
            &raw mut has_y,
            &raw mut has_zoom,
            &raw mut x,
            &raw mut y,
            &raw mut zoom,
        ) != 0
            && (has_x != 0 || has_y != 0))
            .then(|| {
                let ux = if has_x != 0 { x } else { geometry.user_box.x0 };
                let uy = if has_y != 0 { y } else { geometry.user_box.y1 };
                geometry.to_display(Point::new(ux, uy))
            });
        Some(Destination::Page { page, at })
    }

    fn action_destination(&self, doc: &Doc, action: FPDF_ACTION) -> Option<Destination> {
        if action.is_null() {
            return None;
        }
        let api = self.api();
        match api.FPDFAction_GetType(action) {
            PDFACTION_GOTO => self.destination(doc, api.FPDFAction_GetDest(doc.raw, action)),
            PDFACTION_URI => {
                let uri =
                    read_bytes(|buf, len| api.FPDFAction_GetURIPath(doc.raw, action, buf, len));
                (!uri.is_empty()).then_some(Destination::Uri(uri))
            }
            PDFACTION_REMOTEGOTO | PDFACTION_LAUNCH => {
                let path = read_bytes(|buf, len| api.FPDFAction_GetFilePath(action, buf, len));
                (!path.is_empty()).then_some(Destination::File(path))
            }
            _ => None,
        }
    }

    /// Link areas of a page.
    ///
    /// # Errors
    ///
    /// Unknown document or page.
    pub fn links(&self, id: u64, page: usize) -> Result<Vec<Link>> {
        const MAX_LINKS: usize = 10_000;
        let doc = self.doc(id)?;
        let geometry = *doc
            .pages
            .get(page)
            .ok_or_else(|| Failure::new(FailureKind::Invalid, "page out of range"))?;
        let guard = self.load_page(doc, page, false)?;
        let api = self.api();
        let mut out = Vec::new();
        let mut pos: c_int = 0;
        let mut link = null_mut();
        while out.len() < MAX_LINKS
            && api.FPDFLink_Enumerate(guard.raw, &raw mut pos, &raw mut link) != 0
        {
            let mut rect = FS_RECTF {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            };
            if api.FPDFLink_GetAnnotRect(link, &raw mut rect) == 0 {
                continue;
            }
            let destination = self
                .destination(doc, api.FPDFLink_GetDest(doc.raw, link))
                .or_else(|| self.action_destination(doc, api.FPDFLink_GetAction(link)));
            if let Some(destination) = destination {
                out.push(Link {
                    rect: geometry.rect_to_display(fs_rect(&rect)),
                    destination,
                });
            }
        }
        drop(guard);
        Ok(out)
    }

    /// The document outline.
    ///
    /// # Errors
    ///
    /// Unknown document.
    pub fn outline(&self, id: u64) -> Result<Vec<OutlineEntry>> {
        const MAX_ENTRIES: usize = 10_000;
        let doc = self.doc(id)?;
        let mut seen: HashSet<usize> = HashSet::new();
        let mut budget = MAX_ENTRIES;
        Ok(walk(self, doc, null_mut(), 0, &mut seen, &mut budget))
    }

    fn annotation_kind(sub: c_int) -> AnnotationKind {
        match sub {
            subtype::TEXT => AnnotationKind::Text,
            subtype::LINK => AnnotationKind::Link,
            subtype::FREETEXT => AnnotationKind::FreeText,
            subtype::LINE => AnnotationKind::Line,
            subtype::SQUARE => AnnotationKind::Square,
            subtype::CIRCLE => AnnotationKind::Circle,
            subtype::POLYGON => AnnotationKind::Polygon,
            subtype::POLYLINE => AnnotationKind::Polyline,
            subtype::HIGHLIGHT => AnnotationKind::Highlight,
            subtype::UNDERLINE => AnnotationKind::Underline,
            subtype::SQUIGGLY => AnnotationKind::Squiggly,
            subtype::STRIKEOUT => AnnotationKind::StrikeOut,
            subtype::STAMP => AnnotationKind::Stamp,
            subtype::CARET => AnnotationKind::Caret,
            subtype::INK => AnnotationKind::Ink,
            subtype::POPUP => AnnotationKind::Popup,
            subtype::FILEATTACHMENT => AnnotationKind::FileAttachment,
            subtype::WIDGET => AnnotationKind::Widget,
            subtype::REDACT => AnnotationKind::Redact,
            other => AnnotationKind::Other(format!("subtype {other}")),
        }
    }

    /// Each annotation's `/C` colour on `page`, by position, read from the
    /// file's own objects. pdfium reports no colour for an annotation that
    /// has an appearance stream — which is nearly every annotation a reader
    /// wrote — so this is where a taken-over annotation's colour comes from.
    fn annotation_colors(doc: &Doc, page: usize) -> Vec<Option<Rgba>> {
        let Some(pdf) = doc
            .structure
            .get_or_init(|| match &doc.backing {
                Backing::Mapped(map) => hayro_syntax::Pdf::new_with_password(
                    std::sync::Arc::clone(map),
                    doc.password.as_deref().unwrap_or(""),
                )
                .ok(),
                Backing::None => None,
            })
            .as_ref()
        else {
            return Vec::new();
        };
        let Some(page) = pdf.pages().get(page) else {
            return Vec::new();
        };
        let Some(annots) = page.raw().get::<hayro_syntax::object::Array<'_>>(b"Annots") else {
            return Vec::new();
        };
        let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        annots
            .iter::<hayro_syntax::object::Dict<'_>>()
            .take(10_000)
            .map(|annot| {
                let c: Vec<f32> = annot
                    .get::<hayro_syntax::object::Array<'_>>(b"C")?
                    .iter::<f32>()
                    .collect();
                match c.as_slice() {
                    [g] => Some(Rgba::new(channel(*g), channel(*g), channel(*g), 255)),
                    [r, g, b] => Some(Rgba::new(channel(*r), channel(*g), channel(*b), 255)),
                    [c, m, y, k] => {
                        let to = |v: f32| channel((1.0 - v) * (1.0 - k));
                        Some(Rgba::new(to(*c), to(*m), to(*y), 255))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    fn annot_string(&self, annot: FPDF_ANNOTATION, key: &str) -> String {
        let api = self.api();
        read_wide(|buf, len| api.FPDFAnnot_GetStringValue(annot, key, buf, len))
    }

    /// Annotations present on a page, in display space.
    ///
    /// # Errors
    ///
    /// Unknown document or page.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass reading every property of each annotation"
    )]
    pub fn annotations(&self, id: u64, page: usize) -> Result<Vec<AnnotationInfo>> {
        const MAX_ANNOTS: usize = 10_000;
        let doc = self.doc(id)?;
        let geometry = *doc
            .pages
            .get(page)
            .ok_or_else(|| Failure::new(FailureKind::Invalid, "page out of range"))?;
        let guard = self.load_page(doc, page, false)?;
        let api = self.api();
        let count = usize::try_from(api.FPDFPage_GetAnnotCount(guard.raw))
            .unwrap_or(0)
            .min(MAX_ANNOTS);
        let dict_colors = Self::annotation_colors(doc, page);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let annot = api.FPDFPage_GetAnnot(guard.raw, i as c_int);
            if annot.is_null() {
                continue;
            }
            let kind = Self::annotation_kind(api.FPDFAnnot_GetSubtype(annot));
            let mut rect = FS_RECTF {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            };
            let rect = if api.FPDFAnnot_GetRect(annot, &raw mut rect) != 0 {
                geometry.rect_to_display(fs_rect(&rect))
            } else {
                Rect::default()
            };
            let mut rgba = [0u32; 4];
            let [red, green, blue, alpha] = &mut rgba;
            let color = (api.FPDFAnnot_GetColor(annot, COLORTYPE_COLOR, red, green, blue, alpha)
                != 0)
                .then(|| {
                    let [red, green, blue, alpha] = rgba.map(|c| c.min(255) as u8);
                    Rgba::new(red, green, blue, alpha)
                })
                .or_else(|| dict_colors.get(i).copied().flatten());
            let mut quads = Vec::new();
            let quad_count = api.FPDFAnnot_CountAttachmentPoints(annot).min(4096);
            for q in 0..quad_count {
                let mut qp = FS_QUADPOINTSF {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 0.0,
                    y2: 0.0,
                    x3: 0.0,
                    y3: 0.0,
                    x4: 0.0,
                    y4: 0.0,
                };
                if api.FPDFAnnot_GetAttachmentPoints(annot, q, &raw mut qp) != 0 {
                    let xs = [qp.x1, qp.x2, qp.x3, qp.x4];
                    let ys = [qp.y1, qp.y2, qp.y3, qp.y4];
                    let user = Rect {
                        x0: xs.iter().copied().fold(f32::INFINITY, f32::min),
                        y0: ys.iter().copied().fold(f32::INFINITY, f32::min),
                        x1: xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                        y1: ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                    };
                    quads.push(geometry.rect_to_display(user));
                }
            }
            let mut ink = Vec::new();
            let paths = api.FPDFAnnot_GetInkListCount(annot).min(4096);
            for p in 0..paths {
                let len = api
                    .FPDFAnnot_GetInkListPath(annot, p, null_mut(), 0)
                    .min(1 << 20);
                if len == 0 {
                    continue;
                }
                let mut points = vec![FS_POINTF { x: 0.0, y: 0.0 }; len as usize];
                let got = api.FPDFAnnot_GetInkListPath(annot, p, points.as_mut_ptr(), len);
                ink.push(
                    points
                        .iter()
                        .take(got as usize)
                        .map(|pt| geometry.to_display(Point::new(pt.x, pt.y)))
                        .collect(),
                );
            }
            let (mut hr, mut vr, mut width) = (0.0f32, 0.0f32, 0.0f32);
            let border_width = (api.FPDFAnnot_HasKey(annot, "Border") != 0
                && api.FPDFAnnot_GetBorder(annot, &raw mut hr, &raw mut vr, &raw mut width) != 0
                && width.is_finite())
            .then_some(width);
            let linked_index = |key: &str| {
                let linked = api.FPDFAnnot_GetLinkedAnnot(annot, key);
                if linked.is_null() {
                    return None;
                }
                let index = usize::try_from(api.FPDFPage_GetAnnotIndex(guard.raw, linked)).ok();
                api.FPDFPage_CloseAnnot(linked);
                index
            };
            let in_reply_to = linked_index("IRT");
            let popup = linked_index("Popup");
            let reply_type = if self.annot_string(annot, "RT") == "Group" {
                ReplyType::Group
            } else {
                ReplyType::Reply
            };
            let (font_size, text_color) = parse_default_appearance(&self.annot_string(annot, "DA"));
            out.push(AnnotationInfo {
                index: i,
                kind,
                rect,
                color,
                contents: self.annot_string(annot, "Contents"),
                author: self.annot_string(annot, "T"),
                name: self.annot_string(annot, "NM"),
                modified: self.annot_string(annot, "M"),
                created: self.annot_string(annot, "CreationDate"),
                flags: u32::try_from(api.FPDFAnnot_GetFlags(annot)).unwrap_or(0),
                quads,
                ink,
                border_width,
                in_reply_to,
                reply_type,
                popup,
                font_size,
                text_color,
            });
            api.FPDFPage_CloseAnnot(annot);
        }
        drop(guard);
        Ok(out)
    }

    /// The document's form fields with their widgets, grouped by name.
    ///
    /// # Errors
    ///
    /// Unknown document.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass reading every widget property"
    )]
    pub fn form_fields(&self, id: u64) -> Result<Vec<FormField>> {
        const MAX_WIDGETS: usize = 20_000;
        let doc = self.doc(id)?;
        let Some(form) = doc.form.as_ref().map(|f| f.handle) else {
            return Ok(Vec::new());
        };
        let api = self.api();
        let mut fields: Vec<FormField> = Vec::new();
        let mut by_name: HashMap<String, usize> = HashMap::new();
        let mut widgets = 0usize;
        for page in 0..doc.pages.len() {
            let geometry = doc.pages.get(page).copied().unwrap_or_default();
            let guard = self.load_page(doc, page, true)?;
            let count = usize::try_from(api.FPDFPage_GetAnnotCount(guard.raw)).unwrap_or(0);
            for i in 0..count {
                if widgets >= MAX_WIDGETS {
                    break;
                }
                let annot = api.FPDFPage_GetAnnot(guard.raw, i as c_int);
                if annot.is_null() {
                    continue;
                }
                if api.FPDFAnnot_GetSubtype(annot) != subtype::WIDGET {
                    api.FPDFPage_CloseAnnot(annot);
                    continue;
                }
                widgets = widgets.saturating_add(1);
                let name =
                    read_wide(|buf, len| api.FPDFAnnot_GetFormFieldName(form, annot, buf, len));
                let kind = match api.FPDFAnnot_GetFormFieldType(form, annot) {
                    field_type::TEXTFIELD => FieldKind::Text,
                    field_type::CHECKBOX => FieldKind::CheckBox,
                    field_type::RADIOBUTTON => FieldKind::RadioButton,
                    field_type::COMBOBOX => FieldKind::ComboBox,
                    field_type::LISTBOX => FieldKind::ListBox,
                    field_type::PUSHBUTTON => FieldKind::PushButton,
                    field_type::SIGNATURE => FieldKind::Signature,
                    _ => FieldKind::Unknown,
                };
                let flags = api.FPDFAnnot_GetFormFieldFlags(form, annot);
                let mut rect = FS_RECTF {
                    left: 0.0,
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                };
                let rect = if api.FPDFAnnot_GetRect(annot, &raw mut rect) != 0 {
                    geometry.rect_to_display(fs_rect(&rect))
                } else {
                    Rect::default()
                };
                let checked = matches!(kind, FieldKind::CheckBox | FieldKind::RadioButton)
                    && api.FPDFAnnot_IsChecked(form, annot) != 0;
                let export_value = if matches!(kind, FieldKind::CheckBox | FieldKind::RadioButton) {
                    read_wide(|buf, len| {
                        api.FPDFAnnot_GetFormFieldExportValue(form, annot, buf, len)
                    })
                } else {
                    String::new()
                };
                let widget = FieldWidget {
                    page,
                    rect,
                    export_value,
                    checked,
                };
                if let Some(&idx) = by_name.get(&name) {
                    if let Some(field) = fields.get_mut(idx) {
                        field.widgets.push(widget);
                    }
                } else {
                    let options = if matches!(kind, FieldKind::ComboBox | FieldKind::ListBox) {
                        let n = api.FPDFAnnot_GetOptionCount(form, annot).clamp(0, 10_000);
                        (0..n)
                            .map(|o| {
                                read_wide(|buf, len| {
                                    api.FPDFAnnot_GetOptionLabel(form, annot, o, buf, len)
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    by_name.insert(name.clone(), fields.len());
                    fields.push(FormField {
                        value: read_wide(|buf, len| {
                            api.FPDFAnnot_GetFormFieldValue(form, annot, buf, len)
                        }),
                        name,
                        kind,
                        options,
                        read_only: flags & FIELD_FLAG_READONLY != 0,
                        required: flags & FIELD_FLAG_REQUIRED != 0,
                        multiline: kind == FieldKind::Text
                            && flags & FIELD_FLAG_TEXT_MULTILINE != 0,
                        widgets: vec![widget],
                    });
                }
                api.FPDFPage_CloseAnnot(annot);
            }
            drop(guard);
        }
        Ok(fields)
    }

    /// Materialise `edits` against a fresh copy of document `id` and return
    /// the new PDF. Pages come from `id` and the `imports`; annotations
    /// become standard annotation objects with generated appearances, form
    /// values become real field values. The result is an incremental update
    /// of the source bytes and is deterministic for the same inputs.
    ///
    /// # Errors
    ///
    /// Unknown documents, out-of-range references, or engine failures.
    pub fn export(&self, id: u64, edits: &Edits, imports: &[u64]) -> Result<Vec<u8>> {
        let source = self.doc(id)?;
        let Backing::Mapped(map) = &source.backing else {
            return Err(Failure::new(
                FailureKind::Invalid,
                "only documents opened from PDF bytes can be exported",
            ));
        };
        let bytes: &[u8] = map;
        let api = self.api();
        let copy = api.FPDF_LoadMemDocument64(bytes, source.password.as_deref());
        if copy.is_null() {
            return Err(self.load_failure());
        }
        let form = if edits.form_values.is_empty() {
            None
        } else {
            self.init_form(copy)
        };
        // An encrypted source keeps its encryption in an incremental save,
        // so the patch pass (which writes plain objects) cannot run there:
        // stamps then get pdfium-built appearances and replies no links.
        let patchable = api.FPDF_GetSecurityHandlerRevision(copy) < 0;
        let mut plan = crate::patch::PatchPlan::default();
        let result = self
            .apply_edits(
                source,
                copy,
                form.as_ref(),
                edits,
                imports,
                patchable.then_some(&mut plan),
            )
            .and_then(|()| {
                let mut out = Vec::with_capacity(bytes.len().saturating_add(64 * 1024));
                let mut writer = VecWriter {
                    base: FPDF_FILEWRITE {
                        version: 1,
                        WriteBlock: Some(write_block),
                    },
                    out: &raw mut out,
                };
                let flags = if edits.is_empty() {
                    FPDF_NO_INCREMENTAL
                } else {
                    FPDF_INCREMENTAL
                };
                let ok = api.FPDF_SaveAsCopy(copy, (&raw mut writer).cast(), flags) != 0;
                if !ok {
                    return Err(Failure::new(
                        FailureKind::Engine,
                        "saving the document failed",
                    ));
                }
                let seed = export_seed(bytes, edits);
                normalize_trailer_ids(&mut out, bytes, &seed);
                crate::patch::apply(&out, plan).map_err(|declined| {
                    Failure::new(
                        FailureKind::Engine,
                        format!("finishing the annotations failed: {}", declined.0),
                    )
                })
            });
        self.close_raw(copy, form);
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one linear pass over pages, annotations and fields"
    )]
    fn apply_edits(
        &self,
        source: &Doc,
        copy: FPDF_DOCUMENT,
        form: Option<&Form>,
        edits: &Edits,
        imports: &[u64],
        mut patching: Option<&mut crate::patch::PatchPlan>,
    ) -> Result<()> {
        let api = self.api();
        let original_count = source.pages.len();
        if edits
            .adopted
            .iter()
            .any(|a| a.target.page >= original_count)
        {
            return Err(Failure::new(
                FailureKind::Invalid,
                "adopted annotation page out of range",
            ));
        }
        // Source geometry for every output page, to validate references.
        let mut output_rotations: Vec<(Rotation, Rotation)> = Vec::new();
        if !edits.pages.is_empty() {
            // Every output page must end up as a distinct page object: the
            // first use of an original page keeps it, later uses and
            // imports are appended as copies, then everything is moved into
            // order and the unused originals are deleted.
            let mut used = vec![false; original_count];
            let mut current: Vec<c_int> = Vec::with_capacity(edits.pages.len());
            let mut count = original_count;
            for edit in &edits.pages {
                let (index, own) = match edit.source {
                    PageSource::Original(i) => {
                        let own = source
                            .pages
                            .get(i)
                            .ok_or_else(|| {
                                Failure::new(FailureKind::Invalid, "page edit out of range")
                            })?
                            .rotation;
                        if used.get(i).copied() == Some(false) {
                            if let Some(u) = used.get_mut(i) {
                                *u = true;
                            }
                            (i, own)
                        } else {
                            let at = count as c_int;
                            if api.FPDF_ImportPagesByIndex(copy, source.raw, &(i as c_int), 1, at)
                                == 0
                            {
                                return Err(Failure::new(
                                    FailureKind::Engine,
                                    "duplicating a page failed",
                                ));
                            }
                            count = count.saturating_add(1);
                            (count.saturating_sub(1), own)
                        }
                    }
                    PageSource::Imported { import, page } => {
                        let doc_id = imports.get(import).ok_or_else(|| {
                            Failure::new(FailureKind::Invalid, "import out of range")
                        })?;
                        let other = self.doc(*doc_id)?;
                        let own = other
                            .pages
                            .get(page)
                            .ok_or_else(|| {
                                Failure::new(FailureKind::Invalid, "imported page out of range")
                            })?
                            .rotation;
                        let at = count as c_int;
                        if api.FPDF_ImportPagesByIndex(copy, other.raw, &(page as c_int), 1, at)
                            == 0
                        {
                            return Err(Failure::new(
                                FailureKind::Engine,
                                "importing a page failed",
                            ));
                        }
                        count = count.saturating_add(1);
                        (count.saturating_sub(1), own)
                    }
                };
                current.push(index as c_int);
                output_rotations.push((own, edit.rotation));
            }
            let identity = current
                .iter()
                .enumerate()
                .all(|(k, &i)| usize::try_from(i).ok() == Some(k))
                && current.len() == count;
            if !identity {
                if api.FPDF_MovePages(copy, current.as_ptr(), current.len() as c_ulong, 0) == 0 {
                    return Err(Failure::new(FailureKind::Engine, "reordering pages failed"));
                }
                let keep = current.len();
                let total = usize::try_from(api.FPDF_GetPageCount(copy)).unwrap_or(0);
                for index in (keep..total).rev() {
                    api.FPDFPage_Delete(copy, index as c_int);
                }
            }
            for (k, (own, extra)) in output_rotations.iter().enumerate() {
                if *extra == Rotation::None {
                    continue;
                }
                let page = api.FPDF_LoadPage(copy, k as c_int);
                if page.is_null() {
                    return Err(Failure::new(FailureKind::Engine, "rotating a page failed"));
                }
                api.FPDFPage_SetRotation(page, c_int::from(own.then(*extra).quarter_turns()));
                api.FPDF_ClosePage(page);
            }
        }
        let output_count = usize::try_from(api.FPDF_GetPageCount(copy)).unwrap_or(0);
        if !edits.adopted.is_empty() {
            // Which original page each output page shows, and whether it is
            // the occurrence that keeps that page's adopted annotations.
            let occurrences: Vec<Option<(usize, bool)>> = if edits.pages.is_empty() {
                (0..output_count).map(|i| Some((i, true))).collect()
            } else {
                edits
                    .pages
                    .iter()
                    .map(|p| match p.source {
                        PageSource::Original(i) => Some((i, p.primary)),
                        PageSource::Imported { .. } => None,
                    })
                    .collect()
            };
            for (k, occurrence) in occurrences.iter().enumerate() {
                let Some((original, primary)) = *occurrence else {
                    continue;
                };
                let targets: Vec<&AdoptedAnnotation> = edits
                    .adopted
                    .iter()
                    .filter(|a| a.target.page == original && (!primary || !a.unchanged))
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                let page = api.FPDF_LoadPage(copy, k as c_int);
                if page.is_null() {
                    return Err(Failure::new(
                        FailureKind::Engine,
                        "loading a page to replace annotations failed",
                    ));
                }
                self.remove_adopted(page, &targets);
                api.FPDF_ClosePage(page);
            }
        }
        // Annotations the patch pass must find again: those it gives an
        // appearance, replies, and the annotations replied to.
        let by_name: HashMap<&str, usize> = edits
            .annotations
            .iter()
            .enumerate()
            .rev()
            .map(|(k, a)| (a.name.as_str(), k))
            .collect();
        let reply_parent = |a: &NewAnnotation| -> Option<usize> {
            let parent = *by_name.get(a.in_reply_to.as_deref()?)?;
            (edits.annotations.get(parent)?.page == a.page).then_some(parent)
        };
        let replied_to: HashSet<usize> =
            edits.annotations.iter().filter_map(reply_parent).collect();
        let mut annotated: Vec<usize> = Vec::new();
        // Each output page's new annotations' text, in creation order.
        let mut texts: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
        for (k, annotation) in edits.annotations.iter().enumerate() {
            if annotation.page >= output_count {
                return Err(Failure::new(
                    FailureKind::Invalid,
                    "annotation page out of range",
                ));
            }
            let parent = reply_parent(annotation);
            let finished_later = patching.is_some()
                && (matches!(
                    annotation.kind,
                    NewAnnotationKind::FreeText { .. } | NewAnnotationKind::Stamp { .. }
                ) || parent.is_some()
                    || replied_to.contains(&k));
            let page = api.FPDF_LoadPage(copy, annotation.page as c_int);
            if page.is_null() {
                return Err(Failure::new(
                    FailureKind::Engine,
                    "loading a page to annotate failed",
                ));
            }
            let added = self.add_annotation(copy, page, annotation, finished_later);
            api.FPDF_ClosePage(page);
            added?;
            if let Some(plan) = patching.as_deref_mut() {
                // pdfium appends each new annotation to its page's /Annots,
                // after every removal above: its position there is how the
                // patch pass finds it again.
                let ordinal = plan.created.entry(annotation.page).or_insert(0);
                let this = *ordinal;
                *ordinal = ordinal.saturating_add(1);
                if finished_later {
                    let appearance = match &annotation.kind {
                        NewAnnotationKind::FreeText { rect, font_size } => {
                            Some(crate::appearance::free_text(
                                &mut plan.setter,
                                *rect,
                                annotation.color,
                                *font_size,
                                &annotation.contents,
                            ))
                        }
                        NewAnnotationKind::Stamp {
                            rect,
                            strokes,
                            stroke_width,
                            text,
                        } => Some(crate::appearance::stamp(
                            &mut plan.setter,
                            *rect,
                            annotation.color,
                            strokes,
                            *stroke_width,
                            text,
                        )),
                        _ => None,
                    };
                    plan.patches.push(crate::patch::AnnotPatch {
                        key: k,
                        page: annotation.page,
                        ordinal: this,
                        name: annotation.name.clone(),
                        appearance,
                        reply_to: parent.map(|p| (p, annotation.reply_type)),
                    });
                }
            }
            let on_page = texts.entry(annotation.page).or_default();
            on_page.push(annotation.contents.as_str());
            if !annotated.contains(&annotation.page) {
                annotated.push(annotation.page);
            }
        }
        // pdfium writes appearance streams for markup, ink and note
        // annotations (and makes new annotations indirect objects) when it
        // first lays out a page's annotation list; rendering each annotated
        // page once makes it do so before saving, so other readers draw
        // exactly what pdfium draws. The text is set only afterwards: that
        // layout also builds pop-up windows for annotations with text, whose
        // appearances would otherwise be saved as unused objects.
        for index in annotated {
            let page = api.FPDF_LoadPage(copy, index as c_int);
            if page.is_null() {
                continue;
            }
            let mut pixel = [0u8; 4];
            let bitmap =
                api.FPDFBitmap_CreateEx(1, 1, FPDF_BITMAP_BGRA, pixel.as_mut_ptr().cast(), 4);
            if !bitmap.is_null() {
                api.FPDF_RenderPageBitmap(bitmap, page, 0, 0, 1, 1, 0, FPDF_ANNOT);
                api.FPDFBitmap_Destroy(bitmap);
            }
            if let Some(contents) = texts.get(&index) {
                let count = usize::try_from(api.FPDFPage_GetAnnotCount(page)).unwrap_or(0);
                let first = count.saturating_sub(contents.len());
                for (ordinal, text) in contents.iter().enumerate() {
                    if text.is_empty() {
                        continue;
                    }
                    let annot = api.FPDFPage_GetAnnot(page, first.saturating_add(ordinal) as c_int);
                    if annot.is_null() {
                        continue;
                    }
                    let w = wide(text);
                    api.FPDFAnnot_SetStringValue(annot, "Contents", w.as_ptr());
                    api.FPDFPage_CloseAnnot(annot);
                }
            }
            api.FPDF_ClosePage(page);
        }
        if let Some(form) = form {
            self.apply_form_values(copy, form.handle, edits);
        } else if !edits.form_values.is_empty() {
            return Err(Failure::new(
                FailureKind::Invalid,
                "the document has no form",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, reason = "one arm per annotation kind")]
    /// Create `a` on `page`. When the patch pass will finish it
    /// (appearance, reply link) nothing more is drawn here; otherwise a
    /// stamp gets an appearance built from pdfium page objects.
    fn add_annotation(
        &self,
        doc: FPDF_DOCUMENT,
        page: FPDF_PAGE,
        a: &NewAnnotation,
        finished_later: bool,
    ) -> Result<()> {
        let api = self.api();
        let (sub, quads) = match &a.kind {
            NewAnnotationKind::Highlight { quads } => (subtype::HIGHLIGHT, Some(quads)),
            NewAnnotationKind::Underline { quads } => (subtype::UNDERLINE, Some(quads)),
            NewAnnotationKind::StrikeOut { quads } => (subtype::STRIKEOUT, Some(quads)),
            NewAnnotationKind::Squiggly { quads } => (subtype::SQUIGGLY, Some(quads)),
            NewAnnotationKind::Note { .. } => (subtype::TEXT, None),
            NewAnnotationKind::Ink { .. } => (subtype::INK, None),
            NewAnnotationKind::FreeText { .. } => (subtype::FREETEXT, None),
            NewAnnotationKind::Stamp { .. } => (subtype::STAMP, None),
        };
        let annot = api.FPDFPage_CreateAnnot(page, sub);
        if annot.is_null() {
            return Err(Failure::new(
                FailureKind::Engine,
                "creating an annotation failed",
            ));
        }
        let fail = |msg: &str| {
            api.FPDFPage_CloseAnnot(annot);
            Err(Failure::new(FailureKind::Engine, msg))
        };
        let mut bounds: Option<Rect> = None;
        if let Some(quads) = quads {
            for q in quads {
                let r = Rect::from_corners(Point::new(q.x0, q.y0), Point::new(q.x1, q.y1));
                // Acrobat order: upper-left, upper-right, lower-left, lower-right.
                let qp = FS_QUADPOINTSF {
                    x1: r.x0,
                    y1: r.y1,
                    x2: r.x1,
                    y2: r.y1,
                    x3: r.x0,
                    y3: r.y0,
                    x4: r.x1,
                    y4: r.y0,
                };
                if api.FPDFAnnot_AppendAttachmentPoints(annot, &raw const qp) == 0 {
                    return fail("writing markup quads failed");
                }
                bounds = Some(bounds.map_or(r, |b| b.union(&r)));
            }
        }
        match &a.kind {
            NewAnnotationKind::Note { at } => {
                bounds = Some(Rect::from_corners(
                    Point::new(at.x, at.y - 20.0),
                    Point::new(at.x + 20.0, at.y),
                ));
            }
            NewAnnotationKind::Ink { strokes, width } => {
                for stroke in strokes {
                    let points: Vec<FS_POINTF> = stroke
                        .iter()
                        .map(|p| FS_POINTF { x: p.x, y: p.y })
                        .collect();
                    if points.is_empty() {
                        continue;
                    }
                    if api.FPDFAnnot_AddInkStroke(annot, points.as_ptr(), points.len()) < 0 {
                        return fail("writing an ink stroke failed");
                    }
                    for p in stroke {
                        let r = Rect::from_corners(*p, *p);
                        bounds = Some(bounds.map_or(r, |b| b.union(&r)));
                    }
                }
                let w = width.max(0.1);
                api.FPDFAnnot_SetBorder(annot, 0.0, 0.0, w);
                bounds = bounds.map(|b| Rect {
                    x0: b.x0 - w,
                    y0: b.y0 - w,
                    x1: b.x1 + w,
                    y1: b.y1 + w,
                });
            }
            NewAnnotationKind::FreeText { rect, font_size } => {
                bounds = Some(*rect);
                let (r, g, b) = (a.color.r, a.color.g, a.color.b);
                let da = format!(
                    "/Helv {:.2} Tf {:.3} {:.3} {:.3} rg",
                    font_size.max(1.0),
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0
                );
                api.FPDFAnnot_SetStringValue_str(annot, "DA", &da);
                api.FPDFAnnot_SetBorder(annot, 0.0, 0.0, 0.0);
            }
            NewAnnotationKind::Stamp { rect, .. } => bounds = Some(*rect),
            _ => {}
        }
        let Some(bounds) = bounds.filter(Rect::is_finite) else {
            return fail("the annotation has no geometry");
        };
        let rect = to_fs_rect(bounds);
        if api.FPDFAnnot_SetRect(annot, &raw const rect) == 0 {
            return fail("setting the annotation rectangle failed");
        }
        if let NewAnnotationKind::Stamp {
            strokes,
            stroke_width,
            text,
            ..
        } = &a.kind
            && !finished_later
            && !self.stamp_appearance(doc, annot, a.color, strokes, *stroke_width, text)
        {
            return fail("drawing the stamp appearance failed");
        }
        if !matches!(a.kind, NewAnnotationKind::FreeText { .. }) {
            let c = a.color;
            api.FPDFAnnot_SetColor(
                annot,
                COLORTYPE_COLOR,
                u32::from(c.r),
                u32::from(c.g),
                u32::from(c.b),
                u32::from(c.a),
            );
        }
        if finished_later
            && matches!(
                a.kind,
                NewAnnotationKind::FreeText { .. } | NewAnnotationKind::Stamp { .. }
            )
        {
            // A placeholder until the patch pass writes the real appearance:
            // without one pdfium would generate its own (in a standard font)
            // when the page is laid out.
            const APPEARANCE_NORMAL: c_int = 0;
            api.FPDFAnnot_SetAP_str(annot, APPEARANCE_NORMAL, "");
        }
        api.FPDFAnnot_SetFlags(annot, ANNOT_FLAG_PRINT);
        // `/Contents` is set after the layout pass (see `apply_edits`).
        for (key, value) in [("T", &a.author), ("NM", &a.name), ("M", &a.modified)] {
            if !value.is_empty() {
                let w = wide(value);
                api.FPDFAnnot_SetStringValue(annot, key, w.as_ptr());
            }
        }
        api.FPDFPage_CloseAnnot(annot);
        Ok(())
    }

    /// Remove the adopted annotations `targets` from `page`, with their
    /// pop-up windows. An annotation is found by `/NM` where it has one
    /// (checked at its recorded index first), otherwise at its index; a
    /// target whose subtype no longer matches is left alone.
    fn remove_adopted(&self, page: FPDF_PAGE, targets: &[&AdoptedAnnotation]) {
        let api = self.api();
        let count = api.FPDFPage_GetAnnotCount(page).clamp(0, 10_000);
        let mut present: Vec<(AnnotationKind, String)> = Vec::with_capacity(count as usize);
        for i in 0..count {
            let annot = api.FPDFPage_GetAnnot(page, i);
            if annot.is_null() {
                present.push((AnnotationKind::Other(String::new()), String::new()));
                continue;
            }
            present.push((
                Self::annotation_kind(api.FPDFAnnot_GetSubtype(annot)),
                self.annot_string(annot, "NM"),
            ));
            api.FPDFPage_CloseAnnot(annot);
        }
        let mut doomed: Vec<c_int> = Vec::new();
        for adopted in targets {
            let t = &adopted.target;
            let at_index = present
                .get(t.index)
                .is_some_and(|(kind, name)| *kind == t.kind && *name == t.name);
            let index = if at_index {
                Some(t.index)
            } else if t.name.is_empty() {
                None
            } else {
                present
                    .iter()
                    .position(|(kind, name)| *kind == t.kind && *name == t.name)
            };
            let Some(index) = index else {
                continue;
            };
            let Ok(index) = c_int::try_from(index) else {
                continue;
            };
            doomed.push(index);
            let annot = api.FPDFPage_GetAnnot(page, index);
            if !annot.is_null() {
                let popup = api.FPDFAnnot_GetLinkedAnnot(annot, "Popup");
                if !popup.is_null() {
                    let p = api.FPDFPage_GetAnnotIndex(page, popup);
                    if p >= 0 {
                        doomed.push(p);
                    }
                    api.FPDFPage_CloseAnnot(popup);
                }
                api.FPDFPage_CloseAnnot(annot);
            }
        }
        doomed.sort_unstable();
        doomed.dedup();
        for index in doomed.into_iter().rev() {
            api.FPDFPage_RemoveAnnot(page, index);
        }
    }

    /// Build a stamp's appearance from page objects; pdfium writes them as
    /// the annotation's appearance stream with the resources they need.
    /// Used where the patch pass cannot run (encrypted sources): the text is
    /// set in Helvetica, so only Latin text shows there.
    fn stamp_appearance(
        &self,
        doc: FPDF_DOCUMENT,
        annot: FPDF_ANNOTATION,
        color: Rgba,
        strokes: &[Vec<Point>],
        stroke_width: f32,
        text: &[crate::edits::StampText],
    ) -> bool {
        const FILL_NONE: c_int = 0;
        const ROUND: c_int = 1;
        let api = self.api();
        let [red, green, blue, alpha] = [color.r, color.g, color.b, color.a].map(u32::from);
        for stroke in strokes {
            let Some((first, rest)) = stroke.split_first() else {
                continue;
            };
            let path = api.FPDFPageObj_CreateNewPath(first.x, first.y);
            if path.is_null() {
                return false;
            }
            for p in rest {
                api.FPDFPath_LineTo(path, p.x, p.y);
            }
            api.FPDFPageObj_SetStrokeColor(path, red, green, blue, alpha);
            api.FPDFPageObj_SetStrokeWidth(path, stroke_width.max(0.1));
            api.FPDFPageObj_SetLineCap(path, ROUND);
            api.FPDFPageObj_SetLineJoin(path, ROUND);
            api.FPDFPath_SetDrawMode(path, FILL_NONE, 1);
            if api.FPDFAnnot_AppendObject(annot, path) == 0 {
                api.FPDFPageObj_Destroy(path);
                return false;
            }
        }
        for line in text.iter().filter(|l| !l.text.is_empty()) {
            let obj = api.FPDFPageObj_NewTextObj(doc, "Helvetica", line.size.max(1.0));
            if obj.is_null() {
                return false;
            }
            let w = wide(&line.text);
            api.FPDFText_SetText(obj, w.as_ptr());
            api.FPDFPageObj_SetFillColor(obj, red, green, blue, alpha);
            api.FPDFPageObj_Transform(
                obj,
                1.0,
                0.0,
                0.0,
                1.0,
                f64::from(line.at.x),
                f64::from(line.at.y),
            );
            if api.FPDFAnnot_AppendObject(annot, obj) == 0 {
                api.FPDFPageObj_Destroy(obj);
                return false;
            }
        }
        true
    }

    fn apply_form_values(&self, doc: FPDF_DOCUMENT, form: FPDF_FORMHANDLE, edits: &Edits) {
        let api = self.api();
        let count = usize::try_from(api.FPDF_GetPageCount(doc)).unwrap_or(0);
        // Deterministic order: by field name, then as given.
        let mut values: Vec<&crate::edits::FormValue> = edits.form_values.iter().collect();
        values.sort_by(|a, b| a.name.cmp(&b.name));
        for page_index in 0..count {
            let page = api.FPDF_LoadPage(doc, page_index as c_int);
            if page.is_null() {
                continue;
            }
            api.FORM_OnAfterLoadPage(page, form);
            let annots = usize::try_from(api.FPDFPage_GetAnnotCount(page)).unwrap_or(0);
            for i in 0..annots {
                let annot = api.FPDFPage_GetAnnot(page, i as c_int);
                if annot.is_null() {
                    continue;
                }
                if api.FPDFAnnot_GetSubtype(annot) == subtype::WIDGET {
                    let name =
                        read_wide(|buf, len| api.FPDFAnnot_GetFormFieldName(form, annot, buf, len));
                    for value in values.iter().filter(|v| v.name == name) {
                        self.set_field(form, page, annot, &value.value);
                    }
                }
                api.FPDFPage_CloseAnnot(annot);
            }
            api.FORM_ForceToKillFocus(form);
            api.FORM_OnBeforeClosePage(page, form);
            api.FPDF_ClosePage(page);
        }
    }

    fn option_index(
        &self,
        form: FPDF_FORMHANDLE,
        annot: FPDF_ANNOTATION,
        label: &str,
    ) -> Option<c_int> {
        let api = self.api();
        let n = api.FPDFAnnot_GetOptionCount(form, annot).clamp(0, 10_000);
        (0..n).find(|&o| {
            read_wide(|buf, len| api.FPDFAnnot_GetOptionLabel(form, annot, o, buf, len)) == label
        })
    }

    fn set_field(
        &self,
        form: FPDF_FORMHANDLE,
        page: FPDF_PAGE,
        annot: FPDF_ANNOTATION,
        value: &FieldValue,
    ) {
        let api = self.api();
        let kind = api.FPDFAnnot_GetFormFieldType(form, annot);
        let choice_index = match value {
            FieldValue::Text(text) | FieldValue::Choice(text)
                if matches!(kind, field_type::COMBOBOX | field_type::LISTBOX) =>
            {
                self.option_index(form, annot, text)
            }
            _ => None,
        };
        match (kind, value) {
            (field_type::COMBOBOX | field_type::LISTBOX, _) if choice_index.is_some() => {
                if let Some(index) = choice_index
                    && api.FORM_SetFocusedAnnot(form, annot) != 0
                {
                    api.FORM_SetIndexSelected(form, page, index, 1);
                    api.FORM_ForceToKillFocus(form);
                }
            }
            (
                field_type::TEXTFIELD | field_type::COMBOBOX,
                FieldValue::Text(text) | FieldValue::Choice(text),
            ) => {
                // Text fields, and editable combo boxes given free text.
                if api.FORM_SetFocusedAnnot(form, annot) != 0 {
                    api.FORM_SelectAllText(form, page);
                    let w = wide(text);
                    api.FORM_ReplaceSelection(form, page, w.as_ptr());
                    api.FORM_ForceToKillFocus(form);
                }
            }
            (
                field_type::CHECKBOX | field_type::RADIOBUTTON,
                FieldValue::Checked { on, export },
            ) => {
                let this_export = read_wide(|buf, len| {
                    api.FPDFAnnot_GetFormFieldExportValue(form, annot, buf, len)
                });
                let wanted = match export {
                    Some(e) if kind == field_type::RADIOBUTTON => *on && *e == this_export,
                    _ => *on,
                };
                let checked = api.FPDFAnnot_IsChecked(form, annot) != 0;
                // A radio widget is only ever clicked on; the group turns the
                // others off.
                let click = wanted != checked && (kind == field_type::CHECKBOX || wanted);
                if click {
                    let mut rect = FS_RECTF {
                        left: 0.0,
                        top: 0.0,
                        right: 0.0,
                        bottom: 0.0,
                    };
                    if api.FPDFAnnot_GetRect(annot, &raw mut rect) != 0 {
                        let x = f64::from((rect.left + rect.right) * 0.5);
                        let y = f64::from((rect.top + rect.bottom) * 0.5);
                        api.FORM_OnLButtonDown(form, page, 0, x, y);
                        api.FORM_OnLButtonUp(form, page, 0, x, y);
                        api.FORM_ForceToKillFocus(form);
                    }
                }
            }
            _ => {}
        }
    }

    /// Try what the sandbox must refuse; used by the helper's self-test.
    /// Sockets count as refused when none can be bound: Seatbelt lets a
    /// process create one but not bind or connect it.
    #[must_use]
    pub fn probe(path: &str) -> crate::proto::ProbeReport {
        use std::os::unix::net::UnixDatagram;
        let opened_file = File::open(path).is_ok();
        let dir = Path::new(path).parent();
        let created_file = dir.is_some_and(|dir| File::create(dir.join(".tilo-pdf-probe")).is_ok());
        let inet_socket = std::net::UdpSocket::bind("127.0.0.1:0").is_ok()
            || std::net::TcpListener::bind("127.0.0.1:0").is_ok();
        let unix_socket =
            dir.is_some_and(|dir| UnixDatagram::bind(dir.join(".tilo-pdf-probe.sock")).is_ok());
        let exec = std::process::Command::new("/bin/true").status().is_ok();
        crate::proto::ProbeReport {
            opened_file,
            created_file,
            inet_socket,
            unix_socket,
            exec,
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let ids: Vec<u64> = self.docs.keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }
}

const MAX_OUTLINE_DEPTH: usize = 64;

// Depth is capped, so the recursion is bounded.
fn walk(
    engine: &Engine,
    doc: &Doc,
    parent: FPDF_BOOKMARK,
    depth: usize,
    seen: &mut HashSet<usize>,
    budget: &mut usize,
) -> Vec<OutlineEntry> {
    let api = engine.api();
    let mut out = Vec::new();
    if depth > MAX_OUTLINE_DEPTH {
        return out;
    }
    let mut child = api.FPDFBookmark_GetFirstChild(doc.raw, parent);
    while !child.is_null() && *budget > 0 {
        // A malformed outline can link back into itself.
        if !seen.insert(child as usize) {
            break;
        }
        *budget = budget.saturating_sub(1);
        let title = read_wide_void(|buf, len| api.FPDFBookmark_GetTitle(child, buf, len));
        let destination = engine
            .destination(doc, api.FPDFBookmark_GetDest(doc.raw, child))
            .or_else(|| engine.action_destination(doc, api.FPDFBookmark_GetAction(child)));
        let children = walk(engine, doc, child, depth.saturating_add(1), seen, budget);
        out.push(OutlineEntry {
            title,
            destination,
            children,
        });
        child = api.FPDFBookmark_GetNextSibling(doc.raw, child);
    }
    out
}

fn export_seed(bytes: &[u8], edits: &Edits) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo-pdf export v1");
    hasher.update(bytes);
    hasher.update(&postcard::to_allocvec(edits).unwrap_or_default());
    *hasher.finalize().as_bytes()
}

/// pdfium seeds the file identifier it writes into the trailer from a heap
/// address (and a random generator), so two identical exports would differ
/// there — between processes, and between runs whenever the allocator hands
/// out a different address. Replace every identifier string that pdfium
/// generated (any `/ID` string, hex or literal, not present in the source)
/// with one derived from the inputs, keeping its length so no byte offset
/// moves.
fn normalize_trailer_ids(out: &mut [u8], source: &[u8], seed: &[u8; 32]) {
    let start = if out.starts_with(source) {
        source.len()
    } else {
        0
    };
    let mut counter = 0u64;
    let mut pos = start;
    while let Some(found) = find(out, b"/ID", pos) {
        pos = found.saturating_add(3);
        // Expect optional whitespace, '[', then strings.
        let mut i = pos;
        while out.get(i).is_some_and(u8::is_ascii_whitespace) {
            i = i.saturating_add(1);
        }
        if out.get(i) != Some(&b'[') {
            continue;
        }
        i = i.saturating_add(1);
        loop {
            while out.get(i).is_some_and(u8::is_ascii_whitespace) {
                i = i.saturating_add(1);
            }
            // The string's whole token and the bytes between its delimiters.
            let Some((token_end, content)) = pdf_string_at(out, i) else {
                break;
            };
            let token = out
                .get(i..token_end)
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            if find(source, &token, 0).is_none() {
                let mut hasher = blake3::Hasher::new();
                hasher.update(seed);
                hasher.update(&counter.to_le_bytes());
                let mut digits = hasher
                    .finalize()
                    .to_hex()
                    .to_string()
                    .to_ascii_uppercase()
                    .into_bytes();
                let len = content.len();
                while digits.len() < len {
                    let again = digits.clone();
                    digits.extend_from_slice(&again);
                }
                // Hex digits are valid content for either string form.
                if let (Some(dst), Some(src)) = (out.get_mut(content), digits.get(..len)) {
                    dst.copy_from_slice(src);
                }
                counter = counter.saturating_add(1);
            }
            i = token_end;
        }
    }
}

/// The PDF string starting at `at` — `<hex>` or `(literal)` with balanced
/// parentheses and backslash escapes — as the end of its token and the range
/// of the bytes between its delimiters.
fn pdf_string_at(bytes: &[u8], at: usize) -> Option<(usize, std::ops::Range<usize>)> {
    let first = at.checked_add(1)?;
    match bytes.get(at)? {
        b'<' => {
            let end = find(bytes, b">", first)?;
            Some((end.checked_add(1)?, first..end))
        }
        b'(' => {
            let mut depth = 1usize;
            let mut i = first;
            while depth > 0 {
                match bytes.get(i)? {
                    b'\\' => i = i.checked_add(1)?,
                    b'(' => depth = depth.checked_add(1)?,
                    b')' => depth = depth.checked_sub(1)?,
                    _ => {}
                }
                i = i.checked_add(1)?;
            }
            let end = i.checked_sub(1)?;
            Some((i, first..end))
        }
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p.saturating_add(from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_replaced_and_source_ids_kept() {
        // Phase `generated_ids_are_replaced_and_source_ids_kept`.
        {
            let source = b"%PDF-1.7 trailer<</ID[<AAAA><BBBB>]>>".to_vec();
            let mut out = source.clone();
            out.extend_from_slice(b"\ntrailer<</ID[<AAAA><1234>]>>");
            let before = out.clone();
            normalize_trailer_ids(&mut out, &source, &[7; 32]);
            assert_eq!(out.len(), before.len());
            assert!(out.starts_with(&source));
            let tail = &out[source.len()..];
            assert!(find(tail, b"<AAAA>", 0).is_some(), "source id kept");
            assert!(find(tail, b"<1234>", 0).is_none(), "generated id replaced");
            let mut again = before;
            normalize_trailer_ids(&mut again, &source, &[7; 32]);
            assert_eq!(out, again, "deterministic");
        }
        // Phase `generated_ids_after_a_literal_source_id_are_replaced`.
        // Typst (and many writers) use literal strings for `/ID`; pdfium keeps
        // the first and appends its own after it.
        {
            let source = b"%PDF-1.7 trailer<</ID[(X/a\\)b==)(X/a\\)b==)]>>".to_vec();
            let mut out = source.clone();
            out.extend_from_slice(b"\ntrailer<</ID[(X/a\\)b==)<9F3A11>]>>");
            let mut other = source.clone();
            other.extend_from_slice(b"\ntrailer<</ID[(X/a\\)b==)<07BC2E>]>>");
            normalize_trailer_ids(&mut out, &source, &[7; 32]);
            normalize_trailer_ids(&mut other, &source, &[7; 32]);
            assert_eq!(
                out, other,
                "whatever pdfium generated, the result is the same"
            );
            let tail = &out[source.len()..];
            assert!(find(tail, b"(X/a\\)b==)", 0).is_some(), "source id kept");
            // A generated literal string is replaced too.
            let mut literal = source.clone();
            literal.extend_from_slice(b"\ntrailer<</ID[(X/a\\)b==)(zz(q)z)]>>");
            let before = literal.len();
            normalize_trailer_ids(&mut literal, &source, &[7; 32]);
            assert_eq!(literal.len(), before);
            assert!(find(&literal[source.len()..], b"zz(q)z", 0).is_none());
        }
    }
}
