//! What a launch prepares for its window before the app is constructed.
//!
//! The GPU device and vello's renderer are prepared on their own thread from
//! process start (`prepare_native_renderer`). On a Wayland display the launch
//! also builds winit's event loop and the primary window at once, and hands
//! the window to that thread, which creates and configures its surface while
//! the app constructs, and while a thread beside it builds vello's pipelines.
//!
//! Without this, a launch constructed its app first and only then built the
//! event loop (the Wayland connection and two roundtrips), created its window
//! (a roundtrip, then a wait for the first `xdg_surface.configure`) and, at
//! the first redraw, created and configured the window's surface (Mesa's WSI
//! makes roundtrips of its own), all on the main thread. None of that needs
//! the app: it needs the connection and the device. The first redraw now only
//! lays out and draws.
//!
//! The early window carries the id it would have had
//! (`primary_window_app_id`) and a provisional title. `resumed` keeps it when
//! the constructed app wants a window of that id and sets the app's title on
//! it; otherwise (an input surface announces its own id) it makes the window
//! the app wants, as without this module, and the first redraw discards the
//! early surface (`create_window_render_prepared`).

use std::sync::{Arc, mpsc};

use winit::event_loop::EventLoop;
use winit::window::{Window, WindowId};

use super::{
    EARLY_EVENT_LOOP, EARLY_WINDOW, NativeRendererPlan, PreparedRenderer, RenderContext, UserEvent,
    create_native_renderer, primary_window_app_id, primary_window_attributes, window_surface,
};

/// What the renderer preparation hands the first redraw.
pub(crate) struct StartupRender {
    pub(super) prepared: PreparedRenderer,
    /// The early window's configured surface, when the launch made one.
    pub(super) surface: Option<(WindowId, vello::util::RenderSurface<'static>)>,
}

/// The renderer preparation started at process start.
pub(crate) struct RendererPreparation {
    result: mpsc::Receiver<Result<StartupRender, String>>,
    /// The launch made the primary window early (a Wayland display), so the
    /// preparation also makes its surface off the main thread.
    early_window: bool,
}

impl RendererPreparation {
    /// Whether the launch made the primary window early.
    pub(crate) fn made_early_window(&self) -> bool {
        self.early_window
    }

    /// What the preparation made, once it has.
    pub(super) fn wait(self) -> Result<Result<StartupRender, String>, mpsc::RecvError> {
        self.result.recv()
    }
}

/// Start only for a real native launch, before loading app state and fonts.
/// This owns no surface until the launch's early window exists, and can
/// safely finish if construction fails: dropping the receiving end drops the
/// prepared GPU resources at send.
///
/// It runs on its own thread, never in the event loop: on Wayland, device
/// creation inside `resumed` delayed its seat wiring, and mutter then left a
/// first launch's toplevel unfocused with dead input. The device is chosen
/// without a surface; the launcher's GPU pin (`VK_DRIVER_FILES`, the EGL
/// vendor) is this process's environment, so it holds here as well. If the
/// window's surface turns out to need another device, the renderer is built
/// for that one instead (`prepare_window_surface`,
/// `create_window_render_prepared`).
///
/// Then, on the calling (main) thread, the event loop and on Wayland the
/// primary window are made (`open_window_early`).
#[allow(
    clippy::disallowed_methods,
    reason = "native host runner: GPU preparation overlaps app start on its own thread"
)]
pub(crate) fn prepare_native_renderer() -> Option<RendererPreparation> {
    if std::env::var_os("TILO_SERIAL_RENDER_INIT").is_some() {
        return None;
    }
    // Frames drawn by the room's render service: connect to it while the app
    // constructs instead of opening a device. The early window is still made.
    #[cfg(target_os = "linux")]
    if crate::render_thread::remote::requested() {
        crate::render_thread::remote::prepare();
        let (offer, _unwanted) = mpsc::channel();
        let (_never, result) = mpsc::channel();
        let early_window = open_window_early(offer);
        return Some(RendererPreparation {
            result,
            early_window,
        });
    }
    let (sender, receiver) = mpsc::channel();
    let (window_offer, window_wanted) = mpsc::channel::<Arc<Window>>();
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
                // vello's pipelines and the early window's surface need only
                // the device, not each other, and the first frame waits for
                // both: they are made side by side.
                let plan = NativeRendererPlan::for_device(device);
                let gpu = device.device.clone();
                let (renderer, surface) =
                    renderer_beside_surface(&mut context, &plan, &gpu, &window_wanted);
                let device = context
                    .devices
                    .get(device_id)
                    .ok_or_else(|| "prepared drawing device index out of range".to_string())?;
                let renderer = plan.finish(device, renderer?);
                tilo_startup_metrics::mark("native-render-prepare-done");
                let mut prepared = PreparedRenderer {
                    context,
                    device_id,
                    renderer,
                };
                let surface =
                    surface.and_then(|surface| adopt_window_surface(&mut prepared, surface));
                Ok(StartupRender { prepared, surface })
            };
            let _ = sender.send(prepare());
        }) {
        Ok(_) => {
            // Consumes the offer: sent with the window, or dropped unsent,
            // which lets the preparation finish without one.
            let early_window = open_window_early(window_offer);
            Some(RendererPreparation {
                result: receiver,
                early_window,
            })
        }
        Err(error) => {
            tracing::warn!(%error, "could not prepare renderer in parallel");
            None
        }
    }
}

/// vello's renderer built from `plan` on a scoped thread of its own, while
/// this one makes the early window's surface once the launch made the window
/// (or at once without a surface when it made none). The build needs only the
/// device; the surface needs the context.
fn renderer_beside_surface(
    context: &mut RenderContext,
    plan: &NativeRendererPlan,
    gpu: &vello::wgpu::Device,
    window_wanted: &mpsc::Receiver<Arc<Window>>,
) -> (
    Result<vello::Renderer, String>,
    Option<(WindowId, vello::util::RenderSurface<'static>)>,
) {
    std::thread::scope(|scope| {
        let building = scope.spawn(|| plan.build(gpu));
        let surface = window_wanted
            .recv()
            .ok()
            .and_then(|window| early_window_surface(context, window));
        let renderer = building
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (renderer, surface)
    })
}

/// The early window's surface, created and configured. `None` leaves the
/// surface to the first redraw, as without an early window.
fn early_window_surface(
    context: &mut RenderContext,
    window: Arc<Window>,
) -> Option<(WindowId, vello::util::RenderSurface<'static>)> {
    let id = window.id();
    match window_surface(context, window, false) {
        Ok(surface) => Some((id, surface)),
        Err(error) => {
            tracing::warn!(%error, "could not prepare the window's surface; the first redraw makes it");
            None
        }
    }
}

/// Keep the early window's configured surface for the first redraw. The
/// renderer is rebuilt when the surface can be presented only from another
/// device.
fn adopt_window_surface(
    prepared: &mut PreparedRenderer,
    (id, surface): (WindowId, vello::util::RenderSurface<'static>),
) -> Option<(WindowId, vello::util::RenderSurface<'static>)> {
    if surface.dev_id != prepared.device_id {
        let device = prepared.context.devices.get(surface.dev_id)?;
        tilo_startup_metrics::mark("native-render-prepare-discarded");
        match create_native_renderer(device) {
            Ok(renderer) => {
                prepared.renderer = renderer;
                prepared.device_id = surface.dev_id;
            }
            Err(error) => {
                tracing::warn!(%error, "could not prepare a renderer for the window's device");
                return None;
            }
        }
    }
    tilo_startup_metrics::mark("native-render-prepare-surface-done");
    Some((id, surface))
}

/// Build the event loop now and, on a Wayland display, the primary window,
/// and offer the window to the renderer preparation; both wait in this
/// thread's slots for the runner (`take_early_event_loop`,
/// `take_early_window`). Whatever happens, the preparation stops waiting for
/// a window here. Without a session to connect to, the event loop is left to
/// `App::run_with_setup`, which builds it (and fails) as it always did.
fn open_window_early(offer: mpsc::Sender<Arc<Window>>) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("WAYLAND_SOCKET").is_none()
    {
        return false;
    }
    tilo_startup_metrics::mark("event-loop-build-start");
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            tracing::debug!(%error, "no early event loop; the app builds it after constructing");
            return false;
        }
    };
    tilo_startup_metrics::mark("event-loop-build-done");
    let mut made = false;
    if on_wayland(&event_loop) {
        let app_id = primary_window_app_id(None);
        let attributes =
            primary_window_attributes(super::resolve_window_title(None), None, &app_id);
        #[allow(
            deprecated,
            reason = "winit deprecates making a window before the loop runs because macOS, iOS and the web cannot; this runs only on a Wayland display, where window creation completes its own first configure"
        )]
        let created = event_loop.create_window(attributes);
        match created {
            Ok(window) => {
                tilo_startup_metrics::mark("startup-window-created");
                let window = Arc::new(window);
                made = offer.send(Arc::clone(&window)).is_ok();
                EARLY_WINDOW.with(|slot| *slot.borrow_mut() = Some((app_id, window)));
            }
            Err(error) => {
                tracing::debug!(%error, "no early window; `resumed` makes it");
            }
        }
    }
    EARLY_EVENT_LOOP.with(|slot| *slot.borrow_mut() = Some(event_loop));
    made
}

fn on_wayland(event_loop: &EventLoop<UserEvent>) -> bool {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    event_loop
        .display_handle()
        .is_ok_and(|display| matches!(display.as_raw(), RawDisplayHandle::Wayland(_)))
}
