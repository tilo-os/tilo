//! Opening an app in the headless native session, driven only through what
//! the session publishes: a search row of one of the machine's own apps
//! starts its program in the session's environment, and one that stops on
//! its own leaves a notice in its place.

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
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tilo_actions::ResourceSpec;
use tilo_app_launch::Connects as _;
use tilo_compositor::OutputConfig;
use tilo_desktop::desktop_actions::{DESK_RESOURCE, SEARCH_RESOURCE, WINDOW_KIND};
use tilo_desktop_model::TileContent;
use tilo_desktop_policy::Window;
use tilo_desktop_search::providers::host_apps::HostAppEntry;
use tilo_session::mount::app_tabs::ServiceLinks;
use tilo_session::mount::apps::Apps;
use tilo_session::{AppActions, AppLists, Launches, Lists, Options, SearchOptions, Session};
use ui::floating_surface::{FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage};

/// A home of its own with `body` as the desktop entry `desktop_id` starts,
/// the entry declaring `lines` besides its name and command, and the path
/// that body was written to.
fn host_app_declaring(
    home: &tilo_test_env::TestHome,
    desktop_id: &str,
    lines: &str,
    body: &str,
) -> std::path::PathBuf {
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
) -> std::path::PathBuf {
    let claim = mime.map_or_else(String::new, |mime| format!("MimeType={mime}\n"));
    host_app_declaring(home, desktop_id, &claim, body)
}

/// A home of its own with `body` as the desktop entry `desktop_id` starts,
/// and the path that body was written to.
fn host_app(home: &tilo_test_env::TestHome, desktop_id: &str, body: &str) -> std::path::PathBuf {
    host_app_claiming(home, desktop_id, body, None)
}

/// A session whose search finds one of the machine's own apps, and whose
/// launches start on the display at `socket`.
fn session(socket: &std::path::Path) -> (Session, Rc<Apps>) {
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(socket.to_path_buf());
    let launch = Rc::new(Apps::new(environment, None));
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            search: SearchOptions {
                lists: Lists::fixed(AppLists {
                    host_apps: vec![HostAppEntry {
                        desktop_id: "org.example.Probe".into(),
                        name: "Probe".into(),
                        ..HostAppEntry::default()
                    }],
                    ..AppLists::default()
                }),
                ..SearchOptions::default()
            },
            apps: Some(Rc::clone(&launch) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&launch)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    (session, launch)
}

/// Run frames until `done` holds.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..400 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("{what} did not happen");
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

/// Open search on `query` and choose the row `title` through the actions
/// search publishes, as a person typing and pressing it does.
fn choose(session: &mut Session, query: &str, title: &str) {
    invoke(
        session,
        DESK_RESOURCE,
        "open-search",
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
    invoke(session, row.id.as_str(), "choose", serde_json::json!({}));
}

#[test]
fn choosing_a_host_app_runs_its_program_with_the_sessions_display() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let socket = home.runtime().join("wayland-7");
    let marker = home.home().join("probe-ran");
    host_app(
        &home,
        "org.example.Probe.desktop",
        &format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n' \"$WAYLAND_DISPLAY\" \"$XDG_RUNTIME_DIR\" > '{marker}'\n",
            marker = marker.display()
        ),
    );

    let (mut session, _apps) = session(&socket);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the app runs", |_| marker.exists());

    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "wayland-7\n".to_owned() + home.runtime().to_str().unwrap() + "\n",
        "the app starts on this session's display, in its own runtime directory"
    );
}

#[test]
fn a_document_opens_in_the_app_that_claims_its_type() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let socket = home.runtime().join("wayland-7");
    let marker = home.home().join("opened");
    host_app_claiming(
        &home,
        "org.example.Reader.desktop",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > '{}'\n",
            marker.display()
        ),
        Some("text/plain"),
    );
    let document = home.home().join("my notes.txt");
    fs::write(&document, b"notes").unwrap();

    let (mut session, apps) = session(&socket);
    session.step(Duration::from_millis(1)).unwrap();
    // The link a browser or a file manager hands over: the path inside it is
    // percent-encoded, the path the app opens is not.
    let uri = format!(
        "file://{}",
        ui::uri::encode_component(document.to_str().unwrap())
    );
    apps.open_uri("", &uri).expect("the document opens");
    until(&mut session, "the app runs", |_| marker.exists());

    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim_end(),
        document.to_str().unwrap(),
        "the app is told which document to open"
    );
}

#[test]
fn an_app_that_stops_on_its_own_leaves_a_notice_in_its_place() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let socket = home.runtime().join("wayland-7");
    let marker = home.home().join("probe-ran");
    host_app(&home, "org.example.Probe.desktop", "#!/bin/sh\nexit 3\n");

    let (mut session, _apps) = session(&socket);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the notice is shown", |session| {
        find(&session.resources(), DESK_RESOURCE).is_some_and(|desk| {
            desk.children
                .iter()
                .any(|child| child.kind == WINDOW_KIND && child.label.contains("Probe"))
        })
    });
    assert!(!marker.exists(), "the app never wrote anything");
}

/// A window of `app` the compositor mapped.
fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..Window::default()
    }
}

#[test]
fn a_second_launch_raises_the_window_the_app_already_has() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let socket = home.runtime().join("wayland-7");
    let launches = home.home().join("launches");
    host_app_declaring(
        &home,
        "org.example.Probe.desktop",
        "SingleMainWindow=true\n",
        &format!("#!/bin/sh\nprintf 'ran\\n' >> '{}'\n", launches.display()),
    );

    let (mut session, _apps) = session(&socket);
    session.step(Duration::from_millis(1)).unwrap();
    choose(&mut session, "probe", "Probe");
    until(&mut session, "the app runs", |_| launches.exists());

    // The app's window is on the desk, showing another app's window in front.
    session.desktop().with_policy(|policy| {
        policy.map(window(11, "org.example.Probe")).unwrap();
        policy.map(window(12, "org.example.Other")).unwrap();
    });
    until(&mut session, "both windows are on the desk", |session| {
        session
            .desktop()
            .with_policy(|policy| policy.windows_in_front().len())
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
        fs::read_to_string(&launches).unwrap().lines().count(),
        1,
        "the app is not started a second time"
    );
    assert_eq!(
        session.desktop().with_policy(|policy| policy.focused()),
        Some(11),
        "the window the app already has came to the front"
    );
}

/// The app the tab tests install: a Tilo package whose window is a tab of the
/// desktop.
const PROBE: &str = "so.tilo.probe";

/// Install [`PROBE`] in `home`'s own app store, and write the launcher the
/// session starts it through: it records the pid of every start and runs until
/// the test creates `stop`.
fn installed_probe(home: &mut tilo_test_env::TestHome, started: &Path, stop: &Path) -> PathBuf {
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
    let launcher = home.home().join("tilo-launcher");
    tilo_test_env::script(
        &launcher,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{started}'\n\
             while [ ! -e '{stop}' ]; do sleep 0.05; done\n",
            started = started.display(),
            stop = stop.display(),
        ),
    )
    .unwrap();
    launcher
}

/// A session whose apps start from `home`'s own app store, and the record of
/// the launches it makes.
fn probe_session(
    home: &mut tilo_test_env::TestHome,
    started: &Path,
    stop: &Path,
) -> (Session, Rc<Apps>, Arc<Launches>) {
    let socket = home.runtime().join("wayland-7");
    let started_by = installed_probe(home, started, stop);
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(socket);
    let apps = Rc::new(Apps::with_launcher(environment, None, Some(started_by)));
    let launches = Arc::new(Launches::default());
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            launches: Arc::clone(&launches),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    (session, apps, launches)
}

/// The window groups of the workspace in front, each with its tabs' titles in
/// order: the tabs a person sees.
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
/// and hand back the process it runs in and the room it runs in.
fn probe_showing(session: &mut Session, apps: &Apps, started: &Path) -> (u32, String) {
    let room = session
        .desktop()
        .with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.room().to_string())
            })
        })
        .expect("a workspace in front");
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.to_owned(),
            open: None,
        },
        &apps.room_named(&room),
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
        groups(session) == vec![vec![PROBE.to_owned()]]
    });
    (pid, room)
}

/// Ask the session what `app` asks over a window's own shell-ui connection:
/// the connection the session makes for a window of an app it runs, which the
/// service answers for that app's tab. The connection stays open for the
/// caller.
fn ask(
    launches: &Arc<Launches>,
    app: &str,
    room: &str,
    pid: u32,
    message: &FloatingSurfaceClientMessage,
) -> UnixStream {
    let connections = ServiceLinks::new(Arc::clone(launches)).window(app, room, 7, Some(pid));
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
    let stop = home.home().join("stop");
    let (mut session, apps, launches) = probe_session(&mut home, &started, &stop);
    let (pid, room) = probe_showing(&mut session, &apps, &started);

    let _connection = ask(
        &launches,
        PROBE,
        &room,
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
        vec![vec![PROBE.to_owned(), PROBE.to_owned()]],
        "the window took the tab beside the app's own"
    );
    fs::write(&stop, b"").unwrap();
}

#[test]
fn an_address_the_app_does_not_claim_opens_no_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let stop = home.home().join("stop");
    let (mut session, apps, launches) = probe_session(&mut home, &started, &stop);
    let (pid, room) = probe_showing(&mut session, &apps, &started);

    let _connection = ask(
        &launches,
        PROBE,
        &room,
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
        vec![vec![PROBE.to_owned()]],
        "an address the app does not declare opens nothing"
    );
    assert_eq!(
        fs::read_to_string(&started).unwrap().lines().count(),
        1,
        "and starts nothing"
    );
    fs::write(&stop, b"").unwrap();
}

#[test]
fn an_app_closes_only_its_own_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("starts");
    let stop = home.home().join("stop");
    let (mut session, apps, launches) = probe_session(&mut home, &started, &stop);
    let (pid, room) = probe_showing(&mut session, &apps, &started);
    session
        .desktop()
        .with_policy(|policy| policy.map(window(12, "org.example.Other")).unwrap());
    until(&mut session, "both apps show", |session| {
        groups(session).len() == 2
    });

    let _connection = ask(
        &launches,
        PROBE,
        &room,
        pid,
        &FloatingSurfaceClientMessage::CloseTab { instance: None },
    );
    until(&mut session, "the app's own tab is closed", |session| {
        !groups(session).iter().flatten().any(|title| title == PROBE)
    });
    assert_eq!(
        groups(&session),
        vec![vec!["org.example.Other".to_owned()]],
        "the other app's tab stays"
    );

    // Its own reopen brings back what it closed.
    let _connection = ask(
        &launches,
        PROBE,
        &room,
        pid,
        &FloatingSurfaceClientMessage::ReopenClosedTab,
    );
    until(&mut session, "the closed tab is back", |session| {
        groups(session).iter().flatten().any(|title| title == PROBE)
    });
    fs::write(&stop, b"").unwrap();
}
