//! How a list's rows stand as a tree, and how folding a row hides what is under it.

use super::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::testing::Dispatch;
use crate::theme::Theme;

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
fn press<M: ListModel + 'static>(
    list: &mut VirtualList<M>,
    x: f64,
    y: f64,
    taffy: &TaffyTree<()>,
    node: NodeId,
) {
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

/// A tree reads as a tree: the list says what it is, each row says how deep it stands, and a
/// row with subtasks says whether they show.
#[test]
fn a_tree_reads_as_a_tree_and_says_how_deep_each_row_stands() {
    use crate::a11y::Role;

    let marks = Rc::new(TreeMarks::default());
    let mut list = nested().with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    let semantics = list.accessibility().expect("the list has a node");
    assert_eq!(semantics.role, Role::Tree);
    let rows: Vec<_> = semantics.children.iter().collect();
    assert!(
        rows.iter().all(|row| row.role == Role::TreeItem),
        "each row is a tree item"
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.level.expect("every row says its depth"))
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 1, 1],
        "one level deeper for each step under a row"
    );
    assert_eq!(
        rows[0].state.expanded,
        Some(true),
        "it shows what is under it"
    );
    assert_eq!(rows[1].state.expanded, Some(true));
    assert_eq!(
        rows[2].state.expanded, None,
        "a row with nothing under it has no disclosure"
    );

    // Folded away, the branch says so, and the rows under it leave the tree with their rows.
    assert!(list.fold_row(&0, true));
    let _ = super::tests::lay_out(&mut list, 300.0);
    let semantics = list.accessibility().expect("the list has a node");
    assert_eq!(
        semantics.children.len(),
        3,
        "only the rows that show stand in the tree"
    );
    assert_eq!(semantics.children[0].state.expanded, Some(false));
}

/// The left and right keys fold a branch as its disclosure does under the pointer, and step in
/// and out of what it holds.
#[test]
fn the_left_and_right_keys_fold_a_branch_and_step_in_and_out_of_it() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = nested().with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    let key = |named| Key::Named(named);
    assert!(list.on_key(&key(NamedKey::ArrowDown), false, false));
    assert_eq!(
        list.focused_key,
        Some(0),
        "the keyboard starts on the first row"
    );

    // Right steps into the row that stands under it, and into the row under that.
    assert!(list.on_key(&key(NamedKey::ArrowRight), false, false));
    assert_eq!(list.focused_key, Some(1));
    assert!(list.on_key(&key(NamedKey::ArrowRight), false, false));
    assert_eq!(list.focused_key, Some(2));
    assert!(
        !list.on_key(&key(NamedKey::ArrowRight), false, false),
        "a row with nothing under it has nowhere to step"
    );

    // Left steps back out to the row it stands under; on a branch that shows its subtasks it
    // folds them away, as the chevron does, and right shows them again.
    assert!(list.on_key(&key(NamedKey::ArrowLeft), false, false));
    assert_eq!(list.focused_key, Some(1));
    assert!(list.on_key(&key(NamedKey::ArrowLeft), false, false));
    assert!(list.row_folded(&1), "the branch folded its subtask away");
    assert_eq!(
        list.focused_key,
        Some(1),
        "the folded row keeps the keyboard"
    );
    assert!(list.on_key(&key(NamedKey::ArrowRight), false, false));
    assert!(!list.row_folded(&1), "right shows what it folded away");
}

/// A tree whose rows stand under a group heading, as a project page's first task and its
/// subtask do.
struct Headed {
    keys: Vec<u64>,
    parents: HashMap<u64, u64>,
    groups: Vec<Option<RowGroup>>,
}

impl ListModel for Headed {
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

    fn group_at(&self, index: usize) -> Option<RowGroup> {
        self.groups.get(index).cloned().flatten()
    }

    fn discloses(&self) -> bool {
        self.groups.iter().any(Option::is_some)
    }
}

#[test]
fn a_first_row_under_a_heading_draws_its_chevron_on_its_own_row() {
    let marks = Rc::new(TreeMarks::default());
    let model = Headed {
        keys: vec![0, 1, 2],
        // The first row of the list is a parent: its subtask follows it.
        parents: HashMap::from([(1, 0)]),
        groups: vec![
            Some(RowGroup::heading("Jun 30".to_owned())),
            Some(RowGroup::heading("Jun 30".to_owned())),
            Some(RowGroup::heading("Jun 30".to_owned())),
        ],
    };
    let mut list = VirtualList::new(model, 20.0, |_, _, key| {
        Box::new(super::tests::ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(200.0)
    .with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    let _ = super::tests::lay_out(&mut list, 300.0);

    // The viewport holds the first row with the band of the heading over it in one item.
    let item_height = list.viewport.item_height(0).expect("the first row");
    let row = list.row_box(0).expect("the first row stands somewhere");
    let band = list.groups.height;
    assert!(band > 0.0, "the heading over the row takes a band");
    assert!(
        (item_height - (band + row.height())).abs() < 0.5,
        "the item is the band and the row: {item_height} of {band} and {}",
        row.height()
    );

    let (chevron, folded) = list
        .chevrons()
        .rect(&marks, &Theme::dark(), &0, row)
        .expect("the row folds the subtask under it");
    assert!(!folded, "the group starts open");
    assert!(
        (chevron.center().y - row.center().y).abs() < 0.5,
        "the chevron is centred on its own row: {} against the row's {}",
        chevron.center().y,
        row.center().y
    );
    assert!(
        chevron.y0 >= row.y0 - 0.5 && chevron.y1 <= row.y1 + 0.5,
        "the chevron stands inside the row, not over the heading: {:?} in {row:?}",
        chevron
    );
    let group_center =
        list.rect.y0 + list.viewport.item_view_top(0).unwrap_or_default() + item_height / 2.0;
    assert!(
        (chevron.center().y - group_center).abs() > 1.0,
        "it is never centred between the heading and the row: {} against {group_center}",
        chevron.center().y
    );

    // A phone's narrow column lays the heading and the row out the same way.
    let _ = super::tests::lay_out(&mut list, 180.0);
    let narrow = list.row_box(0).expect("the first row on a phone");
    let (chevron, _) = list
        .chevrons()
        .rect(&marks, &Theme::dark(), &0, narrow)
        .expect("the row folds the subtask under it");
    assert!(
        (chevron.center().y - narrow.center().y).abs() < 0.5,
        "a phone's column keeps the chevron on its row: {} against {}",
        chevron.center().y,
        narrow.center().y
    );

    // The press that folds the subtask is read where the chevron stands, on the row.
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let row = list.row_box(0).expect("the first row stands somewhere");
    let (chevron, _) = list
        .chevrons()
        .rect(&marks, &Theme::dark(), &0, row)
        .expect("the row folds the subtask under it");
    press(
        &mut list,
        chevron.center().x,
        chevron.center().y,
        &taffy,
        node,
    );
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert!(
        list.row_folded(&0),
        "the chevron's press folds the subtask under the row"
    );
    let row = list.row_box(0).expect("the folded row stands somewhere");
    let (chevron, folded) = list
        .chevrons()
        .rect(&marks, &Theme::dark(), &0, row)
        .expect("a folded row still shows its disclosure");
    assert!(folded, "the chevron points at what it folded away");
    assert!(
        (chevron.center().y - row.center().y).abs() < 0.5,
        "the folded row's chevron stays on its row: {} against {}",
        chevron.center().y,
        row.center().y
    );
}
