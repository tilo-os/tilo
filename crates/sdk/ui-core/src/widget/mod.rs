mod action_directory_identity;
mod disclosing;
pub use action_directory_identity::ActionDirectoryInvocationDomain;
pub use disclosing::{Disclosing, Placement, event_offset};
pub(crate) use disclosing::{
    Ghosts, KeptBand, content_box, follower_shifts, place_bands, route_to_shifted_children,
};
pub(crate) mod resource_operation;
// The raw operation protocol between the host and the toolkit's own widgets,
// reached from outside the toolkit only through `crate::host_operation`.
pub use resource_operation::resource_operation_handlers;
pub(crate) use resource_operation::start_without_document_writes;
pub(crate) use resource_operation::{
    ResourceOperation, ResourceOperationStart, ResourceOperations,
    local_resource_operation_handlers, poll_resource_operation, start_in_children, start_operation,
};

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
    let mut bindings = Vec::new();
    if let Some(semantics) = semantics {
        semantic_action_bindings_into(&semantics, &mut bindings);
    }
    bindings
}

/// [`semantic_action_bindings`] of `node`, appended to `out`: read in place,
/// so a widget's kept rows are listed without copying them.
pub(crate) fn semantic_action_bindings_into(
    node: &crate::a11y::AccessNode,
    out: &mut Vec<ControlBindingMetadata>,
) {
    fn is_effect_role(role: crate::a11y::Role) -> bool {
        use crate::a11y::Role;
        matches!(
            role,
            Role::Button
                | Role::Tab
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

    if is_effect_role(node.role)
        && let Some(target) = &node.action_target
    {
        out.push(ControlBindingMetadata::semantic(target.clone()));
    }
    for child in &node.children {
        semantic_action_bindings_into(child, out);
    }
}

/// How readily `widget` takes the keyboard when its app does without a
/// control of its own choosing (brought forward on the desktop, or used
/// from the window's bar): its default keyboard control first (`Some(0)`),
/// else one that takes typing, a field, an editor or a terminal
/// (`Some(1)`). Never a button merely for being first (`None`): Enter
/// would press it.
pub(crate) fn keyboard_entry_rank(widget: &dyn Widget) -> Option<u8> {
    if widget.is_default_keyboard_focus() {
        return Some(0);
    }
    widget
        .accessibility()
        .is_some_and(|access| {
            matches!(
                access.role,
                crate::a11y::Role::TextInput
                    | crate::a11y::Role::MultilineTextInput
                    | crate::a11y::Role::Document
                    | crate::a11y::Role::Terminal
            )
        })
        .then_some(1)
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

/// How many frames a root may run in one host frame to settle a sync or
/// layout its previous frame left: a widget that measures itself in one
/// layout pass (a column centred on the width it was given, a header whose
/// height insets what follows) reports its structure dirty and is built
/// again at once, so what is painted is the root at rest. An embedded app
/// settles this way, and so does a History preview of a past version.
pub(crate) const SETTLE_FRAMES: usize = 3;

/// `node`'s laid-out box placed at `origin`: what `apply_layout` stores as a
/// leaf's rect. A node missing from the tree is an empty box at `origin`.
pub fn layout_rect(
    taffy: &TaffyTree<()>,
    node: NodeId,
    origin: vello::kurbo::Point,
) -> vello::kurbo::Rect {
    placed(&taffy.layout(node).copied().unwrap_or_default(), origin)
}

/// Solve the layout of the tree under `root` within `available`: what
/// `TaffyTree::compute_layout` does, for a widget or an app that lays a
/// panel out in a tree of its own.
///
/// Call this rather than the tree's own method. That method is generic, so
/// every app calling it compiled Taffy's flexbox, grid and block solvers
/// again (about 110 KB); this one is compiled once, into the toolkit.
///
/// # Errors
///
/// Returns the solver's error when Taffy rejects the tree.
pub fn solve_layout(
    taffy: &mut TaffyTree<()>,
    root: NodeId,
    available: taffy::Size<taffy::AvailableSpace>,
) -> Result<(), taffy::TaffyError> {
    taffy.compute_layout(root, available)
}

/// Implements [`Widget::apply_layout`] and [`Widget::get_rect`] for a widget
/// that keeps the box layout gives it in `self.rect`.
#[macro_export]
macro_rules! rect_geometry {
    () => {
        fn apply_layout(
            &mut self,
            taffy: &$crate::taffy::TaffyTree<()>,
            node: $crate::taffy::NodeId,
            origin: $crate::kurbo::Point,
        ) {
            self.rect = $crate::widget::layout_rect(taffy, node, origin);
        }

        fn get_rect(&self) -> $crate::kurbo::Rect {
            self.rect
        }
    };
}

/// Implements the [`Widget`] methods of a composite that is one pressable
/// target, from its `self.activation`: focusable once bound, a pointer
/// cursor over the rect at `self.$rect`, and accessibility `Activate`
/// firing the binding. A focus change sets `self.dirty`, so pair it with
/// [`redraw_flag!`].
macro_rules! pressable {
    ($($rect:ident).+) => {
        fn cursor_shape(
            &self,
            point: $crate::kurbo::Point,
        ) -> Option<winit::window::CursorIcon> {
            (self.activation.is_bound() && self.$($rect).+.contains(point))
                .then_some(winit::window::CursorIcon::Pointer)
        }

        fn is_focusable(&self) -> bool {
            self.activation.is_bound()
        }

        fn set_focus(&mut self, focused: bool) {
            if self.activation.state.focused != focused {
                self.activation.state.focused = focused;
                self.dirty = true;
            }
        }

        fn is_focused(&self) -> bool {
            self.activation.state.focused
        }

        fn perform_action(
            &mut self,
            action: &$crate::a11y::Action,
            _key_path: &[u64],
            _state: &mut $crate::widget::Edits<'_>,
            _root: &$crate::widget::ModelPath,
        ) -> bool {
            matches!(action, $crate::a11y::Action::Activate) && self.activation.activate()
        }
    };
}
pub(crate) use pressable;

/// Implements [`Widget::needs_redraw`] and [`Widget::mark_drawn`] for a
/// widget that tracks whether it needs repainting in `self.dirty`.
#[macro_export]
macro_rules! redraw_flag {
    () => {
        fn needs_redraw(&self) -> bool {
            self.dirty
        }

        fn mark_drawn(&mut self) {
            self.dirty = false;
        }
    };
}

/// Let `node` share its row's main axis with its siblings by `grow`: it
/// grows and shrinks from a zero basis instead of keeping its own width.
pub fn share_row(taffy: &mut TaffyTree<()>, node: NodeId, grow: f32) {
    if let Ok(mut style) = taffy.style(node).cloned() {
        style.flex_grow = grow;
        style.flex_shrink = 1.0;
        style.flex_basis = taffy::style::Dimension::length(0.0);
        style.size.width = taffy::style::Dimension::auto();
        let _ = taffy.set_style(node, style);
    }
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

/// Draw `child` unless what it paints misses the viewport. Off-viewport
/// widgets stay dirty, so they re-emit when scrolled back in.
///
/// A widget ends every layer it begins: the render service refuses a scene
/// that leaves a clip open, and with it the whole frame. Debug builds check
/// that here, naming the widget, so a test that draws it fails.
pub fn draw_child(
    child: &mut (impl Widget + ?Sized),
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    if !presence.should_cull(child.paint_bounds()) {
        #[cfg(debug_assertions)]
        let open = scene.encoding().n_open_clips;
        child.draw(scene, transform, text_renderer, theme, presence);
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            scene.encoding().n_open_clips,
            open,
            "{} left a layer open (or ended one it did not begin) in its draw",
            child.concrete_type_name()
        );
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
) {
    let rect = widget.get_rect();
    if let Some(scope) = crate::permissions::capability::permission_scope(widget)
        && !presence.permits(&scope)
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

/// The first widget of `root`'s subtree, `root` included, whose
/// [`Widget::test_id`] is `id`.
pub fn find_test_id<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
    if root.test_id() == Some(id) {
        return Some(root);
    }
    root.children()
        .into_iter()
        .find_map(|child| find_test_id(child, id))
}

/// [`find_test_id`] for a widget to change.
pub fn find_test_id_mut<'a>(root: &'a mut dyn Widget, id: &str) -> Option<&'a mut dyn Widget> {
    if root.test_id() == Some(id) {
        return Some(root);
    }
    root.children_mut()
        .into_iter()
        .find_map(|child| find_test_id_mut(child, id))
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

/// What a structural change a subtree reported ([`Widget::take_structure_change`])
/// asks of its host. Ordered: a change joined with a larger one is the larger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum StructureChange {
    /// Nothing was inserted or removed.
    #[default]
    None,
    /// The widgets that reported it mount and unmount children of their own,
    /// and every widget stays alive: they carry it out in the retained tree,
    /// in place ([`Widget::remount_children`]), and their own damage this
    /// frame covers every pixel that changed with it. A host that cannot
    /// rebuilds its tree, as for [`Self::Described`].
    Children,
    /// The host rebuilds its tree, but the frame's damage still describes it:
    /// the widget that reported it replaced only widgets of its own, and its
    /// own damage this frame covers every pixel that changed with them.
    Described,
    /// The host rebuilds its tree, and any widget may have been replaced:
    /// the frame repaints whole.
    Rebuild,
}

impl StructureChange {
    /// A structural flag as a change: any change is a [`Self::Rebuild`].
    #[must_use]
    pub fn of(dirty: bool) -> Self {
        if dirty { Self::Rebuild } else { Self::None }
    }

    /// Whether the host rebuilds its tree for it, or (for
    /// [`Self::Children`]) changes it in place.
    #[must_use]
    pub fn rebuilds(self) -> bool {
        self != Self::None
    }

    /// This change and `other`, as one.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        self.max(other)
    }

    /// Every one of `children`'s changes, joined: for a container that adds
    /// no structure of its own, so a change a child carries out in place
    /// ([`Self::Children`]) stays one. Each child's change is taken, none
    /// skipped.
    #[must_use]
    pub fn of_children(children: Vec<&mut (dyn Widget + '_)>) -> Self {
        children.into_iter().fold(Self::None, |change, child| {
            change.join(child.take_structure_change())
        })
    }
}

/// Optional cross-device continuation capability for an app root.
///
/// Continuation is deliberately separate from [`Widget`]: ordinary controls
/// have no resumable app state, while roots that do can expose this capability
/// through [`Widget::provide`] without weakening their UI integration.
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

    /// Keep the exact secondary presentation's editor logically focused while
    /// the platform deliberately retains text-input focus on its source.
    ///
    /// This is a host-authenticated input route, not a claim made by the app.
    /// The host clears it when the continuation ends or no longer validates.
    fn on_floating_surface_ime_source_changed(&mut self, _id: Option<&str>) {}

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

    /// IME content type and desktop-logical caret rect for one secondary window.
    fn floating_surface_ime(
        &self,
        _id: &str,
    ) -> Option<(crate::ime::InputContent, vello::kurbo::Rect)> {
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

    /// Paint exactly one app-owned presentation into its native window.
    ///
    /// Native secondary windows are independent buffers. Painting the plural
    /// overlay pass into each one can duplicate sibling surfaces and lets
    /// content escape the window's own clip.
    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool {
        let presentations = self.floating_surface_presentations();
        if presentations.len() != 1 || presentations[0].id != id {
            // A plural paint pass cannot select one of several owners safely.
            // Multi-surface providers must implement the exact-id pass.
            return false;
        }
        // Existing single-surface providers retain their normal paint pass.
        // Multi-surface providers override this to select the exact owner.
        self.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        true
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
pub(crate) fn resource_presentation_epoch() -> u64 {
    RESOURCE_PRESENTATION_EPOCH.with(std::cell::Cell::get)
}

thread_local! {
    static VIEW_ONLY_PAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tell the host that the wheel event being dispatched was consumed only to
/// move a scroll container's view: every widget under the pointer refused
/// it, and the container took it as a pan of its own offset. The host then
/// keeps the published resource tree and skips the full sync, exactly as
/// for a root that declares [`Widget::view_only_scroll`], provided the
/// dispatch changed no document, focus or declared presentation. A scroll
/// container that does anything else with the wheel must not call this.
pub fn note_view_only_pan() {
    VIEW_ONLY_PAN.with(|pan| pan.set(true));
}

/// Whether [`note_view_only_pan`] ran since the last call, clearing it.
pub(crate) fn take_view_only_pan() -> bool {
    VIEW_ONLY_PAN.with(|pan| pan.replace(false))
}

thread_local! {
    static PLACEMENT_MOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// From a widget's `update`: it moved what it lays out with no input asking
/// (a glide, a jump in progress, a give springing back), so its children
/// stand where the last layout put them. The host places its widgets again
/// in the frame; it is no change of the model, and nothing syncs.
pub fn note_placement_moved() {
    PLACEMENT_MOVED.with(|moved| moved.set(true));
}

/// Whether [`note_placement_moved`] ran since the last call, clearing it.
pub(crate) fn take_placement_moved() -> bool {
    PLACEMENT_MOVED.with(|moved| moved.replace(false))
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

    /// `position`, a pointer position in physical pixels, in this event's
    /// logical coordinates.
    #[must_use]
    pub fn logical(
        &self,
        position: impl std::borrow::Borrow<winit::dpi::PhysicalPosition<f64>>,
    ) -> vello::kurbo::Point {
        let position = position.borrow();
        vello::kurbo::Point::new(
            position.x / self.scale_factor,
            position.y / self.scale_factor,
        )
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
    /// ([`crate::collaboration::participants::Participants::set_participant_label`]).
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
    /// A [`crate::record::capability::RecordScoped::record_intent`] alone does not qualify: the runner (or the
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
        defaults::visit_components(self.children_mut(), request)
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
        defaults::visit_resource_owners_mut(self.children_mut(), request)
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
        defaults::mount_native_views(self.children_mut(), host);
    }

    /// Tear native children down while their parent window still exists.
    fn unmount_native_views(&mut self) {
        defaults::unmount_native_views(self.children_mut());
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.unmount_native_view();
        }
    }

    /// Begin a native-view frame through this owner's retained lifecycle topology.
    fn native_view_frame_start(&mut self) {
        if let Some(lifecycle) = capability::capability_mut::<dyn NativeViewLifecycle>(self) {
            lifecycle.native_view_frame_start();
        }
        defaults::native_view_frame_start(self.children_mut());
    }

    /// Receive host-selected platform conventions. The default propagates to
    /// retained children, keeping target-OS branches out of app code.
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        defaults::set_platform_conventions(self.children_mut(), conventions);
    }

    /// Receive the host's current input capabilities. The default propagates
    /// through retained containers, so app code does not need device branches.
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        defaults::set_interaction_environment(self.children_mut(), environment);
    }

    /// Pull model state into this widget before layout. The default syncs the
    /// children [`Widget::children_mut`] returns; a widget overrides it only to
    /// read state of its own, or to sync children it does not expose there.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        defaults::sync(self.children_mut(), state, path);
    }

    /// Notice, between frames, state that lives outside the document (a
    /// chrome revision, a momentum scroll still ticking) and report whether
    /// anything changed. The default updates every child
    /// [`Widget::children_mut`] returns, without short-circuiting, so a
    /// container never freezes its descendants at build-time state; a widget
    /// overrides it to check state of its own and then forward.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        defaults::update(self.children_mut(), state, path)
    }

    /// Apply, after the sync pass, what the widget took up there and must
    /// write: a worker's answer, a chooser's file, an event a child queued.
    /// Writes go only through typed actions on `edits` (a resource's
    /// dispatcher); the host syncs again when the document changed. The
    /// default settles the children [`Widget::children_mut`] returns.
    fn settle(&mut self, edits: &mut Edits<'_>, path: &ObjId) {
        defaults::settle(self.children_mut(), edits, path);
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

    /// Adjust this node's style using its allocated size, before final
    /// geometry is applied. Return true only when the style changed (or an
    /// inherited decision child hooks read). An override may measure its owned
    /// children through ordinary references, entering their callback binding
    /// when present. Never reenter descendants through the raw node map while
    /// borrowing their parent, change topology, or build another layout tree.
    /// The shared host solver visits every registered widget, repeats layout
    /// at most eight times and skips a frame that does not settle.
    fn adjust_layout_style(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        false
    }

    /// Take the geometry layout gave this node. The default keeps none, for
    /// widgets that neither draw nor hit-test.
    fn apply_layout(
        &mut self,
        _taffy: &TaffyTree<()>,
        _node: NodeId,
        _origin: vello::kurbo::Point,
    ) {
    }
    /// Paint into `scene`. The default paints nothing.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

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
        defaults::for_each_child(self.children(), visitor);
    }
    /// The children of [`Self::for_each_child`] that may paint inside the
    /// [`layout_clip`] in effect: what a scroll view's damage and its copy
    /// plan need to look at. Every child by default; a long column visits
    /// only those near the clip, so a frame of a scroll costs what is shown,
    /// not what the column holds.
    fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.for_each_child(visitor);
    }
    /// Every child this widget's [`Self::build_layout`] gives a layout node,
    /// also those [`Self::for_each_child`] keeps from routing (a hidden
    /// pane, a title, a spacer). A retained layout is checked against this
    /// walk before it is called through again, so a widget that replaced a
    /// child without reporting it gets a new layout instead of a call into
    /// freed memory. A child missing here cannot be checked, so debug builds
    /// stop on it. A wrapper that stands for its inner widget's node visits
    /// the inner widget itself.
    ///
    /// Returns whether it listed them; the default lists none and returns
    /// `false`, so the walk uses [`Self::for_each_child`]. (A constant
    /// default is one shared function, not one copy per widget type.)
    fn for_each_layout_child(&self, _visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        false
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![]
    }

    fn name(&self) -> &str;
    /// Stable id for the e2e test harness; `None` falls back to a path-based
    /// key. Must be unique across the live tree — the registry resolves on
    /// first match, so a duplicate shadows the second occurrence.
    fn test_id(&self) -> Option<&str> {
        None
    }

    /// This control as an action of the window's bar, sent back as `id`
    /// ([`crate::window_toolbar::ToolbarAction`]): a button's icon and name.
    /// `None` for anything that is not a button.
    fn window_slot(&self, _id: String) -> Option<crate::window_toolbar::ToolbarAction> {
        None
    }

    /// Downcast to the concrete widget, for hosts and tests that reach a
    /// known widget through `dyn Widget`. Default `None` (a widget may borrow
    /// and so not be `'static`); a widget opts in with `Some(self)`, and a
    /// [`Decorator`] answers itself.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Whether this widget is the one at `address`, or stands in the tree
    /// for it: a [`Decorator`] stands for every widget it wraps, so a
    /// scroller drawn inside one is found where the decorator is
    /// ([`crate::scroll_copy::offer`]).
    fn stands_for(&self, address: *const ()) -> bool {
        std::ptr::from_ref(self).cast::<()>() == address
    }

    /// Context that should remain visible with a focused descendant, such
    /// as a form control and its validation message. Opt-in scroll containers
    /// use this block when it fits; otherwise they prioritise the control.
    fn focus_reveal_context(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Reveal a newly keyboard-focused descendant inside any enclosing
    /// viewport. Containers override this to scroll; ordinary widgets simply
    /// recurse toward the target and report whether it was found.
    fn reveal_descendant(&mut self, target: *const dyn Widget) -> bool {
        defaults::reveal_descendant(self.children_mut(), target)
    }

    /// Named hit rects for widgets that paint several affordances into one
    /// rect instead of composing children. Same space as
    /// [`Widget::get_rect`]; zero-area entries are dropped.
    fn test_rects(&self) -> Vec<(String, vello::kurbo::Rect)> {
        Vec::new()
    }

    /// The rectangle this widget occupies. The default, for widgets that
    /// keep no geometry, is empty.
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }

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
        defaults::take_action_palette_request(self.children_mut())
    }

    /// One-shot request to open the host's search palette, the same one the
    /// platform search shortcut opens. An app's own search button latches it
    /// so search stays one shared host surface rather than an app overlay.
    fn take_search_request(&mut self) -> bool {
        defaults::take_search_request(self.children_mut())
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

    /// What a one-finger contact that begins at `point` (logical coords)
    /// means here, when a drag moves something rather than scrolling.
    ///
    /// [`TouchClaim::Drag`](crate::interaction::TouchClaim::Drag) where a
    /// drag moves things at once (a canvas whose items are dragged, words
    /// selected by dragging): past a tap's slop the contact is a left press
    /// where it landed, a move per update and a release where it lifts.
    /// [`TouchClaim::HoldDrag`](crate::interaction::TouchClaim::HoldDrag)
    /// where a drag scrolls but a held item can be moved (pages in a list):
    /// a still hold presses where the finger rests, and a drag after it
    /// moves the item. Either way a tap still clicks, a hold still reaches
    /// the context menu, and a second finger still pinches or pans (it
    /// ends the drag with a release first). Hit-tested like
    /// [`claims_raw_touch`](Self::claims_raw_touch), which wins over it.
    fn touch_claim(&self, _point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        crate::interaction::TouchClaim::Pointer
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

    /// The window holding this content fills its display (a maximized
    /// window): it has nothing beside it to round its corners against or
    /// to set it apart from, so content that frames or clips itself to the
    /// window's shape draws edge to edge instead. A client surface drawn
    /// that way, last and unclipped, is one the display can scan out
    /// directly. Content that frames nothing ignores it.
    fn set_full_bleed(&mut self, _full_bleed: bool) {}

    /// The form's validation verdict for this control, set by
    /// [`Field::with_error`]: an invalid control edges itself in danger ink
    /// and reports `invalid` to assistive technology. Controls without a
    /// field face ignore it.
    fn set_invalid(&mut self, _invalid: bool) {}

    /// An explicit runner focus request, after resolving the actual keyboard
    /// owner. Retained flags and layout restoration are not user gestures.
    /// Containers can observe this to reveal the owning viewport.
    fn focus_requested(&mut self, target: Option<NodeId>, tree: &TaffyTree<()>) {
        defaults::focus_requested(self.children_mut(), target, tree);
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

    /// Whether Tab and directional focus travel stay inside this subtree
    /// while no trap holds focus: the content of the focused desktop window,
    /// so keyboard travel cycles through the app the person is in and never
    /// wanders onto the desktop's chrome (its bars, menus and window
    /// titles), which the keyboard reaches through its own shortcuts. Unlike
    /// [`Self::traps_focus`] it moves no focus when it starts or ends, and
    /// pointer input is unaffected. Default `false`.
    fn scopes_keyboard_focus(&self) -> bool {
        false
    }

    /// One-shot (reading MUST clear it): the app instance the person just
    /// brought forward (a window they opened, clicked or switched to), whose
    /// keyboard focus the host restores, or puts on its default control, so
    /// what they type next reaches it. Default forwards the first request
    /// found among the children.
    fn take_app_activation(&mut self) -> Option<crate::application::InstanceId> {
        defaults::take_app_activation(self.children_mut())
    }

    /// Role, name/description/value, and
    /// [`AccessState`](crate::a11y::AccessState). Default `None` means
    /// presentational: the node is skipped and children reparent onto the
    /// parent. State must be **derived, not stored** so it can't drift from
    /// `draw`; names must already be [`i18n!`](macro@crate::i18n)-resolved.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        None
    }

    /// [`Self::accessibility`] with its synthetic children shared, for a
    /// widget that keeps them between frames: each child it did not change
    /// is handed over as the very `Arc` of the last frame, which the
    /// semantic tree takes as unchanged without copying or comparing it.
    ///
    /// `None` (the default) means "read [`Self::accessibility`]". A widget
    /// that answers `Some` must answer `accessibility()` with the same node,
    /// its children owned ([`SharedAccessNode::into_owned`](crate::a11y::SharedAccessNode::into_owned)).
    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        None
    }

    /// Whether [`Self::accessibility`] reports this subtree hidden, for a
    /// host that asks it of every node to prune focus traversal. Widgets
    /// whose node is costly to build (text values, per-block children, a
    /// translated name) answer without building it: the focus walk asks
    /// every widget in the window on every recomputation. Any override must
    /// agree with `accessibility()`; debug builds check it in the walk.
    fn accessibility_hidden(&self) -> bool {
        self.accessibility_root()
            .is_some_and(|node| node.state.hidden)
    }

    /// [`Self::accessibility`]'s own node with its `children` left empty:
    /// its role, identity (`semantic_key`) and state. Focus capture and focus
    /// order read only these, for every node on every frame, so a widget
    /// whose node carries a large subtree (a document exporting its every
    /// block) answers without building it. An override must equal
    /// `accessibility()` with `children` cleared.
    fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
        self.accessibility().map(|mut node| {
            node.children.clear();
            node
        })
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

    /// What describes one retained [`children`](Widget::children) entry to
    /// assistive technology, when this widget holds the words for it: a
    /// row's secondary line is its trailing switch's description. The
    /// bridge sets it as that child's node's description unless the child
    /// describes itself.
    fn accessibility_child_description(&self, _child_index: usize) -> Option<String> {
        None
    }

    /// Perform an accessibility [`Action`](crate::a11y::Action) on this
    /// widget, or on one of its **synthetic** accessibility nodes. May mutate
    /// the CRDT like [`handle_event`](Widget::handle_event).
    ///
    /// `key_path` names the target relative to this widget; **empty** means
    /// the widget itself. A widget that paints its own contents (a timeline of
    /// clips, a spreadsheet grid, a table of rows) has no child widgets to
    /// receive an action, so it describes its structure through
    /// [`AccessNode::children`](crate::a11y::AccessNode::children) instead, and
    /// the host resolves an activated node back to `(this widget, key path)`:
    /// one element per level of nesting — `[row]` for a table row, `[row,
    /// cell]` for a cell inside it. Each element is the node's
    /// [`key`](crate::a11y::AccessNode::key), or its ordinal among its siblings
    /// when it declared none.
    ///
    /// For an element keyed by identity rather than position, compare the
    /// incoming key against [`hash_key`](crate::a11y::hash_key) of your own id:
    ///
    /// ```ignore
    /// fn perform_action(&mut self, action: &Action, key_path: &[u64], ..) -> bool {
    ///     let Some(&clip_key) = key_path.first() else { return false };
    ///     let Some(clip) = self.clips.iter().find(|c| hash_key(&c.id) == clip_key) else {
    ///         return false;   // stale reference — do nothing, never guess
    ///     };
    ///     ...
    /// }
    /// ```
    ///
    /// Returns `true` if the widget consumed the action. Default `false` ("not
    /// handled") falls back to generic dispatch (`Focus` moves keyboard focus,
    /// `Activate` synthesises a click on the whole widget); widgets with
    /// richer semantics (slider inc/dec, combo expand/collapse, per-element
    /// canvases) override to handle those verbs precisely.
    fn perform_action(
        &mut self,
        _action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        false
    }

    /// Handle an action addressed to a synthetic accessibility child created
    /// by this widget. `target` is the opaque stable value supplied through
    /// [`AccessNode::with_action_target`](crate::a11y::AccessNode::with_action_target).
    ///
    /// Custom canvases using
    /// [`AccessNode::with_keyed_action_routes`](crate::a11y::AccessNode::with_keyed_action_routes)
    /// do not need to override this method: the default decodes the published
    /// semantic target and forwards it to [`Widget::perform_action`].
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let Some(key_path) = defaults::key_action_path(target) else {
            return false;
        };
        self.perform_action(action, &key_path, state, path)
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
        defaults::ime_requires_deferred_ack(self.ime_cursor_rect(), &|visit| {
            self.for_each_child(visit);
        })
    }

    /// Ordinary input-method commits (a composing keyboard IME, the on-screen
    /// keyboard) cannot reach this field: its text arrives only through
    /// deferred receipts (a browser page field), so hosts keep standard text
    /// input closed outside a dictation session. A field that needs deferred
    /// receipts for dictation but turns an ordinary commit into its own
    /// acknowledged edit (Terminal's native editor) does not refuse them.
    fn ime_refuses_ordinary_commits(&self) -> bool {
        defaults::ime_refuses_ordinary_commits(self.ime_cursor_rect(), &|visit| {
            self.for_each_child(visit);
        })
    }

    /// Begin one bounded deferred session after explicit host consent, preserving
    /// the existing original-field fingerprint. This grants no microphone access.
    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        defaults::start_deferred_text(self.children_mut(), caret)
    }

    /// A private asynchronous final. None leaves ordinary synchronous IME dispatch
    /// to the host. A receipt binds this original field and must be acknowledged
    /// before the host releases its private recovery transcript.
    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        let caret = self.ime_cursor_rect()?;
        defaults::commit_deferred_text(self.children_mut(), caret, text)
    }

    /// The focused widget wants platform text input (Wayland
    /// `text-input-v3`, native IMEs elsewhere). `None` is intentionally the
    /// default: enabling an IME for buttons, canvas tools or games makes the
    /// compositor raise an on-screen keyboard over content that cannot accept
    /// the committed text.
    ///
    /// The answer is what the field holds and how the input method may help
    /// in it, so the on-screen keyboard picks the matching layout (an email
    /// field's `@`, a phone field's dial pad). Return the closest truthful
    /// purpose; platforms that take less (winit's three purposes) get the
    /// nearest of theirs.
    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        None
    }

    /// Whether the original focused editor accepts line feeds as text. This is
    /// field policy, never inferred from a multiline accessibility presentation.
    /// Wrappers inherit only one unambiguous child at their active IME caret.
    fn ime_multiline(&self) -> bool {
        let Some(caret) = self.ime_cursor_rect() else {
            return false;
        };
        defaults::ime_multiline(caret, &|visit| self.for_each_child(visit))
    }

    /// Opaque local invalidation of deferred input, including selection edits
    /// that leave the caret rectangle unchanged. The host supplies an ephemeral
    /// secret key; no text or selection is retained in the resulting digest.
    /// The default shares the accessibility door and excludes protected or
    /// read-only controls. Text controls can override it to avoid cloning text.
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let caret = self.ime_cursor_rect()?;
        defaults::ime_edit_fingerprint(caret, key, &|visit| self.for_each_child(visit), &|| {
            self.accessibility()
        })
    }

    /// Default `true`; the exception is a widget standing in for a window's
    /// content whose first frame is not drawn yet (a process that has started
    /// but not drawn, a stream from another device before its first picture).
    /// Must **latch**: once `true`, never `false` again for the same content,
    /// or an un-presenting widget would make a window vanish mid-resize. What
    /// shows meanwhile is the framework's ([`crate::arriving`]), timed from
    /// [`Widget::arrives_from`].
    fn presented(&self) -> bool {
        true
    }
    /// Where this content's first frame comes from while it has not
    /// [`presented`](Widget::presented): this device (an app starting here),
    /// another device or a host. A frame from this device may hold the
    /// window's first frame up to one frame budget; any other reserves the
    /// window's place and says where it comes from after a while.
    fn arrives_from(&self) -> crate::arriving::Source {
        crate::arriving::Source::Local
    }
    /// How much of what this content's first frame waits on has come, when
    /// that measures itself (its app downloaded before it starts).
    fn arrival_progress(&self) -> Option<crate::arriving::Progress> {
        None
    }
    /// The shape this content declares for the placeholder standing in for
    /// its first frame. Undeclared, the placeholder is the ground alone:
    /// nothing guesses what a window will show.
    fn arrival_shape(&self) -> crate::widget::SkeletonShape {
        crate::widget::SkeletonShape::Ground
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
        defaults::on_nav_gesture(self.children_mut(), dir, state, path)
    }

    /// Which directions [`Self::on_nav_gesture`] would consume now. A widget
    /// that owns history overrides this beside `on_nav_gesture`.
    fn nav_availability(&self) -> crate::gesture::NavAvailability {
        defaults::nav_availability(self.children())
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
        defaults::take_accessibility_request(self.children_mut())
    }

    /// Route history when this widget, or one it contains, is a [`Router`].
    ///
    /// Dev reload writes this beside the document so the same screen is showing
    /// after the process is replaced. Containers forward to the first child
    /// that has one. A [`Router`] reports its own stack and
    /// does not look inside the current screen.
    fn dev_route(&self) -> Option<router::RouteSnapshot> {
        defaults::dev_route(self.children())
    }

    /// Put `route` back. Returns whether a router in this subtree accepted it.
    ///
    /// An unknown route name is refused and the existing screen stays up.
    fn restore_dev_route(&mut self, route: &router::RouteSnapshot) -> bool {
        defaults::restore_dev_route(self.children_mut(), route)
    }

    /// Theme/high-contrast/reduced-motion/scale, polled under
    /// [`crate::app::App::appearance_reconcile`] — how a settings pane
    /// re-themes the running app live without a back-channel.
    /// Level-triggered; the App recurses [`Self::children`] itself, so
    /// containers need not forward it. First `Some` wins.
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        None
    }

    /// The narrowest this window still works at, in logical pixels: the
    /// minimum of its compact layout (a `NavShell` answers with one column
    /// at a time). The host states it to the window system, so the desktop
    /// stops a resize there. `None`: no opinion. The host walks the tree
    /// itself ([`minimum_window_width`]), so containers need not forward
    /// it; the first `Some` wins.
    fn minimum_window_width(&self) -> Option<f64> {
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
        defaults::set_motion_reduced(self.children_mut(), reduced);
    }

    // --- Collab / DnD contract extensions ---
    /// Whether `sync` still needs to run on idle frames without input or
    /// collab events — e.g. waiting out a document-adoption grace window.
    fn needs_idle_sync(&self) -> bool {
        false
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

    fn wants_pointer_capture(&self) -> bool {
        false
    }

    /// While this widget is a band on its way in or out
    /// (`motion::Disclosure`: an in-window banner arriving or leaving), or a
    /// content-sized container one moves in, how far what follows it is
    /// drawn from where it is laid out. A moving band stands out of the
    /// layout; its container places it ([`Self::place_disclosing`]) and
    /// draws the widgets after it lower by transform, so they move with the
    /// band's edge instead of being laid out again every frame. `None` at
    /// rest (the default).
    fn disclosing(&self) -> Option<Disclosing> {
        None
    }

    /// Where a band on its way ([`Disclosing::Band`]) stands while it is
    /// out of the layout: its slot's top and width (`slot`'s height is
    /// zero), set by its container after each layout.
    fn place_disclosing(&mut self, _slot: vello::kurbo::Rect) {}

    /// A band that comes and goes (an in-window banner): when its view
    /// stops building it, its column draws it leaving
    /// (`widget::Disclosing`). Its column leaves it out of its identity
    /// across rebuilds.
    fn is_band(&self) -> bool {
        false
    }

    /// Where a band stands in its column, set by the column after each
    /// layout: what it leaves when dropped, so the column a rebuild makes
    /// can draw it leaving.
    fn note_placement(&mut self, _placement: Placement) {}

    /// Why this widget itself asks for animation frames, for diagnostics
    /// that name a desktop which keeps painting. `None` when it has nothing
    /// to add to its name and geometry.
    fn animation_detail(&self) -> Option<String> {
        None
    }

    /// `App` polls this in `Event::AboutToWait`; without it a curve would
    /// freeze partway until an unrelated event woke it again.
    fn needs_animation_frame(&self) -> bool {
        defaults::needs_animation_frame(&|visit| self.for_each_child(visit))
    }

    /// Earliest wall-clock deadline at which this subtree needs a sync/redraw
    /// even if there is no input. Unlike `needs_animation_frame`, this keeps
    /// the event loop asleep until the requested instant and is appropriate
    /// for filesystem/status polling rather than animation.
    fn next_wake_deadline(&self) -> Option<Instant> {
        defaults::next_wake_deadline(&|visit| self.for_each_child(visit))
    }

    /// Until when the frame that would show this subtree waits for data it
    /// reads from this device ([`crate::arriving`]): a local read that has
    /// not answered and is still inside its frame budget. The host keeps
    /// the frame on screen until then, or until the data lands, so the
    /// person never sees the space empty and then filled. Containers
    /// forward (the default does).
    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        defaults::holds_frame_until(&|visit| self.for_each_child(visit), now)
    }

    /// A deadline this subtree asked for ([`Self::next_wake_deadline`]) is
    /// due at `now`: do the work it was for, and say whether anything the
    /// subtree shows or publishes changed. The host syncs, and drops the
    /// published resource tree, only when it did: a poll that found nothing
    /// costs nothing more. The default says it did, so the host syncs as for
    /// any wake; a widget overrides it only where it serves here every
    /// deadline it asks for and reports every change that serving makes.
    fn serve_deadline(&mut self, _now: Instant) -> bool {
        true
    }

    /// `Some(rect)` when this subtree holds an *open* modal, its panel box.
    /// While any widget reports `Some`, the host suppresses its own global
    /// focus shortcuts and swallows unhandled pointer presses so nothing
    /// behind the modal is clickable. Containers MUST forward (the default
    /// does, via first `Some` among children).
    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        defaults::modal_capture(&|visit| self.for_each_child(visit))
    }

    /// True if this subtree's structural composition (children
    /// inserted/removed) changed since the last frame; the App rebuilds the
    /// taffy tree when so. Reading clears it. The default drains every
    /// retained child (no short-circuit, so no child keeps a stale flag) and
    /// reports whether any was dirty; a leaf has no children and reports
    /// `false`. A container that changes its own child list sets its own flag
    /// and ORs it with this traversal.
    fn take_structure_dirty(&mut self) -> bool {
        defaults::take_structure_dirty(self.children_mut())
    }

    /// [`Self::take_structure_dirty`], saying whether the frame's damage can
    /// still describe the rebuild ([`StructureChange::Described`]). Reading
    /// clears what `take_structure_dirty` clears; the two must agree on
    /// whether there is a change. The default is the flag, as a
    /// [`StructureChange::Rebuild`]: only a widget that knows every pixel its
    /// change touches lands in its own damage says less, and a container
    /// forwards a child's [`StructureChange::Described`] only by overriding
    /// this.
    fn take_structure_change(&mut self) -> StructureChange {
        StructureChange::of(self.take_structure_dirty())
    }

    /// This widget's own layout node while the widgets under it are laid
    /// out again in place, in it, by the coming
    /// [`Self::remount_children`]: it then names none of them through
    /// [`Self::for_each_layout_child`], and the host lets go of every node
    /// under this one, as their widgets may be gone. `None` (the default)
    /// otherwise.
    #[doc(hidden)]
    fn relaid_layout_node(&self) -> Option<NodeId> {
        None
    }

    /// Carry out a [`StructureChange::Children`] this widget or one under it
    /// reported, in the retained layout tree, through `remount` (see
    /// [`remount`](crate::widget::remount)). Returns `false` when it cannot be
    /// carried out in place: the host then rebuilds its tree, and the change
    /// is carried out by [`Self::build_layout`]. A widget reports such a
    /// change until either has carried it out. The default carries out its
    /// children's.
    fn remount_children(&mut self, remount: &mut Remount<'_>) -> bool {
        self.children_mut()
            .into_iter()
            .all(|child| child.remount_children(remount))
    }

    /// Work already requested by this view whose completion will replace a
    /// placeholder. Unlike animations, this can be awaited by still captures.
    fn has_pending_visual_work(&self) -> bool {
        defaults::has_pending_visual_work(&|visit| self.for_each_child(visit))
    }

    /// Temporary feedback currently presented, such as a save acknowledgement.
    /// Captures may intentionally include it; scene-invariance checks can wait
    /// for it to disappear separately from unfinished placeholder work.
    fn has_transient_feedback(&self) -> bool {
        defaults::has_transient_feedback(self.children())
    }

    /// Default `true` (dirty every frame). Opt into incremental redraw by
    /// tracking a `dirty: bool`, setting it in `handle_event`/`sync`, and
    /// clearing it in `mark_drawn()`. Containers MUST forward: dirty iff any
    /// descendant is.
    fn needs_redraw(&self) -> bool {
        defaults::needs_redraw(&|visit| self.for_each_child(visit))
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

    /// Whether, after this frame's draw, this widget's own
    /// [`Self::collect_damage`] covers every pixel a relayout changed by
    /// moving or resizing its layout box: a host that repaints where moved
    /// widgets were and are may then leave this widget's box to it (its
    /// children's boxes still count). A block editor that grew by a
    /// paragraph says where its blocks were and are; the default does not
    /// claim it.
    fn damage_covers_own_box(&self) -> bool {
        false
    }

    /// Called after this widget's scene lands in the composed frame, to
    /// clear a `dirty` flag. Default forwards to children.
    fn mark_drawn(&mut self) {
        defaults::mark_drawn(self.children_mut());
    }

    /// Release retained encoded-scene storage in this subtree without
    /// marking its visual/model state dirty. Outer caches call this after a
    /// successful reuse: while the outer display list is authoritative,
    /// keeping byte-for-byte descendant encodings only duplicates memory.
    /// A later outer rebuild causes descendants to record/cache normally.
    fn release_scene_cache(&mut self) {
        defaults::release_scene_cache(self.children_mut());
    }

    /// Default `true`; widgets with a fixed intrinsic size should override
    /// to `false` unless a size-affecting mutation landed. Containers MUST
    /// forward: layout-dirty iff any descendant is.
    fn is_layout_dirty(&self) -> bool {
        defaults::is_layout_dirty(&|visit| self.for_each_child(visit))
    }

    /// Whether this widget composes its children from the size it was last
    /// allocated (it reads its own rect while building its layout: a compact
    /// layout under a width, a count of columns, an inset from a drawer).
    /// A window resize then needs a second composition against the new
    /// allocation, as well as the re-solve every resize gets. Widgets that
    /// compose independently of their allocation, or that report
    /// `take_structure_dirty` themselves when a new allocation changes what
    /// they show, keep the default: a resize re-solves the retained tree in
    /// place instead of recomposing all of it.
    fn composition_follows_allocation(&self) -> bool {
        false
    }

    /// Whether a `layout_dirty` frame needs taffy to **re-solve** sizes
    /// (`compute_layout`) rather than just reposition already-solved boxes.
    /// [`Scrollable`] is the motivating override: it translates its origin
    /// in `apply_layout` without changing any taffy size, so re-solving on
    /// every scroll frame is pure waste.
    fn layout_needs_measure(&self) -> bool {
        defaults::layout_needs_measure(&|visit| self.for_each_child(visit))
            .unwrap_or_else(|| self.is_layout_dirty())
    }

    /// Everything this widget paints, children included; [`draw_child`]
    /// skips the draw when it misses the viewport. Override for widgets
    /// that paint past their own rect.
    fn paint_bounds(&self) -> vello::kurbo::Rect {
        self.get_rect()
    }

    /// **Top-layer pass**, run once after the whole `draw`
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
        defaults::draw_overlay(
            self.children_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }
}

/// Ergonomic forwarding API for optional floating-surface capabilities.
///
/// This keeps host/container call sites concise while the core [`Widget`]
/// contract contains only capability accessors.
pub trait FloatingSurfaceWidgetExt {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool);
    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>);
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
    ) -> Option<(crate::ime::InputContent, vello::kurbo::Rect)>;
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
    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool;
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

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        if let Some(provider) = capability::capability_mut::<dyn FloatingSurfaceProvider>(self) {
            provider.on_floating_surface_ime_source_changed(id);
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
    ) -> Option<(crate::ime::InputContent, vello::kurbo::Rect)> {
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

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) -> bool {
        capability::capability_mut::<dyn FloatingSurfaceProvider>(self).is_some_and(|provider| {
            provider.draw_floating_surface(id, scene, transform, text_renderer, theme, presence)
        })
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
            size: taffy::Size::from_percent(1.0, 1.0),
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
                size: taffy::Size::from_percent(1.0, 1.0),
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
mod defaults;
pub mod document_effects;
pub use crate::pen::PenSample;
pub use decorator::Decorator;

pub(crate) mod layout;
pub mod remount;
pub use remount::Remount;
#[cfg(test)]
pub(crate) mod replaced_tripwire;
pub use layout::{
    AuthoredExtent, ExtentSizing, SkippedFrames, WidgetLayoutError, compute_widget_layout, insets,
    insets_all, insets_xy, layout_clip, placing_again, text_control_height, with_layout_clip,
};

mod button;
pub use button::{Button, ButtonFace, ButtonInteract, ButtonRole, face_colors, focus_ring_reach};
mod control_size;
pub use control_size::ControlSize;
mod bool_control;
mod checkbox;
#[cfg(test)]
mod control_hand_on_tests;
pub use checkbox::{CheckGlyph, CheckMotion, CheckPose, Checkbox, CheckboxFace, CheckboxInteract};
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
mod face_motions;
pub use face_motions::FaceMotions;
mod toggle;
pub use toggle::{
    SwitchMotion, SwitchPose, Toggle, ToggleColors, ToggleFace, ToggleInteract,
    toggle_accessibility, toggle_face_colors,
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
pub use divider::{Divider, DividerAxis, RuleEdge, paint_rule};
mod link;
pub use link::{Link, os_open as open_external_url, try_os_open as try_open_external_url};
mod flap;
pub use flap::{FlapState, SplitLayout};
mod bottom_sheet;
pub use bottom_sheet::{BottomSheet, SheetDetent, SheetFrame, SheetGeometry, SheetInput};
mod sidebar;
pub use sidebar::{
    DEFAULT_DRAWER_BREAKPOINT, DEFAULT_DRAWER_DOCKED_WIDTH, DEFAULT_DRAWER_WIDTH, SidebarDrawer,
    SidebarSide,
};
mod nav_shell;
/// Remember the sidebar widths of the shells this thread builds in an app's
/// settings on this device (`nav_shell::memory`).
#[cfg(feature = "native")]
pub(crate) use nav_shell::memory::remember_for_app as remember_sidebar_widths_for_app;

/// The narrowest the window `root` lays out still works at
/// ([`Widget::minimum_window_width`]): the first widget in the tree with an
/// opinion, outermost first.
pub fn minimum_window_width<W: Widget + ?Sized>(root: &W) -> Option<f64> {
    // A retained boundary (an app in a tile) answers inside its own scope.
    let _entry = root
        .callback_binding()
        .map(|binding| binding.enter())
        .transpose()
        .ok()?;
    if let Some(width) = root.minimum_window_width() {
        return Some(width);
    }
    let mut found = None;
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = minimum_window_width(child);
        }
    });
    found
}
pub mod window_controls;
pub mod window_header;
pub(crate) mod window_toolbar_row;
pub use nav_shell::{
    ContentKind, InspectorFrame, InspectorLink, InspectorPlacement, NavGeometry, NavMode, NavPane,
    NavShell, OverviewPlacement, SidebarPlacement, WINDOW_BACK, WindowBarLink,
};
pub use window_header::{
    HISTORY_CONTROL_ID, HeaderStatus, HistoryControl, HistoryOpener, STATUS_ACTION_ID,
    StatusAction, WindowBar, WindowBarSlot, WindowHeader, WindowHistory, WindowStatus,
};
mod nav_list;
pub use nav_list::{
    NavAction, NavArt, NavDrop, NavFeed, NavItem, NavItemStatus, NavList, NavPerson, NavSearchTerm,
    NavSection,
};
mod document_viewport;
pub use document_viewport::{
    ContentDraw, DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_PADDING, DEFAULT_PAN_STEP,
    DocumentViewport, PAN_SLOP, SmallContentAlign, ViewportState, ViewportZoom,
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
/// The layout primitives: `Stack`, `Row`, `Grid`, `Split`, `Page`, `Layers`.
pub mod arrange;
mod image;
pub use image::{Image, ImageDecodeError, ImageFit};
mod image_crop;
pub use image_crop::{CropSelection, ImageCropEditor};
mod slider;
pub use slider::{Slider, SliderFace, SliderTouch, SliderTouchStep, paint_held_readout};
mod text_input;
pub use text_input::{
    AccessoryAction, FieldFace, SpanHighlighter, TextActionRequest, TextActionResult, TextInput,
    TextInputKind, paint_invalid_edge, paint_preedit_underline,
};
pub mod code_editor;
pub use code_editor::{CodeEditor, Highlighter, HlState, Language, LineSpans, Span, TokenKind};
mod editable_label;
pub use editable_label::{EditOutcome, EditableLabel};
pub mod acknowledgement;
pub mod autocomplete;
pub use autocomplete::{
    Autocomplete, AutocompleteOutcome, Suggester, Suggestion, SuggestionList, SuggestionPerson,
};
pub mod mention_picker;
pub use mention_picker::{MentionCandidate, MentionQuery};
mod number_input;
pub use number_input::{NumberInput, NumberStepperFace, StepperPart};
pub mod marks;
pub use marks::{Mark, MarkSpan};
pub mod markdown_input;
pub mod rich_text_line;
pub use rich_text_line::{LineSignal, RichTextLine};
pub mod block_editor;
pub mod paint_damage;
pub use block_editor::{
    Block, BlockContent, BlockEditOp, BlockEditor, BlockKind, CalloutKind, CursorPlacement,
    DocumentPosition, DocumentSelection, EditScope, PresenceKeyFn, SlashRequest,
    TransclusionPreview, blocks_from_html, blocks_to_html, legacy_block_id,
};
mod filter_menu;
pub use filter_menu::FilterMenu;
pub mod slash_menu;
pub use slash_menu::{SlashCommand, SlashHostCommand, SlashMenu};
pub mod link_popover;
pub use link_popover::{LinkPopover, PageEntry};
pub mod page_sidebar;
pub use page_sidebar::{PageSidebar, PageSidebarEvent};
mod dropdown;
pub use dropdown::{Dropdown, DropdownFace, FILTER_MIN_OPTIONS};
pub mod aim;
pub use aim::Trajectory;
mod hover_intent;
pub use hover_intent::EdgeAim;
pub mod top_edge_reveal;
pub use top_edge_reveal::{EdgeGeometry, TopEdgeReveal};
pub mod tabs;
pub use tabs::{TabBar, TabDescriptor, TabHolder, TabMark, draw_app_mark};
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
pub mod board;
pub use board::{Board, BoardDrop};
pub mod drag;
pub use drag::{Axis as DragAxis, DragController, ListMetrics, REORDER_THRESHOLD_PX};
mod scrollable;
pub use scrollable::{ScrollAxis, Scrollable};
mod scroll_region;
pub use scroll_region::ScrollRegion;
pub use scrollbar::BarFade;
pub mod panic_scope;
mod tile_host;
pub use tile_host::{
    AppKind, TileContent, TileHost, hold_windows, record_unavailable_title, release_windows,
};
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
pub mod primitives;
pub use context_menu::{
    ContextMenu, MenuButton, MenuItem, note_shift, shift_held, shift_skips_confirmation,
};
mod row_menu;
pub use primitives::{
    BarControlFace, DensityRegion, DisclosureGroup, DisclosureRow, DisclosureState, Fact, Facts,
    FactsFace, FloatingPanel, FloatingPanelFace, FloatingPanelLayout, IconButton, KeyHint,
    KeyHints, KeyHintsFace, Surface, SurfaceLevel, TextStack, TextStackFace, paint_see_through,
    paint_surface,
};
pub use row_menu::RowMenu;
pub mod context_menu_area;
pub use context_menu_area::{ContextMenuArea, MenuEntry};
mod pacer;
pub use pacer::Pacer;
pub(crate) mod block_editor_table;
pub mod data_table;
pub mod table {
    pub use super::block_editor_table::*;
}
pub use table::Table;
mod window_group;
pub use window_group::{
    DEFAULT_STRIP_USABLE_PX, RESIZE_HANDLE_PX, WIDTH_FLOOR_PX, WIDTH_MAX_PERMILLE,
    WIDTH_MIN_PERMILLE, WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
    WindowGroup, WindowGroupBounds, WindowWidth, max_group_px, maximized_chrome_height,
    merged_controls_width, min_group_px, permille_for_px,
};
pub mod cursor;
pub mod edge_resize;
pub use edge_resize::{EdgeGrab, ResizeMode, WidthLimits, WindowEdge};
mod resize_handle;
pub use resize_handle::{HANDLE_HIT_HALF_PX, HANDLE_SLOT_PX, HANDLE_VISIBLE_PX, ResizeHandle};
mod overflow_door;
pub use overflow_door::{DoorPlan, OverflowDoor, plan as plan_overflow};
mod group_row;
pub use group_row::{
    CrossStripDetach, GroupRow, GroupTopology, HintPlaces, JUMP_PX_PER_DETENT, OutputSpan, Pan,
    STRIP_BOTTOM_GUTTER_PX, STRIP_EDGE_GUTTER_PX, StripLane, TabDropTarget, WheelJumpAccum,
    pan_within_canvas, wheel_jump_detents,
};
mod finger_path;
mod strip_deck;
pub use strip_deck::{StripDeck, StripDeckRelease};
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
pub mod file_table;
pub use file_picker::{FilePicker, FilePickerEntry, FilePickerMode, FilePickerOutcome};

pub mod progress_bar;
pub use progress_bar::{ProgressBar, ProgressBarFace};
pub mod page_preview;
pub use page_preview::PagePreview;
pub mod spinner;
pub use spinner::Spinner;
pub mod date_field;
pub mod date_picker;
pub use date_field::DateField;
pub use date_picker::{DateFeed, DatePicker, DateShown, ExternalDatePickerHit};
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
pub mod emoji_picker;
pub use emoji_picker::{EmojiPicker, PickerEvent};
pub mod reaction_strip;
pub use reaction_strip::{ReactionStrip, ReactionStripWidget, ReactionToggle, StripHit};
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
pub mod secret_prompt;
pub use secret_prompt::{
    FieldRole, FieldSpec, NoticeTone, Presentation, PromptOutcome, SecretAnswer, SecretKind,
    SecretPrompt,
};
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
pub mod signal_meter;
pub use signal_meter::{SIGNAL_BARS, SignalLevel, SignalMeter, SignalMeterFace};
pub mod chart;
pub use chart::{
    Chart, ChartLabels, ChartStyle, RollingSeries, Series, Sparkline, TrendCurve, TrendFill,
    TrendRange, TrendStyle,
};
pub(crate) mod chart_data;
pub(crate) mod chart_labels;
pub use chart_data::{ChartItem, ChartLink, ChartNode};
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
pub use polar_area_chart::{PolarAreaChart, PolarAreaStyle};
pub mod heatmap;
pub use heatmap::{Heatmap, HeatmapCell, HeatmapStyle};
pub mod histogram;
pub use histogram::{Histogram, HistogramNormalization};
pub mod candlestick_chart;
pub use candlestick_chart::{Candle, CandlestickChart, CandlestickStyle};
pub mod chord_chart;
pub use chord_chart::{ChordChart, ChordSelection, ChordStyle};
pub mod treemap;
pub use treemap::{Treemap, TreemapStyle};
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
pub use sankey::{SankeyChart, SankeySelection, SankeyStyle};
pub mod sunburst;
pub use sunburst::{SunburstChart, SunburstNode, SunburstStyle};
pub mod funnel_chart;
pub use funnel_chart::{FunnelChart, FunnelDirection, FunnelStyle};
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
    DuplicateListKey, ListEdges, ListModel, RowSelection, VecListModel, VirtualList,
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
pub mod media_transport;
pub use media_transport::{
    MediaTransport, TransportActions, TransportArt, TransportEvent, TransportFeed, TransportRepeat,
    TransportState,
};
pub mod now_playing;
pub use now_playing::{Scrub, Song, SongFace};
pub mod drop_zone;
pub use drop_zone::{DropZone, DropZoneFace};
pub mod file_open;
pub use file_open::{FileOpenEvent, FileOpenSurface};
pub mod skeleton;
pub use skeleton::{Skeleton, SkeletonFace, SkeletonShape, skeleton_tone};
pub mod arrival_failure;
pub use arrival_failure::{WindowFailure, arrival_failure};
pub mod arriving_rows;
pub mod arriving_view;
pub use arriving_rows::{ArrivingRows, RowFade};
pub use arriving_view::{ArrivalFace, ArrivingView, NamedOnce, ReservedShape, Showing};
pub mod stepper;
pub use stepper::{Stepper, StepperFace};
pub mod property_list;
pub use property_list::{
    Property, PropertyColumn, PropertyList, PropertyListFace, PropertyRow, PropertyValue,
    property_label_column,
};
pub mod list_row;
pub use list_row::{
    ControlRow, ListRow, ListRowFace, RowAction, RowLeading, RowLeadingShape, RowShape, RowStatus,
    RowValue, SwipeAction, badge_column_width,
};
pub mod toggle_group;
pub use toggle_group::{
    Segment, SegmentedFace, SegmentedStyle, SegmentedTouch, SegmentedTouchGroup,
    SegmentedTouchResult, ToggleGroup, segment_colors,
};
pub mod view_tabs;
pub use view_tabs::ViewTabs;
pub mod quick_tile;
pub use quick_tile::{QuickTile, QuickTileOutcome, QuickTilePart};

// EmptyState / Banner / StatTile also expose an immediate-mode `paint(...)`
// entry point for apps that render straight to a `Scene` rather than a
// widget tree.
pub mod empty_state;
pub use empty_state::{EmptyState, EmptyStateContent, EmptyStateLayout};
pub mod find_bar;
pub use find_bar::{
    FindAbilities, FindBar, FindMatch, FindMode, FindOptions, FindScope, FindSession, Findable,
    OwnFind, paint_find_match, with_findable,
};
pub mod search_field;
pub use search_field::{
    FilterCount, Filterable, Filtered, SearchField, apply_filter, sync_filter, with_filterable,
};
pub mod window_search;
pub use window_search::{
    InPlace, Queryable, SearchDriver, SearchHandoff, SearchReach, SearchableWindow, WindowSearch,
    WindowSearchRequest, with_queryable,
};
pub mod search_results;
pub use search_results::SearchResultsView;
pub mod stat_tile;
pub use stat_tile::{StatGroup, StatTile, StatTileFace, Trend};
pub mod banner;
pub use banner::{Banner, BannerContent, BannerHit, BannerKind, BannerNotice};
pub mod chip;
pub use chip::{Chip, ChipColors, ChipFace, ChipLead, ChipStyle};
pub mod conversation;
pub use conversation::{
    AttachmentAction, AttachmentFace, AttachmentKind, AttachmentSummary, ComposerIntent,
    ComposerRule, ConversationClock, ConversationDay, ConversationLine, ConversationList,
    ConversationRole, DayBreaks, DayDividerFace, DeliveryFace, DeliveryState, DeliveryStep,
    DraftBook, ListFace, SavedCopy, UploadProgress,
};
pub mod card;
mod tone;
pub use card::{Actions, Card, CardFace, CardGrid, CardWidth, card_column_width, card_columns};
pub use tone::Tone;
mod section;
pub use section::{ObjectHeader, Section, SectionHeader, SectionHeaderFace, StatusLine};
pub mod capture_studio;
pub use capture_studio::{
    AudioCaptureMode, CaptureDestination, CaptureIntent, CaptureMode, CapturePreview,
    CaptureStudio, CaptureTarget, LiveCapture, PipLayout, RecordingCommand, RegionAnchor,
    ReplayCommand, ScreenshotCommand, ScreenshotKept, ScreenshotSendDevice, ScreenshotSendTarget,
    SourceMode,
};
pub mod screenshot_markup;
pub mod selection_handles;
pub use screenshot_markup::{
    MarkupExport, MarkupStroke, MarkupTool, ScreenshotMarkup, ScreenshotMarkupCommand,
};
pub use selection_handles::{Handle as SelectionHandle, SelectionHandles, TextGrip, TextGrips};

pub mod activity_timeline;
pub mod history;
pub mod history_face;
pub mod history_group;
pub mod name_field;
pub use activity_timeline::ActivityTimeline;
pub use history::{
    AuthorRelation, EntryKind, HistoryAttribution, HistoryAuthor, HistoryClassification,
    HistoryEntry, HistoryEntryId, HistoryFilter, RedactionDisplay, format_relative_time,
    hue_to_rgb,
};
pub use history_face::{EntryBody, EntryFace, HistoryEntryPresenter};
// The artifact vocabulary a `HistoryEntry` is written in. Re-exported so a
// caller can build one without naming the collab-types crate directly.
pub use collab_types::ArtifactKind;

pub mod qr;
pub mod resource_actions;
pub use qr::QrWidget;

#[cfg(test)]
mod a11y_tree_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

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
            dyn crate::resources::PublishesResources,
            dyn crate::widget::ResourceOperations
        );

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
        fn name(&self) -> &'static str {
            "ControlResultProbe"
        }
    }

    impl crate::widget::ResourceOperations for Probe {
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
    }
}

pub mod keyboard_map;
pub use keyboard_map::{KeyboardMap, KeyboardMapInput, KeyboardMapKey, KeyboardMapSelection};
/// Logical screen topology canvas shared by local and native peer displays.
pub mod display_arrangement;
mod graph_canvas;
pub use graph_canvas::{GraphCanvas, GraphCanvasConnection, GraphCanvasNode};
