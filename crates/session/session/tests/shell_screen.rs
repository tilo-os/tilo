//! The tilolang shell as the whole screen of the native session (headless backend): the shell's
//! arrangement root stands over every display, its bar, its dock and the person's arrangement as
//! their preset shows it, every window drawn in its place's well.
//! A note goes onto the canvas through the arrangement's published `place` , as an agent puts it
//! there. The preset changes through the shell's own `show_preset` , which the preset switch over
//! the arrangement runs. What stands on screen is read from what the shell lays out for assistive
//! technology, never from pixels.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_session::{Options, Session};
use tilolang_tilo::records_service::RecordService;

use crate::common::{
    CANVAS_ENTRY, HEIGHT, NOTE_TITLE, PERSON, TITLES, WIDTH, notebook as notes, on_display,
    on_screen, press, press_where, show_preset, until,
};

/// What the strips show of a desk holding no window: their own empty state, which no other surface
/// of the desk shows.
const NO_WINDOW: &str = "No window here yet";

/// A notebook kept by the notes app, published as the session publishes a workspace's records,
/// and the notes in it titled [`TITLES`], made through the app's own actions: their ids, in order.
fn notebook() -> (RecordService, Vec<String>) {
    notes(&TITLES)
}

fn session() -> (Session, String) {
    let (session, notes) = session_at(WIDTH, HEIGHT);
    (session, notes.into_iter().next().unwrap())
}

/// The session with the shell, on one display `width` by `height` , and the notes [`notebook`]
/// keeps.
fn session_at(width: i32, height: i32) -> (Session, Vec<String>) {
    let (notebook, notes) = notebook();
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height)],
        Options {
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
    );
    (session, notes)
}

/// Place `resource` on the canvas of the workspace in front, through the arrangement's published
/// `place` .
fn place(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
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

/// The whole screen: the bar's controls stand on it, the preset changes what stands there, and a
/// note placed on the canvas stands on it as the notes app draws it.
#[test]
fn the_shell_is_the_screen_and_its_arrangement_follows_the_published_actions() {
    let _settled = ui::motion::testing::settled();
    let (mut session, note) = session();

    // The desk is up: its bar's controls stand on screen, over the whole output.
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    let bars = session.shell_parts();
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
    // Showing the desk as the framed canvas is the shell's own action, which the preset switch over
    // the arrangement runs. The canvas's entry (typed text finds something and places it on the
    // canvas) stands on it; the strips have none.
    assert!(
        on_screen(&session, CANVAS_ENTRY).is_none(),
        "the strips stand first, not the canvas"
    );
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    let entry = on_screen(&session, CANVAS_ENTRY).unwrap();

    // A note placed through the arrangement stands on the canvas, drawn by its app: its title on
    // screen, under the canvas's entry.
    place(&mut session, &note);
    let shown = shown_as(&session, NOTE_TITLE);
    until(&mut session, "the note's title on the canvas", |session| {
        on_screen(session, &shown).is_some()
    });
    let title = on_screen(&session, &shown).unwrap();
    assert!(
        title.1 >= entry.1 + entry.3,
        "the note stands on the canvas, under its entry: {title:?} under {entry:?}"
    );
}

/// The overview, run as the touchpad's pinch, the overview's key and an agent run it (the chrome's
/// published `overview` , through the door), is the shell's own on the shell's screen: its view
/// stands in what the shell lays out, and the same action closes it again.
#[test]
fn the_overview_through_the_door_is_the_shells_own_on_its_screen() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, NO_WINDOW).is_some()
    });
    assert!(
        on_screen(&session, OVERVIEW).is_none(),
        "the desk is at rest"
    );

    session
        .invoke(CHROME, "overview", &Map::new())
        .unwrap_or_else(|error| panic!("the overview opens: {error:?}"));
    until(&mut session, "the shell's overview", |session| {
        on_screen(session, OVERVIEW).is_some()
    });
    assert!(
        on_screen(&session, NO_WINDOW).is_none(),
        "the overview stands where the strips stood"
    );

    session
        .invoke(
            CHROME,
            "overview",
            json!({"open": false}).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the overview closes: {error:?}"));
    until(&mut session, "the strips again", |session| {
        on_screen(session, OVERVIEW).is_none() && on_screen(session, NO_WINDOW).is_some()
    });
}

/// What the chrome and the desk ask for shows on the shell: the bell, the status and the workspace
/// menu are the shell's notice centre, quick settings and workspaces, and the desk's search is the
/// shell's launcher — with what the person asked for, when a query comes with it.
#[test]
fn the_chromes_actions_show_the_shells_surfaces() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, NO_WINDOW).is_some()
    });
    for (action, surface, shown) in [
        ("notifications", "centre", "Notifications"),
        ("quick-settings", "quick-settings", "Quick settings"),
        ("workspace-menu", "workspaces", "Workspaces"),
    ] {
        assert_eq!(here(&session), "none", "the desk is at rest");
        session
            .invoke(CHROME, action, &Map::new())
            .unwrap_or_else(|error| panic!("{action} shows {surface}: {error:?}"));
        until(&mut session, surface, |session| here(session) == surface);
        // The surface is up in the shell's own state; what it draws is read once it has laid itself
        // out, which is the same wait as every other one here.
        until(
            &mut session,
            &format!("{action} drawing {shown}"),
            |session| on_screen(session, shown).is_some(),
        );
        session
            .invoke(CHROME, action, &Map::new())
            .unwrap_or_else(|error| panic!("{action} closes {surface}: {error:?}"));
        until(&mut session, "the desk at rest", |session| {
            here(session) == "none"
        });
    }

    // The desk's search is the shell's launcher.
    session
        .invoke(DESK_RESOURCE, "search", &Map::new())
        .unwrap_or_else(|error| panic!("search shows the launcher: {error:?}"));
    until(&mut session, "the launcher", |session| {
        here(session) == "launcher"
    });
    // Searching from a chord: the launcher opens on what was asked for.
    assert_eq!(here(&session), "launcher", "the launcher stays up");
    session
        .invoke(
            DESK_RESOURCE,
            "search",
            json!({"query": NOTE_TITLE}).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the search carries its query: {error:?}"));
    // The launcher holds what was asked for: its search-everywhere button names it.
    until(&mut session, "the launcher's query", |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == &format!("Search everywhere for {NOTE_TITLE}"))
    });
    assert_eq!(
        here(&session),
        "launcher",
        "still the launcher, on the query"
    );
}

/// The chrome's `switch-workspace` shows another workspace on the shell's screen: the one made,
/// through the door, and back by a step; the windows' workspace changes under it.
#[test]
fn switching_workspace_through_the_door_shows_it_on_the_shell() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, NO_WINDOW).is_some()
    });
    let first = active_workspace(&session);
    session
        .invoke(CHROME, "new-workspace", &Map::new())
        .unwrap_or_else(|error| panic!("a workspace is made: {error:?}"));
    until(&mut session, "the workspace made", |session| {
        session.handle().read(|tree| tree.workspaces.len()) == 2
    });
    let made = active_workspace(&session);
    assert_ne!(made, first, "the workspace made stands in front");

    session
        .invoke(
            CHROME,
            "switch-workspace",
            json!({"delta": -1}).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the workspaces step back one: {error:?}"));
    until(&mut session, "the first workspace again", |session| {
        active_workspace(session) == first
    });

    // By its key, too: the one named, as the workspaces view names it.
    let made_key = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|workspace| workspace.name == made)
            .map(|workspace| workspace.slot.0.to_string())
    });
    let made_key = made_key.expect("the workspace made is held");
    session
        .invoke(
            CHROME,
            "switch-workspace",
            json!({"workspace": made_key}).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the workspace by its key: {error:?}"));
    until(&mut session, "the workspace by its key", |session| {
        active_workspace(session) == made
    });
}

/// The name of the workspace in front, as the desk holds it.
fn active_workspace(session: &Session) -> String {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.name.clone())
        })
        .expect("a workspace is in front")
}

/// On a laptop's display and a phone's, the bar's switch between the presets and the strips under
/// it stand apart, and every note placed on the desk, one strip each, stands on the display once
/// placed: the desk shows what just arrived on it.
#[test]
fn the_preset_switch_and_the_strips_stand_apart_and_each_placed_note_is_on_screen() {
    for (width, height) in [(WIDTH, HEIGHT), (390, 844)] {
        let _settled = ui::motion::testing::settled();
        let (mut session, notes) = session_at(width, height);
        until(&mut session, "the preset switch", |session| {
            within(session, "Showing Strips", width, height).is_some()
        });
        for (index, (note, title)) in notes.iter().zip(TITLES).enumerate() {
            place(&mut session, note);
            let shown = shown_as(&session, title);
            until(
                &mut session,
                &format!("{shown} on screen at {width}x{height}"),
                |session| within(session, &shown, width, height).is_some(),
            );
            if index > 0 {
                continue;
            }
            // The switch and the first strip under it.
            let first = within(&session, &shown, width, height).unwrap();
            let switch = preset_switch(&session, "Strips");
            assert!(
                !intersects(switch, first),
                "at {width}x{height} the preset switch {switch:?} and the first strip {first:?} stand apart"
            );
        }
        // The framed canvas: the switch and the canvas's entry stand apart too.
        show_preset(&mut session, "Frames");
        until(&mut session, "the canvas's entry", |session| {
            within(session, CANVAS_ENTRY, width, height).is_some()
        });
        let entry = within(&session, CANVAS_ENTRY, width, height).unwrap();
        let switch = preset_switch(&session, "Frames");
        assert!(
            !intersects(switch, entry),
            "at {width}x{height} the preset switch {switch:?} and the canvas's entry {entry:?} stand apart"
        );
    }
}

/// A note placed in the strips preset stands as a window of the desk's height, and the body the
/// notes app draws in it has room of its own: its title field and its running text are laid out a
/// whole line tall, under the window's bar, rather than flat. A view that says `fill` fills the
/// slot it is put in, for a described component's body as for a `stack` 's, so no shell writes a
/// height to make a place's app the window's.
#[test]
fn a_placed_notes_body_stands_at_the_desks_height_under_one_bar() {
    let _settled = ui::motion::testing::settled();
    let (mut session, note) = session();
    until(&mut session, "the bar's preset menu", |session| {
        on_screen(session, "Showing Strips").is_some()
    });

    place(&mut session, &note);
    let shown = shown_as(&session, NOTE_TITLE);
    until(&mut session, "the placed note's window", |session| {
        on_screen(session, &shown).is_some()
    });
    // The window is the desk's height under the bar and the dock, not a line.
    let window =
        window_within(&session, &shown, WIDTH, HEIGHT).expect("the window is on the display");
    assert!(
        window.3 > f64::from(HEIGHT) / 2.0,
        "the placed window is the desk's height, not a line: {window:?}"
    );

    // The notes app's own body: its title field and the running text under it, each a whole line
    // tall and standing inside its window.
    let title = until_bounds(&mut session, "Untitled");
    let body = until_bounds(&mut session, "Block text");
    for (what, rect) in [("title", title), ("body", body)] {
        assert!(
            rect.3 >= 19.0,
            "the note's {what} is laid out a whole line tall, not flat: {rect:?}"
        );
        assert!(
            rect.0 >= window.0
                && rect.1 >= window.1
                && rect.0 + rect.2 <= window.0 + window.2 + 1.0
                && rect.1 + rect.3 <= window.1 + window.3 + 1.0,
            "the note's {what} stands inside its window: {rect:?} in {window:?}"
        );
    }
    // Under the window's bar: the bar is its tabs and the controls, and the app's body stands below
    // it rather than on its line. The window's menu is the first of its controls, named for the
    // window it is the menu of.
    let controls = window_menu(&session, &shown).expect("the window's controls stand");
    assert!(
        title.1 >= controls.1 + controls.3,
        "the note's title stands under the window's bar: {title:?} under {controls:?}"
    );
    assert!(
        body.1 > title.1,
        "the note's body follows its title: {body:?} under {title:?}"
    );
    assert!(
        body.1 + body.3 < f64::from(HEIGHT),
        "the note's body stands on the display: {body:?}"
    );
}

/// Where the control or text `name` of the shell stands, once the app showing it has laid it out:
/// where it stands on the display, and how tall it is.
fn until_bounds(session: &mut Session, name: &str) -> (f64, f64, f64, f64) {
    let mut found = None;
    until(session, name, |session| {
        found = session
            .shell_control(name)
            .or_else(|| session.shell_text_bounds(name));
        found.is_some()
    });
    found.expect("the app's control stands in the shell's tree")
}

/// Where the switch between the presets stands: the bar's own menu, which names the preset the desk
/// shows (`showing`).
fn preset_switch(session: &Session, showing: &str) -> (f64, f64, f64, f64) {
    session
        .shell_control(&format!("Showing {showing}"))
        .unwrap_or_else(|| {
            panic!(
                "the bar shows the preset {showing}: {:?}",
                session.accessible_labels()
            )
        })
}

/// The chrome's published resource.
const CHROME: &str = "desktop:chrome";
/// The desk's published resource, which its actions are called on.
const DESK_RESOURCE: &str = "desktop:desk";
/// The desk's reading: the surface the shell shows now.
const DESK: &str = "session:desk";

/// The surface up now, as the desk's reading names it.
fn here(session: &Session) -> String {
    let reading: Value =
        serde_json::from_value(session.read(DESK).expect("the session publishes its desk"))
            .expect("the desk reads as its JSON");
    reading
        .get("here")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The overview's heading: the overview's own, which no other view of the desk shows.
const OVERVIEW: &str = "Overview";

/// The name the shell shows a window placed under `title` by: the session resolves it once — the
/// window's own title and its app's name — and every surface that names a window takes it from
/// there, so the tests read the name off the same answer they assert on.
fn shown_as(session: &Session, title: &str) -> String {
    let reading = session.arrangement().borrow().reading();
    reading
        .places
        .iter()
        .find(|place| place.title == title)
        .map_or_else(
            || panic!("the desk holds a place for {title}"),
            |place| place.name.clone(),
        )
}

/// Where the shell's control named `name` stands, when the whole of it is on a display `width` by
/// `height` (not only part of it, as [`on_screen`] asks).
fn within(session: &Session, name: &str, width: i32, height: i32) -> Option<(f64, f64, f64, f64)> {
    session
        .shell_control(name)
        .filter(|&rect| whole_on(rect, width, height))
}

/// Whether the whole of the rectangle `(x, y, w, h)` stands on a display `width` by `height`.
fn whole_on((x, y, w, h): (f64, f64, f64, f64), width: i32, height: i32) -> bool {
    w > 0.0
        && h > 0.0
        && x >= 0.0
        && y >= 0.0
        && x + w <= f64::from(width)
        && y + h <= f64::from(height)
}

/// Where the window the shell shows as `name` stands: its frame. The desk says a window's name
/// more than once — its frame is named for it, and so is the window's own menu in its bar, which
/// lists the commands of the place the window shows — so the first control of that name is not
/// the window. The frame is the one the others stand in: the largest of them.
fn window_frame(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session
        .shell_controls(name)
        .into_iter()
        .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
}

/// Where the window the shell shows as `name` stands, when the whole of it is on a display `width`
/// by `height`: [`within`] for the window's frame.
fn window_within(
    session: &Session,
    name: &str,
    width: i32,
    height: i32,
) -> Option<(f64, f64, f64, f64)> {
    window_frame(session, name).filter(|&rect| whole_on(rect, width, height))
}

/// Where the menu of the window the shell shows as `name` stands on the standard display: the
/// first of the window's own controls in its one bar, named for the window it is the menu of. Of
/// the controls by that name it is the one standing in the window's frame at an icon's size: the
/// smallest of them, where a tab of that name is as wide as its title.
fn window_menu(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    let frame = window_frame(session, name)?;
    session
        .shell_controls(name)
        .into_iter()
        .filter(|&(x, y, w, h)| {
            let (across, down) = (x + w / 2.0, y + h / 2.0);
            w * h < frame.2 * frame.3
                && across >= frame.0
                && across <= frame.0 + frame.2
                && down >= frame.1
                && down <= frame.1 + frame.3
        })
        .min_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
        .filter(|&rect| on_display(rect, WIDTH, HEIGHT))
}

/// Press the menu of the window the shell shows as `name` with the pointer, as the person does.
fn press_window_menu(session: &mut Session, name: &str) {
    press_where(
        session,
        &format!("the menu of the window {name}"),
        |session| window_menu(session, name),
    );
}

/// Whether two rectangles `(x, y, w, h)` share any area.
fn intersects(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

/// The session with the shell on a device whose every id sits above `i64::MAX` : the salt is the
/// top of an id's high bits, so no place, strip or row fits an integer.
fn salted_session() -> (Session, Vec<String>) {
    let (notebook, notes) = notebook();
    let session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        salt: 0x8000,
        services: vec![Box::new(notebook)],
        ..Options::default()
    });
    (session, notes)
}

/// Place `resource` on the surface in front, as activating a reference in a view does: the desk's
/// own workspace, row, strip and place ids, minted on this device. The place's id.
fn place_here(session: &mut Session, resource: &str) -> u64 {
    let before: Vec<String> = crate::common::places(session)
        .iter()
        .map(|place| place.key.clone())
        .collect();
    let arguments: Map<String, Value> = json!({
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
    until(session, "the place is in the record", |session| {
        crate::common::places(session).len() > before.len()
    });
    crate::common::number(
        &crate::common::places(session)
            .iter()
            .map(|place| place.key.clone())
            .find(|key| !before.contains(key))
            .expect("a place the record did not hold before"),
    )
}

/// An empty strip called `name` in the row in front: what the person makes by dragging one out. Its
/// id, which is its row's.
fn add_strip(session: &Session, name: &str) -> u64 {
    let handle = session.handle();
    let mut tree = handle.read(Clone::clone);
    let made = tilo_desktop_model::commands::apply_to_active_workspace(
        &mut tree,
        &tilo_desktop_model::commands::DesktopCommand::CreateStrip { name: name.into() },
        &mut || Ok(handle.mint_id()),
    )
    .expect("a strip is made")
    .expect("the strip has an id");
    handle.replace_tree(tree);
    made.0
}

/// The strip the place `place` stands in, by the record's ids.
fn place_strip(session: &Session, place: u64) -> Option<u64> {
    crate::common::strip_of(session, place).map(|strip| crate::common::number(&strip.key))
}

/// The row the place `place` stands in, by the record's ids.
fn place_row(session: &Session, place: u64) -> Option<u64> {
    crate::common::strip_of(session, place).map(|strip| crate::common::number(&strip.row))
}

/// The width of the strip `strip` , in per-mille of its row.
fn strip_width(session: &Session, strip: u64) -> Option<u32> {
    crate::common::strip(session, strip)
        .map(|strip| u32::try_from(strip.width).expect("a width of per-mille fits its count"))
}

/// Keep the screen as `name.png` where `TL_SHELL_SHOTS` names.
fn keep(session: &mut Session, name: &str) {
    let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the screen");
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the shots' directory");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("the shot is written");
}

/// The strip and place controls press the place they name, on a device whose every id is above
/// `i64::MAX` : a key read as a number reaches nothing of this desk.
#[test]
fn the_arranging_controls_press_the_place_they_name_on_a_salted_device() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = salted_session();
    let note = notes.first().expect("a note").clone();
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let placed = place_here(&mut session, &note);
    assert!(
        placed > u64::try_from(i64::MAX).unwrap(),
        "the desk's ids do not fit an integer: {placed}"
    );
    // The window's own controls stand in its one bar: its menu, named for the window it is the
    // menu of, its maximize and its close.
    let shown = shown_as(&session, NOTE_TITLE);
    until(&mut session, "the window's controls", |session| {
        window_menu(session, &shown).is_some() && on_screen(session, "Close").is_some()
    });
    let strip = place_strip(&session, placed).expect("the place stands in a strip");
    assert_ne!(strip_width(&session, strip), Some(1000), "it starts narrow");

    // The window's width menu resizes the window the menu is on.
    press_window_menu(&mut session, &shown);
    until(&mut session, "the width menu", |session| {
        on_screen(session, "Wide").is_some()
    });
    keep(&mut session, "arrangement-window-menu");
    press(&mut session, "Wide");
    until(&mut session, "the window widened", |session| {
        strip_width(session, strip) == Some(1000)
    });

    // The window's pin presses the place the window is on.
    press_window_menu(&mut session, &shown);
    until(&mut session, "the window's menu", |session| {
        on_screen(session, "Pin window").is_some()
    });
    press(&mut session, "Pin window");
    until(&mut session, "the place pinned", |session| {
        crate::common::place(session, placed).is_some_and(|place| place.pinned)
    });

    // A second row on the desk: the window's move names it, by the row's own name.
    let second = add_strip(&session, "Elsewhere");
    until(&mut session, "the second row", |session| {
        crate::common::rows(session)
            .iter()
            .any(|row| crate::common::number(&row.key) == second)
    });
    press_window_menu(&mut session, &shown);
    until(&mut session, "the rows to move to", |session| {
        on_screen(session, "Move to Elsewhere").is_some()
    });
    keep(&mut session, "arrangement-move-to-row-menu");
    press(&mut session, "Move to Elsewhere");
    until(&mut session, "the place moved", |session| {
        place_row(session, placed) == Some(second)
    });

    // Closing presses the place its window holds, never another.
    until(&mut session, "the window's controls", |session| {
        on_screen(session, "Close").is_some()
    });
    press(&mut session, "Close");
    until(&mut session, "the place closed", |session| {
        place_strip(session, placed).is_none()
    });
}

/// Two places in one strip: the strips preset shows a strip of several places as its tab strip
/// (shell.md §2), and every way to make one — a place moved into another strip from the window's
/// menu, a found resource opened into it — is an arranging action the person, the REPL and an agent
/// call alike, attributed and undone as one step. Choosing a tab shows its place, and closing a tab
/// takes that place off the strip and nothing else.
#[test]
fn two_places_in_one_strip_are_its_tabs_on_the_shells_screen() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session_at(WIDTH, HEIGHT);
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let first = notes.first().expect("a note").clone();
    let second = notes.get(1).expect("a second note").clone();

    // Each note starts a window of its own: one place, one strip.
    let one = place(&mut session, &first);
    let two = place(&mut session, &second);
    let strip_of_one = place_strip(&session, one).expect("the first place stands in a strip");
    let strip_of_two = place_strip(&session, two).expect("the second place stands in a strip");
    assert_ne!(
        strip_of_one, strip_of_two,
        "each place was made in a strip of its own"
    );

    // The person moves the first place into the second one's strip from the first window's menu:
    // the arranging action's `strip` , which is what a drag onto another strip means. The window in
    // front is the one the person is working in, so its menu is the one that stands.
    let one_name = shown_as(&session, TITLES[0]);
    let two_name = shown_as(&session, TITLES[1]);
    until(&mut session, "the window's controls", |session| {
        window_menu(session, &one_name).is_some()
    });
    let menu = format!("Move into {two_name} as a tab");
    press_window_menu(&mut session, &one_name);
    until(&mut session, "the strips to move into", |session| {
        on_screen(session, &menu).is_some()
    });
    keep(&mut session, "tabs-move-into-strip-menu");
    press(&mut session, &menu);
    until(&mut session, "both places in one strip", |session| {
        place_strip(session, one) == Some(strip_of_two)
            && place_strip(session, two) == Some(strip_of_two)
    });
    let strip = crate::common::strip(&session, strip_of_two).expect("the strip reads");
    let placed: Vec<u64> = crate::common::places(&session)
        .iter()
        .filter(|place| place.strip == strip.key)
        .map(|place| crate::common::number(&place.key))
        .collect();
    assert_eq!(placed.len(), 2, "the strip holds both places: {placed:?}");

    // The strip shows its tabs: both titles stand in its one bar, and the place moved in is the one
    // in front.
    until(&mut session, "both tab titles", |session| {
        on_screen(session, &one_name).is_some() && on_screen(session, &two_name).is_some()
    });
    keep(&mut session, "tabs-two-in-one-strip");
    assert_eq!(
        strip.active,
        one.to_string(),
        "the place moved in stands in front"
    );

    // Choosing a tab shows its place: the second tab, and the strip's reading follows.
    press(&mut session, &two_name);
    until(&mut session, "the second tab in front", |session| {
        crate::common::strip(session, strip_of_two)
            .is_some_and(|strip| strip.active == two.to_string())
    });

    // And the first one again.
    press(&mut session, &one_name);
    until(&mut session, "the first tab in front", |session| {
        crate::common::strip(session, strip_of_two)
            .is_some_and(|strip| strip.active == one.to_string())
    });

    // Closing a tab takes that place off the strip, and the other stays.
    press(&mut session, "Close");
    until(&mut session, "the first place gone", |session| {
        place_strip(session, one).is_none()
    });
    assert_eq!(
        place_strip(&session, two),
        Some(strip_of_two),
        "the tab that stayed is the second place, in the strip it was in"
    );
    until(&mut session, "one title left in the strip", |session| {
        on_screen(session, &two_name).is_some() && on_screen(session, &one_name).is_none()
    });
}

/// A resource a text finds opens into the strip in front as its tab, through the arranging action
/// the launcher's "Open as a tab" runs (`place-found` with the strip named): one step of the
/// person's, the same an agent or the REPL makes.
#[test]
fn a_found_resource_opens_as_a_tab_of_the_strip_in_front() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session_at(WIDTH, HEIGHT);
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let note = notes.first().expect("a note").clone();
    let placed = place(&mut session, &note);
    let strip = place_strip(&session, placed).expect("the place stands in a strip");
    let before = crate::common::places(&session).len();

    // The launcher's "Open as a tab" runs the arranging action with the strip in front named.
    session
        .invoke(
            "shell:arrangement",
            "place-found",
            json!({
                "text": TITLES[1],
                "strip": strip.to_string(),
                "x": 80,
                "y": 60,
            })
            .as_object()
            .unwrap(),
        )
        .unwrap_or_else(|error| panic!("the found note opens as a tab: {error:?}"));
    until(&mut session, "the found note in the strip", |session| {
        crate::common::places(session).len() > before
            && crate::common::places(session)
                .iter()
                .any(|place| place.strip == strip.to_string() && place.key != placed.to_string())
    });
    let found = crate::common::places(&session)
        .into_iter()
        .find(|place| place.strip == strip.to_string() && place.key != placed.to_string())
        .expect("the found note joined the strip");
    assert_eq!(
        found.key,
        crate::common::strip(&session, strip)
            .expect("the strip reads")
            .active,
        "the found note stands in front, as a tab arrives"
    );
    let title = shown_as(&session, TITLES[1]);
    until(&mut session, "both titles in the strip", |session| {
        on_screen(session, &shown_as(session, TITLES[0])).is_some()
            && on_screen(session, &title).is_some()
    });
    keep(&mut session, "tabs-found-opens-as-a-tab");
}

/// A place arriving on the desk is drawn over the frames between rather than switching in one
/// frame: the shell's windows are components that come in themselves (`motion::Placed` over
/// `WindowMotion::OPEN` ), so what the person sees is the window arriving. The frame-by-frame proof
/// that it reaches its place and then asks for no frames is the component's own
/// (`widget::window_frame`), where "no frames requested" is exact; here the shell screen shows the
/// window changing from one frame to the next as it comes in.
#[test]
fn a_place_arrives_over_the_frames_between() {
    let (mut session, notes) = session_at(WIDTH, HEIGHT);
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    // A note arrives on the desk.
    let note = notes.first().expect("a note").clone();
    place(&mut session, &note);
    let shown = shown_as(&session, NOTE_TITLE);
    let arriving = crate::common::wait_for(&mut session, "the note on the desk", |session| {
        on_screen(session, &shown)
            .is_some()
            .then(|| crate::common::shot(session, "motion-arriving"))
    });
    // The frames right after it appears change: the window is on its way in.
    let mut changes = 0;
    let mut before = arriving;
    for _ in 0..3 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("a frame");
        let after = crate::common::shot(&mut session, "motion-arriving-next");
        changes += crate::common::changed(&before, &after);
        before = after;
    }
    assert!(
        changes > 0,
        "the window comes in over the frames between, so the screen changes as it arrives"
    );
    keep(&mut session, "motion-place-arrived");
}

/// An app the desk opens stands on the framed canvas: the canvas draws a window group by the place
/// the arrangement holds for it, so a group the desk launched with no place of its own is a window
/// the person opened and never sees. The desk gives every group it holds a place, and the opened
/// app's window is on screen.
#[test]
fn an_app_the_desk_opens_stands_on_the_framed_canvas() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _note) = session_at(WIDTH, HEIGHT);
    until(&mut session, "the bar's controls", |session| {
        on_screen(session, "Notifications").is_some()
    });
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas's entry", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });

    // Nothing stands on the canvas yet: a fresh desk holds no window.
    assert!(
        crate::common::places(&session).is_empty(),
        "a fresh desk has nothing placed: {:?}",
        crate::common::places(&session)
    );

    // The person opens an app, as the dock or the launcher does.
    session
        .invoke(
            CHROME,
            "open-app",
            json!({"app": "so.tilo.lang.notes"})
                .as_object()
                .expect("an object"),
        )
        .expect("the app opens on the desk");

    // Every window the desk holds stands on the canvas, and the app's window is on screen.
    until(&mut session, "the opened window on the canvas", |session| {
        !crate::common::places(session).is_empty()
    });
    let drawn = session.handle().read(|tree| {
        tree.active_workspace().is_some_and(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .all(|group| workspace.arrangement.place_of(group.id).is_some())
        })
    });
    assert!(
        drawn,
        "every window the desk holds stands on the canvas: {:?}",
        crate::common::places(&session)
    );
    // And the shell lays the window's frame on the display, not off it: the place's name is what a
    // window's frame is called, and the canvas draws it where its point is.
    let name = crate::common::places(&session)
        .first()
        .map(|place| place.name.clone())
        .expect("the opened window's place");
    until(&mut session, &format!("{name} on the canvas"), |session| {
        on_screen(session, &name).is_some()
    });
    keep(&mut session, "frames-opened-app");
}

/// The desk draws the workspace in front and no other. A desk holds every workspace the person is
/// in — the one it starts from, and one joined through a door (the dev shell joins the community's
/// Lounge), or one made here — and it shows one. Before this, the strips drew each workspace's rows
/// one under the other, so a desk of two cut every window's room in half: the joined workspace's
/// window stood in the lower half, and the empty workspace above it took the upper half for an
/// empty state. The workspace in front is the one the bar's workspace menu says is current, and its
/// window has the room between the bar and the dock.
#[test]
fn the_desk_draws_the_workspace_in_front_and_no_other() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session_at(WIDTH, HEIGHT);
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });

    // The window the person works in, on the workspace they started on.
    let note = notes.first().expect("a note").clone();
    place(&mut session, &note);
    let shown = shown_as(&session, NOTE_TITLE);
    until(&mut session, "the placed note's window", |session| {
        on_screen(session, &shown).is_some()
    });

    // A second workspace, as joining one through a door leaves it: the desk holds two, and the one
    // made stands in front, holding nothing.
    session
        .invoke(CHROME, "new-workspace", &Map::new())
        .unwrap_or_else(|error| panic!("a workspace is made: {error:?}"));
    until(&mut session, "the workspace made", |session| {
        session.handle().read(|tree| tree.workspaces.len()) == 2
    });
    let made = active_workspace(&session);
    until(&mut session, "the empty desk", |session| {
        on_screen(session, NO_WINDOW).is_some()
    });
    // The empty workspace has the desk's whole room: its empty state is the desk's height, not the
    // half of it a second workspace's rows would leave it. The workspace behind holds the note, and
    // none of it stands on this desk.
    let empty = within(&session, NO_WINDOW, WIDTH, HEIGHT).expect("the empty state is on screen");
    assert!(
        empty.3 > f64::from(HEIGHT) / 2.0,
        "the empty workspace in front has the desk's room, not half of it: {empty:?}"
    );
    assert!(
        on_screen(&session, &shown).is_none(),
        "the workspace behind stands nowhere on this desk: {shown:?} at {:?}",
        on_screen(&session, &shown)
    );

    // Back to the workspace that holds the note: its window has the desk's room again, and no
    // empty state of the other workspace stands under it.
    session
        .invoke(
            CHROME,
            "switch-workspace",
            json!({"delta": -1}).as_object().unwrap(),
        )
        .unwrap_or_else(|error| panic!("the workspaces step back one: {error:?}"));
    until(&mut session, "the first workspace again", |session| {
        active_workspace(session) != made
    });
    until(&mut session, "the note's window again", |session| {
        on_screen(session, &shown).is_some()
    });
    let window =
        window_within(&session, &shown, WIDTH, HEIGHT).expect("the window is on the display");
    assert!(
        window.3 > f64::from(HEIGHT) / 2.0,
        "the window in front has the desk's room, not half of it: {window:?}"
    );
    assert!(
        on_screen(&session, NO_WINDOW).is_none(),
        "the other workspace's empty state stands nowhere on this desk: {:?}",
        on_screen(&session, NO_WINDOW)
    );
    keep(&mut session, "workspace-in-front");
}

/// The desk carries no strip label of its own: the strip a person is in is what the bar's strip
/// pill names, and a label over each window's band was bare text a person could not act on, flush
/// against the desk's edge. The desk of the development shell holds two workspaces (the one it
/// starts from and the joined Lounge), each with a `Main` row, which is the desk that showed one
/// bare `Main` over each band. Every `Main` the desk says of a strip is the bar's pill now; nothing
/// says it over the windows.
#[test]
fn the_desk_carries_no_strip_label_of_its_own() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session_at(WIDTH, HEIGHT);
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });

    // A window on the desk, and a second workspace: the desk the development shell's shot showed —
    // two workspaces, each with a `Main` row, and a window on one of them.
    place(&mut session, &notes[0]);
    let shown = shown_as(&session, NOTE_TITLE);
    until(&mut session, "the placed note's window", |session| {
        on_screen(session, &shown).is_some()
    });
    session
        .invoke(CHROME, "new-workspace", &Map::new())
        .unwrap_or_else(|error| panic!("a workspace is made: {error:?}"));
    until(&mut session, "the workspace made", |session| {
        session.handle().read(|tree| tree.workspaces.len()) == 2
    });
    until(&mut session, "the empty desk in front", |session| {
        on_screen(session, NO_WINDOW).is_some()
    });

    // Every saying of the strip's name, once the desk has drawn: the bar's pill, and nothing over a
    // window's band.
    let mains: Vec<(f64, f64, f64, f64)> = session.shell_text_bounds_all("Main");
    assert!(
        !mains.is_empty(),
        "the bar names the strip: {:?}",
        session.accessible_labels()
    );
    for rect in &mains {
        assert!(
            rect.1 < 40.0,
            "every name the desk says stands in the bar, none over a window's band: {rect:?} \
             (all: {mains:?})"
        );
    }
}
