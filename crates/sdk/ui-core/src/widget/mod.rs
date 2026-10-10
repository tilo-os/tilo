mod action_directory_identity;
pub use action_directory_identity::ActionDirectoryInvocationDomain;
pub(crate) mod resource_operation;
// The raw operation protocol between the host and the toolkit's own widgets,
// reached from outside the toolkit only through `crate::host_operation`.
pub(crate) use resource_operation::start_without_document_writes;
pub(crate) use resource_operation::{ResourceOperation, ResourceOperationStart, start_in_children};

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

use crate::time::Instant;
use std::collections::HashMap;
use std::collections::HashSet;

/// Private foreground feedback, stripped of logical keys and typed text.
/// Observers cannot consume the input or obtain a document edit context.
#[derive(Clone, PartialEq, Eq)]
pub enum PhysicalInput {
    Key {
        key: winit::keyboard::PhysicalKey,
        state: winit::event::ElementState,
    },
    Focused(bool),
    /// Immutable configuration from the ordinary focused native event stream.
    Layout(Option<std::sync::Arc<winit::keyboard::KeyboardLayout>>),
}

impl PhysicalInput {
    pub fn from_window_event(event: &WindowEvent) -> Option<Self> {
        match event {
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => Some(Self::Key {
                key: event.physical_key,
                state: event.state,
            }),
            WindowEvent::Focused(focused) => Some(Self::Focused(*focused)),
            WindowEvent::KeyboardLayoutChanged(layout) => Some(Self::Layout(layout.clone())),
            _ => None,
        }
    }
}

/// The app's model, lent to a widget pass. The toolkit only passes it on to
/// children; the layer that owns the model reads it (the document runtime
/// through [`crate::doc::document`]). A host passes its model by reference
/// and it coerces: `widget.sync(&mut doc, &root)`.
pub type AppModel = dyn std::any::Any;

/// Where in the [`AppModel`] a widget is bound, forwarded alongside it.
pub type ModelPath = dyn std::any::Any;

/// Shared disabled-state opacity: wrap draw output in
/// `scene.push_layer(.., DISABLED_ALPHA, ..)` / `pop_layer()`.
pub const DISABLED_ALPHA: f32 = 0.45;

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

/// A laid-out box placed at `origin`, in absolute coordinates.
pub fn placed(layout: &taffy::Layout, origin: vello::kurbo::Point) -> vello::kurbo::Rect {
    vello::kurbo::Rect::from_origin_size(
        (
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        ),
        (f64::from(layout.size.width), f64::from(layout.size.height)),
    )
}

/// `node`'s laid-out box placed at `origin`: what `apply_layout` stores as a
/// leaf's rect. A node missing from the tree is an empty box at `origin`.
pub fn layout_rect(
    taffy: &TaffyTree<()>,
    node: NodeId,
    origin: vello::kurbo::Point,
) -> vello::kurbo::Rect {
    placed(&taffy.layout(node).copied().unwrap_or_default(), origin)
}

/// Route `event` through a container's children, first handler wins. A
/// child latching a drag gets it exclusively, so the drag survives the
/// cursor crossing a peer whose handler would otherwise claim it first; a
/// child holding an open modal owns the input the same way, so nothing
/// standing behind the sheet takes the press meant for it.
pub fn route_to_children(
    children: &mut [Box<dyn Widget>],
    nodes: &[NodeId],
    event: &WindowEvent,
    ctx: &mut EventCtx<'_>,
) {
    let owner = children
        .iter()
        .position(|c| c.wants_pointer_capture())
        .or_else(|| children.iter().position(|c| c.modal_capture().is_some()));
    if let Some(idx) = owner {
        if let (Some(child), Some(&node)) = (children.get_mut(idx), nodes.get(idx)) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
        return;
    }
    for (child, &node) in children.iter_mut().zip(nodes) {
        child.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        if *ctx.handled {
            break;
        }
    }
}

/// What a gated, presence-publishing widget draws over itself once drawn:
/// the denied-permission scrim, then the focus ring of the first peer on it.
pub(crate) fn draw_gate_and_peer_focus(
    widget: &(impl Widget + ?Sized),
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    presence: &PresenceCtx,
    permissions: &crate::permissions::PermissionContext,
) {
    let rect = widget.get_rect();
    if let Some(scope) = crate::permissions::capability::permission_scope(widget)
        && !permissions.check(&scope)
    {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::tokens::colors::PERMISSION_DENIED_SCRIM,
            None,
            &rect,
        );
    }
    if let Some(key) = crate::presence::capability::presence_key(widget)
        && let Some(user) = presence.here.get(&key).and_then(|peers| peers.first())
    {
        presence.draw_peer_focus(scene, theme, user, rect);
    }
}

/// The whole `draw_with_presence` of a gated, presence-publishing leaf:
/// culled off-viewport, drawn, then overlaid by [`draw_gate_and_peer_focus`].
pub(crate) fn draw_gated_leaf(
    widget: &mut (impl Widget + ?Sized),
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
    permissions: &crate::permissions::PermissionContext,
) {
    if presence.should_cull(widget.get_rect()) {
        return;
    }
    widget.draw(scene, transform, text_renderer, theme, presence);
    draw_gate_and_peer_focus(widget, scene, transform, theme, presence, permissions);
}

/// Hand `event` to the widgets a virtualizing container has mounted: the
/// first one holding pointer capture gets it alone, otherwise each laid-out
/// widget sees it, topmost first, until one handles it.
pub(crate) fn route_to_mounted<T>(
    mounted: &mut [T],
    parts: fn(&mut T) -> (&mut Element, Option<NodeId>),
    event: &WindowEvent,
    ctx: &mut EventCtx<'_>,
) {
    if let Some((widget, Some(node))) = mounted
        .iter_mut()
        .map(parts)
        .find(|(widget, _)| widget.wants_pointer_capture())
    {
        widget.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        return;
    }
    for (widget, node) in mounted.iter_mut().rev().map(parts) {
        let Some(node) = node else { continue };
        widget.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        if *ctx.handled {
            break;
        }
    }
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
        if widget.has_separate_document_owner() && widget.callback_binding().is_none() {
            return;
        }
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
                if let Ok(found) =
                    binding.read_document(doc, |doc, _| crate::resources::resources(widget, doc))
                {
                    resources.extend(found);
                }
            } else {
                resources.extend(crate::resources::resources(widget, doc));
            }
        }
        widget.for_each_child(&mut |child| visit(child, doc, binding.clone(), resources));
    }
    visit(widget, doc, None, resources);
}

/// Every resource `widget`'s subtree publishes, as the host's action
/// catalogue collects them. A host that embeds components under its own
/// root (a desktop surface) reads what its search offers through this.
pub fn published_resources(widget: &dyn Widget, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
    let mut resources = Vec::new();
    collect_widget_resources(widget, doc, &mut resources);
    resources
}

/// Every search provider the components in `widget`'s subtree publish, each
/// once, as weak proxies the components keep owning.
pub fn published_search_providers(
    widget: &dyn Widget,
) -> Vec<Box<dyn crate::search::SearchProvider>> {
    crate::bound_dispatch::search_providers(widget)
}

/// Test traversal probe; production callers retain bound provenance.
#[cfg(test)]
pub(crate) fn resolve_widget_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::Resource> {
    resolve_bound_resource(widget, doc, None, &mut |widget, doc| {
        crate::resources::resource(widget, doc, id)
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
                            crate::resources::action_directory_resource_page(
                                widget,
                                doc,
                                cursor,
                                max_results,
                            )
                        })
                        .ok()
                        .flatten()
                } else {
                    crate::resources::action_directory_resource_page(
                        widget,
                        doc,
                        cursor,
                        max_results,
                    )
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
    crate::format::capitalize_first(&tail.replace(['_', '-'], " "))
}

/// A reusable action-intent queue for controls, including custom widgets.
/// Widgets call [`emit`](Self::emit) from any interaction path. Mounted shared
/// controls install independent host custody; custom producers should use
/// [`new_in`](Self::new_in) with their actual owner domain. The private `take`
/// path remains only as explicitly unqualified migration compatibility.
pub struct ActionEmitter<I> {
    binding: tilo_actions::ActionBinding<I>,
    pending: crate::control_custody::ControlHolder,
    available: bool,
}

const ACTION_EMITTER_QUEUE_LIMIT: usize = 64;

impl<I> ActionEmitter<I> {
    pub fn new(binding: tilo_actions::ActionBinding<I>) -> Self {
        Self {
            binding,
            pending: crate::control_custody::ControlHolder::new(
                crate::control_custody::ControlProducerDiscipline::Fifo {
                    limit: ACTION_EMITTER_QUEUE_LIMIT,
                },
            ),
            available: true,
        }
    }

    /// Construct already registered, including for emitting inside a builder.
    pub fn new_in(binding: tilo_actions::ActionBinding<I>, domain: &crate::ControlDomain) -> Self {
        let mut emitter = Self::new(binding);
        emitter.pending = crate::control_custody::ControlHolder::registered(
            domain,
            crate::control_custody::ControlProducerDiscipline::Fifo {
                limit: ACTION_EMITTER_QUEUE_LIMIT,
            },
        );
        emitter
    }
    pub fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) -> Result<(), crate::control_custody::CustodyError> {
        self.pending.install(domain, attachment)
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
            self.pending.emit(invocation);
        }
        Ok(())
    }

    pub fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.pending.take()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCloseAdmission {
    Allow,
    Defer,
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
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool;

    /// Apply a live owner view to an already-bound resource. This opt-in
    /// capability receives no writable document and must preserve focus.
    /// Missing resources or anchors are refused so the host can defer or
    /// cancel the projection when its authenticated source disappears.
    fn restore_live_continuation_metadata(
        &mut self,
        _metadata: &crate::collaboration::ContinuationMetadata,
        _state: &AutoCommit,
        _path: &ObjId,
    ) -> bool {
        false
    }
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
    /// Route native keyboard focus to the exact presentation instance.
    fn on_floating_surface_focus_changed(&mut self, _id: Option<&str>, _focused: bool) {}

    /// Identify the native surface delivering the current input event. The
    /// host resets this immediately after dispatch; it grants no authority.
    fn on_floating_surface_event_source(&mut self, _id: Option<&str>) {}

    /// The retained semantic subtree presented in one secondary window.
    fn floating_surface_accessibility(&self, _id: &str) -> Option<crate::a11y::AccessNode> {
        None
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        _id: &str,
        _target: &str,
        _action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &automerge::ObjId,
    ) -> bool {
        false
    }

    /// IME purpose and desktop-logical caret rect for one secondary window.
    fn floating_surface_ime(
        &self,
        _id: &str,
    ) -> Option<(winit::window::ImePurpose, vello::kurbo::Rect)> {
        None
    }

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
        _state: &mut crate::widget::Edits<'_>,
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

/// The document as a host lends it to one pass that may change it: read
/// freely, changed only through a typed action the action host applies (a
/// resource's dispatcher applies it), so every change is an action that is
/// attributed, recorded in history and undone like any other.
/// Direct CRDT mutation is rejected at the widget boundary:
///
/// ```compile_fail
/// use automerge::{ROOT, transaction::Transactable};
/// use tilo_ui::widget::Edits;
/// fn bypass(edits: &mut Edits<'_>) {
///     edits.put(ROOT, "bypass", true).unwrap();
/// }
/// ```
pub struct Edits<'a> {
    doc: &'a mut AutoCommit,
}

impl<'a> Edits<'a> {
    /// Lend `doc` to one pass.
    #[doc(hidden)]
    pub fn lend(doc: &'a mut AutoCommit) -> Self {
        Self { doc }
    }

    /// The document as it stands, with what this pass applied.
    pub fn doc(&self) -> &AutoCommit {
        self.doc
    }

    /// The same document, lent on to a part of the pass.
    pub fn reborrow(&mut self) -> Edits<'_> {
        Edits {
            doc: &mut *self.doc,
        }
    }

    /// Apply `action` of the action type `type_id` now, through `host`, under
    /// `context`.
    ///
    /// # Errors
    /// Returns the action's refusal; nothing is written then.
    #[doc(hidden)]
    pub fn apply<A>(
        &mut self,
        host: &mut crate::action_host::ActionHost,
        type_id: &str,
        action: &A,
        context: tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError>
    where
        A: tilo_actions::Action + serde::Serialize,
    {
        host.try_dispatch_with_context(self.doc, type_id, action, context)
    }

    /// The lent document, for the hosts that pass it on to a record of
    /// their own.
    pub(crate) fn document_mut(&mut self) -> &mut AutoCommit {
        self.doc
    }
}

impl std::ops::Deref for Edits<'_> {
    type Target = AutoCommit;

    fn deref(&self) -> &Self::Target {
        self.doc()
    }
}

/// Where `doc` stands, to tell whether a settle pass changed it.
pub(crate) fn settle_mark(doc: &AutoCommit) -> (Vec<automerge::ChangeHash>, usize) {
    use automerge::transaction::Transactable as _;
    (doc.base_heads(), doc.pending_ops())
}

thread_local! {
    static RESOURCE_PRESENTATION_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Tell the host that what a widget presents to the resource surface (the
/// resources, actions or semantic controls it declares) changed on input it
/// did not mark handled and without a document edit: hovering a menu row
/// that opens a nested menu. The host keeps the published resource tree
/// across bare pointer motion, so such a widget must call this or the tree
/// agents and assistive technology read would go stale. Clicks, keys and
/// handled events outside the explicit view-only scroll contract republish.
pub fn note_resource_presentation_changed() {
    RESOURCE_PRESENTATION_EPOCH.with(|epoch| epoch.set(epoch.get().wrapping_add(1)));
}

/// How many times [`note_resource_presentation_changed`] ran on this thread.
#[cfg(feature = "native")]
pub(crate) fn resource_presentation_epoch() -> u64 {
    RESOURCE_PRESENTATION_EPOCH.with(std::cell::Cell::get)
}

/// Host services associated with one retained widget event dispatch.
pub struct EventCtx<'a> {
    pub scale_factor: f64,
    pub modifiers: &'a winit::event::Modifiers,
    pub state: Edits<'a>,
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
            state: self.state.reborrow(),
            path,
            handled: &mut *self.handled,
            focus_target: &mut *self.focus_target,
            node_id,
            taffy: self.taffy,
        }
    }

    /// The document this event may change, through typed actions only.
    pub fn edits(&mut self) -> Edits<'_> {
        self.state.reborrow()
    }

    /// The stylus that made the `CursorMoved` or `MouseInput` being
    /// handled: its pressure, tilt and whether the eraser end is in use.
    /// `None` for a mouse, a touch, or any other event. A pen's tip is the
    /// left button and its barrel buttons the right and middle ones, so a
    /// widget needs this only for what a mouse cannot say.
    #[must_use]
    pub fn pen(&self) -> Option<PenSample> {
        crate::pen::current()
    }
}

/// One remote participant's live presence (caret/selection), published by a
/// multi-participant widget under that participant's identity. Same key and
/// payload shape as the local window's own presence (Writer: `wp_block` key,
/// byte offsets into the caret block).
#[derive(Clone, Debug, PartialEq)]
pub struct ParticipantPresence {
    pub participant: tilo_actions::ParticipantId,
    /// Name to show beside the caret, when the host set one
    /// ([`Widget::set_participant_label`]).
    pub label: Option<String>,
    pub key: PresenceKey,
    pub payload: crate::collaboration::PresencePayload,
}

/// Identity of one live node-map widget for once-per-widget host sweeps.
///
/// Several layout nodes may name the same widget, so sweeps deduplicate. The
/// data address alone is not an identity: a struct shares it with its first
/// field, and a `HashMap` sweep would then visit whichever of the two it meets
/// first and silently skip the other (for example a component's committed
/// action bracket). The concrete type keeps wrapper and first field distinct.
///
/// # Safety
/// `pointer` must be live and not mutably borrowed for the duration of the call.
pub(crate) unsafe fn node_sweep_identity(pointer: *const dyn Widget) -> (*const (), &'static str) {
    // SAFETY: the caller guarantees a live, unaliased-for-writes widget.
    let concrete = unsafe { (*pointer).concrete_type_name() };
    (pointer.cast::<()>(), concrete)
}

pub trait Widget {
    /// Concrete identity for host-side frame maps that must distinguish a
    /// wrapper from its first field even when they share a data address.
    /// Unlike trait-object pointer equality, this does not depend on the
    /// compiler reusing one vtable instance across codegen units.
    #[doc(hidden)]
    fn concrete_type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Structural chrome can explicitly leave an inherited document callback.
    #[doc(hidden)]
    fn resets_callback_binding(&self) -> bool {
        false
    }
    /// A legacy owner's actual live layout boundary, never framework chrome.
    #[doc(hidden)]
    fn legacy_callback_boundary(&self) -> Option<(NodeId, crate::history_mode::HistoryOwner)> {
        None
    }
    /// Drawn live content whose input is suspended by a framework surface.
    #[doc(hidden)]
    fn inactive_callback_boundary(&self) -> Option<NodeId> {
        None
    }

    /// Read-only state evidence for opt-in scheduled native input diagnostics.
    /// Called only around diagnostic inputs, never during ordinary input or paint.
    /// Keep this inexpensive and root-local; snapshots may be logged verbatim.
    /// This verifies widget state, not rendered pixels or display presentation.
    fn native_input_snapshot(
        &self,
        _state: &AppModel,
        _path: &ModelPath,
    ) -> Option<serde_json::Value> {
        None
    }

    /// Opt in only when this wheel event changes scroll geometry and nothing
    /// else: no document edit, focus/selection, action availability, resource
    /// declaration, pending command or work that requires `sync`. The runner
    /// still lays out, paints and refreshes accessibility geometry. Unknown
    /// handlers stay on the full synchronization/publication path.
    fn view_only_scroll(&self, _event: &WindowEvent) -> bool {
        false
    }

    /// Explicit app-owned region; never search secondary children implicitly.
    fn primary_vertical_scroll(&self) -> Option<PrimaryScrollRegion> {
        None
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        _action: PrimaryScrollAction,
    ) -> PrimaryScrollResult {
        PrimaryScrollResult::Unsupported
    }

    /// Runtime binding inherited by descendants in a matching layout topology.
    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        None
    }

    /// Whether this widget introduces a document owner distinct from the
    /// runner's outer State. This is a compatibility check, never identity
    /// or authority for history access or restore.
    ///
    /// A [`Self::record_intent`] alone does not qualify: the runner (or the
    /// enclosing [`TileHost`], which reports its own binding) checks that
    /// record out as the very State this widget reads.
    fn has_separate_document_owner(&self) -> bool {
        self.callback_binding().is_some()
    }

    /// Opaque actual custody for framework History source registration.
    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        self.callback_binding()
            .map(crate::history_mode::HistoryOwner::component)
    }

    /// Records this widget keeps open beside the document it shows, each
    /// synced on its own route: a framework app's records in the person's
    /// own room ([`crate::record::person`]). They take no part in this
    /// widget's history or controls.
    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        Vec::new()
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

    /// SDK model service visits retained owners independently of visible children.
    /// The default preserves the original custom component visitor exactly.
    #[doc(hidden)]
    fn visit_retained_components(
        &mut self,
        request: &mut crate::bound_dispatch::RetainedComponentRequest<'_, '_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        self.visit_components(request.inner())
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
        request.visit(self)
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
        for child in self.children_mut() {
            request.visit(child)?;
        }
        Ok(())
    }

    /// Answer a request for a capability this widget takes part in (see
    /// [`capability`]). Declare capabilities with [`crate::provides!`].
    fn provide<'a>(&'a self, _request: &mut capability::Request<'a, '_>) {}

    /// Exclusive counterpart to [`Widget::provide`].
    fn provide_mut<'a>(&'a mut self, _request: &mut capability::RequestMut<'a, '_>) {}

    /// Attach native child views through this owner's retained lifecycle topology.
    ///
    /// Presentation containers with inactive or hidden live descendants override
    /// this traversal so lifecycle ownership remains independent of drawing and
    /// input visibility.
    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.mount_native_view(host);
        }
        for child in self.children_mut() {
            child.mount_native_views(host);
        }
    }

    /// Tear native children down while their parent window still exists.
    fn unmount_native_views(&mut self) {
        for child in self.children_mut() {
            child.unmount_native_views();
        }
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.unmount_native_view();
        }
    }

    /// Begin a native-view frame through this owner's retained lifecycle topology.
    fn native_view_frame_start(&mut self) {
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.native_view_frame_start();
        }
        for child in self.children_mut() {
            child.native_view_frame_start();
        }
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

    /// Pull model state into this widget before layout. The default syncs the
    /// children [`Widget::children_mut`] returns; a widget overrides it only to
    /// read state of its own, or to sync children it does not expose there.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        false
    }

    /// Apply, after the sync pass, what the widget took up there and must
    /// write: a worker's answer, a chooser's file, an event a child queued.
    /// Writes go only through typed actions on `edits` (a resource's
    /// dispatcher); the host syncs again when the document changed. The
    /// default settles the children [`Widget::children_mut`] returns.
    fn settle(&mut self, edits: &mut Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
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

    /// Owned manual policy descriptor; the host invokes it outside widget borrows.
    #[doc(hidden)]
    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        None
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

    /// Observe this app's ordinary foreground physical input before framework
    /// shortcuts consume it. Keep feedback ephemeral; this is not an input grant.
    fn observe_physical_input(&mut self, _input: PhysicalInput) {}

    /// Handle one input event with the runtime services for this retained node.
    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut EventCtx<'_>) {}
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
    /// Implementors of [`Self::start_operation`] must list their locally
    /// handled types here, beside it. The host combines these declarations
    /// across the retained tree and refuses to publish an action whose type has
    /// no handler, preventing a catalogue entry that can only fail at runtime.
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        if crate::undo::has_deferred_local_history(self) {
            &[crate::undo::EDIT_OPERATION]
        } else {
            &[]
        }
    }

    fn resource_operation_handlers(&self) -> HashSet<String> {
        let mut handlers = self
            .local_resource_operation_handlers()
            .iter()
            .map(|handler| (*handler).to_string())
            .collect::<HashSet<_>>();
        // The actual widget's registered readers also declare their selectors.
        // Descriptor availability and qualified owner admission remain separate.
        handlers.extend(
            self.semantic_readers()
                .iter()
                .map(|reader| reader.operation_type().to_string()),
        );
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
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
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

    /// One-shot request to open the host's search palette, the same one the
    /// platform search shortcut opens. An app's own search button latches it
    /// so search stays one shared host surface rather than an app overlay.
    fn take_search_request(&mut self) -> bool {
        self.children_mut()
            .into_iter()
            .any(Widget::take_search_request)
    }

    /// `point` is logical coords, same space as [`Widget::get_rect`];
    /// `None` defers to the default arrow. The host hit-tests the whole
    /// tree via `node_map` and picks the smallest-area claim, so containers
    /// need not override this.
    fn cursor_shape(&self, _point: vello::kurbo::Point) -> Option<winit::window::CursorIcon> {
        None
    }

    /// Opt out of touch-to-pointer emulation for contacts that begin at
    /// `point` (logical coords, same space as [`Widget::get_rect`]).
    ///
    /// By default the host turns direct touch into pointer events: a tap
    /// clicks, a drag scrolls, a hold opens the context menu, and a second
    /// finger turns the contact into a pinch or pan. A widget that returns
    /// `true` here instead receives only the raw per-finger
    /// `WindowEvent::Touch` stream (`Started`/`Moved`/`Ended`/`Cancelled`,
    /// keyed by `Touch::id`, `location` in physical pixels) for every contact
    /// that begins inside it, and those contacts neither cancel nor are
    /// cancelled by other fingers. An on-screen keyboard uses this so two
    /// thumbs can type with overlapping presses.
    ///
    /// Hit-tested like [`cursor_shape`](Self::cursor_shape): the host asks
    /// every laid-out widget whose rect contains the point, so containers
    /// need not forward it.
    fn claims_raw_touch(&self, _point: vello::kurbo::Point) -> bool {
        false
    }

    /// Revealed after a dwell over the widget's rect, via the toolkit's one
    /// tooltip mechanism ([`crate::widget::tooltip::TooltipHost`]),
    /// hit-tested like [`cursor_shape`](Self::cursor_shape) so containers
    /// need not forward it. An icon-only control should also use this as
    /// its accessible name.
    fn tooltip(&self) -> Option<&str> {
        None
    }

    /// Which side of the control its tooltip goes: below by default,
    /// beside it for a control in a vertical rail.
    fn tooltip_side(&self) -> crate::widget::tooltip::TooltipSide {
        crate::widget::tooltip::TooltipSide::Below
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
    /// Preferred control when its app is explicitly activated without a valid
    /// saved control. Hosts still enforce visibility, enabled state and scope.
    fn is_default_keyboard_focus(&self) -> bool {
        false
    }

    /// Exact app instance whose non-text shortcuts this subtree owns when
    /// activation has no control target. Unbound desktop chrome may declare
    /// the active tab; component boundaries default to their captured owner.
    fn keyboard_input_scope(&self) -> Option<crate::application::InstanceId> {
        self.callback_binding().map(|binding| binding.instance_id)
    }

    fn set_focus(&mut self, _focused: bool) {}

    /// The form's validation verdict for this control, set by
    /// [`Field::with_error`]: an invalid control edges itself in danger ink
    /// and reports `invalid` to assistive technology. Controls without a
    /// field face ignore it.
    fn set_invalid(&mut self, _invalid: bool) {}

    /// An explicit runner focus request, after resolving the actual keyboard
    /// owner. Retained flags and layout restoration are not user gestures.
    /// Containers can observe this to reveal the owning viewport.
    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        for child in self.children_mut() {
            child.focus_requested(target, tree);
        }
    }

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
    /// `draw`; names must already be [`i18n!`](macro@crate::i18n)-resolved.
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

    /// Whether one retained [`children`](Widget::children) entry is already
    /// represented in this widget's own synthetic
    /// [`accessibility`](Widget::accessibility) tree, so the bridge must not
    /// export it a second time. A host that publishes a child's semantics
    /// itself (to add its own controls inside the same dialog) answers `true`
    /// for that child; assistive technology then sees one copy, not two.
    fn accessibility_represents_child(&self, _child_index: usize) -> bool {
        false
    }

    /// May mutate the CRDT like [`handle_event`](Widget::handle_event).
    /// Default `false` ("not handled") falls back to generic dispatch
    /// (`Focus` moves keyboard focus, `Activate` synthesises a click);
    /// widgets with richer semantics (slider inc/dec, combo
    /// expand/collapse) override to handle those verbs precisely.
    fn perform_action(
        &mut self,
        _action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
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
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
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
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
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

    /// This field needs a host that consumes deferred receipts. Ordinary native
    /// Wayland windows must not advertise it until they have that delivery path.
    fn ime_requires_deferred_ack(&self) -> bool {
        let caret = self.ime_cursor_rect();
        let mut required = false;
        self.for_each_child(&mut |child| {
            if child.ime_cursor_rect() == caret && child.ime_purpose().is_some() {
                required |= child.ime_requires_deferred_ack();
            }
        });
        required
    }

    /// Begin one bounded deferred session after explicit host consent, preserving
    /// the existing original-field fingerprint. This grants no microphone access.
    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        for child in self.children_mut() {
            if child.ime_cursor_rect() == Some(caret) && child.ime_purpose().is_some() {
                return child.start_deferred_text();
            }
        }
        None
    }

    /// A private asynchronous final. None leaves ordinary synchronous IME dispatch
    /// to the host. A receipt binds this original field and must be acknowledged
    /// before the host releases its private recovery transcript.
    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        for child in self.children_mut() {
            if child.ime_cursor_rect() == Some(caret) && child.ime_purpose().is_some() {
                return child.commit_deferred_text(text);
            }
        }
        None
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

    /// Whether the original focused editor accepts line feeds as text. This is
    /// field policy, never inferred from a multiline accessibility presentation.
    /// Wrappers inherit only one unambiguous child at their active IME caret.
    fn ime_multiline(&self) -> bool {
        let Some(caret) = self.ime_cursor_rect() else {
            return false;
        };
        let mut matching = 0_usize;
        let mut multiline = false;
        self.for_each_child(&mut |child| {
            if child.ime_cursor_rect() == Some(caret)
                && child.ime_purpose() == Some(winit::window::ImePurpose::Normal)
            {
                matching = matching.saturating_add(1);
                multiline = child.ime_multiline();
            }
        });
        matching == 1 && multiline
    }

    /// Opaque local invalidation of deferred input, including selection edits
    /// that leave the caret rectangle unchanged. The host supplies an ephemeral
    /// secret key; no text or selection is retained in the resulting digest.
    /// The default shares the accessibility door and excludes protected or
    /// read-only controls. Text controls can override it to avoid cloning text.
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let caret = self.ime_cursor_rect()?;
        let mut index = 0_usize;
        let mut selected = None;
        let mut ambiguous = false;
        self.for_each_child(&mut |child| {
            if child.ime_cursor_rect() == Some(caret) && child.ime_purpose().is_some() {
                if selected.is_some() {
                    ambiguous = true;
                }
                selected =
                    Some(child.ime_edit_fingerprint(key).map(|stamp| {
                        crate::ime::scoped_fingerprint(&index.to_le_bytes(), stamp, key)
                    }));
            }
            index = index.saturating_add(1);
        });
        if ambiguous {
            return None;
        }
        if let Some(selected) = selected {
            return selected;
        }
        let node = self.accessibility()?;
        if node.state.protected || node.state.read_only {
            return None;
        }
        let selection = node.text_selection?;
        Some(crate::ime::edit_fingerprint(
            &node.value?,
            selection.anchor,
            selection.focus,
            key,
        ))
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
    fn on_external_event(
        &mut self,
        _payload: &str,
        _state: &mut crate::widget::Edits<'_>,
        _root: &ModelPath,
    ) {
    }

    /// Global back/forward gesture (swipe/edge-pull), recognised by
    /// [`crate::gesture::NavGestureRecognizer`] and delivered to the whole
    /// tree; return `true` once consumed. Default forwards to children, so
    /// a widget that owns history becomes swipe-navigable for free.
    fn on_nav_gesture(
        &mut self,
        dir: crate::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        for child in self.children_mut() {
            if child.on_nav_gesture(dir, state, path) {
                return true;
            }
        }
        false
    }

    /// Which directions [`Self::on_nav_gesture`] would consume now. A widget
    /// that owns history overrides this beside `on_nav_gesture`.
    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        self.children()
            .into_iter()
            .fold(crate::gesture::NavAvailability::default(), |seen, child| {
                seen.or(child.nav_availability())
            })
    }

    /// One-shot (reading MUST clear it) signal to move focus to this
    /// subtree's first focusable widget after the next rebuild. Only
    /// consulted under [`crate::app::App::autofocus_on_structure_change`];
    /// a wizard raises it on step transitions.
    fn take_focus_reset(&mut self) -> bool {
        false
    }

    /// One-shot (reading MUST clear it) request to change the window's
    /// accessibility preferences, raised by a control the person used. The
    /// host applies it before the next frame. Default forwards the first
    /// request found among the children.
    fn take_accessibility_request(&mut self) -> Option<crate::a11y::AccessibilityRequest> {
        self.children_mut()
            .into_iter()
            .find_map(|child| child.take_accessibility_request())
    }

    /// Route history when this widget, or one it contains, is a [`Router`](router::Router).
    ///
    /// Dev reload writes this beside the document so the same screen is showing
    /// after the process is replaced. Containers forward to the first child
    /// that has one. A [`Router`](router::Router) reports its own stack and
    /// does not look inside the current screen.
    fn dev_route(&self) -> Option<router::RouteSnapshot> {
        for child in self.children() {
            if let Some(route) = child.dev_route() {
                return Some(route);
            }
        }
        None
    }

    /// Put `route` back. Returns whether a router in this subtree accepted it.
    ///
    /// An unknown route name is refused and the existing screen stays up.
    fn restore_dev_route(&mut self, route: &router::RouteSnapshot) -> bool {
        for child in self.children_mut() {
            if child.restore_dev_route(route) {
                return true;
            }
        }
        false
    }

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

    // --- Collab / DnD contract extensions ---
    /// Whether `sync` still needs to run on idle frames without input or
    /// collab events — e.g. waiting out a document-adoption grace window.
    fn needs_idle_sync(&self) -> bool {
        false
    }

    /// The affordance-driven twin of the Ctrl+K palette: when a widget's own
    /// control dispatches the same [`tilo_actions`] action its
    /// [`ActionSpec`](tilo_actions::ActionSpec) exports, it reports the
    /// document heads captured **just before** here, so the host can record
    /// one undo boundary the same way a palette dispatch does. Rising-edge:
    /// reading MUST clear it.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        None
    }

    /// Rising-edge compensating document actions for the bracket
    /// [`Self::take_committed_action`] just opened: `(undo_via, redo_via)`.
    /// Set when the forward action's
    /// [`tilo_actions::Action::inverse_before`] recorded an outside
    /// consequence. Default: none (pure CRDT undo via inverse patches).
    fn take_action_revert(
        &mut self,
    ) -> Option<(tilo_actions::EncodedAction, tilo_actions::EncodedAction)> {
        None
    }

    /// Whether the undo step this widget's last reported boundary opened
    /// stays open for the actions that continue it — the rest of a word
    /// typed a keystroke at a time. A hosted component then closes the step
    /// at the widget's next boundary, or before an undo, a save or a merge,
    /// instead of after this event. Answered by the widget itself, never its
    /// children.
    fn keeps_undo_step_open(&self) -> bool {
        false
    }

    /// Host → widget: the live state of the undo history, published once per
    /// frame to every laid-out widget.
    ///
    /// A widget receives a read-only document or an [`Edits`] action loan, which carries the
    /// document but not its history — so without this a control could not tell
    /// whether there was anything to undo, and every app was reduced to
    /// printing the shortcut and hoping. Store the status (embed an
    /// [`UndoAccess`](crate::undo::UndoAccess)) and read it back in `draw`, in
    /// [`is_enabled`](Widget::is_enabled), and in
    /// [`accessibility`](Widget::accessibility) — which only gets `&self`, and
    /// is exactly where the disabled state must be reflected.
    ///
    /// The host sweeps its `node_map`, so this reaches a widget at any depth
    /// with no container forwarding. Call the provider only when the widget
    /// consumes status; deriving historical summaries can require a document
    /// diff. Default: ignore the provider without evaluating it.
    fn set_undo_status(&mut self, _status: &mut dyn FnMut() -> crate::undo::UndoStatus) {}

    /// Widget → host: an undo or redo the user asked for.
    ///
    /// The host drains this after each event and performs it through
    /// `App::perform_undo` /
    /// `App::perform_redo` — the same methods
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
        _doc: &mut crate::widget::Edits<'_>,
        _type_id: &str,
        _payload: &[u8],
    ) -> bool {
        false
    }

    /// Admit an exported effect or view operation: the one entry every
    /// handler implements. Document operations never enter it.
    ///
    /// Return `Ok(None)` to decline so routing continues;
    /// [`ResourceOperationStart::Completed`] with the output the action's
    /// `ActionContract` promises when the work is done on return; or
    /// [`ResourceOperationStart::Accepted`] for work that later reports
    /// through [`Self::poll_resource_operation`]. Declare every handled type
    /// in [`Self::local_resource_operation_handlers`].
    ///
    /// Read the operation through [`ResourceOperation::unscoped`] unless the
    /// handler honours a required invocation's custody; a custody-aware
    /// handler reads it through [`ResourceOperation::with_custody`], retains
    /// the scope with any job it starts and uses its checked callbacks at each
    /// later effect or document mutation. Hand operations a widget does not
    /// own to [`start_in_children`], which is also the default.
    ///
    /// # Errors
    ///
    /// Returns the first admission or synchronous-operation error.
    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if let Some(started) = crate::undo::start_local_history(self, op)? {
            return Ok(Some(started));
        }
        start_in_children(self, op, doc, path)
    }

    /// Inert immutable-reader registrations owned by this actual widget.
    /// Registration alone supplies no qualified owner frame or disclosure.
    fn semantic_readers(
        &self,
    ) -> &'static [crate::semantic_document_read::RegisteredSemanticReader] {
        &[]
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
        if let Some(update) =
            crate::undo::poll_local_history(self, invocation, cancellation_requested)
        {
            return Some(update);
        }
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
    fn on_drop(
        &mut self,
        _payload: DragPayload,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) {
    }
    fn wants_pointer_capture(&self) -> bool {
        false
    }

    /// Unlike `wants_pointer_capture`, does not swallow misses — drawn
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
    /// taffy tree when so. Reading clears it. The default drains every
    /// retained child (no short-circuit, so no child keeps a stale flag) and
    /// reports whether any was dirty; a leaf has no children and reports
    /// `false`. A container that changes its own child list sets its own flag
    /// and ORs it with this traversal.
    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    /// Work already requested by this view whose completion will replace a
    /// placeholder. Unlike animations, this can be awaited by still captures.
    fn has_pending_visual_work(&self) -> bool {
        let mut pending = false;
        self.for_each_child(&mut |child| pending |= child.has_pending_visual_work());
        pending
    }

    /// Temporary feedback currently presented, such as a save acknowledgement.
    /// Captures may intentionally include it; scene-invariance checks can wait
    /// for it to disappear separately from unfinished placeholder work.
    fn has_transient_feedback(&self) -> bool {
        self.children()
            .into_iter()
            .any(Widget::has_transient_feedback)
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
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool);
    fn on_floating_surface_event_source(&mut self, id: Option<&str>);
    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode>;
    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool;
    fn floating_surface_ime(
        &self,
        id: &str,
    ) -> Option<(winit::window::ImePurpose, vello::kurbo::Rect)>;
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
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        if let Some(provider) = capability::capability_mut::<dyn FloatingSurfaceProvider>(self) {
            provider.on_floating_surface_focus_changed(id, focused);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        if let Some(provider) = capability::capability_mut::<dyn FloatingSurfaceProvider>(self) {
            provider.on_floating_surface_event_source(id);
        }
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        capability::capability::<dyn FloatingSurfaceProvider>(self)?
            .floating_surface_accessibility(id)
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        capability::capability_mut::<dyn FloatingSurfaceProvider>(self).is_some_and(|provider| {
            provider.perform_floating_surface_accessibility_action(id, target, action, state, path)
        })
    }

    fn floating_surface_ime(
        &self,
        id: &str,
    ) -> Option<(winit::window::ImePurpose, vello::kurbo::Rect)> {
        capability::capability::<dyn FloatingSurfaceProvider>(self)?.floating_surface_ime(id)
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        capability::capability_mut::<dyn FloatingSurfaceProvider>(self)
            .and_then(FloatingSurfaceProvider::take_floating_surface_request)
    }

    fn take_capture_studio_request(&mut self) -> Option<CaptureMode> {
        capability::capability_mut::<dyn FloatingSurfaceProvider>(self)
            .and_then(FloatingSurfaceProvider::take_capture_studio_request)
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
        capability::capability_mut::<dyn FloatingSurfaceProvider>(self).map_or_else(
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
        if let Some(provider) = capability::capability_mut::<dyn FloatingSurfaceProvider>(self) {
            provider.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        capability::capability::<dyn FloatingSurfaceProvider>(self).map_or_else(
            Vec::new,
            FloatingSurfaceProvider::floating_surface_presentations,
        )
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        capability::capability::<dyn FloatingSurfaceProvider>(self).map_or_else(
            Vec::new,
            FloatingSurfaceProvider::active_floating_surface_ids,
        )
    }

    fn set_floating_surface_bounds(&mut self, bounds: vello::kurbo::Rect) {
        if let Some(provider) = capability::capability_mut::<dyn FloatingSurfaceProvider>(self) {
            provider.set_floating_surface_bounds(bounds);
        }
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        if let Some(provider) = capability::capability_mut::<dyn FloatingSurfaceProvider>(self) {
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
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool;
    fn on_audience_presentation_closed(&mut self);
}

impl<T: Widget + ?Sized> AudiencePresentationWidgetExt for T {
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
        capability::capability::<dyn AudiencePresentationProvider>(self)
            .and_then(AudiencePresentationProvider::audience_presentation_request)
    }

    fn on_audience_presentation_changed(&mut self, active: bool) {
        if let Some(provider) = capability::capability_mut::<dyn AudiencePresentationProvider>(self)
        {
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
        capability::capability_mut::<dyn AudiencePresentationProvider>(self).is_some_and(
            |provider| {
                provider.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
            },
        )
    }

    fn handle_audience_presentation_event(
        &mut self,
        event: &WindowEvent,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        capability::capability_mut::<dyn AudiencePresentationProvider>(self)
            .is_some_and(|provider| provider.handle_audience_presentation_event(event, state, path))
    }

    fn on_audience_presentation_closed(&mut self) {
        if let Some(provider) = capability::capability_mut::<dyn AudiencePresentationProvider>(self)
        {
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
        capability::capability::<dyn FullscreenProvider>(self)
            .and_then(FullscreenProvider::fullscreen_intent)
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        capability::capability::<dyn FullscreenProvider>(self).map_or_else(
            || {
                crate::fullscreen::FullscreenRequest::mirror(
                    crate::fullscreen::FullscreenTarget::Largest,
                )
            },
            FullscreenProvider::fullscreen_params,
        )
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        if let Some(provider) = capability::capability_mut::<dyn FullscreenProvider>(self) {
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
        capability::capability::<dyn CompleteContentProvider>(self)
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
        capability::capability_mut::<dyn CompleteContentProvider>(self).is_some_and(|provider| {
            provider.draw_complete_content(scene, bounds, transform, text_renderer, theme, presence)
        })
    }
}

/// Drain collaboration capabilities throughout a retained widget tree.
pub(crate) fn take_collab_requests(
    widget: &mut dyn Widget,
) -> Vec<crate::collaboration::CollabRequest> {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return Vec::new();
    };
    let mut requests = capability::capability_mut::<dyn Collaborative>(&mut *widget)
        .map_or_else(Vec::new, Collaborative::take_collab_requests);
    for child in crate::collaboration::participants::collaboration_children(widget) {
        requests.extend(take_collab_requests(child));
    }
    requests
}

/// Deliver one session event to each explicit collaboration capability once.
/// Bound callback context remains entered for the complete descendant walk.
pub(crate) fn handle_collab_event(
    widget: &mut dyn Widget,
    event: &crate::collaboration::CollabEvent,
) {
    let Ok(_entry) = crate::bound_dispatch::enter_widget(widget) else {
        return;
    };
    if let Some(collaborative) = capability::capability_mut::<dyn Collaborative>(&mut *widget) {
        collaborative.handle_collab_event(event);
    }
    for child in crate::collaboration::participants::collaboration_children(widget) {
        handle_collab_event(child, event);
    }
}

/// Mount native views through each owner's retained lifecycle topology.
#[cfg(feature = "native")]
pub(crate) fn mount_native_views(
    widget: &mut dyn Widget,
    host: &crate::widget::native_view::NativeViewHost<'_>,
) {
    widget.mount_native_views(host);
}

/// Unmount native views through each owner's retained lifecycle topology.
pub(crate) fn unmount_native_views(widget: &mut dyn Widget) {
    widget.unmount_native_views();
}

/// Begin a frame through each owner's retained lifecycle topology.
pub(crate) fn native_view_frame_start(widget: &mut dyn Widget) {
    widget.native_view_frame_start();
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

    /// Insert a leaf node for `widget` and register it in `node_map`: the
    /// whole of a leaf widget's `build_layout`.
    fn widget_leaf(
        &mut self,
        style: taffy::Style,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        widget: &mut (dyn Widget + 'static),
    ) -> NodeId {
        let node = self.leaf_node(style);
        node_map.insert(node, std::ptr::from_mut(widget));
        node
    }

    /// [`Self::widget_leaf`] for a leaf that fills its parent's box: a
    /// canvas, viewport or overlay that sizes and lays out its own content.
    fn fill_leaf(
        &mut self,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        widget: &mut (dyn Widget + 'static),
    ) -> NodeId {
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        self.widget_leaf(style, node_map, widget)
    }

    /// Insert a parent node over `children` for `widget` and register it in
    /// `node_map`: how a container's `build_layout` ends.
    fn widget_node(
        &mut self,
        style: taffy::Style,
        children: &[NodeId],
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        widget: &mut (dyn Widget + 'static),
    ) -> NodeId {
        let node = self.parent_node(style, children);
        node_map.insert(node, std::ptr::from_mut(widget));
        node
    }

    /// Insert a flex parent that fills its own parent around one child: the
    /// root node of a window that hosts a single widget tree.
    fn fill_node(&mut self, child: NodeId) -> NodeId {
        self.parent_node(
            taffy::Style {
                display: taffy::Display::Flex,
                flex_grow: 1.0,
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[child],
        )
    }
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

pub mod binding;
pub mod capability;
mod composed;
pub use composed::Composed;
pub mod controls;
pub mod decorator;
pub mod document_effects;
pub use crate::pen::PenSample;
pub use decorator::Decorator;

pub(crate) mod layout;
pub use layout::{
    AuthoredExtent, ExtentSizing, WidgetLayoutError, compute_widget_layout, insets, insets_all,
    insets_xy, layout_clip, text_control_height, with_layout_clip,
};

mod button;
pub use button::{Button, ButtonFace, ButtonInteract, ButtonRole, face_colors};
mod control_size;
pub use control_size::ControlSize;
mod bool_control;
mod checkbox;
pub use checkbox::{Checkbox, CheckboxFace, CheckboxInteract};
mod column;
pub use column::Column;
mod composition;
mod composition_parts;
mod flow;
mod form;
mod form_composition;
mod layout_info;
pub use form::{CheckControl, FormControl, labeled_check, labeled_field};
pub use form_composition::{Field, Form, FormPage, FormSection, Page};
pub use layout_info::{
    Density, Effective, FieldLayout, FormLayout, LabelPlacement, LayoutInfo, LayoutOrigin,
    ManualConflict, ManualLayout, PageLayout, SectionLayout,
};
mod embedded;
pub use embedded::Embedded;
mod manual;
pub use manual::{
    ChildMeasurement, Manual, ManualContext, ManualInvalidation, ManualPolicy, MeasureConstraint,
};
mod row;
pub use row::Row;
mod toggle;
pub use toggle::{
    Toggle, ToggleColors, ToggleFace, ToggleInteract, toggle_accessibility, toggle_face_colors,
};
mod stack;
pub use stack::Stack;
mod container;
pub use container::Container;
mod router;
pub use router::{RouteError, RouteHandle, RouteSnapshot, Router};
mod document_view;
pub use document_view::ModelView;
mod divider;
pub use divider::{Divider, DividerAxis};
mod link;
pub use link::{Link, os_open as open_external_url, try_os_open as try_open_external_url};
mod flap;
pub use flap::{FlapState, SplitLayout};
mod sidebar;
pub use sidebar::{
    DEFAULT_DRAWER_BREAKPOINT, DEFAULT_DRAWER_DOCKED_WIDTH, DEFAULT_DRAWER_WIDTH, SidebarDrawer,
    SidebarSide,
};
mod nav_shell;
pub mod window_header;
pub use nav_shell::{
    InspectorPlacement, NavGeometry, NavMode, NavPane, NavShell, OverviewPlacement,
    SidebarPlacement,
};
pub use window_header::{HISTORY_CONTROL_ID, HistoryControl, HistoryOpener, WindowHeader};
mod nav_list;
pub use nav_list::{NavAction, NavDrop, NavItem, NavItemStatus, NavList, NavSection};
mod document_viewport;
pub use document_viewport::{
    ContentDraw, DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_PADDING, DEFAULT_PAN_STEP,
    DocumentViewport, PAN_SLOP, SmallContentAlign, ViewportState,
};
mod floating_window;
pub use floating_window::{FloatingWindow, MIN_HEIGHT, MIN_WIDTH, TITLE_BAR_HEIGHT};
mod text;
pub use text::Text;
mod spacer;
pub use spacer::Spacer;
mod when;
pub use when::{Absent, When, optional};
mod slot;
pub use slot::{Slot, SlotHandle, Switch};
mod columns;
pub use columns::Columns;
mod image;
pub use image::{Image, ImageDecodeError, ImageFit};
mod image_crop;
pub use image_crop::{CropSelection, ImageCropEditor};
mod slider;
pub use slider::{Slider, SliderFace};
mod text_input;
pub use text_input::{
    AccessoryAction, FieldFace, SpanHighlighter, TextActionRequest, TextInput, TextInputKind,
    paint_preedit_underline,
};
pub mod code_editor;
pub use code_editor::{CodeEditor, Highlighter, HlState, Language, LineSpans, Span, TokenKind};
mod editable_label;
pub use editable_label::{EditOutcome, EditableLabel};
pub mod autocomplete;
pub use autocomplete::{Autocomplete, AutocompleteOutcome, Suggester, Suggestion, SuggestionList};
mod number_input;
pub use number_input::{NumberInput, NumberStepperFace, StepperPart};
pub mod marks;
pub use marks::{Mark, MarkSpan};
pub mod rich_text_line;
pub use rich_text_line::{LineSignal, RichTextLine};
pub mod block_editor;
pub mod paint_damage;
pub use block_editor::{
    Block, BlockContent, BlockEditOp, BlockEditor, BlockKind, CursorPlacement, DocumentPosition,
    DocumentSelection, EditScope, PresenceKeyFn, TransclusionPreview, legacy_block_id,
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
pub use dropdown::{Dropdown, DropdownFace, FILTER_MIN_OPTIONS};
pub mod tabs;
pub use tabs::{TabBar, TabDescriptor, TabMark, draw_app_mark};
pub mod scrollbar;
pub use scrollbar::{
    PressOutcome, PrimaryScrollAction, PrimaryScrollRegion, PrimaryScrollResult, ScrollMetrics,
    Scrollbar, TrackMark,
};
pub mod date_indexed_scrollbar;
pub use date_indexed_scrollbar::{
    DateIndexIntent, DateIndexMarker, DateIndexMarkerKind, DateIndexPreview, DateIndexedScrollbar,
    ProjectedDateMarker,
};
pub mod drag;
pub use drag::{Axis as DragAxis, DragController, ListMetrics, REORDER_THRESHOLD_PX};
mod scrollable;
pub use scrollable::{ScrollAxis, Scrollable};
mod scroll_region;
pub use scroll_region::ScrollRegion;
pub use scrollbar::BarFade;
pub mod panic_scope;
mod tile_host;
pub use tile_host::{AppKind, TileContent, TileHost};
mod app_widget;
pub(crate) use app_widget::BoundAppWidget;
pub use app_widget::{
    AppCommand, AppHostIntent, AppWidget, DeviceDisplayMoveReply, DocumentOpenCapability,
    DocumentOpenOutcome, DocumentOpenReply, HostShellAllow, IconDescriptor,
};
pub mod command_palette;
pub mod input_hint;
pub mod shortcut_recorder;
pub use input_hint::{InputDeviceGlyph, InputHint, LegendConventions};
pub mod canvas_navigation_hint;
pub use canvas_navigation_hint::{CanvasNavigation, CanvasNavigationHint};
pub use shortcut_recorder::ShortcutRecorder;
pub mod context_menu;
pub mod floating_surface;
pub use context_menu::{ContextMenu, MenuButton, MenuItem};
pub mod context_menu_area;
pub use context_menu_area::{ContextMenuArea, MenuEntry};
pub mod data_table;
mod virtual_table_semantics;
pub use virtual_table_semantics::{
    TableSemanticCell, TableSemanticColumn, TableSemanticControl, TableSemanticRow,
    VirtualTableSemantics,
};
mod virtual_list_semantics;
pub use virtual_list_semantics::{ListSemanticRow, VirtualListSemantics};
pub(crate) mod block_editor_table;
pub mod table {
    pub use super::block_editor_table::*;
}
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
    CrossStripDetach, GroupRow, GroupTopology, JUMP_PX_PER_DETENT, OutputSpan, Pan, StripLane,
    TabDropTarget, WheelJumpAccum, pan_within_canvas, wheel_jump_detents,
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
pub use progress_bar::{ProgressBar, ProgressBarFace};
pub mod page_preview;
pub use page_preview::PagePreview;
pub mod spinner;
pub use spinner::Spinner;
pub mod date_picker;
pub use date_picker::{DatePicker, ExternalDatePickerHit};
pub mod time_picker;
pub mod timeline_density;
pub use time_picker::{TimePicker, TimeValue};
pub use timeline_density::TimelineDensity;
pub mod color_picker;
pub mod color_swatch;
pub use color_picker::{ColorPicker, ColorPickerOutcome};
pub use color_swatch::{ColorSwatch, ColorSwatchFace, ColorSwatchPicker, SwatchPainter};
pub mod popover;
pub use popover::{PanelAlign, Popover, PopoverButton, PopoverPrompt, PopoverSide};
pub mod modal_dialog;
pub use modal_dialog::{
    ModalAction, ModalDialog, ModalEmphasis, ModalHit, ModalLayout, ModalSheet, ModalSpec,
    SheetResponse,
};
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
pub use avatar::{Avatar, AvatarFace, AvatarSize, PresenceStatus, initials_for_name};
pub mod badge;
pub use badge::{Badge, BadgeContent, BadgeFace};
pub mod icon;
pub use icon::{IconSize, IconWidget};
pub mod chart;
pub use chart::{
    Chart, ChartLabels, ChartStyle, RollingSeries, Series, Sparkline, TrendCurve, TrendFill,
    TrendRange, TrendStyle,
};
pub(crate) mod chart_labels;
pub mod chart_legend;
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
pub use gauge::{
    BarGauge, GaugeReading, GaugeStyle, GaugeThresholds, ProgressRing, ProgressRingFace,
    ProgressRingSize, RingReading,
};
pub mod function_plane;
pub use function_plane::FunctionPlane;
pub mod resizable_split;
pub use resizable_split::{ResizableSplit, SplitHandle, SplitOrientation, SplitRatio};
pub mod video_player;
pub use video_player::VideoPlayer;
pub mod webview;
pub use webview::WebView;
pub mod image_viewer;
pub use image_viewer::ImageViewer;
pub mod tree_view;
pub use tree_view::{
    TreeChildState, TreeClickActivation, TreeHeadingAction, TreeLoadReason, TreeLoadRequest,
    TreeModel, TreeModelChange, TreeMove, TreeNode, TreeRowAction, TreeRowDecoration, TreeRowIcon,
    TreeRowInfo, TreeRowKey, TreeRowTone, TreeSlot, TreeView,
};
pub mod virtual_list;
pub use virtual_list::{
    DuplicateListKey, ListModel, ListRowState, RowSelection, VecListModel, VirtualList,
};
mod activation;
pub use activation::{Interaction, PaintedTargets, paint_focus_ring};
pub mod media_card;
pub use media_card::{
    ArtPainter, MediaArt, MediaAspect, MediaBadge, MediaCard, MediaCardFace, MediaGrid, MediaSize,
    MediaSource, OwnedBadge, justified_rows,
};
pub mod capture_button;
pub use capture_button::{CaptureButtonFace, CaptureKind};
pub mod transport;
pub use transport::{TransportControl, TransportFace};
pub mod drop_zone;
pub use drop_zone::{DropZone, DropZoneFace};
pub mod skeleton;
pub use skeleton::{Skeleton, SkeletonFace, SkeletonShape, skeleton_tone};
pub mod stepper;
pub use stepper::{Stepper, StepperFace};
pub mod property_list;
pub use property_list::{
    Property, PropertyColumn, PropertyList, PropertyListFace, PropertyRow, PropertyValue,
    property_label_column,
};
pub mod list_row;
pub use list_row::{
    ControlRow, ListRow, ListRowFace, RowAction, RowLeading, RowStatus, RowValue, SwipeAction,
    badge_column_width,
};
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
pub use toggle_group::{
    Segment, SegmentedFace, SegmentedStyle, SegmentedTouch, SegmentedTouchGroup,
    SegmentedTouchResult, ToggleGroup, segment_colors,
};

// EmptyState / Banner / StatTile also expose an immediate-mode `paint(...)`
// entry point for apps that render straight to a `Scene` rather than a
// widget tree.
pub mod empty_state;
pub use empty_state::{EmptyState, EmptyStateContent, EmptyStateLayout};
pub mod find_bar;
pub use find_bar::{
    FindBar, FindMatch, FindMode, FindOptions, FindScope, Findable, paint_find_match, with_findable,
};
pub mod search_field;
pub use search_field::{FilterCount, Filterable, Filtered, SearchField, apply_filter, sync_filter};
pub mod search_results;
pub use search_results::SearchResultsView;
pub mod stat_tile;
pub use stat_tile::{StatGroup, StatTile, StatTileFace, Trend};
pub mod banner;
pub use banner::{Banner, BannerContent, BannerHit, BannerKind, BannerNotice};
pub mod chip;
pub use chip::{Chip, ChipColors, ChipFace, ChipLead, ChipStyle};
pub mod card;
mod tone;
pub use tone::Tone;
pub mod collaboration_overview;
pub use card::{Actions, Card, CardFace, CardGrid, CardWidth, card_column_width, card_columns};
mod section;
pub use section::{ObjectHeader, Section, SectionHeader, SectionHeaderFace, StatusLine};
pub mod capture_studio;
pub use capture_studio::{
    AudioCaptureMode, CaptureDestination, CaptureExtent, CaptureIntent, CaptureMode,
    CapturePreview, CaptureStudio, CaptureTarget, LiveCapture, PipLayout, RecordingCommand,
    ReplayCommand, ScreenshotCommand, ScreenshotSendDevice, ScreenshotSendTarget, SourceMode,
};
pub mod screenshot_markup;
pub mod selection_handles;
pub use screenshot_markup::{
    MarkupExport, MarkupStroke, MarkupTool, ScreenshotMarkup, ScreenshotMarkupCommand,
};
pub use selection_handles::{Handle as SelectionHandle, SelectionHandles};

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
pub use identity_flow::{IdentityFlow, IdentityFlowScreen, ScreenOutcome};
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
        events: std::rc::Rc<std::cell::Cell<u32>>,
        child: Option<Box<dyn Widget>>,
    }

    impl Collaborative for CollabProbe {
        fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
            vec![crate::collaboration::CollabRequest::ClearContinuation {
                app_id: "probe".into(),
            }]
        }

        fn handle_collab_event(&mut self, _: &crate::collaboration::CollabEvent) {
            self.events.set(self.events.get().saturating_add(1));
        }
    }

    impl AudiencePresentationProvider for CollabProbe {
        fn on_audience_presentation_changed(&mut self, _active: bool) {
            self.events.set(self.events.get().saturating_add(1));
        }
    }

    impl Widget for CollabProbe {
        crate::provides!(dyn Collaborative, dyn AudiencePresentationProvider);

        fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            _: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.leaf_node(taffy::Style::default())
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

    #[test]
    fn widget_trait_defaults_and_native_input_probe_precedence() {
        fn ordinary_window_close_defaults_to_immediate_exit() {
            let mut spacer = Spacer::new();
            assert_eq!(
                crate::component_lifecycle::on_window_close_requested(&mut spacer),
                WindowCloseAdmission::Allow
            );
            assert!(!crate::component_lifecycle::take_window_close_intent(
                &mut spacer
            ));
        }

        fn native_input_snapshot_defaults_to_none() {
            let state = crate::State::new();
            assert!(
                Spacer::new()
                    .native_input_snapshot(&state.doc, &state.root)
                    .is_none()
            );
        }

        fn native_input_snapshot_root_fallback_and_explicit_probe_precedence() {
            struct Observed(std::rc::Rc<std::cell::Cell<usize>>);
            impl Widget for Observed {
                fn native_input_snapshot(
                    &self,
                    doc: &crate::widget::AppModel,
                    path: &crate::widget::ModelPath,
                ) -> Option<serde_json::Value> {
                    use automerge::ReadDoc;
                    let (doc, path) = crate::doc::document_ref(doc, path);
                    self.0.set(self.0.get() + 1);
                    Some(serde_json::json!({"keys": doc.keys(path).collect::<Vec<_>>() }))
                }
                fn build_layout(
                    &mut self,
                    _: &mut TaffyTree<()>,
                    _: &Theme,
                    _: &mut HashMap<NodeId, *mut dyn Widget>,
                ) -> NodeId {
                    unreachable!()
                }
                fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: vello::kurbo::Point) {
                    unreachable!()
                }
                fn draw(
                    &mut self,
                    _: &mut Scene,
                    _: vello::kurbo::Affine,
                    _: &mut TextRenderer,
                    _: &Theme,
                    _: &PresenceCtx,
                ) {
                    unreachable!()
                }
                fn handle_event(
                    &mut self,
                    _: &WindowEvent,
                    _ctx: &mut crate::widget::EventCtx<'_>,
                ) {
                    unreachable!()
                }
                fn name(&self) -> &str {
                    "Observed"
                }
                fn get_rect(&self) -> vello::kurbo::Rect {
                    vello::kurbo::Rect::ZERO
                }
            }
            use automerge::transaction::Transactable;
            let mut state = crate::State::new();
            state
                .doc
                .put(&state.root, "fixture", "actual state")
                .unwrap();
            let calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut app = crate::App::new(state, Box::new(Observed(calls.clone())));
            assert_eq!(
                app.native_input_snapshot().unwrap()["keys"],
                serde_json::json!(["fixture"])
            );
            assert_eq!(calls.get(), 1);
            app.set_native_input_probe(|| serde_json::json!({"explicit": true}));
            assert_eq!(
                app.native_input_snapshot().unwrap(),
                serde_json::json!({"explicit": true})
            );
            assert_eq!(
                calls.get(),
                1,
                "explicit probe must suppress root observation"
            );
        }

        /// Pinned so a future edit that removes the default blanket-breaks
        /// every widget impl that hasn't opted into a stable test id.
        fn widget_test_id_default_is_none() {
            let s = Spacer::new();
            assert!(
                Widget::test_id(&s).is_none(),
                "default Widget::test_id should be None"
            );
        }

        ordinary_window_close_defaults_to_immediate_exit();
        native_input_snapshot_defaults_to_none();
        native_input_snapshot_root_fallback_and_explicit_probe_precedence();
        widget_test_id_default_is_none();
    }

    #[test]
    fn widget_capabilities_forward_through_the_tree_and_emitters_stay_bounded() {
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

        fn audience_presentation_capability_forwards_through_dyn_widget() {
            let events = Rc::new(Cell::new(0));
            let mut widget: Box<dyn Widget> = Box::new(CollabProbe {
                events: Rc::clone(&events),
                child: None,
            });

            widget.on_audience_presentation_changed(true);
            assert_eq!(events.get(), 1);
        }

        fn action_emitter_backlog_is_bounded_and_keeps_the_newest_intent() {
            let mut emitter = ActionEmitter::new(test_bool_action_binding());
            for _ in 0..ACTION_EMITTER_QUEUE_LIMIT.saturating_add(1) {
                emitter.emit(false).expect("valid boolean intent");
            }
            emitter.emit(true).expect("valid boolean intent");
            let mut count = 0;
            let mut last = None;
            while let Some(invocation) = emitter.take() {
                count += 1;
                last = invocation.arguments.get("value").cloned();
            }
            assert_eq!(count, ACTION_EMITTER_QUEUE_LIMIT);
            assert_eq!(last, Some(serde_json::Value::Bool(true)));
        }

        collaboration_capabilities_are_discovered_through_the_widget_tree();
        audience_presentation_capability_forwards_through_dyn_widget();
        action_emitter_backlog_is_bounded_and_keeps_the_newest_intent();
    }

    #[test]
    fn collaboration_capabilities_visit_parent_and_child_exactly_once() {
        let events = std::rc::Rc::new(std::cell::Cell::new(0));
        let child = CollabProbe {
            events: std::rc::Rc::clone(&events),
            child: None,
        };
        let mut root = CollabProbe {
            events: std::rc::Rc::clone(&events),
            child: Some(Box::new(child)),
        };

        assert_eq!(take_collab_requests(&mut root).len(), 2);
        handle_collab_event(
            &mut root,
            &crate::collaboration::CollabEvent::FollowChanged { peer_id: None },
        );
        assert_eq!(events.get(), 2);
    }
}

#[cfg(test)]
pub(crate) mod control_result_probe {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    pub(crate) struct Observed {
        pub(crate) results:
            Rc<RefCell<Vec<Result<ResourceOperationStart, tilo_actions::DoorError>>>>,
        pub(crate) calls: Rc<Cell<usize>>,
        pub(crate) polls: Rc<Cell<usize>>,
        pub(crate) commits: Rc<Cell<usize>>,
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
        reason = "independent test switches exercise acceptance, confirmation and typing"
    )]
    pub(crate) struct Probe {
        observed: Observed,
        accepted: bool,
        pub(crate) confirmation: bool,
        pub(crate) terminal_after: Option<(usize, bool)>,
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
                    dirty_typing: false,
                    typed_text: false,
                },
                observed,
            )
        }
    }
    impl Widget for Probe {
        crate::provides!(
            dyn crate::widget::controls::ActionControls,
            dyn crate::resources::PublishesResources
        );

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
        fn name(&self) -> &'static str {
            "ControlResultProbe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            vello::kurbo::Rect::ZERO
        }
        fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
            &["test.control-result.run"]
        }
        fn start_operation(
            &mut self,
            op: &ResourceOperation<'_>,
            _: &mut crate::widget::Edits<'_>,
            _: &ObjId,
        ) -> Result<Option<ResourceOperationStart>, String> {
            let Some(request) = op.unscoped() else {
                return Ok(None);
            };
            if let Ok(text) = serde_json::from_slice::<String>(request.payload) {
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

    impl crate::resources::PublishesResources for Probe {
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
    }

    impl crate::widget::controls::ActionControls for Probe {
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

pub mod keyboard_map;
pub use keyboard_map::{KeyboardMap, KeyboardMapInput, KeyboardMapKey, KeyboardMapSelection};
/// Logical screen topology canvas shared by local and native peer displays.
pub mod display_arrangement;
mod graph_canvas;
pub use graph_canvas::{GraphCanvas, GraphCanvasConnection, GraphCanvasNode};
