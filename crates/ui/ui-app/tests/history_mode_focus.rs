//! Focus order through History: what it enumerates once per recomputation and which nodes a
//! saved or captured record denies.
use std::collections::HashMap;
use taffy::TaffyTree;
use tilo_ui::history_mode::testing::*;
use tilo_ui::history_mode::*;
use tilo_ui::{widget::Button, widget::Column};
use tilo_ui_base::widget::Widget;

#[test]
fn headless_focus_order_enumerates_history_once() {
    let state = tilo_ui::State::new();
    let (root, visits) = FocusHistoryRoot::new(2_000, &state);
    let mut frame = tilo_ui::headless::Driver::new_with_controlled_inputs(
        state,
        Box::new(root),
        (900, 650),
        tilo_ui_base::Theme::dark(),
        tilo_ui_base::font_catalog::fixture_renderer(),
        tilo_ui_base::permissions::MemberId::person("fixture"),
    );
    // A frame also reads History for its sync, routes and publication; the order's own
    // recomputation is what is counted here.
    frame.relayout();
    visits.set(0);
    frame.app_mut().refresh_focus_order();
    let order = frame.focus_order();
    assert!(order.len() >= 2_000, "all live rows remain focusable");
    assert_eq!(visits.get(), 1, "one History enumeration per recomputation");
}

#[test]
fn headless_relayout_and_focus_moves_enumerate_history_independently_of_rows() {
    // Both recover or move focus by asking every node whether History
    // admits it; the surfaces must be enumerated per pass, not per node.
    let visits_for = |rows| {
        let state = tilo_ui::State::new();
        let (root, visits) = FocusHistoryRoot::new(rows, &state);
        let mut frame = tilo_ui::headless::Driver::new_with_controlled_inputs(
            state,
            Box::new(root),
            (900, 650),
            tilo_ui_base::Theme::dark(),
            tilo_ui_base::font_catalog::fixture_renderer(),
            tilo_ui_base::permissions::MemberId::person("fixture"),
        );
        visits.set(0);
        frame.relayout();
        let relayout = visits.get();
        visits.set(0);
        assert!(frame.tab(false).is_some(), "a live row takes focus");
        (relayout, visits.get())
    };
    assert_eq!(visits_for(2_000), visits_for(20));
}

#[test]
fn saved_and_captured_history_deny_only_live_composition_nodes() {
    let state = tilo_ui::State::new();
    let (mut root, _) = FocusHistoryRoot::new(3, &state);
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    root.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
    let outside = tree.new_leaf(taffy::Style::default()).unwrap();
    let live = root.history.live_layout_node().unwrap();
    let row = tree.children(live).unwrap()[0];
    let owned = nodes
        .keys()
        .copied()
        .find(|&node| root.history.owns_node(node, &tree))
        .unwrap();
    let allows = |root: &FocusHistoryRoot, node| {
        let restrictions = FocusRestrictions::collect(root, &tree);
        assert!(has_surfaces(root));
        let allowed = restrictions.allows(node);
        assert_eq!(allowed, allows_focus(root, node, &tree));
        allowed
    };
    assert!(allows(&root, row));
    root.live.children_mut()[0].set_focus(true);
    root.history.preserve_captured_focus(Some(
        tilo_ui_base::widget::layout::CompositionFocus::capture_live(&root.live).unwrap(),
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

#[test]
fn nested_history_ownership_does_not_escape_outer_focus_denial() {
    let state = tilo_ui::State::new();
    let (inner, _) = FocusHistoryRoot::new(1, &state);
    let (mut outer, _) = FocusHistoryRoot::new(0, &state);
    outer.live = Column::new().push(Box::new(inner));
    let (sibling, _) = FocusHistoryRoot::new(1, &state);
    let mut root = Column::new()
        .push(Box::new(outer))
        .push(Box::new(sibling))
        .push(Box::new(Button::new("outside", "Outside")));
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    let root_node = root.build_layout(&mut tree, &tilo_ui_base::Theme::dark(), &mut nodes);
    let outside = *tree.children(root_node).unwrap().last().unwrap();
    let mut owned_and_live = Vec::new();
    tilo_ui::history_mode::capability::visit_framework_history(&root, &mut |history, _| {
        owned_and_live.push((
            nodes
                .keys()
                .copied()
                .find(|&node| history.owns_node(node, &tree))
                .unwrap(),
            tree.children(history.live_layout_node().unwrap()).unwrap()[0],
        ));
    });
    assert_eq!(owned_and_live.len(), 3, "outer, inner, independent sibling");
    let mut index = 0;
    tilo_ui::history_mode::capability::visit_framework_history(&root, &mut |history, _| {
        if index < 2 {
            assert!(history.request_open());
        }
        index += 1;
    });
    let restrictions = FocusRestrictions::collect(&root, &tree);
    for node in nodes.keys().copied().chain([outside]) {
        assert_eq!(
            restrictions.allows(node),
            allows_focus(&root, node, &tree),
            "snapshot preserves per-surface ownership for {node:?}"
        );
    }
    let allows = |node| restrictions.allows(node);
    assert!(
        allows(owned_and_live[0].0),
        "outer retains its own controls"
    );
    assert!(
        !allows(owned_and_live[1].0),
        "inner ownership cannot override outer capture"
    );
    assert!(
        !allows(owned_and_live[1].1),
        "nested live control is denied"
    );
    assert!(
        allows(owned_and_live[2].0),
        "independent History controls remain allowed"
    );
    assert!(
        allows(owned_and_live[2].1),
        "independent live controls remain allowed"
    );
    assert!(allows(outside));
}
