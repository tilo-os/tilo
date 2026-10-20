//! The primitives laid out by the real solver, at several widths, text sizes and directions.

use std::collections::HashMap;

use taffy::TaffyTree;
use vello::kurbo::Point;

use super::*;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::{Spacer, Widget};

fn solve(root: &mut dyn Widget, theme: &Theme, width: f32) {
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
                height: taffy::AvailableSpace::MaxContent,
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
        if rem >= 15.9 && rem <= 16.1 {
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
    let gutter = gap(&theme, WidthClass::of(390.0 / rem as f32).gutter());
    assert!((rects(&phone)[0].width() - (390.0 - 2.0 * gutter)).abs() < 1.0);
}

#[test]
fn a_page_gutter_grows_with_the_space_class() {
    let theme = Theme::dark();
    let content = || Box::new(Stack::stack().push(boxed(10.0))) as crate::widget::Element;
    let (mut narrow, mut wide) = (Page::full().push(content()), Page::full().push(content()));
    solve(&mut narrow, &theme, 360.0);
    solve(&mut wide, &theme, 1600.0);
    assert!(rects(&narrow)[0].x0 < rects(&wide)[0].x0);
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
