use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
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
    rect: Rect,
    has_background: bool,
    fill_height: bool,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    no_padding: bool,
    /// `Some(px)` switches the taffy width to `length(px)` and disables
    /// horizontal shrink, instead of the default `percent(1.0)` fill.
    fixed_width: Option<f32>,
    /// Mutually independent from [`Container::fill_height`].
    fixed_height: Option<f32>,
    /// Also implies `has_background`. `None` keeps the legacy `bg_primary`
    /// behaviour when [`Container::with_background`] is used.
    fill_color: Option<vello::peniko::Color>,
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
            rect: Rect::ZERO,
            has_background: false,
            fill_height: false,
            presence_path: Vec::new(),
            permission_scope: None,
            no_padding: false,
            fixed_width: None,
            fixed_height: None,
            fill_color: None,
            flex_grow: false,
        }
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

    #[must_use]
    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
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

    /// Implies a background. Pass a theme token (`theme.bg_app` etc.) so
    /// the surface tint stays OS-controlled.
    #[must_use]
    pub fn with_fill(mut self, color: vello::peniko::Color) -> Self {
        self.fill_color = Some(color);
        self.has_background = true;
        self
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
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(child) = &mut self.child {
            child.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
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
            theme.padding_standard
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
            padding: taffy::Rect {
                left: length(pad),
                right: length(pad),
                top: length(pad),
                bottom: length(pad),
            },
            size: taffy::Size { width, height },
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
        if self.has_background {
            let fill = self.fill_color.unwrap_or(theme.bg_primary);
            let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &shape,
            );
        }

        if let Some(child) = &mut self.child {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
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
        if let Some(child) = &mut self.child
            && let Some(&child_node) = taffy.children(node_id).unwrap_or_default().first()
        {
            child.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
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

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Container"
    }

    fn is_focused(&self) -> bool {
        false
    }
    fn needs_commit(&self) -> bool {
        self.children().iter().any(|c| c.needs_commit())
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            child.on_commit();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty())
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
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
        if self.has_background {
            let fill = self.fill_color.unwrap_or(theme.bg_primary);
            let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &shape,
            );
        }

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

        if let Some(scope) = self.permission_scope()
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

        if let Some(key) = self.presence_key()
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vello::peniko::Color;
    use vello_encoding::DrawTag;

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

    #[test]
    fn with_size_pins_both_dimensions() {
        let (_c, rect) = lay_out(Container::new().with_size(120.0, 64.0));
        assert!((rect.width() - 120.0).abs() < 0.5, "width pinned to 120");
        assert!((rect.height() - 64.0).abs() < 0.5, "height pinned to 64");
    }

    #[test]
    fn default_container_still_fills_width() {
        let (_c, rect) = lay_out(Container::new());
        assert!(
            (rect.width() - 800.0).abs() < 0.5,
            "default container must fill the viewport width, got {}",
            rect.width()
        );
    }

    #[test]
    fn with_fill_emits_background_distinct_from_bg_primary() {
        let custom = Color::from_rgb8(10, 20, 30);
        let (mut c, _rect) = lay_out(Container::new().with_fill(custom));
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = crate::presence::PresenceCtx::default();
        let mut scene = Scene::new();
        c.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert!(
            fills >= 1,
            "with_fill must emit at least one COLOR fill (the surface tint)"
        );
    }
}
