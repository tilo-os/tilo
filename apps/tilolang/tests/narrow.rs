//! A narrow window shows one column at a time: the note a scenario opens is in front at rest, a
//! row a person opens is pushed in, and the push settles within its motion and leaves the
//! window asking for no frames.

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;
use ui::headless::{FrameHarness, FrameSchedule};
use ui::motion::{Direction, Drill};
use ui::time::testing::with_monotonic_time;
use ui::time::{Instant, now};
use ui::tokens::MotionSource;

const NARROW: (u32, u32) = (420, 860);

/// Notes at a phone's width with the week of notes staged, as `--shots` stages it: the scenario
/// opens its first note.
fn staged() -> TestApp<TlApp> {
    crate::support::installed("notes");
    TestApp::<TlApp>::capture_at(Some("a week of notes"), NARROW).expect("the scenario stages")
}

/// Whether the live window would take a frame: for an animation, queued work (a component's
/// wake, an invocation, a requested layout) or a wake deadline that is due. A leaf's sticky
/// `needs_redraw` is no reason (a text input is never clean), as in the live window's pump.
fn demands_frame(schedule: &FrameSchedule) -> bool {
    schedule.needs_animation_frame
        || schedule.queued_work
        || schedule
            .next_wake_deadline
            .is_some_and(|deadline| deadline <= now())
}

/// What asks for frames, for a failure message.
fn asking(harness: &FrameHarness) -> String {
    format!(
        "{:?}, asked by {:?}",
        harness.frame_schedule(),
        harness.frame_requesters()
    )
}

/// The instant `after` from now.
fn later(after: ui::time::Duration) -> Instant {
    now().checked_add(after).expect("the clock reaches it")
}

/// Frames as the live window's clock takes them while `until` has not passed.
fn frames_until(app: &mut TestApp<TlApp>, until: Instant) -> usize {
    let mut frames: usize = 0;
    while now() < until {
        if demands_frame(&app.harness().frame_schedule()) && app.frame() {
            frames = frames.saturating_add(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    frames
}

/// Take the frames the window asks for until `within` has passed, then the one after: the
/// window asks for no frame now, and none for a while after. Idle is zero frames.
fn assert_settles(app: &mut TestApp<TlApp>, within: ui::time::Duration, what: &str) {
    frames_until(app, later(within));
    app.frame();
    assert!(
        !demands_frame(&app.harness().frame_schedule()),
        "{what}: the window still asks for frames after {within:?}: {}",
        asking(app.harness())
    );
    let idle = frames_until(app, later(ui::time::Duration::from_millis(100)));
    assert_eq!(
        idle,
        0,
        "{what}: an idle window took frames: {}",
        asking(app.harness())
    );
}

/// The test id of the first control whose accessible name contains `name`.
fn control_named(harness: &FrameHarness, name: &str) -> String {
    fn find(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
        let named = node
            .semantics
            .as_ref()
            .and_then(|access| access.name.as_deref())
            .is_some_and(|shown| shown.contains(name));
        if named && let Some(id) = &node.test_id {
            return Some(id.clone());
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    find(&harness.inspection().root, name).unwrap_or_else(|| panic!("no control named {name}"))
}

/// How long a push from rest takes to settle at the window's motion setting.
fn push_motion(harness: &FrameHarness) -> ui::time::Duration {
    Drill::SPEC.settle_time(
        Direction::Enter,
        harness.theme().motion(),
        MotionSource::Pointer,
    )
}

#[test]
fn the_note_a_scenario_opens_in_a_narrow_window_is_in_front_at_rest() {
    // Staged at one instant, as fast as a machine could stage it: the frame the capture takes
    // is the one staging laid out, however little time passed since the note was opened.
    let at = now();
    let mut app = with_monotonic_time(at, staged);
    with_monotonic_time(at, || {
        assert!(
            !app.harness().frame_schedule().needs_animation_frame,
            "the staged note is still moving: {}",
            asking(app.harness())
        );
    });
    let targets = app.harness().targets();
    assert!(
        targets.contains_key("nav-shell-back"),
        "the note is in front, with Back to the list"
    );
    if let Some(list) = targets.get("tilolang.list") {
        assert!(
            list.x1 <= 0.0,
            "no sliver of the list shows beside the note: {list:?}"
        );
    }
    let motion = push_motion(app.harness());
    assert_settles(&mut app, motion, "the staged note");
}

#[test]
fn a_push_in_a_narrow_window_settles_within_its_motion_and_then_asks_for_no_frames() {
    let mut app = staged();
    let motion = push_motion(app.harness());
    assert!(!motion.is_zero(), "a push moves at this motion setting");
    assert_settles(&mut app, motion, "the staged note");
    // Back to the list, which settles too.
    app.harness()
        .click_named("nav-shell-back")
        .expect("Back is shown");
    assert_settles(&mut app, motion, "back at the list");
    // A person opens another note: one click on its row pushes it in, as selecting an entry of
    // an overview does at this width.
    let row = control_named(app.harness(), "Groceries");
    app.harness().click_named(&row).expect("the row is shown");
    assert!(
        app.harness().frame_schedule().needs_animation_frame,
        "one click on the row slides the note in: {}",
        asking(app.harness())
    );
    assert_settles(&mut app, motion, "the pushed note");
    assert!(
        app.harness().targets().contains_key("nav-shell-back"),
        "the opened note is in front"
    );
}

/// Notes back at its list, settled, with the scenario's note open behind it.
fn at_the_list() -> TestApp<TlApp> {
    let mut app = staged();
    let motion = push_motion(app.harness());
    assert_settles(&mut app, motion, "the staged note");
    app.harness().key("Escape");
    assert_settles(&mut app, motion, "back at the list");
    assert!(
        !app.harness().targets().contains_key("nav-shell-back"),
        "the list is in front"
    );
    app
}

/// The accessibility node of the list row whose name contains `name`.
fn row_node(harness: &FrameHarness, name: &str) -> ui::accesskit::NodeId {
    harness
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::ListItem
                && node.label().is_some_and(|label| label.contains(name))
        })
        .map_or_else(|| panic!("no row named {name}"), |(id, _)| id)
}

#[test]
fn moving_through_the_list_with_the_keyboard_selects_and_enter_opens_at_once() {
    let mut app = at_the_list();
    let groceries = row_node(app.harness(), "Groceries");
    app.harness()
        .perform_a11y_action(groceries, &ui::a11y::Action::Focus)
        .expect("the row takes focus");
    app.harness().key("ArrowDown");
    app.frame();
    assert!(
        !app.harness().targets().contains_key("nav-shell-back"),
        "moving through the list keeps it in front"
    );
    app.harness().key("Enter");
    app.frame();
    assert!(
        app.harness().targets().contains_key("nav-shell-back"),
        "Enter opens the note in front"
    );
    assert!(
        !app.harness().frame_schedule().needs_animation_frame,
        "a key lands the push at once: {}",
        asking(app.harness())
    );
}

#[test]
fn an_open_from_search_or_an_agent_lands_in_front_at_once() {
    let mut app = at_the_list();
    let anchor = tilolang_app::resource::search_entries()
        .into_iter()
        .find(|entry| entry.title.contains("Groceries"))
        .and_then(|entry| entry.anchor)
        .expect("search finds the note");
    app.call("open", serde_json::json!({ "anchor": anchor }))
        .expect("the window opens the hit");
    app.frame();
    assert!(
        app.harness().targets().contains_key("nav-shell-back"),
        "the hit's note is in front"
    );
    assert!(
        !app.harness().frame_schedule().needs_animation_frame,
        "nobody's hand moved it, so it lands at once: {}",
        asking(app.harness())
    );
}
