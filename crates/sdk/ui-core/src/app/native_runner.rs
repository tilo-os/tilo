//! The native event-loop runner: the winit window, render thread, AccessKit
//! adapter and surface host around [`App`]. Compiled with the `native`
//! feature; [`App::run`] is its entry point.
//!
//! It lives beside `App` rather than in [`crate::native`] because it drives
//! `App`'s private state directly; application semantics remain on `App` and
//! its responsibility-specific implementation units.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "linux")]
mod linux_header;

#[cfg(not(target_os = "android"))]
type PrimaryA11yAdapter = accesskit_winit::Adapter;
#[cfg(target_os = "android")]
type PrimaryA11yAdapter = crate::accesskit_android_adapter::Adapter;

/// wgpu `RenderContext`, `RenderSurface`, and `vello::Renderer` live on a
/// dedicated worker thread (see [`crate::render_thread`]) — this struct
/// keeps only the channel ends to ship frames and recycle scene buffers.
struct AppHandler {
    app: App,
    animation_schedule: animation_schedule::AnimationSchedule,
    native_scenario: Option<crate::native_scenario::Scenario>,
    native_input_ids: Vec<usize>,
    native_input_origin: Option<Instant>,
    native_input_pointer_passthrough: bool,
    native_input_route_isolation: bool,
    /// `None` until the first `resumed` — every modern platform gates window
    /// creation behind an `ActiveEventLoop`.
    window: Option<Arc<Window>>,
    /// `None` again in `exiting`, after the worker is joined, so its surface
    /// drops BEFORE the `Window` arc goes away.
    render: Option<RenderThreadHandle>,
    /// Between `suspended` and the next `resumed` the window has no native
    /// surface to render into, so a redraw must not try to create one.
    surface_suspended: bool,
    /// What `suspended` took back from the render worker, for the next
    /// surface.
    suspended_renderer: Option<PreparedRenderer>,
    /// A real, separate fullscreen output for a root widget's audience view.
    /// It is deliberately not modelled as a floating surface: it has no
    /// desktop-space proxy or compositor handoff and is always destroyed
    /// before its renderer/window during stop and app teardown.
    audience_presentation: Option<AudiencePresentationWindow>,
    floating_windows: Vec<NativeFloatingWindow>,
    /// `None` until `resumed` creates the window (must be built while
    /// hidden); lazy, so the hot path pays nothing with no screen reader
    /// running.
    a11y_adapter: Option<PrimaryA11yAdapter>,
    /// Also carries host-chrome requests back into the loop.
    a11y_proxy: EventLoopProxy<UserEvent>,
    /// Last text-input-v3 state applied to winit. Edge-triggering this avoids
    /// emitting a protocol commit on every idle loop.
    ime_state: Option<ImeWindowState>,
    #[cfg(target_os = "android")]
    text_input: super::android_text_input::TextInputSession,
    /// The field with the keyboard was touched since the last IME sync.
    #[cfg(target_os = "android")]
    text_input_touched: bool,
    primary_frames_sent: u64,
    primary_frames_completed: u64,
    last_primary_frame_presented: u64,
    primary_retry: crate::render_thread::retry::RenderRetryState,
    /// `None` waits for the next primary submission; `Some(sequence)` waits
    /// until that exact-or-newer frame is reported Presented by the worker.
    floating_source_clear_frames: std::collections::HashMap<String, Option<u64>>,
    /// Reverse ownership gate. A suspended native surface remains covered by
    /// its compositor-frozen final frame until one primary app frame with the
    /// reattached widget is reported Presented.
    floating_destination_present_frames: std::collections::HashMap<String, Option<u64>>,
    /// Maps one-finger taps onto the existing pointer event contract so every
    /// standard and app-local control gets touch without bespoke branches.
    touch_pointer: crate::interaction::TouchPointerEmulator,
    /// Synthetic mouse events must retain touch modality rather than making
    /// the host immediately classify them as a fine pointer.
    dispatching_touch_pointer: bool,
    /// Per-window state handed to the next in-place reload. Keyed by the
    /// window id that stays for the life of the process.
    dev_window_state: std::collections::HashMap<String, Vec<u8>>,
    /// The reload library this process runs. The dev loop stages the first one
    /// from the build that produced this host, so it starts as the running code.
    dev_loaded_library: Option<std::path::PathBuf>,
    /// The last in-place reload, until its first frame is submitted.
    dev_reload_timing: Option<super::DevReloadTiming>,
    /// The menu bar and title-bar search field, when the host draws them.
    #[cfg(target_os = "macos")]
    host_chrome: Option<crate::host_chrome::macos::MacChrome>,
    /// The menu bar and Jump List, when the host draws them.
    #[cfg(windows)]
    host_chrome: Option<crate::host_chrome::windows::WindowsChrome>,
    /// What the host chrome last presented, to skip unchanged refreshes.
    #[cfg(any(target_os = "macos", windows))]
    host_commands_key: Option<super::host_commands::HostCommandsKey>,
    /// Who decorates the window, and the header bar when the app does.
    #[cfg(target_os = "linux")]
    decorations: linux_header::Decorations,
    /// The host OS's appearance, for a standalone app outside Tilo OS.
    host_appearance: Option<crate::host_appearance::Source>,
    /// Settings.json changed while this process was idle. The next
    /// `about_to_wait` skips the 750ms poll debounce and re-reads Theme.
    settings_store_changed: Arc<AtomicBool>,
    /// Lives for the event loop so an idle app still hears Theme Studio
    /// and compositor appearance writes.
    _settings_watch: Option<settings_store::watch::SettingsWatch>,
}

#[cfg(target_os = "macos")]
impl AppHandler {
    /// Put History, Search and Undo into the menu bar and title bar.
    fn install_host_chrome(&mut self, window: &Window) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if crate::host_chrome::HostChrome::current(self.app.input.platform)
            != crate::host_chrome::HostChrome::Native
        {
            return;
        }
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        let proxy = self.a11y_proxy.clone();
        let chrome = crate::host_chrome::macos::MacChrome::new(mtm, move |request| {
            let _ = proxy.send_event(match request {
                crate::host_chrome::macos::ChromeRequest::Command(invocation) => {
                    UserEvent::HostCommand(invocation)
                }
                crate::host_chrome::macos::ChromeRequest::Search(query) => {
                    UserEvent::HostSearch(query)
                }
            });
        });
        chrome.install(&objc2_app_kit::NSApplication::sharedApplication(mtm));
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::AppKit(handle) = handle.as_raw()
        {
            // SAFETY: winit hands out the live NSView of a window it owns, on
            // the main thread, for at least the duration of this call.
            let view = unsafe { handle.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
            chrome.attach_toolbar(view);
        }
        self.host_chrome = Some(chrome);
        self.host_commands_key = None;
    }
}

#[cfg(any(target_os = "macos", windows))]
impl AppHandler {
    /// Hand the host chrome fresh commands when anything they show changed.
    fn refresh_host_chrome(&mut self) {
        if let Some(chrome) = &self.host_chrome {
            let key = self.app.host_commands_key();
            if self.host_commands_key != Some(key) {
                self.host_commands_key = Some(key);
                chrome.update(self.app.host_commands(true));
            }
        }
    }
}

#[cfg(windows)]
impl AppHandler {
    /// Put History, Search and Undo into the window's menu bar and the Jump
    /// List, and run the command a Jump List task launched this process with.
    fn install_host_chrome(&mut self, window: &Window) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let proxy = self.a11y_proxy.clone();
        if let Some(invocation) = crate::host_chrome::windows::take_startup_command() {
            let _ = proxy.send_event(UserEvent::HostCommand(invocation));
        }
        if crate::host_chrome::HostChrome::current(self.app.input.platform)
            != crate::host_chrome::HostChrome::Native
        {
            return;
        }
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut std::ffi::c_void);
        self.host_chrome = Some(crate::host_chrome::windows::WindowsChrome::install(
            hwnd,
            move |invocation| {
                let _ = proxy.send_event(UserEvent::HostCommand(invocation));
            },
        ));
        self.host_commands_key = None;
    }
}

/// The reload library the dev session currently points at.
fn dev_reload_library(session: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_to_string(session.join("reload.path"))
        .ok()
        .map(|text| std::path::PathBuf::from(text.trim()))
        .filter(|path| path.is_file())
}

struct AudiencePresentationWindow {
    monitor: winit::monitor::MonitorHandle,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
}

/// Keep the audience bound to the same monitor object across hotplug list
/// reordering; indices are only a one-tick selection aid.
fn audience_monitor_still_selected<T: PartialEq>(
    current: Option<&T>,
    selected: Option<&T>,
) -> bool {
    current
        .zip(selected)
        .is_some_and(|(current, selected)| current == selected)
}

#[cfg(test)]
mod audience_monitor_identity_tests {
    use super::audience_monitor_still_selected;

    #[test]
    fn stable_identity_survives_monitor_enumeration_reordering() {
        let audience = "connector-2";
        let reordered = ["connector-2", "connector-1"];
        assert!(audience_monitor_still_selected(
            reordered.first(),
            Some(&audience)
        ));
        assert!(!audience_monitor_still_selected(
            reordered.get(1),
            Some(&audience)
        ));
    }
}

struct NativeFloatingWindow {
    id: String,
    token: String,
    desktop_rect: KurboRect,
    window: Arc<Window>,
    render: Option<RenderThreadHandle>,
    /// Device/surface creation is much slower than a frame on software and
    /// headless backends.  Keeping it off the winit thread lets the frozen
    /// compositor proxy continue following the pointer during handoff.
    render_init: Option<mpsc::Receiver<Result<RenderThreadHandle, String>>>,
    prepared_at: Instant,
    adopted: bool,
    suspended: bool,
}

fn draw_debug_outlines(app: &mut App, scene: &mut Scene, transform: Affine) {
    fn draw_debug_recursive(
        widget: &dyn Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        depth: usize,
        cycle: &[Color; 3],
        label_ground: Color,
    ) {
        let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
            return;
        };
        let rect = widget.get_rect();
        let &[a, b, c] = cycle;
        let color = match depth.checked_rem(3).unwrap_or(0) {
            0 => a,
            1 => b,
            _ => c,
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            color,
            None,
            &rect,
        );
        let label = format!("{}: {}", widget.name(), depth);
        let text_y = (rect.y0 + 12.0).min(rect.y1);
        let bg_rect = KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            label_ground,
            None,
            &bg_rect,
        );
        text_renderer.draw_text(
            scene,
            &label,
            10.0,
            color,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let label_transform = transform * Affine::translate((rect.x0 + 2.0, text_y));
                label_transform
            },
            None,
        );
        widget.for_each_child(&mut |child| {
            draw_debug_recursive(
                child,
                scene,
                transform,
                text_renderer,
                depth.saturating_add(1),
                cycle,
                label_ground,
            );
        });
    }
    let debug_outline_cycle = [
        app.theme.tokens.colors.semantic.warning,
        app.theme.tokens.colors.semantic.info,
        app.theme.tokens.colors.semantic.success,
    ];
    let label_ground = app
        .theme
        .tokens
        .colors
        .surfaces
        .scrim
        .with_alpha(180.0 / 255.0);
    draw_debug_recursive(
        app.root_widget.as_ref(),
        scene,
        transform,
        &mut app.text_renderer,
        0,
        &debug_outline_cycle,
        label_ground,
    );
}

fn create_window_render(
    window: &Arc<Window>,
    transparent: bool,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<RenderThreadHandle, String> {
    create_window_render_prepared(window, transparent, None, proxy)
}

pub(crate) use crate::render_thread::PreparedRenderer;

#[cfg(windows)]
pub(crate) type RendererPreparation = mpsc::Receiver<Result<PreparedRenderer, String>>;

/// Start only for a real native launch, before loading app state and fonts.
/// This owns no window/surface and can safely finish if construction fails:
/// dropping the receiving end drops the prepared GPU resources at send.
#[cfg(windows)]
#[allow(
    clippy::disallowed_methods,
    reason = "native host runner: GPU preparation overlaps app start on its own thread"
)]
pub(crate) fn prepare_native_renderer() -> Option<RendererPreparation> {
    if std::env::var_os("TILO_SERIAL_RENDER_INIT").is_some() {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    match std::thread::Builder::new()
        .name("native-render-prepare".into())
        .spawn(move || {
            let prepare = || {
                tilo_startup_metrics::mark("native-render-prepare-start");
                let mut context = RenderContext::new();
                let device_id = pollster::block_on(context.device(None))
                    .ok_or_else(|| "could not prepare a drawing device".to_string())?;
                let device = context
                    .devices
                    .get(device_id)
                    .ok_or_else(|| "prepared drawing device index out of range".to_string())?;
                let renderer = create_native_renderer(device)?;
                tilo_startup_metrics::mark("native-render-prepare-done");
                Ok(PreparedRenderer {
                    context,
                    device_id,
                    renderer,
                })
            };
            let _ = sender.send(prepare());
        }) {
        Ok(_) => Some(receiver),
        Err(error) => {
            tracing::warn!(%error, "could not prepare renderer in parallel");
            None
        }
    }
}

fn create_window_render_prepared(
    window: &Arc<Window>,
    transparent: bool,
    prepared: Option<PreparedRenderer>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<RenderThreadHandle, String> {
    tilo_startup_metrics::mark("window-render-context-start");
    let size = window.inner_size();
    let (mut render_cx, prepared_renderer) = match prepared {
        Some(prepared) => (
            prepared.context,
            Some((prepared.device_id, prepared.renderer)),
        ),
        None => (RenderContext::new(), None),
    };
    let mut surface = pollster::block_on(render_cx.create_surface(
        window.clone(),
        size.width.max(1),
        size.height.max(1),
        vello::wgpu::PresentMode::AutoVsync,
    ))
    .map_err(|e| format!("could not create a drawing surface: {e}"))?;
    if transparent {
        // `Auto` commonly resolves to Opaque on Wayland even when winit's
        // window was created transparent. Floating surfaces paint
        // premultiplied Vello output, so request the matching compositor mode
        // explicitly; otherwise the transparent gutter becomes a black box.
        surface.config.alpha_mode = vello::wgpu::CompositeAlphaMode::PreMultiplied;
    }
    let device_handle = render_cx
        .devices
        .get(surface.dev_id)
        .ok_or_else(|| "render device index out of range".to_string())?;
    // Flipping to the sRGB suffix here would panic on mismatch with the
    // format `create_surface` already chose. `configure` returns no
    // `Result`, so guard it like the worker's reconfigures do
    // (`render_thread::configure_surface_checked`) — an already-lost
    // surface would otherwise hit wgpu's panicking default handler.
    tilo_startup_metrics::mark("window-surface-validation-scope-start");
    let configure_scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    tilo_startup_metrics::mark("window-surface-checked-configure-start");
    surface
        .surface
        .configure(&device_handle.device, &surface.config);
    tilo_startup_metrics::mark("window-surface-checked-configure-done");
    if let Some(err) = pollster::block_on(configure_scope.pop()) {
        return Err(format!("could not configure the drawing surface: {err}"));
    }
    tilo_startup_metrics::mark("window-surface-validation-scope-done");
    // create_surface checks presentation support and selects another device
    // if necessary. Never use pipelines built for a different device.
    let renderer = match prepared_renderer {
        Some((device_id, renderer)) if device_id == surface.dev_id => renderer,
        _ => create_native_renderer(device_handle)?,
    };
    // After this, every GPU call happens on its render worker.
    let window_id = window.id();
    let render = spawn_render_thread(
        render_cx,
        surface,
        renderer,
        Arc::new(move || {
            // Avoid synchronous macOS main-thread dispatch while the UI waits on submission.
            let _ = proxy.send_event(UserEvent::RenderReady(window_id));
        }),
    )?;
    tilo_startup_metrics::mark("window-render-context-done");
    Ok(render)
}

fn create_native_renderer(device_handle: &crate::gpu::DeviceHandle) -> Result<Renderer, String> {
    let device = &device_handle.device;
    // Vello's default heuristic uses every available core minus two. On a
    // 16-thread laptop that means fourteen simultaneous Vulkan pipeline
    // compilations for each freshly opened app, competing with the compositor
    // and making the whole desktop appear frozen. Two workers keep startup
    // parallel without turning one window into a system-wide CPU burst.
    let shader_threads = std::env::var("TILO_VELLO_INIT_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .and_then(std::num::NonZeroUsize::new)
        .unwrap_or(std::num::NonZeroUsize::MIN.saturating_add(1));
    tilo_startup_metrics::mark("window-vello-init-start");
    let renderer = Renderer::new(
        device,
        RendererOptions {
            // Diagnostic/low-end fallback: CPU coarse stages with the same
            // GPU fine raster. Kept opt-in until the benchmark proves it is
            // faster on a device; the default remains the GPU pipeline.
            use_cpu: crate::env::var_os("TILO_UI_VELLO_CPU").is_some(),
            // Every frame below renders with `AaConfig::Area`. Advertising
            // 8x/16x MSAA as well only compiles and retains shader variants
            // this framework never selects, increasing startup and GPU
            // memory pressure on exactly the low-end devices we target.
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: Some(shader_threads),
            pipeline_cache: None,
        },
    )
    .map_err(|e| format!("could not create the renderer: {e}"))?;
    tilo_startup_metrics::mark("window-vello-init-done");
    #[cfg(all(windows, feature = "dx12-shader-cache"))]
    device_handle.finish_shader_cache();
    Ok(renderer)
}

#[allow(
    clippy::disallowed_methods,
    reason = "native host runner: window render setup probes the GPU on its own thread"
)]
fn begin_window_render_init(
    window: Arc<Window>,
    transparent: bool,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<mpsc::Receiver<Result<RenderThreadHandle, String>>, String> {
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("floating-render-init".into())
        .spawn(move || {
            let result = create_window_render(&window, transparent, proxy);
            // The surface may have been permanently closed while a slow GPU
            // backend was still probing. In that case, stop the newly-created
            // render worker here so its surface is dropped before `window`.
            if let Err(mpsc::SendError(result)) = result_tx.send(result)
                && let Ok(mut render) = result
            {
                render.shutdown_and_join();
            }
        })
        .map_err(|error| format!("could not start floating renderer setup: {error}"))?;
    Ok(result_rx)
}

fn draw_legacy_cursors(app: &mut App, scene: &mut Scene, transform: Affine) {
    for cursor in app.state.collab.cursors.values() {
        let mut path = vello::kurbo::BezPath::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(14.0, 14.0));
        path.line_to(Point::new(6.0, 16.0));
        path.line_to(Point::new(0.0, 22.0));
        path.close_path();

        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow"
        )]
        let cursor_transform =
            transform * Affine::translate((f64::from(cursor.x), f64::from(cursor.y)));
        scene.fill(
            vello::peniko::Fill::NonZero,
            cursor_transform,
            cursor.color,
            None,
            &path,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            cursor_transform,
            app.theme.tokens.colors.on_surface(cursor.color).ink,
            None,
            &path,
        );

        app.text_renderer.draw_text(
            scene,
            &cursor.name,
            10.0,
            app.theme.tokens.colors.on_surface(cursor.color).ink,
            {
                #[allow(clippy::arithmetic_side_effects, reason = "Affine composition/translation is matrix multiplication on f64 fields; it cannot panic or overflow")]
                let name_transform = cursor_transform * Affine::translate((10.0, 20.0));
                name_transform
            },
            None,
        );
    }
}

impl AppHandler {
    fn handle_routed_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        if render_probe_event(&event) {
            let auxiliary_event = self
                .floating_windows
                .iter()
                .any(|surface| surface.window.id() == window_id)
                || self
                    .audience_presentation
                    .as_ref()
                    .is_some_and(|audience| audience.window.id() == window_id);
            if auxiliary_event
                && self.primary_retry.request_probe()
                && let Some(primary) = &self.window
            {
                primary.request_redraw();
            }

            if self
                .window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
                && self.primary_retry.request_probe()
                && let Some(window) = &self.window
            {
                window.request_redraw();
            }
            if let Some(surface) = self
                .floating_windows
                .iter_mut()
                .find(|surface| surface.window.id() == window_id)
                && surface
                    .render
                    .as_mut()
                    .is_some_and(|render| render.retry.request_probe())
            {
                surface.window.request_redraw();
            }
            if let Some(audience) = self
                .audience_presentation
                .as_mut()
                .filter(|audience| audience.window.id() == window_id)
                && audience
                    .render
                    .as_mut()
                    .is_some_and(|render| render.retry.request_probe())
            {
                audience.window.request_redraw();
            }
        }

        if self
            .audience_presentation
            .as_ref()
            .is_some_and(|audience| audience.window.id() == window_id)
        {
            self.handle_audience_window_event(event);
            return;
        }
        if let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.window.id() == window_id)
        {
            self.handle_native_floating_window_event(index, event);
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() == window_id {
            self.handle_primary_window_event(event_loop, &window, event);
        }
    }
    fn visible_auxiliary_needs_shared_root(&self) -> bool {
        self.floating_windows.iter().any(|surface| {
            !surface.suspended
                && surface
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.known_occluded())
        }) || self.audience_presentation.as_ref().is_some_and(|audience| {
            audience
                .render
                .as_ref()
                .is_none_or(|render| !render.retry.known_occluded())
        })
    }
    fn route_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
        origin: native_input_isolation::EventOrigin,
    ) {
        native_input_isolation::dispatch(
            self.native_input_route_isolation,
            origin,
            event,
            |event| {
                self.handle_routed_window_event(event_loop, window_id, event);
            },
        );
    }
    /// Called before showing the window on Windows, and from the first
    /// `RedrawRequested` elsewhere so GPU init doesn't starve Wayland's
    /// initial focus events. `Err` is returned, not panicked, since every step here can
    /// fail when the compositor exits mid-open.
    fn init_render(&mut self, window: &Arc<Window>) -> Result<(), String> {
        if let Some(retained) = self.suspended_renderer.take()
            && !retained.device_lost()
        {
            match create_window_render_prepared(
                window,
                false,
                Some(retained),
                self.a11y_proxy.clone(),
            ) {
                Ok(render) => {
                    self.render = Some(render);
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(%error, "could not render with the renderer kept over suspend");
                }
            }
        }
        #[cfg(windows)]
        if let Some(preparation) = self.app.startup_renderer.take() {
            match preparation.recv() {
                Ok(Ok(prepared)) => {
                    self.render = Some(create_window_render_prepared(
                        window,
                        false,
                        Some(prepared),
                        self.a11y_proxy.clone(),
                    )?);
                    return Ok(());
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "renderer preparation failed; selecting device for the actual surface")
                }
                Err(error) => {
                    tracing::warn!(%error, "renderer preparation disconnected; selecting device for the actual surface")
                }
            }
        }
        self.render = Some(create_window_render(
            window,
            false,
            self.a11y_proxy.clone(),
        )?);
        Ok(())
    }

    /// Reconcile the root's optional audience projection with a *separate*
    /// native toplevel. A missing monitor list, a one-output session, or an
    /// invalid target all leave this `None`; the root's normal fullscreen
    /// intent then supplies the single-output fallback. The chosen output
    /// may be the one the presenter window currently shows on: Tilo pairs
    /// the `.audience` surface and moves the presenter view to the other
    /// screen.
    fn sync_audience_presentation(
        &mut self,
        event_loop: &ActiveEventLoop,
        presenter: &Arc<Window>,
    ) {
        let _entry = self.app.enter_wake_scope();
        let request = self.app.root_widget.audience_presentation_request();
        let monitors: Vec<winit::monitor::MonitorHandle> = presenter.available_monitors().collect();
        let primary_monitor = presenter.primary_monitor();
        let local_monitor = presenter
            .current_monitor()
            .or_else(|| primary_monitor.clone());
        let presenter_index = local_monitor.as_ref().and_then(|current| {
            monitors
                .iter()
                .position(|monitor| monitor == current)
                .and_then(|index| u32::try_from(index).ok())
        });
        let infos: Vec<crate::fullscreen::MonitorInfo> = monitors
            .iter()
            .enumerate()
            .map(|(index, monitor)| {
                super::monitor_info(index, monitor, primary_monitor.as_ref() == Some(monitor))
            })
            .collect();
        let target = request.and_then(|request| {
            crate::fullscreen::select_audience_output(&infos, request.target, presenter_index)
        });

        if self.audience_presentation.as_ref().is_some_and(|audience| {
            audience_monitor_still_selected(
                target.and_then(|index| monitors.get(index as usize)),
                Some(&audience.monitor),
            )
        }) {
            return;
        }
        self.close_audience_presentation();
        let Some(index) = target else {
            return;
        };
        let Some(monitor) = monitors.get(index as usize).cloned() else {
            return;
        };

        let attributes = Window::default_attributes()
            .with_title("Audience presentation")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window.app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .as_deref(),
            );
            // A role-like suffix keeps a compositor from mistaking the
            // audience output for a duplicate editor surface.
            attributes.with_name(format!("{app_id}.audience"), "")
        };
        let Ok(window) = event_loop.create_window(attributes) else {
            tracing::warn!(
                "could not create separate audience presentation window; using single-output fallback"
            );
            return;
        };
        let window = Arc::new(window);
        window.set_fullscreen(Some(Fullscreen::Borderless(Some(monitor.clone()))));
        window.set_visible(true);
        window.request_redraw();
        self.audience_presentation = Some(AudiencePresentationWindow {
            monitor,
            window,
            render: None,
            render_init: None,
        });
        self.app.root_widget.on_audience_presentation_changed(true);
        self.app.layout.layout_dirty = true;
        presenter.request_redraw();
    }

    /// Drop the render worker before the Window Arc, as required by wgpu's
    /// surface lifetime. This is deliberately idempotent: hotplug, explicit
    /// stop, and process teardown can race through the same state transition.
    fn close_audience_presentation(&mut self) {
        let _entry = self.app.enter_wake_scope();
        let Some(mut audience) = self.audience_presentation.take() else {
            return;
        };
        audience.window.set_visible(false);
        if let Some(mut render) = audience.render.take() {
            render.shutdown_and_join();
        }
        self.app.root_widget.on_audience_presentation_changed(false);
        self.app.layout.layout_dirty = true;
    }

    /// Paint one audience frame without running a second widget layout or
    /// synchronising a second document. The root is responsible for a pure
    /// audience projection of the same model used by the presenter surface.
    fn handle_audience_redraw(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(audience) = self.audience_presentation.as_mut() else {
            return false;
        };
        if audience.render.is_none() {
            if audience.render_init.is_none() {
                match begin_window_render_init(
                    audience.window.clone(),
                    false,
                    self.a11y_proxy.clone(),
                ) {
                    Ok(init) => audience.render_init = Some(init),
                    Err(error) => tracing::warn!(%error, "could not start audience renderer"),
                }
            }
            let Some(init) = audience.render_init.as_ref() else {
                return true;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    audience.render = Some(render);
                    audience.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not initialise audience renderer");
                    audience.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    audience.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = audience.render.as_mut() else {
            return false;
        };
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented
                | FrameOutcome::Reconfigured
                | FrameOutcome::Skipped
                | FrameOutcome::Occluded => {
                    render.retry.observe(outcome, crate::time::now());
                }
                FrameOutcome::SurfaceGone => return false,
            }
        }
        match crate::render_thread::prepare_frame(
            &mut render.retry,
            &mut render.buffers,
            crate::time::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(_) => return false,
        }
        let (mut scene, mut damage_regions) = match render.buffers.try_recv() {
            Ok(scene) => scene,
            Err(mpsc::TryRecvError::Empty) => {
                return true;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::warn!("audience render worker exited");
                return false;
            }
        };
        damage_regions.clear();
        let size = audience.window.inner_size();
        let scale = audience.window.scale_factor();
        let rect = vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        let drawn = self.app.root_widget.draw_audience_presentation(
            &mut scene,
            Affine::scale(scale),
            &mut self.app.text_renderer,
            &self.app.theme,
            rect,
        );
        if !drawn {
            return false;
        }
        if render
            .render_tx
            .send(RenderRequest::Frame {
                cpu_frame_id: None,
                on_present: None,
                scene,
                width: size.width.max(1),
                height: size.height.max(1),
                base_color: self.app.theme.tokens.colors.surfaces.content,
                damage_regions,
                full_redraw: true,
            })
            .is_err()
        {
            tracing::warn!(
                "audience render worker stopped; returning to the single-output fallback"
            );
            return false;
        }
        true
    }

    /// A touch on the field that has the keyboard, which Android answers by
    /// settling the composition and bringing a dismissed keyboard back.
    #[cfg(target_os = "android")]
    fn touches_focused_field(&self, window_id: WindowId, location: PhysicalPosition<f64>) -> bool {
        let Some(window) = self
            .window
            .as_ref()
            .filter(|window| window.id() == window_id)
        else {
            return false;
        };
        let Some(field) = self.app.focused_node else {
            return false;
        };
        let scale = window.scale_factor();
        let point = Point::new(location.x / scale, location.y / scale);
        self.app
            .inspect_node(field, |widget| widget.get_rect().contains(point))
            .unwrap_or(false)
    }

    fn sync_ime(&mut self, window: &Window) {
        let _entry = self.app.enter_wake_scope();
        let next = self.app.ime_window_state();
        #[cfg(target_os = "android")]
        if super::android_text_input::bridge::installed() {
            let field = next.and(self.app.focused_node);
            let restart = self.text_input.refocus(field);
            let touched = std::mem::take(&mut self.text_input_touched);
            if next != self.ime_state || restart || touched {
                let surrounding = field.and_then(|node| {
                    self.app
                        .inspect_node(node, crate::ime::ImeSurroundingText::from_widget)
                        .ok()
                        .flatten()
                });
                super::android_text_input::bridge::update(
                    next.as_ref(),
                    surrounding.as_ref(),
                    window.scale_factor(),
                    restart,
                );
                self.ime_state = next;
            }
            // After the update, so the fresh connection starts from the
            // caret the touch just placed.
            if touched {
                super::android_text_input::bridge::touched();
            }
            return;
        }
        if next == self.ime_state {
            return;
        }
        match next {
            Some(intent) => {
                // Apply purpose before enabling: winit's Wayland backend
                // commits the enable immediately, so the first activation
                // must already carry the right content type. Cursor geometry
                // follows enable because winit intentionally ignores cursor
                // updates while IME is disabled.
                window.set_ime_purpose(intent.purpose);
                window.set_ime_allowed(true);
                window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(intent.x, intent.y),
                    winit::dpi::LogicalSize::new(intent.width, intent.height),
                );
            }
            None => window.set_ime_allowed(false),
        }
        self.ime_state = next;
    }

    fn create_native_floating_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: String,
        token: String,
        rect: KurboRect,
    ) -> Result<(), String> {
        let _entry = self.app.enter_wake_scope();
        if let Some(existing) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        {
            if existing.token != token {
                return Err("shell changed the token of a live floating surface".into());
            }
            if existing.desktop_rect.size() != rect.size() {
                let _ = existing
                    .window
                    .request_inner_size(native_floating_logical_size(rect));
            }
            existing.desktop_rect = rect;
            existing.prepared_at = crate::time::now();
            existing.adopted = false;
            existing.suspended = false;
            existing.window.set_visible(true);
            existing.window.request_redraw();
            return Ok(());
        }
        let logical_size = native_floating_logical_size(rect);
        let attributes = Window::default_attributes()
            .with_title(format!("Floating · {id}"))
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(false)
            .with_inner_size(logical_size);
        #[cfg(target_os = "linux")]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name(
                format!(
                    "{}{}",
                    crate::floating_surface::FLOATING_SURFACE_APP_ID_PREFIX,
                    token
                ),
                "",
            )
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("create floating surface window: {error}"))?,
        );
        window.set_visible(true);
        window.request_redraw();
        self.floating_windows.push(NativeFloatingWindow {
            id,
            token,
            desktop_rect: rect,
            window,
            render: None,
            render_init: None,
            prepared_at: crate::time::now(),
            adopted: false,
            suspended: false,
        });
        Ok(())
    }

    fn suspend_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(surface) = self
            .floating_windows
            .iter_mut()
            .find(|surface| surface.id == id)
        else {
            return false;
        };
        surface.window.set_visible(false);
        surface.suspended = true;
        surface.adopted = false;
        true
    }

    fn close_native_floating_window(&mut self, id: &str) -> bool {
        let _entry = self.app.enter_wake_scope();
        let Some(index) = self
            .floating_windows
            .iter()
            .position(|surface| surface.id == id)
        else {
            return false;
        };
        let mut surface = self.floating_windows.remove(index);
        surface.window.set_visible(false);
        if let Some(mut render) = surface.render.take() {
            render.shutdown_and_join();
        }
        true
    }

    fn apply_floating_host_messages(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let _entry = self.app.enter_wake_scope();
        let mut changed = false;
        for message in self.app.take_floating_host_messages() {
            match message {
                crate::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id,
                    token,
                    rect,
                    content_rect: _,
                    transfer_covered,
                    coordinate_offset: _,
                } => match crate::floating_surface::protocol_rect(rect) {
                    Ok(rect) => {
                        if transfer_covered {
                            self.floating_source_clear_frames.insert(id.clone(), None);
                            if let Some(primary) = self.window.as_ref() {
                                primary.request_redraw();
                            }
                        }
                        match self.create_native_floating_window(event_loop, id, token, rect) {
                            Ok(()) => changed = true,
                            Err(error) => {
                                tracing::warn!(%error, "could not prepare native floating surface");
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "shell sent invalid floating surface rect");
                    }
                },
                crate::floating_surface::FloatingSurfaceHostMessage::Adopted { id, rect } => {
                    if crate::floating_surface::protocol_rect(rect).is_ok()
                        && let Some(surface) = self
                            .floating_windows
                            .iter_mut()
                            .find(|surface| surface.id == id)
                    {
                        surface.adopted = true;
                        surface.window.request_redraw();
                        changed = true;
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Reattach { id }
                | crate::floating_surface::FloatingSurfaceHostMessage::Closed { id } => {
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames.remove(&id);
                    changed |= self.close_native_floating_window(&id);
                }
                crate::floating_surface::FloatingSurfaceHostMessage::Suspended { id } => {
                    self.floating_source_clear_frames.remove(&id);
                    self.floating_destination_present_frames
                        .insert(id.clone(), None);
                    changed |= self.suspend_native_floating_window(&id);
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
                crate::floating_surface::FloatingSurfaceHostMessage::DesktopBounds { .. } => {}
            }
        }
        changed
    }

    /// A prepared surface that never commits must not strand its app-owned
    /// content half outside the primary window forever. Keep polling only
    /// while the first buffer is pending, then ask the shell to close the
    /// abandoned capability; `Closed` drives the ordinary animated return.
    fn recover_stalled_floating_windows(&mut self) -> bool {
        // A frozen compositor-side transfer frame keeps the surface visually
        // continuous while a cold GPU backend prepares. This deadline is now
        // a genuine failure bound, not a UX timer: ordinary adoption replaces
        // the proxy on its first committed buffer.
        // A cold secondary WGPU surface can spend several seconds enumerating
        // fallbacks on software/headless systems. The compositor's transfer
        // proxy is already covering every pixel, so returning at four seconds
        // caused a needless visible snap just before a viable backend could
        // finish. Ten seconds remains a bounded failure path without turning
        // ordinary slow device discovery into UX churn.
        const FIRST_BUFFER_DEADLINE: Duration = Duration::from_secs(10);
        let _entry = self.app.enter_wake_scope();
        let stalled = self
            .floating_windows
            .iter()
            .filter(|surface| {
                !surface.suspended
                    && !surface.adopted
                    && surface.prepared_at.elapsed() >= FIRST_BUFFER_DEADLINE
            })
            .map(|surface| surface.id.clone())
            .collect::<Vec<_>>();
        if stalled.is_empty() {
            return false;
        }
        for id in stalled {
            tracing::warn!(%id, "floating surface missed its first-buffer deadline; returning it to the app");
            if let Some(client) = self.app.floating.client.as_mut() {
                let _ = client.send(
                    &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                        id: id.clone(),
                    },
                );
            }
            self.close_native_floating_window(&id);
        }
        true
    }

    fn sync_native_floating_geometry(&mut self) -> bool {
        let _entry = self.app.enter_wake_scope();
        let presentations = self.app.root_widget.floating_surface_presentations();
        let mut changed = false;
        for surface in &mut self.floating_windows {
            if let Some(presentation) = presentations
                .iter()
                .find(|presentation| presentation.id == surface.id)
                && presentation.window_rect != surface.desktop_rect
            {
                if presentation.window_rect.size() != surface.desktop_rect.size() {
                    // App-owned live activities can change silhouette while
                    // detached: a call gains participant rows, navigation
                    // reveals another instruction, or accessibility text
                    // grows. Resize the real transparent buffer with the
                    // shared presentation rect so its soft chrome is never
                    // clipped inside the old native allocation.
                    let _ = surface
                        .window
                        .request_inner_size(native_floating_logical_size(presentation.window_rect));
                }
                surface.desktop_rect = presentation.window_rect;
                surface.window.request_redraw();
                changed = true;
            }
        }
        changed
    }

    fn handle_floating_redraw(&mut self, index: usize) -> bool {
        let _entry = self.app.enter_wake_scope();
        let (app, surfaces) = (&mut self.app, &mut self.floating_windows);
        let Some(surface) = surfaces.get_mut(index) else {
            return false;
        };
        if surface.suspended {
            return true;
        }
        if surface.render.is_none() {
            if surface.render_init.is_none() {
                match begin_window_render_init(
                    surface.window.clone(),
                    true,
                    self.a11y_proxy.clone(),
                ) {
                    Ok(init) => surface.render_init = Some(init),
                    Err(error) => {
                        tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                        return false;
                    }
                }
            }
            let Some(init) = surface.render_init.as_ref() else {
                return false;
            };
            match init.try_recv() {
                Ok(Ok(render)) => {
                    surface.render = Some(render);
                    surface.render_init = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, id = %surface.id, "floating surface renderer failed");
                    surface.render_init = None;
                    return false;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::warn!(id = %surface.id, "floating surface renderer setup stopped");
                    surface.render_init = None;
                    return false;
                }
            }
        }
        let Some(render) = surface.render.as_mut() else {
            return false;
        };
        while let Ok(outcome) = render.frame_complete.try_recv() {
            match outcome {
                FrameOutcome::Presented
                | FrameOutcome::Reconfigured
                | FrameOutcome::Skipped
                | FrameOutcome::Occluded => {
                    render.retry.observe(outcome, crate::time::now());
                }
                FrameOutcome::SurfaceGone => return false,
            }
        }
        match crate::render_thread::prepare_frame(
            &mut render.retry,
            &mut render.buffers,
            crate::time::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(_) => return false,
        }
        let (mut scene, mut damage_regions) = match render.buffers.try_recv() {
            Ok(buffers) => buffers,
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        };
        damage_regions.clear();
        // Every pooled scene is either newly seeded or reset by the worker
        // before recycling. Resetting it again here only rewrote all stream
        // lengths on every frame.
        let inner = surface.window.inner_size();
        let scale = surface.window.scale_factor();
        let transform = crate::scale::composite_transform(scale)
            .pre_translate((-surface.desktop_rect.x0, -surface.desktop_rect.y0).into());
        app.root_widget.draw_floating_surfaces(
            &mut scene,
            transform,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        if render
            .render_tx
            .send(RenderRequest::Frame {
                cpu_frame_id: None,
                on_present: None,
                scene,
                width: inner.width.max(1),
                height: inner.height.max(1),
                base_color: Color::TRANSPARENT,
                damage_regions,
                full_redraw: true,
            })
            .is_err()
        {
            return false;
        }
        true
    }

    fn dispatch_floating_event(
        &mut self,
        window: &Arc<Window>,
        desktop_rect: KurboRect,
        event: WindowEvent,
    ) {
        let _entry = self.app.enter_wake_scope();
        let scale = window.scale_factor();
        let event = match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(
                    position.x + desktop_rect.x0 * scale,
                    position.y + desktop_rect.y0 * scale,
                ),
            },
            WindowEvent::Touch(mut touch) => {
                touch.location = PhysicalPosition::new(
                    touch.location.x + desktop_rect.x0 * scale,
                    touch.location.y + desktop_rect.y0 * scale,
                );
                WindowEvent::Touch(touch)
            }
            other => other,
        };
        let app = &mut self.app;
        let Some(root_node) = app
            .history_layout
            .map(|layout| layout.app)
            .or(app.layout.root_node)
        else {
            return;
        };
        let mut handled = false;
        let mut focus_target = None;
        app.refresh_control_action_availability();
        app.root_widget.handle_event(
            &event,
            &mut crate::widget::EventCtx {
                scale_factor: scale,
                modifiers: &app.input.modifiers,
                state: &mut app.state.doc,
                path: &app.state.root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy: &app.layout.taffy,
            },
        );
        // `handle_event` may change presentation-only resource declarations
        // without touching the CRDT (selected tab, open media, local mode).
        app.invalidate_widget_resource_cache();
        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
        }
        if handled {
            app.commit_action();
            app.needs_sync = true;
            app.layout.layout_dirty |= app.root_widget.is_layout_dirty();
            if let Some(primary) = self.window.as_ref() {
                primary.request_redraw();
            }
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the frame handshake shares render state, timing locals, and early exits"
    )]
    /// Split out of `window_event`'s `RedrawRequested` arm (`clippy::too_many_lines`).
    /// Body unchanged from the original inline arm — see `window_event` for the
    /// event-loop plumbing this is called from. The layout pass, draw pass,
    /// legacy cursor drawing, and debug-outline drawing are further split
    /// into [`run_layout_pass`], [`draw_frame_content`], [`draw_legacy_cursors`]
    /// and [`draw_debug_outlines`] respectively; what remains is a single
    /// frame's render-thread handshake (acquire scene, draw, send, log) whose
    /// phases share `render`/`app`/`scene`/timing locals and several early
    /// `return`s — further fragmenting it risks changing frame-timing or
    /// error-path behaviour with no clippy/cargo loop available here to
    /// verify against.
    #[allow(clippy::too_many_lines, reason = "see doc comment above")]
    fn handle_redraw_requested(&mut self, event_loop: &ActiveEventLoop, window: &Arc<Window>) {
        let native_redraw_entry = self
            .native_input_origin
            .map(|origin| (origin, crate::time::now()));
        // Capture only the initial frame attempts, not steady-state redraws.
        // An early return leaves its final marker absent rather than inventing
        // a completed stage. A later successful submission ends capture.
        let first_primary_frame = self.primary_frames_sent == 0;
        let mark_first_frame = |name| {
            if first_primary_frame {
                tilo_startup_metrics::mark(name);
            }
        };
        mark_first_frame("first-frame-redraw-start");
        let _entry = self.app.enter_wake_scope();
        if self.surface_suspended {
            return;
        }
        if self.render.is_none() {
            tilo_startup_metrics::mark("first-redraw-init-render-start");
            if let Err(reason) = self.init_render(window) {
                // Retrying every redraw would only spin.
                crate::diag!("tilo: {reason}");
                event_loop.exit();
                return;
            }
            tilo_startup_metrics::mark("first-redraw-init-render-done");
        }
        let visible_sibling = self.visible_auxiliary_needs_shared_root();
        if visible_sibling {
            self.primary_retry.keep_shared_cadence(crate::time::now());
        }
        let Some(render) = self.render.as_mut() else {
            return;
        };
        // A `Reconfigured` or `Skipped` previous frame re-requests a
        // redraw so the user doesn't see a stale image.
        let mut needs_followup_redraw = false;
        let mut surface_gone = false;
        while let Ok(outcome) = render.frame_complete.try_recv() {
            self.primary_frames_completed = self.primary_frames_completed.saturating_add(1);
            match outcome {
                FrameOutcome::Presented => {
                    self.last_primary_frame_presented = self.primary_frames_completed;
                    self.primary_retry.observe_for_shared_root(
                        outcome,
                        crate::time::now(),
                        visible_sibling,
                    );
                }
                FrameOutcome::Reconfigured | FrameOutcome::Skipped | FrameOutcome::Occluded => {
                    self.primary_retry.observe_for_shared_root(
                        outcome,
                        crate::time::now(),
                        visible_sibling,
                    );
                }
                FrameOutcome::SurfaceGone => {
                    surface_gone = true;
                }
            }
        }
        let cleared = completed_primary_frame_gates(
            &self.floating_source_clear_frames,
            self.last_primary_frame_presented,
        );
        for id in cleared {
            let sent = self.app.floating.client.as_mut().is_some_and(|client| {
                client
                    .send(
                        &crate::floating_surface::FloatingSurfaceClientMessage::SourceCleared {
                            id: id.clone(),
                        },
                    )
                    .is_ok()
            });
            if sent {
                self.floating_source_clear_frames.remove(&id);
            }
        }
        let destinations_presented = completed_primary_frame_gates(
            &self.floating_destination_present_frames,
            self.last_primary_frame_presented,
        );
        for id in destinations_presented {
            let sent = self
                .app
                .floating
                .client
                .as_mut()
                .is_some_and(|client| {
                    client
                        .send(
                            &crate::floating_surface::FloatingSurfaceClientMessage::DestinationPresented {
                                id: id.clone(),
                            },
                        )
                        .is_ok()
                });
            if sent {
                self.floating_destination_present_frames.remove(&id);
            }
        }
        if surface_gone {
            tracing::info!("drawing surface is gone; closing the window");
            event_loop.exit();
            return;
        }
        // Do not consume sync/layout/dirty state unless a finite render buffer
        // is available. Worker completion wakes this window if admission defers.
        match crate::render_thread::prepare_frame(
            &mut self.primary_retry,
            &mut render.buffers,
            crate::time::now(),
        ) {
            Ok(true) => {}
            Ok(false) => return,
            Err(_) => {
                tracing::error!("ui-render thread died; closing window");
                event_loop.exit();
                return;
            }
        }
        sync_safe_area(&mut self.app, window);
        #[cfg(target_os = "linux")]
        self.decorations.refresh(&mut self.app, window);
        let app = &mut self.app;

        // Continue one bounded provider page per frame. Re-querying resets the
        // palette generation/cursors, so stale work cannot leak into a newer
        // query. A follow-up redraw is requested only while more pages exist.
        let (search_pending, search_announcement) =
            app.actions
                .action_palette
                .as_mut()
                .map_or((false, None), |palette| {
                    let progress = palette.advance_search_page();
                    (progress.pending, palette.take_search_announcement())
                });
        needs_followup_redraw |= search_pending;
        if let (Some(reader), Some(announcement)) = (
            app.accessibility.screen_reader.as_mut(),
            search_announcement,
        ) {
            reader.announce(announcement, tts::Priority::Notification, false);
        }

        let inner = window.inner_size();
        let width = inner.width;
        let height = inner.height;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "scale factor is intentionally narrowed to f32 for the render pipeline; identical to today's output"
        )]
        let scale_factor = window.scale_factor() as f32;
        app.layout.last_frame_size = Some(inner);
        let mut floating_content_changed =
            app.layout.layout_dirty || app.layout.structure_dirty || app.needs_sync;
        let mut full_redraw_required = floating_content_changed;

        // Records first: if the root widget opened a different
        // record, sync against that record's document, not the
        // outgoing one's.
        mark_first_frame("first-frame-record-start");
        let record_changed = app.record_tick();
        mark_first_frame("first-frame-record-done");
        floating_content_changed |= record_changed;
        full_redraw_required |= record_changed;

        let __prof_frame_start = app.profiler.as_ref().map(|_| crate::time::now());
        // Drain model notifications and advance update-driven widgets before
        // synchronization. A worker's `Wake` event reaches this pass without
        // requiring the app to own a proxy or poll inside its model.
        mark_first_frame("first-frame-update-start");
        if app.update_model() {
            floating_content_changed = true;
            full_redraw_required = true;
        }
        mark_first_frame("first-frame-update-done");
        mark_first_frame("first-frame-sync-start");
        if app.sync_model(false) {
            floating_content_changed = true;
            full_redraw_required = true;
        }
        if crate::component_lifecycle::take_window_close_intent(app.root_widget.as_mut()) {
            if app.admit_native_close_request() {
                event_loop.exit();
                return;
            }
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        mark_first_frame("first-frame-sync-done");
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::Sync, __prof_frame_start);
        }

        mark_first_frame("first-frame-live-regions-start");
        app.scan_live_regions();
        mark_first_frame("first-frame-live-regions-done");

        // E.g. a tab split inserting a new WindowGroup: rebuild the
        // taffy tree from scratch so new widgets get node ids the
        // layout pass below can see.
        mark_first_frame("first-frame-build-layout-start");
        app.layout.structure_dirty |= app.root_widget.take_structure_dirty();
        // Captured BEFORE `rebuild_structure` clears it — a fresh
        // taffy tree always needs a full solve regardless of the
        // scroll fast path below.
        let structure_rebuilt = app.layout.structure_dirty;
        floating_content_changed |= structure_rebuilt;
        let __prof_build = app.profiler.as_ref().map(|_| crate::time::now());
        if app.layout.structure_dirty {
            app.rebuild_structure();
        }
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::BuildLayout, __prof_build);
        }

        mark_first_frame("first-frame-build-layout-done");

        let perf = perf_enabled();
        let t_frame_start = if perf { Some(crate::time::now()) } else { None };
        let Some(root_node) = app.layout.root_node else {
            tracing::error!("root_node unset during a redraw; skipping frame");
            return;
        };
        mark_first_frame("first-frame-layout-start");
        let Some((t_relayout_branch, t_compute_us, t_apply_us)) = run_layout_pass(
            app,
            root_node,
            width,
            height,
            scale_factor,
            structure_rebuilt,
            Some(&crate::widget::NativeViewHost::live(
                window,
                app.input.platform,
            )),
        ) else {
            return;
        };
        mark_first_frame("first-frame-layout-done");
        full_redraw_required |= structure_rebuilt || t_relayout_branch;
        // Read before `draw_frame_content` drains widget dirty flags via
        // `mark_drawn`. Clean compositor/exposure redraws do not invalidate
        // independent native floating surfaces.
        floating_content_changed |= app.root_widget.needs_redraw() || app.history_needs_redraw();

        // Admission reserved this pair before state updates. Taking it cannot
        // wait on GPU work; aborted layouts retain it in the handle for retry.
        let __prof_pool_wait = app.profiler.as_ref().map(|_| crate::time::now());
        let Ok((mut scene, mut damage_regions)) = render.buffers.recv() else {
            tracing::error!("ui-render thread died; closing window");
            event_loop.exit();
            return;
        };
        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::ScenePoolWait, __prof_pool_wait);
        }
        damage_regions.clear();
        full_redraw_required |= app.debug_mode
            || app.actions.action_palette.is_some()
            || app.input.tooltip_host.needs_frame()
            || app.input.tooltip_host.visible().is_some()
            || !app.presence_ctx.remote_cursors.is_empty()
            || app
                .profiler
                .as_ref()
                .is_some_and(crate::profiling::FrameProfiler::hud_enabled);
        // The worker resets scenes before returning them to this pool; the
        // pre-seeded scenes are empty too.

        let t_draw_start = if perf { Some(crate::time::now()) } else { None };
        crate::webview::frame_trace::begin_scene();
        let (transform, window_rect, t_draw_us) =
            draw_frame_content(app, &mut scene, width, height, scale_factor, t_draw_start);
        #[cfg(target_os = "linux")]
        {
            self.decorations.draw(app, &mut scene, transform);
            full_redraw_required |= self.decorations.take_repaint();
        }

        let __prof_presence = app.profiler.as_ref().map(|_| crate::time::now());
        draw_legacy_cursors(app, &mut scene, transform);

        // A pointer that arrived on a desktop scope is the
        // compositor's to draw, and the helper skips it — see
        // `presence::PointerOwner`. Only the *painting* has one
        // owner; the shell threads this same context into its own
        // overlay too.
        let interaction_now = crate::time::now();
        app.presence_ctx.prune_interactions(interaction_now);
        crate::presence::draw_remote_presence(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        // Controls and named rows already publish their real accessibility
        // geometry. Reuse that exact focus/selection outline for semantic
        // action presence; this decorates the target without changing local
        // focus, selection, active tab, or scroll position.
        let visible_interactions = app.presence_ctx.visible_interactions(interaction_now);
        if !visible_interactions.is_empty() {
            app.refresh_semantic_tree();
        }
        let native_targets = visible_interactions
            .iter()
            .filter_map(|interaction| {
                let exact = match &interaction.anchor {
                    tilo_actions::InteractionAnchor::SemanticControl { id } => app
                        .semantic_tree()
                        .node(&crate::a11y_tree::SemanticId(id.clone()))
                        .and_then(|node| node.bounds),
                    tilo_actions::InteractionAnchor::ListItems { .. }
                    | tilo_actions::InteractionAnchor::Resource => {
                        let mut matches = app
                            .semantic_tree()
                            .iter()
                            .filter(|node| {
                                node.name.as_deref() == Some(interaction.resource.label.as_str())
                            })
                            .filter_map(|node| node.bounds);
                        let only = matches.next();
                        only.filter(|_| matches.next().is_none())
                    }
                    _ => None,
                }?;
                Some((interaction.clone(), exact))
            })
            .collect::<Vec<_>>();
        for (interaction, rect) in native_targets {
            let user = crate::permissions::UserId(interaction.actor.id);
            let label = app.presence_ctx.display_name(&user);
            app.presence_ctx.draw_peer_outline_labeled(
                &mut crate::paint::PaintCtx::new(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                ),
                &user,
                &label,
                rect,
                f64::from(app.theme.tokens.radii.detail),
            );
        }
        // Action/resource callers have no synthetic pointer. Their truthful
        // semantic lifecycle is painted as an app-level activity rail until a
        // domain widget projects the stable anchor into native geometry.
        app.presence_ctx.draw_interactions(
            &mut scene,
            transform,
            window_rect,
            &mut app.text_renderer,
            &app.theme,
            interaction_now,
        );

        if let Some(p) = app.profiler.as_mut() {
            p.record_since(crate::profiling::Pass::PresenceOverlay, __prof_presence);
        }

        let mut native_main_stages = (self.native_input_origin.is_some()
            && crate::render_thread::timing::enabled())
        .then(MainStages::default);
        record_main_stage(&mut native_main_stages, MainStage::BeforeProfilerCommit);

        // Folds in any `handle_event` time banked since last frame.
        if let (Some(p), Some(start)) = (app.profiler.as_mut(), __prof_frame_start) {
            p.commit_frame(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
            record_main_stage(&mut native_main_stages, MainStage::AfterProfilerCommit);
            if !self.native_input_ids.is_empty() {
                let cpu_frame_id = p.history().back().map(|sample| sample.frame);
                crate::native_scenario::log::emit_json(
                    "tilo-native-frame",
                    serde_json::json!({
                        "cpu_frame_id": cpu_frame_id,
                        "input_ids": std::mem::take(&mut self.native_input_ids),
                        "frame_commit_us": self.native_input_origin.map(|origin| origin.elapsed().as_micros()),
                        // Capture after drawing: layout may clamp scroll state.
                        // Input-time effects alone cannot prove the frame used it.
                        "state": app.native_input_snapshot(),
                    }),
                );
            }
        } else {
            record_main_stage(&mut native_main_stages, MainStage::AfterProfilerCommit);
        }
        crate::webview::frame_trace::finish_scene(
            app.profiler
                .as_ref()
                .and_then(|p| p.history().back())
                .map(|sample| sample.frame),
        );
        record_main_stage(&mut native_main_stages, MainStage::AfterNativeFrameLog);

        if app.debug_mode {
            draw_debug_outlines(app, &mut scene, transform);
        }
        record_main_stage(&mut native_main_stages, MainStage::AfterDebugOutlines);

        // Animated widgets update their visual bounds while drawing (for
        // example, an indeterminate progress segment records its old and new
        // positions there). Capture damage only after that work, but before
        // `mark_drawn` clears regular dirty flags. This makes the submitted
        // scene and its damage describe the same frame rather than trailing
        // animation by one frame.
        if !full_redraw_required {
            collect_submitted_widget_damage(app.root_widget.as_ref(), &mut damage_regions);
            let physical_viewport = KurboRect::new(0.0, 0.0, f64::from(width), f64::from(height));
            let scale = f64::from(scale_factor);
            for rect in &mut damage_regions {
                *rect = KurboRect::new(
                    rect.x0 * scale,
                    rect.y0 * scale,
                    rect.x1 * scale,
                    rect.y1 * scale,
                )
                .inflate(16.0 * scale, 16.0 * scale)
                .intersect(physical_viewport);
            }
            damage_regions.retain(|rect| !rect.is_zero_area());
        }
        // This must follow damage collection: widget `draw` may update
        // damage geometry and `mark_drawn` consumes persistent dirty flags.
        complete_native_frame_content(app, |app| {
            record_main_stage(&mut native_main_stages, MainStage::AfterDamageMarkDrawn);

            // `update_if_active` only builds + sends when an AT is
            // attached, so this is free on the common no-reader path.
            if let Some(adapter) = self.a11y_adapter.as_mut() {
                let app_ref: &mut App = app;
                let scale = f64::from(scale_factor);
                adapter.update_if_active(|| {
                    crate::accesskit_bridge::native_tree_update(
                        app_ref.build_a11y_tree_update(scale),
                    )
                });
            }
            record_main_stage(&mut native_main_stages, MainStage::AfterAccessibility);
        });

        // Painted last, after the frame timing was committed above,
        // so the HUD's own cost never enters its numbers.
        if app
            .profiler
            .as_ref()
            .is_some_and(crate::profiling::FrameProfiler::hud_enabled)
        {
            let (hud_w, hud_h) =
                crate::scale::logical_size((width, height), f64::from(scale_factor));
            if let Some(p) = app.profiler.as_ref() {
                p.draw_hud(
                    &mut scene,
                    transform,
                    &mut app.text_renderer,
                    &app.theme,
                    (hud_w, hud_h),
                );
            }
        }
        if crate::render_thread::retry::submit_redraw_frame(
            full_redraw_required,
            !damage_regions.is_empty(),
        ) {
            let req = RenderRequest::Frame {
                cpu_frame_id: app
                    .profiler
                    .as_ref()
                    .and_then(|profiler| profiler.history().back())
                    .map(|sample| sample.frame),
                scene,
                width,
                height,
                base_color: app.theme.tokens.colors.surfaces.content,
                damage_regions,
                full_redraw: full_redraw_required,
                on_present: self.dev_reload_timing.take().map(|timing| {
                    let submitted = crate::time::wall();
                    Box::new(move |presented| timing.record(submitted, presented))
                        as crate::render_thread::PresentHook
                }),
            };
            record_main_stage(&mut native_main_stages, MainStage::AfterHudRequest);
            if let Some((origin, entered)) = native_redraw_entry {
                // All three values use the input timeline's monotonic origin.
                // This is a CPU submit attempt, not GPU/display completion.
                crate::native_scenario::log::emit_json(
                    "tilo-native-redraw",
                    serde_json::json!({
                        "cpu_frame_id": app.profiler.as_ref().and_then(|p| p.history().back()).map(|s| s.frame),
                        "redraw_enter_us": entered.duration_since(origin).as_micros(),
                        "timed_build_start_us": __prof_frame_start.map(|start| start.duration_since(origin).as_micros()),
                        "submit_attempt_us": origin.elapsed().as_micros(),
                    }),
                );
            }
            record_main_stage(&mut native_main_stages, MainStage::AfterNativeRedrawLog);
            let send_cpu_frame_id = match &req {
                RenderRequest::Frame { cpu_frame_id, .. } => *cpu_frame_id,
                RenderRequest::Shutdown { .. } => None,
            };
            let (send_result, send_bounds) = crate::render_thread::timing::around(
                self.native_input_origin.is_some() && crate::render_thread::timing::enabled(),
                || render.render_tx.send(req),
            );
            if let Some(bounds) = send_bounds {
                crate::native_scenario::log::emit_json(
                    "tilo-native-send",
                    serde_json::json!({
                        "cpu_frame_id": send_cpu_frame_id,
                        "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                        "before_host_lo_s": bounds.before.map(|b| b.lo_s),
                        "before_host_hi_s": bounds.before.map(|b| b.hi_s),
                        "after_host_lo_s": bounds.after.map(|b| b.lo_s),
                        "after_host_hi_s": bounds.after.map(|b| b.hi_s),
                        "send_ok": send_result.is_ok(),
                    }),
                );
            }
            if let Some(stages) = native_main_stages {
                crate::native_scenario::log::emit_json(
                    "tilo-native-main-stages",
                    stages.row(send_cpu_frame_id),
                );
            }
            if send_result.is_err() {
                tracing::error!("ui-render thread receiver closed; exiting");
                event_loop.exit();
                return;
            }
            mark_first_frame("first-frame-submitted");
            self.primary_frames_sent = self.primary_frames_sent.saturating_add(1);
            let submitted = self.primary_frames_sent;
            for required in self.floating_source_clear_frames.values_mut() {
                if required.is_none() {
                    *required = Some(submitted);
                }
            }
            for required in self.floating_destination_present_frames.values_mut() {
                if required.is_none() {
                    *required = Some(submitted);
                }
            }

            let t_send_us = t_frame_start.map_or(0, |t| t.elapsed().as_micros());
            if let Some(_start) = t_frame_start {
                // On a static frame this reads `shape=0us hit=N miss=0`.
                let (shape_hits, shape_misses, shape_ns) = app.text_renderer.take_shape_stats();
                crate::diag!(
                    "perf frame: main_total={:>5}us layout={}({}+{}us) draw={:>5}us (shape={:>4}us hit={} miss={}) send_to_worker={:>5}us",
                    t_send_us,
                    if t_relayout_branch { "Y" } else { "n" },
                    t_compute_us,
                    t_apply_us,
                    t_draw_us,
                    shape_ns.checked_div(1000).unwrap_or(0),
                    shape_hits,
                    shape_misses,
                    t_send_us,
                );
            }
        } else {
            // The callback that woke this redraw changed no pixels. Presenting
            // would commit a new buffer and a new frame callback, and the
            // shell would paint that echo while the desktop sat still.
            scene.reset();
            render.buffers.unprepare((scene, damage_regions));
        }

        if needs_followup_redraw {
            window.request_redraw();
        }
        // The secondary surfaces paint the same app-owned state. A primary
        // frame therefore invalidates each of them exactly once; their own
        // presentation never recursively requests another primary frame.
        if floating_content_changed {
            for surface in &self.floating_windows {
                surface.window.request_redraw();
            }
        }
        if let Some(audience) = self.audience_presentation.as_ref() {
            audience.window.request_redraw();
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "input routing shares mutable state and must retain fixed precedence"
    )]
    /// Whether something consumed the event.
    fn handle_other_window_event(&mut self, window: &Arc<Window>, other: WindowEvent) -> bool {
        let _entry = self.app.enter_wake_scope();
        let event = other;
        if self.app.close_handle_event(&event, window.scale_factor()) {
            window.request_redraw();
            return true;
        }
        let app = &mut self.app;
        let history_input = app.history_controls_event(&event, window.scale_factor());
        let _history_frame = crate::history_mode::frame::FrameworkInputFrame::new(
            &app.framework_input,
            history_input,
        );
        let detected_interaction = if self.dispatching_touch_pointer {
            None
        } else {
            match &event {
                WindowEvent::Touch(_) => Some(app.input.interaction.with_touch_modality()),
                WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. } => {
                    Some(app.input.interaction.with_pointer_modality())
                }
                WindowEvent::KeyboardInput {
                    event:
                        winit::event::KeyEvent {
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => Some(app.input.interaction.with_keyboard_modality()),
                _ => None,
            }
        };
        if let Some(environment) = detected_interaction {
            app.set_interaction_environment(environment);
        }
        if history_input {
            app.dispatch_framework_history_event(&event, window.scale_factor());
            window.request_redraw();
            return true;
        }
        let __prof_input = app.profiler.as_ref().map(|_| crate::time::now());
        // RUST_LOG=tilo_ui::app=debug: pointer-move spam is intentionally
        // not throttled here — when the bug is "no clicks work", you
        // want to see whether moves arrive at all.
        match &event {
            WindowEvent::MouseInput { state, button, .. } => {
                tracing::debug!(?state, ?button, "App::window_event MouseInput");
            }
            WindowEvent::CursorMoved { position, .. } => {
                tracing::trace!(
                    x = position.x,
                    y = position.y,
                    "App::window_event CursorMoved"
                );
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                tracing::debug!(key = ?ke.logical_key, state = ?ke.state, "App::window_event KeyboardInput");
            }
            _ => {}
        }
        // `CursorMoved` is measured in this window's own content,
        // never the desktop — see the `pointer_space` field.
        if app.presence.broadcast_scope.is_some() {
            match &event {
                WindowEvent::CursorMoved { position, .. } => {
                    let now = crate::time::now();
                    if pointer_broadcast_allowed(
                        app.presence.last_pointer_broadcast,
                        now,
                        Duration::from_millis(POINTER_BROADCAST_MIN_GAP_MS),
                    ) {
                        app.presence.last_pointer_broadcast = Some(now);
                        let where_it_is = app.local_pointer_key();
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "presence coordinates are intentionally narrowed to f32 on the wire; identical to today's output"
                        )]
                        app.broadcast_presence(
                            where_it_is,
                            PresencePayload::Pointer {
                                x: position.x as f32,
                                y: position.y as f32,
                            },
                        );
                    }
                }
                WindowEvent::CursorLeft { .. } => {
                    let where_it_was = app.local_pointer_key();
                    app.broadcast_presence(where_it_was, PresencePayload::Left);
                    app.presence.last_pointer_broadcast = None;
                }
                _ => {}
            }
        }

        let mut handled = false;
        let mut history_handled = matches!(event, WindowEvent::KeyboardInput { .. })
            && app
                .focused_node
                .is_some_and(|node| app.is_history_node(node));
        let mut focus_target = None;
        // A synthesized hover-clear may be handled by a control, but it is
        // presentation state and must not create an empty undo boundary.
        let mut is_undo_redo =
            self.dispatching_touch_pointer && matches!(event, WindowEvent::CursorMoved { .. });
        let mut floating_pointer_needs_redraw = false;

        // The palette owns the keyboard while open; Ctrl+K itself is
        // handled below, so let it through.
        if app.actions.action_palette.is_some() {
            let is_ctrl_k = matches!(
                &event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                    ..
                } if app
                    .input
                    .platform
                    .command_modifier_down(app.input.modifiers.state())
                    && crate::platform::HostShortcut::resolve(
                        app.input.platform,
                        c,
                        app.input.modifiers.state().shift_key(),
                    ) == Some(crate::platform::HostShortcut::Search)
            );
            if !is_ctrl_k
                && matches!(
                    event,
                    WindowEvent::KeyboardInput { .. }
                        | WindowEvent::Ime(_)
                        | WindowEvent::CursorMoved { .. }
                        | WindowEvent::CursorLeft { .. }
                        | WindowEvent::MouseInput { .. }
                        | WindowEvent::MouseWheel { .. }
                )
                && app.action_palette_handle_event(&event, window.scale_factor())
            {
                handled = true;
            }
        }

        // Runs FIRST so it wins over Tab/arrow handling.
        if app.handle_screen_reader_key(&event) {
            handled = true;
            is_undo_redo = true; // suppress an undo commit for a read cmd
        }

        let state_pre = if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
        ) {
            app.focused_state_snapshot()
        } else {
            None
        };

        let review_pre = if matches!(event, WindowEvent::KeyboardInput { .. }) {
            app.text_review_snapshot()
        } else {
            None
        };

        // Governs pointer click-through only; keyboard focus
        // containment is the separate `traps_focus()` concern (see
        // `keyboard_nav_allowed`).
        let modal_rect = app.root_widget.modal_capture();

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            app.debug_mode = !app.debug_mode;
            handled = true;
        }

        if !handled
            && app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state())
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Character(ref c),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = app.input.modifiers.state().shift_key();
            match crate::platform::HostShortcut::resolve(app.input.platform, c, shift) {
                Some(crate::platform::HostShortcut::Undo) => {
                    app.perform_undo_request(crate::undo::UndoRequest::Undo);
                    is_undo_redo = true;
                    handled = true;
                }
                Some(crate::platform::HostShortcut::Redo) => {
                    app.perform_undo_request(crate::undo::UndoRequest::Redo);
                    is_undo_redo = true;
                    handled = true;
                }
                // A second press closes the palette.
                Some(crate::platform::HostShortcut::Search) => {
                    if app.actions.action_palette.is_some() {
                        app.close_action_palette();
                    } else {
                        app.open_action_palette();
                    }
                    handled = true;
                }
                Some(crate::platform::HostShortcut::History) => {
                    handled = app.invoke_host_shortcut(crate::platform::HostShortcut::History);
                }
                None => {}
            }
            if handled {
                window.request_redraw();
            }
        }

        if !handled && app.handle_tab_navigation(&event, modal_rect) {
            handled = true;
        }

        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(logical_key),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let dir = match logical_key {
                winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                _ => None,
            };
            if let (Some(d), true, true) = (
                dir,
                app.input.modifiers.state().alt_key(),
                app.keyboard_nav_allowed(modal_rect),
            ) {
                app.navigate_focus(d);
                handled = true;
            }
        }

        // Two-finger swipe, edge pull or mouse side button; delivered via
        // `on_nav_gesture`.
        if !handled && !self.dispatching_touch_pointer {
            use crate::gesture::GesturePhase;
            let map_phase = |p: winit::event::TouchPhase| match p {
                winit::event::TouchPhase::Started => GesturePhase::Begin,
                winit::event::TouchPhase::Moved => GesturePhase::Update,
                winit::event::TouchPhase::Ended => GesturePhase::End,
                winit::event::TouchPhase::Cancelled => GesturePhase::Cancel,
            };
            let nav = match &event {
                WindowEvent::MouseWheel { delta, phase, .. } => {
                    let (dx, dy, precise) = match delta {
                        winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y, true),
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            (f64::from(*x), f64::from(*y), false)
                        }
                    };
                    app.input
                        .nav_gestures
                        .on_scroll(dx, dy, map_phase(*phase), precise)
                }
                WindowEvent::PanGesture { delta, phase, .. } => app.input.nav_gestures.on_scroll(
                    f64::from(delta.x),
                    f64::from(delta.y),
                    map_phase(*phase),
                    true,
                ),
                WindowEvent::MouseInput { .. } => crate::interaction::mouse_nav_direction(&event),
                WindowEvent::Touch(t) => {
                    let width = f64::from(window.inner_size().width);
                    app.input.nav_gestures.on_touch(
                        t.id,
                        t.location.x,
                        t.location.y,
                        map_phase(t.phase),
                        width,
                    )
                }
                _ => None,
            };
            if let Some(dir) = nav {
                let consumed =
                    app.root_widget
                        .on_nav_gesture(dir, &mut app.state.doc, &app.state.root);
                if consumed {
                    app.needs_sync = true;
                    window.request_redraw();
                    handled = true;
                }
            }
        }

        if !handled && app.layout.root_node.is_some() {
            app.refresh_control_action_availability();
            let scale_factor = window.scale_factor();
            let presentation_was_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            let floating_pointer_event = app.primary_floating_pointer_event(&event, scale_factor);
            let used_desktop_pointer_space = floating_pointer_event.is_some();
            let event_for_root = floating_pointer_event.as_ref().unwrap_or(&event);
            let was_mouse_click = matches!(
                event,
                WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            );
            history_handled |= app.presentation_handle_event(
                event_for_root,
                scale_factor,
                &mut handled,
                &mut focus_target,
            );
            // Even a document-free interaction may switch which resources a
            // widget presents. Invalidate once at the mutable tree boundary;
            // all later consumers in this event/frame share the rebuilt cache.
            app.invalidate_widget_resource_cache();
            let presentation_is_floating =
                !app.root_widget.floating_surface_presentations().is_empty();
            floating_pointer_needs_redraw = matches!(event, WindowEvent::CursorMoved { .. })
                && (used_desktop_pointer_space
                    || presentation_was_floating != presentation_is_floating);
            if was_mouse_click {
                tracing::debug!(
                    handled,
                    needs_redraw = app.root_widget.needs_redraw(),
                    is_layout_dirty = app.root_widget.is_layout_dirty(),
                    "root_widget.handle_event(MouseInput Pressed) returned"
                );
            }
            // A press the modal didn't consume (the dimmed backdrop)
            // is still marked handled so it can't reach widgets
            // behind the scrim.
            if modal_rect.is_some()
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: winit::event::ElementState::Pressed,
                        ..
                    } | WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        ..
                    }
                )
            {
                handled = true;
            }
        }

        // Alt+Arrow remains host directional navigation when the focused
        // widget leaves it unhandled. Running this after widget dispatch lets
        // editors consume platform word/selection shortcuts first, while
        // keeping Alt navigation ahead of the plain range-control fallback.
        app.handle_unconsumed_directional_key(&event, modal_rect, true, &mut handled);

        // If no widget consumed Enter/Space (text inputs do), activate
        // the focused control the same way a mouse click would.
        app.handle_unconsumed_activation_key(&event, &mut handled);

        // A focused range control consumes Left/Right; Up/Down and Tab
        // still move focus. Modifiers scale the step:
        //   plain  → one step       Shift → four steps (coarse)
        //   Ctrl   → jump to min/max (Left = min, Right = max)
        // Other widgets ignore the action, falling through to
        // directional navigation below.
        if !handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(named),
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            use crate::a11y::Action;
            let ctrl = app
                .input
                .platform
                .command_modifier_down(app.input.modifiers.state());
            let shift = app.input.modifiers.state().shift_key();
            // (action to repeat, how many times)
            let (action, times) = match named {
                winit::keyboard::NamedKey::ArrowLeft if ctrl => {
                    (Some(Action::SetValue("0".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowRight if ctrl => {
                    (Some(Action::SetValue("100".into())), 1)
                }
                winit::keyboard::NamedKey::ArrowLeft => {
                    (Some(Action::Decrement), if shift { 4 } else { 1 })
                }
                winit::keyboard::NamedKey::ArrowRight => {
                    (Some(Action::Increment), if shift { 4 } else { 1 })
                }
                _ => (None, 0),
            };
            if let (Some(act), Some(fid)) = (action, app.focused_node) {
                let mut any = false;
                for _ in 0..times {
                    any |= app
                        .dispatch_node(fid, |widget, doc, root| {
                            widget.perform_action(&act, doc, root)
                        })
                        .unwrap_or(false);
                }
                if any {
                    handled = true;
                    app.needs_sync = true;
                    app.layout.layout_dirty = true;
                }
            }
        }

        app.handle_unconsumed_directional_key(&event, modal_rect, false, &mut handled);

        if app.dispatch_control_invocations() != 0 {
            handled = true;
            app.needs_sync = true;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        if let Some(address) = app.root_widget.take_action_palette_request()
            && app.open_action_palette_at(&address)
        {
            app.layout.layout_dirty = true;
            window.request_redraw();
        }

        app.broadcast_focus_presence();
        app.broadcast_location_presence();
        app.broadcast_view_presence();
        app.publish_action_surface();
        app.publish_continuation_metadata();
        app.publish_playback_handoff();

        if let Some(id) = focus_target {
            app.set_focused_node(Some(id));
            handled = true;
        }

        app.announce_state_change(state_pre);
        app.announce_text_review(review_pre);

        if handled {
            app.finalize_handled_native_event(&event, is_undo_redo, history_handled);
            app.needs_sync = true;
            // Gating every handled event on `needs_redraw()` is
            // wrong: container widgets can mutate their own
            // active-index state without setting a descendant leaf's
            // `dirty` flag, so recursive `needs_redraw` misses it.
            // Only pointer MOVES (the hot 60+ Hz stream) skip a
            // redraw when nothing's dirty.
            let is_pointer_move = matches!(&event, WindowEvent::CursorMoved { .. });
            if !is_pointer_move || app.root_widget.is_layout_dirty() {
                app.layout.layout_dirty = true;
            }
            if !is_pointer_move
                || floating_pointer_needs_redraw
                || app.root_widget.needs_redraw()
                || app.history_needs_redraw()
            {
                window.request_redraw();
            }
        }

        // Resolved on every `CursorMoved`, not just handled ones, so
        // the shape stays correct crossing into a widget that
        // doesn't flip its own hover.
        if let WindowEvent::CursorMoved { position, .. } = &event {
            let scale = window.scale_factor();
            let logical = Point::new(position.x / scale, position.y / scale);
            let desired = app.resolve_cursor(logical).unwrap_or(CursorIcon::Default);
            if app.input.current_cursor != desired {
                app.input.current_cursor = desired;
                window.set_cursor(desired);
            }
            // `on_pointer` keeps the dwell running over the same
            // region and restarts it on a new one.
            let target = app.tooltip_target_at(logical);
            if app
                .input
                .tooltip_host
                .on_pointer(target, crate::time::now())
            {
                window.request_redraw();
            }
        }

        if matches!(
            &event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::CursorLeft { .. }
        ) && app.input.tooltip_host.dismiss()
        {
            window.request_redraw();
        }

        if let (Some(start), Some(p)) = (__prof_input, app.profiler.as_mut()) {
            p.record(crate::profiling::Pass::HandleEvent, start.elapsed());
        }
        handled
    }
}

impl AppHandler {
    /// `UserEvent` injection keeps the ordinary window dispatch after admission.
    fn injected_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.route_window_event(
            event_loop,
            window_id,
            event,
            native_input_isolation::EventOrigin::Injected,
        );
    }
}

impl AppHandler {
    /// Rebuild into the windows this process already owns.
    ///
    /// The primary window, every live floating window, and an audience window
    /// stay. Reload does not call `create_window`.
    fn reload_open_windows_in_place(&mut self) {
        let mut windows = Vec::new();
        if let Some(window) = &self.window {
            windows.push(("primary".to_owned(), window.clone()));
        }
        for surface in &self.floating_windows {
            if !surface.suspended {
                windows.push((surface.id.clone(), surface.window.clone()));
            }
        }
        if self.audience_presentation.is_some() {
            if let Some(audience) = &self.audience_presentation {
                windows.push(("audience".to_owned(), audience.window.clone()));
            }
        }
        let Some(session) = crate::host::environment().dev_session.clone() else {
            return;
        };
        let library = dev_reload_library(&session)
            .filter(|library| self.dev_loaded_library.as_ref() != Some(library));
        let Some(library) = library else {
            // No new library: a build that did not publish one, or a signal for
            // catalogs alone, keeps the code already running. Catalogs on disk
            // still replace the strings baked into this process.
            super::reload_dev_locale_catalogs_from(std::path::Path::new("locales"));
            self.app.needs_sync = true;
            for (_, window) in &windows {
                window.request_redraw();
            }
            return;
        };
        let received = crate::time::wall();
        let started = crate::time::now();
        let _ = super::write_dev_checkpoint(&self.app, &session);
        let checkpoint = std::fs::read(session.join("checkpoint.bin")).unwrap_or_default();
        let checkpointed = started.elapsed();
        let slots = windows
            .iter()
            .map(|(id, _)| super::DevWindowSlot {
                id: id.clone(),
                state: self
                    .dev_window_state
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| checkpoint.clone()),
            })
            .collect::<Vec<_>>();
        let loading = crate::time::now();
        let reloaded = match super::reload_from_library(&library, &slots) {
            Ok(reloaded) => reloaded,
            Err(error) => {
                eprintln!("tilo: reload rejected; keeping the last good app\n{error}");
                return;
            }
        };
        let loaded = loading.elapsed();
        let installing = crate::time::now();
        if let Some(root) = reloaded.root {
            if !self.app.install_reloaded_view(root) {
                eprintln!(
                    "tilo: the open windows could not take the new view; keeping the running one"
                );
                return;
            }
            self.app.init_layout();
        }
        self.dev_reload_timing = Some(super::DevReloadTiming {
            library: library.clone(),
            received,
            checkpoint: checkpointed,
            load: reloaded.load,
            handoff: loaded.saturating_sub(reloaded.load),
            install: installing.elapsed(),
        });
        self.dev_loaded_library = Some(library);
        self.dev_window_state = reloaded
            .windows
            .into_iter()
            .map(|window| (window.id, window.state))
            .collect();
        super::reload_dev_locale_catalogs_from(std::path::Path::new("locales"));
        self.app.needs_sync = true;
        let count = windows.len();
        for (_, window) in &windows {
            window.request_redraw();
        }
        eprintln!("tilo: reloaded {count} open window(s) in place");
    }
}

impl ApplicationHandler<UserEvent> for AppHandler {
    // SAFETY: the zeroed-DeviceId transmutes below are safe because winit's
    // DeviceId is opaque and the dispatch chain never inspects it (same
    // trick `shell::compositor::native` uses headless).
    #[allow(
        clippy::too_many_lines,
        reason = "winit requires the complete synthetic event dispatch at this callback boundary"
    )]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let _entry = self.app.enter_wake_scope();
        let event = match event {
            UserEvent::RenderReady(id) => {
                if let Some(window) = self.window.as_ref().filter(|window| window.id() == id) {
                    window.request_redraw();
                } else if let Some(surface) = self
                    .floating_windows
                    .iter()
                    .find(|surface| surface.window.id() == id)
                {
                    surface.window.request_redraw();
                } else if let Some(audience) = self
                    .audience_presentation
                    .as_ref()
                    .filter(|audience| audience.window.id() == id)
                {
                    audience.window.request_redraw();
                }
                return;
            }
            UserEvent::Wake(wake) => {
                if self.app.accept_wake(&wake)
                    && wake.requires_sync()
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                return;
            }
            UserEvent::DevSessionStop => {
                self.app.autosave_flush();
                super::checkpoint_and_exit(&self.app, event_loop);
                return;
            }
            UserEvent::DevSessionReload => {
                self.reload_open_windows_in_place();
                return;
            }
            other => other,
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let window_id = window.id();
        let mut effect_before = None;
        let event = match event {
            UserEvent::NativeScenarioInput(packet) => {
                if self.native_input_origin.is_none() {
                    let size = window.inner_size();
                    crate::native_scenario::log::emit_json(
                        "tilo-native-window",
                        serde_json::json!({
                            "scale_factor": window.scale_factor(),
                            "physical_width": size.width,
                            "physical_height": size.height,
                            "pointer_passthrough": self.native_input_pointer_passthrough,
                            "input_route_isolation": self.native_input_route_isolation,
                        }),
                    );
                }
                self.native_input_origin.get_or_insert(packet.origin);
                let (now, dispatch_host) = crate::native_scenario::host_clock::sample();
                crate::native_scenario::log::emit_json(
                    "tilo-native-input",
                    serde_json::json!({
                        "id": packet.id,
                        "warmup": packet.warmup,
                        "input": packet.input,
                        "scheduled_us": packet.scheduled_at.duration_since(packet.origin).as_micros(),
                        "sent_us": packet.sent_at.duration_since(packet.origin).as_micros(),
                        "dispatch_start_us": now.duration_since(packet.origin).as_micros(),
                        "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                        "sent_host_lo_s": packet.sent_host.map(|bounds| bounds.lo_s),
                        "sent_host_hi_s": packet.sent_host.map(|bounds| bounds.hi_s),
                        "dispatch_host_lo_s": dispatch_host.map(|bounds| bounds.lo_s),
                        "dispatch_host_hi_s": dispatch_host.map(|bounds| bounds.hi_s),
                        "lateness_us": now.saturating_duration_since(packet.scheduled_at).as_micros(),
                    }),
                );
                effect_before = self
                    .app
                    .native_input_snapshot()
                    .map(|value| (packet.id, value));
                self.native_input_ids.push(packet.id);
                match packet.input {
                    crate::native_scenario::Input::Hover { x, y } => UserEvent::Hover { x, y },
                    crate::native_scenario::Input::Click { x, y } => UserEvent::Click { x, y },
                    crate::native_scenario::Input::Wheel { dx, dy } => UserEvent::Wheel { dx, dy },
                    crate::native_scenario::Input::Character { value } => {
                        UserEvent::Key(winit::keyboard::Key::Character(value.to_string().into()))
                    }
                    crate::native_scenario::Input::Key { key } => UserEvent::Key(key.into()),
                }
            }
            other => other,
        };
        match event {
            UserEvent::Hover { x, y } => {
                let ev = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.injected_window_event(event_loop, window_id, ev);
            }
            UserEvent::Click { x, y } => {
                let moved = WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: PhysicalPosition::new(x, y),
                };
                self.injected_window_event(event_loop, window_id, moved);
                let press = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                };
                self.injected_window_event(event_loop, window_id, press);
                let release = WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Released,
                    button: MouseButton::Left,
                };
                self.injected_window_event(event_loop, window_id, release);
            }
            UserEvent::Key(key) => {
                for state in [ElementState::Pressed, ElementState::Released] {
                    self.injected_window_event(
                        event_loop,
                        window_id,
                        crate::synthetic_input::key_event(&key, state),
                    );
                }
            }
            UserEvent::Wheel { dx, dy } => {
                let ev = WindowEvent::MouseWheel {
                    device_id: unsafe { std::mem::zeroed() },
                    delta: winit::event::MouseScrollDelta::LineDelta(dx, dy),
                    phase: winit::event::TouchPhase::Moved,
                };
                self.injected_window_event(event_loop, window_id, ev);
            }
            UserEvent::External(payload) => {
                let app = &mut self.app;
                app.root_widget
                    .on_external_event(&payload, &mut app.state.doc, &app.state.root);
                app.needs_sync = true;
                window.request_redraw();
            }
            UserEvent::HostCommand(invocation) => {
                self.app.invoke_host_command(&invocation);
                self.app.needs_sync = true;
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            UserEvent::HostSearch(query) => {
                if self
                    .app
                    .invoke_host_shortcut(crate::platform::HostShortcut::Search)
                    && let Some(palette) = self.app.actions.action_palette.as_mut()
                {
                    palette.set_query(&query);
                }
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            #[cfg(target_os = "android")]
            UserEvent::PlatformTextInput => {
                for op in super::android_text_input::bridge::drain() {
                    for event in self.text_input.events(op) {
                        self.route_window_event(
                            event_loop,
                            window_id,
                            event,
                            native_input_isolation::EventOrigin::Platform,
                        );
                    }
                }
                window.request_redraw();
            }
            UserEvent::Wake(_)
            | UserEvent::NativeScenarioInput(_)
            | UserEvent::RenderReady(_)
            | UserEvent::DevSessionStop
            | UserEvent::DevSessionReload => {}
            UserEvent::AccessKit(ak_event) => {
                use accesskit_winit::WindowEvent as AkWindowEvent;
                match ak_event {
                    AkWindowEvent::InitialTreeRequested => {
                        if let Some(adapter) = self.a11y_adapter.as_mut() {
                            let scale = window.scale_factor();
                            let app_ref: &mut App = &mut self.app;
                            adapter.update_if_active(|| {
                                crate::accesskit_bridge::native_tree_update(
                                    app_ref.build_a11y_tree_update(scale),
                                )
                            });
                        }
                        window.request_redraw();
                    }
                    AkWindowEvent::ActionRequested(req) => {
                        // `Some((x, y))` means "synthesise a click here".
                        if let Some(action) = crate::accesskit_bridge::accesskit_action_to_ours(
                            req.action,
                            req.data.as_ref(),
                        ) {
                            let click = self.app.dispatch_a11y_action(req.target_node, &action);
                            if let Some((x, y)) = click {
                                let scale = window.scale_factor();
                                let (px, py) = (x * scale, y * scale);
                                let moved = WindowEvent::CursorMoved {
                                    device_id: unsafe { std::mem::zeroed() },
                                    position: PhysicalPosition::new(px, py),
                                };
                                self.injected_window_event(event_loop, window_id, moved);
                                let press = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Pressed,
                                    button: MouseButton::Left,
                                };
                                self.injected_window_event(event_loop, window_id, press);
                                let release = WindowEvent::MouseInput {
                                    device_id: unsafe { std::mem::zeroed() },
                                    state: ElementState::Released,
                                    button: MouseButton::Left,
                                };
                                self.injected_window_event(event_loop, window_id, release);
                            }
                            window.request_redraw();
                        }
                    }
                    AkWindowEvent::AccessibilityDeactivated => {}
                }
            }
        }
        if let Some((id, before)) = effect_before {
            crate::native_scenario::log::emit_json(
                "tilo-native-effect",
                serde_json::json!({
                    "id": id, "before": before, "after": self.app.native_input_snapshot(),
                }),
            );
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let _entry = self.app.enter_wake_scope();
        // Fires again after `suspended` on mobile, where the window survives
        // but its native surface was destroyed: the next redraw rebuilds the
        // renderer against the new surface.
        self.surface_suspended = false;
        if let Some(window) = &self.window {
            self.app.layout.layout_dirty = true;
            window.request_redraw();
            return;
        }
        // Required, not incidental: the compositor only accepts
        // `set_selection` from the connection that owns the focused surface.
        crate::host::native::attach_display(event_loop);
        // AccessKit's adapter asserts it's created while the window is
        // hidden, so build invisible, create the adapter, show.
        let title = resolve_window_title(self.app.window.title.as_deref());
        let icon = crate::window_icon::resolve();
        let window_attributes = Window::default_attributes()
            .with_title(title)
            .with_window_icon(icon.clone())
            // Draggable title bar on compositors with no server-side
            // decorations (e.g. GNOME/mutter); explicit though it's the
            // winit default, so the intent is unmistakable.
            .with_decorations(true)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        // Optional profiling placement keeps concurrent benchmark windows from
        // completely occluding one another. Platform window managers may ignore it.
        let window_attributes = match std::env::var("TILO_PROFILE_WINDOW_POSITION")
            .ok()
            .and_then(|value| crate::profiling::profile_window_position(&value))
        {
            Some((x, y)) => window_attributes.with_position(winit::dpi::LogicalPosition::new(x, y)),
            None => window_attributes,
        };
        // Benchmarks can keep their own windows above unrelated desktop apps.
        // This does not change normal application window ordering.
        let window_attributes = if std::env::var("TILO_PROFILE_WINDOW_FRONT").as_deref() == Ok("1")
        {
            window_attributes.with_window_level(winit::window::WindowLevel::AlwaysOnTop)
        } else {
            window_attributes
        };
        // See `resolve_app_id`: a shared id collapses per-app compositor
        // lookups.
        #[cfg(target_os = "linux")]
        let window_attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            let package_id = std::env::var("TILO_PACKAGE_ID").ok();
            let app_id = resolve_app_id(
                self.app.window.app_id.as_deref().or(package_id.as_deref()),
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .as_deref(),
            );
            window_attributes.with_name(app_id, "")
        };
        // `resumed` is a winit `ApplicationHandler` callback returning `()`,
        // so a `create_window` failure (compositor gone) has nowhere to
        // propagate to; there is no window to run without it either way.
        #[allow(
            clippy::unwrap_used,
            reason = "fixed () trait signature; window-creation failure at startup is unrecoverable"
        )]
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::ActiveEventLoopExtMacOS;
            if let Err(error) = event_loop.set_quit_request_window(Some(window.id())) {
                crate::diag!("tilo: cannot protect application Quit: {error}");
                event_loop.exit();
                return;
            }
        }
        tilo_startup_metrics::mark("primary-window-created");
        self.native_input_pointer_passthrough = false;
        self.native_input_route_isolation = false;
        if std::env::var("TILO_PROFILE_POINTER_PASSTHROUGH").as_deref() == Ok("1")
            && std::env::var("TILO_PROFILE_WINDOW_NO_ACTIVATE").as_deref() == Ok("1")
            && (self.native_scenario.is_some() || self.native_input_origin.is_some())
        {
            // Physical pointer movement must not replace the scripted hover.
            // Scripted inputs still use the normal UserEvent dispatch route.
            if let Err(error) = window.set_cursor_hittest(false) {
                eprintln!("Native benchmark pointer isolation failed: {error}");
                event_loop.exit();
                return;
            }
            self.native_input_pointer_passthrough = true;
            self.native_input_route_isolation = true;
        }
        // Some backends only honour the icon post-creation; a no-op on
        // Wayland compositors that source it from the app_id instead.
        window.set_window_icon(icon);

        // Must be created before the window is shown.
        #[cfg(not(target_os = "android"))]
        {
            self.a11y_adapter = Some(accesskit_winit::Adapter::with_event_loop_proxy(
                event_loop,
                &window,
                self.a11y_proxy.clone(),
            ));
        }
        // One provider per view: the previous one must be withdrawn first.
        #[cfg(target_os = "android")]
        {
            self.a11y_adapter = None;
            match PrimaryA11yAdapter::new(event_loop, &window, self.a11y_proxy.clone()) {
                Ok(adapter) => self.a11y_adapter = Some(adapter),
                Err(error) => crate::diag!("tilo: no TalkBack access: {error}"),
            }
        }
        crate::widget::mount_native_views(
            self.app.root_widget.as_mut(),
            &crate::widget::NativeViewHost::live(&window, self.app.input.platform),
        );
        if let Some(host) = self.host_appearance.as_mut() {
            host.note_window_theme(window.theme());
        }
        self.apply_host_appearance(false);
        #[cfg(windows)]
        if let Err(reason) = self.init_render(&window) {
            crate::diag!("tilo: {reason}");
            event_loop.exit();
            return;
        }
        #[cfg(any(target_os = "macos", windows))]
        self.install_host_chrome(&window);
        // Honour system/presentation roots before the first visible configure.
        // The normal reconcile path still owns output selection and later changes.
        self.app.reconcile_fullscreen(&window);
        tilo_startup_metrics::mark("primary-window-show-start");
        window.set_visible(true);
        tilo_startup_metrics::mark("primary-window-show-done");

        self.app.layout.layout_dirty = true;
        // GPU init deferred to first `RedrawRequested` (`init_render`): the
        // ~50ms surface+device init here would block `resumed`'s seat
        // wiring — on mutter that intermittently left the toplevel
        // unfocused with dead input on first launch.
        window.request_redraw();
        tilo_startup_metrics::mark("primary-window-redraw-requested");
        crate::dnd::set_drag_window(&window);
        self.window = Some(window);
        tilo_startup_metrics::mark("primary-window-resumed-done");
    }

    /// Android destroys the window's native surface when the activity leaves
    /// the screen and may kill the process any time after. Save the app's
    /// state and drop the surface while it is still valid; the renderer,
    /// window, widget tree and document stay for the next `resumed`.
    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        let _entry = self.app.enter_wake_scope();
        self.surface_suspended = true;
        for error in self.app.checkpoint_before_close() {
            tracing::error!(%error, "could not save state on suspend");
        }
        if let Some(mut render) = self.render.take() {
            self.suspended_renderer = render.shutdown_keeping_renderer();
        }
    }

    /// BEFORE the `Window` arc is dropped — the worker's
    /// `RenderSurface<'static>` references it, and wgpu may panic if the
    /// surface outlives the window.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.app.close_wake_scope();
        let _entry = self.app.enter_wake_scope();
        // The Unix adapter owns an asynchronous AT-SPI provider.  Drop it
        // before the native views and window disappear, so external clients
        // receive the provider's normal unregister/teardown rather than being
        // left with object paths whose backing Wayland surface is already
        // gone.  This also makes the ordering explicit instead of relying on
        // AppHandler field-drop order.
        self.a11y_adapter = None;
        if !self.app.close_admission.admitted {
            self.app.autosave_flush();
        }
        self.app.close_native_content();
        self.close_audience_presentation();
        for surface in &mut self.floating_windows {
            if let Some(mut render) = surface.render.take() {
                render.shutdown_and_join();
            }
        }
        self.floating_windows.clear();
        if let Some(mut render) = self.render.take() {
            render.shutdown_and_join();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        #[cfg(target_os = "android")]
        if let WindowEvent::Touch(touch) = &event
            && touch.phase == winit::event::TouchPhase::Started
            && self.ime_state.is_some()
            && self.touches_focused_field(window_id, touch.location)
        {
            for settled in self.text_input.settle() {
                self.route_window_event(
                    event_loop,
                    window_id,
                    settled,
                    native_input_isolation::EventOrigin::Platform,
                );
            }
            self.text_input_touched = true;
        }
        self.note_host_appearance_event(&event);
        self.route_window_event(
            event_loop,
            window_id,
            event,
            native_input_isolation::EventOrigin::Platform,
        );
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.service_close_admission();
        if self.app.finish_close_if_ready() {
            event_loop.exit();
            return;
        }
        let _entry = self.app.enter_wake_scope();
        let Some(window) = self.window.clone() else {
            return;
        };
        #[cfg(any(target_os = "macos", windows))]
        self.refresh_host_chrome();
        sync_safe_area(&mut self.app, &window);
        self.poll_touch_hold(&window);
        // Conservative shared-root fallback: hidden primary work may stay at
        // 60Hz while a visible auxiliary depends on its update/sync projection.
        // Only all-known-occluded surfaces receive full idle backoff.
        if self.visible_auxiliary_needs_shared_root() {
            self.primary_retry.keep_shared_cadence(crate::time::now());
        }
        // Once admission defers, periodic animation/startup demand must not
        // spin redraw events against the busy worker. Keep updating dirty state;
        // the worker's armed completion callback requests the next paint.
        let mut auxiliary_retry_at: Option<Instant> = None;
        for (aux_window, render) in self
            .floating_windows
            .iter()
            .filter(|surface| !surface.suspended)
            .map(|surface| (&surface.window, surface.render.as_ref()))
            .chain(
                self.audience_presentation
                    .iter()
                    .map(|audience| (&audience.window, audience.render.as_ref())),
            )
        {
            if let Some(deadline) = render.and_then(|render| render.retry.deadline()) {
                auxiliary_retry_at =
                    Some(auxiliary_retry_at.map_or(deadline, |prior| prior.min(deadline)));
                if deadline <= crate::time::now() && auxiliary_redraw_allowed(render) {
                    aux_window.request_redraw();
                }
            }
        }
        let auxiliary_retry_due =
            auxiliary_retry_at.is_some_and(|deadline| deadline <= crate::time::now());
        let retry_now = crate::time::now();
        let primary_blocked_until = self
            .primary_retry
            .occlusion_blocked(retry_now)
            .then(|| self.primary_retry.deadline())
            .flatten();
        let any_render_available = primary_blocked_until.is_none()
            || self.floating_windows.iter().any(|surface| {
                !surface.suspended
                    && surface
                        .render
                        .as_ref()
                        .is_none_or(|render| !render.retry.occlusion_blocked(retry_now))
            })
            || self.audience_presentation.as_ref().is_some_and(|audience| {
                audience
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.occlusion_blocked(retry_now))
            });
        let retry_at = self.primary_retry.deadline();
        let retry_due = retry_at.is_some_and(|deadline| deadline <= crate::time::now());
        let redraw_deferred = self
            .render
            .as_ref()
            .is_some_and(|render| render.buffers.redraw_deferred())
            || retry_at.is_some_and(|deadline| crate::time::now() < deadline);
        let request_primary_redraw = || {
            if !redraw_deferred {
                window.request_redraw();
            }
        };
        if retry_due {
            request_primary_redraw();
        }
        self.sync_ime(&window);
        if self.last_primary_frame_presented > 0
            && let Some(scenario) = self.native_scenario.take()
        {
            crate::native_scenario::log::emit_json(
                "tilo-native-scenario",
                serde_json::json!({
                    "name": scenario.name, "steps": scenario.steps.len(),
                    "warmup_steps": scenario.warmup_steps, "interval_us": scenario.interval_us,
                    "event": "started_after_first_worker_presentation",
                }),
            );
            let proxy = self.a11y_proxy.clone();
            if let Err(error) = crate::native_scenario::spawn(scenario, move |packet| {
                proxy
                    .send_event(UserEvent::NativeScenarioInput(packet))
                    .is_ok()
            }) {
                crate::native_scenario::log::emit_json(
                    "tilo-native-scenario",
                    serde_json::json!({"event":"failed", "error":error}),
                );
                event_loop.exit();
            }
        }

        if self.app.pump_floating_surface_service() {
            self.app.invalidate_widget_resource_cache();
            self.app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        if self.apply_floating_host_messages(event_loop) {
            self.app.invalidate_widget_resource_cache();
            request_primary_redraw();
        }
        self.sync_native_floating_geometry();
        if self.recover_stalled_floating_windows() {
            request_primary_redraw();
        }
        // `RedrawRequested` polls the asynchronous renderer setup. The shell
        // keeps showing its live transfer proxy in the meantime, so these
        // frames are cheap but essential: the first real buffer should take
        // ownership as soon as it is ready, without waiting for fresh input.
        for surface in &self.floating_windows {
            if !surface.suspended
                && !surface.adopted
                && auxiliary_redraw_allowed(surface.render.as_ref())
            {
                surface.window.request_redraw();
            }
        }
        let awaiting_primary_first_present = primary_startup_pump_needed(
            self.primary_frames_sent,
            self.last_primary_frame_presented,
        );
        if awaiting_primary_first_present {
            request_primary_redraw();
        }
        // This runs before the ordinary fullscreen reconciliation below. If a
        // real second output appears, the root is notified first and keeps
        // the local window as the presenter dashboard; if it disappears the
        // existing fullscreen path immediately becomes the safe fallback.
        self.sync_audience_presentation(event_loop, &window);
        if let Some(audience) = self.audience_presentation.as_ref()
            && audience.render_init.is_some()
        {
            // Poll the worker result without ever blocking the presenter
            // event loop; this is short-lived only while GPU setup runs.
            audience.window.request_redraw();
        }
        let force_theme = self.settings_store_changed.swap(false, Ordering::AcqRel);
        let app = &mut self.app;
        if force_theme {
            if let Some(due) = crate::time::now().checked_sub(std::time::Duration::from_millis(750))
            {
                app.last_theme_store_check = due;
            }
        }

        // Catches a compositor reconfigure that changes `inner_size()`
        // without delivering `Resized`.
        if let Some(prev) = app.layout.last_frame_size
            && window.inner_size() != prev
        {
            app.layout.layout_dirty = true;
            request_primary_redraw();
        }
        if app.pump_collab_presence() {
            request_primary_redraw();
        }
        if app.poll_model_invocations() {
            request_primary_redraw();
        }
        app.publish_playback_handoff();

        // Keep pumping frames while any "just typed" highlighter mark is
        // still fading, so the taper animates instead of freezing.
        let typing_now = crate::time::now();
        app.presence_ctx.prune_typing(typing_now);
        let typing_active = app.presence_ctx.has_active_typing(typing_now);
        if typing_active {
            request_primary_redraw();
        }

        let idle_model = app.service_idle_model();
        if idle_model.changed {
            request_primary_redraw();
        }

        if idle_model.local_commit {
            if let Some(origin) = self.native_input_origin {
                crate::native_scenario::log::emit_json(
                    "tilo-native-commit-redraw",
                    serde_json::json!({
                        "after_cpu_frame": app.profiler.as_ref().and_then(|p| p.history().back()).map(|s| s.frame),
                        "at_us": origin.elapsed().as_micros(),
                    }),
                );
            }
        }

        if app.reconcile_fullscreen(&window) {
            request_primary_redraw();
        }

        // Run both: Settings' open Appearance pane may intentionally override
        // the file-level base with its richer live accessibility intent.
        let configured_changed = app.reconcile_configured_theme();
        // A settings change re-themes from the file; outside Tilo OS the host
        // appearance takes over again.
        let host_changed = self
            .host_appearance
            .as_mut()
            .and_then(|host| host.take_change(configured_changed))
            .is_some_and(|appearance| app.apply_host_appearance(appearance));
        let appearance_changed = app.reconcile_appearance();
        if configured_changed || host_changed || appearance_changed {
            app.needs_sync = true;
            request_primary_redraw();
        }

        // The active page/view can change during `sync` (e.g. the initial
        // auto-select), which the post-event broadcast above wouldn't catch.
        app.broadcast_location_presence();
        app.broadcast_view_presence();

        // Run here too, not only `RedrawRequested`, since painting isn't
        // guaranteed — a background tab, or a first frame behind a shader
        // compile.
        if app.record_tick() {
            request_primary_redraw();
        }

        // Without this a first peer with no input never re-syncs to seed its
        // doc after the grace window, so a late joiner diverges.
        if app.collaboration.manager.is_connected() && app.root_widget.needs_idle_sync() {
            app.needs_sync = true;
            request_primary_redraw();
        }

        app.broadcast_doc_delta();

        // Flag drained by the draw handler, not here — just pump a redraw.
        if app.root_widget.take_structure_dirty() {
            app.layout.structure_dirty = true;
            request_primary_redraw();
        }

        let scheduled_deadline = crate::close_admission::wake_deadline(
            app.root_widget
                .next_wake_deadline()
                .into_iter()
                .chain(self.touch_pointer.long_press_deadline())
                .min(),
            app.close_admission.save_waiting,
            crate::time::now(),
        );
        let scheduled_due =
            scheduled_deadline.is_some_and(|deadline| deadline <= crate::time::now());
        if scheduled_due {
            // A due deadline means "run sync" (poll a directory, settle
            // spellcheck), not "commit a buffer". Files' 750ms metadata
            // poll found nothing and still presented at ~2 Hz because
            // `needs_sync` forced a full redraw.
            let updated = app.update_model();
            let _ = app.sync_model(true);
            let structure = app.root_widget.take_structure_dirty();
            if structure {
                app.layout.structure_dirty = true;
            }
            if present_after_deadline_sync(
                updated || structure || app.layout.layout_dirty,
                app.root_widget.needs_animation_frame(),
            ) {
                request_primary_redraw();
            }
        }

        // The poll above already ran. Re-read what it published so a deadline
        // that is still in the past does not schedule a frame-rate follow-up.
        let scheduled_deadline = crate::close_admission::wake_deadline(
            app.root_widget
                .next_wake_deadline()
                .into_iter()
                .chain(self.touch_pointer.long_press_deadline())
                .min(),
            app.close_admission.save_waiting,
            crate::time::now(),
        );
        let widget_still_due = !scheduled_due
            && scheduled_deadline.is_some_and(|deadline| deadline <= crate::time::now());

        // A hover tooltip is not motion, so it reveals even under
        // reduce-motion — requested independently of the animation pump.
        let tooltip_pending = app.input.tooltip_host.needs_frame();
        if tooltip_pending {
            request_primary_redraw();
        }

        // Reduced motion lets the loop go idle since animated widgets
        // already paint their resting state.
        let needs_frame = app.root_widget.needs_animation_frame();
        let (animation_due, animation_deadline) = self.animation_schedule.poll(
            crate::time::now(),
            any_render_available && animation_pump_needed(needs_frame, app.theme.reduce_motion),
        );
        if animation_due {
            app.needs_sync = true;
            // Forces `apply_layout` since an animation tick (e.g. momentum
            // fling) can reposition content without an input event.
            app.layout.layout_dirty = true;
            request_primary_redraw();
            for surface in &self.floating_windows {
                if auxiliary_redraw_allowed(surface.render.as_ref()) {
                    surface.window.request_redraw();
                }
            }
            if let Some(audience) = self.audience_presentation.as_ref()
                && auxiliary_redraw_allowed(audience.render.as_ref())
            {
                audience.window.request_redraw();
            }
        }

        // A background pump can run while its visible projection is stable.
        // Redrawing only on a changed snapshot avoids full-scene GPU work on
        // every otherwise-idle collaboration wake.
        if let Some(tick) = app.presence.frame_tick.as_mut()
            && tick(&mut app.presence_ctx)
        {
            request_primary_redraw();
        }

        // `Wait` sleeps until real input, zero CPU; escalate to a bounded
        // ~60 Hz tick only while a root still wants another frame. A live
        // collaboration connection is not such a reason: its transport wakes
        // this loop for each inbound event, directory request and freed
        // outbound slot.
        let awaiting_floating_pixels = self.floating_windows.iter().any(|surface| {
            !surface.suspended
                && !surface.adopted
                && surface
                    .render
                    .as_ref()
                    .is_none_or(|render| !render.retry.occlusion_blocked(crate::time::now()))
        });
        let retry_at = retry_at.into_iter().chain(auxiliary_retry_at).min();
        // Widget deadlines are serviced by sync/redraw. While occluded they can
        // be delayed by at most the 250ms probe cap; independent history, record,
        // autosave and collaboration work above continues on its existing doors.
        let demand = crate::render_thread::retry::WakeDemand {
            typing: typing_active,
            primary_startup: awaiting_primary_first_present,
            tooltip: tooltip_pending,
            widget_due: widget_still_due,
            animation: animation_pump_needed(needs_frame, app.theme.reduce_motion),
            auxiliary_startup: awaiting_floating_pixels,
            retry_due: retry_due || auxiliary_retry_due,
        };
        match crate::render_thread::retry::next_wake(
            crate::time::now(),
            primary_blocked_until,
            any_render_available,
            demand,
            scheduled_deadline,
            retry_at,
            animation_deadline,
        ) {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

impl AppHandler {
    fn handle_audience_window_event(&mut self, event: WindowEvent) {
        let Some(audience_window) = self
            .audience_presentation
            .as_ref()
            .map(|audience| audience.window.clone())
        else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                self.app.root_widget.on_audience_presentation_closed();
                self.close_audience_presentation();
                if let Some(primary) = self.window.as_ref() {
                    primary.request_redraw();
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                audience_window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if !self.handle_audience_redraw() {
                    self.close_audience_presentation();
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
            other => {
                let handled = self.app.root_widget.handle_audience_presentation_event(
                    &other,
                    &mut self.app.state.doc,
                    &self.app.state.root,
                );
                if handled {
                    self.app.commit_action();
                    self.app.needs_sync = true;
                    self.app.layout.layout_dirty = true;
                    if let Some(primary) = self.window.as_ref() {
                        primary.request_redraw();
                    }
                    audience_window.request_redraw();
                }
            }
        }
    }

    fn handle_native_floating_window_event(&mut self, index: usize, event: WindowEvent) {
        let Some(surface) = self.floating_windows.get(index) else {
            return;
        };
        let window = surface.window.clone();
        let desktop_rect = surface.desktop_rect;
        let surface_id = surface.id.clone();
        match event {
            WindowEvent::CloseRequested => {
                if let Some(client) = self.app.floating.client.as_mut() {
                    let _ = client.send(
                        &crate::floating_surface::FloatingSurfaceClientMessage::Dismiss {
                            id: surface_id.clone(),
                        },
                    );
                }
                self.close_native_floating_window(&surface_id);
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if !self.handle_floating_redraw(index)
                    && let Some(id) = self
                        .floating_windows
                        .get(index)
                        .map(|surface| surface.id.clone())
                {
                    self.close_native_floating_window(&id);
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
            other => self.dispatch_floating_event(&window, desktop_rect, other),
        }
    }

    fn handle_primary_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
        event: WindowEvent,
    ) {
        if let Some(adapter) = self.a11y_adapter.as_mut() {
            adapter.process_event(window, &event);
        }
        #[cfg(target_os = "linux")]
        {
            let routed = self.decorations.route(&mut self.app, window, &event);
            if routed.close {
                self.close_primary_window(event_loop, window);
            }
            if routed.consumed {
                return;
            }
        }
        self.dispatch_touch_pointer_events(window, &event);
        match event {
            WindowEvent::CloseRequested => self.close_primary_window(event_loop, window),
            WindowEvent::Resized(_) => {
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                crate::widget::mount_native_views(
                    self.app.root_widget.as_mut(),
                    &crate::widget::NativeViewHost::live(window, self.app.input.platform),
                );
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.handle_redraw_requested(event_loop, window),
            WindowEvent::ModifiersChanged(modifiers) => self.app.input.modifiers = modifiers,
            other if self.app.input.platform.platform() == crate::platform::Platform::Android => {
                if let Some(escape) = crate::interaction::back_key_as_escape(&other) {
                    self.handle_back_key(event_loop, window, escape);
                } else {
                    self.handle_other_window_event(window, other);
                }
            }
            other => {
                self.handle_other_window_event(window, other);
            }
        }
    }

    fn close_primary_window(&mut self, event_loop: &ActiveEventLoop, window: &Arc<Window>) {
        match crate::component_lifecycle::on_window_close_requested(self.app.root_widget.as_mut()) {
            crate::widget::WindowCloseAdmission::Allow => {
                if self.app.admit_native_close_request() {
                    event_loop.exit();
                } else {
                    window.request_redraw();
                }
            }
            crate::widget::WindowCloseAdmission::Defer => {
                self.app.needs_sync = true;
                self.app.layout.layout_dirty = true;
                window.request_redraw();
            }
        }
    }

    /// Back dismisses what Escape would, then navigates back, and at the root
    /// closes the app; exiting the event loop lets Android finish the activity.
    fn handle_back_key(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
        escape: WindowEvent,
    ) {
        let pressed = matches!(
            &escape,
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed
        );
        if self.handle_other_window_event(window, escape) || !pressed {
            return;
        }
        let app = &mut self.app;
        if app.root_widget.on_nav_gesture(
            crate::gesture::NavDirection::Back,
            &mut app.state.doc,
            &app.state.root,
        ) {
            app.needs_sync = true;
            window.request_redraw();
            return;
        }
        self.close_primary_window(event_loop, window);
    }

    fn dispatch_touch_pointer_events(&mut self, window: &Arc<Window>, event: &WindowEvent) {
        let WindowEvent::Touch(touch) = event else {
            return;
        };
        self.app
            .set_interaction_environment(self.app.input.interaction.with_touch_modality());
        let scale = window.scale_factor();
        let app = &self.app;
        let pointer_events = self.touch_pointer.events(touch, |position| {
            app.claims_raw_touch_at(vello::kurbo::Point::new(
                position.x / scale,
                position.y / scale,
            ))
        });
        self.dispatch_touch_pointer(window, touch.device_id, pointer_events);
    }

    fn dispatch_touch_pointer(
        &mut self,
        window: &Arc<Window>,
        device_id: winit::event::DeviceId,
        events: Vec<crate::interaction::TouchPointerEvent>,
    ) {
        self.dispatching_touch_pointer = true;
        for pointer_event in events {
            self.handle_other_window_event(window, pointer_event.window_event(device_id));
        }
        self.dispatching_touch_pointer = false;
    }

    /// A still finger that reached the long-press delay becomes a secondary
    /// click even though no touch event marks the moment.
    fn poll_touch_hold(&mut self, window: &Arc<Window>) {
        if let Some((device_id, events)) = self.touch_pointer.poll(crate::time::now()) {
            self.dispatch_touch_pointer(window, device_id, events);
            window.request_redraw();
        }
    }
}

impl App {
    /// Run this application on the platform event loop.
    pub fn run(self) {
        self.run_with_setup(|_| {});
    }

    #[cfg(windows)]
    pub(crate) fn with_startup_renderer(mut self, renderer: Option<RendererPreparation>) -> Self {
        self.startup_renderer = renderer;
        self
    }

    /// Like [`App::run`] but exposes the winit [`EventLoopProxy`] before the
    /// loop starts, so callers can spawn threads that inject [`UserEvent`]s.
    ///
    /// # Panics
    ///
    /// Panics if the platform windowing system can't create the event loop.
    /// Once running, a Wayland compositor disconnect is a normal application
    /// shutdown boundary (session logout, compositor restart), so an
    /// `ExitFailure` is logged and returned rather than converted into a panic.
    #[allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "event-loop creation/run failure is unrecoverable at process startup; the public run()/run_with_setup() signature is called from every app's main() and returning Result here would ripple across the whole app surface for a failure with no fallback"
    )]
    pub fn run_with_setup<F>(mut self, setup: F)
    where
        F: FnOnce(EventLoopProxy<UserEvent>),
    {
        assert!(
            self.wake_scope.is_some(),
            "live App requires from_factory, try_from_factory, or from_scoped_parts"
        );
        self.prepare_native_presentation(crate::host_chrome::HostChrome::current(
            self.input.platform,
        ));
        crate::host::native::install_default_if_unset(self.window.app_id.as_deref());
        // The event loop never waits on async work: anything that must is
        // spawned onto the app runtime and handed back through the wake scope.
        tilo_rt::mark_ui_thread();
        let mut event_loop_builder = EventLoop::<UserEvent>::with_user_event();
        #[cfg(target_os = "macos")]
        if crate::host::environment().diagnostics.window_no_activate {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            // Native scripted input does not need to take the user's keyboard.
            // Keep normal launches unchanged; this is an opt-in profiling mode.
            event_loop_builder.with_activate_ignoring_other_apps(false);
        }
        #[cfg(target_os = "android")]
        {
            use winit::platform::android::EventLoopBuilderExtAndroid;
            event_loop_builder.with_android_app(
                crate::launch::android::app()
                    .expect("an Android app starts through ui::android_main!")
                    .clone(),
            );
        }
        let event_loop = event_loop_builder.build().unwrap();
        tilo_host_search::forward_to_runtime();
        let proxy = event_loop.create_proxy();
        super::install_dev_session_stop(proxy.clone());
        #[cfg(target_os = "android")]
        super::android_text_input::bridge::install(proxy.clone());
        let wake_proxy = proxy.clone();
        self.wake_scope
            .as_mut()
            .unwrap()
            .bind(move |event| {
                let _ = wake_proxy.send_event(UserEvent::Wake(event));
            })
            .expect("App wake scope must be open and unbound at native startup");
        // The AccessKit adapter routes events back through this as
        // `UserEvent::AccessKit`.
        let a11y_proxy = proxy.clone();
        #[allow(
            clippy::panic,
            reason = "invalid explicitly requested benchmark input must stop startup rather than silently run an unmeasured scenario"
        )]
        let native_scenario = crate::native_scenario::load_from_env()
            .unwrap_or_else(|error| panic!("invalid native performance scenario: {error}"));
        assert!(
            native_scenario.is_none() || self.profiler.is_some(),
            "native performance scenarios require TILO_PROFILE_OUT or TILO_PROFILE"
        );
        let settings_store_changed = Arc::new(AtomicBool::new(false));
        let settings_watch = {
            let _entry = self.enter_wake_scope();
            self.watch_system_settings
                .then(|| {
                    let pending = Arc::clone(&settings_store_changed);
                    let wake = crate::wake::WakeHandle::new();
                    settings_store::watch::watch_settings(move || {
                        pending.store(true, Ordering::Release);
                        wake.wake();
                    })
                    .ok()
                })
                .flatten()
        };
        let host_appearance = {
            let _entry = self.enter_wake_scope();
            setup(proxy);
            self.init_layout();
            self.watch_system_settings
                .then(crate::host_appearance::Source::start)
                .flatten()
        };

        let mut handler = AppHandler {
            app: self,
            window: None,
            render: None,
            surface_suspended: false,
            suspended_renderer: None,
            audience_presentation: None,
            floating_windows: Vec::new(),
            a11y_adapter: None,
            a11y_proxy,
            ime_state: None,
            #[cfg(target_os = "android")]
            text_input: super::android_text_input::TextInputSession::default(),
            #[cfg(target_os = "android")]
            text_input_touched: false,
            primary_frames_sent: 0,
            primary_frames_completed: 0,
            last_primary_frame_presented: 0,
            primary_retry: crate::render_thread::retry::RenderRetryState::default(),
            floating_source_clear_frames: std::collections::HashMap::new(),
            floating_destination_present_frames: std::collections::HashMap::new(),
            touch_pointer: crate::interaction::TouchPointerEmulator::default(),
            dispatching_touch_pointer: false,
            native_scenario,
            native_input_ids: Vec::new(),
            native_input_origin: None,
            native_input_pointer_passthrough: false,
            native_input_route_isolation: false,
            animation_schedule: animation_schedule::AnimationSchedule::default(),
            dev_window_state: std::collections::HashMap::new(),
            dev_loaded_library: crate::host::environment()
                .dev_session
                .as_deref()
                .and_then(dev_reload_library),
            dev_reload_timing: None,
            #[cfg(any(target_os = "macos", windows))]
            host_chrome: None,
            #[cfg(any(target_os = "macos", windows))]
            host_commands_key: None,
            #[cfg(target_os = "linux")]
            decorations: linux_header::Decorations::Unsettled,
            host_appearance,
            settings_store_changed,
            _settings_watch: settings_watch,
        };
        match event_loop.run_app(&mut handler) {
            Ok(()) => {}
            Err(winit::error::EventLoopError::ExitFailure(status)) => {
                tracing::debug!(status, "window system disconnected; application exiting");
            }
            Err(error) => {
                tracing::error!(%error, "application event loop failed; application exiting");
            }
        }
    }
}

impl AppHandler {
    fn note_host_appearance_event(&mut self, event: &WindowEvent) {
        let Some(host) = self.host_appearance.as_mut() else {
            return;
        };
        match event {
            WindowEvent::ThemeChanged(theme) => {
                host.note_window_theme(Some(*theme));
                host.refresh();
            }
            WindowEvent::Focused(true) => host.refresh(),
            _ => {}
        }
    }

    fn apply_host_appearance(&mut self, force: bool) -> bool {
        self.host_appearance
            .as_mut()
            .and_then(|host| host.take_change(force))
            .is_some_and(|appearance| self.app.apply_host_appearance(appearance))
    }
}

/// Follow the system chrome drawn over the window. Android reports it
/// from `TiloActivity`'s insets listener; elsewhere it stays zero.
fn sync_safe_area(app: &mut App, window: &Arc<Window>) {
    #[cfg(target_os = "android")]
    {
        let area = crate::scale::SafeArea::from_physical(
            crate::launch::android::safe_area_insets(),
            window.scale_factor(),
        );
        if area != app.layout.safe_area {
            app.layout.safe_area = area;
            app.layout.layout_dirty = true;
            window.request_redraw();
        }
    }
    #[cfg(not(target_os = "android"))]
    let _ = (app, window);
}

/// A due `next_wake_deadline` runs sync. Present only when that sync
/// changed the model, the tree, or an in-flight animation. `needs_redraw`
/// is not a reason: Files stays `needs_redraw` after `mark_drawn` (closed
/// menus and other default leaves), so treating it as dirty recommits
/// every 750ms metadata poll.
fn present_after_deadline_sync(model_or_layout_dirty: bool, needs_animation: bool) -> bool {
    model_or_layout_dirty || needs_animation
}

#[cfg(test)]
mod deadline_present_tests {
    use super::present_after_deadline_sync;

    #[test]
    fn idle_scheduled_sync_does_not_request_frames() {
        assert!(
            !present_after_deadline_sync(false, false),
            "an idle scheduled poll must not keep requesting frames"
        );
        // Sticky `needs_redraw` after mark_drawn is not an argument: the
        // helper no longer takes that bit. Model/structure/layout OR into
        // the first flag at the call site (keeps clippy's bool limit).
        assert!(present_after_deadline_sync(false, true));
        assert!(present_after_deadline_sync(true, false));
    }
}
