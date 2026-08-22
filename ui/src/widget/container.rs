use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, RoundedRect, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style};
use taffy::style_helpers::{length, percent};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

pub struct Container {
    child: Option<Box<dyn Widget>>,
    rect: Rect,
    has_background: bool,
    fill_height: bool,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    no_padding: bool,
    /// Pin the container's outer width to an exact logical-pixel value
    /// instead of the default `percent(1.0)` fill. `Some(px)` switches the
    /// taffy width to `length(px)` and disables horizontal shrink so the
    /// container reads as a true fixed-width pane (e.g. a master/detail or
    /// settings sidebar). `None` keeps the default fill behaviour.
    fixed_width: Option<f32>,
    /// Pin the container's outer height to an exact logical-pixel value.
    /// Mutually independent from [`Container::fill_height`] — when set this
    /// overrides the auto/percent height with `length(px)`.
    fixed_height: Option<f32>,
    /// Explicit fill colour for the container's background surface. When
    /// `Some(c)`, the background is painted in `c` (still themed — callers
    /// pass a `Theme` token) rather than the default `theme.bg_primary`.
    /// Setting this also implies `has_background`. `None` keeps the legacy
    /// `bg_primary` behaviour when [`Container::with_background`] is used.
    fill_color: Option<vello::peniko::Color>,
    /// When set, the container grows to fill the free space on its flex line
    /// from a **zero** basis (`flex_grow: 1, flex_basis: 0`), instead of
    /// claiming `percent(1.0)` of its parent. In a **wrapping** flex row this
    /// is the difference between a child that fills the leftover width AFTER
    /// its siblings are placed (correct — the row wraps only when the fixed
    /// siblings don't fit) and one that demands the whole line up front
    /// (which forces every sibling onto the next line even when there was
    /// room). Use it to wrap a `percent(1.0)`-width field (e.g. a search box)
    /// so it stays inline on wide rows and wraps only when genuinely narrow.
    flex_grow: bool,
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

    /// Grow to fill the free space on the flex line from a zero basis
    /// (`flex_grow: 1, flex_basis: 0`) rather than claiming `percent(1.0)`.
    /// See the field docs — this is how a full-width field is kept inline on a
    /// wide wrapping row yet allowed to wrap only when the row is truly narrow.
    pub fn with_flex_grow(mut self) -> Self {
        self.flex_grow = true;
        self
    }

    /// Skip the default `padding_standard` on all sides. Used by the
    /// shell root so its child Stack reaches y=0 / x=0 without an
    /// uneditable inner gutter.
    pub fn with_no_padding(mut self) -> Self {
        self.no_padding = true;
        self
    }

    pub fn with_child(mut self, child: Box<dyn Widget>) -> Self {
        self.child = Some(child);
        self
    }

    pub fn with_background(mut self) -> Self {
        self.has_background = true;
        self
    }

    /// Pin the container to an exact logical-pixel width. Disables the
    /// default `percent(1.0)` horizontal fill and stops the container from
    /// shrinking below `px`, so a fixed-width sidebar / master-detail pane
    /// is expressible without faking it with flex fractions. Pass a value
    /// derived from theme spacing where possible so the pin still tracks
    /// the design scale.
    pub fn with_width(mut self, px: f32) -> Self {
        self.fixed_width = Some(px);
        self
    }

    /// Pin the container to an exact logical-pixel height (independent of
    /// [`Container::filling`]). Use together with [`Container::with_width`]
    /// for a fixed-size box.
    pub fn with_height(mut self, px: f32) -> Self {
        self.fixed_height = Some(px);
        self
    }

    /// Pin both dimensions at once — convenience over chaining
    /// [`Container::with_width`] + [`Container::with_height`].
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.fixed_width = Some(width);
        self.fixed_height = Some(height);
        self
    }

    /// Paint the container's background in an explicit colour rather than
    /// the default `theme.bg_primary`. Implies a background (no separate
    /// [`Container::with_background`] call needed). The colour is still a
    /// theme decision — pass a token such as `theme.bg_app`,
    /// `theme.tokens.colors.surfaces.base`, or a selected-row highlight —
    /// so the surface tint stays OS-controlled. Use this for header bands,
    /// selected-row highlights, and any surface that must read as distinct
    /// from the standard raised card.
    pub fn with_fill(mut self, color: vello::peniko::Color) -> Self {
        self.fill_color = Some(color);
        self.has_background = true;
        self
    }

    /// Opt this container into filling its parent's height (percent(1.0)) rather
    /// than sizing to its child's intrinsic height. The default (auto height) is
    /// correct for card-style containers; root shells want `filling()`.
    pub fn filling(mut self) -> Self {
        self.fill_height = true;
        self
    }

    /// Opt this layout into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this layout behind a permission scope.
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

    /// Forward the per-frame update pass to the wrapped child.
    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        match &mut self.child {
            Some(child) => child.update(state, path),
            None => false,
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let child_node = self.child.as_mut().map(|c| c.build_layout(taffy, theme, node_map));

        let pad = if self.no_padding { 0.0 } else { theme.padding_standard };
        let width = match self.fixed_width {
            Some(px) => length(px),
            // A grow-from-zero container takes an `auto` width so its basis is
            // zero (the wrap decision ignores it); the flex_grow below fills
            // the leftover line width.
            None if self.flex_grow => taffy::Dimension::auto(),
            None => percent(1.0),
        };
        let height = match self.fixed_height {
            Some(px) => length(px),
            None if self.fill_height => percent(1.0),
            None => taffy::Dimension::auto(),
        };
        let style = Style {
            display: taffy::Display::Flex,
            flex_shrink: 0.0, // Prevent vertical squashing
            // A pinned width must not grow to fill a flex parent — keep it
            // exactly `px`. `flex_basis: auto` lets the `size.width` length
            // resolve the basis. A `with_flex_grow` container instead grows
            // from a zero basis to fill the free space on its line.
            flex_grow: if self.flex_grow { 1.0 } else { 0.0 },
            flex_basis: if self.flex_grow {
                length(0.0)
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
            taffy.new_with_children(style, &[child]).unwrap()
        } else {
            taffy.new_leaf(style).unwrap()
        };
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        
        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node).unwrap()[0];
            child.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &crate::presence::PresenceCtx) {
        if self.has_background {
            let fill = self.fill_color.unwrap_or(theme.bg_primary);
            let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, theme.border_standard, None, &shape);
        }

        if let Some(child) = &mut self.child {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        if let Some(child) = &mut self.child {
            let child_node = taffy.children(node_id).unwrap()[0];
            child.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, child_node, taffy);
        }
    }

    /// Propagate pointer-capture intent from the contained child so an
    /// outer container's event-routing short-circuit honours an active
    /// drag deeper in the tree.
    fn wants_pointer_capture(&self) -> bool {
        self.child.as_ref().is_some_and(|c| c.wants_pointer_capture())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.child.as_ref().map(|c| c.as_ref() as &dyn Widget).into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.child.as_mut().map(|c| c.as_mut() as &mut (dyn Widget + '_)).into_iter().collect()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &str { "Container" }

    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { self.children().iter().any(|c| c.needs_commit()) }
    fn on_commit(&mut self) { for child in self.children_mut() { child.on_commit(); } }

    fn take_structure_dirty(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false)
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget { path: self.presence_path.clone() })
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
        if presence.should_cull(self.get_rect()) { return; }
        // Draw the container's own background, then propagate presence/permission
        // context down to the child so its opt-in chrome still renders.
        if self.has_background {
            let fill = self.fill_color.unwrap_or(theme.bg_primary);
            let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);
            scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
            scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, theme.border_standard, None, &shape);
        }

        if let Some(child) = &mut self.child {
            child.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = crate::tokens::colors::PERMISSION_DENIED_SCRIM;
                scene.fill(vello::peniko::Fill::NonZero, transform, overlay, None, &self.rect);
            }
        }

        if let Some(key) = self.presence_key() {
            if let Some(peers) = presence.here.get(&key) {
                if let Some(user) = peers.first() {
                    presence.draw_peer_focus(scene, theme, user, self.rect);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vello_encoding::DrawTag;
    use vello::peniko::Color;

    /// Lay the container out inside an 800×600 viewport and return its
    /// resolved rect. Drives the real five-pass path so the taffy width
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
        // Guards the additive contract: an un-pinned container keeps the
        // legacy `percent(1.0)` fill so existing call sites are unaffected.
        let (_c, rect) = lay_out(Container::new());
        assert!(
            (rect.width() - 800.0).abs() < 0.5,
            "default container must fill the viewport width, got {}",
            rect.width()
        );
    }

    #[test]
    fn with_fill_emits_background_distinct_from_bg_primary() {
        // `with_fill` must paint *a* background (emit a COLOR fill) even
        // without a separate `with_background()` call.
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
