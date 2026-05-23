use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

pub struct Spacer {
    rect: Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
}

impl Spacer {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
        }
    }

    /// Opt this spacer into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this spacer behind a permission scope.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Spacer {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        }).unwrap();
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
    }

    fn draw(&mut self, _scene: &mut Scene, _transform: Affine, _text_renderer: &mut TextRenderer, _theme: &Theme, _presence: &crate::presence::PresenceCtx) {}

    fn handle_event(&mut self, _event: &WindowEvent, _scale_factor: f64, _modifiers: &winit::event::Modifiers, _state: &mut AutoCommit, _path: &ObjId, _handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {}

    fn name(&self) -> &str { "Spacer" }

    /// Spacers never paint anything — `draw` is a no-op. They're
    /// also fixed-size relative to their parent's flex spec. Both
    /// of these reduce to "never dirty, never needs layout reflow,
    /// never blocks the App redraw gate". The cost saving here is
    /// per-frame: with many Spacers in a tree (toolbars, padding
    /// rows), the default-true `needs_redraw` was pinning the
    /// whole tree to "always-dirty".
    fn needs_redraw(&self) -> bool { false }
    /// Spacer has zero intrinsic primitives but its `flex_grow: 1.0`
    /// means a parent reflow legitimately moves it. Rely on the
    /// default container-recursion at the parent level — Spacer
    /// itself reports false after the first apply_layout.
    fn is_layout_dirty(&self) -> bool { self.rect == Rect::ZERO }

    fn get_rect(&self) -> Rect {
        self.rect
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
        self.draw(scene, transform, text_renderer, theme, presence);

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = vello::peniko::Color::from_rgba8(0, 0, 0, 77);
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
