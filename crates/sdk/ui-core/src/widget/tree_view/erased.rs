//! The [`TreeView`] apps hold: a typed face over one tree the toolkit compiles
//! once.
//!
//! `TreeImpl` is generic over its model, so an app showing a tree compiled
//! the whole widget again for its own model, about 100 KB each in Notes,
//! Tasks, Code Editor, Canvas and Backups. A `TreeView<T, M>` keeps its model
//! behind a trait object ([`DynTreeModel`]) and hands every frame's work to
//! [`TreeCore`], which is not generic: the toolkit compiles the tree once,
//! and an app compiles only this face, its builders and the casts that give
//! its closures and accessors their typed model back.

use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

use taffy::TaffyTree;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::WindowEvent;

use super::{
    NodeId, TreeChildState, TreeClickActivation, TreeHeadingAction, TreeImpl, TreeLoadRequest,
    TreeModel, TreeModelChange, TreeMove, TreeNode, TreeRowAction, TreeRowDecoration, TreeRowInfo,
    TreeRowKey,
};
use crate::presence::PresenceCtx;
use crate::reorder::{DropTarget, ReorderDrop};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::virtual_viewport::SparseAnchor;
use crate::widget::context_menu::MenuItem;
use crate::widget::{AppModel, Element, EventCtx, ModelPath, Widget};

/// A tree model a typed [`TreeView`] can get back from behind a trait object.
pub trait AnyTreeModel: TreeModel + Any {}

impl<M: TreeModel + Any> AnyTreeModel for M {}

/// The model a [`TreeView`] keeps: its own, as a trait object.
pub type DynTreeModel = Box<dyn AnyTreeModel>;

impl TreeModel for DynTreeModel {
    fn root_id(&self) -> NodeId {
        (**self).root_id()
    }
    fn contains(&self, id: NodeId) -> bool {
        (**self).contains(id)
    }
    fn label(&self, id: NodeId) -> Option<&str> {
        (**self).label(id)
    }
    fn is_branch(&self, id: NodeId) -> bool {
        (**self).is_branch(id)
    }
    fn accepts_children(&self, id: NodeId) -> bool {
        (**self).accepts_children(id)
    }
    fn semantic_key(&self, id: NodeId) -> Option<String> {
        (**self).semantic_key(id)
    }
    fn is_selectable(&self, id: NodeId) -> bool {
        (**self).is_selectable(id)
    }
    fn is_renamable(&self, id: NodeId) -> bool {
        (**self).is_renamable(id)
    }
    fn shows_row_actions(&self, id: NodeId) -> bool {
        (**self).shows_row_actions(id)
    }
    fn can_collapse(&self, id: NodeId) -> bool {
        (**self).can_collapse(id)
    }
    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        (**self).parent_id(id)
    }
    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        (**self).child_index(parent, child)
    }
    fn child_count(&self, parent: NodeId) -> usize {
        (**self).child_count(parent)
    }
    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        (**self).child_id(parent, index)
    }
    fn child_state(&self, parent: NodeId, index: usize) -> TreeChildState<'_> {
        (**self).child_state(parent, index)
    }
    fn request_load(&mut self, request: TreeLoadRequest) -> bool {
        (**self).request_load(request)
    }
    fn poll_changes(&mut self) -> Vec<TreeModelChange> {
        (**self).poll_changes()
    }
    fn needs_poll(&self) -> bool {
        (**self).needs_poll()
    }
}

/// `model` as the `M` it was boxed from.
fn typed<M: 'static>(model: &DynTreeModel) -> Option<&M> {
    let model: &dyn AnyTreeModel = &**model;
    (model as &dyn Any).downcast_ref()
}

/// `model` as the `M` it was boxed from, to change.
fn typed_mut<M: 'static>(model: &mut DynTreeModel) -> Option<&mut M> {
    let model: &mut dyn AnyTreeModel = &mut **model;
    (model as &mut dyn Any).downcast_mut()
}

/// The tree over a model behind a trait object: every [`TreeView`]'s, and
/// compiled once, in the toolkit.
#[repr(transparent)]
pub(crate) struct TreeCore(TreeImpl<(), DynTreeModel>);

impl TreeCore {
    fn new(model: DynTreeModel) -> Self {
        Self(TreeImpl::from_source(model))
    }

    fn begin_model_update(&self) -> Option<SparseAnchor<TreeRowKey>> {
        self.0.capture_view_anchor()
    }

    fn end_model_update(&mut self, anchor: Option<&SparseAnchor<TreeRowKey>>) {
        self.0.end_model_update(anchor);
    }

    fn begin_source_update(
        &mut self,
        changes: &[TreeModelChange],
    ) -> (bool, Option<SparseAnchor<TreeRowKey>>) {
        self.0.begin_source_update(changes)
    }

    fn end_source_update(
        &mut self,
        changes: &[TreeModelChange],
        staged: (bool, Option<SparseAnchor<TreeRowKey>>),
    ) {
        self.0.end_source_update(changes, staged);
    }
}

/// [`Widget`] for a tree that holds one in `$($tree)+`: every method the tree
/// implements, handed to it.
macro_rules! forward_widget {
    ($($tree:tt)+) => {
        fn set_interaction_environment(
            &mut self,
            environment: crate::interaction::InteractionEnvironment,
        ) {
            Widget::set_interaction_environment(&mut self.$($tree)+, environment);
        }

        fn sync(&mut self, state: &AppModel, path: &ModelPath) {
            Widget::sync(&mut self.$($tree)+, state, path);
        }

        fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
            Widget::update(&mut self.$($tree)+, state, path)
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let node = Widget::build_layout(&mut self.$($tree)+, taffy, theme, node_map);
            // The tree registered itself for its node; the host reaches it
            // through the widget its parent holds, this one.
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
            Widget::apply_layout(&mut self.$($tree)+, taffy, node, origin);
        }

        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            Widget::draw(&mut self.$($tree)+, scene, transform, text_renderer, theme, presence);
        }

        fn draw_overlay(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            Widget::draw_overlay(
                &mut self.$($tree)+,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            Widget::handle_event(&mut self.$($tree)+, event, ctx);
        }

        fn children(&self) -> Vec<&dyn Widget> {
            Widget::children(&self.$($tree)+)
        }

        fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
            Widget::for_each_child(&self.$($tree)+, visitor);
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            Widget::children_mut(&mut self.$($tree)+)
        }

        fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
            Widget::accessibility_child_parent(&self.$($tree)+, child_index)
        }

        fn wants_pointer_capture(&self) -> bool {
            Widget::wants_pointer_capture(&self.$($tree)+)
        }

        fn needs_redraw(&self) -> bool {
            Widget::needs_redraw(&self.$($tree)+)
        }

        fn collect_damage(&self, regions: &mut Vec<Rect>) {
            Widget::collect_damage(&self.$($tree)+, regions);
        }

        fn release_scene_cache(&mut self) {
            Widget::release_scene_cache(&mut self.$($tree)+);
        }

        fn mark_drawn(&mut self) {
            Widget::mark_drawn(&mut self.$($tree)+);
        }

        fn needs_animation_frame(&self) -> bool {
            Widget::needs_animation_frame(&self.$($tree)+)
        }

        fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
            Widget::next_wake_deadline(&self.$($tree)+)
        }

        fn take_structure_dirty(&mut self) -> bool {
            Widget::take_structure_dirty(&mut self.$($tree)+)
        }

        fn is_layout_dirty(&self) -> bool {
            Widget::is_layout_dirty(&self.$($tree)+)
        }

        fn layout_needs_measure(&self) -> bool {
            Widget::layout_needs_measure(&self.$($tree)+)
        }

        fn name(&self) -> &str {
            Widget::name(&self.$($tree)+)
        }

        fn test_id(&self) -> Option<&str> {
            Widget::test_id(&self.$($tree)+)
        }

        fn get_rect(&self) -> Rect {
            Widget::get_rect(&self.$($tree)+)
        }

        fn is_focusable(&self) -> bool {
            Widget::is_focusable(&self.$($tree)+)
        }

        fn set_focus(&mut self, focused: bool) {
            Widget::set_focus(&mut self.$($tree)+, focused);
        }

        fn is_focused(&self) -> bool {
            Widget::is_focused(&self.$($tree)+)
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Widget::accessibility(&self.$($tree)+)
        }

        fn accessibility_hidden(&self) -> bool {
            Widget::accessibility_hidden(&self.$($tree)+)
        }

        fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
            Widget::accessibility_root(&self.$($tree)+)
        }

        fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
            Widget::accessibility_shared(&self.$($tree)+)
        }

        fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
            Widget::synthetic_accessibility_rect(&self.$($tree)+, path)
        }

        fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
            Widget::focus_synthetic_accessibility(&mut self.$($tree)+, path)
        }

        fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
            Widget::tooltip_target_at(&self.$($tree)+, point)
        }

        fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
            Widget::cursor_shape(&self.$($tree)+, point)
        }

        fn perform_semantic_action(
            &mut self,
            target: &str,
            action: &crate::a11y::Action,
            state: &mut crate::widget::Edits<'_>,
            path: &ModelPath,
        ) -> bool {
            Widget::perform_semantic_action(&mut self.$($tree)+, target, action, state, path)
        }
    };
}

impl Widget for TreeCore {
    forward_widget!(0);
}

/// A tree of rows, each a node of the model `M`: disclosure, selection,
/// keyboard navigation, renaming, reordering and semantics.
///
/// Holds its model behind a trait object and hands its work to the toolkit's
/// one compiled tree; [`Self::model`] and the closures it is given see the
/// `M` it was built from. Every other method of the tree is reached through
/// it.
#[repr(transparent)]
pub struct TreeView<T, M = TreeNode<T>> {
    core: TreeCore,
    types: PhantomData<fn() -> (T, M)>,
}

impl<T, M> Deref for TreeView<T, M> {
    type Target = TreeImpl<(), DynTreeModel>;

    fn deref(&self) -> &Self::Target {
        &self.core.0
    }
}

impl<T, M> DerefMut for TreeView<T, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core.0
    }
}

impl<T: 'static> TreeView<T, TreeNode<T>> {
    pub fn new(root: TreeNode<T>) -> Self {
        Self::of(Box::new(root))
    }

    /// Batch edits to the convenient owned model and reconcile once.
    #[expect(
        clippy::expect_used,
        reason = "the view boxed its model from this `TreeNode<T>` when it was built"
    )]
    pub fn update_model<R>(&mut self, update: impl FnOnce(&mut TreeNode<T>) -> R) -> R {
        let anchor = self.core.begin_model_update();
        let model = typed_mut::<TreeNode<T>>(&mut self.core.0.model)
            .expect("a tree view's model is the one it was built from");
        let result = update(model);
        self.core.end_model_update(anchor.as_ref());
        result
    }

    /// Enable safe before/inside/after dragging for the owned [`TreeNode`]
    /// model. Subtrees can be reordered among siblings or reparented without
    /// app-authored detach/insert bookkeeping.
    #[must_use]
    pub fn reorderable(self) -> Self {
        self.with_reorder(TreeNode::can_reorder, |model, dropped| {
            model.reorder(&dropped)
        })
    }
}

impl<M: TreeModel + 'static> TreeView<(), M> {
    /// Build a view over an app-owned, database-backed, or asynchronous model.
    /// Call `notify_model_changes` after the source applies a batch.
    pub fn from_model(model: M) -> Self {
        Self::of(Box::new(model))
    }
}

/// Builders that hand their arguments to the tree's own.
macro_rules! builders {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {
        $(
            #[doc = concat!("The tree's own `", stringify!($name), "`, on this view.")]
            #[must_use]
            pub fn $name(self, $($arg: $ty),*) -> Self {
                self.map(|tree| tree.$name($($arg),*))
            }
        )*
    };
}

impl<T: 'static, M: TreeModel + 'static> TreeView<T, M> {
    fn of(model: DynTreeModel) -> Self {
        Self {
            core: TreeCore::new(model),
            types: PhantomData,
        }
    }

    fn map(
        self,
        build: impl FnOnce(TreeImpl<(), DynTreeModel>) -> TreeImpl<(), DynTreeModel>,
    ) -> Self {
        Self {
            core: TreeCore(build(self.core.0)),
            types: PhantomData,
        }
    }

    builders! {
        with_id(id: impl Into<String>);
        on_select(f: impl FnMut(NodeId) + 'static);
        on_expand(f: impl FnMut(NodeId, bool) + 'static);
        on_load_request(f: impl FnMut(TreeLoadRequest) + 'static);
        on_activate(f: impl FnMut(NodeId) + 'static);
        click_activation(activation: TreeClickActivation);
        selection_follows_focus(enabled: bool);
        with_heading(heading: impl Into<String>);
        with_ground(ground: fn(&Theme) -> Color);
        hide_root();
        fill();
        with_row_height(height_px: f32);
        with_expanded(ids: impl IntoIterator<Item = NodeId>);
        with_selected(id: Option<NodeId>);
        multi_select(enabled: bool);
        with_selection(selection: crate::selection::KeySelection<NodeId>);
        with_accessible_name(name: impl Into<String>);
        with_semantic_key(key: impl Into<String>);
        with_viewport(height_px: f32, estimated_row_height_px: f32);
        on_context_menu_item(f: impl FnMut(Option<NodeId>, usize) + 'static);
        with_row_actions(actions: Vec<TreeRowAction>);
        on_row_action(f: impl FnMut(NodeId, usize) + 'static);
        with_heading_action(action: TreeHeadingAction);
        on_heading_action(f: impl FnMut() + 'static);
        with_row_hint(hint: impl Into<String>);
        with_indent_guides();
        on_rename(f: impl FnMut(NodeId, String) + 'static);
        rename_on_double_click();
        tab_indents(enabled: bool);
        on_delete(f: impl FnMut(NodeId) + 'static);
    }

    /// The model the view was built from.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the view boxed its model from this `M` when it was built"
    )]
    pub fn model(&self) -> &M {
        typed::<M>(&self.core.0.model).expect("a tree view's model is the one it was built from")
    }

    /// Mutate an app-owned model stored directly in the view and apply its
    /// notification batch afterward. Models backed by shared interior state
    /// can mutate externally and call `notify_model_changes` instead.
    #[expect(
        clippy::expect_used,
        reason = "the view boxed its model from this `M` when it was built"
    )]
    pub fn update_source<R>(
        &mut self,
        changes: &[TreeModelChange],
        update: impl FnOnce(&mut M) -> R,
    ) -> R {
        let staged = self.core.begin_source_update(changes);
        let model = typed_mut::<M>(&mut self.core.0.model)
            .expect("a tree view's model is the one it was built from");
        let result = update(model);
        self.core.end_source_update(changes, staged);
        result
    }

    /// Decorate each built-in row: an icon, a tone, trailing text and an
    /// accessible description, recomputed each frame from the view's `M` and
    /// the active theme.
    #[must_use]
    pub fn with_row_decoration(
        self,
        mut decorate: impl FnMut(&M, TreeRowInfo, &Theme) -> TreeRowDecoration + 'static,
    ) -> Self {
        self.map(|tree| {
            tree.with_row_decoration(move |model, info, theme| {
                typed::<M>(model).map_or_else(TreeRowDecoration::default, |model| {
                    decorate(model, info, theme)
                })
            })
        })
    }

    /// Let rows be dragged: `validate` says whether a drop may land, `commit`
    /// changes the view's `M` and returns `true`; the tree then reconciles
    /// once.
    #[must_use]
    pub fn with_reorder(
        self,
        mut validate: impl FnMut(&M, NodeId, &DropTarget<NodeId>) -> bool + 'static,
        mut commit: impl FnMut(&mut M, ReorderDrop<NodeId>) -> bool + 'static,
    ) -> Self {
        self.map(|tree| {
            tree.with_reorder(
                move |model, source, target| {
                    typed::<M>(model).is_some_and(|model| validate(model, source, target))
                },
                move |model, dropped| {
                    typed_mut::<M>(model).is_some_and(|model| commit(model, dropped))
                },
            )
        })
    }

    /// Replace built-in text rows with widgets built from the view's `M`.
    #[must_use]
    pub fn with_row_delegate(
        self,
        mut delegate: impl FnMut(&M, NodeId, usize) -> Element + 'static,
    ) -> Self {
        self.map(|tree| {
            tree.with_row_delegate(move |model, id, depth| match typed::<M>(model) {
                Some(model) => delegate(model, id, depth),
                None => Box::new(crate::widget::Spacer::new()),
            })
        })
    }

    /// The context menu's items for a row (or the tree's background), read
    /// from the view's `M`.
    #[must_use]
    pub fn with_context_menu(
        self,
        mut items: impl FnMut(&M, Option<NodeId>) -> Vec<MenuItem> + 'static,
    ) -> Self {
        self.map(|tree| {
            tree.with_context_menu(move |model, target| {
                typed::<M>(model).map_or_else(Vec::new, |model| items(model, target))
            })
        })
    }
}

/// What a view does to its tree in place: each a method of [`TreeCore`],
/// compiled once, and the view's own, which only hands it over.
macro_rules! compiled_once {
    ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        impl TreeCore {
            $(
                fn $name(&mut self, $($arg: $ty),*) $(-> $ret)? {
                    self.0.$name($($arg),*)
                }
            )*
        }

        impl<T, M> TreeView<T, M> {
            $(
                #[doc = concat!("The tree's own `", stringify!($name), "`, compiled once.")]
                pub fn $name(&mut self, $($arg: $ty),*) $(-> $ret)? {
                    self.core.$name($($arg),*)
                }
            )*
        }
    };
}

compiled_once! {
    set_heading(heading: Option<String>);
    set_current(id: Option<NodeId>);
    place_at(rect: Rect);
    reveal(id: NodeId);
    toggle_expand(id: NodeId);
    set_expanded(id: NodeId, expanded: bool) -> bool;
    select(id: NodeId);
    select_all();
    set_focused(id: Option<NodeId>);
    notify_model_changes(changes: &[TreeModelChange]);
    set_viewport(height_px: f32, estimated_row_height_px: f32);
    set_scroll_y(offset_px: f32);
    set_scroll_offset(offset_px: f64);
    set_row_height(visible_index: usize, height_px: f32) -> bool;
    request_visible_rows() -> usize;
    stage_reorder(source: NodeId, before: NodeId);
    suppress_tooltips(suppressed: bool);
    close_context_menu();
    open_context_menu(target: Option<NodeId>, at: Point) -> bool;
    begin_rename(id: NodeId) -> bool;
    commit_rename();
    cancel_rename();
    move_node(id: NodeId, movement: TreeMove) -> bool;
}

impl TreeCore {
    fn restore_expanded_of(&mut self, ids: Vec<NodeId>) {
        self.0.restore_expanded(ids);
    }

    fn invalidate_row_delegates_of(&mut self, ids: Vec<NodeId>) {
        self.0.invalidate_row_delegates(ids);
    }
}

impl<T, M> TreeView<T, M> {
    /// Replace the expanded set without firing `on_expand`, for a host
    /// restoring expansion it persists itself. Unknown ids, and leaves that
    /// cannot hold children, are ignored.
    pub fn restore_expanded(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.core.restore_expanded_of(ids.into_iter().collect());
    }

    /// Rebuild delegated widgets for model nodes whose displayed data changed.
    pub fn invalidate_row_delegates(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.core
            .invalidate_row_delegates_of(ids.into_iter().collect());
    }
}

impl<T: 'static, M: TreeModel + 'static> Widget for TreeView<T, M> {
    forward_widget!(core);
}

#[cfg(test)]
mod tests {
    use super::super::{TreeNode, TreeView};

    /// The view gives back the model it was built from, and edits through
    /// it reach the rows it shows.
    #[test]
    fn a_view_reads_and_edits_the_model_it_was_built_from() {
        let mut view = TreeView::new(TreeNode::branch(
            1,
            "root",
            (),
            vec![TreeNode::leaf(2, "a", ()), TreeNode::leaf(3, "b", ())],
        ))
        .with_expanded([1]);
        assert_eq!(view.model().children.len(), 2);
        assert_eq!(view.visible_len(), 3);
        view.update_model(|root| root.children.push(TreeNode::leaf(4, "c", ())));
        assert_eq!(view.model().children.len(), 3);
        assert_eq!(view.visible_len(), 4);
    }
}
