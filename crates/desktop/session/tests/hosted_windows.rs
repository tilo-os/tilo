//! Every window of an app whose windows are hosted opens in a tab of the shell's, and that tab is
//! the window's instance: what the window asks about itself — the controls of its content, its
//! bar, its search — is answered for the tab it stands in, never for another window of its app.
//!
//! The session asks the shell for that tab whatever opened the window: the app's first window, a
//! window of its own beside the one it shows (what search opens from a row with Ctrl+Enter), and a
//! window on a record of the app. Each test here starts one of them and reads the instance the
//! spare window process the session bound to the app was told: it is the tab the shell made, and
//! the tab is the app's.

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
use tilo_desktop_policy::{MadeFor, Window};
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
/// A second tilolang app, whose waiting tab a window of [`APP`] must never take.
const OTHER: &str = "so.tilo.notes";
/// The record one test opens a window on.
const RECORD: &str = "uuid:7a110d00-0000-4000-8000-000000000009";

/// Install [`APP`] in `home` 's own app store as a tilolang app: its entrypoint runs a tilolang
/// runtime, which is what makes its windows hosted.
fn installed_windowed(home: &mut tilo_test_env::TestHome) {
    installed_tilolang(home, &[APP]);
}

/// Install each of `apps` in `home` 's own app store, the signed packages the run built from
/// `apps/tl`: a spare is bound to an app with the module its signed package ships.
fn installed_tilolang(home: &mut tilo_test_env::TestHome, apps: &[&str]) {
    let store = home.data().join("tilo-apps");
    let installed = crate::common::dev_apps::install_named(&store, apps);
    assert_eq!(
        installed.len(),
        apps.len(),
        "every app is installed: {installed:?}"
    );
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// Where the recording spare writes what it was bound to ([`recording_spare`]).
const TOLD_ENV: &str = "TILO_TEST_SPARE_TOLD";

/// A launcher that runs every spare window process the session starts as [`recording_spare`]:
/// each binding it is sent is written to `told`, its process id, app and instance on a line.
fn launcher_as_spares(home: &tilo_test_env::TestHome, told: &Path) -> tilo_test_env::StandIn {
    let test = std::env::current_exe().expect("this test's program");
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "export {TOLD_ENV}='{told}'\n\
             exec '{test}' --exact hosted_windows::recording_spare --nocapture --test-threads 1 \
             >/dev/null 2>>'{told}.stderr'\n",
            told = told.display(),
            test = test.display(),
        ),
    )
    .unwrap()
}

/// Not a test of its own: the spare window process [`launcher_as_spares`] starts, run from this
/// test binary. It takes the link the session handed it as the launcher takes it, holds the display
/// connection it is sent, and writes each binding to [`TOLD_ENV`]'s file until the session lets
/// it go.
#[test]
fn recording_spare() {
    use runtime_portal::process_link::{Packet as _, ToProcess};
    let Some(told) = std::env::var_os(TOLD_ENV) else {
        return;
    };
    // Started held, as the launcher is: the session lets it go on with the spare's link.
    launcher::launch_hold::wait_on_stdin().expect("the session lets the spare go on");
    let link = launcher::launch_hold::take_window_link().expect("a spare is handed its link");
    let mut display = None;
    while let Ok(Some(packet)) = ToProcess::receive(&link) {
        match packet {
            ToProcess::Display { wayland } => display = Some(wayland),
            ToProcess::Bind { binding, .. } => {
                let mut file = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&told)
                    .unwrap();
                writeln!(
                    file,
                    "{} {} {}",
                    std::process::id(),
                    binding.app,
                    binding.window.instance
                )
                .unwrap();
            }
            _ => {}
        }
    }
    drop(display);
}

/// Each binding a spare was sent, oldest first: its process id, the app and the instance.
fn bindings(told: &Path) -> Vec<(u32, String, u64)> {
    fs::read_to_string(told)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.parse().ok()?,
                fields.next()?.to_owned(),
                fields.next()?.parse().ok()?,
            ))
        })
        .collect()
}

/// The process id of the spare bound to `app` and the instance it was told, once one was: the
/// first binding of `app` that is not one of `before`.
fn bound(session: &mut Session, told: &Path, app: &str, before: usize) -> (u32, u64) {
    crate::common::steps_within(session, tilo_test_env::hang_bound(), |_| {
        bindings(told)
            .into_iter()
            .skip(before)
            .find(|(_, bound, _)| bound == app)
            .map(|(pid, _, instance)| (pid, instance))
    })
    .unwrap_or_else(|| {
        panic!(
            "no spare was bound to {app}; the session says {:?}, and its spares said:\n{}",
            session.accessible_labels(),
            fs::read_to_string(told.with_extension("stderr")).unwrap_or_default()
        )
    })
}

/// The workspace's collaboration socket, as far as a window reaches it: the instance of a window a
/// spare is bound for goes to its host with a connection to it, and a window whose
/// instance has none does not open.
fn collaboration_socket() -> std::os::unix::net::UnixListener {
    launcher::services::ensure_services_root().unwrap();
    let path = launcher::services::collab_socket();
    let _ = fs::remove_file(&path);
    std::os::unix::net::UnixListener::bind(path).unwrap()
}

/// The workspace in front.
fn front(session: &Session) -> String {
    session
        .desktop()
        .with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
        })
        .expect("a workspace in front")
}

/// The window a spare bound to `app` in `workspace` maps for `instance`: its connection names the
/// app, and its tag the instance.
fn a_bound_window(id: u64, pid: u32, app: &str, workspace: &str, instance: u64) -> Window {
    a_spares_window(
        id,
        pid,
        app,
        Some(instance),
        MadeFor::App {
            app: app.into(),
            workspace: workspace.into(),
        },
    )
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = bound(&mut session, &told, APP, 0);
    let workspace = front(&session);
    session.desktop().with_policy(|policy| {
        policy
            .map(a_bound_window(1, pid, APP, &workspace, instance))
            .unwrap();
    });
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = bound(&mut session, &told, APP, 0);
    let workspace = front(&session);
    session.desktop().with_policy(|policy| {
        policy
            .map(a_bound_window(instance, pid, APP, &workspace, instance))
            .unwrap();
    });
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = bound(&mut session, &told, APP, 0);

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

#[test]
fn the_first_window_of_a_hosted_app_opens_in_the_tab_the_shell_made() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (_, instance) = bound(&mut session, &told, APP, 0);
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
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

    let workspace = front(&session);
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
    let (_, instance) = bound(&mut session, &told, APP, 0);
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());

    apps.open(&Launch::Record {
        app: APP.to_owned(),
        record: RECORD.to_owned(),
        visitor: false,
    })
    .expect("the app opens on its record");
    let (_, instance) = bound(&mut session, &told, APP, 0);
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = bound(&mut session, &told, APP, 0);

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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));

    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = bound(&mut session, &told, APP, 0);

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
/// `reopen-closed` naming that window, binds a new spare to the app for the tab it stood in: that
/// window, though another window of the same app closed after it. The record it had rides that
/// window's instance to the app's host; the close it reopens keeps it (`closed_tabs`).
#[test]
fn a_window_whose_app_ended_reopens_on_its_record_in_its_place() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_windowed(&mut home);
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());

    apps.open(&Launch::Record {
        app: APP.to_owned(),
        record: RECORD.to_owned(),
        visitor: false,
    })
    .expect("the app opens on its record");
    let (pid, tab) = bound(&mut session, &told, APP, 0);
    let workspace = front(&session);
    session.desktop().with_policy(|policy| {
        policy
            .map(a_bound_window(tab, pid, APP, &workspace, tab))
            .unwrap();
    });
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
    // The stand-in spare runs no app to name its window, so the card names the window by its
    // record; a real spare names it by the app's title.
    crate::common::wait_for(&mut session, "the card in the window's place", |session| {
        let shown = session.accessible_labels();
        (shown.iter().any(|label| label.contains("stopped working"))
            && shown.iter().any(|label| label == "Reopen"))
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
    let (again, instance) = bound(&mut session, &told, APP, 1);
    assert_ne!(again, pid, "a new process of the app");
    assert_eq!(instance, tab, "it opens in the tab the window stood in");
    assert_eq!(place(&session), Some(stood), "in the place it stood");
    assert_eq!(
        tabs_of(&session, APP),
        vec![tab],
        "the window the person closed stays closed"
    );

    // The ended process's window is gone by the time the new one maps its own.
    session.desktop().with_policy(|policy| {
        policy.unmap(tab);
        policy
            .map(a_bound_window(tab + 1, again, APP, &workspace, tab))
            .unwrap();
    });
    let back = crate::common::steps_within(&mut session, tilo_test_env::hang_bound(), |session| {
        let shown = session.accessible_labels().join("\n");
        (!shown.contains("stopped working") && tabs_of(session, APP) == vec![tab]).then_some(())
    });
    assert!(
        back.is_some(),
        "the window back in its tab {tab}: the app's tabs are {:?}, the spares were bound {:?}, the \
         session says {:?}",
        tabs_of(&session, APP),
        bindings(&told),
        session.accessible_labels(),
    );
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
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let started: Arc<Launches> = Arc::new(Launches::default());
    let mut session = session_on(apps.clone(), Arc::clone(&started));
    apps.open(&Launch::Package {
        app: APP.to_owned(),
        open: None,
    })
    .expect("the app is opened");
    let (pid, instance) = bound(&mut session, &told, APP, 0);
    let workspace = front(&session);
    session.desktop().with_policy(|policy| {
        policy
            .map(a_bound_window(1, pid, APP, &workspace, instance))
            .unwrap();
    });
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

/// [`OTHER`] opened in the workspace in front, its launch started: its process id, the tab the
/// shell holds for its window, and that workspace.
fn other_app_waiting(session: &mut Session, apps: &Apps, told: &Path) -> (u32, u64, String) {
    apps.open(&Launch::Package {
        app: OTHER.to_owned(),
        open: None,
    })
    .expect("the other app is opened");
    let (pid, instance) = bound(session, told, OTHER, 0);
    assert_eq!(
        tabs_of(session, OTHER),
        vec![instance],
        "the shell holds the other app's tab"
    );
    let workspace = front(session);
    (pid, instance, workspace)
}

/// A window of a connection the session made for a spare window process, `made_for` what the
/// session bound it to, which calls itself `claims` and asks for the instance `tag`.
fn a_spares_window(id: u64, pid: u32, claims: &str, tag: Option<u64>, made_for: MadeFor) -> Window {
    Window {
        id,
        pid: Some(pid),
        app_id: Some(claims.into()),
        title: Some("Settings".into()),
        tag: tag.map(|tag| tag.to_string()),
        made_for: Some(made_for),
        ..Window::default()
    }
}

/// The window the shell shows in `tab`, if any.
fn window_in(session: &Session, tab: u64) -> Option<u64> {
    session
        .desktop()
        .with_policy(|policy| policy.window_of_tab(tab))
}

/// The tab `window` stands in, if any.
fn tab_of(session: &Session, window: u64) -> Option<u64> {
    session
        .desktop()
        .with_policy(|policy| policy.tab_of(window))
}

/// The other app's own window, made by the spare bound to it, takes the tab held for it: that tab
/// still waited for it.
fn other_takes_its_tab(session: &mut Session, pid: u32, instance: u64, workspace: &str) {
    session.desktop().with_policy(|policy| {
        policy
            .map(a_bound_window(90, pid, OTHER, workspace, instance))
            .unwrap();
    });
    assert_eq!(
        tab_of(session, 90),
        Some(instance),
        "the other app's window takes the tab held for it"
    );
}

/// A process bound to an app is that app whatever its windows say: one that calls itself Settings
/// and asks for the instance of another app's waiting tab shows as the app it was bound to, in a
/// tab of that app's, and the other app's tab still waits for its own window.
#[test]
fn a_bound_process_claiming_another_app_and_its_instance_shows_its_own_app() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_tilolang(&mut home, &[APP, OTHER]);
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());
    let (pid, instance, workspace) = other_app_waiting(&mut session, &apps, &told);

    session.desktop().with_policy(|policy| {
        policy
            .map(a_spares_window(
                21,
                4_000_001,
                "so.tilo.settings",
                Some(instance),
                MadeFor::App {
                    app: APP.into(),
                    workspace: workspace.clone(),
                },
            ))
            .unwrap();
    });
    crate::common::until(
        &mut session,
        "the bound window is on the shell",
        |session| tab_of(session, 21).is_some(),
    );
    let tab = tab_of(&session, 21).unwrap();
    assert_eq!(
        tabs_of(&session, APP),
        vec![tab],
        "it shows as the app it was bound to"
    );
    assert!(
        tabs_of(&session, "so.tilo.settings").is_empty(),
        "nothing on the shell is Settings"
    );
    assert_ne!(tab, instance, "it takes no other app's tab");
    assert_eq!(
        window_in(&session, instance),
        None,
        "the other app's tab still waits"
    );
    other_takes_its_tab(&mut session, pid, instance, &workspace);
}

/// The system may give a spare window process the id of a process another app's launch started:
/// a window of the spare's connection, bound to its own app, takes nothing held for that launch.
#[test]
fn a_reused_process_id_takes_nothing() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_tilolang(&mut home, &[APP, OTHER]);
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());
    let (pid, instance, workspace) = other_app_waiting(&mut session, &apps, &told);

    // The spare's process has the very id the other app's launch had, and its window says it is
    // the other app.
    session.desktop().with_policy(|policy| {
        policy
            .map(a_spares_window(
                22,
                pid,
                OTHER,
                None,
                MadeFor::App {
                    app: APP.into(),
                    workspace: workspace.clone(),
                },
            ))
            .unwrap();
    });
    assert_ne!(
        tab_of(&session, 22),
        Some(instance),
        "the window takes no tab held for the launch whose process id it has"
    );
    assert_eq!(
        window_in(&session, instance),
        None,
        "the other app's tab still waits"
    );
    crate::common::until(
        &mut session,
        "the bound window is on the shell",
        |session| tab_of(session, 22).is_some(),
    );
    assert_eq!(
        tabs_of(&session, APP),
        vec![tab_of(&session, 22).unwrap()],
        "the window shows as the app its connection was bound to"
    );
    other_takes_its_tab(&mut session, pid, instance, &workspace);
}

/// A spare bound to no app yet is no app's: a window it maps gets no tab, whatever process id,
/// app or instance it has.
#[test]
fn an_unbound_spares_window_gets_no_tab() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_tilolang(&mut home, &[APP, OTHER]);
    let told = home.home().join("told");
    let launcher = launcher_as_spares(&home, &told);
    let _collaboration = collaboration_socket();
    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(apps.clone());
    let (pid, instance, workspace) = other_app_waiting(&mut session, &apps, &told);
    let before: Vec<u64> = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(Workspace::tabs)
            .map(|tab| tab.id)
            .collect()
    });

    session.desktop().with_policy(|policy| {
        policy
            .map(a_spares_window(
                23,
                pid,
                OTHER,
                Some(instance),
                MadeFor::Spare,
            ))
            .unwrap();
    });
    assert_eq!(
        tab_of(&session, 23),
        None,
        "the unbound spare's window gets no tab"
    );
    assert_eq!(
        window_in(&session, instance),
        None,
        "the other app's tab still waits"
    );
    let after: Vec<u64> = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(Workspace::tabs)
            .map(|tab| tab.id)
            .collect()
    });
    assert_eq!(before, after, "no tab came or went");
    other_takes_its_tab(&mut session, pid, instance, &workspace);
}
