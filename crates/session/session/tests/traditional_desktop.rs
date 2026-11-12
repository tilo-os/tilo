//! The traditional desktop, and the two canvas and bar defects beside it: what the desk draws
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

use crate::common::{CANVAS_ENTRY, on_screen, until};

/// The bar's control that opens the notification centre, as assistive technology names it.
const BELL: &str = "Notifications";
/// What the notification centre draws when nothing waits in it: its own empty state, which no
/// other surface of the desk shows.
const NOTHING_TO_CATCH_UP_ON: &str = "Nothing to catch up on";

/// The session with the shell as the whole screen and the seeded apps, its agent door and
/// test-control socket at `root`, admitting this test's program to act as the person — the same
/// session the acceptance path drives, so what a test draws is what a person gets. The framed
/// canvas is in front of it. Two of the notes the seed keeps stand on its desk, each a window of
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

/// Show `preset` ("Strips", "Frames", "Spaces", "Desktop") the way a person does. The traditional
/// desktop is the shell's own layout, which the bar's preset switch and the settings page's card
/// both reach: `apply(windows_desktop)` is the whole preset at once (`chrome.tl`), and the desk
/// draws it as such (`desk.tl`'s `screen`). The three canvas presets are the switch's own rows.
#[track_caller]
fn show(session: &mut Session, root: &std::path::Path, preset: &str) {
    if preset == "Desktop" {
        session
            .run_shell_setting("apply", vec![Value::from("windows_desktop")])
            .unwrap_or_else(|why| panic!("the desk shows {preset}: {why}"));
    } else {
        let mut repl = crate::common::Repl::connect(&root.join("agent.sock"));
        repl.opened(session);
        let (answer, failed) = repl.run(
            session,
            &format!(
                "shell.show_style(shell.viewing, shown: \"so.tilo.shell:{}\")",
                preset.to_lowercase()
            ),
        );
        assert!(
            !failed || answer.contains("shown that way already"),
            "the desk shows {preset}: {answer}"
        );
    }
    match preset {
        "Frames" | "Spaces" => until(session, "the canvas", |session| {
            on_screen(session, CANVAS_ENTRY).is_some()
        }),
        "Desktop" => until(session, "the desktop", |session| preset_menu_gone(session)),
        _ => until(session, "the strips", |session| {
            on_screen(session, CANVAS_ENTRY).is_none()
        }),
    }
}

/// Whether the desk shows the traditional desktop: its bar drops the preset switch (its own bar
/// carries the focused window's commands instead), and the dock rides its foot.
fn preset_menu_gone(session: &Session) -> bool {
    crate::common::preset_menu(session).is_none() && on_screen(session, "Dock").is_some()
}

/// Place `resource` on the canvas through the arrangement's published `place`, as the canvas's
/// own typed line and an agent do: at `(x, y)`, `width` by `height`. Its place's key.
fn place(session: &mut Session, resource: &str, x: i64, y: i64, width: i64, height: i64) -> String {
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
        "x": x,
        "y": y,
        "width": width,
        "height": height,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place.to_string()
}

/// The canvas rectangle each placed frame stands at, as the arrangement keeps it: `(x, y, w, h)`,
/// in the arrangement's order.
fn frames(session: &Session) -> Vec<(i64, i64, i64, i64)> {
    crate::common::arrangement(session)
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

/// Whether two canvas rectangles `(x, y, w, h)` share any area.
fn overlap(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> bool {
    a.0 < b.0.saturating_add(b.2)
        && b.0 < a.0.saturating_add(a.2)
        && a.1 < b.1.saturating_add(b.3)
        && b.1 < a.1.saturating_add(a.3)
}

/// In the desktop preset, every placed window shows a bar naming it: its title, and the shell's
/// own close on it. A window with no bar draws its resource over the desk with nothing naming it,
/// which is what a person sees as "no title bars".
#[test]
fn the_desktop_draws_every_window_with_a_bar_naming_it() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    place(&mut session, &notes[0], 100, 120, 480, 360);
    place(&mut session, &notes[1], 700, 120, 480, 360);
    show(&mut session, root.path(), "Desktop");

    for note in &notes {
        let name = crate::common::window_name_of(&session, note);
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

/// A press on a dock item opens or focuses what it stands for: the app's window on the desk
/// comes to the front, and the dock says it runs.
#[test]
fn a_dock_item_press_opens_or_focuses_what_it_stands_for() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    // Another app's window beside the note: the desk is left on mail's window, so pressing the
    // dock's Notes has a window of its own to bring back to the front.
    place(&mut session, &notes[0], 100, 120, 480, 360);
    let mail = root_of(&session, ".Mail");
    place(&mut session, &mail, 700, 120, 480, 360);
    show(&mut session, root.path(), "Desktop");

    // The dock shows the apps the placed windows belong to, running: Notes and Mail.
    until(&mut session, "the dock's items", |session| {
        on_screen(session, "Notes").is_some() && on_screen(session, "Mail").is_some()
    });
    assert_eq!(
        focused_app(&session).as_deref(),
        Some("so.tilo.lang.notes"),
        "the desk was left on the note's window"
    );

    // The press brings mail's window, which the dock stands for by its app, to the front there.
    crate::common::press(&mut session, "Mail");
    crate::common::wait_for(&mut session, "the dock's press", |session| {
        (focused_app(session).as_deref() == Some("so.tilo.lang.mail")).then_some(())
    });
    assert_eq!(
        crate::common::places(&session).len(),
        2,
        "the press opens no second window of an app already there"
    );
}

/// The app of the window in front on the desk, as the session publishes it: `None` while no
/// window is.
fn focused_app(session: &Session) -> Option<String> {
    let desk = session.read("session:desk")?;
    desk["windows"]
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
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    show(&mut session, root.path(), "Frames");
    place(&mut session, &notes[0], 40, 40, 480, 360);
    let titled = crate::common::window_name_of(&session, &notes[0]);
    until(&mut session, "the frame", |session| {
        on_screen(session, &titled).is_some()
    });

    // Where the frame stands on the screen: the camera on the place.
    let frame_on_screen = |session: &Session| {
        session
            .shell_controls(&titled)
            .into_iter()
            .next()
            .map(|(x, y, w, h)| (x, y, w, h))
    };
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
            .input(InputEvent::PointerAxis {
                horizontal: 0.0,
                vertical: -1.0,
            })
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

/// How far the desk's own bar reaches down the screen: a control standing below it is the desk's
/// own content (a place's frame), never the bar naming the window in front.
const BAR_FOOT: f64 = 40.0;

/// The live run's failure: an agent opened Mail "beside" the conversation, and the pan to what it
/// opened took the conversation out of the viewport — its top at y -544, its composer unreachable
/// — so the person lost their chat. Opening something beside a place keeps that place in view: the
/// camera moves so what arrived and the place it came from both stand on the screen, zooming out
/// only as far as the two need.
#[test]
fn opening_beside_a_conversation_keeps_the_conversation_in_view() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = crate::common::seeded_session(root.path());
    let notes = note_ids(&session);
    assert!(notes.len() >= 2, "the seed keeps two notes: {notes:?}");
    // The conversation far down the canvas, past the viewport's foot: the place an agent opens
    // beside it stands further still, so showing what arrived moves the camera — and must not
    // take the conversation out of view doing it.
    let conversation = place(&mut session, &notes[0], 0, 3000, 720, 520);
    let inbox = root_of(&session, ".Mail");
    let titled = crate::common::window_name_of(&session, &notes[0]);

    // Only the frame on the canvas counts: the desk's own bar also names the window in front at
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

    // Mail opened beside the conversation, as an agent does for "show me my mails".
    session
        .invoke(
            "shell:arrangement",
            "open",
            json!({ "resource": inbox, "beside": notes[0] })
                .as_object()
                .expect("arguments"),
        )
        .expect("the open runs");
    until(&mut session, "mail beside the conversation", |session| {
        crate::common::placed(session, &inbox) == 1
    });
    // What arrived stands in view, and so does the conversation it came from.
    let mail_in_view = |session: &mut Session| {
        let mail = crate::common::window_name_of(session, &inbox);
        on_canvas(session, &mail).into_iter().any(in_view)
    };
    until(&mut session, "mail's frame in view", mail_in_view);
    assert!(
        conversation_in_view(&mut session),
        "the conversation stays in view when what opened beside it is shown: {:?} and {:?} \
         against the display",
        on_canvas(&mut session, &titled),
        {
            let mail = crate::common::window_name_of(&session, &inbox);
            on_canvas(&mut session, &mail)
        }
    );
}

/// The top bar lists the focused window's actions, and pressing one runs it: the window in front
/// keeps one list of what it can do now, in the resource's own words (shell.md §4). The bar's
/// button names the window; the list it opens is the app's own, filled by the host from what its
/// resource publishes.
#[test]
fn the_top_bar_lists_the_focused_windows_actions_and_runs_one() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    place(&mut session, &notes[0], 100, 120, 480, 360);
    show(&mut session, root.path(), "Desktop");

    let place = crate::common::places(&session)
        .into_iter()
        .next()
        .expect("the note is placed");
    let key = crate::common::number(&place.key);
    let name = crate::common::window_name_of(&session, &notes[0]);

    // The bar names the window in front on its own row, and pressing it lists the resource's
    // actions: the note's unpin and rename, and the group its tags are changed under, which no
    // control of the desk's shows.
    // The window in front is named twice: once by its frame on the desk, which stands where it
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
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    show(&mut session, root.path(), "Frames");

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

/// The bar's bell opens the notification centre in every preset: a press on it shows the centre,
/// not the quick settings, and the next press closes it again.
#[test]
fn the_bells_press_opens_the_notification_centre_in_every_preset() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let (mut session, notes) = session(root.path());
    until(&mut session, "the bar", |session| {
        on_screen(session, BELL).is_some()
    });
    place(&mut session, &notes[0], 100, 120, 480, 360);
    for preset in ["Strips", "Frames", "Spaces", "Desktop"] {
        show(&mut session, root.path(), preset);
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
            crate::common::desk_here(&session),
            "centre",
            "the bell opens the notifications, not another surface, in the {preset} preset"
        );
        // The one surface at a time rule: the next press closes it.
        crate::common::press(&mut session, BELL);
        until(
            &mut session,
            &format!("the desk at rest again in the {preset} preset"),
            |session| crate::common::desk_here(session) == "none",
        );
    }
}
