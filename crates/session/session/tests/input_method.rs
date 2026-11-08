//! An input-method source in the person's keyboard list: switching to it and back is a source
//! switch like any other, and the seat keeps typing in the layout under it — an engine is not a
//! layout, so it never reaches the keymap the compositor compiles.
//! The engine itself (Fcitx5 with Rime, Anthy or Hangul) is started by the session on a display of
//! its own when the image has it; what it composes is proven against real clients speaking
//! `input-method-v2` , `text-input-v3` and `virtual-keyboard-v1` in the compositor's headless
//! tests.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use tilo_compositor::OutputConfig as CompositorOutput;
use tilo_session::{Options, Session, SessionConfig};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The layout the person types with while the engine source does not compose.
const LAYOUT: Option<&str> = Some("English (US)");

/// A session with one display, reading the sources the person kept.
fn session() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            config: SessionConfig::load(),
            ..Options::default()
        },
        &[CompositorOutput::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session")
}

/// Step the session until `done` .
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    let at = std::panic::Location::caller();
    common::wait_for(session, format!("the condition at {at}"), |session| {
        done(session).then_some(())
    });
}

/// The person's sources as Settings keeps them: a layout, then Hangul.
fn keep_sources() {
    settings_store::update_section(
        settings_store::hardware::INPUT_SECTION,
        serde_json::Map::from_iter([
            (
                "keyboard_layout".to_owned(),
                serde_json::Value::String("us".into()),
            ),
            (
                "keyboard_layouts".to_owned(),
                serde_json::json!(["us", "ime/hangul"]),
            ),
        ]),
    )
    .expect("the person's sources are kept");
}

#[test]
fn an_input_method_source_types_with_the_layout_under_it() {
    keep_sources();
    let mut session = session();
    assert_eq!(
        session.input_sources().to_vec(),
        ["us", "ime/hangul"],
        "the source list keeps the engine"
    );
    assert_eq!(session.active_input_source(), "us");
    assert_eq!(session.compositor().keyboard_layout().as_deref(), LAYOUT);

    session
        .invoke("session:input", "next-source", &serde_json::Map::new())
        .expect("the session publishes the source switch");
    until(&mut session, |session| {
        session.active_input_source() == "ime/hangul"
    });
    assert_eq!(
        session.compositor().keyboard_layout().as_deref(),
        LAYOUT,
        "the engine composes; the keys it lets through still type this layout"
    );

    session
        .invoke("session:input", "next-source", &serde_json::Map::new())
        .expect("the session publishes the source switch");
    until(&mut session, |session| {
        session.active_input_source() == "us"
    });
    assert_eq!(
        session.compositor().keyboard_layout().as_deref(),
        LAYOUT,
        "back on the layout, the seat types it"
    );
}
