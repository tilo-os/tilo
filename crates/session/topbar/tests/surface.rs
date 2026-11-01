//! The top bar is an ordinary Wayland client of the session: it opens its
//! window on the layer the session names, the compositor places it across
//! the top of the output and keeps the windows out of the band it reserves,
//! and what it draws is the app's own frame, composited like any client's.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a failed step in a test is the assertion"
)]

use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tilo_compositor::{Compositor, OutputConfig, StackPolicy};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
/// The height the session tells the bar to take.
const BAR: u32 = 32;
/// How long the bar may take to show its surface here.
const OPENING: Duration = Duration::from_secs(90);
/// One display's refresh: how often a desktop asks its clients for their next
/// frame.
const REFRESH: Duration = Duration::from_millis(16);

#[test]
fn the_bar_shows_on_the_layer_the_session_names() {
    let (mut compositor, mut bar, _runtime) = open();

    // The bar is up where the session put it, and the compositor knows the
    // band it keeps the windows out of.
    let deadline = Instant::now() + OPENING;
    while reserved(&compositor).is_empty() {
        if let Some(exited) = bar.try_wait().expect("wait") {
            panic!(
                "the bar exited before it showed its surface: {exited}\n{}",
                stderr_of(&mut bar)
            );
        }
        assert!(
            Instant::now() < deadline,
            "the bar showed nothing here (headless compositor without a renderer?)"
        );
        compositor.dispatch(REFRESH).expect("dispatch to the bar");
        compositor.render().expect("render");
    }
    assert_eq!(
        reserved(&compositor),
        vec![(
            0,
            0,
            i32::try_from(WIDTH).unwrap(),
            i32::try_from(BAR).unwrap()
        )],
        "the bar reserves the top of the output for itself"
    );

    // What it drew is its own frame, composited by the session like any
    // client's: the bar across the band it took, and what lies below it
    // untouched by it.
    let frame = compositor.screenshot("HEADLESS-1").expect("screenshot");
    let bar_pixel = frame.pixel(WIDTH / 2, BAR / 2).expect("a pixel of the bar");
    let below = frame
        .pixel(WIDTH / 2, HEIGHT / 2)
        .expect("a pixel below the bar");
    assert_ne!(bar_pixel, below, "the bar draws over what is below it");
    assert_eq!(
        frame
            .pixel(WIDTH / 2, BAR)
            .expect("the pixel under the bar"),
        below,
        "the bar takes exactly the band it reserved"
    );

    bar.kill().expect("stop the bar");
    bar.wait().expect("the bar is gone");
}

/// What the bar said on its way out.
fn stderr_of(bar: &mut Child) -> String {
    let mut said = String::new();
    if let Some(mut stderr) = bar.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut stderr, &mut said);
    }
    said
}

/// The rects the layer surfaces reserve on the output.
fn reserved(compositor: &Compositor) -> Vec<(i32, i32, i32, i32)> {
    compositor.reserved("HEADLESS-1")
}

/// The session, and the bar connected to it as a trusted client on a socket
/// of its own.
fn open() -> (Compositor, Child, tempfile::TempDir) {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new(
            "HEADLESS-1",
            i32::try_from(WIDTH).unwrap(),
            i32::try_from(HEIGHT).unwrap(),
        ))
        .expect("output");

    // The socket the bar speaks on: the session trusts the client it hands
    // it, and the process inherits it as `WAYLAND_SOCKET`.
    let client = compositor.connect_trusted_client().expect("client socket");
    let client_fd = client.as_raw_fd();
    let runtime = tempfile::tempdir().expect("runtime directory");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-topbar"));
    command
        // The bar places itself on the top layer of the output, across the
        // top, as tall as the session's bar and reserving that band.
        .env("TILO_SURFACE_LAYER", "top")
        .env("TILO_SURFACE_NAMESPACE", "tilo-bar")
        .env("TILO_SURFACE_ANCHOR", "top,left,right")
        .env("TILO_SURFACE_SIZE", format!("0x{BAR}"))
        .env("TILO_SURFACE_EXCLUSIVE", BAR.to_string())
        .env("TILO_SURFACE_KEYBOARD", "none")
        // Nothing of this desktop: no X display, and a runtime directory of
        // its own.
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env_remove("DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The bar's end of the socket becomes fd 3 in its process; the compositor
    // answers on the other end here.
    unsafe {
        command.pre_exec(move || {
            // Safety: `dup2` onto a fixed descriptor in the child, before it
            // runs anything of the app.
            if libc::dup2(client_fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.env("WAYLAND_SOCKET", "3");
    let bar = command.spawn().expect("bar process");
    // The child has its own copy: this one would only keep the connection's
    // other end open after the bar is gone.
    drop(client);
    (compositor, bar, runtime)
}
