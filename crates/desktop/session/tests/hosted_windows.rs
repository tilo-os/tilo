//! Every window of an app whose windows are hosted opens in a tab of the shell's, and that tab is
//! the window's instance: what the window asks about itself — the controls of its content, its
//! bar, its search — is answered for the tab it stands in, never for another window of its app.
//!
//! The session asks the shell for that tab whatever opened the window: the app's first window, a
//! window of its own beside the one it shows (what search opens from a row with Ctrl+Enter), and a
//! window on a record of the app. Each test here starts one of them and reads the instance the
//! window's process was told: it is the tab the shell made, and the tab is the app's.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session outlives every call"
)]

use std::fs;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use tilo_app_launch::Connects as _;
use tilo_app_launch::{Launch, SessionEnvironment};
use tilo_desktop_model::Workspace;
use tilo_desktop_policy::Window;
use tilo_session::app_tabs::ServiceLinks;
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Launches, Options, Session};
use tilo_test_env::SpawnUnderEnvLock as _;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
};
use ui::window_targets::{TargetAction, WindowTarget};

use crate::common::PERSON;

/// The app whose window each test opens: a tilolang app, whose windows hold none of its data, so
/// each of them is an instance the session's host runs and a tab of the shell's.
const APP: &str = "so.tilo.tasks";
/// The record one test opens a window on.
const RECORD: &str = "uuid:7a110d00-0000-4000-8000-000000000009";

/// Install [`APP`] in `home` 's own app store as a tilolang app: its entrypoint runs a tilolang
/// runtime, which is what makes its windows hosted.
fn installed_windowed(home: &mut tilo_test_env::TestHome) {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(APP);
    fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest = payload.join("manifest.toml");
    fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{APP}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"Tasks\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"off\"\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/tasks\"\n\
             type = \"gui\"\n\
             runtime = \"tilolang/0.1.0\"\n",
            key = "11".repeat(32),
        ),
    )
    .unwrap();
    fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{APP}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .unwrap();
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// A launcher that records the instance each start was told it opens, and runs as the app would
/// until the test is over: the window's process takes its instance from that variable.
fn launcher_recording(home: &tilo_test_env::TestHome, handed: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"${{TILO_CONTINUITY_INSTANCE:-}}\" >> '{handed}'\nhold\n",
            handed = handed.display(),
        ),
    )
    .unwrap()
}

/// As [`launcher_recording`], recording its own process id beside the instance: the window's
/// shell-ui connection is attributed to the process its launch started.
fn launcher_recording_pid(home: &tilo_test_env::TestHome, handed: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s %s\\n' \"$$\" \"${{TILO_CONTINUITY_INSTANCE:-}}\" >> '{handed}'\nhold\n",
            handed = handed.display(),
        ),
    )
    .unwrap()
}

/// The apps this session starts, on its own display, through `launcher`.
fn apps_at(home: &tilo_test_env::TestHome, launcher: PathBuf) -> Rc<Apps> {
    let mut environment = SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    Rc::new(Apps::with_launcher(environment, None, Some(launcher)))
}

/// A window of [`APP`] the compositor mapped, its bar saying the app's own id: what the shell has
/// when the app already shows a window.
fn a_window(id: u64) -> Window {
    Window {
        id,
        app_id: Some(APP.into()),
        title: Some("Tasks".into()),
        ..Window::default()
    }
}

/// A headless session whose shell the tests open windows on, its apps started through `apps`.
fn session(apps: Rc<Apps>) -> Session {
    session_on(apps, Arc::<Launches>::default())
}

/// A headless session whose shell the tests open windows on, its apps started through `apps`, and
/// the launches it records to `launches`: the connections its windows' shell-ui sockets are made
/// on.
fn session_on(apps: Rc<Apps>, launches: Arc<Launches>) -> Session {
    crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(apps),
        launches,
        ..Options::default()
    })
}

/// A window whose process is `pid` and whose instance is the tab the shell made for it.
fn a_window_of(instance: u64, pid: u32) -> Window {
    Window {
        id: instance,
        pid: Some(pid),
        app_id: Some(APP.into()),
        title: Some("Tasks".into()),
        ..Window::default()
    }
}

/// Every frame the session wrote back on `connection` by the turn of its loop on which `heard`
/// holds of them: whole lines, each a host message.
fn host_frames(
    session: &mut Session,
    connection: &mut UnixStream,
    heard: impl Fn(&[FloatingSurfaceHostMessage]) -> bool,
) -> Vec<FloatingSurfaceHostMessage> {
    connection.set_nonblocking(true).unwrap();
    let mut seen = Vec::new();
    let mut frames = Vec::new();
    crate::common::wait_maybe(session, |_| {
        let mut chunk = [0u8; 1024];
        loop {
            match std::io::Read::read(connection, &mut chunk) {
                Ok(0) => break,
                Ok(read) => seen.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("the connection broke: {error}"),
            }
        }
        let whole = seen
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(&seen[..0], |end| &seen[..end]);
        frames = String::from_utf8(whole.to_vec())
            .expect("frames are text")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line.trim()).expect("the session wrote a host message")
            })
            .collect();
        heard(&frames).then_some(())
    });
    frames
}

/// A hosted app's window says what its content offers by what each control does, and a press on
/// one by that name reaches the app on the connection that described it: what a person's tools,
/// an agent and a test press a tilolang window's bar by.
#[test]
fn a_hosted_windows_control_pressed_by_what_it_does_reaches_the_app() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("started");
    let launcher = launcher_recording_pid(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) =
        crate::common::wait_for(&mut session, "the window's process starts", |_| {
            fs::read_to_string(&handed).ok().and_then(|text| {
                let line = text.lines().find(|line| !line.trim().is_empty())?;
                let (pid, instance) = line.trim().split_once(' ')?;
                Some((pid.parse::<u32>().ok()?, instance.parse::<u64>().ok()?))
            })
        });
    session
        .desktop()
        .with_policy(|policy| policy.map(a_window_of(instance, pid)).unwrap());
    crate::common::until(
        &mut session,
        "the app's window is on the shell",
        |session| tabs_of(session, APP).contains(&instance),
    );

    // The window's own shell-ui connection says what its content shows, each control by what it
    // does.
    let identity = format!("{APP}:item{}done", '\u{1f}');
    let mut connection: UnixStream = ServiceLinks::new(Arc::clone(&started))
        .shell_ui(APP, instance, Some(pid))
        .expect("the window's shell-ui connection")
        .into();
    let targets = FloatingSurfaceClientMessage::WindowTargets {
        targets: vec![WindowTarget {
            role: "Button".to_owned(),
            name: "Done".to_owned(),
            identity: Some(identity.clone()),
            rect: [4.0, 6.0, 44.0, 24.0],
            actions: vec![TargetAction::Activate],
            level: None,
            selected: false,
        }],
        instance: None,
    };
    for frame in [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        targets,
    ] {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        connection.write_all(&bytes).unwrap();
    }
    connection.flush().unwrap();
    crate::common::until(
        &mut session,
        "the window's control is listed by what it does",
        |session| {
            session
                .controls()
                .iter()
                .any(|control| control.identity.as_deref() == Some(identity.as_str()))
        },
    );

    // The press the session makes on the control that does the identity, as the control socket
    // makes it: the window in front shows it, so it is that window's.
    session
        .handle()
        .press_window_control_doing(&identity, TargetAction::Activate)
        .expect("the window's control is pressed");

    // The press reaches the app on the connection that described it.
    let pressed = FloatingSurfaceHostMessage::PressTarget {
        name: "Done".to_owned(),
        role: "Button".to_owned(),
        identity: Some(identity),
        action: TargetAction::Activate,
        after_input: None,
    };
    let told = host_frames(&mut session, &mut connection, |told| {
        told.contains(&pressed)
    });
    assert!(
        told.contains(&pressed),
        "the app is told which of its controls was pressed: {told:?}"
    );
}

/// A hosted window's accessibility tree reaches the session's export — its window's name, its
/// nodes' roles, the focus — and an action a client of the export takes on one of its nodes goes
/// back to the window that drew it, as the window's own assistive client's action would.
#[test]
fn a_hosted_windows_tree_reaches_the_export_and_a_clients_action_reaches_the_app() {
    use ui::accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId, TreeInfo, TreeUpdate};

    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("started");
    let launcher = launcher_recording_pid(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) =
        crate::common::wait_for(&mut session, "the window's process starts", |_| {
            fs::read_to_string(&handed).ok().and_then(|text| {
                let line = text.lines().find(|line| !line.trim().is_empty())?;
                let (pid, instance) = line.trim().split_once(' ')?;
                Some((pid.parse::<u32>().ok()?, instance.parse::<u64>().ok()?))
            })
        });
    session
        .desktop()
        .with_policy(|policy| policy.map(a_window_of(instance, pid)).unwrap());
    crate::common::until(
        &mut session,
        "the app's window is on the shell",
        |session| tabs_of(session, APP).contains(&instance),
    );

    // The window sends the session what assistive technology reads of it, as it draws it.
    let mut connection: UnixStream = ServiceLinks::new(Arc::clone(&started))
        .shell_ui(APP, instance, Some(pid))
        .expect("the window's shell-ui connection")
        .into();
    let mut window = Node::new(Role::Window);
    window.set_label("Tasks");
    window.set_children(vec![NodeId(1)]);
    let mut button = Node::new(Role::Button);
    button.set_label("Done");
    button.add_action(Action::Click);
    let tree = TreeUpdate {
        nodes: vec![(NodeId(0), window), (NodeId(1), button)],
        tree: Some(TreeInfo::new(NodeId(0))),
        tree_id: TreeId::ROOT,
        focus: NodeId(1),
    };
    for frame in [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        FloatingSurfaceClientMessage::AccessibilityTree {
            tree,
            name: "Tasks".to_owned(),
            instance: Some(instance),
        },
    ] {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        connection.write_all(&bytes).unwrap();
    }
    connection.flush().unwrap();

    // The session exports the window a screen reader finds: its name, its roles and the node it
    // focuses.
    crate::common::until(&mut session, "the window's tree is exported", |session| {
        session.a11y_windows().into_iter().any(|(_, name, tree)| {
            name == "Tasks"
                && tree.focus == NodeId(1)
                && tree.nodes.iter().any(|(id, node)| {
                    *id == NodeId(1) && node.role() == Role::Button && node.label() == Some("Done")
                })
        })
    });
    let (place, _, tree) = session
        .a11y_windows()
        .into_iter()
        .find(|(_, name, _)| name == "Tasks")
        .expect("the window the app drew");
    let root = tree
        .nodes
        .iter()
        .find(|(id, _)| *id == NodeId(0))
        .map(|(_, node)| node)
        .expect("the window's root");
    assert_eq!(root.role(), Role::Window, "the app's window is a window");
    assert_eq!(
        tree.focus,
        NodeId(1),
        "the node the window focuses is the client's focus"
    );

    // A client activates the button through the export: the window that drew it hears the action.
    assert!(
        session.a11y_perform(place, NodeId(1), &tilo_ui_base::a11y::Action::Activate),
        "the export takes the activation"
    );
    let asked = ActionRequest {
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: NodeId(1),
        data: None,
    };
    let told = host_frames(&mut session, &mut connection, |told| {
        told.iter().any(|message| {
            matches!(
                message,
                FloatingSurfaceHostMessage::AccessibilityAction { request, .. }
                    if request == &asked
            )
        })
    });
    assert!(
        told.iter().any(|message| matches!(
            message,
            FloatingSurfaceHostMessage::AccessibilityAction { request, .. } if request == &asked
        )),
        "the app is told what the client asked of its node: {told:?}"
    );
}

/// A route an app draws itself stands in the shell's card: the card holds a block of the size the
/// app asked for, and the app is told to stand its card's window exactly there, while its own
/// window is not on screen.
#[test]
fn an_app_drawn_route_card_stands_where_the_shells_card_holds_it() {
    use ui::floating_surface::{FloatingRole, FloatingSource, FloatingSurfaceRequest};
    use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};
    use ui::vello::kurbo::{Rect, Size};

    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("started");
    let launcher = launcher_recording_pid(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = crate::common::wait_for(&mut session, "the app's process starts", |_| {
        fs::read_to_string(&handed).ok().and_then(|text| {
            let line = text.lines().find(|line| !line.trim().is_empty())?;
            let (pid, instance) = line.trim().split_once(' ')?;
            Some((pid.parse::<u32>().ok()?, instance.parse::<u64>().ok()?))
        })
    });

    // The route runs, and its app asks for it to float: the app draws the card itself, a body of
    // 300 by 160 in a soft frame of 8 around it.
    let mut published = Published::of(
        Summary::new(Kind::Navigation, "Turn left onto Main Street", "200 m")
            .with_id("route")
            .with_source("Tasks")
            .with_control(Control::new("end", "End route", ControlKind::Close)),
    );
    published.overlay = true;
    session.live_activities().show(
        APP,
        Some(serde_json::to_value(published).expect("an activity is JSON")),
    );
    let body = Rect::new(0.0, 0.0, 300.0, 160.0);
    let request = FloatingSurfaceRequest {
        id: "route".into(),
        title: "Turn left onto Main Street".into(),
        role: FloatingRole::Navigation,
        source: FloatingSource::AppElement {
            app_id: APP.into(),
            element_id: "route".into(),
            region: body,
        },
        preferred_size: Size::new(300.0, 160.0),
        aspect_ratio: None,
        initial_rect: Some(body),
        presentation_rect: Some(body.inflate(8.0, 8.0)),
        tracking_source: None,
    };
    let mut connection: UnixStream = ServiceLinks::new(Arc::clone(&started))
        .shell_ui(APP, instance, Some(pid))
        .expect("the app's shell-ui connection")
        .into();
    for frame in [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        FloatingSurfaceClientMessage::Present {
            request: (&request).into(),
        },
    ] {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        connection.write_all(&bytes).unwrap();
    }
    connection.flush().unwrap();

    // The shell's card holds the block for it, the size of the whole card the app draws.
    let (x, y, width, height) = crate::common::wait_for(
        &mut session,
        "the shell's card holds the app's block",
        |session| session.shell_control("Turn left onto Main Street in Tasks"),
    );
    assert!(
        (height - 176.0).abs() < 0.5,
        "the block is as tall as the card the app draws: {height}"
    );

    // The app is told to stand its card's window on that block, and to draw the body inside its
    // frame.
    let told = host_frames(&mut session, &mut connection, |told| {
        told.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, .. } if id == "route")
        })
    });
    let Some(FloatingSurfaceHostMessage::PrepareSurface {
        rect, content_rect, ..
    }) = told.iter().rev().find(|message| {
        matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, .. } if id == "route")
    })
    else {
        panic!("the app is told where its card stands: {told:?}");
    };
    let held = [x, y, x + width, y + height];
    for (told, held) in rect.iter().zip(held) {
        assert!(
            (told - held).abs() < 0.5,
            "the card's window stands on the block the shell holds: {rect:?} against {held:?}"
        );
    }
    assert!(
        (content_rect[0] - rect[0] - 8.0).abs() < 0.5
            && (content_rect[1] - rect[1] - 8.0).abs() < 0.5,
        "the body stands inside the app's soft frame: {content_rect:?} in {rect:?}"
    );
}

/// The tabs the shell keeps of `app` anywhere on it, whether a window stands in one or one waits for
/// it: the tab a window of the app takes is the app's, as every window of it is.
fn tabs_of(session: &Session, app: &str) -> Vec<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(Workspace::tabs)
            .filter(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile).is_some_and(|id| id == app)
            })
            .map(|tab| tab.id)
            .collect()
    })
}

/// The instance each start of the launcher at `handed` was told it opens, oldest first.
fn instances(handed: &Path) -> Vec<u64> {
    fs::read_to_string(handed)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().parse().expect("an instance is a number"))
        .collect()
}

/// The instance the one start the launcher made was told, once it made it.
fn the_instance(session: &mut Session, handed: &Path) -> u64 {
    crate::common::wait_for(session, "the window's process starts", |_| {
        instances(handed).first().copied()
    })
}

#[test]
fn the_first_window_of_a_hosted_app_opens_in_the_tab_the_shell_made() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let instance = the_instance(&mut session, &handed);
    let tabs = tabs_of(&session, APP);
    assert_eq!(
        tabs,
        vec![instance],
        "the window opens in the tab the shell made, which is its instance"
    );
}

#[test]
fn a_window_of_its_own_opens_in_the_tab_beside_the_one_the_app_has() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());
    // The app shows a window, in the tab the compositor's window took: what search's Ctrl+Enter
    // opens a window of its own beside.
    session
        .desktop()
        .with_policy(|policy| policy.map(a_window(11)).unwrap());
    crate::common::until(
        &mut session,
        "the app's window is on the shell",
        |session| tabs_of(session, APP).len() == 1,
    );
    let had = tabs_of(&session, APP)[0];

    let workspace = session
        .desktop()
        .with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
        })
        .expect("a workspace in front");
    apps.start_beside(
        &Launch::Package {
            app: APP.to_owned(),
            open: None,
        },
        &apps.workspace_named(&workspace),
        None,
        None,
    )
    .expect("a window of its own starts");
    let instance = the_instance(&mut session, &handed);
    let tabs = tabs_of(&session, APP);
    assert_ne!(instance, had, "the window is not the one the app had");
    assert!(
        tabs.contains(&instance),
        "the window opens in the tab the shell made beside the app's: {tabs:?}, instance {instance}"
    );
    assert_eq!(tabs.len(), 2, "a second tab of the app: {tabs:?}");
    assert_eq!(
        tabs[0], had,
        "the window the app had keeps its tab: {tabs:?}"
    );
}

#[test]
fn a_window_on_a_record_opens_in_the_tab_the_shell_made() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());

    apps.open(&Launch::Record {
        app: APP.to_owned(),
        record: RECORD.to_owned(),
        visitor: false,
    })
    .expect("the app opens on its record");
    let instance = the_instance(&mut session, &handed);
    let tabs = tabs_of(&session, APP);
    assert_eq!(
        tabs,
        vec![instance],
        "the window opens on the record, in the tab the shell made"
    );
}

/// An app floats a picture above its own window — the video its page shows, the player's film —
/// while that window stays where the person works: the session stands a window for the picture,
/// the app keeps it where its source stands as that moves, and the app's dismiss returns it.
#[test]
fn a_presented_picture_in_picture_stands_follows_and_returns() {
    use ui::floating_surface::{FloatingRole, FloatingSource, FloatingSurfaceRequest};
    use ui::vello::kurbo::{Rect, Size};

    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("started");
    let launcher = launcher_recording_pid(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = crate::common::wait_for(&mut session, "the app's process starts", |_| {
        fs::read_to_string(&handed).ok().and_then(|text| {
            let line = text.lines().find(|line| !line.trim().is_empty())?;
            let (pid, instance) = line.trim().split_once(' ')?;
            Some((pid.parse::<u32>().ok()?, instance.parse::<u64>().ok()?))
        })
    });

    // The display the picture stands on: a picture the shell's cards hold no block for is stood
    // by the session itself.
    let display = session
        .compositor()
        .outputs()
        .into_iter()
        .find_map(|output| output.rect)
        .expect("the session's display");

    // A page shows a video, and its window floats it: the picture mirrors the video where the
    // page draws it, at the picture's shape.
    let region = Rect::new(40.0, 200.0, 640.0, 540.0);
    let request = FloatingSurfaceRequest {
        id: "browser-video".into(),
        title: "A talk on maps".into(),
        role: FloatingRole::PictureInPicture,
        source: FloatingSource::AppElement {
            app_id: APP.into(),
            element_id: "video_surface".into(),
            region,
        },
        preferred_size: Size::new(360.0, 202.5),
        aspect_ratio: Some(16.0 / 9.0),
        initial_rect: None,
        presentation_rect: None,
        tracking_source: None,
    };
    let mut connection: UnixStream = ServiceLinks::new(Arc::clone(&started))
        .shell_ui(APP, instance, Some(pid))
        .expect("the app's shell-ui connection")
        .into();
    for frame in [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        FloatingSurfaceClientMessage::Present {
            request: (&request).into(),
        },
    ] {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        connection.write_all(&bytes).unwrap();
    }
    connection.flush().unwrap();

    // The session stands the picture on the display, at the shape its app asked for.
    let told = host_frames(&mut session, &mut connection, |told| {
        told.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, .. }
                if id == "browser-video")
        })
    });
    let Some(FloatingSurfaceHostMessage::PrepareSurface { rect, .. }) =
        told.iter().rev().find(|message| {
            matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, .. }
                if id == "browser-video")
        })
    else {
        panic!("the app is told where its picture stands: {told:?}");
    };
    assert!(
        (rect[2] - rect[0] - 360.0).abs() < 0.5 && (rect[3] - rect[1] - 202.5).abs() < 0.5,
        "the picture stands at the shape its app asked for: {rect:?}"
    );
    assert!(
        rect[0] >= f64::from(display.0)
            && rect[1] >= f64::from(display.1)
            && rect[2] <= f64::from(display.0 + display.2)
            && rect[3] <= f64::from(display.1 + display.3),
        "the picture stands on the display the person works on: {rect:?} in {display:?}"
    );

    // The page scrolled: the video moved in the app's window and the surface follows it.
    let mut bytes = serde_json::to_vec(&FloatingSurfaceClientMessage::UpdateRegion {
        id: "browser-video".into(),
        region: [60.0, 240.0, 660.0, 580.0],
    })
    .unwrap();
    bytes.push(b'\n');
    connection.write_all(&bytes).unwrap();
    connection.flush().unwrap();

    // The person dragged the picture by its body: it stands where they let it go, and the app is
    // told the window's place there.
    let moved = [
        rect[0] - 40.0,
        rect[1] - 30.0,
        rect[2] - 40.0,
        rect[3] - 30.0,
    ];
    let mut bytes = serde_json::to_vec(&FloatingSurfaceClientMessage::SurfaceGeometry {
        id: "browser-video".into(),
        rect: moved,
        content_rect: moved,
        input_rect: None,
    })
    .unwrap();
    bytes.push(b'\n');
    connection.write_all(&bytes).unwrap();
    connection.flush().unwrap();
    let dragged = |told: &[FloatingSurfaceHostMessage]| {
        told.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, rect, .. }
                if id == "browser-video" && (rect[0] - moved[0]).abs() < 0.5)
        })
    };
    let stood = host_frames(&mut session, &mut connection, dragged);
    assert!(
        dragged(&stood),
        "the picture's window moves where the person dragged it: {stood:?}"
    );

    // The app takes its picture back: the surface goes and the shell hears it closed.
    let mut bytes = serde_json::to_vec(&FloatingSurfaceClientMessage::Dismiss {
        id: "browser-video".into(),
    })
    .unwrap();
    bytes.push(b'\n');
    connection.write_all(&bytes).unwrap();
    connection.flush().unwrap();
    let frames = host_frames(&mut session, &mut connection, |frames| {
        frames.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::Closed { id } if id == "browser-video")
        })
    });
    assert!(
        frames.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::Closed { id } if id == "browser-video")
        }),
        "the app's picture is returned when it asks: {frames:?}"
    );
    for refused in ["present", "update_region", "surface_geometry"] {
        assert!(
            !frames.iter().any(|message| {
                matches!(message, FloatingSurfaceHostMessage::Unanswered { message, .. }
                    if message == refused)
            }),
            "the app's own picture is not refused as {refused}: {frames:?}"
        );
    }
    assert!(
        !frames.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, .. }
                if id == "browser-video")
        }),
        "the picture stands nowhere once its app takes it back: {frames:?}"
    );

    // The app floats its picture again, as a page that starts another video does: it comes back
    // where the person left it.
    let mut bytes = serde_json::to_vec(&FloatingSurfaceClientMessage::Present {
        request: (&request).into(),
    })
    .unwrap();
    bytes.push(b'\n');
    connection.write_all(&bytes).unwrap();
    connection.flush().unwrap();
    let again = host_frames(&mut session, &mut connection, dragged);
    assert!(
        dragged(&again),
        "the picture comes back where the person left it: {again:?}"
    );
}

/// What one app's window presents is its own: a picture naming another app's window is refused,
/// and nothing of it is stood on the shell.
#[test]
fn a_picture_of_another_apps_window_is_refused() {
    use ui::floating_surface::{FloatingRole, FloatingSource, FloatingSurfaceRequest};
    use ui::vello::kurbo::Size;

    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("started");
    let launcher = launcher_recording_pid(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = crate::common::wait_for(&mut session, "the app's process starts", |_| {
        fs::read_to_string(&handed).ok().and_then(|text| {
            let line = text.lines().find(|line| !line.trim().is_empty())?;
            let (pid, instance) = line.trim().split_once(' ')?;
            Some((pid.parse::<u32>().ok()?, instance.parse::<u64>().ok()?))
        })
    });

    let request = FloatingSurfaceRequest {
        id: "not-mine".into(),
        title: "Someone else's film".into(),
        role: FloatingRole::PictureInPicture,
        source: FloatingSource::AppElement {
            app_id: "so.tilo.video".into(),
            element_id: "video_surface".into(),
            region: ui::vello::kurbo::Rect::new(0.0, 0.0, 360.0, 240.0),
        },
        preferred_size: Size::new(360.0, 240.0),
        aspect_ratio: Some(1.5),
        initial_rect: None,
        presentation_rect: None,
        tracking_source: None,
    };
    let mut connection: UnixStream = ServiceLinks::new(Arc::clone(&started))
        .shell_ui(APP, instance, Some(pid))
        .expect("the app's shell-ui connection")
        .into();
    for frame in [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        FloatingSurfaceClientMessage::Present {
            request: (&request).into(),
        },
    ] {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        connection.write_all(&bytes).unwrap();
    }
    connection.flush().unwrap();

    let frames = host_frames(&mut session, &mut connection, |frames| {
        frames.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::Unanswered { message, .. }
                if message == "present")
        })
    });
    assert!(
        frames.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::Unanswered { message, .. }
                if message == "present")
        }),
        "the app is told its picture is refused: {frames:?}"
    );
    assert!(
        !frames.iter().any(|message| {
            matches!(message, FloatingSurfaceHostMessage::PrepareSurface { id, .. }
                if id == "not-mine")
        }),
        "nothing of another app's window is stood: {frames:?}"
    );
}

/// Each start the launcher at `handed` made, oldest first: its process id, the instance it was
/// told it opens, and what it was handed.
fn starts(handed: &Path) -> Vec<(u32, u64, String)> {
    fs::read_to_string(handed)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let pid = parts.next()?.parse().ok()?;
            let instance = parts.next()?.parse().ok()?;
            Some((pid, instance, parts.next().unwrap_or_default().to_owned()))
        })
        .collect()
}

/// Run `action` of `resource` the session publishes, with `arguments`, as an agent or a view calls it.
fn invoke(session: &mut Session, resource: &str, action: &str, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

/// An app whose process ends under its window leaves the card in its place, and the card's Reopen,
/// `reopen-closed` naming that window, asks for the app again on the record it had, in the tab it
/// stood in: that window, though another window of the same app closed after it.
#[test]
fn a_window_whose_app_ended_reopens_on_its_record_in_its_place() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("handed");
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s %s %s\\n' \"$$\" \"${{TILO_CONTINUITY_INSTANCE:-}}\" \"$*\" >> '{handed}'\n\
             hold\n",
            handed = handed.display(),
        ),
    )
    .unwrap();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());

    apps.open(&Launch::Record {
        app: APP.to_owned(),
        record: RECORD.to_owned(),
        visitor: false,
    })
    .expect("the app opens on its record");
    let (pid, tab, asked) = crate::common::wait_for(&mut session, "the app starts", |_| {
        starts(&handed).first().cloned()
    });
    session
        .desktop()
        .with_policy(|policy| policy.map(a_window_of(tab, pid)).unwrap());
    crate::common::wait_for(&mut session, "the window stands in its tab", |session| {
        (tabs_of(session, APP) == vec![tab]).then_some(())
    });
    let place = |session: &Session| {
        session.handle().read(|tree| {
            tree.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .enumerate()
                        .find_map(|(at, group)| {
                            group
                                .tabs
                                .iter()
                                .position(|held| held.id == tab)
                                .map(|index| (strip.id, group.id, at, index))
                        })
                })
            })
        })
    };
    let stood = place(&session).expect("the window has a place");

    // The app's process ends under its window, as a crash ends it.
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status_locked()
        .expect("kill runs");
    assert!(killed.success(), "the app's process is ended");
    crate::common::wait_for(&mut session, "the card in the window's place", |session| {
        session
            .accessible_labels()
            .join("\n")
            .contains("Tasks stopped working")
            .then_some(())
    });

    // Another window of the same app opens and the person closes it: the newest close of the app
    // is now that one, not the window whose app ended.
    session
        .desktop()
        .with_policy(|policy| policy.map(a_window(77)).unwrap());
    let other = crate::common::wait_for(&mut session, "the other window", |session| {
        tabs_of(session, APP).into_iter().find(|held| *held != tab)
    });
    invoke(
        &mut session,
        &tilo_session_core::desktop::verbs::tab_resource_id(other),
        "close-tab",
        serde_json::json!({}),
    );
    crate::common::wait_for(&mut session, "the other window closed", |session| {
        (!tabs_of(session, APP).contains(&other)).then_some(())
    });

    // The card's Reopen.
    invoke(
        &mut session,
        tilo_session_core::desktop::verbs::CHROME_RESOURCE,
        "reopen-closed",
        serde_json::json!({ "window": tab.to_string() }),
    );
    let (again, instance, handed_again) =
        crate::common::wait_for(&mut session, "the app starts again", |_| {
            starts(&handed).get(1).cloned()
        });
    assert_ne!(again, pid, "a new process of the app");
    assert_eq!(instance, tab, "it opens in the tab the window stood in");
    assert_eq!(handed_again, asked, "on the record it had");
    assert!(
        asked.contains("7a110d00-0000-4000-8000-000000000009"),
        "the launch names the record: {asked}"
    );
    assert_eq!(place(&session), Some(stood), "in the place it stood");
    assert_eq!(
        tabs_of(&session, APP),
        vec![tab],
        "the window the person closed stays closed"
    );

    session
        .desktop()
        .with_policy(|policy| policy.map(a_window_of(tab, again)).unwrap());
    crate::common::wait_for(&mut session, "the window back in place", |session| {
        let shown = session.accessible_labels().join("\n");
        (!shown.contains("stopped working") && tabs_of(session, APP) == vec![tab]).then_some(())
    });
    assert_eq!(place(&session), Some(stood), "where it stood");
}

/// The control the shell's focus ring stands on now, as assistive technology reads it: its name,
/// or `None` while the ring is not in the shell.
fn shell_focus(session: &Session) -> Option<String> {
    let windows = session.a11y_windows();
    let (_, _, tree) = windows
        .iter()
        .find(|(_, name, _)| name == "Desktop shell")?;
    let (_, node) = tree.nodes.iter().find(|(id, _)| *id == tree.focus)?;
    (node.role() != ui::accesskit::Role::Window)
        .then(|| node.label().unwrap_or_default().to_owned())
}

/// A direction a hosted window's own controls have nothing left for is the desktop's: the window
/// asks the shell over its connection, and the shell's ring takes the focus where the press
/// pointed, so the D-pad crosses the window's edge into the shell's bar and back.
#[test]
fn a_direction_a_hosted_windows_focus_has_nothing_left_for_is_the_shells() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let handed = home.home().join("started");
    let launcher = launcher_recording_pid(&home, &handed);
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));
    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) =
        crate::common::wait_for(&mut session, "the window's process starts", |_| {
            fs::read_to_string(&handed).ok().and_then(|text| {
                let line = text.lines().find(|line| !line.trim().is_empty())?;
                let (pid, instance) = line.trim().split_once(' ')?;
                Some((pid.parse::<u32>().ok()?, instance.parse::<u64>().ok()?))
            })
        });
    session
        .desktop()
        .with_policy(|policy| policy.map(a_window_of(instance, pid)).unwrap());
    crate::common::until(
        &mut session,
        "the app's window is on the shell",
        |session| tabs_of(session, APP).contains(&instance),
    );
    // The window in front holds the desktop's focus, and the shell's ring is nowhere.
    crate::common::until(&mut session, "the window in front", |session| {
        session.desktop().with_policy(|policy| policy.focused_tab()) == Some(instance)
    });
    assert_eq!(shell_focus(&session), None);

    // A direction its own controls have nothing for: the window asks the shell to move on.
    let mut connection: UnixStream = ServiceLinks::new(Arc::clone(&started))
        .shell_ui(APP, instance, Some(pid))
        .expect("the window's shell-ui connection")
        .into();
    for frame in [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        FloatingSurfaceClientMessage::FocusLeft {
            direction: ui::floating_surface::FocusDirection::Down,
            instance: None,
        },
    ] {
        let mut bytes = serde_json::to_vec(&frame).unwrap();
        bytes.push(b'\n');
        connection.write_all(&bytes).unwrap();
    }
    connection.flush().unwrap();

    let ring = crate::common::wait_for(&mut session, "the shell's focus ring", |session| {
        shell_focus(session).filter(|name| !name.is_empty())
    });
    assert!(
        !ring.is_empty(),
        "the ring stands on a control of the shell's"
    );
    assert!(
        session.compositor().surface_takes_keys(),
        "the shell answers the keys the window handed back"
    );
}
