//! The shell's own buttons on a touchscreen: as large as an app's buttons are there.

use super::*;
use crate::headless::InspectionNode;
use crate::interaction::InteractionEnvironment;
use crate::widget::Button;

fn actions() -> Vec<Element> {
    (0..6)
        .map(|i| {
            Box::new(Button::icon_only(
                &format!("act-{i}"),
                crate::icons::Icon::Star,
                "Act",
            )) as Element
        })
        .collect()
}

fn shell() -> NavShell {
    NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Notes", Vec::new())
        .content_header("Q3 plan", actions())
}

fn rect_of(node: &InspectionNode, id: &str) -> Option<Rect> {
    if node.test_id.as_deref() == Some(id) && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
        let [x, y, w, h] = node.rect;
        return Some(Rect::new(x, y, x + w, y + h));
    }
    node.children.iter().find_map(|child| rect_of(child, id))
}

/// On a touchscreen the sidebar toggle, Back and the More button the actions overflow into
/// are the minimum target a finger needs, as an app's buttons are; the header row keeps its
/// height, so nothing under it moves. With a mouse they keep their pointer size.
#[test]
fn the_shells_buttons_grow_to_a_fingers_target_on_touch() {
    let theme = Theme::light();
    let touch = f64::from(InteractionEnvironment::TOUCH.minimum_target());
    let pointer = f64::from(theme.tokens.metrics.min_pointer_target);
    // A tablet shows the sidebar toggle; a phone pushes to the content, with Back and the
    // actions behind More.
    for ((w, h), ids) in [
        ((768, 1024), &["nav-shell-sidebar"][..]),
        (
            (320, 568),
            &["nav-shell-back", "nav-shell-more-content"][..],
        ),
    ] {
        for (input, side) in [
            (InteractionEnvironment::TOUCH, touch),
            (InteractionEnvironment::POINTER, pointer),
        ] {
            let mut harness = harness(shell(), w, h, theme.clone());
            harness.set_interaction_environment(input);
            harness.root_mut::<NavShell>().show(NavPane::Content);
            settle(&mut harness, w, h);
            let inspection = harness.inspection();
            for id in ids {
                let rect = rect_of(&inspection.root, id)
                    .unwrap_or_else(|| panic!("{id} shows at {w}×{h}"));
                assert!(
                    (rect.width() - side).abs() < 0.5 && (rect.height() - side).abs() < 0.5,
                    "{id} is {side}×{side} at {w}×{h} for {input:?}: {rect:?}"
                );
            }
            assert_eq!(
                geometry(&harness).content_header().height(),
                f64::from(theme.tokens.metrics.header_height),
                "the header row keeps its height at {w}×{h} for {input:?}"
            );
        }
    }
}

/// Actions an app hands the shell after the window told it a finger reaches it (the header
/// of a page a press opened) are a finger's target as the first ones were.
#[test]
fn actions_handed_over_on_a_touchscreen_are_a_fingers_target() {
    let touch = f64::from(InteractionEnvironment::TOUCH.minimum_target());
    let mut harness = harness(shell(), 1024, 768, Theme::light());
    harness.set_interaction_environment(InteractionEnvironment::TOUCH);
    settle(&mut harness, 1024, 768);
    let discard = Box::new(Button::icon_only(
        "discard",
        crate::icons::Icon::Trash,
        "Discard",
    )) as Element;
    harness
        .root_mut::<NavShell>()
        .set_content_header("New message", vec![discard]);
    settle(&mut harness, 1024, 768);
    let rect = rect_of(&harness.inspection().root, "discard").expect("the new action shows");
    assert!(
        (rect.width() - touch).abs() < 0.5 && (rect.height() - touch).abs() < 0.5,
        "the new action is {touch}×{touch}: {rect:?}"
    );
}
