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

pub struct Column {
    children: Vec<Box<dyn Widget>>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    extra_left_padding: f32,
    no_padding: bool,
    /// When true, the column's height resolves to its content's
    /// intrinsic sum (`Dimension::auto`) instead of filling the parent
    /// (`Dimension::percent(1.0)`). Required when the column is the
    /// child of a vertical `Scrollable` — without it, the column's
    /// height is clamped to the viewport, `content_h == viewport_h`,
    /// and the scrollable has nothing to scroll.
    intrinsic_height: bool,
}

impl Column {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            extra_left_padding: 0.0,
            no_padding: false,
            intrinsic_height: false,
        }
    }

    /// Resolve the column's height from its content (`auto`) instead of
    /// filling the parent (`percent(1.0)`). Required when wrapping the
    /// column in a `Scrollable` — see the field doc on
    /// [`Column::intrinsic_height`] for the gory details.
    pub fn with_intrinsic_height(mut self) -> Self {
        self.intrinsic_height = true;
        self
    }

    /// Skip the default `padding_standard * 2` on all sides. Used by
    /// the shell's foreground column so the topbar lands at y=0
    /// instead of inset by the column's gutter.
    pub fn with_no_padding(mut self) -> Self {
        self.no_padding = true;
        self
    }

    pub fn push(mut self, child: Box<dyn Widget>) -> Self {
        self.children.push(child);
        self
    }

    /// Add extra padding on the left edge on top of the column's
    /// default `padding_standard * 2`. Used to displace a workspace
    /// area to the right of an absolutely-positioned sidebar without
    /// shifting the surrounding chrome.
    pub fn with_extra_left_padding(mut self, px: f32) -> Self {
        self.extra_left_padding = px;
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

impl Widget for Column {
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
                // Intrinsic-height columns run as a Block stack — flex
                // layout in taffy 0.9 silently truncates children when
                // the container's main-axis budget runs out, even with
                // flex_shrink: 0.0 on the parent. The result is an
                // outer Scrollable that thinks its content stops where
                // taffy stopped laying out. Block stacks children
                // without any axis budget, so the column's content
                // grows to its true sum.
                display: if self.intrinsic_height {
                    taffy::Display::Block
                } else {
                    taffy::Display::Flex
                },
                flex_direction: taffy::FlexDirection::Column,
                align_items: Some(taffy::AlignItems::FlexStart),
                flex_shrink: 0.0, // Prevent vertical squashing
                gap: taffy::Size {
                    width: length(0.0),
                    height: length(theme.gap_standard),
                },
                padding: {
                    let base = if self.no_padding { 0.0 } else { theme.padding_standard * 2.0 };
                    taffy::Rect {
                        left: length(base + self.extra_left_padding),
                        right: length(base),
                        top: length(base),
                        bottom: length(base),
                    }
                },
                size: taffy::Size {
                    width: percent(1.0),
                    height: if self.intrinsic_height {
                        taffy::Dimension::auto()
                    } else {
                        percent(1.0)
                    },
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
        self.rect = vello::kurbo::Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        // Pointer-capture short-circuit: when any child has latched a
        // drag (e.g. the slider's knob is being dragged) it returns true
        // from `wants_pointer_capture`. Route ALL events to that child
        // exclusively for the frame so a drag survives the cursor
        // crossing a peer widget's rect — without this the peer's
        // CursorMoved handler can set `handled` and break the loop
        // before the dragging child gets its event.
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

    /// Propagate pointer-capture intent from any descendant. Without
    /// this override, an outer Column wrapping a Row containing a
    /// dragging slider wouldn't see the capture intent (the default
    /// trait impl returns `false` for leaf widgets, and Column would
    /// inherit it). The Column's `handle_event` short-circuit then
    /// wouldn't fire, defeating the purpose.
    fn wants_pointer_capture(&self) -> bool {
        self.children.iter().any(|c| c.wants_pointer_capture())
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

    fn name(&self) -> &str { "Column" }

    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { self.children().iter().any(|c| c.needs_commit()) }
    fn on_commit(&mut self) { for child in self.children_mut() { child.on_commit(); } }

    fn take_structure_dirty(&mut self) -> bool {
        // Forward to every child; fold over the full set so each child's
        // flag is cleared even if an earlier one already reported dirty.
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
        // Propagate presence/permission context down to each child so nested
        // opt-in widgets still render their chrome.
        for child in &mut self.children {
            child.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }

        if let Some(scope) = self.permission_scope() {
            if !permissions.check(&scope) {
                let overlay = vello::peniko::Color::rgba8(0, 0, 0, 77);
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
