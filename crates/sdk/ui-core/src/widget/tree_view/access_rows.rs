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

use super::{NodeId, TreeModel, TreeMove, TreeRowKey, TreeView};
use crate::a11y::{AccessNode, AccessState, Role};
use crate::arriving::Arrival;
use crate::widget::Widget;

/// The rows the last walk kept, and what every one of them shared.
#[derive(Default)]
pub(super) struct KeptAccessRows {
    context: Option<AccessRowsContext<'static>>,
    rows: rustc_hash::FxHashMap<NodeId, KeptAccessRow>,
    /// Counts the walks, to tell the rows the last one reached.
    walk: u64,
}

/// One node row as the last walk built it, and what it was built from.
struct KeptAccessRow {
    inputs: AccessRowInputs<'static>,
    node: Arc<AccessNode>,
    /// The last walk that reached the row.
    walk: u64,
}

/// What every node row of one walk is built from beyond its own inputs:
/// while it changes, no row is kept.
#[derive(Debug, PartialEq)]
struct AccessRowsContext<'a> {
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
    /// For a navigation list, the revision of what its rows are read from
    /// beyond their own inputs: the model (labels, keys, what each row
    /// shows), the checks the rows show and the presentation's theme.
    nav: Option<u64>,
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
            nav: self.nav,
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
    /// Which of [`TreeMove::ALL`] the hierarchy has room for.
    moves: [bool; 4],
    description: Option<Cow<'a, str>>,
    /// What the row reads from the model. A navigation row leaves it to the
    /// context's revision ([`AccessRowsContext::nav`]), which changes with
    /// the model, so a kept row costs no label compare or key allocation.
    model: Option<AccessRowModel<'a>>,
}

/// What a plain tree row reads from its model.
#[derive(Debug, PartialEq)]
struct AccessRowModel<'a> {
    shows_row_actions: bool,
    label: Cow<'a, str>,
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
            moves: self.moves,
            description: self
                .description
                .map(|description| Cow::Owned(description.into_owned())),
            model: self.model.map(|model| AccessRowModel {
                shows_row_actions: model.shows_row_actions,
                label: Cow::Owned(model.label.into_owned()),
                semantic_key: model.semantic_key,
            }),
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
    /// The tree's own node, without its rows: what focus capture and focus
    /// order read of it on every frame ([`Widget::accessibility_root`]).
    pub(super) fn access_root(&self) -> AccessNode {
        // Labels are runtime strings supplied by the host, not FTL keys.
        let value = self
            .selected()
            .and_then(|id| self.model.label(id))
            .map(str::to_owned);
        let mut node = AccessNode::new(Role::Tree)
            .with_optional_name(
                self.accessible_name
                    .as_deref()
                    .map(crate::i18n::I18nManager::lookup_plain)
                    .or_else(|| self.heading.clone()),
            )
            .with_optional_value(value)
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused())
                    .disabled(!self.is_enabled()),
            );
        if let Some(key) = &self.semantic_key {
            node = node.with_semantic_key(key.clone());
        }
        if let Some(sel) = self.selected()
            && let Some(idx) = self.visible_index_of(sel)
            && let Some((TreeRowKey::Node(_), depth)) = self.index.row_at(&self.model, idx)
        {
            node = node.with_position(idx.saturating_add(1), self.index.len());
            node = node.with_level(depth.saturating_add(1));
        }
        node
    }

    /// The visible rows of the accessibility tree, in order. A node row
    /// hosting no delegate widget and no rename field is the very row of the
    /// last walk while everything it is built from is the same; only the
    /// others are built.
    pub(super) fn access_rows(&self) -> Vec<Arc<AccessNode>> {
        let row_count = self.index.len();
        let context = self.access_rows_context(row_count);
        let mut kept = self.access_rows.borrow_mut();
        let kept = &mut *kept;
        if kept.context.as_ref() != Some(&context) {
            kept.rows.clear();
            kept.context = Some(context.into_owned());
        }
        // Rows stay where they are kept; each one this walk reaches is
        // stamped with it, and only when some were not reached are the
        // others let go.
        kept.walk = kept.walk.wrapping_add(1);
        let walk = kept.walk;
        let mut reached = 0_usize;
        let visible = self.visible_row_range();
        let mut rows = Vec::with_capacity(visible.len());
        for index in visible {
            let Some((key, depth)) = self.index.row_at(&self.model, index) else {
                continue;
            };
            let delegate = self.mounted_rows.iter().find(|row| row.key == key);
            let plain_node = match key {
                TreeRowKey::Node(id)
                    if delegate.is_none()
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
            reached = reached.saturating_add(1);
            if let Some(row) = kept.rows.get_mut(&id).filter(|row| row.inputs == inputs)
                && self.kept_access_row_holds(row, id, index, depth, row_count)
            {
                row.walk = walk;
                rows.push(Arc::clone(&row.node));
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
                    walk,
                },
            );
        }
        if kept.rows.len() != reached {
            kept.rows.retain(|_, row| row.walk == walk);
        }
        rows
    }

    /// The node rows `Self::access_rows` built rather than kept, since the
    /// tree was made.
    #[cfg(any(test, feature = "test-support"))]
    pub fn access_row_builds_for_test(&self) -> usize {
        self.access_row_builds.get()
    }

    /// Run `walk` with no row kept, as a tree that keeps none walks, and
    /// keep the rows kept before it again afterwards.
    #[cfg(test)]
    pub(crate) fn walk_keeping_nothing<R>(&self, walk: impl FnOnce() -> R) -> R {
        let kept = self.access_rows.replace(KeptAccessRows::default());
        let walked = walk();
        *self.access_rows.borrow_mut() = kept;
        walked
    }

    /// What every node row of this walk shares.
    fn access_rows_context(&self, row_count: usize) -> AccessRowsContext<'_> {
        AccessRowsContext {
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
            nav: self.nav_revision(),
        }
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
        let model = (!self.is_nav()).then(|| AccessRowModel {
            shows_row_actions: self.model.shows_row_actions(id),
            label: Cow::Borrowed(label),
            semantic_key: self.model.semantic_key(id),
        });
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
            moves: TreeMove::ALL
                .map(|movement| reorderable && self.move_target(id, movement).is_some()),
            description: self
                .row_descriptions
                .get(&id)
                .map(|description| Cow::Borrowed(description.as_str())),
            model,
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
                Arrival::Ready(_) => return None,
                Arrival::Pending { .. } => (
                    delegate_label.unwrap_or_else(|| {
                        crate::i18n::I18nManager::lookup_plain("arriving-loading")
                    }),
                    None,
                    true,
                    false,
                    false,
                ),
                Arrival::Failed(message) => {
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
