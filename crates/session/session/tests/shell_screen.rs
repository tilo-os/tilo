//! The tilolang shell as the whole screen of the native session (headless backend): the
//! shell's arrangement root stands over every display, its bar, its dock and the person's
//! arrangement as their preset shows it, every window drawn in its place's well.
//!
//! A note goes onto the canvas through the arrangement's published `place`, as an agent puts
//! it there. The preset changes through the shell's own `show_preset`, which the preset switch
//! over the arrangement runs. What stands on screen is read from what the shell lays out for
//! assistive technology, never from pixels.
//!
//! With the shell mounted as a bar only, none of the arrangement stands on screen: no preset
//! switch, and the note placed is nowhere.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_service::{Service as _, Started};
use tilo_session::{Options, Session};
use tilo_surface_host::shell_part::ShellLayer;
use tilolang_tilo::host::Host;
use tilolang_tilo::records_service::RecordService;
use tilolang_tilo::runtime::Principal;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";
const NOTES: &str = "so.tilo.lang.notes";
const NOTE_TITLE: &str = "Groceries";
const BTN_LEFT: u32 = 0x110;
/// The framed canvas's entry, named for what it holds: the text typed to find what to place.
const CANVAS_ENTRY: &str = "Typed";

/// A notebook kept by the notes app, published as the session publishes a room's records, and
/// one note in it titled [`NOTE_TITLE`], made through the app's own actions.
fn notebook() -> (RecordService, String) {
    let mut record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new(PERSON), || None).unwrap();
    let notes = host
        .load_builtin(
            include_str!("../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../apps/tl/notes/app.ids"),
        )
        .unwrap();
    host.root_of(NOTES).unwrap();
    for committed in host.take_committed() {
        let action = &committed.action;
        let context =
            ui::tilo_actions::ActionContext::for_resource("room", &action.kind, &action.kind)
                .with_invoker(ui::tilo_actions::ActionInvoker::actor(PERSON));
        host.with_schemas(|| {
            ui::tilo_actions::apply_local_with_context(
                &mut record,
                action,
                action.encode().unwrap(),
                Some(PERSON.as_bytes()),
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
    let mut service = RecordService::new(record, host, PERSON);
    let book = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES}.Notes"))
        .unwrap();
    let Started::Done(made) = service
        .invoke(book.id.as_str(), &action("new_note"), &Map::new())
        .unwrap()
    else {
        panic!("a tilolang action is done once committed");
    };
    let note = made.resources.first().cloned().unwrap();
    service
        .invoke(
            &note,
            &action("retitle"),
            json!({ "title": NOTE_TITLE }).as_object().unwrap(),
        )
        .unwrap();
    service.settle();
    (service, note)
}

fn session(layer: ShellLayer) -> (Session, String) {
    let (notebook, note) = notebook();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(layer),
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, note)
}

/// Run frames until `done` holds; `false` when it never did.
fn reached(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) -> bool {
    for _ in 0..400 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return true;
        }
    }
    false
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, what: &str, done: impl FnMut(&mut Session) -> bool) {
    assert!(
        reached(session, done),
        "{what}: not reached; the shell's refusals: {:?}; the session says: {:?}",
        session.shell_refusals(),
        session.accessible_labels()
    );
}

/// Where the shell's control or region named `name` stands on screen, when it does.
fn on_screen(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session.shell_control(name).filter(|&(x, y, w, h)| {
        w > 0.0
            && h > 0.0
            && x + w > 0.0
            && y + h > 0.0
            && x < f64::from(WIDTH)
            && y < f64::from(HEIGHT)
    })
}

/// Press the shell's control named `name` with the pointer, as the person does.
fn press(session: &mut Session, name: &str) {
    let (x, y, w, h) = on_screen(session, name)
        .unwrap_or_else(|| panic!("the shell shows {name}: {:?}", session.accessible_labels()));
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

/// Place `resource` on the canvas of the workspace in front, through the arrangement's
/// published `place`.
fn place(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// The whole screen: the bar's controls stand on it, the preset changes what stands there, and
/// a note placed on the canvas stands on it as the notes app draws it.
#[test]
fn the_shell_is_the_screen_and_its_arrangement_follows_the_published_actions() {
    let _settled = ui::motion::testing::settled();
    let (mut session, note) = session(ShellLayer::Screen);

    // The desk is up: its bar's controls stand on screen, over the whole output.
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let bars = session.shell_bars();
    assert_eq!(
        bars.len(),
        1,
        "one shell part, on the one display: {bars:?}"
    );
    let (_, _, width, height) = bars[0].rect;
    assert_eq!(
        (i64::from(width), i64::from(height)),
        (i64::from(WIDTH), i64::from(HEIGHT)),
        "the shell covers the display"
    );
    // Showing the desk as the framed canvas is the shell's own action, which the preset
    // switch over the arrangement runs. The canvas's entry (typed text finds something and
    // places it on the canvas) stands on it; the strips have none.
    until(&mut session, "the preset switch", |session| {
        on_screen(session, "Frames").is_some()
    });
    assert!(
        on_screen(&session, CANVAS_ENTRY).is_none(),
        "the strips stand first, not the canvas"
    );
    press(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let entry = on_screen(&session, CANVAS_ENTRY).unwrap();

    // A note placed through the arrangement stands on the canvas, drawn by its app: its title
    // on screen, under the canvas's entry.
    place(&mut session, &note);
    until(&mut session, "the note's title on the canvas", |session| {
        on_screen(session, NOTE_TITLE).is_some()
    });
    let title = on_screen(&session, NOTE_TITLE).unwrap();
    assert!(
        title.1 >= entry.1 + entry.3,
        "the note stands on the canvas, under its entry: {title:?} under {entry:?}"
    );
}

/// With the shell as a bar only, the same action changes the record and nothing of the
/// arrangement stands on screen: no preset switch, no strips, no place. What the screen adds.
#[test]
fn as_a_bar_the_shell_shows_none_of_the_arrangement() {
    let _settled = ui::motion::testing::settled();
    let (mut session, note) = session(ShellLayer::Bar);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let place = place(&mut session, &note).to_string();
    assert!(
        !reached(&mut session, |session| {
            on_screen(session, "Frames").is_some() || on_screen(session, &place).is_some()
        }),
        "a bar shows none of the arrangement"
    );
}
