//! Where a sheet sits: the display the person is working on, the first one when that display is
//! gone, and above a window that fills the display without that window keeping the keyboard.

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

use crate::common::window::{Client, KEY_A, KEY_ESCAPE, WINDOW, WINDOW_BYTES, window_of};
use tilo_actions::ResourceSpec;
use tilo_compositor::{Compositor, OutputConfig, Screenshot};
use tilo_desktop::desk_verbs::DISPLAY_KIND;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;

const WIDTH: i32 = 1000;
const HEIGHT: i32 = 700;

/// A session on `outputs` , each `WIDTH` x `HEIGHT` at the place it names.
fn session_at(outputs: &[OutputConfig]) -> Session {
    let mut session = crate::common::headless_on(
        outputs,
        Options {
            collab: false,
            ..Options::default()
        },
    );
    session.step(Duration::from_millis(1)).unwrap();
    session
}

/// The displays `HEADLESS-1` and further, `WIDTH` x `HEIGHT` at `places` .
fn displays_at(places: &[(i32, i32)]) -> Session {
    let outputs: Vec<OutputConfig> = places
        .iter()
        .enumerate()
        .map(|(index, place)| {
            OutputConfig::new(format!("HEADLESS-{}", index + 1), WIDTH, HEIGHT).at(place.0, place.1)
        })
        .collect();
    session_at(&outputs)
}

/// Two displays side by side.
fn docked() -> Session {
    displays_at(&[(0, 0), (WIDTH, 0)])
}

fn client(session: &mut Session) -> Client {
    let mut client = Client::connect(session.compositor());
    client.pump(session, |_, seen| {
        seen.wm_base.is_some() && seen.shm.is_some() && seen.seat.is_some()
    });
    // The seat tells the client its capabilities on its first event.
    client.pump(session, |_, seen| seen.keyboard.is_some());
    client
}

/// Open toplevel of `app_id` (set up by `before` ), drawn at the size it was configured to, else
/// `fallback` , and wait until it is mapped.
fn map(
    session: &mut Session,
    client: &mut Client,
    app_id: &str,
    fallback: (i32, i32),
    before: impl FnOnce(&XdgToplevel),
) -> usize {
    client.map(session, app_id, fallback, before)
}

/// The window of `app` and its rectangle `(x, y, w, h)` .
fn window(compositor: &Compositor, app: &str) -> Option<(u64, (i32, i32, i32, i32))> {
    window_of(compositor, app)
}

fn rect(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    window(session.compositor(), app).expect("mapped").1
}

/// The displays the desk publishes, in reading order.
fn displays(session: &Session) -> Vec<ResourceSpec> {
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
    find(&session.resources(), &|resource| {
        resource.kind == DISPLAY_KIND
    })
}

fn focuses(session: &mut Session, client: &mut Client, index: usize) {
    let display = displays(session)[index].id.clone();
    session
        .invoke(display.as_str(), "focus-display", &serde_json::Map::new())
        .unwrap_or_else(|error| panic!("{display:?} focus: {error:?}"));
    for _ in 0..4 {
        client.exchange();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// A frame of `output` , once the session has drawn what it has.
fn shot(session: &mut Session, output: &str) -> Screenshot {
    session
        .compositor()
        .screenshot(output)
        .unwrap_or_else(|error| panic!("a frame of {output}: {error}"))
}

fn pixel(frame: &Screenshot, x: i32, y: i32) -> [u8; 4] {
    let x = u32::try_from(x).expect("on the output");
    let y = u32::try_from(y).expect("on the output");
    frame.pixel(x, y).expect("a pixel of the frame")
}

/// The point at the middle of the menu's panel, in its own coordinates, once the menu has laid the
/// panel out.
fn panel_centre(session: &mut Session) -> (i32, i32) {
    let panel = crate::common::wait_for(session, "the menu's panel", |session| {
        let panel = session.power_menu().expect("power sheet").panel();
        (panel.x1 > panel.x0 && panel.y1 > panel.y0).then_some(panel)
    });
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a point on a display, well inside its bounds"
    )]
    let centre = (
        f64::midpoint(panel.x0, panel.x1).round() as i32,
        f64::midpoint(panel.y0, panel.y1).round() as i32,
    );
    centre
}

/// The rectangle of the display `HEADLESS-2` , the right one of two.
fn right() -> (i32, i32, u32, u32) {
    (
        WIDTH,
        0,
        u32::try_from(WIDTH).unwrap(),
        u32::try_from(HEIGHT).unwrap(),
    )
}

/// Where every sheet puts itself: on the display the person is working on.
#[test]
fn a_sheet_opens_on_the_display_in_focus() {
    let _settled = ui::motion::testing::settled();
    let mut session = docked();
    let mut client = client(&mut session);
    assert_eq!(displays(&session).len(), 2, "the desk tells both apart");
    focuses(&mut session, &mut client, 1);

    let before_left = shot(&mut session, "HEADLESS-1");
    let before_right = shot(&mut session, "HEADLESS-2");

    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    client.pump(&mut session, |session, _| {
        let presentation = session.power_menu().expect("power sheet").presentation();
        presentation.place() == Some(right()) && presentation.is_open()
    });
    let power = session.power_menu().expect("power sheet");
    assert_eq!(
        power.presentation().place(),
        Some(right()),
        "the sheet covers the display in focus"
    );
    assert!(power.presentation().is_open(), "and is up for the person");
    let (cx, cy) = panel_centre(&mut session);

    let _after_right = crate::common::wait_for(
        &mut session,
        format!("the sheet drawn on the display in focus, at ({cx}, {cy}) of it"),
        |session| {
            session.step(Duration::from_millis(16)).expect("step");
            let frame = shot(session, "HEADLESS-2");
            (pixel(&frame, cx, cy) != pixel(&before_right, cx, cy)).then_some(frame)
        },
    );
    let after_left = shot(&mut session, "HEADLESS-1");
    assert_eq!(
        pixel(&after_left, cx, cy),
        pixel(&before_left, cx, cy),
        "and nothing of it reaches the other display"
    );
}

/// A window maximised over the whole display stays under the sheet, and the sheet, not the window,
/// gets the person's keys while it is up.
#[test]
fn a_window_filling_the_display_keeps_the_sheet_on_top_and_takes_no_keys() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_at(&[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)]);
    let mut client = client(&mut session);
    let editor = map(
        &mut session,
        &mut client,
        "org.example.Editor",
        (300, 200),
        |_| {},
    );
    client.toplevel(editor).set_maximized();

    // The compositor hands the client the size it fills, which the client then draws at: nothing of
    // the display shows around it.
    let before = client.seen.toplevels[editor].size;
    client.pump(&mut session, |_, seen| seen.toplevel(editor).size != before);
    if let Some((w, h)) = client.seen.toplevels[editor].size
        && w > 0
        && h > 0
    {
        client.show(editor, w, h, WINDOW_BYTES);
    }
    client.pump(&mut session, |session, _| {
        let (x, y, w, h) = rect(session, "org.example.Editor");
        x == 0 && w == WIDTH && y + h == HEIGHT
    });
    client.pump(&mut session, |_, seen| seen.keyboard_focused);

    // The window has the keyboard to itself before the sheet comes up.
    client.tap(&mut session, KEY_A);
    client.pump(&mut session, |_, seen| seen.keys.contains(&(KEY_A, true)));
    assert!(
        !client.seen.keys.iter().any(|(key, _)| *key == KEY_ESCAPE),
        "no key of the sheet's reached the window yet: {:?}",
        client.seen.keys
    );

    let before = shot(&mut session, "HEADLESS-1");
    let power = session.power_menu().expect("power sheet");
    power.presentation().open(false);
    client.pump(&mut session, |session, _| {
        session.power_menu().expect("power sheet").panel().x1 > 0.0
    });
    let (cx, cy) = panel_centre(&mut session);
    let (wx, wy, ww, wh) = rect(&mut session, "org.example.Editor");
    assert!(
        cx >= wx && cx < wx + ww && cy >= wy && cy < wy + wh,
        "the menu's middle ({cx}, {cy}) is over the window {:?}",
        (wx, wy, ww, wh)
    );
    assert_eq!(
        pixel(&before, cx, cy),
        WINDOW,
        "the window filled the display at ({cx}, {cy}) before the sheet"
    );
    crate::common::wait_for(
        &mut session,
        "the sheet painting over the maximised window",
        |session| {
            session.step(Duration::from_millis(16)).expect("step");
            let frame = shot(session, "HEADLESS-1");
            (pixel(&frame, cx, cy) != WINDOW).then_some(())
        },
    );
    let (x, y, w, h) = rect(&mut session, "org.example.Editor");
    assert_eq!(
        (x, w, y + h),
        (0, WIDTH, HEIGHT),
        "and the window still fills the display"
    );

    // Escape is the sheet's while it is up: the window sees nothing of it.
    let seen_before = client.seen.keys.clone();
    client.tap(&mut session, KEY_ESCAPE);
    client.pump(&mut session, |session, _| {
        !session
            .power_menu()
            .expect("power sheet")
            .presentation()
            .is_open()
    });
    assert_eq!(
        client.seen.keys, seen_before,
        "Escape closed the sheet and never reached the window"
    );

    // The window is still the client with focus, and takes keys again once the sheet is gone.
    assert!(client.seen.keyboard_focused, "the window kept the seat");
    client.tap(&mut session, KEY_A);
    client.pump(&mut session, |_, seen| {
        seen.keys
            .iter()
            .filter(|(key, pressed)| *key == KEY_A && *pressed)
            .count()
            >= 2
    });
}
