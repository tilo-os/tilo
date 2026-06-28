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
    extra_bottom_padding: f32,
    no_padding: bool,
    /// When `Some(px)`, override the column's default top + bottom
    /// padding with `px`. Left/right keep using the standard
    /// `padding_standard * 2` (plus `extra_left_padding`) so the
    /// sidebar gutter math stays intact. Set this when a column
    /// should hug the viewport's top + bottom edges tightly without
    /// dropping the horizontal gutters too (the shell's strip Column
    /// uses 4 px — close enough to the screen edges that windows
    /// read as filling the workspace, with just enough room for the
    /// active-tile shadow not to clip).
    vertical_padding_override: Option<f32>,
    /// When `Some((left, right))`, override the column's default
    /// left/right padding (replacing `base + extra_left_padding` on
    /// the left, `base` on the right). Used by the shell's content
    /// column so the strip extends all the way to viewport x=0 (the
    /// wheel disc lives in an overlay layer above the strip and pans
    /// with the strip's `scroll_x`; the strip itself reserves the
    /// wheel's width as a `leading_inset` on `GroupRow`). With the
    /// default standard horizontal padding we'd otherwise leave a
    /// 24 px wallpaper gap to the left of the wheel disc.
    horizontal_padding_override: Option<(f32, f32)>,
    intrinsic_height: bool,
    /// Use `flex_grow: 1 + flex_basis: 0` instead of the default
    /// `height: percent(1.0)` outer size. The percent variant claims
    /// 100 % of the parent's height even when the column is a flex
    /// sibling of a fixed-height header (e.g. the shell's topbar) —
    /// the sum overflows the parent and any bottom padding ends up
    /// off-screen. Fill-remaining-height absorbs the leftover space
    /// after siblings have taken their share, which is what most
    /// "full-bleed below header" layouts actually want.
    fill_remaining_height: bool,
    /// Subtree scene cache — composition of every descendant's
    /// own cached scene, recorded once and translated on subsequent
    /// frames as long as no descendant signals dirty. Saves
    /// hundreds of `Scene::append` calls per frame when most of
    /// the column's content is stable.
    subtree_cache: Scene,
    /// Rect (at cache-emit time) the subtree was composed against.
    /// Size mismatch → rebuild; pure translation → reuse with a
    /// `translate(delta)` on the outer append.
    cache_rect: vello::kurbo::Rect,
}

impl Column {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            extra_left_padding: 0.0,
            extra_bottom_padding: 0.0,
            no_padding: false,
            vertical_padding_override: None,
            horizontal_padding_override: None,
            intrinsic_height: false,
            fill_remaining_height: false,
            subtree_cache: Scene::new(),
            cache_rect: vello::kurbo::Rect::ZERO,
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

    /// Add extra padding on the bottom edge on top of the column's
    /// default `padding_standard * 2`. Used by the shell's content
    /// column to keep the active window group from touching the
    /// screen's bottom edge — the default 24 px gutter wasn't enough
    /// breathing room for tile cards on smaller viewports.
    pub fn with_extra_bottom_padding(mut self, px: f32) -> Self {
        self.extra_bottom_padding = px;
        self
    }

    /// Override the column's top + bottom padding (replaces the
    /// default `padding_standard * 2`). Left + right padding still
    /// uses the standard value plus `extra_left_padding`. Use this
    /// for layouts where the column needs to hug the viewport's
    /// top/bottom edges (e.g. the shell strip wants its windows
    /// almost flush against the topbar / screen bottom).
    pub fn with_vertical_padding(mut self, px: f32) -> Self {
        self.vertical_padding_override = Some(px);
        self
    }

    /// Override the column's left + right padding (replaces
    /// `base + extra_left_padding` on the left, `base` on the
    /// right). Use this when the column needs to extend to a
    /// specific viewport range (e.g. the shell's content column
    /// drops its left padding to zero so the strip can reach
    /// viewport x=0 and the wheel disc can pan into and out of
    /// off-screen-left territory cleanly).
    pub fn with_horizontal_padding(mut self, left: f32, right: f32) -> Self {
        self.horizontal_padding_override = Some((left, right));
        self
    }

    /// Switch the column from `height: percent(1.0)` to
    /// `flex_grow: 1 + flex_basis: 0`. Use this when the column is a
    /// flex sibling of a fixed-height widget (a topbar, say) — the
    /// percent variant would claim 100 % of the parent on top of the
    /// sibling's height, overflowing and pushing the column's own
    /// bottom padding off-screen.
    pub fn with_fill_remaining_height(mut self) -> Self {
        self.fill_remaining_height = true;
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
                    let vert = self.vertical_padding_override.unwrap_or(base);
                    let (left, right) = self
                        .horizontal_padding_override
                        .unwrap_or((base + self.extra_left_padding, base));
                    taffy::Rect {
                        left: length(left),
                        right: length(right),
                        top: length(vert),
                        bottom: length(vert + self.extra_bottom_padding),
                    }
                },
                size: taffy::Size {
                    width: percent(1.0),
                    height: if self.intrinsic_height {
                        taffy::Dimension::auto()
                    } else if self.fill_remaining_height {
                        taffy::Dimension::auto()
                    } else {
                        percent(1.0)
                    },
                },
                flex_basis: if self.fill_remaining_height {
                    taffy::Dimension::length(0.0)
                } else {
                    taffy::Dimension::auto()
                },
                flex_grow: if self.fill_remaining_height { 1.0 } else { 0.0 },
                ..Default::default()
            },
            &child_nodes
        ).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        // Use the larger of the own size and the children's content
        // bounds — taffy can clamp a Block container's reported size
        // to its parent's available main axis (notably inside a
        // Scrollable), leaving the rect smaller than the area its
        // children actually occupy. If we used `layout.size` alone,
        // `should_cull` would incorrectly discard the whole subtree
        // once scrolling pushed the under-reported rect off the
        // viewport, even though children remained on screen.
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
        if presence.should_cull(self.get_rect()) { return; }

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
            // Children must rebuild at `Affine::IDENTITY`: vello's
            // `Scene::append` composes its transform with each stored
            // primitive transform, so passing `transform` here would
            // double-apply it on the outer `append` below — at any DPI
            // scale ≠ 1, cached widgets land off-screen.
            //
            // Clear the active viewport for the recursion so children
            // don't spatially cull themselves out of the cache. If we
            // left culling on, the cache would only contain primitives
            // for the *currently visible* sub-range; a subsequent scroll
            // (cache-hit path, no rebuild) would translate the cache
            // into a new viewport position where the now-visible widgets
            // were never recorded — the visible region renders as
            // background. vello's tile binner skips off-viewport tiles
            // at compose time anyway, so the wasted-work concern that
            // motivated culling here is illusory.
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
