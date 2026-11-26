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
        .sidebar(TestLeaf::fill("sidebar").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
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

tilo_test_env::scenario! {
    fn on_touch_the_shell_keeps_bar_heights_and_shows_only_the_way_back;



    // Actions an app hands the shell after the window told it a finger reaches it (the header
    // of a page a press opened) draw at the bar's control height, as the first ones did.
    phase actions_handed_over_on_a_touchscreen_are_bar_controls;
    {
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

    // A screen that went somewhere deeper has one navigation affordance: its way back, at the
    // leading edge.
    phase a_pushed_phone_screen_shows_only_its_way_back;
    {
        let theme = Theme::light();
        let gutter = crate::chrome::column_gutter(&theme);
        let page = || {
            let can = SharedRc::new(Cell::new(false));
            let read = SharedRc::clone(&can);
            let write = SharedRc::clone(&can);
            let shell = NavShell::new()
                .content_kind(ContentKind::Document)
                .sidebar(TestLeaf::fill("sidebar").boxed().0)
                .content(TestLeaf::fill("content").boxed().0)
                .content_header("Call the landlord", Vec::new())
                .on_back(move || read.get(), move || write.set(false));
            (shell, can)
        };
        let (shell, can) = page();
        let mut harness = harness(shell, 390, 844, theme.clone());
        settle(&mut harness, 390, 844);
        let first = harness.inspection();
        let toggle = rect_of(&first.root, "nav-shell-sidebar")
            .expect("the first screen offers the sidebar's toggle");
        assert!(
            toggle.x0 < gutter,
            "the toggle leads the first screen, in the bar's corner: {toggle:?}"
        );
        assert!(
            rect_of(&first.root, "nav-shell-back").is_none(),
            "there is nowhere to go back to"
        );

        can.set(true);
        settle(&mut harness, 390, 844);
        let pushed = harness.inspection();
        let back = rect_of(&pushed.root, "nav-shell-back").expect("the pushed screen's way back");
        assert!(
            (back.x0 - toggle.x0).abs() < 0.5 && (back.y0 - toggle.y0).abs() < 0.5,
            "back takes the leading corner the toggle held on the first screen: {back:?} \
         against {toggle:?}"
        );
        assert!(
            rect_of(&pushed.root, "nav-shell-sidebar").is_none(),
            "the pushed screen shows no second way to navigate"
        );
    }

}
