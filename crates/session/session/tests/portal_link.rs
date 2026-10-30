//! The portal's link to the session's loop, on the headless backend: the
//! screens an app may share are published, a stream the app starts is
//! answered from the compositor, the app drives the seat, shares the
//! selection, and input captures look at where the session's screens are.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::time::{Duration, Instant};

use tilo_compositor::{OutputConfig, Selection};
use tilo_portal::input_capture::InputCaptures;
use tilo_portal::remote_desktop::RemoteInput;
use tilo_portal::streams::{CaptureSource, SourceKind, StreamRequest};
use tilo_session::portal_link::PortalSide;
use tilo_session::{Options, Session};

const SETTLE: Duration = Duration::from_millis(10);

/// A headless session serving the portal, with the portal's side of it;
/// `captures` are the input captures the session feeds its input to.
fn serving(captures: Option<InputCaptures>) -> (Session, PortalSide) {
    let wake = tilo_service::LoopWake::default();
    let (side, link) = tilo_session::portal_link::channel(wake.handle());
    let link = match captures {
        Some(captures) => link.with_captures(captures),
        None => link,
    };
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            wake,
            portal: Some(link),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session");
    (session, side)
}

/// Step `session` until `probe` finds what it looks for, however many
/// passes that takes.
#[track_caller]
fn until<T>(session: &mut Session, mut probe: impl FnMut(&mut Session) -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(found) = probe(session) {
            return found;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the session never got there"
        );
        session.step(SETTLE).unwrap();
    }
}

/// Ask `ask` of the portal's side on a thread of its own, as the portal's
/// runtime does, and step the session until it is answered: the session
/// answers on its loop.
#[track_caller]
fn asked<T: Send + 'static>(
    session: &mut Session,
    ask: impl FnOnce() -> tilo_portal::Pending<T> + Send + 'static,
) -> T {
    let asking = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(ask())
    });
    let started = Instant::now();
    while !asking.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the session never answered"
        );
        session.step(SETTLE).unwrap();
    }
    asking.join().unwrap()
}

#[test]
fn the_screens_the_session_shows_are_published_as_sources() {
    let (mut session, side) = serving(None);
    assert!(
        side.streams().sources().is_empty(),
        "the session published nothing before it ran"
    );
    session.step(SETTLE).unwrap();
    let sources = side.streams().sources();
    assert_eq!(
        sources.len(),
        1,
        "the display the session shows is its source"
    );
    let source = &sources[0];
    assert_eq!(source.kind, SourceKind::Output);
    assert_eq!(source.id, "HEADLESS-1");
    assert_eq!(source.app_id, None);
    assert!(
        source.label.contains("HEADLESS-1"),
        "the source is named by the display: {:?}",
        source.label
    );
    assert_eq!(
        side.streams().cursor_modes(),
        tilo_portal::streams::cursor_mode::ALL
    );
}

#[test]
fn a_stream_of_a_screen_that_is_gone_is_refused_by_the_session() {
    let (mut session, side) = serving(None);
    session.step(SETTLE).unwrap();
    let streams = side.streams();
    let answer = asked(&mut session, move || {
        streams.start(StreamRequest {
            source: CaptureSource {
                kind: SourceKind::Output,
                id: "HDMI-A-9".to_owned(),
                app_id: None,
                label: "gone".to_owned(),
            },
            cursor_mode: tilo_portal::streams::cursor_mode::EMBEDDED,
            app_id: "org.example.Player".to_owned(),
        })
    });
    assert!(answer.is_err(), "a screen that is not there cannot stream");
}

#[test]
fn the_portals_pointer_moves_the_sessions_pointer() {
    let (mut session, side) = serving(None);
    assert!(
        !side.input().available(),
        "the session drives no seat before it runs"
    );
    session.step(SETTLE).unwrap();
    assert!(side.input().available());

    let before = session.compositor().pointer_location();
    let input = side.input();
    assert!(input.inject(RemoteInput::PointerMotion { dx: 40.0, dy: 25.0 }));
    // A point of a stream no one shares is taken as it is offered: the
    // absolute motion lands where the app said.
    input.inject(RemoteInput::PointerMotionAbsolute {
        stream: 0,
        x: 300.0,
        y: 200.0,
    });
    let seen = until(&mut session, |session| {
        let seen = session.compositor().pointer_location();
        Some(seen).filter(|seen| *seen != before)
    });
    assert_eq!(seen, (300.0, 200.0), "{before:?} -> {seen:?}");
}

#[test]
fn the_portals_clipboard_is_the_desktops_selection() {
    let (mut session, side) = serving(None);
    session.step(SETTLE).unwrap();
    let clipboard = side.clipboard();
    assert!(clipboard.available());

    // What the app copies is what the desktop holds.
    clipboard
        .write("text/plain", b"from the portal".to_vec())
        .expect("the app writes its selection");
    until(&mut session, |session| {
        let mimes = session.compositor().selection_formats(Selection::Clipboard);
        mimes
            .iter()
            .any(|mime| mime == "text/plain")
            .then_some(mimes)
    });

    // And the desktop's bytes are what the next app reads.
    let read = clipboard.clone();
    let bytes = asked(&mut session, move || read.read("text/plain"));
    assert_eq!(bytes.expect("the selection"), b"from the portal");

    // Clearing it leaves the desktop holding nothing.
    clipboard.clear();
    until(&mut session, |session| {
        session
            .compositor()
            .selection_formats(Selection::Clipboard)
            .is_empty()
            .then_some(())
    });
}

#[test]
fn the_sessions_screens_are_where_input_captures_look() {
    let captures = InputCaptures::default();
    assert!(!captures.available());
    let (mut session, _side) = serving(Some(captures.clone()));
    until(&mut session, |_| captures.available().then_some(()));
}
