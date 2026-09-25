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
//! [`as_any_mut`](Widget::as_any_mut) is `None` unless overridden.
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
//!   derived from another method (`draw_with_presence` calls `draw`,
//!   `perform_action_at` calls `perform_action`, `tooltip_target_at` calls
//!   `tooltip`, `adjust_layout_with_measure` calls `adjust_layout_style`,
//!   `collect_damage` calls `needs_redraw`). Here each forwards to the inner
//!   widget's own method, so an override of the derived-from method alone is
//!   not what the host calls: override the entry point the host uses
//!   (`draw_with_presence` for painting in a live frame, and `draw` for
//!   callers that paint directly). Input is the exception: overriding either
//!   [`handle_event`](Widget::handle_event) or
//!   [`handle_event_ctx`](Widget::handle_event_ctx) works, because the
//!   default `handle_event` goes through the decorator's `handle_event_ctx`.
//! * **The inner widget owns its layout node.** The inner widget registers
//!   itself in the node map during `build_layout`, so hooks the host reaches
//!   by sweeping or hit-testing that map (`cursor_shape`, `tooltip`,
//!   `set_undo_status`, `take_undo_request`, `take_committed_action`) are
//!   answered by the inner widget. A decorator that overrides one of those
//!   registers itself for its node instead, with [`register_node`].
//! * **Method calls.** A decorator is both a [`Decorator`] and, through the
//!   blanket impl, a [`Widget`]. With both traits imported, `self.get_rect()`
//!   is ambiguous, so implement the trait by path (`impl
//!   ui::widget::Decorator for ..`) rather than importing it; the prelude
//!   deliberately does not export it.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use super::{
    ActionDirectoryResourcePage, AudiencePresentationProvider, Collaborative,
    CompleteContentProvider, Continuable, ControlBindingMetadata, EventCtx,
    FloatingSurfaceProvider, FullscreenProvider, LayoutInfo, ManualPolicy, NativeViewLifecycle,
    ResourceOperationStart, Widget, WindowCloseAdmission,
};
use crate::dnd::DragPayload;
use crate::permissions::PermissionScope;
use crate::presence::{PresenceCtx, PresenceKey};
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

    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[automerge::ChangeHash],
        doc: &AutoCommit,
        path: &ObjId,
    ) {
        self.inner_mut()
            .document_action_applied(action, before, doc, path);
    }

    fn history_registration(
        &self,
    ) -> Result<
        Option<crate::history_mode::HistoryRegistration>,
        crate::history_mode::HistoryRegistrationError,
    > {
        self.inner().history_registration()
    }

    fn document_history_definition(
        &self,
    ) -> Option<&'static crate::history_mode::HistoryDefinition> {
        self.inner().document_history_definition()
    }

    #[doc(hidden)]
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        self.inner().visit_framework_history(visitor);
    }

    #[doc(hidden)]
    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        self.inner_mut().visit_framework_history_mut(visitor);
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

    fn native_input_snapshot(&self, state: &AutoCommit, path: &ObjId) -> Option<serde_json::Value> {
        self.inner().native_input_snapshot(state, path)
    }

    fn on_window_close_requested(&mut self) -> WindowCloseAdmission {
        self.inner_mut().on_window_close_requested()
    }

    fn take_window_close_intent(&mut self) -> bool {
        self.inner_mut().take_window_close_intent()
    }

    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        self.inner().unsaved_documents(doc)
    }

    fn checkpoint_components(&mut self) -> Vec<String> {
        self.inner_mut().checkpoint_components()
    }

    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.inner_mut().finish_admitted_close()
    }

    fn try_flush_owned_persistence(&mut self) -> Option<Result<(), String>> {
        self.inner_mut().try_flush_owned_persistence()
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.inner().has_pending_control_invocation()
    }

    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.inner_mut().close_components()
    }

    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.inner_mut().take_component_close_reports()
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

    fn collaborative_mut(&mut self) -> Option<&mut dyn Collaborative> {
        self.inner_mut().collaborative_mut()
    }

    fn native_view_lifecycle_mut(&mut self) -> Option<&mut dyn NativeViewLifecycle> {
        self.inner_mut().native_view_lifecycle_mut()
    }

    fn floating_surface_provider(&self) -> Option<&dyn FloatingSurfaceProvider> {
        self.inner().floating_surface_provider()
    }

    fn floating_surface_provider_mut(&mut self) -> Option<&mut dyn FloatingSurfaceProvider> {
        self.inner_mut().floating_surface_provider_mut()
    }

    fn audience_presentation_provider(&self) -> Option<&dyn AudiencePresentationProvider> {
        self.inner().audience_presentation_provider()
    }

    fn audience_presentation_provider_mut(
        &mut self,
    ) -> Option<&mut dyn AudiencePresentationProvider> {
        self.inner_mut().audience_presentation_provider_mut()
    }

    fn fullscreen_provider(&self) -> Option<&dyn FullscreenProvider> {
        self.inner().fullscreen_provider()
    }

    fn fullscreen_provider_mut(&mut self) -> Option<&mut dyn FullscreenProvider> {
        self.inner_mut().fullscreen_provider_mut()
    }

    fn complete_content_provider(&self) -> Option<&dyn CompleteContentProvider> {
        self.inner().complete_content_provider()
    }

    fn complete_content_provider_mut(&mut self) -> Option<&mut dyn CompleteContentProvider> {
        self.inner_mut().complete_content_provider_mut()
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

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.inner_mut().sync(state, path);
    }

    fn continuable(&self) -> Option<&dyn Continuable> {
        self.inner().continuable()
    }

    fn continuable_mut(&mut self) -> Option<&mut dyn Continuable> {
        self.inner_mut().continuable_mut()
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        self.inner_mut().update(state, path)
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

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        self.inner_mut().adjust_layout_style(taffy, node)
    }

    #[doc(hidden)]
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        self.inner().manual_layout_policy()
    }

    fn layout_info(&self) -> Option<LayoutInfo> {
        self.inner().layout_info()
    }

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.inner_mut()
            .adjust_layout_with_measure(taffy, node, text)
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

    #[allow(
        clippy::too_many_arguments,
        reason = "temporary compatibility adapter; implementations use EventCtx"
    )]
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
        Decorator::handle_event_ctx(
            self,
            event,
            &mut EventCtx {
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            },
        );
    }

    fn handle_event_ctx(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.inner_mut().handle_event(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            ctx.state,
            ctx.path,
            ctx.handled,
            ctx.focus_target,
            ctx.node_id,
            ctx.taffy,
        );
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

    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        self.inner().child_clip_rect()
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.inner_mut().take_action_palette_request()
    }

    fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        self.inner().cursor_shape(point)
    }

    fn tooltip(&self) -> Option<&str> {
        self.inner().tooltip()
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

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.inner_mut().perform_action(action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.inner_mut()
            .perform_action_at(action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.inner_mut()
            .perform_semantic_action(target, action, state, path)
    }

    fn is_enabled(&self) -> bool {
        self.inner().is_enabled()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.inner().ime_purpose()
    }

    fn presented(&self) -> bool {
        self.inner().presented()
    }

    fn opening(&self) -> bool {
        self.inner().opening()
    }

    fn needs_commit(&self) -> bool {
        self.inner().needs_commit()
    }

    fn take_commit_context(&mut self) -> Option<tilo_actions::ActionContext> {
        self.inner_mut().take_commit_context()
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.inner_mut().take_control_invocation()
    }

    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        self.inner_mut()
            .on_control_invocation_result(address, result);
    }

    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        self.inner().control_action_bindings()
    }

    fn has_action_control_bindings(&self) -> bool {
        self.inner().has_action_control_bindings()
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        self.inner_mut()
            .refresh_control_action_availability(allowed);
    }

    fn on_commit(&mut self) {
        self.inner_mut().on_commit();
    }

    fn get_state(&self) -> Option<String> {
        self.inner().get_state()
    }

    fn set_state(&mut self, state: &str) {
        self.inner_mut().set_state(state);
    }

    fn text_review(&self) -> Option<(usize, String)> {
        self.inner().text_review()
    }

    fn on_external_event(&mut self, payload: &str, state: &mut AutoCommit, root: &ObjId) {
        self.inner_mut().on_external_event(payload, state, root);
    }

    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.inner_mut().on_nav_gesture(dir, state, path)
    }

    fn take_focus_reset(&mut self) -> bool {
        self.inner_mut().take_focus_reset()
    }

    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        self.inner().record_intent()
    }

    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        self.inner().view_state()
    }

    fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
        self.inner_mut().restore_view_state(view);
    }

    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        self.inner().appearance_intent()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.inner_mut().set_motion_reduced(reduced);
    }

    fn set_record_scope(&mut self, record: &crate::record::RecordId) {
        self.inner_mut().set_record_scope(record);
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        self.inner().presence_key()
    }

    fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        self.inner().presence_payload()
    }

    fn needs_idle_sync(&self) -> bool {
        self.inner().needs_idle_sync()
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.inner().permission_scope()
    }

    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.inner().resources(doc)
    }

    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ActionDirectoryResourcePage> {
        self.inner()
            .action_directory_resource_page(doc, cursor, max_results)
    }

    fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
        self.inner().action_directory_revision(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        self.inner().resource(doc, id)
    }

    fn action_directory_visibility(
        &self,
        doc: &AutoCommit,
        resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        self.inner().action_directory_visibility(doc, resource_id)
    }

    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        self.inner().search_providers()
    }

    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, path: &ObjId) {
        self.inner_mut()
            .activate_search_result(action_id, doc, path);
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.inner_mut().take_committed_action()
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

    fn apply_room_action(&mut self, doc: &mut AutoCommit, type_id: &str, payload: &[u8]) -> bool {
        self.inner_mut().apply_room_action(doc, type_id, payload)
    }

    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<bool, String> {
        self.inner_mut()
            .invoke_resource_operation(kind, type_id, payload, doc, path)
    }

    fn invoke_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        self.inner_mut()
            .invoke_resource_operation_v2(kind, type_id, payload, doc, path)
    }

    fn start_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner_mut()
            .start_resource_operation(invocation, kind, type_id, payload, doc, path)
    }

    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.inner_mut()
            .start_resource_operation_with_document(invocation, kind, type_id, payload, doc, path)
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

    fn on_drop(&mut self, payload: DragPayload, state: &mut AutoCommit, path: &ObjId) {
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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        self.inner_mut().draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
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
}

/// Every decorator is a widget: each method is the decorator's own, which
/// forwards to the inner widget unless the decorator overrides it.
impl<T: Decorator> Widget for T {
    #[doc(hidden)]
    fn concrete_type_name(&self) -> &'static str {
        Decorator::concrete_type_name(self)
    }

    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[automerge::ChangeHash],
        doc: &AutoCommit,
        path: &ObjId,
    ) {
        Decorator::document_action_applied(self, action, before, doc, path);
    }

    fn history_registration(
        &self,
    ) -> Result<
        Option<crate::history_mode::HistoryRegistration>,
        crate::history_mode::HistoryRegistrationError,
    > {
        Decorator::history_registration(self)
    }

    fn document_history_definition(
        &self,
    ) -> Option<&'static crate::history_mode::HistoryDefinition> {
        Decorator::document_history_definition(self)
    }

    #[doc(hidden)]
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        Decorator::visit_framework_history(self, visitor);
    }

    #[doc(hidden)]
    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        Decorator::visit_framework_history_mut(self, visitor);
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

    fn native_input_snapshot(&self, state: &AutoCommit, path: &ObjId) -> Option<serde_json::Value> {
        Decorator::native_input_snapshot(self, state, path)
    }

    fn on_window_close_requested(&mut self) -> WindowCloseAdmission {
        Decorator::on_window_close_requested(self)
    }

    fn take_window_close_intent(&mut self) -> bool {
        Decorator::take_window_close_intent(self)
    }

    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        Decorator::unsaved_documents(self, doc)
    }

    fn checkpoint_components(&mut self) -> Vec<String> {
        Decorator::checkpoint_components(self)
    }

    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        Decorator::finish_admitted_close(self)
    }

    fn try_flush_owned_persistence(&mut self) -> Option<Result<(), String>> {
        Decorator::try_flush_owned_persistence(self)
    }

    fn has_pending_control_invocation(&self) -> bool {
        Decorator::has_pending_control_invocation(self)
    }

    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        Decorator::close_components(self)
    }

    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        Decorator::take_component_close_reports(self)
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

    fn collaborative_mut(&mut self) -> Option<&mut dyn Collaborative> {
        Decorator::collaborative_mut(self)
    }

    fn native_view_lifecycle_mut(&mut self) -> Option<&mut dyn NativeViewLifecycle> {
        Decorator::native_view_lifecycle_mut(self)
    }

    fn floating_surface_provider(&self) -> Option<&dyn FloatingSurfaceProvider> {
        Decorator::floating_surface_provider(self)
    }

    fn floating_surface_provider_mut(&mut self) -> Option<&mut dyn FloatingSurfaceProvider> {
        Decorator::floating_surface_provider_mut(self)
    }

    fn audience_presentation_provider(&self) -> Option<&dyn AudiencePresentationProvider> {
        Decorator::audience_presentation_provider(self)
    }

    fn audience_presentation_provider_mut(
        &mut self,
    ) -> Option<&mut dyn AudiencePresentationProvider> {
        Decorator::audience_presentation_provider_mut(self)
    }

    fn fullscreen_provider(&self) -> Option<&dyn FullscreenProvider> {
        Decorator::fullscreen_provider(self)
    }

    fn fullscreen_provider_mut(&mut self) -> Option<&mut dyn FullscreenProvider> {
        Decorator::fullscreen_provider_mut(self)
    }

    fn complete_content_provider(&self) -> Option<&dyn CompleteContentProvider> {
        Decorator::complete_content_provider(self)
    }

    fn complete_content_provider_mut(&mut self) -> Option<&mut dyn CompleteContentProvider> {
        Decorator::complete_content_provider_mut(self)
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

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        Decorator::sync(self, state, path);
    }

    fn continuable(&self) -> Option<&dyn Continuable> {
        Decorator::continuable(self)
    }

    fn continuable_mut(&mut self) -> Option<&mut dyn Continuable> {
        Decorator::continuable_mut(self)
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        Decorator::update(self, state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        Decorator::build_layout(self, taffy, theme, node_map)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        Decorator::measure(self, known, available, text_renderer)
    }

    fn adjust_layout_style(&mut self, taffy: &mut TaffyTree<()>, node: NodeId) -> bool {
        Decorator::adjust_layout_style(self, taffy, node)
    }

    #[doc(hidden)]
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        Decorator::manual_layout_policy(self)
    }

    fn layout_info(&self) -> Option<LayoutInfo> {
        Decorator::layout_info(self)
    }

    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        Decorator::adjust_layout_with_measure(self, taffy, node, text)
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

    #[allow(
        clippy::too_many_arguments,
        reason = "temporary compatibility adapter; implementations use EventCtx"
    )]
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
        Decorator::handle_event(
            self,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }

    fn handle_event_ctx(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        Decorator::handle_event_ctx(self, event, ctx);
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

    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        Decorator::child_clip_rect(self)
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        Decorator::take_action_palette_request(self)
    }

    fn cursor_shape(&self, point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        Decorator::cursor_shape(self, point)
    }

    fn tooltip(&self) -> Option<&str> {
        Decorator::tooltip(self)
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

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        Decorator::perform_action(self, action, state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        Decorator::perform_action_at(self, action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        Decorator::perform_semantic_action(self, target, action, state, path)
    }

    fn is_enabled(&self) -> bool {
        Decorator::is_enabled(self)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Decorator::ime_purpose(self)
    }

    fn presented(&self) -> bool {
        Decorator::presented(self)
    }

    fn opening(&self) -> bool {
        Decorator::opening(self)
    }

    fn needs_commit(&self) -> bool {
        Decorator::needs_commit(self)
    }

    fn take_commit_context(&mut self) -> Option<tilo_actions::ActionContext> {
        Decorator::take_commit_context(self)
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        Decorator::take_control_invocation(self)
    }

    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        Decorator::on_control_invocation_result(self, address, result);
    }

    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        Decorator::control_action_bindings(self)
    }

    fn has_action_control_bindings(&self) -> bool {
        Decorator::has_action_control_bindings(self)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        Decorator::refresh_control_action_availability(self, allowed);
    }

    fn on_commit(&mut self) {
        Decorator::on_commit(self);
    }

    fn get_state(&self) -> Option<String> {
        Decorator::get_state(self)
    }

    fn set_state(&mut self, state: &str) {
        Decorator::set_state(self, state);
    }

    fn text_review(&self) -> Option<(usize, String)> {
        Decorator::text_review(self)
    }

    fn on_external_event(&mut self, payload: &str, state: &mut AutoCommit, root: &ObjId) {
        Decorator::on_external_event(self, payload, state, root);
    }

    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        Decorator::on_nav_gesture(self, dir, state, path)
    }

    fn take_focus_reset(&mut self) -> bool {
        Decorator::take_focus_reset(self)
    }

    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        Decorator::record_intent(self)
    }

    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        Decorator::view_state(self)
    }

    fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
        Decorator::restore_view_state(self, view);
    }

    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        Decorator::appearance_intent(self)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        Decorator::set_motion_reduced(self, reduced);
    }

    fn set_record_scope(&mut self, record: &crate::record::RecordId) {
        Decorator::set_record_scope(self, record);
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        Decorator::presence_key(self)
    }

    fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        Decorator::presence_payload(self)
    }

    fn needs_idle_sync(&self) -> bool {
        Decorator::needs_idle_sync(self)
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        Decorator::permission_scope(self)
    }

    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        Decorator::resources(self, doc)
    }

    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ActionDirectoryResourcePage> {
        Decorator::action_directory_resource_page(self, doc, cursor, max_results)
    }

    fn action_directory_revision(&self, doc: &AutoCommit) -> Option<String> {
        Decorator::action_directory_revision(self, doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        Decorator::resource(self, doc, id)
    }

    fn action_directory_visibility(
        &self,
        doc: &AutoCommit,
        resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        Decorator::action_directory_visibility(self, doc, resource_id)
    }

    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        Decorator::search_providers(self)
    }

    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, path: &ObjId) {
        Decorator::activate_search_result(self, action_id, doc, path);
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        Decorator::take_committed_action(self)
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

    fn apply_room_action(&mut self, doc: &mut AutoCommit, type_id: &str, payload: &[u8]) -> bool {
        Decorator::apply_room_action(self, doc, type_id, payload)
    }

    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<bool, String> {
        Decorator::invoke_resource_operation(self, kind, type_id, payload, doc, path)
    }

    fn invoke_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        Decorator::invoke_resource_operation_v2(self, kind, type_id, payload, doc, path)
    }

    fn start_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        Decorator::start_resource_operation(self, invocation, kind, type_id, payload, doc, path)
    }

    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        Decorator::start_resource_operation_with_document(
            self, invocation, kind, type_id, payload, doc, path,
        )
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

    fn on_drop(&mut self, payload: DragPayload, state: &mut AutoCommit, path: &ObjId) {
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

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        Decorator::draw_with_presence(
            self,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
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
}

#[cfg(test)]
#[path = "decorator_tests.rs"]
mod tests;
