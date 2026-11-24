//! The shell's send key asks the window the person works in to open its box: the app draws it
//! alone, and a shell with nothing in front still opens Chat.

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

/// The box the session asked the window for within a few passes, whatever else it tells the window
/// meanwhile: whom it opens to, and whether the window points at what it is about first. `None`
/// when it asked for no box at all.
fn asked_for_box(app: &mut Window, session: &mut Session) -> Option<(Vec<Recipient>, bool)> {
    for _ in 0..200 {
        session.step(Duration::from_millis(2)).expect("step");
        while let Some(message) = app.next() {
            if let FloatingSurfaceHostMessage::ShareSelection { to, pointing } = message {
                return Some((to, pointing));
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

/// The apps whose windows the shell holds now, tab by tab.
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

/// The send key over a shell with no window in front opens Chat, which is what a send with nothing
/// to be about has always opened, on a device that has Chat.
#[test]
fn the_send_key_over_an_empty_shell_opens_chat() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        collab: false,
        lists: tilo_session::Lists::fixed(tilo_session::AppLists {
            applications: vec![tilo_desktop_search::providers::AppEntry {
                id: "so.tilo.chat".into(),
                title: "Chat".into(),
                category: None,
            }],
            ..tilo_session::AppLists::default()
        }),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(tab_apps(&session).is_empty(), "the shell starts empty");

    press_send_key(&mut session);
    assert!(
        tab_apps(&session).iter().any(|app| app == "so.tilo.chat"),
        "Chat stands where the box would have: {:?}",
        tab_apps(&session)
    );
}

/// The workspace a report about Tilo itself goes to, as the config this test's computer is told
/// names it.
const FEEDBACK_WORKSPACE: &str = "w_tilo";

/// A home told where a report about Tilo itself goes.
fn told_where_reports_go() -> tilo_test_env::TestHome {
    let mut home = tilo_test_env::TestHome::new().expect("a home of its own");
    let config = home.home().join("community.toml");
    std::fs::write(
        &config,
        format!("[feedback]\nworkspace = \"{FEEDBACK_WORKSPACE}\"\n"),
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
    let (asked, pointing) =
        asked_for_box(&mut app, &mut session).expect("the window in front is asked");
    assert_eq!(asked.len(), 1, "a report goes to the one place it goes");
    assert_eq!(asked[0].kind, RecipientKind::Feedback);
    assert_eq!(asked[0].id, FEEDBACK_WORKSPACE);
    assert!(
        pointing,
        "the window lights what is under the pointer, and the box is about what the person clicks"
    );
    assert!(
        window(session.compositor(), "so.tilo.chat").is_none(),
        "Chat opens no window: the box stands in the window the person works in"
    );

    let _ = app_process.kill();
    let _ = app_process.wait();
}

/// The report chord over a computer told nowhere still points at what is wrong: the box says it is
/// not set up to send reports, and the person sends to an agent or a person instead.
#[test]
fn a_computer_told_nowhere_points_and_says_so_in_the_box() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let _settled = ui::motion::testing::settled();
    let (mut session, _launches, mut app, mut app_process) = session_with_a_window();

    press_report_key(&mut session);
    let (asked, pointing) =
        asked_for_box(&mut app, &mut session).expect("the window in front is asked");
    assert_eq!(asked.len(), 1, "one row says where a report would go");
    assert_eq!(
        asked[0].kind,
        RecipientKind::Feedback,
        "the row that reports a problem"
    );
    assert!(
        asked[0].id.is_empty(),
        "this computer is told nowhere, so there is nowhere to send it"
    );
    assert!(
        asked[0].detail.is_some(),
        "and the row says so where the person reads it"
    );
    assert!(pointing, "the window still lights what the report is about");
    assert!(
        window(session.compositor(), "so.tilo.chat").is_none(),
        "Chat opens no window: the box stands in the window the person works in"
    );

    let _ = app_process.kill();
    let _ = app_process.wait();
}

/// The tab the shell gives `app`'s window `surface`.
fn tab_of_window(session: &Session, app: &str, surface: u64) -> u64 {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .find(|tab| {
                matches!(
                    &tab.tile,
                    tilo_desktop_model::TileContent::Foreign { app_id, surface_id }
                        if app_id == app && *surface_id == surface
                )
            })
            .map_or_else(
                || panic!("the window of {app} at {surface} has no tab"),
                |tab| tab.id,
            )
    })
}

/// The surface of the newest mapped window of `app`.
fn newest_window(session: &mut Session, app: &str) -> u64 {
    session
        .compositor()
        .windows()
        .into_iter()
        .filter(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, _)| info.id)
        .max()
        .expect("the app has a window")
}

/// A session with two windows of [`APP`] open, each its own process and its own shell-ui
/// connection, and the second in front: what an app with a window parked after its tab closed
/// looks like. The tabs of the window behind and the window in front come with it.
fn session_with_two_windows_of_one_app() -> (
    Session,
    Window,
    Window,
    (u64, u64),
    Vec<TestClient>,
    std::process::Child,
    std::process::Child,
) {
    let launches = Arc::new(tilo_session::Launches::default());
    let mut session = crate::common::headless(Options {
        collab: false,
        launches: Arc::clone(&launches),
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    let mut processes = Vec::new();
    let mut windows = Vec::new();
    let mut tabs = Vec::new();
    let mut clients = Vec::new();
    for index in 1..=2 {
        let process = std::process::Command::new("sleep")
            .arg("120")
            .spawn_locked()
            .expect("a launched app's process");
        launches.started(process.id(), APP, "private", false);
        clients.push(mapped(&mut session, APP));
        // The second window of the app maps after the first: wait for it before naming its tab.
        for _ in 0..500 {
            let mapped = session
                .compositor()
                .windows()
                .into_iter()
                .filter(|(info, _)| info.app_id.as_deref() == Some(APP))
                .count();
            if mapped == index {
                break;
            }
            session.step(Duration::from_millis(2)).expect("step");
        }
        let surface = newest_window(&mut session, APP);
        let tab = tab_of_window(&session, APP, surface);
        let mut window = Window::open_at(&launches, APP, process.id(), tab);
        window.hello();
        window.say(&FloatingSurfaceClientMessage::SelectionCapable);
        windows.push(window);
        tabs.push(tab);
        processes.push(process);
    }
    for _ in 0..200 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    let mut windows = windows.into_iter();
    let mut tabs = tabs.into_iter();
    let mut processes = processes.into_iter();
    (
        session,
        windows.next().expect("the window behind"),
        windows.next().expect("the window in front"),
        (
            tabs.next().expect("the tab behind"),
            tabs.next().expect("the tab in front"),
        ),
        clients,
        processes.next().expect("the window behind's process"),
        processes.next().expect("the window in front's process"),
    )
}

/// Two windows of one app, the second in front: the report chord asks the one in front, never
/// the app's other window, which would draw the box where nobody sees it.
#[test]
fn the_report_chord_asks_the_window_in_front_of_two_of_one_app() {
    let _home = told_where_reports_go();
    let _settled = ui::motion::testing::settled();
    let (
        mut session,
        mut behind,
        mut front,
        _tabs,
        _clients,
        mut behind_process,
        mut front_process,
    ) = session_with_two_windows_of_one_app();

    press_report_key(&mut session);
    let (asked, pointing) =
        asked_for_box(&mut front, &mut session).expect("the window in front is asked");
    assert_eq!(asked.len(), 1, "a report goes to the one place it goes");
    assert!(
        pointing,
        "the window lights what is under the pointer, and the box is about what the person clicks"
    );
    assert!(
        asked_for_box(&mut behind, &mut session).is_none(),
        "the app's window behind is not asked"
    );
    assert!(
        window(session.compositor(), "so.tilo.chat").is_none(),
        "Chat opens no window: the box stands in the window the person works in"
    );

    let _ = behind_process.kill();
    let _ = behind_process.wait();
    let _ = front_process.kill();
    let _ = front_process.wait();
}

/// The window the shell's picker names is the window the report is about: the picker brings that
/// window's tab forward and runs the report shortcut, and the box opens there, pointing at what
/// is wrong in it — never in the window that happened to be in front. What that window's box
/// sends is one report to the one place reports go, carrying the part of the window it picked:
/// Chat's `report_problem` takes the words and what it refers to.
#[test]
fn the_window_the_picker_names_is_asked_for_the_report_about_it() {
    let _home = told_where_reports_go();
    let _settled = ui::motion::testing::settled();
    let (
        mut session,
        mut behind,
        mut front,
        (behind_tab, _front_tab),
        _clients,
        mut behind_process,
        mut front_process,
    ) = session_with_two_windows_of_one_app();

    // What the shell's `pick_report_window` asks the session, in the order it asks: the picked
    // window comes forward, then the report's shortcut runs.
    session
        .invoke(
            &format!("desktop:tab:{behind_tab}"),
            "focus",
            &serde_json::Map::new(),
        )
        .expect("the picked window comes forward");
    let report = serde_json::json!({
        "action": ui::shortcut_table::ShortcutAction::ReportProblem,
    });
    session
        .invoke(
            "desktop:chrome",
            "shortcut",
            report.as_object().expect("the shortcut's arguments"),
        )
        .expect("the report shortcut runs");

    let (asked, pointing) =
        asked_for_box(&mut behind, &mut session).expect("the window the picker named is asked");
    assert_eq!(asked.len(), 1, "a report goes to the one place it goes");
    assert_eq!(asked[0].kind, RecipientKind::Feedback);
    assert_eq!(asked[0].id, FEEDBACK_WORKSPACE);
    assert!(
        pointing,
        "the picked window lights what is wrong in it, and the box is about what the person clicks"
    );
    assert!(
        asked_for_box(&mut front, &mut session).is_none(),
        "the window that was in front is not asked for it"
    );

    // What the window's box sends is the report: to the one place reports go, carrying the part
    // of the window the person picked. Chat files it as `report_problem` with that reference.
    let picked = tilo_reference::Attached::resource(
        tilo_reference::ResourceAt {
            app: APP.to_owned(),
            record: None,
            reference: tilo_reference::ResourceRef::whole(
                "desktop.window.part",
                "button:send",
                "Send message",
            ),
        },
        None,
        "Send message",
    );
    let destination =
        Recipient::found(RecipientKind::Feedback, FEEDBACK_WORKSPACE, "Tilo feedback");
    session
        .invoke(
            "session:send-to",
            "send",
            serde_json::json!({
                "to": [destination],
                "attached": [picked],
                "text": "the button does nothing",
            })
            .as_object()
            .expect("the send's arguments"),
        )
        .expect("the picked window's report is taken");
    let report = crate::common::last_sent(&mut session);
    assert_eq!(report["to"][0]["kind"], "feedback", "{report}");
    assert_eq!(report["to"][0]["id"], FEEDBACK_WORKSPACE, "{report}");
    assert_eq!(
        report["attached"][0]["at"]["id"], "button:send",
        "the part the person picked travels with the report: {report}"
    );

    let _ = behind_process.kill();
    let _ = behind_process.wait();
    let _ = front_process.kill();
    let _ = front_process.wait();
}
