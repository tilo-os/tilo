mod contract;
pub use contract::Widget;
mod action_directory_identity;
mod wheel;
pub use wheel::{PhaselessWheel, WHEEL_GESTURE_GAP, line_of_wheel, sideways_travel};
mod action_emitter;
mod disclosing;
pub use action_directory_identity::ActionDirectoryInvocationDomain;
#[cfg(test)]
pub(crate) use action_emitter::ACTION_EMITTER_QUEUE_LIMIT;
pub use action_emitter::ActionEmitter;
pub(crate) use disclosing::{
    BandMotion, Ghosts, content_box, follower_shifts, place_bands, route_to_shifted_children,
};
pub use disclosing::{Disclosing, Placement, event_offset};
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
    CollaborativeDocument(String),
    SemanticAction(String),
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

/// [`semantic_action_bindings`] of `node`, appended to `out`: read in place, so a widget's
/// kept rows are listed without copying them.
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

/// How readily `widget` takes the keyboard when its app does without a control of its own choosing:
/// its default keyboard control first (`Some(0)`), else one that takes typing — a field, an editor,
/// a terminal (`Some(1)`) — never a button merely for being first (`None`): Enter would press it.
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

pub(crate) const SETTLE_FRAMES: usize = 3;

/// `node`'s laid-out box placed at `origin`: what `apply_layout` stores as a leaf's rect.
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
    layout::solve_layout(taffy, root, available, &mut |_, _, _| taffy::Size::ZERO)
}

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
    (measured) => {
        $crate::rect_geometry!();

        fn is_layout_dirty(&self) -> bool {
            self.rect == $crate::kurbo::Rect::ZERO || self.layout_dirty
        }

        fn layout_measured(&mut self) {
            self.layout_dirty = false;
        }
    };
}

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

/// Let `node` share its row's main axis with its siblings by `grow`: it grows and shrinks
/// from a zero basis instead of keeping its own width.
pub fn share_row(taffy: &mut TaffyTree<()>, node: NodeId, grow: f32) {
    if let Ok(mut style) = taffy.style(node).cloned() {
        style.flex_grow = grow;
        style.flex_shrink = 1.0;
        style.flex_basis = taffy::style::Dimension::length(0.0);
        style.size.width = taffy::style::Dimension::auto();
        let _ = taffy.set_style(node, style);
    }
}

/// Whether `child` holds the pointer for its subtree: it latched a drag, or a menu or modal
/// is open somewhere in it.
#[must_use]
pub fn holds_pointer(child: &dyn Widget) -> bool {
    child.wants_pointer_capture() || child.modal_capture().is_some()
}

/// Whether `widget` paints this frame: it changed itself, or a child is dirty or on its way
/// (`needs_animation_frame`) and paints other pixels.
#[must_use]
pub fn redraw_or_children(dirty: bool, widget: &dyn Widget) -> bool {
    let mut dirty = dirty;
    widget.for_each_child(&mut |child: &dyn Widget| {
        dirty |= child.needs_redraw() || child.needs_animation_frame();
    });
    dirty
}

/// Route `event` through a container's children, first handler wins. A child latching a drag gets
/// it exclusively, so the drag survives the cursor crossing a peer that would claim it first; a
/// child holding an open modal owns input the same way.
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

/// Draw `child` unless what it paints misses the viewport. Off-viewport widgets stay dirty, so they
/// re-emit when scrolled back in. A widget ends every layer it begins: the render service refuses a
/// scene that leaves a clip open, and with it the whole frame; debug builds check that here, naming
/// the widget.
pub fn draw_child(
    child: &mut (impl Widget + ?Sized),
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    draw_child_at(
        child,
        None,
        scene,
        transform,
        text_renderer,
        theme,
        presence,
    );
}

/// [`draw_child`] for a container that knows `child`'s place among its
/// children (`index`), so a host that keys widgets
/// ([`crate::scene_parts::key_widgets`]) keys it by that place even when the
/// container leaves siblings before it undrawn.
pub fn draw_child_at(
    child: &mut (impl Widget + ?Sized),
    index: Option<usize>,
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    if presence.should_cull(child.paint_bounds()) {
        if index.is_none() {
            crate::scene_parts::skip_widget();
        }
        return;
    }
    draw_child_unculled(
        child,
        index,
        scene,
        transform,
        text_renderer,
        theme,
        presence,
    );
}

/// Draw `child` with its key ([`draw_child_at`]) even where it misses the
/// viewport: for a container that culls its children itself, draws one
/// whose rect is not where it paints (a child moved by `transform`, a
/// window's own content), or draws a root.
pub fn draw_child_unculled(
    child: &mut (impl Widget + ?Sized),
    index: Option<usize>,
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    presence: &PresenceCtx,
) {
    #[cfg(debug_assertions)]
    let open = scene.encoding().n_open_clips;
    #[cfg(debug_assertions)]
    let _painting = paint_pass::Painting::enter();
    let begun = crate::scene_parts::begin_widget(scene, child, index, transform);
    #[expect(
        clippy::disallowed_methods,
        reason = "the one keyed draw path a widget is drawn through"
    )]
    child.draw(scene, transform, text_renderer, theme, presence);
    crate::scene_parts::end_widget(scene, begun);
    #[cfg(debug_assertions)]
    debug_assert_eq!(
        scene.encoding().n_open_clips,
        open,
        "{} left a layer open (or ended one it did not begin) in its draw",
        child.concrete_type_name()
    );
}

/// [`draw_child_unculled`] as a method, for a container that draws a child
/// itself: `child.draw_child(scene, transform, text_renderer, theme,
/// presence)` takes the place of calling the child's [`Widget::draw`], and
/// carries the child's key into the scene for a host that keys widgets.
pub trait DrawChild {
    fn draw_child(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    );
}

// A widget type a container holds by value is drawn without making it a
// trait object: a type that never was one gets no vtable (which would bring
// every `Widget` method of the type into the program) for being keyed. One
// copy per type, never inlined, so a call site is the one call it was.
impl<W: Widget> DrawChild for W {
    #[inline(never)]
    fn draw_child(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        #[cfg(debug_assertions)]
        let _painting = paint_pass::Painting::enter();
        let begun = crate::scene_parts::begin_widget(scene, self, None, transform);
        #[expect(
            clippy::disallowed_methods,
            reason = "the keyed draw path of a widget held by value"
        )]
        self.draw(scene, transform, text_renderer, theme, presence);
        crate::scene_parts::end_widget(scene, begun);
    }
}

macro_rules! draw_child_of_dyn {
    ($($object:path),* $(,)?) => {$(
        impl DrawChild for dyn $object + '_ {
            fn draw_child(
                &mut self,
                scene: &mut Scene,
                transform: Affine,
                text_renderer: &mut TextRenderer,
                theme: &Theme,
                presence: &PresenceCtx,
            ) {
                draw_child_unculled(self, None, scene, transform, text_renderer, theme, presence);
            }
        }

        impl DrawChild for Box<dyn $object + '_> {
            fn draw_child(
                &mut self,
                scene: &mut Scene,
                transform: Affine,
                text_renderer: &mut TextRenderer,
                theme: &Theme,
                presence: &PresenceCtx,
            ) {
                draw_child_unculled(
                    &mut **self,
                    None,
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                );
            }
        }
    )*};
}

draw_child_of_dyn!(
    Widget,
    AppWidget,
    crate::history_mode::host::HistoryChromeApi,
    crate::action_palette::api::ActionPaletteApi,
);

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
///
/// Inside a component only its root (the widget its boundary hosts)
/// publishes document actions: it speaks for the resource. What the rest of
/// the component's tree publishes is kept to views and effects.
pub(crate) fn collect_widget_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
    resources: &mut Vec<tilo_actions::ResourceSpec>,
) {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<crate::bound_dispatch::CallbackBinding>,
        root: bool,
        resources: &mut Vec<tilo_actions::ResourceSpec>,
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
        if boundary.is_none() {
            let found = if let Some(binding) = &binding {
                binding
                    .read_document(doc, |doc, _| crate::resources::resources(widget, doc))
                    .unwrap_or_default()
            } else {
                crate::resources::resources(widget, doc)
            };
            if root || binding.is_none() {
                resources.extend(found);
            } else {
                resources.extend(found.into_iter().map(crate::resources::outside_document));
            }
        }
        let below = boundary.is_some();
        widget.for_each_child(&mut |child| visit(child, doc, binding.clone(), below, resources));
    }
    visit(widget, doc, None, true, resources);
}

/// The resource `id` in `widget`'s subtree, resolved by the widget that publishes it rather
/// than by building every resource the subtree holds: [`collect_widget_resources`] for one
/// address.
#[must_use]
pub fn find_resource(
    widget: &dyn Widget,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::ResourceSpec> {
    fn visit(
        widget: &dyn Widget,
        doc: &AutoCommit,
        inherited: Option<crate::bound_dispatch::CallbackBinding>,
        root: bool,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        if widget.has_separate_document_owner() && widget.callback_binding().is_none() {
            return None;
        }
        let boundary = widget.callback_binding();
        let binding = boundary.clone().or(inherited);
        let Ok(_entry) = binding
            .as_ref()
            .map(crate::bound_dispatch::CallbackBinding::enter)
            .transpose()
        else {
            return None;
        };
        if boundary.is_none() {
            let found = if let Some(binding) = &binding {
                binding
                    .read_document(doc, |doc, _| crate::resources::resource(widget, doc, id))
                    .ok()
                    .flatten()
            } else {
                crate::resources::resource(widget, doc, id)
            };
            if let Some(found) = found {
                return Some(if root || binding.is_none() {
                    found
                } else {
                    crate::resources::outside_document(found)
                });
            }
        }
        let below = boundary.is_some();
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = visit(child, doc, binding.clone(), below, id);
            }
        });
        found
    }
    visit(widget, doc, None, true, id)
}

/// Every resource `widget`'s subtree publishes, as the host's action
/// catalogue collects them. A host that embeds components under its own
/// root (a desktop surface) reads what its search offers through this.
pub fn published_resources(
    widget: &dyn Widget,
    doc: &AutoCommit,
) -> Vec<tilo_actions::ResourceSpec> {
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
) -> Option<tilo_actions::ResourceSpec> {
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
    pub resources: Vec<tilo_actions::ResourceSpec>,
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

#[cfg(test)]
mod test_controls;
#[cfg(test)]
pub(crate) use test_controls::{Uncustodied, test_value_action_binding};

#[cfg(test)]
fn test_action_binding<I>(spec: tilo_actions::ActionSpec) -> tilo_actions::ActionBinding<I>
where
    I: serde::Serialize,
{
    let resource =
        tilo_actions::ResourceSpec::new("test:control", "test", "Test").with_action(spec.clone());
    tilo_actions::ActionBinding::new(&resource, &spec, |value| {
        serde_json::json!({"value": value})
            .as_object()
            .unwrap()
            .clone()
    })
    .unwrap()
}

#[cfg(test)]
pub(crate) fn test_bool_action_binding() -> tilo_actions::ActionBinding<bool> {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
    test_action_binding(ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_bool",
        vec![ActionParam::required("value", ParamType::Boolean, "Value")],
        |args| EncodedAction::encode_operation("test.set_bool", &args.boolean("value")).unwrap(),
    ))
}

#[cfg(test)]
pub(crate) fn test_string_action_binding() -> tilo_actions::ActionBinding<String> {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
    test_action_binding(ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_string",
        vec![ActionParam::required("value", ParamType::Text, "Value")],
        |args| EncodedAction::encode_operation("test.set_string", &args.text("value")).unwrap(),
    ))
}

#[cfg(test)]
pub(crate) fn test_number_action_binding() -> tilo_actions::ActionBinding<f64> {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
    test_action_binding(ActionSpec::typed_operation(
        "set",
        "Set",
        "value",
        OperationKind::Effect,
        "test.set_number",
        vec![ActionParam::required("value", ParamType::Number, "Value")],
        |args| EncodedAction::encode_operation("test.set_number", &args.number("value")).unwrap(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCloseAdmission {
    Allow,
    Defer,
}

/// What a structural change a subtree reported ([`Widget::take_structure_change`]) asks of
/// its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum StructureChange {
    #[default]
    None,
    /// The widgets that reported it mount and unmount children of their own, and every widget stays
    /// alive: they carry it out in the retained tree in place ([`Widget::remount_children`]), and
    /// their own damage covers every pixel that changed. A host that cannot rebuilds its tree, as
    /// for [`Self::Described`].
    Children,
    /// The host rebuilds its tree, but the frame's damage still describes it:
    /// the widget that reported it replaced only widgets of its own, and its
    /// own damage this frame covers every pixel that changed with them.
    Described,
    Rebuild,
}

impl StructureChange {
    /// A structural flag as a change: any change is a [`Self::Rebuild`].
    #[must_use]
    pub fn of(dirty: bool) -> Self {
        if dirty { Self::Rebuild } else { Self::None }
    }

    /// Whether the host rebuilds its tree for it, or (for [`Self::Children`]) changes it in
    /// place.
    #[must_use]
    pub fn rebuilds(self) -> bool {
        self != Self::None
    }

    /// This change and `other`, as one.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        self.max(other)
    }

    /// Every one of `children`'s changes, joined: for a container that adds no structure of
    /// its own, so a change a child carries out in place ([`Self::Children`]) stays one.
    #[must_use]
    pub fn of_children(children: Vec<&mut (dyn Widget + '_)>) -> Self {
        children.into_iter().fold(Self::None, |change, child| {
            change.join(child.take_structure_change())
        })
    }
}

/// Optional cross-device continuation capability for an app root, deliberately separate
/// from [`Widget`]: ordinary controls have no resumable app state, while roots that do expose it through
/// [`Widget::provide`] without weakening their UI integration.
pub trait Continuable {
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

/// Optional session collaboration capability for widgets that own live broker-backed
/// activity.
pub trait Collaborative {
    fn take_collab_requests(&mut self) -> Vec<crate::collaboration::CollabRequest> {
        Vec::new()
    }

    fn handle_collab_event(&mut self, _event: &crate::collaboration::CollabEvent) {}
}

/// Optional lifecycle for widgets backed by a platform-native child view.
pub trait NativeViewLifecycle {
    fn mount_native_view(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>);

    fn unmount_native_view(&mut self);

    fn native_view_frame_start(&mut self);
}

/// Optional capture and detachable-surface capability.
pub trait FloatingSurfaceProvider {
    fn on_floating_surface_focus_changed(&mut self, _id: Option<&str>, _focused: bool) {}

    fn on_floating_surface_ime_source_changed(&mut self, _id: Option<&str>) {}

    fn on_floating_surface_event_source(&mut self, _id: Option<&str>) {}

    fn floating_surface_event(
        &mut self,
        _id: &str,
        _event: &crate::winit::event::WindowEvent,
        _ctx: &mut EventCtx<'_>,
    ) -> bool {
        false
    }

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

    fn floating_surface_ime(
        &self,
        _id: &str,
    ) -> Option<(crate::ime::InputContent, vello::kurbo::Rect)> {
        None
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        None
    }

    fn take_capture_studio_request(&mut self) -> Option<CaptureMode> {
        None
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, vello::kurbo::Rect)> {
        Vec::new()
    }

    fn draw_floating_surfaces(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut crate::text_render::TextRenderer,
        _theme: &crate::theme::Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
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
        let presentations = self.floating_surface_presentations();
        if presentations.len() != 1 || presentations[0].id != id {
            // A plural paint pass cannot select one of several owners safely.
            // Multi-surface providers must implement the exact-id pass.
            return false;
        }
        self.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        true
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        Vec::new()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        Vec::new()
    }

    fn set_floating_surface_bounds(&mut self, _bounds: vello::kurbo::Rect) {}

    fn on_floating_surface_host_message(
        &mut self,
        _message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
    }
}

/// Optional second-output presentation capability for app roots.
pub trait AudiencePresentationProvider {
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
        None
    }

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

    fn handle_audience_presentation_event(
        &mut self,
        _event: &WindowEvent,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        false
    }

    fn on_audience_presentation_closed(&mut self) {}
}

/// Optional OS-fullscreen capability for roots and forwarding containers.
pub trait FullscreenProvider {
    /// `None` means no opinion; otherwise this is a level-triggered intent.
    fn fullscreen_intent(&self) -> Option<bool> {
        None
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        crate::fullscreen::FullscreenRequest::mirror(crate::fullscreen::FullscreenTarget::Largest)
    }

    fn on_fullscreen_changed(&mut self, _active: bool) {}
}

/// Optional full-document/canvas capture capability.
pub trait CompleteContentProvider {
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

    /// Apply `action` of the action type `type_id` now, through `host`, under `context`. #
    /// Errors Returns the action's refusal; nothing is written then.
    #[doc(hidden)]
    pub fn apply<A>(
        &mut self,
        host: &mut crate::local_dispatch::LocalDispatch,
        type_id: &str,
        action: &A,
        context: tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError>
    where
        A: tilo_actions::Action + serde::Serialize,
    {
        host.try_dispatch_with_context(self.doc, type_id, action, context)
    }

    /// The lent document, for the hosts that pass it on to a record of their own.
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
    static LAYOUT_MOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// From a widget's `update`: an animation of its own moved or resized what
/// it lays out (a glide, a jump in progress, a give springing back, a pane
/// gliding open, a reveal growing a pill), so the last layout no longer
/// stands. The host lays the window out again in this frame; it is no
/// change of the model, and nothing syncs.
///
/// A frame only an animation asked for is otherwise only drawn: an
/// animation that moves nothing it lays out (a spinner, a fade, a pose
/// applied as a draw transform) costs a paint, not a layout and a
/// republication of the app's actions. A widget whose animation moves
/// layout calls this on every frame it runs, including the one it lands on.
pub fn note_layout_moved() {
    LAYOUT_MOVED.with(|moved| moved.set(true));
}

/// Whether [`note_layout_moved`] ran since the last call, clearing it: a
/// host asks after its widgets' update, and lays out again when it did.
pub fn take_layout_moved() -> bool {
    LAYOUT_MOVED.with(|moved| moved.replace(false))
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

    /// The stylus that made the `CursorMoved` or `MouseInput` being handled: its pressure, tilt and
    /// whether the eraser end is in use. `None` for a mouse, a touch or any other event; a pen's
    /// tip is the left button and its barrel buttons the right and middle, so a widget needs this
    /// only for what a mouse cannot say.
    #[must_use]
    pub fn pen(&self) -> Option<PenSample> {
        crate::pen::current()
    }

    /// `position`, a pointer position in physical pixels, in this event's logical
    /// coordinates.
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

/// One remote participant's live presence (caret/selection), published by a multi-
/// participant widget under that participant's identity.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticipantPresence {
    pub participant: tilo_actions::ParticipantId,
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

/// Ergonomic forwarding API for optional floating-surface capabilities.
///
/// This keeps host/container call sites concise while the core [`Widget`]
/// contract contains only capability accessors.
pub trait FloatingSurfaceWidgetExt {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool);
    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>);
    fn on_floating_surface_event_source(&mut self, id: Option<&str>);
    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &crate::winit::event::WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool;
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

    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &crate::winit::event::WindowEvent,
        ctx: &mut EventCtx<'_>,
    ) -> bool {
        capability::capability_mut::<dyn FloatingSurfaceProvider>(self)
            .is_some_and(|provider| provider.floating_surface_event(id, event, ctx))
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
    fn leaf_node(&mut self, style: taffy::Style) -> NodeId;

    fn parent_node(&mut self, style: taffy::Style, children: &[NodeId]) -> NodeId;

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

pub mod action_proposal;
pub use action_proposal::{
    ActionProposal, ActionProposalView, DiffLine, DiffTag, ProposalDecision, ProposalTarget,
};
pub mod binding;
pub mod capability;
mod composed;
pub use composed::Composed;
pub mod controls;
pub mod decorator;
mod defaults;
pub mod document_effects;
pub mod document_offers;
pub mod identity;
pub use crate::pen::PenSample;
pub use decorator::Decorator;
pub use document_offers::{DocumentOffer, PaintedOffers};
pub use identity::Identified;

pub(crate) mod layout;
pub mod remount;
pub use remount::Remount;
mod reach;
pub(crate) use reach::find;
pub use reach::find_mut;
#[cfg(debug_assertions)]
mod paint_pass;
#[cfg(test)]
pub(crate) mod replaced_tripwire;
pub use layout::{
    AuthoredExtent, DeviceScaleGuard, ExtentSizing, SkippedFrames, WidgetLayoutError,
    compute_widget_layout, device_scale, enter_device_scale, insets, insets_all, insets_xy,
    layout_clip, placing_again, text_control_height, with_layout_clip,
};
#[cfg(debug_assertions)]
pub use paint_pass::painting;

mod cached_paint;
pub use cached_paint::CachedPaint;
mod button;
pub use button::{
    Button, ButtonFace, ButtonProps, ButtonRole, REST_BORDER_WIDTH, face_colors, focus_ring_reach,
};
mod control_size;
pub use control_size::ControlSize;
mod bool_control;
pub(crate) use bool_control::BoolAction;
mod checkbox;
#[cfg(test)]
mod control_hand_on_tests;
pub use checkbox::{CheckGlyph, CheckMotion, CheckPose, Checkbox, CheckboxFace, CheckboxProps};
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
    Density, Effective, FieldLayout, FormLayout, HeaderRowLayout, HeaderRows, LabelPlacement,
    LayoutInfo, LayoutOrigin, ListLayout, ManualConflict, ManualLayout, PageLayout, RowLayout,
    ScrollDirection, ScrollLayout, SectionLayout, ShellLayout, SidebarStanding, TextLayout,
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
    SwitchMotion, SwitchPose, Toggle, ToggleColors, ToggleFace, ToggleProps, toggle_accessibility,
    toggle_face_colors,
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
pub use link::{
    Link, LinkProps, os_open as open_external_url, try_os_open as try_open_external_url,
};
mod flap;
pub use flap::{FlapState, SplitLayout};
mod bottom_sheet;
pub use bottom_sheet::{BottomSheet, SheetDetent, SheetFrame, SheetGeometry, SheetInput};
mod page_slide;
pub use page_slide::{PageSlide, PageSlideProps};
mod sidebar;
pub use sidebar::{
    DEFAULT_DRAWER_BREAKPOINT, DEFAULT_DRAWER_DOCKED_WIDTH, DEFAULT_DRAWER_WIDTH, SidebarDrawer,
    SidebarSide,
};
mod nav_shell;
#[cfg(feature = "native")]
pub(crate) use nav_shell::memory::remember_for_app as remember_sidebar_widths_for_app;

/// The narrowest the window `root` lays out still works at
/// ([`Widget::minimum_window_width`]): the first widget in the tree with an
/// opinion, outermost first.
pub fn minimum_window_width<W: Widget + ?Sized>(root: &W) -> Option<f64> {
    first_window_answer(root, W::minimum_window_width, &|widget| {
        widget.minimum_window_width()
    })
}

/// The size the window `root` lays out wants to be ([`Widget::preferred_window_size`]): the first
/// widget in the tree with an opinion, outermost first.
pub fn preferred_window_size<W: Widget + ?Sized>(root: &W) -> Option<(f64, f64)> {
    first_window_answer(root, W::preferred_window_size, &|widget| {
        widget.preferred_window_size()
    })
}

fn first_window_answer<W: Widget + ?Sized, T>(
    root: &W,
    own: impl FnOnce(&W) -> Option<T>,
    ask: &dyn Fn(&dyn Widget) -> Option<T>,
) -> Option<T> {
    let _entry = root
        .callback_binding()
        .map(|binding| binding.enter())
        .transpose()
        .ok()?;
    if let Some(answer) = own(root) {
        return Some(answer);
    }
    let mut found = None;
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = first_answer_under(child, ask);
        }
    });
    found
}

fn first_answer_under<T>(widget: &dyn Widget, ask: &dyn Fn(&dyn Widget) -> Option<T>) -> Option<T> {
    first_window_answer(widget, |widget| ask(widget), ask)
}
pub mod window_controls;
pub mod window_header;
pub(crate) mod window_toolbar_row;
pub use nav_shell::{
    ContentKind, InspectorFrame, InspectorLink, InspectorPlacement, NavGeometry, NavMode, NavPane,
    NavShell, NavShellProps, OverviewPlacement, SidebarPlacement, WINDOW_BACK, WINDOW_SIDEBAR,
    WindowBarLink,
};
pub use window_header::{
    ColumnFrame, HISTORY_CONTROL_ID, HeaderStatus, HistoryControl, HistoryOpener, STATUS_ACTION_ID,
    StatusAction, WindowBar, WindowBarSlot, WindowHeader, WindowHistory, WindowStatus,
    merged_row_holds_search, merged_row_width, merged_row_width_in,
};
mod nav_list;
pub use nav_list::{
    NavAction, NavArt, NavDrop, NavEntry, NavFeed, NavItem, NavItemProps, NavItemStatus, NavList,
    NavListProps, NavPerson, NavSearchTerm, NavSection, NavSectionProps,
};
mod document_viewport;
pub use document_viewport::{
    ContentDraw, DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, DEFAULT_PADDING, DEFAULT_PAN_STEP,
    DocumentViewport, PAN_SLOP, SmallContentAlign, ViewportState, ViewportZoom,
};
mod floating_window;
pub use floating_window::{FloatingWindow, MIN_HEIGHT, MIN_WIDTH, TITLE_BAR_HEIGHT};
mod text;
pub use text::{Text, TextProps};
mod relative_time;
pub use relative_time::{RelativeTime, TimeProps, clock_text, next_change, relative_text};
mod spacer;
pub use spacer::Spacer;
mod when;
pub use when::{Absent, Fitted, When, optional};
mod slot;
pub use slot::{Slot, SlotHandle, Switch};
mod columns;
pub use columns::Columns;
/// The layout primitives: `Stack`, `Row`, `Grid`, `Split`, `Page`, `Layers`.
pub mod arrange;
mod image;
pub use image::{Image, ImageDecodeError, ImageFit, ImageProps, PictureSource};
mod image_crop;
pub use image_crop::{CropEditor, CropSelection, ImageCropEditor, ImageCropEditorProps};
mod slider;
pub use slider::{
    Slider, SliderFace, SliderProps, SliderTouch, SliderTouchStep, paint_held_readout,
};
mod text_input;
pub use text_input::{
    AccessoryAction, FieldFace, PlaceholderFit, PromptFit, SpanHighlighter, TextActionRequest,
    TextActionResult, TextInput, TextInputKind, TextInputProps, paint_invalid_edge,
    paint_preedit_underline,
};
pub mod code_editor;
pub use code_editor::{
    CodeEditor, CodeEditorProps, Highlighter, HlState, Language, LineSpans, Span, TokenKind,
};
mod editable_label;
pub use editable_label::{EditOutcome, EditableLabel, EditableLabelProps};
pub mod acknowledgement;
pub mod autocomplete;
pub use autocomplete::{
    Autocomplete, AutocompleteOutcome, AutocompleteProps, CheckColumn, Suggester, Suggestion,
    SuggestionList, SuggestionPerson, SuggestionProps,
};
pub mod mention_picker;
pub use mention_picker::{MentionCandidate, MentionQuery};
pub mod completion;
pub(crate) mod completion_card;
pub mod text_completion;
pub use text_completion::{EmojiQuery, TextCompletion};
mod number_input;
pub use number_input::{NumberInput, NumberInputProps, NumberStepperFace, StepperPart};
pub mod marks;
pub use marks::{Mark, MarkSpan};
pub mod markdown_input;
pub mod rich_text_line;
pub use rich_text_line::{DeclaredLine, LineSignal, RichTextLine, RichTextLineProps};
pub mod block_editor;
mod block_editor_body;
pub mod paint_damage;
pub mod rich_doc;
pub use block_editor::references::{BlockEditorProps, OPEN_REFERENCE};
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
pub use page_sidebar::{DeclaredRail, PageSidebar, PageSidebarEvent, PageSidebarProps};
mod dropdown;
pub use dropdown::{Dropdown, DropdownFace, DropdownProps, FILTER_MIN_OPTIONS};
pub mod aim;
pub use aim::Trajectory;
mod hover_intent;
pub use hover_intent::EdgeAim;
pub mod top_edge_reveal;
pub use top_edge_reveal::{EdgeGeometry, TopEdgeReveal};
pub mod tabs;
pub use tabs::{
    PresenceFaceProps, TabBar, TabDescriptor, TabHolder, TabItemProps, TabMark, TabStrip,
    TabStripProps, draw_app_mark, pack_point, unpack_point,
};
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
pub use board::{Board, BoardCardProps, BoardDrop, BoardLaneProps, BoardProps};
pub use drag_bus::{InFlight, Landed};
pub use drag_run::{DragDrop, DragRun, RunAxis, RunGroup, RunItem};
pub use drag_run_component::{DragItemProps, DragRunProps};
pub use drag_source::{DragSource, DragSourceProps, RESOURCE_PAYLOAD};
pub use drop_target::{DropTarget, DropTargetProps};
pub mod drag;
pub mod drag_bus;
pub mod drag_run;
pub mod drag_run_component;
pub mod drag_source;
pub mod drop_target;
pub use drag::{Axis as DragAxis, DragController, ListMetrics, REORDER_THRESHOLD_PX};
mod scrollable;
pub use scrollable::{ScrollAxis, ScrollProps, Scrollable};
mod scroll_region;
pub use scroll_region::ScrollRegion;
pub use scrollbar::BarFade;
mod overlay_scrollbar;
pub use overlay_scrollbar::OverlayScrollbar;
pub mod panic_scope;
mod tile_host;
pub use tile_host::{
    AppKind, TileContent, TileHost, hold_windows, record_unavailable_title, release_windows,
};
mod app_widget;
pub(crate) use app_widget::BoundAppWidget;
pub use app_widget::{
    AppCommand, AppHostIntent, AppWidget, DeviceDisplayMoveReply, HostShellAllow, IconDescriptor,
    WindowPublication,
};
pub mod input_hint;
pub mod shortcut_recorder;
pub use input_hint::{InputDeviceGlyph, InputHint, InputHintProps, LegendConventions};
pub mod canvas_navigation_hint;
pub use canvas_navigation_hint::{CanvasNavigation, CanvasNavigationHint};
pub use shortcut_recorder::{ShortcutRecorder, ShortcutRecorderProps};
pub mod context_menu;
pub mod floating_list;
pub use floating_list::{FloatingList, FloatingMetrics, FloatingRow, RowState};
pub mod floating_surface;
pub mod primitives;
pub use context_menu::{
    ContextMenu, MenuButton, MenuButtonProps, MenuItem, MenuItemProps, MenuPick, note_shift,
    shift_held, shift_skips_confirmation,
};
mod row_menu;
pub use chrome_body::paint_bar_body;
pub use primitives::{
    ActivityCardFace, BarChip, BarChipFace, BarChipLayout, BarChipProps, BarControlFace,
    DeclaredModal, DeclaredPanel, DeclaredPopover, DeclaredPopoverButton, DensityRegion,
    DisclosureGroup, DisclosureRow, DisclosureRowProps, DisclosureState, Fact, FactMark, FactProps,
    Facts, FactsFace, FactsProps, FloatingPanel, FloatingPanelFace, FloatingPanelLayout,
    FloatingPanelProps, IconButton, KeyHint, KeyHintProps, KeyHints, KeyHintsFace, KeyHintsProps,
    LinkKind, LinkStatus, ModalProps, NotificationListControl, NotificationListLayout,
    NotificationListRow, NotificationListView, NotificationRowControls, PopoverProps,
    StatusCluster, StatusClusterFace, StatusClusterLayout, StatusSlot, Surface, SurfaceLevel,
    TextBlockFace, TextLineAlign, TextLineFace, TextStack, TextStackFace, TextStackProps,
    badge_label, badge_size, control_fill, control_radius, paint_control_row, paint_see_through,
    paint_severity_rail, paint_surface, paint_unread_badge, paint_window_card,
};
pub use row_menu::RowMenu;
pub mod context_menu_area;
pub use context_menu_area::{ContextMenuArea, MenuEntry};
pub mod send_to;
pub use send_to::{SendToBox, SendToFocus};
pub mod attached;
mod hover_toolbar;
pub use hover_toolbar::HoverToolbar;
mod swipe_row;
pub use attached::{
    Attachments, ContextMenuProps, EmojiBarProps, HoverToolbarProps, MenuPart, MenuSeparatorProps,
    SwipeProps,
};
mod rich_input;
pub use rich_input::{RichInput, RichInputProps};
mod chat_parts;
pub use chat_parts::{EmojiPickerButton, EmojiPickerProps, Quote, QuoteProps, ReactionsProps};
pub use swipe_row::SwipeRow;
mod pacer;
pub use pacer::Pacer;
pub(crate) mod block_editor_table;
pub mod data_table;
mod data_table_component;
pub use data_table_component::{
    DataTableCellProps, DataTableColumnProps, DataTableProps, DataTableRowProps, DescribedTable,
};
pub mod table {
    pub use super::block_editor_table::*;
}
pub use table::Table;
mod window_group;
pub(crate) use window_group::place_search as place_window_search;
pub use window_group::{
    DEFAULT_STRIP_USABLE_PX, RESIZE_HANDLE_PX, WIDTH_FLOOR_PX, WIDTH_MAX_PERMILLE,
    WIDTH_MIN_PERMILLE, WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
    WindowGroup, WindowGroupBounds, WindowWidth, max_group_px, min_group_px, permille_for_px,
};
pub mod cursor;
pub mod edge_resize;
pub use edge_resize::{EdgeGrab, ResizeMode, WidthLimits, WindowEdge};
mod resize_handle;
pub use resize_handle::{HANDLE_HIT_HALF_PX, HANDLE_SLOT_PX, HANDLE_VISIBLE_PX, ResizeHandle};
mod overflow_door;
pub(crate) use overflow_door::give_way;
pub use overflow_door::{OverflowDoor, plan as plan_overflow};
mod group_row;
pub use group_row::{
    CANVAS_ZOOM_MAX, CANVAS_ZOOM_MIN, CanvasView, CrossStripDetach, FLOAT_CARD_ROOM,
    FLOAT_CARD_SIZE, FLOAT_SIZE, FloatAsk, GroupRow, GroupTopology, HintPlaces, JUMP_PX_PER_DETENT,
    OutputSpan, Pan, STRIP_BOTTOM_GUTTER_PX, STRIP_EDGE_GUTTER_PX, STRIP_TOP_GAP_PX, StripLane,
    TabDropTarget, WheelJumpAccum, keep_float_inside, pan_within_canvas, wheel_jump_detents,
};
mod finger_path;
pub mod tooltip;
pub use tooltip::{Tooltip, TooltipHost};
pub mod permission_prompt;
pub mod toast;
pub use permission_prompt::{PROMPT_HEIGHT, PermissionPrompt, PromptChoice};
pub mod file_picker;
pub mod file_table;
pub use file_picker::{FilePicker, FilePickerEntry, FilePickerMode, FilePickerOutcome};

pub mod progress_bar;
pub use progress_bar::{ProgressBar, ProgressBarFace, ProgressBarProps};
pub mod page_preview;
pub use page_preview::{PagePreview, PagePreviewProps};
pub mod spinner;
pub use spinner::{Spinner, SpinnerProps};
pub mod program_step;
pub use program_step::{ProgramStepView, StepAnswer};
pub mod date_field;
pub mod date_picker;
pub mod enum_field;
pub mod person_field;
pub use date_field::{DateField, DateFieldProps};
pub use date_picker::{DateFeed, DatePicker, DatePickerProps, DateShown, ExternalDatePickerHit};
pub use enum_field::{EnumField, EnumFieldProps, EnumOptionProps};
pub use person_field::{PersonField, PersonFieldProps};
pub mod time_field;
pub mod time_picker;
pub mod timeline_density;
pub use time_field::{TimeField, TimeFieldProps};
pub use time_picker::{TimePicker, TimePickerProps, TimeValue};
pub use timeline_density::TimelineDensity;
pub mod color_picker;
pub mod color_swatch;
pub use color_picker::{ColorPicker, ColorPickerOutcome, ColorPickerProps};
pub use color_swatch::{
    ColorSwatch, ColorSwatchFace, ColorSwatchPicker, ColorSwatchPickerProps, SwatchPainter,
};
pub mod picture;
pub mod picture_choice;
pub use picture_choice::{
    ColourFilter, Picture, PictureChoice, PictureChoiceProps, PictureKind, PictureOption,
    PictureOptionProps, TouchpadGesture,
};
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
pub use toolbar::{
    Toolbar, ToolbarButtonProps, ToolbarPart, ToolbarProps, ToolbarSeparatorProps,
    ToolbarSpacerProps,
};
pub mod undo_button;
pub use undo_button::UndoButton;
pub mod breadcrumb;
pub use breadcrumb::{Breadcrumb, BreadcrumbProps, BreadcrumbSegmentProps};
pub mod avatar;
pub use avatar::{Avatar, AvatarFace, AvatarProps, AvatarSize, PresenceStatus, initials_for_name};
pub mod badge;
pub use badge::{Badge, BadgeContent, BadgeFace, BadgeProps};
pub mod icon;
pub use icon::{IconProps, IconSize, IconWidget};
pub mod signal_meter;
pub use signal_meter::{SIGNAL_BARS, SignalLevel, SignalMeter, SignalMeterFace, SignalMeterProps};
pub mod drawing;
pub use drawing::{Drawing, DrawingProps, Ink, Meaning, Shape, Weight};
pub mod chart;
pub use chart::{
    Chart, ChartLabels, ChartStyle, RollingSeries, Series, Sparkline, TrendCurve, TrendFill,
    TrendRange, TrendStyle,
};
pub(crate) mod chart_data;
pub(crate) mod chart_geometry;
pub(crate) mod chart_labels;
pub use chart_data::{ChartItem, ChartLink, ChartNode, GraphSelection};
pub mod chart_legend;
pub mod chart_navigation;
pub use chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action,
    moved_index as move_chart_index, moved_item as move_chart_item,
};
pub mod plot;
pub use plot::{Plot, PlotFrame, PlotItem, PlotMarks};
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
    BarGauge, BarGaugeProps, GaugeReading, GaugeStyle, GaugeThresholds, ProgressRing,
    ProgressRingFace, ProgressRingProps, ProgressRingSize, RingReading,
};
pub mod function_plane;
pub use function_plane::FunctionPlane;
pub mod resizable_split;
pub use resizable_split::{ResizableSplit, SplitHandle, SplitOrientation, SplitRatio};
pub mod video_player;
pub use video_player::{DeclaredVideo, VideoPlayer, VideoPlayerProps};
pub mod webview;
pub use webview::{WebView, WebViewProps};
pub mod image_viewer;
mod image_viewer_component;
pub use image_viewer::ImageViewer;
pub use image_viewer_component::{ImageViewerProps, ZoomableImage};
mod tree_component;
#[cfg(test)]
pub(crate) use tree_component::items_tree;
pub use tree_component::{
    DescribedTreeView, RemovedNodes, TREE_ROW_HEIGHT, TreeTone, TreeViewNodeProps, TreeViewProps,
    TreeViewRowProps,
};
/// A box a widget paints text into, whose own box cannot hold that text: the height the text
/// takes at the width of its box, the room the box gives it, and what it says. Rows a box lays
/// out and never paints read as a line that had nothing to say above space standing empty, so
/// an audit looks for them rather than trusting the geometry around them.
#[derive(Clone, Debug, PartialEq)]
pub struct CutText {
    pub said: String,
    pub needs: f32,
    pub has: f32,
}

/// A widget that paints text into boxes of its own: a box that cannot hold its text is asked for
/// through [`capability`](crate::widget::capability::capability).
pub trait TextFits {
    fn cut_text(&mut self, text: &mut crate::text_render::TextRenderer) -> Option<CutText>;
}

mod row_selection;
mod rows;
pub mod virtual_list;
pub use virtual_list::{
    DescribedList, DescribedRows, DuplicateListKey, ItemRow, ListEdges, ListItemProps, ListModel,
    PaintedRowLifts, RowActionRects, RowBuild, RowContentBox, RowGroup, RowLift, RowRenameField,
    RowSelection, RowSelectionSink, TREE_INDENT, TreeMarks, TreeMove, TreeRow, VecListModel,
    VirtualList, VirtualListProps, row_content_box,
};
mod activation;
pub(crate) mod control_verb;
mod press;
pub use press::Press;
mod dismissal;
pub(crate) mod view_choice;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use activation::NATIVE_INVOKE;
pub use activation::{Interaction, PaintedTargets, paint_disabled, paint_focus_ring};
pub(crate) use activation::{press_waiting, take_presses};
pub mod media_card;
mod media_grid_component;
pub use media_card::{
    ArtPainter, MediaArt, MediaAspect, MediaBadge, MediaCard, MediaCardFace, MediaGrid, MediaSize,
    MediaSource, OwnedBadge, justified_rows,
};
pub use media_grid_component::{MediaCardProps, MediaGridProps};
pub mod capture_button;
pub use capture_button::{CaptureButtonFace, CaptureKind};
pub mod transport;
pub use transport::{TransportControl, TransportFace};
pub mod media_transport;
mod media_transport_component;
pub use media_transport::{
    MediaTransport, TransportActions, TransportArt, TransportEvent, TransportFeed, TransportRepeat,
    TransportState, TransportTriggers,
};
pub use media_transport_component::{DeclaredTransport, MediaTransportProps};
pub mod now_playing;
pub use now_playing::{Scrub, Song, SongFace};
pub mod drop_zone;
pub use drop_zone::{DropZone, DropZoneFace, DropZoneProps};
pub mod file_open;
pub use file_open::{FileOpenEvent, FileOpenSurface};
pub mod skeleton;
pub use skeleton::{
    Skeleton, SkeletonFace, SkeletonForm, SkeletonProps, SkeletonShape, skeleton_tone,
};
pub mod arrival_failure;
pub use arrival_failure::{WindowFailure, arrival_failure};
pub mod arriving_rows;
pub mod arriving_view;
pub use arriving_rows::{ArrivingRows, RowFade};
pub use arriving_view::{ArrivalFace, ArrivingView, NamedOnce, ReservedShape, Showing};
pub mod stepper;
pub use stepper::{Stepper, StepperFace, StepperProps};
pub mod property_list;
pub use property_list::{
    Property, PropertyColumn, PropertyList, PropertyListFace, PropertyListProps, PropertyProps,
    PropertyRow, PropertyValue, property_label_column,
};
pub mod list_row;
pub use list_row::{
    CARD_TITLE_LINES, CardBody, ControlRow, ControlRowProps, ListRow, ListRowFace, ListRowProps,
    RowAction, RowLeading, RowLeadingShape, RowShape, RowStatus, RowValue, ShownChange,
    SwipeAction, SwipeActionProps, badge_column_width, mark_text_inset,
};
pub mod toggle_group;
pub use toggle_group::{
    Segment, SegmentProps, SegmentedFace, SegmentedStyle, SegmentedTouch, SegmentedTouchGroup,
    SegmentedTouchResult, ToggleGroup, ToggleGroupProps, ViewTabsProps, segment_colors,
};
pub mod view_tabs;
pub use view_tabs::ViewTabs;
pub mod quick_tile;
pub use quick_tile::{QuickTile, QuickTileOutcome, QuickTilePart};

pub mod chrome_body;
pub use chrome_body::{ChromeBody, ChromeEdge, ChromeProps, ChromeShape};
pub mod docked;
pub use docked::{Docked, DockedProps};
pub mod map;
pub use map::{MAP_ZOOM_MAX, MAP_ZOOM_MIN, Map, MapCamera, MapPinProps, MapProps, PinPlace};
pub mod window_well;
pub use window_well::{Wells, WindowWell, WindowWellProps};
pub mod window_preview;
pub use window_preview::{Previews, WindowPreview, WindowPreviewProps};
pub mod window_frame;
pub use window_frame::{WindowFrame, WindowFrameProps};
pub mod empty_state;
pub use empty_state::{
    EmptyState, EmptyStateContent, EmptyStateLayout, EmptyStateProps, PaintedStack,
};
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
    InPlace, QueryView, Queryable, SearchDriver, SearchHandoff, SearchReach, SearchableWindow,
    WindowSearch, WindowSearchRequest, with_queryable,
};
pub mod search_results;
pub use search_results::SearchResultsView;
pub mod stat_tile;
pub use stat_tile::{StatGroup, StatGroupProps, StatTile, StatTileFace, StatTileProps, Trend};
pub mod banner;
pub use banner::{Banner, BannerContent, BannerHit, BannerKind, BannerNotice, BannerProps};
mod disclosure_band;
pub use disclosure_band::{DisclosureBand, DisclosureBandProps};
pub mod chip;
pub use chip::{Chip, ChipColors, ChipFace, ChipLead, ChipProps, ChipStyle, REFERENCE_LEAD};
pub mod change_mark;
pub use change_mark::{ChangeMark, ChangeMarkProps};
pub mod conversation;
pub use conversation::*;
pub mod card;
mod tone;
pub use card::{
    Actions, Card, CardFace, CardGrid, CardGridCardProps, CardGridProps, CardProps, CardWidth,
    DescribedCardGrid, card_column_width, card_columns,
};
pub use tone::Tone;
mod section;
pub use section::{
    ObjectHeader, ObjectHeaderProps, Section, SectionHeader, SectionHeaderFace, SectionProps,
    StatusLine, StatusLineProps, band_height, heading_count,
};
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
mod activity_timeline_component;
pub mod history;
pub mod history_face;
pub mod history_group;
pub mod name_field;
pub use activity_timeline::ActivityTimeline;
pub use activity_timeline_component::{
    ActivityArtifact, ActivityEntryProps, ActivityTimelineProps, DescribedActivityTimeline,
};
pub use collab_types::ArtifactKind;
pub use history::{
    AuthorRelation, EntryKind, HistoryAttribution, HistoryAuthor, HistoryClassification,
    HistoryEntry, HistoryEntryId, HistoryFilter, RedactionDisplay, format_relative_time,
    hue_to_rgb,
};
pub use history_face::{EntryBody, EntryFace, HistoryEntryPresenter};

pub mod qr;
mod qr_component;
pub mod resource_actions;
pub use qr::QrWidget;
pub use qr_component::QrProps;

#[cfg(test)]
mod a11y_tree_tests;

#[cfg(test)]
#[path = "flow_cache_tests.rs"]
mod flow_cache_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "control_touch_tests.rs"]
mod control_touch_tests;

#[cfg(test)]
pub(crate) mod control_result_probe;

pub mod keyboard_map;
pub use keyboard_map::{KeyboardMap, KeyboardMapInput, KeyboardMapKey, KeyboardMapSelection};
/// Logical screen topology canvas shared by local and native peer displays.
pub mod display_arrangement;
mod graph_canvas;
pub use graph_canvas::{GraphCanvas, GraphCanvasConnection, GraphCanvasNode};
