//! X11 programs of a kept launch: a real X client on the keeper's own display, its server
//! (xwayland-satellite and Xwayland) a client of the keeper, on a real headless desktop.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_compositor::Selection;
use x11rb::connection::Connection as _;
use x11rb::errors::ConnectError;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SELECTION_NOTIFY_EVENT,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;

use crate::support::Rig;

const MIME: &str = "text/plain;charset=utf-8";
const BLUE: u32 = 0x0000_00ff;
const RED: u32 = 0x00ff_0000;

/// Whether the X server a keeper runs is installed here.
fn servers() -> Result<(), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for program in ["xwayland-satellite", "Xwayland"] {
        if !std::env::split_paths(&path).any(|dir| dir.join(program).is_file()) {
            return Err(format!("{program} is not on PATH"));
        }
    }
    Ok(())
}

/// A keeper serving an X11 display, attached to a desktop, and an X client connected to it.
struct X {
    rig: Rig,
    x11: RustConnection,
    root: Window,
    /// The clipboard this client owns, answered while the rig runs.
    clipboard: Option<Vec<u8>>,
    /// Whether it names its text only `UTF8_STRING`, as old toolkits do.
    old_names: bool,
}

/// Connect to the keeper's display as its app would, with the cookie its keeper wrote, or with
/// none, while the rig runs: `None` when the server refused.
fn connect(rig: &mut Rig, with_cookie: bool) -> Option<RustConnection> {
    let display = rig
        .keeper
        .x11_display()
        .expect("the keeper binds a display");
    let authority = std::fs::read(rig.dir.path().join("keeper").join("Xauthority"))
        .expect("the keeper wrote the display's cookie");
    let cookie = authority[authority.len() - 16..].to_vec();
    let socket = format!("/tmp/.X11-unix/X{display}");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let stream = std::os::unix::net::UnixStream::connect(socket)
            .and_then(DefaultStream::from_unix_stream)
            .map_err(ConnectError::from);
        let connected = stream.and_then(|(stream, _)| {
            if with_cookie {
                RustConnection::connect_to_stream_with_auth_info(
                    stream,
                    0,
                    b"MIT-MAGIC-COOKIE-1".to_vec(),
                    cookie,
                )
            } else {
                RustConnection::connect_to_stream(stream, 0)
            }
        });
        let _ = sender.send(connected);
    });
    let started = Instant::now();
    loop {
        rig.settle(&mut []);
        if let Ok(connected) = receiver.try_recv() {
            return connected.ok();
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the keeper's X server never answered"
        );
    }
}

impl X {
    fn start() -> Self {
        let mut rig = Rig::with_x11();
        let x11 = connect(&mut rig, true).expect("the X client connects with the cookie");
        let root = x11.setup().roots[0].root;
        Self {
            rig,
            x11,
            root,
            clipboard: None,
            old_names: false,
        }
    }

    fn atom(&self, name: &str) -> u32 {
        self.x11
            .intern_atom(false, name.as_bytes())
            .expect("atom")
            .reply()
            .expect("atom")
            .atom
    }

    /// A window at `at` of `size` in `colour`, mapped: a toplevel of `class`, or an
    /// override-redirect one (a menu) when `class` is `None`.
    fn window(&self, class: Option<&str>, at: (i16, i16), size: (u16, u16), colour: u32) -> Window {
        let window = self.x11.generate_id().expect("id");
        self.x11
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                at.0,
                at.1,
                size.0,
                size.1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .background_pixel(colour)
                    .override_redirect(u32::from(class.is_none()))
                    .event_mask(EventMask::STRUCTURE_NOTIFY),
            )
            .expect("window");
        if let Some(class) = class {
            self.x11
                .change_property8(
                    PropMode::REPLACE,
                    window,
                    AtomEnum::WM_CLASS,
                    AtomEnum::STRING,
                    format!("{}\0{class}\0", class.to_lowercase()).as_bytes(),
                )
                .expect("class");
            self.x11
                .change_property8(
                    PropMode::REPLACE,
                    window,
                    AtomEnum::WM_NAME,
                    AtomEnum::STRING,
                    b"An X11 window",
                )
                .expect("title");
        }
        self.x11.map_window(window).expect("map");
        self.x11.flush().expect("flush");
        window
    }

    /// Where `window` is in the X server's root.
    fn root_position(&self, window: Window) -> (i32, i32) {
        let at = self
            .x11
            .translate_coordinates(window, self.root, 0, 0)
            .expect("position")
            .reply()
            .expect("position");
        (i32::from(at.dst_x), i32::from(at.dst_y))
    }

    /// Run the desktop, the keeper and the X client until `done`.
    fn pump(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            self.x11.flush().expect("flush");
            self.rig.settle(&mut []);
            while let Some(event) = self.x11.poll_for_event().expect("event") {
                if let Event::SelectionRequest(request) = &event {
                    self.answer(request);
                }
            }
            if done(self) {
                return;
            }
        }
        panic!("never got there: {:?}", self.rig.windows());
    }

    /// The desktop's window of the class `class`, once it shows: its rectangle.
    fn shown(&mut self, class: &str) -> (i32, i32, i32, i32) {
        let mut found = None;
        self.pump(|x| {
            found = x
                .rig
                .windows()
                .into_iter()
                .find(|(app_id, _, _)| app_id.as_deref() == Some(class))
                .map(|(_, _, rect)| rect);
            found.is_some()
        });
        found.expect("shown")
    }

    /// Answer a request for the clipboard this client owns.
    fn answer(&self, request: &SelectionRequestEvent) {
        let Some(bytes) = &self.clipboard else {
            return;
        };
        // As GTK and Qt offer text: under its X11 name and its MIME type.
        let utf8 = self.atom("UTF8_STRING");
        let mime = if self.old_names {
            utf8
        } else {
            self.atom(MIME)
        };
        let served = if request.target == self.atom("TARGETS") {
            self.x11
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    request.property,
                    AtomEnum::ATOM,
                    &[self.atom("TARGETS"), utf8, mime],
                )
                .is_ok()
        } else if request.target == utf8 || request.target == mime {
            self.x11
                .change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    request.property,
                    utf8,
                    bytes,
                )
                .is_ok()
        } else {
            false
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served {
                request.property
            } else {
                x11rb::NONE
            },
        };
        let _ = self
            .x11
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify);
        let _ = self.x11.flush();
    }
}

tilo_test_env::scenario! {
    fn an_x11_program_of_a_kept_launch_lives_on_the_keepers_display;

    phase its_window_shows_with_its_class_and_survives_a_desktop_restart;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut x = X::start();
        x.window(Some("Kept-X11"), (0, 0), (200, 150), BLUE);
        let (_, _, w, _) = x.shown("Kept-X11");
        assert_eq!(w, 200, "the window keeps its size");

        x.rig.drop_desktop();
        x.rig.settle(&mut []);
        // The X server lives on with the keeper: the same connection still answers.
        x.x11.get_input_focus().expect("ask").reply().expect("the X server answers");
        x.rig.attach_new(640, 480);
        let (_, _, w, _) = x.shown("Kept-X11");
        assert_eq!(w, 200, "and shows again on the next desktop");
        x.x11.get_input_focus().expect("ask").reply().expect("on the same connection");
    }

    phase a_menu_opens_where_the_program_put_it;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut x = X::start();
        let toplevel = x.window(Some("Menus"), (0, 0), (300, 200), BLUE);
        let (left, top, _, height) = x.shown("Menus");
        // The window's content starts below whatever frame the desktop or the server drew.
        let top = (top..top + height)
            .find(|row| x.rig.pixel((left + 150) as u32, *row as u32)[..3] == [0, 0, 255])
            .expect("the window's content shows");
        let (root_x, root_y) = x.root_position(toplevel);
        let (dx, dy) = (60, 40);
        let at = (
            i16::try_from(root_x + dx).expect("fits"),
            i16::try_from(root_y + dy).expect("fits"),
        );
        x.window(None, at, (50, 30), RED);
        let inside = ((left + dx + 25) as u32, (top + dy + 15) as u32);
        let outside = ((left + dx - 10) as u32, (top + dy - 10) as u32);
        x.pump(|x| x.rig.pixel(inside.0, inside.1)[..3] == [255, 0, 0]);
        assert_eq!(x.rig.pixel(outside.0, outside.1)[..3], [0, 0, 255], "the menu is where it was put, not elsewhere");
    }

    phase only_a_client_with_the_launchs_cookie_reaches_its_display;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut x = X::start();
        assert!(
            connect(&mut x.rig, false).is_none(),
            "a client without the cookie, another user's or another launch's, is refused"
        );
        x.x11.get_input_focus().expect("ask").reply().expect("the app's client still is served");
    }

    phase a_copy_in_the_x11_program_pastes_on_the_desktop;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        // As GTK and Qt name text, and as an old program does, which a reader of `text/plain`
        // reads all the same.
        for (old_names, wanted) in [(false, MIME), (true, "text/plain")] {
            let mut x = X::start();
            x.old_names = old_names;
            let window = x.window(Some("Copying"), (0, 0), (200, 150), BLUE);
            x.shown("Copying");
            // A copy takes the keyboard focus the desktop gave the window.
            x.pump(|x| {
                x.x11
                    .get_input_focus()
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .is_some_and(|focus| focus.focus == window)
            });
            x.clipboard = Some(b"copied in X11".to_vec());
            let clipboard = x.atom("CLIPBOARD");
            x.x11
                .set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
                .expect("owner");
            x.pump(|x| {
                x.rig
                    .desktop()
                    .selection_formats(Selection::Clipboard)
                    .iter()
                    .any(|mime| mime == wanted)
            });
            let reader = x
                .rig
                .desktop()
                .receive_selection(Selection::Clipboard, wanted)
                .expect("the desktop reads the copy");
            let bytes = Arc::new(Mutex::new(None));
            let written = Arc::clone(&bytes);
            std::thread::spawn(move || {
                let mut read = Vec::new();
                let mut reader = reader;
                let _ = std::io::Read::read_to_end(&mut reader, &mut read);
                *written.lock().unwrap() = Some(read);
            });
            x.pump(|_| bytes.lock().unwrap().is_some());
            assert_eq!(bytes.lock().unwrap().take().unwrap(), b"copied in X11", "read as {wanted}");
        }
    }
}
