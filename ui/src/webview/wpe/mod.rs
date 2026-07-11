//! WPE WebKit engine backend (feature `webview-wpe`).
//!
//! Runs WebKit's WPE port fully offscreen on a dedicated engine thread:
//! a WPEBackend-fdo "exportable" view backend in SHM mode hands us every
//! composited frame as a shared-memory buffer, which we convert to RGBA
//! and publish into the widget's [`FrameSlot`]. Commands arrive over an
//! mpsc channel drained by a GLib timeout source on the engine thread;
//! navigation state flows back as [`WebViewEvent`]s from GObject signal
//! handlers.
//!
//! The SHM path costs one CPU copy + BGRA→RGBA swizzle per frame —
//! deliberate v1 simplicity; the DMA-BUF → wgpu external-texture import
//! is the follow-up optimisation and slots in behind the same trait.
//!
//! Requires the WPE stack built by `os/dev/build-wpe.sh` (Fedora ships
//! no WPE packages). Point `PKG_CONFIG_PATH`/`LD_LIBRARY_PATH` at the
//! prefix when building/running with this feature.
//!
//! Lifetime: the engine thread runs for the process lifetime — a
//! browser window owns exactly one engine. Dropping `WpeEngine` leaks
//! the thread by design (WebKit teardown is not worth the shutdown
//! races for a per-window process).

mod ffi;
pub mod egl;
pub mod import;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel, sync_channel};
use std::sync::{Arc, Mutex};

use super::{
    Frame, FrameSlot, KeyInput, MouseButton, NamedKey, WebViewCommand, WebViewEngine, WebViewEvent,
};

/// Command-drain cadence on the engine thread's GLib loop.
const DRAIN_INTERVAL_MS: c_uint = 8;

/// Desktop User-Agent — a current Safari string, truthful about the
/// engine (we are WebKit) so feature detection matches, recent enough
/// that UA-sniffing sites serve their modern responsive layouts.
const USER_AGENT_DESKTOP: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15";
/// Mobile User-Agent — a current mobile Safari (iPhone) string. At
/// narrow widths sites that ignore CSS width and sniff the UA (search
/// engines, video sites) then serve their phone layout, which uses a
/// `width=device-width` viewport and fits any narrow window instead of
/// overflowing a min-width desktop layout.
const USER_AGENT_MOBILE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_6 like Mac OS X) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Mobile/15E148 Safari/604.1";
/// Below this logical CSS width the view presents the mobile UA — the
/// point where desktop SERPs start overflowing and phone layouts fit.
const MOBILE_BREAKPOINT_PX: u32 = 600;

/// `wl_shm` formats we understand (both 32-bit little-endian, i.e. BGRA
/// / BGRX byte order in memory).
const WL_SHM_FORMAT_ARGB8888: u32 = 0;
const WL_SHM_FORMAT_XRGB8888: u32 = 1;

/// Shared between the engine thread and the widget/app side.
struct Shared {
    frames: FrameSlot,
    /// Latest exported dmabuf frame (zero-copy path). Consumed by the
    /// render side via [`WpeEngine::take_dmabuf`].
    dmabuf: Mutex<Option<egl::DmabufFrame>>,
    events: Mutex<Vec<WebViewEvent>>,
    /// Host wake callback — pokes the winit loop so an idle app paints
    /// engine activity promptly.
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// The engine thread's `GMainContext`, stored as a pointer once the
    /// thread has created it (0 until then). `send()` wakes it so a just-
    /// enqueued command is drained immediately rather than on the next
    /// timer tick. `g_main_context_wakeup` is explicitly thread-safe.
    ctx: AtomicUsize,
}

impl Shared {
    fn push(&self, event: WebViewEvent) {
        self.events.lock().unwrap().push(event);
        self.wake();
    }

    fn wake(&self) {
        if let Ok(guard) = self.waker.lock() {
            if let Some(waker) = guard.as_ref() {
                waker();
            }
        }
    }
}

/// See module docs.
pub struct WpeEngine {
    tx: Sender<WebViewCommand>,
    shared: Arc<Shared>,
}

impl WpeEngine {
    /// Boot WebKit on a fresh engine thread. Fails if the WPE loader or
    /// the fdo SHM backend can't initialise (e.g. libraries missing at
    /// runtime).
    pub fn new() -> Result<Arc<Self>, String> {
        let (tx, rx) = channel::<WebViewCommand>();
        let shared = Arc::new(Shared {
            frames: Arc::new(Mutex::new(None)),
            dmabuf: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
            ctx: AtomicUsize::new(0),
        });

        let (init_tx, init_rx) = sync_channel::<Result<(), String>>(1);
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("wpe-engine".into())
            .spawn(move || engine_thread(thread_shared, rx, init_tx))
            .map_err(|e| e.to_string())?;

        match init_rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Arc::new(Self { tx, shared })),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("WPE engine initialisation timed out".into()),
        }
    }
}

impl WebViewEngine for WpeEngine {
    fn send(&self, cmd: WebViewCommand) {
        // A dropped send only happens if the thread died, which `new()`
        // guards.
        let _ = self.tx.send(cmd);
        // Wake the engine loop so the command is drained now instead of
        // waiting up to the timer interval — this is the difference
        // between input latency tracking the drain cadence and tracking
        // the engine's actual work.
        let ctx = self.shared.ctx.load(Ordering::Acquire);
        if ctx != 0 {
            unsafe { ffi::g_main_context_wakeup(ctx as *mut ffi::GMainContext) };
        }
    }

    fn frames(&self) -> FrameSlot {
        self.shared.frames.clone()
    }

    fn drain_events(&self) -> Vec<WebViewEvent> {
        std::mem::take(&mut self.shared.events.lock().unwrap())
    }

    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.shared.waker.lock().unwrap() = Some(waker);
    }

    /// Take the latest exported dmabuf frame (zero-copy path), if one is
    /// pending. The caller imports it into a wgpu texture; on the next
    /// paint the engine publishes a fresh one.
    fn take_dmabuf(&self) -> Option<crate::webview::DmabufFrame> {
        self.shared.dmabuf.lock().unwrap().take()
    }
}

/// Everything the engine thread owns. Boxed and leaked so GLib callbacks
/// hold a stable pointer for the process lifetime.
struct EngineState {
    shared: Arc<Shared>,
    rx: Receiver<WebViewCommand>,
    exportable: *mut ffi::wpe_view_backend_exportable_fdo,
    backend: *mut ffi::wpe_view_backend,
    view: *mut ffi::WebKitWebView,
    filter_store: *mut ffi::WebKitUserContentFilterStore,
    /// `wpe_input_modifier` bits for currently-held mouse buttons.
    button_modifiers: u32,
    scale_factor: f32,
    /// Whether the mobile UA is currently presented (see the Resize
    /// handler's breakpoint switch).
    mobile_mode: bool,
    /// A UA change is waiting for a reload once the window stops resizing
    /// — so dragging across the mobile breakpoint doesn't reload (and
    /// flash blank) on every intermediate size.
    ua_reload_pending: bool,
    /// Timestamp of the last Resize, to debounce that reload.
    last_resize: Option<std::time::Instant>,
    /// Whether we've received the first Resize yet. Until we have, the
    /// real viewport (and thus the correct mobile/desktop UA) is unknown,
    /// so the initial navigation is held in `pending_nav` to avoid loading
    /// at the wrong size and then reloading — the startup flicker.
    first_resize_seen: bool,
    /// A Navigate that arrived before the first Resize, deferred until the
    /// viewport is known. Also fired by `nav_deadline` as a fallback so a
    /// window that never resizes still loads.
    pending_nav: Option<String>,
    /// Fallback deadline: load `pending_nav` even without a Resize.
    nav_deadline: Option<std::time::Instant>,
    /// Whether the app wants built-in ad blocking on (via `SetAdBlocking`).
    /// The install lifecycle is reconciled in the drain loop against the
    /// shared filter service.
    adblock_desired: bool,
    /// Version of the ad-filter set currently installed (`None` = none).
    /// Guards against re-installing the same compiled filters every frame.
    adblock_installed_version: Option<u64>,
    /// EGL/dmabuf export state — `Some` only on the zero-copy path
    /// (`QOS_WEBVIEW_DMABUF=1`); `None` uses the SHM copy path.
    egl: Option<egl::EglExport>,
}

fn engine_thread(
    shared: Arc<Shared>,
    rx: Receiver<WebViewCommand>,
    init_tx: std::sync::mpsc::SyncSender<Result<(), String>>,
) {
    unsafe {
        // The engine context must be this thread's default BEFORE the fdo
        // backend initialises: wpe_fdo_initialize_shm() attaches its nested
        // Wayland host as a source on the thread-default context, and only
        // this context is ever iterated by the loop below. Initialising
        // first would strand the host on a context nobody runs — the web
        // process then dies with "Failed to bind wpe_bridge".
        let ctx = ffi::g_main_context_new();
        ffi::g_main_context_push_thread_default(ctx);
        // Publish the context so `send()` on other threads can wake it.
        shared.ctx.store(ctx as usize, Ordering::Release);

        let fdo_lib = CString::new("libWPEBackend-fdo-1.0.so.1").unwrap();
        if !ffi::wpe_loader_init(fdo_lib.as_ptr()) {
            let _ = init_tx.send(Err("wpe_loader_init(libWPEBackend-fdo) failed".into()));
            return;
        }
        // Zero-copy dmabuf export is opt-in; if EGL setup fails we fall
        // back to the SHM copy path so the browser still runs.
        let egl = if std::env::var_os("QOS_WEBVIEW_DMABUF").is_some() {
            egl::EglExport::setup()
        } else {
            None
        };
        let init_ok = match &egl {
            Some(e) => {
                ffi::wpe_fdo_initialize_for_egl_display(e.display);
                true
            }
            None => ffi::wpe_fdo_initialize_shm(),
        };
        if !init_ok {
            let _ = init_tx.send(Err("wpe_fdo initialize failed".into()));
            return;
        }

        let state = Box::leak(Box::new(EngineState {
            shared,
            rx,
            exportable: std::ptr::null_mut(),
            backend: std::ptr::null_mut(),
            view: std::ptr::null_mut(),
            filter_store: std::ptr::null_mut(),
            button_modifiers: 0,
            scale_factor: 1.0,
            mobile_mode: false,
            ua_reload_pending: false,
            last_resize: None,
            first_resize_seen: false,
            pending_nav: None,
            nav_deadline: None,
            adblock_desired: false,
            adblock_installed_version: None,
            egl,
        }));
        let state_ptr = state as *mut EngineState as *mut c_void;

        static SHM_CLIENT: ffi::wpe_view_backend_exportable_fdo_client =
            ffi::wpe_view_backend_exportable_fdo_client {
                export_buffer_resource: None,
                export_dmabuf_resource: None,
                export_shm_buffer: Some(on_export_shm_buffer),
                _wpe_reserved0: None,
                _wpe_reserved1: None,
            };
        static EGL_CLIENT: ffi::wpe_view_backend_exportable_fdo_egl_client =
            ffi::wpe_view_backend_exportable_fdo_egl_client {
                export_egl_image: None,
                export_fdo_egl_image: Some(on_export_fdo_egl_image),
                export_shm_buffer: None,
                _wpe_reserved0: None,
                _wpe_reserved1: None,
            };

        state.exportable = if state.egl.is_some() {
            ffi::wpe_view_backend_exportable_fdo_egl_create(&EGL_CLIENT, state_ptr, 1024, 768)
        } else {
            ffi::wpe_view_backend_exportable_fdo_create(&SHM_CLIENT, state_ptr, 1024, 768)
        };
        state.backend =
            ffi::wpe_view_backend_exportable_fdo_get_view_backend(state.exportable);

        let view_backend =
            ffi::webkit_web_view_backend_new(state.backend, std::ptr::null(), std::ptr::null_mut());
        // The 2.0 GLib API constructs views via GObject properties (a
        // default WebKitNetworkSession / context / settings apply).
        let backend_prop = CString::new("backend").unwrap();
        state.view = ffi::g_object_new(
            ffi::webkit_web_view_get_type(),
            backend_prop.as_ptr(),
            view_backend,
            std::ptr::null::<c_char>(),
        ) as *mut ffi::WebKitWebView;
        if state.view.is_null() {
            let _ = init_tx.send(Err("constructing WebKitWebView failed".into()));
            return;
        }

        ffi::wpe_view_backend_add_activity_state(
            state.backend,
            ffi::WPE_VIEW_ACTIVITY_STATE_VISIBLE | ffi::WPE_VIEW_ACTIVITY_STATE_IN_WINDOW,
        );

        // Advertise a modern desktop Safari UA (honest about the WebKit
        // engine) so UA-sniffing sites serve their normal responsive
        // layouts instead of the degraded one WPE's stale default
        // triggers. The Resize handler swaps to the mobile UA at narrow
        // widths.
        set_user_agent(state.view, USER_AGENT_DESKTOP);

        // Content-filter store lives under the user cache dir.
        let store_dir = filter_store_dir();
        std::fs::create_dir_all(&store_dir).ok();
        let store_path = CString::new(store_dir.to_string_lossy().as_bytes()).unwrap();
        state.filter_store = ffi::webkit_user_content_filter_store_new(store_path.as_ptr());

        connect(state.view, "notify::uri", on_notify_uri as *const c_void, state_ptr);
        connect(state.view, "notify::title", on_notify_title as *const c_void, state_ptr);
        connect(
            state.view,
            "notify::estimated-load-progress",
            on_notify_progress as *const c_void,
            state_ptr,
        );
        connect(state.view, "load-changed", on_load_changed as *const c_void, state_ptr);
        connect(state.view, "load-failed", on_load_failed as *const c_void, state_ptr);

        // Command drain source.
        let source = ffi::g_timeout_source_new(DRAIN_INTERVAL_MS);
        ffi::g_source_set_callback(source, on_drain_commands, state_ptr, std::ptr::null());
        ffi::g_source_attach(source, ctx);
        ffi::g_source_unref(source);

        let _ = init_tx.send(Ok(()));

        let main_loop = ffi::g_main_loop_new(ctx, ffi::GFALSE);
        ffi::g_main_loop_run(main_loop);
    }
}

fn filter_store_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("qos-webview").join("filter-store")
}

unsafe fn connect(
    instance: *mut ffi::WebKitWebView,
    signal: &str,
    handler: ffi::GCallback,
    data: *mut c_void,
) {
    let name = CString::new(signal).unwrap();
    unsafe {
        ffi::g_signal_connect_data(
            instance as *mut c_void,
            name.as_ptr(),
            handler,
            data,
            std::ptr::null(),
            0,
        );
    }
}

unsafe fn state_from(data: *mut c_void) -> &'static mut EngineState {
    unsafe { &mut *(data as *mut EngineState) }
}

/// Whether a view of this logical CSS width should present the mobile UA.
fn wants_mobile(logical_width: u32) -> bool {
    logical_width < MOBILE_BREAKPOINT_PX
}

/// Start loading a URI. A fresh load always applies the current UA, so any
/// pending mobile/desktop reload is now redundant — clear it to avoid a
/// spurious extra reload (and the blank flash that comes with it).
fn load_uri(state: &mut EngineState, uri: &str) {
    state.ua_reload_pending = false;
    if let Ok(c) = CString::new(uri) {
        unsafe { ffi::webkit_web_view_load_uri(state.view, c.as_ptr()) };
    }
}

/// Replace all content filters with the given chunked ad-filter set (empty
/// `chunks` just clears them). Shared by the explicit `SetContentFilters`
/// command and the `SetAdBlocking` reconcile.
///
/// Always `save` (compile fresh) rather than loading a cached
/// WKContentRuleList: a loaded (rather than freshly compiled) filter
/// reproducibly blanked whole pages a few seconds after load. `version`
/// only keeps the on-disk id stable across launches.
fn install_content_filters(state: &mut EngineState, version: u64, chunks: &[Arc<String>]) {
    let sptr = state as *mut EngineState as *mut c_void;
    unsafe {
        let ucm = ffi::webkit_web_view_get_user_content_manager(state.view);
        ffi::webkit_user_content_manager_remove_all_filters(ucm);
        for (i, json) in chunks.iter().enumerate() {
            let Ok(id) = CString::new(format!("qos-adblock-{version:016x}-{i}")) else {
                continue;
            };
            let bytes = ffi::g_bytes_new(json.as_ptr() as *const c_void, json.len());
            ffi::webkit_user_content_filter_store_save(
                state.filter_store,
                id.as_ptr(),
                bytes,
                std::ptr::null_mut(),
                on_filter_saved,
                sptr,
            );
            ffi::g_bytes_unref(bytes);
        }
    }
}

/// Replace all user scripts with the given set (empty just clears them),
/// injected at document-start in all frames.
fn install_user_scripts(state: &mut EngineState, scripts: &[String]) {
    unsafe {
        let ucm = ffi::webkit_web_view_get_user_content_manager(state.view);
        ffi::webkit_user_content_manager_remove_all_scripts(ucm);
        for src in scripts.iter() {
            if let Ok(c) = CString::new(src.as_str()) {
                let script = ffi::webkit_user_script_new(
                    c.as_ptr(),
                    ffi::WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES,
                    ffi::WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START,
                    std::ptr::null(),
                    std::ptr::null(),
                );
                if !script.is_null() {
                    ffi::webkit_user_content_manager_add_script(ucm, script);
                    ffi::webkit_user_script_unref(script);
                }
            }
        }
    }
}

/// Reconcile the installed ad-filter state against `adblock_desired` and
/// the shared filter service. Idempotent + cheap when nothing changed, so
/// it can run every drain tick.
#[cfg(feature = "webview-adblock")]
fn reconcile_adblock(state: &mut EngineState) {
    if state.adblock_desired {
        if let Some(f) = crate::webview::adblock_service::service().filters() {
            if state.adblock_installed_version != Some(f.version) {
                install_content_filters(state, f.version, &f.chunks);
                install_user_scripts(state, &f.user_scripts);
                state.adblock_installed_version = Some(f.version);
                tracing::info!(version = f.version, "ad blocking installed");
            }
        }
    } else if state.adblock_installed_version.is_some() {
        install_content_filters(state, 0, &[]);
        install_user_scripts(state, &[]);
        state.adblock_installed_version = None;
        tracing::info!("ad blocking cleared");
    }
}

/// Set the view's User-Agent on its settings object. Takes effect on the
/// next page load, so callers reload if they need it applied now.
fn set_user_agent(view: *mut ffi::WebKitWebView, ua: &str) {
    unsafe {
        let settings = ffi::webkit_web_view_get_settings(view);
        if let Ok(c) = CString::new(ua) {
            ffi::webkit_settings_set_user_agent(settings, c.as_ptr());
        }
    }
}

fn cstr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

// ─────────────────────────── frame export ─────────────────────────────────

/// DMA-BUF export callback (zero-copy path). WebKit hands us a
/// GPU-rendered `EGLImageKHR`; we export it to a dmabuf fd. Phase 1 logs
/// the handle to prove the GPU export works; Phase 2 forwards it to the
/// render thread for zero-copy import.
extern "C" fn on_export_fdo_egl_image(
    data: *mut c_void,
    image: *mut ffi::wpe_fdo_egl_exported_image,
) {
    unsafe {
        let state = state_from(data);
        if let Some(egl) = &state.egl {
            let egl_image = ffi::wpe_fdo_egl_exported_image_get_egl_image(image);
            let w = ffi::wpe_fdo_egl_exported_image_get_width(image);
            let h = ffi::wpe_fdo_egl_exported_image_get_height(image);
            if let Some(dmabuf) = egl.export(egl_image, w, h) {
                tracing::debug!(
                    fourcc = %egl::fourcc_str(dmabuf.fourcc),
                    modifier = format_args!("0x{:x}", dmabuf.modifier),
                    planes = dmabuf.planes.len(),
                    "dmabuf frame exported {}x{}",
                    dmabuf.width,
                    dmabuf.height,
                );
                *state.shared.dmabuf.lock().unwrap() = Some(dmabuf);
                state.shared.wake();
            }
        }
        ffi::wpe_view_backend_exportable_fdo_egl_dispatch_release_exported_image(
            state.exportable,
            image,
        );
        ffi::wpe_view_backend_exportable_fdo_dispatch_frame_complete(state.exportable);
    }
}

extern "C" fn on_export_shm_buffer(
    data: *mut c_void,
    buffer: *mut ffi::wpe_fdo_shm_exported_buffer,
) {
    unsafe {
        let state = state_from(data);
        let shm = ffi::wpe_fdo_shm_exported_buffer_get_shm_buffer(buffer);
        if !shm.is_null() {
            ffi::wl_shm_buffer_begin_access(shm);
            let width = ffi::wl_shm_buffer_get_width(shm).max(0) as u32;
            let height = ffi::wl_shm_buffer_get_height(shm).max(0) as u32;
            let stride = ffi::wl_shm_buffer_get_stride(shm).max(0) as usize;
            let format = ffi::wl_shm_buffer_get_format(shm);
            let src = ffi::wl_shm_buffer_get_data(shm) as *const u8;

            if !src.is_null()
                && width > 0
                && height > 0
                && matches!(format, WL_SHM_FORMAT_ARGB8888 | WL_SHM_FORMAT_XRGB8888)
            {
                let swizzle_start = std::time::Instant::now();
                let opaque = format == WL_SHM_FORMAT_XRGB8888;
                let w = width as usize;
                let n = w * height as usize * 4;
                // Fill an *uninitialised* buffer — every byte is written
                // below, so the `vec![0u8; n]` zero-init was a wasted 5 MB
                // memset per frame. Swap B<->R a whole 32-bit pixel at a
                // time (LLVM vectorises the word op): source is little-
                // endian [AX]RGB8888 = bytes B,G,R,[AX] → word
                // `A<<24|R<<16|G<<8|B`; RGBA wants `A<<24|B<<16|G<<8|R`.
                let mut rgba = Vec::<u8>::with_capacity(n);
                let dst = rgba.as_mut_ptr();
                for row in 0..height as usize {
                    let mut s = src.add(row * stride) as *const u32;
                    let mut d = dst.add(row * w * 4) as *mut u32;
                    for _ in 0..w {
                        let px = s.read_unaligned();
                        let mut out = (px & 0xFF00_FF00)
                            | ((px >> 16) & 0x0000_00FF)
                            | ((px & 0x0000_00FF) << 16);
                        if opaque {
                            out |= 0xFF00_0000;
                        }
                        d.write_unaligned(out);
                        s = s.add(1);
                        d = d.add(1);
                    }
                }
                rgba.set_len(n);
                crate::webview::profile::record_frame(width, height, swizzle_start.elapsed());
                *state.shared.frames.lock().unwrap() = Some(Frame {
                    rgba: Arc::new(rgba),
                    width,
                    height,
                });
                state.shared.wake();
            }
            ffi::wl_shm_buffer_end_access(shm);
        }
        ffi::wpe_view_backend_exportable_fdo_dispatch_release_shm_exported_buffer(
            state.exportable,
            buffer,
        );
        ffi::wpe_view_backend_exportable_fdo_dispatch_frame_complete(state.exportable);
    }
}

// ─────────────────────────── signal handlers ──────────────────────────────

fn push_history_state(state: &mut EngineState) {
    let (back, forward) = unsafe {
        (
            ffi::webkit_web_view_can_go_back(state.view) != 0,
            ffi::webkit_web_view_can_go_forward(state.view) != 0,
        )
    };
    state.shared.push(WebViewEvent::HistoryChanged {
        can_go_back: back,
        can_go_forward: forward,
    });
}

extern "C" fn on_notify_uri(
    _obj: *mut c_void,
    _pspec: *mut ffi::GParamSpec,
    data: *mut c_void,
) {
    let state = unsafe { state_from(data) };
    if let Some(uri) = cstr_to_string(unsafe { ffi::webkit_web_view_get_uri(state.view) }) {
        state.shared.push(WebViewEvent::UriChanged(uri));
    }
    push_history_state(state);
}

extern "C" fn on_notify_title(
    _obj: *mut c_void,
    _pspec: *mut ffi::GParamSpec,
    data: *mut c_void,
) {
    let state = unsafe { state_from(data) };
    if let Some(title) = cstr_to_string(unsafe { ffi::webkit_web_view_get_title(state.view) }) {
        state.shared.push(WebViewEvent::TitleChanged(title));
    }
}

extern "C" fn on_notify_progress(
    _obj: *mut c_void,
    _pspec: *mut ffi::GParamSpec,
    data: *mut c_void,
) {
    let state = unsafe { state_from(data) };
    let p = unsafe { ffi::webkit_web_view_get_estimated_load_progress(state.view) };
    state.shared.push(WebViewEvent::LoadProgress(p));
}

extern "C" fn on_load_changed(_view: *mut c_void, event: c_uint, data: *mut c_void) {
    let state = unsafe { state_from(data) };
    if event == ffi::WEBKIT_LOAD_STARTED {
        state.shared.push(WebViewEvent::LoadProgress(0.0));
    }
    if event == ffi::WEBKIT_LOAD_FINISHED {
        state.shared.push(WebViewEvent::LoadFinished);
        push_history_state(state);
    }
}

extern "C" fn on_load_failed(
    _view: *mut c_void,
    _event: c_uint,
    failing_uri: *const c_char,
    error: *mut ffi::GError,
    data: *mut c_void,
) -> ffi::gboolean {
    let state = unsafe { state_from(data) };
    let uri = cstr_to_string(failing_uri).unwrap_or_default();
    let message = if error.is_null() {
        "unknown error".to_string()
    } else {
        cstr_to_string(unsafe { (*(error as *mut ffi::GErrorRepr)).message })
            .unwrap_or_else(|| "unknown error".to_string())
    };
    state.shared.push(WebViewEvent::LoadFailed { uri, error: message });
    ffi::GFALSE // let WebKit show its error page
}

extern "C" fn on_filter_saved(
    source: *mut c_void,
    result: *mut ffi::GAsyncResult,
    data: *mut c_void,
) {
    unsafe {
        let state = state_from(data);
        let store = source as *mut ffi::WebKitUserContentFilterStore;
        let mut error: *mut ffi::GError = std::ptr::null_mut();
        let filter =
            ffi::webkit_user_content_filter_store_save_finish(store, result, &mut error);
        if filter.is_null() {
            let msg = if error.is_null() {
                "unknown".to_string()
            } else {
                cstr_to_string((*(error as *mut ffi::GErrorRepr)).message).unwrap_or_default()
            };
            if !error.is_null() {
                ffi::g_error_free(error);
            }
            tracing::warn!("compiling content filter in WebKit failed: {msg}");
            return;
        }
        let ucm = ffi::webkit_web_view_get_user_content_manager(state.view);
        ffi::webkit_user_content_manager_add_filter(ucm, filter);
        tracing::info!("content filter installed");
    }
}

// ─────────────────────────── command dispatch ─────────────────────────────

extern "C" fn on_drain_commands(data: *mut c_void) -> ffi::gboolean {
    let state = unsafe { state_from(data) };
    while let Ok(cmd) = state.rx.try_recv() {
        dispatch(state, cmd);
    }
    // Fallback: if the initial navigation was deferred waiting for a
    // viewport that never came, load it once the grace period elapses so
    // the page isn't stuck blank.
    if state.pending_nav.is_some() {
        if let Some(deadline) = state.nav_deadline {
            if std::time::Instant::now() >= deadline {
                state.first_resize_seen = true;
                if let Some(uri) = state.pending_nav.take() {
                    state.nav_deadline = None;
                    load_uri(state, &uri);
                }
            }
        }
    }
    // Debounced UA reload: once resizing has been quiet for a moment,
    // reload so the current page re-serves at the new (mobile/desktop)
    // width. Deferred to here so a drag across the breakpoint doesn't
    // reload on every step.
    if state.ua_reload_pending {
        if let Some(t) = state.last_resize {
            if t.elapsed() > std::time::Duration::from_millis(350) {
                state.ua_reload_pending = false;
                unsafe { ffi::webkit_web_view_reload(state.view) };
            }
        }
    }
    // Install / clear ad blocking once the shared filter service is ready.
    #[cfg(feature = "webview-adblock")]
    reconcile_adblock(state);
    1 // keep the source
}

fn dispatch(state: &mut EngineState, cmd: WebViewCommand) {
    unsafe {
        match cmd {
            WebViewCommand::Navigate(uri) => {
                // Hold the very first navigation until the app reports the
                // real viewport, so the page loads once at the correct
                // size + UA instead of loading at the default and then
                // reloading (the startup flicker). A short fallback in the
                // drain loop still loads it if no Resize ever arrives.
                if !state.first_resize_seen {
                    state.pending_nav = Some(uri);
                    state.nav_deadline =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
                } else {
                    load_uri(state, &uri);
                }
            }
            WebViewCommand::GoBack => ffi::webkit_web_view_go_back(state.view),
            WebViewCommand::GoForward => ffi::webkit_web_view_go_forward(state.view),
            WebViewCommand::Reload => ffi::webkit_web_view_reload(state.view),
            WebViewCommand::StopLoading => ffi::webkit_web_view_stop_loading(state.view),
            WebViewCommand::Resize {
                width,
                height,
                scale_factor,
            } => {
                state.scale_factor = scale_factor.max(0.1);
                let lw = (width as f32 / state.scale_factor).round().max(1.0) as u32;
                let lh = (height as f32 / state.scale_factor).round().max(1.0) as u32;
                ffi::wpe_view_backend_dispatch_set_device_scale_factor(
                    state.backend,
                    state.scale_factor,
                );
                ffi::wpe_view_backend_dispatch_set_size(state.backend, lw, lh);

                // Present a phone UA below the breakpoint so UA-sniffing
                // sites serve their fits-any-width mobile layout. The UA
                // takes effect on the next load, so mark a reload pending
                // and fire it once the window stops resizing (see the
                // drain loop) — never mid-drag, which would flash blank.
                let want_mobile = wants_mobile(lw);
                if want_mobile != state.mobile_mode {
                    state.mobile_mode = want_mobile;
                    set_user_agent(
                        state.view,
                        if want_mobile { USER_AGENT_MOBILE } else { USER_AGENT_DESKTOP },
                    );
                    // Only an already-loaded page needs a reload to pick up
                    // the new UA; a deferred first navigation will simply
                    // load with it (handled below).
                    if state.pending_nav.is_none() {
                        state.ua_reload_pending = true;
                    }
                }
                state.last_resize = Some(std::time::Instant::now());

                // First viewport known: fire the held initial navigation
                // now that the size and UA are correct — one clean load.
                if !state.first_resize_seen {
                    state.first_resize_seen = true;
                    if let Some(uri) = state.pending_nav.take() {
                        state.nav_deadline = None;
                        load_uri(state, &uri);
                    }
                }
            }
            WebViewCommand::MouseMove { x, y } => {
                let mut event = ffi::wpe_input_pointer_event {
                    type_: ffi::WPE_INPUT_POINTER_EVENT_TYPE_MOTION,
                    time: 0,
                    x: logical(state, x),
                    y: logical(state, y),
                    button: 0,
                    state: 0,
                    modifiers: state.button_modifiers,
                };
                ffi::wpe_view_backend_dispatch_pointer_event(state.backend, &mut event);
            }
            WebViewCommand::MouseButton { button, down, x, y } => {
                let (number, modifier) = match button {
                    MouseButton::Left => (1, 1u32 << 20),
                    MouseButton::Middle => (2, 1u32 << 21),
                    MouseButton::Right => (3, 1u32 << 22),
                };
                if down {
                    state.button_modifiers |= modifier;
                } else {
                    state.button_modifiers &= !modifier;
                }
                let mut event = ffi::wpe_input_pointer_event {
                    type_: ffi::WPE_INPUT_POINTER_EVENT_TYPE_BUTTON,
                    time: 0,
                    x: logical(state, x),
                    y: logical(state, y),
                    button: number,
                    state: down as u32,
                    modifiers: state.button_modifiers,
                };
                ffi::wpe_view_backend_dispatch_pointer_event(state.backend, &mut event);
            }
            WebViewCommand::Scroll { dx, dy, x, y } => {
                let mut event = ffi::wpe_input_axis_2d_event {
                    base: ffi::wpe_input_axis_event {
                        type_: ffi::WPE_INPUT_AXIS_EVENT_TYPE_MOTION_SMOOTH
                            | ffi::WPE_INPUT_AXIS_EVENT_TYPE_MASK_2D,
                        time: 0,
                        x: logical(state, x),
                        y: logical(state, y),
                        axis: 0,
                        value: 0,
                        modifiers: state.button_modifiers,
                    },
                    x_axis: (dx / state.scale_factor) as f64,
                    y_axis: (dy / state.scale_factor) as f64,
                };
                ffi::wpe_view_backend_dispatch_axis_event(state.backend, &mut event.base);
            }
            WebViewCommand::Key { key, down, modifiers } => {
                // Translate held modifiers into WPE's bitmask so editing
                // shortcuts (Ctrl+Backspace, Ctrl+A, Shift+Arrow, …) reach
                // the page. OR in any held mouse-button modifiers so a
                // key pressed mid-drag carries the same state.
                let mut mods = state.button_modifiers;
                if modifiers.ctrl {
                    mods |= ffi::WPE_INPUT_KEYBOARD_MODIFIER_CONTROL;
                }
                if modifiers.shift {
                    mods |= ffi::WPE_INPUT_KEYBOARD_MODIFIER_SHIFT;
                }
                if modifiers.alt {
                    mods |= ffi::WPE_INPUT_KEYBOARD_MODIFIER_ALT;
                }
                if modifiers.meta {
                    mods |= ffi::WPE_INPUT_KEYBOARD_MODIFIER_META;
                }
                for key_code in keysyms(&key) {
                    let mut event = ffi::wpe_input_keyboard_event {
                        time: 0,
                        key_code,
                        hardware_key_code: 0,
                        pressed: down,
                        modifiers: mods,
                    };
                    ffi::wpe_view_backend_dispatch_keyboard_event(state.backend, &mut event);
                }
            }
            WebViewCommand::SetFocused(focused) => {
                if focused {
                    ffi::wpe_view_backend_add_activity_state(
                        state.backend,
                        ffi::WPE_VIEW_ACTIVITY_STATE_FOCUSED,
                    );
                } else {
                    ffi::wpe_view_backend_remove_activity_state(
                        state.backend,
                        ffi::WPE_VIEW_ACTIVITY_STATE_FOCUSED,
                    );
                }
            }
            WebViewCommand::SetContentFilter(Some(json)) => {
                let id = CString::new("qos-adblock").unwrap();
                let bytes = ffi::g_bytes_new(json.as_ptr() as *const c_void, json.len());
                ffi::webkit_user_content_filter_store_save(
                    state.filter_store,
                    id.as_ptr(),
                    bytes,
                    std::ptr::null_mut(),
                    on_filter_saved,
                    state as *mut EngineState as *mut c_void,
                );
                ffi::g_bytes_unref(bytes);
            }
            WebViewCommand::SetContentFilter(None) => {
                let ucm = ffi::webkit_web_view_get_user_content_manager(state.view);
                ffi::webkit_user_content_manager_remove_all_filters(ucm);
            }
            WebViewCommand::SetContentFilters { version, chunks } => {
                install_content_filters(state, version, &chunks);
            }
            WebViewCommand::SetUserScripts(scripts) => {
                install_user_scripts(state, &scripts);
            }
            WebViewCommand::SetAdBlocking(on) => {
                #[cfg(feature = "webview-adblock")]
                {
                    state.adblock_desired = on;
                    // Kick the shared loader eagerly so the compile overlaps
                    // the current page load; the install is reconciled in the
                    // drain loop once the filters are ready.
                    if on {
                        let _ = crate::webview::adblock_service::service();
                    }
                }
                #[cfg(not(feature = "webview-adblock"))]
                let _ = on;
            }
            WebViewCommand::ClearWebsiteData => {
                let session = ffi::webkit_web_view_get_network_session(state.view);
                if !session.is_null() {
                    let mgr = ffi::webkit_network_session_get_website_data_manager(session);
                    if !mgr.is_null() {
                        ffi::webkit_website_data_manager_clear(
                            mgr,
                            ffi::WEBKIT_WEBSITE_DATA_ALL,
                            0, // all time
                            std::ptr::null_mut(),
                            std::ptr::null(), // fire-and-forget
                            std::ptr::null_mut(),
                        );
                    }
                }
            }
        }
    }
}

/// Physical → logical device coordinate.
fn logical(state: &EngineState, v: f32) -> i32 {
    (v / state.scale_factor).round() as i32
}

/// X11-style keysym(s) for a key. Character input maps per code point
/// (Latin-1 directly, everything else via the Unicode keysym range).
fn keysyms(key: &KeyInput) -> Vec<u32> {
    match key {
        KeyInput::Character(text) => text
            .chars()
            .map(|ch| {
                let cp = ch as u32;
                if (0x20..=0xFF).contains(&cp) {
                    cp
                } else {
                    0x0100_0000 + cp
                }
            })
            .collect(),
        KeyInput::Named(named) => vec![match named {
            NamedKey::Enter => ffi::WPE_KEY_RETURN,
            NamedKey::Backspace => ffi::WPE_KEY_BACKSPACE,
            NamedKey::Delete => ffi::WPE_KEY_DELETE,
            NamedKey::Tab => ffi::WPE_KEY_TAB,
            NamedKey::Escape => ffi::WPE_KEY_ESCAPE,
            NamedKey::ArrowUp => ffi::WPE_KEY_UP,
            NamedKey::ArrowDown => ffi::WPE_KEY_DOWN,
            NamedKey::ArrowLeft => ffi::WPE_KEY_LEFT,
            NamedKey::ArrowRight => ffi::WPE_KEY_RIGHT,
            NamedKey::Home => ffi::WPE_KEY_HOME,
            NamedKey::End => ffi::WPE_KEY_END,
            NamedKey::PageUp => ffi::WPE_KEY_PAGE_UP,
            NamedKey::PageDown => ffi::WPE_KEY_PAGE_DOWN,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_ua_engages_only_below_the_breakpoint() {
        assert!(wants_mobile(390), "phone width → mobile UA");
        assert!(wants_mobile(MOBILE_BREAKPOINT_PX - 1));
        assert!(!wants_mobile(MOBILE_BREAKPOINT_PX), "at the breakpoint → desktop");
        assert!(!wants_mobile(1280), "desktop width → desktop UA");
    }

    #[test]
    fn keysym_mapping_is_stable() {
        assert_eq!(keysyms(&KeyInput::Character("a".into())), vec![0x61]);
        assert_eq!(keysyms(&KeyInput::Character("é".into())), vec![0xE9]);
        assert_eq!(
            keysyms(&KeyInput::Character("€".into())),
            vec![0x0100_0000 + 0x20AC]
        );
        assert_eq!(keysyms(&KeyInput::Named(NamedKey::Enter)), vec![0xff0d]);
    }
}
