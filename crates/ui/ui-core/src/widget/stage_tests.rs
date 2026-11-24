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
    assert_eq!(
        shown(&mut host).pan(),
        Vec2::ZERO,
        "a fitted page is centred"
    );

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
    assert!(
        !more,
        "at the edge the page stops and the wheel is the view's"
    );

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
fn boxes_props(queue: &crate::Intents<crate::component::Fired>, count: i64) -> StageProps {
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
            menu: Vec::new(),
            body: Vec::new(),
        })
        .collect();
    StageProps {
        name: Some("Slide".into()),
        canvas: vec![1280, 720],
        pins,
        select: Event::View(Intent::new(queue, "stage", "select")),
        moved: Event::View(Intent::new(queue, "stage", "moved")),
        open: Event::View(Intent::new(queue, "stage", "open")),
        delete: Event::View(Intent::new(queue, "stage", "delete")),
        ..StageProps::default()
    }
}

fn boxes_stage(queue: &crate::Intents<crate::component::Fired>, count: i64) -> Stage {
    Stage::from_props("stage", boxes_props(queue, count))
}

/// A stage laid over a page another view draws takes nothing of its own: the wheel does not
/// zoom it and no key reaches it, while its places still move with the hand.
#[test]
fn a_still_stage_takes_no_zoom_or_keys_and_its_places_still_move() {
    let queue = crate::Intents::default();
    let mut props = boxes_props(&queue, 1);
    props.still = true;
    let mut host = FrameHarness::new(
        crate::State::new(),
        Box::new(Stage::from_props("stage", props)),
        (800, 600),
    );
    assert!(
        !shown(&mut host).is_focusable(),
        "a still stage takes no keyboard"
    );
    let fitted = shown(&mut host).scale();

    host.dispatch_to_root(&synthetic_input::cursor_moved(400.0, 300.0));
    let wheeled = host.dispatch_to_root_with(&synthetic_input::mouse_wheel(0.0, 120.0), &command());
    assert!(!wheeled, "the wheel is the view's");
    assert!(
        (shown(&mut host).scale() - fitted).abs() < 1e-9,
        "the page did not zoom"
    );

    // The first place stands at page (96, 90), 160 by 90: its middle is at (110, 159) on a
    // stage 800 by 600 with the page fitted. Dragging it 20 window pixels moves it 32 page
    // units, exactly where the hand went.
    host.dispatch_to_root(&synthetic_input::cursor_moved(110.0, 159.0));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    host.dispatch_to_root(&synthetic_input::cursor_moved(130.0, 159.0));
    host.dispatch_to_root(&synthetic_input::mouse_release());
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
        moved.last(),
        Some(&("box-0".to_string(), vec![128, 90])),
        "the place moved where the hand went, with no margin to line up with: {moved:?}"
    );
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
    host.dispatch_to_root(&synthetic_input::named_key(
        winit::keyboard::NamedKey::ArrowRight,
    ));
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

/// A stage whose first place, "box-0", offers two commands, and whose second offers none.
fn menu_stage(queue: &crate::Intents<crate::component::Fired>) -> Stage {
    use crate::widget::{MenuItemProps, MenuPart};

    let command = |label: &str, event: &str, destructive: bool| {
        MenuPart::Item(MenuItemProps {
            label: label.into(),
            destructive,
            choose: Event::View(Intent::new(queue, "box-0", event)),
            ..MenuItemProps::default()
        })
    };
    let pins = vec![
        StagePinProps {
            key: "pin-0".into(),
            item: Some("box-0".into()),
            x: Some(96),
            y: Some(90),
            width: Some(160),
            height: Some(90),
            moves: true,
            menu: vec![
                command("Bring to front", "front", false),
                MenuPart::Separator,
                command("Delete", "delete", true),
            ],
            ..StagePinProps::default()
        },
        StagePinProps {
            key: "pin-1".into(),
            item: Some("box-1".into()),
            x: Some(400),
            y: Some(90),
            width: Some(160),
            height: Some(90),
            moves: true,
            menu: Vec::new(),
            ..StagePinProps::default()
        },
    ];
    Stage::from_props(
        "stage",
        StageProps {
            name: Some("Slide".into()),
            canvas: vec![1280, 720],
            pins,
            select: Event::View(Intent::new(queue, "stage", "select")),
            ..StageProps::default()
        },
    )
}

#[test]
fn a_places_menu_opens_where_it_is_pressed_and_runs_its_command() {
    // The editor fits a 1280 × 720 page into 800 × 600 at 0.625, centred: page (96, 90) stands
    // at window (60, 131), and the place's middle at about (110, 159).
    let queue = crate::Intents::default();
    let mut host = FrameHarness::new(
        crate::State::new(),
        Box::new(menu_stage(&queue)),
        (800, 600),
    );
    host.dispatch_to_root(&synthetic_input::cursor_moved(110.0, 159.0));
    host.dispatch_to_root(&synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Right,
    ));

    let stage = shown(&mut host);
    assert!(stage.menu.is_open(), "the place's menu opened under the press");
    let labels: Vec<String> = stage
        .menu
        .items()
        .iter()
        .filter_map(|item| item.label().map(str::to_owned))
        .collect();
    assert_eq!(
        labels,
        ["Bring to front", "Delete"],
        "the commands the view placed on the place"
    );
    let opened_at = stage
        .menu
        .anchor_box()
        .expect("the menu stands against the press")
        .origin();
    assert!(
        (opened_at.x - 110.0).abs() < 0.5 && (opened_at.y - 159.0).abs() < 0.5,
        "it opens where the press landed: {opened_at:?}"
    );

    // Choosing a command runs what the view bound, and the menu is no longer about a place.
    let deleted = stage
        .menu
        .items()
        .iter()
        .position(|item| item.label() == Some("Delete"))
        .expect("the Delete row");
    shown(&mut host).menu.click_item(deleted);
    let fired: Vec<(String, String)> = queue
        .take()
        .into_iter()
        .map(|fired| (fired.key, fired.event))
        .collect();
    assert_eq!(
        fired,
        [("box-0".to_owned(), "delete".to_owned())],
        "the command the view bound ran"
    );
    assert!(!shown(&mut host).menu.is_open(), "and the menu closed");

    // A place that names no command opens none: the press is the view's own report and nothing
    // more.
    host.dispatch_to_root(&synthetic_input::cursor_moved(300.0, 159.0));
    host.dispatch_to_root(&synthetic_input::mouse_input(
        ElementState::Pressed,
        MouseButton::Right,
    ));
    assert!(
        !shown(&mut host).menu.is_open(),
        "a place without commands opens no menu"
    );
    let selected: Vec<String> = queue
        .take()
        .into_iter()
        .map(|fired| fired.event)
        .collect();
    assert!(
        !selected.contains(&"delete".to_owned()),
        "nothing ran for the press"
    );
}

/// The page points of the two boxes in `boxes_stage`, as the 800 × 600 window shows a fitted
/// 1280 × 720 page: 0.625 and centred, so the first box's middle is at (110, 159) and the
/// second's at (300, 159).
const BOX_0: (f64, f64) = (110.0, 159.0);
const BOX_1: (f64, f64) = (300.0, 159.0);

#[test]
fn a_finger_moves_a_box_taps_select_and_a_double_tap_opens_it() {
    use winit::event::TouchPhase;

    let queue = crate::Intents::default();
    let mut host = FrameHarness::new(
        crate::State::new(),
        Box::new(boxes_stage(&queue, 2)),
        (800, 600),
    );

    // A finger dragged across the first box moves it with the hand, as a pointer does.
    let (x, y) = BOX_0;
    host.touch(0, TouchPhase::Started, x, y);
    host.touch(0, TouchPhase::Moved, x + 40.0, y);
    host.touch(0, TouchPhase::Moved, x + 80.0, y);
    host.touch(0, TouchPhase::Ended, x + 80.0, y);
    let moved: Vec<(String, i64)> = queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == "moved")
        .map(|fired| match fired.value {
            Some(crate::component::Value::List(mut parts)) => {
                let item = match parts.remove(0) {
                    crate::component::Value::Text(item) => item,
                    other => panic!("a place is named: {other:?}"),
                };
                let at = match parts.remove(0) {
                    crate::component::Value::List(mut at) => match at.remove(0) {
                        crate::component::Value::Int(x) => x,
                        other => panic!("a page point is whole: {other:?}"),
                    },
                    other => panic!("a page point: {other:?}"),
                };
                (item, at)
            }
            other => panic!("a place moved: {other:?}"),
        })
        .collect();
    assert!(
        moved
            .iter()
            .any(|(item, at)| item == "box-0" && *at > 96),
        "a dragged finger moves the place with it: {moved:?}"
    );

    // A tap chooses the place, and a second tap on the same place opens it.
    let (x, y) = BOX_1;
    host.touch(1, TouchPhase::Started, x, y);
    host.touch(1, TouchPhase::Ended, x, y);
    host.touch(2, TouchPhase::Started, x, y);
    host.touch(2, TouchPhase::Ended, x, y);
    let fired: Vec<String> = queue
        .take()
        .into_iter()
        .map(|fired| fired.event)
        .collect();
    assert!(
        fired.iter().any(|event| event == "select"),
        "a tap chooses the place: {fired:?}"
    );
    assert!(
        fired.iter().any(|event| event == "open"),
        "a double tap opens it: {fired:?}"
    );
}

#[test]
fn a_finger_held_on_a_place_opens_its_menu() {
    let queue = crate::Intents::default();
    let mut host = FrameHarness::new(
        crate::State::new(),
        Box::new(menu_stage(&queue)),
        (800, 600),
    );
    let (x, y) = BOX_0;
    host.touch_hold_at(x, y);
    assert!(
        shown(&mut host).menu.is_open(),
        "a held finger is the right press that opens the place's menu"
    );
}
