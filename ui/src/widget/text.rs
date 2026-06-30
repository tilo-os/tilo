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
use crate::i18n::ArgValue;

pub struct Text {
    label: String,
    /// Named variables interpolated into the label's Fluent placeables. Held
    /// owned so a localized string like `counter = Count is { $count }` resolves
    /// to `Count is 5` (not the literal `{$count}`) every frame, even though the
    /// widget caches its rendered scene. Empty for plain, variable-free labels.
    args: Vec<(String, ArgValue)>,
    size: f32,
    /// Optional explicit colour. `None` (the default) renders with the
    /// theme's `text_primary`, keeping Text theme-driven; `Some(c)` lets
    /// callers paint secondary/dim/semantic copy (e.g. the installer's
    /// per-field helper text in `theme.text_dim`) without hard-coding a
    /// colour at the call site — they still pass a Theme token.
    color: Option<vello::peniko::Color>,
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
            args: Vec::new(),
            size: 16.0,
            color: None,
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            cached: Scene::new(),
            dirty: true,
            cache_rect: vello::kurbo::Rect::ZERO,
        }
    }

    /// Construct text from a key whose Fluent message takes variables, e.g.
    /// `Text::with_args("counter", &[("count", 5.into())])` resolving
    /// `counter = Count is { $count }` to `Count is 5`.
    pub fn with_args(key: &str, args: &[(&str, ArgValue)]) -> Self {
        let mut text = Self::new(key);
        text.args = args.iter().map(|(n, v)| (n.to_string(), v.clone())).collect();
        text
    }

    /// Supply one Fluent variable for the label's placeables. Chainable, so
    /// multiple variables can be set: `Text::new("greeting").with_arg("name",
    /// "Ada").with_arg("count", 3)`. Accepts numbers and strings.
    pub fn with_arg(mut self, name: &str, value: impl Into<ArgValue>) -> Self {
        self.args.push((name.to_string(), value.into()));
        self
    }

    /// Resolve the label through the i18n layer, interpolating any variables
    /// supplied via [`with_arg`](Self::with_arg) / [`with_args`](Self::with_args).
    fn resolved_label(&self) -> String {
        if self.args.is_empty() {
            crate::i18n!(&self.label)
        } else {
            let args: Vec<(&str, ArgValue)> =
                self.args.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
            crate::i18n::I18nManager::format(&self.label, &args)
        }
    }

    pub fn with_size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Render this text in an explicit colour instead of the theme's
    /// `text_primary`. Pass a Theme token (e.g. `theme.text_dim` or
    /// `theme.tokens.colors.semantic.danger`) so the choice still tracks
    /// the active theme.
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.color = Some(color);
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
        let label = self.resolved_label();
        
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
            let label = self.resolved_label();
            let max_width = self.rect.width() as f32;
            let color = self.color.unwrap_or(theme.tokens.colors.text_primary);
            text_renderer.draw_text(
                &mut self.cached,
                &label,
                self.size,
                color,
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

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The FTL-resolved text *is* the accessible name. Empty text carries no
        // meaning, so it stays presentational (collapses out of the tree).
        let label = self.resolved_label();
        if label.is_empty() {
            return None;
        }
        Some(AccessNode::new(Role::StaticText).with_name(label))
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
    use crate::a11y::Role;

    #[test]
    fn accessibility_reports_static_text_with_label_as_name() {
        let t = Text::new("Hello world");
        let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
        assert_eq!(node.role, Role::StaticText);
        assert_eq!(node.name.as_deref(), Some("Hello world"));
        assert_eq!(node.value, None);
        assert!(!node.state.focusable, "static text is not focusable");
    }

    #[test]
    fn accessibility_empty_text_is_presentational() {
        let t = Text::new("");
        assert!(
            Widget::accessibility(&t).is_none(),
            "empty text collapses out of the a11y tree",
        );
    }

    use crate::i18n::I18nManager;

    // The i18n manager is a thread-local, so these tests are isolated per test
    // thread; each re-inits a fresh bundle and asserts only on its own keys.

    #[test]
    fn with_arg_interpolates_fluent_variable() {
        I18nManager::init("en-US");
        I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

        let t = Text::new("counter").with_arg("count", 5);
        let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
        // The placeholder must be replaced, not echoed literally.
        assert_eq!(node.name.as_deref(), Some("Count is 5"));
        assert!(!node.name.unwrap().contains("$count"));
    }

    #[test]
    fn with_args_accepts_strings_and_numbers() {
        I18nManager::init("en-US");
        I18nManager::add_resource(
            "en-US",
            "greeting = Hi { $name }, you have { $count } messages".to_string(),
        );

        let t = Text::with_args("greeting", &[("name", "Ada".into()), ("count", 3.into())]);
        let node = Widget::accessibility(&t).expect("resolved");
        assert_eq!(node.name.as_deref(), Some("Hi Ada, you have 3 messages"));
    }

    #[test]
    fn missing_arg_degrades_without_panic() {
        I18nManager::init("en-US");
        I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

        // No `count` supplied — must not panic; the formatter renders a
        // best-effort placeholder rather than crashing.
        let t = Text::new("counter");
        let node = Widget::accessibility(&t).expect("resolved");
        let name = node.name.unwrap();
        assert!(name.contains("Count is"), "got {name:?}");
    }

    #[test]
    fn unknown_key_falls_back_to_key() {
        I18nManager::init("en-US");
        // No resource for this key at all.
        let t = Text::new("totally-unknown-key").with_arg("x", 1);
        let node = Widget::accessibility(&t).expect("resolved");
        assert_eq!(node.name.as_deref(), Some("totally-unknown-key"));
    }
}
