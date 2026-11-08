//! Real X11 clients against a headless compositor: Xwayland maps their windows, their program and
//! icon reach the desktop, and the clipboard and drags cross between them and Wayland clients.

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

use tilo_compositor::selection::TEXT_MIME;
use tilo_compositor::{
    Compositor, IconArtwork, KeyboardLayout, OutputConfig, Selection, StackPolicy,
};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
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
    /// The clipboard this X11 client owns, per format, answered while the session runs; `None` when
    /// it owns none.
    clipboard: Option<Vec<(String, Vec<u8>)>>,
}

impl Session {
    fn start(scale: f64) -> Self {
        Self::start_in_layout(scale, None)
    }

    /// A session whose person types in `layout` before any X11 app opens, as at login; `None`
    /// leaves the layout the session starts with.
    fn start_in_layout(scale: f64, layout: Option<KeyboardLayout>) -> Self {
        let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("headless");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 1280, 720).with_scale(scale))
            .expect("output");
        if let Some(layout) = layout {
            compositor.set_keyboard_layout(&layout);
        }
        let display = compositor
            .x11_displays()
            .expect("displays")
            .display_for("xwayland")
            .expect("X11 display");
        let (x11, root) = x11_client(&mut compositor, display);
        Self {
            compositor,
            x11,
            root,
            events: Vec::new(),
            clipboard: None,
        }
    }

    /// The character `keycode` types for X clients, in the first group: what the toolkit of an X11
    /// app reads from the server's own keymap.
    fn character(&self, keycode: u8) -> char {
        let keysyms = self
            .x11
            .get_keyboard_mapping(keycode, 1)
            .expect("keyboard mapping")
            .reply()
            .expect("keyboard mapping reply")
            .keysyms;
        match keysyms.first().copied() {
            Some(keysym) => char::from_u32(keysym).unwrap_or('\0'),
            None => '\0',
        }
    }

    /// The fields of the root window's `_XKB_RULES_NAMES` , which layout tools (`setxkbmap -query`)
    /// report: rules, model, layout, variant and options.
    fn rules_names(&self) -> Vec<String> {
        let property = self
            .x11
            .get_property(
                false,
                self.root,
                self.atom("_XKB_RULES_NAMES"),
                AtomEnum::STRING,
                0,
                u32::MAX,
            )
            .expect("rules names")
            .reply()
            .expect("rules names reply")
            .value;
        String::from_utf8_lossy(&property)
            .split('\0')
            .take(5)
            .map(str::to_owned)
            .collect()
    }

    /// A window of `size` whose class is `class` , unmapped.
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

    /// Set a 32-bit property of `window` under `name` .
    fn set_property32(&self, window: Window, name: &str, values: &[u32]) -> Atom {
        let atom = self.atom(name);
        self.x11
            .change_property32(PropMode::REPLACE, window, atom, AtomEnum::CARDINAL, values)
            .expect("property");
        self.x11.flush().expect("flush");
        atom
    }

    /// The atom named `name` .
    fn atom(&self, name: &str) -> Atom {
        self.x11
            .intern_atom(false, name.as_bytes())
            .expect("atom")
            .reply()
            .expect("atom reply")
            .atom
    }

    /// The integer setting `name` of the XSETTINGS manager, as X clients read it; `None` while it
    /// has none.
    fn setting(&self, name: &str) -> Option<i32> {
        let owner = self
            .x11
            .get_selection_owner(self.atom("_XSETTINGS_S0"))
            .expect("owner")
            .reply()
            .expect("owner reply")
            .owner;
        if owner == x11rb::NONE {
            return None;
        }
        let value = self
            .x11
            .get_property(
                false,
                owner,
                self.atom("_XSETTINGS_SETTINGS"),
                AtomEnum::ANY,
                0,
                u32::MAX,
            )
            .expect("settings")
            .reply()
            .expect("settings reply")
            .value;
        xsetting_integer(&value, name)
    }

    /// The root window's `RESOURCE_MANAGER` , as X clients read it.
    fn resources(&self) -> String {
        let value = self
            .x11
            .get_property(
                false,
                self.root,
                AtomEnum::RESOURCE_MANAGER,
                AtomEnum::STRING,
                0,
                u32::MAX,
            )
            .expect("resources")
            .reply()
            .expect("resources reply")
            .value;
        String::from_utf8(value).expect("utf-8 resources")
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

    /// Set a window's `WM_NAME` after it was created, as a program does when what it shows changes.
    fn set_title(&self, window: Window, title: &str) {
        self.x11
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .expect("title");
        self.x11.flush().expect("flush");
    }

    fn map(&self, window: Window) {
        self.x11.map_window(window).expect("map");
        self.x11.flush().expect("flush");
    }

    /// Answer an X11 client asking for the clipboard this session's client owns: its formats, or
    /// one of them.
    fn answer(&self, request: &SelectionRequestEvent) {
        let Some(offer) = &self.clipboard else {
            return;
        };
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let served = if request.target == self.atom("TARGETS") {
            let mut targets = vec![self.atom("TARGETS"), self.atom("UTF8_STRING")];
            targets.extend(offer.iter().map(|(mime, _)| self.atom(mime)));
            self.x11
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )
                .is_ok()
        } else {
            let name = self
                .x11
                .get_atom_name(request.target)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| String::from_utf8(reply.name).ok());
            let value = name.as_deref().and_then(|name| {
                offer
                    .iter()
                    .find(|(mime, _)| mime == name)
                    .or_else(|| {
                        // X11 clients ask for text by its older names.
                        matches!(name, "UTF8_STRING" | "STRING" | "TEXT")
                            .then(|| offer.iter().find(|(mime, _)| mime.starts_with("text/")))
                            .flatten()
                    })
                    .map(|(_, bytes)| bytes)
            });
            match value {
                Some(bytes) => self
                    .x11
                    .change_property8(
                        PropMode::REPLACE,
                        request.requestor,
                        property,
                        self.atom("UTF8_STRING"),
                        bytes,
                    )
                    .is_ok(),
                None => false,
            }
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if served { property } else { x11rb::NONE },
        };
        let _ = self
            .x11
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify);
        let _ = self.x11.flush();
    }

    /// Make `window` own the clipboard with `offer` (text per format), and wait until the session
    /// offers what it says.
    fn own_clipboard(&mut self, window: Window, offer: &[(&str, &[u8])]) -> Vec<String> {
        self.clipboard = Some(
            offer
                .iter()
                .map(|(mime, bytes)| ((*mime).to_owned(), (*bytes).to_vec()))
                .collect(),
        );
        self.x11
            .set_selection_owner(window, self.atom("CLIPBOARD"), x11rb::CURRENT_TIME)
            .expect("owner");
        self.x11.flush().expect("flush");
        self.pump(|session| {
            !session
                .compositor
                .selection_formats(Selection::Clipboard)
                .is_empty()
        });
        self.compositor.selection_formats(Selection::Clipboard)
    }

    /// Run the session on for a moment, as it would between two of a person's actions.
    fn settle(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(100) {
            self.x11.flush().expect("flush");
            self.compositor
                .dispatch(Duration::from_millis(2))
                .expect("dispatch");
            while let Some(event) = self.x11.poll_for_event().expect("event") {
                if let Event::SelectionRequest(request) = &event {
                    self.answer(request);
                }
                self.events.push(event);
            }
        }
    }

    /// Read `selection` as `mime` off the session, as a Wayland client would,
    /// while the session runs.
    fn read_selection(&mut self, selection: Selection, mime: &str) -> Vec<u8> {
        let reader = self
            .compositor
            .receive_selection(selection, mime)
            .expect("reader");
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(None));
        let filled = bytes.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut read = Vec::new();
            let received = std::io::Read::read_to_end(&mut reader, &mut read).map(|_| read);
            *filled.lock().expect("lock") = Some(received.unwrap_or_default());
        });
        self.pump(|_| bytes.lock().expect("lock").is_some());
        bytes.lock().expect("lock").take().expect("bytes")
    }

    /// Let the compositor and the X11 client run until `done` .
    fn pump(&mut self, mut done: impl FnMut(&Session) -> bool) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            self.x11.flush().expect("flush");
            self.compositor
                .dispatch(Duration::from_millis(2))
                .expect("dispatch");
            while let Some(event) = self.x11.poll_for_event().expect("event") {
                if let Event::SelectionRequest(request) = &event {
                    self.answer(request);
                }
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

/// An X11 client on `display` : Xwayland answers the connection's setup only after it has
/// roundtripped with the compositor, so the client waits in a thread while the compositor
/// dispatches here.
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

/// An integer in an `_XSETTINGS_SETTINGS` value, as X clients decode it: byte order (padded),
/// serial, count, then per setting its type, name and value.
fn xsetting_integer(data: &[u8], name: &str) -> Option<i32> {
    let big_endian = match data.first()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let u16_at = |offset: usize| -> Option<u16> {
        let bytes: [u8; 2] = data.get(offset..offset + 2)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    };
    let u32_at = |offset: usize| -> Option<u32> {
        let bytes: [u8; 4] = data.get(offset..offset + 4)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let count = u32_at(8)?;
    let mut offset = 12;
    for _ in 0..count {
        let kind = *data.get(offset)?;
        let length = usize::from(u16_at(offset + 2)?);
        let start = offset + 4;
        let named = data.get(start..start + length)? == name.as_bytes();
        offset = start + length.next_multiple_of(4) + 4;
        let value = match kind {
            // Integer: one 32-bit value.
            0 => {
                let value = u32_at(offset)?;
                offset += 4;
                i32::try_from(value).ok()
            }
            // String: length-prefixed, padded; a color: four 16-bit channels.
            1 => {
                let length = usize::try_from(u32_at(offset)?).ok()?;
                offset += 4 + length.next_multiple_of(4);
                None
            }
            _ => {
                offset += 8;
                None
            }
        };
        if named {
            return value;
        }
    }
    None
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
    // Xwayland associates a window's surface once it is mapped; the icon is read again then, as
    // part of the window's facts.
    session.pump(|session| {
        session
            .compositor
            .windows()
            .first()
            .is_some_and(|(info, _)| info.icon.is_some())
    });
    let (info, _) = session.compositor.windows().remove(0);
    let identity = session.compositor.x11_identity(info.id).expect("identity");
    assert_eq!(identity.pid, Some(std::process::id()));
    assert_eq!(identity.instance.as_deref(), Some("xterm"));
    assert_eq!(identity.class.as_deref(), Some("XTerm"));
    let icon = identity.icon.as_ref().expect("icon");
    assert_eq!((icon.width, icon.height), (2, 2));
    assert_eq!(icon.pixels.len(), 16);
    assert_eq!(&icon.pixels[..4], &[255, 0, 0, 255]);

    // The window's facts carry the same icon: what the desktop draws.
    let own = info.icon.expect("the window's own icon");
    let IconArtwork::Pixels(images) = own.artwork() else {
        panic!("the pixels the window handed over");
    };
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].width, images[0].height), (2, 2));
    assert_eq!(&images[0].pixels[..4], &[255, 0, 0, 255]);
}

#[test]
fn an_x11_window_that_is_renamed_is_known_by_its_new_class() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    session.pump(|session| !session.compositor.windows().is_empty());
    let (mapped, _) = session.compositor.windows().remove(0);
    session.set_class(window, "thunderbird", "Thunderbird");
    session.set_title(window, "Inbox - Thunderbird");
    // The two properties arrive as two separate changes; the desk names the window once it has
    // both, so wait for both.
    session.pump(|session| {
        session
            .compositor
            .windows()
            .first()
            .is_some_and(|(info, _)| {
                info.app_id.as_deref() == Some("Thunderbird")
                    && info.title.as_deref() == Some("Inbox - Thunderbird")
            })
    });
    let identity = session
        .compositor
        .x11_identity(mapped.id)
        .expect("identity");
    assert_eq!(identity.instance.as_deref(), Some("thunderbird"));
    assert_eq!(identity.class.as_deref(), Some("Thunderbird"));
    // The desk names the tab by what the client says now, not by what it said when its window
    // mapped.
    let (renamed, _) = session.compositor.windows().remove(0);
    assert_eq!(renamed.id, mapped.id);
    assert_eq!(renamed.app_id.as_deref(), Some("Thunderbird"));
    assert_eq!(renamed.title.as_deref(), Some("Inbox - Thunderbird"));
}

#[test]
fn x_clients_hear_how_dense_the_display_is() {
    let mut session = Session::start(2.0);
    // A 96 DPI display at scale 2 is a 192 DPI display to X clients.
    session.pump(|session| session.setting("Xft/DPI") == Some(192 * 1024));
    assert_eq!(session.setting("Gdk/WindowScalingFactor"), Some(2));
    assert_eq!(session.setting("Gdk/UnscaledDPI"), Some(96 * 1024));
    let resources = session.resources();
    assert!(resources.contains("Xft.dpi:\t192"), "{resources}");
    assert!(resources.contains("Xcursor.size:\t48"), "{resources}");

    // Their windows are sized in those pixels: 400 by 300 X pixels are 200 by 150 logical ones,
    // centred on the output's 640 by 360 logical size like any other window.
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    session.pump(|session| !session.compositor.windows().is_empty());
    let (_, rect) = session.compositor.windows().remove(0);
    assert_eq!(rect, (220, 105, 200, 150));
}

#[test]
fn x_clients_draw_the_pointer_theme_the_person_chose() {
    let mut session = Session::start(1.0);
    session.pump(|session| session.resources().contains("Xcursor.theme:"));
    // X clients draw their own pointers from the theme they are told.
    let mut a11y = session.compositor.a11y().clone();
    a11y.cursor.theme = "Bibata-Modern-Ice".to_owned();
    session
        .compositor
        .set_a11y(a11y)
        .expect("the theme applies");
    session.pump(|session| {
        session
            .resources()
            .contains("Xcursor.theme:\tBibata-Modern-Ice")
    });
    let resources = session.resources();
    assert!(
        resources.contains("Xcursor.theme:\tBibata-Modern-Ice"),
        "{resources}"
    );
    assert_eq!(
        resources.matches("Xcursor.theme:").count(),
        1,
        "one theme line, not two: {resources}"
    );
}

#[test]
fn a_denser_output_tells_x_clients_a_new_scale() {
    let mut session = Session::start(1.0);
    session.pump(|session| session.setting("Xft/DPI") == Some(96 * 1024));
    assert!(session.resources().contains("Xft.dpi:\t96"));
    session
        .compositor
        .add_output(&OutputConfig::new("HEADLESS-2", 1280, 720).with_scale(2.0))
        .expect("output");
    session.pump(|session| session.setting("Xft/DPI") == Some(192 * 1024));
    assert_eq!(session.setting("Gdk/WindowScalingFactor"), Some(2));
    let resources = session.resources();
    assert!(resources.contains("Xft.dpi:\t192"), "{resources}");
    assert!(resources.contains("Xcursor.size:\t48"), "{resources}");
}

/// The key that types `y` on a US keyboard and `z` on a German one.
const KEY_Y: u8 = 29;

#[test]
fn x11_apps_type_in_the_layout_the_person_chose() {
    // The session sets the person's layout at login; the first X11 app they open types in it.
    let session = Session::start_in_layout(
        1.0,
        Some(KeyboardLayout {
            layout: "de".into(),
            ..KeyboardLayout::default()
        }),
    );
    assert_eq!(
        session.character(KEY_Y),
        'z',
        "an X11 app opened after login types in the person's German layout"
    );
    assert_eq!(
        session.rules_names(),
        ["evdev", "pc105", "de", "", ""],
        "and what asks which layout is in force hears the German one"
    );
}

#[test]
fn a_layout_change_reaches_x11_apps() {
    let mut session = Session::start(1.0);
    assert_eq!(
        session.character(KEY_Y),
        'y',
        "the X server types in its own layout before the person changes it"
    );
    assert_eq!(session.rules_names(), ["evdev", "pc105", "us", "", ""]);

    session.compositor.set_keyboard_layout(&KeyboardLayout {
        layout: "de".into(),
        ..KeyboardLayout::default()
    });
    session.pump(|session| session.character(KEY_Y) == 'z');
    assert_eq!(
        session.rules_names(),
        ["evdev", "pc105", "de", "", ""],
        "the layout tools report what the keys type"
    );

    // A second change reaches the same server, windows and all.
    session
        .compositor
        .set_keyboard_layout(&KeyboardLayout::default());
    session.pump(|session| session.character(KEY_Y) == 'y');
}

#[test]
fn a_copy_in_an_x11_app_pastes_in_a_wayland_app() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    let formats = session.own_clipboard(window, &[(TEXT_MIME, b"copied in xterm")]);
    assert!(formats.iter().any(|mime| mime == TEXT_MIME), "{formats:?}");
    let bytes = session.read_selection(Selection::Clipboard, TEXT_MIME);
    assert_eq!(bytes, b"copied in xterm");
}

#[test]
fn a_copy_in_a_wayland_app_pastes_in_an_x11_app() {
    let mut session = Session::start(1.0);
    let window = session.window("xterm", "XTerm", (400, 300));
    session.map(window);
    // An X11 client reads the clipboard only while one of its windows has the keyboard focus.
    session.pump(|session| {
        session
            .events
            .iter()
            .any(|event| matches!(event, Event::FocusIn(_)))
    });
    session
        .compositor
        .set_selection(
            Selection::Clipboard,
            vec![(TEXT_MIME.to_owned(), b"copied in a wayland app".to_vec())],
        )
        .expect("selection");
    // Xwayland tells X clients about the selection on its own connection,
    // which runs when the session does.
    session.settle();
    let copied = session.atom("TILO_COPIED");
    session
        .x11
        .convert_selection(
            window,
            session.atom("CLIPBOARD"),
            session.atom("UTF8_STRING"),
            copied,
            x11rb::CURRENT_TIME,
        )
        .expect("convert");
    session.x11.flush().expect("flush");
    session.pump(|session| {
        session.events.iter().any(
            |event| matches!(event, Event::SelectionNotify(notify) if notify.property == copied),
        )
    });
    let value = session
        .x11
        .get_property(false, window, copied, AtomEnum::ANY, 0, u32::MAX)
        .expect("property")
        .reply()
        .expect("property reply")
        .value;
    assert_eq!(value, b"copied in a wayland app");
}

/// A window `x11` creates on `root` , with the class and title a program gives a toplevel,
/// unmapped.
fn client_window(x11: &RustConnection, root: Window, instance: &str, class: &str) -> Window {
    let window = x11.generate_id().expect("id");
    x11.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        400,
        300,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .expect("window");
    x11.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        format!("{instance}\0{class}\0").as_bytes(),
    )
    .expect("class");
    x11.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        format!("{instance} window").as_bytes(),
    )
    .expect("title");
    x11.flush().expect("flush");
    window
}

/// The atom named `name` on `x11` 's server.
fn client_atom(x11: &RustConnection, name: &str) -> Atom {
    x11.intern_atom(false, name.as_bytes())
        .expect("atom")
        .reply()
        .expect("atom reply")
        .atom
}

/// Run a compositor without a client of the test's own, until `done` .
fn pump_until(compositor: &mut Compositor, mut done: impl FnMut(&Compositor) -> bool) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        if done(compositor) {
            return;
        }
    }
    panic!("the compositor never got there: {:?}", compositor.windows());
}

#[test]
fn each_workspace_gets_its_own_x_server_and_clipboard() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("headless");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 1280, 720))
        .expect("output");
    let displays = compositor.x11_displays().expect("displays");
    let mine = displays.display_for("workspace-one").expect("a display");
    let theirs = displays.display_for("workspace-two").expect("a display");
    assert_ne!(mine, theirs, "two workspaces, two displays");
    assert_eq!(
        displays.display_for("workspace-one"),
        Some(mine),
        "a workspace keeps its display while it runs apps"
    );

    let (one, one_root) = x11_client(&mut compositor, mine);
    let (two, two_root) = x11_client(&mut compositor, theirs);
    let window = client_window(&one, one_root, "xterm", "XTerm");
    one.map_window(window).expect("map");
    one.flush().expect("flush");
    pump_until(&mut compositor, |compositor| {
        compositor.windows().len() == 1
    });
    let (info, _) = compositor.windows().remove(0);
    assert_eq!(info.app_id.as_deref(), Some("XTerm"));

    // The other workspace's X server has never heard of that window.
    let children = two
        .query_tree(two_root)
        .expect("tree")
        .reply()
        .expect("tree reply")
        .children;
    assert!(
        !children.contains(&window),
        "a window of one workspace is not on another's X server: {children:?}"
    );

    // A copy in one workspace is only a copy in that workspace.
    two.flush().expect("flush");
    assert_eq!(
        two.get_selection_owner(client_atom(&two, "CLIPBOARD"))
            .expect("owner")
            .reply()
            .expect("owner reply")
            .owner,
        x11rb::NONE,
        "the other workspace's clipboard is empty"
    );
    one.set_selection_owner(window, client_atom(&one, "CLIPBOARD"), x11rb::CURRENT_TIME)
        .expect("owner");
    one.flush().expect("flush");
    assert_eq!(
        one.get_selection_owner(client_atom(&one, "CLIPBOARD"))
            .expect("owner")
            .reply()
            .expect("owner reply")
            .owner,
        window,
        "its own workspace's clipboard is the app's"
    );
    two.flush().expect("flush");
    assert_eq!(
        two.get_selection_owner(client_atom(&two, "CLIPBOARD"))
            .expect("owner")
            .reply()
            .expect("owner reply")
            .owner,
        x11rb::NONE,
        "and the copy does not reach the other workspace"
    );
}

#[test]
fn a_workspace_keeps_the_server_of_a_client_with_no_window_while_it_lives() {
    // A clipboard owner or a helper connects and never maps a window. It needs its display while it
    // lives, and its display's server follows it out: what keeps a windowless server running is its
    // clients, and the session's own connections are not ones.
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("headless");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 1280, 720))
        .expect("output");
    let displays = compositor.x11_displays().expect("displays");
    let number = displays.display_for("workspace").expect("a display");

    let (x11, _root) = x11_client(&mut compositor, number);
    assert_eq!(
        displays.running(),
        vec![number],
        "the windowless app's connection starts its workspace's server"
    );
    let holding = Instant::now();
    while holding.elapsed() < Duration::from_secs(6) {
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        assert_eq!(
            displays.running(),
            vec![number],
            "no window yet, but the app is on the display"
        );
    }

    drop(x11);
    pump_until(&mut compositor, |_| displays.running().is_empty());
    assert!(
        displays.running().is_empty(),
        "no X server runs once its last app client is gone"
    );
}

#[test]
fn a_workspace_runs_its_x_server_only_while_its_apps_run() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("headless");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 1280, 720))
        .expect("output");
    let displays = compositor.x11_displays().expect("displays");
    let number = displays.display_for("workspace").expect("a display");
    assert!(
        displays.running().is_empty(),
        "no X server runs before an X11 app opens"
    );

    let (x11, root) = x11_client(&mut compositor, number);
    assert_eq!(
        displays.running(),
        vec![number],
        "the app's connection starts its workspace's server"
    );
    let window = client_window(&x11, root, "xterm", "XTerm");
    x11.map_window(window).expect("map");
    x11.flush().expect("flush");
    pump_until(&mut compositor, |compositor| {
        compositor.windows().len() == 1
    });

    // The app lets its window go: the server follows shortly, its apps gone.
    x11.destroy_window(window).expect("destroy");
    x11.flush().expect("flush");
    pump_until(&mut compositor, |compositor| {
        compositor.windows().is_empty()
    });
    pump_until(&mut compositor, |_| displays.running().is_empty());
    assert!(
        displays.running().is_empty(),
        "no X server runs with no window of the workspace's apps"
    );

    // The display stays: the workspace's next app finds the same number, and a fresh server behind
    // it.
    assert_eq!(
        compositor
            .x11_displays()
            .expect("displays")
            .display_for("workspace"),
        Some(number),
        "the workspace's display outlived its server"
    );
    // A server on its way out still holds the display's socket for a moment and answers a client
    // that connects right then; the next one reaches the fresh server.
    let started = Instant::now();
    loop {
        let (_again, _root) = x11_client(&mut compositor, number);
        if displays.running() == vec![number] {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the workspace's next app never got its server"
        );
    }
}
