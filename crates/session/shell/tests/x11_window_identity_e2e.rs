//! An X11 window is traced to the process that owns it and keeps its own
//! icon.

use super::*;

/// An X11 window reaches Tilo through satellite's connection, so the
/// compositor asks the X server which process owns it (X-Resource, which no
/// client can fake, unlike `_NET_WM_PID`) and takes its `_NET_WM_ICON` as
/// the window's own icon.
#[test]
fn an_x11_window_is_traced_to_its_process_and_keeps_its_own_icon() {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, CreateGCAux, CreateWindowAux, PropMode, Rectangle,
        WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as _;
    assert!(
        Path::new("/usr/bin/xwayland-satellite").is_file(),
        "xwayland-satellite is not installed, so no X11 window can be traced"
    );
    let compositor = Compositor::start_host_desktop();
    let mut native = TestClient::new(compositor.connect(), "org.tilo.test.x11-traced-native");
    native.map_window(&compositor);
    let conn = x11_connect(&compositor);
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
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"x11-traced\0X11Traced\0",
    )
    .unwrap();
    // A 16-pixel opaque blue square, and a non-square image that is skipped.
    let mut icon = vec![3_u32, 2];
    icon.extend([0_u32; 6]);
    icon.extend([16_u32, 16]);
    icon.extend(std::iter::repeat_n(0xff_2a_3f_d1_u32, 256));
    let net_wm_icon = conn
        .intern_atom(false, b"_NET_WM_ICON")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_icon,
        AtomEnum::CARDINAL,
        &icon,
    )
    .unwrap();
    conn.map_window(window).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(
        gc,
        window,
        &CreateGCAux::new().foreground(screen.white_pixel),
    )
    .unwrap();
    conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        }],
    )
    .unwrap();
    conn.flush().unwrap();
    // This test process owns the window: the X server says so.
    compositor.wait_for_log(
        &[&format!(
            "traced an X11 window to its process Some({}) with 1 icon sizes",
            std::process::id()
        )],
        "the X11 window was never traced to this process with its icon",
    );
}
