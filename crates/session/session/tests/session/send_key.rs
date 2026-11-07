//! The shell's send key asks the window the person works in to open its box:
//! the app draws it alone, and a desk with nothing in front still opens Chat.

use std::sync::Arc;
use std::time::Duration;

use tilo_test_env::SpawnUnderEnvLock as _;
use ui::floating_surface::{FloatingSurfaceClientMessage, FloatingSurfaceHostMessage};

use super::window_warmth::Window;
use super::*;

/// The app a window of this test runs: no app of this package, so nothing but
/// the session's own answers replies on its connection.
const APP: &str = "org.example.Notes";

/// A session with a window of [`APP`] in front, and that window's own
/// shell-ui connection.
fn session_with_a_window() -> (
    Session,
    Arc<tilo_session::Launches>,
    Window,
    std::process::Child,
) {
    let launches = Arc::new(tilo_session::Launches::default());
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            launches: Arc::clone(&launches),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a frame");
    let app_process = std::process::Command::new("sleep")
        .arg("120")
        .spawn_locked()
        .expect("a launched app's process");
    launches.started(app_process.id(), APP, "private", false);
    let _client = mapped(&mut session, APP);
    let mut window = Window::open(&launches, APP, "private", app_process.id());
    window.hello();
    window.say(&FloatingSurfaceClientMessage::SelectionCapable);
    // The pass that takes the window's word answers its hello, so the box is
    // asked for after the window has heard of the session at all.
    for _ in 0..200 {
        session.step(Duration::from_millis(2)).expect("step");
        if window.next().is_some() {
            break;
        }
    }
    (session, launches, window, app_process)
}

/// Press the send key over the window in front.
fn press_send_key(session: &mut Session) {
    key(session, keys::LEFT_META, true);
    tap(session, keys::C);
    key(session, keys::LEFT_META, false);
}

/// Whether the session asks the window to send within a few passes, whatever
/// else it tells the window meanwhile.
fn asked_to_send(app: &mut Window, session: &mut Session) -> bool {
    for _ in 0..200 {
        session.step(Duration::from_millis(2)).expect("step");
        while let Some(message) = app.next() {
            if matches!(message, FloatingSurfaceHostMessage::ShareSelection) {
                return true;
            }
        }
    }
    false
}

/// The send key over a window that opens a box asks that window, and opens no
/// Chat window for it.
#[test]
fn the_send_key_asks_the_window_in_front_to_open_its_box() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let (mut session, _launches, mut app, mut app_process) = session_with_a_window();

    press_send_key(&mut session);
    assert!(
        asked_to_send(&mut app, &mut session),
        "the window in front is asked for its box; tabs now {:?}",
        tab_apps(&session)
    );
    assert!(
        window(session.compositor(), "so.tilo.chat").is_none(),
        "Chat opens no window: the box stands in the window the person works in"
    );

    let _ = app_process.kill();
    let _ = app_process.wait();
}

/// The apps whose windows the desk holds now, tab by tab.
fn tab_apps(session: &Session) -> Vec<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .map(|tab| match &tab.tile {
                tilo_desktop_model::TileContent::Foreign { app_id, .. }
                | tilo_desktop_model::TileContent::LocalApp { app_id, .. }
                | tilo_desktop_model::TileContent::PeerWindow { app_id, .. }
                | tilo_desktop_model::TileContent::HostedWindow { app_id, .. } => app_id.clone(),
                tilo_desktop_model::TileContent::AppPlaceholder { label, .. }
                | tilo_desktop_model::TileContent::Stub { label, .. } => label.clone(),
            })
            .collect()
    })
}

/// The send key over a desk with no window in front opens Chat, which is what
/// a send with nothing to be about has always opened.
#[test]
fn the_send_key_over_an_empty_desk_opens_chat() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(tab_apps(&session).is_empty(), "the desk starts empty");

    press_send_key(&mut session);
    assert!(
        tab_apps(&session).iter().any(|app| app == "so.tilo.chat"),
        "Chat stands where the box would have: {:?}",
        tab_apps(&session)
    );
}
