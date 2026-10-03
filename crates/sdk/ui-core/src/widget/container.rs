use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::any::Any;
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

/// Each bool below is an independent builder toggle (background, height
/// fill, padding, grow behaviour) set at construction time, not a state
/// machine with meaningful combinations to enumerate — a bitflags/enum
/// refactor would ripple across every one of `Container`'s call sites in
/// the crate for no behavioural gain.
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent, orthogonal builder toggles; not encoded state"
)]
pub struct Container {
    child: Option<Box<dyn Widget>>,
    /// Stable graph / test identity when built from a serialized UI node.
    id: Option<String>,
    rect: Rect,
    has_background: bool,
    focus_trap: bool,
    fill_height: bool,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    no_padding: bool,
    /// Symmetric inset override. `None` follows the theme's standard
    /// container inset; `with_no_padding` remains the explicit zero case.
    padding_override: Option<f32>,
    /// `Some(px)` switches the taffy width to `length(px)` and disables
    /// horizontal shrink, instead of the default `percent(1.0)` fill.
    fixed_width: Option<f32>,
    /// Caps a normally fluid container and uses automatic horizontal margins
    /// to keep it centred in a wider parent. Unlike [`Container::with_width`]
    /// this remains responsive below the cap.
    max_width: Option<f32>,
    /// Lower horizontal bound used by responsive flex children. Unlike
    /// `fixed_width`, this still lets the container grow to share the row.
    min_width: Option<f32>,
    /// Mutually independent from [`Container::fill_height`].
    fixed_height: Option<f32>,
    /// Pressing the container activates it; it lifts under the pointer.
    activation: crate::widget::activation::Activation,
    /// Grows from a zero basis instead of claiming `percent(1.0)`. In a
    /// wrapping flex row this is the difference between filling the
    /// leftover width AFTER siblings are placed (correct) and demanding
    /// the whole line up front, forcing every sibling onto the next line
    /// even when there was room.
    flex_grow: bool,
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Container {
    pub fn new() -> Self {
        Self {
            child: None,
            id: None,
            rect: Rect::ZERO,
            has_background: false,
            focus_trap: false,
            fill_height: false,
            presence_path: Vec::new(),
            permission_scope: None,
            no_padding: false,
            padding_override: None,
            fixed_width: None,
            min_width: None,
            max_width: None,
            fixed_height: None,
            activation: crate::widget::activation::Activation::default(),
            flex_grow: false,
        }
    }

    /// Confine host keyboard traversal to this visible overlay subtree.
    /// Ordinary containers leave the host focus order unchanged.
    #[must_use]
    pub fn with_focus_trap(mut self) -> Self {
        self.focus_trap = true;
        self
    }

    /// See the field docs.
    #[must_use]
    pub fn with_flex_grow(mut self) -> Self {
        self.flex_grow = true;
        self
    }

    /// Used by the shell root so its child Stack reaches y=0 / x=0 without
    /// an uneditable inner gutter.
    #[must_use]
    pub fn with_no_padding(mut self) -> Self {
        self.no_padding = true;
        self
    }

    /// Override the themed default with a deliberate symmetric inset.
    /// Useful for compact raised tracks and tool surfaces whose contents
    /// already provide full-size interaction targets.
    #[must_use]
    pub fn with_padding(mut self, px: f32) -> Self {
        self.no_padding = false;
        self.padding_override = Some(px.max(0.0));
        self
    }

    #[must_use]
    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
    }

    /// Address this container by a stable id in inspection and graph reconcile.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Take the single child for identity-preserving reconcile.
    pub fn take_child(&mut self) -> Option<Box<dyn Widget>> {
        self.child.take()
    }

    /// Replace the single child after a graph reconcile pass.
    pub fn set_child(&mut self, child: Box<dyn Widget>) {
        self.child = Some(child);
    }

    /// Apply layout flags from a serialized container node without rebuilding.
    pub fn apply_graph_props(
        &mut self,
        id: &str,
        background: bool,
        no_padding: bool,
        fill_height: bool,
        width: Option<f32>,
        height: Option<f32>,
    ) {
        self.id = Some(id.to_owned());
        self.has_background = background;
        self.no_padding = no_padding;
        self.fill_height = fill_height;
        self.fixed_width = width;
        self.fixed_height = height;
    }

    #[must_use]
    pub fn with_background(mut self) -> Self {
        self.has_background = true;
        self
    }

    /// Disables the default `percent(1.0)` horizontal fill and stops the
    /// container from shrinking below `px`.
    #[must_use]
    pub fn with_width(mut self, px: f32) -> Self {
        self.fixed_width = Some(px);
        self
    }

    /// Keeps the container fluid in narrow spaces, while centring it at no
    /// more than `px` in a wider parent. This is useful for readable forms
    /// and sparse settings panes without constraining data-dense layouts.
    #[must_use]
    pub fn with_centered_max_width(mut self, px: f32) -> Self {
        self.max_width = Some(px);
        self
    }

    /// Keep this container usable when it participates in a wrapping row,
    /// while still allowing it to grow and share spare horizontal space.
    #[must_use]
    pub fn with_min_width(mut self, px: f32) -> Self {
        self.min_width = Some(px);
        self
    }

    /// Keep readable content from becoming an excessively wide wall while
    /// still shrinking naturally on compact windows.
    #[must_use]
    pub fn with_max_width(mut self, px: f32) -> Self {
        self.max_width = Some(px);
        self
    }

    /// Independent of [`Container::filling`].
    #[must_use]
    pub fn with_height(mut self, px: f32) -> Self {
        self.fixed_height = Some(px);
        self
    }

    #[must_use]
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.fixed_width = Some(width);
        self.fixed_height = Some(height);
        self
    }

    /// Make the whole container one pressable target (a tile that opens
    /// something): it lifts under the pointer and when pressed, takes
    /// keyboard focus with a ring, and runs `f` on a click, Enter, Space or
    /// an assistive-technology activation. Controls inside keep their own
    /// clicks. `id` names it for keyboard, tests and assistive technology.
    #[must_use]
    pub fn interactive(mut self, id: impl Into<String>, f: impl FnMut() + 'static) -> Self {
        self.activation = crate::widget::activation::Activation::new(id);
        self.activation.set_callback(Box::new(f));
        self
    }

    /// [`Self::interactive`] through an exported resource action.
    #[must_use]
    pub fn interactive_action(
        mut self,
        id: impl Into<String>,
        binding: tilo_actions::ActionBinding<()>,
    ) -> Self {
        self.activation = crate::widget::activation::Activation::new(id);
        self.activation.set_action(binding);
        self
    }

    /// The container's surface under its current interaction: the raised
    /// well when it has a background (lifted on hover and press), the
    /// ground's selection lift when it is bare and interactive.
    fn surface_fill(&self, theme: &Theme) -> Option<vello::peniko::Color> {
        let colors = &theme.tokens.colors;
        let state = self.activation.state;
        let live = self.activation.is_bound() && !state.disabled;
        if self.has_background {
            let raised = colors.surfaces.raised;
            Some(if live && state.pressed {
                colors.lift_on(raised, 1.0)
            } else if live && state.hovered {
                colors.lift_on(raised, 0.5)
            } else {
                raised
            })
        } else if live && state.pressed {
            Some(colors.intent.selection)
        } else if live && state.hovered {
            Some(colors.intent.selection_hover)
        } else {
            None
        }
    }

    fn paint_surface(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        match self.surface_fill(theme) {
            // Rest edge derived from the fill itself (HIG §9.5), crisp 1 px
            // inset so the stroke sits on the surface.
            Some(fill) if self.has_background => {
                crate::surface::card(scene, transform, self.rect, theme.radius_standard, fill);
            }
            Some(fill) => scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &vello::kurbo::RoundedRect::from_rect(self.rect, theme.radius_standard),
            ),
            None => {}
        }
        if self.activation.state.focused && self.activation.is_bound() {
            crate::widget::paint_focus_ring(
                scene,
                transform,
                self.rect,
                theme.radius_standard,
                theme,
            );
        }
    }

    /// The default (auto height) is correct for card-style containers;
    /// root shells want `filling()`.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill_height = true;
        self
    }

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Container {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::undo::UndoPoints
    );

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        // This container owns no operation handler: offer each child once.
        crate::widget::start_in_children(self, op, doc, path)
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        match &mut self.child {
            Some(child) => child.update(state, path),
            None => false,
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child_node = self
            .child
            .as_mut()
            .map(|c| c.build_layout(taffy, theme, node_map));

        let pad = if self.no_padding {
            0.0
        } else {
            self.padding_override.unwrap_or(theme.padding_standard)
        };
        let width = match self.fixed_width {
            Some(px) => length(px),
            // `auto` so the basis is zero (the wrap decision ignores it);
            // flex_grow below fills the leftover line width.
            None if self.flex_grow => taffy::Dimension::auto(),
            None => percent(1.0_f32),
        };
        let height = match self.fixed_height {
            Some(px) => length(px),
            None if self.fill_height => percent(1.0_f32),
            None => taffy::Dimension::auto(),
        };
        let style = Style {
            display: taffy::Display::Flex,
            flex_shrink: 0.0, // Prevent vertical squashing
            // A pinned width must not grow to fill a flex parent: `flex_basis:
            // auto` lets `size.width` resolve the basis instead.
            flex_grow: if self.flex_grow { 1.0 } else { 0.0 },
            flex_basis: if self.flex_grow {
                length(0.0_f32)
            } else {
                taffy::Dimension::auto()
            },
            padding: super::layout::insets_all(pad),
            size: taffy::Size { width, height },
            min_size: taffy::Size {
                width: self.min_width.map_or(taffy::Dimension::auto(), length),
                height: taffy::Dimension::auto(),
            },
            max_size: taffy::Size {
                width: self.max_width.map_or(taffy::Dimension::auto(), length),
                height: taffy::Dimension::auto(),
            },
            margin: taffy::Rect {
                left: if self.max_width.is_some() {
                    auto()
                } else {
                    length(0.0_f32)
                },
                right: if self.max_width.is_some() {
                    auto()
                } else {
                    length(0.0_f32)
                },
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            ..Default::default()
        };

        let node = if let Some(child) = child_node {
            taffy.parent_node(style, &[child])
        } else {
            taffy.leaf_node(style)
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );

        if let Some(child) = &mut self.child
            && let Some(&child_node) = taffy.children(node).unwrap_or_default().first()
        {
            child.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.paint_surface(scene, transform, theme);

        if let Some(child) = &mut self.child {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(child) = &mut self.child
            && let Some(&child_node) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first()
        {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
        }
        if self.activation.is_bound() {
            if *ctx.handled {
                // A control inside took the press; the container stays put.
                self.activation.state.pressed = false;
                return;
            }
            self.activation.handle_pointer(event, ctx, self.rect, &[]);
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.activation.is_bound() && self.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.activation.is_bound()
    }

    fn set_focus(&mut self, focused: bool) {
        self.activation.state.focused = focused && self.activation.is_bound();
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _root: &ModelPath,
    ) -> bool {
        matches!(action, crate::a11y::Action::Activate) && self.activation.activate()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        self.activation.is_bound().then(|| {
            AccessNode::new(Role::Button)
                .with_semantic_key(self.activation.id())
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.activation.state.focused),
                )
        })
    }

    /// So an outer container's event-routing short-circuit honours an
    /// active drag deeper in the tree.
    fn wants_pointer_capture(&self) -> bool {
        self.child
            .as_ref()
            .is_some_and(|c| c.wants_pointer_capture())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(child) = &self.child {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn mark_drawn(&mut self) {
        if let Some(child) = &mut self.child {
            child.mark_drawn();
        }
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.focus_trap
    }

    fn name(&self) -> &'static str {
        "Container"
    }

    fn test_id(&self) -> Option<&str> {
        self.id
            .as_deref()
            .or_else(|| self.activation.is_bound().then(|| self.activation.id()))
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn is_focused(&self) -> bool {
        self.activation.state.focused
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty())
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.paint_surface(scene, transform, theme);

        if let Some(child) = &mut self.child {
            child.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }

        if let Some(scope) = crate::permissions::capability::permission_scope(self)
            && !permissions.check(&scope)
        {
            let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                overlay,
                None,
                &self.rect,
            );
        }

        if let Some(key) = crate::presence::capability::presence_key(self)
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }
}

impl crate::undo::UndoPoints for Container {
    fn needs_commit(&self) -> bool {
        self.children()
            .iter()
            .any(|c| crate::undo::needs_commit(*c))
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            crate::undo::on_commit(child);
        }
    }
}

crate::widget::activation::activation_controls!(Container, activation);

impl crate::presence::capability::PublishesPresence for Container {
    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }
}

impl crate::permissions::capability::PermissionGated for Container {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 800×600 viewport. Drives the real five-pass path so the taffy width
    /// pin is exercised end-to-end.
    fn lay_out(mut c: Container) -> (Container, Rect) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = c.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        c.apply_layout(&taffy, node, Point::ZERO);
        let r = c.get_rect();
        (c, r)
    }

    #[test]
    fn container_sizing_padding_centering_and_fill() {
        fn with_width_pins_outer_width() {
            let (_c, rect) = lay_out(Container::new().with_width(280.0));
            assert!(
                (rect.width() - 280.0).abs() < 0.5,
                "with_width(280) must pin the container to 280 px, got {}",
                rect.width()
            );
            assert!(
                rect.width() < 799.0,
                "a pinned-width container must NOT fill the 800 px viewport"
            );
        }

        fn with_size_pins_both_dimensions() {
            let (_c, rect) = lay_out(Container::new().with_size(120.0, 64.0));
            assert!((rect.width() - 120.0).abs() < 0.5, "width pinned to 120");
            assert!((rect.height() - 64.0).abs() < 0.5, "height pinned to 64");
        }

        fn default_container_still_fills_width() {
            let (_c, rect) = lay_out(Container::new());
            assert!(
                (rect.width() - 800.0).abs() < 0.5,
                "default container must fill the viewport width, got {}",
                rect.width()
            );
        }

        fn padding_override_insets_child_without_losing_fluid_width() {
            let (container, rect) = lay_out(
                Container::new()
                    .with_padding(8.0)
                    .with_child(Box::new(Container::new().with_size(10.0, 10.0))),
            );
            let child = container.children()[0].get_rect();
            assert!((rect.width() - 800.0).abs() < 0.5);
            assert!((child.x0 - 8.0).abs() < 0.5);
            assert!((child.y0 - 8.0).abs() < 0.5);
            assert!((rect.y1 - child.y1 - 8.0).abs() < 0.5);
        }

        fn centered_max_width_caps_wide_parent_and_remains_fluid() {
            let (wide, _) = lay_out(
                Container::new()
                    .with_child(Box::new(Container::new().with_centered_max_width(480.0))),
            );
            let wide_child = wide.children()[0].get_rect();
            assert!((wide_child.width() - 480.0).abs() < 0.5);
            assert!(
                (wide_child.x0 - 160.0).abs() < 0.5,
                "capped child must be centred, got {wide_child:?}"
            );

            let (_narrow, narrow_rect) = lay_out(Container::new().with_centered_max_width(960.0));
            assert!(
                (narrow_rect.width() - 800.0).abs() < 0.5,
                "cap above available width must remain fluid"
            );
        }

        fn interactive_containers_lift_and_activate() {
            let theme = Theme::dark();
            let hits = std::rc::Rc::new(std::cell::Cell::new(0));
            let counter = hits.clone();
            let mut c = Container::new()
                .with_background()
                .interactive("open-channel", move || counter.set(counter.get() + 1));
            let rest = c.surface_fill(&theme);
            assert_eq!(rest, Some(theme.tokens.colors.surfaces.raised));
            c.activation.state.hovered = true;
            assert_ne!(c.surface_fill(&theme), rest, "hover lifts");
            c.activation.state.pressed = true;
            let pressed = c.surface_fill(&theme);
            c.activation.state.pressed = false;
            assert_ne!(pressed, c.surface_fill(&theme), "press lifts further");
            let bare = Container::new();
            assert_eq!(
                bare.surface_fill(&theme),
                None,
                "a plain container paints nothing"
            );
            assert!(c.is_focusable());
            let mut doc = automerge::AutoCommit::new();
            assert!(Widget::perform_action(
                &mut c,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert_eq!(hits.get(), 1);
            let node = Widget::accessibility(&c).unwrap();
            assert_eq!(node.role, crate::a11y::Role::Button);
        }

        with_width_pins_outer_width();
        with_size_pins_both_dimensions();
        default_container_still_fills_width();
        padding_override_insets_child_without_losing_fluid_width();
        centered_max_width_caps_wide_parent_and_remains_fluid();
        interactive_containers_lift_and_activate();
    }
}
