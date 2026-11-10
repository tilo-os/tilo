//! How a list's rows stand as a tree, and how folding a row hides what is under it.

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use automerge::AutoCommit;

use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::testing::Dispatch;
use crate::theme::Theme;

/// A model whose rows name the row they stand under, as a list of subtasks does: the rows keep
/// the model's order, and a parent need not be the row above. `groups` are the headings its rows
/// stand under, as a list of tasks grouped by the day each is due is.
struct Nested {
    keys: Vec<u64>,
    parents: HashMap<u64, u64>,
    groups: HashMap<u64, String>,
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

    fn group_at(&self, index: usize) -> Option<RowGroup> {
        let key = self.key_at(index)?;
        self.groups
            .get(&key)
            .map(|heading| RowGroup::heading(heading.clone()))
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<u64>> {
        let key = self.key_at(index)?;
        let parent = self
            .parents
            .get(&key)
            .copied()
            .filter(|parent| self.keys.contains(parent));
        Some(TreeRow {
            parent,
            depth: None,
            above: None,
        })
    }
}

fn nested() -> VirtualList<Nested> {
    let model = Nested {
        keys: vec![0, 1, 2, 3, 4],
        // 1 and 2 nest under 0, one under the other; 4 names 0 from away from it.
        parents: HashMap::from([(1, 0), (2, 1), (4, 0)]),
        groups: HashMap::new(),
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

    // Its disclosure shows them again, where the list says the person presses: the box the row
    // shows its chevron in.
    let at = list
        .disclosure_rect(&0)
        .expect("the folded row shows its disclosure")
        .center();
    press(&mut list, at.x, at.y, &taffy, node);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    assert!(!list.row_folded(&0), "the press unfolded it");
    assert!((list.viewport.content_extent() - 5.0 * row).abs() < 0.5);

    // Deeper in, the chevron stands one indent further in, and folds only what is under it.
    let at = list
        .disclosure_rect(&1)
        .expect("the branch one depth in shows its disclosure")
        .center();
    press(&mut list, at.x, at.y, &taffy, node);
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert!(
        list.row_folded(&1) && !list.row_folded(&0),
        "the press read the row it stands on"
    );
    assert!((list.viewport.content_extent() - 4.0 * row).abs() < 0.5);
}

/// A row that starts a group stands at the group's own first level: its parent is a row of the
/// group above, under another heading, so indenting it would put it under a row that is not
/// there. It says what it is in instead, and the rows that stay in their parent's group keep
/// their nesting.
#[test]
fn a_row_whose_parent_stands_in_another_group_stands_on_the_groups_first_level() {
    let marks = Rc::new(TreeMarks::default());
    let model = Nested {
        keys: vec![0, 1, 2, 3],
        // 1 is a step of 0; 2 and 3 are steps of 0 too, and the sort put them under another
        // day than the task they belong to.
        parents: HashMap::from([(1, 0), (2, 0), (3, 2)]),
        groups: HashMap::from([
            (0, "Monday".to_owned()),
            (1, "Monday".to_owned()),
            (2, "Thursday".to_owned()),
            (3, "Thursday".to_owned()),
        ]),
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
    assert_eq!(
        shape(&list, &marks),
        vec![
            (0, 0, true, false),
            (1, 1, false, true),
            // The group's first row stands on its own level, naming the task it is a step of.
            (2, 0, true, false),
            // A row whose parent stands in its own group keeps its place under it.
            (3, 1, false, true),
        ],
        "what stands under what"
    );
}

/// A row standing open into its item folds its branch: the rows under it are what the item's
/// document lists, so the list keeps them out of its own, and closing the row brings them
/// back where they stood. What the person folded stays folded underneath.
#[test]
fn a_row_standing_open_folds_its_branch_and_closing_it_brings_the_rows_back() {
    let marks = Rc::new(TreeMarks::default());
    let list = OpenableTree {
        keys: vec![0, 1, 2, 3],
        // 1 and 2 nest under 0, one under the other; 3 stands beside them.
        parents: HashMap::from([(1, 0), (2, 1)]),
    };
    let mut list = VirtualList::new(list, 20.0, |_, _, key| {
        Box::new(super::tests::ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(400.0)
    .with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    let row = 20.0;
    assert!(
        (list.viewport.content_extent() - 4.0 * row).abs() < 0.5,
        "every row shows before anything opens"
    );

    list.set_open(Some(0));
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(
        list.viewport.item_height(1),
        Some(0.0),
        "the branch folds under the row standing open"
    );
    assert_eq!(
        list.viewport.item_height(2),
        Some(0.0),
        "all the way down its chain"
    );
    assert_eq!(
        list.viewport.item_height(3),
        Some(row),
        "a row beside the open one shows"
    );

    list.set_open(None);
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(
        list.viewport.item_height(1),
        Some(row),
        "closing the row brings its branch back"
    );
    assert_eq!(
        list.viewport.item_height(2),
        Some(row),
        "as deep as it stood"
    );
    assert!(
        (list.viewport.content_extent() - 4.0 * row).abs() < 0.5,
        "the whole list stands as it did"
    );
}

/// A tree whose first level folds nothing keeps no column for a chevron: its rows stand on
/// the column the words over them do, and one with a branch to fold keeps the step.
#[test]
fn a_tree_with_nothing_to_fold_keeps_no_chevrons_column() {
    let marks = Rc::new(TreeMarks::default());
    let list = VirtualList::new(
        OpenableTree {
            keys: vec![0, 1],
            parents: HashMap::new(),
        },
        20.0,
        |_, _, key| {
            Box::new(super::tests::ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        },
    )
    .with_viewport_height(200.0)
    .with_tree(Rc::clone(&marks));
    let mut list = list;
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(list.row_depth(&0), 0);
    assert!(!list.row_is_branch(&0));
    assert!(
        (list.lead - list.bleed).abs() < 0.5,
        "no chevron stands in front of the first level, so its rows keep no column: \
         {} in from {}",
        list.lead,
        list.bleed
    );

    let marks = Rc::new(TreeMarks::default());
    let mut list = VirtualList::new(
        Nested {
            keys: vec![0, 1],
            parents: HashMap::from([(1, 0)]),
            groups: HashMap::new(),
        },
        20.0,
        |_, _, key| {
            Box::new(super::tests::ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        },
    )
    .with_viewport_height(200.0)
    .with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert!(
        list.lead > list.bleed + 0.5,
        "a row with subtasks hands its chevron a column: {} in from {}",
        list.lead,
        list.bleed
    );
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

/// A tree whose rows a host moves: a drop lands what it moved where it names, so a move the
/// list places is the move its person sees.
struct Movable {
    keys: Vec<u64>,
    parents: HashMap<u64, u64>,
    labels: HashMap<u64, String>,
    takes_children: HashSet<u64>,
}

impl ListModel for Movable {
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

    fn label(&self, index: usize) -> Option<String> {
        self.labels.get(&self.key_at(index)?).cloned()
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<u64>> {
        let key = self.key_at(index)?;
        let parent = self
            .parents
            .get(&key)
            .copied()
            .filter(|parent| self.keys.contains(parent));
        Some(TreeRow {
            parent,
            depth: None,
            above: None,
        })
    }

    fn accepts_children(&self, index: usize) -> bool {
        self.key_at(index)
            .is_some_and(|key| self.takes_children.contains(&key))
    }
}

/// Where `key` stands: under `ancestor`, following the parents it names.
fn stands_under(parents: &HashMap<u64, u64>, key: u64, ancestor: u64) -> bool {
    let mut cursor = key;
    for _ in 0..64 {
        let Some(parent) = parents.get(&cursor).copied() else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        cursor = parent;
    }
    false
}

/// Where a host lands a drop: the row moves beside the row it names, into it as its last
/// subtask, and takes the depth of where it landed.
fn land(model: &mut Movable, dropped: ReorderDrop<u64>) -> bool {
    if dropped.source == dropped.target.key {
        return false;
    }
    let Some(from) = model.index_of(&dropped.source) else {
        return false;
    };
    if model.index_of(&dropped.target.key).is_none() {
        return false;
    }
    let parent = match dropped.target.placement {
        DropPlacement::Inside => Some(dropped.target.key),
        DropPlacement::Before | DropPlacement::After => {
            model.parents.get(&dropped.target.key).copied()
        }
    };
    model.keys.remove(from);
    let at = model.index_of(&dropped.target.key).unwrap_or_default();
    let into = match dropped.target.placement {
        DropPlacement::Before => at,
        DropPlacement::Inside => {
            let mut into = at.saturating_add(1);
            while into < model.keys.len()
                && stands_under(&model.parents, model.keys[into], dropped.target.key)
            {
                into = into.saturating_add(1);
            }
            into
        }
        DropPlacement::After => at.saturating_add(1),
    };
    model
        .keys
        .insert(into.min(model.keys.len()), dropped.source);
    match parent {
        Some(parent) => {
            model.parents.insert(dropped.source, parent);
        }
        None => {
            model.parents.remove(&dropped.source);
        }
    }
    true
}

fn movable(keys: Vec<u64>, parents: &[(u64, u64)]) -> VirtualList<Movable> {
    let model = Movable {
        labels: keys
            .iter()
            .map(|key| (*key, format!("row {key}")))
            .collect(),
        keys: keys.clone(),
        parents: parents.iter().copied().collect(),
        takes_children: keys.into_iter().collect(),
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

/// Send `event` to the list as the window would.
fn send<M: ListModel + 'static>(
    list: &mut VirtualList<M>,
    event: &WindowEvent,
    laid_out: (&TaffyTree<()>, NodeId),
) -> bool {
    Dispatch::new()
        .layout(laid_out.0, laid_out.1)
        .send(list, event)
        .handled
}

/// Send `event` with `modifiers` held.
fn send_with<M: ListModel + 'static>(
    list: &mut VirtualList<M>,
    event: &WindowEvent,
    modifiers: winit::keyboard::ModifiersState,
    laid_out: (&TaffyTree<()>, NodeId),
) -> bool {
    Dispatch::new()
        .modifiers(modifiers)
        .layout(laid_out.0, laid_out.1)
        .send(list, event)
        .handled
}

#[test]
fn tab_indents_the_focused_row_into_the_row_above_it_and_shift_tab_steps_it_back_out() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = movable(vec![0, 1, 2], &[])
        .tab_indents(true)
        .with_tree(Rc::clone(&marks))
        .with_reorder(|_, source, target| source != &target.key, land);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    list.set_focus(true);
    assert!(list.reveal(&2));
    assert_eq!(list.focused_key(), Some(&2));

    // Tab moves it under the row above it at its own depth, as an outliner does.
    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::Tab),
        laid_out
    ));
    assert_eq!(
        list.model().parents.get(&2),
        Some(&1),
        "under the row above"
    );
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(marks.depth(&2), 1, "one step deeper");
    assert_eq!(
        list.focused_key(),
        Some(&2),
        "the moved row keeps the keyboard"
    );

    // Shift+Tab steps it back out, directly after the row it stood under.
    assert!(send_with(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::Tab),
        winit::keyboard::ModifiersState::SHIFT,
        laid_out,
    ));
    assert_eq!(list.model().parents.get(&2), None, "out of it again");
    assert_eq!(
        list.model().keys,
        vec![0, 1, 2],
        "beside the row it stood under"
    );
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(marks.depth(&2), 0);
}

#[test]
fn a_tab_that_cannot_move_the_row_is_left_to_focus_traversal() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = movable(vec![0, 1], &[])
        .tab_indents(true)
        .with_tree(Rc::clone(&marks))
        .with_reorder(|_, source, target| source != &target.key, land);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    list.set_focus(true);
    assert!(list.reveal(&0));
    assert!(
        !send(
            &mut list,
            &crate::synthetic_input::named_key(NamedKey::Tab),
            (&taffy, node)
        ),
        "a top row has no row above to indent into"
    );
    assert_eq!(list.model().parents.get(&0), None);
}

#[test]
fn the_alt_arrows_move_a_row_through_its_tree_and_ctrl_moves_it_beside_its_siblings() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = movable(vec![0, 1, 2], &[(1, 0)])
        .with_tree(Rc::clone(&marks))
        .with_reorder(|_, source, target| source != &target.key, land);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    list.set_focus(true);
    let alt = winit::keyboard::ModifiersState::ALT;
    let ctrl = winit::keyboard::ModifiersState::CONTROL;

    // Alt+Right indents it into the row it stands beside, the row above at its own depth;
    // Alt+Left steps it back out beside that row.
    assert!(list.reveal(&2));
    assert!(send_with(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::ArrowRight),
        alt,
        laid_out
    ));
    assert_eq!(
        list.model().parents.get(&2),
        Some(&0),
        "under the row above it"
    );
    assert!(send_with(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::ArrowLeft),
        alt,
        laid_out
    ));
    assert_eq!(list.model().parents.get(&2), None);

    // Ctrl+Up moves it up through its own depth, never into the branch it passes: row 1
    // stands under row 0, and row 2 passes both to stand above them, still a top row.
    assert!(send_with(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::ArrowUp),
        ctrl,
        laid_out
    ));
    assert_eq!(list.model().parents.get(&2), None, "still a top row");
    assert_eq!(list.model().keys, vec![2, 0, 1]);
    assert_eq!(list.focused_key(), Some(&2));
}

#[test]
fn a_drop_into_the_middle_of_a_row_lands_the_dragged_row_inside_it() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = movable(vec![0, 1, 2], &[])
        .with_tree(Rc::clone(&marks))
        .with_reorder(|_, source, target| source != &target.key, land);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);

    // Pick row 2 up and hold it over the middle of row 0.
    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 50.0),
        laid_out,
    );
    send(&mut list, &crate::synthetic_input::mouse_press(), laid_out);
    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 10.0),
        laid_out,
    );
    assert_eq!(
        list.reorder_target(),
        Some(&DropTarget::new(0, DropPlacement::Inside)),
        "the middle of a row that takes subtasks takes the row into it"
    );
    assert!(list.reorder_preview().is_some());
    send(
        &mut list,
        &crate::synthetic_input::mouse_release(),
        laid_out,
    );

    assert_eq!(list.model().parents.get(&2), Some(&0), "it landed inside");
    let _ = super::tests::lay_out(&mut list, 300.0);
    assert_eq!(marks.depth(&2), 1);
    assert!(marks.is_branch(&0), "the row it landed in holds subtasks");
}

#[test]
fn the_seam_below_an_open_branch_lands_at_the_first_row_it_holds() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = movable(vec![0, 1, 2], &[(1, 0)])
        .with_tree(Rc::clone(&marks))
        .with_reorder(|_, source, target| source != &target.key, land);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);

    // Row 0 holds row 1; the foot of row 0 is the seam above row 1, not row 0's next sibling.
    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 50.0),
        laid_out,
    );
    send(&mut list, &crate::synthetic_input::mouse_press(), laid_out);
    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 18.0),
        laid_out,
    );
    assert_eq!(
        list.reorder_target(),
        Some(&DropTarget::new(1, DropPlacement::Before))
    );
    let preview = list.reorder_preview().expect("a line where it lands");
    assert!(
        (preview.center().y - 20.0).abs() < 2.0,
        "above the row it holds: {preview:?}"
    );
}

#[test]
fn a_pointer_resting_on_a_folded_branch_shows_what_it_holds() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = movable(vec![0, 1, 2], &[(1, 0)])
        .with_tree(Rc::clone(&marks))
        .with_reorder(|_, source, target| source != &target.key, land);
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    assert!(list.fold_row(&0, true));
    let _ = super::tests::lay_out(&mut list, 300.0);
    list.reorder = crate::reorder::ReorderController::new()
        .with_drag_threshold(0.0)
        .with_hover_expand_delay(crate::time::Duration::ZERO);

    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 30.0),
        laid_out,
    );
    send(&mut list, &crate::synthetic_input::mouse_press(), laid_out);
    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 10.0),
        laid_out,
    );
    assert_eq!(
        list.reorder_target(),
        Some(&DropTarget::new(0, DropPlacement::Inside))
    );
    let doc = AutoCommit::new();
    assert!(
        list.update(&doc, &automerge::ROOT),
        "the rest brings its rows into view"
    );
    assert!(!list.row_folded(&0), "what it holds comes into view");
}

#[test]
fn delete_asks_the_host_and_removes_no_row_itself() {
    let removed: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&removed);
    let mut list = movable(vec![0, 1], &[]).on_delete(move |key| seen.borrow_mut().push(key));
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    list.set_focus(true);
    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::ArrowDown),
        laid_out
    ));
    assert_eq!(list.focused_key(), Some(&0));
    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::Delete),
        laid_out
    ));
    assert_eq!(
        *removed.borrow(),
        vec![0],
        "the focused row, for the host to remove"
    );
    assert_eq!(list.model().len(), 2, "the list removes nothing itself");
}

#[test]
fn f2_renames_the_focused_row_in_place_and_reports_the_trimmed_title() {
    let renamed: Rc<RefCell<Vec<(u64, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&renamed);
    let mut list =
        movable(vec![0, 1], &[]).on_rename(move |key, title| seen.borrow_mut().push((key, title)));
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    list.set_focus(true);
    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::ArrowDown),
        laid_out
    ));

    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::F2),
        laid_out
    ));
    assert_eq!(list.renaming(), Some(&0));
    let field = list.rename_field().expect("the field stands over the row");
    assert_eq!(field.value(), "row 0", "the row's own title to change");
    let row = list.row_box(0).expect("the row stands somewhere");
    assert!(
        field.get_rect().y0 >= row.y0 - 0.5 && field.get_rect().y1 <= row.y1 + 0.5,
        "the field sits on the row: {:?} in {row:?}",
        field.get_rect()
    );

    // Typing goes into the field, never the list's type-ahead, and Enter commits.
    assert!(send(
        &mut list,
        &crate::synthetic_input::char_key("!"),
        laid_out
    ));
    let semantics = list.accessibility().expect("the list has a node");
    let field = semantics.children[0]
        .children
        .iter()
        .find(|child| child.role == crate::a11y::Role::TextInput)
        .expect("the row carries the field over its label, for a reader to reach");
    assert_eq!(field.value.as_deref(), Some("row 0!"));
    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::Enter),
        laid_out
    ));
    assert_eq!(list.renaming(), None, "the field closed");
    assert_eq!(*renamed.borrow(), vec![(0, "row 0!".to_owned())]);
}

#[test]
fn escape_cancels_a_rename_and_a_press_elsewhere_commits_it() {
    let renamed: Rc<RefCell<Vec<(u64, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&renamed);
    let mut list =
        movable(vec![0, 1], &[]).on_rename(move |key, title| seen.borrow_mut().push((key, title)));
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    list.set_focus(true);
    assert!(list.begin_rename(&0));
    assert!(send(
        &mut list,
        &crate::synthetic_input::char_key("x"),
        laid_out
    ));
    assert!(send(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::Escape),
        laid_out
    ));
    assert_eq!(list.renaming(), None);
    assert!(
        renamed.borrow().is_empty(),
        "a cancelled rename reports nothing"
    );

    assert!(list.begin_rename(&0));
    assert!(send(
        &mut list,
        &crate::synthetic_input::char_key("y"),
        laid_out
    ));
    // A press on the row below is outside the field: it commits the title and reaches the row.
    send(
        &mut list,
        &crate::synthetic_input::cursor_moved(20.0, 30.0),
        laid_out,
    );
    send(&mut list, &crate::synthetic_input::mouse_press(), laid_out);
    send(
        &mut list,
        &crate::synthetic_input::mouse_release(),
        laid_out,
    );
    assert_eq!(*renamed.borrow(), vec![(0, "row 0y".to_owned())]);
    assert_eq!(
        list.focused_key(),
        Some(&1),
        "the press chose the row below"
    );
}

#[test]
fn a_double_click_renames_the_row_it_lands_on_instead_of_opening_it() {
    let renamed: Rc<RefCell<Vec<(u64, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let opened: Rc<RefCell<Vec<u64>>> = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&renamed);
    let chosen = Rc::clone(&opened);
    let mut list = movable(vec![0, 1], &[])
        .on_rename(move |key, title| seen.borrow_mut().push((key, title)))
        .rename_on_double_click()
        .on_activate(move |key| chosen.borrow_mut().push(key));
    let (taffy, node) = super::tests::lay_out(&mut list, 300.0);
    press(&mut list, 20.0, 10.0, &taffy, node);
    press(&mut list, 20.0, 10.0, &taffy, node);

    assert_eq!(
        list.renaming(),
        Some(&0),
        "the second click renames the row"
    );
    assert!(opened.borrow().is_empty(), "and does not open it");
    assert_eq!(
        list.focused_key(),
        Some(&0),
        "the row it renames holds the keyboard"
    );
    assert_eq!(
        list.rename_field().map(|field| field.value()),
        Some("row 0")
    );
}

#[test]
fn a_tree_row_keeps_its_own_height_and_the_ends_and_pages_reach_its_rows() {
    let marks = Rc::new(TreeMarks::default());
    // Row 1 draws taller than its siblings, as a row with two lines does.
    let model = Movable {
        labels: (0..3).map(|key| (key, format!("row {key}"))).collect(),
        keys: vec![0, 1, 2],
        parents: [(1, 0)].into_iter().collect(),
        takes_children: (0..3).collect(),
    };
    let mut list = VirtualList::new(model, 20.0, |_, _, key| {
        let height = if *key == 1 { 44.0 } else { 20.0 };
        Box::new(super::tests::ProbeRow::new(
            key.to_string(),
            height,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(200.0)
    .with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    // The taller row keeps its height indented under row 0, and the row below it clears it.
    assert_eq!(marks.depth(&1), 1);
    assert_eq!(list.viewport.item_height(1), Some(f64::from(44.0_f32)));
    assert_eq!(
        list.row_box(2).map(|row| row.y0),
        Some(64.0),
        "row 2 starts under row 1's full height"
    );

    let key = |named| Key::Named(named);
    assert!(list.on_key(&key(NamedKey::End), false, false));
    assert_eq!(list.focused_key(), Some(&2), "End reaches the last row");
    assert!(list.on_key(&key(NamedKey::Home), false, false));
    assert_eq!(list.focused_key(), Some(&0), "Home the first");
    assert!(list.on_key(&key(NamedKey::PageDown), false, false));
    assert_eq!(list.focused_key(), Some(&2), "a page down the flat rows");
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
        Some(TreeRow {
            parent,
            depth: None,
            above: None,
        })
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
        .rect(&marks, &Theme::dark(), &0, row, false)
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
        .rect(&marks, &Theme::dark(), &0, narrow, false)
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
        .rect(&marks, &Theme::dark(), &0, row, false)
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
        .rect(&marks, &Theme::dark(), &0, row, list.row_folded(&0))
        .expect("a folded row still shows its disclosure");
    assert!(folded, "the chevron points at what it folded away");
    assert!(
        (chevron.center().y - row.center().y).abs() < 0.5,
        "the folded row's chevron stays on its row: {} against {}",
        chevron.center().y,
        row.center().y
    );
}

/// The chevron that folds a row hangs in the step in front of the row's own column, so a row
/// with something to fold starts on the column its neighbours do and the top level stands its
/// own one step in from the list's leading edge — never a step for the chevron and another for
/// the depth on top of it.
#[test]
fn a_branch_row_starts_on_the_column_its_neighbours_do() {
    let marks = Rc::new(TreeMarks::default());
    let mut list = nested().with_tree(Rc::clone(&marks));
    let _ = super::tests::lay_out(&mut list, 300.0);
    let branch = list.row_rect(&0).expect("the branch row");
    let leaf = list.row_rect(&3).expect("a leaf at the same depth");
    assert_eq!(list.row_depth(&0), 0);
    assert_eq!(list.row_depth(&3), 0);
    assert!(
        (branch.x0 - leaf.x0).abs() < 0.5,
        "rows at one depth stand on one column: {} and {}",
        branch.x0,
        leaf.x0
    );

    // The top level stands the one step its chevron takes in from the list's leading edge:
    // the rows' column is `lead` in, and the chevron hangs in the step in front of it.
    let column = branch.x0 + list.lead;
    assert!(
        (column - (branch.x0 + TREE_INDENT)).abs() < 0.5,
        "the top level stands one step in from the list's leading edge, not two: {column} \
         from {}",
        branch.x0
    );
    let chevron = list
        .disclosure_rect(&0)
        .expect("the branch shows its disclosure");
    assert!(
        (chevron.x1 - column).abs() < 0.5,
        "the chevron ends where the row's box begins: {} against {column}",
        chevron.x1
    );
    assert!(
        (chevron.x0 - (column - TREE_INDENT)).abs() < 0.5,
        "it hangs in the step in front of the box: {} against {}",
        chevron.x0,
        column - TREE_INDENT
    );
    assert!(
        list.disclosure_rect(&3).is_none(),
        "a leaf folds nothing, and still keeps the step free"
    );
}

/// A tree whose rows open the item into a document of its own where they stand, one under
/// another: the rows say how they nest, and hold the fields that edit them.
struct OpenableTree {
    keys: Vec<u64>,
    parents: HashMap<u64, u64>,
}

impl ListModel for OpenableTree {
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
        Some(TreeRow {
            parent: self.parents.get(&key).copied(),
            depth: None,
            above: None,
        })
    }

    fn opens_in_place(&self, _index: usize) -> bool {
        true
    }
}

/// A tree row standing open into the item's document: its title field, the page under it and
/// the box that ticks it off.
#[test]
fn a_tree_row_open_in_its_place_hands_a_reader_what_it_holds() {
    use crate::a11y::Action;
    use crate::headless::FrameHarness;
    use crate::widget::{Checkbox, Column, TextInput};
    use accesskit::Role as AkRole;

    let done = crate::LocalState::new(false);
    let state = done.clone();
    let model = OpenableTree {
        keys: vec![0, 1, 2],
        parents: HashMap::from([(1, 0)]),
    };
    let list = VirtualList::with_selection_delegate(model, 20.0, move |_, _, key, build| {
        if build.expanded {
            let document: Element = Box::new(
                Column::new()
                    .push(Box::new(
                        TextInput::new(Vec::new())
                            .with_text_role(crate::tokens::TextRole::Title)
                            .with_id("title")
                            .with_accessible_name("Title")
                            .with_initial_value("Subtasks"),
                    ))
                    .push(Box::new(
                        TextInput::new(Vec::new())
                            .with_id("notes")
                            .multiline(true)
                            .with_accessible_name("Notes"),
                    ))
                    .push(Box::new(
                        Checkbox::from_state(&state).with_accessible_name("Done"),
                    )),
            );
            Box::new(super::open::RowOpen::new(document, true)) as Element
        } else {
            Box::new(super::tests::ProbeRow::new(
                format!("task {key}"),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            )) as Element
        }
    })
    .with_tree(Rc::new(TreeMarks::default()))
    .with_viewport_height(300.0);
    let mut harness = FrameHarness::for_root(crate::State::new(), list, (400, 500), Theme::dark());
    harness
        .root_mut::<VirtualList<OpenableTree>>()
        .set_open(Some(0));
    harness.relayout();

    let nodes = harness.a11y_nodes();
    let (row_id, row) = super::tests::named(&nodes, AkRole::TreeItem, "Subtasks");
    let row_bounds = row.bounds().expect("the row stands where it is drawn");
    let controls = super::tests::inside(&nodes, row_id);
    assert!(
        controls
            .iter()
            .all(|(_, node)| !matches!(node.role(), AkRole::ListItem | AkRole::TreeItem)),
        "the open row is one tree item, not one inside another"
    );
    super::tests::named(&controls, AkRole::TextInput, "Title");
    super::tests::named(&controls, AkRole::MultilineTextInput, "Notes");
    let (checkbox, _) = super::tests::named(&controls, AkRole::CheckBox, "Done");
    for (id, _) in &controls {
        let bounds = super::tests::exported(&nodes, *id)
            .bounds()
            .expect("a control stands where it is drawn");
        assert!(
            super::tests::within(row_bounds, bounds),
            "{bounds:?} stands outside its row {row_bounds:?}"
        );
    }

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    assert!(done.with(|checked| *checked), "the tick reaches the box");
}
