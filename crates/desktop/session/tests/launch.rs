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
use tilo_desktop_model::TileContent;
use tilo_desktop_policy::Window;
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_session::app_tabs::ServiceLinks;
use tilo_session::apps::{Apps, LaunchState};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{AppActions, AppLists, Launches, Lists, Options, Session};
use tilo_session_core::desktop::verbs::{
    CHROME_RESOURCE, SEARCH_RESOURCE, SHELL_RESOURCE, WINDOW_KIND,
};
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
};

use crate::common::linked::{Builder, DEFAULT_APP};

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

/// This device's lists as these tests find apps in it: the kinds the fixture home's desktop
/// entries claim, and the fixture's own host app listed for search by its name.
fn host_lists(home: &tilo_test_env::TestHome) -> Lists {
    let entries = desktop_entry::scan::scan_dirs(&[home.data().join("applications")], &[], &[]);
    Lists::fixed(AppLists {
        host_apps: vec![HostAppEntry {
            desktop_id: "org.example.Probe".into(),
            name: "Probe".into(),
            ..HostAppEntry::default()
        }],
        candidates: entries
            .entries
            .iter()
            .filter(|entry| !entry.no_display && !entry.hidden)
            .filter_map(|entry| {
                desktop_policy::AppCandidate::host(&entry.id, entry.name.clone(), &entry.mime_types)
            })
            .collect(),
        ..AppLists::default()
    })
}

/// Run frames until `done` holds.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    crate::common::wait_for(session, what, |session| done(session).then_some(()));
}

/// The first resource `accepts`, searching each resource before its children.
fn find(
    resources: &[ResourceSpec],
    accepts: impl Fn(&ResourceSpec) -> bool + Copy,
) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if accepts(resource) {
            Some(resource.clone())
        } else {
            find(&resource.children, accepts)
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
        SHELL_RESOURCE,
        "search",
        serde_json::json!({"query": query}),
    );
    until(session, "search shows the app", |session| {
        find(&session.resources(), |resource| {
            resource.id.as_str() == SEARCH_RESOURCE
        })
        .is_some_and(|search| {
            search
                .children
                .iter()
                .any(|row| row.label.starts_with(title))
        })
    });
    let row = find(&session.resources(), |resource| {
        resource.id.as_str() == SEARCH_RESOURCE
    })
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
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let socket = home.runtime().join("wayland-7");
    let records = home.home().join("records");
    host_app(&home, "org.example.Probe.desktop", "#!/bin/sh\nexit 0\n");
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, _apps, _launches) = session_through(&mut home, &launcher, None);
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
    let mut home = tilo_test_env::TestHome::new().unwrap();
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

    let (mut session, apps, _launches) = session_through(&mut home, &launcher, None);
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
    let mut home = tilo_test_env::TestHome::new().unwrap();
    host_app(&home, "org.example.Probe.desktop", "#!/bin/sh\nexit 3\n");
    // The launcher is the app's process: it ends with it.
    let launcher = tilo_test_env::stand_in(home.home().join("tilo-launcher"), "exit 3\n").unwrap();

    let (mut session, _apps, _launches) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the notice is shown", |session| {
        find(&session.resources(), |resource| {
            resource.id.as_str() == SHELL_RESOURCE
        })
        .is_some_and(|session| {
            session
                .children
                .iter()
                .any(|child| child.kind == WINDOW_KIND && child.label.contains("Probe"))
        })
    });
}

/// The launcher the session starts `PROBE` through, recording the pid of each start: a stand-in
/// that lives until its test drops it, ending as `end` says when it does.
fn probe_launcher(
    home: &tilo_test_env::TestHome,
    started: &Path,
    end: &str,
) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\nhold\n{end}",
            started = started.display(),
        ),
    )
    .unwrap()
}

/// The pid the launcher recorded for its last start, once it has written it: the file is made
/// before the number is written, so a file that is there is not yet a file that says anything.
fn recorded_pid(started: &Path) -> Option<u32> {
    fs::read_to_string(started)
        .ok()
        .and_then(|text| text.lines().next().map(str::trim).map(str::to_owned))
        .filter(|pid| !pid.is_empty())
        .and_then(|pid| pid.parse().ok())
}

/// A window of `app` made by the process `pid`, as the compositor maps it once its app shows it.
fn window_of(id: u64, app: &str, pid: u32) -> Window {
    Window {
        pid: Some(pid),
        ..window(id, app)
    }
}

#[test]
fn a_window_a_launchs_own_process_maps_says_the_launch_opened() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, _launches, _launcher) = probe_session(&mut home, &started);
    session.step(Duration::from_millis(1)).unwrap();
    let (workspace, _) = front_workspace(&session);
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.to_owned(),
            open: None,
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the app is asked for");
    let pid = crate::common::wait_for(&mut session, "the app runs", |_| recorded_pid(&started));
    assert_eq!(
        apps.launch_state(PROBE, &workspace),
        Some(LaunchState::Arriving),
        "the launch is asked for, and no window of it has come"
    );

    session
        .desktop()
        .with_policy(|policy| policy.map(window_of(11, PROBE, pid)).unwrap());
    until(&mut session, "the launch opened", |_| {
        apps.launch_state(PROBE, &workspace) == Some(LaunchState::Landed)
    });
}

#[test]
fn an_app_that_ends_before_its_window_could_open_says_why() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_probe_package(&mut home);
    // The app says why it cannot come and goes, before any window of it appears.
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        "echo 'the display refused this app' >&2\nexit 1\n",
    )
    .unwrap();
    let (mut session, apps, _launches) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    let (workspace, _) = front_workspace(&session);
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.to_owned(),
            open: None,
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the app is asked for");

    let state = crate::common::wait_for(&mut session, "the launch could not open", |_| {
        apps.launch_state(PROBE, &workspace)
            .filter(|state| matches!(state, LaunchState::CouldNotOpen { .. }))
    });
    assert_eq!(
        state,
        LaunchState::CouldNotOpen {
            why: "the display refused this app".to_owned()
        },
        "an agent asking after the launch reads what the app itself said"
    );
    let shown = session.accessible_labels().join("\n");
    assert!(
        shown.contains("Probe could not open"),
        "the tab says the app could not open: {shown}"
    );

    // The card leads with a designed sentence, not the app's raw output, and quotes what the app
    // said under it, in view.
    let said = described(&session);
    assert!(
        said.contains("It stopped before its window appeared."),
        "the card says in the shell's words why the window did not come: {said}"
    );
    // What the app printed is kept behind the card's Details, where the person reads it.
    let detailed = crate::common::wait_for(&mut session, "the card's Details", |session| {
        session.a11y_windows().into_iter().find_map(|(_, _, tree)| {
            tree.nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Details"))
                .and_then(|(_, node)| node.description())
                .map(str::to_owned)
        })
    });
    assert!(
        detailed.contains("the display refused this app"),
        "with what the app said: {detailed}"
    );
    assert!(
        said.contains("It said: the display refused this app"),
        "with what the app said, quoted under it: {said}"
    );
}

/// What the shell's cards say of themselves, for assistive technology: their labels and the
/// descriptions that carry their messages and the words they quote.
fn described(session: &Session) -> String {
    session
        .a11y_windows()
        .into_iter()
        .flat_map(|(_, _, tree)| tree.nodes.into_iter())
        .flat_map(|(_, node)| {
            node.label()
                .map(str::to_owned)
                .into_iter()
                .chain(node.description().map(str::to_owned))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_app_whose_window_came_and_then_ended_stopped_and_did_not_fail_to_open() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_probe_package(&mut home);
    let started = home.home().join("starts");
    let launcher = probe_launcher(&home, &started, "exit 3\n");
    let (mut session, apps, _launches) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    let (workspace, _) = front_workspace(&session);
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.to_owned(),
            open: None,
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the app is asked for");
    let pid = crate::common::wait_for(&mut session, "the app runs", |_| recorded_pid(&started));
    session
        .desktop()
        .with_policy(|policy| policy.map(window_of(11, PROBE, pid)).unwrap());
    until(&mut session, "the launch opened", |_| {
        apps.launch_state(PROBE, &workspace) == Some(LaunchState::Landed)
    });

    // The app ends now: its window came before it did, so its end is an app that stopped.
    drop(launcher);
    until(&mut session, "the notice is shown", |session| {
        session
            .accessible_labels()
            .join("\n")
            .contains("Probe stopped working")
    });
    assert_eq!(
        apps.launch_state(PROBE, &workspace),
        Some(LaunchState::Landed),
        "it opened: its end is no window that failed to arrive"
    );
    let shown = session.accessible_labels().join("\n");
    assert!(
        !shown.contains("could not open"),
        "an app whose window came did not fail to open: {shown}"
    );
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
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let records = home.home().join("records");
    host_app_declaring(
        &home,
        "org.example.Probe.desktop",
        "SingleMainWindow=true\n",
        "#!/bin/sh\nexit 0\n",
    );
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, _apps, _launches) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the launcher starts", |_| records.exists());

    // The app's window is on the shell, showing another app's window in front.
    session.desktop().with_policy(|policy| {
        policy.map(window(11, "org.example.Probe")).unwrap();
        policy.map(window(12, "org.example.Other")).unwrap();
    });
    until(&mut session, "both windows are on the session", |session| {
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

/// The person's own action that takes them somewhere in an app with a window here (Message on a
/// person opening the conversation in Chat) brings that window forward, as the app's `open` does;
/// an action that takes them nowhere leaves the window in front where it is.
#[test]
fn an_action_that_takes_the_person_into_an_app_brings_its_window_forward() {
    let mut fixture = Builder::of(DEFAULT_APP).start();
    // A conversation in Chat, made by Chat's own Message action, for the action that opens it.
    let conversations = fixture
        .published(DEFAULT_APP)
        .into_iter()
        .find(|descriptor| descriptor.resource_kind.ends_with(".Conversations"))
        .unwrap_or_else(|| {
            panic!(
                "{DEFAULT_APP} publishes its conversations: {:?}",
                fixture.published(DEFAULT_APP)
            )
        })
        .address
        .resource;
    let person = fixture.person_id.clone();
    fixture
        .invoke_with(
            DEFAULT_APP,
            &conversations,
            "message_person",
            serde_json::json!({"who": {"person": person}})
                .as_object()
                .cloned()
                .expect("an object"),
        )
        .expect("the person opens a conversation with themself");
    // The conversation's own view opens it: an action of the `View` kind the app publishes on the
    // resource it shows, which is what takes the person somewhere.
    let direct = fixture.until_publishing(DEFAULT_APP, "Direct", "open");
    // A second app in front, so the app the action takes them into stands behind.
    fixture.open("so.tilo.notes");
    fixture.until_in_front("so.tilo.notes");
    fixture
        .invoke(DEFAULT_APP, &direct, "open")
        .expect("the person's own action runs");
    fixture.until_in_front(DEFAULT_APP);
    // Something that takes them nowhere leaves the window in front where it is.
    fixture.open("so.tilo.notes");
    fixture.until_in_front("so.tilo.notes");
    fixture
        .invoke(DEFAULT_APP, &direct, "unread_count")
        .expect("the person's own read runs");
    fixture
        .session
        .step(std::time::Duration::from_millis(1))
        .expect("step");
    assert_eq!(
        fixture.in_front().as_deref(),
        Some("so.tilo.notes"),
        "an action that takes the person nowhere leaves the window in front where it is"
    );
}

/// The app the tab tests install: a Tilo package whose window is a tab of the desktop.
/// The screen the session draws its shell on.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
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
    let paths = tilo_device_identity::IdentityPaths::under(root.path());
    // The person's root is kept in the account's own store, where every reader looks, sealed with
    // the device key the environment's identity root holds.
    home.env().set("TILO_IDENTITY_ROOT", root.path());
    tilo_identity::sources::test_trust::install();
    tilo_identity::provisioning::ensure_collaboration_identity(
        &tilo_identity::store::account_identity_dir(&home.config().join("tilo").join("identity")),
        &paths,
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
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
) -> (
    crate::common::held::Hosted,
    Rc<Apps>,
    Arc<Launches>,
    tilo_test_env::StandIn,
) {
    probe_session_on(home, started, None)
}

/// The same session with the desktop services of a login running beside it,
/// which is where the questions the person answers on the sheet are asked.
fn probe_session_with_services(
    home: &mut tilo_test_env::TestHome,
    started: &Path,
    bus: &tilo_test_env::PrivateBus,
) -> (
    crate::common::held::Hosted,
    Rc<Apps>,
    Arc<Launches>,
    tilo_test_env::StandIn,
) {
    probe_session_on(home, started, Some(bus))
}

/// The session `home` starts its apps in, with `bus` 's desktop services beside it when there is a
/// login to serve.
fn probe_session_on(
    home: &mut tilo_test_env::TestHome,
    started: &Path,
    bus: Option<&tilo_test_env::PrivateBus>,
) -> (
    crate::common::held::Hosted,
    Rc<Apps>,
    Arc<Launches>,
    tilo_test_env::StandIn,
) {
    let probe = installed_probe(home, started);
    let (session, apps, launches) = session_through(home, &probe, bus);
    (session, apps, launches, probe)
}

/// Whether this test's environment already names a device its daemon can serve the person as.
fn an_identity_is_here() -> bool {
    std::env::var_os("TILO_IDENTITY_ROOT").is_some_and(|root| {
        tilo_device_identity::IdentityPaths::under(std::path::PathBuf::from(root))
            .device_cert
            .exists()
    })
}

/// A session over `home` 's own app store whose packages start through `launcher` .
fn session_through(
    home: &mut tilo_test_env::TestHome,
    launcher: &tilo_test_env::StandIn,
    bus: Option<&tilo_test_env::PrivateBus>,
) -> (crate::common::held::Hosted, Rc<Apps>, Arc<Launches>) {
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
    if let Some(desktop) = &desktop {
        crate::common::person_asked_on(&desktop.asks().1);
    }
    // The device this session is the shell of needs an identity its daemon can serve the
    // person as: a test that reads no person of its own gets one of its own.
    let identity = (!an_identity_is_here()).then(|| this_computer(home));
    let options = Options {
        lists: host_lists(home),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        launches: Arc::clone(&started),
        services,
        wake,
        // A login's services take part in no peer network of their own; the tests of the
        // shell alone do.
        collab: desktop.is_none(),
        desktop,
        ..Options::default()
    };
    let mut session = crate::common::held::hosted(
        options,
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            WIDTH,
            HEIGHT,
        )],
    );
    if let Some((root, _)) = identity {
        session = session.keeping(root);
    }
    // The daemon founds the person's own workspace: it is in front once it is here.
    crate::common::wait_for(
        &mut session,
        "the daemon's workspace is in front",
        |session| {
            session
                .handle()
                .read(|tree| tree.active_workspace().is_some())
                .then_some(())
        },
    );
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
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    let mut connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
        &FloatingSurfaceClientMessage::WindowTargets {
            targets: vec![WindowTarget {
                role: "ListItem".to_owned(),
                name: "Documents".to_owned(),
                identity: None,
                rect: [4.0, 6.0, 44.0, 24.0],
                actions: vec![TargetAction::Activate, TargetAction::ShowContextMenu],
                level: None,
                selected: false,
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
                .any(|control| control.name == "Documents")
        },
    );
    let (x, y, width, height) = session
        .controls()
        .into_iter()
        .find(|control| control.name == "Documents")
        .map(|control| control.rect)
        .expect("the control the app described");
    assert_eq!(
        (width, height),
        (40.0, 18.0),
        "listed at the size it is drawn, on the session"
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
    // menu, not as a point aimed at. Where the input before it went is the session's own reading
    // of the window, which this walk of its frames leaves out.
    let documents_pressed = |told: &FloatingSurfaceHostMessage| {
        matches!(
            told,
            FloatingSurfaceHostMessage::PressTarget {
                name,
                role,
                identity,
                action,
                ..
            } if name == "Documents"
                && role == "ListItem"
                && identity.is_none()
                && *action == TargetAction::ShowContextMenu
        )
    };
    let told = host_frames(&mut session, &mut connection, |told| {
        told.iter().any(documents_pressed)
    });
    assert!(
        told.iter().any(documents_pressed),
        "the app is told which of its controls was pressed, and how: {told:?}"
    );
}

/// A window that maps after the tab the shell held for its launch left the layout (a slow window,
/// while the workspace's layout is laid over the shell) takes a tab of its own. Its app names its
/// window by the held tab, the instance it was launched with: what it describes of its content
/// still reaches the session for that window, never refused as a window the shell does not show.
#[test]
fn a_window_whose_held_tab_left_still_describes_its_controls() {
    use ui::window_targets::{TargetAction, WindowTarget};

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, _apps, launches, _launcher) = probe_session(&mut home, &started);
    open_from_the_shell(&session, PROBE);
    let pid = crate::common::wait_for(&mut session, "the app runs", |_| recorded_pid(&started));
    let held = launches
        .held_tab_of(pid)
        .expect("the shell held a tab for the launch");

    // The held tab leaves the layout before the window maps.
    session.desktop().with_policy(|policy| {
        let mut tree = policy.handle().read(Clone::clone);
        assert!(
            tree.close_tab(held).is_some(),
            "the held tab was on the shell"
        );
        policy.handle().replace_tree(tree);
    });
    session
        .desktop()
        .with_policy(|policy| policy.map(window_of(11, PROBE, pid)).unwrap());
    until(
        &mut session,
        "the app's window is on the session",
        |session| groups(session) == vec![vec![PROBE_NAME.to_owned()]],
    );
    let tab = session
        .desktop()
        .with_policy(|policy| policy.tab_of(11))
        .expect("the tab the window stands in");
    assert_ne!(tab, held, "the window took a tab of its own");

    let _connection = ask(
        &launches,
        PROBE,
        pid,
        held,
        &FloatingSurfaceClientMessage::WindowTargets {
            targets: vec![WindowTarget {
                role: "ListItem".to_owned(),
                name: "Documents".to_owned(),
                identity: None,
                rect: [4.0, 6.0, 44.0, 24.0],
                actions: vec![TargetAction::Activate],
                level: None,
                selected: false,
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
                .any(|control| control.name == "Documents")
        },
    );
}

/// Every frame the session wrote back on `connection` by the turn of its loop on which `heard`
/// holds of them: whole lines, each a host message. The wait is for the frame the caller names and
/// ends on the turn the session wrote it, never after a count of turns; a session that never writes
/// it is left at the suite's hang bound, and what it did write is handed back for the caller's
/// assertion to report.
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
        // A frame still arriving is not one yet: only the lines the session finished are read.
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

/// Whether `frames` hold the session's word that it does not answer the message sent as `name` .
fn unanswered(frames: &[FloatingSurfaceHostMessage], name: &str) -> bool {
    frames.iter().any(|frame| {
        matches!(
            frame,
            FloatingSurfaceHostMessage::Unanswered { message, .. } if message == name
        )
    })
}

/// A presented frame's marker inventory is read, not refused: the window paints its own markers at
/// the control an agent would press, and its host has nothing to add to them.
#[test]
fn a_windows_action_markers_are_read_without_the_host_refusing_them() {
    use ui::PaintedActionMarker;

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    let interaction = tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: tilo_actions::Acting::agent("scribe".to_owned(), "ada"),
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "rename".to_owned(),
            label: "Rename".to_owned(),
            app: None,
            step: None,
            creates: None,
            did: None,
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
        pid,
        tab,
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
    // The session takes a connection's messages in the order the window sent them, and says what
    // it does not answer on the turn it takes the message. So the answer to a message sent after
    // the markers ends the wait: by then the markers were taken, and a refusal of them would
    // stand before it.
    tell(
        &mut connection,
        &FloatingSurfaceClientMessage::UpdateRegion {
            id: "primary".to_owned(),
            region: [0.0, 0.0, 320.0, 240.0],
        },
    );
    let answered = host_frames(&mut session, &mut connection, |answered| {
        unanswered(answered, "update_region")
    });
    assert!(
        unanswered(&answered, "update_region"),
        "the session went on to the message after the markers: {answered:?}"
    );
    assert!(
        !unanswered(&answered, "action_markers"),
        "the session reads the markers a window presents: {answered:?}"
    );
    assert_eq!(
        groups(&session),
        vec![vec![PROBE_NAME.to_owned()]],
        "the window whose markers were read stays on the session"
    );
}

/// What this session serves nothing for besides the region a test opens with: each kind as the
/// app names it on the wire, with one message of it, in the order a test tells them. The last is
/// refused for what its connection never did (it offered no page), every other whatever the
/// window is.
fn unanswerable() -> Vec<(&'static str, FloatingSurfaceClientMessage)> {
    vec![
        // What a window embeds of other apps' resources, and what else it shares live.
        (
            "embeds",
            FloatingSurfaceClientMessage::Embeds {
                embeds: Vec::new(),
                instance: None,
            },
        ),
        (
            "shared_windows",
            FloatingSurfaceClientMessage::SharedWindows { shared: Vec::new() },
        ),
        // The resource an agent would be asked about.
        (
            "window_resource",
            FloatingSurfaceClientMessage::WindowResource { resource: None },
        ),
        // The control under a point, and the parts a person picks: answers to questions this
        // session asked no window.
        (
            "pointed",
            FloatingSurfaceClientMessage::Pointed {
                request: 1,
                control: None,
            },
        ),
        (
            "pick_answered",
            FloatingSurfaceClientMessage::PickAnswered {
                request: 1,
                answer: ui::pick_wire::PickAnswer::Targets { parts: Vec::new() },
            },
        ),
        // The answer to a device-shell operation nobody here asked for.
        (
            "device_shell_outcome",
            FloatingSurfaceClientMessage::DeviceShellOutcome {
                request: 1,
                result: Err("refused".to_owned()),
            },
        ),
        // The connection said its version when it opened: a second greeting is told so too.
        (
            "hello",
            FloatingSurfaceClientMessage::Hello {
                version: FLOATING_SURFACE_PROTOCOL_VERSION,
            },
        ),
        // A frame of a page, on a connection that offered no page.
        (
            "paint_stream",
            FloatingSurfaceClientMessage::PaintStream {
                payload: String::new(),
                page: [0.0, 0.0, 320.0, 240.0],
                scale: 1.0,
                instance: None,
                part: None,
            },
        ),
    ]
}

/// A message this session does not answer is still said so, so nothing of an app's waits on an
/// answer that will not come: every kind of message it serves nothing for is named back to the
/// app as the app sent it, in the order the app sent them.
#[test]
fn a_message_the_session_cannot_answer_is_said_so() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    // An app's card is the app's own window: the session mirrors no region of a floating surface.
    let mut connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
        &FloatingSurfaceClientMessage::UpdateRegion {
            id: "primary".to_owned(),
            region: [0.0, 0.0, 320.0, 240.0],
        },
    );
    // The rest of what this session serves nothing for, each as the app names it on the wire,
    // told on the same connection after it.
    let rest = unanswerable();
    for (_, message) in &rest {
        tell(&mut connection, message);
    }
    let answered = host_frames(&mut session, &mut connection, |answered| {
        unanswered(answered, "paint_stream")
    });
    assert!(
        unanswered(&answered, "update_region"),
        "a message the session cannot answer is said so: {answered:?}"
    );
    // Each is said so by the name the app sent it under, and in the order the app sent them: the
    // last answers the session wrote are these, one for one.
    let said: Vec<&str> = answered
        .iter()
        .filter_map(|frame| match frame {
            FloatingSurfaceHostMessage::Unanswered { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    let sent: Vec<&str> = std::iter::once("update_region")
        .chain(rest.iter().map(|(name, _)| *name))
        .collect();
    let last = &said[said.len().saturating_sub(sent.len())..];
    for (at, name) in sent.iter().enumerate() {
        assert_eq!(
            last.get(at),
            Some(name),
            "message {at} (`{name}`) is said to be unanswered where the app sent it: of {sent:?} \
             the session said {said:?}"
        );
    }
}

/// The workspace in front, and who the shell holds as its owner.
fn front_workspace(session: &Session) -> (String, Option<String>) {
    session.handle().read(|tree| {
        tree.active_workspace().map_or_else(
            || (String::new(), None),
            |workspace| {
                (
                    workspace.id().to_string(),
                    workspace.policy.owner().map(ToString::to_string),
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
    let (mut session, apps, _started) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();

    let (name, _) = front_workspace(&session);
    invoke(
        &mut session,
        CHROME_RESOURCE,
        "new-workspace",
        serde_json::json!({"name": "Fresh"}),
    );
    until(
        &mut session,
        "the session holds a workspace of the person's",
        |session| {
            let (workspace, owner) = front_workspace(session);
            workspace != name && owner.as_deref() == Some(person.as_str())
        },
    );
    let (workspace, owner) = front_workspace(&session);
    assert_eq!(
        owner.as_deref(),
        Some(person.as_str()),
        "the person who made the workspace owns it"
    );

    let (_pid, launched_into, _tab) = probe_showing(&mut session, &apps, &started);
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

/// A workspace this session joined, as a join leaves it: the id it is shared under, the slot derived
/// from that id, and no strip of this session's own — its strips are the workspace's, which come from
/// the workspace's record. A person in it who opens an app gets that app's window there,
/// never in the workspace of their own they came from: once for the first workspace they join, and
/// again for a second one.
#[test]
fn an_app_opened_in_a_joined_workspace_opens_there() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (_identity, _person) = this_computer(&mut home);
    let started = home.home().join("started");
    let arguments = home.home().join("arguments");
    installed_probe_package(&mut home);
    let launcher = recording_launcher(&home, &started, &arguments);
    let (mut session, _apps, _started) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    let (their_own, _) = front_workspace(&session);
    assert!(!their_own.is_empty(), "this session starts in a workspace");
    assert!(!apps_in(&session, &their_own).contains(&PROBE.to_owned()));

    // Where the person starts: the app they open in their own workspace runs there, so what a join
    // changes is read against a shell the same press already worked on.
    open_from_the_shell(&session, PROBE);
    until(&mut session, "the app is started", |_| {
        starts(&arguments) > 0
    });
    assert_eq!(handed(&arguments, "--workspace-id"), their_own);
    assert!(
        apps_in(&session, &their_own).contains(&PROBE.to_owned()),
        "the app's tab stands in the workspace the person opened it in"
    );

    // What a join hands the shell, through the shell's own verb: the workspace's id, and nothing
    // else of this session's.
    for (joined, name) in [
        ("0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c", "Studio"),
        ("7d3b6c1e-2f4a-4c8e-9d1b-5a6e7f8c9d0e", "Garden"),
    ] {
        invoke(
            &mut session,
            CHROME_RESOURCE,
            "new-workspace",
            serde_json::json!({"workspace": joined, "name": name}),
        );
        until(
            &mut session,
            "the session stands in the workspace",
            |session| front_workspace(session).0 == joined,
        );
        assert_ne!(joined, their_own, "the person left their own workspace");

        // The person opens the app from the shell's own launchpad, which holds the tab its window
        // takes from the frame they pressed it in.
        let before = starts(&arguments);
        open_from_the_shell(&session, PROBE);
        until(&mut session, "the app is started", |_| {
            starts(&arguments) > before
        });
        assert_eq!(
            handed(&arguments, "--workspace-id"),
            joined,
            "the app runs in the workspace the person stands in"
        );
        until(&mut session, "the window's tab stands there", |session| {
            apps_in(session, joined).contains(&PROBE.to_owned())
        });
        assert_eq!(
            front_workspace(&session).0,
            joined,
            "the session stays where the person stood"
        );
    }
}

/// How many starts the launcher at `records` has made: one line per start.
fn starts(records: &Path) -> usize {
    fs::read_to_string(records).map_or(0, |lines| lines.lines().count())
}

/// Open `app` from the shell, as a press on its tile there does: the shell makes the tab the app's
/// window takes, in the workspace the person is in, and queues the launch the session starts.
fn open_from_the_shell(session: &Session, app: &str) {
    session
        .handle()
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                app: app.to_owned(),
                beside: None,
                background: None,
                float: None,
            },
        )
        .expect("the session takes the open");
}

/// The tab the shell made for an app's window decides where the app runs: a person who opens an app
/// and whose shell moves on before the pass that starts it — a switch of theirs, a workspace that
/// arrived — gets the app in the workspace they opened it in, with that workspace's records and
/// grants, never in the one the shell stands in by then.
#[test]
fn an_app_runs_in_the_workspace_the_shell_made_its_tab_in() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (_identity, _person) = this_computer(&mut home);
    let started = home.home().join("started");
    let arguments = home.home().join("arguments");
    installed_probe_package(&mut home);
    let launcher = recording_launcher(&home, &started, &arguments);
    let (mut session, _apps, _started) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    let (their_own, _) = front_workspace(&session);

    // Another workspace of theirs, made through the shell's own verb and left for last.
    invoke(
        &mut session,
        CHROME_RESOURCE,
        "new-workspace",
        serde_json::json!({"name": "Elsewhere"}),
    );
    until(
        &mut session,
        "the session stands in the workspace made",
        |session| front_workspace(session).0 != their_own,
    );
    let (elsewhere, _) = front_workspace(&session);
    stand_in(&session, &their_own);
    until(&mut session, "the session stands there again", |session| {
        front_workspace(session).0 == their_own
    });

    // The person opens the app where they stand, and their shell moves on to the other workspace
    // before the shell's next pass — the pass that starts the app.
    open_from_the_shell(&session, PROBE);
    stand_in(&session, &elsewhere);
    until(&mut session, "the app is started", |_| {
        starts(&arguments) > 0
    });
    assert_eq!(
        front_workspace(&session).0,
        elsewhere,
        "the session stands where the person switched to"
    );
    assert_eq!(
        handed(&arguments, "--workspace-id"),
        their_own,
        "the app runs in the workspace whose tab the session made for its window"
    );
}

/// Go to the workspace `workspace` , by its id, as a person switching to it does.
fn stand_in(session: &Session, workspace: &str) {
    let slot = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .map(|held| held.id().to_string())
    });
    let slot = slot.unwrap_or_else(|| panic!("the session holds the workspace {workspace}"));
    session
        .handle()
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::SwitchWorkspace {
                workspace: Some(slot),
                delta: None,
            },
        )
        .expect("the session takes the switch");
}

/// What the projection says about the app's own `new_note` action: the question a window bar asks to
/// offer New note.
fn may_make_a_note(
    projection: &collab_types::action_authority::AppActionAuthorityProjection,
) -> Answer {
    projection
        .authority()
        .decide(
            &projection.app_node().action("new_note"),
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

/// The apps whose windows the workspace `workspace` holds on this session, by app id, wherever the
/// shell stands: the tabs it keeps, each named by the app its window belongs to.
fn apps_in(session: &Session, workspace: &str) -> Vec<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .filter(|held| held.id().to_string() == workspace)
            .flat_map(tilo_desktop_model::Workspace::tabs)
            .filter_map(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile))
            .map(String::from)
            .collect()
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

/// Open [`PROBE`] as the shell does, then its window as the compositor would,
/// and hand back the process it runs in, the workspace it runs in, and the tab
/// its window took: the instance the session names that window's own
/// connections by.
fn probe_showing(session: &mut Session, apps: &Apps, started: &Path) -> (u32, String, u64) {
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
    // The launcher records its pid with a redirect that makes the file before it writes the
    // number, so a file that is there is not yet a file that says anything.
    let recorded = || {
        fs::read_to_string(started)
            .ok()
            .and_then(|text| text.lines().next().map(str::trim).map(str::to_owned))
            .filter(|pid| !pid.is_empty())
    };
    until(session, "the app runs", |_| recorded().is_some());
    let pid = recorded()
        .expect("the launcher recorded its pid")
        .parse()
        .expect("a pid");
    // The window is the app's: its process is the launcher the session started, which a launch
    // reads as its own window having arrived.
    let mut mapped = window(11, PROBE);
    mapped.pid = Some(pid);
    session
        .desktop()
        .with_policy(|policy| policy.map(mapped).unwrap());
    until(session, "the app's window is on the session", |session| {
        groups(session) == vec![vec![PROBE_NAME.to_owned()]]
    });
    let tab = session
        .desktop()
        .with_policy(|policy| policy.tab_of(11))
        .expect("the tab the window stands in");
    (pid, workspace, tab)
}

/// Ask the session what `app` asks over the shell-ui connection of its window
/// in `tab`: the connection the session makes for a window of an app it runs,
/// which the service answers for that window's tab. The connection stays open
/// for the caller.
fn ask(
    launches: &Arc<Launches>,
    app: &str,
    pid: u32,
    tab: u64,
    message: &FloatingSurfaceClientMessage,
) -> UnixStream {
    let mut stream: UnixStream = ServiceLinks::new(Arc::clone(launches))
        .shell_ui(app, tab, Some(pid))
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
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    let _connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
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
/// in, by the travel it left: the shell moves the window, and everything the app drew in it, by that
/// much.
#[test]
fn a_sideways_wheel_the_app_leaves_pans_the_strip_its_window_stands_in() {
    use ui::window_targets::{TargetAction, WindowTarget};

    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    // The window in front describes a control of its content, so where it stands on the shell is
    // something this test reads.
    let mut connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
        &FloatingSurfaceClientMessage::WindowTargets {
            targets: vec![WindowTarget {
                role: "ListItem".to_owned(),
                name: "Documents".to_owned(),
                identity: None,
                rect: [4.0, 6.0, 44.0, 24.0],
                actions: vec![TargetAction::Activate],
                level: None,
                selected: false,
            }],
            instance: None,
        },
    );
    // More windows than the shell shows at once, each of an app of its own,
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
            .any(|control| control.name == "Documents")
    });

    let control = |session: &Session| {
        session
            .controls()
            .into_iter()
            .find(|control| control.name == "Documents")
            .map(|control| control.rect)
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
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    let _connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
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
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);
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
        pid,
        tab,
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

    // The session serves no question about the device's own shell here, so even an app handed
    // it is told the target stays closed on the connection it asked on.
    probe_declares_ush(&home);
    device_shell_stays_closed(&mut session, &launches, pid, tab);
}

/// Ask, for [`PROBE`]'s window, for the device's own shell: the app is told the target stays
/// closed on the connection it asked on. What it was told, as the test reads it.
fn device_shell_stays_closed(
    session: &mut Session,
    launches: &Arc<Launches>,
    pid: u32,
    tab: u64,
) -> String {
    let mut asking = ask(
        launches,
        PROBE,
        pid,
        tab,
        &FloatingSurfaceClientMessage::AllowHostShell,
    );
    let closed = FloatingSurfaceHostMessage::HostShellAllowed { allowed: false };
    let told = host_frames(session, &mut asking, |told| told.contains(&closed));
    assert!(
        told.contains(&closed),
        "the app is told the target stays closed: {told:?}"
    );
    format!("{told:?}")
}

/// Make [`PROBE`]'s package declare the runtime (`ush`), as Terminal's does: only such an app asks
/// for the device's own shell.
fn probe_declares_ush(home: &tilo_test_env::TestHome) {
    let manifest = home
        .data()
        .join("tilo-apps")
        .join("payload")
        .join(PROBE)
        .join("manifest.toml");
    let mut source = fs::read_to_string(&manifest).unwrap();
    source.push_str("\n[sandbox.services]\nush = \"required@31\"\n");
    fs::write(&manifest, source).unwrap();
}

/// An app not handed the device's shell never puts the question in front of the person: it is told
/// the target stays closed, and why.
#[test]
fn only_an_app_handed_the_device_shell_asks_for_it() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let bus = tilo_test_env::PrivateBus::start();
    let (mut session, apps, launches, _launcher) =
        probe_session_with_services(&mut home, &started, &bus);
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);

    let told = device_shell_stays_closed(&mut session, &launches, pid, tab);
    assert!(told.contains("only an app handed"), "and why: {told}");
    session.step(Duration::from_millis(1)).unwrap();
    assert!(
        crate::common::person_asked_now().is_empty(),
        "the person is asked nothing"
    );
}

/// The person's answer to the question about the device's own shell reaches the app: the question
/// is asked of the person, the answer is handed to the runtime's supervisor as the allow it
/// records, and the app hears on the connection it asked on that the target is open.
#[test]
fn the_answer_to_the_device_shell_question_reaches_the_app() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let bus = tilo_test_env::PrivateBus::start();
    let (mut session, apps, launches, _launcher) =
        probe_session_with_services(&mut home, &started, &bus);
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);
    probe_declares_ush(&home);

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
        pid,
        tab,
        &FloatingSurfaceClientMessage::AllowHostShell,
    );
    crate::common::person_answers(&mut session, tilo_service::Answer::default());

    asking
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    // The answer reaches the app on a turn of the session's; a counted number of turns is not the
    // answer arriving, so the wait is for the answer itself.
    let mut seen = Vec::new();
    let heard = crate::common::wait_maybe(&mut session, |session| {
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

/// A closed window comes back: the app is asked for a window of its own in the restored tab, even
/// while it shows another window, rather than raising that one and leaving the tab empty.
#[test]
fn reopening_a_closed_window_starts_its_app_again() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let records = home.home().join("records");
    host_app_declaring(
        &home,
        "org.example.Probe.desktop",
        &format!("StartupWMClass={X11_CLASS}\nSingleMainWindow=true\n"),
        "#!/bin/sh\nexit 0\n",
    );
    let launcher = recording_host_launcher(&home, &records);

    let (mut session, _apps, _launches) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    // The client's two windows: their class is the app, their title the tabs'.
    session.desktop().with_policy(|policy| {
        for id in [11, 13] {
            policy
                .map(titled_window(id, X11_CLASS, PROBE_NAME))
                .unwrap();
        }
    });
    let tabs = |session: &Session| groups(session).concat().len();
    until(&mut session, "the windows are tabs", |session| {
        tabs(session) == 2
    });

    // The person closes one tab, and its window goes with it.
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.close_tab_of(X11_CLASS)),
        "the tab's close is taken"
    );
    until(&mut session, "the tab is closed", |session| {
        tabs(session) == 1
    });
    assert!(!records.exists(), "nothing had started the app yet");

    // Reopen puts the tab back and asks the app for its window: the class alone names that app
    // through the entry claiming it.
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.reopen_closed_of(X11_CLASS)),
        "the reopen is taken"
    );
    until(&mut session, "the app is asked again", |_| records.exists());
    assert_eq!(tabs(&session), 2, "the tab is back, waiting for the app");

    // The window the app opens takes the restored tab.
    session.desktop().with_policy(|policy| {
        policy
            .map(titled_window(14, X11_CLASS, PROBE_NAME))
            .unwrap();
    });
    until(
        &mut session,
        "the app's window is in the restored tab",
        |session| shown_windows(session).contains(&Some(14)),
    );
    assert_eq!(tabs(&session), 2, "two tabs, not three");
    assert_eq!(
        fs::read_to_string(&records).unwrap().lines().count(),
        1,
        "the app was asked once"
    );
}

#[test]
fn an_app_closes_only_its_own_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, launches, _launcher) = probe_session(&mut home, &started);
    let (pid, _, tab) = probe_showing(&mut session, &apps, &started);
    session
        .desktop()
        .with_policy(|policy| policy.map(window(12, "org.example.Other")).unwrap());
    until(&mut session, "both apps show", |session| {
        groups(session).len() == 2
    });

    let _connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
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

    // The person closes the other app's window after it: an app's reopen brings back what it
    // closed, never another app's window that closed later.
    session
        .desktop()
        .with_policy(|policy| policy.map(window(13, "org.example.Third")).unwrap());
    until(&mut session, "the third app shows", |session| {
        groups(session).len() == 2
    });
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.close_tab_of("org.example.Other"))
    );
    until(&mut session, "the other app's tab is closed", |session| {
        groups(session).len() == 1
    });
    let _connection = ask(
        &launches,
        PROBE,
        pid,
        tab,
        &FloatingSurfaceClientMessage::ReopenClosedTab,
    );
    until(&mut session, "the closed tab is back", |session| {
        groups(session)
            .iter()
            .flatten()
            .any(|title| title == PROBE_NAME)
    });
    assert!(
        !groups(&session)
            .iter()
            .flatten()
            .any(|title| title == "Other"),
        "the other app's window stays closed: {:?}",
        groups(&session)
    );
}

#[test]
fn a_session_dropped_while_its_app_runs_leaves_no_launcher_behind() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let (mut session, apps, _launches, launcher) = probe_session(&mut home, &started);
    let (pid, _room, _tab) = probe_showing(&mut session, &apps, &started);

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

/// The window groups of the workspace in front, each by its id with its tabs' ids.
fn group_ids(session: &Session) -> Vec<(u64, Vec<u64>)> {
    session.handle().read(|tree| {
        tree.active_workspace().map_or_else(Vec::new, |workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
                .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                .collect()
        })
    })
}

/// Each start the launcher writing to `started` made: its pid, and the tab it was told its window
/// takes.
fn starts_told(started: &Path) -> Vec<(u32, Option<u64>)> {
    fs::read_to_string(started)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (pid, tab) = line.split_once(' ')?;
            Some((pid.parse().ok()?, tab.trim().parse().ok()))
        })
        .collect()
}

/// Opening an app is never undoing a close: once its window was closed, the workspace's open of the
/// app (the row search and agents run) makes a new window with a tab of its own, and the app is
/// told that new tab. Only the explicit reopen brings the closed window back, in the tab and the
/// window group it had.
#[test]
fn opening_an_app_after_closing_its_window_makes_a_new_window() {
    close_then_open_again("0");
}

/// An app that ends badly after the person closed its window (a crash on the way out) leaves the
/// window closed: nothing comes back to say it stopped, and opening it again makes a new window.
#[test]
fn an_app_failing_after_its_window_closed_brings_nothing_back() {
    close_then_open_again("1");
}

/// Open [`PROBE`] through the workspace's open of it, close its window as the person does, let its
/// process end with `status`, and open it again: a new window. Then the explicit reopen puts the
/// closed one back where it was.
fn close_then_open_again(status: &str) {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let stops = home.home().join("stops");
    fs::create_dir_all(&stops).unwrap();
    installed_probe_package(&mut home);
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            // Each start records its pid and the tab it was told, and runs until the test asks it
            // to end (as an app does once its last window closed) or the test is over.
            "printf '%s %s\\n' \"$$\" \"${{TILO_CONTINUITY_INSTANCE:-}}\" >> '{started}'\n\
             while [ ! -e \"{stops}/$$\" ] && [ -e '{started}' ]; do sleep 0.05; done\n\
             exit \"$(cat \"{stops}/$$\" 2>/dev/null || echo 0)\"\n",
            started = started.display(),
            stops = stops.display(),
        ),
    )
    .unwrap();
    let (mut session, apps, _launches) = session_through(&mut home, &launcher, None);
    session.step(Duration::from_millis(1)).unwrap();
    let (workspace, _) = front_workspace(&session);
    let open = |apps: &Apps| {
        apps.invoke(
            tilo_actions::remote::application_open_target(&workspace, PROBE),
            serde_json::Map::new(),
        )
        .expect("the workspace opens the app");
    };
    let show = |session: &mut Session, id: u64, pid: u32| {
        let mut mapped = window(id, PROBE);
        mapped.pid = Some(pid);
        session
            .desktop()
            .with_policy(|policy| policy.map(mapped).unwrap());
        until(session, "the app's window is on the session", |session| {
            shown_windows(session).contains(&Some(id))
        });
    };

    open(&apps);
    until(&mut session, "the app runs", |_| {
        starts_told(&started).len() == 1
    });
    let (first_pid, told) = starts_told(&started)[0];
    show(&mut session, 11, first_pid);
    let closed = group_ids(&session);
    let [(closed_group, ref closed_tabs)] = closed[..] else {
        panic!("one window: {closed:?}");
    };
    assert_eq!(
        told,
        Some(closed_tabs[0]),
        "the app is told the tab it opens in"
    );

    // The person closes the window, the app's window goes with it, and its process ends.
    invoke(
        &mut session,
        CHROME_RESOURCE,
        "close-window",
        serde_json::json!({"group": closed_group.to_string()}),
    );
    until(&mut session, "the window is closed", |session| {
        group_ids(session).is_empty()
    });
    session.desktop().with_policy(|policy| policy.unmap(11));
    fs::write(stops.join(first_pid.to_string()), status).unwrap();
    tilo_test_env::exited("the closed app's process", first_pid);
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert!(
        group_ids(&session).is_empty(),
        "nothing comes back when the closed app ends: {:?}",
        group_ids(&session)
    );

    // Opening the app again makes a new window: a group and a tab that never were.
    open(&apps);
    until(&mut session, "the app runs again", |_| {
        starts_told(&started).len() == 2
    });
    let (second_pid, told) = starts_told(&started)[1];
    let opened = group_ids(&session);
    let [(opened_group, ref opened_tabs)] = opened[..] else {
        panic!("one window opened: {opened:?}");
    };
    assert_ne!(opened_group, closed_group, "a new window group: {opened:?}");
    assert_ne!(
        opened_tabs, closed_tabs,
        "a new tab, not the closed one: {opened:?}"
    );
    assert_eq!(
        told,
        Some(opened_tabs[0]),
        "the app is told the new tab, not the one that closed"
    );
    show(&mut session, 12, second_pid);

    // The app ends. The explicit reopen puts the closed window back where it was, in its own tab
    // and group, and asks the app for its window there.
    fs::write(stops.join(second_pid.to_string()), "0").unwrap();
    tilo_test_env::exited("the app's second process", second_pid);
    session
        .handle()
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::ReopenClosed {
                app: None,
                window: None,
            },
        )
        .expect("the session takes the reopen");
    until(&mut session, "the closed window is back", |session| {
        group_ids(session)
            .iter()
            .any(|(group, tabs)| *group == closed_group && tabs == closed_tabs)
    });
    until(
        &mut session,
        "the app is asked for the reopened window",
        |_| starts_told(&started).len() == 3,
    );
    assert_eq!(
        starts_told(&started)[2].1,
        Some(closed_tabs[0]),
        "the reopened window takes the tab it had"
    );
}
