use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PathSegment, PresenceKey};
use crate::permissions::PermissionScope;

pub struct Text {
    label: String,
    size: f32,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Cached `Scene` for this widget's last `draw` output. Reused
    /// across frames via `Scene::append` so an unchanged Text
    /// widget contributes ~zero shape / emit work per frame.
    cached: Scene,
    /// Whether the cached scene needs to be re-emitted. Flipped on
    /// when `self.rect` changes (apply_layout), when the label or
    /// size changes (no public setters yet, so this only happens
    /// at construction), and on theme changes (apply_layout
    /// invalidates as a side-effect of rect-write).
    dirty: bool,
    /// Last rect the cache was emitted at. apply_layout flips
    /// `dirty` when the rect actually changed — Column / Row
    /// re-layouts often produce identical rects, no need to
    /// invalidate then.
    last_rect: vello::kurbo::Rect,
}

impl Text {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            size: 16.0,
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            cached: Scene::new(),
            dirty: true,
            last_rect: vello::kurbo::Rect::ZERO,
        }
    }

    pub fn with_size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Text {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::auto(),
            },
            // flex_shrink: 1.0 is CRITICAL for wrapping. 
            // It allows Taffy to propose a smaller width than natural_w.
            flex_shrink: 1.0,
            flex_grow: 0.0,
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(&mut self, known_dimensions: taffy::Size<Option<f32>>, available_space: taffy::Size<taffy::AvailableSpace>, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        let label = crate::i18n!(&self.label);
        
        // 1. Determine the maximum width we are allowed to have
        let max_w = if let Some(w) = known_dimensions.width {
            Some(w)
        } else {
            match available_space.width {
                taffy::AvailableSpace::Definite(w) => Some(w),
                taffy::AvailableSpace::MinContent => Some(0.0), // Force wrap to min-content
                taffy::AvailableSpace::MaxContent => None,      // Allow infinite width
            }
        };

        // 2. Measure with that constraint
        let (w, h) = text_renderer.measure(&label, self.size, max_w);
        
        // 3. Return the measured size. 
        // We add a tiny 0.1 epsilon to width to prevent rounding errors in Row/Column
        // from thinking the text is "just a bit too big" and wrapping it unnecessarily.
        taffy::Size { width: w + 0.1, height: h }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        // Mark the cache stale only when the rect actually moved —
        // Column / Row re-runs often produce identical rects (e.g.
        // sibling widgets toggle their state but our position
        // doesn't change). Without this guard the cache would
        // invalidate every frame and the whole incremental story
        // collapses.
        if self.rect != self.last_rect {
            self.dirty = true;
            self.last_rect = self.rect;
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        if self.dirty {
            self.cached.reset();
            let label = crate::i18n!(&self.label);
            let max_width = self.rect.width() as f32;
            // Cache holds the scene in *logical* (transform-free)
            // coords — the App's `transform` is reapplied on append
            // below. This makes the cache survive DPI changes.
            text_renderer.draw_text(
                &mut self.cached,
                &label,
                self.size,
                theme.tokens.colors.text_primary,
                Affine::translate((self.rect.x0, self.rect.y0)),
                Some(max_width + 1.0),
            );
        }
        scene.append(&self.cached, Some(transform));
    }

    fn handle_event(&mut self, _event: &WindowEvent, _scale_factor: f64, _modifiers: &winit::event::Modifiers, _state: &mut AutoCommit, _path: &ObjId, _handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {}

    fn name(&self) -> &str { "Text" }

    /// Opt into incremental redraw. Returns true while the cache
    /// has not been written yet (or has been invalidated by a rect
    /// change). The App's redraw gate uses this to skip frames
    /// where no Text widget changed.
    fn needs_redraw(&self) -> bool { self.dirty }

    /// Clears the cache-stale flag after the App has consumed the
    /// scene. Subsequent frames will re-use `self.cached` until
    /// the rect changes again.
    fn mark_drawn(&mut self) { self.dirty = false; }

    /// Text never changes its own intrinsic size between frames
    /// (the label is fixed at construction, the size too) — once
    /// `apply_layout` has set `self.rect` to its non-zero
    /// resolved value, we report `false` so an unrelated sibling
    /// reflow doesn't drag a Text re-shape along with it. The
    /// `Rect::ZERO` sentinel covers "haven't been laid out yet"
    /// so the first frame still runs `compute_layout`.
    fn is_layout_dirty(&self) -> bool { self.rect == vello::kurbo::Rect::ZERO }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { false }

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
        self.draw(scene, transform, text_renderer, theme, presence);

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
