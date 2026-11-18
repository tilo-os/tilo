//! Drag-and-drop between the launch's X11 programs and everything on the desktop.
//!
//! The launch's X11 server (`xwayland-satellite`) never speaks XDND, so a drag out of an X11
//! window would end at its edge, and a desktop drag over one would reach nobody. The keeper
//! carries both with the XDND bridge the compositor uses for its own X11 displays
//! ([`tilo_x11_transfer::bridge`]), on an X11 connection of its own with the display's cookie.
//! The bridge waits for the X11 server's answers, so it lives on a thread: an X11 server that
//! stops answering costs drag-and-drop with X11 programs, never the keeper's loop.
//!
//! - **Out of an X11 window.** While a button is held on a window of the X11 server, an X11
//!   client owns `XdndSelection` (it may be dragging) and the desktop's pointer is outside that
//!   window, the bridge raises its proxy over the X11 root, so the X11 drag finds it. When the
//!   drag enters the proxy, the keeper starts a desktop drag offering the X11 drag's types, as the
//!   window's own press allows; the desktop drag takes the pointer from there. When it is dropped
//!   or cancelled, the X11 server is shown its pointer entering the window again, which releases
//!   the held button inside the X11 server (Xwayland releases held buttons on entry), so the X11
//!   source drops on the proxy, or leaves it when the drop was refused, and is not left in its
//!   grab.
//! - **Into an X11 window.** A desktop drag over a window of the X11 server is not handed to the
//!   server's data device (it would go nowhere): the server is shown its pointer following the
//!   drag, so the X11 pointer is over the window the drag is over, and the bridge offers the drag
//!   in XDND to the window under the X11 pointer. The target's answers become the desktop
//!   offer's, its requests for data are read from the desktop offer.
//!
//! A desktop that goes away cancels both.
//!
//! The same thread follows the X11 server's windows on a second connection
//! ([`tilo_x11_transfer::windows`]): their icons and pings, see [`crate::x11_windows`].

use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc;

use smithay::reexports::calloop::channel;
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use tilo_x11_transfer::bridge::{BridgeEvent, Request, XdndBridge};
use tilo_x11_transfer::windows::{WindowEvent, WindowWatch};
use tilo_x11_transfer::xdnd::{self, Action};
use tilo_x11_transfer::{MAX_SELECTION_BYTES, pick_format};
use wayland_client::protocol as c;
use wayland_client::{Proxy as _, WEnum};

use crate::state::State;

/// What the keeper asks of the bridge.
pub(crate) enum Command {
    /// Raise the proxy over the root, or take it down.
    ShowProxy(bool),
    /// Answer the X11 drag over the proxy: whether a drop would be taken, and how.
    Status { accept: bool, action: Action },
    /// The X11 drag's drop is done.
    Finish { success: bool, action: Action },
    /// Read the X11 drag's data as a type into a pipe.
    Convert { target: String, fd: OwnedFd },
    /// The desktop drag is wherever the X11 pointer is, offering these types.
    Over { types: Vec<String>, action: Action },
    /// The desktop drag left the X11 window.
    Leave,
    /// The desktop drag was dropped on the X11 window.
    Drop,
    /// The data an X11 window asked for, or none.
    Answer {
        request: Request,
        data: Option<Vec<u8>>,
    },
    /// Ask the X11 programs whether they are alive.
    Ping(u32),
}

/// What the bridge tells the keeper.
pub(crate) enum Heard {
    Bridge(BridgeEvent),
    /// Whether an X11 client owns `XdndSelection` : it may be dragging.
    Dragging(bool),
    /// The X11 window under the desktop drag would not take its drop.
    DropRefused,
    /// The bridge's connection ended.
    Gone,
    /// What the X11 server's windows did.
    Window(WindowEvent),
}

/// The keeper's end of the bridge's thread.
#[derive(Clone)]
pub(crate) struct Bridge {
    commands: mpsc::Sender<Command>,
    wake: Arc<OwnedFd>,
}

impl Bridge {
    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = rustix::io::write(&*self.wake, &[1]);
        }
    }
}

/// Start the bridge to the display `:display` on a thread of its own; what it hears is sent to
/// `heard`.
pub(crate) fn start(
    display: u32,
    cookie: Vec<u8>,
    heard: channel::Sender<Heard>,
) -> std::io::Result<Bridge> {
    let (wake_read, wake_write) = rustix::pipe::pipe_with(
        rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
    )?;
    let (commands, received) = mpsc::channel();
    std::thread::Builder::new()
        .name("x11-windows".to_owned())
        .spawn(move || serve(display, &cookie, &received, &wake_read, &heard))?;
    Ok(Bridge {
        commands,
        wake: Arc::new(wake_write),
    })
}

/// The bridge's thread: connect, then carry commands and events until either side ends.
fn serve(
    display: u32,
    cookie: &[u8],
    commands: &mpsc::Receiver<Command>,
    wake: &OwnedFd,
    heard: &channel::Sender<Heard>,
) {
    let mut bridge = match XdndBridge::connect(display, Some(cookie)) {
        Ok(bridge) => bridge,
        Err(error) => {
            tracing::warn!(%error, "no drag-and-drop with the launch's X11 programs");
            let _ = heard.send(Heard::Gone);
            return;
        }
    };
    let Ok(connection) = bridge.fd() else {
        let _ = heard.send(Heard::Gone);
        return;
    };
    let mut windows = WindowWatch::connect(display, Some(cookie))
        .inspect_err(|error| tracing::warn!(%error, "no icons or pings for the X11 programs"))
        .ok();
    let windows_connection = windows.as_ref().and_then(|windows| windows.fd().ok());
    let mut dragging = false;
    loop {
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if let Some(answer) = carry(&mut bridge, windows.as_mut(), command) {
                        let _ = heard.send(answer);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        // Waiting for replies may have queued events the descriptors no longer show.
        let events = bridge.events().into_iter().map(Heard::Bridge);
        let window_events = windows
            .as_mut()
            .map(WindowWatch::events)
            .unwrap_or_default();
        for event in events.chain(window_events.into_iter().map(Heard::Window)) {
            if heard.send(event).is_err() {
                return;
            }
        }
        if bridge.x11_may_drag() != dragging {
            dragging = bridge.x11_may_drag();
            let _ = heard.send(Heard::Dragging(dragging));
        }
        let mut fds = vec![
            rustix::event::PollFd::new(&connection, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(wake, rustix::event::PollFlags::IN),
        ];
        if let Some(windows_connection) = windows_connection.as_ref().filter(|_| windows.is_some())
        {
            fds.push(rustix::event::PollFd::new(
                windows_connection,
                rustix::event::PollFlags::IN,
            ));
        }
        if rustix::event::poll(&mut fds, None).is_err() {
            continue;
        }
        let ended = |fd: &rustix::event::PollFd<'_>| {
            fd.revents()
                .intersects(rustix::event::PollFlags::HUP | rustix::event::PollFlags::ERR)
        };
        if fds.first().is_some_and(ended) {
            let _ = heard.send(Heard::Gone);
            return;
        }
        if fds.get(2).is_some_and(ended) {
            windows = None;
            let _ = heard.send(Heard::Window(WindowEvent::Answered { stamp: 0 }));
        }
        let mut drained = [0_u8; 64];
        while matches!(rustix::io::read(wake, &mut drained), Ok(1..)) {}
    }
}

fn carry(
    bridge: &mut XdndBridge,
    windows: Option<&mut WindowWatch>,
    command: Command,
) -> Option<Heard> {
    match command {
        Command::Ping(stamp) => {
            // Nobody to ask is an answer at once.
            if !windows.is_some_and(|windows| windows.ping(stamp)) {
                return Some(Heard::Window(WindowEvent::Answered { stamp }));
            }
        }
        Command::ShowProxy(shown) => bridge.show_proxy(shown),
        Command::Status { accept, action } => bridge.status(accept, action),
        Command::Finish { success, action } => bridge.finish(success, action),
        Command::Convert { target, fd } => bridge.convert(target, fd),
        Command::Over { types, action } => {
            if !bridge.source_over_pointer(&types, action) {
                return Some(Heard::Bridge(BridgeEvent::Status {
                    accept: false,
                    action: Action::None,
                }));
            }
        }
        Command::Leave => bridge.source_leave(),
        Command::Drop => {
            if !bridge.source_drop() {
                return Some(Heard::DropRefused);
            }
        }
        Command::Answer { request, data } => bridge.answer(&request, data.as_deref()),
    }
    None
}

/// A button held on a window of the X11 server: where an X11 drag may start.
struct Held {
    surface: WlSurface,
    serial: u32,
    button: u32,
    at: (f64, f64),
}

/// An X11 drag carried on by the desktop.
struct Out {
    twin: c::wl_data_source::WlDataSource,
    types: Vec<String>,
    /// The X11 server's window it started from, and where the pointer last was on it.
    surface: WlSurface,
    at: (f64, f64),
    /// The button the X11 drag holds.
    button: u32,
    mime: Option<String>,
    action: Action,
    /// The X11 source dropped on the proxy.
    x11_dropped: bool,
    /// The desktop's verdict: finished (`true`) or cancelled (`false`).
    ended: Option<bool>,
}

impl Out {
    /// Whether the desktop's target would take a drop, and how. A target that chose an action
    /// takes it: not every desktop tells the source which format a target accepted.
    fn verdict(&self) -> (bool, Action) {
        (
            self.mime.is_some() || self.action != Action::None,
            self.action,
        )
    }
}

/// A desktop drag over a window of the X11 server.
struct In {
    offer: c::wl_data_offer::WlDataOffer,
    serial: u32,
    surface: WlSurface,
    mimes: Vec<String>,
    source_actions: u32,
    dropped: bool,
}

/// Drag-and-drop with the launch's X11 programs.
#[derive(Default)]
pub(crate) struct Xdnd {
    bridge: Option<Bridge>,
    /// The X11 server's connection to the keeper.
    server: Option<ClientId>,
    dragging: bool,
    /// Where the desktop's pointer last was on a window of the X11 server.
    at: (f64, f64),
    held: Option<Held>,
    proxy: bool,
    out: Option<Out>,
    into: Option<In>,
}

impl Xdnd {
    /// The bridge to a new X11 server is up.
    pub(crate) fn started(&mut self, bridge: Bridge) {
        self.bridge = Some(bridge);
        self.dragging = false;
        self.proxy = false;
    }

    /// The connection `client` is the X11 server's.
    pub(crate) fn server_connected(&mut self, client: ClientId) {
        self.server = Some(client);
    }

    fn send(&self, command: Command) {
        if let Some(bridge) = &self.bridge {
            bridge.send(command);
        }
    }

    fn show_proxy(&mut self, shown: bool) {
        if self.proxy != shown {
            self.proxy = shown;
            self.send(Command::ShowProxy(shown));
        }
    }

    /// Ask the X11 programs whether they are alive; `true` while their answer is awaited.
    pub(crate) fn ping_x11(&self, stamp: u32) -> bool {
        if self.bridge.is_none() {
            return false;
        }
        self.send(Command::Ping(stamp));
        true
    }

    /// The X11 server's connection to the keeper.
    pub(crate) const fn server(&self) -> Option<&ClientId> {
        self.server.as_ref()
    }

    pub(crate) fn is_server(&self, surface: &WlSurface) -> bool {
        self.server.is_some() && surface.client().map(|client| client.id()) == self.server
    }
}

impl State {
    /// A pointer event the desktop sent for `surface`: a press on a window of the X11 server may
    /// start an X11 drag, which leaves the X11 windows where the pointer leaves that window.
    pub(crate) fn x11_drag_pointer(&mut self, surface: &WlSurface, event: &c::wl_pointer::Event) {
        if !self.xdnd.is_server(surface) || self.xdnd.out.is_some() {
            return;
        }
        match event {
            c::wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(c::wl_pointer::ButtonState::Pressed),
                ..
            } => {
                let at = self.xdnd.at;
                self.xdnd.held = Some(Held {
                    surface: surface.clone(),
                    serial: *serial,
                    button: *button,
                    at,
                });
            }
            c::wl_pointer::Event::Button { .. } => {
                self.xdnd.held = None;
                self.xdnd.show_proxy(false);
            }
            c::wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                self.xdnd.at = (*surface_x, *surface_y);
                self.xdnd.held = None;
                self.xdnd.show_proxy(false);
            }
            c::wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                let at = (*surface_x, *surface_y);
                self.xdnd.at = at;
                let Some(held) = self.xdnd.held.as_mut() else {
                    return;
                };
                held.at = at;
                self.x11_drag_reaches_out();
            }
            _ => {}
        }
    }

    /// Raise the proxy while an X11 client may be dragging with the button held on its window
    /// and the pointer outside that window; take it down when the pointer is back.
    fn x11_drag_reaches_out(&mut self) {
        if self.xdnd.out.is_some() {
            return;
        }
        let Some(held) = &self.xdnd.held else {
            return;
        };
        let at = held.at;
        let outside = self.xdnd.dragging
            && self
                .logical_size(&held.surface)
                .is_some_and(|(w, h)| at.0 < 0.0 || at.1 < 0.0 || at.0 >= w || at.1 >= h);
        self.xdnd.show_proxy(outside);
    }

    /// The size `surface` shows at on the desktop.
    fn logical_size(&self, surface: &WlSurface) -> Option<(f64, f64)> {
        if let (_, Some(destination)) = State::viewport_of(surface) {
            return Some((f64::from(destination.w), f64::from(destination.h)));
        }
        let kept = self.surfaces.get(&surface.id())?;
        let (width, height) = self.buffers.size_of(kept.buffer?)?;
        let scale = f64::from(kept.scale.max(1));
        Some((f64::from(width) / scale, f64::from(height) / scale))
    }

    /// What the bridge heard.
    pub(crate) fn xdnd_heard(&mut self, heard: Heard) {
        match heard {
            Heard::Dragging(dragging) => {
                self.xdnd.dragging = dragging;
                self.x11_drag_reaches_out();
            }
            Heard::Gone => {
                self.x11_windows_gone();
                self.end_out(false);
                self.end_in();
                self.xdnd.bridge = None;
                self.xdnd.dragging = false;
                self.xdnd.proxy = false;
            }
            Heard::DropRefused => self.end_in(),
            Heard::Bridge(event) => self.bridge_event(event),
            Heard::Window(event) => self.x11_window_event(event),
        }
    }

    fn bridge_event(&mut self, event: BridgeEvent) {
        match event {
            BridgeEvent::Enter { types } => self.x11_drag_out(types),
            BridgeEvent::Position { .. } => {}
            BridgeEvent::Leave => {
                // The X11 source left the proxy before its drag reached the desktop, or after
                // a refused drop.
                if let Some(out) = self.xdnd.out.take() {
                    out.twin.destroy();
                    self.xdnd.held = None;
                    self.xdnd.show_proxy(false);
                }
            }
            BridgeEvent::Drop => {
                if let Some(out) = self.xdnd.out.as_mut() {
                    out.x11_dropped = true;
                }
                self.settle_out();
            }
            BridgeEvent::Status { accept, action } => self.x11_target_status(accept, action),
            BridgeEvent::Finished => {
                if let Some(into) = self.xdnd.into.take() {
                    if into.offer.version() >= 3 {
                        into.offer.finish();
                    }
                    into.offer.destroy();
                    self.seat.synthetic_pointer_leave(&into.surface);
                }
            }
            BridgeEvent::Request(request) => self.x11_target_request(request),
        }
    }

    // Out of an X11 window.

    /// The X11 drag reached the proxy: the desktop carries it from here, from the window the
    /// button was pressed on.
    fn x11_drag_out(&mut self, types: Vec<String>) {
        let Some(held) = self.xdnd.held.take() else {
            // No press of ours started it: refuse, and let it go.
            self.xdnd.send(Command::Status {
                accept: false,
                action: Action::None,
            });
            return;
        };
        let origin = self
            .surfaces
            .get(&held.surface.id())
            .and_then(|kept| kept.up.as_ref())
            .map(|up| up.surface.clone());
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let (Some(manager), Some(device), Some(origin)) =
            (&up.data_device_manager, &up.data_device, origin)
        else {
            return;
        };
        let twin = manager.create_data_source(&up.qh, ());
        for mime in xdnd::mimes_of_targets(&types) {
            twin.offer(mime);
        }
        twin.set_actions(c::wl_data_device_manager::DndAction::from_bits_truncate(
            xdnd::dnd_action::COPY | xdnd::dnd_action::MOVE | xdnd::dnd_action::ASK,
        ));
        device.start_drag(Some(&twin), &origin, None, held.serial);
        self.xdnd.out = Some(Out {
            twin,
            types,
            surface: held.surface,
            at: held.at,
            button: held.button,
            mime: None,
            action: Action::None,
            x11_dropped: false,
            ended: None,
        });
    }

    /// Whether `source` is the desktop twin of an X11 drag; its event is handled if so.
    pub(crate) fn x11_drag_source_event(
        &mut self,
        source: &c::wl_data_source::WlDataSource,
        event: c::wl_data_source::Event,
    ) -> Option<c::wl_data_source::Event> {
        use c::wl_data_source::Event;
        let Some(out) = self.xdnd.out.as_mut().filter(|out| out.twin == *source) else {
            return Some(event);
        };
        match event {
            Event::Target { mime_type } => {
                out.mime = mime_type;
                let (accept, action) = out.verdict();
                self.xdnd.send(Command::Status { accept, action });
            }
            Event::Action {
                dnd_action: WEnum::Value(action),
            } => {
                out.action = Action::preferred(action.bits());
                let (accept, action) = out.verdict();
                self.xdnd.send(Command::Status { accept, action });
            }
            Event::Send { mime_type, fd } => {
                let target = pick_format(&mime_type, &out.types).unwrap_or(mime_type);
                self.xdnd.send(Command::Convert { target, fd });
            }
            Event::DndDropPerformed => self.release_x11_button(),
            Event::DndFinished => {
                out.ended = Some(true);
                self.settle_out();
            }
            Event::Cancelled => self.end_out(false),
            _ => {}
        }
        None
    }

    /// Show the X11 server its pointer back on the drag's window where the drag left it, and
    /// the button the X11 drag holds coming up: the X11 source drops or leaves.
    fn release_x11_button(&self) {
        if let Some(out) = &self.xdnd.out {
            self.seat.synthetic_pointer_enter(&out.surface, out.at);
            self.seat
                .synthetic_pointer_release(&out.surface, out.button);
            self.seat.synthetic_pointer_leave(&out.surface);
        }
    }

    /// The desktop ended the X11 drag: `false` cancels it on the X11 side.
    fn end_out(&mut self, finished: bool) {
        let Some(out) = self.xdnd.out.as_mut() else {
            return;
        };
        out.ended = Some(finished);
        if !finished {
            self.xdnd.send(Command::Status {
                accept: false,
                action: Action::None,
            });
            self.release_x11_button();
        }
        self.settle_out();
    }

    /// Finish the X11 drag once both sides are done with it: the X11 source dropped, and the
    /// desktop's target finished (or the drag was cancelled).
    fn settle_out(&mut self) {
        let done = self
            .xdnd
            .out
            .as_ref()
            .is_some_and(|out| out.x11_dropped && out.ended.is_some());
        if !done {
            return;
        }
        let Some(out) = self.xdnd.out.take() else {
            return;
        };
        let success = out.ended == Some(true);
        self.xdnd.send(Command::Finish {
            success,
            action: out.action,
        });
        // The bridge took the proxy down with the finish.
        self.xdnd.proxy = false;
        out.twin.destroy();
    }

    // Into an X11 window.

    /// Whether the desktop's drag entered a window of the X11 server; it is carried in XDND if
    /// so.
    pub(crate) fn x11_drag_enter(
        &mut self,
        serial: u32,
        surface: &WlSurface,
        at: (f64, f64),
        offer: Option<c::wl_data_offer::WlDataOffer>,
        (mimes, source_actions): (Vec<String>, u32),
    ) -> bool {
        if !self.xdnd.is_server(surface) || self.xdnd.bridge.is_none() {
            return false;
        }
        self.end_in();
        let Some(offer) = offer else {
            return true;
        };
        // The X11 drag the desktop carries is over its own server's window: the X11 server
        // handles that one itself, under its own grab.
        if self.xdnd.out.is_some() {
            return true;
        }
        self.seat.synthetic_pointer_enter(surface, at);
        self.xdnd.into = Some(In {
            offer,
            serial,
            surface: surface.clone(),
            mimes,
            source_actions,
            dropped: false,
        });
        self.x11_drag_over();
        true
    }

    fn x11_drag_over(&self) {
        let Some(into) = &self.xdnd.into else {
            return;
        };
        self.xdnd.send(Command::Over {
            types: xdnd::targets_of_mimes(&into.mimes),
            action: Action::preferred(into.source_actions),
        });
    }

    /// Whether the desktop's drag over an X11 window moved; carried if so.
    pub(crate) fn x11_drag_motion(&self, time: u32, at: (f64, f64)) -> bool {
        let Some(into) = self.xdnd.into.as_ref().filter(|into| !into.dropped) else {
            return false;
        };
        self.seat.synthetic_pointer_motion(&into.surface, time, at);
        self.x11_drag_over();
        true
    }

    /// Whether the desktop's drag was dropped on an X11 window; carried if so.
    pub(crate) fn x11_drag_drop(&mut self) -> bool {
        let Some(into) = self.xdnd.into.as_mut() else {
            return false;
        };
        into.dropped = true;
        self.xdnd.send(Command::Drop);
        true
    }

    /// Whether the desktop's drag left an X11 window; carried if so.
    pub(crate) fn x11_drag_leave(&mut self) -> bool {
        match &self.xdnd.into {
            // The X11 target reads what was dropped, then finishes.
            Some(into) if into.dropped => true,
            Some(_) => {
                self.end_in();
                true
            }
            None => false,
        }
    }

    /// The desktop offer `id` names its source's actions, or the action chosen.
    pub(crate) fn x11_drag_offer_actions(
        &mut self,
        id: &c::wl_data_offer::WlDataOffer,
        actions: u32,
        chosen: bool,
    ) -> bool {
        let Some(into) = self.xdnd.into.as_mut().filter(|into| into.offer == *id) else {
            return false;
        };
        if !chosen {
            into.source_actions = actions;
            self.x11_drag_over();
        }
        true
    }

    /// The X11 window under the desktop's drag answered: the desktop offer says the same.
    fn x11_target_status(&self, accept: bool, action: Action) {
        let Some(into) = self.xdnd.into.as_ref().filter(|into| !into.dropped) else {
            return;
        };
        let mime = xdnd::read_ahead(&into.mimes)
            .into_iter()
            .next()
            .or_else(|| into.mimes.first().cloned());
        if into.offer.version() >= 3 {
            let actions = c::wl_data_device_manager::DndAction::from_bits_truncate(
                into.source_actions
                    & (xdnd::dnd_action::COPY | xdnd::dnd_action::MOVE | xdnd::dnd_action::ASK),
            );
            let preferred = c::wl_data_device_manager::DndAction::from_bits_truncate(action.bits());
            into.offer.set_actions(
                if accept {
                    actions
                } else {
                    c::wl_data_device_manager::DndAction::empty()
                },
                preferred,
            );
        }
        into.offer.accept(into.serial, mime.filter(|_| accept));
    }

    /// An X11 window asks for the dropped data: read from the desktop offer, off the loop.
    fn x11_target_request(&self, request: Request) {
        let Some(bridge) = self.xdnd.bridge.clone() else {
            return;
        };
        let into = self.xdnd.into.as_ref();
        let mime = into.and_then(|into| pick_format(&request.target, &into.mimes));
        let (Some(into), Some(mime)) = (into, mime) else {
            bridge.send(Command::Answer {
                request,
                data: None,
            });
            return;
        };
        let Ok((reader, writer)) = std::io::pipe() else {
            bridge.send(Command::Answer {
                request,
                data: None,
            });
            return;
        };
        into.offer.receive(mime, writer.as_fd());
        drop(writer);
        let read = std::thread::Builder::new()
            .name("x11-drop-read".to_owned())
            .spawn(move || {
                let mut data = Vec::new();
                let limit = u64::try_from(MAX_SELECTION_BYTES).unwrap_or(u64::MAX);
                let read = reader.take(limit).read_to_end(&mut data);
                bridge.send(Command::Answer {
                    request,
                    data: read.ok().map(|_| data),
                });
            });
        if let Err(error) = read {
            tracing::warn!(%error, "a drop on an X11 window could not be read");
        }
    }

    /// The desktop drag over an X11 window is over: the X11 side hears it left.
    fn end_in(&mut self) {
        let Some(into) = self.xdnd.into.take() else {
            return;
        };
        if !into.dropped {
            self.xdnd.send(Command::Leave);
        }
        if into.offer.is_alive() {
            into.offer.destroy();
        }
        self.seat.synthetic_pointer_leave(&into.surface);
    }

    /// The desktop went away: drags with X11 programs are cancelled on the X11 side.
    pub(crate) fn hold_xdnd(&mut self) {
        self.end_out(false);
        if let Some(into) = self.xdnd.into.as_mut() {
            into.dropped = false;
        }
        self.end_in();
        self.xdnd.held = None;
    }

    /// The surface `id` is gone.
    pub(crate) fn xdnd_surface_gone(&mut self, id: &ObjectId) {
        if self
            .xdnd
            .held
            .as_ref()
            .is_some_and(|held| held.surface.id() == *id)
        {
            self.xdnd.held = None;
        }
    }
}
