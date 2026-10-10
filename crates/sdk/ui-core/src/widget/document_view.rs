//! Keyed, model-derived retained subtrees.
//!
//! [`ModelView`] is the structural counterpart to
//! [`Text::from_document`](super::Text::from_document): a projection reads the
//! live Automerge document or typed [`LocalState`](crate::LocalState), and a
//! builder creates a subtree only when that projected value changes. Unrelated
//! updates therefore keep widget identity, focus, scroll positions, and other
//! retained presentation state.

use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use super::{Element, FullscreenProvider, FullscreenWidgetExt as _, TaffyTreeExt, Text, Widget};

/// The style of the boundary node a [`ModelView`] puts around its child.
///
/// A dynamic page is a transparent layout boundary, not an intrinsic
/// desktop-width island: it fills its parent's width so percent-width cards
/// and wrapping rows reflow on narrow windows. It hands its child's own
/// claim on the parent through: a percent height (a pane that fills its
/// column), flex growth (a `Scrollable` under a fixed bar) and its basis,
/// so the child sizes exactly as if the boundary were not there. A child
/// out of the flow takes the boundary out too.
fn boundary_style(child: Option<&Style>) -> Style {
    let mut style = Style {
        display: taffy::Display::Flex,
        flex_direction: taffy::FlexDirection::Column,
        size: taffy::Size {
            width: taffy::Dimension::percent(1.0),
            height: taffy::Dimension::auto(),
        },
        min_size: taffy::Size {
            width: taffy::Dimension::length(0.0),
            height: taffy::Dimension::auto(),
        },
        flex_shrink: 1.0,
        ..Style::default()
    };
    let Some(child) = child else {
        return style;
    };
    if child.display == taffy::Display::None {
        style.display = taffy::Display::None;
        return style;
    }
    if child.size.height.tag() == taffy::CompactLength::PERCENT_TAG {
        style.size.height = child.size.height;
    }
    if child.min_size.height.tag() == taffy::CompactLength::PERCENT_TAG {
        style.min_size.height = child.min_size.height;
    }
    style.flex_grow = child.flex_grow;
    if child.flex_grow > 0.0 {
        style.flex_basis = child.flex_basis;
        // The child grows into whatever the boundary is given.
        style.min_size.height = taffy::Dimension::length(0.0);
    }
    style
}

type Projection<K> = Box<dyn FnMut(&AutoCommit, &ObjId) -> K>;
type Builder<K> = Box<dyn FnMut(&K, Option<&crate::ControlDomain>) -> Element>;

/// A retained subtree selected by a value projected from application state.
///
/// `K` is both the value passed to `build` and the reconciliation key. Keep it
/// small and make its equality match structural meaning: if two values compare
/// equal, the existing subtree is retained and synchronized normally.
pub struct ModelView<K> {
    project: Projection<K>,
    build: Builder<K>,
    control_domain: Option<crate::ControlDomain>,
    child_domain: Option<crate::ControlDomain>,
    scope_domain: Option<crate::ControlDomain>,
    current: Option<K>,
    child: Element,
    rect: Rect,
    structure_dirty: bool,
    focus_reset: bool,
    autofocus: bool,
    wake_deadline: Option<Box<dyn Fn() -> Option<crate::time::Instant>>>,
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
        mut build: impl FnMut(&K) -> Element + 'static,
    ) -> Self {
        Self {
            project: Box::new(project),
            build: Box::new(move |key, _| build(key)),
            control_domain: None,
            child_domain: None,
            scope_domain: None,
            current: None,
            // The first sync happens before layout. This inert child merely
            // keeps the wrapper valid for headless/manual users that inspect it
            // before synchronizing.
            child: Box::new(Text::new("")),
            rect: Rect::ZERO,
            structure_dirty: false,
            focus_reset: false,
            autofocus: false,
            wake_deadline: None,
        }
    }

    /// Build with the exact installed domain before any constructor emission.
    /// The supplied domain must be the owner obtained from HostContext/runner.
    pub fn from_document_in(
        domain: &crate::ControlDomain,
        project: impl FnMut(&AutoCommit, &ObjId) -> K + 'static,
        mut build: impl FnMut(&K, &crate::ControlDomain) -> Element + 'static,
    ) -> Self {
        let mut view = Self::from_document(project, |_| Box::new(Text::new("")));
        view.control_domain = Some(domain.clone());
        // A view with an installed domain always builds under its child domain;
        // the installed owner is only the fallback for that invariant.
        let installed = domain.clone();
        view.build = Box::new(move |key, domain| build(key, domain.unwrap_or(&installed)));
        view
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

    /// Reconcile a time-sensitive projection through the host's existing wake
    /// scheduler, including while the window is idle. Return None once the
    /// projection no longer needs a deadline; this never starts a polling loop.
    #[must_use]
    pub fn with_wake_deadline(
        mut self,
        deadline: impl Fn() -> Option<crate::time::Instant> + 'static,
    ) -> Self {
        self.wake_deadline = Some(Box::new(deadline));
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
        if let Some(old) = self.child_domain.take() {
            old.close();
        }
        if self.scope_domain.is_none() {
            self.scope_domain = self
                .control_domain
                .as_ref()
                .map(crate::ControlDomain::child);
        }
        self.child_domain = self.scope_domain.as_ref().map(crate::ControlDomain::child);
        self.child = (self.build)(&next, self.child_domain.as_ref());
        if let Some(domain) = &self.child_domain {
            crate::widget::controls::install_control_custody(
                self.child.as_mut(),
                domain,
                crate::control_custody::CustodyAttachment::Dynamic,
            );
        }
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

impl<K> FullscreenProvider for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn fullscreen_intent(&self) -> Option<bool> {
        self.child.fullscreen_intent()
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        self.child.fullscreen_params()
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        self.child.on_fullscreen_changed(active);
    }
}

impl<K> Widget for ModelView<K>
where
    K: PartialEq + 'static,
{
    crate::provides!(
        dyn crate::widget::FullscreenProvider,
        dyn crate::undo::CommittedActions,
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::search::capability::SearchSource,
        dyn crate::record::capability::RecordScoped,
        dyn crate::view::capability::ViewState
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = crate::doc::document_ref(state, path);
        self.reconcile(state, path);
        self.child.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let (state, path) = crate::doc::document_ref(state, path);
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
        taffy.widget_node(
            boundary_style(taffy.style(child).ok()),
            &[child],
            node_map,
            self,
        )
    }

    /// A child that left the flow (an absent element, bound text with
    /// nothing to say) takes the boundary out with it.
    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        let child = taffy
            .children(node)
            .ok()
            .and_then(|children| children.first().copied());
        let Some(child) = child else {
            return false;
        };
        let wanted = boundary_style(taffy.style(child).ok());
        match taffy.style(node) {
            Ok(style) if style.display != wanted.display => {
                let mut style = style.clone();
                style.display = wanted.display;
                taffy.set_style(node, style).is_ok()
            }
            _ => false,
        }
    }

    crate::forward_to_child!(child, rect);

    fn name(&self) -> &'static str {
        "ModelView"
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

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.wake_deadline
            .as_ref()
            .and_then(|deadline| deadline())
            .into_iter()
            .chain(self.child.next_wake_deadline())
            .min()
    }

    fn on_external_event(
        &mut self,
        payload: &str,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) {
        self.child.on_external_event(payload, state, root);
    }

    fn apply_room_action(
        &mut self,
        doc: &mut crate::widget::Edits<'_>,
        type_id: &str,
        payload: &[u8],
    ) -> bool {
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

impl<K> crate::view::capability::ViewState for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        crate::view::capability::view_state(self.child.as_ref())
    }
    fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
        crate::view::capability::restore_view_state(self.child.as_mut(), view);
    }
}

impl<K> crate::record::capability::RecordScoped for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        crate::record::capability::record_intent(self.child.as_ref())
    }
    fn set_record_scope(&mut self, record: &crate::record::RecordId) {
        crate::record::capability::set_record_scope(self.child.as_mut(), record);
    }
    fn release_record_scope(&mut self) -> Result<(), String> {
        crate::record::capability::release_record_scope(self.child.as_mut())
    }
}

impl<K> crate::search::capability::SearchSource for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        crate::search::capability::search_providers(self.child.as_ref())
    }
    fn activate_search_result(
        &mut self,
        action_id: &str,
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        crate::search::capability::activate_search_result(
            self.child.as_mut(),
            action_id,
            doc,
            path,
        );
    }
}

impl<K> crate::resources::PublishesResources for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        // The host visits the admitted child after this boundary. A view
        // owns no declarations of its own and must not publish them twice.
        Vec::new()
    }
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        crate::resources::document_action_result(self.child.as_ref(), doc, address, changes)
    }
}

impl<K> crate::widget::controls::ActionControls for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(old) = &self.control_domain {
            if !old.same(domain) {
                if !domain.permits_renewal(old, attachment) {
                    domain.unsafe_attachment();
                    return;
                }
                if let Some(scope) = self.scope_domain.take() {
                    scope.close();
                }
                self.child_domain = None;
            }
        }
        self.control_domain = Some(domain.clone());
        let scope = self.scope_domain.get_or_insert_with(|| domain.child());
        let child_domain = self.child_domain.get_or_insert_with(|| scope.child());
        crate::widget::controls::install_control_custody(
            self.child.as_mut(),
            child_domain,
            attachment,
        );
    }
}

impl<K> crate::undo::CommittedActions for ModelView<K>
where
    K: PartialEq + 'static,
{
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        crate::undo::take_committed_action(self.child.as_mut())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use automerge::{ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    use super::*;
    use vello::kurbo::Point;

    #[test]
    fn deadline_reconciles_an_idle_projection_through_the_real_frame_host() {
        let deadline = Rc::new(Cell::new(
            crate::time::now() + crate::time::Duration::from_secs(60),
        ));
        let expired = Rc::new(Cell::new(false));
        let projected_deadline = deadline.clone();
        let projected_expired = expired.clone();
        let built_expired = expired.clone();
        let scheduled_deadline = deadline.clone();
        let scheduled_expired = expired.clone();
        let view = ModelView::from_document(
            move |_, _| projected_expired.get() || crate::time::now() >= projected_deadline.get(),
            move |expired| {
                built_expired.set(*expired);
                Box::new(Text::new(if *expired {
                    "Pairing expired"
                } else {
                    "Pairing active"
                }))
            },
        )
        .with_wake_deadline(move || (!scheduled_expired.get()).then(|| scheduled_deadline.get()));
        let mut host =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(view), (600, 400));
        assert!(!expired.get());
        assert_eq!(
            host.frame_schedule().next_wake_deadline,
            Some(deadline.get())
        );
        assert!(!host.frame_schedule().needs_animation_frame);
        // Move the actual projection's deadline to now, without delivering an
        // input or editing its model. Its existing host wake must remain due.
        deadline.set(crate::time::now());
        assert!(host.frame_schedule().next_wake_deadline.unwrap() <= crate::time::now());
        assert!(host.wait_until(crate::time::Duration::ZERO, |_| expired.get()));
        assert_eq!(host.frame_schedule().next_wake_deadline, None);
        assert!(host.a11y_nodes().iter().any(|(_, node)| {
            node.value() == Some("Pairing expired") || node.label() == Some("Pairing expired")
        }));
    }

    #[test]
    fn model_view_publishes_each_admitted_child_resource_once() {
        use crate::widget::ToggleGroup;
        use crate::widget::toggle_group::Segment;
        let state = crate::LocalState::new(0_u8);
        let mut view = ModelView::from_state(&state, |_| {
            Box::new(
                ToggleGroup::new(vec![Segment::new("One"), Segment::new("Two")], 0)
                    .on_view("model-view-mode", |_| {}),
            )
        });
        let doc = AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        let resources = crate::widget::published_resources(&view, &doc);
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].id.as_str(), "tilo.control:model-view-mode");
        assert_eq!(resources[0].actions.len(), 1);
        assert_eq!(resources[0].actions[0].id, "select");
    }

    fn string(doc: &AutoCommit, key: &str) -> String {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(value) => value.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    fn solve(root: &mut dyn Widget, width: f32, height: f32) {
        let theme = Theme::dark();
        let mut tree = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let node = root.build_layout(&mut tree, &theme, &mut map);
        crate::text_render::with_layout_renderer(1.0, |text| {
            tree.compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(height),
                },
                |known, available, node, _, _| {
                    map.get(&node).map_or(taffy::Size::ZERO, |&widget| {
                        // SAFETY: the map points into `root`, alive and not
                        // otherwise borrowed for the duration of this solve.
                        unsafe { (*widget).measure(known, available, text) }
                    })
                },
            )
            .expect("layout solves");
        });
        root.apply_layout(&tree, node, Point::ZERO);
    }

    #[test]
    fn the_boundary_passes_percent_height_and_growth_through() {
        use crate::widget::{Column, Scrollable, Spacer};
        let page = || -> Element {
            Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(Box::new(
                        Column::new()
                            .with_intrinsic_height()
                            .push(Box::new(Spacer::fixed(2000.0))),
                    )),
            )
        };
        let state = crate::LocalState::new(0_u8);
        let mut root = Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(Spacer::fixed(40.0)))
            .push(Box::new(ModelView::from_state(&state, move |_| page())));
        let doc = AutoCommit::new();
        root.sync(&doc, &ROOT);
        solve(&mut root, 400.0, 300.0);
        let view = root.children()[1].get_rect();
        assert!(
            view.height() > 200.0 && view.y1 <= 300.5,
            "the scroller fills the column below the bar: {view:?}"
        );

        let absent = crate::LocalState::new(0_u8);
        let mut root = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0)
            .push(Box::new(Spacer::fixed(10.0)))
            .push(Box::new(ModelView::from_state(&absent, |_| {
                crate::widget::optional(None)
            })))
            .push(Box::new(Spacer::fixed(10.0)));
        root.sync(&doc, &ROOT);
        solve(&mut root, 400.0, 300.0);
        let children = root.children();
        let gap = children[2].get_rect().y0 - children[0].get_rect().y1;
        assert!(
            (gap - 10.0).abs() < 0.01,
            "an absent view takes no gap: {gap}"
        );
    }

    #[test]
    fn document_view_projects_keyed_subtrees_and_tracks_undo() {
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

            view.sync(&doc, &ROOT);
            assert_eq!(builds.get(), 1);
            assert!(view.take_structure_dirty());
            assert!(!view.take_structure_dirty(), "the signal is edge-triggered");

            doc.put(&ROOT, "unrelated", 42).unwrap();
            view.sync(&doc, &ROOT);
            assert_eq!(builds.get(), 1, "equal keys retain the existing subtree");
            assert!(!view.take_structure_dirty());

            doc.put(&ROOT, "selection", "beta").unwrap();
            view.sync(&doc, &ROOT);
            assert_eq!(builds.get(), 2);
            assert!(view.take_structure_dirty());
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("beta")
            );
        }

        fn document_view_tracks_undo_without_app_owned_notifications() {
            let mut state = crate::State::new();
            state.doc.put(&ROOT, "mode", "list").unwrap();
            state.commit_action(None, None);
            let mut view = ModelView::from_document(
                |doc, _| string(doc, "mode"),
                |mode| Box::new(Text::new(mode)),
            )
            .autofocus_on_change(true);

            view.sync(&state.doc, &ROOT);
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("list")
            );
            assert!(view.take_focus_reset());

            state.doc.put(&ROOT, "mode", "detail").unwrap();
            state.commit_action(None, None);
            view.sync(&state.doc, &ROOT);
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("detail")
            );

            state.undo(None, None).unwrap();
            view.sync(&state.doc, &ROOT);
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("list")
            );
        }

        fn typed_state_selects_arbitrary_subtrees() {
            let state = crate::LocalState::new(String::from("loading"));
            let mut view = ModelView::from_state(&state, |value| Box::new(Text::new(value)));
            let doc = AutoCommit::new();

            view.sync(&doc, &ROOT);
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("loading")
            );
            state.set(String::from("ready"));
            view.sync(&doc, &ROOT);
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("ready")
            );
        }

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
            let doc = AutoCommit::new();

            view.sync(&doc, &ROOT);
            state.set(LoadState::Ready(Project {
                title: String::from("First"),
            }));
            view.sync(&doc, &ROOT);
            assert_eq!(builds.get(), 2);
            assert_eq!(
                view.children()[0].accessibility().unwrap().name.as_deref(),
                Some("First")
            );

            state.set(LoadState::Ready(Project {
                title: String::from("Updated without structural replacement"),
            }));
            view.sync(&doc, &ROOT);
            assert_eq!(builds.get(), 2, "the Ready subtree keeps its identity");
        }

        keyed_projection_retains_subtree_across_unrelated_document_edits();
        document_view_tracks_undo_without_app_owned_notifications();
        typed_state_selects_arbitrary_subtrees();
        separately_keyed_state_does_not_clone_or_compare_the_payload();
    }
}

impl<K> Drop for ModelView<K> {
    fn drop(&mut self) {
        if let Some(domain) = &self.scope_domain {
            domain.close();
        }
    }
}
