//! A narrow window shows one column at a time: the thing a scenario opens is in front at rest, a
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

/// A phone's own window, as a person holds it.
const PHONE: (u32, u32) = (390, 844);

/// Notes at a phone's width with the week of notes staged, as `--shots` stages it: the scenario
/// opens its first note. A capture is photographed at rest, so this window moves on no frame.
fn staged() -> TestApp<TlApp> {
    crate::support::installed("notes");
    TestApp::<TlApp>::capture_at(Some("a week of notes"), NARROW).expect("the scenario stages")
}

/// The same scene in a window a person drives: the same fixtures and the same note open, with the
/// theme's motion playing as it plays live.
fn driven() -> TestApp<TlApp> {
    crate::support::installed("notes");
    TestApp::<TlApp>::drive_at("a week of notes", NARROW).expect("the scenario stages")
}

/// Mail at a phone's width with its inbox staged, in a window a person drives: the scenario's
/// conversation opens beside the list, which at this width stands in front of it.
fn mail_driven() -> TestApp<TlApp> {
    crate::support::installed("mail");
    TestApp::<TlApp>::drive_at("a busy inbox", NARROW).expect("the scenario stages")
}

/// The same week of notes in a phone's own window, a person driving it.
fn phone_driven() -> TestApp<TlApp> {
    crate::support::installed("notes");
    TestApp::<TlApp>::drive_at("a week of notes", PHONE).expect("the scenario stages")
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

/// The frames a slide owes once its motion has landed: the one whose tick finds it ended
/// (a tick runs after the frame's paint), the one that lays the landed pose out, and the
/// structural one that drops the parts that pose hides.
const LANDING_FRAMES: usize = 3;

/// Take the frames the window asks for until `within` has passed, then the frames landing
/// owes: the window asks for no frame now, and none for a while after. Idle is zero frames.
fn assert_settles(app: &mut TestApp<TlApp>, within: ui::time::Duration, what: &str) {
    frames_until(app, later(within));
    for _ in 0..LANDING_FRAMES {
        if demands_frame(&app.harness().frame_schedule()) {
            app.frame();
        }
    }
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

/// The test id of the first control whose accessible name contains `name`: a note by its title,
/// which is content the person reads, never a control they look up to press.
fn named_text(harness: &FrameHarness, name: &str) -> Option<String> {
    let named = |node: &&ui::headless::InspectionNode| {
        node.semantics
            .as_ref()
            .and_then(|access| access.name.as_deref())
            .is_some_and(|shown| shown.contains(name))
    };
    (harness.inspection().root.nodes().into_iter())
        .filter(named)
        .find_map(|node| node.test_id.clone())
}

/// The test id of the first control that does `identity`: what the window says it does, wherever
/// the window's composition draws it and whatever the person's language calls it.
fn doing_control(harness: &FrameHarness, identity: &str) -> Option<String> {
    let does = |node: &&ui::headless::InspectionNode| {
        node.semantics
            .as_ref()
            .and_then(|access| access.action.as_deref())
            == Some(identity)
    };
    (harness.inspection().root.nodes().into_iter())
        .filter(does)
        .find_map(|node| node.test_id.clone())
}

/// What the mail conversation's Reply button does: it answers the thread.
fn answer() -> String {
    doing("mail", "Thread", "answer")
}

/// What the app `app`'s action `action` on its records of `ty` is known by: the record kind the
/// app publishes and the id its ids lock gives the declaration (`answer = a4rv5wm6`).
fn doing(app: &str, ty: &str, action: &str) -> String {
    let lock = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tl")
        .join(app)
        .join("app.ids");
    let ids = std::fs::read_to_string(&lock).expect("the app's ids lock");
    let Some((_, id)) = ids
        .lines()
        .filter_map(|line| line.split_once(" = "))
        .find(|(declared, _)| *declared == action)
    else {
        panic!("{} declares no {action}", lock.display());
    };
    let id = id.trim().to_owned();
    format!("tl.so.tilo.lang.{app}.{ty}\u{1f}{id}")
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
fn a_narrow_window_opens_on_its_tree_at_rest_and_asks_for_no_frames() {
    // Staged at one instant, as fast as a machine could stage it: the frame the capture takes
    // is the one staging laid out, however little time passed since the notebook opened.
    let at = now();
    let mut app = with_monotonic_time(at, staged);
    with_monotonic_time(at, || {
        assert!(
            !app.harness().frame_schedule().needs_animation_frame,
            "the staged window is still moving: {}",
            asking(app.harness())
        );
    });
    // The tree is the window's first screen, as a list of files is: a note is pushed in front
    // of it, with Back to the tree, and nothing is pushed at rest.
    let targets = app.harness().targets();
    assert!(
        targets.keys().any(|key| key.ends_with("/TreeView")),
        "the tree is in front"
    );
    assert!(
        !targets.contains_key("nav-shell-back"),
        "nothing is pushed over the tree at rest"
    );
}

#[test]
fn a_push_in_a_narrow_window_settles_within_its_motion_and_then_asks_for_no_frames() {
    // A conversation opens beside the list, which at this width stands in front of it.
    let mut app = mail_driven();
    let motion = push_motion(app.harness());
    assert!(!motion.is_zero(), "a push moves at this motion setting");
    assert_settles(&mut app, motion, "the staged conversation");
    app.harness()
        .click_named("nav-shell-back")
        .expect("Back is shown");
    assert_settles(&mut app, motion, "back at the list");
    assert!(
        doing_control(app.harness(), &answer()).is_none(),
        "the list stands in front"
    );
    // A person opens a conversation, which stands in front of the list.
    let row = row_node(app.harness(), "Mill Lane Lettings");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .expect("the row opens what it names");
    app.frame();
    assert!(
        !app.harness().frame_schedule().needs_animation_frame,
        "a row activated from the keyboard lands at once: {}",
        asking(app.harness())
    );
    assert_settles(&mut app, motion, "the opened conversation");
    assert!(
        doing_control(app.harness(), &answer()).is_some(),
        "the conversation stands in front of the list"
    );
    // And back once more, which settles too: the list stands in front again.
    app.harness()
        .click_named("nav-shell-back")
        .expect("Back is shown");
    assert_settles(&mut app, motion, "back at the list");
    assert!(
        doing_control(app.harness(), &answer()).is_none(),
        "the list is back in front"
    );
}

/// Notes back at its list, settled, with the scenario's note open behind it.
fn at_the_list() -> TestApp<TlApp> {
    let mut app = driven();
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

/// The accessibility node of the tree row whose name contains `name`.
fn tree_row_node(harness: &FrameHarness, name: &str) -> ui::accesskit::NodeId {
    harness
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::TreeItem
                && node.label().is_some_and(|label| label.contains(name))
        })
        .map_or_else(|| panic!("no tree row named {name}"), |(id, _)| id)
}

#[test]
fn a_row_of_the_tree_pushes_its_note_and_back_returns_to_the_tree() {
    let mut app = staged();
    // The window stands in the desktop's bar; its way back stands in its header row.
    app.harness().present_in_window_bar();
    app.harness().relayout();
    assert!(
        named_text(app.harness(), "Groceries").is_some(),
        "the tree is the window's first screen"
    );
    // The person opens a note of the tree, which comes forward in front of it.
    let row = tree_row_node(app.harness(), "Groceries");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .expect("the row opens what it names");
    app.harness().relayout();
    assert!(
        named_text(app.harness(), "Groceries").is_none(),
        "the tree went behind the note its row opened"
    );
    app.harness()
        .click_named(ui::widget::WINDOW_BACK)
        .expect("the row holds Back");
    app.harness().relayout();
    assert!(
        named_text(app.harness(), "Groceries").is_some(),
        "Back brought the tree back"
    );
}

#[test]
fn back_in_a_driven_phone_window_shows_the_tree_in_the_frame_it_takes() {
    let mut app = phone_driven();
    // The window stands in the desktop's bar; its way back stands in its header row.
    app.harness().present_in_window_bar();
    let motion = push_motion(app.harness());
    assert_settles(&mut app, motion, "the staged tree");
    let row = tree_row_node(app.harness(), "Groceries");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .expect("the row opens what it names");
    assert_settles(&mut app, motion, "the note the row opened");
    assert!(
        named_text(app.harness(), "Groceries").is_none(),
        "the tree stands behind the note's page"
    );
    app.harness()
        .click_named(ui::widget::WINDOW_BACK)
        .expect("the row holds Back");
    // The frame the press asks for shows the tree it returned to: the press's own frame is
    // never one that rested on what stood before it.
    app.frame();
    assert!(
        named_text(app.harness(), "Groceries").is_some(),
        "the frame after the press shows the tree: {}",
        asking(app.harness())
    );
    assert_settles(&mut app, motion, "the tree");
}

#[test]
fn moving_through_the_list_with_the_keyboard_selects_and_enter_opens_at_once() {
    let mut app = mail_driven();
    let motion = push_motion(app.harness());
    assert_settles(&mut app, motion, "the staged conversation");
    app.harness()
        .click_named("nav-shell-back")
        .expect("Back is shown");
    assert_settles(&mut app, motion, "back at the list");
    let row = row_node(app.harness(), "Mill Lane Lettings");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Focus)
        .expect("the row takes focus");
    app.harness().key("ArrowDown");
    app.frame();
    assert!(
        doing_control(app.harness(), &answer()).is_none(),
        "moving through the list keeps it in front"
    );
    app.harness().key("Enter");
    app.frame();
    assert!(
        doing_control(app.harness(), &answer()).is_some(),
        "Enter opens the conversation in front: {:?}",
        {
            fn actions(
                node: &ui::headless::InspectionNode,
                out: &mut Vec<(Option<String>, Option<String>)>,
            ) {
                if let Some(access) = &node.semantics {
                    out.push((access.action.clone(), access.name.clone()));
                }
                for child in &node.children {
                    actions(child, out);
                }
            }
            let mut out = Vec::new();
            actions(&app.harness().inspection().root, &mut out);
            out.into_iter()
                .filter(|(doing, _)| doing.is_some())
                .collect::<Vec<_>>()
        }
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
