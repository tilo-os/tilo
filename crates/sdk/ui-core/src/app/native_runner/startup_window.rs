//! The primary window, made before the app is constructed.
//!
//! A launch used to construct its app first and only then build winit's
//! event loop (the Wayland connection and two roundtrips), create its window
//! (a roundtrip, then a wait for the first `xdg_surface.configure`) and, at
//! the first redraw, create and configure the window's GPU surface (Mesa's
//! WSI makes roundtrips of its own). None of that needs the app: it needs
//! the Wayland connection and the GPU device. So on Wayland the launch makes
//! the event loop and the window first, and hands the window to the
//! renderer preparation, which creates and configures its surface while the
//! app constructs. The first redraw then only lays out and draws.
//!
//! The window carries the id it would have had (`primary_window_app_id`)
//! and a provisional title. `resumed` keeps it when the constructed app
//! wants a window of that id, sets the app's title on it, and otherwise
//! makes the window it wants, as without this module; the early surface is
//! then discarded at the first redraw (`create_window_render_prepared`).

use std::sync::Arc;

use winit::event_loop::EventLoop;
use winit::window::Window;

use super::{RendererPreparation, UserEvent, primary_window_app_id, primary_window_attributes};

/// What the launch made before constructing the app.
#[derive(Default)]
pub(crate) struct EarlyWindow {
    event_loop: Option<EventLoop<UserEvent>>,
    /// The window and the app id it announces.
    window: Option<(Arc<Window>, String)>,
}

impl RendererPreparation {
    /// Build the event loop now and, on a Wayland display, the primary window,
    /// and offer the window to the renderer preparation. Whatever happens, the
    /// preparation stops waiting for a window here. A failure leaves the event
    /// loop to `App::run_with_setup`, which builds it as it always did.
    pub(crate) fn open_window_early(&mut self) {
        let offer = self.window_offer.take();
        tilo_startup_metrics::mark("event-loop-build-start");
        let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
            Ok(event_loop) => event_loop,
            Err(error) => {
                tracing::debug!(%error, "no early event loop; the app builds it after constructing");
                return;
            }
        };
        tilo_startup_metrics::mark("event-loop-build-done");
        if on_wayland(&event_loop) {
            let app_id = primary_window_app_id(None);
            let attributes =
                primary_window_attributes(super::resolve_window_title(None), None, &app_id);
            #[allow(
                deprecated,
                reason = "winit deprecates making a window before the loop runs because macOS, iOS and the web cannot; this path runs only on a Wayland display, where window creation completes its own first configure"
            )]
            let created = event_loop.create_window(attributes);
            match created {
                Ok(window) => {
                    tilo_startup_metrics::mark("startup-window-created");
                    let window = Arc::new(window);
                    if let Some(offer) = offer {
                        let _ = offer.send(Arc::clone(&window));
                    }
                    self.early.window = Some((window, app_id));
                }
                Err(error) => {
                    tracing::debug!(%error, "no early window; `resumed` makes it");
                }
            }
        }
        self.early.event_loop = Some(event_loop);
    }

    /// The event loop the launch built, once.
    pub(crate) fn take_event_loop(&mut self) -> Option<EventLoop<UserEvent>> {
        self.early.event_loop.take()
    }

    /// The window the launch made and the app id it announces, once.
    pub(crate) fn take_window(&mut self) -> Option<(Arc<Window>, String)> {
        self.early.window.take()
    }
}

fn on_wayland(event_loop: &EventLoop<UserEvent>) -> bool {
    use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    event_loop
        .display_handle()
        .is_ok_and(|display| matches!(display.as_raw(), RawDisplayHandle::Wayland(_)))
}
