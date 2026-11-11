//! The primitives laid out by the real solver, at several widths, text sizes and directions.

use std::collections::HashMap;

use taffy::TaffyTree;
use vello::kurbo::Point;

use super::*;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::{Spacer, Widget};

fn solve(root: &mut dyn Widget, theme: &Theme, width: f32) {
    solve_in(root, theme, width, taffy::AvailableSpace::MaxContent);
}

/// Solve `root` at `width`, its height under `height`: a column that shrinks what it holds to
/// its minimum lays it out under a min-content constraint.
fn solve_in(root: &mut dyn Widget, theme: &Theme, width: f32, height: taffy::AvailableSpace) {
    let mut tree = TaffyTree::new();
    let mut nodes: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let node = root.build_layout(&mut tree, theme, &mut nodes);
    let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
    let mut text = crate::font_catalog::fixture_renderer();
    // SAFETY: exact live, exclusively owned widget tree and matching maps.
    unsafe {
        bindings.rebuild(root, &tree, node, &nodes);
        crate::widget::compute_widget_layout(
            &mut tree,
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height,
            },
            &nodes,
            &bindings,
            &mut text,
        )
        .expect("settles");
    }
    root.apply_layout(&tree, node, Point::ZERO);
}

fn boxed(size: f32) -> crate::widget::Element {
    Box::new(Spacer::fixed(size))
}

fn rects(widget: &dyn Widget) -> Vec<vello::kurbo::Rect> {
    widget
        .children()
        .iter()
        .map(|child| child.get_rect())
        .collect()
}

fn gap(theme: &Theme, step: Rhythm) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.get(step))
}

#[test]
fn a_stack_runs_top_to_bottom_with_the_sibling_gap() {
    let theme = Theme::dark();
    let mut stack = Stack::stack()
        .align(Align::Start)
        .push(boxed(20.0))
        .push(boxed(30.0));
    solve(&mut stack, &theme, 400.0);
    let r = rects(&stack);
    assert!(r[0].y0.abs() < 0.5);
    assert!((r[1].y0 - (20.0 + gap(&theme, Rhythm::Sibling))).abs() < 0.5);
}

#[test]
fn an_absent_child_takes_its_gap_with_it() {
    let theme = Theme::dark();
    let mut stack = Stack::stack()
        .align(Align::Start)
        .push(boxed(20.0))
        .push(Box::new(crate::widget::Absent::new()))
        .push(boxed(30.0));
    solve(&mut stack, &theme, 400.0);
    let last = stack
        .children()
        .last()
        .map(|child| child.get_rect())
        .expect("three children");
    assert!(
        (last.y0 - (20.0 + gap(&theme, Rhythm::Sibling))).abs() < 0.5,
        "one gap between the two present children: {last:?}"
    );
}

/// A container that leads stands what it holds one row lead in — the gutter a row's leading
/// control stands in, its cell and the gap after it — so a document's body lands under the text
/// its heading's control leads to, whatever size the person's controls take.
#[test]
fn a_container_that_leads_stands_what_it_holds_under_the_text_a_control_leads_to() {
    use crate::interaction::InteractionEnvironment;
    let theme = Theme::dark();
    for environment in [
        InteractionEnvironment::POINTER,
        InteractionEnvironment::TOUCH,
    ] {
        let target = f64::from(environment.minimum_target());
        let heading = Row::row()
            .fill_width()
            .push(boxed(target as f32))
            .push(boxed(100.0));
        let mut body = Stack::stack().fill_width().push(boxed(100.0));
        body.set_lead(1);
        let mut root = Stack::stack()
            .fill_width()
            .push(Box::new(heading))
            .push(Box::new(body));
        root.set_interaction_environment(environment);
        solve(&mut root, &theme, 600.0);
        let r = rects(&root);
        // The heading's text begins past the control that leads it.
        let text = r[0].x0 + target + gap(&theme, Rhythm::Related);
        let held = held_in(&root);
        assert!(
            (held.x0 - text).abs() < 0.5,
            "{environment:?}: the body stands at {}, the text at {text}",
            held.x0
        );
        assert!(
            (held.x0 - (r[1].x0 + target + gap(&theme, Rhythm::Related))).abs() < 0.5,
            "{environment:?}: one lead in: {held:?}"
        );
        // A container without a lead keeps its content at its edge.
        let mut plain = Stack::stack().fill_width().push(boxed(100.0));
        solve(&mut plain, &theme, 600.0);
        assert!(rects(&plain)[0].x0.abs() < 0.5);
    }
}

/// A container that keeps a padding holds what it holds that far in on every side, so a surface
/// standing against a window's edge keeps the window's gutter whatever it covers; one that keeps
/// none holds it at its own edge, and a lead stands on top of the padding it keeps.
#[test]
fn a_container_that_keeps_a_padding_insets_what_it_holds() {
    let theme = Theme::dark();
    let pad = gap(&theme, Rhythm::Section);
    let mut padded = Stack::stack()
        .fill_width()
        .padding(Rhythm::Section)
        .push(boxed(100.0));
    solve(&mut padded, &theme, 400.0);
    let held = rects(&padded)[0];
    assert!(
        (held.x0 - pad).abs() < 0.5 && (held.y0 - pad).abs() < 0.5,
        "the padding stands what it holds {pad} in from its edge: {held:?}"
    );

    let mut bare = Stack::stack().fill_width().push(boxed(100.0));
    solve(&mut bare, &theme, 400.0);
    assert!(rects(&bare)[0].x0.abs() < 0.5, "no padding, no inset");

    let mut led = Stack::stack()
        .fill_width()
        .padding(Rhythm::Section)
        .push(boxed(100.0));
    led.set_lead(1);
    // A column with the room for a lead: on a phone's screen it keeps none, and this is the
    // padding's own test.
    solve(&mut led, &theme, 600.0);
    let held = rects(&led)[0];
    assert!(
        held.x0 > pad + 0.5,
        "a lead stands what it holds further in than the padding alone: {held:?}"
    );
}

/// A column without the room for a row lead keeps none: on a phone's screen what a document's
/// body holds starts on the page's gutter, so the words keep the width a control's cell would
/// take from every line, while the column what it holds is read in is a column.
#[test]
fn a_phone_screen_keeps_no_lead_where_a_column_does() {
    use super::units::COMPACT_BELOW;
    let theme = Theme::dark();
    let rem = f64::from(rem_of(&theme));
    let lead = f64::from(Resolved::of(&theme).lead());
    assert!(lead > 0.0);
    // The narrowest a column what the lead holds stands at, and the widest box that leaves it
    // below: `WidthClass::Medium`, where a lead has room again.
    let column = f64::from(COMPACT_BELOW) * rem;
    for (width, kept) in [
        (320.0, false),
        (390.0, false),
        (column, false),
        (column + lead - 1.0, false),
        (column + lead, true),
        (1280.0, true),
    ] {
        let mut body = Stack::stack().fill_width().push(boxed(100.0));
        body.set_lead(1);
        solve(&mut body, &theme, width as f32);
        let held = rects(&body)[0];
        let inset = if kept { lead } else { 0.0 };
        assert!(
            (held.x0 - inset).abs() < 0.5,
            "{width} wide: what it holds stands at {}, where a lead is {lead}",
            held.x0
        );
    }
}

/// Where the body of `root` — its second child, which leads — holds what is written in it.
fn held_in(root: &Stack) -> vello::kurbo::Rect {
    root.children()[1]
        .children()
        .first()
        .map(|child| child.get_rect())
        .expect("the body holds one child")
}

#[test]
fn a_row_runs_leading_to_trailing_and_mirrors_in_right_to_left() {
    let theme = Theme::dark();
    let mut row = Row::row().fill_width().push(boxed(20.0)).push(boxed(20.0));
    solve(&mut row, &theme, 400.0);
    let r = rects(&row);
    assert!(r[0].x0 < r[1].x0);
    let mut rtl = Row::row()
        .fill_width()
        .direction(Direction::Rtl)
        .push(boxed(20.0))
        .push(boxed(20.0));
    solve(&mut rtl, &theme, 400.0);
    let r = rects(&rtl);
    assert!(
        r[0].x0 > r[1].x0,
        "the first child is at the leading, right edge"
    );
    assert!((r[0].x1 - 400.0).abs() < 0.5);
}

#[test]
fn a_filled_child_takes_the_rest_and_weights_share_it() {
    let theme = Theme::dark();
    let mut row = Row::row()
        .fill_width()
        .gap(Rhythm::Tight)
        .push(boxed(40.0))
        .push_fit(boxed(1.0), Fit::fill(1.0))
        .push_fit(boxed(1.0), Fit::fill(2.0));
    solve(&mut row, &theme, 400.0);
    let r = rects(&row);
    let free = 400.0 - 40.0 - 2.0 * gap(&theme, Rhythm::Tight);
    // The wrappers share the free space 1:2; the children hug inside.
    assert!((r[2].x0 - r[1].x0) > 0.0);
    assert!(r[2].x0 + 1.0 >= 40.0 + gap(&theme, Rhythm::Tight) + free / 3.0 - 1.0);
}

#[test]
fn a_wrapping_row_flows_onto_the_next_line() {
    let theme = Theme::dark();
    let mut row = Row::row().wrap().align(Align::Start);
    for _ in 0..6 {
        row = row.push(boxed(100.0));
    }
    solve(&mut row, &theme, 260.0);
    let r = rects(&row);
    assert!(r[1].y0 < 1.0, "two fit on the first line");
    assert!(r[5].y0 > 100.0, "the rest flow below");
}

fn grid_of(items: usize, min_rem: f32) -> Grid {
    let mut grid = Grid::auto(Length::Rem(min_rem));
    for _ in 0..items {
        grid = grid.push(boxed(20.0));
    }
    grid
}

#[test]
fn a_grid_has_as_many_columns_as_fit_and_reflows_with_the_width() {
    let theme = Theme::dark();
    let rem = rem_of(&theme);
    let g = f64::from(gap(&theme, Rhythm::Sibling) as f32);
    for (width, expected) in [(320.0, 1_u16), (1000.0, 5), (2000.0, 10)] {
        let mut grid = grid_of(12, 11.0);
        solve(&mut grid, &theme, width);
        let fit = columns_for(width, 11.0 * rem, g as f32);
        assert_eq!(usize::from(grid.layout().columns()), fit);
        if (15.9..=16.1).contains(&rem) {
            assert_eq!(grid.layout().columns(), expected, "width {width}");
        }
        let r = rects(&grid);
        let columns = usize::from(grid.layout().columns());
        if columns < 12 {
            assert!(
                r[columns].y0 > r[0].y1 - 0.5,
                "item {columns} starts the second row"
            );
        }
        assert!(r[0].y0.abs() < 0.5 && (columns < 2 || r[1].y0.abs() < 0.5));
    }
}

#[test]
fn larger_text_leaves_fewer_grid_columns() {
    let normal = Theme::dark();
    let large = Theme::dark().with_text_scale(2.0);
    let mut a = grid_of(12, 11.0);
    let mut b = grid_of(12, 11.0);
    solve(&mut a, &normal, 1200.0);
    solve(&mut b, &large, 1200.0);
    assert!(
        b.layout().columns() < a.layout().columns(),
        "{} columns at 200 % text, {} at 100 %",
        b.layout().columns(),
        a.layout().columns()
    );
}

#[test]
fn a_hugging_grid_keeps_each_item_as_tall_as_its_content() {
    let theme = Theme::dark();
    let item = |height: f32| {
        Box::new(Stack::stack().fill_height().push(boxed(height))) as crate::widget::Element
    };
    let mut stretched = Grid::columns(2).push(item(20.0)).push(item(100.0));
    solve(&mut stretched, &theme, 600.0);
    let mut hugging = Grid::columns(2)
        .hugging()
        .push(item(20.0))
        .push(item(100.0));
    solve(&mut hugging, &theme, 600.0);
    assert!(
        rects(&stretched)[0].height() > 90.0,
        "a plain grid stretches its items to the row"
    );
    let r = rects(&hugging);
    assert!(r[0].height() < 30.0, "the short item hugs: {:?}", r[0]);
    assert!(r[0].y0.abs() < 0.5, "at the top of its row");
}

/// A hugging grid's own box holds every item it places: an item as tall as its content is
/// never taller than the grid that shows it, at any display scale (a dashboard's cards, each
/// as tall as what it lists, reached the grid's edge and past it by a device pixel).
#[test]
fn a_hugging_grid_holds_the_items_it_places() {
    for scale in [1.0, 1.5, 2.0] {
        let theme = Theme::dark();
        let _held = crate::widget::enter_device_scale(scale);
        let item = |height: f32, width: f32| {
            Box::new(
                Stack::stack()
                    .push(boxed(height))
                    .push(Box::new(Spacer::fixed(width)) as crate::widget::Element),
            ) as crate::widget::Element
        };
        let mut grid = Grid::columns(2)
            .hugging()
            .push(item(20.0, 30.3))
            .push(item(100.3, 44.7))
            .push(item(57.1, 12.2));
        solve(&mut grid, &theme, 601.3);
        let bounds = grid.get_rect();
        for (index, r) in rects(&grid).into_iter().enumerate() {
            assert!(
                r.y1 <= bounds.y1 + 0.01 && r.x1 <= bounds.x1 + 0.01,
                "at {scale}x, item {index} {r:?} is inside the grid {bounds:?}"
            );
        }
    }
}

/// A hugging grid at its smallest still holds its cards whole: a card's body is as tall as
/// what it holds whether the card is measured or laid out, so a column that shrinks the grid
/// to its minimum never leaves a card standing past the grid's edge or over the next row. (A
/// layers' cell takes the room it is given; under a min-content constraint it is given none,
/// so a card whose body holds layers measured them away: a preset picker's card stood past
/// its grid.)
#[test]
fn a_hugging_grid_at_its_smallest_holds_its_cards_whole() {
    for scale in [1.0, 1.5, 2.0] {
        let theme = Theme::dark();
        let _held = crate::widget::enter_device_scale(scale);
        let card = |art: f32, rest: f32| {
            let body = Stack::stack()
                .push(Box::new(Layers::layers().push(boxed(art))) as crate::widget::Element)
                .push(boxed(rest));
            Box::new(crate::widget::Card::new(Box::new(body)).with_title("A preset"))
                as crate::widget::Element
        };
        let mut grid = Grid::columns(2)
            .hugging()
            .push(card(24.3, 40.0))
            .push(card(17.1, 90.7))
            .push(card(16.0, 60.2));
        solve_in(&mut grid, &theme, 601.3, taffy::AvailableSpace::MinContent);
        let bounds = grid.get_rect();
        let placed = rects(&grid);
        for (index, r) in placed.iter().enumerate() {
            assert!(
                r.y1 <= bounds.y1 + 0.01,
                "at {scale}x, card {index} {r:?} is inside the grid {bounds:?}"
            );
        }
        assert!(
            placed[0].y1.max(placed[1].y1) <= placed[2].y0,
            "at {scale}x, the second row stands under the first: {placed:?}"
        );
    }
}

#[test]
fn a_grid_item_spans_columns() {
    let theme = Theme::dark();
    let mut grid = Grid::columns(3)
        .push(boxed(20.0))
        .push_span(boxed(20.0), 2)
        .push(boxed(20.0));
    solve(&mut grid, &theme, 600.0);
    let r = rects(&grid);
    assert!(
        r[1].x0 > r[0].x0,
        "the spanning item starts in the second column"
    );
    assert!(r[2].y0 > r[0].y0, "the next item wraps below");
}

fn split() -> Split {
    Split::new(Length::Rem(18.0), Length::Rem(22.0))
        .push(boxed(20.0))
        .push(boxed(20.0))
}

#[test]
fn a_split_stands_side_by_side_until_the_minimums_no_longer_fit() {
    let theme = Theme::dark();
    let rem = rem_of(&theme);
    let needed = (18.0 + 22.0) * rem + gap(&theme, Rhythm::Section) as f32;
    let mut wide = split();
    solve(&mut wide, &theme, needed + 4.0);
    assert_eq!(wide.layout().arrangement(), Arrangement::SideBySide);
    let r = rects(&wide);
    assert!(r[1].x0 > r[0].x0 && (r[0].y0 - r[1].y0).abs() < 0.5);

    let mut narrow = split();
    solve(&mut narrow, &theme, needed - 4.0);
    assert!(narrow.layout().collapsed());
    let r = rects(&narrow);
    assert!(
        r[1].y0 >= r[0].y1,
        "the secondary pane stands under the primary"
    );
}

#[test]
fn a_split_collapses_earlier_at_larger_text() {
    let theme = Theme::dark();
    let large = Theme::dark().with_text_scale(1.5);
    let width = (18.0 + 22.0) * rem_of(&theme) + 40.0;
    let mut normal = split();
    let mut enlarged = split();
    solve(&mut normal, &theme, width);
    solve(&mut enlarged, &large, width);
    assert!(!normal.layout().collapsed());
    assert!(enlarged.layout().collapsed());
}

#[test]
fn a_split_puts_the_primary_pane_on_its_side() {
    let theme = Theme::dark();
    let mut trailing = split().side(Side::Trailing);
    solve(&mut trailing, &theme, 1400.0);
    let r = rects(&trailing);
    assert!(
        r[0].x0 > r[1].x0,
        "the primary pane is at the trailing edge"
    );
    let mut rtl = split().direction(Direction::Rtl);
    solve(&mut rtl, &theme, 1400.0);
    let r = rects(&rtl);
    assert!(
        r[0].x0 > r[1].x0,
        "leading is the right edge in right-to-left"
    );
}

#[test]
fn a_navigating_split_shows_one_pane_and_a_drawer_opens_the_other() {
    let theme = Theme::dark();
    let mut navigate = split().collapse(Collapse::Navigate);
    solve(&mut navigate, &theme, 300.0);
    assert!(navigate.layout().hidden(1) && !navigate.layout().hidden(0));
    navigate.layout_mut().show(Pane::Secondary);
    solve(&mut navigate, &theme, 300.0);
    assert!(navigate.layout().hidden(0) && !navigate.layout().hidden(1));

    let mut drawer = split().collapse(Collapse::Drawer);
    solve(&mut drawer, &theme, 300.0);
    assert!(drawer.layout().hidden(1));
    drawer.layout_mut().set_drawer_open(true);
    solve(&mut drawer, &theme, 300.0);
    assert!(!drawer.layout().hidden(1));
}

#[test]
fn a_page_is_bounded_to_its_measure_and_centred() {
    let theme = Theme::dark();
    let rem = f64::from(rem_of(&theme));
    let content = || Box::new(Stack::stack().push(boxed(10.0))) as crate::widget::Element;
    let mut page = Page::new().push(content());
    solve(&mut page, &theme, 1920.0);
    let r = rects(&page);
    assert!((r[0].width() - 40.0 * rem).abs() < 1.0, "{}", r[0].width());
    assert!(((r[0].x0 + r[0].x1) / 2.0 - 960.0).abs() < 1.0, "centred");

    let mut full = Page::full().push(content());
    solve(&mut full, &theme, 1920.0);
    assert!(
        rects(&full)[0].width() > 1800.0,
        "page(full) drops the bound"
    );

    let mut phone = Page::new().push(content());
    solve(&mut phone, &theme, 390.0);
    let gutter = gap(&theme, Rhythm::Group);
    assert!(
        (rects(&phone)[0].width() - (390.0 - 2.0 * gutter)).abs() < 1.0,
        "a phone's page keeps the content gutter"
    );
}

#[test]
fn a_filling_page_takes_the_panes_whole_width_and_starts_at_the_page_gutter() {
    let theme = Theme::dark();
    let content = || Box::new(Stack::stack().push(boxed(10.0))) as crate::widget::Element;
    let mut plain = Page::full().push(content());
    let mut filling = Page::full().filling().push(content());
    solve(&mut plain, &theme, 864.0);
    solve(&mut filling, &theme, 864.0);
    let inset = rects(&plain)[0];
    let surface = rects(&filling)[0];
    assert!(
        inset.x0 > surface.x0 + 1.0 && inset.x1 < surface.x1 - 1.0,
        "a page keeps its gutter at its sides: {inset:?} in {surface:?}"
    );
    assert!(
        (surface.x0 - 0.0).abs() < 1.0 && (surface.x1 - 864.0).abs() < 1.0,
        "a page that fills its pane is a surface, whole width to whole width: {surface:?}"
    );
    let edge = f64::from(
        theme
            .tokens
            .spacing
            .rhythm
            .get(crate::tokens::Rhythm::Group),
    );
    assert!(
        (surface.y0 - edge).abs() < 1.0,
        "a canvas starts one content gap under the bar: {surface:?}"
    );
    assert!(
        (surface.y0 - inset.y0).abs() < 1.0,
        "both start the same distance under the bar: {surface:?} over {inset:?}"
    );
}

/// A page keeps one content gutter at every width: a phone, a laptop and a wide monitor hold
/// what they show the same distance in from the column, so what stands at the top of a page has
/// the same room over it as the gap under it wherever the window is.
#[test]
fn a_page_keeps_one_content_gutter_at_every_width() {
    let theme = Theme::dark();
    let content = || Box::new(Stack::stack().push(boxed(10.0))) as crate::widget::Element;
    let gutter = gap(&theme, Rhythm::Group);
    for width in [360.0_f64, 1280.0, 1600.0, 3440.0] {
        let mut page = Page::full().push(content());
        solve(&mut page, &theme, crate::num::f32_of_f64(width));
        let r = rects(&page)[0];
        assert!(
            (r.x0 - gutter).abs() < 1.0 && (r.y0 - gutter).abs() < 1.0,
            "the content gutter at {width}: {r:?}"
        );
        assert!(
            (width - r.x1 - gutter).abs() < 1.0,
            "and the same on the trailing side at {width}: {r:?}"
        );
    }
}

#[test]
fn layers_put_children_over_each_other_at_logical_positions() {
    let theme = Theme::dark();
    let base = Box::new(Stack::stack().push(boxed(200.0))) as crate::widget::Element;
    let mut layers = Layers::layers()
        .push(base)
        .push_at(boxed(20.0), At::BottomTrailing)
        .push_at(boxed(20.0), At::TopLeading);
    solve(&mut layers, &theme, 400.0);
    let r = rects(&layers);
    let host = layers.get_rect();
    assert!((r[1].x1 - host.x1).abs() < 0.5 && (r[1].y1 - host.y1).abs() < 0.5);
    assert!((r[2].x0 - host.x0).abs() < 0.5 && (r[2].y0 - host.y0).abs() < 0.5);

    let mut rtl = Layers::layers()
        .direction(Direction::Rtl)
        .push(Box::new(Stack::stack().push(boxed(200.0))) as crate::widget::Element)
        .push_at(boxed(20.0), At::TopLeading);
    solve(&mut rtl, &theme, 400.0);
    let host = rtl.get_rect();
    assert!(
        (rects(&rtl)[1].x1 - host.x1).abs() < 0.5,
        "leading is the right edge"
    );
}

#[test]
fn a_child_replaced_inserted_removed_or_moved_in_place_is_a_described_change() {
    use crate::widget::StructureChange;
    let theme = Theme::default();
    let mut stack = Stack::stack().push(boxed(10.0)).push(boxed(20.0));
    solve(&mut stack, &theme, 300.0);
    assert_eq!(stack.take_structure_change(), StructureChange::None);

    let kept = std::ptr::from_ref(stack.children()[1]).cast::<()>();
    stack.replace(0, boxed(30.0));
    assert_eq!(stack.take_structure_change(), StructureChange::Described);
    stack.insert(2, Slot::new(boxed(40.0)));
    stack.move_slot(2, 0);
    stack.set_placement(1, Fit::hug(), 2, None);
    assert_eq!(stack.take_structure_change(), StructureChange::Described);
    assert!(stack.remove(0).is_some());
    assert_eq!(stack.take_structure_change(), StructureChange::Described);
    assert_eq!(
        stack.take_structure_change(),
        StructureChange::None,
        "read once"
    );
    assert_eq!(
        std::ptr::from_ref(stack.children()[1]).cast::<()>(),
        kept,
        "the other child is the same widget"
    );
    solve(&mut stack, &theme, 300.0);
    assert_eq!(
        rects(&stack).iter().map(|r| r.height()).collect::<Vec<_>>(),
        vec![30.0, 20.0]
    );

    // Children replaced wholesale say nothing of what changed.
    let slots = stack.take_slots();
    stack.set_slots(slots);
    assert_eq!(stack.take_structure_change(), StructureChange::Rebuild);
}

/// A layout its parent places stays where it is placed when it settles what it shows: a page a
/// virtual list puts at an inset lays its columns out again for the width it was given without
/// taking itself out of the place its parent put it.
#[test]
fn a_container_its_parent_places_stays_where_it_is_placed_when_it_settles() {
    use taffy::style_helpers::length;

    let theme = Theme::dark();
    let mut tree = TaffyTree::new();
    let mut nodes: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let mut page = Grid::auto(Length::Rem(11.0));
    for _ in 0..6 {
        page = page.push(boxed(20.0));
    }
    let node = page.build_layout(&mut tree, &theme, &mut nodes);
    // Placed as a virtual list places a row.
    let mut placed = tree.style(node).expect("styled").clone();
    placed.position = taffy::Position::Absolute;
    placed.inset.top = length(100.0_f32);
    placed.inset.left = length(10.0_f32);
    placed.inset.right = length(10.0_f32);
    placed.size.width = taffy::Dimension::auto();
    tree.set_style(node, placed).expect("placed");
    let parent = tree
        .new_with_children(
            taffy::Style {
                size: taffy::Size::from_lengths(900.0_f32, 600.0_f32),
                ..Default::default()
            },
            &[node],
        )
        .expect("parent");
    let space = taffy::Size {
        width: taffy::AvailableSpace::MaxContent,
        height: taffy::AvailableSpace::MaxContent,
    };
    tree.compute_layout(parent, space).expect("laid out");
    let mut text = crate::font_catalog::fixture_renderer();
    assert!(
        page.adjust_layout_style(&mut tree, node, &mut text),
        "the grid settles its columns"
    );
    tree.compute_layout(parent, space).expect("laid out again");
    let laid = tree.layout(node).expect("laid");
    assert!((laid.location.y - 100.0).abs() < 0.5, "{laid:?}");
    assert!((laid.location.x - 10.0).abs() < 0.5, "{laid:?}");
    assert!((laid.size.width - 880.0).abs() < 0.5, "{laid:?}");
}

/// A leaf that repaints its box while `dirty`.
struct Repaints {
    inner: Spacer,
    dirty: bool,
}

impl Widget for Repaints {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.inner.build_layout(tree, theme, nodes)
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.inner.apply_layout(tree, node, origin);
    }

    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text: &mut crate::text_render::TextRenderer,
        _theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.inner.get_rect()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn name(&self) -> &'static str {
        "Repaints"
    }
}

#[test]
fn a_stack_repaints_what_its_children_repaint_not_its_box() {
    let theme = Theme::dark();
    let leaf = |dirty| {
        Box::new(Repaints {
            inner: Spacer::fixed(20.0),
            dirty,
        }) as crate::widget::Element
    };
    let mut stack = Stack::stack()
        .align(Align::Start)
        .push(leaf(false))
        .push(leaf(true))
        .push(leaf(false));
    solve(&mut stack, &theme, 400.0);
    let mut damage = Vec::new();
    stack.collect_damage(&mut damage);
    assert_eq!(
        damage,
        vec![rects(&stack)[1]],
        "the changed child alone, not the stack's box"
    );
}

/// Every widget's box under `widget`, outermost first.
fn boxes(widget: &dyn Widget, out: &mut Vec<(String, vello::kurbo::Rect)>) {
    out.push((widget.name().to_owned(), widget.get_rect()));
    for child in widget.children() {
        boxes(child, out);
    }
}

#[test]
fn every_edge_lands_on_a_device_pixel_at_the_display_scale() {
    use crate::widget::{Manual, Text};
    use vello::kurbo::Rect;
    // Sizes no whole number of pixels, in a manual region (a window's shell is one) placed at
    // a fraction of a pixel, in text at a size whose lines are no whole number of pixels.
    let content = Page::new().push(Box::new(
        Stack::stack()
            .push(boxed(10.3))
            .push(Box::new(Text::new(
                "A paragraph long enough to wrap onto a few lines in a narrow window",
            )))
            .push(Box::new(Row::row().push(boxed(7.7)).push(boxed(3.35))))
            .push(boxed(5.45)),
    ));
    let region = Manual::new(|ctx| {
        let available = ctx.available();
        ctx.place(
            0,
            Rect::new(
                3.3,
                7.1,
                f64::from(available.width) - 2.2,
                f64::from(available.height) - 1.7,
            ),
        )?;
        Ok(available)
    })
    .child(Box::new(content));
    let mut harness = crate::headless::FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(region),
        (333, 517),
        Theme::light().with_text_scale(1.3),
    );
    for scale in [1.0, 1.5, 2.0, 1.25] {
        harness.set_input_scale(scale).expect("a scale");
        harness.relayout();
        let mut laid = Vec::new();
        boxes(harness.root_widget(), &mut laid);
        assert!(laid.len() > 8, "{laid:?}");
        for (name, rect) in laid {
            for edge in [rect.x0, rect.y0, rect.x1, rect.y1] {
                assert!(
                    (edge * scale - (edge * scale).round()).abs() < 1e-3,
                    "{name} {rect:?}: {edge} is no device pixel at {scale}x"
                );
            }
        }
    }
}

#[test]
fn a_page_is_as_tall_as_its_text_wraps_at_its_measure() {
    use crate::widget::Text;
    // Paragraphs that wrap at the page's readable measure and would not at the window's width.
    let body = "The fifth works for the beta. I would move the release to the fourteenth so the \
                docs can catch up, and the help pages with them.";
    for text_scale in [1.0, 1.5] {
        let theme = Theme::light().with_text_scale(text_scale);
        let mut page = Page::new().push(Box::new(
            Stack::stack()
                .push(Box::new(Text::new(body)))
                .push(Box::new(Text::new(body))),
        ));
        solve(&mut page, &theme, 2510.0);
        let column = rects(&page)[0];
        let text = column.height();
        let bounds = page.get_rect();
        assert!(
            column.width() < 1000.0,
            "the column is at the measure: {column:?}"
        );
        assert!(
            column.y1 <= bounds.y1 - (column.y0 - bounds.y0) + 0.5,
            "at text {text_scale}, the page {bounds:?} holds its column {column:?} ({text} high) \
             and the gutter below it"
        );
    }
}

/// A spacer that takes a row's leftover room is room, not a sibling: the row's gap stands once
/// between what comes before it and what follows, so a row with no room to spare still holds
/// both, and with room the spacer sends what follows it to the far end — in either direction.
#[test]
fn a_spacer_takes_the_leftover_room_and_no_gap_of_its_own() {
    let theme = Theme::dark();
    let between = gap(&theme, Rhythm::Related);
    for direction in [Direction::Ltr, Direction::Rtl] {
        let row = || {
            let mut spacer = Slot::new(Box::new(Spacer::new()));
            spacer.fit = Fit::fill(1.0);
            Stack::row()
                .fill_width()
                .direction(direction)
                .push(boxed(100.0))
                .push_slot(spacer)
                .push(boxed(100.0))
        };
        // Exactly room for both and one gap.
        #[allow(clippy::cast_possible_truncation, reason = "a small test width")]
        let tight = (200.0 + between) as f32;
        let mut short = row();
        solve(&mut short, &theme, tight);
        let r = rects(&short);
        let (left, right) = if direction == Direction::Ltr {
            (r[0], r[2])
        } else {
            (r[2], r[0])
        };
        assert!(
            left.x0.abs() < 0.5 && (right.x1 - f64::from(tight)).abs() < 0.5,
            "{direction:?}: {r:?}"
        );
        assert!(
            (right.x0 - left.x1 - between).abs() < 0.5,
            "one gap between them: {r:?}"
        );
        let mut roomy = row();
        solve(&mut roomy, &theme, 600.0);
        let r = rects(&roomy);
        let (left, right) = if direction == Direction::Ltr {
            (r[0], r[2])
        } else {
            (r[2], r[0])
        };
        assert!(
            left.x0.abs() < 0.5 && (right.x1 - 600.0).abs() < 0.5,
            "{direction:?}: {r:?}"
        );
    }
}

/// A text field has no width of its own content; where a row that wraps lays it out at its own
/// width it stands at the theme's least width for a field, never 0 — hugging or filling, with
/// the show/hide control or without, beside a button — at a phone's widths and a desktop's.
#[test]
fn every_text_field_in_a_wrapping_row_stands_at_least_its_own_width() {
    use crate::widget::{Button, TextInput};
    let theme = Theme::dark();
    let least = f64::from(TextInput::own_width_in(&theme));
    assert!(least > 0.0, "a field's own width is never 0");
    let field = || TextInput::new(vec![crate::PathSegment::Field("name".into())]);
    for width in [320.0, 390.0, 1280.0] {
        let mut row = Row::row()
            .fill_width()
            .wrap()
            .push(Box::new(field().with_placeholder("Name a new workspace")))
            .push(Box::new(field().with_secret(true)))
            .push_fit(Box::new(field()), Fit::hug().width(Sizing::fill(1.0)))
            .push(Box::new(Button::new("create", "Create")));
        solve(&mut row, &theme, width);
        let r = rects(&row);
        for (index, field) in r.iter().take(3).enumerate() {
            assert!(
                field.width() + 0.5 >= least,
                "field {index} at {width} px is {}, less than its {least}",
                field.width()
            );
            assert!(
                field.width() <= f64::from(width) + 0.5,
                "field {index} at {width} px stays inside the row: {}",
                field.width()
            );
        }
    }
}

/// A view that fills the slot it is in on both axes, by its stable id: the video box.
const SUBJECT: &str = "aspect-subject";

fn filling_view() -> crate::widget::Element {
    Box::new(
        Stack::stack()
            .fill_width()
            .fill_height()
            .with_id(SUBJECT)
            .push(boxed(1.0)),
    )
}

fn subject_rect(root: &dyn Widget) -> vello::kurbo::Rect {
    crate::widget::find_test_id(root, SUBJECT)
        .expect("the view in the slot is laid out")
        .get_rect()
}

/// A child that says `aspect: 16/9` and `fill` is 16:9 wide in the slot it fills. The slot's
/// width is the container's, decided during layout, so the ratio decides the height the slot
/// takes once that width is solved.
#[test]
fn a_child_that_fills_at_an_aspect_keeps_its_ratio_in_a_column() {
    let theme = Theme::dark();
    let mut stack = Stack::stack()
        .fill_width()
        .push_fit(filling_view(), Fit::fill(1.0).aspect(16.0 / 9.0))
        .push(boxed(30.0));
    solve(&mut stack, &theme, 800.0);
    let r = subject_rect(&stack);
    assert!(
        (r.width() - r.height() * 16.0 / 9.0).abs() < 1.0,
        "a filled child at 16:9 in a column is {r:?}"
    );
    assert!(
        (r.width() - 800.0).abs() < 1.0,
        "it takes the column's width: {r:?}"
    );
}

/// The same in a row, where the ratio takes the width the row gives the slot and the row's
/// height follows it.
#[test]
fn a_child_that_fills_at_an_aspect_keeps_its_ratio_in_a_row() {
    let theme = Theme::dark();
    let mut row = Row::row()
        .fill_width()
        .align(Align::Start)
        .push_fit(filling_view(), Fit::fill(1.0).aspect(16.0 / 9.0));
    solve(&mut row, &theme, 800.0);
    let r = subject_rect(&row);
    assert!(
        (r.width() - r.height() * 16.0 / 9.0).abs() < 1.0,
        "a filled child at 16:9 in a row is {r:?}"
    );
}

/// The same in a canvas that fills its pane and scrolls inside its own parts, where the layers
/// gets no width of its own from the page: the ratio must not feed its width back from the
/// height it derived, or the box shrinks to a fraction of the room and grows back every solve.
#[test]
fn an_aspect_layer_in_a_filling_page_keeps_its_width_and_ratio() {
    let theme = Theme::dark();
    let inner = Layers::layers().push_fit(filling_view(), Fit::fill(1.0).aspect(16.0 / 9.0));
    let column = Stack::stack()
        .fill_width()
        .fill_height()
        .push(Box::new(inner))
        .push(boxed(40.0));
    let mut page = Page::full().filling().push(Box::new(column));
    solve_in(
        &mut page,
        &theme,
        1000.0,
        taffy::AvailableSpace::Definite(744.0),
    );
    let r = subject_rect(&page);
    assert!(
        (r.width() - r.height() * 16.0 / 9.0).abs() < 1.0,
        "a layer at 16:9 in a filling page is {r:?}"
    );
    assert!(
        (r.width() - 1000.0).abs() < 1.0,
        "it takes the page's width: {r:?}"
    );
}

/// The video page's own shape: a canvas page holding a column whose first child is the 16:9
/// player box, and, in that box, the poster filling the cell and Play centred over it. The box
/// takes the page content's width and its 16:9 height; Play keeps its own size in the middle
/// rather than stretching across the box into a tall strip, and the picture takes the box.
#[test]
fn the_video_box_is_the_page_contents_width_at_sixteen_by_nine() {
    use crate::widget::Button;
    for (width, height) in [(1000.0_f32, 744.0_f32), (320.0, 512.0)] {
        let theme = Theme::dark();
        let play = Row::row()
            .fill_width()
            .fill_height()
            .align(Align::Center)
            .distribute(Distribute::Center)
            .push(Box::new(Button::new("play", "Play")));
        let player = Layers::layers()
            .with_id("player")
            .push_fit(filling_view(), Fit::fill(1.0).aspect(16.0 / 9.0))
            .push_fit(Box::new(play), Fit::fill(1.0));
        let column = Stack::stack()
            .fill_width()
            .push(Box::new(player))
            .push(Box::new(Stack::stack().fill_width().push(boxed(80.0))));
        let mut page = Page::full().push(Box::new(column));
        solve_in(
            &mut page,
            &theme,
            width,
            taffy::AvailableSpace::Definite(height),
        );
        let box_rect = crate::widget::find_test_id(&page, "player")
            .expect("the player box is laid out")
            .get_rect();
        let play = crate::widget::find_test_id(&page, "play")
            .expect("the play button is laid out")
            .get_rect();
        let view = subject_rect(&page);
        assert!(
            (box_rect.width() / box_rect.height() - 16.0 / 9.0).abs() < 0.02,
            "at {width} the player box is {}x{}, not 16:9",
            box_rect.width(),
            box_rect.height()
        );
        assert!(
            (view.width() - box_rect.width()).abs() < 1.0,
            "at {width} the picture is {} wide in a {} box",
            view.width(),
            box_rect.width()
        );
        assert!(
            play.width() < box_rect.width() * 0.9 && play.height() < box_rect.height() * 0.9,
            "at {width} Play is {}x{} in a {}x{} box: it stretches across the picture",
            play.width(),
            play.height(),
            box_rect.width(),
            box_rect.height()
        );
        assert!(
            (play.center().x - box_rect.center().x).abs() < 1.0,
            "at {width} Play stands at {} in a box centred at {}",
            play.center().x,
            box_rect.center().x
        );
    }
}

/// A canvas page that does not fill keeps its reading gutter beside its content; a filling one
/// lays its own parts out from the page gutter and drops the side gutters. The video page is a
/// canvas because its player is wide, not because it fills, so it must keep them.
#[test]
fn a_canvas_page_that_does_not_fill_keeps_its_side_gutter() {
    let theme = Theme::dark();
    for filling in [false, true] {
        let player = Layers::layers()
            .with_id("gutter-box")
            .push_fit(filling_view(), Fit::fill(1.0).aspect(16.0 / 9.0));
        let column = Stack::stack()
            .fill_width()
            .push(Box::new(player))
            .push(Box::new(Stack::stack().fill_width().push(boxed(80.0))));
        let page = Page::full();
        let page = if filling { page.filling() } else { page };
        let mut page = page.push(Box::new(column));
        solve_in(
            &mut page,
            &theme,
            1000.0,
            taffy::AvailableSpace::Definite(744.0),
        );
        let page_rect = page.get_rect();
        let box_rect = crate::widget::find_test_id(&page, "gutter-box")
            .expect("the box is laid out")
            .get_rect();
        let inset = box_rect.x0 - page_rect.x0;
        if filling {
            assert!(
                inset < 1.0,
                "a filling page lays its parts from its own edge: inset {inset}"
            );
        } else {
            assert!(
                inset > 1.0,
                "a canvas page keeps its reading gutter, but the box stands at inset {inset}"
            );
        }
    }
}

/// A layer that fills the cell at an aspect takes the cell's width and the height the ratio
/// gives, not the whole cell's height: `layers(fill, aspect: 16/9)` is the video box.
#[test]
fn a_layer_that_fills_at_an_aspect_keeps_its_ratio() {
    let theme = Theme::dark();
    let layers = Layers::layers().push_fit(filling_view(), Fit::fill(1.0).aspect(16.0 / 9.0));
    // The layers stand in a page's column, which gives them the width they lay out in.
    let mut page = Stack::stack()
        .fill_width()
        .fill_height()
        .push(Box::new(layers));
    solve_in(
        &mut page,
        &theme,
        800.0,
        taffy::AvailableSpace::Definite(600.0),
    );
    let r = subject_rect(&page);
    assert!(
        (r.width() - r.height() * 16.0 / 9.0).abs() < 1.0,
        "a filled layer at 16:9 is {r:?}"
    );
    assert!(
        (r.width() - 800.0).abs() < 1.0,
        "it takes the cell's width: {r:?}"
    );
}

/// The page row a virtual list wraps each row in is a page whose slot takes the row's whole
/// width. In a row beside another child, that `width: 100%` is measured against a width the
/// row is still deciding, and the page comes out as tall as its content at the narrowest width
/// it was measured at: the row's own height must not come from that measurement.
#[test]
fn a_page_rows_width_does_not_decide_its_height_in_a_row() {
    let theme = Theme::dark();
    for container_height in [200.0_f32, 504.0] {
        let content = Stack::stack()
            .with_id("page-row-content")
            .push(boxed(16.0))
            .push(boxed(35.0))
            .push(boxed(28.0));
        let page = Page::full()
            .row_after(Rhythm::Section)
            .push(Box::new(content));
        let mut line = Stack::row()
            .fill_width()
            .align(Align::Center)
            .push(Box::new(page))
            .push(Box::new(Spacer::fixed(36.0)));
        solve_in(
            &mut line,
            &theme,
            436.0,
            taffy::AvailableSpace::Definite(container_height),
        );
        let content = crate::widget::find_test_id(&line, "page-row-content")
            .expect("the row's content is laid out")
            .get_rect();
        assert!(
            content.height() < 120.0,
            "at a {container_height}-tall container the row's content is {} tall",
            content.height()
        );
    }
}

/// The same rule for the wrapper a virtual list puts a row in: the row's page
/// ([`Page::full().row_after`]) holds the row at the row's own height, and a column inside a
/// row beside a chip takes the row's height, never the list's. This is the chat transcript's
/// shape: a message's column, a reaction chip trailing it, in a page row inside the list.
#[test]
fn a_message_column_in_a_page_row_keeps_the_rows_height() {
    let theme = Theme::dark();
    for rows in [1_usize, 12] {
        let items: Vec<u32> = (0..rows as u32).collect();
        let model = crate::widget::VecListModel::new(items, |n| *n).expect("a list model");
        let list = crate::widget::VirtualList::new(model, 40.0, |_, index, _| {
            let column = Stack::stack()
                .with_id(format!("msg{index}"))
                .push(boxed(16.0))
                .push(boxed(35.0))
                .push(boxed(28.0));
            let line = Stack::row()
                .fill_width()
                .align(Align::Center)
                .push(Box::new(column))
                .push(Box::new(Spacer::fixed(36.0)));
            Box::new(Page::full().row_after(Rhythm::Section).push(Box::new(line)))
                as crate::widget::Element
        });
        let mut docked = Stack::stack()
            .fill_width()
            .fill_height()
            .push_fit(Box::new(list), Fit::fill(1.0))
            .push_fit(Box::new(Spacer::fixed(60.0)), Fit::hug());
        solve_in(
            &mut docked,
            &theme,
            540.0,
            taffy::AvailableSpace::Definite(504.0),
        );
        let column = crate::widget::find_test_id(&docked, "msg0")
            .expect("the first message is laid out")
            .get_rect();
        assert!(
            column.height() < 120.0,
            "with {rows} rows the message column is {} tall: it took the list's room, not its \
             own content's",
            column.height()
        );
    }
}

/// A row of a list that fills its column keeps its own height, not the column's: a chat
/// message's row in a transcript is as tall as what it says, however tall the list around it
/// is. A slot asking for no aspect gets no derived height, so nothing about a ratio may reach
/// it — the same rule for a row inside a virtual list as for any other slot.
#[test]
fn a_rows_slot_keeps_its_own_height_beside_a_foot() {
    use crate::widget::{Button, VecListModel, VirtualList};
    let theme = Theme::dark();
    for rows in [1_usize, 12] {
        let items: Vec<u32> = (0..rows as u32).collect();
        let model = VecListModel::new(items, |n| *n).expect("a list model");
        let list = VirtualList::new(model, 40.0, |_, index, _| {
            Box::new(
                Stack::stack()
                    .fill_width()
                    .with_id(format!("row{index}"))
                    .push(boxed(40.0)),
            ) as crate::widget::Element
        });
        let docked = Stack::stack()
            .fill_width()
            .fill_height()
            .push_fit(Box::new(list), Fit::fill(1.0))
            .push_fit(Box::new(Button::new("send", "Send")), Fit::hug());
        let mut column = Stack::stack()
            .fill_width()
            .fill_height()
            .push(Box::new(docked));
        solve_in(
            &mut column,
            &theme,
            540.0,
            taffy::AvailableSpace::Definite(560.0),
        );
        let row = crate::widget::find_test_id(&column, "row0")
            .expect("the first row is laid out")
            .get_rect();
        assert!(
            row.height() < 120.0,
            "with {rows} rows the first is {} tall: it took the list's room, not its own",
            row.height()
        );
    }
}
