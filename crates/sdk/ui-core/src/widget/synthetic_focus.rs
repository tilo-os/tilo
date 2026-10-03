//! Focus helpers for immediate-mode widgets with synthetic accessible children.

use crate::a11y::AccessNode;
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, RoundedRect, Stroke};

/// Resolve an ordinal child path supplied to
/// [`Widget::focus_synthetic_accessibility`](super::Widget::focus_synthetic_accessibility)
/// to its stable semantic action target.
pub fn semantic_target_at_path<'a>(root: &'a AccessNode, path: &[usize]) -> Option<&'a str> {
    let mut node = root;
    for &index in path {
        node = node.children.get(index)?;
    }
    node.action_target.as_deref()
}

/// Mark exactly one semantic target focused, clearing stale focus flags from
/// its siblings. Returns whether the live tree still contains the target.
pub fn mark_semantic_focus(root: &mut AccessNode, target: &str) -> bool {
    fn visit(node: &mut AccessNode, target: &str, found: &mut bool) {
        let matches = node.action_target.as_deref() == Some(target);
        node.state.focused = matches;
        *found |= matches;
        for child in &mut node.children {
            visit(child, target, found);
        }
    }
    let mut found = false;
    visit(root, target, &mut found);
    found
}

/// Stable, enabled keyboard order for actionable synthetic descendants.
/// Structural and static nodes are omitted; semantic tree order is canonical.
pub fn semantic_focus_order(root: &AccessNode) -> Vec<String> {
    fn visit(node: &AccessNode, targets: &mut Vec<String>) {
        if node.state.focusable
            && !node.state.disabled
            && let Some(target) = &node.action_target
        {
            targets.push(target.clone());
        }
        for child in &node.children {
            visit(child, targets);
        }
    }

    let mut targets = Vec::new();
    visit(root, &mut targets);
    targets
}

/// Bounds published by one semantic action target, searching recursively.
/// Composite widgets use this after keyboard/a11y focus changes to keep the
/// newly focused immediate-mode control inside their scroll viewport.
pub fn semantic_target_bounds(root: &AccessNode, target: &str) -> Option<vello::kurbo::Rect> {
    if root.action_target.as_deref() == Some(target) {
        return root.bounds.map(crate::a11y::AccessBounds::to_rect);
    }
    root.children
        .iter()
        .find_map(|child| semantic_target_bounds(child, target))
}

/// Move one step through the live semantic order, wrapping within the
/// composite. A stale or absent current target resolves to the first item.
pub fn adjacent_semantic_target(
    root: &AccessNode,
    current: Option<&str>,
    forward: bool,
) -> Option<String> {
    let targets = semantic_focus_order(root);
    if targets.is_empty() {
        return None;
    }
    let current = current.and_then(|target| targets.iter().position(|item| item == target));
    let index = match (current, forward) {
        (Some(index), true) => {
            let next = index.saturating_add(1);
            if next >= targets.len() { 0 } else { next }
        }
        (Some(0), false) => targets.len().saturating_sub(1),
        (Some(index), false) => index.saturating_sub(1),
        (None, _) => 0,
    };
    targets.get(index).cloned()
}

/// Keep synthetic focus coherent while an immediate-mode collection mutates.
///
/// Callers capture the semantic tree before a deletion, perform the mutation,
/// then pass the freshly-built tree here. If the focused control disappeared
/// (or its ordinal target was re-used by the following row), focus moves to the
/// control now occupying the same place in keyboard order, falling back to the
/// preceding control at the end of a collection.
pub fn recover_semantic_focus(
    before: &AccessNode,
    after: &AccessNode,
    current: Option<&str>,
) -> Option<String> {
    let before_order = semantic_focus_order(before);
    let after_order = semantic_focus_order(after);
    let current = current?;
    let index = before_order.iter().position(|target| target == current)?;

    if before_order == after_order {
        return Some(current.to_owned());
    }
    after_order
        .get(index.min(after_order.len().saturating_sub(1)))
        .cloned()
}

/// Paint the standard focus ring around a semantic target's published bounds.
/// This is the immediate-mode counterpart to retained widgets' focus paint.
pub fn paint_semantic_focus(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    root: &AccessNode,
    target: &str,
) -> bool {
    let Some(rect) = semantic_target_bounds(root, target) else {
        return false;
    };
    scene.stroke(
        &Stroke::new(2.0),
        transform,
        theme.border_focus,
        None,
        &RoundedRect::from_rect(rect, theme.radius_standard),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessState, Role};
    use vello::kurbo::Rect;

    #[test]
    fn synthetic_focus_targets_and_recovers_in_the_live_tree() {
        // Case `path_target_and_focus_marking_share_the_live_tree`.
        {
            let mut root = AccessNode::new(Role::Group).with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name("One")
                    .with_action_target("one")
                    .with_bounds(Rect::new(0.0, 0.0, 40.0, 30.0))
                    .with_state(AccessState::new().focusable(true)),
                AccessNode::new(Role::Button)
                    .with_name("Two")
                    .with_action_target("two")
                    .with_bounds(Rect::new(50.0, 0.0, 90.0, 30.0))
                    .with_state(AccessState::new().focusable(true)),
            ]);
            assert_eq!(semantic_target_at_path(&root, &[1]), Some("two"));
            assert_eq!(
                semantic_target_bounds(&root, "two"),
                Some(Rect::new(50.0, 0.0, 90.0, 30.0))
            );
            assert!(mark_semantic_focus(&mut root, "two"));
            assert!(!root.children[0].state.focused);
            assert!(root.children[1].state.focused);
            assert_eq!(semantic_focus_order(&root), vec!["one", "two"]);
            assert_eq!(
                adjacent_semantic_target(&root, Some("two"), true).as_deref(),
                Some("one")
            );
        }

        // Case `focus_recovery_uses_same_slot_then_previous_at_the_end`.
        {
            let node = |target: &str| {
                AccessNode::new(Role::Button)
                    .with_name(target)
                    .with_action_target(target)
                    .with_state(AccessState::new().focusable(true))
            };
            let before = AccessNode::new(Role::Group).with_children(vec![
                node("one"),
                node("two"),
                node("three"),
            ]);
            let middle_removed =
                AccessNode::new(Role::Group).with_children(vec![node("one"), node("three")]);
            assert_eq!(
                recover_semantic_focus(&before, &middle_removed, Some("two")).as_deref(),
                Some("three")
            );

            let end_removed =
                AccessNode::new(Role::Group).with_children(vec![node("one"), node("two")]);
            assert_eq!(
                recover_semantic_focus(&before, &end_removed, Some("three")).as_deref(),
                Some("two")
            );
        }
    }
}
