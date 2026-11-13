//! The native session across two displays, with real Wayland clients: each display holds its own
//! windows, new windows open on the display with focus, focus and windows move between displays
//! through the desk's published actions, a mirror is one display, and dialogs float over the window
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
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE, DISPLAY_KIND, WINDOW_KIND};
use tilo_desktop_model::WindowWidth;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
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

/// The displays the desk publishes, in reading order.
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

/// The name the desk gives a window whose bar says its own app id.
fn desk_name(app: &str) -> String {
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
/// displays are two strips' worth of room, so nothing is shared across the seam.
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

#[test]
fn new_windows_open_on_the_display_with_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    let names: Vec<String> = displays(&session).into_iter().map(|d| d.label).collect();
    assert_eq!(
        names.len(),
        2,
        "the desk publishes both displays: {names:?}"
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

/// The displays' own arrangement action moves the pictures: mirrored, the display beside the one
/// with the window shows that window too.
#[test]
fn an_arrangement_action_mirrors_the_other_display() {
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
        DESK_RESOURCE,
        "step-focus",
        serde_json::json!({"steps": -1}),
    );
    client.pump(&mut session, |session, _| {
        focused(session).as_deref() == Some("org.example.Left")
    });

    invoke(
        &mut session,
        &mut client,
        DESK_RESOURCE,
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
    assert_eq!(window_tabs(&session), [desk_name("org.example.Only")]);
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
        [desk_name("org.example.Editor")],
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

#[test]
fn a_fixed_size_window_floats_on_the_display_with_focus() {
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
    let (x, y, w, h) = rect(&mut session, "org.example.Prompt");
    assert_eq!((w, h), (400, 240));
    assert_eq!(
        (x, y),
        (WIDTH + (WIDTH - w) / 2, (HEIGHT - h) / 2),
        "centred on the focused display"
    );
    assert!(window_tabs(&session).is_empty(), "no tab for a dialog");
}

#[test]
fn a_question_opens_on_the_display_of_the_window_in_focus() {
    let _settled = ui::motion::testing::settled();
    let questions = tilo_service::Questions::new();
    let outputs = [
        OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT).at(0, 0),
        OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT).at(WIDTH, 0),
    ];
    let mut session = crate::common::headless_on(
        &outputs,
        Options {
            questions: Some(questions.clone()),
            ..Options::default()
        },
    );
    session.step(Duration::from_millis(1)).unwrap();
    let mut client = client(&mut session);
    tab(&mut session, &mut client, "org.example.Left");
    focus_display(&mut session, &mut client, 1);
    tab(&mut session, &mut client, "org.example.Right");
    client.pump(&mut session, |session, _| {
        focused(session).as_deref() == Some("org.example.Right")
    });

    // Held for the whole test: dropping it withdraws the question.
    let _asked = questions.ask(tilo_service::Question {
        handle: "display-test:/access".to_owned(),
        kind: "portal.access".to_owned(),
        app_id: Some("org.example.Counter".to_owned()),
        label: "org.example.Counter".to_owned(),
        details: serde_json::json!({
            "capability": "screencast",
            "reason": null,
            "may_remember": false,
            "text": null,
        }),
    });
    let right = (
        WIDTH,
        0,
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    );
    client.pump(&mut session, |session, _| {
        let presentation = session.portal().expect("portal sheet").presentation();
        presentation.place().is_some() && presentation.is_open()
    });
    assert_eq!(
        session
            .portal()
            .expect("portal sheet")
            .presentation()
            .place(),
        Some(right),
        "the sheet covers the display of the window in focus"
    );
    assert!(
        session
            .portal()
            .expect("portal sheet")
            .presentation()
            .is_open(),
        "and is up for the person"
    );
}
