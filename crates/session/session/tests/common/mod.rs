//! What the headless-session tests share: a private home for a session's sockets, the frames a test
//! runs until the desk has done what it asked, the shell's laid-out tree read as assistive
//! technology hears it, a press on one of its controls, the REPL's end of the agent door, and the
//! test-control socket `tilo-control` drives a session over.
//! Included by each test binary with `mod common;` , as a directory is not a test target of its
//! own: the same helpers serve the REPL's test, the shell screen's, the test-control socket's and
//! the acceptance path's.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures shared by the session's test binaries: a helper a binary does not use is not an error, and a failed step is the assertion"
)]

pub mod audio;
pub mod dev_apps;
pub mod people;
pub mod personal;
pub mod window;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_service::{Questions, Service as _, Started};
use tilo_session::{ActionPlane, Options, Session};
use tilolang_tilo::host::Host;
use tilolang_tilo::records_service::RecordService;
use tilolang_tilo::runtime::Principal;

/// The canvas's entry, as assistive technology names it: a field bound to a view's state takes the
/// name the view gives it for a person to read, its own words, rather than the state's name in the
/// code (`typed`).
pub const CANVAS_ENTRY: &str = "Find something, or ask";
/// The launcher's field, announced the same way: the view's own words for it, not the state's name
/// in the code (`query`).
pub const LAUNCHER_SEARCH: &str = "Search apps, windows, actions and files";
/// The overview's field, announced the same way rather than by the state's name in the code
/// (`query`).
pub const OVERVIEW_SEARCH: &str = "Search windows across your workspaces";
/// The workspaces surface's field, announced the same way rather than by the state's name in the
/// code (`name`).
pub const WORKSPACE_NAME: &str = "Name a new workspace";
/// The display every headless session here draws on.
pub const WIDTH: i32 = 1280;
/// See [`WIDTH`].
pub const HEIGHT: i32 = 800;
/// The person at the session.
pub const PERSON: &str = "Pat";
/// The notes app, as a package names it.
pub const NOTES: &str = "so.tilo.lang.notes";
/// The mail app, as a package names it.
pub const MAIL: &str = "so.tilo.lang.mail";
/// A seeded conversation in the inbox, as its subject reads.
pub const SUBJECT: &str = "Launch plan for the autumn release";
/// The left button of a two-and-more button pointer.
pub const BTN_LEFT: u32 = 0x110;
/// The note the shell's own screen tests place: a seeded one, the first the notes app holds.
pub const NOTE_TITLE: &str = "Groceries";
/// The titles the shell's screen tests keep, [`NOTE_TITLE`] first: one strip each.
pub const TITLES: [&str; 12] = [
    NOTE_TITLE,
    "Packing list",
    "Recipes",
    "Reading",
    "Garden",
    "Errands",
    "Budget",
    "Birthday ideas",
    "Books to read",
    "Trip plan",
    "Paint colours",
    "Plumber quotes",
];

/// A directory only this user reaches, for the door, the plane and the test-control socket.
pub fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the session's directory is this user's");
    root
}

/// Run frames until `done` holds; `false` when it never did.
pub fn reached(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) -> bool {
    steps_until(session, |session| done(session).then_some(())).is_some()
}

/// Step the session's loop until `probe` answers, or the suite's hang bound runs out: one wait,
/// whose only bound is the bound every other test helper uses.
fn steps_until<T>(
    session: &mut Session,
    probe: impl FnMut(&mut Session) -> Option<T>,
) -> Option<T> {
    steps_within(session, tilo_test_env::hang_bound(), probe)
}

/// Step the session's loop until `probe` answers, or `within` runs out.
/// One slice of a wait a test drives itself: a signal the product carries as live frames rather
/// than as state it keeps may be shed, so the test asks again between slices and keeps its own
/// deadline, instead of sampling once.
pub fn steps_within<T>(
    session: &mut Session,
    within: std::time::Duration,
    mut probe: impl FnMut(&mut Session) -> Option<T>,
) -> Option<T> {
    let deadline = std::time::Instant::now()
        .checked_add(within)
        .expect("the wait's bound fits in an Instant");
    loop {
        if let Some(answer) = probe(session) {
            return Some(answer);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        session.step(Duration::from_millis(1)).expect("step");
        std::thread::yield_now();
    }
}

/// Step the session's loop until `probe` answers, and return its answer.
/// The wait ends the moment the condition holds; its only bound is the suite's hang bound
/// (`TILO_TEST_HANG_BOUND_SECS`, two minutes by default), so a busy machine makes the wait longer
/// instead of failing it. A condition that never holds fails naming `what` , with the shell's
/// refusals and what it shows as the report: every wait in the session's tests goes through here or
/// [`tilo_test_env::wait_for`], never a frame count or a guessed deadline.
#[track_caller]
pub fn wait_for<T>(
    session: &mut Session,
    what: impl std::fmt::Display,
    mut probe: impl FnMut(&mut Session) -> Option<T>,
) -> T {
    match steps_until(session, &mut probe) {
        Some(answer) => answer,
        None => panic!(
            "gave up waiting for {what} after {:?}; it looks hung (set TILO_TEST_HANG_BOUND_SECS to wait longer).\n\
             the shell's refusals: {:?}\nthe session says: {:?}",
            tilo_test_env::hang_bound(),
            session.shell_refusals(),
            session.accessible_labels()
        ),
    }
}

/// [`wait_for`] without failing: whatever `probe` answers before the suite's hang bound runs out,
/// for a test that asserts on its own observation (and says what it saw) rather than taking the
/// helper's message.
#[track_caller]
pub fn wait_maybe<T>(
    session: &mut Session,
    mut probe: impl FnMut(&mut Session) -> Option<T>,
) -> Option<T> {
    steps_until(session, |session| probe(session))
}

/// [`wait_maybe`] for a wait that spans a step the product itself times out: `patience` is how long
/// the product allows that step, and the suite's hang bound is added after it, so a host that takes
/// longer than the suite's bound but less than the product's patience is not reported as hung — the
/// wait ends when the product says what became of the step, and its own message is what the test
/// reports when the wait runs out.
#[track_caller]
pub fn wait_maybe_past<T>(
    session: &mut Session,
    patience: Duration,
    mut probe: impl FnMut(&mut Session) -> Option<T>,
) -> Option<T> {
    steps_within(
        session,
        patience.saturating_add(tilo_test_env::hang_bound()),
        |session| probe(session),
    )
}

/// Run frames until `done` holds.
#[track_caller]
pub fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    assert!(
        reached(session, done),
        "{what}: not reached in {:?}; the shell's refusals: {:?}; the session says: {:?}",
        tilo_test_env::hang_bound(),
        session.shell_refusals(),
        session.accessible_labels()
    );
}

/// The standard display's screen now, kept as `name.png` where `TL_SHELL_SHOTS` names.
pub fn shot(session: &mut Session, name: &str) -> tilo_compositor::Screenshot {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a screenshot of the display");
    if let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("the captures' directory");
        image::save_buffer(
            dir.join(format!("{name}.png")),
            &shot.rgba,
            shot.width,
            shot.height,
            image::ColorType::Rgba8,
        )
        .expect("the capture written");
    }
    shot
}

/// How many pixels differ between two frames of the display: how much of the screen moved.
#[track_caller]
pub fn changed(before: &tilo_compositor::Screenshot, after: &tilo_compositor::Screenshot) -> usize {
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "the two frames are of one screen"
    );
    before
        .rgba
        .chunks_exact(4)
        .zip(after.rgba.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count()
}

/// Where the shell's control or region named `name` stands on the standard display, when it does.
pub fn on_screen(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    on_screen_at(session, name, WIDTH, HEIGHT)
}

/// Where the shell's control or region named `name` stands on a display `width` by `height`,
/// when it does.
pub fn on_screen_at(
    session: &Session,
    name: &str,
    width: i32,
    height: i32,
) -> Option<(f64, f64, f64, f64)> {
    session
        .shell_control(name)
        .filter(|&rect| on_display(rect, width, height))
}

/// Whether any of the rectangle `(x, y, w, h)` stands on a display `width` by `height`.
pub fn on_display((x, y, w, h): (f64, f64, f64, f64), width: i32, height: i32) -> bool {
    w > 0.0
        && h > 0.0
        && x + w > 0.0
        && y + h > 0.0
        && x < f64::from(width)
        && y < f64::from(height)
}

/// Press the shell's control named `name` on the standard display with the pointer, as the person
/// does.
pub fn press(session: &mut Session, name: &str) {
    press_at(session, name, WIDTH, HEIGHT);
}

/// Press the shell's control named `name` on a display `width` by `height` with the pointer, as the
/// person does.
pub fn press_at(session: &mut Session, name: &str, width: i32, height: i32) {
    press_where(session, &format!("the shell's {name}"), |session| {
        on_screen_at(session, name, width, height)
    });
}

/// Press with the pointer, as the person does, the control `find` answers where it stands: one a
/// name alone does not single out, so the test says which of the controls by that name it means (a
/// window's menu is named for its window, as its frame is). `what` names it when it never stands.
#[track_caller]
pub fn press_where(
    session: &mut Session,
    what: &str,
    find: impl Fn(&Session) -> Option<(f64, f64, f64, f64)>,
) {
    // The press lands where the control is drawn once it stood still across two frames. A surface
    // still arriving moves its controls from one frame to the next, and a press aimed at a place a
    // control has left misses it: the person waits for the menu to be there, and so does the test.
    let (x, y, w, h) = wait_for(session, format!("{what} to stand still"), |session| {
        let mut place = find(session)?;
        for _ in 0..2 {
            session.step(Duration::from_millis(16)).expect("step");
            let next = find(session)?;
            if next != place {
                return None;
            }
            place = next;
        }
        Some(place)
    });
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// Type `text` on the keyboard, a key at a time, as the person does: each character's key on the
/// seat's layout, with the modifiers it needs, pressed and let go before the next frame. A key held
/// across a frame would repeat whenever that frame outlasts the repeat delay, as one on a loaded
/// machine does.
pub fn type_text(session: &mut Session, text: &str) {
    for letter in text.chars() {
        assert!(
            session.compositor().type_char(letter).unwrap(),
            "no key types {letter:?}"
        );
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// The desk's preset switch on screen: the bar's menu, named by the preset the desk shows ("Showing
/// Strips", "Showing Frames", …). Which one it names depends on the desk a test starts from, so a
/// test asks for the menu rather than for a name.
pub fn preset_menu(session: &Session) -> Option<String> {
    session
        .accessible_labels()
        .into_iter()
        .find(|label| label.starts_with("Showing ") && on_screen(session, label).is_some())
}

/// Show `preset` ("Strips", "Frames", "Spaces", "Desktop") from the desk's own bar: open the preset
/// menu the bar carries and press the preset it offers.
/// A press that arrives before the menu stands where the desk lays it out dismisses the menu
/// instead of choosing from it, as it does for a person who presses again to get the row they aimed
/// at. So the wait presses again — asking the bar for the menu when a press closed it — until the
/// desk shows the preset, and a desk that never does is reported with what the bar said instead.
pub fn show_preset(session: &mut Session, preset: &str) {
    let shown = format!("Showing {preset}");
    // Turns since the last press: the menu opens on the turn after its own button's press, and
    // closes on the turn after a row's, so the wait reads the desk's answer before it presses
    // again.
    let mut since = usize::MAX;
    wait_for(
        session,
        format!("the desk to show the {preset} preset"),
        |session| {
            if preset_menu(session).as_deref() == Some(shown.as_str()) {
                return Some(());
            }
            if since < 4 {
                since = since.saturating_add(1);
                return None;
            }
            if on_screen(session, preset).is_some() {
                press(session, preset);
            } else if let Some(menu) = preset_menu(session) {
                press(session, &menu);
            }
            since = 0;
            None
        },
    );
}

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// and a note in it for each of `titles`, made through the app's own actions: their ids, in
/// order.
pub fn notebook(titles: &[&str]) -> (RecordService, Vec<String>) {
    notebook_of(PERSON, titles)
}

/// [`notebook`] as kept by `person`, whom the notes are theirs and every step of it is theirs:
/// the id a session acts as on a device that has an identity of its own.
pub fn notebook_of(person: &str, titles: &[&str]) -> (RecordService, Vec<String>) {
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(person), || None).unwrap();
    host.load_builtin(
        include_str!("../../../../../apps/tl/discussion/app.tl"),
        include_str!("../../../../../apps/tl/discussion/app.ids"),
    )
    .unwrap();
    let notes = host
        .load_builtin(
            include_str!("../../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../../apps/tl/notes/app.ids"),
        )
        .unwrap();
    host.root_of(NOTES).unwrap();
    for committed in host.take_committed() {
        let action = &committed.action;
        let context =
            ui::tilo_actions::ActionContext::for_resource("workspace", &action.kind, &action.kind)
                .with_invoker(ui::tilo_actions::ActionInvoker::actor(person));
        host.with_schemas(|| {
            ui::tilo_actions::apply_local_with_context(
                &mut record,
                action,
                action.encode().unwrap(),
                Some(person.as_bytes()),
                context,
            )
        })
        .unwrap();
        record.commit();
        host.sync_from(&record);
    }
    let action = |name: &str| {
        notes
            .meta
            .actions
            .iter()
            .find(|action| action.name == name)
            .unwrap()
            .id
            .clone()
    };
    let mut service = RecordService::new(record, host, person);
    let book = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES}.Notebook"))
        .unwrap();
    let mut made = Vec::new();
    for title in titles {
        let Started::Done(note) = service
            .invoke(book.id.as_str(), &action("new_note"), &Map::new())
            .unwrap()
        else {
            panic!("a tilolang action is done once committed");
        };
        let id = note.resources.first().cloned().unwrap();
        service
            .invoke(
                &id,
                &action("retitle"),
                json!({ "title": title }).as_object().unwrap(),
            )
            .unwrap();
        made.push(id);
    }
    service.settle();
    (service, made)
}

/// A headless session of the test's own: one `HEADLESS-1` output of [`WIDTH`] by [`HEIGHT`],
/// drawing the fixture runner's inputs, and `options` as the test needs them.
///
/// # Panics
/// The session does not start.
#[track_caller]
pub fn headless(options: Options) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..options
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// [`headless`] on the outputs `outputs` : a test that wants another display, or another size.
///
/// # Panics
/// The session does not start.
#[track_caller]
pub fn headless_on(outputs: &[OutputConfig], options: Options) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..options
        },
        outputs,
    )
    .expect("session")
}

/// The headless session with the shell as the whole screen and the seeded apps published (notes,
/// tasks, chat and mail), its agent door and test-control socket at `root`, admitting this
/// test's program to act as the person, and the framed canvas in front. The apps are the ones
/// installed in this run's app store ([`app_store`]).
pub fn seeded_session(root: &Path) -> Session {
    let program = std::env::current_exe().expect("this test's program");
    seeded_session_at(root, vec![program], None)
}

/// Install the app `app` — named `name` — in `home` 's own app store: a package whose entrypoint is
/// the launcher's to start, and whose provider scheme is its own.
///
/// # Panics
/// The store, its manifest or its row cannot be written.
pub fn probe_app(home: &mut tilo_test_env::TestHome, app: &str, name: &str) {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(app);
    std::fs::create_dir_all(payload.join("bin")).expect("the app's payload");
    let manifest = payload.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{app}\"\n\
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
             \n\
             [providers]\n\
             schemes = [\"probe\"]\n",
            key = "11".repeat(32),
        ),
    )
    .expect("the app's manifest");
    std::fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{app}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .expect("the app's row");
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// The app store this test process's tilolang apps are installed in, built the first time it is
/// asked for: [`dev_apps::install`] builds each app from this tree into a signed `.tapp` and
/// puts it in the store, the way a device has them. Every session a test starts reads that
/// store, as it would read a device's own.
///
/// The store lives under the run's temporary directory (`$TMPDIR`), which the test harness
/// gives the run and removes with it, so a process builds the apps once. `TILO_APPS_DATA_DIR`
/// names it for the rest of the process ([`tilo_test_env::pin`], written under the environment
/// lock the scopes take): the store outlives every test that reads it, as a device's does.
///
/// # Panics
/// A package does not build or install, or the host has no Tilo runtime to run them on.
pub fn app_store() -> &'static Path {
    static STORE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("tilo-test-apps-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("a directory for the apps");
        let installed = dev_apps::install(&root, &root.join("build"));
        assert_eq!(
            installed.len(),
            6,
            "every app of the development session is installed, the shell among them: {installed:?}"
        );
        // The store the environment names is the one the session's launcher reads, and the one its
        // connector planning plans from. Pinned for the rest of the process, as the store is: every
        // later test's session reads the same one, and a scope that names its own for a test is
        // taken after it and restores it.
        tilo_test_env::pin("TILO_APPS_DATA_DIR", &root);
        root
    })
}

/// The records of the tilolang apps installed in [`app_store`], each filled by its first
/// scenario, published for [`PERSON`]: what a development session seeds.
///
/// # Panics
/// The apps do not load.
pub fn seeded_apps() -> RecordService {
    seeded_apps_in(app_store())
}

/// [`seeded_apps`] for the tilolang apps installed in the store `store`: a test that installed
/// them in its own home's store reads them from there, as a device does.
///
/// # Panics
/// The apps do not load.
pub fn seeded_apps_in(store: &Path) -> RecordService {
    let catalog = package_store::Catalog::at(store, None);
    tilo_session::seeded_apps::seeded(PERSON, &catalog).expect("the seeded apps")
}

/// Build the tilolang apps a development session runs into the store `store`, from this tree's
/// packages, each naming the image's installed Tilo runtime.
///
/// # Panics
/// A package does not build or install, or the host has no Tilo runtime to run them on.
pub fn install_apps_in(store: &Path, scratch: &Path) {
    std::fs::create_dir_all(scratch).expect("a directory for the build");
    let installed = dev_apps::install(store, scratch);
    assert_eq!(
        installed.len(),
        6,
        "every app of the development session is installed, the shell among them: {installed:?}"
    );
}

/// [`seeded_session`] with `apps` starting them (a stand-in launcher a test brought), and the
/// programs named as admitted to act as the person ahead of time.
pub fn seeded_session_at(
    root: &Path,
    admit_repl: Vec<std::path::PathBuf>,
    apps: Option<Rc<dyn tilo_session::AppActions>>,
) -> Session {
    let seeded = seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(seeded)],
            questions: Some(Questions::new()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl,
            test_control: Some(root.join("test-control.sock")),
            apps,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    session
}

/// A headless session with the shell as the whole screen, the notes [`notebook`] published, its
/// agent door and action plane at `root`, the questions the door's admission is asked through,
/// and the programs named as admitted to act as the person ahead of time.
pub fn repl_session(
    root: &Path,
    questions: &Questions,
    admit_repl: Vec<std::path::PathBuf>,
    notes: RecordService,
) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(notes)],
            questions: Some(questions.clone()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The person's arrangement as a shell reads it: the desk as it stands now,
/// every workspace, row, strip and place by key.
pub fn arrangement(session: &Session) -> tilo_session::arrangement::Reading {
    session.arrangement().borrow().reading()
}

/// An id as the arrangement reads it out: its whole number.
pub fn number(key: &str) -> u64 {
    key.parse().expect("an id reads as its number")
}

/// The places of the arrangement, by key.
pub fn places(session: &Session) -> Vec<tilo_session::arrangement::PlaceReading> {
    arrangement(session).places
}

/// The rows of the arrangement, by key.
pub fn rows(session: &Session) -> Vec<tilo_session::arrangement::RowReading> {
    arrangement(session).rows
}

/// The place of the arrangement `key` names.
pub fn place(session: &Session, key: u64) -> Option<tilo_session::arrangement::PlaceReading> {
    let key = key.to_string();
    places(session).into_iter().find(|place| place.key == key)
}

/// The strip of the arrangement `key` names.
pub fn strip(session: &Session, key: u64) -> Option<tilo_session::arrangement::StripReading> {
    let key = key.to_string();
    arrangement(session)
        .strips
        .into_iter()
        .find(|strip| strip.key == key)
}

/// The strip the place `key` stands in, as the arrangement reads it.
pub fn strip_of(session: &Session, key: u64) -> Option<tilo_session::arrangement::StripReading> {
    let place = place(session, key)?;
    strip(session, number(&place.strip))
}

/// The places of the arrangement that show `resource` .
pub fn placed(session: &Session, resource: &str) -> usize {
    places(session)
        .iter()
        .filter(|place| place.record == resource)
        .count()
}

/// What each place of the arrangement shows, by the kind and label of the resource its `open`
/// resolves to: what the shell draws there.
pub fn placed_resources(session: &Session) -> Vec<(String, String)> {
    let resources = session.resources();
    places(session)
        .iter()
        .filter_map(|place| {
            tilo_actions::door::locate(
                &resources,
                &tilo_actions::ActionAddress::new(&place.record, "open"),
            )
            .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
        })
        .collect()
}

/// The REPL's end of the agent door, on a thread of its own so the session runs while it waits: it
/// sends each request it is handed and hands back the answer to it.
pub struct Repl {
    asks: Sender<Value>,
    answers: Receiver<Value>,
    next: u64,
}

impl Repl {
    /// Connect to the door at `socket` as `tilolang repl` does: asking to act as the person.
    pub fn connect(socket: &Path) -> Self {
        Self::asking(socket, true)
    }

    /// Connect to the door at `socket` , asking to act as the person (`as_person`) or as an agent
    /// of theirs.
    pub fn asking(socket: &Path, as_person: bool) -> Self {
        let socket = socket.to_path_buf();
        let (asks, asked) = std::sync::mpsc::channel::<Value>();
        let (answer, answers) = std::sync::mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let stream = UnixStream::connect(&socket).expect("the agent door's socket");
            let mut writer = stream.try_clone().expect("a second handle on the door");
            let mut reader = BufReader::new(stream);
            for request in asked {
                writer
                    .write_all(format!("{request}\n").as_bytes())
                    .expect("the door reads");
                let Some(id) = request.get("id").cloned() else {
                    continue;
                };
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let reply: Value = serde_json::from_str(&line).expect("a JSON reply");
                    if reply.get("id") == Some(&id) {
                        answer.send(reply).expect("the test waits");
                        break;
                    }
                }
            }
        });
        let repl = Self {
            asks,
            answers,
            next: 1,
        };
        let capabilities = if as_person {
            json!({"experimental": {"so.tilo": {"actsAs": "person"}}})
        } else {
            json!({})
        };
        repl.asks
            .send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": capabilities,
                    "clientInfo": {"name": "tilolang repl", "version": "test"}
                }
            }))
            .unwrap();
        repl
    }

    /// The answer to the last request, running the session until it is in.
    pub fn answer(&self, session: &mut Session) -> Value {
        wait_for(session, "the door's answer", |_| {
            self.answers.try_recv().ok()
        })
    }

    /// Send `method` with `params` and wait for its answer.
    pub fn ask(&mut self, session: &mut Session, method: &str, params: &Value) -> Value {
        self.next = self.next.saturating_add(1);
        self.asks
            .send(json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}))
            .unwrap();
        self.answer(session)
    }

    /// Call the door's tool `name` : its text, and whether it is an error.
    pub fn call(&mut self, session: &mut Session, name: &str, arguments: &Value) -> (String, bool) {
        let reply = self.ask(
            session,
            "tools/call",
            &json!({"name": name, "arguments": arguments}),
        );
        let result = reply
            .get("result")
            .unwrap_or_else(|| panic!("{name} answered: {reply}"));
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<String>();
        (text, result["isError"] == true)
    }

    /// Admitted, it says it is initialized and lists its tools, as the REPL does.
    pub fn opened(&mut self, session: &mut Session) -> Vec<String> {
        let initialized = self.answer(session);
        assert!(
            initialized.pointer("/result/serverInfo").is_some(),
            "the REPL is admitted: {initialized}"
        );
        self.asks
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .unwrap();
        let listed = self.ask(session, "tools/list", &json!({}));
        listed["result"]["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .collect()
    }

    /// Run the program `program` through the door's `tilo` tool: its answer, and whether it is an
    /// error.
    pub fn run(&mut self, session: &mut Session, program: &str) -> (String, bool) {
        self.call(session, "tilo", &json!({"program": program}))
    }
}

/// Press the portal sheet's `effect` , as its control does.
pub fn press_sheet(session: &mut Session, effect: &str, args: &Value) {
    fn publisher(resources: &[tilo_actions::ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                publisher(&resource.children, action)
            }
        })
    }
    let mut args = args.as_object().cloned().unwrap_or_default();
    if let Some(handle) = session
        .portal()
        .and_then(tilo_sheets::portal::PortalSeat::dialog)
        .map(|dialog| dialog.handle)
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(&session.resources(), effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

/// Send `request` on the test-control socket at `socket` as `tilo-control` does, running the
/// session until it answers.
pub fn control(session: &mut Session, socket: &Path, request: &Value) -> Value {
    let socket = socket.to_path_buf();
    let request = request.to_string();
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stream = UnixStream::connect(&socket).expect("the test-control socket");
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
    wait_for(session, "the test-control socket's answer", |_| {
        answered.try_recv().ok()
    })
}

/// The surface the desk shows over its windows now, as its own reading names it: `none` at rest,
/// `centre` for the notification centre, `quick-settings`, `launcher` and the rest.
#[must_use]
pub fn desk_here(session: &Session) -> String {
    let reading: Value = serde_json::from_value(
        session
            .read("session:desk")
            .expect("the session publishes its desk"),
    )
    .expect("the desk reads as its JSON");
    reading
        .get("here")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The name the shell shows the place `record` is on: the window's own name, resolved once by the
/// session for the bar, the chips, the switcher and the overview, and its app's name beside it.
///
/// # Panics
/// The desk holds no place for `record`.
#[track_caller]
pub fn window_name_of(session: &Session, record: &str) -> String {
    places(session)
        .into_iter()
        .find(|place| place.record == record)
        .map_or_else(
            || panic!("the desk holds a place for {record}"),
            |place| place.name,
        )
}

/// Place what "Inbox" finds on the canvas through the door, as the REPL's program does, and wait
/// until the place shows the seeded conversation.
pub fn place_the_inbox(session: &mut Session, root: &Path) {
    let mut repl = Repl::connect(&root.join("agent.sock"));
    repl.opened(session);
    let (answer, failed) = repl.call(
        session,
        "tilo",
        &json!({"program": "session.place_found(text: \"Inbox\", view: .none, x: .some(80), y: .some(60))"}),
    );
    assert!(!failed, "the program ran: {answer}");
    until(
        session,
        "the inbox's conversation on the canvas",
        |session| session.shell_says(SUBJECT),
    );
}
