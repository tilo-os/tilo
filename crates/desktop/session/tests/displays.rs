//! The native session across two displays, with real Wayland clients: each display holds its own
//! windows, new windows open on the display with focus, focus and windows move between displays
//! through the shell's published actions, a mirror is one display, and dialogs float over the window
//! they belong to instead of taking a tab.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::time::Duration;

use crate::common::window::{Client, window_of};
use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, InputEvent, OutputConfig};
use tilo_desktop_model::WindowWidth;
use tilo_session::{Options, Session};
use tilo_session_core::desktop::verbs::{
    CHROME_RESOURCE, DISPLAY_KIND, SHELL_RESOURCE, WINDOW_KIND,
};
use tilo_sheets::SheetState as _;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// The colour the compositor paints the test windows' pixels, read back as RGBA.
const WINDOW: [u8; 3] = [40, 40, 200];

/// A session on `outputs` , each `WIDTH` x `HEIGHT` at its place.
fn session(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT).at(place.0, place.1)
        })
        .collect();
    let mut session = crate::common::headless_on(
        &outputs,
        Options {
            collab: false,
            ..Options::default()
        },
    );
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// Two displays side by side.
fn docked() -> Session {
    session(&[(0, 0), (WIDTH, 0)])
}

/// A window client that heard the session's globals.
fn client(session: &mut Session) -> Client {
    let mut client = Client::connect(session.compositor());
    client.pump(session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some()
    });
    client
}

/// Open a window of `app_id` and draw it, set up by `before` before its first commit.
fn map(
    session: &mut Session,
    client: &mut Client,
    app_id: &str,
    fallback: (i32, i32),
    before: impl FnOnce(&XdgToplevel),
) -> usize {
    client.map(session, app_id, fallback, before)
}

/// Open a window of `app_id` at the size it chose.
fn tab(session: &mut Session, client: &mut Client, app_id: &str) -> usize {
    map(session, client, app_id, (300, 200), |_| {})
}

/// The window of `app` and its rectangle `(x, y, w, h)` .
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    window_of(compositor, app)
}

fn rect(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    window(session.compositor(), app).expect("mapped").1
}

/// Which display (0 left, 1 right) the window of `app` starts on; `None` while it is out of sight.
fn display_of(session: &mut Session, app: &str) -> Option<i32> {
    let (x, y, _, _) = rect(session, app);
    (x >= 0 && y >= 0).then(|| x.div_euclid(WIDTH))
}

fn find(resources: &[ResourceSpec], hit: &dyn Fn(&ResourceSpec) -> bool) -> Vec<ResourceSpec> {
    resources
        .iter()
        .flat_map(|resource| {
            let mut found = find(&resource.children, hit);
            if hit(resource) {
                found.insert(0, resource.clone());
            }
            found
        })
        .collect()
}

/// The displays the shell publishes, in reading order.
fn displays(session: &Session) -> Vec<ResourceSpec> {
    find(&session.resources(), &|resource| {
        resource.kind == DISPLAY_KIND
    })
}

fn window_tabs(session: &Session) -> Vec<String> {
    find(&session.resources(), &|resource| {
        resource.kind == WINDOW_KIND
    })
    .into_iter()
    .map(|resource| resource.label)
    .collect()
}

/// The name the shell gives a window whose bar says its own app id.
fn shell_name(app: &str) -> String {
    tilo_desktop_search::names::window_name(app, app)
}

/// Invoke `action` of `resource` , then let the session run a few frames.
fn invoke(
    session: &mut Session,
    client: &mut Client,
    resource: &str,
    action: &str,
    arguments: serde_json::Value,
) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

fn focus_display(session: &mut Session, client: &mut Client, index: usize) {
    let display = displays(session)[index].id.clone();
    invoke(
        session,
        client,
        display.as_str(),
        "focus-display",
        serde_json::json!({}),
    );
}

/// The strip each display shows, by display id.
fn shown_strips(session: &Session) -> Vec<(u32, String)> {
    session.desktop().with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.outputs
                .displays
                .iter()
                .map(|display| {
                    let strip = tree.strip_shown_on(display.id);
                    (display.id, format!("{strip:?}"))
                })
                .collect()
        })
    })
}

/// Each strip group's width as the model holds it, with the display it lives on and the window it
/// shows: a display's windows keep their width whatever opens on another.
fn widths(session: &Session) -> Vec<(u32, WindowWidth, Option<String>)> {
    session.desktop().with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .map(|group| {
                    (
                        tree.outputs.display_of(group.id).unwrap_or(u32::MAX),
                        group.width,
                        group.tabs.first().map(|tab| tab.title.clone()),
                    )
                })
                .collect()
        })
    })
}

fn focused(session: &mut Session) -> Option<String> {
    session
        .compositor()
        .focused_window()
        .and_then(|info| info.app_id)
}

/// How many pixels of `output` show the test windows' colour: what a person sees of the windows on
/// that display.
fn painted(session: &mut Session, output: &str) -> usize {
    session
        .compositor()
        .screenshot(output)
        .expect("a frame")
        .rgba
        .chunks_exact(4)
        .filter(|pixel| pixel[..3] == WINDOW)
        .count()
}

/// Each display keeps the strip it shows: making another strip and stepping strips moves the
/// focused display's own strip, and the other display keeps its strip and its windows throughout.
#[test]
fn each_display_keeps_the_strip_it_shows() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    client.pump(&mut session, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") > 0
    });

    // The person works on the left display: the new strip shows there, and the right display keeps
    // the strip it shows, window and all.
    focus_display(&mut session, &mut client, 0);
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "new-strip",
        serde_json::json!({}),
    );
    for _ in 0..60 {
        client.exchange();
        session.step(Duration::from_millis(16)).unwrap();
    }
    let shown = shown_strips(&session);
    assert_ne!(
        shown[0].1, shown[1].1,
        "the left display took the new strip and the right kept its own: {shown:?}"
    );
    client.pump(&mut session, |session, _| {
        painted(session, "HEADLESS-1") == 0 && painted(session, "HEADLESS-2") > 0
    });

    // Back to the strip the left display showed, its window is there again.
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "step-strip",
        serde_json::json!({"steps": -1}),
    );
    client.pump(&mut session, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") > 0
    });

    // The person goes on to the right display and steps on there: that display moves to the other
    // strip, the left one stays where it is.
    focus_display(&mut session, &mut client, 1);
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "step-strip",
        serde_json::json!({"steps": 1}),
    );
    client.pump(&mut session, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") == 0
    });

    // And back, each display with its window.
    invoke(
        &mut session,
        &mut client,
        CHROME_RESOURCE,
        "step-strip",
        serde_json::json!({"steps": -1}),
    );
    client.pump(&mut session, |session, _| {
        painted(session, "HEADLESS-1") > 0 && painted(session, "HEADLESS-2") > 0
    });
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Right"));
}

/// A window opening on one display leaves the windows of the other display the size they are: two
/// displays are two strips' worth of room, so nothing is shared across the border.
#[test]
fn a_window_opening_beside_one_display_leaves_the_other_alone() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    client.pump(&mut session, |session, _| {
        display_of(session, "org.example.Left") == Some(0)
    });
    let alone = rect(&mut session, "org.example.Left");
    let width = widths(&session);
    assert!(
        painted(&mut session, "HEADLESS-1") > 0,
        "the left display shows its window"
    );

    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    client.pump(&mut session, |session, _| {
        display_of(session, "org.example.Right") == Some(1)
    });
    assert_eq!(
        widths(&session).first(),
        width.first(),
        "the left display's window keeps the width it is given"
    );
    assert_eq!(
        rect(&mut session, "org.example.Left"),
        alone,
        "and the place and size a person sees it at"
    );
    assert!(
        painted(&mut session, "HEADLESS-2") > 0,
        "the window that opened shows on its own display"
    );
}

/// The pixels of `output` along its bottom edge from column `from` to `to`: where the strip's rail
/// stands.
fn bottom_edge(session: &mut Session, output: &str, (from, to): (i32, i32)) -> Vec<u8> {
    let frame = session.compositor().screenshot(output).expect("a frame");
    let width = usize::try_from(frame.width).unwrap();
    let height = usize::try_from(frame.height).unwrap();
    let columns = usize::try_from(from).unwrap() * 4..usize::try_from(to).unwrap() * 4;
    (height - 16..height)
        .flat_map(|row| frame.rgba[row * width * 4..][columns.clone()].to_vec())
        .collect()
}

/// The strip's rail stands under the strip of the display the person works on, never across the
/// border onto the other display.
#[test]
fn the_strip_rail_stands_on_the_display_in_use() {
    let _settled = ui::motion::testing::settled();
    // One instant for the whole test: the rail a window's opening shows stays up while it is read.
    tilo_clock::testing::with_monotonic_time(tilo_clock::now(), || {
        let mut session = docked();
        let mut client = client(&mut session);
        let middle = (WIDTH / 2 - 40, WIDTH / 2 + 40);
        let border = (0, 40);
        let empty = (
            bottom_edge(&mut session, "HEADLESS-1", middle),
            bottom_edge(&mut session, "HEADLESS-2", border),
        );
        tab(&mut session, &mut client, "org.example.Left");
        client.pump(&mut session, |session, _| {
            display_of(session, "org.example.Left") == Some(0)
                && bottom_edge(session, "HEADLESS-1", middle) != empty.0
        });
        assert_eq!(
            bottom_edge(&mut session, "HEADLESS-2", border),
            empty.1,
            "nothing of the rail stands on the other display"
        );
    });
}

#[test]
fn new_windows_open_on_the_display_with_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    let names: Vec<String> = displays(&session).into_iter().map(|d| d.label).collect();
    assert_eq!(
        names.len(),
        2,
        "the shell publishes both displays: {names:?}"
    );

    tab(&mut session, &mut client, "org.example.Left");
    assert_eq!(display_of(&mut session, "org.example.Left"), Some(0));

    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    client.pump(&mut session, |session, _| {
        display_of(session, "org.example.Right") == Some(1)
    });
    assert_eq!(
        display_of(&mut session, "org.example.Left"),
        Some(0),
        "the first window stays on its display"
    );
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Right"));
}

/// The displays' own mode action moves the pictures: mirrored, the display beside the one
/// with the window shows that window too.
#[test]
fn a_mode_action_mirrors_the_other_display() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    assert_eq!(
        painted(&mut session, "HEADLESS-2"),
        0,
        "the display beside the window shows nothing yet"
    );

    invoke(
        &mut session,
        &mut client,
        tilo_session::displays::RESOURCE,
        "set-mode",
        serde_json::json!({"mode": "mirror"}),
    );
    client.pump(&mut session, |session, _| {
        painted(session, "HEADLESS-2") > 0
    });
    assert_eq!(
        painted(&mut session, "HEADLESS-2"),
        painted(&mut session, "HEADLESS-1"),
        "a mirror shows both displays the same picture"
    );
}

#[test]
fn focus_follows_the_pointer_onto_the_other_display() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Right"));

    let (x, y, w, h) = rect(&mut session, "org.example.Left");
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(x + w / 2),
            y: f64::from(y + h / 2),
        })
        .unwrap();
    client.pump(&mut session, |session, _| {
        focused(session).as_deref() == Some("org.example.Left")
    });

    // Moving about the same display leaves focus where it is.
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(x + 1),
            y: f64::from(y + 1),
        })
        .unwrap();
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Left"));
}

#[test]
fn focus_steps_across_displays_and_a_window_moves_to_the_next() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");

    invoke(
        &mut session,
        &mut client,
        SHELL_RESOURCE,
        "step-focus",
        serde_json::json!({"steps": -1}),
    );
    client.pump(&mut session, |session, _| {
        focused(session).as_deref() == Some("org.example.Left")
    });

    invoke(
        &mut session,
        &mut client,
        SHELL_RESOURCE,
        "move-to-display",
        serde_json::json!({"steps": 1}),
    );
    client.pump(&mut session, |session, _| {
        display_of(session, "org.example.Left") == Some(1)
    });
    assert_eq!(display_of(&mut session, "org.example.Right"), Some(1));
    assert_eq!(focused(&mut session).as_deref(), Some("org.example.Left"));
}

#[test]
fn a_mirror_is_one_display() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(&[(0, 0), (0, 0)]);
    let mut client = client(&mut session);
    assert!(
        displays(&session).is_empty(),
        "one display needs no telling apart"
    );
    tab(&mut session, &mut client, "org.example.Only");
    assert_eq!(display_of(&mut session, "org.example.Only"), Some(0));
    assert_eq!(window_tabs(&session), [shell_name("org.example.Only")]);
}

#[test]
fn a_dialog_floats_centred_over_its_parent_without_a_tab() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    let parent = tab(&mut session, &mut client, "org.example.Editor");
    let parent_toplevel = client.toplevel(parent).clone();
    map(
        &mut session,
        &mut client,
        "org.example.Editor.Save",
        (320, 180),
        |toplevel| toplevel.set_parent(Some(&parent_toplevel)),
    );

    let (px, py, pw, ph) = rect(&mut session, "org.example.Editor");
    let (dx, dy, dw, dh) = rect(&mut session, "org.example.Editor.Save");
    assert_eq!((dw, dh), (320, 180), "a dialog keeps the size it chose");
    assert!(
        (dx - (px + (pw - dw) / 2)).abs() <= 1 && (dy - (py + (ph - dh) / 2)).abs() <= 1,
        "centred over its parent: parent {:?}, dialog {:?}",
        (px, py, pw, ph),
        (dx, dy, dw, dh)
    );
    assert_eq!(
        window_tabs(&session),
        [shell_name("org.example.Editor")],
        "a dialog takes no tab"
    );
    assert_eq!(
        focused(&mut session).as_deref(),
        Some("org.example.Editor.Save"),
        "the dialog has the keyboard"
    );
    let info = session
        .compositor()
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some("org.example.Editor.Save"))
        .unwrap()
        .0;
    assert!(info.parent.is_some(), "the compositor tells its parent");
}

/// A window that keeps one size — a program's own top-level window, as an X11 settings window is
/// under Proton — takes a tab on the display with focus, and the keyboard with it.
#[test]
fn a_fixed_size_window_takes_a_tab_and_the_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    focus_display(&mut session, &mut client, 1);
    map(
        &mut session,
        &mut client,
        "org.example.Prompt",
        (400, 240),
        |toplevel| {
            toplevel.set_min_size(400, 240);
            toplevel.set_max_size(400, 240);
        },
    );
    let (_, _, w, h) = rect(&mut session, "org.example.Prompt");
    assert!(
        w >= 400 && h >= 240,
        "its tab is at least the size the window keeps: {w}x{h}"
    );
    assert_eq!(
        window_tabs(&session),
        [shell_name("org.example.Prompt")],
        "it takes a tab"
    );
    assert_eq!(
        display_of(&mut session, "org.example.Prompt"),
        Some(1),
        "on the display with focus"
    );
    assert_eq!(
        focused(&mut session).as_deref(),
        Some("org.example.Prompt"),
        "the tab has the keyboard"
    );
}

/// A fixed-size window is a tab like any other: opening it beside a tab gives it its own, every
/// pass leaves the keyboard with it, and a tab opened after it comes in front as any tab does.
#[test]
fn a_fixed_size_window_takes_its_own_tab_beside_the_tabs() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Manager");
    map(
        &mut session,
        &mut client,
        "org.example.Prompt",
        (400, 240),
        |toplevel| {
            toplevel.set_min_size(400, 240);
            toplevel.set_max_size(400, 240);
        },
    );
    assert_eq!(
        window_tabs(&session),
        [
            shell_name("org.example.Manager"),
            shell_name("org.example.Prompt")
        ],
        "it takes a tab of its own"
    );
    // The keyboard stays with its tab, pass after pass.
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        focused(&mut session).as_deref(),
        Some("org.example.Prompt"),
        "what the person types goes to the window in front"
    );
    tab(&mut session, &mut client, "org.example.Later");
    assert_eq!(
        focused(&mut session).as_deref(),
        Some("org.example.Later"),
        "a tab opened after it comes in front"
    );
    let stack: Vec<Option<String>> = session
        .compositor()
        .windows()
        .into_iter()
        .map(|(info, _)| info.app_id)
        .collect();
    assert_eq!(
        stack.last().cloned().flatten().as_deref(),
        Some("org.example.Later"),
        "the newest tab stands in front, bottom first: {stack:?}"
    );
}
