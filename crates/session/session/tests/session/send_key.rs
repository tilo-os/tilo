//! The shell's send key asks the window the person works in to open its box: the app draws it
//! alone, and a desk with nothing in front still opens Chat.

use std::sync::Arc;
use std::time::Duration;

use tilo_test_env::SpawnUnderEnvLock as _;
use ui::floating_surface::{
    FloatingSurfaceClientMessage, FloatingSurfaceHostMessage, Recipient, RecipientKind,
};

use super::window_warmth::Window;
use super::*;

/// The app a window of this test runs: no app of this package, so nothing but the session's own
/// answers replies on its connection.
const APP: &str = "org.example.Notes";

/// A session with a window of [`APP`] in front, and that window's own shell-ui connection.
fn session_with_a_window() -> (
    Session,
    Arc<tilo_session::Launches>,
    Window,
    std::process::Child,
) {
    let launches = Arc::new(tilo_session::Launches::default());
    let mut session = crate::common::headless(Options {
        collab: false,
        launches: Arc::clone(&launches),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    let app_process = std::process::Command::new("sleep")
        .arg("120")
        .spawn_locked()
        .expect("a launched app's process");
    launches.started(app_process.id(), APP, "private", false);
    let _client = mapped(&mut session, APP);
    let mut window = Window::open(&launches, APP, app_process.id());
    window.hello();
    window.say(&FloatingSurfaceClientMessage::SelectionCapable);
    // The pass that takes the window's word answers its hello, so the box is asked for after the
    // window has heard of the session at all.
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

/// Whether the session asks the window to send within a few passes, whatever else it tells the
/// window meanwhile.
fn asked_to_send(app: &mut Window, session: &mut Session) -> bool {
    asked_for_box(app, session).is_some()
}

/// Whom the session asked the window to open its box to within a few passes,
/// whatever else it tells the window meanwhile: `None` when it asked for no
/// box at all.
fn asked_for_box(app: &mut Window, session: &mut Session) -> Option<Vec<Recipient>> {
    for _ in 0..200 {
        session.step(Duration::from_millis(2)).expect("step");
        while let Some(message) = app.next() {
            if let FloatingSurfaceHostMessage::ShareSelection { to } = message {
                return Some(to);
            }
        }
    }
    None
}

/// The send key over a window that opens a box asks that window, and opens no Chat window for it.
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
                | tilo_desktop_model::TileContent::HostedWindow { app_id, .. } => app_id.clone(),
                tilo_desktop_model::TileContent::AppPlaceholder { label, .. }
                | tilo_desktop_model::TileContent::Stub { label, .. } => label.clone(),
            })
            .collect()
    })
}

/// The send key over a desk with no window in front opens Chat, which is what a send with nothing
/// to be about has always opened.
#[test]
fn the_send_key_over_an_empty_desk_opens_chat() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(tab_apps(&session).is_empty(), "the desk starts empty");

    press_send_key(&mut session);
    assert!(
        tab_apps(&session).iter().any(|app| app == "so.tilo.chat"),
        "Chat stands where the box would have: {:?}",
        tab_apps(&session)
    );
}

/// The workspace and the conversation a report about Tilo itself goes to, as the config this test's
/// computer is told names them.
const FEEDBACK_WORKSPACE: &str = "w_tilo";
const FEEDBACK_CHANNEL: &str = "c_feedback";

/// A home told where a report about Tilo itself goes.
fn told_where_reports_go() -> tilo_test_env::TestHome {
    let mut home = tilo_test_env::TestHome::new().expect("a home of its own");
    let config = home.home().join("community.toml");
    std::fs::write(
        &config,
        format!(
            "[feedback]\nworkspace = \"{FEEDBACK_WORKSPACE}\"\nchannel = \"{FEEDBACK_CHANNEL}\"\n"
        ),
    )
    .expect("a config naming where reports go");
    home.env().set("TILO_COMMUNITY_CONFIG", &config);
    home
}

/// Press the report-a-problem chord over the window in front.
fn press_report_key(session: &mut Session) {
    key(session, keys::LEFT_META, true);
    tap(session, keys::R);
    key(session, keys::LEFT_META, false);
}

/// The report chord asks the window in front for its box, with the one place this computer sends
/// reports on it: the person writes what went wrong over the window it is about, and nothing but
/// the words is left to fill in.
#[test]
fn a_report_asks_the_window_in_front_for_its_box_at_the_destination() {
    let _home = told_where_reports_go();
    let _settled = ui::motion::testing::settled();
    let (mut session, _launches, mut app, mut app_process) = session_with_a_window();

    press_report_key(&mut session);
    let asked = asked_for_box(&mut app, &mut session).expect("the window in front is asked");
    assert_eq!(asked.len(), 1, "a report goes to the one place it goes");
    assert_eq!(asked[0].kind, RecipientKind::Feedback);
    assert_eq!(asked[0].id, FEEDBACK_CHANNEL);
    assert!(
        window(session.compositor(), "so.tilo.chat").is_none(),
        "Chat opens no window: the box stands in the window the person works in"
    );

    let _ = app_process.kill();
    let _ = app_process.wait();
}

/// A computer told nowhere to send reports asks no window for a box: a report with nowhere to go is
/// not written at all.
#[test]
fn a_computer_told_nowhere_asks_no_window_for_a_report() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let (mut session, _launches, mut app, mut app_process) = session_with_a_window();

    press_report_key(&mut session);
    assert!(
        asked_for_box(&mut app, &mut session).is_none(),
        "no box is opened for a report that has nowhere to go"
    );
    assert!(
        window(session.compositor(), "so.tilo.chat").is_none(),
        "and nothing is opened in its place"
    );

    let _ = app_process.kill();
    let _ = app_process.wait();
}
