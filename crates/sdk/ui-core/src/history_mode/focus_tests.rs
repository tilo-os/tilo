use super::{FrameworkHistorySurface, capability::FrameworkHistoryTopology};
use crate::widget::{Button, Column, Widget};
use std::{cell::Cell, collections::HashMap, rc::Rc};
use taffy::{NodeId, TaffyTree};

/// Real History composition with a probe at its lifecycle enumeration boundary.
pub(crate) struct FocusHistoryRoot {
    pub(crate) history: FrameworkHistorySurface,
    pub(crate) live: Column,
    visits: Rc<Cell<usize>>,
}

impl FocusHistoryRoot {
    pub(crate) fn new(rows: usize, state: &crate::State) -> (Self, Rc<Cell<usize>>) {
        let mut live = Column::new();
        for row in 0..rows {
            live = live.push(Box::new(Button::new(&format!("row-{row}"), "Row")));
        }
        let visits = Rc::new(Cell::new(0));
        let history =
            FrameworkHistorySurface::new(super::host_tests::registration(), &state.doc).unwrap();
        (
            Self {
                history,
                live,
                visits: visits.clone(),
            },
            visits,
        )
    }
}

impl FrameworkHistoryTopology for FocusHistoryRoot {
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&FrameworkHistorySurface, &dyn Widget),
    ) {
        self.visits.set(self.visits.get() + 1);
        visitor(&self.history, &self.live);
    }

    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut FrameworkHistorySurface, &mut dyn Widget),
    ) {
        visitor(&mut self.history, &mut self.live);
    }
}

impl Widget for FocusHistoryRoot {
    crate::provides!(dyn FrameworkHistoryTopology);
    crate::testing::inert_widget_methods!(draw, name = "History focus probe");

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &crate::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.history
            .build_composition(&mut self.live, tree, theme, nodes)
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, root: NodeId, origin: vello::kurbo::Point) {
        self.history
            .apply_composition(&mut self.live, tree, root, origin);
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.history.get_rect()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.history, &self.live]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.history, &mut self.live]
    }
}

#[test]
fn headless_focus_order_enumerates_history_once() {
    let state = crate::State::new();
    let (root, visits) = FocusHistoryRoot::new(2_000, &state);
    let frame = crate::headless::FrameHarness::new_with_controlled_inputs(
        state,
        Box::new(root),
        (900, 650),
        crate::Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::UserId("fixture".into()),
    );
    visits.set(0);
    let order = frame.focus_order();
    assert!(order.len() >= 2_000, "all live rows remain focusable");
    assert_eq!(visits.get(), 1, "one History enumeration per recomputation");
}

#[test]
fn saved_and_captured_history_deny_only_live_composition_nodes() {
    let state = crate::State::new();
    let (mut root, _) = FocusHistoryRoot::new(3, &state);
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    root.build_layout(&mut tree, &crate::Theme::dark(), &mut nodes);
    let outside = tree.new_leaf(taffy::Style::default()).unwrap();
    let live = root.history.live_layout_node().unwrap();
    let row = tree.children(live).unwrap()[0];
    let owned = nodes
        .keys()
        .copied()
        .find(|&node| root.history.owns_node(node, &tree))
        .unwrap();
    let allows = |root: &FocusHistoryRoot, node| super::embedded::allows_focus(root, node, &tree);
    assert!(allows(&root, row));
    root.live.children_mut()[0].set_focus(true);
    root.history.preserve_captured_focus(Some(
        crate::widget::layout::CompositionFocus::capture_live(&root.live).unwrap(),
    ));
    assert!(root.history.has_saved_focus());
    assert!(!root.history.captures_input());
    assert!(
        !allows(&root, row),
        "saved focus alone restricts the live app"
    );
    assert!(allows(&root, owned));
    assert!(allows(&root, outside));
    root.history.discard_saved_focus();
    assert!(
        allows(&root, row),
        "releasing saved focus allows the live app"
    );
    assert!(root.history.request_open());
    assert!(root.history.captures_input());
    assert!(!root.history.has_saved_focus());
    assert!(
        !allows(&root, row),
        "input capture alone restricts the live app"
    );
    assert!(allows(&root, owned));
    assert!(allows(&root, outside));
}
