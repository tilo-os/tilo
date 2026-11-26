//! The bar's gutter across the passes of one layout.
use super::test_support::*;

use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};

use super::Scrollable;
use crate::{widget::Column, widget::Text};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;

/// A parent sizing itself to its content lays the view out in a box other than the one it gets.
#[test]
fn a_gutter_needed_only_in_a_box_measured_on_the_way_is_given_back() {
    let mut lines = Column::new().with_intrinsic_height();
    for i in 0..4 {
        lines = lines.push(Box::new(Text::new(&format!("line {i}"))));
    }
    let mut scroll = Scrollable::new_vertical().with_child(Box::new(lines));
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = scroll.build_layout(&mut taffy, &Theme::dark(), &mut nodes);

    assert!(
        pass(&mut scroll, &mut taffy, node, &nodes, 20.0),
        "the short box overflows: a gutter"
    );
    assert!(
        !pass(&mut scroll, &mut taffy, node, &nodes, 300.0),
        "the content fits the box it gets, so its bar's gutter is given back"
    );
}

/// A page in a navigation shell, as Clocks shows its timers: the shell's first solve gives the
/// scroll view no width, where the wrapped page is far taller than the window, and the next
/// gives it its place, where the page fits.
#[test]
fn content_that_overflowed_before_the_view_had_its_place_keeps_no_gutter() {
    let page = || -> tilo_ui_base::widget::Element {
        let mut column = Column::new().with_intrinsic_height();
        for _ in 0..6 {
            column = column.push(Box::new(Text::new(
                "A paragraph long enough to wrap onto many lines when the view it \
                 is laid out in has no width yet, and onto one or two once it has.",
            )));
        }
        Box::new(column)
    };
    let shell = crate::widget::NavShell::new()
        .content_kind(tilo_ui_base::widget::ContentKind::Page)
        .mode(crate::widget::NavMode::Push)
        .sidebar(Box::new(crate::widget::NavList::new(vec![
            crate::widget::NavSection::untitled(
                "sections",
                ["One", "Two", "Three"]
                    .into_iter()
                    .map(|label| crate::widget::NavItem::new(label.to_owned(), label))
                    .collect(),
            ),
        ])))
        .content(Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(page()),
        ));
    let mut harness = crate::app::driver::Driver::new_with_theme(
        crate::State::new(),
        Box::new(shell),
        (900, 760),
        Theme::dark(),
    );
    assert!(harness.take_skip_report().is_none());
    for width in [880, 700, 520] {
        harness
            .resize((width, 760))
            .unwrap_or_else(|error| panic!("{width} px: {error:?}"));
        let gutter = harness
            .with_widget_mut(|scroll: &mut Scrollable| scroll.reserve_vertical)
            .unwrap();
        assert!(!gutter, "{width} px: the page fits, so no gutter is kept");
    }
}
