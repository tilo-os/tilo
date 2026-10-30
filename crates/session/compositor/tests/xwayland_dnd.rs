//! Files dragged between an X11 client and the session's surface apps on a
//! headless compositor: an X11 window as the drag's target, and an X11
//! client as the drag's source, over the compositor's XDND bridge.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;
use tilo_compositor::{Compositor, HostLayer, InputEvent, OutputConfig, StackPolicy, SurfaceHost};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const BTN_LEFT: u32 = 0x110;
const URI_LIST: &str = "text/uri-list";
const FILES: &[u8] = b"file:///home/someone/a%20b.txt\r\nfile:///home/someone/c.png\r\n";

/// A compositor with one output and Xwayland, an X11 client on it and a
/// surface app that takes file lists.
struct Session {
    compositor: Compositor,
    x11: RustConnection,
    root: Window,
    /// The files this X11 client offers when it is the drag's source.
    files: Vec<u8>,
    /// What the X11 client saw: its messages and the data it was given.
    events: Vec<Event>,
    /// The files an X11 drag dropped on it carried.
    dropped: Option<Vec<u8>>,
    log: Rc<RefCell<DropLog>>,
}

impl Session {
    fn start() -> Self {
        let log = Rc::new(RefCell::new(DropLog::default()));
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("headless");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
            .expect("output");
        compositor.add_host(Box::new(DropZone::new(Rc::clone(&log))));
        let display = compositor.start_xwayland().expect("Xwayland");
        let (x11, root) = x11_client(&mut compositor, display);
        Self {
            compositor,
            x11,
            root,
            files: FILES.to_vec(),
            events: Vec::new(),
            dropped: None,
            log,
        }
    }

    /// A mapped 200x150 window the stack policy centres, `XdndAware` at the
    /// newest version.
    fn x11_window(&mut self) -> Window {
        let window = self.x11.generate_id().expect("id");
        self.x11
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                200,
                150,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(
                    EventMask::PROPERTY_CHANGE
                        | EventMask::ENTER_WINDOW
                        | EventMask::LEAVE_WINDOW
                        | EventMask::FOCUS_CHANGE,
                ),
            )
            .expect("window");
        self.x11
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"thunar\0Thunar\0",
            )
            .expect("class");
        self.x11
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"Thunar",
            )
            .expect("title");
        self.x11
            .change_property32(
                PropMode::REPLACE,
                window,
                self.atom("XdndAware"),
                AtomEnum::ATOM,
                &[5],
            )
            .expect("aware");
        let screen = self.x11.setup().roots.first().expect("a screen").clone();
        let gc = self.x11.generate_id().expect("gc");
        self.x11
            .create_gc(
                gc,
                window,
                &x11rb::protocol::xproto::CreateGCAux::new()
                    .foreground(screen.black_pixel)
                    .graphics_exposures(0),
            )
            .expect("gc");
        self.x11.map_window(window).expect("map");
        self.x11.flush().expect("flush");
        // A mapped window that painted is one Xwayland has a buffer for, and
        // so one the session finds under the pointer.
        self.x11
            .poly_fill_rectangle(
                window,
                gc,
                &[x11rb::protocol::xproto::Rectangle {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 150,
                }],
            )
            .expect("paint");
        self.x11.free_gc(gc).expect("gc freed");
        self.x11.flush().expect("flush");
        // Xwayland gives the window its surface, and the session its focus,
        // a moment after it maps; only then can anything land on it.
        self.pump(|session| {
            session
                .events
                .iter()
                .any(|event| matches!(event, Event::FocusIn(_)))
        });
        self.pump(|session| {
            session
                .compositor
                .windows()
                .iter()
                .any(|(_, rect)| *rect == (220, 165, 200, 150))
        });
        window
    }

    /// The window the pointer is over that takes XDND messages, and the
    /// version it speaks.
    fn target_under_pointer(&self) -> Option<(Window, u32)> {
        let child = self
            .x11
            .query_pointer(self.root)
            .expect("pointer")
            .reply()
            .expect("pointer reply")
            .child;
        if child == x11rb::NONE {
            return None;
        }
        let version = self
            .x11
            .get_property(false, child, self.atom("XdndAware"), AtomEnum::ANY, 0, 1)
            .expect("aware")
            .reply()
            .expect("aware reply")
            .value32()
            .and_then(|mut words| words.next())?;
        Some((child, version))
    }

    /// The atom named `name`.
    fn atom(&self, name: &str) -> Atom {
        self.x11
            .intern_atom(false, name.as_bytes())
            .expect("atom")
            .reply()
            .expect("atom reply")
            .atom
    }

    /// Send `words` of `kind` to `window`.
    fn message(&self, window: Window, kind: Atom, words: [u32; 5]) {
        let event = ClientMessageEvent::new(32, window, kind, words);
        self.x11
            .send_event(false, window, EventMask::NO_EVENT, event)
            .expect("message");
        self.x11.flush().expect("flush");
    }

    /// Start dragging `self.files` into `target`.
    fn enter(&self, target: Window, version: u32, source: Window) {
        self.message(
            target,
            self.atom("XdndEnter"),
            [source, version << 24, self.atom(URI_LIST), 0, 0],
        );
    }

    /// The drag is at root position `(x, y)` over `target`.
    fn position(&self, target: Window, source: Window, x: f64, y: f64) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a point on a screen, far inside i16's range"
        )]
        let (x, y) = (x as i16, y as i16);
        self.message(
            target,
            self.atom("XdndPosition"),
            [
                source,
                0,
                u32::from(x.cast_unsigned()) << 16 | u32::from(y.cast_unsigned()),
                x11rb::CURRENT_TIME,
                self.atom("XdndActionCopy"),
            ],
        );
    }

    /// The drag dropped on `target`.
    fn drop(&self, target: Window, source: Window) {
        self.message(
            target,
            self.atom("XdndDrop"),
            [source, 0, x11rb::CURRENT_TIME, 0, 0],
        );
    }

    /// Answer a window's position: a drop here is taken, as a copy.
    fn status(&self, target: Window, source: Window) {
        self.message(
            target,
            self.atom("XdndStatus"),
            [source, 1, 0, 0, self.atom("XdndActionCopy")],
        );
    }

    /// The drop is done with `source`.
    fn finished(&self, target: Window, source: Window) {
        self.message(
            source,
            self.atom("XdndFinished"),
            [target, 1, self.atom("XdndActionCopy"), 0, 0],
        );
    }

    /// Hand the drag's data to a window that asked for it.
    fn serve(&self, request: &SelectionRequestEvent) {
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let served = request.target == self.atom(URI_LIST);
        if served {
            self.x11
                .change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    self.atom(URI_LIST),
                    &self.files,
                )
                .expect("data");
        }
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { x11rb::NONE },
        };
        self.x11
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)
            .expect("notify");
        self.x11.flush().expect("flush");
    }

    /// Read the drag's data a target was given, as `text/uri-list`.
    fn read_drop(&mut self, window: Window, source: Window) {
        self.x11
            .convert_selection(
                window,
                self.atom("XdndSelection"),
                self.atom(URI_LIST),
                self.atom("XDND_DATA"),
                x11rb::CURRENT_TIME,
            )
            .expect("convert");
        self.x11.flush().expect("flush");
        self.pump(|session| {
            session.events.iter().any(|event| {
                matches!(event, Event::SelectionNotify(notify) if notify.property != x11rb::NONE)
            })
        });
        let value = self
            .x11
            .get_property(
                false,
                window,
                self.atom("XDND_DATA"),
                AtomEnum::ANY,
                0,
                u32::MAX,
            )
            .expect("data")
            .reply()
            .expect("data reply")
            .value;
        self.dropped = Some(value);
        // The source hears the drop is taken, and with what.
        self.finished(source, window);
    }

    /// Run the session on for a moment, as it would between two of a
    /// person's actions.
    fn settle(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(100) {
            self.x11.flush().expect("flush");
            self.compositor
                .dispatch(Duration::from_millis(2))
                .expect("dispatch");
            while let Some(event) = self.x11.poll_for_event().expect("event") {
                self.answer(&event);
                self.events.push(event);
            }
        }
    }

    /// Let the compositor and the X11 client run until `done`.
    fn pump(&mut self, mut done: impl FnMut(&Session) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            self.x11.flush().expect("flush");
            self.compositor
                .dispatch(Duration::from_millis(2))
                .expect("dispatch");
            while let Some(event) = self.x11.poll_for_event().expect("event") {
                self.answer(&event);
                self.events.push(event);
            }
            self.compositor.render().expect("render");
            if done(self) {
                return;
            }
        }
        panic!(
            "the drag never got there: {:?} {:?} {:?} {:?}",
            self.compositor.windows(),
            self.log.borrow().offered,
            self.log.borrow().dropped,
            self.events
        );
    }

    /// Answer what the compositor's bridge asks the X11 client.
    fn answer(&mut self, event: &Event) {
        match event {
            Event::SelectionRequest(request) => self.serve(request),
            Event::ClientMessage(message) if message.format == 32 => {
                let words = message.data.as_data32();
                let target = message.window;
                if message.type_ == self.atom("XdndPosition") {
                    // The drag's source hears where it is over this window.
                    self.status(words[0], target);
                } else if message.type_ == self.atom("XdndDrop") {
                    let source = words[0];
                    // Our own connection does the reading: collect the
                    // answer on the next turn of the loop.
                    self.read_drop(target, source);
                }
            }
            _ => {}
        }
    }
}

/// An X11 client on `display`: Xwayland answers the connection's setup only
/// after it has roundtripped with the compositor, so the client waits in a
/// thread while the compositor dispatches here.
fn x11_client(compositor: &mut Compositor, display: u32) -> (RustConnection, Window) {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(x11rb::connect(Some(&format!(":{display}"))));
    });
    let started = Instant::now();
    loop {
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        if let Ok(connected) = receiver.try_recv() {
            let (x11, screen) = connected.expect("X11 connection");
            let root = x11.setup().roots[screen].root;
            return (x11, root);
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "Xwayland never answered the X11 client"
        );
    }
}

fn input(compositor: &mut Compositor, event: InputEvent) {
    compositor.input(event).expect("input");
}

#[derive(Default)]
struct DropLog {
    offered: Vec<Vec<String>>,
    left: u32,
    dropped: Vec<(String, Vec<u8>)>,
    drag_ended: Option<bool>,
}

/// A surface app at (500, 20, 100, 100) that takes file lists.
struct DropZone {
    buffer: MemoryRenderBuffer,
    log: Rc<RefCell<DropLog>>,
}

impl DropZone {
    const RECT: (f64, f64, f64, f64) = (500.0, 20.0, 100.0, 100.0);

    fn new(log: Rc<RefCell<DropLog>>) -> Self {
        let buffer = MemoryRenderBuffer::from_slice(
            &[0, 255, 0, 255].repeat(100 * 100),
            Fourcc::Argb8888,
            (100, 100),
            1,
            Transform::Normal,
            None,
        );
        Self { buffer, log }
    }
}

impl SurfaceHost for DropZone {
    fn layers(&self) -> Vec<HostLayer> {
        vec![HostLayer {
            location: (500, 20),
            size: (100, 100),
            buffer: self.buffer.clone(),
        }]
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (rx, ry, w, h) = Self::RECT;
        x >= rx && y >= ry && x < rx + w && y < ry + h
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn drop_offered(&mut self, mimes: &[String], _x: f64, _y: f64) -> Option<String> {
        self.log.borrow_mut().offered.push(mimes.to_vec());
        mimes.iter().find(|mime| *mime == URI_LIST).cloned()
    }

    fn drop_left(&mut self) {
        self.log.borrow_mut().left += 1;
    }

    fn drop_data(&mut self, mime: &str, data: Vec<u8>) {
        self.log.borrow_mut().dropped.push((mime.to_owned(), data));
    }

    fn drag_ended(&mut self, dropped: bool) {
        self.log.borrow_mut().drag_ended = Some(dropped);
    }
}

/// The middle of a 200x150 window centred on the 640x480 output.
const WINDOW_POINTER: (f64, f64) = (320.0, 240.0);
/// The middle of the surface app.
const HOST_POINTER: (f64, f64) = (550.0, 70.0);

#[test]
fn files_dragged_out_of_a_surface_app_drop_on_an_x11_window() {
    let mut session = Session::start();
    let _window = session.x11_window();

    input(
        &mut session.compositor,
        InputEvent::PointerMotion {
            x: HOST_POINTER.0,
            y: HOST_POINTER.1,
        },
    );
    input(
        &mut session.compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    assert!(
        session
            .compositor
            .start_drag(vec![(URI_LIST.to_owned(), FILES.to_vec())])
            .expect("drag")
    );

    input(
        &mut session.compositor,
        InputEvent::PointerMotion {
            x: WINDOW_POINTER.0,
            y: WINDOW_POINTER.1,
        },
    );
    // The X11 window answers the position, so the drop is offered to it.
    session.pump(|session| {
        session.events.iter().any(|event| {
            matches!(event, Event::ClientMessage(message) if message.type_ == session.atom("XdndPosition"))
        })
    });
    // The window's answer reaches the session on its own turn of the loop,
    // as it would while a person keeps the button down.
    session.settle();
    input(
        &mut session.compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    session.pump(|session| session.dropped.is_some());
    assert_eq!(session.dropped.as_deref(), Some(FILES));
    session.pump(|session| {
        session.events.iter().any(|event| {
            matches!(event, Event::ClientMessage(message) if message.type_ == session.atom("XdndFinished"))
        })
    });
    assert_eq!(session.log.borrow().drag_ended, Some(true));
    assert!(
        session
            .compositor
            .windows()
            .iter()
            .any(|(_, rect)| *rect == (220, 165, 200, 150))
    );
}

#[test]
fn files_dragged_out_of_an_x11_window_drop_on_a_surface_app() {
    let mut session = Session::start();
    let window = session.x11_window();
    // The X11 client owns the drag's data before its window is pressed.
    session
        .x11
        .set_selection_owner(window, session.atom("XdndSelection"), x11rb::CURRENT_TIME)
        .expect("owner");
    session.x11.flush().expect("flush");
    // The session sees the new drag source on its own round of the loop
    // before the pointer leaves the window.
    session.settle();

    input(
        &mut session.compositor,
        InputEvent::PointerMotion {
            x: WINDOW_POINTER.0,
            y: WINDOW_POINTER.1,
        },
    );
    input(
        &mut session.compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    // The compositor raises its proxy over everything else while the X11
    // client may be dragging.
    input(
        &mut session.compositor,
        InputEvent::PointerMotion {
            x: HOST_POINTER.0,
            y: HOST_POINTER.1,
        },
    );
    session.pump(|session| session.target_under_pointer().is_some());
    let (proxy, version) = session.target_under_pointer().expect("the drag proxy");
    assert_ne!(
        proxy, window,
        "the pointer is over the session, not the window"
    );

    session.enter(proxy, version, window);
    session.position(proxy, window, HOST_POINTER.0, HOST_POINTER.1);
    session.pump(|session| !session.log.borrow().offered.is_empty());
    let offered = session
        .log
        .borrow()
        .offered
        .last()
        .cloned()
        .expect("offered");
    assert!(offered.contains(&URI_LIST.to_owned()), "{offered:?}");

    input(
        &mut session.compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    session.drop(proxy, window);
    session.pump(|session| !session.log.borrow().dropped.is_empty());
    let dropped = session.log.borrow().dropped.clone();
    assert_eq!(dropped, vec![(URI_LIST.to_owned(), FILES.to_vec())]);
    session.pump(|session| {
        session.events.iter().any(|event| {
            matches!(event, Event::ClientMessage(message) if message.type_ == session.atom("XdndFinished"))
        })
    });
}
