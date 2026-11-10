//! An app whose model the session serves launches hosted (`docs/architecture/apps.md` §8 step 2,
//! `docs/designs/app-language/one-app-model.md` §4).
//! Whether an app is hosted follows from the session having its model module: a service that runs
//! the app's model publishes its record, and a launch the session makes for that app names the
//! resource it publishes, so the app's process opens no document of its own. No per-app code says
//! so, and the launcher hands the name from the session into the app's sandbox.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_model_host::ModelService;
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};

use common::PERSON;

/// The app the session serves here: Tasks, whose model runs in its own module.
const APP: &str = "so.tilo.tasks";
/// The record kind the model declares, and the address it is published at.
const TRACKER: &str = "tasks.tracker";
const ADDRESS: &str = "tasks.tracker";

/// The tracker's model module: `tasks-model` built for wasm32 by `tilo-model-host` 's build script,
/// the same code the app ships.
fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("tasks-model")
        .expect("tilo-model-host's build script built the module")
}

/// The session's document host for the tracker: the app's model running in its module, its record
/// published at [`ADDRESS`].
fn tracker() -> ModelService {
    let mut service = ModelService::new(PERSON).expect("a model host");
    service
        .genesis(APP, TRACKER, ADDRESS, &wasm())
        .expect("the tracker's record");
    service
}

/// Install [`APP`] in `home` 's own app store, its entrypoint the launcher's to start.
fn installed_tasks(home: &mut tilo_test_env::TestHome) {
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
             [resources]\n\
             kinds = [\"{TRACKER}\"]\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/tasks\"\n\
             type = \"gui\"\n",
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

/// What the last launch the launcher was handed named as its hosted resource, if it named one: each
/// start is recorded as its pid and then the resource, one field of it.
fn hosted_at(handed: &Path) -> Option<String> {
    let said = fs::read_to_string(handed).unwrap_or_default();
    let line = said.lines().next_back()?;
    line.split_once(' ')
        .map(|(_, resource)| resource.to_owned())
        .filter(|resource| !resource.is_empty())
}

/// Whether the launcher was handed any start yet.
fn launched(handed: &Path) -> bool {
    fs::read_to_string(handed).is_ok_and(|said| said.lines().any(|line| !line.trim().is_empty()))
}

/// A launcher that records the hosted resource each start was handed.
fn launcher_recording(home: &tilo_test_env::TestHome, handed: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s %s\\n' \"$$\" \"${{TILO_HOSTED_RESOURCE:-}}\" >> '{handed}'\nhold\n",
            handed = handed.display(),
        ),
    )
    .unwrap()
}

/// The apps this session starts, on its own display, through `launcher` .
fn apps_at(home: &tilo_test_env::TestHome, launcher: PathBuf) -> Rc<Apps> {
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    Rc::new(Apps::with_launcher(environment, None, Some(launcher)))
}

/// A desk with one window of `app` in it, as the session restores it.
fn one_window(app: &str) -> DesktopTree {
    let tab = Tab {
        id: 100,
        title: app.to_owned(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: app.to_owned(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let group = WindowGroup {
        id: 10,
        tabs: vec![tab],
        active_tab: 100,
        width: WindowWidth::Fraction(600),
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut strip = Strip::new(StripId(1), "Windows");
    strip.window_groups = vec![group];
    let mut home = Workspace::new(WorkspaceSlot(1), "Home");
    home.strips = vec![strip];
    let mut tree = DesktopTree {
        active: home.slot,
        workspaces: vec![home],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(1));
    tree.set_active_group(StripId(1), 10);
    tree
}

fn desk_file(state: &Path) -> PathBuf {
    state.join("tilo").join("desk.json")
}

/// A session whose desk has one window of [`APP`] in it, with the tracker's records published and
/// the apps it starts through `launcher` .
fn session(state: &Path, apps: Rc<Apps>, served: bool) -> Session {
    let services: Vec<Box<dyn tilo_service::Service>> = if served {
        vec![Box::new(tracker())]
    } else {
        Vec::new()
    };
    crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        state: Some(state.to_path_buf()),
        services,
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(apps),
        ..Options::default()
    })
}

#[test]
fn an_app_the_session_serves_restores_as_a_place_of_its_root() {
    // An app whose model the session serves has no window process: the window the person left
    // open comes back as the place its root stands in, drawn by the shell from the session's
    // reading, and the launcher is never handed it
    // (`docs/designs/app-language/one-app-model.md` §3).
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_tasks(&mut home);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let state = home.state().to_path_buf();
    fs::create_dir_all(desk_file(&state).parent().unwrap()).unwrap();
    fs::write(
        desk_file(&state),
        serde_json::to_vec(&one_window(APP)).expect("the desk serializes"),
    )
    .unwrap();

    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(&state, apps, true);
    common::until(&mut session, "the app's root on the canvas", |session| {
        common::placed(session, ADDRESS) > 0
    });
    assert!(
        !launched(&handed),
        "a served app opens no window process: its place is the shell's"
    );
}

#[test]
fn an_app_the_session_does_not_serve_opens_its_own_record() {
    // No service runs this app's model: nothing is hosted, so the launch names no resource and the
    // app keeps its record itself, as every app did before the session served one.
    let mut home = tilo_test_env::TestHome::new().unwrap();
    installed_tasks(&mut home);
    let handed = home.home().join("handed");
    let launcher = launcher_recording(&home, &handed);
    let state = home.state().to_path_buf();
    fs::create_dir_all(desk_file(&state).parent().unwrap()).unwrap();
    fs::write(
        desk_file(&state),
        serde_json::to_vec(&one_window(APP)).expect("the desk serializes"),
    )
    .unwrap();

    let apps = apps_at(&home, launcher.path().to_owned());
    let mut session = session(&state, apps, false);
    common::wait_for(&mut session, "the launch is made", |_| {
        launched(&handed).then_some(())
    });
    assert_eq!(
        hosted_at(&handed),
        None,
        "an app no service serves launches with no hosted resource"
    );
}
