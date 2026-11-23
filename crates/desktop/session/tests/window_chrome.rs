//! The window chrome the tilolang shell draws, driven as a person drives it: the window menu's
//! every row, the strip menu under the bar's strip name, the start cards an empty shell offers, and
//! the passing notice with the Undo it carries. Each row presses one published action of the
//! session's (`so.tilo.session`), so a press, a script and an agent make the same change, and what
//! stands on screen is read from what the shell lays out for assistive technology.

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
use tilo_compositor::{KeyRepeat, OutputConfig};
use tilo_session::{Options, Session};

use crate::common::{
    HEIGHT, PERSON, TITLES, WIDTH, headless_on, notebook as notes, on_screen, press, until,
};

/// The Escape key, evdev: what closes the launcher.
const KEY_ESCAPE: u32 = 1;
/// The Enter key, evdev: what commits a typed name.
const KEY_ENTER: u32 = 28;

/// The session with the tilolang shell as its whole screen, on `outputs`.
fn session_on(outputs: &[OutputConfig]) -> (Session, Vec<String>) {
    let (notebook, notes) = notes(&TITLES);
    let mut session = headless_on(
        outputs,
        Options {
            person: PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(notebook)],
            ..Options::default()
        },
    );
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    (session, notes)
}

/// The session on one display.
fn session() -> (Session, Vec<String>) {
    session_on(&[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
}

/// Place `resource` on the workspace in front, in a strip of its own: the place's id.
fn place(session: &mut Session, resource: &str) -> u64 {
    let before: Vec<String> = crate::common::places(session)
        .into_iter()
        .map(|place| place.key)
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
        .invoke("shell:layout", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    let made = crate::common::wait_for(session, "the place is in the record", |session| {
        crate::common::places(session)
            .into_iter()
            .find(|place| !before.contains(&place.key))
    });
    crate::common::number(&made.key)
}

/// The share of its row the strip `strip` keeps, in per-mille.
fn strip_width(session: &Session, strip: u64) -> Option<i64> {
    crate::common::strip(session, strip).map(|strip| strip.width)
}

/// Whether the strip `strip` fills the shell alone.
fn strip_maximized(session: &Session, strip: u64) -> Option<bool> {
    crate::common::strip(session, strip).map(|strip| strip.maximized)
}

/// Every window the session publishes, and every display it spreads over.
fn windows(session: &Session) -> Value {
    session
        .read("session:windows")
        .expect("the session publishes its windows")
}

/// The window `key` as the session publishes it.
fn window(session: &Session, key: &str) -> Value {
    windows(session)["all"]
        .as_array()
        .expect("windows")
        .iter()
        .find(|window| window["key"] == json!(key))
        .cloned()
        .unwrap_or_else(|| panic!("the window {key} is published"))
}

/// The passing notice the shell says now.
fn said(session: &Session) -> Value {
    session
        .read("session:notice")
        .expect("the session publishes its notice")
}

/// Where the frame of the window the shell shows as `name` stands: the largest control of that
/// name (the app's own menus stand in it too).
fn window_frame(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session
        .shell_controls(name)
        .into_iter()
        .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
}

/// The menu of the window the shell shows as `name`: the "This window" control inside its frame.
fn window_menu(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    let frame = window_frame(session, name)?;
    session
        .shell_controls("This window")
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
}

/// Press the window menu of the window the shell shows as `name`.
fn press_window_menu(session: &mut Session, name: &str) {
    crate::common::press_where(session, &format!("the menu of {name}"), |session| {
        window_menu(session, name)
    });
}

/// The name the shell shows the place `key` by, as its window's bar reads it.
fn shown_as(session: &Session, key: &str) -> String {
    crate::common::layout(session)
        .places
        .into_iter()
        .find(|place| place.key == key)
        .map_or_else(
            || panic!("the shell holds a place {key}"),
            |place| place.name,
        )
}

/// Wait for the shell to lay the place `key` out, then open its window menu, waiting for the shell
/// to show it. A window behind another tab of its strip comes forward first, by its tab, so its bar
/// stands.
fn read_land(session: &mut Session, key: &str) {
    until(session, "the window laid out", |session| {
        crate::common::place(session, crate::common::number(key)).is_some()
    });
}

/// Open the window menu of the placed window `key`, waiting for the shell to show it. A window
/// behind another tab of its strip comes forward first, by its tab, so its bar stands.
fn open_menu(session: &mut Session, key: &str) -> String {
    let shown = shown_as(session, key);
    if window_frame(session, &shown).is_none() {
        until(session, "the window's tab", |session| {
            on_screen(session, &shown).is_some()
        });
        press(session, &shown);
        until(session, "the window in front of its strip", |session| {
            window_frame(session, &shown).is_some()
        });
    }
    until(session, "the window's menu", |session| {
        window_menu(session, &shown).is_some()
    });
    press_window_menu(session, &shown);
    shown
}

#[test]
fn the_window_menu_offers_every_row_a_window_can_choose() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let note = notes.first().expect("a note").clone();
    let placed = place(&mut session, &note);
    open_menu(&mut session, &placed.to_string());

    for row in [
        "Reveal",
        "Full screen",
        "Fill the strip",
        "Float this window",
        "Half the strip",
        "Narrow",
        "Normal",
        "Wide",
        "Every window the same",
        "Wider",
        "Narrower",
        "New tab of this app",
        "Pin window",
        "Close this window",
    ] {
        until(&mut session, row, |session| {
            on_screen(session, row).is_some()
        });
    }
    assert!(
        on_screen(&session, "Rename").is_none(),
        "an app's window is named by its app, so no row offers to name it: {:?}",
        session.accessible_labels()
    );
    assert!(
        on_screen(&session, "Split right").is_none(),
        "a window of one tab offers no split"
    );
}

#[test]
fn the_menus_rows_change_the_window_through_published_actions() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let note = notes.first().expect("a note").clone();
    let placed = place(&mut session, &note);
    let strip = crate::common::strip_of(&session, placed)
        .map(|strip| crate::common::number(&strip.key))
        .expect("the place stands in a strip");
    let key = placed.to_string();

    // Its share of the row, one step at a time.
    open_menu(&mut session, &key);
    press(&mut session, "Wide");
    until(&mut session, "the window widened", |session| {
        strip_width(session, strip) == Some(1000)
    });
    open_menu(&mut session, &key);
    press(&mut session, "Narrower");
    until(&mut session, "the window narrowed a step", |session| {
        strip_width(session, strip) == Some(667)
    });
    open_menu(&mut session, &key);
    press(&mut session, "Narrow");
    until(&mut session, "the window narrow", |session| {
        strip_width(session, strip) == Some(333)
    });
    open_menu(&mut session, &key);
    press(&mut session, "Half the strip");
    until(&mut session, "half the row", |session| {
        strip_width(session, strip) == Some(500)
    });

    // Filling the shell with the window, and giving it back.
    open_menu(&mut session, &key);
    press(&mut session, "Fill the strip");
    until(&mut session, "the window fills the shell", |session| {
        strip_maximized(session, strip) == Some(true)
    });
    open_menu(&mut session, &key);
    press(&mut session, "Restore");
    until(&mut session, "the window back beside its row", |session| {
        strip_maximized(session, strip) == Some(false)
    });

    // Covering the display, and giving it back.
    open_menu(&mut session, &key);
    press(&mut session, "Full screen");
    until(&mut session, "the window covers its display", |session| {
        window(session, &key)["fullscreen"] == json!(true)
    });
    open_menu(&mut session, &key);
    press(&mut session, "Exit full screen");
    until(&mut session, "the display back", |session| {
        window(session, &key)["fullscreen"] == json!(false)
    });

    // Over the shell, and back into its strip.
    open_menu(&mut session, &key);
    press(&mut session, "Float this window");
    until(&mut session, "the window floats", |session| {
        window(session, &key)["floating"] == json!(true)
    });
    open_menu(&mut session, &key);
    press(&mut session, "Snap back");
    until(&mut session, "the window back in its strip", |session| {
        window(session, &key)["floating"] == json!(false)
    });
}

#[test]
fn a_windows_menu_carries_a_window_into_another_window_and_another_display() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session_on(&[
        OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT),
        OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT).at(WIDTH, 0),
    ]);
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let one = place(&mut session, &notes[0]);
    let two = place(&mut session, &notes[1]);
    let other = shown_as(&session, &two.to_string());
    let key = one.to_string();

    // Into the other window, as a tab of it.
    open_menu(&mut session, &key);
    let row = format!("Move into {other} as a tab");
    until(&mut session, "the windows to move into", |session| {
        on_screen(session, &row).is_some()
    });
    press(&mut session, &row);
    until(&mut session, "both windows in one strip", |session| {
        let first = crate::common::place(session, one).map(|place| place.strip);
        let second = crate::common::place(session, two).map(|place| place.strip);
        first == second && first.is_some()
    });

    // Onto the other display, by its number as the session counts them: the window the strip
    // shows in front is the one moved into it, and its bar is the strip's one bar.
    let second_display = windows(&session)["displays"]
        .as_array()
        .expect("displays")
        .iter()
        .find(|display| display["number"] == json!(2))
        .cloned()
        .expect("a second display");
    until(&mut session, "the strip's one bar", |session| {
        on_screen(session, "This window").is_some()
    });
    press(&mut session, "This window");
    until(&mut session, "the displays to move to", |session| {
        on_screen(session, "Display 2").is_some()
    });
    press(&mut session, "Display 2");
    until(
        &mut session,
        "the window on the second display",
        |session| window(session, &key)["display_id"] == second_display["id"],
    );
}

#[test]
fn the_strip_menu_lists_the_workspaces_bands_and_makes_one() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let _one = place(&mut session, &notes[0]);
    let _two = place(&mut session, &notes[1]);
    let band = crate::common::layout(&session)
        .rows
        .into_iter()
        .find(|row| row.current)
        .expect("a band in front");
    let band_keys = |session: &Session| -> Vec<String> {
        crate::common::layout(session)
            .rows
            .into_iter()
            .map(|row| row.key)
            .collect()
    };

    // The bar's strip name opens the workspace's bands, and the menu makes another.
    until(&mut session, "the strip menu", |session| {
        on_screen(session, &band.name).is_some()
    });
    press(&mut session, &band.name);
    until(&mut session, "the strip menu's rows", |session| {
        on_screen(session, "New strip").is_some()
    });
    let rows_before = band_keys(&session);
    press(&mut session, "New strip");
    let rows_after = crate::common::wait_for(&mut session, "a band made", |session| {
        let rows = band_keys(session);
        (rows.len() > rows_before.len()).then_some(rows)
    });
    let made = rows_after
        .iter()
        .find(|key| !rows_before.contains(key))
        .cloned()
        .expect("the new band");

    // A band moves among the workspace's own: the one in front steps left of the band before it.
    let in_front = crate::common::layout(&session)
        .rows
        .into_iter()
        .find(|row| row.current)
        .expect("a band in front");
    until(&mut session, "the new band's name on the bar", |session| {
        on_screen(session, &in_front.name).is_some()
    });
    press(&mut session, &in_front.name);
    until(&mut session, "the bands to move among", |session| {
        on_screen(session, "Move left").is_some()
    });
    let before = band_keys(&session);
    press(&mut session, "Move left");
    crate::common::wait_for(&mut session, "the band moved", |session| {
        let after = band_keys(session);
        (after != before).then_some(())
    });
    assert!(
        band_keys(&session).contains(&made),
        "the band made stands in the workspace: {:?}",
        band_keys(&session)
    );
}

#[test]
fn the_empty_shell_shows_its_start_cards() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _notes) = session();
    until(&mut session, "the start cards", |session| {
        on_screen(session, "Start something").is_some()
    });
    for card in ["Notes", "Tasks", "Invite people", "Catch up", "Open an app"] {
        until(&mut session, card, |session| {
            on_screen(session, card).is_some()
        });
    }
    assert!(
        windows(&session)["all"]
            .as_array()
            .expect("windows")
            .is_empty(),
        "the shell holds no window yet"
    );
    crate::common::shot(&mut session, "layout-start-cards");
    // The button under the cards opens the launcher, as the same press does anywhere else.
    press(&mut session, "Open an app");
    until(&mut session, "the launcher", |session| {
        crate::common::on_screen(session, "Search apps, windows, actions and files").is_some()
    });
}

#[test]
fn the_passing_notice_says_what_closed_and_undoes_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let note = notes.first().expect("a note").clone();
    let placed = place(&mut session, &note);
    let key = placed.to_string();

    // Closing the window from its own menu says so, with the Undo that reopens it.
    open_menu(&mut session, &key);
    press(&mut session, "Close this window");
    until(&mut session, "the window closed", |session| {
        crate::common::place(session, placed).is_none()
    });
    let said_text = crate::common::wait_for(&mut session, "the passing notice", |session| {
        let text = said(session)["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (!text.is_empty()).then_some(text)
    });
    assert!(!said_text.is_empty(), "the notice says what closed");
    until(&mut session, "the notice on screen", |session| {
        on_screen(session, "Undo").is_some() && on_screen(session, "Dismiss").is_some()
    });
    crate::common::shot(&mut session, "layout-passing-notice");
    press(&mut session, "Undo");
    until(&mut session, "the window back", |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.resource == note)
    });

    // Dismiss takes the notice away without doing anything more.
    session.handle().acknowledge("Link copied");
    until(&mut session, "the second notice", |session| {
        said(session)["text"] == json!("Link copied")
    });
    until(&mut session, "the notice's dismiss button", |session| {
        on_screen(session, "Dismiss").is_some()
    });
    press(&mut session, "Dismiss");
    until(&mut session, "the notice gone", |session| {
        said(session)["text"] == json!("")
    });
}

#[test]
fn the_window_bar_carries_its_tab_a_new_tab_its_search_and_history() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let note = notes.first().expect("a note").clone();
    let placed = place(&mut session, &note);
    let shown = shown_as(&session, &placed.to_string());
    let strip = crate::common::strip_of(&session, placed)
        .map(|strip| strip.key)
        .expect("the place stands in a strip");

    // The window's bar names it with the app's own header, and its controls offer the ways to open
    // another tab, search the window, and go back and forward through what it showed.
    until(&mut session, "the window's bar", |session| {
        on_screen(session, &shown).is_some()
            && on_screen(session, "New tab of this window").is_some()
            && on_screen(session, "Search this window").is_some()
            && on_screen(session, "History of this window").is_some()
            && on_screen(session, "Back in this window").is_some()
            && on_screen(session, "Forward in this window").is_some()
    });

    crate::common::shot(&mut session, "layout-window-bar");
    // The window's search opens the shell's search over the windows.
    press(&mut session, "Search this window");
    until(&mut session, "the search over the windows", |session| {
        crate::common::on_screen(session, "Search apps, windows, actions and files").is_some()
    });
    crate::common::window::press_key(&mut session, KEY_ESCAPE, true);
    crate::common::window::press_key(&mut session, KEY_ESCAPE, false);
    until(&mut session, "the search closed", |session| {
        crate::common::on_screen(session, "Search apps, windows, actions and files").is_none()
    });

    // Another tab of the same app joins the window, as the bar's `+` says, and the window's bar
    // shows both places' tabs.
    let before = crate::common::places(&session)
        .into_iter()
        .filter(|place| place.strip == strip)
        .count();
    until(&mut session, "the bar's new tab", |session| {
        on_screen(session, "New tab of this window").is_some()
    });
    press(&mut session, "New tab of this window");
    until(&mut session, "a second tab", |session| {
        crate::common::places(session)
            .into_iter()
            .filter(|place| place.strip == strip)
            .count()
            > before
    });
}

#[test]
fn the_dock_leaves_the_windows_their_room() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let note = notes.first().expect("a note").clone();
    let placed = place(&mut session, &note);
    let shown = shown_as(&session, &placed.to_string());
    until(&mut session, "the window and the dock", |session| {
        window_frame(session, &shown).is_some() && on_screen(session, "Dock").is_some()
    });
    let frame = window_frame(&session, &shown).expect("the window's frame");
    let dock = on_screen(&session, "Dock").expect("the dock");
    crate::common::shot(&mut session, "layout-dock-room");
    assert!(
        frame.1 + frame.3 <= dock.1 + 1.0,
        "the window stops at the dock's top edge, not under it: window {frame:?}, dock {dock:?}"
    );
}

#[test]
fn the_tilo_menu_holds_the_layouts_history_and_the_devices_own_pages() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    press(&mut session, "Tilo");
    for row in [
        "Undo layout change",
        "Redo layout change",
        "Settings",
        "About this computer",
    ] {
        until(&mut session, row, |session| {
            on_screen(session, row).is_some()
        });
    }
    // The menu stands until it is put away: a second press on its own control closes it.
    press(&mut session, "Tilo");
    until(&mut session, "the menu closed", |session| {
        on_screen(session, "Undo layout change").is_none()
    });
}

/// The band (the row across the shell) the place `key` stands in.
fn band_of(session: &Session, key: &str) -> Option<String> {
    let layout = crate::common::layout(session);
    let strip = layout
        .places
        .into_iter()
        .find(|place| place.key == key)?
        .strip;
    layout
        .strips
        .into_iter()
        .find(|strip| strip.key == strip)
        .map(|strip| strip.row)
}

#[test]
fn a_window_moves_to_a_strip_of_its_own_through_the_window_menu() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let one = place(&mut session, &notes[0]);
    let _two = place(&mut session, &notes[1]);
    let key = one.to_string();
    let before = crate::common::layout(&session).rows.len();
    let was = band_of(&session, &key).expect("the window stands in a band");

    // A window sharing its band moves into a strip of its own from its menu.
    open_menu(&mut session, &key);
    until(&mut session, "the strip of its own", |session| {
        on_screen(session, "Move to a new strip").is_some()
    });
    press(&mut session, "Move to a new strip");
    until(&mut session, "a strip made for it", |session| {
        crate::common::layout(session).rows.len() == before + 1
    });
    assert_ne!(
        band_of(&session, &key).as_deref(),
        Some(was.as_str()),
        "the window left the band it shared"
    );
}

#[test]
fn deleting_a_strip_carries_its_windows_into_the_next_one() {
    let _settled = ui::motion::testing::settled();
    let (mut session, notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let one = place(&mut session, &notes[0]);
    let two = place(&mut session, &notes[1]);
    let workspace = crate::common::layout(&session)
        .workspaces
        .into_iter()
        .find(|workspace| workspace.current)
        .expect("a workspace in front")
        .key;
    let first = crate::common::layout(&session)
        .rows
        .first()
        .expect("a band")
        .key
        .clone();
    session
        .invoke("desktop:chrome", "new-strip", &Map::new())
        .expect("a second band is made");
    let made = crate::common::wait_for(&mut session, "two bands", |session| {
        let rows = crate::common::layout(session).rows;
        (rows.len() == 2).then(|| {
            rows.iter()
                .find(|row| row.key != first)
                .map(|row| row.key.clone())
        })?
    })
    .expect("the band made");

    // Deleting the first band carries both its windows into the next; neither is lost.
    session
        .invoke(
            "session:windows",
            "delete-strip",
            json!({"strip": first}).as_object().unwrap(),
        )
        .expect("the band is deleted");
    until(&mut session, "one band left", |session| {
        crate::common::layout(session).rows.len() == 1
    });
    for place_key in [one, two] {
        let key = place_key.to_string();
        assert!(
            crate::common::place(&session, place_key).is_some(),
            "the window {key} survives the band it stood in"
        );
        assert_eq!(
            band_of(&session, &key).as_deref(),
            Some(made.as_str()),
            "the window {key} moved to the band that stayed"
        );
    }
    let _ = workspace;
}

/// A session whose one window is a host (non-Tilo) window: a foreign surface the compositor holds,
/// which the person may name and which can stop answering.
fn host_window_session() -> Session {
    let (mut session, _notes) = session();
    until(&mut session, "the strips", |session| {
        on_screen(session, "Showing Strips").is_some()
    });
    let mut tree = crate::common::tree::layout_tree();
    tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile =
        tilo_desktop_model::TileContent::Foreign {
            app_id: "org.example.Host".into(),
            surface_id: 7,
        };
    session.handle().replace_tree(tree);
    until(&mut session, "the host window's bar", |session| {
        on_screen(session, "This window").is_some()
    });
    session
}

#[test]
fn a_host_windows_name_is_changed_from_its_menu() {
    let _settled = ui::motion::testing::settled();
    let mut session = host_window_session();
    read_land(&mut session, "20");
    open_menu(&mut session, "20");
    until(&mut session, "the rename row", |session| {
        on_screen(session, "Rename").is_some()
    });
    press(&mut session, "Rename");
    until(&mut session, "the name field", |session| {
        on_screen(session, "A new name").is_some()
    });
    crate::common::type_text(&mut session, "Renamed");
    crate::common::window::press_key(&mut session, KEY_ENTER, true);
    crate::common::window::press_key(&mut session, KEY_ENTER, false);
    until(&mut session, "the window renamed", |session| {
        crate::common::place(session, 20).is_some_and(|place| place.title == "Renamed")
    });
}

#[test]
fn a_window_that_stops_answering_says_so_on_the_window_itself() {
    let _settled = ui::motion::testing::settled();
    let mut session = host_window_session();
    session.handle().set_unresponsive_windows(vec![7]);
    until(&mut session, "the hung card", |session| {
        on_screen(session, "Not responding").is_some()
            && on_screen(session, "Wait").is_some()
            && on_screen(session, "Force quit").is_some()
    });
    crate::common::shot(&mut session, "layout-hung-card");
    press(&mut session, "Wait");
    // Waiting keeps the window and takes the card away until the app answers again.
    session.handle().set_unresponsive_windows(Vec::new());
    until(&mut session, "the card gone", |session| {
        on_screen(session, "Not responding").is_none()
    });
}
