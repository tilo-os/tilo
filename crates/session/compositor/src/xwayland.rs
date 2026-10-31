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

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use smithay::desktop::Window;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle, SERIAL_COUNTER};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, WmWindowProperty, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler};

use crate::policy::WindowRequest;
use crate::state::State;
use crate::{Error, LoopData, io};

/// The directory Xwayland listens in: the compositor opens its sockets
/// there, so it has to exist before the server starts. A session cannot rely
/// on the X server package's tmpfiles having made it.
fn socket_directory() -> Result<(), Error> {
    const DIRECTORY: &str = "/tmp/.X11-unix";
    match std::fs::create_dir(DIRECTORY) {
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(DIRECTORY, std::fs::Permissions::from_mode(0o1777))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Start Xwayland and return its display number (for `DISPLAY`). Clients
/// may connect at once: Xwayland accepts on the sockets prepared for it.
pub(crate) fn start(handle: &LoopHandle<'static, LoopData>, state: &State) -> Result<u32, Error> {
    socket_directory()?;
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
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                start_wm(&wm_handle, data, x11_socket, client.clone(), display_number);
                data.state.start_xdnd(display_number);
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
    display: u32,
) {
    match X11Wm::start_wm(handle.clone(), socket, client.clone()) {
        Ok(wm) => data.state.xwm = Some(wm),
        Err(error) => {
            tracing::warn!(%error, "X11 window manager did not start");
            return;
        }
    }
    // The session's own connection to the X server: it reads what a window
    // says about its program, gives X input focus and tells X clients how
    // dense the display is and which pointer to draw.
    match crate::x11_windows::X11Windows::connect(display, client) {
        Ok(mut x11) => {
            x11.publish_x_hints(&mut data.state);
            data.x11_windows = Some(x11);
        }
        Err(error) => tracing::warn!(%error, "X11 windows have no identity"),
    }
}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell
    }

    /// Xwayland associates a window's `wl_surface` only after the window
    /// mapped, so the keyboard focus the policy gave it had no surface to
    /// go to yet: it is given now. A window another one took the focus from
    /// since is no longer activated and keeps it.
    fn surface_associated(&mut self, _xwm: XwmId, _surface: WlSurface, window: X11Surface) {
        if !window.is_activated() {
            return;
        }
        if let Some(element) = self.element_of(&window) {
            self.focus_window(&element, SERIAL_COUNTER.next_serial());
        }
    }
}

impl XWaylandShellHandler for LoopData {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.state.xwayland_shell
    }

    fn surface_associated(&mut self, xwm: XwmId, surface: WlSurface, window: X11Surface) {
        XWaylandShellHandler::surface_associated(&mut self.state, xwm, surface, window);
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
            self.keep_closing(&window);
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
        // The drag-and-drop proxy takes X11 drags; it is never shown.
        if self.dnd_proxy(&surface) {
            return;
        }
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
        // An X11 app has no client of its own here: its window going says
        // it exited.
        if !surface.is_override_redirect()
            && let Some(pid) = surface.pid()
            && let Ok(mut exited) = self.exited.lock()
        {
            exited.push(pid);
        }
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
        surface: X11Surface,
        _button: u32,
        resize_edge: ResizeEdge,
    ) {
        if let Some(window) = self.element_of(&surface) {
            let request = WindowRequest::Resize(crate::requests::x11_edges(resize_edge));
            self.window_request(&window, &request, None);
        }
    }

    fn move_request(&mut self, _xwm: XwmId, surface: X11Surface, _button: u32) {
        if let Some(window) = self.element_of(&surface) {
            self.window_request(&window, &WindowRequest::Move, None);
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.x11_request(&surface, &WindowRequest::Maximize(true));
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.x11_request(&surface, &WindowRequest::Maximize(false));
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.x11_request(
            &surface,
            &WindowRequest::Fullscreen {
                on: true,
                output: None,
            },
        );
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.x11_request(
            &surface,
            &WindowRequest::Fullscreen {
                on: false,
                output: None,
            },
        );
    }

    fn minimize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        self.x11_request(&surface, &WindowRequest::Minimize);
    }

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        self.x11_selection_access(xwm)
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        self.x11_reads_selection(selection.into(), &mime_type, fd);
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        self.x11_set_selection(selection.into(), mime_types);
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        self.x11_cleared_selection(selection.into());
    }
}

impl State {
    /// An X11 window's request; one not mapped yet just keeps its state.
    fn x11_request(&mut self, surface: &X11Surface, request: &WindowRequest) {
        match self.element_of(surface) {
            Some(window) => self.window_request(&window, request, None),
            None => {
                let _ = match request {
                    WindowRequest::Maximize(on) => surface.set_maximized(*on),
                    WindowRequest::Fullscreen { on, .. } => surface.set_fullscreen(*on),
                    _ => Ok(()),
                };
            }
        }
    }
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
        // What the window says about its program is kept before it shows, so
        // the desktop knows whose window it is from the first frame.
        if let Some(x11) = self.x11_windows.as_mut() {
            x11.learn(&window);
        }
        self.state.map_window_request(xwm, window);
    }

    fn mapped_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.mapped_override_redirect_window(xwm, window);
    }

    fn unmapped_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unmapped_window(xwm, window);
    }

    fn destroyed_window(&mut self, xwm: XwmId, window: X11Surface) {
        if let Some(x11) = self.x11_windows.as_mut() {
            x11.forget(window.window_id());
        }
        self.state.destroyed_window(xwm, window);
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        let renamed = matches!(
            property,
            WmWindowProperty::Class | WmWindowProperty::StartupId | WmWindowProperty::Pid
        );
        if renamed && let Some(x11) = self.x11_windows.as_mut() {
            x11.learn(&window);
        }
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

    fn maximize_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.maximize_request(xwm, window);
    }

    fn unmaximize_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unmaximize_request(xwm, window);
    }

    fn fullscreen_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.fullscreen_request(xwm, window);
    }

    fn unfullscreen_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unfullscreen_request(xwm, window);
    }

    fn minimize_request(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.minimize_request(xwm, window);
    }

    fn allow_selection_access(&mut self, xwm: XwmId, selection: SelectionTarget) -> bool {
        self.state.allow_selection_access(xwm, selection)
    }

    fn send_selection(
        &mut self,
        xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        self.state.send_selection(xwm, selection, mime_type, fd);
    }

    fn new_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        XwmHandler::new_selection(&mut self.state, xwm, selection, mime_types);
    }

    fn cleared_selection(&mut self, xwm: XwmId, selection: SelectionTarget) {
        self.state.cleared_selection(xwm, selection);
    }
}

/// The `wl_surface` an X11 window draws into, once Xwayland told us.
pub(crate) fn x11_wl_surface(window: &Window) -> Option<WlSurface> {
    window.x11_surface().and_then(X11Surface::wl_surface)
}
