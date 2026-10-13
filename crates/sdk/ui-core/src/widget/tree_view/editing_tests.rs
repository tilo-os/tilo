//! Rename, structural moves, context menu, row and heading actions, and the
//! drag feedback built on them.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use automerge::AutoCommit;
use taffy::TaffyTree;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::tests::{
    cursor_moved, dispatch_laid_out, draw_view, key_press, lay_out_tree, mouse_press,
    mouse_release, navigator_tree, row_center,
};
use super::*;
use crate::a11y::{Action, Role};
use crate::presence::PresenceCtx;
use crate::testing::Dispatch;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::context_menu::MenuItem;

type Log<T> = Rc<RefCell<Vec<T>>>;

fn log<T>() -> Log<T> {
    Rc::new(RefCell::new(Vec::new()))
}

fn dispatch_with<T: 'static, M: TreeModel + 'static>(
    view: &mut TreeView<T, M>,
    event: &WindowEvent,
    taffy: &TaffyTree<()>,
    node: taffy::NodeId,
    modifiers: ModifiersState,
) -> bool {
    Dispatch::new()
        .modifiers(modifiers)
        .layout(taffy, node)
        .send(view, event)
        .handled
}

fn right_press() -> WindowEvent {
    crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right)
}

fn typed(text: &str) -> WindowEvent {
    let WindowEvent::KeyboardInput {
        device_id,
        mut event,
        is_synthetic,
    } = key_press(Key::Character(text.into()))
    else {
        unreachable!("key_press builds keyboard input");
    };
    event.text = Some(text.into());
    WindowEvent::KeyboardInput {
        device_id,
        event,
        is_synthetic,
    }
}

fn semantic(view: &mut TreeView<()>, target: &str, action: &Action) -> bool {
    let mut doc = AutoCommit::new();
    Widget::perform_semantic_action(
        view,
        target,
        action,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    )
}

/// Visible `(id, depth)` pairs.
fn rows<T, M: TreeModel>(view: &TreeView<T, M>) -> Vec<(NodeId, usize)> {
    view.visible_rows()
        .into_iter()
        .map(|(id, _, depth)| (id, depth))
        .collect()
}

/// `root → [a(1), b(2), c(3)]`, root hidden, reorderable through the owned
/// model.
fn flat_list() -> TreeView<()> {
    TreeView::new(TreeNode::branch(
        10,
        "root",
        (),
        vec![
            TreeNode::leaf(1, "a", ()),
            TreeNode::leaf(2, "b", ()),
            TreeNode::branch(3, "c", (), Vec::new()),
        ],
    ))
    .hide_root()
    .reorderable()
}

fn ids(node: &TreeNode<()>) -> Vec<NodeId> {
    node.children.iter().map(|child| child.id).collect()
}

// --- structural moves ------------------------------------------------------

#[test]
fn keyboard_and_assistive_moves_reorder_through_the_commit() {
    // Case `alt_arrows_move_the_focused_row_through_the_reorder_commit`.
    {
        let mut view = flat_list();
        view.set_focus(true);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        view.focused = Some(TreeRowKey::Node(1));
        let alt = ModifiersState::ALT;

        assert!(dispatch_with(
            &mut view,
            &key_press(Key::Named(NamedKey::ArrowDown)),
            &taffy,
            node,
            alt
        ));
        assert_eq!(
            ids(view.model()),
            [2, 1, 3],
            "Alt+Down moves after the next"
        );
        assert_eq!(view.focused(), Some(1), "the moved row keeps focus");

        assert!(dispatch_with(
            &mut view,
            &key_press(Key::Named(NamedKey::ArrowUp)),
            &taffy,
            node,
            alt
        ));
        assert_eq!(ids(view.model()), [1, 2, 3]);
        assert!(
            !dispatch_with(
                &mut view,
                &key_press(Key::Named(NamedKey::ArrowUp)),
                &taffy,
                node,
                alt
            ),
            "the first row has nowhere up to go"
        );
        assert_eq!(
            view.focused(),
            Some(1),
            "and a refused move is not navigation"
        );

        // Indent into the previous sibling only when it accepts children.
        view.focused = Some(TreeRowKey::Node(2));
        assert!(
            !dispatch_with(
                &mut view,
                &key_press(Key::Named(NamedKey::ArrowRight)),
                &taffy,
                node,
                alt
            ),
            "a leaf above cannot adopt"
        );
        view.focused = Some(TreeRowKey::Node(3));
        view.move_node(3, TreeMove::Up);
        assert_eq!(ids(view.model()), [1, 3, 2]);
        view.focused = Some(TreeRowKey::Node(2));
        assert!(dispatch_with(
            &mut view,
            &key_press(Key::Named(NamedKey::ArrowRight)),
            &taffy,
            node,
            alt
        ));
        assert_eq!(ids(view.model()), [1, 3]);
        assert!(
            view.is_expanded(3),
            "the new parent opens to show its child"
        );
        assert_eq!(rows(&view), [(1, 0), (3, 0), (2, 1)]);

        assert!(dispatch_with(
            &mut view,
            &key_press(Key::Named(NamedKey::ArrowLeft)),
            &taffy,
            node,
            alt
        ));
        assert_eq!(
            ids(view.model()),
            [1, 3, 2],
            "Alt+Left lands after the parent"
        );
    }

    // Case `tab_indents_only_when_asked_and_only_when_it_moves`.
    {
        let mut view = flat_list();
        view.set_focus(true);
        view.move_node(3, TreeMove::Up);
        view.focused = Some(TreeRowKey::Node(2));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let tab = key_press(Key::Named(NamedKey::Tab));
        assert!(
            !dispatch_with(&mut view, &tab, &taffy, node, ModifiersState::empty()),
            "Tab stays focus traversal by default"
        );

        let mut view = flat_list().tab_indents(true);
        view.set_focus(true);
        view.move_node(3, TreeMove::Up);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        view.focused = Some(TreeRowKey::Node(1));
        assert!(
            !dispatch_with(&mut view, &tab, &taffy, node, ModifiersState::empty()),
            "a first row cannot indent, so Tab traverses"
        );
        view.focused = Some(TreeRowKey::Node(2));
        assert!(dispatch_with(
            &mut view,
            &tab,
            &taffy,
            node,
            ModifiersState::empty()
        ));
        assert_eq!(rows(&view), [(1, 0), (3, 0), (2, 1)]);
        assert!(dispatch_with(
            &mut view,
            &tab,
            &taffy,
            node,
            ModifiersState::SHIFT
        ));
        assert_eq!(rows(&view), [(1, 0), (3, 0), (2, 0)]);
    }

    // Case `moves_are_buttons_for_assistive_technology`.
    {
        let mut view = flat_list();
        let _ = lay_out_tree(&mut view, 240.0);
        let semantics = Widget::accessibility(&view).unwrap();
        let first = &semantics.children[0];
        let moves: Vec<_> = first
            .children
            .iter()
            .filter(|child| child.role == Role::Button)
            .map(|child| {
                (
                    child.action_target.clone().unwrap(),
                    child.state.disabled,
                    child.name.clone().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            moves
                .iter()
                .map(|(target, disabled, _)| (target.as_str(), *disabled))
                .collect::<Vec<_>>(),
            [
                ("move:up:1", true),
                ("move:down:1", false),
                ("move:indent:1", true),
                ("move:outdent:1", true),
            ]
        );
        assert!(moves.iter().all(|(_, _, name)| !name.is_empty()));
        assert!(semantic(&mut view, "move:down:1", &Action::Activate));
        assert_eq!(ids(view.model()), [2, 1, 3]);
        assert!(!semantic(&mut view, "move:up:2", &Action::Activate));
        assert!(!semantic(&mut view, "move:down:1", &Action::Increment));

        let mut plain = TreeView::new(navigator_tree()).hide_root();
        let _ = lay_out_tree(&mut plain, 240.0);
        let semantics = Widget::accessibility(&plain).unwrap();
        assert!(
            semantics
                .children
                .iter()
                .all(|item| item.children.is_empty()),
            "a tree that cannot reorder offers no moves"
        );
    }
}

// --- drops -------------------------------------------------------------------

/// A hierarchy in which every node may hold children (an outline), backed by
/// the owned tree. Leaves keep no disclosure.
struct Outline(TreeNode<()>);

impl TreeModel for Outline {
    fn root_id(&self) -> NodeId {
        self.0.root_id()
    }
    fn contains(&self, id: NodeId) -> bool {
        self.0.contains(id)
    }
    fn label(&self, id: NodeId) -> Option<&str> {
        self.0.label(id)
    }
    fn is_branch(&self, id: NodeId) -> bool {
        self.0
            .find(id)
            .is_some_and(|node| !node.children.is_empty())
    }
    fn accepts_children(&self, id: NodeId) -> bool {
        self.0.contains(id)
    }
    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        self.0.parent_id(id)
    }
    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        self.0.child_index(parent, child)
    }
    fn child_count(&self, parent: NodeId) -> usize {
        self.0.child_count(parent)
    }
    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.0.child_id(parent, index)
    }
}

fn outline(drops: &Log<ReorderDrop<NodeId>>) -> TreeView<(), Outline> {
    let drops = Rc::clone(drops);
    TreeView::from_model(Outline(TreeNode::branch(
        10,
        "root",
        (),
        vec![
            TreeNode::leaf(1, "a", ()),
            TreeNode::leaf(2, "b", ()),
            TreeNode::branch(3, "c", (), vec![TreeNode::leaf(4, "d", ())]),
        ],
    )))
    .hide_root()
    .with_reorder(
        |_, _, _| true,
        move |model, dropped| {
            drops.borrow_mut().push(dropped.clone());
            let Some(moved) = TreeNode::take_descendant(&mut model.0, dropped.source) else {
                return false;
            };
            match dropped.target.placement {
                DropPlacement::Inside => {
                    let Some(parent) = model.0.find_mut(dropped.target.key) else {
                        return false;
                    };
                    parent.children.push(moved);
                }
                DropPlacement::Before | DropPlacement::After => {
                    let Some(parent_id) = model.0.parent_id(dropped.target.key) else {
                        return false;
                    };
                    let Some(parent) = model.0.find_mut(parent_id) else {
                        return false;
                    };
                    let Some(at) = parent
                        .children
                        .iter()
                        .position(|child| child.id == dropped.target.key)
                    else {
                        return false;
                    };
                    let at = at + usize::from(dropped.target.placement == DropPlacement::After);
                    parent.children.insert(at, moved);
                }
            }
            true
        },
    )
}

fn drag<M: TreeModel + 'static>(
    view: &mut TreeView<(), M>,
    taffy: &TaffyTree<()>,
    node: taffy::NodeId,
    from: Point,
    to: Point,
) {
    dispatch_laid_out(view, &cursor_moved(from.x, from.y), taffy, node);
    dispatch_laid_out(view, &mouse_press(), taffy, node);
    dispatch_laid_out(view, &cursor_moved(to.x, to.y), taffy, node);
}

#[test]
fn pointer_drops_target_leaves_and_seams_with_a_lifted_scrolling_preview() {
    // Case `a_leaf_that_accepts_children_takes_a_drop_and_opens`.
    {
        let drops = log();
        let mut view = outline(&drops);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let from = row_center(&view, 1);
        let onto = row_center(&view, 2);
        drag(&mut view, &taffy, node, from, onto);
        assert_eq!(
            view.reorder_target(),
            Some(&DropTarget::new(2, DropPlacement::Inside)),
            "the middle of a leaf nests into it"
        );
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert!(view.is_expanded(2), "the adopting row opens");
        assert_eq!(rows(&view), [(2, 0), (1, 1), (3, 0)]);
        assert_eq!(drops.borrow().len(), 1);
    }

    // Case `the_seam_under_an_expanded_branch_is_before_its_first_child`.
    {
        let drops = log();
        let mut view = outline(&drops).with_expanded([3]);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let branch = view.row_rect(3).unwrap();
        let from = row_center(&view, 1);
        drag(
            &mut view,
            &taffy,
            node,
            from,
            Point::new(from.x, branch.y1 - 1.0),
        );
        assert_eq!(
            view.reorder_target(),
            Some(&DropTarget::new(4, DropPlacement::Before)),
            "the line under an open branch means its first child"
        );
        let preview = view.reorder_preview().unwrap();
        assert!((preview.center().y - branch.y1).abs() < 1.5);
        assert!(
            preview.x0 > view.row_rect(3).unwrap().x0 + view.indent_px - 0.5,
            "indented to the child's depth"
        );
    }

    // Case `dragging_paints_a_lifted_copy_and_scrolls_at_the_edges`.
    {
        let children = (1_u64..=40)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(100, "root", (), children))
            .hide_root()
            .fill()
            .reorderable();
        view.place_at(Rect::new(0.0, 0.0, 240.0, 200.0));
        let (taffy, node) = (TaffyTree::<()>::new(), taffy::NodeId::from(0_u64));
        view.transitions.clear();
        let idle = {
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            scene.encoding().draw_tags.len()
        };
        let from = row_center(&view, 2);
        let edge = Point::new(from.x, view.rect.y1 - 2.0);
        drag(&mut view, &taffy, node, from, edge);
        let mut scene = Scene::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        assert!(
            scene.encoding().draw_tags.len() > idle + 2,
            "the dragged row is lifted as a painted copy, plus the drop line"
        );
        let before = view.scroll_offset();
        view.last_tick = Some(crate::time::now() - Duration::from_millis(50));
        let doc = AutoCommit::new();
        view.update(&doc, &automerge::ROOT);
        assert!(
            view.scroll_offset() > before,
            "holding a drag at the bottom edge reveals rows below"
        );
    }
}

// --- rename ----------------------------------------------------------------

fn renamable() -> (TreeView<()>, Log<(NodeId, String)>, Log<NodeId>) {
    let renamed = log();
    let activated = log();
    let r = Rc::clone(&renamed);
    let a = Rc::clone(&activated);
    let view = TreeView::new(navigator_tree())
        .hide_root()
        .with_expanded([2])
        .on_rename(move |id, title| r.borrow_mut().push((id, title)))
        .on_activate(move |id| a.borrow_mut().push(id))
        .rename_on_double_click();
    (view, renamed, activated)
}

#[test]
fn inline_rename_starts_commits_and_cancels() {
    // Case `f2_renames_the_focused_row_and_enter_commits`.
    {
        let (mut view, renamed, _) = renamable();
        view.set_focus(true);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        view.focused = Some(TreeRowKey::Node(5));
        assert!(dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::F2)),
            &taffy,
            node
        ));
        assert_eq!(view.renaming(), Some(5));
        assert!(view.rename_field().unwrap().is_plain(), "styled as the row");
        // Compare against the settled row, not one still easing in.
        view.transitions.clear();
        draw_view(&mut view);
        let field = view.rename_field().unwrap().get_rect();
        let row = view.row_rect(5).unwrap();
        assert!(
            field.y0 >= row.y0 - 0.5 && field.y1 <= row.y1 + 0.5,
            "{field:?} in {row:?}"
        );
        assert!(field.x0 > row.x0, "the field sits over the label");

        assert!(dispatch_laid_out(&mut view, &typed("!"), &taffy, node));
        assert!(
            dispatch_laid_out(
                &mut view,
                &key_press(Key::Named(NamedKey::ArrowUp)),
                &taffy,
                node
            ),
            "keys belong to the field"
        );
        assert_eq!(view.focused(), Some(5), "not to tree navigation");
        assert!(dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::Enter)),
            &taffy,
            node
        ));
        assert_eq!(view.renaming(), None);
        let renamed = renamed.borrow();
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].0, 5);
        assert!(renamed[0].1.contains('!'), "{:?}", renamed[0].1);
    }

    // Case `escape_cancels_and_blank_titles_are_dropped`.
    {
        let (mut view, renamed, _) = renamable();
        view.set_focus(true);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        assert!(view.begin_rename(3));
        dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::Escape)),
            &taffy,
            node,
        );
        assert_eq!(view.renaming(), None);
        assert!(renamed.borrow().is_empty());

        assert!(view.begin_rename(3));
        view.rename.as_mut().unwrap().input.select_all();
        dispatch_laid_out(&mut view, &typed(" "), &taffy, node);
        view.commit_rename();
        assert!(
            renamed.borrow().is_empty(),
            "a blank title keeps the old one"
        );

        let mut plain = TreeView::new(navigator_tree()).hide_root();
        assert!(!plain.begin_rename(3), "rename needs a host to apply it");
    }

    // Case `a_click_elsewhere_or_losing_focus_commits`.
    {
        let (mut view, renamed, _) = renamable();
        view.set_focus(true);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        assert!(view.begin_rename(3));
        draw_view(&mut view);
        let other = row_center(&view, 5);
        dispatch_laid_out(&mut view, &cursor_moved(other.x, other.y), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(view.renaming(), None);
        assert_eq!(*renamed.borrow(), [(3, "main.rs".to_owned())]);
        assert_eq!(view.selected(), Some(5), "the click still lands on its row");

        assert!(view.begin_rename(4));
        view.set_focus(false);
        assert_eq!(view.renaming(), None);
        assert_eq!(renamed.borrow().last(), Some(&(4, "lib.rs".to_owned())));
    }

    // Case `double_click_renames_instead_of_activating_when_asked`.
    {
        let (mut view, _, activated) = renamable();
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let at = Point::new(view.rect.x0 + 60.0, row_center(&view, 5).y);
        for _ in 0..2 {
            dispatch_laid_out(&mut view, &cursor_moved(at.x, at.y), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        }
        assert_eq!(view.renaming(), Some(5));
        assert!(activated.borrow().is_empty());
        let semantics = Widget::accessibility(&view).unwrap();
        let readme = semantics
            .children
            .iter()
            .find(|item| item.name.as_deref() == Some("README.md"))
            .unwrap();
        let field = readme
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some("rename:5"))
            .expect("the rename field is semantic");
        assert_eq!(field.role, Role::TextInput);
        assert!(semantic(
            &mut view,
            "rename:5",
            &Action::SetValue("NOTES.md".into())
        ));
        view.commit_rename();
        assert!(!semantic(
            &mut view,
            "rename:5",
            &Action::SetValue("stale".into())
        ));
    }
}

// --- context menu ------------------------------------------------------------

fn with_menu() -> (TreeView<()>, Log<(Option<NodeId>, usize)>, Log<NodeId>) {
    let chosen = log();
    let selected = log();
    let c = Rc::clone(&chosen);
    let s = Rc::clone(&selected);
    let view = TreeView::new(navigator_tree())
        .hide_root()
        .with_expanded([2])
        .with_context_menu(|model, target| match target {
            Some(id) => vec![
                MenuItem::action(format!("Open {}", model.label(id).unwrap_or("")), || {}),
                MenuItem::separator(),
                MenuItem::action("Delete", || {}),
            ],
            None => vec![MenuItem::action("New", || {})],
        })
        .on_context_menu_item(move |target, index| c.borrow_mut().push((target, index)))
        .on_select(move |id| s.borrow_mut().push(id));
    (view, chosen, selected)
}

#[test]
fn row_context_menu_opens_from_pointer_keys_and_semantics() {
    // Case `right_click_selects_the_row_and_opens_its_menu`.
    {
        let (mut view, chosen, selected) = with_menu();
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let at = row_center(&view, 4);
        dispatch_laid_out(&mut view, &cursor_moved(at.x, at.y), &taffy, node);
        assert!(dispatch_laid_out(&mut view, &right_press(), &taffy, node));
        assert!(view.context_menu_is_open());
        assert_eq!(*selected.borrow(), [4]);
        assert!(
            view.wants_pointer_capture(),
            "the open menu keeps the pointer"
        );

        // Up wraps to the last item (Delete, index 2 past the separator).
        dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::ArrowUp)),
            &taffy,
            node,
        );
        dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::Enter)),
            &taffy,
            node,
        );
        assert_eq!(*chosen.borrow(), [(Some(4), 2)]);
        assert!(!view.context_menu_is_open());

        let empty = Point::new(at.x, view.rect.y1 - 1.0);
        view.place_at(Rect::new(0.0, 0.0, 240.0, 200.0));
        dispatch_laid_out(&mut view, &cursor_moved(empty.x, 190.0), &taffy, node);
        dispatch_laid_out(&mut view, &right_press(), &taffy, node);
        dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::ArrowDown)),
            &taffy,
            node,
        );
        dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::Enter)),
            &taffy,
            node,
        );
        assert_eq!(
            chosen.borrow().last(),
            Some(&(None, 0)),
            "empty space offers the tree's own menu"
        );
    }

    // Case `the_menu_key_and_shift_f10_open_on_the_focused_row`.
    {
        let (mut view, _, _) = with_menu();
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        view.focused = Some(TreeRowKey::Node(3));
        assert!(
            !dispatch_laid_out(
                &mut view,
                &key_press(Key::Named(NamedKey::ContextMenu)),
                &taffy,
                node
            ),
            "an unfocused tree leaves the key alone"
        );
        view.set_focus(true);
        assert!(dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::ContextMenu)),
            &taffy,
            node
        ));
        assert!(view.context_menu_is_open());
        assert_eq!(view.menu_target, Some(3));
        dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::Escape)),
            &taffy,
            node,
        );
        assert!(!view.context_menu_is_open());
        assert!(dispatch_with(
            &mut view,
            &key_press(Key::Named(NamedKey::F10)),
            &taffy,
            node,
            ModifiersState::SHIFT
        ));
        assert!(view.context_menu_is_open());
    }

    // Case `the_menu_is_semantic_and_show_context_menu_opens_it`.
    {
        let (mut view, chosen, _) = with_menu();
        let _ = lay_out_tree(&mut view, 240.0);
        assert!(semantic(&mut view, "node:5", &Action::ShowContextMenu));
        let semantics = Widget::accessibility(&view).unwrap();
        let menu = semantics
            .children
            .iter()
            .find(|child| child.role == Role::Menu)
            .expect("the open menu is in the tree's semantics");
        let names: Vec<_> = menu
            .children
            .iter()
            .map(|item| item.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["Open README.md", "Delete"]);
        let delete = menu.children[1].action_target.clone().unwrap();
        assert!(delete.starts_with("menu:"));
        assert!(!semantic(&mut view, &delete, &Action::Increment));
        assert!(semantic(&mut view, &delete, &Action::Activate));
        assert_eq!(*chosen.borrow(), [(Some(5), 2)]);
        assert!(
            !semantic(&mut view, &delete, &Action::Activate),
            "a closed menu's target is stale"
        );
    }
}

// --- row and heading actions -------------------------------------------------

fn with_actions() -> (TreeView<()>, Log<(NodeId, usize)>, Rc<RefCell<usize>>) {
    let actions = log();
    let heading = Rc::new(RefCell::new(0));
    let a = Rc::clone(&actions);
    let h = Rc::clone(&heading);
    let view = TreeView::new(navigator_tree())
        .hide_root()
        .with_heading("Files")
        .with_heading_action(
            TreeHeadingAction::new(Icon::Plus, "New file")
                .accessible_name("New file")
                .description("Create a file at the top level"),
        )
        .on_heading_action(move || *h.borrow_mut() += 1)
        .with_row_actions(vec![
            TreeRowAction::new(Icon::Plus, "New child").accessible_name("New child under"),
            TreeRowAction::new(Icon::MoreHorizontal, "More").opens_context_menu(),
        ])
        .on_row_action(move |id, index| a.borrow_mut().push((id, index)))
        .with_context_menu(|_, _| vec![MenuItem::action("Rename", || {})])
        .with_row_hint("Click to open")
        .reorderable();
    (view, actions, heading)
}

#[test]
fn row_affordances_expose_actions_deletion_and_presence() {
    // Case `row_actions_show_on_hover_and_act_without_selecting_or_dragging`.
    {
        let (mut view, actions, _) = with_actions();
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let plus = view.row_action_rect(5, 0).unwrap();
        let more = view.row_action_rect(5, 1).unwrap();
        assert!(
            plus.x1 <= more.x0,
            "actions keep their order at the trailing edge"
        );
        assert!(view.row_rect(5).unwrap().contains(plus.center()));

        dispatch_laid_out(
            &mut view,
            &cursor_moved(plus.center().x, plus.center().y),
            &taffy,
            node,
        );
        assert_eq!(
            view.tooltip_target_at(plus.center()),
            Some((plus, "New child".to_owned()))
        );
        assert!(dispatch_laid_out(&mut view, &mouse_press(), &taffy, node));
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(*actions.borrow(), [(5, 0)]);
        assert_eq!(view.selected(), None, "an action is not a row click");
        assert!(!view.reorder.is_tracking(), "nor the start of a drag");

        dispatch_laid_out(
            &mut view,
            &cursor_moved(more.center().x, more.center().y),
            &taffy,
            node,
        );
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        assert!(
            view.context_menu_is_open(),
            "the ellipsis opens the row menu"
        );
        assert_eq!(view.selected(), Some(5));

        let row = row_center(&view, 2);
        view.close_context_menu();
        let hint = view.tooltip_target_at(Point::new(view.rect.x0 + 40.0, row.y));
        assert_eq!(
            hint.map(|(_, text)| text),
            Some("src\nClick to open".to_owned())
        );
    }

    // Case `heading_action_is_a_button_before_the_rows`.
    {
        let (mut view, actions, heading) = with_actions();
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        let button = view.heading_action_rect().unwrap();
        assert!(
            button.y1 <= view.body_top() + 0.5,
            "inside the heading band"
        );
        assert!(button.x1 <= view.rect.x1);
        dispatch_laid_out(
            &mut view,
            &cursor_moved(button.center().x, button.center().y),
            &taffy,
            node,
        );
        assert!(dispatch_laid_out(&mut view, &mouse_press(), &taffy, node));
        assert_eq!(*heading.borrow(), 1, "fires on press");

        let semantics = Widget::accessibility(&view).unwrap();
        let first = &semantics.children[0];
        assert_eq!(first.role, Role::Button);
        assert_eq!(first.name.as_deref(), Some("New file"));
        assert_eq!(
            first.description.as_deref(),
            Some("Create a file at the top level")
        );
        assert_eq!(
            Widget::synthetic_accessibility_rect(&view, &[0]),
            Some(button)
        );
        let item = &semantics.children[1];
        assert_eq!(item.role, Role::TreeItem);
        let child_action = item
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some("row-action:0:2"))
            .unwrap();
        assert_eq!(child_action.name.as_deref(), Some("New child under src"));
        assert!(semantic(&mut view, "row-action:0:2", &Action::Activate));
        assert_eq!(*actions.borrow(), [(2, 0)]);
        assert!(semantic(&mut view, "heading-action", &Action::Activate));
        assert_eq!(*heading.borrow(), 2);

        assert!(Widget::focus_synthetic_accessibility(&mut view, &[2]));
        assert_eq!(
            view.focused(),
            Some(5),
            "item paths skip the heading button"
        );
    }

    // Case `delete_reports_the_focused_row`.
    {
        let deleted = log();
        let d = Rc::clone(&deleted);
        let mut view = TreeView::new(navigator_tree())
            .hide_root()
            .on_delete(move |id| d.borrow_mut().push(id));
        view.set_focus(true);
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        view.focused = Some(TreeRowKey::Node(5));
        assert!(dispatch_laid_out(
            &mut view,
            &key_press(Key::Named(NamedKey::Delete)),
            &taffy,
            node
        ));
        assert_eq!(*deleted.borrow(), [5]);
        assert!(view.model().contains(5), "the host decides");
    }

    // Case `presence_mist_marks_rows_collaborators_are_on`.
    {
        use crate::permissions::UserId;
        use crate::presence::PresenceKey;
        let mut view = TreeView::new(navigator_tree())
            .hide_root()
            .with_row_decoration(|_, row, _| {
                TreeRowDecoration::new().presence(PresenceKey::Tab {
                    id: row.id.to_string(),
                })
            });
        let _ = lay_out_tree(&mut view, 240.0);
        let draw = |view: &mut TreeView<()>, presence: &PresenceCtx| {
            view.transitions.clear();
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                presence,
            );
            scene.encoding().draw_tags.len()
        };
        let alone = draw(&mut view, &PresenceCtx::empty());
        let mut presence = PresenceCtx::empty();
        presence.set_tab_focus(&PresenceKey::Tab { id: "5".into() }, UserId("ada".into()));
        assert!(draw(&mut view, &presence) > alone);
    }
}

fn long_list() -> TreeView<()> {
    let children = (1_u64..=40)
        .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
        .collect();
    TreeView::new(TreeNode::branch(100, "root", (), children))
        .hide_root()
        .fill()
}

#[test]
fn a_reveal_before_the_first_layout_waits_for_a_viewport() {
    let mut view = long_list();
    view.reveal(3);
    view.place_at(Rect::new(0.0, 0.0, 240.0, 200.0));
    assert!(
        view.scroll_offset().abs() < f64::EPSILON,
        "a row already in view does not scroll the first page away"
    );
    let mut view = long_list();
    view.reveal(40);
    view.place_at(Rect::new(0.0, 0.0, 240.0, 200.0));
    assert!(
        view.row_rect(40).is_some(),
        "the far row is brought into view"
    );
}

// --- scrolling -------------------------------------------------------------

#[test]
fn a_filled_tree_hides_its_bar_at_rest_and_shows_it_while_scrolling() {
    let children = (1_u64..=40)
        .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
        .collect();
    let mut view = TreeView::new(TreeNode::branch(100, "root", (), children))
        .hide_root()
        .fill();
    view.place_at(Rect::new(0.0, 0.0, 240.0, 200.0));
    view.transitions.clear();
    let paint = |view: &mut TreeView<()>| {
        let mut scene = Scene::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    };
    paint(&mut view);
    assert!(view.bar_fade.get().is_idle(), "no bar at rest");
    assert!(view.next_wake_deadline().is_none());

    view.set_scroll_offset(120.0);
    paint(&mut view);
    let now = crate::time::now();
    assert!(
        view.bar_fade.get().opacity_at(now, false) > 0.99,
        "the bar shows while the rows scroll"
    );
    assert!(
        view.next_wake_deadline().is_some(),
        "and the tree wakes to fade it out"
    );
}

#[test]
fn hosted_affordance_focus_refuses_disabled_moves_and_expires_after_model_changes() {
    use crate::headless::FrameHarness;
    use crate::state::State;
    let view = flat_list().with_id("tree");
    let mut h = FrameHarness::for_root(State::new(), view, (280, 360), Theme::dark());
    h.relayout();
    assert!(
        h.perform_semantic_named("tree", Some("move:up:1"), &Action::Focus)
            .is_err(),
        "a disabled published move cannot receive focus"
    );
    h.perform_semantic_named("tree", Some("move:down:1"), &Action::Focus)
        .expect("available move takes focus");
    h.key("Enter");
    assert_eq!(ids(h.root::<TreeView<()>>().model()), [2, 1, 3]);
    h.key("Enter");
    assert_eq!(
        ids(h.root::<TreeView<()>>().model()),
        [2, 1, 3],
        "the changed model invalidates the old affordance focus"
    );
    h.perform_semantic_named("tree", Some("move:down:1"), &Action::Focus)
        .expect("current move may be explicitly focused anew");
    h.key("ArrowUp");
    h.key("Enter");
    assert_eq!(
        ids(h.root::<TreeView<()>>().model()),
        [2, 1, 3],
        "ordinary row navigation clears button focus"
    );
}
