//! Opening an app while the canvas stands its turns (`docs/designs/app-language/canvas.md`):
//! the app's own window comes, and every turn stands as it did.
//!
//! A turn of the person's conversation with their agent is a place of its own, showing Chat's
//! `turn` view of the turn's resource. Opening Chat — from the notification a message raises, or
//! from the launcher — is the app's window, never a second meaning given to one of those
//! places: no turn is retargeted, retitled, or filled with the whole window, and the person's
//! keyboard goes to the canvas until they choose the window. A question asked after the window
//! came stands its own turn, as one asked before did.
//!
//! Every step is the production path: the turn is stood as the session stands it when Chat holds
//! the ask, the banner is the desktop's own notification and its own activation, the launcher's
//! open is the shell's `open-app`, and the window that comes is a window the policy binds, as a
//! real app's window is. What is stubbed: Chat's process, whose answer this test stands in for
//! (`chat_holds`), exactly as `canvas_point.rs` does.

#![cfg(target_os = "linux")]

use std::path::Path;
use std::rc::Rc;

use tilo_app_launch::SessionEnvironment;
use tilo_desktop_policy::Window;
use tilo_desktop_surface::notifications::BannerAsk;
use tilo_session::Session;
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options};
use tilo_session_core::desktop::notifications::Notification;

use crate::common::{
    CANVAS, FRAMES, PERSON, canvas_shown, device_in, headless, number, on_screen, places,
    point_and_say, seeded_apps, show_preset, steps_within, strip, until, wait_for,
};

/// The conversation the turns of these tests are in, as Chat publishes a channel.
const CONVERSATION: &str = "chat.workspace/channels/c_open";

/// The app the window is opened as: the packaged Chat, which announces itself by this id, while
/// the layout publishes the turns' places under the app's short name (`chat`).
const CHAT: &str = "so.tilo.lang.chat";

/// The resource a turn place shows: Chat's resource for one turn of the conversation.
const TURN: &str = "chat.workspace.turns:";

/// A window the policy may bind: what an app's own toolkit window announces itself by, and the
/// surface it draws through.
const WINDOW: u64 = 11;

/// A window of `app` the compositor mapped, as a toolkit window announces itself.
fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..Window::default()
    }
}

/// Every placed strip's canvas box, by the resource its place shows: `(x, y, w, h)` in canvas
/// pixels, as the shell holds it.
fn canvas_box(session: &Session, needle: &str) -> Option<(i64, i64, i64, i64)> {
    places(session).iter().find_map(|place| {
        let strip = strip(session, number(&place.strip))?;
        (strip.placed && place.resource.contains(needle)).then_some((
            strip.canvas_x,
            strip.canvas_y,
            strip.canvas_width,
            strip.canvas_height,
        ))
    })
}

/// What a turn place says and draws: its resource, the view it shows it through, its title on
/// the canvas, what the shell calls it, and where it stands.
type Turn = (String, String, String, String, (i64, i64, i64, i64));

/// What the layout holds of the tab `key`: its title, its tile as the canvas draws it, and the
/// view it shows its resource through.
fn tab_of(session: &Session, key: u64) -> (String, String, String) {
    session.handle().read(|tree| {
        let tab = tree
            .tabs()
            .find(|tab| tab.id == key)
            .unwrap_or_else(|| panic!("the tab {key} is on the layout"));
        let tile = match &tab.tile {
            tilo_desktop_model::TileContent::LocalApp {
                app_id,
                label,
                note,
            } => {
                format!("LocalApp {app_id} {label} {note:?}")
            }
            other => format!("{other:?}"),
        };
        (tab.title.clone(), tile, tab.view.clone())
    })
}

/// Every turn the canvas stands now, as each will be compared after the app opens.
fn turns(session: &Session) -> Vec<Turn> {
    let mut turns: Vec<Turn> = places(session)
        .into_iter()
        .filter(|place| place.resource.starts_with(TURN))
        .filter_map(|place| {
            let strip = strip(session, number(&place.strip))?;
            Some((
                place.resource,
                place.shown_as,
                place.title,
                place.name,
                (
                    strip.canvas_x,
                    strip.canvas_y,
                    strip.canvas_width,
                    strip.canvas_height,
                ),
            ))
        })
        .collect();
    turns.sort();
    turns
}

/// Chat holding the person's ask as the turn `question` of `conversation`: what Chat's process
/// does in a person's session, posting the message the ask sent it, answering the send with that
/// message, and holding it in the resources it publishes, which the session reads the turn back
/// from.
fn chat_holds(session: &Session, conversation: &str, question: &str) {
    let turn = format!("{TURN}{question}");
    let asked = tilo_session_core::layout::Asked {
        conversation: conversation.to_owned(),
        message: question.to_owned(),
        turn: turn.clone(),
        opened_as: format!(
            "chat.workspace.channels:{}",
            conversation.trim_start_matches("chat.workspace/channels/")
        ),
    };
    let mut published = tilo_actions::ResourceSpec::new(turn, "chat.workspace.turns", question)
        .with_view(tilo_actions::ResourceViewSpec::new(
            "turn", question, "open",
        ));
    published.app_id = Some("chat".to_owned());
    let mut whole = tilo_actions::ResourceSpec::new(
        asked.opened_as.clone(),
        "chat.workspace.channels",
        "Assistant",
    )
    .with_view(tilo_actions::ResourceViewSpec::new(
        "page",
        "Assistant",
        "open",
    ));
    whole.app_id = Some("chat".to_owned());
    let published = vec![published, whole];
    assert!(
        session.layout().borrow().awaits_turns(),
        "the ask waits for Chat to hold it"
    );
    let stood = session.layout().borrow_mut().stand_asked(
        &|_| Some((asked.clone(), published.clone())),
        tilo_session_core::layout::Busy::default(),
    );
    assert_eq!(stood, 1, "the ask waited for its turn");
}

/// What the session's send reading says it carried out: each send's words, oldest first.
fn sends(session: &Session) -> Vec<String> {
    session
        .read("session:send-to")
        .and_then(|read| {
            read["sent"].as_array().map(|sent| {
                sent.iter()
                    .filter_map(|sent| sent["text"].as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// Ask `text` on the canvas's free ground, and answer once the session carried this ask's send
/// out: the reading's sends grew by one carrying these words, which it already holds for a
/// question asked before this one.
fn ask(session: &mut Session, text: &str) {
    let asked = sends(session).len();
    let at = free_ground(session);
    point_and_say(session, at, text);
    until(session, "the ask sent", |session| {
        session.settle_services();
        let sent = sends(session);
        sent.len() > asked && sent.last().is_some_and(|last| last == text)
    });
}

/// A screen point on the canvas below every place it holds: free ground, whatever the places
/// above it take up, as `canvas_point.rs` finds one.
fn free_ground(session: &Session) -> (f64, f64) {
    let (x, y, w, h) = on_screen(session, CANVAS).expect("the canvas");
    let below = places(session)
        .iter()
        .filter_map(|place| canvas_box(session, &place.resource))
        .map(|(_, py, _, ph)| py + ph)
        .max()
        .unwrap_or_default();
    #[allow(clippy::cast_precision_loss, reason = "canvas pixels")]
    (x + w * 0.5, (y + below as f64 + y + h) / 2.0)
}

/// The apps this session starts: on its own display, through the launcher at `launcher` in `home`.
fn apps_at(home: &tilo_test_env::TestHome, launcher: &Path) -> Rc<Apps> {
    let mut environment = SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    Rc::new(Apps::with_launcher(
        environment,
        None,
        Some(launcher.to_owned()),
    ))
}

/// A launcher that writes down each start it was handed and holds, so the process a launch
/// started stays until the test is over.
fn launcher_noting_starts(home: &tilo_test_env::TestHome, handed: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{handed}'\nhold\n",
            handed = handed.display()
        ),
    )
    .unwrap()
}

/// The pid the launcher wrote down for the start it was handed: the process the window it opens
/// belongs to.
fn started_pid(handed: &Path) -> Option<u32> {
    std::fs::read_to_string(handed).ok().and_then(|said| {
        said.lines()
            .next()
            .map(str::trim)
            .and_then(|pid| pid.parse().ok())
    })
}

/// A session on the canvas with the question asked and Chat holding its turn, and that turn's
/// tab: the session's apps start through a launcher of the test's own, which writes down each
/// start it was handed.
fn session_with_a_turn() -> (
    tilo_test_env::TestHome,
    Session,
    tilo_test_env::StandIn,
    u64,
) {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let state = device_in(&mut home);
    let handed = home.home().join("handed");
    let launcher = launcher_noting_starts(&home, &handed);
    let apps = apps_at(&home, launcher.path());
    let mut session = headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: seeded_apps(),
        state: Some(state),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        ..Options::default()
    });
    show_preset(&mut session, FRAMES);
    until(&mut session, "the canvas", |session| canvas_shown(session));
    ask(&mut session, "what is on my task list?");
    chat_holds(&session, CONVERSATION, "q1");
    let turn = wait_for(&mut session, "the turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource.starts_with(TURN))
    });
    (home, session, launcher, number(&turn.key))
}

/// Chat's process tells the shell how its window's bar names the open conversation: a window that
/// declares a location in its bar names its tab by it, and Chat names the conversation it opened.
fn bar_names_thread(session: &Session, tab: u64) {
    session.handle().declare_bar(
        tab,
        WINDOW,
        Some(ui::window_toolbar::WindowToolbar {
            location: Some(ui::window_toolbar::ToolbarLocation {
                label: "Thread".to_owned(),
                ..ui::window_toolbar::ToolbarLocation::default()
            }),
            ..ui::window_toolbar::WindowToolbar::default()
        }),
    );
}

/// Frames until the desktop took the bar's ask up: the name the ask declared is what the shell
/// calls `tab` now, so whatever the frame does with the tab it has done. The desktop takes an ask
/// up in the frame after the one it was made in, so a short bound is all this wait needs.
fn bar_taken_up(session: &mut Session, tab: u64) {
    let taken = steps_within(session, std::time::Duration::from_secs(5), |session| {
        (session
            .desktop()
            .with_policy(|policy| policy.handle().window_name(tab))
            .as_deref()
            == Some("Thread"))
        .then_some(())
    });
    assert!(
        taken.is_some(),
        "the desktop takes the bar's name up in the frame after the ask"
    );
}

/// Map the window the open asked for, made by the process the launcher started, and wait until
/// the policy binds it to a tab.
fn window_comes(session: &mut Session, handed: &Path) {
    let pid = wait_for(session, "the launcher started Chat", |_| {
        started_pid(handed)
    });
    session
        .desktop()
        .with_policy(|policy| {
            policy.map(Window {
                pid: Some(pid),
                ..window(WINDOW, "chat")
            })
        })
        .expect("Chat's window maps");
    until(session, "the window bound to its tab", |session| {
        session
            .desktop()
            .with_policy(|policy| policy.tab_of(WINDOW))
            .is_some()
    });
}

/// The banner a message from Chat raises, activated as the person clicks it: the open that once
/// took the first turn's tab on the canvas for Chat's own.
#[test]
fn a_message_opening_chat_leaves_every_turn_in_place() {
    let _settled = ui::motion::testing::settled();
    let (home, mut session, _launcher, turn_tab) = session_with_a_turn();
    let handed = home.home().join("handed");
    let before = turns(&session);
    let before_tab = tab_of(&session, turn_tab);
    assert_eq!(before.len(), 1, "one turn stands: {before:?}");
    assert_eq!(before[0].1, "turn", "the turn stands through its view");

    let id = session.notices().post(
        Notification::new("Chat", "Guest wrote to you")
            .with_app_id(CHAT)
            .with_app_name("Chat"),
    );
    session.notices().answer(BannerAsk::Activate(id));

    // Chat's own process opens its window, and the shell's policy takes it in.
    window_comes(&mut session, &handed);
    // Chat's window says its bar names the open conversation "Thread", as it does the moment it
    // draws, and the desktop takes the name up in its next frame: the turn's place keeps its own
    // title and its view, and no window's name is written into it.
    bar_names_thread(&session, turn_tab);
    bar_taken_up(&mut session, turn_tab);

    let after = turns(&session);
    assert_eq!(
        after, before,
        "every turn keeps its view, its title and what it draws when the app opens"
    );
    let focused = session.desktop().with_policy(|policy| policy.focused_tab());
    assert_ne!(
        focused,
        Some(turn_tab),
        "the keyboard does not land in the turn's place: {:?}",
        places(&session)
    );
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.tab_of(WINDOW))
            .is_some_and(|tab| tab != turn_tab),
        "the window took a tab of the app's own, not the turn's"
    );
    let after_tab = tab_of(&session, turn_tab);
    let (_title, tile, view) = &after_tab;
    assert_eq!(view, "turn", "the turn still shows its view");
    assert_eq!(
        after_tab, before_tab,
        "the turn keeps its own title and stays Chat's place, with no window bound into it"
    );
    assert!(
        tile.starts_with("LocalApp chat "),
        "the turn is still Chat's place, not a window: {tile:?}"
    );
}

/// The same open from the launcher, and a question asked after it: the app's window comes, the
/// turn stands, and the later ask stands a turn place of its own.
#[test]
fn an_open_from_the_launcher_leaves_the_turn_and_the_next_ask_stands_its_own() {
    let _settled = ui::motion::testing::settled();
    let (home, mut session, _launcher, turn_tab) = session_with_a_turn();
    let handed = home.home().join("handed");
    let before = turns(&session);
    let before_tab = tab_of(&session, turn_tab);
    let first_box = canvas_box(&session, "chat.workspace.turns:q1");
    assert!(
        first_box.is_some(),
        "the turn stands: {:?}",
        places(&session)
    );

    session
        .invoke(
            tilo_desktop::verbs::CHROME_RESOURCE,
            "open-app",
            serde_json::json!({ "app": CHAT })
                .as_object()
                .expect("an object"),
        )
        .unwrap_or_else(|error| panic!("{CHAT} opens on the shell: {error:?}"));
    // Chat's own toolkit opens its window, and the policy binds it.
    window_comes(&mut session, &handed);
    let window_tab = session
        .desktop()
        .with_policy(|policy| policy.tab_of(WINDOW))
        .expect("the window's tab");
    bar_names_thread(&session, window_tab);
    bar_taken_up(&mut session, window_tab);
    assert_eq!(
        turns(&session),
        before,
        "the turn keeps its view, its title and what it draws when the app opens"
    );

    // A question asked now, with the app's window open: the person's keyboard is on the canvas
    // where they click, so it stands a turn of its own.
    ask(&mut session, "and my notes?");
    chat_holds(&session, CONVERSATION, "q2");
    let second = wait_for(&mut session, "the second turn placed", |session| {
        places(session)
            .into_iter()
            .find(|place| place.resource == "chat.workspace.turns:q2")
    });
    assert_eq!(
        second.shown_as, "turn",
        "the later ask stands through the turn view"
    );
    assert_eq!(
        canvas_box(&session, "chat.workspace.turns:q1"),
        first_box,
        "the first turn still stands where it did"
    );
    assert_eq!(
        canvas_box(&session, "chat.workspace.turns:q2"),
        Some((
            strip(&session, number(&second.strip))
                .expect("the second turn's strip")
                .canvas_x,
            strip(&session, number(&second.strip))
                .expect("the second turn's strip")
                .canvas_y,
            strip(&session, number(&second.strip))
                .expect("the second turn's strip")
                .canvas_width,
            strip(&session, number(&second.strip))
                .expect("the second turn's strip")
                .canvas_height,
        )),
        "the later ask has a place of its own: {:?}",
        places(&session)
    );
    assert_ne!(
        number(&second.key),
        turn_tab,
        "the later ask's place is not the first turn's"
    );
    let after_tab = tab_of(&session, turn_tab);
    assert_eq!(
        after_tab, before_tab,
        "the first turn keeps its view, its title, and stays Chat's place with no window bound into it"
    );
    let focused = session.desktop().with_policy(|policy| policy.focused_tab());
    assert_ne!(
        focused,
        Some(turn_tab),
        "the keyboard does not land in the turn's place"
    );

    // Asking for the app once more, by the name the layout publishes its places under, goes to the
    // window the open made: a turn's place is not a window of Chat to take the person to.
    session
        .invoke(
            tilo_desktop::verbs::CHROME_RESOURCE,
            "open-app",
            serde_json::json!({ "app": "chat" })
                .as_object()
                .expect("an object"),
        )
        .unwrap_or_else(|error| panic!("chat opens on the shell: {error:?}"));
    let went = steps_within(&mut session, std::time::Duration::from_secs(5), |session| {
        (session.desktop().with_policy(|policy| policy.focused_tab()) == Some(window_tab))
            .then_some(())
    });
    assert!(
        went.is_some(),
        "the open went to the app's window, not the turn's place: focused {:?}, window {window_tab}, turn {turn_tab}",
        session.desktop().with_policy(|policy| policy.focused_tab())
    );
    assert_eq!(
        tab_of(&session, turn_tab),
        before_tab,
        "the turn keeps its view and its title when the app is asked for again"
    );
}
