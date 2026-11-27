//! A Terminal window's shell, as a person's device runs it: the app's process holds no route of
//! its own to the shell runtime, so it asks the daemon for each shell on its own line, and the
//! daemon opens it on its one connection to the runtime's supervisor and carries it back. Here
//! the supervisor is the real one and the daemon's end of the line is the daemon's own lane; the
//! window is the app's window, typed into as a person types.

use std::io::{BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value as Json, json};
use tilo_action_services::{LineClient, serve_lines_with};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, HostedContentPuts,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilolang_tilo::host::RootView;
use ush::control_line::ControlLine;

use crate::change_marks::{Driven as _, action_id};

const TERMINAL: &str = "so.tilo.terminal";

/// What the window says when its shell could not start: its card, whose one action ("Try
/// again") opens the shell again, and is addressed by the card.
const CLIPBOARD: &str = "Let programs set the clipboard";
const REFUSED: &str = "The shell couldn't start";

/// How long the shell may take to do what the person asked of it.
const PATIENCE: Duration = Duration::from_secs(30);

/// The daemon's dispatch, which nothing here asks: the app's line carries its shells beside it.
#[derive(Clone)]
struct NoActions;

impl ActionProvider for NoActions {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// The person's settings, as the session's settings service keeps them for this process: an
/// app's preferences (`settings/apps/<app>`), each changed only through its field's setter and
/// published again whole. Nothing else of the platform is read here.
#[derive(Clone, Default)]
struct Settings {
    /// Each app's preferences, as their setters wrote them, and their newest reading not yet
    /// taken.
    apps: Arc<Mutex<std::collections::BTreeMap<String, Preferences>>>,
}

#[derive(Default)]
struct Preferences {
    stored: Map<String, Json>,
    fresh: Arc<Mutex<Option<String>>>,
}

/// What a window follows of one app's preferences: each new reading once.
struct Followed(Arc<Mutex<Option<String>>>);

impl tilolang_tilo::platform_apps::Reading for Followed {
    fn latest(&self) -> Option<Result<String, String>> {
        self.0.lock().unwrap().take().map(Ok)
    }
}

const NOTHING_READ: &str = "nothing reads the platform's resources here";

impl tilolang_tilo::platform_apps::PublishedSource for Settings {
    fn follow(
        &self,
        resource: &str,
        _wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn tilolang_tilo::platform_apps::Reading>, String> {
        if !resource.starts_with("settings/apps/") {
            return Err(NOTHING_READ.to_owned());
        }
        let (stored, fresh) = {
            let mut apps = self.apps.lock().unwrap();
            let app = apps.entry(resource.to_owned()).or_default();
            let section = (app.stored.clone(), Arc::clone(&app.fresh));
            drop(apps);
            section
        };
        // A section nobody set yet reads as its defaults.
        *fresh.lock().unwrap() = Some(Json::Object(stored).to_string());
        Ok(Box::new(Followed(fresh)))
    }

    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
    ) -> Result<(), String> {
        if !resource.starts_with("settings/apps/") {
            return Err(NOTHING_READ.to_owned());
        }
        let field = action
            .strip_prefix("set-")
            .ok_or_else(|| format!("`{action}` is no preference's setter"))?
            .replace('-', "_");
        let value = arguments
            .get("value")
            .cloned()
            .ok_or_else(|| format!("`{action}` takes the preference's value"))?;
        let mut apps = self.apps.lock().unwrap();
        let app = apps.entry(resource.to_owned()).or_default();
        app.stored.insert(field, value);
        *app.fresh.lock().unwrap() = Some(Json::Object(app.stored.clone()).to_string());
        drop(apps);
        Ok(())
    }
}

/// The settings service this test binary reads through: a process installs its source once.
fn settings() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tilolang_tilo::platform_apps::install(Box::new(Settings::default()));
    });
}

/// The signed-in person owns this device, as the files sign-in and provisioning write say: the
/// device's own shell is theirs to open once they allow it.
fn signed_in_owner(dir: &Path, env: &mut tilo_test_env::ScopedEnv) {
    let key = [7_u8; 32];
    let root = dir.join("identity-root");
    std::fs::create_dir_all(root.join("user-identity")).unwrap();
    std::fs::write(root.join("user-identity/pubkey.bin"), key).unwrap();
    let config = dir.join("config");
    std::fs::create_dir_all(config.join("tilo/identity")).unwrap();
    std::fs::write(
        config.join("tilo/identity/primary.json"),
        json!({"threshold_identity": {"group_public_key": key}}).to_string(),
    )
    .unwrap();
    env.set("TILO_IDENTITY_ROOT", &root);
    env.set("XDG_CONFIG_HOME", &config);
}

/// The device's shell runtime's supervisor, serving at a socket under `dir`.
fn supervisor(dir: &Path) -> PathBuf {
    let socket = dir.join("ush.sock");
    let store = ush::Store::at(dir.join("ush"));
    let serving = socket.clone();
    std::thread::spawn(move || ush::supervisor::serve(&serving, &store, Path::new("/")));
    let until = Instant::now() + PATIENCE;
    while !socket.exists() {
        assert!(Instant::now() < until, "the supervisor never listens");
        std::thread::sleep(Duration::from_millis(10));
    }
    socket
}

/// The app's line to its daemon, the daemon's end carrying its shells on `control`: what the
/// daemon hands an app's own process.
fn line_to(control: &'static ControlLine) -> LineClient {
    let (app, daemon) = UnixStream::pair().unwrap();
    let writer = Mutex::new(daemon.try_clone().unwrap());
    let out: tilo_action_services::line::shells::Out = Arc::new(move |line: String| {
        let _ = writeln!(writer.lock().unwrap(), "{line}");
    });
    let shells = tilo_action_services::line::shells::AppShells::new(
        "",
        Ok(()),
        Arc::new(|| Ok(())),
        Arc::new(ush::app_shells::Runtime(control)),
        out,
    );
    let input = BufReader::new(daemon.try_clone().unwrap());
    std::thread::spawn(move || {
        serve_lines_with(
            |_| Ok((NoActions, Caller::local_system())),
            &HostedContentPuts::default(),
            None,
            input,
            daemon,
            Duration::from_secs(5),
            Some(&shells),
        );
    });
    LineClient::of_stream(app).unwrap()
}

/// The window's session record.
fn session_of(app: &super::Session) -> tilolang_tilo::runtime::ResourceRef {
    app.instance_host()
        .root_of_layer(
            TERMINAL,
            &RootView {
                root: Some("session".to_owned()),
                view: None,
            },
        )
        .expect("the window shows its session")
}

/// What the window's session says of itself: `field` of its record, as text.
fn said(app: &super::Session, field: &str) -> String {
    let session = session_of(app);
    match app.instance_host().runtime().read(&session, field) {
        Ok(tilolang_tilo::runtime::Value::Text(text)) => text,
        Ok(other) => format!("{other:?}"),
        Err(error) => panic!("the session reads its {field}: {error:?}"),
    }
}

/// The address the window publishes its session at: the resource offering its "Inspect session".
fn session_address(app: &mut super::Session) -> String {
    let resources = app.harness().resources();
    let published = crate::change_marks::flattened(&resources);
    published
        .iter()
        .find(|resource| {
            resource
                .actions
                .iter()
                .any(|action| action.label == "Inspect session")
        })
        .map_or_else(
            || {
                let ids: Vec<_> = published.iter().map(|r| r.id.as_str()).collect();
                panic!("the window publishes no session, only {ids:?}")
            },
            |resource| resource.id.as_str().to_owned(),
        )
}

/// How the window's session stands, in its own words (`failed: This device, why`), as its
/// "Inspect session" action says it.
fn standing(app: &mut super::Session) -> String {
    let session = session_address(app);
    let inspect = action_id(app, &session, "Inspect session");
    let answer = app
        .invoke(None, None, &session, &inspect, &Map::new())
        .expect("the session says how it stands");
    match answer.value {
        Some(Json::String(text)) => text,
        other => format!("{other:?}"),
    }
}

/// A few of the host's turns and the window's frames: a window with a live shell never rests,
/// since what the shell prints keeps arriving, so it is turned rather than settled.
fn pump(app: &mut super::Session) {
    for _ in 0..4 {
        let changed = app.host.turn();
        let framed = app.window.frame();
        let _ = app.dispatch();
        if !changed && !framed && !app.host.owes_work() {
            break;
        }
    }
}

/// Settle the window until `done` says it is where the person waits for it.
fn wait_for(app: &mut super::Session, what: &str, done: impl Fn(&mut super::Session) -> bool) {
    let until = Instant::now() + PATIENCE;
    loop {
        pump(app);
        if done(app) {
            return;
        }
        assert!(
            Instant::now() < until,
            "{what}: the session's trouble is {:?}, its screen {:?}",
            said(app, "trouble"),
            screen(app)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether the window's session holds a shell its device runs now, as the engine says: read
/// without the window, which a live shell never lets rest.
fn live(app: &mut super::Session) -> bool {
    let shell = said(app, "shell");
    !shell.is_empty()
        && tilo_terminal::ops::call(
            "inspect",
            &json!({ "shell": shell }).to_string(),
            &tilo_terminal::ops::NoOpener,
        )
        .ok()
        .and_then(|inspect| serde_json::from_str::<Json>(&inspect).ok())
        .is_some_and(|inspect| inspect["ended"] == false)
}

/// What the engine says of the window's shell now.
fn inspected(app: &super::Session) -> Json {
    let shell = said(app, "shell");
    let inspect = tilo_terminal::ops::call(
        "inspect",
        &json!({ "shell": shell }).to_string(),
        &tilo_terminal::ops::NoOpener,
    )
    .expect("the window's shell is the engine's");
    serde_json::from_str(&inspect).unwrap()
}

/// Where the window draws its grid: x, y, width and height.
fn grid_rect(app: &mut super::Session) -> [f64; 4] {
    app.harness().relayout();
    let inspection = app.harness().inspection();
    let nodes = inspection.root.nodes();
    nodes
        .iter()
        .find(|node| node.widget == "Terminal")
        .map_or_else(
            || {
                let shown: Vec<_> = nodes.iter().map(|node| node.widget.clone()).collect();
                panic!("the window shows no shell: {shown:?}")
            },
            |node| node.rect,
        )
}

/// Open the grid's menu, as a person does with a secondary click on it.
fn open_grid_menu(app: &mut super::Session) {
    let [x, y, width, height] = grid_rect(app);
    app.harness()
        .right_click_at(x + width / 2.0, y + height / 2.0);
    pump(app);
}

/// How the grid's menu shows the clipboard setting, read with the menu open, then closed again.
fn menu_check(app: &mut super::Session) -> Option<ui::accesskit::Toggled> {
    open_grid_menu(app);
    let (_, row) = app
        .window
        .menu_item(CLIPBOARD)
        .expect("the setting in the menu");
    app.harness().key("Escape");
    pump(app);
    row.toggled()
}

/// Choose `name` in the grid's menu, pressed where it is drawn.
fn choose_in_grid_menu(app: &mut super::Session, name: &str) {
    open_grid_menu(app);
    let rect = app
        .window
        .menu_item(name)
        .unwrap_or_else(|| panic!("{name:?} in the grid's menu"))
        .1
        .bounds()
        .expect("the row is laid out");
    app.window.click_at(
        f64::midpoint(rect.x0, rect.x1),
        f64::midpoint(rect.y0, rect.y1),
    );
    pump(app);
}

/// The text the window's shell shows now.
fn screen(app: &super::Session) -> String {
    let shell = said(app, "shell");
    let read = tilo_terminal::ops::call(
        "read_screen",
        &json!({ "shell": shell }).to_string(),
        &tilo_terminal::ops::NoOpener,
    )
    .unwrap_or_default();
    serde_json::from_str::<Json>(&read).unwrap_or_default()["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The id of the control the window names `name`, when it shows one.
fn control_named(app: &mut super::Session, name: &str) -> Option<String> {
    app.harness().relayout();
    let inspection = app.harness().inspection();
    let nodes = inspection.root.nodes();
    let found = nodes
        .iter()
        .filter(|node| node.test_id.is_some())
        .find(|node| {
            node.semantics.as_ref().is_some_and(|access| {
                access.name.as_deref() == Some(name) || access.shown_name() == Some(name)
            })
        })
        .and_then(|node| node.test_id.clone());
    if found.is_none() {
        let named: Vec<_> = nodes
            .iter()
            .filter_map(|node| {
                let access = node.semantics.as_ref()?;
                Some((access.shown_name().map(str::to_owned), node.test_id.clone()))
            })
            .collect();
        eprintln!("no control named {name:?}; the window names {named:?}");
    }
    found
}

/// Type `line` at the window's prompt and press Enter, as the person does.
fn type_line(app: &mut super::Session, line: &str) {
    let [x, y, width, height] = grid_rect(app);
    app.harness().click_at(x + width / 2.0, y + height / 2.0);
    pump(app);
    app.harness().type_text(line);
    app.harness().key("Enter");
}

#[test]
fn a_terminal_window_runs_a_real_shell_its_refusals_are_offered_again_and_exit_closes_it() {
    settings();
    let dir = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    signed_in_owner(dir.path(), &mut env);
    let control: &'static ControlLine =
        Box::leak(Box::new(ControlLine::at(supervisor(dir.path()))));
    tilo_terminal::ops::shells_run_at(tilolang_process::daemon_line::shells_over(line_to(control)));
    let mut app = super::Session::new(TERMINAL, "ada");

    // The window opens its shell in the workspace's default environment, which no device service
    // keeps here: the supervisor refuses, the window says why and offers it again.
    wait_for(&mut app, "the default environment is refused", |app| {
        standing(app).starts_with("failed")
    });
    assert!(!said(&app, "trouble").is_empty(), "the refusal says why");
    assert!(
        control_named(&mut app, REFUSED).is_some(),
        "a refused shell is offered again"
    );

    // The person moves the session to their own shell on this device, which they have not
    // allowed yet: refused in the supervisor's words, and offered again.
    let session = session_address(&mut app);
    let open_in = action_id(&mut app, &session, "Open in…");
    let mut target = Map::new();
    target.insert("target".to_owned(), Json::String("host".to_owned()));
    let _ = app.invoke(None, None, &session, &open_in, &target);
    wait_for(&mut app, "the device's own shell is refused", |app| {
        standing(app).starts_with("failed") && said(app, "target") == "host"
    });
    let again = control_named(&mut app, REFUSED).expect("the refusal is offered again");

    // They allow it, as their Settings does, and try again: the shell opens, at the device.
    ush::host::hand_allow(Some(tilo_action_policy::Lifetime::Once));
    app.harness().click_named(&again).unwrap();
    wait_for(&mut app, "the device's own shell opens", live);

    // What they type runs in that shell, and what it prints is in the window.
    type_line(&mut app, "echo hi");
    wait_for(&mut app, "the shell's answer is in the window", |app| {
        screen(app).lines().any(|line| line.trim() == "hi")
    });

    // The person lets programs set the clipboard from the grid's own menu: the shell follows
    // at once, and the menu shows the setting on when it opens again.
    assert!(
        !inspected(&app)["allows_clipboard"].as_bool().unwrap(),
        "off until allowed"
    );
    assert_eq!(
        menu_check(&mut app),
        Some(ui::accesskit::Toggled::False),
        "the menu shows it off"
    );
    choose_in_grid_menu(&mut app, CLIPBOARD);
    for _ in 0..20 {
        pump(&mut app);
    }
    assert_eq!(
        menu_check(&mut app),
        Some(ui::accesskit::Toggled::True),
        "the menu shows it on"
    );
    wait_for(
        &mut app,
        "the shell lets programs set the clipboard",
        |app| inspected(app)["allows_clipboard"] == true,
    );

    // The shell exits, and its window closes, as the person closes a tab.
    let _ = ui::shell_tabs::take_requests();
    type_line(&mut app, "exit");
    let until = Instant::now() + PATIENCE;
    let mut asked = Vec::new();
    while !asked
        .iter()
        .any(|request| matches!(request, ui::shell_tabs::TabRequest::CloseThis))
    {
        assert!(
            Instant::now() < until,
            "the window never closed: its screen is {:?}",
            screen(&app)
        );
        pump(&mut app);
        asked.extend(ui::shell_tabs::take_requests());
        std::thread::sleep(Duration::from_millis(20));
    }
}
