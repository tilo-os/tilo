//! The tools service a session serves its apps, and the one launch path everything that is not the
//! shell starts apps through: an agent's launch asked of the session's launcher, Files asking for
//! the drives and the apps it may open, and a tool opened at the folder it was asked for.

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

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use runtime_portal::tools::{
    AppKind, AppRemoval, TOOL_PROTOCOL_VERSION, ToolIntent, ToolReply, ToolRequest,
};
use tilo_app_host::services::{LaunchPlace, LauncherServices as _};
use tilo_app_launch::SessionEnvironment;
use tilo_desktop_model::Workspace;
use tilo_desktop_policy::Window;
use tilo_notifications_service::Notifications;
use tilo_session::apps::Apps;
use tilo_session::drives::{Drives, Media, Medium};
use tilo_session::{AppActions, AppLists, Launches, Lists, Options, Session};

/// The notes app, as its package names it: what an agent asks for.
const NOTES: &str = "so.tilo.notes";
/// The terminal a tool's open starts.
const TERMINAL: &str = "so.tilo.terminal";
/// The app the person's own lists are answered to.
const FILES: &str = "so.tilo.file-explorer";
/// Another app, with a window of its own beside Notes.
const CLOCKS: &str = "so.tilo.clocks";
/// An app that declares `files.open`: it opens a file in the app that opens its kind.
const TASKS: &str = "so.tilo.tasks";
/// A tilolang app whose own action opens Markdown files.
const READER: &str = "so.tilo.reader";

/// Install the packages `(id, name, more)` in `home` 's own app store: `more` continues the
/// manifest after its `[entrypoint]` fields, so a package declares more keys there or a table of
/// its own.
fn packages(home: &mut tilo_test_env::TestHome, packages: &[(&str, &str, &str)]) {
    let store = home.data().join("tilo-apps");
    let mut registry = String::new();
    for (id, name, more) in packages {
        let payload = store.join("payload").join(id);
        fs::create_dir_all(payload.join("bin")).unwrap();
        let manifest = payload.join("manifest.toml");
        fs::write(
            &manifest,
            format!(
                "[package]\n\
                 id = \"{id}\"\n\
                 version = \"1.0.0\"\n\
                 kind = \"app\"\n\
                 name = \"{name}\"\n\
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
                 {more}\n",
                key = "11".repeat(32),
            ),
        )
        .unwrap();
        write!(
            registry,
            "[[entry]]\nid = \"{id}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        )
        .unwrap();
    }
    fs::write(store.join("registry.toml"), registry).unwrap();
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// The launcher this session starts packages through: it writes down the arguments it was started
/// with and the tool context it was handed, then runs until the test is over.
fn launcher(home: &tilo_test_env::TestHome, records: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\t%s\\t%s\\n' \"$*\" \"${{TILO_TOOL_CONTEXT-}}\" \"$$\" >> '{records}'\nhold\n",
            records = records.display(),
        ),
    )
    .unwrap()
}

/// The media a test plugged in: what appeared since the last pass, which is each of them once.
struct Plugged(std::cell::RefCell<Vec<Medium>>);

impl Media for Plugged {
    fn appeared(&self) -> Vec<Medium> {
        std::mem::take(&mut *self.0.borrow_mut())
    }

    fn eject(&self, _: &Medium) {}
}

/// A headless session whose packages start through `launcher` .
fn session(
    home: &tilo_test_env::TestHome,
    launcher: &tilo_test_env::StandIn,
    drives: Option<Rc<Drives>>,
) -> (Session, Arc<Launches>) {
    let socket = home.runtime().join("wayland-7");
    let mut environment = SessionEnvironment::from_process();
    environment.wayland_socket = Some(socket);
    let apps = Rc::new(Apps::with_launcher(
        environment,
        None,
        Some(launcher.path().to_owned()),
    ));
    let opened = Arc::new(Launches::default());
    let session = crate::common::headless(Options {
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        launches: Arc::clone(&opened),
        // The person's lists as the device reads them, read once: what the tools service serves
        // Files and what search lists is that same list.
        lists: Lists::fixed(AppLists::read()),
        drives,
        // The identity backup is a layer over the desktop, which a test stands in for: what a
        // pass raises is asserted, not the ceremony's own pages.
        identity_backup: |_| Ok(tilo_onboarding_session::view::OnboardingView::new_tour()),
        wake: tilo_service::LoopWake::default(),
        ..Options::default()
    });
    (session, opened)
}

/// Run frames until `done` holds.
fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    crate::common::until(session, what, done);
}

/// The workspace in front, as the session holds it.
fn front(session: &Session) -> Workspace {
    session
        .desktop()
        .with_policy(|policy| {
            policy
                .handle()
                .read(|tree| tree.active_workspace().cloned())
        })
        .expect("a workspace in front")
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

/// What the launcher was started with, one entry per start: its arguments,
/// the tool context it was handed, and the process that runs it.
fn starts(records: &Path) -> Vec<(String, String, u32)> {
    fs::read_to_string(records)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let args = fields.next().unwrap_or_default().to_owned();
            let context = fields.next().unwrap_or_default().to_owned();
            let pid = fields
                .next()
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0);
            (args, context, pid)
        })
        .collect()
}

/// The start of `app` , as [`starts`] wrote it down.
fn started(records: &Path, app: &str) -> (String, String, u32) {
    starts(records)
        .into_iter()
        .find(|(args, _, _)| args.contains(&format!("--app-id {app}")))
        .unwrap_or_else(|| panic!("{app} started"))
}

/// Ask the session's tools service `intent` for `workspace` , as a launched app does over its
/// socket.
fn ask(home: &tilo_test_env::TestHome, workspace: &str, intent: ToolIntent) -> ToolReply {
    let socket = home.runtime().join("tilo/services/tools.sock");
    let mut stream = tilo_files::native::socket::connect(&socket).expect("the tools socket");
    let request = ToolRequest {
        protocol: TOOL_PROTOCOL_VERSION,
        workspace_id: workspace.to_owned(),
        intent,
        on_behalf_of: None,
    };
    let mut line = serde_json::to_vec(&request).unwrap();
    line.push(b'\n');
    stream.write_all(&line).unwrap();
    let mut reading = BufReader::new(stream);
    let mut answer = String::new();
    reading.read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).expect("a tool reply")
}

/// Ask the session's tools service from an app's own thread while the session's loop runs: a
/// request that waits for a pass of the loop would otherwise wait on the thread that makes one.
fn ask_while_stepping(
    session: &mut Session,
    home: &tilo_test_env::TestHome,
    workspace: String,
    intent: ToolIntent,
) -> ToolReply {
    let asking = {
        let home = home.runtime().to_owned();
        std::thread::spawn(move || {
            let socket = home.join("tilo/services/tools.sock");
            let mut stream =
                tilo_files::native::socket::connect(&socket).expect("the tools socket");
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
                workspace_id: workspace,
                intent,
                on_behalf_of: None,
            };
            let mut line = serde_json::to_vec(&request).unwrap();
            line.push(b'\n');
            stream.write_all(&line).unwrap();
            let mut reading = BufReader::new(stream);
            let mut answer = String::new();
            reading.read_line(&mut answer).unwrap();
            serde_json::from_str::<ToolReply>(&answer).expect("a tool reply")
        })
    };
    // The pass that answers: frames run until the app's thread has its answer.
    until(session, "the tools service answers", |_| {
        asking.is_finished()
    });
    asking.join().expect("the asking thread")
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

/// The pid the tools service reads a request's app from: this test's own process, recorded as the
/// launch of `app` in `workspace` , as the session records an app it started.
fn launch_as(launches: &Launches, app: &str, workspace: &str) {
    launches.started(std::process::id(), app, workspace, false);
}

tilo_test_env::scenario! {
    fn an_agents_tools_open_what_the_shell_shows;

    phase an_agent_opens_notes_in_a_workspace;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let records = home.home().join("starts");
        packages(&mut home, &[(NOTES, "Notes", "")]);
        let launcher = launcher(&home, &records);
        let (mut session, opened) = session(&home, &launcher, None);
        session.step(Duration::from_millis(1)).unwrap();
        let workspace = front(&session);

        // An agent asks the session's launcher for Notes in the workspace in front, from a thread that
        // owns no part of the shell: the broker's launcher service is the one path such an open takes.
        let asked = {
            let opened = Arc::clone(&opened);
            let workspace = workspace.clone();
            std::thread::spawn(move || opened.launch(NOTES, &workspace, None, &LaunchPlace::Focus))
        };
        until(&mut session, "the app runs", |_| {
            starts(&records)
                .iter()
                .any(|(args, _, _)| args.contains(&format!("--app-id {NOTES}")))
        });
        asked
            .join()
            .expect("the asking thread")
            .expect("the agent's launch is made");

        // The launch is the shell's own path: the app is planned into the workspace the agent named, by
        // the pass that starts it, and written down as a launch of this session in that workspace.
        let (args, _, pid) = started(&records, NOTES);
        assert!(
            args.contains(&format!("--workspace-id {}", workspace.id())),
            "the app starts in the workspace the agent named"
        );
        assert_eq!(
            opened.workspace_of(pid, NOTES).as_deref(),
            Some(workspace.id().to_string().as_str()),
            "the session recorded its launch in that workspace"
        );
        session
            .desktop()
            .with_policy(|policy| policy.map(window(11, NOTES)).unwrap());
        until(
            &mut session,
            "the app's window is on the shell",
            |session| groups(session) == vec![vec!["Notes".to_owned()]],
        );
    }

    phase an_agents_open_brings_the_window_the_shell_already_shows_forward;
    // An agent whose `notes#open` asks for the place beside the view it works in, on a shell that
    // already shows Notes: the person's own window comes forward, and nothing starts a second one.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let records = home.home().join("starts");
        packages(
            &mut home,
            &[(NOTES, "Notes", ""), (CLOCKS, "Clocks", "")],
        );
        let launcher = launcher(&home, &records);
        let (mut session, opened) = session(&home, &launcher, None);
        session.step(Duration::from_millis(1)).unwrap();
        let workspace = front(&session);

        // The person's shell: Notes, and another app's window in front of it.
        for (id, app) in [(11, NOTES), (12, CLOCKS)] {
            session
                .desktop()
                .with_policy(|policy| policy.map(window(id, app)).unwrap());
        }
        until(&mut session, "both windows are on the shell", |session| {
            groups(session) == vec![vec!["Notes".to_owned()], vec!["Clocks".to_owned()]]
        });
        let clocks = session
            .desktop()
            .with_policy(|policy| policy.tab_of(12))
            .expect("the tab the other app's window stands in");
        session
            .desktop()
            .with_policy(|policy| policy.focus_tab(clocks));
        until(&mut session, "the other app is in front", |session| {
            in_front(session, "Clocks")
        });

        // The agent has worked in Notes, so its open asks for the place beside that view: the place is
        // where a window of the app stands, not that there is another one.
        let asked = {
            let opened = Arc::clone(&opened);
            let place = LaunchPlace::Beside {
                app_id: NOTES.to_owned(),
            };
            std::thread::spawn(move || opened.launch(NOTES, &workspace, None, &place))
        };
        // The pass that answers the ask is the shell's own: frames run until the window comes forward,
        // and the asker is told once it is made.
        until(
            &mut session,
            "the window the shell had comes forward",
            |session| in_front(session, "Notes"),
        );
        asked
            .join()
            .expect("the asking thread")
            .expect("the agent's open is made");
        assert_eq!(
            groups(&session),
            vec![vec!["Notes".to_owned()], vec!["Clocks".to_owned()]],
            "the app's window comes forward: no second window of it opens"
        );
        assert!(
            !starts(&records)
                .iter()
                .any(|(args, _, _)| args.contains(&format!("--app-id {NOTES}"))),
            "nothing starts the app again: {:?}",
            starts(&records)
        );
    }

    phase files_lists_the_drives_and_the_apps_it_may_open;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let records = home.home().join("starts");
        packages(&mut home, &[(NOTES, "Notes", "")]);
        let launcher = launcher(&home, &records);
        let mount = home.home().join("Backup");
        fs::create_dir_all(&mount).unwrap();
        let drives = Rc::new(Drives::new(
            Rc::new(Plugged(std::cell::RefCell::new(vec![Medium {
                device: "/org/freedesktop/UDisks2/block_devices/sdb1".to_owned(),
                label: Some("Backup".to_owned()),
                size: 64 * 1024 * 1024 * 1024,
                mount: mount.clone(),
            }]))),
            Notifications::default(),
        ));
        let (mut session, opened) = session(&home, &launcher, Some(drives));
        session.step(Duration::from_millis(1)).unwrap();
        let workspace = front(&session).id().to_string();
        launch_as(&opened, FILES, &workspace);

        let listed = ask(&home, &workspace, ToolIntent::Drives);
        assert!(listed.accepted, "{}", listed.message);
        assert_eq!(listed.drives.len(), 1, "one drive is plugged in");
        assert_eq!(listed.drives[0].id, "sdb1");
        assert_eq!(listed.drives[0].label, "Backup");
        assert_eq!(
            listed.drives[0].open_at.as_deref(),
            Some(mount.to_str().unwrap()),
            "the mounted folder is where Files opens it"
        );

        let apps = ask(&home, &workspace, ToolIntent::Applications);
        assert!(apps.accepted, "{}", apps.message);
        let notes = apps
            .applications
            .iter()
            .find(|app| app.id == NOTES)
            .unwrap_or_else(|| panic!("the installed app is listed: {:?}", apps.applications));
        assert_eq!(notes.name, "Notes");
        assert_eq!(notes.removal, AppRemoval::Uninstall);
        assert_eq!(notes.kind, AppKind::Tilo);

        // The person's app list is Files' own: no other app is told it.
        launch_as(&opened, NOTES, &workspace);
        let refused = ask(&home, &workspace, ToolIntent::Applications);
        assert!(
            !refused.accepted,
            "another app is not told the person's apps"
        );
        assert!(refused.applications.is_empty());
    }

    phase a_file_is_opened_for_files_and_for_an_app_that_declares_files_open;
    // A file opens in the app that opens its kind: Files opens one for the person, and another app
    // only when the manifest it was installed with declares `files.open`. The app that opens a
    // Markdown file is told which, and started.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let records = home.home().join("starts");
        packages(
            &mut home,
            &[
                (
                    TASKS,
                    "Tasks",
                    "\n[capabilities]\nrequired = [\"files.open\"]",
                ),
                (
                    READER,
                    "Reader",
                    "runtime = \"tilolang/1\"\n\n[providers]\next = [\"md\"]\nmime = [\"text/markdown\"]",
                ),
            ],
        );
        let launcher = launcher(&home, &records);
        let (mut session, opened) = session(&home, &launcher, None);
        session.step(Duration::from_millis(1)).unwrap();
        let workspace = front(&session).id().to_string();

        // Files sees the app that opens Markdown.
        launch_as(&opened, FILES, &workspace);
        let asking = ask_while_stepping(&mut session, &home, workspace.clone(), ToolIntent::DocumentApps {
            path: "file.md".into(),
        });
        assert!(asking.accepted, "{}", asking.message);
        assert!(
            asking.documents.iter().any(|app| app.id == READER),
            "the app that opens Markdown is listed: {:?}",
            asking.documents
        );

        // An app whose package did not declare `files.open` opens nothing and is told why.
        launch_as(&opened, NOTES, &workspace);
        let and_not = ask_while_stepping(&mut session, &home, workspace.clone(), ToolIntent::OpenContent {
            name: "plan.md".into(),
            hash: "a".repeat(64),
            app: READER.into(),
        });
        assert!(!and_not.accepted, "an app that did not ask opens nothing");
        assert!(
            and_not.message.contains("files.open"),
            "the refusal names what the app's package must declare: {}",
            and_not.message
        );
        assert!(
            !starts(&records)
                .iter()
                .any(|(args, _, _)| args.contains(&format!("--app-id {READER}"))),
            "nothing opened it"
        );

        // One that did asks the session to open the file, and it starts the app that opens its
        // kind.
        launch_as(&opened, TASKS, &workspace);
        let asked = ask_while_stepping(&mut session, &home, workspace.clone(), ToolIntent::OpenContent {
            name: "plan.md".into(),
            hash: "a".repeat(64),
            app: READER.into(),
        });
        assert!(asked.accepted, "{}", asked.message);
        // The process starts on the pass that answers: the record is written once it does.
        until(&mut session, "the app that opens Markdown starts", |_| {
            starts(&records)
                .iter()
                .any(|(args, _, _)| args.contains(&format!("--app-id {READER}")))
        });
        let (args, _, _) = started(&records, READER);
        assert!(
            args.contains(&format!("--workspace-id {workspace}")),
            "the app starts in the workspace the caller works in: {args}"
        );
    }

    phase an_identity_backup_open_raises_setup_over_the_desktop;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let home = tilo_test_env::TestHome::new().unwrap();
        let records = home.home().join("starts");
        let launcher = launcher(&home, &records);
        let (mut session, opened) = session(&home, &launcher, None);
        session.step(Duration::from_millis(1)).unwrap();
        let workspace = front(&session).id().to_string();
        launch_as(&opened, FILES, &workspace);
        assert!(
            !session.setup_up(),
            "a desktop that is used has nothing over it"
        );

        // The backup is a layer only a pass of the loop raises, so it is asked from the app's own
        // thread while the session's loop runs.
        let reply = ask_while_stepping(&mut session, &home, workspace, ToolIntent::IdentityBackup);
        assert!(reply.accepted, "{}", reply.message);
        assert!(
            session.setup_up(),
            "the backup stands over the desktop, taking the keyboard until the person is done"
        );
    }

    phase open_in_terminal_starts_a_terminal_at_the_folder;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let records = home.home().join("starts");
        packages(&mut home, &[(TERMINAL, "Terminal", "")]);
        let launcher = launcher(&home, &records);
        let (mut session, opened) = session(&home, &launcher, None);
        session.step(Duration::from_millis(1)).unwrap();
        let workspace = front(&session).id().to_string();
        launch_as(&opened, FILES, &workspace);
        let folder = home.home().join("Documents");
        fs::create_dir_all(&folder).unwrap();
        let intent = ToolIntent::Terminal {
            directory: Some(folder.display().to_string()),
        };

        // The request waits for the pass that starts it, so it is asked from the app's own thread while
        // the session's loop runs.
        let asked = {
            let home = home.runtime().to_owned();
            let workspace = workspace.clone();
            let intent = intent.clone();
            std::thread::spawn(move || {
                let socket = home.join("tilo/services/tools.sock");
                let mut stream =
                    tilo_files::native::socket::connect(&socket).expect("the tools socket");
                let request = ToolRequest {
                    protocol: TOOL_PROTOCOL_VERSION,
                    workspace_id: workspace,
                    intent,
                    on_behalf_of: None,
                };
                let mut line = serde_json::to_vec(&request).unwrap();
                line.push(b'\n');
                stream.write_all(&line).unwrap();
                let mut reading = BufReader::new(stream);
                let mut answer = String::new();
                reading.read_line(&mut answer).unwrap();
                serde_json::from_str::<ToolReply>(&answer).expect("a tool reply")
            })
        };
        until(&mut session, "the terminal starts", |_| {
            starts(&records)
                .iter()
                .any(|(args, _, _)| args.contains(&format!("--app-id {TERMINAL}")))
        });
        let reply = asked.join().expect("the asking thread");
        assert!(reply.accepted, "{}", reply.message);

        let (args, context, _) = started(&records, TERMINAL);
        assert!(
            args.contains(&format!("--workspace-id {workspace}")),
            "the terminal opens in the workspace it was asked from"
        );
        assert_eq!(
            serde_json::from_str::<ToolIntent>(&context).expect("the tool context"),
            intent,
            "the terminal is told which folder to start in"
        );
    }
}

/// Whether the window titled `title` stands in front of its row.
fn in_front(session: &Session, title: &str) -> bool {
    session.handle().read(|tree| {
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        workspace.strips.iter().any(|strip| {
            let Some(front) = tree.active_group.get(&strip.id) else {
                return false;
            };
            strip
                .window_groups
                .iter()
                .any(|group| group.id == *front && group.tabs.iter().any(|tab| tab.title == title))
        })
    })
}
