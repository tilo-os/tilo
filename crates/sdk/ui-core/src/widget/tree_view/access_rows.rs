//! The tree's rows of the accessibility tree, kept between walks.
//!
//! A tree beside an editor (a notebook's page list) is walked on every
//! keystroke the editor takes, and nothing about its rows changed. Each node
//! row is kept with what it was built from ([`AccessRowInputs`], and what
//! every row shares, [`AccessRowsContext`]); while those are the same the
//! walk hands over the very `Arc` it handed over last time, which the
//! semantic tree takes as unchanged without comparing it
//! ([`Widget::accessibility_shared`]). Debug builds build the kept rows too
//! and prove them equal.

use std::borrow::Cow;
use std::sync::Arc;

use vello::kurbo::Rect;

use super::{NodeId, TreeChildState, TreeModel, TreeMove, TreeRowKey, TreeView};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::widget::Widget;

/// The rows the last walk kept, and what every one of them shared.
#[derive(Default)]
pub(super) struct KeptAccessRows {
    context: Option<AccessRowsContext<'static>>,
    rows: rustc_hash::FxHashMap<NodeId, KeptAccessRow>,
}

/// One node row as the last walk built it, and what it was built from.
struct KeptAccessRow {
    inputs: AccessRowInputs<'static>,
    node: Arc<AccessNode>,
}

/// What every node row of one walk is built from beyond its own inputs:
/// while it changes, no row is kept.
#[derive(Debug, PartialEq)]
struct AccessRowsContext<'a> {
    /// Every row's "N of M".
    row_count: usize,
    disabled: bool,
    /// The revision of the translations the move buttons' labels are looked
    /// up in.
    labels: u64,
    /// The affordance holding the tree's focus, if any.
    focused_target: Option<Cow<'a, str>>,
    /// The row actions' spoken names, and the spacing their squares are
    /// placed by.
    row_actions: Vec<Cow<'a, str>>,
    spacing: crate::tokens::Spacing,
    /// Whether every row carries the move buttons.
    reorderable: bool,
}

impl AccessRowsContext<'_> {
    fn into_owned(self) -> AccessRowsContext<'static> {
        AccessRowsContext {
            row_count: self.row_count,
            disabled: self.disabled,
            labels: self.labels,
            focused_target: self
                .focused_target
                .map(|target| Cow::Owned(target.into_owned())),
            row_actions: self
                .row_actions
                .into_iter()
                .map(|name| Cow::Owned(name.into_owned()))
                .collect(),
            spacing: self.spacing,
            reorderable: self.reorderable,
        }
    }
}

/// Everything a plain node row is built from besides the
/// [`AccessRowsContext`]: while these are the same, so is the row. The
/// geometry a scroll moves is compared first, the text last.
#[derive(Debug, PartialEq)]
struct AccessRowInputs<'a> {
    /// The row's bounds, and the part of it on screen its buttons sit in.
    bounds: Option<Rect>,
    visible: Option<Rect>,
    index: usize,
    depth: usize,
    expanded: Option<bool>,
    selected: bool,
    focused: bool,
    shows_row_actions: bool,
    /// Which of [`TreeMove::ALL`] the hierarchy has room for.
    moves: [bool; 4],
    label: Cow<'a, str>,
    description: Option<Cow<'a, str>>,
    semantic_key: Option<String>,
}

impl AccessRowInputs<'_> {
    fn into_owned(self) -> AccessRowInputs<'static> {
        AccessRowInputs {
            bounds: self.bounds,
            visible: self.visible,
            index: self.index,
            depth: self.depth,
            expanded: self.expanded,
            selected: self.selected,
            focused: self.focused,
            shows_row_actions: self.shows_row_actions,
            moves: self.moves,
            label: Cow::Owned(self.label.into_owned()),
            description: self
                .description
                .map(|description| Cow::Owned(description.into_owned())),
            semantic_key: self.semantic_key,
        }
    }
}

impl<T, M: TreeModel> TreeView<T, M> {
    /// The affordance holding the tree's focus, while the tree has it.
    pub(super) fn focused_affordance_target(&self) -> Option<&str> {
        self.focused_affordance
            .as_deref()
            .filter(|_| self.is_focused && self.affordance_focus_valid)
    }
}

impl<T: 'static, M: TreeModel + 'static> TreeView<T, M> {
    /// The visible rows of the accessibility tree, in order. A node row
    /// hosting no delegate widget and no rename field is the very row of the
    /// last walk while everything it is built from is the same; only the
    /// others are built.
    pub(super) fn access_rows(&self) -> Vec<Arc<AccessNode>> {
        let row_count = self.index.len();
        let context = self.access_rows_context(row_count);
        let mut kept = self.access_rows.borrow_mut();
        let kept = &mut *kept;
        if kept.context.as_ref() != context.as_ref() {
            kept.rows.clear();
            kept.context = context.map(AccessRowsContext::into_owned);
        }
        let keeps = kept.context.is_some();
        let mut previous = std::mem::take(&mut kept.rows);
        let visible = self.visible_row_range();
        let mut rows = Vec::with_capacity(visible.len());
        for index in visible {
            let Some((key, depth)) = self.index.row_at(&self.model, index) else {
                continue;
            };
            let delegate = self.mounted_rows.iter().find(|row| row.key == key);
            let plain_node = match key {
                TreeRowKey::Node(id)
                    if keeps
                        && delegate.is_none()
                        && self.rename.as_ref().is_none_or(|field| field.id != id) =>
                {
                    Some(id)
                }
                TreeRowKey::Node(_) | TreeRowKey::Slot(_) => None,
            };
            let Some(id) = plain_node else {
                let delegate = delegate.and_then(|row| row.widget.accessibility());
                rows.extend(
                    self.access_row(index, key, depth, row_count, delegate)
                        .map(Arc::new),
                );
                continue;
            };
            let Some(inputs) = self.access_row_inputs(id, index, depth) else {
                continue;
            };
            if let Some(row) = previous.remove(&id).filter(|row| row.inputs == inputs)
                && self.kept_access_row_holds(&row, id, index, depth, row_count)
            {
                rows.push(Arc::clone(&row.node));
                kept.rows.insert(id, row);
                continue;
            }
            #[cfg(any(test, feature = "test-support"))]
            self.access_row_builds
                .set(self.access_row_builds.get().wrapping_add(1));
            let Some(node) = self.access_row(index, key, depth, row_count, None) else {
                continue;
            };
            let node = Arc::new(node);
            rows.push(Arc::clone(&node));
            kept.rows.insert(
                id,
                KeptAccessRow {
                    inputs: inputs.into_owned(),
                    node,
                },
            );
        }
        rows
    }

    /// The node rows [`Self::access_rows`] built rather than kept, since the
    /// tree was made.
    #[cfg(any(test, feature = "test-support"))]
    pub fn access_row_builds_for_test(&self) -> usize {
        self.access_row_builds.get()
    }

    /// What every node row of this walk shares, or `None` when rows are not
    /// kept: a navigation list's rows read its presentation for their
    /// descriptions, checks and section buttons.
    fn access_rows_context(&self, row_count: usize) -> Option<AccessRowsContext<'_>> {
        if self.nav.is_some() {
            return None;
        }
        Some(AccessRowsContext {
            row_count,
            disabled: !self.is_enabled(),
            labels: crate::i18n::I18nManager::revision(),
            focused_target: self.focused_affordance_target().map(Cow::Borrowed),
            row_actions: self
                .row_actions
                .iter()
                .map(|action| Cow::Borrowed(action.accessible_name.as_str()))
                .collect(),
            spacing: self.spacing,
            reorderable: self.reorder_validator.is_some() && self.reorder_commit.is_some(),
        })
    }

    /// What node row `id`, visible row `index`, is built from. `None` when
    /// the model has no label for it, which publishes no row.
    fn access_row_inputs(
        &self,
        id: NodeId,
        index: usize,
        depth: usize,
    ) -> Option<AccessRowInputs<'_>> {
        let label = self.model.label(id)?;
        let reorderable = self.reorder_validator.is_some() && self.reorder_commit.is_some();
        Some(AccessRowInputs {
            bounds: self.access_row_bounds(index),
            visible: self.visible_row_rect(index),
            index,
            depth,
            expanded: self
                .model
                .is_branch(id)
                .then(|| self.expanded.contains(&id)),
            selected: self.selection.is_selected(&id),
            focused: self.row_has_focus(TreeRowKey::Node(id)),
            shows_row_actions: self.model.shows_row_actions(id),
            moves: TreeMove::ALL
                .map(|movement| reorderable && self.move_target(id, movement).is_some()),
            label: Cow::Borrowed(label),
            description: self
                .row_descriptions
                .get(&id)
                .map(|description| Cow::Borrowed(description.as_str())),
            semantic_key: self.model.semantic_key(id),
        })
    }

    /// Whether the row kept for `id` is the row it would build now. Release
    /// builds trust its inputs; debug builds build it and compare, failing a
    /// test outright where they differ and rebuilding it (and logging) in any
    /// other debug build, so a row is never stale where it can be seen.
    fn kept_access_row_holds(
        &self,
        row: &KeptAccessRow,
        id: NodeId,
        index: usize,
        depth: usize,
        row_count: usize,
    ) -> bool {
        if !cfg!(debug_assertions) {
            return true;
        }
        let fresh = self.access_row(index, TreeRowKey::Node(id), depth, row_count, None);
        let holds = fresh.as_ref() == Some(&*row.node);
        assert!(
            holds || !cfg!(any(test, feature = "test-support")),
            "tree row {id} changed its accessibility node with nothing it is kept by changing: kept {:?}, built {fresh:?}",
            row.node,
        );
        if !holds {
            tracing::error!(
                row = id,
                "a tree row changed its accessibility node with nothing it is kept by changing"
            );
        }
        holds
    }

    /// Whether `key`'s row itself (not one of its buttons) has focus.
    fn row_has_focus(&self, key: TreeRowKey) -> bool {
        self.is_focused && self.focused_affordance.is_none() && self.focused == Some(key)
    }

    /// Visible row `index`'s bounds, whether or not it is on screen.
    fn access_row_bounds(&self, index: usize) -> Option<Rect> {
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?;
        Some(Rect::new(
            self.rect.x0,
            self.body_top() + top,
            self.rect.x1,
            self.body_top() + top + height,
        ))
    }

    /// Visible row `index`'s node: a tree item over `key` at `depth`, of
    /// `row_count` rows, holding `delegate_child` (the node of the widget a
    /// row delegate mounted for it) and its buttons. `None` for a slot whose
    /// child is ready (its node row stands for it) and a node without a
    /// label.
    fn access_row(
        &self,
        index: usize,
        key: TreeRowKey,
        depth: usize,
        row_count: usize,
        delegate_child: Option<AccessNode>,
    ) -> Option<AccessNode> {
        let delegate_label = delegate_child.as_ref().and_then(|child| child.name.clone());
        let (label, expanded, busy, invalid, selected) = match key {
            TreeRowKey::Node(id) => (
                self.model.label(id)?.to_owned(),
                self.model
                    .is_branch(id)
                    .then(|| self.expanded.contains(&id)),
                false,
                false,
                self.selection.is_selected(&id),
            ),
            TreeRowKey::Slot(slot) => match self.model.child_state(slot.parent, slot.index) {
                TreeChildState::Ready(_) => return None,
                TreeChildState::Loading => (
                    delegate_label.unwrap_or_else(|| "Loading".to_owned()),
                    None,
                    true,
                    false,
                    false,
                ),
                TreeChildState::Error { message } => {
                    let label = delegate_label.unwrap_or_else(|| {
                        format!("{}. Activate to retry", message.unwrap_or("Could not load"))
                    });
                    (label, None, false, true, false)
                }
            },
        };
        let mut item = AccessNode::new(Role::TreeItem)
            .with_action_target(match key {
                TreeRowKey::Node(id) => format!("node:{id}"),
                TreeRowKey::Slot(slot) => format!("slot:{}:{}", slot.parent, slot.index),
            })
            .with_name(label)
            .with_position(index.saturating_add(1), row_count)
            .with_level(depth.saturating_add(1))
            .with_state(AccessState {
                focusable: true,
                focused: self.row_has_focus(key),
                selected,
                expanded,
                disabled: !self.is_enabled(),
                busy,
                invalid,
                ..Default::default()
            });
        if let Some(bounds) = self.access_row_bounds(index) {
            item = item.with_bounds(bounds);
        }
        if let TreeRowKey::Node(id) = key
            && let Some(description) = self.row_descriptions.get(&id)
        {
            item = item.with_description(description.clone());
        }
        if let TreeRowKey::Node(id) = key
            && let Some((description, checked)) = self.nav_semantics(id)
        {
            if let Some(description) = description {
                item = item.with_description(description);
            }
            item.state.checked = checked;
        }
        if let TreeRowKey::Node(id) = key
            && let Some(semantic) = self.model.semantic_key(id)
        {
            item.key = Some(crate::a11y::hash_key(&semantic));
            item = item.with_semantic_key(semantic);
        }
        let mut item_children = Vec::new();
        if let Some(child) = delegate_child {
            item_children.push(child);
        }
        if let TreeRowKey::Node(id) = key {
            let label = self.model.label(id).unwrap_or_default();
            item_children.extend(self.row_semantic_children(
                id,
                label,
                self.visible_row_rect(index),
            ));
        }
        if !item_children.is_empty() {
            item = item.with_children(item_children);
        }
        Some(item)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::tests::lay_out_tree;
    use super::super::{TreeNode, TreeRowAction, TreeView};
    use super::KeptAccessRows;
    use crate::icons::Icon;
    use crate::widget::Widget;

    /// A page list of `count` pages under a hidden root, with a row action
    /// and the move buttons on every row, laid out with every row on screen.
    fn pages(count: u64) -> TreeView<()> {
        let children = (2..count.saturating_add(2))
            .map(|id| TreeNode::leaf(id, format!("Page {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children))
            .hide_root()
            .with_expanded([1])
            .with_row_actions(vec![
                TreeRowAction::new(Icon::Plus, "New page").accessible_name("New page under"),
            ])
            .with_reorder(|_, _, _| true, |_, _| true);
        let _ = lay_out_tree(&mut view, 320.0);
        view
    }

    /// The tree as a walk that kept nothing would publish it now.
    fn published_afresh(view: &TreeView<()>) -> Option<crate::a11y::AccessNode> {
        let kept = view.access_rows.replace(KeptAccessRows::default());
        let fresh = Widget::accessibility(view);
        *view.access_rows.borrow_mut() = kept;
        fresh
    }

    /// A walk that finds the tree as it was (a keystroke in the editor
    /// beside it) builds none of its rows and hands each over as the very
    /// node of the last walk, however many rows it lists. A change builds
    /// the rows it changed, and every walk publishes what a tree keeping
    /// nothing publishes.
    #[test]
    fn an_unchanged_tree_builds_no_row_and_a_change_builds_the_rows_it_changed() {
        for count in [3_u64, 24] {
            let rows = usize::try_from(count).expect("a small count");
            let mut view = pages(count);
            let first = Widget::accessibility_shared(&view).expect("the tree's node");
            assert_eq!(first.children.len(), rows, "every page is on screen");
            assert_eq!(
                view.access_row_builds_for_test(),
                rows,
                "the first walk builds every row"
            );

            let again = Widget::accessibility_shared(&view).expect("the tree's node");
            assert_eq!(
                view.access_row_builds_for_test(),
                rows,
                "a walk of {count} unchanged rows built one"
            );
            assert!(
                first
                    .children
                    .iter()
                    .zip(&again.children)
                    .all(|(before, after)| Arc::ptr_eq(before, after)),
                "an unchanged row is handed over as the very node it was"
            );

            // Selecting a page changes its row; moving the selection on
            // changes the row it left and the row it reached.
            view.select(3);
            let _ = Widget::accessibility_shared(&view);
            assert_eq!(view.access_row_builds_for_test(), rows + 1);
            view.select(4);
            let _ = Widget::accessibility_shared(&view);
            assert_eq!(view.access_row_builds_for_test(), rows + 3);
            assert_eq!(Widget::accessibility(&view), published_afresh(&view));

            // Focus reaching the tree is published as a fresh walk
            // publishes it.
            view.set_focus(true);
            let _ = Widget::accessibility_shared(&view);
            assert_eq!(Widget::accessibility(&view), published_afresh(&view));
        }
    }

    /// A row a change elsewhere moved (the tree laid out narrower) is built
    /// again: its bounds are part of what it is kept by.
    #[test]
    fn rows_a_change_moves_are_built_again() {
        let mut view = pages(6);
        let _ = Widget::accessibility_shared(&view);
        let builds = view.access_row_builds_for_test();
        let _ = lay_out_tree(&mut view, 280.0);
        let _ = Widget::accessibility_shared(&view);
        assert_eq!(
            view.access_row_builds_for_test(),
            builds + 6,
            "every row's bounds moved"
        );
        assert_eq!(Widget::accessibility(&view), published_afresh(&view));
    }
}
