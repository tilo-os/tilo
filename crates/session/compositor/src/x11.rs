//! X11 displays, one per workspace.
//! X11 isolates no clients of a display from each other: any client of `:3` reads every other
//! client's input, windows and clipboard. One X server therefore never spans two workspaces. A
//! workspace gets its own display number the first time the session starts an app in it, and only
//! that workspace's apps are told the number ([`X11Displays::display_for`]).
//! A display with no client runs no server. The compositor binds the display's sockets and leaves
//! them listening: a client that connects waits in the queue until a server accepts it, and only
//! then does Xwayland start ([`XWayland::spawn_on`]). A workspace that never runs an X11 app never
//! runs an X server, and an app's display costs nothing until it connects.
//! A server stops a couple of seconds after its last window goes, or after its last app client does
//! when its clients never showed a window; the display stays, and the next client starts a fresh
//! server on the same number. Displays outlive the workspaces they served, so the display of a
//! workspace that has not run an app for the longest gives up its number when the session runs out
//! of them.

use std::cell::RefCell;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::xwayland::xwm::XwmId;
use smithay::xwayland::{X11Surface, XWayland, XWaylandEvent, XWaylandListener};

use crate::LoopData;
use crate::state::State;

/// How long a server with no window keeps running after its last client goes: also how often a
/// display without a window is looked at. Wine and similar runtimes leave the X socket behind once
/// their window is gone; two seconds is long enough for a legitimate re-map and short enough that
/// the process does not sit around.
const WINDOWLESS: Duration = Duration::from_secs(2);
/// How long a server may take to become ready, and how long a display must go without an app before
/// it gives up its number. Long enough that an app restarting itself (an installer, a launcher that
/// execs) finds its server still up.
const NO_CLIENTS: Duration = Duration::from_secs(60);
/// How long a stopped server may take to report its exit before its display is served again anyway:
/// a server that does not report back must not take its workspace's X11 with it.
const STOPPING: Duration = Duration::from_secs(5);
/// The clients every display has that are not apps: Xwayland's own connection for the window
/// manager's socket, the window manager's, the one [`crate::x11_windows::X11Windows`] reads window
/// properties on, and [`crate::xdnd_bridge::XdndBridge`]'s. Any besides them are apps.
const OWN_CLIENTS: usize = 4;

/// The displays of this session, by workspace.
/// The app launcher holds one ([`crate::Compositor::x11_displays`]) and is the only thing that asks
/// for a display; the compositor starts and stops the servers on the displays that exist.
#[derive(Clone)]
pub struct X11Displays {
    inner: Rc<RefCell<Displays>>,
}

/// What the handles share: the loop that runs the servers, the Wayland display they connect to, and
/// the workspaces' displays.
struct Displays {
    handle: LoopHandle<'static, LoopData>,
    display: DisplayHandle,
    /// Whether this session serves X11 at all (`--no-x11` turns it off).
    enabled: bool,
    workspaces: Vec<Workspace>,
}

/// One workspace's X11 display: its sockets, what watches them for a client,
/// and the server while one runs.
struct Workspace {
    /// The workspace's id, as its launches name it.
    id: String,
    listener: XWaylandListener,
    /// When this workspace last asked for its display: what decides which display gives up its
    /// number when none is free.
    used: Instant,
    /// What watches the display's sockets for a client waiting to connect;
    /// empty while a server starts, runs or stops.
    watching: Vec<RegistrationToken>,
    /// The Xwayland source between the first client and the server being ready, and when it was
    /// started.
    starting: Option<(RegistrationToken, Instant)>,
    running: Option<Running>,
    /// A stopped server that has not reported its exit yet, and when it was stopped: the display's
    /// sockets wait for it before serving an app.
    stopping: Option<(XwmId, Instant)>,
}

/// The Xwayland server serving one display.
struct Running {
    /// Its loop source. Removing it drops the server, and with it the server's Wayland client and
    /// process.
    source: RegistrationToken,
    xwm: XwmId,
    /// When the server is looked at again, while no window of it shows.
    check: Option<Instant>,
    /// One of its clients showed a window, so no window now says its apps are gone rather than that
    /// they never showed one.
    showed_window: bool,
}

/// What a workspace's display does at a deadline.
enum Due {
    /// The server has run long enough without windows or clients.
    Stop,
    /// The server never became ready.
    GiveUp,
    /// A stopped server never reported back.
    Forget,
}

impl X11Displays {
    pub(crate) fn new(handle: LoopHandle<'static, LoopData>, display: DisplayHandle) -> Self {
        if let Err(error) = crate::xwayland::socket_directory() {
            tracing::warn!(%error, "no X11 socket directory: no X11 apps");
        }
        Self {
            inner: Rc::new(RefCell::new(Displays {
                handle,
                display,
                enabled: true,
                workspaces: Vec::new(),
            })),
        }
    }

    /// The display of `workspace` , bound on first use: `:N` is what the apps started there are
    /// told. `None` when this session serves no X11, or when no display number is free.
    #[must_use]
    pub fn display_for(&self, workspace: &str) -> Option<u32> {
        self.inner.borrow_mut().display_for(workspace)
    }

    /// Serve no X11 display from now on (`--no-x11`).
    pub fn disable(&self) {
        self.inner.borrow_mut().enabled = false;
    }

    /// The displays whose Xwayland server runs now.
    #[must_use]
    pub fn running(&self) -> Vec<u32> {
        self.inner
            .borrow()
            .workspaces
            .iter()
            .filter(|workspace| workspace.running.is_some())
            .map(|workspace| workspace.listener.display_number())
            .collect()
    }

    /// When the loop looks at the servers next, for [`crate::Compositor::next_wait`].
    pub(crate) fn next_check(&self) -> Option<Instant> {
        self.inner
            .borrow()
            .workspaces
            .iter()
            .flat_map(|workspace| {
                let starting = workspace
                    .starting
                    .as_ref()
                    .and_then(|(_, started)| started.checked_add(NO_CLIENTS));
                let stopping = workspace
                    .stopping
                    .as_ref()
                    .and_then(|(_, stopped)| stopped.checked_add(STOPPING));
                starting
                    .into_iter()
                    .chain(stopping)
                    .chain(workspace.running.as_ref().and_then(|running| running.check))
            })
            .min()
    }

    /// Do what the displays owe at `now` : stop the servers that have been idle for their time, and
    /// give up on servers that never started.
    pub(crate) fn tick(&self, data: &mut LoopData, now: Instant) {
        let mut displays = self.inner.borrow_mut();
        let handle = displays.handle.clone();
        let mut due = Vec::new();
        for (index, workspace) in displays.workspaces.iter_mut().enumerate() {
            if let Some((_, started)) = workspace.starting
                && due_at(started, NO_CLIENTS, now)
            {
                due.push((index, Due::GiveUp));
                continue;
            }
            if let Some((_, stopped)) = workspace.stopping
                && due_at(stopped, STOPPING, now)
            {
                due.push((index, Due::Forget));
                continue;
            }
            if workspace.stopping.is_some() {
                continue;
            }
            let Some(running) = workspace.running.as_mut() else {
                continue;
            };
            if running.check.is_none_or(|check| check > now) {
                continue;
            }
            if windows_of(&data.state, running.xwm) > 0 {
                running.check = None;
                continue;
            }
            // A client that showed no window may be a helper of an app that still runs: only its
            // last client going stops the server. Apps that showed windows are gone when the
            // windows are.
            let clients = clients_of(&data.state, running.xwm);
            if running.showed_window || clients.is_none_or(|clients| clients == 0) {
                due.push((index, Due::Stop));
            } else {
                running.check = Instant::now().checked_add(WINDOWLESS);
            }
        }
        // Back to front, so each removal leaves the others' indices alone.
        for (index, due) in due.into_iter().rev() {
            let mut workspace = displays.workspaces.remove(index);
            match due {
                Due::Stop => workspace.stop_server(data, &handle),
                Due::GiveUp => workspace.give_up(data, &handle),
                Due::Forget => workspace.forget_server(data, &handle),
            }
            displays.workspaces.insert(index, workspace);
        }
    }

    /// An X11 window of `xwm` showed: its display's server has work to do.
    pub(crate) fn window_mapped(&self, xwm: XwmId) {
        if let Some(running) = self.inner.borrow_mut().running_mut(xwm) {
            running.showed_window = true;
            running.check = None;
        }
    }

    /// A window of `xwm` went away: if it was the last one, the server is looked at again shortly.
    pub(crate) fn window_gone(&self, state: &State, xwm: XwmId) {
        if windows_of(state, xwm) > 0 {
            return;
        }
        if let Some(running) = self.inner.borrow_mut().running_mut(xwm) {
            running.check = Instant::now().checked_add(WINDOWLESS);
        }
    }

    /// The server of `xwm` is gone, whatever ended it: what it showed leaves the scene, and the
    /// display waits for the next client.
    pub(crate) fn server_gone(&self, data: &mut LoopData, xwm: XwmId) {
        let mut displays = self.inner.borrow_mut();
        let handle = displays.handle.clone();
        let Some(index) = displays.workspaces.iter().position(|workspace| {
            workspace
                .running
                .as_ref()
                .is_some_and(|running| running.xwm == xwm)
                || workspace
                    .stopping
                    .as_ref()
                    .is_some_and(|(stopping, _)| *stopping == xwm)
        }) else {
            crate::xwayland::forget(data, xwm);
            return;
        };
        let mut workspace = displays.workspaces.remove(index);
        if workspace
            .running
            .as_ref()
            .is_some_and(|running| running.xwm == xwm)
        {
            workspace.stop_server(data, &handle);
        }
        workspace.forget_server(data, &handle);
        displays.workspaces.insert(index, workspace);
    }

    /// A client waits on `workspace` 's display: start the server it waits for.
    fn client_waiting(&self, workspace: &str) {
        let mut displays = self.inner.borrow_mut();
        let handle = displays.handle.clone();
        let display = displays.display.clone();
        let Some(index) = displays
            .workspaces
            .iter()
            .position(|entry| entry.id == workspace)
        else {
            return;
        };
        let Some(entry) = displays.workspaces.get_mut(index) else {
            return;
        };
        if entry.starting.is_some() || entry.running.is_some() || entry.stopping.is_some() {
            return;
        }
        for token in entry.watching.drain(..) {
            handle.remove(token);
        }
        let spawned = XWayland::spawn_on(
            &entry.listener,
            &display,
            std::iter::empty::<(String, String)>(),
            std::process::Stdio::null(),
            std::process::Stdio::inherit(),
            |_| {},
        );
        let (xwayland, client) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                tracing::warn!(
                    display = entry.listener.display_number(),
                    %error,
                    "Xwayland did not start: no X11 apps in this workspace"
                );
                let mut entry = displays.workspaces.remove(index);
                entry.shut(&handle);
                return;
            }
        };
        let id = workspace.to_owned();
        let started = handle.insert_source(xwayland, move |event, (), data| {
            let Some(x11) = data.state.x11.clone() else {
                return;
            };
            match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => x11.ready(data, &id, client.clone(), x11_socket, display_number),
                XWaylandEvent::Error => x11.server_failed(&id),
            }
        });
        match started {
            Ok(token) => entry.starting = Some((token, Instant::now())),
            Err(error) => {
                tracing::warn!(%error.error, "Xwayland is not watched: no X11 apps here");
                let mut entry = displays.workspaces.remove(index);
                entry.shut(&handle);
            }
        }
    }

    /// Xwayland `display` 's server is ready: it gets a window manager, which is what makes its
    /// windows toplevels the desktop places.
    fn ready(
        &self,
        data: &mut LoopData,
        workspace: &str,
        client: Client,
        socket: UnixStream,
        display: u32,
    ) {
        let mut displays = self.inner.borrow_mut();
        let handle = displays.handle.clone();
        let Some(index) = displays
            .workspaces
            .iter()
            .position(|entry| entry.id == workspace)
        else {
            return;
        };
        let Some(entry) = displays.workspaces.get_mut(index) else {
            return;
        };
        let Some((source, _)) = entry.starting.take() else {
            return;
        };
        let Some(xwm) = crate::xwayland::start_wm(data, socket, client, display) else {
            // Nothing manages windows on this display: no app can show one.
            let mut entry = displays.workspaces.remove(index);
            entry.give_up(data, &handle);
            return;
        };
        let Some(entry) = displays.workspaces.get_mut(index) else {
            return;
        };
        entry.running = Some(Running {
            source,
            xwm,
            check: Instant::now().checked_add(WINDOWLESS),
            showed_window: false,
        });
    }

    /// Xwayland `display` exited before it was ready.
    fn server_failed(&self, workspace: &str) {
        let mut displays = self.inner.borrow_mut();
        let handle = displays.handle.clone();
        let Some(index) = displays
            .workspaces
            .iter()
            .position(|entry| entry.id == workspace)
        else {
            return;
        };
        let mut entry = displays.workspaces.remove(index);
        // Its app never got a server: the display goes, and the workspace's next X11 launch binds a
        // fresh one.
        entry.shut(&handle);
    }
}

impl Displays {
    fn display_for(&mut self, workspace: &str) -> Option<u32> {
        let now = Instant::now();
        if let Some(entry) = self
            .workspaces
            .iter_mut()
            .find(|entry| entry.id == workspace)
        {
            entry.used = now;
            return Some(entry.listener.display_number());
        }
        if !self.enabled {
            return None;
        }
        let listener = self.prepare(now)?;
        let number = listener.display_number();
        let mut entry = Workspace {
            id: workspace.to_owned(),
            listener,
            used: now,
            watching: Vec::new(),
            starting: None,
            running: None,
            stopping: None,
        };
        let handle = self.handle.clone();
        entry.watch(&handle);
        self.workspaces.push(entry);
        Some(number)
    }

    /// Bind a display's sockets on a free number, making one free by giving up the display of the
    /// workspace that has been idle longest when none is. Only a display with no server on it gives
    /// up its number: a workspace whose apps run keeps its X11.
    fn prepare(&mut self, now: Instant) -> Option<XWaylandListener> {
        loop {
            match XWaylandListener::prepare(None, true) {
                Ok(listener) => return Some(listener),
                Err(error) => {
                    let handle = self.handle.clone();
                    let idle = self
                        .workspaces
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.idle(now))
                        .min_by_key(|(_, entry)| entry.used)
                        .map(|(index, _)| index);
                    let Some(index) = idle else {
                        tracing::warn!(
                            %error,
                            "no free X11 display: no X11 apps in this workspace"
                        );
                        return None;
                    };
                    let mut entry = self.workspaces.remove(index);
                    entry.shut(&handle);
                }
            }
        }
    }

    fn running_mut(&mut self, xwm: XwmId) -> Option<&mut Running> {
        self.workspaces.iter_mut().find_map(|workspace| {
            workspace
                .running
                .as_mut()
                .filter(|running| running.xwm == xwm)
        })
    }
}

impl Workspace {
    /// Whether this display does nothing now, and has been asked for its number long enough ago
    /// that no launch into its workspace can still be waiting to connect to it.
    fn idle(&self, now: Instant) -> bool {
        self.starting.is_none()
            && self.running.is_none()
            && self.stopping.is_none()
            && now.saturating_duration_since(self.used) > NO_CLIENTS
    }

    /// Watch the display's sockets for a client waiting to connect.
    fn watch(&mut self, handle: &LoopHandle<'static, LoopData>) {
        let number = self.listener.display_number();
        for socket in self.listener.sockets() {
            let Ok(watching) = socket.try_clone() else {
                tracing::warn!("X11 :{number} is not watched: no X11 apps here");
                return;
            };
            let id = self.id.clone();
            let inserted = handle.insert_source(
                Generic::new(watching, Interest::READ, Mode::Level),
                move |_, _, data: &mut LoopData| {
                    if let Some(x11) = data.state.x11.clone() {
                        x11.client_waiting(&id);
                    }
                    Ok(PostAction::Continue)
                },
            );
            match inserted {
                Ok(token) => self.watching.push(token),
                Err(error) => {
                    tracing::warn!("X11 :{number} is not watched: {}", error.error);
                }
            }
        }
    }

    /// Stop the running server: its windows leave the scene, its process exits, and the display
    /// waits for the server to report gone before it serves the next client.
    fn stop_server(&mut self, data: &mut LoopData, handle: &LoopHandle<'static, LoopData>) {
        let Some(running) = self.running.take() else {
            return;
        };
        crate::xwayland::detach(data, running.xwm);
        handle.remove(running.source);
        self.stopping = Some((running.xwm, Instant::now()));
    }

    /// The server reported its exit: the display serves the next client.
    fn forget_server(&mut self, data: &mut LoopData, handle: &LoopHandle<'static, LoopData>) {
        let Some((xwm, _)) = self.stopping.take() else {
            return;
        };
        crate::xwayland::forget(data, xwm);
        self.watch(handle);
    }

    /// The server never became usable: its app gets no display, and the workspace's next X11 launch
    /// binds a fresh one.
    fn give_up(&mut self, data: &mut LoopData, handle: &LoopHandle<'static, LoopData>) {
        if let Some((token, _)) = self.starting.take() {
            // Dropping the source drops the server with it.
            handle.remove(token);
        }
        if let Some(running) = self.running.take() {
            crate::xwayland::detach(data, running.xwm);
            handle.remove(running.source);
            crate::xwayland::forget(data, running.xwm);
        }
        self.shut(handle);
    }

    /// The display stops existing: its server, sockets and lock go.
    fn shut(&mut self, handle: &LoopHandle<'static, LoopData>) {
        for token in self.watching.drain(..) {
            handle.remove(token);
        }
        if let Some((token, _)) = self.starting.take() {
            handle.remove(token);
        }
        if let Some(running) = self.running.take() {
            handle.remove(running.source);
        }
        self.stopping = None;
    }
}

/// Whether `since` plus `wait` has come by `now` .
fn due_at(since: Instant, wait: Duration, now: Instant) -> bool {
    since.checked_add(wait).is_some_and(|at| at <= now)
}

/// How many windows `xwm` 's server shows now.
fn windows_of(state: &State, xwm: XwmId) -> usize {
    state
        .space
        .elements()
        .filter(|window| window.x11_surface().and_then(X11Surface::xwm_id) == Some(xwm))
        .count()
}

/// How many apps are connected to `xwm` 's display besides the session's own connections; `None`
/// when the display cannot say.
fn clients_of(state: &State, xwm: XwmId) -> Option<usize> {
    let windows = state.x11_server(xwm)?.windows.as_ref()?;
    windows
        .clients()
        .map(|clients| clients.saturating_sub(OWN_CLIENTS))
}
