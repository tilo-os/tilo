//! What a person sees while a view's layout changes: parts that stay slide to their new places,
//! parts that arrive fade in and parts that leave fade out where they stood, at the pace the
//! container's `motion:` names. A change no hand made, reduced motion and `motion: .still` land at
//! once.

use super::*;
use tilo_render_protocol::patch::{Patch, local};
use ui::time::{Duration, Instant};
use ui::tokens::MotionSource;
use ui::widget::arrange::{Arranged, DrawnPart, FlexLayout};

fn text(key: &str) -> UiNode {
    let mut node = UiNode::empty("ui.Text", key);
    node.props.insert("value".to_owned(), Value::text(key));
    node
}

/// A page holding a stack keyed `list` of a text per key, moving as `motion` names.
fn page(keys: &[&str], motion: Option<&str>) -> Vec<ToUi> {
    let mut list = UiNode::empty("stack", "list");
    if let Some(motion) = motion {
        list.props.insert("motion".to_owned(), Value::text(motion));
    }
    list.children = keys.iter().map(|key| text(key)).collect();
    let mut page = UiNode::empty("stack", "page");
    page.children.push(list);
    vec![ToUi::Open {
        level: Level::Root,
        target: ResourceRef {
            id: "doc".to_owned(),
            ty: "notes.Note".to_owned(),
        },
        tree: page,
        front: None,
    }]
}

fn send(app: &mut local::End, batch: &[ToUi]) {
    app.send(wire::encode_batch(batch).expect("encodes"))
        .expect("sends");
}

/// A window showing `batch`, laid out and drawn once, the person's pointer having just moved: the
/// first layout never moves, whoever made it.
fn shown(batch: &[ToUi]) -> (local::End, ui::headless::FrameHarness) {
    let (mut app, end) = local::pair();
    let mut window = AppWindow::new(Box::new(end), Vec::new());
    send(&mut app, batch);
    ui::motion::note_source(MotionSource::Pointer);
    window.receive();
    let mut harness =
        ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (600, 800));
    harness.relayout();
    (app, harness)
}

/// The app changes the list by `patches`, `source` having just acted: the instant the frame that
/// lays the change out starts it at.
fn change(
    app: &mut local::End,
    harness: &mut ui::headless::FrameHarness,
    source: MotionSource,
    patches: Vec<Patch>,
) -> Instant {
    let _frame = ui::motion::testing::frozen();
    ui::motion::note_source(source);
    send(
        app,
        &[ToUi::Patch {
            level: Level::Root,
            patches,
        }],
    );
    harness
        .root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AppWindow>())
        .expect("the window")
        .receive();
    harness.relayout();
    ui::motion::frame_now()
}

/// The list's parts as drawn at `at`, in order, then those fading out.
fn drawn(harness: &ui::headless::FrameHarness, at: Instant) -> Vec<DrawnPart> {
    ui::widget::find_test_id(harness.root_widget(), "list")
        .and_then(|list| list.as_any())
        .and_then(|any| any.downcast_ref::<Arranged<FlexLayout>>())
        .expect("the list is a stack")
        .drawn(at)
}

fn at_rest(parts: &[DrawnPart]) -> bool {
    parts
        .iter()
        .all(|part| !part.leaving && part.opacity >= 1.0)
}

fn remove(key: &str) -> Patch {
    Patch::Remove {
        parent: vec!["list".to_owned()],
        key: key.to_owned(),
    }
}

fn insert(index: u32, key: &str) -> Patch {
    Patch::Insert {
        parent: vec!["list".to_owned()],
        index,
        node: text(key),
    }
}

fn shift(key: &str, to: u32) -> Patch {
    Patch::Move {
        parent: vec!["list".to_owned()],
        key: key.to_owned(),
        to,
    }
}

const MIDWAY: Duration = Duration::from_millis(30);
const LATER: Duration = Duration::from_secs(2);

/// `b` taken out, `n` put first and `c` moved above `a`: `[a, b, c]` becomes `[n, c, a]`.
fn reorder() -> Vec<Patch> {
    vec![remove("b"), insert(0, "n"), shift("c", 1)]
}

#[test]
fn a_change_by_hand_slides_what_stays_fades_in_what_arrives_and_fades_out_what_leaves() {
    let (mut app, mut harness) = shown(&page(&["a", "b", "c"], None));
    let now = ui::time::now();
    let before = drawn(&harness, now);
    assert!(
        at_rest(&before),
        "the first layout stands still: {before:?}"
    );
    let [a_was, b_was, c_was] = [before[0].rect, before[1].rect, before[2].rect];

    let started = change(&mut app, &mut harness, MotionSource::Pointer, reorder());
    let rest = drawn(&harness, started + LATER);
    assert!(at_rest(&rest), "the change lands: {rest:?}");
    assert_eq!(rest.len(), 3, "what left is gone once faded: {rest:?}");
    let [n_at, c_at, a_at] = [rest[0].rect, rest[1].rect, rest[2].rect];
    assert!(
        c_at.y0 < c_was.y0 && a_at.y0 > a_was.y0,
        "c rose and a sank"
    );

    let midway = drawn(&harness, started + MIDWAY);
    let [n, c, a] = [midway[0], midway[1], midway[2]];
    assert!(
        n.opacity > 0.0 && n.opacity < 1.0 && n.rect == n_at,
        "n fades in where it is laid out: {n:?}"
    );
    assert!(
        c.rect.y0 > c_at.y0 && c.rect.y0 < c_was.y0 && c.opacity >= 1.0,
        "c slides up from where it was: {c:?}"
    );
    assert!(
        a.rect.y0 < a_at.y0 && a.rect.y0 > a_was.y0,
        "a slides down from where it was: {a:?}"
    );
    let leaving: Vec<_> = midway.iter().filter(|part| part.leaving).collect();
    assert!(
        matches!(leaving[..], [b] if b.rect == b_was && b.opacity > 0.0 && b.opacity < 1.0),
        "b fades out where it stood: {leaving:?}"
    );
    assert!(
        harness.root_widget().needs_animation_frame(),
        "frames are asked for while the parts move"
    );
}

#[test]
fn a_window_resizing_moves_nothing_on_its_own() {
    let (_app, mut harness) = shown(&page(&["a", "b", "c"], None));
    let _frame = ui::motion::testing::frozen();
    ui::motion::note_source(MotionSource::Pointer);
    harness.resize((900, 800)).expect("lays out");
    let now = ui::motion::frame_now();
    let resized = drawn(&harness, now);
    assert!(at_rest(&resized), "{resized:?}");
    assert!(
        resized.iter().all(|part| part.rect.width() > 600.0),
        "the parts stand at the new width at once: {resized:?}"
    );
}

#[test]
fn a_shortcut_a_sync_and_reduced_motion_land_at_once() {
    let (mut app, mut harness) = shown(&page(&["a", "b", "c"], None));
    let started = change(&mut app, &mut harness, MotionSource::Keyboard, reorder());
    let landed = drawn(&harness, started + Duration::from_millis(1));
    assert!(at_rest(&landed) && landed.len() == 3, "{landed:?}");

    let (mut app, mut harness) = shown(&page(&["a", "b", "c"], None));
    let theme = harness.theme().clone().with_reduce_motion(true);
    harness.set_theme(theme);
    harness.relayout();
    let started = change(&mut app, &mut harness, MotionSource::Pointer, reorder());
    let landed = drawn(&harness, started + Duration::from_millis(1));
    assert!(
        at_rest(&landed) && landed.len() == 3,
        "reduced motion lands: {landed:?}"
    );
    assert!(!harness.root_widget().needs_animation_frame());
}

#[test]
fn a_still_container_lands_at_once() {
    let (mut app, mut harness) = shown(&page(&["a", "b", "c"], Some("still")));
    let started = change(&mut app, &mut harness, MotionSource::Pointer, reorder());
    let landed = drawn(&harness, started + Duration::from_millis(1));
    assert!(at_rest(&landed) && landed.len() == 3, "{landed:?}");
}

/// How far `c` (second after the change) still has to go at `after` the change started, in a list
/// moving at `feel`'s pace.
fn left_to_go(feel: &str, after: Duration) -> f64 {
    let (mut app, mut harness) = shown(&page(&["a", "b", "c"], Some(feel)));
    let started = change(&mut app, &mut harness, MotionSource::Pointer, reorder());
    let at = |since: Duration| {
        let instant = started.checked_add(since).expect("an instant");
        drawn(&harness, instant)[1].rect.y0
    };
    let (moving, rest) = (at(after), at(LATER));
    (moving - rest).abs()
}

#[test]
fn a_calm_list_moves_slower_than_a_brisk_one() {
    let after = Duration::from_millis(60);
    let (brisk, standard, calm) = (
        left_to_go("brisk", after),
        left_to_go("standard", after),
        left_to_go("calm", after),
    );
    assert!(
        brisk < standard && standard < calm,
        "{brisk} < {standard} < {calm}"
    );
}
