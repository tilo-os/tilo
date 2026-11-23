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
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
};
use ui::window_targets::{TargetAction, WindowTarget};

use crate::common::PERSON;

/// The app whose window each test opens: a tilolang app, whose windows hold none of its data, so
/// each of them is an instance the session's host runs and a tab of the shell's.
const APP: &str = "so.tilo.lang.tasks";
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
    let _settled = ui::motion::testing::settled();
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
    };
    let told = host_frames(&mut session, &mut connection, |told| {
        told.contains(&pressed)
    });
    assert!(
        told.contains(&pressed),
        "the app is told which of its controls was pressed: {told:?}"
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
