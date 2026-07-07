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

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_uint, c_void};
use std::sync::mpsc::{Receiver, Sender, channel, sync_channel};
use std::sync::{Arc, Mutex};

use super::{
    Frame, FrameSlot, KeyInput, MouseButton, NamedKey, WebViewCommand, WebViewEngine, WebViewEvent,
};

/// Command-drain cadence on the engine thread's GLib loop.
const DRAIN_INTERVAL_MS: c_uint = 8;

/// `wl_shm` formats we understand (both 32-bit little-endian, i.e. BGRA
/// / BGRX byte order in memory).
const WL_SHM_FORMAT_ARGB8888: u32 = 0;
const WL_SHM_FORMAT_XRGB8888: u32 = 1;

/// Shared between the engine thread and the widget/app side.
struct Shared {
    frames: FrameSlot,
    events: Mutex<Vec<WebViewEvent>>,
    /// Host wake callback — pokes the winit loop so an idle app paints
    /// engine activity promptly.
    waker: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
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
            events: Mutex::new(Vec::new()),
            waker: Mutex::new(None),
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
        // The engine thread drains on a fixed cadence; a dropped send
        // only happens if the thread died, which `new()` guards.
        let _ = self.tx.send(cmd);
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

        let fdo_lib = CString::new("libWPEBackend-fdo-1.0.so.1").unwrap();
        if !ffi::wpe_loader_init(fdo_lib.as_ptr()) {
            let _ = init_tx.send(Err("wpe_loader_init(libWPEBackend-fdo) failed".into()));
            return;
        }
        if !ffi::wpe_fdo_initialize_shm() {
            let _ = init_tx.send(Err("wpe_fdo_initialize_shm failed".into()));
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
        }));
        let state_ptr = state as *mut EngineState as *mut c_void;

        static CLIENT: ffi::wpe_view_backend_exportable_fdo_client =
            ffi::wpe_view_backend_exportable_fdo_client {
                export_buffer_resource: None,
                export_dmabuf_resource: None,
                export_shm_buffer: Some(on_export_shm_buffer),
                _wpe_reserved0: None,
                _wpe_reserved1: None,
            };

        state.exportable =
            ffi::wpe_view_backend_exportable_fdo_create(&CLIENT, state_ptr, 1024, 768);
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
    base.join("qos-browser").join("filter-store")
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

fn cstr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
}

// ─────────────────────────── frame export ─────────────────────────────────

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
                let opaque = format == WL_SHM_FORMAT_XRGB8888;
                let mut rgba = vec![0u8; width as usize * height as usize * 4];
                for row in 0..height as usize {
                    let src_row = std::slice::from_raw_parts(
                        src.add(row * stride),
                        width as usize * 4,
                    );
                    let dst_row =
                        &mut rgba[row * width as usize * 4..(row + 1) * width as usize * 4];
                    // Little-endian [AX]RGB8888 in memory is B,G,R,[AX].
                    for px in 0..width as usize {
                        dst_row[px * 4] = src_row[px * 4 + 2];
                        dst_row[px * 4 + 1] = src_row[px * 4 + 1];
                        dst_row[px * 4 + 2] = src_row[px * 4];
                        dst_row[px * 4 + 3] = if opaque { 0xFF } else { src_row[px * 4 + 3] };
                    }
                }
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
    1 // keep the source
}

fn dispatch(state: &mut EngineState, cmd: WebViewCommand) {
    unsafe {
        match cmd {
            WebViewCommand::Navigate(uri) => {
                if let Ok(c) = CString::new(uri) {
                    ffi::webkit_web_view_load_uri(state.view, c.as_ptr());
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
            WebViewCommand::Key { key, down } => {
                for key_code in keysyms(&key) {
                    let mut event = ffi::wpe_input_keyboard_event {
                        time: 0,
                        key_code,
                        hardware_key_code: 0,
                        pressed: down,
                        modifiers: 0,
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
