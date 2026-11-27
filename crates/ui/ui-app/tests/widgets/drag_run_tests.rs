//! A drag run's items stand a gap apart, share a hugging row, and report one
//! move before the item under them as they are dragged.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion"
)]
fn item(key: &str, width: f32) -> RunItem {
    RunItem {
        key: key.to_owned(),
        label: key.to_owned(),
        item: None,
        group: RunGroup::Leading,
        view: Box::new(
            Container::new()
                .with_size(width, 48.0)
                .with_child(Box::new(Spacer::fixed(1.0))),
        ),
    }
}

fn group_item(key: &str, width: f32, group: RunGroup) -> RunItem {
    RunItem {
        group,
        ..item(key, width)
    }
}

fn bar() -> DragRun {
    DragRun::new("bar")
        .name("Bar")
        .push(group_item("mark", 24.0, RunGroup::Leading))
        .push(group_item("menu", 90.0, RunGroup::Leading))
        .push(group_item("clock", 50.0, RunGroup::Center))
        .push(group_item("people", 70.0, RunGroup::Trailing))
        .push(group_item("status", 24.0, RunGroup::Trailing))
}

tilo_test_env::scenario! {
    /// How a run lays its items out.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_drag_run_lays_out_its_items_and_groups;

    // Every item keeps the width its view takes, packed one sibling gap apart: a run never
    // gives its items an equal share of its length.
    phase a_run_is_its_items_at_their_own_size_one_gap_apart;
    {
        let theme = Theme::dark();
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let host = harness(
            DragRun::new("dock")
                .push(item("a", 48.0))
                .push(item("b", 80.0))
                .push(item("c", 24.0)),
        );
        let rects = host.root::<DragRun>().item_rects_for_tests();
        assert_eq!(rects.len(), 3);
        assert!((rects[0].width() - 48.0).abs() < 0.5, "{rects:?}");
        assert!((rects[1].width() - 80.0).abs() < 0.5, "{rects:?}");
        assert!((rects[2].width() - 24.0).abs() < 0.5, "{rects:?}");
        for pair in rects.windows(2) {
            let [left, right] = pair else { continue };
            assert!(
                (right.x0 - left.x1 - gap).abs() < 0.5,
                "one sibling gap between items: {rects:?}"
            );
        }
    }

    phase a_grouped_run_puts_its_groups_at_their_own_edges;
    {
        let width = 1280.0;
        let host = Driver::for_root(
            tilo_ui_app::State::new(),
            bar(),
            (width as u32, 200),
            Theme::dark(),
        );
        let rects = host.root::<DragRun>().item_rects_for_tests();
        assert_eq!(rects.len(), 5);
        assert!((rects[0].x0).abs() < 0.5, "the mark leads: {rects:?}");
        assert!(
            (rects[1].x0 - rects[0].x1 - 12.0).abs() < 0.5,
            "the menu follows the mark a sibling gap on: {rects:?}"
        );
        let clock = rects[2];
        assert!(
            (clock.center().x - f64::from(width) / 2.0).abs() < 1.0,
            "the clock stands in the middle: {rects:?}"
        );
        assert!(
            (rects[4].x1 - f64::from(width)).abs() < 0.5,
            "the status ends at the trailing edge: {rects:?}"
        );
        assert!(
            rects[4].x0 >= rects[3].x1,
            "the status follows the people: {rects:?}"
        );
    }

    phase a_dock_packs_its_items_where_its_distribute_says;
    {
        let width = 1280.0;
        let host = Driver::for_root(
            tilo_ui_app::State::new(),
            DragRun::new("dock")
                .distribute(tilo_ui_app::widget::arrange::Distribute::Center)
                .push(item("a", 48.0))
                .push(item("b", 48.0))
                .push(item("c", 48.0)),
            (width as u32, 200),
            Theme::dark(),
        );
        let rects = host.root::<DragRun>().item_rects_for_tests();
        let left = rects[0].x0;
        let right = rects[2].x1;
        assert!(
            (left - (f64::from(width) - right)).abs() < 1.0,
            "equal area at both ends: {rects:?}"
        );
        assert!(
            (rects[1].x0 - rects[0].x1 - 12.0).abs() < 0.5,
            "packed a sibling gap apart: {rects:?}"
        );
    }
}

/// A dock of icon-only buttons beside the apps running now, in a row that hugs them inside a
/// sideways scroll: both share the row's room, and the run still gets the width of its items,
/// one sibling gap apart, so no button (nor the count on its corner) stands over the next.
#[test]
fn a_run_sharing_a_hugging_row_keeps_its_items_a_gap_apart() {
    use tilo_ui_app::widget::arrange::Fit;
    use tilo_ui_app::widget::{BadgeContent, Button, ButtonRole, ChipStyle, Scrollable};
    fn dock() -> Element {
        let mut run =
            DragRun::new("dock").distribute(tilo_ui_app::widget::arrange::Distribute::Center);
        for (index, key) in ["files", "writer", "mail", "photos", "music"]
            .into_iter()
            .enumerate()
        {
            let mut button = Button::icon_only(key, tilo_ui_base::icons::Icon::Bell, key)
                .role(ButtonRole::Plain);
            if index == 2 {
                button = button.with_count(BadgeContent::Count(12), ChipStyle::default());
            }
            run = run.push(RunItem {
                key: key.to_owned(),
                label: key.to_owned(),
                item: None,
                group: RunGroup::Leading,
                view: Box::new(button),
            });
        }
        let mut running = tilo_ui_app::widget::arrange::Row::row().gap(Rhythm::Sibling);
        for key in ["chat", "notes"] {
            running = running.push(Box::new(
                Button::icon_only(key, tilo_ui_base::icons::Icon::Bell, key)
                    .role(ButtonRole::Plain),
            ));
        }
        Box::new(
            tilo_ui_app::widget::arrange::Row::row()
                .gap(Rhythm::Sibling)
                .push_fit(Box::new(run), Fit::fill(1.0))
                .push_fit(Box::new(running), Fit::fill(1.0)),
        )
    }
    fn collect(node: &tilo_ui_app::headless::InspectionNode, out: &mut Vec<Rect>) {
        if node.widget == "Button" {
            let [x, y, w, h] = node.rect;
            out.push(Rect::new(x, y, x + w, y + h));
        }
        for child in &node.children {
            collect(child, out);
        }
    }
    for theme in [Theme::light(), Theme::dark()] {
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let host = Driver::for_root(
            tilo_ui_app::State::new(),
            Scrollable::new_horizontal().with_child(dock()),
            (1280, 200),
            theme,
        );
        let mut buttons = Vec::new();
        collect(&host.inspection().root, &mut buttons);
        assert_eq!(
            buttons.len(),
            7,
            "five in the run, two running: {buttons:?}"
        );
        for pair in buttons.windows(2) {
            let [left, right] = pair else { continue };
            assert!(
                (right.x0 - left.x1 - gap).abs() < 0.5,
                "each button a sibling gap after the one before: {buttons:?}"
            );
        }
    }
}

fn harness(run: DragRun) -> Driver {
    Driver::for_root(tilo_ui_app::State::new(), run, (600, 200), Theme::dark())
}

fn run() -> DragRun {
    DragRun::new("dock")
        .name("Dock")
        .push(item("a", 48.0))
        .push(item("b", 48.0))
        .push(item("c", 48.0))
        .push(item("d", 48.0))
}

#[test]
fn dragging_an_item_past_the_second_reports_one_move_before_it() {
    let mut host = harness(run());
    let rects = host.root::<DragRun>().item_rects_for_tests();
    let (a, c) = (rects[0], rects[2]);
    host.drag_at((a.center().x, a.center().y), (c.x0 + 2.0, c.center().y));
    let drops = host.root_mut::<DragRun>().take_drops();
    assert_eq!(
        drops,
        [DragDrop {
            run: "dock".to_owned(),
            moved: "a".to_owned(),
            at: 1,
            before: Some("c".to_owned()),
        }]
    );
    assert!(!host.root::<DragRun>().is_lifting());
}

#[test]
fn a_click_moves_nothing() {
    let mut host = harness(run());
    let a = host.root::<DragRun>().item_rects_for_tests()[0];
    host.click_at(a.center().x, a.center().y);
    assert!(host.root_mut::<DragRun>().take_drops().is_empty());
}

#[test]
fn letting_go_where_it_already_stands_moves_nothing() {
    let mut host = harness(run());
    let a = host.root::<DragRun>().item_rects_for_tests()[0];
    host.drag_at((a.center().x, a.center().y), (a.x1 - 2.0, a.center().y));
    assert!(host.root_mut::<DragRun>().take_drops().is_empty());
}

#[test]
fn dragging_past_the_end_lands_after_the_last_item() {
    let mut host = harness(run());
    let rects = host.root::<DragRun>().item_rects_for_tests();
    let (a, d) = (rects[0], rects[3]);
    host.drag_at((a.center().x, a.center().y), (d.x1 - 2.0, d.center().y));
    let drops = host.root_mut::<DragRun>().take_drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].before, None, "the end of the run");
}
