//! The lock screen is an ordinary Wayland client of the session: its
//! process asks the compositor for the session's lock, the compositor shows
//! that client's surface on every output and gives it every input, and the
//! lock ends when its client goes. What a window behind the lock shows is
//! the compositor's own test (its `headless` suite); this one is about the
//! process a session starts.

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
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tilo_compositor::{Compositor, InputEvent, OutputConfig, Screenshot, StackPolicy};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;
/// How long the lock screen may take to show here.
const OPENING: Duration = Duration::from_secs(60);
/// One display's refresh.
const REFRESH: Duration = Duration::from_millis(16);
/// `A`, the first key of a password.
const KEY_A: u32 = 30;

#[test]
fn the_lock_screen_shows_on_the_output_when_its_process_asks_for_the_lock() {
    let (mut compositor, mut screen, _runtime) = open();
    compositor.render().expect("render");
    let desktop = compositor.screenshot("HEADLESS-1").expect("screenshot");

    // The lock screen's process asked for the session's lock: from here on
    // the session shows what the lock client draws, and nothing else.
    wait_for(
        &mut compositor,
        &mut screen,
        "the session is locked",
        |session| session.locked(),
    );

    // The lock screen's own frame, drawn by its process and composited like
    // any client's. The session has nothing else on the output, so what the
    // frame gains over the empty desktop is what the lock screen drew.
    let locked = wait_for_frame(&mut compositor, &mut screen, &desktop);
    save(&locked, "lock");

    // Input is the lock screen's: a key reaches the password field it shows,
    // and what it draws changes.
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: true,
        })
        .expect("key down");
    compositor
        .input(InputEvent::Key {
            keycode: KEY_A,
            pressed: false,
        })
        .expect("key up");
    let typed = wait_for_frame(&mut compositor, &mut screen, &locked);
    save(&typed, "lock-typed");

    // The lock ends with its client: the desktop shows again, exactly as it
    // was before the lock.
    screen.kill().expect("stop the lock screen");
    screen.wait().expect("the lock screen is gone");
    wait_for(
        &mut compositor,
        &mut screen,
        "the lock is lifted",
        |session| !session.locked(),
    );
    let after = compositor.screenshot("HEADLESS-1").expect("screenshot");
    assert_eq!(
        after.rgba, desktop.rgba,
        "the desktop shows once the lock screen is gone"
    );
}

/// The frame the session shows once it differs from `was`: the lock screen
/// drew, and its client's pixels are on the output.
fn wait_for_frame(compositor: &mut Compositor, screen: &mut Child, was: &Screenshot) -> Screenshot {
    let deadline = Instant::now() + OPENING;
    loop {
        let frame = compositor.screenshot("HEADLESS-1").expect("screenshot");
        if frame.rgba != was.rgba {
            return frame;
        }
        if Instant::now() >= deadline {
            panic!(
                "the lock screen never drew (locked: {})\n{}",
                compositor.locked(),
                stderr_of(screen)
            );
        }
        pumped(compositor, screen, "the lock screen drew");
    }
}

/// Wait for the session to be in the state `done` asks for, dispatching to
/// its clients and drawing what they drew meanwhile.
fn wait_for(
    compositor: &mut Compositor,
    screen: &mut Child,
    what: &str,
    done: impl Fn(&Compositor) -> bool,
) {
    let deadline = Instant::now() + OPENING;
    while !done(compositor) {
        assert!(Instant::now() < deadline, "the session never was {what}");
        pumped(compositor, screen, what);
    }
}

/// One round of the display's loop, unless the lock screen is gone.
fn pumped(compositor: &mut Compositor, screen: &mut Child, what: &str) {
    if let Some(exited) = screen.try_wait().expect("wait") {
        if exited.success() {
            return;
        }
        panic!(
            "the lock screen exited before {what}: {exited}\n{}",
            stderr_of(screen)
        );
    }
    compositor.dispatch(REFRESH).expect("dispatch");
    compositor.render().expect("render");
}

/// Write `frame` where a person can look at it, when the run asked for
/// shots: `TILO_SURFACE_SHOTS` names the directory.
fn save(frame: &Screenshot, name: &str) {
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    let Some(pixels) = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone())
    else {
        panic!("the frame's pixels do not fill it");
    };
    pixels
        .save_with_format(
            directory.join(format!("{name}.png")),
            image::ImageFormat::Png,
        )
        .expect("the shot");
}

/// What the lock screen said so far: of a run that failed, what it said
/// before it ended.
fn stderr_of(screen: &mut Child) -> String {
    let mut said = String::new();
    if let Some(mut stderr) = screen.stderr.take() {
        if screen.try_wait().ok().flatten().is_none() {
            screen.kill().expect("stop the lock screen");
            screen.wait().expect("the lock screen is gone");
        }
        let _ = std::io::Read::read_to_string(&mut stderr, &mut said);
    }
    said
}

/// The session, and the lock screen connected to it as a trusted client on
/// a socket of its own.
fn open() -> (Compositor, Child, tempfile::TempDir) {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new(
            "HEADLESS-1",
            i32::try_from(WIDTH).unwrap(),
            i32::try_from(HEIGHT).unwrap(),
        ))
        .expect("output");

    // The socket the lock screen speaks on: the session trusts the client it
    // hands it, and the process inherits it as `WAYLAND_SOCKET`.
    let client = compositor.connect_trusted_client().expect("client socket");
    let client_fd = client.as_raw_fd();
    let runtime = tempfile::tempdir().expect("runtime directory");
    let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-lock-screen"));
    command
        // Its window is the session's lock: on every output, taking every
        // input, showing who the session locked for.
        .env("TILO_SURFACE_LOCK", "1")
        .env("TILO_SURFACE_PERSON", "Ada Lovelace")
        // Nothing of this desktop: no X display, and a runtime directory of
        // its own.
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env_remove("DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The lock screen's end of the socket becomes fd 3 in its process; the
    // compositor answers on the other end here.
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
    let screen = command.spawn().expect("lock screen process");
    // The child has its own copy: this one would only keep the connection's
    // other end open after the lock screen is gone.
    drop(client);
    (compositor, screen, runtime)
}
