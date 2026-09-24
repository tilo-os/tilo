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

use std::collections::{HashMap, HashSet};
use std::ffi::{c_int, c_ulong, c_void};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::ptr::null_mut;

use memmap2::Mmap;
use pdfium_render::prelude::{
    FPDF_ACTION, FPDF_ANNOTATION, FPDF_BITMAP, FPDF_BOOKMARK, FPDF_DEST, FPDF_DOCUMENT,
    FPDF_FILEWRITE, FPDF_FORMFILLINFO, FPDF_FORMHANDLE, FPDF_PAGE, FPDF_WCHAR, FS_MATRIX,
    FS_POINTF, FS_QUADPOINTSF, FS_RECTF, Pdfium, PdfiumLibraryBindings,
};

use crate::edits::{Edits, FieldValue, NewAnnotation, NewAnnotationKind, PageSource};
use crate::geom::{PageGeometry, Point, Rect, Rotation};
use crate::model::{
    AnnotationInfo, AnnotationKind, Destination, DocumentInfo, FieldKind, FieldWidget, FormField,
    Link, MAX_RENDER_PIXELS, Metadata, OutlineEntry, RenderOptions, Rgba,
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
    let mut out = Vec::new();
    if let Some(path) = std::env::var_os("TILO_PDFIUM_LIB").filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(path));
    }
    for p in [
        "/usr/lib64/libpdfium.so",
        "/usr/lib/libpdfium.so",
        "/usr/lib/x86_64-linux-gnu/libpdfium.so",
        "/usr/lib/aarch64-linux-gnu/libpdfium.so",
        "/app/lib/libpdfium.so",
    ] {
        out.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(Path::new(&home).join(".local/lib/pdfium/libpdfium.so"));
    }
    out
}

/// Directories pdfium reads system fonts from when a document uses a font
/// it does not embed.
#[must_use]
pub fn font_directories() -> Vec<PathBuf> {
    [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/usr/share/X11/fonts/Type1",
        "/usr/share/X11/fonts/TTF",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
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
        Err("no usable pdfium library was found; install one (e.g. \
             ~/.local/lib/pdfium/libpdfium.so) or set TILO_PDFIUM_LIB"
            .to_string())
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
    Mapped(#[allow(dead_code, reason = "kept alive for pdfium, never read directly")] Mmap),
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
        kind: OpenKind,
        password: Option<String>,
    ) -> Result<DocumentInfo> {
        if self.docs.contains_key(&id) {
            return Err(Failure::new(FailureKind::Invalid, "document id in use"));
        }
        let (raw, backing) = match kind {
            OpenKind::Pdf => {
                let map = crate::shm::map_document(file)
                    .map_err(|e| Failure::new(FailureKind::Format, e.to_string()))?;
                let raw = self.api().FPDF_LoadMemDocument64(&map, password.as_deref());
                if raw.is_null() {
                    return Err(self.load_failure());
                }
                (raw, Backing::Mapped(map))
            }
            OpenKind::Image => (self.image_document(file)?, Backing::None),
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
            },
        );
        Ok(info)
    }

    /// Wrap an image file as a one-page document, 612 pt wide with the
    /// image's aspect ratio.
    fn image_document(&self, file: &File) -> Result<FPDF_DOCUMENT> {
        const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
        // Mapped rather than read: the descriptor's offset is shared with
        // the sender and may sit anywhere.
        let bytes = crate::shm::map_document(file)
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
        drop(guard);
        Ok(())
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

    fn annot_string(&self, annot: FPDF_ANNOTATION, key: &str) -> String {
        let api = self.api();
        read_wide(|buf, len| api.FPDFAnnot_GetStringValue(annot, key, buf, len))
    }

    /// Annotations present on a page, in display space.
    ///
    /// # Errors
    ///
    /// Unknown document or page.
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
                });
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
            out.push(AnnotationInfo {
                kind,
                rect,
                color,
                contents: self.annot_string(annot, "Contents"),
                author: self.annot_string(annot, "T"),
                name: self.annot_string(annot, "NM"),
                quads,
                ink,
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
        let Backing::Mapped(bytes) = &source.backing else {
            return Err(Failure::new(
                FailureKind::Invalid,
                "only documents opened from PDF bytes can be exported",
            ));
        };
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
        let result = self
            .apply_edits(source, copy, form.as_ref(), edits, imports)
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
                Ok(out)
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
    ) -> Result<()> {
        let api = self.api();
        let original_count = source.pages.len();
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
        let mut annotated: Vec<usize> = Vec::new();
        for annotation in &edits.annotations {
            if annotation.page >= output_count {
                return Err(Failure::new(
                    FailureKind::Invalid,
                    "annotation page out of range",
                ));
            }
            let page = api.FPDF_LoadPage(copy, annotation.page as c_int);
            if page.is_null() {
                return Err(Failure::new(
                    FailureKind::Engine,
                    "loading a page to annotate failed",
                ));
            }
            let added = self.add_annotation(page, annotation);
            api.FPDF_ClosePage(page);
            added?;
            if !annotated.contains(&annotation.page) {
                annotated.push(annotation.page);
            }
        }
        // pdfium writes appearance streams for markup, ink and note
        // annotations when it first lays out a page's annotation list;
        // rendering each annotated page once makes it do so before saving,
        // so other readers draw exactly what pdfium draws.
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
    fn add_annotation(&self, page: FPDF_PAGE, a: &NewAnnotation) -> Result<()> {
        let api = self.api();
        let (sub, quads) = match &a.kind {
            NewAnnotationKind::Highlight { quads } => (subtype::HIGHLIGHT, Some(quads)),
            NewAnnotationKind::Underline { quads } => (subtype::UNDERLINE, Some(quads)),
            NewAnnotationKind::StrikeOut { quads } => (subtype::STRIKEOUT, Some(quads)),
            NewAnnotationKind::Squiggly { quads } => (subtype::SQUIGGLY, Some(quads)),
            NewAnnotationKind::Note { .. } => (subtype::TEXT, None),
            NewAnnotationKind::Ink { .. } => (subtype::INK, None),
            NewAnnotationKind::FreeText { .. } => (subtype::FREETEXT, None),
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
            }
            _ => {}
        }
        let Some(bounds) = bounds.filter(Rect::is_finite) else {
            return fail("the annotation has no geometry");
        };
        let rect = to_fs_rect(bounds);
        if api.FPDFAnnot_SetRect(annot, &raw const rect) == 0 {
            return fail("setting the annotation rectangle failed");
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
        api.FPDFAnnot_SetFlags(annot, ANNOT_FLAG_PRINT);
        for (key, value) in [
            ("Contents", &a.contents),
            ("T", &a.author),
            ("NM", &a.name),
            ("M", &a.modified),
        ] {
            if !value.is_empty() {
                let w = wide(value);
                api.FPDFAnnot_SetStringValue(annot, key, w.as_ptr());
            }
        }
        api.FPDFPage_CloseAnnot(annot);
        Ok(())
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
    #[must_use]
    pub fn probe(path: &str) -> crate::proto::ProbeReport {
        use std::os::unix::net::UnixDatagram;
        let opened_file = File::open(path).is_ok();
        let created_file = Path::new(path)
            .parent()
            .is_some_and(|dir| File::create(dir.join(".tilo-pdf-probe")).is_ok());
        // SAFETY: plain socket(2) calls; any returned descriptor is closed.
        let inet_socket = unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            if fd >= 0 {
                libc::close(fd);
            }
            fd >= 0
        };
        let unix_socket = UnixDatagram::unbound().is_ok();
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
/// address, so two identical exports would differ there. Replace every
/// identifier string that pdfium generated (any `/ID` hex string not
/// present in the source) with one derived from the inputs, keeping its
/// length so no byte offset moves.
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
        // Expect optional whitespace, '[', then hex strings.
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
            if out.get(i) != Some(&b'<') {
                break;
            }
            let hex_start = i.saturating_add(1);
            let Some(hex_end) = find(out, b">", hex_start) else {
                break;
            };
            let literal = out.get(i..=hex_end).map(<[u8]>::to_vec).unwrap_or_default();
            if find(source, &literal, 0).is_none() {
                let mut hasher = blake3::Hasher::new();
                hasher.update(seed);
                hasher.update(&counter.to_le_bytes());
                let digest = hasher.finalize();
                let mut hex_digits = digest
                    .to_hex()
                    .to_string()
                    .to_ascii_uppercase()
                    .into_bytes();
                let len = hex_end.saturating_sub(hex_start);
                while hex_digits.len() < len {
                    let again = hex_digits.clone();
                    hex_digits.extend_from_slice(&again);
                }
                if let (Some(dst), Some(src)) =
                    (out.get_mut(hex_start..hex_end), hex_digits.get(..len))
                {
                    dst.copy_from_slice(src);
                }
                counter = counter.saturating_add(1);
            }
            i = hex_end.saturating_add(1);
        }
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
}
