use crate::i18n::ArgValue;
use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::{
    FontChoice, HangingPunctuation, NumericStyle, OpenTypeTag, OpticalSizing, ParagraphAlign,
    TextRenderer, TextStyle, TextWeight, TextWrapMode,
};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;

pub struct Text {
    label: String,
    /// Named variables interpolated into the label's Fluent placeables. Held
    /// owned so a localized string like `counter = Count is { $count }` resolves
    /// to `Count is 5` (not the literal `{$count}`) every frame, even though the
    /// widget caches its rendered scene. Empty for plain, variable-free labels.
    args: Vec<(String, ArgValue)>,
    style: TextStyle,
    /// `None` (default) renders with the theme's `text_primary`.
    color: Option<vello::peniko::Color>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Cached `Scene` for this widget's last `draw` output. Reused
    /// across frames via `Scene::append` so an unchanged Text
    /// widget contributes ~zero shape / emit work per frame.
    cached: Scene,
    /// Whether the cache content needs to be re-emitted, flipped on when
    /// the *size* of `self.rect` changes — pure translation (e.g.
    /// `Scrollable` offsetting child rects) keeps the cache, absorbed by
    /// the `Scene::append` transform in `draw`.
    dirty: bool,
    /// Rect the cache was emitted at: size is compared against the
    /// current rect to decide whether to re-emit, and origin gives the
    /// per-frame translate so the cache follows its widget without
    /// re-encoding.
    cache_rect: vello::kurbo::Rect,
}

impl Text {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            args: Vec::new(),
            style: TextStyle::new(16.0),
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
        text.args = args
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect();
        text
    }

    /// Supply one Fluent variable for the label's placeables. Chainable, so
    /// multiple variables can be set: `Text::new("greeting").with_arg("name",
    /// "Ada").with_arg("count", 3)`. Accepts numbers and strings.
    #[must_use]
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
            let args: Vec<(&str, ArgValue)> = self
                .args
                .iter()
                .map(|(n, v)| (n.as_str(), v.clone()))
                .collect();
            crate::i18n::I18nManager::format(&self.label, &args)
        }
    }

    #[must_use]
    pub fn with_size(mut self, size: f32) -> Self {
        self.style.size = size;
        self
    }

    /// Apply a complete style in one operation. Measurement and painting use
    /// this same value, so leading, weight, face, and italic cannot drift.
    #[must_use]
    pub fn with_style(mut self, style: TextStyle) -> Self {
        self.style = style;
        self.dirty = true;
        self
    }

    /// Resolve a semantic role from the active theme. This is the preferred
    /// application-level API: one role selects size, leading, weight, and face
    /// together and automatically reflects text/UI scaling in `theme`.
    #[must_use]
    pub fn with_role(self, role: TextRole, theme: &Theme) -> Self {
        self.with_style(theme.tokens.typography.style(role))
    }

    #[must_use]
    pub fn with_line_height(mut self, multiplier: f32) -> Self {
        self.style.line_height = multiplier.max(0.1);
        self.dirty = true;
        self
    }

    /// Enable TeX/Liang automatic hyphenation for this text block using a
    /// BCP-47 language tag such as `"en-US"` or `"de"`.
    #[must_use]
    pub fn with_auto_hyphenation(mut self, language: impl Into<String>) -> Self {
        self.style = self.style.with_auto_hyphenation(language);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_weight(mut self, weight: TextWeight) -> Self {
        self.style.weight = weight;
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_italic(mut self, italic: bool) -> Self {
        self.style.italic = italic;
        self.dirty = true;
        self
    }

    /// Render this label in an explicit [`FontChoice`] instead of the
    /// default sans. Both measurement and draw shape use the same family,
    /// so a label previewing a font in its own face lays out correctly.
    #[must_use]
    pub fn with_font_family(mut self, family: FontChoice) -> Self {
        self.style.family = family;
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_letter_spacing(mut self, px: f32) -> Self {
        self.style = self.style.with_letter_spacing(px);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_tracking(mut self, em: f32) -> Self {
        self.style = self.style.with_tracking(em);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_font_feature(mut self, tag: OpenTypeTag, value: u32) -> Self {
        self.style = self.style.with_font_feature(tag, value);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_kerning(mut self, enabled: bool) -> Self {
        self.style = self.style.with_kerning(enabled);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_discretionary_ligatures(mut self, enabled: bool) -> Self {
        self.style = self.style.with_discretionary_ligatures(enabled);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_small_caps(mut self, enabled: bool) -> Self {
        self.style = self.style.with_small_caps(enabled);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_numeric_style(mut self, numeric_style: NumericStyle) -> Self {
        self.style = self.style.with_numeric_style(numeric_style);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_font_variation(mut self, tag: OpenTypeTag, value: f32) -> Self {
        self.style = self.style.with_font_variation(tag, value);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_optical_sizing(mut self, optical_sizing: OpticalSizing) -> Self {
        self.style = self.style.with_optical_sizing(optical_sizing);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_align(mut self, align: ParagraphAlign) -> Self {
        self.style = self.style.with_align(align);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_balanced_wrap(mut self) -> Self {
        self.style = self.style.with_wrap_mode(TextWrapMode::Balance);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_hanging_punctuation(mut self, hanging: HangingPunctuation) -> Self {
        self.style = self.style.with_hanging_punctuation(hanging);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_fragmentation(mut self, orphans: u8, widows: u8) -> Self {
        self.style = self.style.with_fragmentation(orphans, widows);
        self.dirty = true;
        self
    }

    /// Mutate the font-family override after construction. `None` restores the
    /// default sans face.
    pub fn set_font_family(&mut self, family: Option<FontChoice>) {
        let family = family.unwrap_or(FontChoice::SansSerif);
        if self.style.family != family {
            self.style.family = family;
            self.dirty = true;
        }
    }

    pub fn text_style(&self) -> &TextStyle {
        &self.style
    }

    /// External-layout assignment for callers placing this label outside
    /// the taffy tree. Mirrors
    /// [`crate::widget::TextInput::apply_layout_external`].
    pub fn apply_layout_external(&mut self, rect: vello::kurbo::Rect) {
        // Exact equality is the point — this is change detection for the
        // paint cache, not a numerical comparison that needs a tolerance.
        #[allow(
            clippy::float_cmp,
            reason = "exact-change detection for the paint cache, not numerical comparison"
        )]
        let size_changed =
            rect.width() != self.cache_rect.width() || rect.height() != self.cache_rect.height();
        self.rect = rect;
        if size_changed {
            self.dirty = true;
        }
    }

    /// Pass a Theme token (e.g. `theme.text_dim` or
    /// `theme.tokens.colors.semantic.danger`) so the choice still tracks
    /// the active theme.
    #[must_use]
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Widget for Text {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::auto(),
            },
            // flex_shrink 1.0 lets Taffy propose a smaller width than
            // natural_w — critical for wrapping.
            flex_shrink: 1.0,
            flex_grow: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known_dimensions: taffy::Size<Option<f32>>,
        available_space: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let label = self.resolved_label();

        let max_w = if let Some(w) = known_dimensions.width {
            Some(w)
        } else {
            match available_space.width {
                taffy::AvailableSpace::Definite(w) => Some(w),
                taffy::AvailableSpace::MinContent => Some(0.0), // force wrap to min-content
                taffy::AvailableSpace::MaxContent => None,      // allow infinite width
            }
        };

        let (w, h) = text_renderer.measure_style(&label, max_w, &self.style);

        // Tiny epsilon on width so rounding in Row/Column doesn't read the
        // text as "just a bit too big" and wrap it unnecessarily.
        taffy::Size {
            width: w + 0.1,
            height: h,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = vello::kurbo::Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Without this, scrolling a 1000-widget tree would invalidate every
        // cache every frame and the per-widget cache goes net-negative.
        // Exact equality is the point — this is change detection for the
        // paint cache, not a numerical comparison that needs a tolerance.
        #[allow(
            clippy::float_cmp,
            reason = "exact-change detection for the paint cache, not numerical comparison"
        )]
        let size_changed = self.rect.width() != self.cache_rect.width()
            || self.rect.height() != self.cache_rect.height();
        if size_changed {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        if self.dirty || self.cache_rect == vello::kurbo::Rect::ZERO {
            self.cached.reset();
            let label = self.resolved_label();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "label width in logical pixels is far below f32::MAX for any realistic UI element"
            )]
            let max_width = self.rect.width() as f32;
            let color = self.color.unwrap_or(theme.tokens.colors.text_primary);
            let origin = Affine::translate((self.rect.x0, self.rect.y0));
            text_renderer.draw_style(
                &mut self.cached,
                &label,
                color,
                origin,
                Some(max_width + 1.0),
                &self.style,
            );
            self.cache_rect = self.rect;
        }
        let dx = self.rect.x0 - self.cache_rect.x0;
        let dy = self.rect.y0 - self.cache_rect.y0;
        scene.append(
            &self.cached,
            Some(transform.pre_translate(vello::kurbo::Vec2::new(dx, dy))),
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "Text"
    }

    /// Opt into incremental redraw: true while the cache is unwritten or
    /// invalidated by a rect change. The App's redraw gate uses this to
    /// skip frames where no Text widget changed.
    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    /// Clears the cache-stale flag after the App consumes the scene.
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Text's intrinsic size is fixed at construction, so once
    /// `apply_layout` sets a non-zero `self.rect` this reports `false` —
    /// an unrelated sibling reflow shouldn't drag a re-shape along.
    /// `Rect::ZERO` means "not laid out yet," so the first frame still runs.
    fn is_layout_dirty(&self) -> bool {
        self.rect == vello::kurbo::Rect::ZERO
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        false
    }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool {
        false
    }
    fn needs_commit(&self) -> bool {
        false
    }

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
        self.draw(scene, transform, text_renderer, theme, presence);

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

    #[test]
    fn complete_style_is_retained_by_the_widget() {
        let style = TextStyle::new(18.0)
            .with_line_height(1.6)
            .with_weight(TextWeight::Semibold)
            .with_italic(true)
            .with_family(FontChoice::Serif);
        let text = Text::new("Reading").with_style(style.clone());
        assert_eq!(text.text_style(), &style);
    }

    #[test]
    fn semantic_role_uses_active_theme_typography() {
        let theme = Theme::dark().with_text_scale(1.5);
        let text = Text::new("Readable").with_role(TextRole::Reading, &theme);
        assert_eq!(text.text_style().size, theme.tokens.typography.scale.base);
        assert_eq!(
            text.text_style().line_height,
            theme.tokens.typography.line_height_relaxed
        );
    }
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

    /// A label with a font-family override resolves to that family: after
    /// it draws once, re-shaping the same string in the SAME named family
    /// is a shape-cache hit, while the sans default is a distinct miss —
    /// proving the override reaches the family-aware path.
    #[test]
    fn font_family_override_label_resolves_to_named_family() {
        use crate::presence::PresenceCtx;
        let mut tr = TextRenderer::new();
        let fams = tr.system_families();
        assert!(!fams.is_empty(), "the system font db exposes some families");
        let fam = fams[0].clone();

        let theme = Theme::dark();
        let presence = PresenceCtx::empty();

        let mut label = Text::new("Reading").with_font_family(FontChoice::Named(fam.clone()));
        // The label draws with a max_width of `rect.width() + 1.0`.
        label.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 400.0, 40.0));
        let mut scene = Scene::new();
        label.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        // Same string, size (16 default), wrap width (401.0) and family → a hit.
        let _ = tr.take_shape_stats();
        tr.draw_text_font(
            &mut scene,
            "Reading",
            16.0,
            theme.tokens.colors.text_primary,
            Affine::IDENTITY,
            Some(401.0),
            TextWeight::Regular,
            false,
            &FontChoice::Named(fam),
        );
        let (hits, _m, _ns) = tr.take_shape_stats();
        assert_eq!(hits, 1, "the label shaped its text in the named family");

        // Sans of the same string is a separate shape entry.
        tr.draw_text_font(
            &mut scene,
            "Reading",
            16.0,
            theme.tokens.colors.text_primary,
            Affine::IDENTITY,
            Some(401.0),
            TextWeight::Regular,
            false,
            &FontChoice::SansSerif,
        );
        let (_h, misses, _ns) = tr.take_shape_stats();
        assert_eq!(misses, 1, "sans is a separate shape from the named family");
    }
}
