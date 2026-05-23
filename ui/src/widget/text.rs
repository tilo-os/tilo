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
    /// Whether the cache content needs to be re-emitted. Flipped
    /// on when the *size* of `self.rect` changes — pure
    /// translation (e.g. `Scrollable` offsetting all child rects
    /// during a scroll) keeps the cache, with the new position
    /// absorbed by the `Scene::append` transform below.
    dirty: bool,
    /// Rect the cache was emitted at. We compare *size* against
    /// the current rect to decide whether to re-emit, and use the
    /// origin to compute the per-frame translate that lets the
    /// cached scene follow its widget across the screen without
    /// re-encoding.
    cache_rect: vello::kurbo::Rect,
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
            cache_rect: vello::kurbo::Rect::ZERO,
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
        // `dirty` here only tracks **size** changes — pure
        // translation is absorbed by the per-frame `Scene::append`
        // transform computed in `draw`. Without this, scrolling a
        // 1000-widget tree would invalidate every cache every
        // frame (every widget's rect shifts by the scroll offset)
        // and the per-widget cache becomes net-negative.
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        if self.dirty || self.cache_rect == vello::kurbo::Rect::ZERO {
            self.cached.reset();
            let label = crate::i18n!(&self.label);
            let max_width = self.rect.width() as f32;
            text_renderer.draw_text(
                &mut self.cached,
                &label,
                self.size,
                theme.tokens.colors.text_primary,
                Affine::translate((self.rect.x0, self.rect.y0)),
                Some(max_width + 1.0),
            );
            self.cache_rect = self.rect;
        }
        // Per-frame translation between cache emit-time origin
        // and current origin — applied via the append transform
        // so the cached primitives don't get re-encoded just for
        // a scroll.
        let dx = self.rect.x0 - self.cache_rect.x0;
        let dy = self.rect.y0 - self.cache_rect.y0;
        scene.append(&self.cached, Some(transform * Affine::translate((dx, dy))));
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
