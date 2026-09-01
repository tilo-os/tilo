//! Hand-written FFI declarations for the WPE WebKit stack (feature
//! `webview-wpe`): GLib/GObject basics, libwpe, WPEBackend-fdo's SHM
//! exportable, libwayland-server's SHM buffer accessors, and the
//! wpe-webkit-2.0 GLib API surface the engine uses.
//!
//! Declarations mirror the C headers of libwpe 1.16 / wpebackend-fdo
//! 1.16 / wpewebkit 2.52 (see `os/dev/build-wpe.sh`). Kept minimal on
//! purpose — only what `super::WpeEngine` calls.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_double, c_int, c_uint, c_void};

// ───────────────────────────── GLib / GObject ─────────────────────────────

pub type gboolean = c_int;
pub const GFALSE: gboolean = 0;

pub enum GMainContext {}
pub enum GMainLoop {}
pub enum GSource {}
pub enum GBytes {}
pub enum GError {}
pub enum GCancellable {}
pub enum GAsyncResult {}
pub enum GParamSpec {}
pub enum JSCValue {}

pub type GSourceFunc = extern "C" fn(user_data: *mut c_void) -> gboolean;
pub type GAsyncReadyCallback =
    extern "C" fn(source: *mut c_void, res: *mut GAsyncResult, user_data: *mut c_void);
pub type GCallback = *const c_void;
pub type GClosureNotify = *const c_void;

#[repr(C)]
pub struct GErrorRepr {
    pub domain: u32,
    pub code: c_int,
    pub message: *const c_char,
}

#[link(name = "glib-2.0")]
unsafe extern "C" {
    pub fn g_main_context_new() -> *mut GMainContext;
    pub fn g_main_context_push_thread_default(ctx: *mut GMainContext);
    /// Thread-safe: wakes a context blocked in its poll so a command
    /// pushed from another thread is drained now, not on the next timer
    /// tick. This is what keeps input latency off the drain cadence.
    pub fn g_main_context_wakeup(ctx: *mut GMainContext);
    pub fn g_main_loop_new(ctx: *mut GMainContext, is_running: gboolean) -> *mut GMainLoop;
    pub fn g_main_loop_run(l: *mut GMainLoop);
    pub fn g_timeout_source_new(interval_ms: c_uint) -> *mut GSource;
    pub fn g_source_set_callback(
        source: *mut GSource,
        func: GSourceFunc,
        data: *mut c_void,
        notify: GClosureNotify,
    );
    pub fn g_source_attach(source: *mut GSource, ctx: *mut GMainContext) -> c_uint;
    pub fn g_source_unref(source: *mut GSource);
    pub fn g_bytes_new(data: *const c_void, size: usize) -> *mut GBytes;
    pub fn g_bytes_unref(bytes: *mut GBytes);
    pub fn g_error_free(err: *mut GError);
    pub fn g_free(mem: *mut c_void);
}

#[link(name = "gobject-2.0")]
unsafe extern "C" {
    pub fn g_signal_connect_data(
        instance: *mut c_void,
        detailed_signal: *const c_char,
        c_handler: GCallback,
        data: *mut c_void,
        destroy_data: GClosureNotify,
        connect_flags: c_uint,
    ) -> u64;
    /// `GType` is `gsize`. Construction property list is NULL-terminated.
    pub fn g_object_new(object_type: usize, first_property_name: *const c_char, ...)
    -> *mut c_void;
    pub fn g_object_unref(object: *mut c_void);
}

// ─────────────────────────────── libwpe ───────────────────────────────────

pub enum wpe_view_backend {}

#[repr(C)]
pub struct wpe_input_keyboard_event {
    pub time: u32,
    pub key_code: u32,
    pub hardware_key_code: u32,
    pub pressed: bool,
    pub modifiers: u32,
}

// `enum wpe_input_modifier` — keyboard bits (pointer button bits live in
// the 1<<20+ range and are computed in mod.rs).
pub const WPE_INPUT_KEYBOARD_MODIFIER_CONTROL: u32 = 1 << 0;
pub const WPE_INPUT_KEYBOARD_MODIFIER_SHIFT: u32 = 1 << 1;
pub const WPE_INPUT_KEYBOARD_MODIFIER_ALT: u32 = 1 << 2;
pub const WPE_INPUT_KEYBOARD_MODIFIER_META: u32 = 1 << 3;

pub const WPE_INPUT_POINTER_EVENT_TYPE_MOTION: c_uint = 1;
pub const WPE_INPUT_POINTER_EVENT_TYPE_BUTTON: c_uint = 2;

#[repr(C)]
pub struct wpe_input_pointer_event {
    pub type_: c_uint,
    pub time: u32,
    pub x: c_int,
    pub y: c_int,
    pub button: u32,
    pub state: u32,
    pub modifiers: u32,
}

pub const WPE_INPUT_AXIS_EVENT_TYPE_MOTION_SMOOTH: c_uint = 2;
pub const WPE_INPUT_AXIS_EVENT_TYPE_MASK_2D: c_uint = 1 << 16;

#[repr(C)]
pub struct wpe_input_axis_event {
    pub type_: c_uint,
    pub time: u32,
    pub x: c_int,
    pub y: c_int,
    pub axis: u32,
    pub value: i32,
    pub modifiers: u32,
}

#[repr(C)]
pub struct wpe_input_axis_2d_event {
    pub base: wpe_input_axis_event,
    pub x_axis: c_double,
    pub y_axis: c_double,
}

// Keysyms (X11 keysym values, from <wpe/keysyms.h>).
pub const WPE_KEY_BACKSPACE: u32 = 0xff08;
pub const WPE_KEY_TAB: u32 = 0xff09;
pub const WPE_KEY_RETURN: u32 = 0xff0d;
pub const WPE_KEY_ESCAPE: u32 = 0xff1b;
pub const WPE_KEY_HOME: u32 = 0xff50;
pub const WPE_KEY_LEFT: u32 = 0xff51;
pub const WPE_KEY_UP: u32 = 0xff52;
pub const WPE_KEY_RIGHT: u32 = 0xff53;
pub const WPE_KEY_DOWN: u32 = 0xff54;
pub const WPE_KEY_PAGE_UP: u32 = 0xff55;
pub const WPE_KEY_PAGE_DOWN: u32 = 0xff56;
pub const WPE_KEY_END: u32 = 0xff57;
pub const WPE_KEY_DELETE: u32 = 0xffff;

pub const WPE_VIEW_ACTIVITY_STATE_VISIBLE: u32 = 1 << 0;
pub const WPE_VIEW_ACTIVITY_STATE_FOCUSED: u32 = 1 << 1;
pub const WPE_VIEW_ACTIVITY_STATE_IN_WINDOW: u32 = 1 << 2;

#[link(name = "wpe-1.0")]
unsafe extern "C" {
    pub fn wpe_loader_init(impl_library_name: *const c_char) -> bool;
    pub fn wpe_view_backend_dispatch_keyboard_event(
        backend: *mut wpe_view_backend,
        event: *mut wpe_input_keyboard_event,
    );
    pub fn wpe_view_backend_dispatch_pointer_event(
        backend: *mut wpe_view_backend,
        event: *mut wpe_input_pointer_event,
    );
    pub fn wpe_view_backend_dispatch_axis_event(
        backend: *mut wpe_view_backend,
        event: *mut wpe_input_axis_event,
    );
    pub fn wpe_view_backend_dispatch_set_size(
        backend: *mut wpe_view_backend,
        width: u32,
        height: u32,
    );
    pub fn wpe_view_backend_dispatch_set_device_scale_factor(
        backend: *mut wpe_view_backend,
        scale: f32,
    );
    pub fn wpe_view_backend_add_activity_state(backend: *mut wpe_view_backend, state: u32);
    pub fn wpe_view_backend_remove_activity_state(backend: *mut wpe_view_backend, state: u32);
}

// ─────────────────────────── WPEBackend-fdo ───────────────────────────────

pub enum wpe_view_backend_exportable_fdo {}
pub enum wpe_fdo_shm_exported_buffer {}
pub enum wpe_fdo_egl_exported_image {}
pub enum wl_resource {}
pub enum wl_shm_buffer {}

/// EGL-export variant of the fdo client — used by the DMA-BUF zero-copy
/// path. `export_fdo_egl_image` fires per frame with a WebKit-rendered
/// `EGLImageKHR`.
#[repr(C)]
pub struct wpe_view_backend_exportable_fdo_egl_client {
    pub export_egl_image: Option<extern "C" fn(data: *mut c_void, image: EGLImageKHR)>,
    pub export_fdo_egl_image:
        Option<extern "C" fn(data: *mut c_void, image: *mut wpe_fdo_egl_exported_image)>,
    pub export_shm_buffer:
        Option<extern "C" fn(data: *mut c_void, buffer: *mut wpe_fdo_shm_exported_buffer)>,
    pub _wpe_reserved0: Option<extern "C" fn()>,
    pub _wpe_reserved1: Option<extern "C" fn()>,
}

#[repr(C)]
pub struct wpe_view_backend_exportable_fdo_client {
    pub export_buffer_resource: Option<extern "C" fn(data: *mut c_void, buffer: *mut wl_resource)>,
    pub export_dmabuf_resource: Option<extern "C" fn(data: *mut c_void, dmabuf: *mut c_void)>,
    pub export_shm_buffer:
        Option<extern "C" fn(data: *mut c_void, buffer: *mut wpe_fdo_shm_exported_buffer)>,
    pub _wpe_reserved0: Option<extern "C" fn()>,
    pub _wpe_reserved1: Option<extern "C" fn()>,
}

#[link(name = "WPEBackend-fdo-1.0")]
unsafe extern "C" {
    pub fn wpe_fdo_initialize_shm() -> bool;
    pub fn wpe_view_backend_exportable_fdo_create(
        client: *const wpe_view_backend_exportable_fdo_client,
        data: *mut c_void,
        width: u32,
        height: u32,
    ) -> *mut wpe_view_backend_exportable_fdo;
    pub fn wpe_view_backend_exportable_fdo_get_view_backend(
        exportable: *mut wpe_view_backend_exportable_fdo,
    ) -> *mut wpe_view_backend;
    pub fn wpe_view_backend_exportable_fdo_dispatch_frame_complete(
        exportable: *mut wpe_view_backend_exportable_fdo,
    );
    pub fn wpe_view_backend_exportable_fdo_dispatch_release_shm_exported_buffer(
        exportable: *mut wpe_view_backend_exportable_fdo,
        buffer: *mut wpe_fdo_shm_exported_buffer,
    );
    pub fn wpe_fdo_shm_exported_buffer_get_shm_buffer(
        buffer: *mut wpe_fdo_shm_exported_buffer,
    ) -> *mut wl_shm_buffer;

    // ── EGL / DMA-BUF export path ──
    pub fn wpe_fdo_initialize_for_egl_display(egl_display: EGLDisplay);
    pub fn wpe_view_backend_exportable_fdo_egl_create(
        client: *const wpe_view_backend_exportable_fdo_egl_client,
        data: *mut c_void,
        width: u32,
        height: u32,
    ) -> *mut wpe_view_backend_exportable_fdo;
    pub fn wpe_fdo_egl_exported_image_get_egl_image(
        image: *mut wpe_fdo_egl_exported_image,
    ) -> EGLImageKHR;
    pub fn wpe_fdo_egl_exported_image_get_width(image: *mut wpe_fdo_egl_exported_image) -> u32;
    pub fn wpe_fdo_egl_exported_image_get_height(image: *mut wpe_fdo_egl_exported_image) -> u32;
    pub fn wpe_view_backend_exportable_fdo_egl_dispatch_release_exported_image(
        exportable: *mut wpe_view_backend_exportable_fdo,
        image: *mut wpe_fdo_egl_exported_image,
    );
}

// ─────────────────────────────── EGL + GBM ────────────────────────────────
// For the DMA-BUF zero-copy path: open a render node via gbm, get an EGL
// display on it, and export WebKit's per-frame EGLImage to a dmabuf fd.

pub type EGLDisplay = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLImageKHR = *mut c_void;
pub type EGLBoolean = c_uint;
pub type EGLenum = c_uint;
pub type EGLint = i32;
pub enum gbm_device {}

pub const EGL_TRUE: EGLBoolean = 1;
pub const EGL_NONE: EGLint = 0x3038;
pub const EGL_PLATFORM_GBM_KHR: EGLenum = 0x31D7;
pub const EGL_OPENGL_ES_API: EGLenum = 0x30A0;
pub const EGL_CONTEXT_CLIENT_VERSION: EGLint = 0x3098;
pub const EGL_SURFACE_TYPE: EGLint = 0x3033;
pub const EGL_WINDOW_BIT: EGLint = 0x0004;
pub const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
pub const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
pub const EGL_RED_SIZE: EGLint = 0x3024;
pub const EGL_GREEN_SIZE: EGLint = 0x3023;
pub const EGL_BLUE_SIZE: EGLint = 0x3022;
pub const EGL_ALPHA_SIZE: EGLint = 0x3021;

#[link(name = "gbm")]
unsafe extern "C" {
    pub fn gbm_create_device(fd: c_int) -> *mut gbm_device;
}

#[link(name = "EGL")]
unsafe extern "C" {
    /// Core EGL 1.5 — a platform display on the gbm device.
    pub fn eglGetPlatformDisplay(
        platform: EGLenum,
        native_display: *mut c_void,
        attrib_list: *const isize,
    ) -> EGLDisplay;
    pub fn eglInitialize(dpy: EGLDisplay, major: *mut EGLint, minor: *mut EGLint) -> EGLBoolean;
    pub fn eglBindAPI(api: EGLenum) -> EGLBoolean;
    pub fn eglChooseConfig(
        dpy: EGLDisplay,
        attrib_list: *const EGLint,
        configs: *mut EGLConfig,
        config_size: EGLint,
        num_config: *mut EGLint,
    ) -> EGLBoolean;
    pub fn eglCreateContext(
        dpy: EGLDisplay,
        config: EGLConfig,
        share_context: EGLContext,
        attrib_list: *const EGLint,
    ) -> EGLContext;
    pub fn eglMakeCurrent(
        dpy: EGLDisplay,
        draw: EGLSurface,
        read: EGLSurface,
        ctx: EGLContext,
    ) -> EGLBoolean;
    pub fn eglGetError() -> EGLint;
    pub fn eglGetProcAddress(procname: *const c_char) -> *mut c_void;
}

/// `eglExportDMABUFImageQueryMESA` — loaded via `eglGetProcAddress` (it is
/// an extension, not part of the core library ABI).
pub type PFNEglExportDmabufImageQueryMesa = unsafe extern "C" fn(
    dpy: EGLDisplay,
    image: EGLImageKHR,
    fourcc: *mut c_int,
    num_planes: *mut c_int,
    modifiers: *mut u64,
) -> EGLBoolean;
/// `eglExportDMABUFImageMESA` — one fd/stride/offset per plane.
pub type PFNEglExportDmabufImageMesa = unsafe extern "C" fn(
    dpy: EGLDisplay,
    image: EGLImageKHR,
    fds: *mut c_int,
    strides: *mut EGLint,
    offsets: *mut EGLint,
) -> EGLBoolean;

// ─────────────────────────── libwayland-server ────────────────────────────

#[link(name = "wayland-server")]
unsafe extern "C" {
    pub fn wl_shm_buffer_begin_access(buffer: *mut wl_shm_buffer);
    pub fn wl_shm_buffer_end_access(buffer: *mut wl_shm_buffer);
    pub fn wl_shm_buffer_get_data(buffer: *mut wl_shm_buffer) -> *mut c_void;
    pub fn wl_shm_buffer_get_stride(buffer: *mut wl_shm_buffer) -> i32;
    pub fn wl_shm_buffer_get_width(buffer: *mut wl_shm_buffer) -> i32;
    pub fn wl_shm_buffer_get_height(buffer: *mut wl_shm_buffer) -> i32;
    pub fn wl_shm_buffer_get_format(buffer: *mut wl_shm_buffer) -> u32;
}

// ───────────────────────────── wpe-webkit-2.0 ─────────────────────────────

pub enum WebKitWebViewBackend {}
pub enum WebKitWebView {}
pub enum WebKitSettings {}
pub enum WebKitUserContentManager {}
pub enum WebKitUserContentFilterStore {}
pub enum WebKitUserContentFilter {}
pub enum WebKitNetworkSession {}
pub enum WebKitUserScript {}
pub enum WebKitWebsiteDataManager {}

/// `WebKitUserContentInjectedFrames::ALL_FRAMES`
pub const WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES: c_uint = 0;
/// `WebKitUserScriptInjectionTime::AT_DOCUMENT_START` — run before the
/// page's own scripts, so an ad-skip script wins the race.
pub const WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START: c_uint = 0;
/// `WebKitWebsiteDataTypes::ALL` — cookies, caches, storage, service
/// workers, the lot. `(1 << 12) - 1` in the 2.52 header.
pub const WEBKIT_WEBSITE_DATA_ALL: c_uint = (1 << 12) - 1;

/// `GTimeSpan` (microseconds); `0` for `clear` means "all time".
pub type GTimeSpan = i64;

/// `WebKitLoadEvent`
pub const WEBKIT_LOAD_STARTED: c_uint = 0;
pub const WEBKIT_LOAD_FINISHED: c_uint = 3;

#[link(name = "WPEWebKit-2.0")]
unsafe extern "C" {
    pub fn webkit_web_view_backend_new(
        backend: *mut wpe_view_backend,
        notify: GClosureNotify,
        user_data: *mut c_void,
    ) -> *mut WebKitWebViewBackend;
    /// The 2.0 API has no `webkit_web_view_new`; views are constructed
    /// via `g_object_new(webkit_web_view_get_type(), "backend", …)`.
    pub fn webkit_web_view_get_type() -> usize;

    pub fn webkit_web_view_load_uri(view: *mut WebKitWebView, uri: *const c_char);
    pub fn webkit_web_view_reload(view: *mut WebKitWebView);
    pub fn webkit_web_view_stop_loading(view: *mut WebKitWebView);
    pub fn webkit_web_view_go_back(view: *mut WebKitWebView);
    pub fn webkit_web_view_go_forward(view: *mut WebKitWebView);
    pub fn webkit_web_view_can_go_back(view: *mut WebKitWebView) -> gboolean;
    pub fn webkit_web_view_can_go_forward(view: *mut WebKitWebView) -> gboolean;
    pub fn webkit_web_view_get_uri(view: *mut WebKitWebView) -> *const c_char;
    pub fn webkit_web_view_get_title(view: *mut WebKitWebView) -> *const c_char;
    pub fn webkit_web_view_get_estimated_load_progress(view: *mut WebKitWebView) -> c_double;
    pub fn webkit_web_view_get_settings(view: *mut WebKitWebView) -> *mut WebKitSettings;
    /// Override the User-Agent. WPE's default advertises a stale Safari
    /// version, which makes UA-sniffing sites (search engines, video
    /// sites) serve degraded/non-responsive layouts; a modern UA gets
    /// their normal responsive pages.
    pub fn webkit_settings_set_user_agent(settings: *mut WebKitSettings, user_agent: *const c_char);
    pub fn webkit_web_view_get_user_content_manager(
        view: *mut WebKitWebView,
    ) -> *mut WebKitUserContentManager;
    pub fn webkit_web_view_evaluate_javascript(
        view: *mut WebKitWebView,
        script: *const c_char,
        length: isize,
        world_name: *const c_char,
        source_uri: *const c_char,
        cancellable: *mut GCancellable,
        callback: GAsyncReadyCallback,
        user_data: *mut c_void,
    );
    pub fn webkit_web_view_evaluate_javascript_finish(
        view: *mut WebKitWebView,
        result: *mut GAsyncResult,
        error: *mut *mut GError,
    ) -> *mut JSCValue;

    pub fn webkit_user_content_manager_add_filter(
        ucm: *mut WebKitUserContentManager,
        filter: *mut WebKitUserContentFilter,
    );
    pub fn webkit_user_content_manager_remove_all_filters(ucm: *mut WebKitUserContentManager);

    // ── injected user scripts (scriptlet-style blocking, e.g. YouTube ads) ──
    pub fn webkit_user_script_new(
        source: *const c_char,
        injected_frames: c_uint,
        injection_time: c_uint,
        allow_list: *const *const c_char,
        block_list: *const *const c_char,
    ) -> *mut WebKitUserScript;
    pub fn webkit_user_script_unref(script: *mut WebKitUserScript);
    pub fn webkit_user_content_manager_add_script(
        ucm: *mut WebKitUserContentManager,
        script: *mut WebKitUserScript,
    );
    pub fn webkit_user_content_manager_remove_all_scripts(ucm: *mut WebKitUserContentManager);

    // ── website data (clear cookies / cache / storage) ──
    pub fn webkit_web_view_get_network_session(
        view: *mut WebKitWebView,
    ) -> *mut WebKitNetworkSession;
    pub fn webkit_network_session_get_website_data_manager(
        session: *mut WebKitNetworkSession,
    ) -> *mut WebKitWebsiteDataManager;
    /// Async; pass a null callback for fire-and-forget.
    pub fn webkit_website_data_manager_clear(
        manager: *mut WebKitWebsiteDataManager,
        types: c_uint,
        timespan: GTimeSpan,
        cancellable: *mut GCancellable,
        callback: *const c_void,
        user_data: *mut c_void,
    );

    pub fn webkit_user_content_filter_store_new(
        storage_path: *const c_char,
    ) -> *mut WebKitUserContentFilterStore;
    pub fn webkit_user_content_filter_store_save(
        store: *mut WebKitUserContentFilterStore,
        identifier: *const c_char,
        source: *mut GBytes,
        cancellable: *mut GCancellable,
        callback: GAsyncReadyCallback,
        user_data: *mut c_void,
    );
    pub fn webkit_user_content_filter_store_save_finish(
        store: *mut WebKitUserContentFilterStore,
        result: *mut GAsyncResult,
        error: *mut *mut GError,
    ) -> *mut WebKitUserContentFilter;
    /// Retrieve a *previously compiled* filter by identifier without
    /// recompiling — the fast path when the source is unchanged.
    pub fn webkit_user_content_filter_store_load(
        store: *mut WebKitUserContentFilterStore,
        identifier: *const c_char,
        cancellable: *mut GCancellable,
        callback: GAsyncReadyCallback,
        user_data: *mut c_void,
    );
    pub fn webkit_user_content_filter_store_load_finish(
        store: *mut WebKitUserContentFilterStore,
        result: *mut GAsyncResult,
        error: *mut *mut GError,
    ) -> *mut WebKitUserContentFilter;

    // WPEWebKit's public API exposes JSCValue and exports this symbol itself.
    // Recent WPE installations do not ship a separate
    // `libWPEJavaScriptCore-2.0`, so keep it in the WPEWebKit link block.
    pub fn jsc_value_to_string(value: *mut JSCValue) -> *mut c_char;
}
