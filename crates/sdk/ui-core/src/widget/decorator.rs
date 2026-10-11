//! Extending a widget without re-implementing it: the [`Decorator`] trait.
//!
//! A widget that adds one behaviour to another — a diagnostic probe, an
//! app root that indexes its content for search, a surface host that
//! refreshes what it presents — should cost the hook it adds, not a hundred
//! lines of forwarding that silently go stale whenever [`Widget`] grows.
//!
//! Implement [`Decorator`] instead of [`Widget`]: name the wrapped widget,
//! give the two accessors, and override only what changes. Every [`Widget`]
//! method has a default here that forwards to the inner widget, and a
//! blanket impl makes every decorator a [`Widget`], so it can be boxed as an
//! [`Element`](super::Element), take part in any traversal, and implement
//! further traits that require [`Widget`] (such as
//! [`AppWidget`](super::AppWidget)).
//!
//! ```ignore
//! struct Probe { content: Column }
//!
//! impl ui::widget::Decorator for Probe {
//!     type Inner = Column;
//!     fn inner(&self) -> &Column { &self.content }
//!     fn inner_mut(&mut self) -> &mut Column { &mut self.content }
//!
//!     fn native_input_snapshot(&self, doc: &AutoCommit, path: &ObjId) -> Option<serde_json::Value> {
//!         Some(serde_json::json!({ "rect": format!("{:?}", self.content.get_rect()) }))
//!     }
//! }
//! ```
//!
//! # A decorator is its inner widget plus overrides
//!
//! Nothing is forwarded "partly": [`children`](Widget::children) forwards
//! too, so a host traversal meets the decorator in the inner widget's place
//! and visits every node exactly once. The only methods that describe the
//! decorator rather than the inner widget are the identity ones:
//! `concrete_type_name` names the decorator's type and
//! [`as_any`](Widget::as_any) and [`as_any_mut`](Widget::as_any_mut) are
//! `None` unless overridden.
//!
//! A boxed [`Element`](super::Element) is wrapped with `type Inner = dyn
//! Widget;` and accessors returning `&(dyn Widget + 'static)`.
//!
//! Four consequences are worth knowing:
//!
//! * **An override replaces the inner widget's answer.** Because the inner
//!   widget is not visited separately, a decorator that adds to a
//!   collecting hook (`resources`, `search_providers`,
//!   `take_control_invocation`) includes the inner widget's answer:
//!   `let mut all = mine; all.extend(self.inner().resources(doc)); all`.
//! * **Override the host's entry point.** Some [`Widget`] defaults are
//!   derived from another method (`perform_action_at` calls
//!   `perform_action`, `tooltip_target_at` calls `tooltip`,
//!   `collect_damage` calls `needs_redraw`). Here each forwards to the inner
//!   widget's own method, so an override of the derived-from method alone is
//!   not what the host calls: override the entry point the host uses.
//!   Painting and input each have a single entry point,
//!   [`draw`](Widget::draw) and [`handle_event`](Widget::handle_event).
//! * **The decorator stands in for its inner widget's layout node.** The
//!   inner widget registers itself in the node map during `build_layout`;
//!   the decorator then takes that entry, so a host that reaches a widget
//!   through the map (an accessibility action, focused input, a cursor
//!   shape, a tooltip, undo status) meets the decorator in the inner
//!   widget's place, and everything it does not override still reaches the
//!   inner widget. A node the inner widget handed to a widget of its own (a
//!   field it lays out as itself) stays that widget's. A decorator that
//!   builds a node of its own around the inner one registers itself for it
//!   with [`register_node`].
//! * **Method calls.** A decorator is both a [`Decorator`] and, through the
//!   blanket impl, a [`Widget`]. With both traits imported, `self.get_rect()`
//!   is ambiguous, so implement the trait by path (`impl
//!   ui::widget::Decorator for ..`) rather than importing it; the prelude
//!   deliberately does not export it.

use std::collections::{HashMap, HashSet};

use super::{AppModel, ModelPath, capability};
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use super::{
    EventCtx, LayoutInfo, ManualPolicy, PrimaryScrollAction, PrimaryScrollRegion,
    PrimaryScrollResult, ResourceOperationStart, Widget,
};
use crate::dnd::DragPayload;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;

/// Point `node` at `widget` in the host's node map, replacing the inner
/// widget's own registration. For a [`Decorator`] that overrides a hook the
/// host reaches through the node map (a cursor shape, a tooltip, undo
/// status); call it from `build_layout` after the inner widget built its
/// node.
pub fn register_node<W: Widget + 'static, S: std::hash::BuildHasher>(
    node_map: &mut HashMap<NodeId, *mut dyn Widget, S>,
    node: NodeId,
    widget: &mut W,
) {
    node_map.insert(node, std::ptr::from_mut::<dyn Widget>(widget));
}

/// Point `node` at `outer` when the inner widget whose address and type are
/// `inner` registered itself for it: the decorator stands in for its inner
/// widget.
fn stand_in(
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    node: NodeId,
    inner: (*const (), &'static str),
    outer: &mut (dyn Widget + 'static),
) {
    let Some(&registered) = node_map.get(&node) else {
        return;
    };
    // SAFETY: `registered` was put in the map by the build_layout that just
    // returned, for a widget inside `outer` that lives as long as this frame's
    // layout; reading its type name neither retains nor mutates it.
    let registered_type = unsafe { (*registered).concrete_type_name() };
    if std::ptr::eq(registered.cast::<()>().cast_const(), inner.0) && registered_type == inner.1 {
        node_map.insert(node, std::ptr::from_mut(outer));
    }
}

/// A widget that extends another by overriding a few [`Widget`] methods and
/// forwarding the rest. See the [module docs](self).
pub trait Decorator {
    /// The widget this one extends.
    type Inner: Widget + ?Sized;

    /// The extended widget.
    fn inner(&self) -> &Self::Inner;

    /// The extended widget, mutably.
    fn inner_mut(&mut self) -> &mut Self::Inner;

    #[doc(hidden)]
    fn concrete_type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    #[doc(hidden)]
    fn resets_callback_binding(&self) -> bool {
        self.inner().resets_callback_binding()
    }

    #[doc(hidden)]
    fn legacy_callback_boundary(&self) -> Option<(NodeId, crate::history_mode::HistoryOwner)> {
        self.inner().legacy_callback_boundary()
    }

    #[doc(hidden)]
    fn inactive_callback_boundary(&self) -> Option<NodeId> {
        self.inner().inactive_callback_boundary()
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        self.inner().native_input_snapshot(state, path)
    }

    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        self.inner().callback_binding()
    }

    fn has_separate_document_owner(&self) -> bool {
        self.inner().has_separate_document_owner()
    }

    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        self.inner().history_owner()
    }

    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        self.inner().record_owners()
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "opaque toolkit-only request prevents a public root/registry decoder loan"
    )]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        self.inner_mut().visit_components(request)
    }

    #[doc(hidden)]
    fn visit_retained_components(
        &mut self,
        request: &mut crate::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        self.inner_mut().visit_retained_components(request)
    }

    fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>) {
        self.inner().provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut capability::RequestMut<'a, '_>) {
        self.inner_mut().provide_mut(request);
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.inner_mut().set_platform_conventions(conventions);
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.inner_mut().set_interaction_environment(environment);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.inner_mut().sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.inner_mut().update(state, path)
    }

    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, path: &automerge::ObjId) {
        self.inner_mut().settle(edits, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner_mut().build_layout(taffy, theme, node_map)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.inner_mut().measure(known, available, text_renderer)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.inner_mut().adjust_layout_style(taffy, node, text)
    }

    #[doc(hidden)]
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        self.inner().manual_layout_policy()
    }

    fn layout_info(&self) -> Option<LayoutInfo> {
        self.inner().layout_info()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        self.inner_mut().apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn observe_physical_input(&mut self, input: super::PhysicalInput) {
        self.inner_mut().observe_physical_input(input);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.inner_mut().handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.inner().children()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.inner().for_each_child(visitor);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.inner_mut().children_mut()
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        self.inner().local_resource_operation_handlers()
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        self.inner().resource_operation_handlers()
    }

    fn name(&self) -> &str {
        self.inner().name()
    }

    fn test_id(&self) -> Option<&str> {
        self.inner().test_id()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    fn reveal_descendant(&mut self, target: *const dyn Widget) -> bool {
        self.inner_mut().reveal_descendant(target)
    }

    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
        self.inner().test_rects()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.inner().get_rect()
    }

    fn paint_bounds(&self) -> vello::kurbo::Rect {
        self.inner().paint_bounds()
    }

    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        self.inner().child_clip_rect()
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.inner_mut().take_action_palette_request()
    }

    fn take_search_request(&mut self) -> bool {
        self.inner_mut().take_search_request()
    }

    fn take_action_revert(
        &mut self,
    ) -> Option<(tilo_actions::EncodedAction, tilo_actions::EncodedAction)> {
        self.inner_mut().take_action_revert()
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.inner_mut().take_committed_action()
    }

    fn keeps_undo_step_open(&self) -> bool {
        self.inner().keeps_undo_step_open()
    }

    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> crate::undo::UndoStatus) {
        self.inner_mut().set_undo_status(status)
    }

    fn take_undo_request(&mut self) -> Option<crate::undo::UndoRequest> {
        self.inner_mut().take_undo_request()
    }

    fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
        self.inner_mut().handle_local_undo_request(request)
    }

    fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        self.inner().cursor_shape(point)
    }

    fn claims_raw_touch(&self, point: vello::kurbo::Point) -> bool {
        self.inner().claims_raw_touch(point)
    }

    fn tooltip(&self) -> Option<&str> {
        self.inner().tooltip()
    }

    fn tooltip_side(&self) -> super::tooltip::TooltipSide {
        self.inner().tooltip_side()
    }

    fn tooltip_target_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Option<(vello::kurbo::Rect, String)> {
        self.inner().tooltip_target_at(point)
    }

    fn is_focusable(&self) -> bool {
        self.inner().is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.inner_mut().set_focus(focused);
    }

    fn set_invalid(&mut self, invalid: bool) {
        self.inner_mut().set_invalid(invalid);
    }

    fn is_focused(&self) -> bool {
        self.inner().is_focused()
    }

    fn wants_tab_input(&self) -> bool {
        self.inner().wants_tab_input()
    }

    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        self.inner().ime_cursor_rect()
    }

    fn traps_focus(&self) -> bool {
        self.inner().traps_focus()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.inner().accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<vello::kurbo::Rect> {
        self.inner().synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.inner_mut().focus_synthetic_accessibility(path)
    }

    fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
        self.inner().accessibility_child_parent(child_index)
    }

    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        self.inner().accessibility_child_path(child_index)
    }

    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        self.inner().accessibility_represents_child(child_index)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.inner_mut().perform_action(action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.inner_mut()
            .perform_action_at(action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.inner_mut()
            .perform_semantic_action(target, action, state, path)
    }

    fn is_enabled(&self) -> bool {
        self.inner().is_enabled()
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        self.inner().ime_requires_deferred_ack()
    }

    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        self.inner_mut().start_deferred_text()
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        self.inner_mut().commit_deferred_text(text)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.inner().ime_edit_fingerprint(key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.inner().ime_purpose()
    }

    fn ime_multiline(&self) -> bool {
        self.inner().ime_multiline()
    }

    fn presented(&self) -> bool {
        self.inner().presented()
    }

    fn opening(&self) -> bool {
        self.inner().opening()
    }

    fn text_review(&self) -> Option<(usize, String)> {
        self.inner().text_review()
    }

    fn on_external_event(
        &mut self,
        payload: &str,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) {
        self.inner_mut().on_external_event(payload, state, root);
    }

    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        self.inner_mut().on_nav_gesture(dir, state, path)
    }

    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        self.inner().nav_availability()
    }

    fn take_focus_reset(&mut self) -> bool {
        self.inner_mut().take_focus_reset()
    }

    fn take_accessibility_request(&mut self) -> Option<crate::a11y::AccessibilityRequest> {
        self.inner_mut().take_accessibility_request()
    }

    fn dev_route(&self) -> Option<super::router::RouteSnapshot> {
        self.inner().dev_route()
    }

    fn restore_dev_route(&mut self, route: &super::router::RouteSnapshot) -> bool {
        self.inner_mut().restore_dev_route(route)
    }

    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        self.inner().appearance_intent()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.inner_mut().set_motion_reduced(reduced);
    }

    fn needs_idle_sync(&self) -> bool {
        self.inner().needs_idle_sync()
    }

    fn apply_room_action(
        &mut self,
        doc: &mut crate::widget::Edits<'_>,
        type_id: &str,
        payload: &[u8],
    ) -> bool {
        self.inner_mut().apply_room_action(doc, type_id, payload)
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner_mut().start_operation(op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.inner_mut()
            .poll_resource_operation(invocation, cancellation_requested, doc, path)
    }

    fn accepts_drop(&self, payload: &DragPayload) -> bool {
        self.inner().accepts_drop(payload)
    }

    fn on_drop(
        &mut self,
        payload: DragPayload,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        self.inner_mut().on_drop(payload, state, path);
    }

    fn wants_pointer_capture(&self) -> bool {
        self.inner().wants_pointer_capture()
    }

    fn is_floating(&self) -> bool {
        self.inner().is_floating()
    }

    fn needs_animation_frame(&self) -> bool {
        self.inner().needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.inner().next_wake_deadline()
    }

    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        self.inner().modal_capture()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner_mut().take_structure_dirty()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.inner().has_pending_visual_work()
    }

    fn has_transient_feedback(&self) -> bool {
        self.inner().has_transient_feedback()
    }

    fn needs_redraw(&self) -> bool {
        self.inner().needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        self.inner().collect_damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.inner_mut().mark_drawn();
    }

    fn release_scene_cache(&mut self) {
        self.inner_mut().release_scene_cache();
    }

    fn is_layout_dirty(&self) -> bool {
        self.inner().is_layout_dirty()
    }

    fn layout_needs_measure(&self) -> bool {
        self.inner().layout_needs_measure()
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner_mut()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: PrimaryScrollAction,
    ) -> PrimaryScrollResult {
        self.inner_mut().apply_primary_vertical_scroll(action)
    }

    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        self.inner_mut().focus_requested(target, tree)
    }

    fn is_default_keyboard_focus(&self) -> bool {
        self.inner().is_default_keyboard_focus()
    }

    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        self.inner().keyboard_input_scope()
    }

    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        self.inner_mut().mount_native_views(host)
    }

    fn native_view_frame_start(&mut self) {
        self.inner_mut().native_view_frame_start()
    }

    /// Decorators must opt in themselves: an outer handler can change what
    /// its inner widget declares even when the inner scroll is view-only.
    fn view_only_scroll(&self, _event: &WindowEvent) -> bool {
        false
    }

    fn primary_vertical_scroll(&self) -> Option<PrimaryScrollRegion> {
        self.inner().primary_vertical_scroll()
    }

    fn semantic_readers(
        &self,
    ) -> &'static [crate::semantic_document_read::RegisteredSemanticReader] {
        self.inner().semantic_readers()
    }

    fn unmount_native_views(&mut self) {
        self.inner_mut().unmount_native_views()
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners(
        &self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceReadRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        self.inner().visit_resource_owners(request)
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners_mut(
        &mut self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        self.inner_mut().visit_resource_owners_mut(request)
    }
}

/// Every decorator is a widget: each method is the decorator's own, which
/// forwards to the inner widget unless the decorator overrides it.
impl<T: Decorator + 'static> Widget for T {
    #[doc(hidden)]
    fn concrete_type_name(&self) -> &'static str {
        Decorator::concrete_type_name(self)
    }

    #[doc(hidden)]
    fn resets_callback_binding(&self) -> bool {
        Decorator::resets_callback_binding(self)
    }

    #[doc(hidden)]
    fn legacy_callback_boundary(&self) -> Option<(NodeId, crate::history_mode::HistoryOwner)> {
        Decorator::legacy_callback_boundary(self)
    }

    #[doc(hidden)]
    fn inactive_callback_boundary(&self) -> Option<NodeId> {
        Decorator::inactive_callback_boundary(self)
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        Decorator::native_input_snapshot(self, state, path)
    }

    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        Decorator::callback_binding(self)
    }

    fn has_separate_document_owner(&self) -> bool {
        Decorator::has_separate_document_owner(self)
    }

    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        Decorator::history_owner(self)
    }

    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        Decorator::record_owners(self)
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "opaque toolkit-only request prevents a public root/registry decoder loan"
    )]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        Decorator::visit_components(self, request)
    }

    #[doc(hidden)]
    fn visit_retained_components(
        &mut self,
        request: &mut crate::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        Decorator::visit_retained_components(self, request)
    }

    fn provide<'a>(&'a self, request: &mut capability::Request<'a, '_>) {
        Decorator::provide(self, request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut capability::RequestMut<'a, '_>) {
        Decorator::provide_mut(self, request);
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        Decorator::set_platform_conventions(self, conventions);
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        Decorator::set_interaction_environment(self, environment);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        Decorator::sync(self, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        Decorator::update(self, state, path)
    }

    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, path: &automerge::ObjId) {
        Decorator::settle(self, edits, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Decorator::build_layout(self, taffy, theme, node_map);
        let inner = (
            std::ptr::from_ref(self.inner()).cast::<()>(),
            self.inner().concrete_type_name(),
        );
        stand_in(node_map, node, inner, self);
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        Decorator::measure(self, known, available, text_renderer)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        Decorator::adjust_layout_style(self, taffy, node, text)
    }

    #[doc(hidden)]
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        Decorator::manual_layout_policy(self)
    }

    fn layout_info(&self) -> Option<LayoutInfo> {
        Decorator::layout_info(self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        Decorator::apply_layout(self, taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Decorator::draw(self, scene, transform, text_renderer, theme, presence);
    }

    fn observe_physical_input(&mut self, input: super::PhysicalInput) {
        Decorator::observe_physical_input(self, input);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Decorator::handle_event(self, event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        Decorator::children(self)
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        Decorator::for_each_child(self, visitor);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        Decorator::children_mut(self)
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        Decorator::local_resource_operation_handlers(self)
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        Decorator::resource_operation_handlers(self)
    }

    fn name(&self) -> &str {
        Decorator::name(self)
    }

    fn test_id(&self) -> Option<&str> {
        Decorator::test_id(self)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Decorator::as_any(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Decorator::as_any_mut(self)
    }

    fn reveal_descendant(&mut self, target: *const dyn Widget) -> bool {
        Decorator::reveal_descendant(self, target)
    }

    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
        Decorator::test_rects(self)
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        Decorator::get_rect(self)
    }

    fn paint_bounds(&self) -> vello::kurbo::Rect {
        Decorator::paint_bounds(self)
    }

    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        Decorator::child_clip_rect(self)
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        Decorator::take_action_palette_request(self)
    }

    fn take_search_request(&mut self) -> bool {
        Decorator::take_search_request(self)
    }

    fn take_action_revert(
        &mut self,
    ) -> Option<(tilo_actions::EncodedAction, tilo_actions::EncodedAction)> {
        Decorator::take_action_revert(self)
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        Decorator::take_committed_action(self)
    }

    fn keeps_undo_step_open(&self) -> bool {
        Decorator::keeps_undo_step_open(self)
    }

    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> crate::undo::UndoStatus) {
        Decorator::set_undo_status(self, status)
    }

    fn take_undo_request(&mut self) -> Option<crate::undo::UndoRequest> {
        Decorator::take_undo_request(self)
    }

    fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
        Decorator::handle_local_undo_request(self, request)
    }

    fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        Decorator::cursor_shape(self, point)
    }

    fn claims_raw_touch(&self, point: vello::kurbo::Point) -> bool {
        Decorator::claims_raw_touch(self, point)
    }

    fn tooltip(&self) -> Option<&str> {
        Decorator::tooltip(self)
    }

    fn tooltip_side(&self) -> super::tooltip::TooltipSide {
        Decorator::tooltip_side(self)
    }

    fn tooltip_target_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Option<(vello::kurbo::Rect, String)> {
        Decorator::tooltip_target_at(self, point)
    }

    fn is_focusable(&self) -> bool {
        Decorator::is_focusable(self)
    }

    fn set_focus(&mut self, focused: bool) {
        Decorator::set_focus(self, focused);
    }

    fn set_invalid(&mut self, invalid: bool) {
        Decorator::set_invalid(self, invalid);
    }

    fn is_focused(&self) -> bool {
        Decorator::is_focused(self)
    }

    fn wants_tab_input(&self) -> bool {
        Decorator::wants_tab_input(self)
    }

    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        Decorator::ime_cursor_rect(self)
    }

    fn traps_focus(&self) -> bool {
        Decorator::traps_focus(self)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Decorator::accessibility(self)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<vello::kurbo::Rect> {
        Decorator::synthetic_accessibility_rect(self, path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        Decorator::focus_synthetic_accessibility(self, path)
    }

    fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
        Decorator::accessibility_child_parent(self, child_index)
    }

    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        Decorator::accessibility_child_path(self, child_index)
    }

    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        Decorator::accessibility_represents_child(self, child_index)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Decorator::perform_action(self, action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Decorator::perform_action_at(self, action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Decorator::perform_semantic_action(self, target, action, state, path)
    }

    fn is_enabled(&self) -> bool {
        Decorator::is_enabled(self)
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        Decorator::ime_requires_deferred_ack(self)
    }

    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        Decorator::start_deferred_text(self)
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        Decorator::commit_deferred_text(self, text)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        Decorator::ime_edit_fingerprint(self, key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Decorator::ime_purpose(self)
    }

    fn ime_multiline(&self) -> bool {
        Decorator::ime_multiline(self)
    }

    fn presented(&self) -> bool {
        Decorator::presented(self)
    }

    fn opening(&self) -> bool {
        Decorator::opening(self)
    }

    fn text_review(&self) -> Option<(usize, String)> {
        Decorator::text_review(self)
    }

    fn on_external_event(
        &mut self,
        payload: &str,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) {
        Decorator::on_external_event(self, payload, state, root);
    }

    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        Decorator::on_nav_gesture(self, dir, state, path)
    }

    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        Decorator::nav_availability(self)
    }

    fn take_focus_reset(&mut self) -> bool {
        Decorator::take_focus_reset(self)
    }

    fn take_accessibility_request(&mut self) -> Option<crate::a11y::AccessibilityRequest> {
        Decorator::take_accessibility_request(self)
    }

    fn dev_route(&self) -> Option<super::router::RouteSnapshot> {
        Decorator::dev_route(self)
    }

    fn restore_dev_route(&mut self, route: &super::router::RouteSnapshot) -> bool {
        Decorator::restore_dev_route(self, route)
    }

    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        Decorator::appearance_intent(self)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        Decorator::set_motion_reduced(self, reduced);
    }

    fn needs_idle_sync(&self) -> bool {
        Decorator::needs_idle_sync(self)
    }

    fn apply_room_action(
        &mut self,
        doc: &mut crate::widget::Edits<'_>,
        type_id: &str,
        payload: &[u8],
    ) -> bool {
        Decorator::apply_room_action(self, doc, type_id, payload)
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        Decorator::start_operation(self, op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        Decorator::poll_resource_operation(self, invocation, cancellation_requested, doc, path)
    }

    fn accepts_drop(&self, payload: &DragPayload) -> bool {
        Decorator::accepts_drop(self, payload)
    }

    fn on_drop(
        &mut self,
        payload: DragPayload,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        Decorator::on_drop(self, payload, state, path);
    }

    fn wants_pointer_capture(&self) -> bool {
        Decorator::wants_pointer_capture(self)
    }

    fn is_floating(&self) -> bool {
        Decorator::is_floating(self)
    }

    fn needs_animation_frame(&self) -> bool {
        Decorator::needs_animation_frame(self)
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        Decorator::next_wake_deadline(self)
    }

    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        Decorator::modal_capture(self)
    }

    fn take_structure_dirty(&mut self) -> bool {
        Decorator::take_structure_dirty(self)
    }

    fn has_pending_visual_work(&self) -> bool {
        Decorator::has_pending_visual_work(self)
    }

    fn has_transient_feedback(&self) -> bool {
        Decorator::has_transient_feedback(self)
    }

    fn needs_redraw(&self) -> bool {
        Decorator::needs_redraw(self)
    }

    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        Decorator::collect_damage(self, regions);
    }

    fn mark_drawn(&mut self) {
        Decorator::mark_drawn(self);
    }

    fn release_scene_cache(&mut self) {
        Decorator::release_scene_cache(self);
    }

    fn is_layout_dirty(&self) -> bool {
        Decorator::is_layout_dirty(self)
    }

    fn layout_needs_measure(&self) -> bool {
        Decorator::layout_needs_measure(self)
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Decorator::draw_overlay(self, scene, transform, text_renderer, theme, presence);
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: PrimaryScrollAction,
    ) -> PrimaryScrollResult {
        Decorator::apply_primary_vertical_scroll(self, action)
    }

    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        Decorator::focus_requested(self, target, tree)
    }

    fn is_default_keyboard_focus(&self) -> bool {
        Decorator::is_default_keyboard_focus(self)
    }

    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        Decorator::keyboard_input_scope(self)
    }

    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        Decorator::mount_native_views(self, host)
    }

    fn native_view_frame_start(&mut self) {
        Decorator::native_view_frame_start(self)
    }

    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        Decorator::view_only_scroll(self, event)
    }

    fn primary_vertical_scroll(&self) -> Option<PrimaryScrollRegion> {
        Decorator::primary_vertical_scroll(self)
    }

    fn semantic_readers(
        &self,
    ) -> &'static [crate::semantic_document_read::RegisteredSemanticReader] {
        Decorator::semantic_readers(self)
    }

    fn unmount_native_views(&mut self) {
        Decorator::unmount_native_views(self)
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners(
        &self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceReadRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        Decorator::visit_resource_owners(self, request)
    }

    #[doc(hidden)]
    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners_mut(
        &mut self,
        request: &mut crate::bound_dispatch::legacy_document::ResourceRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        Decorator::visit_resource_owners_mut(self, request)
    }
}

#[cfg(test)]
#[path = "decorator_tests.rs"]
mod tests;
