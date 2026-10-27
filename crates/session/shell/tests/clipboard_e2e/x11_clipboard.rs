//! The X11 side of the clipboard suite: an X11 program (the Chromium
//! flatpak runs under Xwayland) owning the CLIPBOARD, a second X11 program
//! pasting it through the X server, and an X11 window reached through
//! satellite's connection so the compositor can put the copy on the Tilo
//! seat.
//!
//! These are the suite's fixtures, not its expectations: each test stays in
//! `clipboard_e2e` and says what the compositor must do.

use super::*;

/// An X11 program (the Chromium flatpak runs under Xwayland) owning the
/// CLIPBOARD: serves `TARGETS` and the text targets from a thread until
/// dropped.
pub(super) struct X11ClipboardOwner {
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// Set when the X server told the owner another client took the selection.
    cleared: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl X11ClipboardOwner {
    /// Wait until the compositor has taken the copy over from this owner:
    /// the X server clears its ownership when the selection moves on.
    pub(super) fn wait_until_taken_over(&self) {
        tilo_test_env::wait_until("the compositor to take the X11 copy over", || {
            self.cleared.load(std::sync::atomic::Ordering::SeqCst)
        });
    }
}

impl Drop for X11ClipboardOwner {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Connect to the workspace's private X display.
pub(super) fn x11_connect(compositor: &Compositor) -> x11rb::rust_connection::RustConnection {
    let (display, authority) = private_xwayland_connection(compositor).expect("private display");
    let number = display.trim_start_matches(':').to_string();
    let cookie_file = std::fs::read(&authority).expect("read Xauthority");
    let cookie = cookie_file[cookie_file.len().saturating_sub(16)..].to_vec();
    let stream = UnixStream::connect(format!("/tmp/.X11-unix/X{number}")).expect("X socket");
    let (stream, _) = x11rb::rust_connection::DefaultStream::from_unix_stream(stream).unwrap();
    x11rb::rust_connection::RustConnection::connect_to_stream_with_auth_info(
        stream,
        0,
        b"MIT-MAGIC-COOKIE-1".to_vec(),
        cookie,
    )
    .expect("X handshake")
}

/// Paste the X11 CLIPBOARD as UTF-8 text, the way a second X11 program
/// (another browser tab process, say) does: through the X server.
pub(super) fn x11_paste(compositor: &Compositor) -> String {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, WindowClass};
    let conn = x11_connect(compositor);
    let screen = &conn.setup().roots[0];
    let window = conn.generate_id().unwrap();
    conn.create_window(
        0,
        window,
        screen.root,
        0,
        0,
        10,
        10,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
    let (clipboard, utf8, prop) = (
        atom(b"CLIPBOARD"),
        atom(b"UTF8_STRING"),
        atom(b"TILO_PASTE"),
    );
    conn.convert_selection(window, clipboard, utf8, prop, x11rb::CURRENT_TIME)
        .unwrap();
    conn.flush().unwrap();
    let end = Instant::now()
        .checked_add(deadline())
        .expect("a representable deadline");
    loop {
        assert!(
            Instant::now() < end,
            "the X11 paste never got a SelectionNotify\nselection/X11 log lines:\n{}",
            compositor
                .full_log()
                .lines()
                .filter(|l| {
                    let l = l.to_ascii_lowercase();
                    l.contains("selection") || l.contains("satellite") || l.contains("x11")
                })
                .collect::<Vec<_>>()
                .join("\n")
        );
        let Some(event) = conn.poll_for_event().unwrap() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        assert!(
            !matches!(event, Event::Error(_)),
            "the X server rejected a paste request: {event:?}"
        );
        if let Event::SelectionNotify(notify) = event {
            assert_ne!(
                notify.property,
                0,
                "the X11 paste was refused (nothing copied)\nselection/X11 log lines:\n{}",
                compositor
                    .full_log()
                    .lines()
                    .filter(|l| {
                        let l = l.to_ascii_lowercase();
                        l.contains("selection") || l.contains("satellite") || l.contains("x11")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let reply = conn
                .get_property(true, window, prop, AtomEnum::ANY, 0, u32::MAX / 4)
                .unwrap()
                .reply()
                .unwrap();
            return String::from_utf8(reply.value).expect("utf-8 paste");
        }
    }
}

/// Connect to the workspace's private X display, map a window carrying the
/// launch activation token (so Tilo focuses it like any launched app) and wait
/// until it has keyboard focus. Returns the connection plus its window.
pub(super) fn x11_focused_window(
    compositor: &Compositor,
    native: &mut TestClient,
    token: &str,
) -> (x11rb::rust_connection::RustConnection, u32) {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as _;
    let conn = x11_connect(compositor);
    let screen = &conn.setup().roots[0];
    let window = conn.generate_id().unwrap();
    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        200,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
    let utf8 = atom(b"UTF8_STRING");
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        utf8,
        b"x11 copier",
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        atom(b"_NET_STARTUP_ID"),
        utf8,
        token.as_bytes(),
    )
    .unwrap();
    // Ask for FocusIn: Xwayland sends it once satellite holds Wayland keyboard
    // focus, which is what lets satellite make a copy the selection owner. A
    // program copies after it has focus, so the test must not copy before.
    conn.change_window_attributes(
        window,
        &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
            .event_mask(x11rb::protocol::xproto::EventMask::FOCUS_CHANGE),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.flush().unwrap();
    native.wait_until(
        |state| !state.keyboard_focused,
        "the X11 copier window never took keyboard focus",
    );
    // Paint something: a real program draws, and satellite only attaches a
    // buffer (which is what lets Tilo show and focus the window) once the
    // window has content.
    let gc = conn.generate_id().unwrap();
    conn.create_gc(
        gc,
        window,
        &x11rb::protocol::xproto::CreateGCAux::new().foreground(screen.white_pixel),
    )
    .unwrap();
    let paint = |conn: &x11rb::rust_connection::RustConnection| {
        conn.poly_fill_rectangle(
            window,
            gc,
            &[x11rb::protocol::xproto::Rectangle {
                x: 0,
                y: 0,
                width: 200,
                height: 100,
            }],
        )
        .unwrap();
        conn.flush().unwrap();
    };
    paint(&conn);
    // Wait for Wayland keyboard focus on satellite's surface for this window
    // and for it to hold: a window that has just opened is resized to its tab,
    // which briefly unmaps it, and a program copies once its window has
    // settled, not mid-resize. The X server reports the window as the input
    // focus once satellite holds Wayland focus.
    let probe = x11_connect(compositor);
    let end = deadline_end();
    let mut last_paint = Instant::now();
    loop {
        assert!(
            Instant::now() < end,
            "the X11 window never held keyboard focus: satellite never got (or kept) Wayland keyboard focus\n{}",
            compositor.recent_log()
        );
        if last_paint.elapsed() > Duration::from_millis(500) {
            paint(&conn);
            last_paint = Instant::now();
        }
        // Settled: focused, with the compositor no longer moving any window
        // into place, and still focused after that was read.
        let focused = || probe.get_input_focus().unwrap().reply().unwrap().focus == window;
        if focused() && !compositor.foreign_surface_animating() && focused() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (conn, window)
}

pub(super) fn serve_x11_clipboard(
    conn: x11rb::rust_connection::RustConnection,
    window: u32,
    text: String,
) -> X11ClipboardOwner {
    serve_x11_clipboard_gated(conn, window, text, |_| {})
}

pub(super) fn serve_x11_clipboard_gated(
    conn: x11rb::rust_connection::RustConnection,
    window: u32,
    text: String,
    mut gate: impl FnMut(usize) + Send + 'static,
) -> X11ClipboardOwner {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, EventMask, PropMode, SELECTION_NOTIFY_EVENT,
        SelectionNotifyEvent,
    };
    use x11rb::wrapper::ConnectionExt as _;
    let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
    let clipboard = atom(b"CLIPBOARD");
    let targets = atom(b"TARGETS");
    let utf8 = atom(b"UTF8_STRING");
    let mime = atom(b"text/plain;charset=utf-8");
    let timestamp = atom(b"TIMESTAMP");
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    conn.flush().unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let cleared = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lost = Arc::clone(&cleared);
    let thread = std::thread::spawn(move || {
        let mut payload_requests: usize = 0;
        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok(Some(event)) = conn.poll_for_event() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            if let Event::SelectionClear(clear) = &event
                && clear.selection == clipboard
            {
                lost.store(true, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            let Event::SelectionRequest(request) = event else {
                continue;
            };
            let property = if request.property == 0 {
                request.target
            } else {
                request.property
            };
            if request.target != targets {
                payload_requests = payload_requests.saturating_add(1);
                gate(payload_requests);
            }
            let served = if request.target == targets {
                conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &[targets, timestamp, utf8, AtomEnum::STRING.into(), mime],
                )
                .is_ok()
            } else if request.target == utf8 || request.target == mime {
                conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    utf8,
                    text.as_bytes(),
                )
                .is_ok()
            } else if request.target == u32::from(AtomEnum::STRING) {
                conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::STRING,
                    text.as_bytes(),
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
                property: if served { property } else { 0 },
            };
            let _ = conn.send_event(false, request.requestor, EventMask::NO_EVENT, notify);
            let _ = conn.flush();
        }
    });
    X11ClipboardOwner {
        stop,
        cleared,
        thread: Some(thread),
    }
}
