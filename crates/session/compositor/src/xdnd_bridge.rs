//! The compositor's own X11 connection to Xwayland, speaking XDND for
//! drags that cross between X11 windows and everything else.
//!
//! smithay's X11 window manager knows selections but not XDND, so the
//! bridge holds a second connection with two windows of its own:
//!
//! - the *proxy*, an override-redirect window that is `XdndAware`. While an
//!   X11 client drags and the pointer is over something that is not an X11
//!   window (a Wayland window, a hosted surface, the desktop), it is raised
//!   over the whole root, so the X11 source finds it under the pointer and
//!   sends it the drag. The compositor never shows it.
//! - the *source window*, unmapped, which owns `XdndSelection` and sends
//!   the XDND messages while a Wayland or hosted drag is over an X11 window.
//!
//! The bridge only speaks the protocol and moves bytes; what a drag means
//! (which target is under the pointer, who serves the data) is the state's,
//! told through [`BridgeEvent`]s.

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsFd as _, OwnedFd};

use x11rb::connection::Connection as _;
use x11rb::errors::ReplyOrIdError;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property, SELECTION_NOTIFY_EVENT,
    SelectionNotifyEvent, SelectionRequestEvent, StackMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::selection::MAX_SELECTION_BYTES;
use crate::xdnd::{self, Action, Message, XdndAtoms};

x11rb::atom_manager! {
    /// The atoms the bridge uses, interned once.
    Atoms: AtomsCookie {
        XdndAware,
        XdndSelection,
        XdndEnter,
        XdndPosition,
        XdndStatus,
        XdndLeave,
        XdndDrop,
        XdndFinished,
        XdndTypeList,
        XdndActionCopy,
        XdndActionMove,
        XdndActionAsk,
        XdndActionPrivate,
        XdndProxy,
        TARGETS,
        INCR,
        TRANSFER: b"_TILO_XDND_DATA",
    }
}

/// Bytes per property write when a transfer is sent in pieces (INCR).
const CHUNK: usize = 64 * 1024;

/// What happened on the X11 side that the state has to act on.
pub(crate) enum BridgeEvent {
    /// An X11 drag reached the proxy, offering these types.
    Enter { types: Vec<String> },
    /// The X11 drag is at this root position over the proxy.
    Position { x: i32, y: i32 },
    /// The X11 drag left the proxy.
    Leave,
    /// The X11 drag was dropped on the proxy.
    Drop,
    /// The X11 window our drag is over answered a position.
    Status { accept: bool, action: Action },
    /// The X11 window our drag was dropped on is done.
    Finished,
    /// An X11 window asks for our drag's data as `target`.
    Request(Request),
}

/// An X11 window's request for the data of a drag we own.
pub(crate) struct Request {
    event: SelectionRequestEvent,
    /// The format asked for, by name.
    pub(crate) target: String,
}

/// The X11 drag the proxy is the target of.
struct Incoming {
    source: Window,
    version: u32,
}

/// The X11 window our drag is over.
struct Outgoing {
    /// The window the drag is over (the `window` field of messages).
    target: Window,
    /// Where messages go: the target, or the window it names as proxy.
    to: Window,
    accepted: bool,
    /// The drop was sent; waiting for `XdndFinished`.
    dropped: bool,
}

/// A conversion of the X11 drag's data into a pipe.
struct Conversion {
    target: String,
    fd: OwnedFd,
}

/// The conversion waiting for the X11 source's answer.
struct Converting {
    conversion: Conversion,
    /// The answer comes in pieces; the bytes so far.
    incremental: Option<Vec<u8>>,
}

/// Data sent to an X11 requestor in pieces.
struct Sending {
    requestor: Window,
    property: Atom,
    kind: Atom,
    data: Vec<u8>,
    offset: usize,
}

/// The connection, its windows and the drags in flight.
pub(crate) struct XdndBridge {
    conn: RustConnection,
    atoms: Atoms,
    xdnd: XdndAtoms,
    root: Window,
    proxy: Window,
    proxy_shown: bool,
    source_window: Window,
    names: HashMap<Atom, String>,
    ids: HashMap<String, Atom>,
    /// Whether an X11 client owns `XdndSelection`: it may be dragging.
    x11_owner: Option<Window>,
    incoming: Option<Incoming>,
    outgoing: Option<Outgoing>,
    /// The types our drag offers X11 windows.
    offered: Vec<String>,
    conversions: VecDeque<Conversion>,
    converting: Option<Converting>,
    sending: Vec<Sending>,
}

impl XdndBridge {
    /// Connect to Xwayland's display `:display` and set up the windows.
    pub(crate) fn connect(display: u32) -> Result<Self, String> {
        let (conn, screen) =
            x11rb::connect(Some(&format!(":{display}"))).map_err(|error| error.to_string())?;
        Self::new(conn, screen).map_err(|error| error.to_string())
    }

    fn new(conn: RustConnection, screen: usize) -> Result<Self, ReplyOrIdError> {
        let root = conn
            .setup()
            .roots
            .get(screen)
            .map(|screen| screen.root)
            .ok_or(ReplyOrIdError::IdsExhausted)?;
        let atoms = Atoms::new(&conn)?.reply()?;
        let xdnd = XdndAtoms {
            enter: atoms.XdndEnter,
            position: atoms.XdndPosition,
            status: atoms.XdndStatus,
            leave: atoms.XdndLeave,
            drop: atoms.XdndDrop,
            finished: atoms.XdndFinished,
            action_copy: atoms.XdndActionCopy,
            action_move: atoms.XdndActionMove,
            action_ask: atoms.XdndActionAsk,
            action_private: atoms.XdndActionPrivate,
        };
        let attributes = CreateWindowAux::new()
            .override_redirect(1)
            .event_mask(EventMask::PROPERTY_CHANGE);
        let proxy = conn.generate_id()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            proxy,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &attributes,
        )?;
        conn.change_property32(
            PropMode::REPLACE,
            proxy,
            atoms.XdndAware,
            AtomEnum::ATOM,
            &[xdnd::VERSION],
        )?;
        let source_window = conn.generate_id()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            source_window,
            root,
            -100,
            -100,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1)
                .event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        conn.xfixes_query_version(5, 0)?.reply()?;
        conn.xfixes_select_selection_input(
            source_window,
            atoms.XdndSelection,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?;
        conn.flush()?;
        Ok(Self {
            conn,
            atoms,
            xdnd,
            root,
            proxy,
            proxy_shown: false,
            source_window,
            names: HashMap::new(),
            ids: HashMap::new(),
            x11_owner: None,
            incoming: None,
            outgoing: None,
            offered: Vec::new(),
            conversions: VecDeque::new(),
            converting: None,
            sending: Vec::new(),
        })
    }

    /// The connection's descriptor, readable when events arrived.
    pub(crate) fn fd(&self) -> std::io::Result<OwnedFd> {
        self.conn.stream().as_fd().try_clone_to_owned()
    }

    /// The proxy window, which the window manager must leave unmapped in
    /// the scene.
    pub(crate) const fn proxy(&self) -> Window {
        self.proxy
    }

    /// Whether an X11 client may be dragging: it owns `XdndSelection`.
    pub(crate) const fn x11_may_drag(&self) -> bool {
        self.x11_owner.is_some()
    }

    /// Raise the proxy over the whole root (or take it down).
    pub(crate) fn show_proxy(&mut self, shown: bool) {
        if shown == self.proxy_shown {
            return;
        }
        self.proxy_shown = shown;
        let result = if shown {
            self.raise_proxy()
        } else {
            self.conn
                .unmap_window(self.proxy)
                .map(drop)
                .map_err(ReplyOrIdError::from)
        };
        self.flush_or_log(result, "drag proxy not changed");
    }

    fn raise_proxy(&self) -> Result<(), ReplyOrIdError> {
        let root = self.conn.get_geometry(self.root)?.reply()?;
        self.conn.configure_window(
            self.proxy,
            &ConfigureWindowAux::new()
                .x(0)
                .y(0)
                .width(u32::from(root.width.max(1)))
                .height(u32::from(root.height.max(1)))
                .stack_mode(StackMode::ABOVE),
        )?;
        self.conn.map_window(self.proxy)?;
        Ok(())
    }

    fn flush_or_log(&self, result: Result<(), ReplyOrIdError>, what: &str) {
        let flushed = result.and_then(|()| self.conn.flush().map_err(ReplyOrIdError::from));
        if let Err(error) = flushed {
            tracing::debug!(%error, "{what}");
        }
    }

    fn atom(&mut self, name: &str) -> Result<Atom, ReplyOrIdError> {
        if let Some(atom) = self.ids.get(name) {
            return Ok(*atom);
        }
        let atom = self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom;
        self.ids.insert(name.to_owned(), atom);
        self.names.insert(atom, name.to_owned());
        Ok(atom)
    }

    fn name(&mut self, atom: Atom) -> Option<String> {
        if let Some(name) = self.names.get(&atom) {
            return Some(name.clone());
        }
        let reply = self.conn.get_atom_name(atom).ok()?.reply().ok()?;
        let name = String::from_utf8(reply.name).ok()?;
        self.names.insert(atom, name.clone());
        self.ids.insert(name.clone(), atom);
        Some(name)
    }

    fn send(&self, to: Window, window: Window, message: &Message) -> Result<(), ReplyOrIdError> {
        let (kind, words) = message.encode(&self.xdnd);
        let event = ClientMessageEvent::new(32, window, kind, words);
        self.conn
            .send_event(false, to, EventMask::NO_EVENT, event)?;
        Ok(())
    }

    // The proxy as the target of an X11 drag.

    /// Answer the X11 drag's last position: whether a drop there would be
    /// taken, and how.
    pub(crate) fn status(&self, accept: bool, action: Action) {
        let Some(incoming) = &self.incoming else {
            return;
        };
        let action = if accept { action } else { Action::None };
        let message = Message::Status {
            target: self.proxy,
            accept,
            action: action.atom(&self.xdnd),
        };
        let result = self.send(incoming.source, self.proxy, &message);
        self.flush_or_log(result, "drag status not sent");
    }

    /// Tell the X11 source the drop is done, and take the proxy down.
    pub(crate) fn finish(&mut self, success: bool, action: Action) {
        let Some(incoming) = self.incoming.take() else {
            return;
        };
        // Versions before 5 carry no result: a finish is a success.
        let message = Message::Finished {
            target: self.proxy,
            success: success || incoming.version < 5,
            action: if success { action } else { Action::None }.atom(&self.xdnd),
        };
        let result = self.send(incoming.source, self.proxy, &message);
        self.flush_or_log(result, "drag finish not sent");
        self.show_proxy(false);
    }

    /// Read the X11 drag's data as `target` into `fd`, after any reads
    /// before it.
    pub(crate) fn convert(&mut self, target: String, fd: OwnedFd) {
        self.conversions.push_back(Conversion { target, fd });
        self.next_conversion();
    }

    fn next_conversion(&mut self) {
        while self.converting.is_none() {
            let Some(conversion) = self.conversions.pop_front() else {
                return;
            };
            let requested = self.atom(&conversion.target).and_then(|target| {
                self.conn
                    .convert_selection(
                        self.proxy,
                        self.atoms.XdndSelection,
                        target,
                        self.atoms.TRANSFER,
                        x11rb::CURRENT_TIME,
                    )
                    .map_err(ReplyOrIdError::from)
            });
            match requested {
                Ok(_) => {
                    self.flush_or_log(Ok(()), "drag data not asked for");
                    self.converting = Some(Converting {
                        conversion,
                        incremental: None,
                    });
                }
                // Dropping the pipe ends the reader's read.
                Err(error) => tracing::debug!(%error, "drag data not asked for"),
            }
        }
    }

    // The source window as the source of a Wayland or hosted drag.

    /// Our drag entered X11 window `target`, offering `types`; false when it
    /// takes no XDND drops.
    pub(crate) fn source_enter(&mut self, target: Window, types: &[String]) -> bool {
        self.source_leave();
        match self.enter(target, types) {
            Ok(entered) => entered,
            Err(error) => {
                tracing::debug!(%error, "drag not offered to the X11 window");
                false
            }
        }
    }

    fn enter(&mut self, target: Window, types: &[String]) -> Result<bool, ReplyOrIdError> {
        let Some((to, version)) = self.aware(target)? else {
            return Ok(false);
        };
        let atoms = types
            .iter()
            .map(|name| self.atom(name))
            .collect::<Result<Vec<_>, _>>()?;
        self.conn.set_selection_owner(
            self.source_window,
            self.atoms.XdndSelection,
            x11rb::CURRENT_TIME,
        )?;
        self.conn.change_property32(
            PropMode::REPLACE,
            self.source_window,
            self.atoms.XdndTypeList,
            AtomEnum::ATOM,
            &atoms,
        )?;
        self.offered = types.to_vec();
        let message = Message::Enter {
            source: self.source_window,
            version,
            more_types: atoms.len() > 3,
            types: atoms,
        };
        self.send(to, target, &message)?;
        self.conn.flush()?;
        self.outgoing = Some(Outgoing {
            target,
            to,
            accepted: false,
            dropped: false,
        });
        Ok(true)
    }

    /// Where `window` takes XDND messages and in which version, if at all.
    fn aware(&self, window: Window) -> Result<Option<(Window, u32)>, ReplyOrIdError> {
        let proxy = self
            .conn
            .get_property(false, window, self.atoms.XdndProxy, AtomEnum::WINDOW, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut values| values.next());
        let to = proxy.unwrap_or(window);
        let version = self
            .conn
            .get_property(false, to, self.atoms.XdndAware, AtomEnum::ATOM, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut values| values.next());
        Ok(version
            .and_then(xdnd::negotiate)
            .map(|version| (to, version)))
    }

    /// Our drag moved to this root position over the X11 window.
    pub(crate) fn source_position(&self, x: f64, y: f64, action: Action) {
        let Some(outgoing) = &self.outgoing else {
            return;
        };
        let message = Message::Position {
            source: self.source_window,
            x: xdnd::root_coordinate(x),
            y: xdnd::root_coordinate(y),
            time: x11rb::CURRENT_TIME,
            action: action.atom(&self.xdnd),
        };
        let result = self.send(outgoing.to, outgoing.target, &message);
        self.flush_or_log(result, "drag position not sent");
    }

    /// Our drag left the X11 window (before a drop).
    pub(crate) fn source_leave(&mut self) {
        let Some(outgoing) = self.outgoing.take_if(|outgoing| !outgoing.dropped) else {
            return;
        };
        let message = Message::Leave {
            source: self.source_window,
        };
        let result = self.send(outgoing.to, outgoing.target, &message);
        self.flush_or_log(result, "drag leave not sent");
    }

    /// Whether the X11 window our drag is over would take a drop.
    pub(crate) fn source_accepted(&self) -> bool {
        self.outgoing
            .as_ref()
            .is_some_and(|outgoing| outgoing.accepted)
    }

    /// Drop our drag on the X11 window; false (and a leave) when it would
    /// not take it.
    pub(crate) fn source_drop(&mut self) -> bool {
        if !self.source_accepted() {
            self.source_leave();
            return false;
        }
        let Some(outgoing) = self.outgoing.as_mut() else {
            return false;
        };
        outgoing.dropped = true;
        let (to, target) = (outgoing.to, outgoing.target);
        let message = Message::Drop {
            source: self.source_window,
            time: x11rb::CURRENT_TIME,
        };
        let result = self.send(to, target, &message);
        self.flush_or_log(result, "drop not sent");
        true
    }

    /// Answer an X11 window's request for our drag's data (`None`: we have
    /// none in that format).
    pub(crate) fn answer(&mut self, request: &Request, data: Option<&[u8]>) {
        let result = self.reply(&request.event, data);
        self.flush_or_log(result, "drag data not sent");
    }

    fn reply(
        &mut self,
        request: &SelectionRequestEvent,
        data: Option<&[u8]>,
    ) -> Result<(), ReplyOrIdError> {
        // Obsolete clients name no property: the target is the property.
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let stored = match data {
            Some(data) if data.len() > CHUNK => {
                let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
                self.conn.change_window_attributes(
                    request.requestor,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                )?;
                self.conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    self.atoms.INCR,
                    &[size],
                )?;
                self.sending.push(Sending {
                    requestor: request.requestor,
                    property,
                    kind: request.target,
                    data: data.to_vec(),
                    offset: 0,
                });
                true
            }
            Some(data) => {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    request.target,
                    data,
                )?;
                true
            }
            None => false,
        };
        self.notify(request, if stored { property } else { x11rb::NONE })
    }

    fn notify(
        &self,
        request: &SelectionRequestEvent,
        property: Atom,
    ) -> Result<(), ReplyOrIdError> {
        let event = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
        };
        self.conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, event)?;
        Ok(())
    }

    // Events.

    /// Handle what arrived on the connection; what the state must act on
    /// is returned.
    pub(crate) fn events(&mut self) -> Vec<BridgeEvent> {
        let mut out = Vec::new();
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(event)) => self.event(&event, &mut out),
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "X11 drag-and-drop connection failed");
                    break;
                }
            }
        }
        if let Err(error) = self.conn.flush() {
            tracing::debug!(%error, "X11 drag-and-drop connection not flushed");
        }
        out
    }

    fn event(&mut self, event: &Event, out: &mut Vec<BridgeEvent>) {
        match event {
            Event::XfixesSelectionNotify(event) if event.selection == self.atoms.XdndSelection => {
                self.x11_owner = (event.owner != x11rb::NONE && event.owner != self.source_window)
                    .then_some(event.owner);
            }
            Event::ClientMessage(event) if event.format == 32 => {
                let words = event.data.as_data32();
                if let Some(message) = Message::decode(event.type_, words, &self.xdnd) {
                    self.message(event.window, message, out);
                }
            }
            Event::SelectionRequest(event) if event.selection == self.atoms.XdndSelection => {
                self.request(*event, out);
            }
            Event::SelectionNotify(event) if event.requestor == self.proxy => {
                self.converted(event.property);
            }
            Event::PropertyNotify(event) => self.property(event.window, event.atom, event.state),
            Event::SelectionClear(event) if event.selection == self.atoms.XdndSelection => {
                self.offered.clear();
            }
            _ => {}
        }
    }

    fn message(&mut self, window: Window, message: Message, out: &mut Vec<BridgeEvent>) {
        match message {
            Message::Enter {
                source,
                version,
                more_types,
                types,
            } if window == self.proxy => {
                let Some(version) = xdnd::negotiate(version) else {
                    return;
                };
                let atoms = if more_types {
                    self.type_list(source).unwrap_or(types)
                } else {
                    types
                };
                let types = atoms
                    .into_iter()
                    .filter_map(|atom| self.name(atom))
                    .collect();
                self.incoming = Some(Incoming { source, version });
                out.push(BridgeEvent::Enter { types });
            }
            Message::Position { source, x, y, .. }
                if self
                    .incoming
                    .as_ref()
                    .is_some_and(|incoming| incoming.source == source) =>
            {
                out.push(BridgeEvent::Position {
                    x: i32::from(x),
                    y: i32::from(y),
                });
            }
            Message::Leave { source }
                if self
                    .incoming
                    .as_ref()
                    .is_some_and(|incoming| incoming.source == source) =>
            {
                self.incoming = None;
                out.push(BridgeEvent::Leave);
            }
            Message::Drop { source, .. }
                if self
                    .incoming
                    .as_ref()
                    .is_some_and(|incoming| incoming.source == source) =>
            {
                out.push(BridgeEvent::Drop);
            }
            Message::Status {
                target,
                accept,
                action,
            } if window == self.source_window => {
                if let Some(outgoing) = self
                    .outgoing
                    .as_mut()
                    .filter(|outgoing| outgoing.target == target)
                {
                    outgoing.accepted = accept;
                    let action = Action::of_atom(action, &self.xdnd);
                    out.push(BridgeEvent::Status { accept, action });
                }
            }
            Message::Finished { target, .. }
                if window == self.source_window
                    && self
                        .outgoing
                        .as_ref()
                        .is_some_and(|outgoing| outgoing.target == target && outgoing.dropped) =>
            {
                self.outgoing = None;
                out.push(BridgeEvent::Finished);
            }
            _ => {}
        }
    }

    fn type_list(&self, source: Window) -> Option<Vec<Atom>> {
        let reply = self
            .conn
            .get_property(
                false,
                source,
                self.atoms.XdndTypeList,
                AtomEnum::ATOM,
                0,
                1024,
            )
            .ok()?
            .reply()
            .ok()?;
        Some(reply.value32()?.collect())
    }

    fn request(&mut self, event: SelectionRequestEvent, out: &mut Vec<BridgeEvent>) {
        if event.owner != self.source_window {
            return;
        }
        if event.target == self.atoms.TARGETS {
            let offered = self.offered.clone();
            let result = offered
                .iter()
                .map(|name| self.atom(name))
                .collect::<Result<Vec<_>, _>>()
                .and_then(|mut atoms| {
                    atoms.push(self.atoms.TARGETS);
                    let property = if event.property == x11rb::NONE {
                        event.target
                    } else {
                        event.property
                    };
                    self.conn.change_property32(
                        PropMode::REPLACE,
                        event.requestor,
                        property,
                        AtomEnum::ATOM,
                        &atoms,
                    )?;
                    self.notify(&event, property)
                });
            self.flush_or_log(result, "drag targets not sent");
            return;
        }
        if let Some(target) = self.name(event.target) {
            out.push(BridgeEvent::Request(Request { event, target }));
        } else {
            let result = self.notify(&event, x11rb::NONE);
            self.flush_or_log(result, "drag data refusal not sent");
        }
    }

    /// The X11 source answered our conversion.
    fn converted(&mut self, property: Atom) {
        if self.converting.is_none() {
            return;
        }
        if property == x11rb::NONE {
            self.converting = None;
            self.next_conversion();
            return;
        }
        let reply = self
            .conn
            .get_property(true, self.proxy, property, AtomEnum::ANY, 0, u32::MAX / 4)
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        match reply {
            Some(reply) if reply.type_ == self.atoms.INCR => {
                // The pieces follow as property changes.
                if let Some(converting) = self.converting.as_mut() {
                    converting.incremental = Some(Vec::new());
                }
                self.flush_or_log(Ok(()), "drag data property not deleted");
            }
            Some(reply) => {
                if let Some(done) = self.converting.take() {
                    crate::selection::serve(done.conversion.fd, reply.value);
                }
                self.next_conversion();
            }
            None => {
                self.converting = None;
                self.next_conversion();
            }
        }
    }

    fn property(&mut self, window: Window, atom: Atom, state: Property) {
        if window == self.proxy && atom == self.atoms.TRANSFER && state == Property::NEW_VALUE {
            self.incoming_piece();
            return;
        }
        if state == Property::DELETE {
            self.outgoing_piece(window, atom);
        }
    }

    /// One piece of an incremental answer to our conversion.
    fn incoming_piece(&mut self) {
        let receiving = self
            .converting
            .as_ref()
            .is_some_and(|converting| converting.incremental.is_some());
        if !receiving {
            return;
        }
        let piece = self
            .conn
            .get_property(
                true,
                self.proxy,
                self.atoms.TRANSFER,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.value);
        self.flush_or_log(Ok(()), "drag data piece not deleted");
        let Some(received) = self
            .converting
            .as_mut()
            .and_then(|converting| converting.incremental.as_mut())
        else {
            return;
        };
        let finished = match piece {
            Some(piece) if !piece.is_empty() => {
                received.extend_from_slice(&piece);
                // Too much: what arrived so far is all the reader gets.
                received.len() > MAX_SELECTION_BYTES
            }
            _ => true,
        };
        if finished {
            if let Some(done) = self.converting.take() {
                let mut bytes = done.incremental.unwrap_or_default();
                bytes.truncate(MAX_SELECTION_BYTES);
                crate::selection::serve(done.conversion.fd, bytes);
            }
            self.next_conversion();
        }
    }

    /// A requestor took a piece of an incremental transfer; send the next.
    fn outgoing_piece(&mut self, requestor: Window, property: Atom) {
        let Some(index) = self
            .sending
            .iter()
            .position(|sending| sending.requestor == requestor && sending.property == property)
        else {
            return;
        };
        let Some(sending) = self.sending.get_mut(index) else {
            return;
        };
        let end = sending.offset.saturating_add(CHUNK).min(sending.data.len());
        let piece = sending.data.get(sending.offset..end).unwrap_or_default();
        let result = self
            .conn
            .change_property8(
                PropMode::REPLACE,
                sending.requestor,
                sending.property,
                sending.kind,
                piece,
            )
            .map(drop)
            .map_err(ReplyOrIdError::from);
        let last = piece.is_empty();
        sending.offset = end;
        if last {
            self.sending.remove(index);
        }
        self.flush_or_log(result, "drag data piece not sent");
    }
}
