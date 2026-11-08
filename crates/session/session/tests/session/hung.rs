//! An app that stops answering: its window says so over the desk, the desk offers to wait for it or
//! to quit it, and the person's choice reaches the compositor. The client that stops answering is a
//! real one, and the desk is the session's own.

use super::*;
use serde_json::json;
use tilo_desktop::desk_verbs::DESK_RESOURCE;
use tilo_surface_host::shell_part::DESK_RESOURCE as DESK_READING;

/// The app that stops answering, and one that keeps working.
const STUCK: &str = "org.example.Stuck";
const OTHER: &str = "org.example.Other";

/// `ms` from now, when the compositor next judges its pings.
fn later(ms: u64) -> Instant {
    Instant::now()
        .checked_add(Duration::from_millis(ms))
        .expect("in range")
}

/// Click the middle of the window at `rect` : the app given input is the app the compositor asks to
/// answer.
fn click(session: &mut Session, rect: (i32, i32, i32, i32)) {
    let (x, y, width, height) = rect;
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(x + width / 2),
            y: f64::from(y + height / 2),
        })
        .expect("the pointer moves");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the pointer presses");
    }
}

/// The desk's reading of its windows, as the overview and the bar read them.
fn windows_reading(session: &Session) -> Vec<serde_json::Value> {
    session
        .read(DESK_READING)
        .unwrap_or_else(|| panic!("the session publishes its desk"))
        .get("windows")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .expect("the desk reads its windows")
}

/// The windows the desk says stopped answering.
fn not_responding(session: &Session) -> Vec<String> {
    windows_reading(session)
        .into_iter()
        .filter(|window| window["not_responding"] == json!(true))
        .filter_map(|window| window["title"].as_str().map(str::to_owned))
        .collect()
}

/// Run frames until `done` holds, with the other app kept answered.
fn until_answered(
    session: &mut Session,
    other: &mut TestClient,
    what: &str,
    mut done: impl FnMut(&mut Session) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        other.exchange();
        session.step(Duration::from_millis(2)).expect("step");
        other.exchange();
        if done(session) {
            return;
        }
        let refusing = session.compositor().not_responding_windows().to_vec();
        let desk = session.handle().unresponsive_tabs();
        assert!(
            Instant::now() < deadline,
            "condition not reached: {what}; the compositor says {refusing:?}, the desk says {desk:?}"
        );
    }
}

#[test]
fn a_window_whose_app_stopped_answering_says_so_and_the_person_can_wait_or_quit_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let mut other = mapped(&mut session, OTHER);
    let mut stuck = mapped(&mut session, STUCK);
    let (stuck_window, stuck_rect) = window(session.compositor(), STUCK).expect("its window");
    let (other_window, _) = window(session.compositor(), OTHER).expect("the other's window");

    // The person presses the app: it is asked to answer and does not.
    click(&mut session, stuck_rect);
    assert!(session.compositor().advance_app_liveness_at(later(6_000)));
    until_answered(
        &mut session,
        &mut other,
        "the desk says a window stopped answering",
        |session| !session.handle().unresponsive_tabs().is_empty(),
    );

    assert_eq!(
        session.compositor().not_responding_windows(),
        [stuck_window]
    );
    let tabs = session.handle().unresponsive_tabs();
    assert_eq!(tabs.len(), 1, "one window stopped answering");
    let tab = tabs[0];
    assert_eq!(session.handle().window_of_tab(tab), Some(stuck_window));
    assert_eq!(
        not_responding(&session).len(),
        1,
        "the reading every view of the desk reads says so: {:?}",
        session.read(DESK_READING)
    );

    // Wait: the app is given another period, and the card goes.
    invoke_with(
        &mut session,
        DESK_RESOURCE,
        "wait-window",
        &json!({ "tab": tab.to_string() }),
    );
    until_answered(
        &mut session,
        &mut other,
        "waiting clears the card",
        |session| session.handle().unresponsive_tabs().is_empty(),
    );
    assert!(session.compositor().not_responding_windows().is_empty());
    assert!(not_responding(&session).is_empty());

    // Still silent after another full period: it says so again.
    assert!(session.compositor().advance_app_liveness_at(later(6_000)));
    until_answered(&mut session, &mut other, "it says so again", |session| {
        !session.handle().unresponsive_tabs().is_empty()
    });
    assert_eq!(not_responding(&session).len(), 1);

    // Force quit ends that app alone.
    invoke_with(
        &mut session,
        DESK_RESOURCE,
        "force-quit-app",
        &json!({ "tab": tab.to_string() }),
    );
    // The compositor ends that app's connection alone, and the desk says nothing more of it. The
    // app reads the error and its window goes, as a real app's event loop would.
    assert!(session.compositor().not_responding_windows().is_empty());
    stuck.exchange();
    until_answered(
        &mut session,
        &mut other,
        "the quit window is gone",
        |session| window(session.compositor(), STUCK).is_none(),
    );
    assert_eq!(
        window(session.compositor(), OTHER).map(|(id, _)| id),
        Some(other_window),
        "the app that kept answering is untouched"
    );
    assert!(session.compositor().not_responding_windows().is_empty());
    assert!(not_responding(&session).is_empty());
    assert!(
        !session
            .handle()
            .read(|tree| tree
                .workspaces
                .iter()
                .any(|workspace| workspace.strips.iter().any(|strip| strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|shown| shown.id == tab))))),
        "the window it quit is gone from the desk"
    );
}
