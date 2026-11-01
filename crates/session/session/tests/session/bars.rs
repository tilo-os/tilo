//! A bar on every display: the desk places one per display, the display in
//! use full and the others compact, follows the displays as they come and
//! go, and steps a compact bar aside for a fullscreen window on its
//! display.

use super::*;

use tilo_compositor::OutputChange;
use tilo_session::{BarShown, Session};

/// Two displays side by side, the built-in one first: the arrangement a
/// session with a monitor plugged in on the right starts in.
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

/// Where each bar stands: the display it stands on, its ends across the
/// desk, whether it is the full bar and whether it stepped aside.
fn bars_by_display(session: &Session) -> Vec<(Option<u32>, f64, f64, bool, bool)> {
    session
        .bars()
        .into_iter()
        .map(|bar| (bar.display, bar.rect.x0, bar.rect.x1, bar.full, bar.aside))
        .collect()
}

/// Two displays carry two bars, each on its own display: the one in use
/// full, the other compact.
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
    assert!(!full.aside && !compact.aside, "nothing steps aside yet");
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

/// A bar stands where its display is, so a display arriving or going takes
/// the bars with it on the next pass.
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

/// Ask for the whole display (`on`) or give it back, as a client's own
/// toplevel says.
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

/// A fullscreen window on the second display owns it: that display's
/// compact bar steps aside, and the bar of the display the person works
/// on stays.
#[test]
fn a_fullscreen_window_on_a_display_steps_that_displays_bar_aside() {
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

    // The person goes back to the first display: the second one carries
    // the compact bar the window will take.
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
        (session.bar_placement().fullscreen.contains(&other)
            && session.bars().iter().any(|bar| bar.aside))
        .then_some(())
    })
    .expect("the compact bar steps aside for the fullscreen window");
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
        session.bars().iter().all(|bar| !bar.aside).then_some(())
    })
    .expect("the compact bar returns");
    assert!(session.bar_placement().fullscreen.is_empty());
}
