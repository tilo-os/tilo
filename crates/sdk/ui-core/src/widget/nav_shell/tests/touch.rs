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

fn widget_rect(node: &InspectionNode, widget: &str) -> Option<Rect> {
    if node.widget == widget && node.rect[2] > 0.0 && node.rect[3] > 0.0 {
        let [x, y, w, h] = node.rect;
        return Some(Rect::new(x, y, x + w, y + h));
    }
    node.children
        .iter()
        .find_map(|child| widget_rect(child, widget))
}

/// A window bar's controls keep the bar's control height on a touchscreen, as they do for a
/// mouse: the target a finger needs stands in each control's hit rect, beyond the shape it
/// draws. The header row keeps its height, so nothing under it moves.
#[test]
fn the_shells_buttons_keep_the_bars_control_height_on_touch() {
    let theme = Theme::light();
    let side = f64::from(theme.tokens.metrics.min_pointer_target);
    // A tablet shows the sidebar toggle; a phone pushes to the content, with Back and the
    // actions behind More.
    for ((w, h), ids) in [
        ((768, 1024), &["nav-shell-sidebar"][..]),
        (
            (320, 568),
            &["nav-shell-back", "nav-shell-more-content"][..],
        ),
    ] {
        for input in [
            InteractionEnvironment::TOUCH,
            InteractionEnvironment::POINTER,
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
                    "{id} draws {side}×{side} at {w}×{h} for {input:?}: {rect:?}"
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

/// The bar's trailing controls end at the same gutter its leading title starts at, at a
/// phone as at a desktop.
#[test]
fn the_bars_trailing_controls_keep_the_titles_gutter() {
    let theme = Theme::light();
    let gutter = crate::chrome::column_gutter(&theme);
    let page = || {
        NavShell::new()
            .content(Probe::boxed("content").0)
            .content_header("Q3 plan", actions())
    };
    for (w, h) in [(1280, 800), (390, 844)] {
        let mut harness = harness(page(), w, h, theme.clone());
        settle(&mut harness, w, h);
        let inspection = harness.inspection();
        let title = widget_rect(&inspection.root, "NavShellTitle")
            .unwrap_or_else(|| panic!("the title shows at {w}×{h}"));
        assert!(
            (title.x0 - gutter).abs() < 0.5,
            "the title starts at the gutter {gutter} at {w}×{h}: {title:?}"
        );
        let last = rect_of(&inspection.root, "act-5").expect("the actions show");
        assert!(
            (last.x1 - (f64::from(w) - gutter)).abs() < 0.5,
            "the trailing actions end at the gutter {gutter} at {w}×{h}: {last:?}"
        );
    }
}

/// Actions an app hands the shell after the window told it a finger reaches it (the header
/// of a page a press opened) draw at the bar's control height, as the first ones did.
#[test]
fn actions_handed_over_on_a_touchscreen_are_bar_controls() {
    let theme = Theme::light();
    let side = f64::from(theme.tokens.metrics.min_pointer_target);
    let mut harness = harness(shell(), 1024, 768, theme.clone());
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
        (rect.width() - side).abs() < 0.5 && (rect.height() - side).abs() < 0.5,
        "the new action draws {side}×{side}: {rect:?}"
    );
}
