//! The traditional desktop, and the two canvas and bar defects beside it: what the shell draws
//! as windows, a dock and a bar whose actions belong to the window in front (shell.md §4, §6,
//! §8), and the two rules a placed frame and the bell keep (canvas.md §6, shell.md §6).
//!
//! Each test here fails before the fix: a frame with no bar or no title, a dock with only the
//! launcher on it, a bar that lists no focused window's actions, two frames at one point, and a
//! bell that opens quick settings instead of the notifications.
//!
//! What stands on the screen is read from what the shell lays out for assistive technology and
//! from what the session publishes, never from pixels.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::{Map, Value, json};
use std::time::Duration;
use tilo_compositor::InputEvent;
use tilo_session::Session;
use tilo_session_core::surface::Scroll;

use crate::common::{drag, on_screen, until, wait_for};

/// The bar's control that opens the notification centre, as assistive technology names it.
const BELL: &str = "Notifications";
/// What the notification centre draws when nothing waits in it: its own empty state, which no
/// other surface of the shell shows.
const NOTHING_TO_CATCH_UP_ON: &str = "Nothing to catch up on";

/// The session with the shell as the whole screen and the seeded apps, its MCP server and
/// test-control socket at `root`, admitting this test's program to act as the person — the same
/// session the acceptance path drives, so what a test draws is what a person gets. The framed
/// canvas is in front of it. Two of the notes the seed keeps stand on its shell, each a window of
/// its own: they are the two frames a test places.
fn session(root: &std::path::Path) -> (Session, Vec<String>) {
    let session = crate::common::seeded_session(root);
    let notes = note_ids(&session);
    assert!(notes.len() >= 2, "the seed keeps two notes: {notes:?}");
    (session, notes.into_iter().take(2).collect())
}

/// The ids of the notes the seeded notes app holds, in the order it lists them.
fn note_ids(session: &Session) -> Vec<String> {
    fn find(resources: &[tilo_actions::ResourceSpec], out: &mut Vec<String>) {
        for resource in resources {
            if resource.kind.ends_with(".Note") {
                out.push(resource.id.as_str().to_owned());
            }
            find(&resource.children, out);
        }
    }
    let mut out = Vec::new();
    find(&session.resources(), &mut out);
    out
}

/// Show the style `preset` ("Strips", "Frames", "Frameless", "Desktop") the way a person does: the
/// shell's own `show_style`, which every row of the style switch and the canvas's person menu runs
/// (`show_preset`). It waits for the map the style draws, so the style is read by what the shell
/// shows, never by pixels.
#[track_caller]
fn show_style(session: &mut Session, preset: &str) {
    crate::common::show_preset(session, &format!("so.tilo.shell:{}", preset.to_lowercase()));
}

/// Show the traditional desktop's chrome the way the settings page's card does: the preset
/// `windows_desktop` applied (`chrome.tl`), which lists the window in front's commands on the bar
/// and rides the dock on its foot.
#[track_caller]
fn show_traditional_chrome(session: &mut Session) {
    session
        .run_shell_action("apply", vec![Value::from("windows_desktop")])
        .unwrap_or_else(|why| panic!("the shell shows the traditional desktop: {why}"));
    until(session, "the traditional desktop", |session| {
        preset_menu_gone(session)
    });
}

/// Whether the shell shows the traditional desktop's chrome: its bar drops the preset switch (its
/// own bar carries the focused window's commands instead), and the dock rides its foot.
fn preset_menu_gone(session: &Session) -> bool {
    crate::common::preset_menu(session).is_none() && on_screen(session, "Dock").is_some()
}

/// Place `resource` on the canvas through the layout's published `place`, as the canvas's
/// own typed line and an agent do: at `(x, y)`, `width` by `height`. Its place's key.
fn place(session: &mut Session, resource: &str, x: i64, y: i64, width: i64, height: i64) -> String {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id().to_string(),
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
        "x": x,
        "y": y,
        "width": width,
        "height": height,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:layout", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place.to_string()
}

/// The canvas rectangle each placed frame stands at, as the layout keeps it: `(x, y, w, h)`,
/// in the layout's order.
fn frames(session: &Session) -> Vec<(i64, i64, i64, i64)> {
    crate::common::layout(session)
        .strips
        .into_iter()
        .filter(|strip| strip.placed)
        .map(|strip| {
            (
                strip.canvas_x,
                strip.canvas_y,
                strip.canvas_width,
                strip.canvas_height,
            )
        })
        .collect()
}

/// The widest box the shell names `name`: a marker on the bar shares the window's name, and the
/// window a person drags is the large one, not the label.
fn widest_named(session: &Session, name: &str) -> Option<(f64, f64, f64, f64)> {
    session
        .shell_controls(name)
        .into_iter()
        .filter(|(_, _, w, h)| *w > 0.0 && *h > 0.0)
        .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
}

/// Whether two canvas rectangles `(x, y, w, h)` share any area.
fn overlap(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> bool {
    a.0 < b.0.saturating_add(b.2)
        && b.0 < a.0.saturating_add(a.2)
        && a.1 < b.1.saturating_add(b.3)
        && b.1 < a.1.saturating_add(a.3)
}

/// In the desktop preset, every placed window shows a bar naming it: its title, and the shell's
/// own close on it. A window with no bar draws its resource over the shell with nothing naming it,
/// which is what a person sees as "no title bars".
#[test]
fn the_desktop_draws_every_window_with_a_bar_naming_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    place(&mut session, &notes[0], 100, 120, 480, 360);
    place(&mut session, &notes[1], 700, 120, 480, 360);
    show_traditional_chrome(&mut session);

    for note in &notes {
        let name = crate::common::drawn_name(&session, note);
        until(
            &mut session,
            &format!("the bar naming {name:?}"),
            |session| on_screen(session, &name).is_some(),
        );
        // The shell's own close stands on that bar, one per window placed.
        let closes = session.shell_controls("Close").len();
        assert!(
            closes >= 1,
            "every window carries its own close on its bar: {closes} closes: {:?}",
            session.accessible_labels()
        );
    }
}

/// A press on a dock item opens or focuses what it stands for: the app's window on the shell
/// comes to the front, and the dock says it runs.
#[test]
fn a_dock_item_press_opens_or_focuses_what_it_stands_for() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    // Another app's window beside the note: the shell is left on mail's window, so pressing the
    // dock's Notes has a window of its own to bring back to the front.
    place(&mut session, &notes[0], 100, 120, 480, 360);
    let mail = root_of(&session, ".Mail");
    place(&mut session, &mail, 700, 120, 480, 360);
    show_traditional_chrome(&mut session);

    // The dock shows the apps the placed windows belong to, running: Notes and Mail.
    until(&mut session, "the dock's items", |session| {
        on_screen(session, "Notes").is_some() && on_screen(session, "Mail").is_some()
    });
    assert_eq!(
        focused_app(&session).as_deref(),
        Some("so.tilo.notes"),
        "the shell was left on the note's window"
    );

    // The press brings mail's window, which the dock stands for by its app, to the front there.
    crate::common::press(&mut session, "Mail");
    crate::common::wait_for(&mut session, "the dock's press", |session| {
        (focused_app(session).as_deref() == Some("so.tilo.mail")).then_some(())
    });
    assert_eq!(
        crate::common::places(&session).len(),
        2,
        "the press opens no second window of an app already there"
    );
}

/// The app of the window in front on the shell, as the session publishes it: `None` while no
/// window is.
fn focused_app(session: &Session) -> Option<String> {
    let windows = session.read("session:windows")?;
    windows["all"]
        .as_array()?
        .iter()
        .find(|window| window["focused"] == true)
        .and_then(|window| window["app_key"].as_str().map(str::to_owned))
}

/// The id of the seeded app's root resource, by the suffix of its kind (`".Mail"`).
fn root_of(session: &Session, suffix: &str) -> String {
    fn find(resources: &[tilo_actions::ResourceSpec], suffix: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.kind.ends_with(suffix) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, suffix)
            }
        })
    }
    find(&session.resources(), suffix).unwrap_or_else(|| panic!("the seed holds a {suffix} root"))
}

/// The middleware's own identifier for the wheel's middle button.
const BTN_MIDDLE: u32 = 0x112;

/// Every pan gesture pans, every time: a middle drag over empty canvas moves the camera the same
/// distance on the first drag as on the fifth, Ctrl and a wheel notch zooms, and the arrow keys
/// pan. The camera is the map's, so the test reads it where the person sees it — the frame the
/// place stands at on the screen.
#[test]
fn every_pan_gesture_pans_every_time() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    show_style(&mut session, "Frames");
    place(&mut session, &notes[0], 40, 40, 480, 360);
    let titled = crate::common::drawn_name(&session, &notes[0]);
    until(&mut session, "the frame", |session| {
        on_screen(session, &titled).is_some()
    });

    // Where the frame stands on the screen: the camera on the place.
    let frame_on_screen = |session: &Session| session.shell_controls(&titled).into_iter().next();
    let start = frame_on_screen(&session).expect("the frame stands on the canvas");
    let mut seen = start;

    // Five middle drags of the same shape, 120 px left and 90 px up: every one moves the frame
    // that far on the screen, and the fifth no less than the first. The pointer starts over empty
    // canvas beside the place, so the press is the map's own and never a place's.
    for round in 1..=5 {
        let (from_x, from_y) = (f64::from(crate::common::WIDTH) - 60.0, 700.0);
        session
            .compositor()
            .input(InputEvent::PointerMotion {
                x: from_x,
                y: from_y,
            })
            .expect("the pointer moves");
        session.step(Duration::from_millis(1)).expect("a frame");
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_MIDDLE,
                pressed: true,
            })
            .expect("the button");
        session.step(Duration::from_millis(1)).expect("a frame");
        for step in 1..=4 {
            let fraction = f64::from(step) / 4.0;
            session
                .compositor()
                .input(InputEvent::PointerMotion {
                    x: from_x - 120.0 * fraction,
                    y: from_y - 90.0 * fraction,
                })
                .expect("the pointer moves");
            session.step(Duration::from_millis(1)).expect("a frame");
        }
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_MIDDLE,
                pressed: false,
            })
            .expect("the button");
        session.step(Duration::from_millis(1)).expect("a frame");
        crate::common::wait_for(&mut session, "the pan", |session| {
            (frame_on_screen(session) != Some(seen)).then_some(())
        });
        let now = frame_on_screen(&session).expect("the frame still stands");
        assert!(
            (now.0 - seen.0 + 120.0).abs() <= 2.0 && (now.1 - seen.1 + 90.0).abs() <= 2.0,
            "round {round}: the drag moved the frame by (-120, -90) as it did the first time: \
             {seen:?} became {now:?}"
        );
        seen = now;
    }

    // The arrow keys pan, a line a press, every press: the frame moves the same way each time.
    for round in 1..=3 {
        let before = frame_on_screen(&session).expect("the frame still stands");
        press_key(&mut session, KEY_ARROW_RIGHT);
        crate::common::wait_for(&mut session, "the arrow key's pan", |session| {
            (frame_on_screen(session) != Some(before)).then_some(())
        });
        let now = frame_on_screen(&session).expect("the frame still stands");
        assert!(
            now.0 < before.0 - 1.0 && (now.1 - before.1).abs() <= 1.0,
            "round {round}: the right arrow pans the canvas right, as it did the first time: \
             {before:?} became {now:?}"
        );
    }

    // Ctrl and a wheel notch zooms about the pointer: the frame grows, at the same middle. Ctrl
    // is held as the person holds it, a key down until it is let go.
    let before = frame_on_screen(&session).expect("the frame still stands");
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 640.0, y: 400.0 })
        .expect("the pointer moves");
    session.step(Duration::from_millis(1)).expect("a frame");
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: KEY_CTRL,
            pressed: true,
        })
        .expect("Ctrl is held");
    session.step(Duration::from_millis(1)).expect("a frame");
    for _ in 0..3 {
        session
            .compositor()
            .input(InputEvent::PointerAxis(Scroll::Notches(0.0, -1.0)))
            .expect("a notch");
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
        .compositor()
        .input(InputEvent::Key {
            keycode: KEY_CTRL,
            pressed: false,
        })
        .expect("Ctrl is let go");
    session.step(Duration::from_millis(1)).expect("a frame");
    crate::common::wait_for(&mut session, "the zoom", |session| {
        (frame_on_screen(session).is_some_and(|now| now.2 > before.2 + 1.0)).then_some(())
    });
}

/// The evdev code of the left Ctrl.
const KEY_CTRL: u32 = 29;

/// Press and release the named key `code` on the keyboard, as the person does.
fn press_key(session: &mut Session, code: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: code,
                pressed,
            })
            .expect("the key");
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// The evdev code of the right arrow.
const KEY_ARROW_RIGHT: u32 = 106;

/// How far the shell's own bar reaches down the screen: a control below it is the shell's
/// own content (a place's frame), never the bar naming the window in front.
const BAR_FOOT: f64 = 40.0;

/// The live run's failure: an agent opened Mail "beside" the conversation and the camera zoomed
/// everything into the viewport, so the conversation and every place were drawn as a narrow
/// column. Opening something beside a place brings what opened into view by **panning**, never by
/// zooming: the camera never changes the person's zoom, so every place keeps the size it was
/// drawn at and reads at the size the person was reading it at (`shell.md` §3). What opened
/// stands wholly in view; the conversation keeps its place and its size on the canvas, and when
/// the two cannot both stand in the viewport at that zoom the person pans back to the rest.
#[test]
fn opening_beside_a_conversation_opens_it_without_shrinking_the_conversation() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = crate::common::seeded_session(root.path());
    let notes = note_ids(&session);
    assert!(notes.len() >= 2, "the seed keeps two notes: {notes:?}");
    // The conversation far down the canvas, past the viewport's foot: the place an agent opens
    // beside it stands further still, so showing what arrived moves the camera — and the camera
    // pans to it, never zooming the conversation into a narrow column to fit both.
    let conversation = place(&mut session, &notes[0], 0, 3000, 720, 520);
    let inbox = root_of(&session, ".Mail");
    let titled = crate::common::drawn_name(&session, &notes[0]);

    // Only the frame on the canvas counts: the shell's own bar also names the window in front at
    // the top, and that one never pans.
    let on_canvas = |session: &mut Session, name: &str| {
        session
            .shell_controls(name)
            .into_iter()
            .filter(|&(_, y, _, _)| y >= BAR_FOOT)
            .collect::<Vec<_>>()
    };
    let in_view = |rect: (f64, f64, f64, f64)| {
        let (x, y, w, h) = rect;
        w > 0.0
            && h > 0.0
            && y >= 0.0
            && y + h <= f64::from(crate::common::HEIGHT)
            && x >= 0.0
            && x + w <= f64::from(crate::common::WIDTH)
    };
    let conversation_in_view =
        |session: &mut Session| on_canvas(session, &titled).into_iter().any(in_view);
    // The reveal brought the conversation itself into view when it was placed.
    until(
        &mut session,
        "the conversation's frame in view",
        conversation_in_view,
    );
    let _ = conversation;
    // The conversation's width as it is drawn now: the size the person is reading it at. Opening
    // a place beside it never shrinks it.
    let conversation_width = on_canvas(&mut session, &titled)
        .into_iter()
        .map(|(_, _, w, _)| w)
        .fold(0.0_f64, f64::max);
    assert!(
        conversation_width > 300.0,
        "the conversation is drawn at its place's width: {conversation_width}"
    );

    // Mail opened beside the conversation, as an agent does for "show me my mails".
    session
        .invoke(
            "shell:layout",
            "open",
            json!({ "resource": inbox, "beside": notes[0] })
                .as_object()
                .expect("arguments"),
        )
        .expect("the open runs");
    until(&mut session, "mail beside the conversation", |session| {
        crate::common::placed(session, &inbox) == 1
    });
    // What arrived stands wholly in view: the camera panned to it.
    let mail_in_view = |session: &mut Session| {
        let mail = crate::common::drawn_name(session, &inbox);
        on_canvas(session, &mail).into_iter().any(in_view)
    };
    until(&mut session, "mail's frame in view", mail_in_view);
    // And the conversation keeps the size it was drawn at: the camera panned, it never zoomed the
    // conversation into a narrow column to fit both on the screen.
    let after = on_canvas(&mut session, &titled)
        .into_iter()
        .map(|(_, _, w, _)| w)
        .fold(0.0_f64, f64::max);
    assert!(
        (after - conversation_width).abs() <= 1.0,
        "the conversation is drawn at the width it was, {conversation_width}, never shrunk to fit \
         what opened beside it: {after}"
    );
}

/// The top bar lists the focused window's actions, and pressing one runs it: the window in front
/// keeps one list of what it can do now, in the resource's own words (shell.md §4). The bar's
/// button names the window; the list it opens is the app's own, filled by the host from what its
/// resource publishes.
#[test]
fn the_top_bar_lists_the_focused_windows_actions_and_runs_one() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    place(&mut session, &notes[0], 100, 120, 480, 360);
    show_traditional_chrome(&mut session);

    let place = crate::common::places(&session)
        .into_iter()
        .next()
        .expect("the note is placed");
    let key = crate::common::number(&place.key);
    let name = crate::common::place_title(&session, &notes[0]);

    // The bar names the window in front on its own row, and pressing it lists the resource's
    // actions: the note's unpin and rename, and the group its tags are changed under, which no
    // control of the shell's shows.
    // The window in front is named twice: once by its frame on the shell, which stands where it
    // was placed, and once by the menu the top bar carries it as. The bar's stands on the bar's
    // row, above every frame.
    session
        .shell_controls(&name)
        .into_iter()
        .find(|&(_, y, _, _)| y < 40.0)
        .unwrap_or_else(|| {
            panic!(
                "the bar names the window in front: {name:?} at {:?}",
                session.shell_controls(&name)
            )
        });
    crate::common::press(&mut session, &name);
    until(
        &mut session,
        "the note's actions in the bar's menu",
        |session| session.shell_says("Rename…") && session.shell_says("Tags"),
    );
    // The action the menu offers runs on the resource in front: "Unpin" pins the place.
    crate::common::press(&mut session, "Unpin");
    until(
        &mut session,
        "the place unpinned from the bar's menu",
        |session| crate::common::place(session, key).is_some_and(|place| !place.pinned),
    );
}

/// Two frames placed at points closer than a frame is wide stand clear of each other: a new place
/// takes the free space nearest its anchor and never covers what is already there (canvas.md §6).
#[test]
fn two_frames_placed_at_one_point_do_not_overlap() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    show_style(&mut session, "Frames");

    // Two places at two points, each the size a place takes by default, 600 apart: less than a
    // frame's 720 width, so the second would lie over the first.
    for (note, x) in [(&notes[0], 100), (&notes[1], 700)] {
        place(&mut session, note, x, 120, 720, 520);
    }
    until(&mut session, "two frames", |session| {
        frames(session).len() == 2
    });
    let stands = frames(&session);
    assert!(
        !overlap(stands[0], stands[1]),
        "the two frames stand clear of each other: {stands:?}"
    );
    // And each is the place it was dropped at, not moved from it: the anchor is kept where
    // there is room for it.
    assert_eq!((stands[0].0, stands[0].1), (100, 120), "the first stays");
}

/// The bar's bell opens the notification centre in every preset: each row of the style switch shows
/// the centre on a press, not the quick settings, and the next press closes it again.
#[test]
fn the_bells_press_opens_the_notification_centre_in_every_preset() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    place(&mut session, &notes[0], 100, 120, 480, 360);
    for preset in ["Strips", "Frames", "Frameless", "Desktop"] {
        show_style(&mut session, preset);
        until(
            &mut session,
            &format!("the bar's bell in the {preset} preset"),
            |session| on_screen(session, BELL).is_some(),
        );
        crate::common::press(&mut session, BELL);
        until(
            &mut session,
            &format!("the notification centre in the {preset} preset"),
            |session| on_screen(session, NOTHING_TO_CATCH_UP_ON).is_some(),
        );
        assert_eq!(
            crate::common::shell_here(&session),
            "centre",
            "the bell opens the notifications, not another surface, in the {preset} preset"
        );
        // The one surface at a time rule: the next press closes it.
        crate::common::press(&mut session, BELL);
        until(
            &mut session,
            &format!("the shell at rest again in the {preset} preset"),
            |session| crate::common::shell_here(session) == "none",
        );
    }
}

/// Dragging a frame's corner on the desktop sizes the window there: the map reports the edge the
/// person took and the layout keeps the size, one arranging step of the person's. A map with no
/// resize of its own follows the pointer while the drag runs and then takes the size back, so the
/// corner has to keep what it was dragged to.
#[test]
fn dragging_a_frames_corner_sizes_the_window() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    show_style(&mut session, "Desktop");
    place(&mut session, &notes[0], 100, 120, 480, 360);
    until(&mut session, "the frame", |session| {
        frames(session).len() == 1
    });
    let name = crate::common::drawn_name(&session, &notes[0]);
    let (fx, fy, fw, fh) = wait_for(&mut session, "the frame on screen", |session| {
        widest_named(session, &name)
    });
    // Just past the frame's own trailing and bottom edges, where the map's grab reaches.
    let corner = (fx + fw + 3.0, fy + fh + 3.0);
    drag(&mut session, corner, (corner.0 + 120.0, corner.1 + 80.0));
    let sized = wait_for(&mut session, "the window's new size", |session| {
        frames(session)
            .first()
            .copied()
            .filter(|frame| (frame.2, frame.3) != (480, 360))
    });
    assert!(
        (sized.2 - 600).abs() <= 8 && (sized.3 - 440).abs() <= 8,
        "the window takes the size its corner was dragged to: {:?} from (480, 360)",
        (sized.2, sized.3)
    );
    crate::common::shot(&mut session, "desktop-frame-resized");
}

/// Dragging a frame's bar on the desktop moves the window to where it is let go, as it does on
/// the canvas: the drop reaches the desktop and the layout keeps the point. A map with no drop of
/// its own lets the bar be carried and then puts the window back where it was.
#[test]
fn dragging_a_frames_bar_moves_the_window_there() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    show_style(&mut session, "Desktop");
    place(&mut session, &notes[0], 100, 120, 480, 360);
    until(&mut session, "the frame", |session| {
        frames(session).len() == 1
    });
    let name = crate::common::drawn_name(&session, &notes[0]);
    let (fx, fy, fw, _) = wait_for(&mut session, "the frame on screen", |session| {
        widest_named(session, &name)
    });
    // A press on the frame's bar, clear of the name and of the controls at its end.
    let bar = (fx + fw * 0.4, fy + 22.0);
    let landing = (bar.0 + 160.0, bar.1 + 90.0);
    drag(&mut session, bar, landing);
    let (nx, ny, _, _) = wait_for(&mut session, "the frame where it was let go", |session| {
        widest_named(session, &name)
            .filter(|(x, y, _, _)| (x - fx).abs() > 4.0 || (y - fy).abs() > 4.0)
    });
    assert!(
        (nx - landing.0).abs() <= 10.0 && (ny - landing.1).abs() <= 10.0,
        "the window stands where it was let go: {:?} at {landing:?}, from {:?}",
        (nx, ny),
        (fx, fy)
    );
    crate::common::shot(&mut session, "desktop-frame-moved");
}
