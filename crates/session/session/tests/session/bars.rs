//! A bar on every display: the desk places one per display, the display in use full and the others
//! compact, follows the displays as they come and go, and takes a bar off its display for a
//! fullscreen window there, back over that window when the person reaches for it.

use super::*;

use tilo_compositor::{InputEvent, OutputChange};
use tilo_desktop::chrome::TopBarPresence;
use tilo_session::{BarShown, Session};

/// Two displays side by side, the built-in one first: the arrangement a session with a monitor
/// plugged in on the right starts in.
fn two_displays() -> Session {
    let external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT).at(WIDTH, 0);
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0), external],
    )
    .expect("session")
}

/// Where each bar stands: the display it stands on, its ends across the desk, whether it is the
/// full bar, and whether it left the desk for a window over it.
fn bars_by_display(session: &Session) -> Vec<(Option<u32>, f64, f64, bool, bool)> {
    session
        .bars()
        .into_iter()
        .map(|bar| {
            (
                bar.display,
                bar.rect.x0,
                bar.rect.x1,
                bar.full,
                bar.presence != TopBarPresence::Docked,
            )
        })
        .collect()
}

/// Two displays carry two bars, each on its own display: the one in use full, the other compact.
#[test]
fn every_display_carries_a_bar_of_its_own() {
    let _settled = ui::motion::testing::settled();
    let mut session = two_displays();
    settle(&mut session, 4);
    let in_use = session
        .handle()
        .focused_display()
        .expect("a display in use of two");
    let other = *session
        .handle()
        .displays()
        .iter()
        .find(|display| **display != in_use)
        .expect("a second display");

    let bars = session.bars();
    assert_eq!(bars.len(), 2, "one bar per display: {bars:?}");
    let full: &BarShown = bars.iter().find(|bar| bar.full).expect("a full bar");
    let compact: &BarShown = bars.iter().find(|bar| !bar.full).expect("a compact bar");
    assert_eq!(
        full.display, None,
        "the full bar stands on the display in use"
    );
    assert_eq!(compact.display, Some(other));
    assert_eq!(
        (full.presence, compact.presence),
        (TopBarPresence::Docked, TopBarPresence::Docked),
        "both bars are docked"
    );
    let height = full.rect.height();
    assert!(height > 0.0, "the bar has a height: {full:?}");
    assert!(
        (compact.rect.height() - height).abs() < f64::EPSILON,
        "both are the bar's height: {compact:?}"
    );
    // Each on its own display's rectangle, its top edge across it.
    assert_eq!(
        (full.rect.x0, full.rect.x1, full.rect.y0),
        (0.0, f64::from(WIDTH), 0.0),
        "the full bar crosses the display in use"
    );
    assert_eq!(
        (compact.rect.x0, compact.rect.x1, compact.rect.y0),
        (f64::from(WIDTH), f64::from(2 * WIDTH), 0.0),
        "the compact bar is where its display is"
    );

    // What the bar is told, in the desk's own terms.
    let placement = session.bar_placement();
    assert_eq!(placement.bars.len(), 2);
    assert_eq!(
        placement
            .bars
            .iter()
            .find(|place| place.full)
            .map(|place| place.display),
        Some(in_use),
        "the display in use carries the full bar"
    );
    assert!(placement.fullscreen.is_empty());
}

/// A bar stands where its display is, so a display arriving or going takes the bars with it on the
/// next pass.
#[test]
fn a_display_arriving_or_going_changes_the_bars() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0)],
    )
    .expect("session");
    settle(&mut session, 4);
    assert_eq!(
        bars_by_display(&session),
        vec![(None, 0.0, f64::from(WIDTH), true, false)],
        "one display carries the full bar alone"
    );

    // A monitor is plugged in on the right.
    let external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT).at(WIDTH, 0);
    session
        .compositor()
        .add_output(&external)
        .expect("the output is added");
    settle(&mut session, 4);
    assert_eq!(
        bars_by_display(&session),
        vec![
            (None, 0.0, f64::from(WIDTH), true, false),
            (
                Some(1),
                f64::from(WIDTH),
                f64::from(2 * WIDTH),
                false,
                false
            ),
        ],
        "the display that arrived carries a bar of its own"
    );

    // And unplugged again.
    session
        .compositor()
        .configure_outputs(&[OutputChange::disable("HDMI-A-1")])
        .expect("the display is switched off");
    settle(&mut session, 4);
    assert_eq!(
        bars_by_display(&session),
        vec![(None, 0.0, f64::from(WIDTH), true, false)],
        "with one display left the full bar stands across it"
    );
}

/// Ask for the whole display (`on`) or give it back, as a client's own toplevel says.
fn set_fullscreen(client: &mut TestClient, on: bool) {
    if let Some(toplevel) = &client.toplevel {
        if on {
            toplevel.set_fullscreen(None);
        } else {
            toplevel.unset_fullscreen();
        }
    }
    if let Some(surface) = &client.surface {
        surface.commit();
    }
    client.exchange();
}

/// A fullscreen window on the second display owns it: that display's compact bar leaves the desk,
/// and the bar of the display the person works on stays.
#[test]
fn a_fullscreen_window_on_a_display_takes_that_displays_bar_away() {
    let _settled = ui::motion::testing::settled();
    let mut session = two_displays();
    settle(&mut session, 4);
    let other = session.handle().displays()[1];
    let in_use = session.handle().displays()[0];
    assert!(
        session.handle().focus_display(other),
        "the person moves to the second display"
    );
    settle(&mut session, 2);

    // A window opens where the person is working, on that display.
    let mut client = mapped(&mut session, "org.tilo.Bars");
    let (_, rect) = window(session.compositor(), "org.tilo.Bars").expect("the window is shown");
    assert!(
        rect.0 >= WIDTH,
        "the window stands on the second display: {rect:?}"
    );

    // The person goes back to the first display: the second one carries the compact bar the window
    // will take.
    assert!(session.handle().focus_display(in_use), "back to the panel");
    settle(&mut session, 2);
    assert_eq!(
        bars_by_display(&session),
        vec![
            (None, 0.0, f64::from(WIDTH), true, false),
            (
                Some(other),
                f64::from(WIDTH),
                f64::from(2 * WIDTH),
                false,
                false
            ),
        ],
        "the compact bar stands on the second display"
    );

    set_fullscreen(&mut client, true);
    until(&mut session, &mut client, |session| {
        (session.bar_placement().fullscreen.contains(&Some(other))
            && session
                .bars()
                .iter()
                .any(|bar| bar.presence != TopBarPresence::Docked))
        .then_some(())
    })
    .expect("the compact bar leaves the fullscreen window's display");
    assert_eq!(
        bars_by_display(&session),
        vec![
            (None, 0.0, f64::from(WIDTH), true, false),
            (
                Some(other),
                f64::from(WIDTH),
                f64::from(2 * WIDTH),
                false,
                true
            ),
        ],
        "the display in use keeps its bar; the fullscreen window's display does not"
    );

    // The window gives the display back: its bar returns.
    set_fullscreen(&mut client, false);
    until(&mut session, &mut client, |session| {
        session
            .bars()
            .iter()
            .all(|bar| bar.presence == TopBarPresence::Docked)
            .then_some(())
    })
    .expect("the compact bar returns");
    assert!(session.bar_placement().fullscreen.is_empty());
}

/// One display, as a laptop on its own starts.
fn one_display() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0)],
    )
    .expect("session")
}

/// What a mouse's left button and the Super key send.
const BTN_LEFT: u32 = 0x110;
const KEY_LEFT_META: u32 = 125;

fn motion(session: &mut Session, x: f64, y: f64) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("pointer motion");
}

fn press(session: &mut Session, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed,
        })
        .expect("pointer button");
}

fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(InputEvent::Key { keycode, pressed })
        .expect("key");
}

/// The rectangle the window of `app` stands in.
fn rect_of(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    window(session.compositor(), app)
        .map(|(_, rect)| rect)
        .expect("the window is shown")
}

/// What the person sees on the display at `y` down from its top edge, in the middle of it.
fn pixel_at(session: &mut Session, y: i32) -> [u8; 4] {
    let frame = session
        .compositor()
        .screenshot("eDP-1")
        .expect("the display is shown");
    let x = u32::try_from(WIDTH / 2).expect("a display wide enough");
    frame
        .pixel(x, u32::try_from(y).expect("a point on the display"))
        .expect("the point is on the display")
}

/// A line of the window well below the bar's band, where the bar never reaches.
const WINDOW_LINE: i32 = 200;

/// Whether two pixels are the same colour to the eye: one composited on a card is rounded by a unit
/// or two off another.
fn same_colour(one: [u8; 4], other: [u8; 4]) -> bool {
    one.iter()
        .zip(other)
        .all(|(one, other)| one.abs_diff(other) <= 4)
}

/// A window of `app` filling the display, and the rect it then stands in: the person took it full
/// screen and the bar has gone again.
fn covering_window(
    session: &mut Session,
    client: &mut TestClient,
    app: &str,
) -> (i32, i32, i32, i32) {
    set_fullscreen(client, true);
    take_size(session, client, app, (WIDTH, HEIGHT));
    let whole = rect_of(session, app);
    until(session, client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Hidden).then_some(())
    })
    .expect("with nothing holding it, the bar goes");
    whole
}

/// Taken full screen, the window takes the whole display and the bar's room with it; entering shows
/// the bar over the window for its idle time, and then it goes again.
#[test]
fn a_window_covering_its_display_takes_the_bars_room() {
    let _settled = ui::motion::testing::settled();
    let app = "org.tilo.Fullscreen";
    let mut session = one_display();
    settle(&mut session, 4);
    let mut client = mapped(&mut session, app);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });

    // Docked, the bar takes the strip above the window.
    let docked = rect_of(&mut session, app);
    let height = session.bars()[0].rect.height();
    assert!(height > 0.0, "the docked bar has a height");
    assert!(
        f64::from(docked.1) >= height,
        "the window stands below the docked bar: {docked:?}"
    );
    assert!(
        !same_colour(
            pixel_at(&mut session, 12),
            pixel_at(&mut session, WINDOW_LINE)
        ),
        "the bar is up there"
    );

    // Taken full screen, the window fills its display: the bar takes no room.
    set_fullscreen(&mut client, true);
    take_size(&mut session, &mut client, app, (WIDTH, HEIGHT));
    let whole = rect_of(&mut session, app);
    assert_eq!(
        whole,
        (0, 0, WIDTH, HEIGHT),
        "the window covers the display, top edge to bottom"
    );

    // Entering shows where the bar went, painted over the window.
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Shown).then_some(())
    })
    .expect("entering full screen shows the bar");
    let bar = session.bars()[0];
    assert!(
        bar.rect.height() > 0.0 && (bar.rect.y0 - 0.0).abs() < f64::EPSILON,
        "the bar shows in its docked place: {bar:?}"
    );
    assert!(
        !same_colour(
            pixel_at(&mut session, 12),
            pixel_at(&mut session, WINDOW_LINE)
        ),
        "the bar paints over the window"
    );
    assert_eq!(
        rect_of(&mut session, app),
        whole,
        "the bar paints over the window; the window keeps its whole size"
    );

    // Nothing holds it, so it goes on its own.
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Hidden).then_some(())
    })
    .expect("with nothing holding it, it goes");
    let (top, below) = (
        pixel_at(&mut session, 12),
        pixel_at(&mut session, WINDOW_LINE),
    );
    assert!(
        same_colour(top, below),
        "hidden, the bar paints nothing: the window shows through ({top:?} over {below:?})"
    );
    assert_eq!(rect_of(&mut session, app), whole);
}

/// Over a window covering its display, the bar stays away until it is reached for: the top edge, a
/// key, and leaving full screen again, with a press in the band staying the window's while the bar
/// is away.
#[test]
fn the_bar_comes_back_over_a_window_covering_its_display() {
    let _settled = ui::motion::testing::settled();
    let app = "org.tilo.Fullscreen";
    let mut session = one_display();
    settle(&mut session, 4);
    let mut client = mapped(&mut session, app);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });
    let height = session.bars()[0].rect.height();
    let whole = covering_window(&mut session, &mut client, app);

    // A press in the top band while it is away is the window's, and does not bring it back.
    motion(&mut session, f64::from(WIDTH) / 2.0, 1.0);
    settle(&mut session, 2);
    client.seen.buttons.clear();
    press(&mut session, true);
    press(&mut session, false);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.buttons.contains(&(BTN_LEFT, true))
    });
    assert_eq!(
        session.bars()[0].presence,
        TopBarPresence::Hidden,
        "the window got the press; the bar stayed away"
    );
    assert_eq!(rect_of(&mut session, app), whole);

    // The person pushes the pointer to the top edge: the bar comes back over the window.
    for y in [200.0, 60.0, 12.0, 0.0] {
        motion(&mut session, f64::from(WIDTH) / 2.0, y);
        settle(&mut session, 2);
    }
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Shown).then_some(())
    })
    .expect("the top edge brings the bar back");
    assert_eq!(
        rect_of(&mut session, app),
        whole,
        "the bar shows over the window; the window does not move"
    );

    // Down into the window takes it away again.
    for y in [60.0, 200.0, 400.0] {
        motion(&mut session, f64::from(WIDTH) / 2.0, y);
        settle(&mut session, 2);
    }
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Hidden).then_some(())
    })
    .expect("leaving it for the window hides it");
    assert_eq!(rect_of(&mut session, app), whole);

    // Super asks for it too, and the window under it never sees the key.
    client.seen.keys.clear();
    key(&mut session, KEY_LEFT_META, true);
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Shown).then_some(())
    })
    .expect("Super shows the bar");
    assert!(
        !client
            .seen
            .keys
            .iter()
            .any(|(code, _)| *code == KEY_LEFT_META),
        "the window never sees Super: {:?}",
        client.seen.keys
    );
    key(&mut session, KEY_LEFT_META, false);

    // Shown over the window, the bar takes the pointer there: a press on it is the bar's, and the
    // window under it never sees it.
    motion(&mut session, f64::from(WIDTH) / 2.0, 12.0);
    settle(&mut session, 2);
    client.seen.buttons.clear();
    press(&mut session, true);
    press(&mut session, false);
    settle(&mut session, 6);
    assert!(
        client.seen.buttons.is_empty(),
        "the bar over the window took the press: {:?}",
        client.seen.buttons
    );
    assert_eq!(
        session.bars()[0].presence,
        TopBarPresence::Shown,
        "the press on the bar did not send it away"
    );
    assert_eq!(rect_of(&mut session, app), whole);

    // Leaving full screen docks the bar again and forgets the reveal.
    set_fullscreen(&mut client, false);
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Docked).then_some(())
    })
    .expect("leaving full screen docks the bar");
    assert!(
        f64::from(rect_of(&mut session, app).1) >= height,
        "the window stands below the bar again"
    );
    assert!(
        !same_colour(
            pixel_at(&mut session, 12),
            pixel_at(&mut session, WINDOW_LINE)
        ),
        "and the bar is up there"
    );
    assert!(
        session.bar_placement().fullscreen.is_empty(),
        "no display is covered any more"
    );
}

/// The person's own "hide the top bar", with no fullscreen window anywhere: the bar leaves the
/// desk, the windows take its strip, and turning the setting off docks it again.
#[test]
fn hiding_the_top_bar_in_settings_gives_its_strip_to_the_windows() {
    let _settled = ui::motion::testing::settled();
    let app = "org.tilo.Hidden";
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let mut session = one_display();
    settle(&mut session, 4);
    let mut client = mapped(&mut session, app);
    pump(&mut session, &mut [&mut client], |_, clients| {
        clients[0].seen.keyboard_focused
    });
    let docked = rect_of(&mut session, app);
    let height = session.bars()[0].rect.height();
    assert!(height > 0.0, "the docked bar has a height");
    assert!(
        f64::from(docked.1) >= height,
        "the window stands below it: {docked:?}"
    );

    let hide = |on| {
        let mut section = serde_json::Map::new();
        section.insert("hide_top_bar".to_owned(), serde_json::Value::Bool(on));
        settings_store::update_section(
            settings_store::desktop_controls::DESKTOP_CONTROLS_DEVICE_SECTION,
            section,
        )
        .expect("the person's settings are kept");
    };

    hide(true);
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Hidden).then_some(())
    })
    .expect("hiding the bar in the settings takes it off the desk");

    // The desk gives the window the strip the bar took: it grows by the bar's height, and nothing
    // else moves.
    let docked_size = (docked.2, docked.3);
    let mut taller = None;
    pump(&mut session, &mut [&mut client], |_, clients| {
        taller = clients[0].seen.size.filter(|size| *size != docked_size);
        taller.is_some()
    });
    let taller = taller.expect("the desk asks the window for the strip's size");
    assert_eq!(taller.0, docked.2, "the window keeps its width");
    client.show(taller.0, taller.1, [200, 40, 40, 255]);
    within(&mut session, &mut client, |session| {
        window(session.compositor(), app).is_some_and(|(_, rect)| (rect.2, rect.3) == taller)
    });
    let taken = rect_of(&mut session, app);
    assert_eq!(taken.0, docked.0, "the window keeps its left edge");
    assert_eq!(
        taken.1 + taken.3,
        docked.1 + docked.3,
        "and its bottom edge"
    );
    assert!(
        (f64::from(docked.1 - taken.1) - height).abs() < 0.5,
        "the window takes the bar's strip, no more: {docked:?} -> {taken:?}"
    );
    assert!(
        session.bar_placement().fullscreen.is_empty(),
        "no window covers a display: the person hid the bar"
    );

    hide(false);
    until(&mut session, &mut client, |session| {
        (session.bars().first()?.presence == TopBarPresence::Docked).then_some(())
    })
    .expect("turning the setting off docks the bar again");
    take_size(&mut session, &mut client, app, docked_size);
    assert_eq!(
        rect_of(&mut session, app),
        docked,
        "the window stands where it stood"
    );
    assert!(
        !same_colour(
            pixel_at(&mut session, 12),
            pixel_at(&mut session, WINDOW_LINE)
        ),
        "and the bar is up there again"
    );
}
