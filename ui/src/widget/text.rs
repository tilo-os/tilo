use crate::i18n::ArgValue;
use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::{
    FontChoice, HangingPunctuation, NumericStyle, OpenTypeTag, OpticalSizing, ParagraphAlign,
    TextLayoutConstraints, TextLayoutEngine, TextRenderer, TextStyle, TextWeight, TextWrapMode,
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

type DynamicTextSource = Box<dyn FnMut() -> Option<String>>;
type DocumentTextSource = Box<dyn FnMut(&AutoCommit, &ObjId) -> String>;

enum TextSource {
    Local(DynamicTextSource),
    Document(DocumentTextSource),
}

pub struct Text {
    label: String,
    test_id: Option<String>,
    /// Returns `Some` only when a bound value should replace `label`.
    source: Option<TextSource>,
    /// Named variables interpolated into the label's Fluent placeables. Held
    /// owned so a localized string like `counter = Count is { $count }` resolves
    /// to `Count is 5` (not the literal `{$count}`) every frame, even though the
    /// widget caches its rendered scene. Empty for plain, variable-free labels.
    args: Vec<(String, ArgValue)>,
    i18n_revision: u64,
    style: TextStyle,
    /// Semantic typography follows the active theme on every layout build.
    /// Explicit style modifiers clear this value and take ownership of style.
    semantic_role: Option<TextRole>,
    layout_engine: Option<Box<dyn TextLayoutEngine>>,
    engine_revision: u64,
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
    /// A content change can alter intrinsic size as well as paint output.
    layout_dirty: bool,
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
            test_id: None,
            source: None,
            args: Vec::new(),
            i18n_revision: crate::i18n::I18nManager::revision(),
            style: TextStyle::new(16.0),
            semantic_role: None,
            layout_engine: None,
            engine_revision: 0,
            color: None,
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            cached: Scene::new(),
            dirty: true,
            layout_dirty: false,
            cache_rect: vello::kurbo::Rect::ZERO,
        }
    }

    /// Give this retained text node a stable address for inspection and
    /// headless scenarios.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Text recomputed during widget synchronization.
    ///
    /// Prefer [`Self::from_state`] for [`LocalState`](crate::LocalState): it
    /// uses the state's revision to skip the formatter when unchanged.
    pub fn dynamic(mut source: impl FnMut() -> String + 'static) -> Self {
        let initial = source();
        let mut text = Self::new(&initial);
        text.source = Some(TextSource::Local(Box::new(move || Some(source()))));
        text
    }

    /// Bind rendered text to typed process-local state.
    pub fn from_state<T: 'static>(
        state: &crate::LocalState<T>,
        mut render: impl FnMut(&T) -> String + 'static,
    ) -> Self {
        let initial = state.with(&mut render);
        let state = state.clone();
        let mut observed_revision = state.revision();
        let mut observed_i18n_revision = crate::i18n::I18nManager::revision();
        let mut text = Self::new(&initial);
        text.source = Some(TextSource::Local(Box::new(move || {
            let revision = state.revision();
            let i18n_revision = crate::i18n::I18nManager::revision();
            if revision == observed_revision && i18n_revision == observed_i18n_revision {
                return None;
            }
            observed_revision = revision;
            observed_i18n_revision = i18n_revision;
            Some(state.with(&mut render))
        })));
        text
    }

    /// Derive text from the shared Automerge model during the standard sync
    /// pass. Several views can read the same fields without notifications or
    /// app-owned fan-out; each invalidates only when its rendered value changes.
    pub fn from_document(render: impl FnMut(&AutoCommit, &ObjId) -> String + 'static) -> Self {
        let mut text = Self::new("");
        text.source = Some(TextSource::Document(Box::new(render)));
        text
    }

    /// Display a string scalar or Automerge `Text` object stored under `key`.
    /// This is the zero-boilerplate companion to
    /// [`TextInput::from_document_string`](super::TextInput::from_document_string)
    /// using the same key; use [`Self::from_document`] for projections.
    pub fn from_document_string(key: impl Into<String>) -> Self {
        use automerge::{ObjType, ReadDoc, ScalarValue, Value};

        let key = key.into();
        Self::from_document(move |doc, root| match doc.get(root, key.as_str()) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(value) => value.to_string(),
                _ => String::new(),
            },
            Ok(Some((Value::Object(ObjType::Text), object))) => {
                doc.text(&object).unwrap_or_default()
            }
            _ => String::new(),
        })
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
        self.use_explicit_style();
        self.style.size = size;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Apply a complete style in one operation. Measurement and painting use
    /// this same value, so leading, weight, face, and italic cannot drift.
    #[must_use]
    pub fn with_style(mut self, style: TextStyle) -> Self {
        self.use_explicit_style();
        self.style = style;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Replace measurement and painting while retaining Text's localization,
    /// accessibility, Taffy lifecycle, caching, and inspection behavior.
    #[must_use]
    pub fn with_layout_engine<E>(mut self, engine: E) -> Self
    where
        E: TextLayoutEngine + 'static,
    {
        self.engine_revision = engine.revision();
        self.layout_engine = Some(Box::new(engine));
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Force a remeasure/repaint after mutating engine state through a shared
    /// owner whose revision cannot be observed automatically.
    pub fn invalidate_layout_engine(&mut self) {
        self.dirty = true;
        self.layout_dirty = true;
    }

    fn observe_engine_revision(&mut self) {
        let Some(engine) = self.layout_engine.as_deref() else {
            return;
        };
        let revision = engine.revision();
        if revision != self.engine_revision {
            self.engine_revision = revision;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    fn observe_i18n_revision(&mut self) {
        let revision = crate::i18n::I18nManager::revision();
        if revision != self.i18n_revision {
            self.i18n_revision = revision;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    /// Select typography by intent instead of copying sizes into application
    /// code. The role follows the active theme, text scale, and UI scale when
    /// the same retained widget is laid out again.
    ///
    /// A later explicit typography modifier such as [`Self::with_size`] or
    /// [`Self::with_style`] opts out of the role. This keeps custom text fully
    /// controllable without making modifier order ambiguous.
    #[must_use]
    pub fn with_role(mut self, role: TextRole) -> Self {
        self.semantic_role = Some(role);
        let style = Theme::default().tokens.typography.style(role);
        self.style = self.merged_semantic_style(&style);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    fn use_explicit_style(&mut self) {
        self.semantic_role = None;
    }

    fn merged_semantic_style(&self, style: &TextStyle) -> TextStyle {
        let mut resolved = self.style.clone();
        resolved.size = style.size;
        resolved.line_height = style.line_height;
        resolved.weight = style.weight;
        resolved.italic = style.italic;
        resolved.family.clone_from(&style.family);
        resolved
    }

    fn observe_theme_role(&mut self, theme: &Theme) {
        let Some(role) = self.semantic_role else {
            return;
        };
        let style = theme.tokens.typography.style(role);
        let resolved = self.merged_semantic_style(&style);
        if self.style != resolved {
            self.style = resolved;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    #[must_use]
    pub fn with_line_height(mut self, multiplier: f32) -> Self {
        self.use_explicit_style();
        self.style.line_height = multiplier.max(0.1);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Enable TeX/Liang automatic hyphenation for this text block using a
    /// BCP-47 language tag such as `"en-US"` or `"de"`.
    #[must_use]
    pub fn with_auto_hyphenation(mut self, language: impl Into<String>) -> Self {
        self.style = self.style.with_auto_hyphenation(language);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Use an application-supplied discretionary-break algorithm while
    /// retaining the standard Text widget lifecycle and accessibility.
    #[must_use]
    pub fn with_hyphenator(
        mut self,
        hyphenator: std::sync::Arc<dyn crate::text_render::Hyphenator>,
    ) -> Self {
        self.style = self.style.with_hyphenator(hyphenator);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_weight(mut self, weight: TextWeight) -> Self {
        self.use_explicit_style();
        self.style.weight = weight;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_italic(mut self, italic: bool) -> Self {
        self.use_explicit_style();
        self.style.italic = italic;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Render this label in an explicit [`FontChoice`] instead of the
    /// default sans. Both measurement and draw shape use the same family,
    /// so a label previewing a font in its own face lays out correctly.
    #[must_use]
    pub fn with_font_family(mut self, family: FontChoice) -> Self {
        self.use_explicit_style();
        self.style.family = family;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_letter_spacing(mut self, px: f32) -> Self {
        self.style = self.style.with_letter_spacing(px);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_tracking(mut self, em: f32) -> Self {
        self.style = self.style.with_tracking(em);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_font_feature(mut self, tag: OpenTypeTag, value: u32) -> Self {
        self.style = self.style.with_font_feature(tag, value);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_kerning(mut self, enabled: bool) -> Self {
        self.style = self.style.with_kerning(enabled);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_discretionary_ligatures(mut self, enabled: bool) -> Self {
        self.style = self.style.with_discretionary_ligatures(enabled);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_small_caps(mut self, enabled: bool) -> Self {
        self.style = self.style.with_small_caps(enabled);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_numeric_style(mut self, numeric_style: NumericStyle) -> Self {
        self.style = self.style.with_numeric_style(numeric_style);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_font_variation(mut self, tag: OpenTypeTag, value: f32) -> Self {
        self.style = self.style.with_font_variation(tag, value);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_optical_sizing(mut self, optical_sizing: OpticalSizing) -> Self {
        self.style = self.style.with_optical_sizing(optical_sizing);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_align(mut self, align: ParagraphAlign) -> Self {
        self.style = self.style.with_align(align);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_balanced_wrap(mut self) -> Self {
        self.style = self.style.with_wrap_mode(TextWrapMode::Balance);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_hanging_punctuation(mut self, hanging: HangingPunctuation) -> Self {
        self.style = self.style.with_hanging_punctuation(hanging);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    #[must_use]
    pub fn with_fragmentation(mut self, orphans: u8, widows: u8) -> Self {
        self.style = self.style.with_fragmentation(orphans, widows);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Mutate the font-family override after construction. `None` restores the
    /// default sans face.
    pub fn set_font_family(&mut self, family: Option<FontChoice>) {
        self.use_explicit_style();
        let family = family.unwrap_or(FontChoice::SansSerif);
        if self.style.family != family {
            self.style.family = family;
            self.dirty = true;
            self.layout_dirty = true;
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
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.observe_i18n_revision();
        self.observe_engine_revision();
        let label = match self.source.as_mut() {
            Some(TextSource::Local(source)) => source(),
            Some(TextSource::Document(source)) => Some(source(state, path)),
            None => None,
        };
        let Some(label) = label else {
            return;
        };
        if self.label != label {
            self.label = label;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.observe_theme_role(theme);
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
        self.observe_i18n_revision();
        self.observe_engine_revision();
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

        let max_h = if let Some(height) = known_dimensions.height {
            Some(height)
        } else {
            match available_space.height {
                taffy::AvailableSpace::Definite(height) => Some(height),
                taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => None,
            }
        };
        let constraints = TextLayoutConstraints {
            max_width: max_w,
            max_height: max_h,
        };
        let (w, h) = if let Some(engine) = self.layout_engine.as_deref_mut() {
            engine.measure(text_renderer, &label, constraints, &self.style)
        } else {
            text_renderer.measure_style(&label, max_w, &self.style)
        };

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
        self.observe_theme_role(theme);
        self.observe_i18n_revision();
        self.observe_engine_revision();
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
            let constraints = TextLayoutConstraints {
                max_width: Some(max_width + 1.0),
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "text widget height is an ordinary logical-pixel value"
                )]
                max_height: Some(self.rect.height() as f32),
            };
            if let Some(engine) = self.layout_engine.as_deref_mut() {
                engine.draw(
                    text_renderer,
                    &mut self.cached,
                    &label,
                    constraints,
                    &self.style,
                    color,
                    origin,
                );
            } else {
                text_renderer.draw_style(
                    &mut self.cached,
                    &label,
                    color,
                    origin,
                    constraints.max_width,
                    &self.style,
                );
            }
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

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    /// Opt into incremental redraw: true while the cache is unwritten or
    /// invalidated by a rect change. The App's redraw gate uses this to
    /// skip frames where no Text widget changed.
    fn needs_redraw(&self) -> bool {
        self.dirty || self.i18n_revision != crate::i18n::I18nManager::revision()
    }

    /// Clears the cache-stale flag after the App consumes the scene.
    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.layout_dirty = false;
    }

    /// Text's intrinsic size is fixed at construction, so once
    /// `apply_layout` sets a non-zero `self.rect` this reports `false` —
    /// an unrelated sibling reflow shouldn't drag a re-shape along.
    /// `Rect::ZERO` means "not laid out yet," so the first frame still runs.
    fn is_layout_dirty(&self) -> bool {
        self.rect == vello::kurbo::Rect::ZERO
            || self.layout_dirty
            || self.i18n_revision != crate::i18n::I18nManager::revision()
    }

    fn needs_idle_sync(&self) -> bool {
        self.i18n_revision != crate::i18n::I18nManager::revision()
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

    struct RecordingEngine {
        measures: std::rc::Rc<std::cell::Cell<u32>>,
        draws: std::rc::Rc<std::cell::Cell<u32>>,
        revision: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl TextLayoutEngine for RecordingEngine {
        fn measure(
            &mut self,
            _renderer: &mut TextRenderer,
            _text: &str,
            _constraints: TextLayoutConstraints,
            _style: &TextStyle,
        ) -> (f32, f32) {
            self.measures.set(self.measures.get().saturating_add(1));
            (123.0, 45.0)
        }

        fn draw(
            &mut self,
            _renderer: &mut TextRenderer,
            _scene: &mut Scene,
            _text: &str,
            _constraints: TextLayoutConstraints,
            _style: &TextStyle,
            _color: vello::peniko::Color,
            _transform: Affine,
        ) {
            self.draws.set(self.draws.get().saturating_add(1));
        }

        fn revision(&self) -> u64 {
            self.revision.get()
        }
    }

    #[test]
    fn custom_layout_engine_keeps_text_widget_lifecycle() {
        let measures = std::rc::Rc::new(std::cell::Cell::new(0));
        let draws = std::rc::Rc::new(std::cell::Cell::new(0));
        let revision = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut text = Text::new("engine text").with_layout_engine(RecordingEngine {
            measures: std::rc::Rc::clone(&measures),
            draws: std::rc::Rc::clone(&draws),
            revision: std::rc::Rc::clone(&revision),
        });
        let mut renderer = TextRenderer::new();
        let size = Widget::measure(
            &mut text,
            taffy::Size {
                width: None,
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut renderer,
        );
        assert_eq!(size.width, 123.1);
        assert_eq!(size.height, 45.0);
        assert_eq!(measures.get(), 1);

        text.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 123.1, 45.0));
        let mut scene = Scene::new();
        Widget::draw(
            &mut text,
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &Theme::dark(),
            &crate::presence::PresenceCtx::empty(),
        );
        assert_eq!(draws.get(), 1);
        let node = Widget::accessibility(&text).expect("custom text stays accessible");
        assert_eq!(node.role, crate::a11y::Role::StaticText);
        assert_eq!(node.name.as_deref(), Some("engine text"));

        Widget::mark_drawn(&mut text);
        assert!(!Widget::needs_redraw(&text));
        assert!(!Widget::is_layout_dirty(&text));
        revision.set(1);
        let mut document = AutoCommit::new();
        Widget::sync(&mut text, &mut document, &automerge::ROOT);
        assert!(Widget::needs_redraw(&text));
        assert!(Widget::is_layout_dirty(&text));
    }

    #[test]
    fn local_state_binding_invalidates_text_only_after_a_change() {
        let state = crate::LocalState::new(String::from("Ada"));
        let mut text = Text::from_state(&state, |name| format!("Hello, {name}"));
        text.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 200.0, 40.0));
        Widget::mark_drawn(&mut text);
        assert!(!Widget::needs_redraw(&text));
        assert!(!Widget::is_layout_dirty(&text));
        assert_eq!(
            Widget::accessibility(&text).unwrap().name.as_deref(),
            Some("Hello, Ada")
        );

        let mut document = AutoCommit::new();
        Widget::sync(&mut text, &mut document, &automerge::ROOT);
        assert!(!Widget::needs_redraw(&text));
        assert!(!Widget::is_layout_dirty(&text));

        state.set(String::from("Grace"));
        Widget::sync(&mut text, &mut document, &automerge::ROOT);
        assert!(Widget::needs_redraw(&text));
        assert!(Widget::is_layout_dirty(&text));
        assert_eq!(
            Widget::accessibility(&text).unwrap().name.as_deref(),
            Some("Hello, Grace")
        );
    }

    #[test]
    fn dynamic_text_compares_output_before_invalidating() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let captured = std::rc::Rc::clone(&calls);
        let mut text = Text::dynamic(move || {
            captured.set(captured.get().saturating_add(1));
            String::from("constant")
        });
        text.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 200.0, 40.0));
        Widget::mark_drawn(&mut text);

        let mut document = AutoCommit::new();
        Widget::sync(&mut text, &mut document, &automerge::ROOT);
        assert_eq!(calls.get(), 2);
        assert!(!Widget::needs_redraw(&text));
        assert!(!Widget::is_layout_dirty(&text));
    }

    #[test]
    fn document_text_keeps_multiple_views_in_sync_through_undo() {
        use automerge::{ReadDoc, ScalarValue, Value, transaction::Transactable};

        fn title(doc: &AutoCommit, root: &ObjId) -> String {
            match doc.get(root, "title") {
                Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                    ScalarValue::Str(value) => value.to_string(),
                    _ => String::new(),
                },
                _ => String::new(),
            }
        }

        let mut state = crate::State::new();
        state.doc.put(&state.root, "title", "Alpha").unwrap();
        state.commit_action(None, None);
        let mut master = Text::from_document_string("title");
        let mut detail = Text::from_document(|doc, root| format!("Editing: {}", title(doc, root)));
        Widget::sync(&mut master, &mut state.doc, &state.root);
        Widget::sync(&mut detail, &mut state.doc, &state.root);
        assert_eq!(
            Widget::accessibility(&master).unwrap().name.as_deref(),
            Some("Alpha")
        );
        assert_eq!(
            Widget::accessibility(&detail).unwrap().name.as_deref(),
            Some("Editing: Alpha")
        );

        master.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 100.0, 20.0));
        detail.apply_layout_external(vello::kurbo::Rect::new(0.0, 20.0, 100.0, 40.0));
        Widget::mark_drawn(&mut master);
        Widget::mark_drawn(&mut detail);
        state.doc.put(&state.root, "title", "Beta").unwrap();
        state.commit_action(None, None);
        Widget::sync(&mut master, &mut state.doc, &state.root);
        Widget::sync(&mut detail, &mut state.doc, &state.root);
        assert_eq!(
            Widget::accessibility(&master).unwrap().name.as_deref(),
            Some("Beta")
        );
        assert_eq!(
            Widget::accessibility(&detail).unwrap().name.as_deref(),
            Some("Editing: Beta")
        );
        assert!(Widget::needs_redraw(&master));
        assert!(Widget::needs_redraw(&detail));

        state.undo(None, None).unwrap();
        Widget::sync(&mut master, &mut state.doc, &state.root);
        Widget::sync(&mut detail, &mut state.doc, &state.root);
        assert_eq!(
            Widget::accessibility(&master).unwrap().name.as_deref(),
            Some("Alpha")
        );
        assert_eq!(
            Widget::accessibility(&detail).unwrap().name.as_deref(),
            Some("Editing: Alpha")
        );
    }

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
        let mut text = Text::new("Readable").with_role(TextRole::Reading);
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        Widget::build_layout(&mut text, &mut taffy, &theme, &mut nodes);
        assert_eq!(text.text_style().size, theme.tokens.typography.scale.base);
        assert_eq!(
            text.text_style().line_height,
            theme.tokens.typography.line_height_relaxed
        );
    }

    #[test]
    fn semantic_role_follows_theme_changes_without_rebuilding_widget() {
        let mut text = Text::new("Readable")
            .with_role(TextRole::Reading)
            .with_auto_hyphenation("en-US");
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let default_theme = Theme::default();
        Widget::build_layout(&mut text, &mut taffy, &default_theme, &mut nodes);
        let default_size = text.text_style().size;

        let scaled_theme = Theme::dark().with_text_scale(1.5);
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        Widget::build_layout(&mut text, &mut taffy, &scaled_theme, &mut nodes);

        assert!(text.text_style().size > default_size);
        assert_eq!(
            text.text_style().size,
            scaled_theme.tokens.typography.style(TextRole::Reading).size
        );
        assert!(matches!(
            &text.text_style().hyphenation,
            crate::text_render::Hyphenation::Auto(language) if language == "en-US"
        ));
    }

    #[test]
    fn explicit_typography_modifier_opts_out_of_semantic_role() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut text = Text::new("Fixed")
            .with_role(TextRole::Title)
            .with_size(19.0);
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        Widget::build_layout(&mut text, &mut taffy, &theme, &mut nodes);

        assert_eq!(text.text_style().size, 19.0);
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

    #[test]
    fn locale_switch_remeasures_and_repaints_retained_text_and_button() {
        fn named<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
            if node.test_id.as_deref() == Some(id) {
                return Some(node);
            }
            node.children.iter().find_map(|child| named(child, id))
        }

        use crate::headless::InspectionNode;
        use crate::widget::{Button, Column};

        const EN: &str = "title = Apps\naction = Open";
        const DE: &str = "title = Anwendungen\naction = Einstellungen öffnen";
        I18nManager::install("en-US", "en-US", &[("en-US", EN), ("de", DE)]).unwrap();
        let root = Column::new()
            .push(Box::new(Text::new("title")))
            .push(Box::new(
                Button::new("localized-action", "action", None).fit_content(40.0),
            ));
        let mut driver =
            crate::headless::Driver::new(crate::State::new(), Box::new(root), (700, 300));

        let before = driver.inspection();
        let before_button = named(&before.root, "localized-action").unwrap();
        let before_width = before_button.rect[2];
        assert_eq!(
            before_button
                .semantics
                .as_ref()
                .and_then(|node| node.name.as_deref()),
            Some("Open")
        );

        I18nManager::set_locale("de-DE");
        driver.relayout();
        let after = driver.inspection();
        let after_button = named(&after.root, "localized-action").unwrap();
        assert_eq!(
            after_button
                .semantics
                .as_ref()
                .and_then(|node| node.name.as_deref()),
            Some("Einstellungen öffnen")
        );
        assert!(after_button.rect[2] > before_width);
        assert!(driver.inspection_json().unwrap().contains("Anwendungen"));
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
