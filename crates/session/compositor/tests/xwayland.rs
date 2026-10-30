//! Real X11 clients against a headless compositor: Xwayland maps their
//! windows, their program and icon reach the desktop, and the clipboard and
//! drags cross between them and Wayland clients.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::{Duration, Instant};

use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// A compositor with one output and Xwayland, and an X11 client on it.
struct Session {
    compositor: Compositor,
    x11: RustConnection,
    root: Window,
    /// Everything the X11 client was sent so far.
    events: Vec<Event>,
}

impl Session {
    fn start(scale: f64) -> Self {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("headless");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 1280, 720).with_scale(scale))
            .expect("output");
        let display = compositor.start_xwayland().expect("Xwayland");
        let (x11, root) = x11_client(&mut compositor, display);
        Self {
            compositor,
            x11,
            root,
            events: Vec::new(),
        }
    }

    /// A window of `size` whose class is `class`, unmapped.
    fn window(&self, instance: &str, class: &str, size: (u16, u16)) -> Window {
        let window = self.x11.generate_id().expect("id");
        self.x11
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                size.0,
                size.1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .event_mask(EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE),
            )
            .expect("window");
        self.x11
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                format!("{instance}\0{class}\0").as_bytes(),
            )
            .expect("class");
        self.x11
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                format!("{instance} window").as_bytes(),
            )
            .expect("title");
        window
    }

    /// Set a 32-bit property of `window` under `name`.
    fn set_property32(&self, window: Window, name: &str, values: &[u32]) -> Atom {
        let atom = self
            .x11
            .intern_atom(false, name.as_bytes())
            .expect("atom")
            .reply()
            .expect("atom reply")
            .atom;
        self.x11
            .change_property32(PropMode::REPLACE, window, atom, AtomEnum::CARDINAL, values)
            .expect("property");
        self.x11.flush().expect("flush");
        atom
    }

    /// Set a window's `WM_CLASS` after it was created.
    fn set_class(&self, window: Window, instance: &str, class: &str) {
        self.x11
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                format!("{instance}\0{class}\0").as_bytes(),
            )
            .expect("class");
        self.x11.flush().expect("flush");
    }

    fn map(&self, window: Window) {
        self.x11.map_window(window).expect("map");
        self.x11.flush().expect("flush");
    }

    fn destroy(&self, window: Window) {
        self.x11.destroy_window(window).expect("destroy");
        self.x11.flush().expect("flush");
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
                self.events.push(event);
            }
            if done(self) {
                return;
            }
        }
        panic!(
            "the compositor never got there: {:?}",
            self.compositor.windows()
        );
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

#[test]
fn an_x11_window_maps_as_a_window_of_its_class() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    session.pump(|session| !session.compositor.windows().is_empty());
    let (info, rect) = session.compositor.windows().remove(0);
    assert_eq!(info.app_id.as_deref(), Some("XTerm"));
    assert_eq!(info.title.as_deref(), Some("xterm window"));
    // Centred on the output by the stack policy, like any other window.
    assert_eq!(rect, (440, 210, 400, 300));
}

#[test]
fn an_x11_window_takes_focus_and_closes_like_any_other_window() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    session.pump(|session| {
        session
            .events
            .iter()
            .any(|event| matches!(event, Event::FocusIn(_)))
    });
    session.pump(|session| !session.compositor.windows().is_empty());
    let (info, _) = session.compositor.windows().remove(0);
    session.compositor.close_window(info.id);
    session.pump(|session| session.compositor.windows().is_empty());
}

#[test]
fn an_x11_window_knows_its_program_and_its_icon() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    // A window may claim any process; the X server's own record decides.
    session.set_property32(window, "_NET_WM_PID", &[999_999]);
    session.set_property32(
        window,
        "_NET_WM_ICON",
        // 2x2: red, green, blue and white, each opaque.
        &[2, 2, 0xFFFF_0000, 0xFF00_FF00, 0xFF00_00FF, 0xFFFF_FFFF],
    );
    session.map(window);
    session.pump(|session| !session.compositor.windows().is_empty());
    let (info, _) = session.compositor.windows().remove(0);
    let identity = session.compositor.x11_identity(info.id).expect("identity");
    assert_eq!(identity.pid, Some(std::process::id()));
    assert_eq!(identity.instance.as_deref(), Some("xterm"));
    assert_eq!(identity.class.as_deref(), Some("XTerm"));
    let icon = identity.icon.as_ref().expect("icon");
    assert_eq!((icon.width, icon.height), (2, 2));
    assert_eq!(icon.pixels.len(), 16);
    assert_eq!(&icon.pixels[..4], &[255, 0, 0, 255]);
}

#[test]
fn an_x11_window_that_is_renamed_is_known_by_its_new_class() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    session.pump(|session| !session.compositor.windows().is_empty());
    let (info, _) = session.compositor.windows().remove(0);
    session.set_class(window, "thunderbird", "Thunderbird");
    session.pump(|session| {
        session
            .compositor
            .x11_identity(info.id)
            .is_some_and(|identity| identity.class.as_deref() == Some("Thunderbird"))
    });
    let identity = session.compositor.x11_identity(info.id).expect("identity");
    assert_eq!(identity.instance.as_deref(), Some("thunderbird"));
    assert_eq!(identity.class.as_deref(), Some("Thunderbird"));
}
