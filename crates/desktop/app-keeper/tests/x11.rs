//! X11 programs of a kept launch: a real X client on the keeper's own display, its server
//! (xwayland-satellite and Xwayland) a client of the keeper, on a real headless desktop.

use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use smithay::xwayland::XWaylandListener;

use tilo_compositor::Selection;
use tilo_test_env::SpawnUnderEnvLock as _;
use x11rb::connection::Connection as _;
use x11rb::errors::ConnectError;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;

use crate::support::Rig;

const MIME: &str = "text/plain;charset=utf-8";
const BLUE: u32 = 0x0000_00ff;
const RED: u32 = 0x00ff_0000;

/// Whether the X server a keeper runs is installed here.
pub fn servers() -> Result<(), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for program in ["xwayland-satellite", "Xwayland"] {
        if !std::env::split_paths(&path).any(|dir| dir.join(program).is_file()) {
            return Err(format!("{program} is not on PATH"));
        }
    }
    Ok(())
}

/// A keeper serving an X11 display, attached to a desktop, and an X client connected to it.
pub struct X {
    pub rig: Rig,
    pub x11: RustConnection,
    pub root: Window,
    /// The clipboard this client owns, answered while the rig runs.
    clipboard: Option<Vec<u8>>,
    /// Whether it names its text only `UTF8_STRING`, as old toolkits do.
    old_names: bool,
    /// Whether it answers `_NET_WM_PING` when it reads one.
    pub answers_pings: bool,
    /// The pings it read.
    pub pings: Vec<ClientMessageEvent>,
}

/// Connect to the keeper's display as its app would, with the cookie its keeper wrote, or with
/// none, while the rig runs: `None` when the server refused.
fn connect(rig: &mut Rig, with_cookie: bool) -> Option<RustConnection> {
    let dir = rig.dir.path().join("keeper");
    connect_in(rig, &dir, with_cookie)
}

/// Connect to the display of the keeper whose directory is `dir`, as its app would.
fn connect_in(rig: &mut Rig, dir: &std::path::Path, with_cookie: bool) -> Option<RustConnection> {
    let display: u32 = std::fs::read_to_string(dir.join("x11"))
        .expect("the keeper binds a display")
        .trim()
        .trim_start_matches(':')
        .parse()
        .expect("a display number");
    let authority =
        std::fs::read(dir.join("Xauthority")).expect("the keeper wrote the display's cookie");
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

/// A window at `at` of `size` in `colour` on `conn`'s display, mapped: a toplevel of `class`, or
/// an override-redirect one (a menu) when `class` is `None`.
pub fn create_window(
    conn: &RustConnection,
    root: Window,
    class: Option<&str>,
    at: (i16, i16),
    size: (u16, u16),
    colour: u32,
) -> Window {
    let window = conn.generate_id().expect("id");
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
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
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            format!("{}\0{class}\0", class.to_lowercase()).as_bytes(),
        )
        .expect("class");
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"An X11 window",
        )
        .expect("title");
    }
    conn.map_window(window).expect("map");
    conn.flush().expect("flush");
    window
}

impl X {
    pub fn start() -> Self {
        Self::on(Rig::with_x11())
    }

    /// A rig whose desktop two keepers show on, and this launch's X client of one of them.
    pub fn start_pair() -> Self {
        Self::on(Rig::with_x11_pair())
    }

    /// The X client of `rig`'s own keeper.
    fn on(mut rig: Rig) -> Self {
        let x11 = connect(&mut rig, true).expect("the X client connects with the cookie");
        let root = x11.setup().roots[0].root;
        Self {
            rig,
            x11,
            root,
            clipboard: None,
            old_names: false,
            answers_pings: true,
            pings: Vec::new(),
        }
    }

    pub fn atom(&self, name: &str) -> u32 {
        self.x11
            .intern_atom(false, name.as_bytes())
            .expect("atom")
            .reply()
            .expect("atom")
            .atom
    }

    /// The other keeper's display, a client of it: its connection and root window.
    pub fn second_display(&mut self) -> (RustConnection, Window) {
        let dir = self.rig.dir.path().join("other-keeper");
        let x11 = connect_in(&mut self.rig, &dir, true).expect("the second X client connects");
        let root = x11.setup().roots[0].root;
        (x11, root)
    }

    /// A window at `at` of `size` in `colour`, mapped: a toplevel of `class`, or an
    /// override-redirect one (a menu) when `class` is `None`.
    pub fn window(
        &self,
        class: Option<&str>,
        at: (i16, i16),
        size: (u16, u16),
        colour: u32,
    ) -> Window {
        create_window(&self.x11, self.root, class, at, size, colour)
    }

    /// Answer the last ping read, as a program does: the message goes back to the root.
    pub fn pong(&self) {
        let mut message = *self.pings.last().expect("a ping was read");
        message.window = self.root;
        self.x11
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                message,
            )
            .expect("pong");
        self.x11.flush().expect("flush");
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
                match &event {
                    Event::SelectionRequest(request) => self.answer(request),
                    Event::ClientMessage(message)
                        if message.data.as_data32()[0] == self.atom("_NET_WM_PING") =>
                    {
                        self.pings.push(*message);
                        if self.answers_pings {
                            self.pong();
                        }
                    }
                    _ => {}
                }
            }
            if done(self) {
                return;
            }
        }
        panic!("never got there: {:?}", self.rig.windows());
    }

    /// The desktop's window of the class `class`, once it shows: its rectangle.
    pub fn shown(&mut self, class: &str) -> (i32, i32, i32, i32) {
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

/// Kept launches take display numbers machine-wide, each in its own process, and one that was
/// killed leaves its lock behind. Several taking that number at once: one holds it, with its lock
/// and socket file in place for as long as it does, and leaves none behind.
#[test]
fn a_display_left_by_a_killed_launch_goes_to_one_of_those_taking_it_at_once() {
    // Far above the displays a desktop on this machine uses.
    let number = 7000 + std::process::id() % 1000;
    let lock = std::path::PathBuf::from(format!("/tmp/.X{number}-lock"));
    let path = std::path::PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
    let mut gone = std::process::Command::new("true")
        .spawn_locked()
        .expect("a process");
    let dead = gone.id();
    gone.wait().expect("it ended");
    let _ = std::fs::create_dir("/tmp/.X11-unix");
    for round in 0..100 {
        // As a killed launch leaves its display.
        std::fs::write(&lock, format!("{dead:>10}\n")).expect("a lock");
        let barrier = Arc::new(Barrier::new(8));
        let mut takers = Vec::new();
        for _ in 0..8 {
            let barrier = Arc::clone(&barrier);
            takers.push(std::thread::spawn(move || {
                barrier.wait();
                XWaylandListener::prepare(Some(number), true).ok()
            }));
        }
        let mut held = Vec::new();
        for taker in takers {
            held.extend(taker.join().expect("taker"));
        }
        assert_eq!(held.len(), 1, "round {round}: one taker holds the display");
        let written = std::fs::read_to_string(&lock);
        assert_eq!(
            written.as_deref().map(str::trim).ok(),
            Some(std::process::id().to_string().as_str()),
            "round {round}: the holder's lock stays in place"
        );
        assert!(
            std::os::unix::net::UnixStream::connect(&path).is_ok(),
            "round {round}: its socket file reaches it"
        );
        drop(held);
        assert!(
            !lock.exists() && !path.exists(),
            "round {round}: the holder leaves nothing behind"
        );
    }
}

/// The pid of the X server serving `number`: the `xwayland-satellite` this test process started.
fn server_pid(number: u32) -> Option<u32> {
    let wanted = format!(":{number}");
    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
        if args
            .first()
            .is_some_and(|arg| arg.ends_with(b"xwayland-satellite"))
            && args.get(1).is_some_and(|arg| *arg == wanted.as_bytes())
        {
            return Some(pid);
        }
    }
    None
}

fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: kill takes a pid and a signal number.
    let sent = unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), signal) };
    assert_eq!(sent, 0, "the signal reaches {pid}");
}

tilo_test_env::scenario! {
    fn an_x11_program_of_a_kept_launch_lives_on_the_keepers_display;

    phase the_keepers_directory_names_its_display_and_the_cookie_beside_it;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let x = X::start();
        let display = x.rig.keeper.x11_display().expect("the keeper binds a display");
        let dir = x.rig.dir.path().join("keeper");
        assert_eq!(
            std::fs::read_to_string(dir.join("x11")).expect("the display's name"),
            format!(":{display}"),
            "a session serving the display reads its name here"
        );
        assert!(dir.join("Xauthority").is_file(), "and the cookie beside it");
        assert!(
            dir.join("x11-server").is_file(),
            "a display whose server is up is named as one a session can serve"
        );
    }

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

    phase an_x_programs_server_ending_takes_its_windows_and_nothing_else;
    {
        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut x = X::start();
        let number = x.rig.keeper.x11_display().expect("a display");
        let dir = x.rig.dir.path().join("keeper");
        x.window(Some("Ending"), (0, 0), (200, 150), BLUE);
        x.shown("Ending");
        assert!(dir.join("x11-server").is_file(), "the server is named while it serves");
        // The server crashes, as Xwayland does when a program kills it.
        let pid = server_pid(number).expect("the server is up");
        signal(pid, libc::SIGKILL);
        let started = Instant::now();
        while !x.rig.windows().is_empty() {
            x.rig.settle(&mut []);
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the window never left: {:?}",
                x.rig.windows()
            );
        }
        assert!(
            !dir.join("x11-server").exists(),
            "a display whose server ended is not named as one to serve"
        );
        assert_eq!(x.rig.keeper.describe().fault, None, "the launch goes on");

        // The next client starts a fresh server on the same display, as it would after a crash.
        let second = connect(&mut x.rig, true).expect("the next X client connects");
        let root = second.setup().roots[0].root;
        create_window(&second, root, Some("Again"), (0, 0), (160, 120), BLUE);
        let started = Instant::now();
        while !x.rig.windows().iter().any(|(app_id, _, _)| app_id.as_deref() == Some("Again")) {
            x.rig.settle(&mut []);
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the next window never showed: {:?}",
                x.rig.windows()
            );
        }
        assert!(dir.join("x11-server").is_file(), "and the new server is named");
    }

    phase its_icon_reaches_the_desktop_follows_changes_and_survives_a_restart;
    {
        use tilo_compositor::IconArtwork;

        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut x = X::start();
        let set_icon = |x: &X, window: Window, argb: u32| {
            // A 16×8 image, which the desktop gets centred on a 16×16 square.
            let mut words = vec![16, 8];
            words.extend(std::iter::repeat_n(argb, 16 * 8));
            x.x11
                .change_property32(PropMode::REPLACE, window, x.atom("_NET_WM_ICON"), AtomEnum::CARDINAL, &words)
                .expect("icon");
            x.x11.flush().expect("flush");
        };
        let first = x.window(Some("Iconic"), (0, 0), (200, 150), BLUE);
        set_icon(&x, first, 0xffff_0000);
        x.shown("Iconic");
        let pixel_at = |x: &mut X, title: &str, row: usize| {
            x.rig.desktop().windows().into_iter().find(|(info, _)| info.title.as_deref() == Some(title)).and_then(|(info, _)| {
                let IconArtwork::Pixels(images) = info.icon?.artwork().clone() else {
                    return None;
                };
                let image = images.first()?;
                assert_eq!((image.width, image.height), (16, 16), "a square icon");
                image.pixels.get(row * 16 * 4 + 32..row * 16 * 4 + 36).map(<[u8]>::to_vec)
            })
        };
        x.pump(|x| pixel_at(x, "An X11 window", 4) == Some(vec![255, 0, 0, 255]));
        assert_eq!(pixel_at(&mut x, "An X11 window", 0), Some(vec![0, 0, 0, 0]), "the square's top is transparent");

        set_icon(&x, first, 0xff00_00ff);
        x.pump(|x| pixel_at(x, "An X11 window", 4) == Some(vec![0, 0, 255, 255]));

        x.rig.drop_desktop();
        x.rig.settle(&mut []);
        x.rig.attach_new(640, 480);
        x.pump(|x| pixel_at(x, "An X11 window", 4) == Some(vec![0, 0, 255, 255]));
        assert_eq!(x.rig.keeper.describe().fault, None);
    }

    phase a_hung_x11_program_looks_hung_until_it_answers;
    {
        use tilo_compositor::app_liveness::UNANSWERED;
        use tilo_compositor::InputEvent;

        tilo_test_env::require_capability!(XServer, servers(), "the keeper's X server");
        let mut x = X::start();
        let window = x.window(Some("Pinged"), (0, 0), (200, 150), BLUE);
        let protocols = x.atom("WM_PROTOCOLS");
        let ping = x.atom("_NET_WM_PING");
        x.x11
            .change_property32(PropMode::REPLACE, window, protocols, AtomEnum::ATOM, &[ping])
            .expect("protocols");
        x.shown("Pinged");
        let id = x.rig.desktop().windows()[0].0.id;

        // The program stops reading its events.
        x.answers_pings = false;
        x.rig.desktop().input(InputEvent::Key { keycode: 30, pressed: true }).expect("key");
        x.pump(|x| !x.pings.is_empty());
        x.rig.settle(&mut []);
        let later = Instant::now() + UNANSWERED + Duration::from_millis(10);
        x.rig.desktop().advance_app_liveness_at(later);
        assert_eq!(x.rig.desktop().not_responding_windows(), [id], "the X11 program's silence reached the desktop");

        x.pong();
        x.pump(|x| {
            x.rig.desktop().advance_app_liveness_at(Instant::now());
            x.rig.desktop().not_responding_windows().is_empty()
        });
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
