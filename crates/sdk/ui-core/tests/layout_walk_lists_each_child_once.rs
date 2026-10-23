//! A retained layout is trusted again only after a walk of
//! `for_each_layout_child` meets every widget it names. A widget listed
//! twice there means its owners disagree about who lays it out; debug
//! builds stop on it rather than let the disagreement hide.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use tilo_ui::State;
use tilo_ui::headless::FrameHarness;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{TaffyTreeExt, Widget};

/// A sized leaf: zero-sized widgets share addresses and are not counted.
struct Leaf {
    _sized: u8,
}

impl Widget for Leaf {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn name(&self) -> &'static str {
        "Leaf"
    }
}

/// Lays out one leaf and lists it `listed` times.
struct Owner {
    leaf: Leaf,
    listed: usize,
}

impl Widget for Owner {
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        for _ in 0..self.listed {
            visitor(&self.leaf);
        }
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let leaf = self.leaf.build_layout(taffy, theme, node_map);
        taffy.widget_node(Style::default(), &[leaf], node_map, self)
    }

    fn name(&self) -> &'static str {
        "Owner"
    }
}

fn lay_out(listed: usize) {
    let mut harness = FrameHarness::for_root(
        State::new(),
        Owner {
            leaf: Leaf { _sized: 0 },
            listed,
        },
        (200, 100),
        Theme::dark(),
    );
    harness.relayout();
}

#[test]
fn a_child_listed_once_is_walked() {
    lay_out(1);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "listed more than once by for_each_layout_child")]
fn a_child_listed_twice_stops_a_debug_build() {
    lay_out(2);
}
