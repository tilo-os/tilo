//! The owner's control socket on a headless session: what the account whose desktop this is
//! drives with its own scripts — the action model and the state it shows — and nothing else,
//! with nothing invoked and nothing shown of what the lock covers while the screen is locked.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::{RemoteActionMessage, RemoteActionTarget};

use crate::common::{action_answer, action_frame, invoking, listing, refused};
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::surfaces::Surface;
use tilo_session::{Options, Session};
use tilo_test_env::ScopedEnv;

const PERSON: &str = "Pat";
/// The window the shell shows: what the lock must not tell the owner's socket about.
const TITLE: &str = "Groceries";

/// The ops only a development session's test-control socket answers: synthetic input, the hosted
/// surfaces' controls, the fixtures and a picture of the screen.
const DEV_ONLY: [&str; 17] = [
    "click_at",
    "click_named",
    "drag",
    "key",
    "key_down",
    "key_up",
    "list_targets",
    "modifiers",
    "pinch",
    "pointer_button",
    "pointer_move",
    "scroll",
    "search_rows",
    "snapshot",
    "swipe",
    "type",
    "unlock",
];

/// Run frames until `done` holds; `false` when it never did.
fn reached(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) -> bool {
    crate::common::reached(session, |session| done(session))
}

/// An action no provider publishes.
fn missing() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: "nothing".to_owned(),
        address: tilo_actions::ActionAddress::new("nowhere:missing", "missing"),
    }
}

/// Send `request` on the socket at `socket` as `tilo-control` does, running the session until it
/// answers.
fn ask(session: &mut Session, socket: &Path, request: &Value) -> Value {
    let socket = socket.to_path_buf();
    let request = request.to_string();
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stream = UnixStream::connect(&socket).expect("the control socket");
        stream
            .write_all(format!("{request}\n").as_bytes())
            .expect("the socket reads");
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .expect("an answer");
        answer
            .send(serde_json::from_str::<Value>(&line).expect("a JSON answer"))
            .unwrap();
    });
    crate::common::wait_for(session, "the control socket's answer", |_| {
        answered.try_recv().ok()
    })
}

/// The shell the session starts on: one workspace with one window in front.
fn layout_tree() -> Layout {
    let tabs = vec![Tab {
        id: 20,
        title: TITLE.into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: TITLE.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

/// The probe this package's tests ship, in place of the lock screen: it connects and stays up
/// while the session keeps it, as the screen that shows the lock does.
fn lock_screen() -> PathBuf {
    tilo_test_env::sibling_binary("tilo-session", "tilo-surface-probe")
}

/// Keep the stand-in lock screen up for the test's life: a screen that draws nothing and ends only
/// when the session stops it, which is what keeps the session locked.
fn lock_environment() -> ScopedEnv {
    let mut environment = ScopedEnv::new();
    environment.set("SURFACE_PROBE_STAY", "1");
    environment
}

/// The headless session: its shell with one window on it, both control sockets, and a lock screen
/// that stays up.
fn session(root: &Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        surfaces: vec![Surface::new("lock", lock_screen()).lock()],
        test_control: Some(root.join("test-control.sock")),
        admin_control: Some(root.join("admin-control.sock")),
        ..Options::default()
    });
    session.handle().replace_tree(layout_tree());
    session.step(Duration::from_millis(1)).expect("a turn");
    session
}

/// The owner's socket serves the action model and the state it shows, and refuses every op only a
/// development session's socket answers.
#[test]
fn the_owner_socket_serves_the_action_model_and_the_state_it_shows() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let owner = root.path().join("admin-control.sock");
    let dev = root.path().join("test-control.sock");
    let mut session = session(root.path());

    // A person's own scripts drive the desktop through the model it publishes: the actions it lists
    // and invokes, and the state it shows.
    let listed = ask(&mut session, &owner, &listing(""));
    assert!(
        !crate::common::listed(&listed).is_empty(),
        "the actions are listed: {listed}"
    );
    let invoked = ask(&mut session, &owner, &invoking(&missing(), &json!({})));
    assert_eq!(
        refused(&invoked),
        Some("unknown_action"),
        "the socket invokes through the broker, which says no such action: {invoked}"
    );
    let invocation = tilo_actions::InvocationId::new();
    let status = ask(
        &mut session,
        &owner,
        &action_frame(RemoteActionMessage::Observe {
            correlation: 1,
            invocation,
            wait_ms: None,
            seen: None,
        }),
    );
    assert!(
        matches!(
            action_answer(&status),
            RemoteActionMessage::Update { state: None, .. }
        ),
        "the socket follows an invocation, and knows of none it never started: {status}"
    );
    let cancelled = ask(
        &mut session,
        &owner,
        &action_frame(RemoteActionMessage::Cancel { invocation }),
    );
    assert!(
        matches!(
            action_answer(&cancelled),
            RemoteActionMessage::CancelResponse { result: Err(_), .. }
        ),
        "the socket cancels an invocation, and none it never started: {cancelled}"
    );
    let state = ask(&mut session, &owner, &json!({"op": "state_dump"}));
    assert_eq!(state["state"]["chrome"]["session_locked"], false, "{state}");
    assert!(
        state["state"]["workspace_tree"].is_object(),
        "the shell is shown while it is unlocked: {state}"
    );
    let windows = ask(&mut session, &owner, &json!({"op": "windows"}));
    assert_eq!(
        windows["answer"]["windows"][0]["title"], TITLE,
        "the windows are shown while the shell is unlocked: {windows}"
    );
    assert_eq!(
        ask(&mut session, &owner, &json!({"op": "watch"})),
        json!({"ok": {}}),
        "the socket streams the state it shows"
    );
    assert_eq!(
        ask(
            &mut session,
            &owner,
            &json!({"op": "watch", "watching": false})
        ),
        json!({"ok": {}})
    );

    // Every op that reaches into the seat — injected input, screenshots of the person's screen, the
    // hosted surfaces' controls, the fixtures — is refused there, by name.
    for op in DEV_ONLY {
        let refused = ask(&mut session, &owner, &json!({"op": op}));
        assert_eq!(
            refused["err"]["code"], "not_permitted",
            "{op} is not the owner's socket's: {refused}"
        );
        assert!(
            refused["err"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(op)),
            "{op} is named in its refusal: {refused}"
        );
        // The development socket is unchanged: the same op reaches what answers it.
        let dev_answer = ask(&mut session, &dev, &json!({"op": op}));
        assert_ne!(
            dev_answer["err"]["code"], "not_permitted",
            "a development session answers {op}: {dev_answer}"
        );
    }
    // The test identity is a development fixture too, and the owner's socket mints none.
    let minted = ask(
        &mut session,
        &owner,
        &json!({"op": "provision_test_identity"}),
    );
    assert_eq!(minted["err"]["code"], "not_permitted", "{minted}");

    // An op the protocol does not answer is unsupported on both sockets, and each names its own.
    let unknown = ask(&mut session, &owner, &json!({"op": "nonsense"}));
    assert_eq!(unknown["err"]["code"], "unsupported", "{unknown}");
    let message = unknown["err"]["message"].as_str().unwrap();
    assert!(!message.contains("`snapshot`"), "{unknown}");
    assert!(message.contains("`state_dump`"), "{unknown}");
}

/// A locked session invokes nothing through the owner's socket, and shows nothing of what the lock
/// covers — while the development socket, which drives the seat, still sees the shell.
#[test]
fn a_locked_session_invokes_nothing_through_the_owner_socket() {
    let _settled = ui::motion::testing::settled();
    let _lock = lock_environment();
    let root = crate::common::private_home();
    let owner = root.path().join("admin-control.sock");
    let dev = root.path().join("test-control.sock");
    let mut session = session(root.path());

    session.compositor().lock_now().expect("the screen locks");
    assert!(
        reached(&mut session, |session| session.compositor().locked()),
        "the session is locked"
    );
    // A pass more, so the socket reads the lock the frame was drawn under.
    session.step(Duration::from_millis(1)).expect("a turn");

    let invoked = ask(&mut session, &owner, &invoking(&missing(), &json!({})));
    assert!(
        refused(&invoked).is_some() && invoked.to_string().contains("locked"),
        "the owner's socket invokes nothing while the screen is locked: {invoked}"
    );
    // The listing stays readable: what the person may run is not the lock's to hide.
    let listed = ask(&mut session, &owner, &listing(""));
    assert!(
        !crate::common::listed(&listed).is_empty(),
        "the actions are still listed: {listed}"
    );
    // The state it shows carries nothing the lock hides: no windows, no workspaces, no titles.
    let locked = ask(&mut session, &owner, &json!({"op": "state_dump"}));
    assert_eq!(
        locked["state"]["chrome"]["session_locked"], true,
        "{locked}"
    );
    assert_eq!(
        locked["state"]["workspace_tree"],
        Value::Null,
        "nothing of the shell is shown while it is locked: {locked}"
    );
    assert_eq!(locked["state"]["held_workspaces"], json!([]), "{locked}");
    assert!(
        !locked.to_string().contains(TITLE),
        "no title of a window is shown while it is locked: {locked}"
    );
    let windows = ask(&mut session, &owner, &json!({"op": "windows"}));
    assert_eq!(
        windows["answer"]["windows"],
        json!([]),
        "no window is listed while the screen is locked: {windows}"
    );
    let watcher = ask(&mut session, &owner, &json!({"op": "watch"}));
    assert_eq!(watcher, json!({"ok": {}}), "{watcher}");
    assert_eq!(
        ask(
            &mut session,
            &owner,
            &json!({"op": "watch", "watching": false})
        ),
        json!({"ok": {}})
    );

    // The development socket drives the seat it was started for: it sees the shell it drives.
    let driven = ask(&mut session, &dev, &json!({"op": "state_dump"}));
    assert!(
        driven["state"]["workspace_tree"].is_object(),
        "a development socket sees the locked shell: {driven}"
    );

    unlock_through_the_development_socket(&mut session, root.path(), &dev, &owner);
}

/// A tool that drives the display unlocks the session the one way it unlocks — and no picture
/// leaves the session while it is locked, not even through the development socket that drives it.
fn unlock_through_the_development_socket(
    session: &mut Session,
    root: &Path,
    dev: &Path,
    owner: &Path,
) {
    let locked_png = root.join("locked.png");
    let shot = ask(
        session,
        dev,
        &json!({"op": "snapshot", "path": locked_png.to_string_lossy()}),
    );
    assert_eq!(
        shot["err"]["code"], "session_locked",
        "a locked session takes no picture: {shot}"
    );
    assert!(
        !locked_png.exists(),
        "nothing was written for the refused picture"
    );

    // The screen leaves and the lock comes down with it, as for an accepted password.
    let unlocked = ask(session, dev, &json!({"op": "unlock"}));
    assert_eq!(
        unlocked,
        json!({"ok": {"locked": false, "was_locked": true}}),
        "the lock comes down"
    );
    assert!(
        reached(session, |session| !session.compositor().locked()),
        "the session is unlocked"
    );
    let again = ask(session, dev, &json!({"op": "unlock"}));
    assert_eq!(
        again["ok"]["was_locked"], false,
        "a session already unlocked is left as it is: {again}"
    );
    session.step(Duration::from_millis(1)).expect("a turn");
    let shown = ask(session, owner, &json!({"op": "state_dump"}));
    assert!(
        shown["state"]["workspace_tree"].is_object(),
        "the shell is shown again once it is unlocked: {shown}"
    );
    let unlocked_png = root.join("unlocked.png");
    let shot = ask(
        session,
        dev,
        &json!({"op": "snapshot", "path": unlocked_png.to_string_lossy()}),
    );
    assert_eq!(
        shot,
        json!({"ok": {"frame": 0}}),
        "the picture is taken once the session is unlocked"
    );
    assert!(unlocked_png.exists(), "the picture is written");
    let invoked = ask(session, owner, &invoking(&missing(), &json!({})));
    assert_eq!(
        refused(&invoked),
        Some("unknown_action"),
        "the socket invokes again once the screen is unlocked: {invoked}"
    );
}

/// The chrome advertises its shortcuts by the table's own ids, and a person's socket runs an
/// advertised one: asking for a shortcut the shell publishes is never answered as an action that
/// does not exist.
#[test]
fn the_owner_socket_runs_an_advertised_desktop_shortcut() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let owner = root.path().join("admin-control.sock");
    let mut session = session(root.path());
    session.compositor().unlock().expect("the screen unlocks");
    assert!(
        reached(&mut session, |session| !session.compositor().locked()),
        "the session is unlocked"
    );
    session.step(Duration::from_millis(1)).expect("a turn");

    let listed = ask(&mut session, &owner, &listing("shortcut"));
    let advertised = crate::common::listed(&listed)
        .into_iter()
        .filter_map(|row| {
            Some(RemoteActionTarget {
                provider: row.provider,
                address: row.action?.address,
            })
        })
        .find(|target| {
            target.address.resource == "desktop:chrome" && target.address.action == "shortcut"
        })
        .unwrap_or_else(|| panic!("the chrome advertises its shortcut action: {listed}"));

    // `tab_menu` is a shortcut of the table, and the shell runs it.
    let invoked = ask(
        &mut session,
        &owner,
        &invoking(&advertised, &json!({"action": "tab_menu"})),
    );
    assert_eq!(refused(&invoked), None, "the shell runs it: {invoked}");

    // A step the shell does not publish is told apart from one it would not run: a caller reads
    // that no such action exists, rather than that its own authority was refused.
    let unpublished = RemoteActionTarget {
        provider: advertised.provider,
        address: tilo_actions::ActionAddress::new("desktop:chrome", "not-a-shortcut"),
    };
    let missing = ask(&mut session, &owner, &invoking(&unpublished, &json!({})));
    assert_eq!(
        refused(&missing),
        Some("unknown_action"),
        "an address the shell publishes nothing at says so: {missing}"
    );
}
