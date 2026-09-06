//! Keyed, model-derived retained subtrees.
//!
//! [`ModelView`] is the structural counterpart to
//! [`Text::from_document`](super::Text::from_document): a projection reads the
//! live Automerge document or typed [`LocalState`](crate::LocalState), and a
//! builder creates a subtree only when that projected value changes. Unrelated
//! updates therefore keep widget identity, focus, scroll positions, and other
//! retained presentation state.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use super::{Element, TaffyTreeExt, Text, Widget};

type Projection<K> = Box<dyn FnMut(&AutoCommit, &ObjId) -> K>;
type Builder<K> = Box<dyn FnMut(&K) -> Element>;

/// A retained subtree selected by a value projected from application state.
///
/// `K` is both the value passed to `build` and the reconciliation key. Keep it
/// small and make its equality match structural meaning: if two values compare
/// equal, the existing subtree is retained and synchronized normally.
pub struct ModelView<K> {
    project: Projection<K>,
    build: Builder<K>,
    current: Option<K>,
    child: Element,
    rect: Rect,
    structure_dirty: bool,
    focus_reset: bool,
    autofocus: bool,
}

impl<K> ModelView<K>
where
    K: PartialEq + 'static,
{
    /// Create a document-derived subtree.
    ///
    /// The projection first runs during the normal pre-layout synchronization
    /// pass. `build` then runs once, and again only when the projected value is
    /// unequal to the previous value.
    pub fn from_document(
        project: impl FnMut(&AutoCommit, &ObjId) -> K + 'static,
        build: impl FnMut(&K) -> Element + 'static,
    ) -> Self {
        Self {
            project: Box::new(project),
            build: Box::new(build),
            current: None,
            // The first sync happens before layout. This inert child merely
            // keeps the wrapper valid for headless/manual users that inspect it
            // before synchronizing.
            child: Box::new(Text::new("")),
            rect: Rect::ZERO,
            structure_dirty: false,
            focus_reset: false,
            autofocus: false,
        }
    }

    /// Focus the first focusable descendant after a projection changes.
    ///
    /// This is opt-in because replacing a passive detail pane should not steal
    /// focus from the master control that caused the change.
    #[must_use]
    pub fn autofocus_on_change(mut self, autofocus: bool) -> Self {
        self.autofocus = autofocus;
        self
    }

    fn reconcile(&mut self, state: &AutoCommit, path: &ObjId) {
        let next = (self.project)(state, path);
        if self
            .current
            .as_ref()
            .is_some_and(|current| *current == next)
        {
            return;
        }
        self.child = (self.build)(&next);
        self.current = Some(next);
        self.structure_dirty = true;
        self.focus_reset |= self.autofocus;
    }
}

impl<K> ModelView<K>
where
    K: Clone + PartialEq + 'static,
{
    /// Create a subtree keyed directly by typed process-local state.
    ///
    /// A [`LocalStateWriter`](crate::LocalStateWriter) wakes an idle app after
    /// worker-thread updates, so this follows the same reconciliation path for
    /// UI callbacks and background results.
    pub fn from_state(
        state: &crate::LocalState<K>,
        build: impl FnMut(&K) -> Element + 'static,
    ) -> Self {
        let state = state.clone();
        Self::from_document(move |_, _| state.get(), build)
    }
}

impl<K> ModelView<K>
where
    K: PartialEq + 'static,
{
    /// Create a typed-state subtree with a small, separately derived key.
    ///
    /// This is the preferred form for large or non-cloneable state. `key`
    /// decides whether structure changed; `build` reads the current value only
    /// when it did. Equal keys retain the existing subtree across unrelated
    /// payload mutations.
    pub fn from_state_by<T: 'static>(
        state: &crate::LocalState<T>,
        mut key: impl FnMut(&T) -> K + 'static,
        mut build: impl FnMut(&T) -> Element + 'static,
    ) -> Self {
        let project_state = state.clone();
        let build_state = state.clone();
        Self::from_document(
            move |_, _| project_state.with(&mut key),
            move |_| build_state.with(&mut build),
        )
    }
}

impl<K> Widget for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.reconcile(state, path);
        self.child.sync(state, path);
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        self.reconcile(state, path);
        self.child.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(Style::default(), &[child]);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.child
                .apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.child
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Ok(children) = taffy.children(node_id)
            && let Some(child) = children.first()
        {
            self.child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                *child,
                taffy,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.child.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn name(&self) -> &'static str {
        "ModelView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_commit(&self) -> bool {
        self.child.needs_commit()
    }

    fn on_commit(&mut self) {
        self.child.on_commit();
    }

    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.focus_reset) || self.child.take_focus_reset()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) | self.child.take_structure_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        self.child.needs_idle_sync()
    }

    fn on_external_event(&mut self, payload: &str, state: &mut AutoCommit, root: &ObjId) {
        self.child.on_external_event(payload, state, root);
    }

    fn fullscreen_intent(&self) -> Option<bool> {
        self.child.fullscreen_intent()
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        self.child.fullscreen_params()
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        self.child.on_fullscreen_changed(active);
    }

    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.child.record_intent()
    }

    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        self.child.view_state()
    }

    fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
        self.child.restore_view_state(view);
    }

    fn set_record_scope(&mut self, record: &crate::record::RecordId) {
        self.child.set_record_scope(record);
    }

    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        self.child.resources(doc)
    }

    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        self.child.search_providers()
    }

    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, path: &ObjId) {
        self.child.activate_search_result(action_id, doc, path);
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.child.take_committed_action()
    }

    fn apply_room_action(&mut self, doc: &mut AutoCommit, type_id: &str, payload: &[u8]) -> bool {
        self.child.apply_room_action(doc, type_id, payload)
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.child.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use automerge::{ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    use super::*;

    fn string(doc: &AutoCommit, key: &str) -> String {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(value) => value.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    #[test]
    fn keyed_projection_retains_subtree_across_unrelated_document_edits() {
        let builds = Rc::new(Cell::new(0));
        let observed_builds = Rc::clone(&builds);
        let mut view = ModelView::from_document(
            |doc, _| string(doc, "selection"),
            move |selection| {
                observed_builds.set(observed_builds.get() + 1);
                Box::new(Text::new(selection))
            },
        );
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "selection", "alpha").unwrap();

        view.sync(&mut doc, &ROOT);
        assert_eq!(builds.get(), 1);
        assert!(view.take_structure_dirty());
        assert!(!view.take_structure_dirty(), "the signal is edge-triggered");

        doc.put(&ROOT, "unrelated", 42).unwrap();
        view.sync(&mut doc, &ROOT);
        assert_eq!(builds.get(), 1, "equal keys retain the existing subtree");
        assert!(!view.take_structure_dirty());

        doc.put(&ROOT, "selection", "beta").unwrap();
        view.sync(&mut doc, &ROOT);
        assert_eq!(builds.get(), 2);
        assert!(view.take_structure_dirty());
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("beta")
        );
    }

    #[test]
    fn document_view_tracks_undo_without_app_owned_notifications() {
        let mut state = crate::State::new();
        state.doc.put(&ROOT, "mode", "list").unwrap();
        state.commit_action(None, None);
        let mut view = ModelView::from_document(
            |doc, _| string(doc, "mode"),
            |mode| Box::new(Text::new(mode)),
        )
        .autofocus_on_change(true);

        view.sync(&mut state.doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("list")
        );
        assert!(view.take_focus_reset());

        state.doc.put(&ROOT, "mode", "detail").unwrap();
        state.commit_action(None, None);
        view.sync(&mut state.doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("detail")
        );

        state.undo(None, None).unwrap();
        view.sync(&mut state.doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("list")
        );
    }

    #[test]
    fn typed_state_selects_arbitrary_subtrees() {
        let state = crate::LocalState::new(String::from("loading"));
        let mut view = ModelView::from_state(&state, |value| Box::new(Text::new(value)));
        let mut doc = AutoCommit::new();

        view.sync(&mut doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("loading")
        );
        state.set(String::from("ready"));
        view.sync(&mut doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("ready")
        );
    }

    #[test]
    fn separately_keyed_state_does_not_clone_or_compare_the_payload() {
        struct Project {
            title: String,
        }
        enum LoadState {
            Loading,
            Ready(Project),
        }
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Screen {
            Loading,
            Ready,
        }

        let state = crate::LocalState::new(LoadState::Loading);
        let builds = Rc::new(Cell::new(0));
        let observed_builds = Rc::clone(&builds);
        let mut view = ModelView::from_state_by(
            &state,
            |state| match state {
                LoadState::Loading => Screen::Loading,
                LoadState::Ready(_) => Screen::Ready,
            },
            move |state| {
                observed_builds.set(observed_builds.get() + 1);
                let label = match state {
                    LoadState::Loading => "loading",
                    LoadState::Ready(project) => &project.title,
                };
                Box::new(Text::new(label))
            },
        );
        let mut doc = AutoCommit::new();

        view.sync(&mut doc, &ROOT);
        state.set(LoadState::Ready(Project {
            title: String::from("First"),
        }));
        view.sync(&mut doc, &ROOT);
        assert_eq!(builds.get(), 2);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("First")
        );

        state.set(LoadState::Ready(Project {
            title: String::from("Updated without structural replacement"),
        }));
        view.sync(&mut doc, &ROOT);
        assert_eq!(builds.get(), 2, "the Ready subtree keeps its identity");
    }
}
