//! X11 clients through Xwayland.
//!
//! [`Compositor::start_xwayland`](crate::Compositor::start_xwayland) starts
//! a rootless Xwayland on a free display; once it is ready, the window
//! manager ([`X11Wm`]) maps its windows into the same stack as Wayland
//! ones. Managed windows are placed and laid out by the [`WindowPolicy`]
//! like any toplevel; override-redirect windows (menus, tooltips) sit where
//! the client put them. Keyboard focus reaches an X11 window through its
//! `wl_surface`, which Xwayland routes to the X window.
//!
//! [`WindowPolicy`]: crate::WindowPolicy

use std::os::unix::net::UnixStream;

use smithay::desktop::Window;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler};

use crate::state::State;
use crate::{Error, LoopData, io};

/// Start Xwayland and return its display number (for `DISPLAY`). Clients
/// may connect at once: Xwayland accepts on the sockets prepared for it.
pub(crate) fn start(handle: &LoopHandle<'static, LoopData>, state: &State) -> Result<u32, Error> {
    let (xwayland, client) = XWayland::spawn(
        &state.display,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        std::process::Stdio::null(),
        std::process::Stdio::inherit(),
        |_| {},
    )?;
    let display = xwayland.display_number();
    let wm_handle = handle.clone();
    handle
        .insert_source(xwayland, move |event, (), data| match event {
            XWaylandEvent::Ready { x11_socket, .. } => {
                start_wm(&wm_handle, data, x11_socket, client.clone());
            }
            XWaylandEvent::Error => tracing::warn!("Xwayland did not start"),
        })
        .map_err(|error| io(error.error))?;
    Ok(display)
}

fn start_wm(
    handle: &LoopHandle<'static, LoopData>,
    data: &mut LoopData,
    socket: UnixStream,
    client: smithay::reexports::wayland_server::Client,
) {
    match X11Wm::start_wm(handle.clone(), socket, client) {
        Ok(wm) => data.state.xwm = Some(wm),
        Err(error) => tracing::warn!(%error, "X11 window manager did not start"),
    }
}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell
    }
}

impl XWaylandShellHandler for LoopData {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.state.xwayland_shell
    }
}

smithay::delegate_xwayland_shell!(State);

impl State {
    /// The element showing `surface`, if it is mapped.
    fn element_of(&self, surface: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| window.x11_surface() == Some(surface))
            .cloned()
    }

    fn unmap(&mut self, surface: &X11Surface) {
        if let Some(window) = self.element_of(surface) {
            self.space.unmap_elem(&window);
            self.rearrange();
        }
    }
}

impl XwmHandler for State {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Events only come from the window manager that was started.
        #[allow(
            clippy::expect_used,
            reason = "X11 events only arrive once the wm exists"
        )]
        self.xwm.as_mut().expect("X11 window manager")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Err(error) = surface.set_mapped(true) {
            tracing::warn!(?error, "X11 window not mapped");
            return;
        }
        let window = Window::new_x11_window(surface);
        self.map_window(&window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        let location = surface.geometry().loc;
        let window = Window::new_x11_window(surface);
        self.space.map_element(window, location, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.unmap(&surface);
        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.unmap(&surface);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // A mapped managed window goes where the policy put it; it may only
        // change its size. An unmapped one may ask for anything.
        let mapped = self.element_of(&surface).is_some();
        let mut geometry = surface.geometry();
        if !mapped {
            geometry.loc.x = x.unwrap_or(geometry.loc.x);
            geometry.loc.y = y.unwrap_or(geometry.loc.y);
        }
        if let Some(w) = w.and_then(|w| i32::try_from(w).ok()) {
            geometry.size.w = w;
        }
        if let Some(h) = h.and_then(|h| i32::try_from(h).ok()) {
            geometry.size.h = h;
        }
        let _ = surface.configure(geometry);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        // Override-redirect windows place themselves.
        if surface.is_override_redirect()
            && let Some(window) = self.element_of(&surface)
        {
            self.space.map_element(window, geometry.loc, false);
        }
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _resize_edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}
}

/// The loop's data answers the window manager's events with the state's
/// handler: the window manager runs in the loop, the protocol in the state.
impl XwmHandler for LoopData {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        self.state.xwm_state(xwm)
    }

    fn new_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.new_window(xwm, window);
    }

    fn new_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.new_override_redirect_window(xwm, window);
    }

    fn map_window_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.map_window_request(xwm, window);
    }

    fn mapped_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.mapped_override_redirect_window(xwm, window);
    }

    fn unmapped_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unmapped_window(xwm, window);
    }

    fn destroyed_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.destroyed_window(xwm, window);
    }

    fn configure_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        reorder: Option<Reorder>,
    ) {
        self.state
            .configure_request(xwm, window, x, y, w, h, reorder);
    }

    fn configure_notify(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<u32>,
    ) {
        self.state.configure_notify(xwm, window, geometry, above);
    }

    fn resize_request(
        &mut self,
        xwm: XwmId,
        window: X11Surface,
        button: u32,
        resize_edge: ResizeEdge,
    ) {
        self.state.resize_request(xwm, window, button, resize_edge);
    }

    fn move_request(&mut self, xwm: XwmId, window: X11Surface, button: u32) {
        self.state.move_request(xwm, window, button);
    }
}

/// The `wl_surface` an X11 window draws into, once Xwayland told us.
pub(crate) fn x11_wl_surface(window: &Window) -> Option<WlSurface> {
    window.x11_surface().and_then(X11Surface::wl_surface)
}
