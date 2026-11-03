//! How a list's rows stand as a tree, and how folding a row hides what is under it.

use super::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::testing::Dispatch;

/// A model whose rows name the row they stand under, as a list of subtasks does: the rows keep
/// the model's order, and a parent need not be the row above.
struct Nested {
    keys: Vec<u64>,
    parents: HashMap<u64, u64>,
}

impl ListModel for Nested {
    type Key = u64;

    fn len(&self) -> usize {
        self.keys.len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.keys.get(index).copied()
    }

    fn index_of(&self, key: &u64) -> Option<usize> {
        self.keys.iter().position(|candidate| candidate == key)
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<u64>> {
        let key = self.key_at(index)?;
        let parent = self
            .parents
            .get(&key)
            .copied()
            .filter(|parent| self.keys.contains(parent));
        Some(TreeRow { parent })
    }
}

fn nested() -> VirtualList<Nested> {
    let model = Nested {
        keys: vec![0, 1, 2, 3, 4],
        // 1 and 2 nest under 0, one under the other; 4 names 0 from away from it.
        parents: HashMap::from([(1, 0), (2, 1), (4, 0)]),
    };
    VirtualList::new(model, 20.0, |_, _, key| {
        Box::new(super::tests::ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(200.0)
}

/// The tree's rows as the list works them out: each row's depth, whether it has subtasks, and
/// whether it stands under the row it follows.
fn shape(list: &VirtualList<Nested>, marks: &TreeMarks<u64>) -> Vec<(u64, u32, bool, bool)> {
    (0..list.model.len())
        .map(|index| {
            let key = list.model.key_at(index).expect("every row has a key");
            (
                key,
                marks.depth(&key),
                marks.is_branch(&key),
                marks.is_nested(&key),
            )
        })
        .collect()
}

/// Press and let go at `x`/`y`.
fn press(list: &mut VirtualList<Nested>, x: f64, y: f64, taffy: &TaffyTree<()>, node: NodeId) {
    let at = crate::synthetic_input::cursor_moved(x, y);
    let _ = Dispatch::new().layout(taffy, node).send(list, &at);
    for state in [ElementState::Pressed, ElementState::Released] {
        let event = crate::synthetic_input::mouse_input(state, MouseButton::Left);
        let _ = Dispatch::new().layout(taffy, node).send(list, &event);
    }
}

#[test]
fn a_list_indents_a_row_under_the_row_it_names_and_folds_it_away() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = nested().with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(
        shape(&list, &marks),
        vec![
            (0, 0, true, false),
            (1, 1, true, true),
            (2, 2, false, true),
            (3, 0, false, false),
            // The list keeps its order: a row whose parent the sort put elsewhere stands on
            // its own and names it, rather than indenting under nothing.
            (4, 0, false, false),
        ],
        "what stands under what"
    );
    let row = 20.0;
    assert!((list.viewport.content_extent() - 5.0 * row).abs() < 0.5);

    // Folding a row takes the room its subtasks had, down the chain.
    assert!(list.fold_row(&0, true), "the row has subtasks to fold");
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    assert!(list.row_folded(&0));
    assert!((list.viewport.content_extent() - 3.0 * row).abs() < 0.5);
    assert_eq!(list.viewport.item_height(1), Some(0.0));
    assert_eq!(list.viewport.item_height(2), Some(0.0), "the whole chain");
    assert_eq!(list.viewport.item_height(3), Some(row));

    // Its disclosure shows them again, where the person presses: the chevron of the row.
    press(&mut list, 10.0, 10.0, &taffy, node);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    assert!(!list.row_folded(&0), "the press unfolded it");
    assert!((list.viewport.content_extent() - 5.0 * row).abs() < 0.5);

    // Deeper in, the chevron stands one indent further in, and folds only what is under it.
    press(&mut list, 16.0 + 10.0, row + 10.0, &taffy, node);
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert!(
        list.row_folded(&1) && !list.row_folded(&0),
        "the press read the row it stands on"
    );
    assert!((list.viewport.content_extent() - 4.0 * row).abs() < 0.5);
}
