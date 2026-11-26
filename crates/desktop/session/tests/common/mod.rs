//! What the headless-session tests share: a private home for a session's sockets, the frames a test
//! runs until the shell has done what it asked, the shell's laid-out tree read as assistive
//! technology hears it, a press on one of its controls, the person's programs at Tilo's API, an
//! agent's end of the MCP server, and the test-control socket `tilo-control` drives a session over.
//! Included once by `tests/it.rs` as `mod common`, as a directory is not a test target of its own:
//! the same helpers serve the REPL's test, the shell screen's, the test-control socket's and the
//! acceptance path's.

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
pub mod directory;
pub mod held;
pub mod linked;
pub mod people;
pub mod probe;
pub mod switcher;
pub mod tree;
pub mod window;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use tilo_session_core::layout;

use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    CatalogueQuery, CatalogueRow, Confirmation, InvocationReceipt, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_api::v1::routing::{encode_path, encode_query};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_service::{Service as _, Started};
use tilo_session::{AppSocket, Options, Session};
use tilolang_service::ResourceService;
use tilolang_tilo::host::Host;

/// The canvas, as assistive technology names it: the map the framed style draws its places
/// on. What a test waits for to know the canvas is up, since nothing else stands on an empty one.
pub const CANVAS: &str = "Canvas";
/// The framed style, the canvas, by its id: what a fixture asks the shell to show, whatever
/// the person's language calls it.
pub const FRAMES: &str = "so.tilo.shell:frames";
/// The caret pointing puts on the canvas (canvas.md §1), as assistive technology names it: the
/// field's own words for a person to read.
pub const CANVAS_CARET: &str = "Ask or find anything";
/// The Enter key, as the compositor's keyboard reads it.
pub const KEY_ENTER: u32 = 28;
/// The Escape key.
pub const KEY_ESCAPE: u32 = 1;
/// The launcher's field, announced the same way: the view's own words for it, not the state's name
/// in the code (`query`).
pub const LAUNCHER_SEARCH: &str = "Search apps, windows, actions and files";
/// The overview's field, announced the same way rather than by the state's name in the code
/// (`query`).
pub const OVERVIEW_SEARCH: &str = "Search windows";
/// The workspaces surface's field, announced the same way rather than by the state's name in the
/// code (`name`).
pub const WORKSPACE_NAME: &str = "Name a new workspace";
/// The display every headless session here draws on.
pub const WIDTH: i32 = 1280;
/// See [`WIDTH`].
pub const HEIGHT: i32 = 800;
/// The person at the session.
pub const PERSON: &str = "Pat";

/// The agent `id`, as the one who acts.
pub fn agent(id: &str) -> ui::tilo_actions::Actor {
    ui::tilo_actions::MemberId::agent(id, "ada").into()
}

/// The person `id`, as the one who acts.
pub fn as_person(id: &str) -> ui::tilo_actions::Actor {
    ui::tilo_actions::MemberId::person(id).into()
}
/// The notes app, as a package names it.
pub const NOTES: &str = "so.tilo.notes";
/// The mail app, as a package names it.
pub const MAIL: &str = "so.tilo.mail";
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

/// A directory only this user reaches, for the server, the app wire and the test-control socket.
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

/// The `n`th send the session carried out, as `session:send-to` reads, once it is there.
pub fn sent(session: &mut Session, n: usize) -> Value {
    wait_for(session, format!("send {n} to be carried out"), |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array()?.get(n).cloned())
    })
}

/// The newest send the session carried out, as `session:send-to` reads, once there is one.
pub fn last_sent(session: &mut Session) -> Value {
    wait_for(session, "the last send to be carried out", |session| {
        session.settle_services();
        session
            .read("session:send-to")
            .and_then(|read| read["sent"].as_array()?.last().cloned())
    })
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

/// A window of an app as it draws now, settled, kept as `name.png` where `TL_SHELL_SHOTS` names.
pub fn app_shot<A: tilo_app::framework::ResourceApp>(
    app: &mut tilo_app_test_host::TestApp<A>,
    name: &str,
) {
    let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    app.settle();
    std::fs::create_dir_all(&dir).expect("the captures' directory");
    app.harness()
        .render_png(&std::path::Path::new(&dir).join(format!("{name}.png")));
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

/// Where the control named `name` stands on the standard display, among everything the session
/// shows: the shell's parts, the desktop's own content (the card a tab shows in place of a window)
/// and the controls apps describe of their windows.
pub fn control_on_screen(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|control| control.name == name)
        .map(|control| control.rect)
        .filter(|&rect| on_display(rect, WIDTH, HEIGHT))
}

/// Press the control named `name` wherever the session shows it ([`control_on_screen`]) with the
/// pointer, as the person does.
pub fn press_control(session: &mut Session, name: &str) {
    press_where(session, name, |session| control_on_screen(session, name));
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

/// Move the pointer to the screen point `at`.
pub fn point_to(session: &mut Session, at: (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: at.0, y: at.1 })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
}

/// Press or let go of the left button where the pointer is.
fn left(session: &mut Session, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed,
        })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
}

/// Click the screen point `at`, as the person does.
pub fn click(session: &mut Session, at: (f64, f64)) {
    point_to(session, at);
    left(session, true);
    left(session, false);
}

/// Drag from the screen point `from` to `to` with the left button, in steps, as a hand does.
pub fn drag(session: &mut Session, from: (f64, f64), to: (f64, f64)) {
    point_to(session, from);
    left(session, true);
    for step in 1..=8 {
        let t = f64::from(step) / 8.0;
        point_to(
            session,
            (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t),
        );
        session.step(Duration::from_millis(16)).unwrap();
    }
    left(session, false);
}

/// Press and let go of the key `keycode`.
pub fn key(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// A screen point on the canvas's free ground: in its lower right, where no place stands until
/// something is put there.
pub fn free_point(session: &mut Session) -> (f64, f64) {
    let (x, y, w, h) = wait_for(session, "the canvas", |session| on_screen(session, CANVAS));
    (x + w * 0.7, y + h * 0.75)
}

/// Point at the screen point `at` on the canvas's free ground and say `text` there: the click puts
/// the caret there, the keys type into it and Enter sends it (canvas.md §1). The one way every
/// test asks or finds on the canvas.
pub fn point_and_say(session: &mut Session, at: (f64, f64), text: &str) {
    click(session, at);
    say(session, text);
}

/// Drag a rectangle on the canvas's free ground from `from` to `to` and say `text` in it: the
/// caret stands inside the rectangle, and what the words find fills it (canvas.md §1).
pub fn frame_and_say(session: &mut Session, from: (f64, f64), to: (f64, f64), text: &str) {
    drag(session, from, to);
    say(session, text);
}

/// Say `text` at the caret pointing put on the canvas: type it, then Enter.
pub fn say(session: &mut Session, text: &str) {
    until(session, "the caret where the person pointed", |session| {
        on_screen(session, CANVAS_CARET).is_some()
    });
    type_text(session, text);
    key(session, KEY_ENTER);
}

/// Type `text` on the keyboard, a key at a time, as the person does: each character's key on the
/// state's layout, with the modifiers it needs, pressed and let go before the next frame. A key held
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

/// The shell's preset switch on screen: the bar's menu, named by the preset the shell shows ("Showing
/// Strips", "Showing Desktop", …). Which one it names depends on the shell a test starts from, so a
/// test asks for the menu rather than for a name. The canvas keeps no switch on its bar
/// (`canvas.md` §7): there the presets are in the person's own menu, their face on the bar.
pub fn preset_menu(session: &Session) -> Option<String> {
    session
        .accessible_labels()
        .into_iter()
        .find(|label| label.starts_with("Showing ") && on_screen(session, label).is_some())
        .or_else(|| person_menu(session))
}

/// The person's own menu on the bar, their face, named for what it holds: on the canvas it holds
/// the styles and the workspaces.
pub fn person_menu(session: &Session) -> Option<String> {
    session.accessible_labels().into_iter().find(|label| {
        label.ends_with(": style and workspaces") && on_screen(session, label).is_some()
    })
}

/// What the canvas is called in the language the session speaks: [`CANVAS`] in English, the
/// shell's own catalogue's word for it in any other.
pub fn canvas_word() -> String {
    shell_word("canvas", CANVAS)
}

/// The shell's own word for the name `declared` its views pass to a map (`canvas`, `frameless`,
/// `desktop`), in the language the session speaks, read from the shell's catalogue; `otherwise`
/// where the shell names none.
fn shell_word(declared: &str, otherwise: &str) -> String {
    use tilolang_tilo::tilolang::messages;
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    let locale = tilo_ui_base::i18n::I18nManager::current_locale();
    let catalogue = std::fs::read_to_string(
        shell
            .join(messages::LOCALES_DIR)
            .join(format!("{locale}.ftl")),
    )
    .unwrap_or_default();
    let entry = format!("{declared} = ");
    std::fs::read_to_string(shell.join("app.ids"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix(entry.as_str()))
        .and_then(|id| messages::word(&catalogue, &messages::key_of(id.trim())))
        .unwrap_or_else(|| otherwise.to_owned())
}

/// The map the style `style` (`so.tilo.shell:frameless`) draws its places in: the name its view
/// passes (`canvas`, `frameless`, `desktop`) and the word an English shell names it by. `None` for a style
/// that draws no map of its own (the strips), whose bar's switch says which style it shows instead.
fn style_map(style: &str) -> Option<(&'static str, &'static str)> {
    match style {
        "so.tilo.shell:frames" => Some(("canvas", CANVAS)),
        "so.tilo.shell:frameless" => Some(("frameless", "Frameless")),
        "so.tilo.shell:desktop" => Some(("desktop", "Desktop")),
        _ => None,
    }
}

/// Whether the shell shows the Frames canvas: its canvas stands, and its bar carries no preset
/// switch.
pub fn canvas_shown(session: &Session) -> bool {
    on_screen(session, &canvas_word()).is_some()
        && !session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Showing ") && on_screen(session, label).is_some())
}

/// The words the shell lists the style `style` (`so.tilo.shell:frames`) under: its doc line, or its
/// view's name when its author wrote none, as the bar's switch and Settings both name it. A
/// session no daemon serves lists none; the shell's own styles are then named as a daemon would
/// offer them, by the one rule that finds a package's styles.
fn style_label(session: &Session, style: &str) -> Option<String> {
    let listed = styles(session);
    let found = if listed.is_empty() {
        tilolang_workspace::styles::installed(&[])
    } else {
        listed
    }
    .into_iter()
    .find(|found| found.style == style)?;
    [found.line, found.name, found.package]
        .into_iter()
        .find(|words| !words.is_empty())
}

/// Whether the shell's switch says it shows the style listed `label`: its words are the style's,
/// with the sentence its own language puts them in around them.
fn switch_says(session: &Session, label: &str) -> bool {
    session.accessible_labels().into_iter().any(|words| {
        words != label && words.ends_with(label) && on_screen(session, &words).is_some()
    })
}

/// Show the style `style` (`so.tilo.shell:frames`) on the shell, as the bar's switch does: the
/// shell's own `show_style` action, which every row of its menu runs, named by the style's id
/// rather than by the words the person's language gives it.
pub fn show_preset(session: &mut Session, style: &str) {
    // The shell's own words for it, read from the styles it lists: what its switch names the style
    // by, in whatever language the person reads.
    let label = wait_for(
        session,
        format!("the shell to list the {style} style"),
        |session| style_label(session, style),
    );
    // A session whose shell has no display runs no action of it: the bar that names the style is
    // drawn before the style is asked for.
    wait_for(session, "the shell to draw", |session| {
        (!session.shell_parts().is_empty()).then_some(())
    });
    // A shell already showing it says so, and refuses to show it again in the person's language:
    // what the switch names decides it. A style whose bar carries no switch — the canvas's, which
    // keeps only the clock and the status (`canvas.md` §7), and the desktop's, which carries the
    // window in front's commands — says it is shown by the map it draws.
    let shows = |session: &Session| {
        switch_says(session, &label)
            || style_map(style).is_some_and(|(declared, english)| {
                on_screen(session, &shell_word(declared, english)).is_some()
            })
    };
    if !shows(session) {
        session
            .run_shell_action("show_style", vec![Value::from(style)])
            .unwrap_or_else(|why| panic!("the shell shows {style}: {why}"));
    }
    wait_for(session, format!("the shell to show {style}"), |session| {
        shows(session).then_some(())
    });
}

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// and a note in it for each of `titles`, made through the app's own actions: their ids, in
/// order.
pub fn notebook(titles: &[&str]) -> (ResourceService, Vec<String>) {
    notebook_of(PERSON, titles)
}

/// [`notebook`] as kept by `person`, whom the notes are theirs and every step of it is theirs:
/// the id a session acts as on a device that has an identity of its own.
pub fn notebook_of(person: &str, titles: &[&str]) -> (ResourceService, Vec<String>) {
    let host = Host::new(
        tilo_actions::Actor::from(tilo_actions::MemberId::person(person)),
        || None,
    )
    .unwrap();
    host.load_builtin(
        include_str!("../../../../../apps/tl/discussion/app.tl"),
        include_str!("../../../../../apps/tl/discussion/app.ids"),
    )
    .unwrap();
    host.load_builtin(
        include_str!("../../../../../apps/tl/notes/app.tl"),
        include_str!("../../../../../apps/tl/notes/app.ids"),
    )
    .unwrap();
    host.root_of(NOTES).unwrap();
    let mut service = ResourceService::new(host, person);
    let book = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES}.Notebook"))
        .unwrap();
    let mut made = Vec::new();
    for title in titles {
        let Started::Done(note) = service
            .invoke(book.id.as_str(), "new_note", &Map::new())
            .unwrap()
        else {
            panic!("a tilolang action is done once committed");
        };
        let id = note.resources.first().cloned().unwrap();
        service
            .invoke(
                &id,
                // Actions are published by their names, as a caller reads them.
                "retitle",
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

/// A home for a computer of the test's own, set up as one of a person's devices: the identity
/// directory its daemon keeps the person's root in, the file secret store beside it, and the
/// sealed file store a headless machine keeps its workspace keys' vault key in ([`secrets`]):
/// without one the daemon holds no group keys, and every record it seals is refused.
///
/// # Panics
/// The home cannot be made.
#[track_caller]
pub fn device_home() -> tilo_test_env::TestHome {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let identity = home.root().join("identity");
    home.env()
        .set("TILO_IDENTITY_ROOT", &identity)
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .set("TILO_SECRETS_DIR", secrets());
    home
}

/// The sealed file store this process keeps its secrets in: a process opens the store
/// `TILO_SECRETS_DIR` names once, so every device of this test binary keeps its keys in the one
/// store, which outlives each test's home.
fn secrets() -> &'static Path {
    static STORE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-test-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the secret store's directory");
        dir
    })
}

/// The device's daemon, started in this process over `profile` with the language runtime mounted on
/// it, as the `tilo-runtime` program mounts it: the daemon reads the records of the built-in apps,
/// and the styles the store's packages offer, through it.
///
/// # Panics
/// The daemon does not start.
#[track_caller]
pub fn start_daemon(profile: &Path) -> tilo_runtime::daemon::Running {
    static MOUNTED: std::sync::Once = std::sync::Once::new();
    MOUNTED.call_once(|| {
        use tilo_runtime::device::capabilities::{Capabilities, mount};
        mount(Capabilities {
            tilolang: Some(Box::new(tilolang_workspace::host)),
            ..Default::default()
        });
    });
    tilo_runtime::daemon::Running::start(profile).expect("the device's daemon starts")
}

/// A device of a person's, of the test's own: its daemon, started in this process over `home` and
/// a profile of its own, serving the person founded on it. A daemon whose identity names no person
/// stays up serving the device's own actions alone, waiting to be founded, so a test that only
/// started it would wait forever for the person's services: `name` is founded here as a
/// computer's first set-up founds its person. `None` takes the account's own name, as that set-up
/// does.
///
/// # Panics
/// The profile cannot be made private, the daemon does not start, or it does not serve its person.
#[track_caller]
pub fn device_daemon(home: tilo_test_env::TestHome, name: Option<&str>) -> Device {
    use std::os::unix::fs::PermissionsExt as _;
    let profile = tempfile::tempdir().expect("a profile for the daemon");
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the profile is this user's");
    let within = tilo_test_env::hang_bound();
    let daemon = start_daemon(profile.path());
    // The directory the daemon keeps its sockets in: a profile named apart keeps them beside
    // itself, not inside it, so the endpoint it listens on says where they are.
    let sockets = daemon
        .native_socket()
        .parent()
        .expect("the daemon's endpoint is in its sockets directory")
        .to_path_buf();
    // This computer's person is founded by its daemon, as the first set-up on a machine founds
    // them: their root stays with this account, their certificate names this device, and their
    // services start once it does.
    let founded = tilo_api::daemon::DaemonPlace::at(&sockets, profile.path()).found(
        &tilo_api::daemon::Founding {
            name: name.map(str::to_owned),
            ..tilo_api::daemon::Founding::default()
        },
    );
    if let Err(error) = founded {
        // A daemon that ended while founding took its endpoint with it, so the refusal only
        // says nothing answers: what it stopped with says why.
        let stopped = daemon.stop();
        panic!("this computer's person is founded: {error}; the daemon stopped: {stopped:?}");
    }
    let person = match daemon.person(within) {
        Ok(person) => person,
        Err(error) => {
            // A daemon whose services could not start keeps why until it is stopped.
            let stopped = daemon.stop();
            panic!("the device's daemon serves its person: {error}; it stopped: {stopped:?}");
        }
    };
    Device {
        daemon,
        person,
        home,
        profile,
    }
}

/// A device of a person's, of the test's own: its daemon, the person it serves, and the home and
/// profile the daemon reads while it runs.
pub struct Device {
    /// The daemon, as this process runs it.
    pub daemon: tilo_runtime::daemon::Running,
    /// The person's root, as the daemon and their workspaces name them.
    pub person: String,
    /// The home the daemon's identity lives in.
    pub home: tilo_test_env::TestHome,
    /// The profile the daemon serves, its sockets inside it.
    pub profile: tempfile::TempDir,
}

/// A headless session as the shell of a device daemon of the test's own, started in this process
/// over a profile and a home of its own: the session is the daemon's person's (`options.person`
/// is theirs), on the link the daemon made for it, as a computer's session is on the daemon that
/// started it. What the daemon publishes in the person's own workspace (the styles the installed
/// packages offer among it) is what the session reads. The daemon stops after the session ends.
///
/// # Panics
/// The home, the daemon or the session does not start.
#[track_caller]
pub fn on_its_daemon(outputs: &[OutputConfig], options: Options) -> Session {
    on_its_daemon_with(outputs, |_| options)
}

/// [`on_its_daemon`] with the options `options` makes for the device once its daemon serves its
/// person: what the session runs that must be that person's, as the seeded apps are
/// ([`seeded_apps_of`]).
///
/// # Panics
/// The home, the daemon or the session does not start.
#[track_caller]
pub fn on_its_daemon_with(
    outputs: &[OutputConfig],
    options: impl FnOnce(&Device) -> Options,
) -> Session {
    // What the daemon says of the writes it refuses, at `RUST_LOG`'s word (warnings by default).
    people::install_log();
    let within = tilo_test_env::hang_bound();
    let device = device_daemon(device_home(), None);
    let options = options(&device);
    let Device {
        daemon,
        person,
        home,
        profile,
    } = device;
    let shell_link = daemon
        .link_shell(within)
        .expect("the daemon makes the session's link");
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person,
            shell_link: Some(shell_link),
            shell_daemon: Some(Box::new((daemon, profile, home))),
            ..options
        },
        outputs,
    )
    .expect("session")
}

impl Device {
    /// A headless session on this device, its own services keeping their files in `state`: the
    /// device started again.
    ///
    /// # Panics
    /// The daemon makes no link, or the session does not start.
    #[track_caller]
    pub fn session(&self, state: &Path) -> Session {
        self.session_with(Options {
            collab: false,
            state: Some(state.to_path_buf()),
            ..Options::default()
        })
    }

    /// [`Self::session`] started with `options`: the session is this device's person's, on the
    /// link its daemon makes for it.
    ///
    /// # Panics
    /// The daemon makes no link, or the session does not start.
    #[track_caller]
    pub fn session_with(&self, options: Options) -> Session {
        let link = self
            .daemon
            .link_shell(tilo_test_env::hang_bound())
            .expect("the daemon makes the session's link");
        Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                person: self.person.clone(),
                shell_link: Some(link),
                ..options
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        )
        .expect("a session")
    }
}

/// Who made the last step of the workspace's layout, once the workspace has the change that made
/// it and it is `verb`'s: a layout's change is written off the loop, so the record holds it a look
/// or more after the action.
pub fn last_step(session: &mut Session, verb: &str) -> ui::tilo_actions::Actor {
    wait_for(
        session,
        "the layout's change to reach the workspace",
        |session| {
            let (who, made) = session.layout().borrow_mut().last_step()?;
            (made == verb).then_some(who)
        },
    )
}

/// The state directory of a device that holds its workspaces: a home of its own, the throwaway
/// identity `dist/dev/nested-session.sh` mints, and a secret store. Made once for the test, and
/// kept as long as the test's thread lives: the home goes with that thread, and with it the
/// environment lock it holds, so the next test of the same process takes the environment in turn.
/// A home leaked for the process kept that lock past its test, and every later test that needed
/// the environment waited for it forever.
///
/// # Panics
/// The home, the identity or their directories cannot be made.
pub fn device_state() -> std::path::PathBuf {
    thread_local! {
        static STATE: (tilo_test_env::TestHome, std::path::PathBuf) = {
            let mut home = tilo_test_env::TestHome::new().expect("a home");
            let state = device_in(&mut home);
            (home, state)
        };
    }
    STATE.with(|(_, state)| state.clone())
}

thread_local! {
    /// The device keys this test's thread took as the machine's daemon: kept while the thread
    /// lives, so every session the test starts in this process signs as the device, and let go of
    /// with the thread, before the next test of the process runs. A test that starts a daemon of
    /// its own here ([`let_go_of_device_keys`]) lets go of them first.
    static TAKEN_KEYS: std::cell::RefCell<Vec<tilo_device_identity::persist::DeviceKeyLock>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Take the device key at `paths` for this test's thread, as the machine's daemon takes it: what
/// the identity provisioned afterwards is sealed with, and what the test starts signs with.
///
/// # Panics
/// The key cannot be made or read, or this test took it already.
pub fn take_device_key(paths: &tilo_device_identity::IdentityPaths) {
    let taken = tilo_identity::provisioning::device_key_taken_as_daemon(paths)
        .expect("this computer's device key");
    TAKEN_KEYS.with(|keys| keys.borrow_mut().push(taken));
}

/// Let go of every device key this test's thread took, so a daemon this test starts in this process
/// takes the key itself.
pub fn let_go_of_device_keys() {
    TAKEN_KEYS.with(|keys| keys.borrow_mut().clear());
}

/// Sets up a device in `home`: the throwaway identity `dist/dev/nested-session.sh` mints, kept in
/// the store every reader of this account's identity reads, and a secret store, so a session
/// there holds its workspaces. Its state directory.
///
/// # Panics
/// The identity or its directories cannot be made.
pub fn device_in(home: &mut tilo_test_env::TestHome) -> std::path::PathBuf {
    tilo_identity::sources::test_trust::install();
    let identity = tilo_identity::store::account_identity_dir(&home.config().join("tilo/identity"));
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let root = home.root().join("system");
    home.env().set("TILO_IDENTITY_ROOT", &root);
    let secrets = home.state().join("secrets");
    home.env().set("TILO_SECRETS_DIR", &secrets);
    // This process stands in for the machine's daemon while the identity is provisioned, and the
    // sessions this test starts sign with the key it took.
    take_device_key(&tilo_identity::provisioning::collaboration_identity_paths());
    tilo_identity::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");
    home.state().to_path_buf()
}

/// The headless session with the shell as the whole screen and the seeded apps published (notes,
/// tasks, chat and mail), its sockets at `root` as [`api_options`] places them and its state
/// beside them, and the framed canvas in front. The apps are the ones installed in this run's app store
/// ([`app_store`]), seeded as the daemon's person. The session is the shell of a device daemon of
/// its own ([`on_its_daemon`]): the workspace it shows, and the apps' resources kept there, are
/// the ones that daemon holds.
pub fn seeded_session(root: &Path) -> Session {
    let mut session = on_its_daemon_with(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        // Not `api_options`: its state is a device of another identity's, and the daemon's
        // identity is the one this session is.
        |device| Options {
            services: seeded_apps_of(device),
            test_control: Some(root.join("test-control.sock")),
            state: Some(root.join("state")),
            asks: Some(person_asked()),
            app_socket: AppSocket::At(root.join("app_wire.sock")),
            mcp_server: Some(root.join("agent.sock")),
            api_socket: Some(root.join("api.sock")),
            ..Options::default()
        },
    );
    // The shell shows the Frames canvas once it says so: what the canvas's own controls are called
    // is the person's language's, so a fixture reads the style, not their words.
    show_preset(&mut session, FRAMES);
    until(&mut session, "the canvas", |session| canvas_shown(session));
    kept_in_workspace(&mut session);
    session
}

/// Wait until the seeded apps' resources are kept in the workspace the session shows: what a test
/// reads of them, their ids included, is the workspace's from then on.
pub fn kept_in_workspace(session: &mut Session) {
    let kept = reached(session, |session| session.apps_kept_in_workspace());
    assert!(
        kept,
        "the seeded apps' resources in the workspace: not reached in {:?}, {}; the shell's \
         refusals: {:?}",
        tilo_test_env::hang_bound(),
        session.apps_in_workspace(),
        session.shell_refusals(),
    );
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
    // The app's row goes beside the rows of the apps installed before it.
    let registry = store.join("registry.toml");
    let rows = std::fs::read_to_string(&registry).unwrap_or_default();
    std::fs::write(
        &registry,
        format!(
            "{rows}[[entry]]\nid = \"{app}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .expect("the app's row");
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// The app store this test process's tilolang apps are installed in, made the first time it is
/// asked for: [`dev_apps::install`] puts each app, built from this tree into a signed `.tapp` ,
/// in the store, the way a device has them. Every session a test starts reads that store, as it
/// would read a device's own.
///
/// The store lives under the process's temporary directory (`$TMPDIR`), which the test harness
/// gives it and removes with it, so a process installs the apps once, into a store no other
/// process reads or writes; the packages themselves are built once for the run. `TILO_APPS_DATA_DIR`
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
            dev_apps::APPS.len(),
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
/// scenario, published for [`PERSON`]: what a development session seeds, each app's record a
/// service of its own, as the session runs them.
///
/// # Panics
/// The apps do not load.
pub fn seeded_apps() -> Vec<Box<dyn tilo_service::Service>> {
    seeded_apps_in(app_store())
}

/// [`seeded_apps`] for the tilolang apps installed in the store `store`: a test that installed
/// them in its own home's store reads them from there, as a device does.
///
/// # Panics
/// The apps do not load.
pub fn seeded_apps_in(store: &Path) -> Vec<Box<dyn tilo_service::Service>> {
    seeded_as(PERSON, None, store)
}

/// [`seeded_apps`] as `device`'s person and this device seed them, as a computer's session does
/// (`--seed-apps`): the apps' first scenarios are that person's, made on this device, so the
/// workspace its daemon keeps takes them as theirs.
///
/// # Panics
/// The apps do not load.
pub fn seeded_apps_of(device: &Device) -> Vec<Box<dyn tilo_service::Service>> {
    let (person, own_device) = tilo_session::identity::own_ids(&Options {
        person: device.person.clone(),
        ..Options::default()
    });
    seeded_as(&person, own_device.as_deref(), app_store())
}

fn seeded_as(
    person: &str,
    device: Option<&str>,
    store: &Path,
) -> Vec<Box<dyn tilo_service::Service>> {
    let catalog = package_store::Catalog::at(store, None);
    tilo_session::seeded_apps::seeded(person, device, &catalog)
        .expect("the seeded apps")
        .into_iter()
        .map(|app| Box::new(app) as Box<dyn tilo_service::Service>)
        .collect()
}

/// Every resource `services` publish, as the session's catalogue lists them.
pub fn published(services: &[Box<dyn tilo_service::Service>]) -> Vec<tilo_actions::ResourceSpec> {
    services
        .iter()
        .flat_map(|service| service.resources())
        .collect()
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
        dev_apps::APPS.len(),
        "every app of the development session is installed, the shell among them: {installed:?}"
    );
}

/// What a headless session the person drives from a program of theirs is started with: the shell
/// as the whole screen on a device that holds its workspaces ([`device_state`]), and at `root` its
/// app wire, the MCP server agents are admitted at, and Tilo's API for the person's own programs
/// ([`Api`]).
pub fn api_options(root: &Path) -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        collab: false,
        state: Some(device_state()),
        asks: Some(person_asked()),
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        mcp_server: Some(root.join("agent.sock")),
        api_socket: Some(root.join("api.sock")),
        ..Options::default()
    }
}

/// The person's layout as a shell reads it: the shell as it stands now,
/// every workspace, row, strip and place by key.
pub fn layout(session: &Session) -> layout::Reading {
    session.layout().borrow().reading()
}

/// An id as the layout reads it out: its whole number.
pub fn number(key: &str) -> u64 {
    key.parse().expect("an id reads as its number")
}

/// The places of the layout, by key.
pub fn places(session: &Session) -> Vec<layout::PlaceReading> {
    layout(session).places
}

/// The rows of the layout, by key.
pub fn rows(session: &Session) -> Vec<layout::RowReading> {
    layout(session).rows
}

/// The place of the layout `key` names.
pub fn place(session: &Session, key: u64) -> Option<layout::PlaceReading> {
    let key = key.to_string();
    places(session).into_iter().find(|place| place.key == key)
}

/// The strip of the layout `key` names.
pub fn strip(session: &Session, key: u64) -> Option<layout::StripReading> {
    let key = key.to_string();
    layout(session)
        .strips
        .into_iter()
        .find(|strip| strip.key == key)
}

/// The strip the place `key` stands in, as the layout reads it.
pub fn strip_of(session: &Session, key: u64) -> Option<layout::StripReading> {
    let place = place(session, key)?;
    strip(session, number(&place.strip))
}

/// The places of the layout that show `resource` .
pub fn placed(session: &Session, resource: &str) -> usize {
    places(session)
        .iter()
        .filter(|place| place.resource == resource)
        .count()
}

/// What each place of the layout shows, by the kind and label of the resource its `open`
/// resolves to: what the shell draws there.
pub fn placed_resources(session: &Session) -> Vec<(String, String)> {
    let resources = session.resources();
    places(session)
        .iter()
        .filter_map(|place| {
            tilo_actions::api::locate(
                &resources,
                &tilo_actions::ActionAddress::new(&place.resource, "open"),
            )
            .map(|(resource, _)| (resource.kind.clone(), resource.label.clone()))
        })
        .collect()
}

/// One request to the API a session serves its own person's programs on its socket
/// (`api.sock`): the socket answers as the device's own key, so the request carries no key. The
/// answer's JSON, or what the API refused it with.
pub fn api_request(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    api_request_typed(socket, method, path, body, None)
}

/// [`api_request`] as a generated SDK sends it: its `Tilo-Surface` header names `surface`, the
/// surface its types were written for.
pub fn api_request_typed(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&Value>,
    surface: Option<&str>,
) -> Result<Value, String> {
    use std::io::{Read as _, Write as _};
    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|error| format!("the session's API socket {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|error| format!("the session's API socket: {error}"))?;
    let body = body.map(ToString::to_string).unwrap_or_default();
    let surface = surface.map_or_else(String::new, |surface| {
        format!("Tilo-Surface: {surface}\r\n")
    });
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         {surface}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("the session's API socket: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("the session's API socket: {error}"))?;
    let mut reply = Vec::new();
    stream
        .read_to_end(&mut reply)
        .map_err(|error| format!("the session's API socket: {error}"))?;
    let split = reply
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "the API answered no HTTP reply".to_owned())?;
    let head = String::from_utf8_lossy(&reply[..split]).into_owned();
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| format!("the API answered no status: {head}"))?;
    let lower = head.to_ascii_lowercase();
    let body = if lower.contains("transfer-encoding: chunked") {
        chunks(&reply[split + 4..])?
    } else if let Some(length) = lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|length| length.trim().parse::<usize>().ok())
    {
        reply[split + 4..]
            .get(..length)
            .unwrap_or_default()
            .to_vec()
    } else {
        reply[split + 4..].to_vec()
    };
    let answered: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
    if (200..300).contains(&status) {
        Ok(answered)
    } else {
        let error = answered.get("error");
        let field = |name: &str| error.and_then(|error| error.get(name));
        let code = field("code").and_then(Value::as_str).unwrap_or("failed");
        let message = field("message")
            .and_then(Value::as_str)
            .unwrap_or("the API refused the call");
        Err(format!("{message} ({code})"))
    }
}

/// The body of a chunked answer.
fn chunks(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut whole = Vec::new();
    loop {
        let end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "a chunked answer ended early".to_owned())?;
        let size = std::str::from_utf8(&body[..end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.split(';').next()?.trim(), 16).ok())
            .ok_or_else(|| "a chunked answer has no size".to_owned())?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(whole);
        }
        let (chunk, rest) = body
            .split_at_checked(size)
            .ok_or_else(|| "a chunked answer ended early".to_owned())?;
        whole.extend_from_slice(chunk);
        body = rest.strip_prefix(b"\r\n").unwrap_or(rest);
    }
}

/// An action of the account's (`account/workspaces#list`) at the session's API: what it
/// answered, its `result`, or why it refused. One put to the person, who has not answered yet,
/// is refused as waiting for them.
pub fn api_account(
    socket: &Path,
    resource: &str,
    action: &str,
    args: &Value,
) -> Result<Value, String> {
    let path = format!("/v1/resources/{}/actions/{action}", encode_path(resource));
    let mut answer = api_request(socket, "POST", &path, Some(args))?;
    if answer.get("status").and_then(Value::as_str) == Some("pending") {
        return Err("Waiting for your approval".to_owned());
    }
    Ok(answer
        .get_mut("result")
        .map_or_else(|| json!({}), Value::take))
}

/// A program of the person's on this computer at the session's API (`tilolang repl --session`):
/// it acts as the person in the workspace in front, and its programs run in one program session,
/// as the REPL's lines do, so `undo` takes back the steps before it. Each request is answered on a
/// thread of its own while the test runs the session's loop.
pub struct Api {
    socket: std::path::PathBuf,
    workspace: String,
    program_session: Option<String>,
}

/// A request to the API still out.
pub struct Asked(std::sync::Arc<std::sync::OnceLock<Result<Value, String>>>);

impl Asked {
    /// The API's answer, or why it refused, running the session until it is in.
    pub fn answer(&self, session: &mut Session) -> Result<Value, String> {
        wait_for(session, "the API's answer", |_| self.0.get().cloned())
    }
}

impl Api {
    /// The API of the session whose sockets are at `root` ([`api_options`]), in the workspace in
    /// front.
    pub fn open(session: &mut Session, root: &Path) -> Self {
        let workspace = wait_for(session, "the workspace in front", |session| {
            session.handle().read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
        });
        Self {
            socket: root.join("api.sock"),
            workspace,
            program_session: None,
        }
    }

    /// Ask the workspace's `route` (`resources?path=…`,
    /// `resources/search/actions/find`), posting `body` when there
    /// is one, and answer nothing here: [`Asked::answer`] waits for it.
    pub fn send(&self, route: &str, body: Option<Value>) -> Asked {
        self.send_typed(route, body, None)
    }

    /// [`Self::send`] with types written for `surface` ([`api_request_typed`]).
    pub fn send_typed(&self, route: &str, body: Option<Value>, surface: Option<String>) -> Asked {
        let socket = self.socket.clone();
        let path = format!("/v1/workspaces/{}/{route}", self.workspace);
        let answered = std::sync::Arc::new(std::sync::OnceLock::new());
        let answer = std::sync::Arc::clone(&answered);
        std::thread::spawn(move || {
            let method = if body.is_some() { "POST" } else { "GET" };
            let asked =
                api_request_typed(&socket, method, &path, body.as_ref(), surface.as_deref());
            let _ = answer.set(asked);
        });
        Asked(answered)
    }

    /// The listing of the resources under `path` (the apps at `""`), from `cursor` on: the API's
    /// answer, or why it refused.
    pub fn listing(
        &self,
        session: &mut Session,
        path: &str,
        cursor: Option<&str>,
    ) -> Result<Value, String> {
        let path = encode_query(path);
        let route = match cursor {
            Some(cursor) => format!("resources?path={path}&cursor={}", encode_query(cursor)),
            None => format!("resources?path={path}"),
        };
        self.send(&route, None).answer(session)
    }

    /// The listing of the resources under `path` (the apps at `""`).
    ///
    /// # Panics
    /// The API refused it.
    pub fn ls(&self, session: &mut Session, path: &str) -> Value {
        self.listing(session, path, None)
            .unwrap_or_else(|why| panic!("{path} lists: {why}"))
    }

    /// The rows of `listing`.
    pub fn rows(listing: &Value) -> impl Iterator<Item = &Value> {
        listing["rows"].as_array().into_iter().flatten()
    }

    /// The action `id` of the resource `listing` lists, with what it takes.
    pub fn action(listing: &Value, id: &str) -> Option<Value> {
        listing["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|action| action["id"] == id)
            .cloned()
    }

    /// The parameter `name` of `action`.
    ///
    /// # Panics
    /// The action takes no such parameter.
    pub fn param(action: &Value, name: &str) -> Value {
        action["params"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|param| param["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("it takes {name}: {action}"))
    }

    /// What search finds for `query`, best first: the `search#find` action's answer.
    pub fn find(&self, session: &mut Session, query: &str) -> Vec<Value> {
        let found = self
            .send(
                "resources/search/actions/find",
                Some(json!({ "query": query })),
            )
            .answer(session)
            .unwrap_or_else(|why| panic!("search for {query}: {why}"));
        found["result"]["hits"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The session's whole answer to `program` in this program session: the lines the model
    /// reads (`output`, a resource by the reference a later program names it by), the lines a
    /// person reads (`text`), and whether it failed.
    pub fn program_answer(&mut self, session: &mut Session, program: &str) -> Value {
        let mut body = json!({"program": program});
        if let Some(running) = &self.program_session {
            body["session"] = running.clone().into();
        }
        let answer = self
            .send("programs", Some(body))
            .answer(session)
            .unwrap_or_else(|why| panic!("{program}: {why}"));
        if let Some(running) = answer["session"].as_str() {
            self.program_session = Some(running.to_owned());
        }
        answer
    }

    /// `program` in this program session: what a person reads of its answer, and whether it
    /// failed.
    pub fn run(&mut self, session: &mut Session, program: &str) -> (String, bool) {
        let answer = self.program_answer(session, program);
        (
            answer["text"].as_str().unwrap_or_default().to_owned(),
            answer["status"] == "failed",
        )
    }
}

/// An agent's end of the session's MCP server, on a thread of its own so the session runs while it
/// waits: it sends each request it is handed and hands back the answer to it.
pub struct Agent {
    asks: Sender<Value>,
    answers: Receiver<Value>,
    next: u64,
}

impl Agent {
    /// Connect to the server at `socket` as an agent of the person's, asking to be admitted.
    pub fn connect(socket: &Path) -> Self {
        let socket = socket.to_path_buf();
        let (asks, asked) = std::sync::mpsc::channel::<Value>();
        let (answer, answers) = std::sync::mpsc::channel::<Value>();
        std::thread::spawn(move || {
            let stream = UnixStream::connect(&socket).expect("the MCP server's socket");
            let mut writer = stream.try_clone().expect("a second handle on the server");
            let mut reader = BufReader::new(stream);
            for request in asked {
                writer
                    .write_all(format!("{request}\n").as_bytes())
                    .expect("the server reads");
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
        let agent = Self {
            asks,
            answers,
            next: 1,
        };
        agent
            .asks
            .send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "an agent", "version": "test"}
                }
            }))
            .unwrap();
        agent
    }

    /// An agent at the server at `socket`, admitted by the person once its question is asked
    /// of them ([`person_answers`]), for as long as this login lasts.
    pub fn admitted(session: &mut Session, socket: &Path) -> Self {
        let mut agent = Self::connect(socket);
        person_answers(session, allowed_for(0));
        agent.opened(session);
        agent
    }

    /// The answer to the last request, running the session until it is in.
    pub fn answer(&self, session: &mut Session) -> Value {
        wait_for(session, "the server's answer", |_| {
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

    /// Admitted, it says it is initialized and lists its tools.
    pub fn opened(&mut self, session: &mut Session) -> Vec<String> {
        let initialized = self.answer(session);
        assert!(
            initialized.pointer("/result/serverInfo").is_some(),
            "the agent is admitted: {initialized}"
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

    /// Run the program `program` through the server's `tilo` tool: its answer, and whether it is an
    /// error.
    pub fn run(&mut self, session: &mut Session, program: &str) -> (String, bool) {
        let reply = self.ask(
            session,
            "tools/call",
            &json!({"name": "tilo", "arguments": {"program": program}}),
        );
        let result = reply
            .get("result")
            .unwrap_or_else(|| panic!("{program} answered: {reply}"));
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<String>();
        (text, result["isError"] == true)
    }
}

thread_local! {
    /// The person's side of the questions the session on this test's thread is asked: what
    /// [`person_answers`] answers, as they would on the question's asked record.
    static ASKED: std::cell::RefCell<Option<tilo_service::AskReceiver>> =
        const { std::cell::RefCell::new(None) };
}

/// Where the session started on this thread is asked its questions, for [`person_answers`] to
/// answer: what [`Options::asks`] takes.
pub fn person_asked() -> (tilo_service::AskSender, tilo_service::AskReceiver) {
    let (sender, receiver) = tilo_service::asks::channel();
    ASKED.with(|person| *person.borrow_mut() = Some(receiver.clone()));
    (sender, receiver)
}

/// The person on this thread is asked on `receiver`: the side of the questions a login's desktop
/// services ask, which the session started beside them puts to the person.
pub fn person_asked_on(receiver: &tilo_service::AskReceiver) {
    ASKED.with(|person| *person.borrow_mut() = Some(receiver.clone()));
}

/// The question the session on this thread asks the person next, once it is asked, running the
/// session until it is.
#[track_caller]
pub fn person_asked_next(session: &mut Session) -> tilo_service::Question {
    let person = ASKED
        .with(|person| person.borrow().clone())
        .expect("the session is started with `person_asked`");
    wait_for(session, "a question for the person", |_| {
        person.pending().into_iter().next()
    })
}

/// The answer that allows a permission question for the `nth` of the durations it offers, as
/// they stand in the one list every prompt offers ([`tilo_action_policy::Lifetime::offered`]),
/// the timed one left out as a question of buttons leaves it out.
pub fn allowed_for(nth: usize) -> tilo_service::Answer {
    let choice = tilo_action_policy::Lifetime::offered(tilo_clock::unix_now_ms())
        .into_iter()
        .enumerate()
        .filter(|(_, lifetime)| !matches!(lifetime, tilo_action_policy::Lifetime::Until(_)))
        .nth(nth)
        .map(|(choice, _)| choice)
        .expect("that many durations are offered");
    tilo_service::Answer {
        choice: Some(choice),
        ..tilo_service::Answer::default()
    }
}

/// What the session on this thread asks the person now, unanswered.
pub fn person_asked_now() -> Vec<tilo_service::Question> {
    ASKED
        .with(|person| person.borrow().clone())
        .expect("the session is started with `person_asked`")
        .pending()
}

/// The person answers the question the session on this thread asks next with `answer`.
#[track_caller]
pub fn person_answers(session: &mut Session, answer: tilo_service::Answer) {
    let question = person_asked_next(session);
    let person = ASKED
        .with(|person| person.borrow().clone())
        .expect("the session is started with `person_asked`");
    assert!(
        person.answer(&question.handle, answer),
        "the question waits for its answer"
    );
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

/// The action frame that asks `message` of a session's control socket, as the line it sends.
pub fn action_frame(message: RemoteActionMessage) -> Value {
    serde_json::to_value(RemoteActionFrame::new(message)).expect("a frame")
}

/// The action message a session's control socket answered a frame with.
pub fn action_answer(answer: &Value) -> RemoteActionMessage {
    serde_json::from_value::<RemoteActionFrame>(answer.clone())
        .unwrap_or_else(|error| panic!("{answer} is no action frame: {error}"))
        .message
}

/// The frame that lists the first page of what a session publishes that mentions `needle`.
pub fn listing(needle: &str) -> Value {
    action_frame(RemoteActionMessage::CatalogueRequest {
        correlation: 1,
        query: CatalogueQuery {
            needle: needle.to_owned(),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        },
    })
}

/// The rows a listing frame was answered with.
pub fn listed(answer: &Value) -> Vec<CatalogueRow> {
    match action_answer(answer) {
        RemoteActionMessage::CatalogueResponse {
            result: Ok(page), ..
        } => page.rows,
        other => panic!("a listing was answered with {other:?}"),
    }
}

/// The request that invokes `target` with `args` (none when empty), holding its answer up to
/// `wait_ms` for it to settle.
pub fn invocation(
    target: &RemoteActionTarget,
    args: &Value,
    confirmation: Option<Confirmation>,
    wait_ms: Option<u32>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: target.clone(),
        argument: match args.as_object() {
            Some(args) if !args.is_empty() => InvokeArgument::Json(args.clone()),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation,
        revision: None,
        on_behalf_of: None,
        wait_ms,
        appearance: None,
        signed: None,
    }
}

/// The frame that invokes `target` with `args`, answered once it is admitted.
pub fn invoking(target: &RemoteActionTarget, args: &Value) -> Value {
    action_frame(RemoteActionMessage::Invoke(invocation(
        target, args, None, None,
    )))
}

/// The code an invocation frame was refused with, or `None` when it was admitted.
pub fn refused(answer: &Value) -> Option<&'static str> {
    match action_answer(answer) {
        RemoteActionMessage::Receipt(InvocationReceipt {
            status: ReceiptStatus::Rejected { error },
            ..
        }) => Some(error.code()),
        RemoteActionMessage::Receipt(_) => None,
        other => panic!("an invocation was answered with {other:?}"),
    }
}

/// The surface the shell shows over the windows now, as the surface stack's reading names it:
/// `none` at rest, `centre` for the notification centre, `quick-settings`, `launcher` and the rest.
#[must_use]
pub fn shell_here(session: &Session) -> String {
    surface_stack(session)
        .get("here")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Whether the switcher's panel shows, as the surface stack's reading says.
#[must_use]
pub fn switcher_revealed(session: &Session) -> bool {
    surface_stack(session)
        .get("switcher_revealed")
        .and_then(Value::as_bool)
        .unwrap_or_default()
}

fn surface_stack(session: &Session) -> Value {
    session
        .read("desktop:overlays")
        .expect("the session publishes its surface stack")
}

/// The name the shell shows the place `record` is on in its bars and menus: the window's own name,
/// resolved once by the session for the bar, the chips, the switcher and the overview, and its
/// app's name beside it.
///
/// # Panics
/// The shell holds no place for `record`.
#[track_caller]
pub fn place_title(session: &Session, record: &str) -> String {
    places(session)
        .into_iter()
        .find(|place| place.resource == record)
        .map_or_else(
            || panic!("the shell holds a place for {record}"),
            |place| place.name,
        )
}

/// What the canvas draws the place `record` by: its own title, or its window's name where it has
/// none.
///
/// # Panics
/// The shell holds no place for `record`.
#[track_caller]
pub fn drawn_name(session: &Session, record: &str) -> String {
    places(session)
        .into_iter()
        .find(|place| place.resource == record)
        .map_or_else(
            || panic!("the shell holds a place for {record}"),
            |place| {
                if place.title.is_empty() {
                    place.name
                } else {
                    place.title
                }
            },
        )
}

/// Place what "Inbox" finds on the canvas through the API, as the person's program does, and wait
/// until the place shows the seeded conversation.
pub fn place_the_inbox(session: &mut Session, root: &Path) {
    let (answer, failed) = Api::open(session, root).run(
        session,
        "session.place_found(text: \"Inbox\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    until(
        session,
        "the inbox's conversation on the canvas",
        |session| session.shell_says(SUBJECT),
    );
}

/// The shell's windows, as `session:windows` publishes them.
pub fn windows(session: &Session) -> Vec<tilo_session::windows::Window> {
    session
        .read("session:windows")
        .and_then(|windows| serde_json::from_value(windows["all"].clone()).ok())
        .expect("the session publishes its windows")
}

/// The styles the session's shell lists: what the device's daemon publishes in the person's own
/// workspace ([`STYLES_APP`](tilo_workspace_service::styles::STYLES_APP)), each marked as this
/// session's shell last loaded it. Empty for a session no daemon serves.
pub fn styles(session: &Session) -> Vec<tilo_workspace_service::styles::Style> {
    session
        .read(tilo_workspace_service::styles::STYLES_APP)
        .and_then(|styles| serde_json::from_value(styles["offered"].clone()).ok())
        .expect("the session shows the styles")
}

/// The resource publishing `action` among everything the session publishes.
pub fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[tilo_actions::ResourceSpec], action: &str) -> Option<String> {
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

/// What the device's record `name` holds of what a session keeps for the next one, once every write
/// handed to `records` before this was answered.
///
/// # Panics
/// The record could not be read.
pub fn kept(
    records: &tilo_session_client::device_records::DeviceRecords,
    name: &str,
) -> Option<serde_json::Value> {
    records.flush();
    records
        .record(name)
        .read()
        .unwrap_or_else(|error| panic!("{name} is read: {error}"))
}
