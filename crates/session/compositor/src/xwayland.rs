//! X11 clients through Xwayland: one display per workspace.
//!
//! [`crate::X11Displays`] binds a workspace's display and starts its server
//! when an app connects to it; this module turns that server into toplevels
//! the desktop places. Once a display is ready, its window manager
//! ([`X11Wm`]) maps its windows into the same stack as Wayland ones. Managed
//! windows are placed and laid out by the [`WindowPolicy`] like any
//! toplevel; override-redirect windows (menus, tooltips) sit where the client
//! put them. Keyboard focus reaches an X11 window through its `wl_surface`,
//! which Xwayland routes to the X window.
//!
//! A display's server stops when its last app goes ([`detach`]); the display
//! stays for the next app.
//!
//! [`WindowPolicy`]: crate::WindowPolicy

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use smithay::desktop::Window;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle, SERIAL_COUNTER};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, WmWindowProperty, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::policy::WindowRequest;
use crate::state::{State, X11Server};
use crate::{Error, LoopData};

/// The directory Xwayland listens in: the compositor opens its sockets there, so it has to exist
/// before a display is bound. A session cannot rely on the X server package's tmpfiles having made
/// it.
pub(crate) fn socket_directory() -> Result<(), Error> {
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

/// Give the X11 display `number` a window manager: its windows become toplevels of the desktop.
/// `None` when the display cannot be managed, and then nothing can show there.
pub(crate) fn start_wm(
    data: &mut LoopData,
    socket: UnixStream,
    client: Client,
    number: u32,
) -> Option<XwmId> {
    let Some(handle) = data.state.loop_handle.clone() else {
        tracing::warn!("no event loop for the X11 server on :{number}");
        return None;
    };
    let wm = match X11Wm::start_wm(handle, socket, client.clone()) {
        Ok(wm) => wm,
        Err(error) => {
            tracing::warn!("the X11 window manager on :{number} did not start: {error}");
            return None;
        }
    };
    let xwm = wm.id();
    data.state.xwms.push(X11Server {
        wm,
        windows: None,
        display: number,
    });
    // The session's own connection to the X server: it reads what a window says about its program,
    // gives X input focus and tells X clients how dense the display is and which pointer to draw.
    if let Ok(mut windows) = crate::x11_windows::X11Windows::connect(number, client) {
        let scale = crate::x11_windows::display_scale(&data.state);
        let theme = data.state.cursor.theme_name().to_owned();
        // The layout the person chose before the server started is the one its clients are told of.
        let keyboard = data.state.keyboard.clone();
        if let Some(server) = data.state.xwms.iter_mut().find(|server| server.id() == xwm) {
            windows.publish_x_hints(scale, &theme, &mut server.wm);
            if let Some(keyboard) = &keyboard {
                windows.publish_keyboard(keyboard);
            }
            server.windows = Some(windows);
        }
    } else {
        tracing::warn!("X11 windows on :{number} have no identity");
    }
    data.state.start_xdnd(number, xwm);
    Some(xwm)
}

/// Take `xwm` 's display out of the session: its windows leave the scene, and what the session kept
/// of them goes with them.
/// Nothing else will say these windows went: their server is stopping, or is already gone.
pub(crate) fn detach(data: &mut LoopData, xwm: XwmId) {
    let state = &mut data.state;
    let windows: Vec<Window> = state
        .space
        .elements()
        .filter(|window| window.x11_surface().and_then(X11Surface::xwm_id) == Some(xwm))
        .cloned()
        .collect();
    for window in windows {
        state.keep_closing(&window);
        state.space.unmap_elem(&window);
        if let Some(surface) = window.x11_surface()
            && let Some(id) = crate::state::x11_window_id(state, surface)
        {
            state.icons.set(id, None);
        }
    }
    crate::state::forget_unmapped_of(state, xwm);
    state.rearrange();
    state.forget_x11_dnd(xwm);
    state.forget_x11_selection(xwm);
}

/// The server of `xwm` is gone: drop what ran it.
pub(crate) fn forget(data: &mut LoopData, xwm: XwmId) {
    data.state.xwms.retain(|server| server.id() != xwm);
}

/// The window manager of `xwm` 's display, while it runs.
impl State {
    pub(crate) fn x11_server(&self, xwm: XwmId) -> Option<&X11Server> {
        self.xwms.iter().find(|server| server.id() == xwm)
    }

    pub(crate) fn x11_server_mut(&mut self, xwm: XwmId) -> Option<&mut X11Server> {
        self.xwms.iter_mut().find(|server| server.id() == xwm)
    }

    /// The session's own connection to `xwm` 's display, while it runs.
    pub(crate) fn x11_windows_mut(
        &mut self,
        xwm: XwmId,
    ) -> Option<&mut crate::x11_windows::X11Windows> {
        self.x11_server_mut(xwm)?.windows.as_mut()
    }

    /// Tell the session's displays `xwm` showed a window.
    fn window_mapped(&self, xwm: XwmId) {
        if let Some(x11) = &self.x11 {
            x11.window_mapped(xwm);
        }
    }

    /// Tell the session's displays one of `xwm` 's windows went.
    fn window_gone(&self, xwm: XwmId) {
        if let Some(x11) = &self.x11 {
            x11.window_gone(self, xwm);
        }
    }

    /// The element showing `surface` , if it is mapped.
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

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell
    }

    /// Xwayland associates a window's `wl_surface` only after the window mapped, so the keyboard
    /// focus the policy gave it had no surface to go to yet: it is given now. A window another one
    /// took the focus from since is no longer activated and keeps it.
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

impl XwmHandler for State {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        #[allow(
            clippy::expect_used,
            reason = "the window manager is only dropped once its channel closed"
        )]
        self.x11_server_mut(xwm)
            .map(|server| &mut server.wm)
            .expect("X11 events only arrive while their server runs")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, xwm: XwmId, surface: X11Surface) {
        if let Err(error) = surface.set_mapped(true) {
            tracing::warn!(?error, "X11 window not mapped");
            return;
        }
        let window = Window::new_x11_window(surface);
        self.map_window(&window);
        self.window_mapped(xwm);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        let location = surface.geometry().loc;
        let window = Window::new_x11_window(surface);
        self.space.map_element(window, location, true);
    }

    fn unmapped_window(&mut self, xwm: XwmId, surface: X11Surface) {
        self.unmap(&surface);
        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(false);
            self.window_gone(xwm);
        }
    }

    fn destroyed_window(&mut self, xwm: XwmId, surface: X11Surface) {
        self.unmap(&surface);
        if surface.is_override_redirect() {
            return;
        }
        // An X11 app has no client of its own here: its window going says it exited.
        if let Some(pid) = surface.pid()
            && let Ok(mut exited) = self.exited.lock()
        {
            exited.push(pid);
        }
        self.window_gone(xwm);
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
        // A mapped managed window goes where the policy put it; it may only change its size. An
        // unmapped one may ask for anything.
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

    fn new_selection(&mut self, xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        self.x11_set_selection(xwm, selection.into(), mime_types);
    }

    fn cleared_selection(&mut self, xwm: XwmId, selection: SelectionTarget) {
        self.x11_cleared_selection(xwm, selection.into());
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        // The window manager runs in the event loop's data, which cleans up what a stopped display
        // leaves behind.
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

/// The loop's data answers the window manager's events with the state's handler: the window manager
/// runs in the loop, the protocol in the state.
impl LoopData {
    fn remember_icon(&mut self, xwm: XwmId, window: &X11Surface) {
        let id = window.window_id();
        let icon = self
            .state
            .x11_server(xwm)
            .and_then(|server| server.windows.as_ref())
            .and_then(|x11| x11.window_icon(id));
        if let Some(id) = crate::state::x11_window_id(&self.state, window) {
            self.state.icons.set(id, icon);
        }
    }
}

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
        // What the window says about its program is kept before it shows, so the desktop knows
        // whose window it is from the first frame.
        if let Some(x11) = self.state.x11_windows_mut(xwm) {
            x11.learn(&window);
        }
        self.remember_icon(xwm, &window);
        self.state.map_window_request(xwm, window);
    }

    fn mapped_override_redirect_window(&mut self, xwm: XwmId, window: X11Surface) {
        // The drag-and-drop proxy takes X11 drags; it is never shown.
        if self.state.dnd_proxy(xwm, &window) {
            return;
        }
        self.state.mapped_override_redirect_window(xwm, window);
    }

    fn unmapped_window(&mut self, xwm: XwmId, window: X11Surface) {
        self.state.unmapped_window(xwm, window);
    }

    fn destroyed_window(&mut self, xwm: XwmId, window: X11Surface) {
        if let Some(x11) = self.state.x11_windows_mut(xwm) {
            x11.forget(window.window_id());
        }
        if let Some(id) = crate::state::x11_window_id(&self.state, &window) {
            self.state.icons.set(id, None);
        }
        self.state.destroyed_window(xwm, window);
    }

    fn property_notify(&mut self, xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        let renamed = matches!(
            property,
            WmWindowProperty::Class | WmWindowProperty::StartupId | WmWindowProperty::Pid
        );
        if renamed {
            if let Some(x11) = self.state.x11_windows_mut(xwm) {
                x11.learn(&window);
            }
            self.remember_icon(xwm, &window);
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

    fn disconnected(&mut self, xwm: XwmId) {
        // The display's server exited: its windows leave the scene, and the display waits for the
        // next app to start a fresh one.
        match self.state.x11.clone() {
            Some(x11) => x11.server_gone(self, xwm),
            None => crate::xwayland::forget(self, xwm),
        }
    }
}
