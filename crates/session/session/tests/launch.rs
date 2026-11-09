//! Opening an app in the headless native session, driven only through what the session publishes: a
//! search row of one of the machine's own apps starts its program in the session's environment, and
//! one that stops on its own leaves a notice in its place.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use std::fs;

use std::io::{BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_action_policy::{ActionKind, Answer, Context};
use tilo_actions::ResourceSpec;
use tilo_app_launch::Connects as _;
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE, SEARCH_RESOURCE, WINDOW_KIND};
use tilo_desktop_model::TileContent;
use tilo_desktop_policy::{DesktopPolicy, Window};
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::mount::app_tabs::ServiceLinks;
use tilo_session::mount::apps::Apps;
use tilo_session::{AppActions, AppLists, Launches, Lists, Options, Session};
use ui::floating_surface::{FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage};

/// A home of its own with `body` as the desktop entry `desktop_id` starts,
/// the entry declaring `lines` besides its name and command, and the path
/// that body was written to.
fn host_app_declaring(
    home: &tilo_test_env::TestHome,
    desktop_id: &str,
    lines: &str,
    body: &str,
) -> PathBuf {
    let applications = home.data().join("applications");
    fs::create_dir_all(&applications).unwrap();
    let program = home.home().join("probe");
    tilo_test_env::script(&program, body).unwrap();
    fs::write(
        applications.join(desktop_id),
        format!(
            "[Desktop Entry]\nType=Application\nName=Probe\n{lines}Exec={} %f\n",
            program.display()
        ),
    )
    .unwrap();
    desktop_entry::cache::invalidate();
    program
}

/// A home of its own with `body` as the desktop entry `desktop_id` starts,
/// claiming `mime` when given, and the path that body was written to.
fn host_app_claiming(
    home: &tilo_test_env::TestHome,
    desktop_id: &str,
    body: &str,
    mime: Option<&str>,
) -> PathBuf {
    let claim = mime.map_or_else(String::new, |mime| format!("MimeType={mime}\n"));
    host_app_declaring(home, desktop_id, &claim, body)
}

/// A home of its own with `body` as the desktop entry `desktop_id` starts,
/// and the path that body was written to.
fn host_app(home: &tilo_test_env::TestHome, desktop_id: &str, body: &str) -> PathBuf {
    host_app_claiming(home, desktop_id, body, None)
}

/// This device's lists as these tests find apps in it: the fixture's own host app, resolved through
/// the entry the session's scan reads.
fn host_lists() -> Lists {
    Lists::fixed(AppLists {
        host_apps: vec![HostAppEntry {
            desktop_id: "org.example.Probe".into(),
            name: "Probe".into(),
            ..HostAppEntry::default()
        }],
        ..AppLists::default()
    })
}

/// Run frames until `done` holds.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    common::wait_for(session, what, |session| done(session).then_some(()));
}

fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

fn invoke(session: &mut Session, resource: &str, action: &str, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

/// Open search on `query` and choose the row `title` through the actions search publishes, as a
/// person typing and pressing it does.
fn choose(session: &mut Session, query: &str, title: &str) {
    invoke(
        session,
        DESK_RESOURCE,
        "search",
        serde_json::json!({"query": query}),
    );
    until(session, "search shows the app", |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some_and(|search| {
            search
                .children
                .iter()
                .any(|row| row.label.starts_with(title))
        })
    });
    let row = find(&session.resources(), SEARCH_RESOURCE)
        .expect("search")
        .children
        .into_iter()
        .find(|row| row.label.starts_with(title))
        .expect("the row");
    invoke(
        session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
}

#[test]
fn choosing_a_host_app_starts_it_through_the_launcher_on_the_sessions_display() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let socket = home.runtime().join("wayland-7");
    let records = home.home().join("records");
    host_app(&home, "org.example.Probe.desktop", "#!/bin/sh\nexit 0\n");
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, _apps, _launches) = session_through(&home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the launcher starts", |_| records.exists());

    assert_eq!(
        handed(&records, "--desktop-id"),
        "org.example.Probe",
        "the entry the person chose"
    );
    // The launcher derives WAYLAND_DISPLAY and XDG_RUNTIME_DIR from the socket; the session hands
    // it this session's, never the ones its own process inherited.
    assert_eq!(
        handed(&records, "--wayland-socket"),
        socket.to_str().unwrap(),
        "the launcher is told this session's display"
    );
}

#[test]
fn a_document_opens_in_the_app_that_claims_its_type() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let records = home.home().join("records");
    host_app_claiming(
        &home,
        "org.example.Reader.desktop",
        "#!/bin/sh\nexit 0\n",
        Some("text/plain"),
    );
    let document = home.home().join("my notes.txt");
    fs::write(&document, b"notes").unwrap();
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, apps, _launches) = session_through(&home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    // The link a browser or a file manager hands over: the path inside it is percent-encoded, the
    // path the app opens is not.
    let uri = format!(
        "file://{}",
        ui::uri::encode_component(document.to_str().unwrap())
    );
    apps.open_uri("", &uri).expect("the document opens");
    until(&mut session, "the launcher starts", |_| records.exists());

    assert_eq!(
        handed(&records, "--desktop-id"),
        "org.example.Reader",
        "the app that claims the type"
    );
    assert!(
        fs::read_to_string(&records)
            .unwrap()
            .contains(&format!("--open {}", document.display())),
        "the app is told which document to open: {:?}",
        fs::read_to_string(&records).unwrap()
    );
}

#[test]
fn an_app_that_stops_on_its_own_leaves_a_notice_in_its_place() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    host_app(&home, "org.example.Probe.desktop", "#!/bin/sh\nexit 3\n");
    // The launcher is the app's process: it ends with it.
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "exit 3\n").unwrap();

    let (mut session, _apps, _launches) = session_through(&home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the notice is shown", |session| {
        find(&session.resources(), DESK_RESOURCE).is_some_and(|desk| {
            desk.children
                .iter()
                .any(|child| child.kind == WINDOW_KIND && child.label.contains("Probe"))
        })
    });
}

/// A window of `app` the compositor mapped, its bar saying the app's own id: the window of an app
/// run without its list, whose tab is named for the app.
fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..Window::default()
    }
}

/// A window of `app` whose bar says `title` , the place it shows.
fn titled_window(id: u64, app: &str, title: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(title.into()),
        ..Window::default()
    }
}

#[test]
fn a_second_launch_raises_the_window_the_app_already_has() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let records = home.home().join("records");
    host_app_declaring(
        &home,
        "org.example.Probe.desktop",
        "SingleMainWindow=true\n",
        "#!/bin/sh\nexit 0\n",
    );
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, _apps, _launches) = session_through(&home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the launcher starts", |_| records.exists());

    // The app's window is on the desk, showing another app's window in front.
    session.desktop().with_policy(|policy| {
        policy.map(window(11, "org.example.Probe")).unwrap();
        policy.map(window(12, "org.example.Other")).unwrap();
    });
    until(&mut session, "both windows are on the desk", |session| {
        session
            .desktop()
            .with_policy(|policy| policy.app_tabs_in_front().len())
            == 2
    });
    assert_eq!(
        session.desktop().with_policy(|policy| policy.focused()),
        Some(12),
        "the other app's window is the one in front"
    );

    choose(&mut session, "probe", "Probe");
    // A second process would have written its line by now.
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        fs::read_to_string(&records).unwrap().lines().count(),
        1,
        "the app is not started a second time"
    );
    assert_eq!(
        session.desktop().with_policy(|policy| policy.focused()),
        Some(11),
        "the window the app already has came to the front"
    );
}

/// The app the tab tests install: a Tilo package whose window is a tab of the desktop.
const PROBE: &str = "so.tilo.probe";
/// What the person reads for [`PROBE`]: the name its package gives it, which its window bar says
/// and its tab takes.
const PROBE_NAME: &str = "Probe";

/// Install [`PROBE`] in `home` 's own app store, and write the launcher the session starts it
/// through: it records the pid of every start and runs, as the app would, until the test is over.
fn installed_probe(home: &mut tilo_test_env::TestHome, started: &Path) -> tilo_test_env::StandIn {
    installed_probe_package(home);
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\nhold\n",
            started = started.display(),
        ),
    )
    .unwrap()
}

/// Install [`PROBE`] in `home` 's own app store: a package whose entrypoint is the launcher's to
/// start.
fn installed_probe_package(home: &mut tilo_test_env::TestHome) {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(PROBE);
    fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest = payload.join("manifest.toml");
    fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{PROBE}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"Probe\"\n\
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
             bin = \"bin/app\"\n\
             type = \"gui\"\n\
             \n\
             [providers]\n\
             schemes = [\"probe\"]\n",
            key = "11".repeat(32),
        ),
    )
    .unwrap();
    fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{PROBE}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .unwrap();
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// This computer's own: a device certified for a person's root key, kept in an identity root of the
/// test's own, and the person's id — the one their workspaces' governance names. The root lives as
/// long as the caller keeps it: the session reads the device's key from it.
fn this_computer(home: &mut tilo_test_env::TestHome) -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &home.config().join("tilo").join("identity"),
        &paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    home.env().set("TILO_IDENTITY_ROOT", root.path());
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this computer's certificate");
    (root, hex::encode(cert.user_public_key))
}

/// A launcher that records the pid of every start and what it was handed, and runs, as the app
/// would, until the test is over.
fn recording_launcher(
    home: &tilo_test_env::TestHome,
    started: &Path,
    records: &Path,
) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             printf '%s\\n' \"$*\" >> '{records}'\n\
             hold\n",
            started = started.display(),
            records = records.display(),
        ),
    )
    .unwrap()
}

/// A launcher that records what the session handed it, and runs as an app would until the test is
/// over: the launcher is a host app's process, and it ends when the app does.
fn recording_host_launcher(
    home: &tilo_test_env::TestHome,
    records: &Path,
) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$*\" >> '{records}'\nhold\n",
            records = records.display()
        ),
    )
    .unwrap()
}

/// What the last start the launcher at `records` made was handed for `flag` : the argument that
/// follows it.
fn handed(records: &Path, flag: &str) -> String {
    let recorded = fs::read_to_string(records).expect("the launcher recorded its start");
    let arguments: Vec<&str> = recorded
        .lines()
        .next_back()
        .expect("a line per start")
        .split_whitespace()
        .collect();
    let at = arguments
        .iter()
        .position(|argument| *argument == flag)
        .unwrap_or_else(|| panic!("the launch carries {flag}: {arguments:?}"));
    arguments
        .get(at.saturating_add(1))
        .unwrap_or_else(|| panic!("{flag} names a value: {arguments:?}"))
        .to_string()
}

/// A session whose apps start from `home` 's own app store, the record of the launches it makes,
/// and the launcher they start through.
fn probe_session(
    home: &mut tilo_test_env::TestHome,
    started: &Path,
) -> (Session, Rc<Apps>, Arc<Launches>, tilo_test_env::StandIn) {
    probe_session_on(home, started, None)
}

/// The same session with the desktop services of a login running beside it,
/// which is where the questions the person answers on the sheet are asked.
fn probe_session_with_services(
    home: &mut tilo_test_env::TestHome,
    started: &Path,
    bus: &tilo_test_env::PrivateBus,
) -> (Session, Rc<Apps>, Arc<Launches>, tilo_test_env::StandIn) {
    probe_session_on(home, started, Some(bus))
}

/// The session `home` starts its apps in, with `bus` 's desktop services beside it when there is a
/// login to serve.
fn probe_session_on(
    home: &mut tilo_test_env::TestHome,
    started: &Path,
    bus: Option<&tilo_test_env::PrivateBus>,
) -> (Session, Rc<Apps>, Arc<Launches>, tilo_test_env::StandIn) {
    let probe = installed_probe(home, started);
    let (session, apps, launches) = session_through(home, &probe, bus);
    (session, apps, launches, probe)
}

/// A session over `home` 's own app store whose packages start through `launcher` .
fn session_through(
    home: &tilo_test_env::TestHome,
    launcher: &tilo_test_env::StandIn,
    bus: Option<&tilo_test_env::PrivateBus>,
) -> (Session, Rc<Apps>, Arc<Launches>) {
    let socket = home.runtime().join("wayland-7");
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(socket);
    let apps = Rc::new(Apps::with_launcher(
        environment,
        None,
        Some(launcher.path().to_owned()),
    ));
    let started = Arc::new(Launches::default());
    let wake = tilo_service::LoopWake::default();
    let desktop = bus.map(|bus| {
        let (side, _link) = tilo_session::portal_link::channel(wake.handle());
        Rc::new(
            DesktopServices::start(
                &Buses {
                    session: Bus::Address(bus.address().to_owned()),
                    system: Bus::None,
                },
                &wake.handle(),
                Arc::new(tilo_portal::NoCapture),
                &side,
                None,
            )
            .expect("desktop services"),
        )
    });
    let services = desktop
        .as_ref()
        .map_or_else(Vec::new, |desktop| desktop.services());
    let session = crate::common::headless(Options {
        lists: host_lists(),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        launches: Arc::clone(&started),
        services,
        wake,
        // A login's services take part in no collaboration plane of their own; the tests of the
        // desk alone do.
        collab: desktop.is_none(),
        desktop,
        ..Options::default()
    });
    (session, apps, started)
}

/// What a window's app describes of its own content reaches the session by name, and the press the
/// session makes there goes back to the app on its own connection.
#[test]
fn a_windows_app_describes_the_controls_of_its_content_and_hears_the_press() {
    use ui::window_targets::{TargetAction, WindowTarget};

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    let mut connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::WindowTargets {
            targets: vec![WindowTarget {
                role: "ListItem".to_owned(),
                name: "Documents".to_owned(),
                rect: [4.0, 6.0, 44.0, 24.0],
                actions: vec![TargetAction::Activate, TargetAction::ShowContextMenu],
            }],
            instance: None,
        },
    );
    until(
        &mut session,
        "the window's own control is listed",
        |session| {
            session
                .controls()
                .iter()
                .any(|(name, _)| name == "Documents")
        },
    );
    let (_, (x, y, width, height)) = session
        .controls()
        .into_iter()
        .find(|(name, _)| name == "Documents")
        .expect("the control the app described");
    assert_eq!(
        (width, height),
        (40.0, 18.0),
        "listed at the size it is drawn, on the desk"
    );
    assert!(
        (x, y) != (4.0, 6.0),
        "listed where the window stands, not at the window's own origin"
    );
    capture_to(&mut session, "app-window-controls-listed");

    session
        .press_action("Documents", TargetAction::ShowContextMenu)
        .expect("the window's own control is pressed");

    // The press reaches the app on the connection that described it, as the control's own context
    // menu, not as a point aimed at.
    connection
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut seen = Vec::new();
    let mut pressed = false;
    for _ in 0..400 {
        session.step(Duration::from_millis(1)).unwrap();
        let mut chunk = [0u8; 512];
        match std::io::Read::read(&mut connection, &mut chunk) {
            Ok(read) => seen.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("the connection broke: {error}"),
        }
        if seen.contains(&b'\n') {
            let whole = seen
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(&seen[..0], |end| &seen[..end]);
            pressed = String::from_utf8(whole.to_vec())
                .expect("frames are text")
                .lines()
                .filter(|line| !line.trim().is_empty())
                .any(|line| {
                    serde_json::from_str::<ui::floating_surface::FloatingSurfaceHostMessage>(
                        line.trim(),
                    )
                    .is_ok_and(|frame| {
                        frame
                            == ui::floating_surface::FloatingSurfaceHostMessage::PressTarget {
                                name: "Documents".to_owned(),
                                role: "ListItem".to_owned(),
                                action: TargetAction::ShowContextMenu,
                            }
                    })
                });
            if pressed {
                break;
            }
            seen.clear();
        }
    }
    assert!(
        pressed,
        "the app is told which of its controls was pressed, and how"
    );
}

/// Every frame the session wrote back on `connection` within `steps` of its loop: whole lines, each
/// a host message.
fn host_frames(
    session: &mut Session,
    connection: &mut UnixStream,
    steps: usize,
) -> Vec<ui::floating_surface::FloatingSurfaceHostMessage> {
    connection
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..steps {
        session.step(Duration::from_millis(1)).unwrap();
        let mut chunk = [0u8; 1024];
        match std::io::Read::read(connection, &mut chunk) {
            Ok(read) => seen.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("the connection broke: {error}"),
        }
    }
    String::from_utf8(seen)
        .expect("frames are text")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line.trim()).expect("the session wrote a host message"))
        .collect()
}

/// A presented frame's marker inventory is read, not refused: the window paints its own markers at
/// the control an agent would press, and its host has nothing to add to them.
#[test]
fn a_windows_action_markers_are_read_without_the_host_refusing_them() {
    use ui::PaintedActionMarker;
    use ui::floating_surface::FloatingSurfaceHostMessage;

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    let interaction = tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::InteractionActor {
            id: "scribe".to_owned(),
            kind: tilo_actions::ActorKind::Agent,
            for_person: None,
        },
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "rename".to_owned(),
            label: "Rename".to_owned(),
            app: None,
        },
        resource: tilo_actions::ActionResource {
            id: "documents".to_owned(),
            kind: "folder".to_owned(),
            label: "Documents".to_owned(),
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    };
    let mut connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::ActionMarkers {
            markers: vec![PaintedActionMarker {
                interaction,
                viewport: [0.0, 0.0, 800.0, 600.0],
                marker: [30.0, 18.0, 14.0, 14.0],
                locus: [4.0, 6.0, 44.0, 24.0],
                protected: [4.0, 6.0, 44.0, 24.0],
            }],
        },
    );
    let answered = host_frames(&mut session, &mut connection, 300);
    assert!(
        !answered.iter().any(|frame| matches!(
            frame,
            FloatingSurfaceHostMessage::Unanswered { message, .. } if message == "action_markers"
        )),
        "the session reads the markers a window presents: {answered:?}"
    );
    assert_eq!(
        groups(&session),
        vec![vec![PROBE_NAME.to_owned()]],
        "the window whose markers were read stays on the desk"
    );
}

/// A message this session does not answer is still said so, so nothing of an app's waits on an
/// answer that will not come.
#[test]
fn a_message_the_session_cannot_answer_is_said_so() {
    use ui::floating_surface::FloatingSurfaceHostMessage;

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    // An app's card is the app's own window: the session mirrors no region of a floating surface.
    let mut connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::UpdateRegion {
            id: "primary".to_owned(),
            region: [0.0, 0.0, 320.0, 240.0],
        },
    );
    let answered = host_frames(&mut session, &mut connection, 300);
    assert!(
        answered.iter().any(|frame| matches!(
            frame,
            FloatingSurfaceHostMessage::Unanswered { message, .. } if message == "update_region"
        )),
        "a message the session cannot answer is said so: {answered:?}"
    );
}

/// The workspace in front, and who the desk holds as its owner.
fn front_workspace(session: &Session) -> (String, Option<String>) {
    session.handle().read(|tree| {
        tree.active_workspace().map_or_else(
            || (String::new(), None),
            |workspace| {
                (
                    workspace.id().to_string(),
                    workspace.owner().map(ToString::to_string),
                )
            },
        )
    })
}

/// The person who made a workspace is handed their authority in it, so the notes they open there
/// are theirs to add to: the app is started with the projection its window bar asks about, and it
/// says the person may make a note in the workspace nobody else has joined.
#[test]
fn the_person_who_made_a_workspace_may_add_a_note_in_it() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (_identity, person) = this_computer(&mut home);
    let started = home.home().join("started");
    let arguments = home.home().join("arguments");
    installed_probe_package(&mut home);
    let launcher = recording_launcher(&home, &started, &arguments);
    let (mut session, apps, _started) = session_through(&home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();

    let (desk, _) = front_workspace(&session);
    invoke(
        &mut session,
        CHROME_RESOURCE,
        "new-workspace",
        serde_json::json!({"name": "Fresh"}),
    );
    until(
        &mut session,
        "the desk holds a workspace of the person's",
        |session| {
            let (workspace, owner) = front_workspace(session);
            workspace != desk && owner.as_deref() == Some(person.as_str())
        },
    );
    let (workspace, owner) = front_workspace(&session);
    assert_eq!(
        owner.as_deref(),
        Some(person.as_str()),
        "the person who made the workspace owns it"
    );

    let (_pid, launched_into) = probe_showing(&mut session, &apps, &started);
    assert_eq!(
        launched_into, workspace,
        "the app runs in the workspace the person made"
    );

    let path = PathBuf::from(handed(&arguments, "--action-authority"));
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("current.json"),
        "the launcher binds the projection only under the name it reads"
    );
    let bytes =
        tilo_files::native::held::read(&path, collab_types::action_authority::MAX_PROJECTION_BYTES)
            .expect("the projection is readable")
            .expect("the session holds it for as long as it runs");
    let projection: collab_types::action_authority::AppActionAuthorityProjection =
        serde_json::from_slice(&bytes).expect("a projection");
    projection
        .validate()
        .expect("a projection the launcher admits");
    assert!(
        projection.matches(&workspace, PROBE, &person),
        "the projection is for this app, in this workspace, of this person: {projection:?}"
    );
    assert_eq!(
        projection.policy.owner.as_deref(),
        Some(person.as_str()),
        "the workspace's own governance names them its owner"
    );
    assert_eq!(
        may_make_a_note(&projection),
        Answer::Allow,
        "the notes window bar offers New note: the person may make a note in the workspace"
    );

    capture_to(&mut session, "fresh-workspace-new-note");
}

/// What the projection says about the app's own new-note action: the question a window bar asks to
/// offer New note.
fn may_make_a_note(
    projection: &collab_types::action_authority::AppActionAuthorityProjection,
) -> Answer {
    projection
        .authority(&collab_types::action_authority::foreign_device_bound())
        .decide(
            &projection.app_node().action("new-note"),
            ActionKind::Undoable,
            &Context::at_secs(tilo_clock::unix_now()),
        )
        .answer
}

/// The window groups of the workspace in front, each with its tabs' titles in order: the tabs a
/// person sees.
fn groups(session: &Session) -> Vec<Vec<String>> {
    session.handle().read(|tree| {
        tree.active_workspace().map_or_else(Vec::new, |workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .map(|group| group.tabs.iter().map(|tab| tab.title.clone()).collect())
                .collect()
        })
    })
}

/// The window each tab of the workspace in front shows, in order.
fn shown_windows(session: &Session) -> Vec<Option<u64>> {
    session.handle().read(|tree| {
        tree.active_workspace().map_or_else(Vec::new, |workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .map(|tab| match tab.tile {
                    TileContent::Foreign { surface_id, .. } => Some(surface_id),
                    _ => None,
                })
                .collect()
        })
    })
}

/// Open [`PROBE`] as the desk does, then its window as the compositor would,
/// and hand back the process it runs in and the workspace it runs in.
fn probe_showing(session: &mut Session, apps: &Apps, started: &Path) -> (u32, String) {
    let workspace = session
        .desktop()
        .with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
        })
        .expect("a workspace in front");
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.to_owned(),
            open: None,
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the app starts");
    until(session, "the app runs", |_| started.exists());
    let pid = fs::read_to_string(started)
        .unwrap()
        .lines()
        .next()
        .expect("the app's pid")
        .trim()
        .parse()
        .expect("a pid");
    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)).unwrap());
    until(session, "the app's window is on the desk", |session| {
        groups(session) == vec![vec![PROBE_NAME.to_owned()]]
    });
    (pid, workspace)
}

/// Ask the session what `app` asks over a window's own shell-ui connection: the connection the
/// session makes for a window of an app it runs, which the service answers for that app's tab. The
/// connection stays open for the caller.
fn ask(
    launches: &Arc<Launches>,
    app: &str,
    workspace: &str,
    pid: u32,
    message: &FloatingSurfaceClientMessage,
) -> UnixStream {
    let connections = ServiceLinks::new(Arc::clone(launches)).window(app, workspace, 7, Some(pid));
    let mut stream: UnixStream = connections
        .shell_ui
        .expect("the app's shell-ui connection")
        .into();
    let frames = [
        FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        },
        message.clone(),
    ];
    for frame in frames {
        let mut frame = serde_json::to_vec(&frame).unwrap();
        frame.push(b'\n');
        stream.write_all(&frame).unwrap();
    }
    stream.flush().unwrap();
    stream
}

#[test]
fn a_window_the_app_asks_for_opens_beside_its_own_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    let _connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::OpenTabBeside {
            target: None,
            background: false,
            instance: None,
        },
    );
    until(
        &mut session,
        "the tab the app asked for is beside its own",
        |session| {
            let groups = groups(session);
            groups.len() == 1 && groups[0].len() == 2
        },
    );

    // The window the app opens for it takes the tab beside its own.
    session
        .desktop()
        .with_policy(|policy| policy.map(window(12, PROBE)).unwrap());
    until(&mut session, "the new window is in its tab", |session| {
        shown_windows(session) == vec![Some(11), Some(12)]
    });
    assert_eq!(
        groups(&session),
        vec![vec![PROBE_NAME.to_owned(), PROBE_NAME.to_owned()]],
        "the window took the tab beside the app's own"
    );
}

/// Tell the session `message` on a window's own connection, as the app does.
fn tell(connection: &mut UnixStream, message: &FloatingSurfaceClientMessage) {
    let mut frame = serde_json::to_vec(message).unwrap();
    frame.push(b'\n');
    connection.write_all(&frame).unwrap();
    connection.flush().unwrap();
}

/// A sideways wheel the app under the pointer did not scroll with pans the strip its window stands
/// in, by the travel it left: the desk moves the window, and everything the app drew in it, by that
/// much.
#[test]
fn a_sideways_wheel_the_app_leaves_pans_the_strip_its_window_stands_in() {
    use ui::window_targets::{TargetAction, WindowTarget};

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    // The window in front describes a control of its content, so where it stands on the desk is
    // something this test reads.
    let mut connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::WindowTargets {
            targets: vec![WindowTarget {
                role: "ListItem".to_owned(),
                name: "Documents".to_owned(),
                rect: [4.0, 6.0, 44.0, 24.0],
                actions: vec![TargetAction::Activate],
            }],
            instance: None,
        },
    );
    // More windows than the desk shows at once, each of an app of its own,
    // so its strip has somewhere to pan to.
    for id in 12..=15 {
        session
            .desktop()
            .with_policy(|policy| policy.map(window(id, "org.example.Other")).unwrap());
    }
    let shown: Vec<Option<u64>> = (11..=15).map(Some).collect();
    until(
        &mut session,
        "every window of the strip is shown",
        |session| shown_windows(session) == shown,
    );
    // The app's own window in front again, where its control is drawn.
    session
        .desktop()
        .with_policy(|policy| assert!(policy.focus(11), "the app's window comes forward"));
    until(&mut session, "the window's control is listed", |session| {
        session
            .controls()
            .iter()
            .any(|(name, _)| name == "Documents")
    });

    let control = |session: &Session| {
        session
            .controls()
            .into_iter()
            .find(|(name, _)| name == "Documents")
            .map(|(_, rect)| rect)
    };
    let (at, _, _, _) = control(&session).expect("the control the app described");

    tell(
        &mut connection,
        &FloatingSurfaceClientMessage::SidewaysWheel { travel: 160.0 },
    );
    let mut panned = 0.0;
    until(
        &mut session,
        "the strip pans what the app leaves",
        |session| {
            let Some((now, _, _, _)) = control(session) else {
                return false;
            };
            panned = at - now;
            panned.abs() > 100.0
        },
    );
    assert!(
        (panned - 160.0).abs() < 2.0,
        "the windows move by the travel the app left: {panned}"
    );

    // The rest of the gesture pans it on, event by event.
    tell(
        &mut connection,
        &FloatingSurfaceClientMessage::SidewaysWheel { travel: 60.0 },
    );
    until(
        &mut session,
        "every event the app leaves pans the strip",
        |session| {
            let Some((now, _, _, _)) = control(session) else {
                return false;
            };
            panned = at - now;
            panned.abs() > 200.0
        },
    );
    assert!(
        (panned - 220.0).abs() < 2.0,
        "the strip pans by the whole gesture the app left: {panned}"
    );
}

#[test]
fn an_address_the_app_does_not_claim_opens_no_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    let _connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::OpenTabBeside {
            target: Some("https://example.invalid/notes".into()),
            background: false,
            instance: None,
        },
    );
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        groups(&session),
        vec![vec![PROBE_NAME.to_owned()]],
        "an address the app does not declare opens nothing"
    );
    assert_eq!(
        fs::read_to_string(&started).unwrap().lines().count(),
        1,
        "and starts nothing"
    );
}

/// Where this test's captures are written: in the build's own directory when it runs somewhere else
/// than where they are read (a remote builder mirrors that directory back), else under the home's
/// comparison captures.
fn shots_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SHELL_UI_SHOTS_OUT") {
        return PathBuf::from(dir);
    }
    // The run says where its target directory is; else this binary stands in
    // `target/<profile>/deps` , and its build's directory is the one holding the profile.
    let binary = std::env::current_exe().expect("the running test's path");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            binary
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        });
    if let Some(target) = target {
        return target.join("shell-ui-shots");
    }
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home).join(".cache/visual-compare/shell-ui")
}

/// Write the frame the session shows now, and the names it gives what it shows, beside it.
fn capture_to(session: &mut Session, name: &str) {
    let dir = shots_dir();
    fs::create_dir_all(&dir).expect("create the shots directory");
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("screenshot");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("write the frame");
    fs::write(
        dir.join(format!("{name}.labels")),
        session.accessible_labels().join("\n"),
    )
    .expect("write the labels");
}

/// The slots a window of an app in its own process declares over its
/// `shell-ui` connection stand in its window's bar in the running session,
/// and an ask about the device's own shell is answered on the connection
/// that made it rather than left waiting.
#[test]
fn an_apps_own_slots_stand_in_its_windows_bar_in_a_running_session() {
    use ui::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);
    let toolbar = WindowToolbar {
        location: Some(ToolbarLocation {
            label: "Home".to_owned(),
            editable: true,
            ..ToolbarLocation::default()
        }),
        actions: vec![ToolbarAction::new("save", "plus", "Save")],
        ..WindowToolbar::default()
    };
    let _connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::Toolbar {
            toolbar: Some(toolbar),
            instance: None,
        },
    );
    until(
        &mut session,
        "the app's slot is in its window's bar",
        |session| {
            session
                .accessible_labels()
                .iter()
                .any(|label| label == "Save")
        },
    );
    capture_to(&mut session, "app-slots-in-the-bar");

    // The session serves no question about the device's own shell here, so the app is told the
    // target stays closed on the connection it asked on.
    let mut asking = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::AllowHostShell,
    );
    asking
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    // The session answers on its own pass, so the answer arrives while it steps; nothing waits on a
    // read while the desk stands still.
    let mut seen = Vec::new();
    for _ in 0..400 {
        session.step(Duration::from_millis(1)).unwrap();
        let mut chunk = [0u8; 512];
        match std::io::Read::read(&mut asking, &mut chunk) {
            Ok(read) => seen.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("the connection broke: {error}"),
        }
        if seen.contains(&b'\n') {
            break;
        }
    }
    let whole = seen
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&seen[..0], |end| &seen[..end]);
    let frames: Vec<ui::floating_surface::FloatingSurfaceHostMessage> =
        String::from_utf8(whole.to_vec())
            .expect("frames are text")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line.trim()).expect("a shell-ui frame"))
            .collect();
    assert!(
        frames.contains(
            &ui::floating_surface::FloatingSurfaceHostMessage::HostShellAllowed { allowed: false }
        ),
        "the app is told the target stays closed: {frames:?}"
    );
}

/// The resource publishing `action` among everything the session publishes.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

/// Answer `effect` on the portal sheet's door for `handle` .
fn answer_on_the_sheet(session: &mut Session, handle: &str, effect: &str) {
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    let arguments = serde_json::json!({ "handle": handle })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&sheet, effect, &arguments)
        .expect("the sheet takes the answer");
}

/// The person's answer to the question about the device's own shell reaches the app: the question
/// stands on the portal sheet, the answer is handed to the runtime's supervisor as the allow it
/// records, and the app hears on the connection it asked on that the target is open.
#[test]
fn the_answer_to_the_device_shell_question_reaches_the_app() {
    use tilo_sheets::portal::PortalEffect;

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let bus = tilo_test_env::PrivateBus::start();
    let (mut session, apps, launches, _launcher) =
        probe_session_with_services(&mut home, &started, &bus);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);

    // The runtime's supervisor stands where the session dials it, and records the allow it is
    // handed.
    let socket = launcher::services::socket_for("ush");
    fs::create_dir_all(socket.parent().expect("a socket in a directory")).unwrap();
    let supervisor =
        std::os::unix::net::UnixListener::bind(&socket).expect("the supervisor listens");
    let recorded = std::thread::spawn(move || {
        let (stream, _) = supervisor.accept().expect("the session dials in");
        let mut line = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut line)
            .expect("the allow");
        let request: serde_json::Value = serde_json::from_str(line.trim()).expect("a request");
        assert_eq!(
            request["op"], "allow-host",
            "the allow is the only op asked for"
        );
        assert!(
            request.get("expiry").is_some(),
            "the allow names how long it lasts: {request}"
        );
        (&stream)
            .write_all(b"{\"result\":\"allowed\"}\n")
            .expect("the recorded allow is answered");
    });

    let mut asking = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::AllowHostShell,
    );
    until(&mut session, "the question is on the sheet", |session| {
        session
            .portal()
            .is_some_and(|portal| portal.dialog().is_some())
    });
    capture_to(&mut session, "device-shell-question");
    let handle = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .expect("a question is up")
        .handle;
    answer_on_the_sheet(&mut session, &handle, PortalEffect::CONFIRM.id);

    asking
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    // The answer reaches the app on a turn of the session's; a counted number of turns is not the
    // answer arriving, so the wait is for the answer itself.
    let mut seen = Vec::new();
    let heard = common::wait_maybe(&mut session, |session| {
        session.step(Duration::from_millis(1)).unwrap();
        let mut chunk = [0u8; 512];
        match std::io::Read::read(&mut asking, &mut chunk) {
            Ok(read) => seen.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("the connection broke: {error}"),
        }
        String::from_utf8_lossy(&seen)
            .contains("\"allowed\":true")
            .then_some(())
    });
    assert!(
        heard.is_some(),
        "the app hears the target is open: {:?}",
        String::from_utf8_lossy(&seen)
    );
    recorded.join().expect("the supervisor saw the allow");
}

/// The class the X11 app's window announces: no program name here matches it,
/// so the app is only found through the entry that claims it.
const X11_CLASS: &str = "tilo.test.class";

#[test]
fn reopening_a_closed_window_starts_its_app_again() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let records = home.home().join("records");
    host_app_declaring(
        &home,
        "org.example.Probe.desktop",
        &format!("StartupWMClass={X11_CLASS}\n"),
        "#!/bin/sh\nexit 0\n",
    );
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, _apps, _launches) = session_through(&home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    // The client's window: its class is the app, its title the tab's.
    session.desktop().with_policy(|policy| {
        policy
            .map(titled_window(11, X11_CLASS, PROBE_NAME))
            .unwrap();
    });
    until(&mut session, "the window is a tab", |session| {
        groups(session) == vec![vec![PROBE_NAME.to_owned()]]
    });

    // The person closes the tab, and the window goes with it.
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.close_tab_of(X11_CLASS)),
        "the tab's close is taken"
    );
    until(&mut session, "the tab is closed", |session| {
        groups(session).is_empty()
    });
    assert!(!records.exists(), "nothing had started the app yet");

    // Reopen puts the tab back and starts the app that opens it again: the class alone names that
    // app through the entry claiming it.
    assert!(
        session.desktop().with_policy(DesktopPolicy::reopen_closed),
        "the reopen is taken"
    );
    until(&mut session, "the app runs again", |_| records.exists());
    assert_eq!(
        groups(&session),
        vec![vec![PROBE_NAME.to_owned()]],
        "the tab is back where it was, waiting for the app"
    );

    // The window the app opens takes the restored tab.
    session.desktop().with_policy(|policy| {
        policy
            .map(titled_window(12, X11_CLASS, PROBE_NAME))
            .unwrap();
    });
    until(
        &mut session,
        "the app's window is in the restored tab",
        |session| shown_windows(session) == vec![Some(12)],
    );
    assert_eq!(
        groups(&session),
        vec![vec![PROBE_NAME.to_owned()]],
        "one tab, not two"
    );
    assert_eq!(
        fs::read_to_string(&records).unwrap().lines().count(),
        1,
        "the app started once"
    );
}

#[test]
fn an_app_closes_only_its_own_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, workspace) = probe_showing(&mut session, &apps, &started);
    session
        .desktop()
        .with_policy(|policy| policy.map(window(12, "org.example.Other")).unwrap());
    until(&mut session, "both apps show", |session| {
        groups(session).len() == 2
    });

    let _connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::CloseTab { instance: None },
    );
    until(&mut session, "the app's own tab is closed", |session| {
        !groups(session)
            .iter()
            .flatten()
            .any(|title| title == PROBE_NAME)
    });
    assert_eq!(
        groups(&session),
        vec![vec!["Other".to_owned()]],
        "the other app's tab stays"
    );

    // Its own reopen brings back what it closed.
    let _connection = ask(
        &launches,
        PROBE,
        &workspace,
        pid,
        &FloatingSurfaceClientMessage::ReopenClosedTab,
    );
    until(&mut session, "the closed tab is back", |session| {
        groups(session)
            .iter()
            .flatten()
            .any(|title| title == PROBE_NAME)
    });
}

#[test]
fn a_session_dropped_while_its_app_runs_leaves_no_launcher_behind() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, _launches, launcher) = probe_session(&mut home, &started);
    let (pid, _room) = probe_showing(&mut session, &apps, &started);

    // The session goes while its app runs, as when a test fails part-way: the launcher it started
    // goes with it, though the test is still here.
    drop(session);
    drop(apps);
    tilo_test_env::exited("the launcher of a session that is gone", pid);

    // And a launcher the session never got to end ends with its test.
    let (sender, ended) = std::sync::mpsc::channel();
    let mut orphan = launcher
        .script()
        .command()
        .stdin(std::process::Stdio::null())
        .spawn_locked()
        .unwrap();
    std::thread::spawn(move || {
        let _ = orphan.wait();
        let _ = sender.send(());
    });
    drop(launcher);
    tilo_test_env::recv("the launcher of a test that is over", &ended);
}
