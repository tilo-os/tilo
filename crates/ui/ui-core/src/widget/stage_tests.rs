//! The page a stage shows zooms and pans: a person works on a slide closer than the window holds
//! it, and never loses it past its own edges.

use super::*;
use crate::component::{Described, Event, Intent};
use crate::headless::FrameHarness;
use crate::synthetic_input;

/// A page of the size a slide has, standing in the stage as the editor's does.
fn stage() -> Stage {
    Stage::from_props(
        "stage",
        StageProps {
            name: Some("Slide".into()),
            canvas: vec![1280, 720],
            ..StageProps::default()
        },
    )
}

/// The stage in a window of 800 × 600, as the editor shows it.
fn harness() -> FrameHarness {
    FrameHarness::new(crate::State::new(), Box::new(stage()), (800, 600))
}

fn shown(host: &mut FrameHarness) -> &mut Stage {
    host.root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Stage>())
        .expect("the root is the stage")
}

/// The command modifier of the platform the tests run on.
fn command() -> winit::event::Modifiers {
    winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL)
}

#[test]
fn the_slide_zooms_and_pans_without_losing_its_edges() {
    let mut host = harness();
    let fitted = shown(&mut host).scale();
    assert!(
        (fitted - 0.625).abs() < 1e-9,
        "the editor opens fitted: {fitted}"
    );
    assert_eq!(shown(&mut host).pan(), Vec2::ZERO, "a fitted page is centred");

    // A wheel over the fitted page: it has nowhere to move, so the page's view keeps it.
    host.dispatch_to_root(&synthetic_input::cursor_moved(400.0, 300.0));
    let moved = host.dispatch_to_root(&synthetic_input::mouse_wheel(0.0, -120.0));
    assert!(!moved, "the fitted page's wheel is the view's");
    assert_eq!(shown(&mut host).pan(), Vec2::ZERO, "and it moved nothing");

    // The command modifier and a wheel zoom about the pointer, and the zoom stays pinned
    // where the person put it.
    host.dispatch_to_root_with(&synthetic_input::mouse_wheel(0.0, 120.0), &command());
    let zoomed = shown(&mut host).scale();
    assert!(
        zoomed > fitted,
        "the page is drawn larger: {zoomed} against {fitted}"
    );

    // At a zoom the stage cannot hold, the wheel moves the page, and no further than its own
    // overflow: the page's edge stops at the stage's edge.
    let mut moved = false;
    for _ in 0..20 {
        moved |= host.dispatch_to_root(&synthetic_input::mouse_wheel(0.0, -1000.0));
    }
    assert!(moved, "a zoomed page moves with the wheel");
    let scale = shown(&mut host).scale();
    let reach = ((720.0 * scale - 600.0) / 2.0).max(0.0);
    let pan = shown(&mut host).pan();
    assert!(
        (pan.y + reach).abs() < 0.01,
        "the page stops with its edge at the stage's: {pan:?} against {reach:.1}"
    );
    let reach_x = ((1280.0 * scale - 800.0) / 2.0).max(0.0);
    assert!(
        pan.x.abs() <= reach_x + 0.01,
        "its other edge stays inside the stage too: {pan:?} within {reach_x:.1}"
    );
    let more = host.dispatch_to_root(&synthetic_input::mouse_wheel(0.0, -1000.0));
    assert!(!more, "at the edge the page stops and the wheel is the view's");

    // Ctrl+0 returns it to fitted, whole and centred, as it opened.
    shown(&mut host).set_focus(true);
    host.dispatch_to_root_with(
        &synthetic_input::key_press(winit::keyboard::Key::Character("0".into()), Some("0")),
        &command(),
    );
    assert!(
        (shown(&mut host).scale() - fitted).abs() < 1e-9,
        "Ctrl+0 fits the page again"
    );
    assert_eq!(shown(&mut host).pan(), Vec2::ZERO, "and centres it");
}

/// A stage holding `count` boxes a hundred page pixels apart, whose choosing and moving it
/// reports on `queue`.
fn boxes_stage(queue: &crate::Intents<crate::component::Fired>, count: i64) -> Stage {
    let pins = (0..count)
        .map(|index| StagePinProps {
            key: format!("box-{index}"),
            name: Some(format!("Box {index}")),
            item: Some(format!("box-{index}")),
            x: Some(96 + index * 200),
            y: Some(90),
            width: Some(160),
            height: Some(90),
            moves: true,
            body: Vec::new(),
        })
        .collect();
    Stage::from_props(
        "stage",
        StageProps {
            name: Some("Slide".into()),
            canvas: vec![1280, 720],
            pins,
            select: Event::View(Intent::new(queue, "stage", "select")),
            moved: Event::View(Intent::new(queue, "stage", "moved")),
            open: Event::View(Intent::new(queue, "stage", "open")),
            delete: Event::View(Intent::new(queue, "stage", "delete")),
            ..StageProps::default()
        },
    )
}

#[test]
fn tab_walks_the_places_and_the_arrows_nudge_the_chosen_one() {
    let queue = crate::Intents::default();
    let mut host = FrameHarness::new(
        crate::State::new(),
        Box::new(boxes_stage(&queue, 2)),
        (800, 600),
    );
    shown(&mut host).set_focus(true);

    // Tab chooses the first place, then the next, wrapping back to the first.
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::Tab));
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::Tab));
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::Tab));
    let chosen: Vec<String> = queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == "select")
        .map(|fired| match fired.value {
            Some(crate::component::Value::List(mut parts)) => match parts.remove(0) {
                crate::component::Value::Text(item) => item,
                other => panic!("a place is named: {other:?}"),
            },
            other => panic!("a place was chosen: {other:?}"),
        })
        .collect();
    assert_eq!(
        chosen,
        ["box-0", "box-1", "box-0"],
        "Tab walks the places and wraps"
    );

    // An arrow nudges the chosen place ten page pixels, and Shift by one.
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::ArrowRight));
    host.dispatch_to_root_with(
        &synthetic_input::named_key(winit::keyboard::NamedKey::ArrowDown),
        &winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT),
    );
    let moved: Vec<(String, Vec<i64>)> = queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == "moved")
        .map(|fired| match fired.value {
            Some(crate::component::Value::List(parts)) => {
                let mut parts = parts.into_iter();
                let item = match parts.next() {
                    Some(crate::component::Value::Text(item)) => item,
                    other => panic!("a place is named: {other:?}"),
                };
                let at = match parts.next() {
                    Some(crate::component::Value::List(at)) => at
                        .into_iter()
                        .map(|value| match value {
                            crate::component::Value::Int(number) => number,
                            other => panic!("a page point is whole: {other:?}"),
                        })
                        .collect(),
                    other => panic!("a page point: {other:?}"),
                };
                (item, at)
            }
            other => panic!("a place moved: {other:?}"),
        })
        .collect();
    assert_eq!(
        moved,
        [
            ("box-0".to_string(), vec![106, 90]),
            ("box-0".to_string(), vec![106, 91]),
        ],
        "an arrow moves it ten, and with Shift one"
    );
}
