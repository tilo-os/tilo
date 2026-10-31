//! A question the session puts to the person about themselves: it waits on
//! their own avatar in the top bar, never as a banner over their windows,
//! and their answer takes it away.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_desktop_surface::{HostAction, HostActionSource};
use tilo_session::{Options, Session};

const SETTLE: Duration = Duration::from_millis(1);
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// A headless session at its desk, drawn once.
fn session() -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(SETTLE).expect("a frame");
    session
}

/// Keeping who the person is here: the offer a page makes from their own
/// avatar.
fn keep_identity() -> HostAction {
    HostAction {
        id: "keep-identity".to_owned(),
        title: "Keep with passkey".to_owned(),
        subtitle: None,
        source: Some(HostActionSource::Identity),
        label: None,
    }
}

#[test]
fn an_ask_about_the_person_waits_on_their_avatar_until_they_answer_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let quiet = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");

    tilo_session::identity::offer(
        session.handle(),
        &session.notifications(),
        keep_identity(),
        "Keep your name and work?",
        "A passkey brings you back as yourself on this device.",
    );
    session.step(SETTLE).expect("a frame");

    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("Keep your name and work?")),
        "the ask waits on the avatar: {:?}",
        session.accessible_labels()
    );
    let asking = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    assert_ne!(
        asking.rgba, quiet.rgba,
        "the bar draws the ask on the person's own avatar"
    );
    assert!(
        session
            .notifications()
            .log()
            .active()
            .iter()
            .all(|notice| notice
                .actions
                .iter()
                .all(|(key, _)| key != "host:keep-identity")),
        "an ask about the person is not a banner too"
    );

    // The person answers it on their avatar: the bar's answer asks the desk
    // exactly this.
    session.handle().answer_personal_ask();
    session.step(SETTLE).expect("a frame");
    session.step(SETTLE).expect("a frame");

    assert!(
        session.handle().personal_ask().is_none(),
        "answered, the ask goes"
    );
    assert!(
        session
            .accessible_labels()
            .iter()
            .all(|label| !label.contains("Keep your name and work?")),
        "the bar no longer asks: {:?}",
        session.accessible_labels()
    );
    let answered = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    assert_ne!(
        answered.rgba, asking.rgba,
        "the bar draws no ask once it is answered"
    );
    assert!(
        session.handle().take_host_choices().is_empty(),
        "the session performed the answer rather than leaving it in the desk"
    );
}
