//! WPE WebKit engine backend (feature `webview-wpe`).
//!
//! Runs WebKit's WPE port fully offscreen on a dedicated engine thread: a
//! WPEBackend-fdo "exportable" view backend in SHM mode hands us every
//! composited frame as a shared-memory buffer, which we convert to RGBA and
//! publish into the widget's [`FrameSlot`]. Commands arrive over an mpsc
//! channel drained by a GLib timeout source on the engine thread; navigation
//! state flows back as [`WebViewEvent`]s from GObject signal handlers.
//!
//! The SHM path costs one CPU copy and a BGRA→RGBA swizzle per frame. The
//! backend trait also permits a direct DMA-BUF import implementation.
//!
//! Requires the WPE stack built by `os/dev/build-wpe.sh` (Fedora ships no WPE
//! packages); point `PKG_CONFIG_PATH`/`LD_LIBRARY_PATH` at the prefix.
//!
//! The engine thread runs for the process lifetime — a browser window owns
//! exactly one engine. Dropping `WpeEngine` leaks the thread by design
//! (WebKit teardown is not worth the shutdown races for a per-window process).
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: WPE WebKit runs on a dedicated engine thread"
)]

pub mod egl;
mod ffi;
pub mod import;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel, sync_channel};
use std::sync::{Arc, Mutex};

use super::{
    Frame, FrameSlot, KeyInput, MediaElementInfo, MouseButton, NamedKey, WebViewCommand,
    WebViewEngine, WebViewEvent,
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
    dmabuf: Mutex<Option<egl::DmabufFrame>>,
    events: Mutex<Vec<WebViewEvent>>,
    /// Pokes the winit loop so an idle app paints engine activity promptly.
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// The engine thread's `GMainContext`, stored as a pointer once the
    /// thread has created it (0 until then). `g_main_context_wakeup` is
    /// explicitly thread-safe.
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

        match init_rx.recv_timeout(crate::time::Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(Arc::new(Self { tx, shared })),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("WPE engine initialisation timed out".into()),
        }
    }
}

impl WebViewEngine for WpeEngine {
    fn send(&self, cmd: WebViewCommand) {
        let _ = self.tx.send(cmd);
        // Wake the engine loop so the command is drained now instead of
        // waiting up to the timer interval.
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

    /// The caller imports it into a wgpu texture; the engine publishes a
    /// fresh one on the next paint.
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
    mobile_mode: bool,
    /// Waiting for a reload once the window stops resizing, so dragging
    /// across the mobile breakpoint doesn't flash blank on every step.
    ua_reload_pending: bool,
    /// To debounce that reload.
    last_resize: Option<crate::time::Instant>,
    /// Until the first Resize, the real viewport (and correct UA) is
    /// unknown, so the initial navigation is held in `pending_nav` to avoid
    /// loading at the wrong size and reloading — the startup flicker.
    first_resize_seen: bool,
    /// A Navigate that arrived before the first Resize, deferred until the
    /// viewport is known. Also fired by `nav_deadline` as a fallback so a
    /// window that never resizes still loads.
    pending_nav: Option<String>,
    nav_deadline: Option<crate::time::Instant>,
    downloads: std::collections::HashMap<String, *mut ffi::WebKitDownload>,
    download_ids: std::collections::HashMap<usize, String>,
    cancelled_downloads: std::collections::HashSet<String>,
    download_progress_emitted_at: std::collections::HashMap<String, crate::time::Instant>,
    /// `Some` only on the zero-copy path (`TILO_WEBVIEW_DMABUF=1`); `None`
    /// uses the SHM copy path.
    egl: Option<egl::EglExport>,
}

struct MediaProbe {
    state: *mut EngineState,
    request_id: u64,
}

const MEDIA_ELEMENT_PROBE: &str = r#"(() => {
  const vw = Math.max(window.innerWidth, 1);
  const vh = Math.max(window.innerHeight, 1);
  const videos = Array.from(document.querySelectorAll('video'));
  let best = null;
  let bestScore = 0;
  for (const video of videos) {
    const r = video.getBoundingClientRect();
    const style = getComputedStyle(video);
    const x0 = Math.max(0, r.left), y0 = Math.max(0, r.top);
    const x1 = Math.min(vw, r.right), y1 = Math.min(vh, r.bottom);
    const visible = Math.max(0, x1 - x0) * Math.max(0, y1 - y0);
    if (r.width < 80 || r.height < 45 || visible < 3600 ||
        style.display === 'none' || style.visibility === 'hidden' ||
        Number(style.opacity) < 0.05) continue;
    const score = visible * (!video.paused && !video.ended ? 4 : 1) *
      (video.readyState >= 2 ? 1.5 : 1);
    if (score > bestScore) { best = { video, r, x0, y0, x1, y1 }; bestScore = score; }
  }
  if (!best) return '';
  const v = best.video;
  const ids = window.__tiloPipElementIds || (window.__tiloPipElementIds = new WeakMap());
  const elements = window.__tiloPipElements || (window.__tiloPipElements = new Map());
  let id = ids.get(v);
  if (!id) {
    const next = (window.__tiloPipElementSequence || 0) + 1;
    window.__tiloPipElementSequence = next;
    id = `media:${next}`;
    ids.set(v, id);
  }
  elements.set(id, v);
  const label = v.getAttribute('aria-label') || v.title ||
    document.title || 'Browser video';
  return JSON.stringify({
    id,
    rect: [best.x0 / vw, best.y0 / vh,
      (best.x1 - best.x0) / vw, (best.y1 - best.y0) / vh],
    size: [v.videoWidth || Math.round(best.r.width),
      v.videoHeight || Math.round(best.r.height)],
    label
  });
})()"#;

const MEDIA_ELEMENT_REFRESH_PROBE: &str = r#"(() => {
  const id = __TILO_ELEMENT_ID__;
  const v = window.__tiloPipElements && window.__tiloPipElements.get(id);
  if (!v || !v.isConnected) return '';
  const vw = Math.max(window.innerWidth, 1);
  const vh = Math.max(window.innerHeight, 1);
  const r = v.getBoundingClientRect();
  const style = getComputedStyle(v);
  const x0 = Math.max(0, r.left), y0 = Math.max(0, r.top);
  const x1 = Math.min(vw, r.right), y1 = Math.min(vh, r.bottom);
  const visible = Math.max(0, x1 - x0) * Math.max(0, y1 - y0);
  if (r.width < 80 || r.height < 45 || visible < 3600 ||
      style.display === 'none' || style.visibility === 'hidden' ||
      Number(style.opacity) < 0.05) return '';
  const label = v.getAttribute('aria-label') || v.title ||
    document.title || 'Browser video';
  return JSON.stringify({
    id,
    rect: [x0 / vw, y0 / vh, (x1 - x0) / vw, (y1 - y0) / vh],
    size: [v.videoWidth || Math.round(r.width),
      v.videoHeight || Math.round(r.height)],
    label
  });
})()"#;

fn media_element_refresh_probe(element_id: &str) -> Option<CString> {
    let encoded = serde_json::to_string(element_id).ok()?;
    CString::new(MEDIA_ELEMENT_REFRESH_PROBE.replace("__TILO_ELEMENT_ID__", &encoded)).ok()
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
        let egl = if std::env::var_os("TILO_WEBVIEW_DMABUF").is_some() {
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
            downloads: std::collections::HashMap::new(),
            download_ids: std::collections::HashMap::new(),
            cancelled_downloads: std::collections::HashSet::new(),
            download_progress_emitted_at: std::collections::HashMap::new(),
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
        state.backend = ffi::wpe_view_backend_exportable_fdo_get_view_backend(state.exportable);

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

        // Advertise a modern desktop Safari UA (accurate for the WebKit
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

        connect(
            state.view,
            "notify::uri",
            on_notify_uri as *const c_void,
            state_ptr,
        );
        let network_session = ffi::webkit_web_view_get_network_session(state.view);
        if !network_session.is_null() {
            connect(
                network_session.cast(),
                "download-started",
                on_download_started as *const c_void,
                state_ptr,
            );
        }
        connect(
            state.view,
            "notify::title",
            on_notify_title as *const c_void,
            state_ptr,
        );
        connect(
            state.view,
            "notify::estimated-load-progress",
            on_notify_progress as *const c_void,
            state_ptr,
        );
        connect(
            state.view,
            "load-changed",
            on_load_changed as *const c_void,
            state_ptr,
        );
        connect(
            state.view,
            "load-failed",
            on_load_failed as *const c_void,
            state_ptr,
        );

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
    let base = tilo_paths::xdg::cache_home()
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default();
    base.join("tilo-browser").join("filter-store")
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
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

// ─────────────────────────── frame export ─────────────────────────────────

/// DMA-BUF export callback (zero-copy path). WebKit hands us a
/// GPU-rendered `EGLImageKHR`; we export it to a dmabuf fd for the render
/// thread to import.
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
                let swizzle_start = crate::time::now();
                let opaque = format == WL_SHM_FORMAT_XRGB8888;
                let w = width as usize;
                let n = w * height as usize * 4;
                // Uninitialised: every byte is written below, so a zero-init
                // Vec would be a wasted memset per frame. Swap B<->R a whole
                // 32-bit pixel at a time (LLVM vectorises the word op):
                // source word is `A<<24|R<<16|G<<8|B`, RGBA wants
                // `A<<24|B<<16|G<<8|R`.
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
                    trace_id: None,
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

extern "C" fn on_notify_uri(_obj: *mut c_void, _pspec: *mut ffi::GParamSpec, data: *mut c_void) {
    let state = unsafe { state_from(data) };
    if let Some(uri) = cstr_to_string(unsafe { ffi::webkit_web_view_get_uri(state.view) }) {
        state.shared.push(WebViewEvent::UriChanged(uri));
    }
    push_history_state(state);
}

extern "C" fn on_notify_title(_obj: *mut c_void, _pspec: *mut ffi::GParamSpec, data: *mut c_void) {
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
    state.shared.push(WebViewEvent::LoadFailed {
        uri,
        error: message,
    });
    ffi::GFALSE // let WebKit show its error page
}

extern "C" fn on_download_started(
    _session: *mut c_void,
    download: *mut ffi::WebKitDownload,
    data: *mut c_void,
) {
    if download.is_null() {
        return;
    }
    let state = unsafe { state_from(data) };
    let mut id = uuid::Uuid::new_v4().to_string();
    while state.downloads.contains_key(&id) {
        id = uuid::Uuid::new_v4().to_string();
    }
    state.downloads.insert(id.clone(), download);
    state.download_ids.insert(download as usize, id);
    unsafe {
        connect(
            download.cast(),
            "decide-destination",
            on_download_decide_destination as *const c_void,
            data,
        );
        connect(
            download.cast(),
            "received-data",
            on_download_received_data as *const c_void,
            data,
        );
        connect(
            download.cast(),
            "finished",
            on_download_finished as *const c_void,
            data,
        );
        connect(
            download.cast(),
            "failed",
            on_download_failed as *const c_void,
            data,
        );
    }
}

extern "C" fn on_download_decide_destination(
    download: *mut ffi::WebKitDownload,
    suggested_filename: *const c_char,
    data: *mut c_void,
) -> ffi::gboolean {
    let state = unsafe { state_from(data) };
    let Some(id) = state.download_ids.get(&(download as usize)).cloned() else {
        return ffi::GFALSE;
    };
    let request = unsafe { ffi::webkit_download_get_request(download) };
    let uri = if request.is_null() {
        String::new()
    } else {
        cstr_to_string(unsafe { ffi::webkit_uri_request_get_uri(request) }).unwrap_or_default()
    };
    let suggested_filename = cstr_to_string(suggested_filename)
        .unwrap_or_else(|| "download".into())
        .chars()
        .take(255)
        .collect();
    state.shared.push(WebViewEvent::DownloadRequested {
        id,
        uri,
        suggested_filename,
    });
    1 // WebKit 2.40+: wait asynchronously for destination or cancellation.
}

extern "C" fn on_download_received_data(
    download: *mut ffi::WebKitDownload,
    _chunk_bytes: u64,
    data: *mut c_void,
) {
    let state = unsafe { state_from(data) };
    let Some(id) = state.download_ids.get(&(download as usize)).cloned() else {
        return;
    };
    let now = crate::time::now();
    if state
        .download_progress_emitted_at
        .get(&id)
        .is_some_and(|last| now.duration_since(*last) < crate::time::Duration::from_millis(150))
    {
        return;
    }
    state.download_progress_emitted_at.insert(id.clone(), now);
    let response = unsafe { ffi::webkit_download_get_response(download) };
    let total = if response.is_null() {
        None
    } else {
        let bytes = unsafe { ffi::webkit_uri_response_get_content_length(response) };
        (bytes > 0).then_some(bytes)
    };
    state.shared.push(WebViewEvent::DownloadProgress {
        id,
        received_bytes: unsafe { ffi::webkit_download_get_received_data_length(download) },
        total_bytes: total,
        progress: unsafe { ffi::webkit_download_get_estimated_progress(download) }.clamp(0.0, 1.0),
    });
}

fn finish_download(state: &mut EngineState, download: *mut ffi::WebKitDownload) -> Option<String> {
    let id = state.download_ids.remove(&(download as usize))?;
    state.downloads.remove(&id);
    state.download_progress_emitted_at.remove(&id);
    Some(id)
}

extern "C" fn on_download_finished(download: *mut ffi::WebKitDownload, data: *mut c_void) {
    let state = unsafe { state_from(data) };
    let destination_uri = cstr_to_string(unsafe { ffi::webkit_download_get_destination(download) })
        .unwrap_or_default();
    let destination = url::Url::parse(&destination_uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or(destination_uri);
    if let Some(id) = finish_download(state, download) {
        state.cancelled_downloads.remove(&id);
        state.shared.push(WebViewEvent::DownloadFinished {
            id,
            path: destination,
        });
    }
}

extern "C" fn on_download_failed(
    download: *mut ffi::WebKitDownload,
    error: *mut ffi::GError,
    data: *mut c_void,
) {
    let state = unsafe { state_from(data) };
    let message = if error.is_null() {
        "Download failed".into()
    } else {
        cstr_to_string(unsafe { (*(error as *mut ffi::GErrorRepr)).message })
            .unwrap_or_else(|| "Download failed".into())
    };
    if let Some(id) = finish_download(state, download) {
        let cancelled = state.cancelled_downloads.remove(&id);
        state.shared.push(WebViewEvent::DownloadFailed {
            id,
            error: message,
            cancelled,
        });
    }
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
        let filter = ffi::webkit_user_content_filter_store_save_finish(store, result, &mut error);
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

extern "C" fn on_media_element(
    source: *mut c_void,
    result: *mut ffi::GAsyncResult,
    data: *mut c_void,
) {
    unsafe {
        let probe = Box::from_raw(data.cast::<MediaProbe>());
        let state = &mut *probe.state;
        let mut error: *mut ffi::GError = std::ptr::null_mut();
        let value = ffi::webkit_web_view_evaluate_javascript_finish(
            source.cast::<ffi::WebKitWebView>(),
            result,
            &mut error,
        );
        if !error.is_null() {
            ffi::g_error_free(error);
        }
        let element = if value.is_null() {
            None
        } else {
            let raw = ffi::jsc_value_to_string(value);
            let parsed = if raw.is_null() {
                None
            } else {
                let json = CStr::from_ptr(raw).to_string_lossy().into_owned();
                ffi::g_free(raw.cast());
                parse_media_element(&json)
            };
            ffi::g_object_unref(value.cast());
            parsed
        };
        state.shared.push(element.map_or(
            WebViewEvent::MediaElementUnavailable {
                request_id: probe.request_id,
            },
            |element| WebViewEvent::MediaElement {
                request_id: probe.request_id,
                element,
            },
        ));
    }
}

fn parse_media_element(json: &str) -> Option<MediaElementInfo> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let rect = value.get("rect")?.as_array()?;
    let size = value.get("size")?.as_array()?;
    if rect.len() != 4 || size.len() != 2 {
        return None;
    }
    let mut viewport_rect = [0.0; 4];
    for (slot, value) in viewport_rect.iter_mut().zip(rect) {
        *slot = value.as_f64()?.clamp(0.0, 1.0);
    }
    if viewport_rect[2] <= 0.0 || viewport_rect[3] <= 0.0 {
        return None;
    }
    let element_id: String = value
        .get("id")?
        .as_str()?
        .trim()
        .chars()
        .take(128)
        .collect();
    if element_id.is_empty() {
        return None;
    }
    Some(MediaElementInfo {
        element_id,
        viewport_rect,
        intrinsic_size: [
            u32::try_from(size[0].as_u64()?).ok()?.max(1),
            u32::try_from(size[1].as_u64()?).ok()?.max(1),
        ],
        label: value
            .get("label")
            .and_then(serde_json::Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .unwrap_or("Browser video")
            .trim()
            .chars()
            .take(96)
            .collect(),
    })
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
            if crate::time::now() >= deadline {
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
            if t.elapsed() > crate::time::Duration::from_millis(350) {
                state.ua_reload_pending = false;
                unsafe { ffi::webkit_web_view_reload(state.view) };
            }
        }
    }
    1 // keep the source
}

fn dispatch(state: &mut EngineState, cmd: WebViewCommand) {
    unsafe {
        match cmd {
            WebViewCommand::Navigate(uri) => {
                // Hold the very first navigation until the real viewport is
                // known (the startup flicker); the drain loop's fallback
                // still loads it if no Resize ever arrives.
                if !state.first_resize_seen {
                    state.pending_nav = Some(uri);
                    state.nav_deadline =
                        Some(crate::time::now() + crate::time::Duration::from_millis(250));
                } else {
                    load_uri(state, &uri);
                }
            }
            WebViewCommand::RequestNavigationHistory { .. }
            | WebViewCommand::NavigateToHistoryEntry { .. } => {}
            WebViewCommand::GoBack => ffi::webkit_web_view_go_back(state.view),
            WebViewCommand::GoForward => ffi::webkit_web_view_go_forward(state.view),
            WebViewCommand::Reload => ffi::webkit_web_view_reload(state.view),
            WebViewCommand::StopLoading => ffi::webkit_web_view_stop_loading(state.view),
            WebViewCommand::SetAudioMuted(_) => {}
            // The WPE engine never enters document or video fullscreen, so
            // there is nothing to leave.
            WebViewCommand::ExitFullscreen => {}
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

                // The UA takes effect on the next load, so mark a reload
                // pending and fire it once resizing stops (see the drain
                // loop) — never mid-drag, which would flash blank.
                let want_mobile = wants_mobile(lw);
                if want_mobile != state.mobile_mode {
                    state.mobile_mode = want_mobile;
                    set_user_agent(
                        state.view,
                        if want_mobile {
                            USER_AGENT_MOBILE
                        } else {
                            USER_AGENT_DESKTOP
                        },
                    );
                    // Only an already-loaded page needs a reload to pick up
                    // the new UA; a deferred first navigation will simply
                    // load with it (handled below).
                    if state.pending_nav.is_none() {
                        state.ua_reload_pending = true;
                    }
                }
                state.last_resize = Some(crate::time::now());

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
            WebViewCommand::Key {
                key,
                down,
                modifiers,
            } => {
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
                let id = CString::new("tilo-adblock").unwrap();
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
                // Replace all filters, then compile+install each chunk fresh.
                //
                // We do NOT load a previously compiled filter back from the
                // store: loading a cached WKContentRuleList (rather than
                // compiling it this run) reproducibly blanked whole pages a
                // few seconds after load — the compiled-fresh filter is fine,
                // the loaded one is not. So always `save` (compile). `version`
                // only keeps the on-disk id stable across launches.
                let ucm = ffi::webkit_web_view_get_user_content_manager(state.view);
                ffi::webkit_user_content_manager_remove_all_filters(ucm);
                for (i, json) in chunks.iter().enumerate() {
                    let Ok(id) = CString::new(format!("tilo-adblock-{version:016x}-{i}")) else {
                        continue;
                    };
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
            }
            WebViewCommand::SetUserScripts(scripts) => {
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
            WebViewCommand::RequestMediaElement { request_id } => {
                if let Ok(script) = CString::new(MEDIA_ELEMENT_PROBE) {
                    let probe = Box::new(MediaProbe {
                        state: state as *mut EngineState,
                        request_id,
                    });
                    ffi::webkit_web_view_evaluate_javascript(
                        state.view,
                        script.as_ptr(),
                        -1,
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        on_media_element,
                        Box::into_raw(probe).cast(),
                    );
                } else {
                    state
                        .shared
                        .push(WebViewEvent::MediaElementUnavailable { request_id });
                }
            }
            WebViewCommand::RequestMediaElementById {
                request_id,
                element_id,
            } => {
                if let Some(script) = media_element_refresh_probe(&element_id) {
                    let probe = Box::new(MediaProbe {
                        state: state as *mut EngineState,
                        request_id,
                    });
                    ffi::webkit_web_view_evaluate_javascript(
                        state.view,
                        script.as_ptr(),
                        -1,
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        on_media_element,
                        Box::into_raw(probe).cast(),
                    );
                } else {
                    state
                        .shared
                        .push(WebViewEvent::MediaElementUnavailable { request_id });
                }
            }
            WebViewCommand::Page { request_id, .. } => {
                state.shared.push(WebViewEvent::PageResult {
                    request_id,
                    result: Err(super::PageFailure::unavailable()),
                });
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
            WebViewCommand::SetClearOnExit(_)
            | WebViewCommand::ShowDeveloperTools
            | WebViewCommand::PermissionDecision { .. }
            | WebViewCommand::DownloadDecision { .. }
            | WebViewCommand::Find { .. }
            | WebViewCommand::ContextMenuAction { .. }
            | WebViewCommand::StopFinding
            | WebViewCommand::SetZoomLevel(_)
            | WebViewCommand::CancelNativeDownload { .. }
            | WebViewCommand::RetryDownload { .. }
            | WebViewCommand::CancelAllDownloads
            | WebViewCommand::SetNativeFilterRules { .. }
            | WebViewCommand::SetBlockingScripts(_)
            | WebViewCommand::SetSiteExceptions(_)
            | WebViewCommand::RequestSiteStatus { .. }
            | WebViewCommand::Credentials(_)
            | WebViewCommand::SiteAction { .. } => {}
            WebViewCommand::StartDownload { uri } => {
                if (uri.starts_with("https://") || uri.starts_with("http://"))
                    && let Ok(uri) = CString::new(uri)
                {
                    ffi::webkit_web_view_download_uri(state.view, uri.as_ptr());
                }
            }
            WebViewCommand::SetDownloadDestination { id, path } => {
                if std::path::Path::new(&path).is_absolute()
                    && let Some(download) = state.downloads.get(&id).copied()
                    && let Ok(destination) = url::Url::from_file_path(&path)
                    && let Ok(destination) = CString::new(destination.as_str())
                {
                    ffi::webkit_download_set_destination(download, destination.as_ptr());
                }
            }
            WebViewCommand::CancelDownload { id } => {
                if let Some(download) = state.downloads.get(&id).copied() {
                    state.cancelled_downloads.insert(id);
                    ffi::webkit_download_cancel(download);
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
    fn wpe_input_mapping_is_stable_and_mobile_ua_engages_below_the_breakpoint() {
        // phase: mobile_ua_engages_only_below_the_breakpoint
        {
            assert!(wants_mobile(390), "phone width → mobile UA");
            assert!(wants_mobile(MOBILE_BREAKPOINT_PX - 1));
            assert!(
                !wants_mobile(MOBILE_BREAKPOINT_PX),
                "at the breakpoint → desktop"
            );
            assert!(!wants_mobile(1280), "desktop width → desktop UA");
        }
        // phase: keysym_mapping_is_stable
        {
            assert_eq!(keysyms(&KeyInput::Character("a".into())), vec![0x61]);
            assert_eq!(keysyms(&KeyInput::Character("é".into())), vec![0xE9]);
            assert_eq!(
                keysyms(&KeyInput::Character("€".into())),
                vec![0x0100_0000 + 0x20AC]
            );
            assert_eq!(keysyms(&KeyInput::Named(NamedKey::Enter)), vec![0xff0d]);
        }
    }
}
