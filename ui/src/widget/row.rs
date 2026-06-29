use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style};
use taffy::style_helpers::{length, percent};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

pub struct Row {
    children: Vec<Box<dyn Widget>>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// When set, the row keeps all children on one line (no flex wrap) —
    /// the right choice for a toolbar of fixed-size buttons.
    no_wrap: bool,
    /// When set, the row grows to fill the remaining main-axis space of
    /// its parent (flex_grow:1) and stretches its children to its height —
    /// the right choice for a sidebar|content body row.
    fill_height: bool,
    /// Subtree cache — same pattern as `Column::subtree_cache`.
    /// See that field for the gory rationale.
    subtree_cache: Scene,
    cache_rect: vello::kurbo::Rect,
}

impl Row {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            no_wrap: false,
            fill_height: false,
            subtree_cache: Scene::new(),
            cache_rect: vello::kurbo::Rect::ZERO,
        }
    }

    pub fn push(mut self, child: Box<dyn Widget>) -> Self {
        self.children.push(child);
        self
    }

    /// Keep all children on a single line (disable the default flex-wrap).
    pub fn no_wrap(mut self) -> Self {
        self.no_wrap = true;
        self
    }

    /// Grow to fill the remaining vertical space of a parent column and
    /// stretch children to the row's full height.
    pub fn with_fill_remaining_height(mut self) -> Self {
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

impl Widget for Row {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        for child in &mut self.children {
            child.sync(state, path);
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let child_nodes: Vec<NodeId> = self.children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
            
        let node = taffy.new_with_children(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                flex_wrap: if self.no_wrap { taffy::FlexWrap::NoWrap } else { taffy::FlexWrap::Wrap },
                // Stretch children to the row height when it is a fill-body
                // row (so the file list spans the available height);
                // otherwise centre them (toolbar/button rows).
                align_items: Some(if self.fill_height {
                    taffy::AlignItems::Stretch
                } else {
                    taffy::AlignItems::Center
                }),
                flex_grow: if self.fill_height { 1.0 } else { 0.0 },
                flex_shrink: 0.0, // Prevent vertical squashing
                gap: taffy::Size {
                    width: length(theme.gap_standard),
                    height: length(theme.gap_standard),
                },
                size: taffy::Size {
                    width: percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &child_nodes
        ).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        // See `Column::apply_layout` for why we max() against
        // content_size — covers taffy under-reporting under a
        // constrained-height parent.
        let w = layout.size.width.max(layout.content_size.width) as f64;
        let h = layout.size.height.max(layout.content_size.height) as f64;
        self.rect = vello::kurbo::Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + w,
            origin.y + layout.location.y as f64 + h,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);

        let children = taffy.children(node).unwrap();
        for (child_widget, child_node) in self.children.iter_mut().zip(children.iter()) {
            child_widget.apply_layout(taffy, *child_node, new_origin);
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &crate::presence::PresenceCtx) {
        for child in &mut self.children {
            child.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>) {
        let child_nodes = taffy.children(node_id).unwrap();
        // Same pointer-capture short-circuit as `Column::handle_event` —
        // a dragging child gets exclusive event routing so the drag
        // survives the cursor wandering through peer widgets.
        if let Some(idx) = self.children.iter().position(|c| c.wants_pointer_capture()) {
            let cnode = child_nodes[idx];
            self.children[idx].handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, cnode, taffy);
            return;
        }
        for (child_widget, &child_node) in self.children.iter_mut().zip(child_nodes.iter()) {
            child_widget.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, child_node, taffy);
            if *handled {
                break;
            }
        }
    }

    /// Propagate pointer-capture intent from any descendant — mirrors
    /// `Column::wants_pointer_capture`. See that comment for why.
    fn wants_pointer_capture(&self) -> bool {
        self.children.iter().any(|c| c.wants_pointer_capture())
    }

    /// Direct-field recurse — the default impl allocates a
    /// `Vec` via `children()`, which the subtree-cache check
    /// hits per frame.
    fn needs_redraw(&self) -> bool {
        self.cache_rect == vello::kurbo::Rect::ZERO
            || self.children.iter().any(|c| c.needs_redraw())
    }

    fn is_layout_dirty(&self) -> bool {
        self.children.iter().any(|c| c.is_layout_dirty())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children.iter().map(|c| c.as_ref()).collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children.iter_mut().map(|c| c.as_mut() as &mut (dyn Widget + '_)).collect()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn name(&self) -> &str { "Row" }

    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { self.children().iter().any(|c| c.needs_commit()) }
    fn on_commit(&mut self) { for child in self.children_mut() { child.on_commit(); } }

    fn take_structure_dirty(&mut self) -> bool {
        self.children
            .iter_mut()
            .map(|c| c.take_structure_dirty())
            .fold(false, |acc, d| acc || d)
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
        // Subtree cache — see `Column::draw_with_presence` for the
        // `Affine::IDENTITY` rebuild requirement.
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        let any_child_dirty = self.children.iter().any(|c| c.needs_redraw());
        let needs_rebuild = self.cache_rect == vello::kurbo::Rect::ZERO
            || size_changed
            || any_child_dirty;
        if !needs_rebuild {
            let dx = self.rect.x0 - self.cache_rect.x0;
            let dy = self.rect.y0 - self.cache_rect.y0;
            scene.append(&self.subtree_cache, Some(transform * Affine::translate((dx, dy))));
        } else {
            // Drop the active viewport for the recursion — see
            // `Column::draw_with_presence` for why.
            let prev_vp = presence.viewport.replace(None);
            let mut local = std::mem::take(&mut self.subtree_cache);
            local.reset();
            for child in &mut self.children {
                child.draw_with_presence(&mut local, Affine::IDENTITY, text_renderer, theme, presence, permissions);
            }
            scene.append(&local, Some(transform));
            self.subtree_cache = local;
            self.cache_rect = self.rect;
            presence.viewport.set(prev_vp);
        }

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
