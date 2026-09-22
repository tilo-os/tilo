use crate::dnd::DragPayload;
use crate::permissions::PermissionScope;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::time::Instant;

/// Shared disabled-state opacity: wrap draw output in
/// `scene.push_layer(.., DISABLED_ALPHA, ..)` / `pop_layer()`.
pub const DISABLED_ALPHA: f32 = 0.45;

/// Result of admitting an effect/view operation into a widget.
///
/// `Accepted` is deliberately distinct from success: the widget must later
/// report progress and exactly one terminal [`tilo_actions::InvocationUpdate`]
/// from [`Widget::poll_resource_operation`]. This keeps queued and streaming
/// work from being reported as completed merely because it entered a queue.
#[derive(Clone, Debug, PartialEq)]
pub enum ResourceOperationStart {
    Completed(tilo_actions::ActionOutput),
    Accepted,
}

/// Machine-readable classification of a control's activation path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlBindingKind {
    Action(tilo_actions::ActionAddress),
    /// A framework-owned CRDT edit stream. The host supplies authorship,
    /// collaboration, history boundaries, undo, and permissions directly;
    /// this avoids turning every keystroke or slider tick into a heavyweight
    /// discrete action while retaining the same platform guarantees.
    CollaborativeDocument(String),
    /// An explicitly declared stable accessibility operation. The host
    /// projects the semantic verb into the resource catalogue and routes it
    /// through the normal action door; useful for custom-painted controls and
    /// process-local editing state that does not belong in document history.
    SemanticAction(String),
    LegacyDocumentPath(String),
    LegacyCallback,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlBindingMetadata {
    pub control_id: String,
    pub kind: ControlBindingKind,
}

impl ControlBindingMetadata {
    /// Declare that a custom control's stable semantic target is its native
    /// action route. The host publishes supported accessibility verbs through
    /// the resource/action door, so no parallel callback registry is needed.
    pub fn semantic(control_id: impl Into<String>) -> Self {
        let control_id = control_id.into();
        Self {
            kind: ControlBindingKind::SemanticAction(control_id.clone()),
            control_id,
        }
    }
}

/// Derive native semantic-action provenance for actionable synthetic nodes in
/// a custom widget's accessibility tree. This is the intended low-level escape
/// hatch: authors draw anything they want, provide stable semantic targets,
/// and opt those targets into the shared action route with one method call.
pub fn semantic_action_bindings(
    semantics: Option<crate::a11y::AccessNode>,
) -> Vec<ControlBindingMetadata> {
    fn is_effect_role(role: crate::a11y::Role) -> bool {
        use crate::a11y::Role;
        matches!(
            role,
            Role::Button
                | Role::ToggleButton
                | Role::Checkbox
                | Role::RadioButton
                | Role::Switch
                | Role::ComboBox
                | Role::Slider
                | Role::SpinButton
                | Role::TextInput
                | Role::MultilineTextInput
        )
    }

    fn visit(node: &crate::a11y::AccessNode, out: &mut Vec<ControlBindingMetadata>) {
        if is_effect_role(node.role)
            && let Some(target) = &node.action_target
        {
            out.push(ControlBindingMetadata::semantic(target.clone()));
        }
        for child in &node.children {
            visit(child, out);
        }
    }

    let mut bindings = Vec::new();
    if let Some(semantics) = semantics {
        visit(&semantics, &mut bindings);
    }
    bindings
}

/// Collect resources from every retained widget without requiring container
/// authors to remember to forward their children's declarations. Resource
/// ownership stays local to the widget that implements it; composition is a
/// host property, like action-binding and accessibility traversal.
pub(crate) fn collect_widget_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    resources: &mut Vec<tilo_actions::Resource>,
) {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<crate::bound_dispatch::CallbackBinding>,
        resources: &mut Vec<tilo_actions::Resource>,
    ) {
        let boundary = widget.callback_binding();
        let binding = boundary.clone().or(inherited);
        let Ok(_entry) = binding
            .as_ref()
            .map(crate::bound_dispatch::CallbackBinding::enter)
            .transpose()
        else {
            return;
        };
        // Boundary adapters expose their real root as a child. Collect each
        // declaration once, and release its document borrow before descending.
        if boundary.is_none() {
            if let Some(binding) = &binding {
                if let Ok(found) = binding.read_document(doc, |doc, _| widget.resources(doc)) {
                    resources.extend(found);
                }
            } else {
                resources.extend(widget.resources(doc));
            }
        }
        widget.for_each_child(&mut |child| visit(child, doc, binding.clone(), resources));
    }
    visit(widget, doc, None, resources);
}

/// Test traversal probe; production callers retain bound provenance.
#[cfg(test)]
pub(crate) fn resolve_widget_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::Resource> {
    resolve_bound_resource(widget, doc, None, &mut |widget, doc| {
        widget.resource(doc, id)
    })
}

#[cfg(test)]
fn resolve_bound_resource<R>(
    widget: &dyn Widget,
    doc: &AutoCommit,
    inherited: Option<crate::bound_dispatch::CallbackBinding>,
    lookup: &mut dyn FnMut(&dyn Widget, &AutoCommit) -> Option<R>,
) -> Option<R> {
    let boundary = widget.callback_binding();
    let binding = boundary.clone().or(inherited);
    let _entry = binding
        .as_ref()
        .map(crate::bound_dispatch::CallbackBinding::enter)
        .transpose()
        .ok()?;
    if boundary.is_none() {
        let found = if let Some(binding) = &binding {
            binding
                .read_document(doc, |doc, _| lookup(widget, doc))
                .ok()
                .flatten()
        } else {
            lookup(widget, doc)
        };
        if found.is_some() {
            return found;
        }
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| resolve_bound_resource(child, doc, binding.clone(), lookup))
}

/// One bounded page from a widget-owned remote root catalogue.  `next_cursor`
/// is meaningful only while `exhausted` is false and is never interpreted by
/// a different widget or document revision.
pub struct ActionDirectoryResourcePage {
    pub resources: Vec<tilo_actions::Resource>,
    pub next_cursor: Vec<u8>,
    pub exhausted: bool,
}

/// Read a bounded page without allowing its retained widget reference or
/// originating binding to escape the traversal.
#[allow(
    clippy::option_option,
    reason = "outer None means end of traversal; inner None means this ordinal has no bounded provider"
)]
pub(crate) fn action_directory_page_at(
    widget: &dyn Widget,
    doc: &AutoCommit,
    target: usize,
    cursor: &[u8],
    max_results: usize,
) -> Option<
    Option<(
        ActionDirectoryResourcePage,
        Option<crate::bound_dispatch::CallbackBinding>,
    )>,
> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        target: usize,
        ordinal: &mut usize,
        inherited: Option<crate::bound_dispatch::CallbackBinding>,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<
        Option<(
            ActionDirectoryResourcePage,
            Option<crate::bound_dispatch::CallbackBinding>,
        )>,
    > {
        let boundary = widget.callback_binding();
        let binding = boundary.clone().or(inherited);
        let _entry = binding
            .as_ref()
            .map(crate::bound_dispatch::CallbackBinding::enter)
            .transpose()
            .ok()?;
        if *ordinal == target {
            if boundary.is_some() {
                return Some(None);
            }
            return Some(
                (if let Some(binding) = &binding {
                    binding
                        .read_document(doc, |doc, _| {
                            widget.action_directory_resource_page(doc, cursor, max_results)
                        })
                        .ok()
                        .flatten()
                } else {
                    widget.action_directory_resource_page(doc, cursor, max_results)
                })
                .map(|page| (page, binding)),
            );
        }
        *ordinal = ordinal.saturating_add(1);
        widget.children().into_iter().find_map(|child| {
            visit(
                child,
                doc,
                target,
                ordinal,
                binding.clone(),
                cursor,
                max_results,
            )
        })
    }
    visit(widget, doc, target, &mut 0, None, cursor, max_results)
}

/// Last-resort accessible label for controls whose constructor already
/// requires a stable id. Explicit localized names remain preferable, but a
/// standard control should never become silent merely because an author did
/// not repeat the same short label twice.
pub(crate) fn humanize_control_id(id: &str) -> String {
    let tail = id.rsplit('.').next().unwrap_or(id);
    let words = tail.replace(['_', '-'], " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A reusable action-intent queue for controls, including custom widgets.
/// Widgets call [`emit`](Self::emit) from any interaction path and expose the
/// queue through [`Widget::take_control_invocation`].
pub struct ActionEmitter<I> {
    binding: tilo_actions::ActionBinding<I>,
    pending: VecDeque<tilo_actions::ControlInvocation>,
    available: bool,
}

const ACTION_EMITTER_QUEUE_LIMIT: usize = 64;

impl<I> ActionEmitter<I> {
    pub fn new(binding: tilo_actions::ActionBinding<I>) -> Self {
        Self {
            binding,
            pending: VecDeque::new(),
            available: true,
        }
    }

    /// Queues the typed invocation when the action is currently available.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` cannot be encoded against the action's
    /// declared parameter contract.
    pub fn emit(&mut self, input: I) -> Result<(), tilo_actions::ActionBindingError> {
        if self.available {
            let invocation = self.binding.invoke(input)?;
            if self.pending.len() >= ACTION_EMITTER_QUEUE_LIMIT {
                self.pending.pop_front();
                tracing::warn!(
                    action = ?self.binding.address(),
                    limit = ACTION_EMITTER_QUEUE_LIMIT,
                    "control invocation backlog full; evicting oldest intent"
                );
            }
            self.pending.push_back(invocation);
        }
        Ok(())
    }

    pub fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.pending.pop_front()
    }

    pub fn binding(&self) -> &tilo_actions::ActionBinding<I> {
        &self.binding
    }

    /// Refresh whether the bound action is in the host's current,
    /// permission-filtered resource surface.
    pub fn refresh_availability(&mut self, allowed: &HashSet<tilo_actions::ActionAddress>) {
        self.available = allowed.contains(self.binding.address());
        if !self.available {
            self.pending.clear();
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Inventory entry for a custom control using this emitter.
    pub fn metadata(&self, control_id: impl Into<String>) -> ControlBindingMetadata {
        ControlBindingMetadata {
            control_id: control_id.into(),
            kind: ControlBindingKind::Action(self.binding.address().clone()),
        }
    }
}

#[cfg(test)]
pub(crate) fn test_bool_action_binding() -> tilo_actions::ActionBinding<bool> {
    use tilo_actions::{
        ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    };
    let spec = ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_bool",
        vec![ActionParam::required("value", ParamType::Boolean, "Value")],
        |args| EncodedAction::encode_operation("test.set_bool", &args.boolean("value")).unwrap(),
    );
    let resource = Resource::new("test:control", "test", "Test").with_action(spec.clone());
    tilo_actions::ActionBinding::new(&resource, &spec, |value| {
        serde_json::json!({"value": value})
            .as_object()
            .unwrap()
            .clone()
    })
    .unwrap()
}

#[cfg(test)]
pub(crate) fn test_string_action_binding() -> tilo_actions::ActionBinding<String> {
    use tilo_actions::{
        ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    };
    let spec = ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_string",
        vec![ActionParam::required("value", ParamType::Text, "Value")],
        |args| EncodedAction::encode_operation("test.set_string", &args.text("value")).unwrap(),
    );
    let resource = Resource::new("test:control", "test", "Test").with_action(spec.clone());
    tilo_actions::ActionBinding::new(&resource, &spec, |value| {
        serde_json::json!({"value": value})
            .as_object()
            .unwrap()
            .clone()
    })
    .unwrap()
}

#[cfg(test)]
pub(crate) fn test_number_action_binding() -> tilo_actions::ActionBinding<f64> {
    use tilo_actions::{
        ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
    };
    let spec = ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_number",
        vec![ActionParam::required("value", ParamType::Number, "Value")],
        |args| EncodedAction::encode_operation("test.set_number", &args.number("value")).unwrap(),
    );
    let resource = Resource::new("test:control", "test", "Test").with_action(spec.clone());
    tilo_actions::ActionBinding::new(&resource, &spec, |value| {
        serde_json::json!({"value": value})
            .as_object()
            .unwrap()
            .clone()
    })
    .unwrap()
}

/// Optional cross-device continuation capability for an app root.
///
/// Continuation is deliberately separate from [`Widget`]: ordinary controls
/// have no resumable app state, while roots that do can expose this capability
/// through [`Widget::continuable`] without weakening their UI integration.
pub trait Continuable {
    /// Return a compact, path-free projection of state that may be resumed on
    /// another device owned by the same person.
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<crate::collaboration::ContinuationMetadata>;

    /// Apply a continuation projection after launch. Implementations must not
    /// change focus or start media; malformed or unknown metadata is ignored.
    fn restore_continuation_metadata(
        &mut self,
        metadata: &crate::collaboration::ContinuationMetadata,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool;
}

/// Optional session collaboration capability for widgets that own live
/// broker-backed activity.
pub trait Collaborative {
    /// Drain session-local requests for the authenticated collaboration host.
    fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
        Vec::new()
    }

    /// Receive one session-local broker event.
    fn handle_collab_event(&mut self, _event: &crate::collaboration::CollabEvent) {}
}

/// Optional lifecycle for widgets backed by a platform-native child view.
pub trait NativeViewLifecycle {
    /// Attach the native child after the host window exists.
    fn mount_native_view(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>);

    /// Tear the native child down while its parent window is still alive.
    fn unmount_native_view(&mut self);

    /// Hide the native child at the start of a frame until drawing reveals it.
    fn native_view_frame_start(&mut self);
}

/// Optional capture and detachable-surface capability.
pub trait FloatingSurfaceProvider {
    /// One-shot request to hand app content to the shared floating-surface
    /// host. Shell and in-app hosts consume the same description, so `PiP` does
    /// not require a second rendering/widget API.
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        None
    }

    /// One-shot request for the session shell's shared capture/`PiP` chooser.
    fn take_capture_studio_request(&mut self) -> Option<CaptureMode> {
        None
    }

    /// Rising-edge source-geometry updates for an adopted floating surface.
    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
        Vec::new()
    }

    /// Paint app-owned content whose presentation has moved into shell space.
    fn draw_floating_surfaces(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut crate::text_render::TextRenderer,
        _theme: &crate::theme::Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
    }

    /// Desktop-space presentations that need a native secondary window.
    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        Vec::new()
    }

    /// Stable IDs whose underlying activity still exists.
    fn active_floating_surface_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// Push the shell's authoritative combined-output bounds into the owner.
    fn set_floating_surface_bounds(&mut self, _bounds: vello::kurbo::Rect) {}

    /// Deliver an acknowledged shell lifecycle edge back to the owner.
    fn on_floating_surface_host_message(
        &mut self,
        _message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
    }
}

/// Optional second-output presentation capability for app roots.
pub trait AudiencePresentationProvider {
    /// Request a real audience toplevel on a different output.
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
        None
    }

    /// Report whether the audience toplevel is currently live.
    fn on_audience_presentation_changed(&mut self, _active: bool) {}

    /// Paint the audience-only projection of the app's existing state.
    fn draw_audience_presentation(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _rect: vello::kurbo::Rect,
    ) -> bool {
        false
    }

    /// Route controls originating on the audience surface.
    fn handle_audience_presentation_event(
        &mut self,
        _event: &WindowEvent,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        false
    }

    /// Treat closing the audience toplevel as an explicit stop request.
    fn on_audience_presentation_closed(&mut self) {}
}

/// Optional OS-fullscreen capability for roots and forwarding containers.
pub trait FullscreenProvider {
    /// `None` means no opinion; otherwise this is a level-triggered intent.
    fn fullscreen_intent(&self) -> Option<bool> {
        None
    }

    /// Select the output and mirrored-vs-distinct presentation mode.
    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        crate::fullscreen::FullscreenRequest::mirror(crate::fullscreen::FullscreenTarget::Largest)
    }

    /// Report an actual OS fullscreen transition, including compositor exits.
    fn on_fullscreen_changed(&mut self, _active: bool) {}
}

/// Optional full-document/canvas capture capability.
pub trait CompleteContentProvider {
    /// Semantic canvas size available without resizing the live widget.
    fn complete_content_size(&self) -> Option<vello::kurbo::Size>;

    /// Paint the complete semantic content without mutating live view state.
    fn draw_complete_content(
        &mut self,
        scene: &mut vello::Scene,
        bounds: vello::kurbo::Rect,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool;
}

/// Host services associated with one retained widget event dispatch.
pub struct EventCtx<'a> {
    pub scale_factor: f64,
    pub modifiers: &'a winit::event::Modifiers,
    pub state: &'a mut AutoCommit,
    pub path: &'a ObjId,
    pub handled: &'a mut bool,
    pub focus_target: &'a mut Option<NodeId>,
    pub node_id: NodeId,
    pub taffy: &'a TaffyTree<()>,
}

impl EventCtx<'_> {
    /// Reborrow this event for a retained child with its own document path and node.
    pub fn reborrow<'b>(&'b mut self, path: &'b ObjId, node_id: NodeId) -> EventCtx<'b> {
        EventCtx {
            scale_factor: self.scale_factor,
            modifiers: self.modifiers,
            state: &mut *self.state,
            path,
            handled: &mut *self.handled,
            focus_target: &mut *self.focus_target,
            node_id,
            taffy: self.taffy,
        }
    }
}

pub trait Widget {
    /// App-declared external document obligations. An in-memory RecordSet is
    /// not evidence that an editable file has been saved.
    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        self.children()
            .into_iter()
            .flat_map(|child| child.unsaved_documents(doc))
            .collect()
    }
    /// Non-destructive persistence checkpoint. Never closes a wake scope or
    /// removes a tree; a failed host close must leave every child usable.
    fn checkpoint_components(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        errors.extend(
            self.children_mut()
                .into_iter()
                .flat_map(Widget::checkpoint_components),
        );
        errors
    }
    /// Only the host may call this after every checkpoint succeeded, without
    /// dispatching another document mutation in between.
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(Widget::finish_admitted_close)
            .collect()
    }

    /// Transitional durable writer owned by this root rather than the host.
    /// `None` means no such writer exists. Implementations must wait for all
    /// pending writes and report failure; a queued flush is not success.
    fn try_flush_owned_persistence(&mut self) -> Option<Result<(), String>> {
        None
    }

    /// Local control intent queued behind asynchronous startup preparation.
    /// This is a scheduling hint only; the host still checks live authority.
    fn has_pending_control_invocation(&self) -> bool {
        self.children()
            .iter()
            .any(|child| child.has_pending_control_invocation())
    }
    /// Owning lifecycle path: capture and close nested components before removing
    /// their live trees, transferring all recovery reports to the caller.
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(Widget::close_components)
            .collect()
    }
    /// Drain inert close-report ownership once; never invoke ordinary app callbacks
    /// through a closed component. Bound owners override this to use cached metadata.
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.children_mut()
            .into_iter()
            .flat_map(Widget::take_component_close_reports)
            .collect()
    }

    /// Runtime binding inherited by descendants in a matching layout topology.
    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        None
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
        for child in self.children_mut() {
            if request.target.is_some() && request.found {
                break;
            }
            child.visit_components(request)?;
        }
        Ok(())
    }

    /// Expose session collaboration only for widgets that own it.
    fn collaborative_mut(&mut self) -> Option<&mut dyn Collaborative> {
        None
    }

    /// Expose platform-native child lifecycle only for widgets that own it.
    fn native_view_lifecycle_mut(&mut self) -> Option<&mut dyn NativeViewLifecycle> {
        None
    }

    /// Expose detachable/capture surfaces only for widgets that own them.
    fn floating_surface_provider(&self) -> Option<&dyn FloatingSurfaceProvider> {
        None
    }

    /// Mutable counterpart to [`Widget::floating_surface_provider`].
    fn floating_surface_provider_mut(&mut self) -> Option<&mut dyn FloatingSurfaceProvider> {
        None
    }

    /// Expose second-output presentation only for app roots that own it.
    fn audience_presentation_provider(&self) -> Option<&dyn AudiencePresentationProvider> {
        None
    }

    /// Mutable counterpart to [`Widget::audience_presentation_provider`].
    fn audience_presentation_provider_mut(
        &mut self,
    ) -> Option<&mut dyn AudiencePresentationProvider> {
        None
    }

    /// Expose OS fullscreen behavior only for widgets that own or forward it.
    fn fullscreen_provider(&self) -> Option<&dyn FullscreenProvider> {
        None
    }

    /// Mutable counterpart to [`Widget::fullscreen_provider`].
    fn fullscreen_provider_mut(&mut self) -> Option<&mut dyn FullscreenProvider> {
        None
    }

    /// Expose full-content capture only for widgets that can reproduce it.
    fn complete_content_provider(&self) -> Option<&dyn CompleteContentProvider> {
        None
    }

    /// Mutable counterpart to [`Widget::complete_content_provider`].
    fn complete_content_provider_mut(&mut self) -> Option<&mut dyn CompleteContentProvider> {
        None
    }

    /// Receive host-selected platform conventions. The default propagates to
    /// retained children, keeping target-OS branches out of app code.
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        for child in self.children_mut() {
            child.set_platform_conventions(conventions);
        }
    }

    /// Receive the host's current input capabilities. The default propagates
    /// through retained containers, so app code does not need device branches.
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        for child in self.children_mut() {
            child.set_interaction_environment(environment);
        }
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId);

    /// Expose cross-device continuation for app roots that support it.
    fn continuable(&self) -> Option<&dyn Continuable> {
        None
    }

    /// Mutable counterpart to [`Widget::continuable`].
    fn continuable_mut(&mut self) -> Option<&mut dyn Continuable> {
        None
    }
    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        false
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId;
    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size::ZERO
    }
    /// Adjust this node's style using its allocated size, before final geometry
    /// is applied. Return true only when the style changed. Do not replace
    /// children or mutate widget structure here. The shared host solver repeats
    /// layout at most eight times and skips a frame that does not settle.
    /// Overrides affect only this node; the host visits every registered widget.
    fn adjust_layout_style(&mut self, _taffy: &mut TaffyTree<()>, _node: NodeId) -> bool {
        false
    }

    /// Read-only resolved decisions for widgets with an explicit layout policy.
    /// `None` means the widget has no such policy; ordinary manual widgets remain
    /// valid. Values are available after build/settlement and carry real origins.
    fn layout_info(&self) -> Option<LayoutInfo> {
        None
    }

    /// Renderer-aware prepaint policy adjustment. The default preserves the
    /// existing self-node-only `adjust_layout_style` contract. An override may
    /// measure its owned children through ordinary references, entering their
    /// callback binding when present. Never reenter descendants through the raw
    /// node map while borrowing their parent, change topology, or build another
    /// layout tree. Change only this node's style; inherited decisions may be
    /// retained for child hooks. Return true if styles or those decisions change.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        self.adjust_layout_style(taffy, node)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point);
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    );

    /// Compatibility dispatcher for retained containers and host code.
    ///
    /// New widgets implement [`Widget::handle_event_ctx`]. Existing positional
    /// implementations remain valid while call sites migrate independently.
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
        self.handle_event_ctx(
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

    /// Handle one input event with the runtime services for this retained node.
    fn handle_event_ctx(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {}
    fn children(&self) -> Vec<&dyn Widget> {
        vec![]
    }
    /// Allocation-free read-only child traversal. Existing custom containers
    /// inherit a compatibility adapter over [`Self::children`]; toolkit
    /// containers override this to walk their retained storage directly.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in self.children() {
            visitor(child);
        }
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![]
    }

    /// Effect/view operation type ids this widget can actually dispatch.
    ///
    /// Implementors that override `invoke_resource_operation(_v2)` must list
    /// their locally handled types here. The host combines these declarations
    /// across the retained tree and refuses to publish an action whose type has
    /// no handler, preventing a catalogue entry that can only fail at runtime.
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[]
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = self
            .local_resource_operation_handlers()
            .iter()
            .map(|handler| (*handler).to_string())
            .collect::<HashSet<_>>();
        self.for_each_child(&mut |child| handlers.extend(child.resource_operation_handlers()));
        handlers
    }
    fn name(&self) -> &str;
    /// Stable id for the e2e test harness; `None` falls back to a path-based
    /// key. Must be unique across the live tree — the registry resolves on
    /// first match, so a duplicate shadows the second occurrence.
    fn test_id(&self) -> Option<&str> {
        None
    }

    /// Lets a screenshot harness hold an animation at a chosen millisecond
    /// and render the real `draw` at that instant. Default `None`; a widget
    /// opts in with `Some(self)`.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Reveal a newly keyboard-focused descendant inside any enclosing
    /// viewport. Containers override this to scroll; ordinary widgets simply
    /// recurse toward the target and report whether it was found.
    fn reveal_descendant(&mut self, target: *const dyn Widget) -> bool {
        self.children_mut().into_iter().any(|child| {
            std::ptr::addr_eq(std::ptr::from_ref::<dyn Widget>(child), target)
                || child.reveal_descendant(target)
        })
    }

    /// Named hit rects for widgets that paint several affordances into one
    /// rect instead of composing children. Same space as
    /// [`Widget::get_rect`]; zero-area entries are dropped.
    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
        Vec::new()
    }

    fn get_rect(&self) -> vello::kurbo::Rect;

    /// Absolute logical clip applied to ordinary children during painting and
    /// pointer routing. Inspectors and enclosing scroll regions must respect
    /// this boundary instead of treating clipped descendants as visible extent.
    fn child_clip_rect(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// One-shot request to open the host's action palette directly at a typed
    /// resource action. Parameter collection and permission checks remain host
    /// policy; widgets only name an address already present in `resources()`.
    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.children_mut()
            .into_iter()
            .find_map(Widget::take_action_palette_request)
    }

    /// `point` is logical coords, same space as [`Widget::get_rect`];
    /// `None` defers to the default arrow. The host hit-tests the whole
    /// tree via `node_map` and picks the smallest-area claim, so containers
    /// need not override this.
    fn cursor_shape(&self, _point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        None
    }

    /// Revealed after a dwell over the widget's rect, via the toolkit's one
    /// tooltip mechanism ([`crate::widget::tooltip::TooltipHost`]),
    /// hit-tested like [`cursor_shape`](Self::cursor_shape) so containers
    /// need not forward it. An icon-only control should also use this as
    /// its accessible name.
    fn tooltip(&self) -> Option<&str> {
        None
    }

    fn tooltip_target_at(
        &self,
        point: vello::kurbo::Point,
    ) -> Option<(vello::kurbo::Rect, String)> {
        let rect = self.get_rect();
        if !rect.contains(point) {
            return None;
        }
        self.tooltip()
            .filter(|tooltip| !tooltip.is_empty())
            .map(|tooltip| (rect, tooltip.to_owned()))
    }

    fn is_focusable(&self) -> bool {
        false
    }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool {
        false
    }

    /// Whether a focused widget owns Tab instead of the host's focus
    /// traversal. This is deliberately opt-in: ordinary form fields and
    /// controls retain platform-standard Tab/Shift+Tab navigation, while a
    /// code editor or terminal can receive the key for indentation or
    /// completion.
    fn wants_tab_input(&self) -> bool {
        false
    }

    /// Caret rectangle for the platform input method, in window-logical
    /// coordinates. Returning `Some` enables IME for the focused widget and
    /// positions its candidate window; non-text controls keep the default
    /// `None` and therefore disable IME.
    fn ime_cursor_rect(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// While any widget answers `true`, the host restricts Tab/directional
    /// focus to this subtree (wrapping at the ends) and restores the prior
    /// focus once it goes back to `false`. Default `false`.
    fn traps_focus(&self) -> bool {
        false
    }

    /// Role, name/description/value, and
    /// [`AccessState`](crate::a11y::AccessState). Default `None` means
    /// presentational: the node is skipped and children reparent onto the
    /// parent. State must be **derived, not stored** so it can't drift from
    /// `draw`; names must already be [`i18n!`](crate::i18n)-resolved.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        None
    }

    /// Geometry for an immediate-mode semantic child from
    /// [`AccessNode::children`](crate::a11y::AccessNode::children). The path is
    /// its child-index path beneath this widget's accessibility node. Returning
    /// a rect lets AccessKit activation inject the same click as pointer input.
    fn synthetic_accessibility_rect(&self, _path: &[usize]) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Move this widget's own immediate-mode focus to a synthetic semantic
    /// child. Retained widgets continue to use the ordinary taffy focus path.
    fn focus_synthetic_accessibility(&mut self, _path: &[usize]) -> bool {
        false
    }

    /// Reparent one retained [`children`](Widget::children) entry beneath a
    /// synthetic child returned by [`accessibility`](Widget::accessibility).
    /// The returned index addresses `AccessNode::children` for this frame.
    /// Collections use this to place a real interactive row delegate inside
    /// its virtual `ListItem`/`TreeItem` instead of exporting a duplicate.
    fn accessibility_child_parent(&self, _child_index: usize) -> Option<usize> {
        None
    }

    /// Reparent one retained [`children`](Widget::children) entry beneath a
    /// nested path of synthetic children returned by
    /// [`accessibility`](Widget::accessibility).
    ///
    /// The default preserves the one-level
    /// [`accessibility_child_parent`](Widget::accessibility_child_parent)
    /// contract. Tables override this to place a real cell control beneath
    /// its synthetic row and cell, for example `[visible_row, column]`.
    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        self.accessibility_child_parent(child_index)
            .map(|parent| vec![parent])
    }

    /// May mutate the CRDT like [`handle_event`](Widget::handle_event).
    /// Default `false` ("not handled") falls back to generic dispatch
    /// (`Focus` moves keyboard focus, `Activate` synthesises a click);
    /// widgets with richer semantics (slider inc/dec, combo
    /// expand/collapse) override to handle those verbs precisely.
    fn perform_action(
        &mut self,
        _action: &crate::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        false
    }

    /// Perform an accessibility [`Action`](crate::a11y::Action) aimed at one of
    /// this widget's **synthetic** accessibility nodes — the routed counterpart
    /// to [`perform_action`](Widget::perform_action).
    ///
    /// A widget that paints its own contents (a timeline of clips, a
    /// spreadsheet grid, a table of rows) has no child widgets to receive an
    /// action, so it describes its structure through
    /// [`AccessNode::children`](crate::a11y::AccessNode::children) instead. When
    /// an assistive technology activates one of those nodes, the host resolves
    /// the node's id back to `(this widget, key path)` and calls this method, so
    /// the widget knows **which** element the user meant.
    ///
    /// `key_path` names the target relative to this widget: one element per
    /// level of [`AccessNode::children`](crate::a11y::AccessNode::children)
    /// nesting — `[row]` for a table row, `[row, cell]` for a cell inside it.
    /// Each element is the node's [`key`](crate::a11y::AccessNode::key), or its
    /// ordinal among its siblings when it declared none. An **empty** path means
    /// the action targets the widget itself.
    ///
    /// For an element keyed by identity rather than position, compare the
    /// incoming key against [`hash_key`](crate::a11y::hash_key) of your own id:
    ///
    /// ```ignore
    /// fn perform_action_at(&mut self, action: &Action, key_path: &[u64], ..) -> bool {
    ///     let Some(&clip_key) = key_path.first() else { return false };
    ///     let Some(clip) = self.clips.iter().find(|c| hash_key(&c.id) == clip_key) else {
    ///         return false;   // stale reference — do nothing, never guess
    ///     };
    ///     ...
    /// }
    /// ```
    ///
    /// Returns `true` if the widget consumed the action. The **default forwards
    /// to [`perform_action`](Widget::perform_action)**, ignoring the path — so
    /// every existing widget keeps behaving exactly as it did, and a widget with
    /// no synthetic children (where the path is always empty) need not override
    /// this at all. If a widget declines, the host falls back to its generic
    /// dispatch just as it does for `perform_action`; note that the fallback is
    /// widget-granular (it moves focus to, or clicks the centre of, the whole
    /// widget), so a canvas that wants per-element operation must implement this.
    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        let _ = key_path;
        self.perform_action(action, state, path)
    }

    /// Handle an action addressed to a synthetic accessibility child created
    /// by this widget. `target` is the opaque stable value supplied through
    /// [`AccessNode::with_action_target`](crate::a11y::AccessNode::with_action_target).
    ///
    /// Custom canvases using
    /// [`AccessNode::with_keyed_action_routes`](crate::a11y::AccessNode::with_keyed_action_routes)
    /// do not need to override this method: the default decodes the published
    /// semantic target and forwards it to [`Widget::perform_action_at`].
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        // `AccessNode::with_keyed_action_routes` publishes a custom-painted
        // control through the same semantic target that search and agents
        // consume. Decode that target here, at the Widget boundary, so the
        // app author implements `perform_action_at` once and gets every input
        // route promised by the adapter. Explicit semantic targets keep using
        // their widget-specific implementations because they do not carry
        // this reserved prefix.
        let Some((_, encoded_path)) = target
            .strip_prefix("tilo.key-action:")
            .and_then(|target| target.rsplit_once(':'))
        else {
            return false;
        };
        let Ok(key_path) = encoded_path
            .split('/')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
        else {
            return false;
        };
        if key_path.is_empty() {
            return false;
        }
        self.perform_action_at(action, &key_path, state, path)
    }

    /// A disabled (`false`) widget is excluded from focus traversal, must
    /// reject input in `handle_event`, and renders dimmed (see
    /// [`DISABLED_ALPHA`]).
    fn is_enabled(&self) -> bool {
        true
    }

    /// The focused widget wants platform text input (Wayland
    /// `text-input-v3`, native IMEs elsewhere). `None` is intentionally the
    /// default: enabling an IME for buttons, canvas tools or games makes the
    /// compositor raise an on-screen keyboard over content that cannot accept
    /// the committed text.
    ///
    /// Winit currently exposes the protocol's normal, password and terminal
    /// purposes. Widgets should return the closest truthful value; the
    /// compositor still receives the focus/enable/cursor lifecycle even when
    /// a more specialised purpose (email, URL, numeric) is unavailable at the
    /// toolkit boundary.
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        None
    }

    /// Default `true`; the exception is a widget standing in for a process
    /// that has started but not drawn. Must **latch**: once `true`, never
    /// `false` again for the same content, or an un-presenting widget would
    /// make a window vanish mid-resize. What shows meanwhile is decided by
    /// [`Widget::opening`].
    fn presented(&self) -> bool {
        true
    }
    /// True once waiting long enough, while not yet
    /// [`presented`](Widget::presented), that showing nothing would read as
    /// broken. Delayed so a fast open never flashes an indicator.
    fn opening(&self) -> bool {
        false
    }
    fn needs_commit(&self) -> bool {
        let mut needed = false;
        self.for_each_child(&mut |child| {
            if !needed {
                needed = child.needs_commit();
            }
        });
        needed
    }
    /// Drain semantic provenance for the native transaction currently asking
    /// the host to commit. Use this for CRDT-native edits such as flowing text;
    /// replayable discrete operations should continue through `ActionHost`.
    fn take_commit_context(&mut self) -> Option<tilo_actions::ActionContext> {
        self.children_mut()
            .into_iter()
            .find_map(Widget::take_commit_context)
    }
    /// Drain one queued action-bound control intent from this subtree.
    /// Custom-painted widgets can own an [`ActionEmitter`] and override this
    /// method; they do not need to use the toolkit's [`Button`].
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.children_mut()
            .into_iter()
            .find_map(Widget::take_control_invocation)
    }
    /// Report the admission result of one queued local control invocation.
    /// `Accepted` means queued, not completed; confirmation remains an error
    /// while the host offers its normal confirmation UI. This notification
    /// grants no document authority and must not retry the invocation itself.
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        for child in self.children_mut() {
            child.on_control_invocation_result(address, result);
        }
    }
    /// Recursively inventory action-native and explicitly legacy controls.
    /// Custom widgets that own an [`ActionEmitter`] should prepend its
    /// [`metadata`](ActionEmitter::metadata) to their descendants' inventory.
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let mut bindings = semantic_action_bindings(self.accessibility());
        self.for_each_child(&mut |child| bindings.extend(child.control_action_bindings()));
        bindings
    }
    /// Push the fresh local permission-filtered action surface into every
    /// retained control. Custom widgets should refresh their own emitter and
    /// then recurse into children.
    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        for child in self.children_mut() {
            child.refresh_control_action_availability(allowed);
        }
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            child.on_commit();
        }
    }
    fn get_state(&self) -> Option<String> {
        None
    }
    fn set_state(&mut self, _state: &str) {}

    /// `(caret_byte, text)`, byte offset on a char boundary. The host diffs
    /// this before/after an event so caret-only moves (arrows, Home/End)
    /// get announced, not just typing.
    fn text_review(&self) -> Option<(usize, String)> {
        None
    }

    /// Delivers an out-of-band string payload from
    /// [`crate::app::UserEvent::External`] (e.g. a worker thread streaming
    /// output into a status log). Default no-op; the App redraws and
    /// re-syncs after dispatch, so mutating state here is enough.
    fn on_external_event(&mut self, _payload: &str, _state: &mut AutoCommit, _root: &ObjId) {}

    /// Global back/forward gesture (swipe/edge-pull), recognised by
    /// [`crate::gesture::NavGestureRecognizer`] and delivered to the whole
    /// tree; return `true` once consumed. Default forwards to children, so
    /// a widget that owns history becomes swipe-navigable for free.
    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        for child in self.children_mut() {
            if child.on_nav_gesture(dir, state, path) {
                return true;
            }
        }
        false
    }

    /// One-shot (reading MUST clear it) signal to move focus to this
    /// subtree's first focusable widget after the next rebuild. Only
    /// consulted under [`crate::app::App::autofocus_on_structure_change`];
    /// a wizard raises it on step transitions.
    fn take_focus_reset(&mut self) -> bool {
        false
    }

    /// Which **record** (see [`crate::record`]) this widget's document
    /// should be, so opening a second file checks out a different document
    /// instead of overwriting the first. Root widget only; `None` keeps the
    /// default package-level document.
    fn record_intent(&self) -> Option<crate::record::RecordCheckout> {
        None
    }

    /// Presentation fields (scroll offset, view mode) contributed to the
    /// App's [`View`](crate::view::View), restored via
    /// [`restore_view_state`](Self::restore_view_state). Distinct from
    /// [`get_state`](Self::get_state) (the *focused* widget's
    /// caret/selection): this is root-owned, usually not the focused
    /// widget's.
    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        None
    }

    fn restore_view_state(&mut self, _view: &crate::view::ViewContribution) {}

    /// Theme/high-contrast/reduced-motion/scale, polled under
    /// [`crate::app::App::appearance_reconcile`] — how a settings pane
    /// re-themes the running app live without a back-channel.
    /// Level-triggered; the App recurses [`Self::children`] itself, so
    /// containers need not forward it. First `Some` wins.
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        None
    }

    /// Apply the effective reduced-motion preference before the host asks this
    /// subtree whether it needs another animation frame. The default forwards
    /// recursively, making the preference a framework policy instead of an
    /// optional convention that every container has to remember to implement.
    ///
    /// Custom-painted roots with app-local animation state should override
    /// this, settle any in-flight decorative transitions when `reduced` is
    /// true, and then forward to retained child widgets if they have any.
    fn set_motion_reduced(&mut self, reduced: bool) {
        for child in self.children_mut() {
            child.set_motion_reduced(reduced);
        }
    }

    /// Inverse of [`Self::record_intent`]. A widget that publishes
    /// presence **must** fold this into its presence key — two instances
    /// of one app are two records, and a key that only names the field
    /// collides between them, so a peer's caret lands in whichever
    /// instance drew last.
    fn set_record_scope(&mut self, _record: &crate::record::RecordId) {}

    // --- Collab / DnD contract extensions ---
    /// Stable identity for presence/permission lookup. Default: opt-out.
    fn presence_key(&self) -> Option<PresenceKey> {
        None
    }
    /// Outbound presence (typically caret/selection) broadcast while this
    /// widget holds focus; the App polls the focused widget after each event
    /// and diffs it out to peers.
    fn presence_payload(&self) -> Option<(PresenceKey, crate::collaboration::PresencePayload)> {
        None
    }
    /// Whether `sync` still needs to run on idle frames without input or
    /// collab events — e.g. waiting out a document-adoption grace window.
    fn needs_idle_sync(&self) -> bool {
        false
    }
    /// Which permission scope, if any, gates this widget. Default: no gating.
    fn permission_scope(&self) -> Option<PermissionScope> {
        None
    }

    /// The tree of [`tilo_actions::Resource`]s this widget currently has
    /// open, each with the actions it exports to Ctrl+K — e.g. a playing
    /// video plus its comments. `doc` lets a provider enumerate resources
    /// living in the CRDT.
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        Vec::new()
    }

    /// One cursor-addressable page of resource roots safe to expose through
    /// the broker pull directory. `max_results` is a hard ceiling; a live
    /// provider must return a non-empty opaque cursor when it has a tail.
    /// `None` is intentionally the default: a legacy
    /// [`Self::resources`] implementation may build an unbounded contextual
    /// snapshot, which must not be reached from a remote query or revision.
    fn action_directory_resource_page(
        &self,
        _doc: &AutoCommit,
        _cursor: &[u8],
        _max_results: usize,
    ) -> Option<ActionDirectoryResourcePage> {
        None
    }

    /// Revision of non-CRDT bounded directory roots. Return `None` when this
    /// widget has no independently mutable remote directory state; callers
    /// combine provided values with document heads and search generations.
    fn action_directory_revision(&self, _doc: &AutoCommit) -> Option<String> {
        None
    }

    /// Resolve an addressable resource omitted from the contextual snapshot.
    /// Large apps override this through their `ResourceProvider` index.
    fn resource(&self, _doc: &AutoCommit, _id: &str) -> Option<tilo_actions::Resource> {
        None
    }

    /// Explicit disclosure authority for an actionless resource container in
    /// the broker pull directory. Returning `None` means its label and
    /// hierarchy are not remotely discoverable unless the resource itself has
    /// a safe view action. This is intentionally separate from `resources`:
    /// selecting an arbitrary mutating action as a visibility proxy could
    /// leak a container name to someone who may only perform that action in a
    /// different context.
    fn action_directory_visibility(
        &self,
        _doc: &AutoCommit,
        _resource_id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        None
    }

    /// Folded into the unified Ctrl+K palette alongside
    /// [`resources`](Self::resources) actions — one
    /// [`SearchProvider`](crate::search::SearchProvider) per findable kind.
    /// A hit activates via
    /// [`activate_search_result`](Self::activate_search_result).
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        Vec::new()
    }

    /// Presentation (navigation), not a CRDT action — mutate the doc only
    /// if selecting changes shared state.
    fn activate_search_result(&mut self, _action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {}

    /// The affordance-driven twin of the Ctrl+K palette: when a widget's own
    /// control dispatches the same [`tilo_actions`] action its
    /// [`ActionSpec`](tilo_actions::ActionSpec) exports, it reports the
    /// document heads captured **just before** here, so the host can record
    /// one undo boundary the same way a palette dispatch does. Rising-edge:
    /// reading MUST clear it.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        None
    }

    /// Host → widget: the live state of the undo history, published once per
    /// frame to every laid-out widget.
    ///
    /// A widget only ever receives `&mut AutoCommit`, which carries the
    /// document but not its history — so without this a control could not tell
    /// whether there was anything to undo, and every app was reduced to
    /// printing the shortcut and hoping. Store the status (embed an
    /// [`UndoAccess`](crate::undo::UndoAccess)) and read it back in `draw`, in
    /// [`is_enabled`](Widget::is_enabled), and in
    /// [`accessibility`](Widget::accessibility) — which only gets `&self`, and
    /// is exactly where the disabled state must be reflected.
    ///
    /// The host sweeps its `node_map`, so this reaches a widget at any depth
    /// with no container forwarding. Default: ignore it.
    fn set_undo_status(&mut self, _status: &crate::undo::UndoStatus) {}

    /// Widget → host: an undo or redo the user asked for.
    ///
    /// The host drains this after each event and performs it through
    /// [`App::perform_undo`](crate::app::App::perform_undo) /
    /// [`App::perform_redo`](crate::app::App::perform_redo) — the same methods
    /// the Ctrl+Z key handler calls, so a button and the shortcut are one
    /// implementation and cannot drift.
    ///
    /// Rising-edge: reading MUST clear it, so one activation is one undo.
    /// Default: nothing requested.
    fn take_undo_request(&mut self) -> Option<crate::undo::UndoRequest> {
        None
    }

    /// Offer a host keyboard undo/redo shortcut to focused widget-local
    /// history before the application document consumes it.
    ///
    /// The default recursively asks retained children. A component with its
    /// own editing history should return `true` only while focus is inside
    /// that component and it has consumed the request. Returning `false`
    /// preserves the standard collaboration-safe document undo behavior.
    fn handle_local_undo_request(&mut self, request: crate::undo::UndoRequest) -> bool {
        self.children_mut()
            .into_iter()
            .any(|child| child.handle_local_undo_request(request))
    }

    /// Apply an action another app in this room published, if this app owns
    /// the document — lets an app address a document it doesn't hold by
    /// saying what should happen instead of writing bytes directly, keeping
    /// each document to one writer. An unrecognised `type_id` is silently
    /// declined, not an error.
    fn apply_room_action(
        &mut self,
        _doc: &mut AutoCommit,
        _type_id: &str,
        _payload: &[u8],
    ) -> bool {
        false
    }

    /// Perform an exported non-document operation owned by this widget.
    /// Containers inherit recursive routing; an application overrides only
    /// for the effect/view types it publishes and returns `Ok(true)` when it
    /// handled the payload. Document operations never enter this hook.
    ///
    /// # Errors
    ///
    /// Returns the first error reported while routing the operation through
    /// the retained widget tree.
    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        for child in self.children_mut() {
            if child.invoke_resource_operation(kind, type_id, payload, doc, path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// V2 operation route with a typed result. Existing widgets inherit the
    /// legacy adapter; migrated operations override this method and return the
    /// output promised by their `ActionContract`.
    ///
    /// # Errors
    ///
    /// Returns an error when the legacy operation handler rejects the payload
    /// or fails while routing it through the retained widget tree.
    fn invoke_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        self.invoke_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| handled.then(tilo_actions::ActionOutput::default))
    }

    /// Admit an exported non-document operation with a stable invocation id.
    ///
    /// Existing synchronous handlers inherit this adapter. A background or
    /// streaming handler overrides it, records `invocation`, and returns
    /// [`ResourceOperationStart::Accepted`].
    ///
    /// # Errors
    ///
    /// Returns the first admission or synchronous-operation error reported
    /// while routing the payload through the retained widget tree.
    fn start_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        // Give descendants their lifecycle-aware route before falling back to
        // the v2 synchronous adapter. Without this pass, an async leaf nested
        // under an ordinary retained container would be downgraded to the
        // legacy immediate route.
        for child in self.children_mut() {
            if let Some(started) =
                child.start_resource_operation(invocation, kind, type_id, payload, doc, path)?
            {
                return Ok(Some(started));
            }
        }
        self.invoke_resource_operation_v2(kind, type_id, payload, doc, path)
            .map(|result| result.map(ResourceOperationStart::Completed))
    }

    /// Admit an operation that may need to commit a document follow-up after
    /// completing device I/O. The mutable document is deliberately available
    /// only at this top-level admission boundary: ordinary effect handlers
    /// inherit the read-only adapter above, while a compound operation can
    /// perform its effect and dispatch the exact replayable metadata action in
    /// one host-owned turn instead of publishing a permanently disabled UI.
    ///
    /// The default preserves a widget's existing read-only admission override.
    /// Transparent containers that host mutable handlers must forward this
    /// method to their children (as `Column`, `Row`, `Container` and `TileHost` do).
    /// They must not then call the read-only fallback: that would visit a
    /// declining handler twice. Custom containers retain this forwarding duty.
    ///
    /// # Errors
    ///
    /// Returns the same admission/operation failures as
    /// [`Self::start_resource_operation`].
    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        self.start_resource_operation(invocation, kind, type_id, payload, doc, path)
    }

    /// Poll one admitted operation without blocking the UI thread.
    ///
    /// `cancellation_requested` is a cooperative signal from the action
    /// broker. Cancellable handlers should stop at a safe boundary and return
    /// `Cancelled`; non-cancellable handlers may ignore it. The default
    /// recursively polls retained children.
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        for child in self.children_mut() {
            if let Some(update) =
                child.poll_resource_operation(invocation, cancellation_requested, doc, path)
            {
                return Some(update);
            }
        }
        None
    }

    fn accepts_drop(&self, _payload: &DragPayload) -> bool {
        false
    }
    fn on_drop(&mut self, _payload: DragPayload, _state: &mut AutoCommit, _path: &ObjId) {}
    fn wants_pointer_capture(&self) -> bool {
        false
    }

    /// Unlike [`wants_pointer_capture`], does not swallow misses — drawn
    /// and hit-tested first, so a small on-video affordance gets z-order
    /// priority without deadening the content under it. Default `false`.
    fn is_floating(&self) -> bool {
        false
    }
    /// `App` polls this in `Event::AboutToWait`; without it a curve would
    /// freeze partway until an unrelated event woke it again.
    fn needs_animation_frame(&self) -> bool {
        let mut needed = false;
        self.for_each_child(&mut |child| {
            if !needed {
                needed = child.needs_animation_frame();
            }
        });
        needed
    }

    /// Earliest wall-clock deadline at which this subtree needs a sync/redraw
    /// even if there is no input. Unlike `needs_animation_frame`, this keeps
    /// the event loop asleep until the requested instant and is appropriate
    /// for filesystem/status polling rather than animation.
    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut earliest = None;
        self.for_each_child(&mut |child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old: Instant| old.min(deadline)));
            }
        });
        earliest
    }

    /// `Some(rect)` when this subtree holds an *open* modal, its panel box.
    /// While any widget reports `Some`, the host suppresses its own global
    /// focus shortcuts and swallows unhandled pointer presses so nothing
    /// behind the modal is clickable. Containers MUST forward (the default
    /// does, via first `Some` among children).
    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        let mut capture = None;
        self.for_each_child(&mut |child| {
            if capture.is_none() {
                capture = child.modal_capture();
            }
        });
        capture
    }

    /// True if this subtree's structural composition (children
    /// inserted/removed) changed since the last frame; the App rebuilds the
    /// taffy tree when so. Default `false` is correct for leaves but WRONG
    /// for any container storing child widgets — MUST forward or the
    /// recursion breaks.
    fn take_structure_dirty(&mut self) -> bool {
        false
    }

    /// Work already requested by this view whose completion will replace a
    /// placeholder. Unlike animations, this can be awaited by still captures.
    fn has_pending_visual_work(&self) -> bool {
        let mut pending = false;
        self.for_each_child(&mut |child| pending |= child.has_pending_visual_work());
        pending
    }

    /// Default `true` (dirty every frame). Opt into incremental redraw by
    /// tracking a `dirty: bool`, setting it in `handle_event`/`sync`, and
    /// clearing it in `mark_drawn()`. Containers MUST forward: dirty iff any
    /// descendant is.
    fn needs_redraw(&self) -> bool {
        let mut has_children = false;
        let mut dirty = false;
        self.for_each_child(&mut |child| {
            has_children = true;
            if !dirty {
                dirty = child.needs_redraw();
            }
        });
        !has_children || dirty
    }

    /// Logical-pixel bounds whose visual output differs from the last frame.
    ///
    /// The conservative default covers this widget's complete layout box.
    /// Pure layout containers should override this and union descendant
    /// damage so a small animated leaf does not force a full-window raster.
    /// Callers add an anti-alias/effect safety margin and fall back to a full
    /// redraw for layout, structure, overlays, or invalid retained targets.
    fn collect_damage(&self, regions: &mut Vec<vello::kurbo::Rect>) {
        if self.needs_redraw() {
            regions.push(self.get_rect());
        }
    }

    /// Called after this widget's scene lands in the composed frame, to
    /// clear a `dirty` flag. Default forwards to children.
    fn mark_drawn(&mut self) {
        for c in self.children_mut() {
            c.mark_drawn();
        }
    }

    /// Release retained encoded-scene storage in this subtree without
    /// marking its visual/model state dirty. Outer caches call this after a
    /// successful reuse: while the outer display list is authoritative,
    /// keeping byte-for-byte descendant encodings only duplicates memory.
    /// A later outer rebuild causes descendants to record/cache normally.
    fn release_scene_cache(&mut self) {
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }

    /// Default `true`; widgets with a fixed intrinsic size should override
    /// to `false` unless a size-affecting mutation landed. Containers MUST
    /// forward: layout-dirty iff any descendant is.
    fn is_layout_dirty(&self) -> bool {
        let mut has_children = false;
        let mut dirty = false;
        self.for_each_child(&mut |child| {
            has_children = true;
            if !dirty {
                dirty = child.is_layout_dirty();
            }
        });
        !has_children || dirty
    }

    /// Whether a `layout_dirty` frame needs taffy to **re-solve** sizes
    /// (`compute_layout`) rather than just reposition already-solved boxes.
    /// [`Scrollable`] is the motivating override: it translates its origin
    /// in `apply_layout` without changing any taffy size, so re-solving on
    /// every scroll frame is pure waste.
    fn layout_needs_measure(&self) -> bool {
        let mut has_children = false;
        let mut dirty = false;
        self.for_each_child(&mut |child| {
            has_children = true;
            if !dirty {
                dirty = child.layout_needs_measure();
            }
        });
        if has_children {
            dirty
        } else {
            self.is_layout_dirty()
        }
    }

    /// Default forwards to `draw`. **Spatial culling**: short-circuits via
    /// [`PresenceCtx::should_cull`] when the rect misses
    /// `presence.viewport` — overrides MUST call it first. Off-viewport
    /// widgets stay dirty so they re-emit when scrolled back in.
    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    /// **Top-layer pass**, run once after the whole `draw_with_presence`
    /// recursion, so anything emitted here composites above every sibling
    /// regardless of tree order — the toolkit's overlay mechanism for
    /// content that escapes its layout box (an open [`Dropdown`], a
    /// popover). Default recurses `children_mut`.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }
}

/// Ergonomic forwarding API for optional floating-surface capabilities.
///
/// This keeps host/container call sites concise while the core [`Widget`]
/// contract contains only capability accessors.
pub trait FloatingSurfaceWidgetExt {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest>;
    fn take_capture_studio_request(&mut self) -> Option<CaptureMode>;
    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)>;
    fn draw_floating_surfaces(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    );
    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation>;
    fn active_floating_surface_ids(&self) -> Vec<String>;
    fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect);
    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    );
}

impl<T: Widget + ?Sized> FloatingSurfaceWidgetExt for T {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        self.floating_surface_provider_mut()
            .and_then(FloatingSurfaceProvider::take_floating_surface_request)
    }

    fn take_capture_studio_request(&mut self) -> Option<CaptureMode> {
        self.floating_surface_provider_mut()
            .and_then(FloatingSurfaceProvider::take_capture_studio_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
        self.floating_surface_provider_mut().map_or_else(
            Vec::new,
            FloatingSurfaceProvider::take_floating_surface_source_updates,
        )
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if let Some(provider) = self.floating_surface_provider_mut() {
            provider.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        self.floating_surface_provider().map_or_else(
            Vec::new,
            FloatingSurfaceProvider::floating_surface_presentations,
        )
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.floating_surface_provider().map_or_else(
            Vec::new,
            FloatingSurfaceProvider::active_floating_surface_ids,
        )
    }

    fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect) {
        if let Some(provider) = self.floating_surface_provider_mut() {
            provider.set_floating_surface_bounds(bounds);
        }
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        if let Some(provider) = self.floating_surface_provider_mut() {
            provider.on_floating_surface_host_message(message);
        }
    }
}

/// Ergonomic host API for optional second-output presentation support.
pub trait AudiencePresentationWidgetExt {
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest>;
    fn on_audience_presentation_changed(&mut self, active: bool);
    fn draw_audience_presentation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: vello::kurbo::Rect,
    ) -> bool;
    fn handle_audience_presentation_event(
        &mut self,
        event: &WindowEvent,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool;
    fn on_audience_presentation_closed(&mut self);
}

impl<T: Widget + ?Sized> AudiencePresentationWidgetExt for T {
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
        self.audience_presentation_provider()
            .and_then(AudiencePresentationProvider::audience_presentation_request)
    }

    fn on_audience_presentation_changed(&mut self, active: bool) {
        if let Some(provider) = self.audience_presentation_provider_mut() {
            provider.on_audience_presentation_changed(active);
        }
    }

    fn draw_audience_presentation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: vello::kurbo::Rect,
    ) -> bool {
        self.audience_presentation_provider_mut()
            .is_some_and(|provider| {
                provider.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
            })
    }

    fn handle_audience_presentation_event(
        &mut self,
        event: &WindowEvent,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        self.audience_presentation_provider_mut()
            .is_some_and(|provider| provider.handle_audience_presentation_event(event, state, path))
    }

    fn on_audience_presentation_closed(&mut self) {
        if let Some(provider) = self.audience_presentation_provider_mut() {
            provider.on_audience_presentation_closed();
        }
    }
}

/// Ergonomic host API for optional fullscreen support.
pub trait FullscreenWidgetExt {
    fn fullscreen_intent(&self) -> Option<bool>;
    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest;
    fn on_fullscreen_changed(&mut self, active: bool);
}

impl<T: Widget + ?Sized> FullscreenWidgetExt for T {
    fn fullscreen_intent(&self) -> Option<bool> {
        self.fullscreen_provider()
            .and_then(FullscreenProvider::fullscreen_intent)
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        self.fullscreen_provider().map_or_else(
            || {
                crate::fullscreen::FullscreenRequest::mirror(
                    crate::fullscreen::FullscreenTarget::Largest,
                )
            },
            FullscreenProvider::fullscreen_params,
        )
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        if let Some(provider) = self.fullscreen_provider_mut() {
            provider.on_fullscreen_changed(active);
        }
    }
}

/// Ergonomic host API for optional complete-content capture.
pub trait CompleteContentWidgetExt {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size>;
    fn draw_complete_content(
        &mut self,
        scene: &mut vello::Scene,
        bounds: vello::kurbo::Rect,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool;
}

impl<T: Widget + ?Sized> CompleteContentWidgetExt for T {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
        self.complete_content_provider()
            .and_then(CompleteContentProvider::complete_content_size)
    }

    fn draw_complete_content(
        &mut self,
        scene: &mut vello::Scene,
        bounds: vello::kurbo::Rect,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool {
        self.complete_content_provider_mut()
            .is_some_and(|provider| {
                provider.draw_complete_content(
                    scene,
                    bounds,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                )
            })
    }
}

/// Drain collaboration capabilities throughout a retained widget tree.
pub(crate) fn take_collab_requests(
    widget: &mut dyn Widget,
) -> Vec<crate::collaboration::CollabRequest> {
    let mut requests = widget
        .collaborative_mut()
        .map_or_else(Vec::new, Collaborative::take_collab_requests);
    for child in widget.children_mut() {
        requests.extend(take_collab_requests(child));
    }
    requests
}

/// Deliver a collaboration event to every explicit capability in the tree.
pub(crate) fn handle_collab_event(
    widget: &mut dyn Widget,
    event: &crate::collaboration::CollabEvent,
) {
    if let Some(collaborative) = widget.collaborative_mut() {
        collaborative.handle_collab_event(event);
    }
    for child in widget.children_mut() {
        handle_collab_event(child, event);
    }
}

/// Mount every native-view capability in a retained widget tree.
pub(crate) fn mount_native_views(
    widget: &mut dyn Widget,
    host: &crate::widget::native_view::NativeViewHost<'_>,
) {
    if let Some(lifecycle) = widget.native_view_lifecycle_mut() {
        lifecycle.mount_native_view(host);
    }
    for child in widget.children_mut() {
        mount_native_views(child, host);
    }
}

/// Unmount every native-view capability before the parent window disappears.
pub(crate) fn unmount_native_views(widget: &mut dyn Widget) {
    for child in widget.children_mut() {
        unmount_native_views(child);
    }
    if let Some(lifecycle) = widget.native_view_lifecycle_mut() {
        lifecycle.unmount_native_view();
    }
}

/// Begin a frame for every native-view capability in the retained tree.
pub(crate) fn native_view_frame_start(widget: &mut dyn Widget) {
    if let Some(lifecycle) = widget.native_view_lifecycle_mut() {
        lifecycle.native_view_frame_start();
    }
    for child in widget.children_mut() {
        native_view_frame_start(child);
    }
}

/// Panic-free node construction, used by every `build_layout` implementation
/// in this module. `TaffyTree::new_leaf` and `new_with_children` are typed
/// `-> TaffyResult<NodeId>` for API generality, but taffy 0.10's
/// implementation shows neither can return `Err`: `new_leaf` always
/// inserts and returns `Ok`, and `new_with_children` only fails for a child
/// `NodeId` belonging to a *different* tree, which never happens here since
/// every child passed in was just built in this same tree by the calls
/// immediately above each call site. [`Widget::build_layout`] returns a bare
/// `NodeId` (not a `Result`) across every implementor, so surfacing a
/// theoretical error here would mean threading a `Result` through the whole
/// toolkit for a branch that can't be taken.
pub trait TaffyTreeExt {
    /// Insert a leaf node for a widget-owned style.
    fn leaf_node(&mut self, style: taffy::Style) -> NodeId;

    /// Insert a parent whose child ids were built in this same tree.
    fn parent_node(&mut self, style: taffy::Style, children: &[NodeId]) -> NodeId;
}

impl TaffyTreeExt for TaffyTree<()> {
    #[allow(clippy::unwrap_used, reason = "see TaffyTreeExt doc comment")]
    fn leaf_node(&mut self, style: taffy::Style) -> NodeId {
        self.new_leaf(style).unwrap()
    }

    #[allow(clippy::unwrap_used, reason = "see TaffyTreeExt doc comment")]
    fn parent_node(&mut self, style: taffy::Style, children: &[NodeId]) -> NodeId {
        self.new_with_children(style, children).unwrap()
    }
}

pub type Element = Box<dyn Widget>;

pub(crate) mod layout;
pub use layout::{WidgetLayoutError, compute_widget_layout};

mod button;
pub use button::{Button, ButtonFace, ButtonInteract, ButtonStyle, face_colors};
mod checkbox;
pub use checkbox::{Checkbox, CheckboxFace, CheckboxInteract};
mod column;
pub use column::Column;
mod composition;
mod composition_parts;
mod form;
mod form_composition;
mod layout_info;
pub use form::{CheckControl, FormControl, labeled_check, labeled_field};
pub use form_composition::{Field, Form, FormPage, FormSection};
pub use layout_info::{
    Density, Effective, FieldLayout, FormLayout, LabelPlacement, LayoutInfo, LayoutOrigin,
    PageLayout, SectionLayout,
};
mod row;
pub use row::Row;
mod toggle;
pub use toggle::{Toggle, ToggleFace, ToggleInteract, toggle_accessibility, toggle_face_colors};
mod stack;
pub use stack::Stack;
mod container;
pub use container::Container;
mod router;
pub use router::{RouteError, RouteHandle, Router};
mod document_view;
pub use document_view::ModelView;
mod divider;
pub use divider::{Divider, DividerAxis};
mod link;
pub use link::{Link, os_open as open_external_url, try_os_open as try_open_external_url};
mod nav_split;
pub use nav_split::{
    DEFAULT_BREAKPOINT, DEFAULT_SIDEBAR_WIDTH, FlapState, NavSplit, PushPane, SplitLayout,
    SplitMode,
};
mod app_shell;
pub use app_shell::{
    AppShell, DEFAULT_SHELL_BREAKPOINT, DEFAULT_SHELL_SIDEBAR_WIDTH, NarrowScreen, ShellLayout,
};
mod sidebar;
pub use sidebar::{
    DEFAULT_DRAWER_BREAKPOINT, DEFAULT_DRAWER_WIDTH, SidebarDrawer, SidebarNav, SidebarNavItem,
    SidebarNavSection, SidebarSide,
};
mod nav_shell;
pub use nav_shell::{
    Affordance, ContentLayout, DEFAULT_NAVSHELL_BREAKPOINT, DEFAULT_NAVSHELL_OVERVIEW_WIDTH,
    DEFAULT_NAVSHELL_SIDEBAR_WIDTH, NavPane, NavShell, ShellMode,
};
mod document_viewport;
pub use document_viewport::{
    ContentDraw, DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_PADDING, DEFAULT_PAN_STEP,
    DocumentViewport, PAN_SLOP, SmallContentAlign, ViewportState,
};
mod floating_window;
pub use floating_window::{FloatingWindow, MIN_HEIGHT, MIN_WIDTH, TITLE_BAR_HEIGHT};
mod segmented;
pub use segmented::{
    ColorSwatchPicker, DEFAULT_PALETTE, SegmentTone, TriStateControl, packed_color,
};
mod text;
pub use text::Text;
mod spacer;
pub use spacer::Spacer;
mod image;
pub use image::{Image, ImageDecodeError, ImageFit};
mod image_crop;
pub use image_crop::{CropSelection, ImageCropEditor};
mod slider;
pub use slider::{Slider, SliderFace};
mod text_input;
pub use text_input::{TextActionRequest, TextInput, TextInputKind};
pub mod code_editor;
pub use code_editor::{
    CodeEditor, Highlighter, HlState, Language, LineSpans, Span, TokenKind, token_color,
};
mod editable_label;
pub use editable_label::{EditOutcome, EditableLabel};
pub mod autocomplete;
pub use autocomplete::{Autocomplete, AutocompleteOutcome, Suggester, Suggestion};
mod number_input;
pub use number_input::NumberInput;
pub mod marks;
pub use marks::{Mark, MarkSpan};
pub mod rich_text_line;
pub use rich_text_line::{LineSignal, RichTextLine};
pub mod rich_text_editor;
pub use rich_text_editor::{RichBlockStyle, RichTextEditor};
pub mod block_editor;
pub use block_editor::{
    Block, BlockEditOp, BlockEditor, BlockKind, CursorPlacement, DocumentPosition,
    DocumentSelection, TransclusionPreview, legacy_block_id,
};
pub mod slash_menu;
pub use slash_menu::SlashMenu;
pub mod link_popover;
pub use link_popover::{LinkPopover, PageEntry};
pub mod page_palette;
pub use page_palette::PageJumpPalette;
pub mod page_sidebar;
pub use page_sidebar::{PageSidebar, PageSidebarEvent};
mod dropdown;
pub use dropdown::Dropdown;
pub mod tabs;
pub use tabs::{TabBar, TabDescriptor};
pub mod scrollbar;
pub use scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
pub mod date_indexed_scrollbar;
pub use date_indexed_scrollbar::{
    DateIndexIntent, DateIndexMarker, DateIndexMarkerKind, DateIndexPreview, DateIndexedScrollbar,
    ProjectedDateMarker,
};
pub mod drag;
pub use drag::{Axis as DragAxis, DragController, ListMetrics, REORDER_THRESHOLD_PX};
mod scrollable;
pub use scrollable::{ScrollAxis, Scrollable};
mod tile_host;
pub use tile_host::{AppKind, TileContent, TileHost};
mod app_widget;
pub(crate) use app_widget::BoundAppWidget;
pub use app_widget::{AppCommand, AppHostIntent, AppWidget, IconDescriptor};
pub mod command_palette;
pub mod input_hint;
pub mod shortcut_recorder;
pub use input_hint::{InputDeviceGlyph, InputHint};
pub mod canvas_navigation_hint;
pub use canvas_navigation_hint::{CanvasNavigation, CanvasNavigationHint};
pub use shortcut_recorder::ShortcutRecorder;
pub mod context_menu;
mod floating_surface;
pub use context_menu::{ContextMenu, MenuItem};
pub mod data_table;
mod virtual_table_semantics;
pub use virtual_table_semantics::{
    TableSemanticCell, TableSemanticColumn, TableSemanticControl, TableSemanticRow,
    VirtualTableSemantics,
};
mod virtual_list_semantics;
pub use virtual_list_semantics::{ListSemanticRow, VirtualListSemantics};
pub mod table;
pub use table::Table;
pub mod icon_grid;
pub use icon_grid::{IconGrid, IconGridItem};
mod window_group;
pub use window_group::{
    DEFAULT_STRIP_USABLE_PX, RESIZE_HANDLE_PX, WIDTH_FLOOR_PX, WIDTH_MAX_PERMILLE,
    WIDTH_MIN_PERMILLE, WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
    WindowGroup, WindowWidth, max_group_px, min_group_px, permille_for_px,
};
mod resize_handle;
pub use resize_handle::{
    HANDLE_HIT_HALF_PX, HANDLE_HOVER_PX, HANDLE_SLOT_PX, HANDLE_VISIBLE_PX, ResizeHandle,
};
mod group_row;
pub use group_row::{
    CrossStripDetach, GroupRow, GroupTopology, JUMP_PX_PER_DETENT, OutputSpan, TabDropTarget,
    WheelJumpAccum, wheel_jump_detents,
};
mod strip_view;
pub use strip_view::{STRIP_COOLDOWN_MS, StripView};
mod workspace_view;
pub use workspace_view::{WorkspaceSwitchDirection, WorkspaceView};
pub mod tooltip;
pub use tooltip::{Tooltip, TooltipHost};
pub mod permission_prompt;
pub mod toast;
pub use permission_prompt::{PROMPT_HEIGHT, PermissionPrompt, PromptChoice};
pub mod file_picker;
pub use file_picker::{FilePicker, FilePickerEntry, FilePickerMode, FilePickerOutcome};

pub mod progress_bar;
pub use progress_bar::ProgressBar;
pub mod page_preview;
pub use page_preview::PagePreview;
pub mod spinner;
pub use spinner::Spinner;
pub mod date_picker;
pub use date_picker::{DatePicker, ExternalDatePickerHit};
pub mod time_picker;
pub use time_picker::{TimePicker, TimeValue};
pub mod color_picker;
pub mod color_swatch;
pub use color_picker::ColorPicker;
pub use color_swatch::ColorSwatchFace;
pub mod popover_widget;
pub use popover_widget::PopoverWidget;
pub mod popover;
pub use popover::Popover;
pub mod modal_dialog;
pub use modal_dialog::ModalDialog;
pub mod modal_frame;
pub use modal_frame::{ModalAction, ModalEmphasis, ModalFrame, ModalHit, ModalLayout, ModalSpec};
pub mod native_view;
pub use native_view::{
    NativeView, NativeViewAccessibility, NativeViewBackend, NativeViewGeometry, NativeViewHost,
    NativeViewInput,
};
pub mod modal_prompt;
pub use modal_prompt::PromptBody;
pub mod toolbar;
pub use toolbar::Toolbar;
pub mod undo_button;
pub use undo_button::UndoButton;
pub mod breadcrumb;
pub use breadcrumb::Breadcrumb;
pub mod avatar;
pub use avatar::Avatar;
pub mod badge;
pub use badge::{Badge, BadgeTone};
pub mod icon;
pub use icon::IconWidget;
pub mod chart;
pub use chart::{Chart, Series, Sparkline};
mod chart_legend;
pub mod chart_navigation;
pub use chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action,
    moved_index as move_chart_index, moved_item as move_chart_item,
};
pub mod cartesian_plot;
pub use cartesian_plot::{
    AxisLabelFormat, AxisRange, AxisScale, AxisTick, CartesianAnimationStyle, CartesianAnnotation,
    CartesianAnnotationKind, CartesianBrush, CartesianBrushSelection, CartesianGesturePolicy,
    CartesianPan, CartesianPlot, CartesianRenderingStyle, CartesianStyle, CartesianTooltipAnchor,
    CartesianTooltipStyle, CartesianZoom, LegendPosition, MarkerShape, PlotAxis, PlotDatum,
    PlotSeries, SeriesKind,
};
pub mod pie_chart;
pub use pie_chart::{PieChart, PieSlice, PieStyle, PieTooltipAnchor};
pub mod polar_area_chart;
pub use polar_area_chart::{PolarAreaChart, PolarAreaDatum, PolarAreaStyle};
pub mod heatmap;
pub use heatmap::{Heatmap, HeatmapCell, HeatmapStyle};
pub mod histogram;
pub use histogram::{Histogram, HistogramNormalization};
pub mod candlestick_chart;
pub use candlestick_chart::{Candle, CandlestickChart, CandlestickStyle};
pub mod chord_chart;
pub use chord_chart::{ChordChart, ChordLink, ChordNode, ChordSelection, ChordStyle};
pub mod treemap;
pub use treemap::{Treemap, TreemapItem, TreemapStyle};
pub mod radar_chart;
pub use radar_chart::{RadarAxis, RadarChart, RadarLegendStyle, RadarSeries, RadarStyle};
pub mod range_chart;
pub use range_chart::{RangeBar, RangeChart, RangeStyle};
pub mod category_chart;
pub use category_chart::{
    Category, CategoryChart, CategoryChartStyle, CategoryLayout, CategorySeries, CategoryValue,
};
pub mod distribution_chart;
pub use distribution_chart::{
    DistributionChart, DistributionKind, DistributionSeries, DistributionStyle,
};
pub mod sankey;
pub use sankey::{SankeyChart, SankeyLink, SankeyNode, SankeySelection, SankeyStyle};
pub mod sunburst;
pub use sunburst::{SunburstChart, SunburstNode, SunburstStyle};
pub mod funnel_chart;
pub use funnel_chart::{FunnelChart, FunnelDirection, FunnelStage, FunnelStyle};
pub mod gauge;
pub use gauge::{BarGauge, GaugeThresholds, RingGauge};
pub mod resizable_split;
pub use resizable_split::ResizableSplit;
pub mod video_player;
pub use video_player::VideoPlayer;
pub mod webview;
pub use webview::WebView;
pub mod image_viewer;
pub use image_viewer::ImageViewer;
pub mod tree_view;
pub use tree_view::{
    TreeChildState, TreeLoadReason, TreeLoadRequest, TreeModel, TreeModelChange, TreeNode,
    TreeRowKey, TreeSlot, TreeView,
};
pub mod virtual_list;
pub use virtual_list::{DuplicateListKey, ListModel, ListRowState, VecListModel, VirtualList};
pub mod action_row;
pub use action_row::ActionRowFace;
pub mod choice_card;
pub use choice_card::ChoiceCardFace;
pub mod keyframe;
pub use keyframe::{KeyframeFace, KeyframeState};
pub mod disclosure_header;
pub use disclosure_header::DisclosureHeaderFace;
pub mod synthetic_focus;
pub use synthetic_focus::{
    adjacent_semantic_target, mark_semantic_focus, paint_semantic_focus, recover_semantic_focus,
    semantic_focus_order, semantic_target_at_path, semantic_target_bounds,
};
pub mod toggle_group;
pub use toggle_group::{SegmentedFace, SegmentedStyle, ToggleGroup, segment_colors};

// EmptyState / Banner / StatTile also expose an immediate-mode `paint(...)`
// entry point for apps that render straight to a `Scene` rather than a
// widget tree.
pub mod empty_state;
pub use empty_state::EmptyState;
pub mod search_field;
pub use search_field::SearchField;
pub mod search_results;
pub use search_results::SearchResultsView;
pub mod stat_tile;
pub use stat_tile::{StatTile, Trend};
pub mod banner;
pub use banner::{Banner, BannerKind};
pub mod chip;
pub use chip::{Chip, ChipLead, ChipStyle};
mod card_composition;
pub use card_composition::{Actions, ContentCard};
pub mod card;
pub mod collaboration_overview;
pub use card::{Card, CardGrid, card_column_width, card_columns};
mod section;
pub use section::{DEFAULT_SECTION_GAP, Section};
pub mod capture_studio;
pub use capture_studio::{
    AudioCaptureMode, CaptureDestination, CaptureExtent, CaptureIntent, CaptureMode,
    CapturePreview, CaptureStudio, CaptureTarget, PipLayout, RecordingCommand, ReplayCommand,
    ScreenshotCommand, SourceMode,
};
pub mod screenshot_markup;
pub use screenshot_markup::{
    MarkupExport, MarkupStroke, MarkupTool, ScreenshotMarkup, ScreenshotMarkupCommand,
};

pub mod activity_timeline;
pub mod history;
pub mod history_group;
pub mod history_scrubber;
pub mod name_field;
pub use activity_timeline::ActivityTimeline;
pub use history::{
    HistoryAuthor, HistoryClassification, HistoryEntry, HistoryEntryId, HistoryFilter,
    RedactionDisplay, format_relative_time, hue_to_rgb, op_class_glyph, op_class_is_revertible,
};
pub use history_scrubber::HistoryScrubber;
// The op/artifact vocabulary a `HistoryEntry` is written in. Re-exported so a
// caller can build one without naming the collab-types crate directly.
pub use collab_types::{ArtifactKind, OpClass};

// Wheel-disc spoke overflow integration lives in `shell`.
pub mod channel_browser;
pub use channel_browser::{ChannelBrowser, ChannelEntry};

pub mod conflict_banner;
pub mod conflict_picker;
pub mod merge_strategy;

// Threshold-identity invite ceremony plus a QR widget; state machine entry
// point is `identity_flow/mod.rs`.
pub mod identity_flow;
pub mod qr;
pub mod resource_actions;
pub use conflict_banner::ConflictBanner;
pub use conflict_picker::{ConflictPicker, PickerOutcome};
pub use identity_flow::{
    IdentityFlow, IdentityFlowScreen, IdentityRecord, IdentityStore, InviteEnvelope, InviteExpiry,
    MemoryIdentityStore, PairingHandshake, PairingPayload, ScreenOutcome,
};
pub use merge_strategy::{
    Alternative, ConcurrentOp, ConflictKind, ConflictResolver, MergeStrategy, surface_banner,
};
pub use qr::QrWidget;

#[cfg(test)]
mod a11y_tree_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct CollabProbe {
        events: Rc<Cell<u32>>,
        child: Option<Box<dyn Widget>>,
    }

    impl Collaborative for CollabProbe {
        fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
            vec![crate::collaboration::CollabRequest::ClearContinuation {
                app_id: "probe".into(),
            }]
        }

        fn handle_collab_event(&mut self, _event: &crate::collaboration::CollabEvent) {
            self.events.set(self.events.get().saturating_add(1));
        }
    }

    impl AudiencePresentationProvider for CollabProbe {
        fn on_audience_presentation_changed(&mut self, _active: bool) {
            self.events.set(self.events.get().saturating_add(1));
        }
    }

    impl Widget for CollabProbe {
        fn collaborative_mut(&mut self) -> Option<&mut dyn Collaborative> {
            Some(self)
        }

        fn audience_presentation_provider(&self) -> Option<&dyn AudiencePresentationProvider> {
            Some(self)
        }

        fn audience_presentation_provider_mut(
            &mut self,
        ) -> Option<&mut dyn AudiencePresentationProvider> {
            Some(self)
        }

        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            _node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
        }

        fn apply_layout(
            &mut self,
            _taffy: &TaffyTree<()>,
            _node: NodeId,
            _origin: vello::kurbo::Point,
        ) {
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            let Some(child) = self.child.as_mut() else {
                return Vec::new();
            };
            let child: &mut (dyn Widget + '_) = child.as_mut();
            vec![child]
        }

        fn name(&self) -> &'static str {
            "CollabProbe"
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
    }

    /// Pinned so a future edit that removes the default blanket-breaks
    /// every widget impl that hasn't opted into a stable test id.
    #[test]
    fn widget_test_id_default_is_none() {
        let s = Spacer::new();
        assert!(
            Widget::test_id(&s).is_none(),
            "default Widget::test_id should be None"
        );
    }

    #[test]
    fn collaboration_capabilities_are_discovered_through_the_widget_tree() {
        let events = Rc::new(Cell::new(0));
        let child = CollabProbe {
            events: Rc::clone(&events),
            child: None,
        };
        let mut root = CollabProbe {
            events: Rc::clone(&events),
            child: Some(Box::new(child)),
        };

        assert_eq!(take_collab_requests(&mut root).len(), 2);
        handle_collab_event(
            &mut root,
            &crate::collaboration::CollabEvent::FollowChanged { peer_id: None },
        );
        assert_eq!(events.get(), 2);
    }

    #[test]
    fn audience_presentation_capability_forwards_through_dyn_widget() {
        let events = Rc::new(Cell::new(0));
        let mut widget: Box<dyn Widget> = Box::new(CollabProbe {
            events: Rc::clone(&events),
            child: None,
        });

        widget.on_audience_presentation_changed(true);
        assert_eq!(events.get(), 1);
    }

    #[test]
    fn action_emitter_backlog_is_bounded_and_keeps_the_newest_intent() {
        let mut emitter = ActionEmitter::new(test_bool_action_binding());
        for _ in 0..ACTION_EMITTER_QUEUE_LIMIT.saturating_add(1) {
            emitter.emit(false).expect("valid boolean intent");
        }
        emitter.emit(true).expect("valid boolean intent");
        assert_eq!(emitter.pending.len(), ACTION_EMITTER_QUEUE_LIMIT);

        let mut last = None;
        while let Some(invocation) = emitter.take() {
            last = invocation.arguments.get("value").cloned();
        }
        assert_eq!(last, Some(serde_json::Value::Bool(true)));
    }
}

#[cfg(test)]
pub(crate) mod control_result_probe {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone, Default)]
    pub(crate) struct Observed {
        pub(crate) results:
            Rc<RefCell<Vec<Result<ResourceOperationStart, tilo_actions::DoorError>>>>,
        pub(crate) calls: Rc<Cell<usize>>,
        pub(crate) polls: Rc<Cell<usize>>,
        pub(crate) commits: Rc<Cell<usize>>,
        pub(crate) flushes: Rc<Cell<usize>>,
        pub(crate) pending: Rc<RefCell<VecDeque<tilo_actions::ControlInvocation>>>,
        pub(crate) payloads: Rc<RefCell<Vec<String>>>,
    }
    impl Observed {
        pub(crate) fn enqueue(&self, action: &str) {
            self.pending
                .borrow_mut()
                .push_back(tilo_actions::ControlInvocation {
                    address: tilo_actions::ActionAddress::new("test.control-result", action),
                    arguments: serde_json::Map::new(),
                });
        }
        pub(crate) fn enqueue_text(&self, action: &str, text: &str) {
            self.pending
                .borrow_mut()
                .push_back(tilo_actions::ControlInvocation {
                    address: tilo_actions::ActionAddress::new("test.control-result", action),
                    arguments: serde_json::Map::from_iter([(
                        "text".to_string(),
                        serde_json::Value::String(text.to_string()),
                    )]),
                });
        }
    }
    #[allow(
        clippy::struct_excessive_bools,
        reason = "independent test switches exercise acceptance, confirmation, typing, and persistence"
    )]
    pub(crate) struct Probe {
        observed: Observed,
        accepted: bool,
        pub(crate) confirmation: bool,
        pub(crate) terminal_after: Option<(usize, bool)>,
        pub(crate) owned_flush: Option<Result<(), String>>,
        pub(crate) dirty_typing: bool,
        pub(crate) typed_text: bool,
    }
    impl Probe {
        pub(crate) fn new(accepted: bool) -> (Self, Observed) {
            let observed = Observed::default();
            (
                Self {
                    observed: observed.clone(),
                    accepted,
                    confirmation: false,
                    terminal_after: None,
                    owned_flush: None,
                    dirty_typing: false,
                    typed_text: false,
                },
                observed,
            )
        }
    }
    impl Widget for Probe {
        fn try_flush_owned_persistence(&mut self) -> Option<Result<(), String>> {
            self.observed
                .flushes
                .set(self.observed.flushes.get().saturating_add(1));
            self.owned_flush.clone()
        }
        fn needs_commit(&self) -> bool {
            self.dirty_typing
        }
        fn on_commit(&mut self) {
            self.observed
                .commits
                .set(self.observed.commits.get().saturating_add(1));
            self.dirty_typing = false;
        }
        fn poll_resource_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            _: bool,
            _: &AutoCommit,
            _: &ObjId,
        ) -> Option<tilo_actions::InvocationUpdate> {
            let (after, succeeds) = self.terminal_after?;
            let polls = self.observed.polls.get().saturating_add(1);
            self.observed.polls.set(polls);
            if polls < after {
                return None;
            }
            Some(if succeeds {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "fixture_failed".into(),
                        message: "Delayed finalizer failed".into(),
                        details: None,
                        retryable: false,
                    },
                }
            })
        }
        fn sync(&mut self, _: &mut AutoCommit, _: &ObjId) {}
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = tree.new_leaf(taffy::Style::default()).unwrap();
            map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: vello::kurbo::Point) {}
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        #[allow(
            clippy::too_many_arguments,
            reason = "test probe implements the Widget event contract"
        )]
        fn handle_event(
            &mut self,
            _: &WindowEvent,
            _: f64,
            _: &winit::event::Modifiers,
            _: &mut AutoCommit,
            _: &ObjId,
            _: &mut bool,
            _: &mut Option<NodeId>,
            _: NodeId,
            _: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &'static str {
            "ControlResultProbe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
            let mut contract = tilo_actions::ActionContract::default();
            if self.accepted {
                contract.execution = tilo_actions::ExecutionMode::Background { cancellable: false };
                contract.result = tilo_actions::ResultSpec::Job;
            }
            if self.confirmation {
                contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            let action = if self.typed_text {
                tilo_actions::ActionSpec::typed_operation(
                    "run",
                    "Run",
                    "text",
                    tilo_actions::OperationKind::Effect,
                    "test.control-result.run",
                    vec![tilo_actions::ActionParam::required(
                        "text",
                        tilo_actions::ParamType::Text,
                        "Captured text",
                    )],
                    |args| {
                        tilo_actions::EncodedAction::encode_operation(
                            "test.control-result.run",
                            &args.text("text").unwrap_or_default(),
                        )
                        .expect("a borrowed string always serializes")
                    },
                )
            } else {
                tilo_actions::ActionSpec::operation(
                    "run",
                    "Run",
                    tilo_actions::OperationKind::Effect,
                    "test.control-result.run",
                    &(),
                )
                .unwrap()
            };
            vec![
                tilo_actions::Resource::new("test.control-result", "test", "Probe")
                    .with_action(action.with_contract(contract)),
            ]
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["test.control-result.run"]
        }
        fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
            self.observed.pending.borrow_mut().pop_front()
        }
        fn on_control_invocation_result(
            &mut self,
            _: &tilo_actions::ActionAddress,
            result: &Result<ResourceOperationStart, tilo_actions::DoorError>,
        ) {
            self.observed.results.borrow_mut().push(result.clone());
        }
        fn start_resource_operation(
            &mut self,
            _: tilo_actions::InvocationId,
            _: tilo_actions::OperationKind,
            _: &str,
            payload: &[u8],
            _: &AutoCommit,
            _: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            if let Ok(text) = serde_json::from_slice::<String>(payload) {
                self.observed.payloads.borrow_mut().push(text);
            }
            self.observed
                .calls
                .set(self.observed.calls.get().saturating_add(1));
            Ok(Some(if self.accepted {
                ResourceOperationStart::Accepted
            } else {
                ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
            }))
        }
    }
    thread_local! { static NEXT: RefCell<Option<Probe>> = const { RefCell::new(None) }; }
    fn factory(
        mut context: crate::application::HostContext,
        _: crate::application::LaunchTarget,
        _: crate::application::RestoreInput,
    ) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(crate::launch::LaunchError::Record)?;
        Ok(crate::application::AppComponent::new(
            context,
            Box::new(NEXT.with_borrow_mut(|next| next.take().unwrap())),
            tilo_actions::ActionRegistry::new(),
        ))
    }
    pub(crate) fn component(
        host: &crate::application_test_support::ProbeHost,
        instance: u64,
        accepted: bool,
    ) -> (crate::application::AppComponent, Observed) {
        let (probe, observed) = Probe::new(accepted);
        NEXT.with_borrow_mut(|next| *next = Some(probe));
        let definition = crate::application_test_support::definition(factory);
        let target = crate::application::LaunchTarget::New;
        let context = host
            .prepare_with_permissions(
                &definition,
                instance,
                &target,
                crate::application_test_support::runner_permissions(),
            )
            .unwrap();
        let component = definition
            .instantiate(context, target, crate::application::RestoreInput::None)
            .unwrap();
        (component, observed)
    }

    impl AppWidget for Probe {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Control result probe"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Glyph("P")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }
}
