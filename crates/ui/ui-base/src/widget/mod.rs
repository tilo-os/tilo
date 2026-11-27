mod contract;
#[cfg(feature = "model")]
pub use contract::AppActivation;
pub use {
    contract::COMPACT_BELOW, contract::EXPANDED_BELOW, contract::FIT_EPS, contract::HYSTERESIS,
    contract::Length, contract::MEDIUM_BELOW, contract::MediaAspect, contract::PAGE_FRACTION,
    contract::PrimaryScrollAction, contract::PrimaryScrollRegion, contract::PrimaryScrollResult,
    contract::RowLeadingShape, contract::RowShape, contract::ScrollAxis, contract::ScrollMetrics,
    contract::SkeletonShape, contract::TooltipSide, contract::Widget, contract::WidthClass,
};
mod action_directory_identity;
pub mod bound_nodes;
pub mod component_walk;
pub mod input_provenance;
pub mod undo_access;
pub mod undo_points;
pub mod wheel;
pub use {
    wheel::PhaselessWheel, wheel::WHEEL_GESTURE_GAP, wheel::line_of_wheel, wheel::sideways_travel,
};
pub mod action_emitter;
pub mod disclosing;
pub use action_directory_identity::ActionDirectoryInvocationDomain;
pub use action_emitter::ActionEmitter;
pub use {
    disclosing::BandMotion, disclosing::Ghosts, disclosing::content_box,
    disclosing::follower_shifts, disclosing::place_bands, disclosing::route_to_shifted_children,
};
pub use {disclosing::Disclosing, disclosing::Placement, disclosing::event_offset};
pub mod resource_operation;
// The raw operation protocol between the host and the toolkit's own widgets,
// reached from outside the toolkit only through `crate::host_operation`.
pub use resource_operation::resource_operation_handlers;
#[cfg(feature = "model")]
pub use resource_operation::start_operation_refused;
pub use {
    resource_operation::ResourceOperation, resource_operation::ResourceOperationStart,
    resource_operation::ResourceOperations, resource_operation::local_resource_operation_handlers,
    resource_operation::poll_resource_operation, resource_operation::start_in_children,
    resource_operation::start_operation,
};

#[cfg(feature = "model")]
pub use crate::presence::PresenceKey;
pub use crate::text_render::TextRenderer;
pub use crate::theme::Theme;
pub use vello::Scene;
pub use vello::kurbo::Affine;
pub use winit::event::WindowEvent;
pub use {crate::draw::DrawCtx, crate::draw::DrawEnv};
pub use {automerge::AutoCommit, automerge::ObjId};
pub use {taffy::NodeId, taffy::TaffyTree};

use crate::time::Instant;
pub use std::collections::HashMap;
#[cfg(feature = "model")]
pub use std::collections::HashSet;

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
    Action(tilo_action_types::ActionAddress),
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
    /// the resource/action API, so no parallel callback registry is needed.
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

/// Where `widget` stands in the widget tree, under `parent_path`: the name its parent calls it by
/// ([`Widget::child_path_name`]), else its kind numbered among the siblings of that kind before it.
/// With its kind and that number, which a path's bare form (`parent/Kind`, the first of its kind)
/// reads. `counts` holds the numbering of one parent's children.
#[expect(
    clippy::implicit_hasher,
    reason = "a walk keeps its sibling counts with the default hasher"
)]
pub fn widget_path(
    widget: &dyn Widget,
    parent_path: &str,
    called: Option<String>,
    counts: &mut std::collections::HashMap<String, usize>,
) -> (String, String, usize) {
    let name = widget.name().to_string();
    let index = counts.entry(name.clone()).or_insert(0);
    let current = *index;
    // Bounded by siblings under one parent; saturating makes that explicit.
    *index = index.saturating_add(1);
    let part = called.unwrap_or_else(|| format!("{name}[{current}]"));
    let path = if parent_path.is_empty() {
        part
    } else {
        format!("{parent_path}/{part}")
    };
    (name, path, current)
}

/// How readily `widget` takes the keyboard when its app does without a control of its own choosing:
/// its default keyboard control first (`Some(0)`), else the main answer of the panel it stands in
/// beside one that takes typing — a field, an editor, a terminal (`Some(1)`) — never a button merely
/// for being first (`None`): Enter would press it.
pub fn keyboard_entry_rank(widget: &dyn Widget) -> Option<u8> {
    if widget.is_default_keyboard_focus() {
        return Some(0);
    }
    if widget.is_main_answer() {
        return Some(1);
    }
    // The widget's own node is enough to read its role: the whole subtree of a
    // document (every block of a long note) would be built on every relayout.
    widget
        .accessibility_root()
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

/// Offer the focused control under `widget` the activation the window gives on Space, or on Enter
/// (`buttons_only`) only when it is a button; whether it took it. A surface that holds the
/// keyboard itself — a dialog, a panel — presses the answer the keyboard is on with this, as the
/// window would otherwise do behind it.
pub fn activate_focused(
    widget: &mut dyn Widget,
    state: &mut crate::widget::Edits<'_>,
    path: &ModelPath,
    buttons_only: bool,
) -> bool {
    if widget.is_focused() && widget.is_focusable() {
        let button = widget
            .accessibility()
            .is_some_and(|node| node.role == crate::a11y::Role::Button);
        return (button || !buttons_only)
            && widget.perform_action(&crate::a11y::Action::Activate, &[], state, path);
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| activate_focused(child, state, path, buttons_only))
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

pub const SETTLE_FRAMES: usize = 3;

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

#[doc(hidden)]
#[macro_export]
macro_rules! pressable {
    ($($rect:ident).+) => {
        fn cursor_shape(
            &self,
            point: $crate::kurbo::Point,
        ) -> Option<$crate::winit::window::CursorIcon> {
            (self.activation.is_bound() && self.$($rect).+.contains(point))
                .then_some($crate::winit::window::CursorIcon::Pointer)
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
    env: &DrawEnv<'_>,
) {
    draw_child_at(child, None, scene, transform, text_renderer, theme, env);
}

/// Draw `paint` at `opacity`, in a layer only while it is part-way, and draw nothing at all once
/// it is gone: the one way a view drawn translucent (one fading in or out over another) reaches
/// the scene.
pub fn in_opacity_layer(
    scene: &mut Scene,
    transform: Affine,
    clip: vello::kurbo::Rect,
    opacity: f32,
    paint: impl FnOnce(&mut Scene),
) {
    if opacity <= 0.0 {
        return;
    }
    let part = opacity < 1.0;
    if part {
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &clip.inflate(2.0, 2.0),
        );
    }
    paint(scene);
    if part {
        scene.pop_layer();
    }
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
    env: &DrawEnv<'_>,
) {
    if env.should_cull(child.paint_bounds()) {
        if index.is_none() {
            crate::scene_parts::skip_widget();
        }
        return;
    }
    draw_child_unculled(child, index, scene, transform, text_renderer, theme, env);
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
    env: &DrawEnv<'_>,
) {
    #[cfg(debug_assertions)]
    let open = scene.encoding().n_open_clips;
    #[cfg(debug_assertions)]
    let _painting = paint_pass::Painting::enter();
    let begun = crate::scene_parts::begin_widget(scene, child, index, transform);
    {
        let mut ctx = DrawCtx::new(scene, transform, text_renderer, theme, env.clone());
        #[expect(
            clippy::disallowed_methods,
            reason = "the one keyed draw path a widget is drawn through"
        )]
        child.draw(&mut ctx);
    }
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
/// itself: `child.draw_child(scene, transform, text_renderer, theme, env)`
/// takes the place of calling the child's [`Widget::draw`], and carries the
/// child's key into the scene for a host that keys widgets.
pub trait DrawChild {
    fn draw_child(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        env: &DrawEnv<'_>,
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
        env: &DrawEnv<'_>,
    ) {
        #[cfg(debug_assertions)]
        let _painting = paint_pass::Painting::enter();
        let begun = crate::scene_parts::begin_widget(scene, self, None, transform);
        {
            let mut ctx = DrawCtx::new(scene, transform, text_renderer, theme, env.clone());
            #[expect(
                clippy::disallowed_methods,
                reason = "the keyed draw path of a widget held by value"
            )]
            self.draw(&mut ctx);
        }
        crate::scene_parts::end_widget(scene, begun);
    }
}

/// `DrawChild` for a widget trait object, boxed or not: drawn whole, never culled, since a
/// trait object's own box is all a parent knows of it.
#[doc(hidden)]
#[macro_export]
macro_rules! draw_child_of_dyn {
    ($($object:path),* $(,)?) => {$(
        impl $crate::widget::DrawChild for dyn $object + '_ {
            fn draw_child(
                &mut self,
                scene: &mut $crate::vello::Scene,
                transform: $crate::kurbo::Affine,
                text_renderer: &mut $crate::text_render::TextRenderer,
                theme: &$crate::theme::Theme,
                env: &$crate::draw::DrawEnv<'_>,
            ) {
                $crate::widget::draw_child_unculled(self, None, scene, transform, text_renderer, theme, env);
            }
        }

        impl $crate::widget::DrawChild for Box<dyn $object + '_> {
            fn draw_child(
                &mut self,
                scene: &mut $crate::vello::Scene,
                transform: $crate::kurbo::Affine,
                text_renderer: &mut $crate::text_render::TextRenderer,
                theme: &$crate::theme::Theme,
                env: &$crate::draw::DrawEnv<'_>,
            ) {
                $crate::widget::draw_child_unculled(
                    &mut **self,
                    None,
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    env,
                );
            }
        }
    )*};
}

crate::draw_child_of_dyn!(Widget);

/// What a widget draws over itself once drawn: the denied-permission scrim
/// its authority forbids, then whatever the host paints over it — the focus
/// ring of whoever else is on it, their caret and selection where the widget
/// publishes the geometry for them.
pub fn draw_over(widget: &dyn Widget, ctx: &mut DrawCtx<'_>) {
    let rect = widget.get_rect();
    if !ctx.env.permits_widget(widget) {
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            ctx.transform,
            crate::tokens::colors::PERMISSION_DENIED_SCRIM,
            None,
            &rect,
        );
    }
    if let Some(overlay) = ctx.env.overlay {
        overlay.paint_over(
            widget,
            ctx.scene,
            ctx.transform,
            ctx.text_renderer,
            ctx.theme,
        );
    }
}

/// Hand `event` to the widgets a virtualizing container has mounted: the
/// first one holding pointer capture gets it alone, otherwise each laid-out
/// widget sees it, topmost first, until one handles it.
pub fn route_to_mounted<T>(
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

/// One bounded page from a widget-owned remote root catalogue.  `next_cursor`
/// is meaningful only while `exhausted` is false and is never interpreted by
/// a different widget or document revision.
pub struct ActionDirectoryResourcePage {
    pub resources: Vec<tilo_actions::ResourceSpec>,
    pub next_cursor: Vec<u8>,
    pub exhausted: bool,
}

/// Last-resort accessible label for controls whose constructor already
/// requires a stable id. Explicit localized names remain preferable, but a
/// standard control should never become silent merely because an author did
/// not repeat the same short label twice.
pub fn humanize_control_id(id: &str) -> String {
    let tail = id.rsplit('.').next().unwrap_or(id);
    crate::format::capitalize_first(&tail.replace(['_', '-'], " "))
}

#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    reason = "test support: failing the calling test is the point"
)]
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

#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    reason = "test support: failing the calling test is the point"
)]
pub fn test_bool_action_binding() -> tilo_actions::ActionBinding<bool> {
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

#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    reason = "test support: failing the calling test is the point"
)]
pub fn test_string_action_binding() -> tilo_actions::ActionBinding<String> {
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

#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::unwrap_used,
    reason = "test support: failing the calling test is the point"
)]
pub fn test_number_action_binding() -> tilo_actions::ActionBinding<f64> {
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
#[cfg(feature = "model")]
pub trait Continuable {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<::collaboration::ContinuationMetadata>;

    /// Apply a continuation projection after launch. Implementations must not
    /// change focus or start media; malformed or unknown metadata is ignored.
    fn restore_continuation_metadata(
        &mut self,
        metadata: &::collaboration::ContinuationMetadata,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool;
}

/// Optional session collaboration capability for widgets that own live broker-backed
/// activity.
#[cfg(feature = "model")]
pub trait Collaborative {
    fn take_collab_requests(&mut self) -> Vec<::collaboration::CollabRequest> {
        Vec::new()
    }

    fn handle_collab_event(&mut self, _event: &::collaboration::CollabEvent) {}
}

/// Optional lifecycle for widgets backed by a platform-native child view.
pub trait NativeViewLifecycle {
    fn mount_native_view(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>);

    fn unmount_native_view(&mut self);

    fn native_view_frame_start(&mut self);
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

/// A player asking the host to float its picture above the window, as picture-in-picture does:
/// the region it draws on, the picture's shape, and what the person calls it.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureInPictureAsk {
    /// The player's region in the window, which the floating surface mirrors.
    pub region: vello::kurbo::Rect,
    /// The picture's shape, width over height: the surface keeps it whatever its size.
    pub aspect: f64,
    /// What the floating surface is called, as the video's title.
    pub title: String,
}

/// Optional picture-in-picture capability for a player a view declares. The person pressed the
/// player's Picture-in-picture control; the host floats the picture with the region and shape the
/// player gives. Read through `&self` because a host walks the tree it draws immutably to find
/// the player, and takes the request once (a [`std::cell::Cell`] inside the player holds it).
pub trait PictureInPictureProvider {
    /// Whether a request waits to be taken, without taking it: a host asks this before it
    /// offers itself as the one that answers for the tree.
    fn wants_picture_in_picture(&self) -> bool {
        false
    }

    fn take_picture_in_picture(&self) -> Option<PictureInPictureAsk> {
        None
    }

    /// Whether the app floats this picture now: the host keeps the surface up while it does,
    /// and returns the picture to where it stands once the app stops. A player whose one press
    /// makes one surface has no lasting state, and answers nothing.
    fn picture_in_picture_floating(&self) -> bool {
        false
    }

    /// The picture stopped floating: the shell's window for it went away, or the person took it
    /// back. What the picture kept of the float is done. A page's float is the app's own prop and
    /// answers nothing.
    fn picture_in_picture_returned(&mut self) {}

    /// Where the picture stands in its window now, once it moved since the host last asked: the
    /// floated surface mirrors that region, so a page that scrolls or a window that resizes
    /// carries the picture with it.
    fn floating_picture_in_picture_region(&self) -> Option<vello::kurbo::Rect> {
        None
    }

    /// Draw the picture into the floating window: what stands at `source` in the window, drawn
    /// over `target` on the shell. Whether it drew anything.
    fn draw_picture_in_picture(
        &mut self,
        _source: vello::kurbo::Rect,
        _target: vello::kurbo::Rect,
        _ctx: &mut crate::draw::DrawCtx<'_>,
    ) -> bool {
        false
    }
}

/// Optional sound capability for a player a view declares: the player decodes the sound but
/// opens no device, and its host plays the one stream in every place the session's outputs name
/// (a speaker in another room, a cast target). The samples are stereo 48 kHz interleaved S16,
/// the shape every playback sink takes. Read through `&self` because a host walks the tree it
/// draws immutably to find the player, and takes the sound once.
pub trait AudioTapProvider {
    fn take_audio(&self) -> Vec<i16> {
        Vec::new()
    }
}

/// Optional full-document/canvas capture capability.
pub trait CompleteContentProvider {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size>;

    fn draw_complete_content(&mut self, bounds: vello::kurbo::Rect, ctx: &mut DrawCtx<'_>) -> bool;
}

/// The document as a host lends it to one pass that may change it: read
/// freely, changed only through a typed action the action host applies (a
/// resource's dispatcher applies it), so every change is an action that is
/// attributed, recorded in history and undone like any other.
/// Direct CRDT mutation is rejected at the widget boundary:
///
/// ```compile_fail
/// use automerge::{ROOT, transaction::Transactable};
/// use crate::widget::Edits;
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

    /// The lent document, for the hosts that pass it on to a record of their own.
    pub fn document_mut(&mut self) -> &mut AutoCommit {
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
/// resources, actions or semantic controls it declares) changed without the
/// host seeing it: on input the widget did not mark handled (hovering a menu
/// row that opens a nested menu), or as it paints (the bounds a control
/// measured from what it drew). The host keeps the published resource tree
/// across bare pointer motion, so such a widget must call this or the tree
/// agents and assistive technology read would go stale. Clicks, keys and
/// handled events outside the explicit view-only scroll contract republish.
pub fn note_resource_presentation_changed() {
    RESOURCE_PRESENTATION_EPOCH.with(|epoch| epoch.set(epoch.get().wrapping_add(1)));
}

/// How many times [`note_resource_presentation_changed`] ran on this thread.
pub fn resource_presentation_epoch() -> u64 {
    RESOURCE_PRESENTATION_EPOCH.with(std::cell::Cell::get)
}

thread_local! {
    static VIEW_ONLY_PAN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tell the host that the event being dispatched was consumed only to move
/// what is laid out: a wheel every widget under the pointer refused that a
/// scroll container took as a pan of its own offset, or a drag step that
/// moved a pane's edge. The host then keeps the published resource tree and
/// skips the full sync, exactly as for a root that declares
/// [`Widget::view_only_scroll`], provided the dispatch changed no document,
/// focus or declared presentation. A widget that does anything else with
/// the event must not call this.
pub fn note_view_only_pan() {
    VIEW_ONLY_PAN.with(|pan| pan.set(true));
}

/// Whether [`note_view_only_pan`] ran since the last call, clearing it.
pub fn take_view_only_pan() -> bool {
    VIEW_ONLY_PAN.with(|pan| pan.replace(false))
}

/// The window holding `widget` lost the keyboard: every control in it that held the keyboard gives
/// it up, as a native window's controls do when another window becomes active. What the person
/// types next is the window they moved to, never the one they left. Whether anything let go.
pub fn release_keyboard(widget: &mut dyn Widget) -> bool {
    let mut released = widget.is_focusable() && widget.is_focused();
    if released {
        widget.set_focus(false);
    }
    for child in widget.children_mut() {
        released |= release_keyboard(child);
    }
    released
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

/// One [`reveal`] asked for, and how many hosts looked for its widget and did not hold it.
struct RevealAsked {
    target: *const dyn Widget,
    rect: vello::kurbo::Rect,
    missed: u8,
}

/// How many hosts may look for an asked widget without holding it before the ask is dropped: an
/// app hosted inside another is laid out after its host, which looks first, and a widget gone
/// before any host laid it out is never shown.
const REVEAL_MISSES: u8 = 32;

thread_local! {
    static REVEALS: std::cell::RefCell<Vec<RevealAsked>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Show `rect` of `target` (as laid out, in its window's coordinates): every view that scrolls
/// around it moves the least it takes, the nearest first. Focus moving to a control asks this
/// for the control; a widget asks it for a part of itself the person moved to (the row the
/// keyboard reached in a list that is not its own scroller). The host lays out again before
/// it draws, at the offsets this gave.
pub fn reveal(target: *const dyn Widget, rect: vello::kurbo::Rect) {
    REVEALS.with(|asked| {
        asked.borrow_mut().push(RevealAsked {
            target,
            rect,
            missed: 0,
        });
    });
}

/// Show what [`reveal`] asked for of what `root` holds; an ask for a widget it does not hold
/// stays for the host that holds it. Whether any view moved, so the host lays out again.
pub fn reveal_asked(root: &mut dyn Widget) -> bool {
    let asked = REVEALS.with(std::cell::RefCell::take);
    if asked.is_empty() {
        return false;
    }
    let mut shown = false;
    let mut kept = Vec::new();
    for mut ask in asked {
        if root.reveal_descendant(ask.target, ask.rect) {
            shown = true;
        } else if ask.missed < REVEAL_MISSES {
            ask.missed += 1;
            kept.push(ask);
        }
    }
    if !kept.is_empty() {
        REVEALS.with(|asked| {
            let mut asked = asked.borrow_mut();
            kept.append(&mut asked);
            *asked = kept;
        });
    }
    shown
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

/// One remote presence's live presence (caret/selection), published by a multi-
/// presence widget under that presence's identity.
#[derive(Clone, Debug, PartialEq)]
#[cfg(feature = "model")]
pub struct ViewPresence {
    pub presence: tilo_action_types::PresenceId,
    pub label: Option<String>,
    pub key: PresenceKey,
    pub payload: ::collaboration::PresencePayload,
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
pub unsafe fn node_sweep_identity(pointer: *const dyn Widget) -> (*const (), &'static str) {
    // SAFETY: the caller guarantees a live, unaliased-for-writes widget.
    let concrete = unsafe { (*pointer).concrete_type_name() };
    (pointer.cast::<()>(), concrete)
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
    fn draw_complete_content(&mut self, bounds: vello::kurbo::Rect, ctx: &mut DrawCtx<'_>) -> bool;
}

impl<T: Widget + ?Sized> CompleteContentWidgetExt for T {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
        capability::capability::<dyn CompleteContentProvider>(self)
            .and_then(CompleteContentProvider::complete_content_size)
    }

    fn draw_complete_content(&mut self, bounds: vello::kurbo::Rect, ctx: &mut DrawCtx<'_>) -> bool {
        capability::capability_mut::<dyn CompleteContentProvider>(self)
            .is_some_and(|provider| provider.draw_complete_content(bounds, ctx))
    }
}

/// Drain collaboration capabilities throughout a retained widget tree.
#[cfg(feature = "model")]
pub fn take_collab_requests(widget: &mut dyn Widget) -> Vec<::collaboration::CollabRequest> {
    let Ok(_entry) = crate::widget::bound_nodes::enter_widget(widget) else {
        return Vec::new();
    };
    let mut requests = capability::capability_mut::<dyn Collaborative>(&mut *widget)
        .map_or_else(Vec::new, Collaborative::take_collab_requests);
    for child in crate::widget::presence_topology::collaboration_children(widget) {
        requests.extend(take_collab_requests(child));
    }
    requests
}

/// Deliver one session event to each explicit collaboration capability once.
/// Bound callback context remains entered for the complete descendant walk.
#[cfg(feature = "model")]
pub fn handle_collab_event(widget: &mut dyn Widget, event: &::collaboration::CollabEvent) {
    let Ok(_entry) = crate::widget::bound_nodes::enter_widget(widget) else {
        return;
    };
    if let Some(collaborative) = capability::capability_mut::<dyn Collaborative>(&mut *widget) {
        collaborative.handle_collab_event(event);
    }
    for child in crate::widget::presence_topology::collaboration_children(widget) {
        handle_collab_event(child, event);
    }
}

/// Mount native views through each owner's retained lifecycle topology.
pub fn mount_native_views(
    widget: &mut dyn Widget,
    host: &crate::widget::native_view::NativeViewHost<'_>,
) {
    widget.mount_native_views(host);
}

/// Unmount native views through each owner's retained lifecycle topology.
pub fn unmount_native_views(widget: &mut dyn Widget) {
    widget.unmount_native_views();
}

/// Begin a frame through each owner's retained lifecycle topology.
pub fn native_view_frame_start(widget: &mut dyn Widget) {
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
    #[allow(
        clippy::unwrap_used,
        reason = "taffy only refuses a `NodeId` from another tree, which the toolkit never passes"
    )]
    fn leaf_node(&mut self, style: taffy::Style) -> NodeId {
        self.new_leaf(style).unwrap()
    }

    #[allow(
        clippy::unwrap_used,
        reason = "taffy only refuses a `NodeId` from another tree, which the toolkit never passes"
    )]
    fn parent_node(&mut self, style: taffy::Style, children: &[NodeId]) -> NodeId {
        self.new_with_children(style, children).unwrap()
    }
}

pub type Element = Box<dyn Widget>;

pub mod binding;
pub mod capability;
mod content_kind;
#[cfg(feature = "model")]
pub mod permission;
#[cfg(feature = "model")]
pub mod presence_topology;
pub mod resources;
pub use content_kind::ContentKind;
pub mod controls;
pub mod defaults;
pub mod document_effects;
pub mod identity;
pub use crate::pen::PenSample;
pub use {
    identity::Decorated, identity::DecoratedMut, identity::Decorator, identity::Identified,
    identity::register_node,
};

pub mod layout;
mod offered;
pub use offered::offered_room;
pub mod remount;
pub use remount::Remount;
mod reach;
pub use reach::find;
pub use reach::find_mut;
#[cfg(debug_assertions)]
mod paint_pass;
pub use layout::counts;
#[cfg(debug_assertions)]
pub use paint_pass::painting;
pub use {
    layout::AuthoredExtent, layout::DeviceScaleGuard, layout::ExtentSizing, layout::SkippedFrames,
    layout::WidgetLayoutError, layout::compute_widget_layout, layout::device_scale,
    layout::enter_device_scale, layout::insets, layout::insets_all, layout::insets_xy,
    layout::layout_clip, layout::placing_again, layout::solve_built_layout,
    layout::take_unsettled_layout, layout::take_unsettled_report, layout::text_control_height,
    layout::with_layout_clip,
};

mod cached_paint;
pub use cached_paint::CachedPaint;
pub mod composition_parts;
pub mod layout_info;
pub use {
    layout_info::Density, layout_info::Effective, layout_info::FieldLayout,
    layout_info::FormLayout, layout_info::HeaderRowLayout, layout_info::HeaderRows,
    layout_info::LabelPlacement, layout_info::LayoutInfo, layout_info::LayoutOrigin,
    layout_info::ListLayout, layout_info::ListPillLayout, layout_info::ManualConflict,
    layout_info::ManualLayout, layout_info::PageLayout, layout_info::RowLayout,
    layout_info::ScrollDirection, layout_info::ScrollLayout, layout_info::SectionLayout,
    layout_info::ShellLayout, layout_info::SidebarPosition, layout_info::TextLayout,
};
pub mod manual;
pub use {
    manual::ChildMeasurement, manual::Manual, manual::ManualContext, manual::ManualInvalidation,
    manual::ManualPolicy, manual::MeasureConstraint,
};
pub mod router;
pub use {router::RouteError, router::RouteHandle, router::RouteSnapshot, router::Router};

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
    let _entry = component_walk::enter(component_walk::binding_of(root).as_ref())?;
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
pub mod spacer;
pub use spacer::Spacer;
pub mod aim;
pub use aim::Trajectory;
pub mod hover_intent;
pub use hover_intent::EdgeAim;
pub mod cursor;
pub mod panic_scope;

pub use {
    tilo_image_codec::curve::CurvePoint, tilo_image_codec::curve::normalised as curve_points,
    tilo_image_codec::curve::value_at as curve_value_at,
};
pub mod native_view;
pub use {
    native_view::NativeView, native_view::NativeViewAccessibility, native_view::NativeViewBackend,
    native_view::NativeViewGeometry, native_view::NativeViewHost, native_view::NativeViewInput,
};
pub mod avatar_face;
pub use {
    avatar_face::AvatarFace, avatar_face::AvatarSize, avatar_face::PresenceStatus,
    avatar_face::initials_for_name,
};

/// A box a widget paints text into, whose own box cannot hold that text: the height the text
/// takes at the width of its box, the width the box gives it, and what it says. Rows a box lays
/// out and never paints read as a line that had nothing to say above space left empty, so
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

/// An action a widget offers as its own, beside the rows its owner offers where a context menu
/// opens over it ([`Widget::own_actions`]): a link's copy, a picture's save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnAction {
    /// What the row that runs it is called, in the person's language.
    pub name: String,
    /// Where [`Widget::perform_action`] runs it, relative to the widget.
    pub key_path: Vec<u64>,
}

impl OwnAction {
    /// The action called `name`, run at `key_path`.
    #[must_use]
    pub fn new(name: impl Into<String>, key_path: Vec<u64>) -> Self {
        Self {
            name: name.into(),
            key_path,
        }
    }
}

mod press;
pub use press::Press;

mod tone;
pub use tone::Tone;

#[cfg(feature = "model")]
pub use collab_types::ArtifactKind;
pub use tilo_action_types::HistoryEntryId;
