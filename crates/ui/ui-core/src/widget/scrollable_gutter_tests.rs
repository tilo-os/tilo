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
