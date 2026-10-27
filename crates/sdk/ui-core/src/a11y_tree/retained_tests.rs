//! A widget's synthetic children are retained between refreshes: a refresh
//! moves over the nodes of the children it is handed again unchanged, builds
//! the rest, and answers exactly what a refresh that keeps nothing answers.

use super::*;
use crate::theme::Theme;
use taffy::TaffyTree;

/// A list whose rows have no widgets of their own, as an editor's paragraphs.
struct Rows {
    name: &'static str,
    rows: Vec<AccessNode>,
}

impl Widget for Rows {
    crate::testing::inert_widget_methods!(leaf_layout, name_field);
    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::List)
                .with_semantic_key("rows")
                .with_children(self.rows.clone()),
        )
    }
}

fn rows(rows: Vec<AccessNode>) -> Rows {
    Rows { name: "Rows", rows }
}

fn row(key: &str, text: &str) -> AccessNode {
    AccessNode::new(Role::ListItem)
        .with_hashed_key(key)
        .with_name(format!("Row {key}"))
        .with_action_target(format!("row:{key}"))
        .with_children(vec![
            AccessNode::new(Role::MultilineTextInput)
                .with_value(text)
                .with_action_target(format!("text:{key}")),
            AccessNode::new(Role::Button)
                .with_name("Move up")
                .with_action_target(format!("up:{key}")),
        ])
}

fn layout(root: &mut Rows) -> HashMap<TaffyNodeId, *mut dyn Widget> {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes = HashMap::new();
    root.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    nodes
}

/// Every node in reading order, and the ids repeated.
fn frame(tree: &SemanticTree) -> (Vec<SemanticNode>, Vec<SemanticId>) {
    (
        tree.iter().cloned().collect(),
        tree.duplicate_ids().to_vec(),
    )
}

/// What a tree that keeps nothing between refreshes answers for `next` after
/// `last`.
fn rebuilt(last: &mut Rows, next: &mut Rows) -> (TreeDelta, (Vec<SemanticNode>, Vec<SemanticId>)) {
    let mut tree = SemanticTree::new();
    let nodes = layout(last);
    tree.refresh(last, &nodes, None, &[]);
    tree.retained.clear();
    let nodes = layout(next);
    let delta = tree.refresh(next, &nodes, None, &[]);
    (delta, frame(&tree))
}

/// Where the value of the text node of row `key` lives: the same place after
/// a refresh only when that refresh moved the node over rather than building
/// it again from the widget's fresh copy.
fn text_at(tree: &SemanticTree, key: &str) -> *const u8 {
    let target = format!("text:{key}");
    tree.iter()
        .find(|node| node.semantic_target.as_deref() == Some(target.as_str()))
        .and_then(|node| node.value.as_ref())
        .map(|value| value.as_ptr())
        .unwrap()
}

#[test]
fn a_refresh_moves_unchanged_rows_over_and_answers_as_a_full_rebuild_does() {
    let frames = [
        vec![row("a", "alpha"), row("b", "beta"), row("c", "gamma")],
        vec![row("a", "alpha"), row("b", "betaa"), row("c", "gamma")],
        vec![row("a", "alpha"), row("c", "gamma")],
        vec![row("c", "gamma"), row("a", "alpha"), row("b", "betaa")],
        // Two rows with one key: a repeat only the full walk reports.
        vec![row("a", "alpha"), row("a", "alpha"), row("c", "gamma")],
        vec![row("a", "alpha"), row("c", "gamma")],
        vec![],
    ];
    let mut tree = SemanticTree::new();
    let mut last = rows(Vec::new());
    let nodes = layout(&mut last);
    tree.refresh(&last, &nodes, None, &[]);
    for next in frames {
        let mut next = rows(next);
        let nodes = layout(&mut next);
        let delta = tree.refresh(&next, &nodes, None, &[]);
        let (expected, nodes) = rebuilt(&mut last, &mut next);
        assert_eq!(delta, expected, "{:?}", next.rows);
        assert_eq!(frame(&tree), nodes, "{:?}", next.rows);
        last = next;
    }
}

#[test]
fn a_key_typed_into_one_row_rebuilds_that_row_alone() {
    let mut tree = SemanticTree::new();
    let mut first = rows(vec![row("a", "alpha"), row("b", "beta")]);
    let nodes = layout(&mut first);
    tree.refresh(&first, &nodes, None, &[]);
    let (a, b) = (text_at(&tree, "a"), text_at(&tree, "b"));

    let mut typed = rows(vec![row("a", "alpha"), row("b", "betaa")]);
    let nodes = layout(&mut typed);
    let delta = tree.refresh(&typed, &nodes, None, &[]);
    assert_eq!(
        text_at(&tree, "a"),
        a,
        "the unchanged row's nodes moved over"
    );
    assert_ne!(text_at(&tree, "b"), b, "the typed row was built again");
    assert_eq!(
        delta.updated.len(),
        1,
        "only the typed text changed: {delta:?}"
    );
    assert!(delta.added.is_empty() && delta.removed.is_empty());
}
