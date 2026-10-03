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
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

type DynamicTextSource = Box<dyn FnMut() -> Option<String>>;
type DocumentTextSource = Box<dyn FnMut(&AutoCommit, &ObjId) -> String>;

/// Top edge of one text line centred in a band — the shared rule from
/// [`crate::text_metrics`], re-exported here because [`super::Button`] and
/// the other controls learned it from this module. Pass the `.1` of
/// [`TextRenderer::measure`] / [`TextRenderer::measure_style`], never the
/// font size.
pub use crate::text_metrics::centered_text_top;

/// Test helper shared by the widgets that centre a line with
/// [`centered_text_top`]: asserts that glyph run `run_index` of `scene` sits
/// where `text` at `size` lands when its *measured* line is centred in the
/// band `band_y0 ..= band_y0 + band_height`. The expectation is a reference
/// `draw_text` at the computed top rather than an assumed baseline offset, so
/// the check follows the renderer's own line metrics.
#[cfg(test)]
pub(crate) fn assert_glyph_run_centred_in_band(
    scene: &Scene,
    run_index: usize,
    text: &str,
    size: f32,
    band_y0: f64,
    band_height: f64,
    renderer: &mut TextRenderer,
    color: vello::peniko::Color,
) {
    let (_, measured_h) = renderer.measure(text, size, None);
    let measured_h = f64::from(measured_h);
    assert!(
        measured_h > f64::from(size),
        "{text:?}: the measured line ({measured_h}) must exceed the font size ({size}); \
         otherwise font-size centring could not drift"
    );
    let expected_top = centered_text_top(band_y0, band_height, measured_h);
    let band_centre = band_y0 + band_height / 2.0;
    assert!(
        (expected_top + measured_h / 2.0 - band_centre).abs() < 1e-9,
        "{text:?}: line centre must be the band centre {band_centre}"
    );
    let mut reference = Scene::new();
    renderer.draw_text(
        &mut reference,
        text,
        size,
        color,
        Affine::translate((0.0, expected_top)),
        None,
    );
    let reference = &reference.encoding().resources.glyph_runs[0];
    let runs = &scene.encoding().resources.glyph_runs;
    let run = runs.get(run_index).unwrap_or_else(|| {
        panic!(
            "{text:?}: glyph run {run_index} missing ({} runs painted)",
            runs.len()
        )
    });
    assert_eq!(
        run.transform.translation[1],
        reference.transform.translation[1],
        "{text:?} is not centred on its measured line in band {band_y0}..{}",
        band_y0 + band_height
    );
}

enum TextSource {
    Local(DynamicTextSource),
    Document(DocumentTextSource),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent typography, wrapping, localization, and render-cache flags are orthogonal presentation options rather than a single state machine"
)]
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
    /// Semantic intent drives accessibility and, by default, live typography.
    semantic_role: Option<TextRole>,
    follows_role_style: bool,
    /// An explicit size can follow the accessibility text multiplier without
    /// changing its visual hierarchy at the default scale.
    scales_with_theme: bool,
    /// The explicit size is already a final logical-pixel size and therefore
    /// cancels the renderer's accessibility multiplier.
    has_fixed_size: bool,
    explicit_base_size: f32,
    /// Sanitized Theme factor that produced any effective size in `style`.
    /// `measure` has no Theme parameter, so build/draw retain it here.
    observed_theme_text_scale: f32,
    /// Renderer scale used for the retained measurement and glyph scene.
    observed_renderer_text_scale: f32,
    heading_level: Option<u8>,
    /// Announce changes to this text without moving keyboard focus. Static
    /// copy leaves this unset; status and error text opt in explicitly.
    live: Option<crate::a11y::Politeness>,
    layout_engine: Option<Box<dyn TextLayoutEngine>>,
    engine_revision: u64,
    /// `None` (default) renders with the theme's `text_primary`.
    color: Option<vello::peniko::Color>,
    /// Resolve to the active theme's secondary ink.
    dimmed: bool,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Cached `Scene` for this widget's last `draw` output. Reused
    /// across frames via `Scene::append` so an unchanged Text
    /// widget contributes ~zero shape / emit work per frame.
    cached: Scene,
    /// The ink `cached` was drawn in, so a theme change redraws it.
    drawn_color: Option<vello::peniko::Color>,
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
            follows_role_style: false,
            scales_with_theme: false,
            has_fixed_size: false,
            explicit_base_size: 16.0,
            observed_theme_text_scale: 1.0,
            observed_renderer_text_scale: 1.0,
            heading_level: None,
            live: None,
            layout_engine: None,
            engine_revision: 0,
            color: None,
            dimmed: false,
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            permission_scope: None,
            cached: Scene::new(),
            drawn_color: None,
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

    /// Set a renderer-relative base size. The shared renderer applies its
    /// accessibility multiplier to this size during measurement and drawing.
    #[must_use]
    pub fn with_size(mut self, size: f32) -> Self {
        self.use_explicit_style();
        self.scales_with_theme = false;
        self.has_fixed_size = false;
        self.explicit_base_size = size;
        self.style.size = size;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Sets an explicit size that follows the active accessibility text scale.
    /// Use this for application hierarchy with a deliberate base size that
    /// still needs to respect the person's large-text preference. `Text`
    /// reconciles this resolved logical size with the renderer so the theme
    /// multiplier is applied exactly once even when their scales differ.
    #[must_use]
    pub fn with_scaled_size(mut self, size: f32) -> Self {
        self.use_explicit_style();
        self.scales_with_theme = true;
        self.has_fixed_size = false;
        self.explicit_base_size = size;
        self.style.size = size;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Apply a complete renderer-relative style in one operation. The shared
    /// renderer applies its accessibility multiplier to the size and letter
    /// spacing; measurement and painting use the same resulting metrics.
    #[must_use]
    pub fn with_style(mut self, style: TextStyle) -> Self {
        self.use_explicit_style();
        self.scales_with_theme = false;
        self.has_fixed_size = false;
        self.explicit_base_size = style.size;
        self.style = style;
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    /// Set an exact logical font size that does not follow either the theme or
    /// renderer accessibility multiplier.
    ///
    /// This opts out of a preceding semantic role's visual typography while
    /// retaining its accessibility semantics. Letter spacing remains
    /// renderer-relative, matching [`Self::with_size`] and [`Self::with_style`].
    #[must_use]
    pub fn with_fixed_size(mut self, size: f32) -> Self {
        self.use_explicit_style();
        self.scales_with_theme = false;
        self.has_fixed_size = true;
        self.explicit_base_size = size;
        self.style.size = size;
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
        self.follows_role_style = true;
        self.scales_with_theme = false;
        self.has_fixed_size = false;
        let style = Theme::default().tokens.typography.style(role);
        self.style = self.merged_semantic_style(&style);
        self.dirty = true;
        self.layout_dirty = true;
        self
    }

    fn use_explicit_style(&mut self) {
        self.follows_role_style = false;
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

    fn sanitized_theme_text_scale(theme: &Theme) -> f32 {
        if theme.text_scale.is_finite() {
            theme.text_scale.clamp(0.5, 4.0)
        } else {
            1.0
        }
    }

    fn renderer_input_style(&self, renderer: &TextRenderer) -> TextStyle {
        if self.follows_role_style || self.scales_with_theme || self.has_fixed_size {
            let mut style = renderer.renderer_input_style(&self.style);
            // Text's public modifiers have always defined pixel spacing as a
            // renderer-relative value, including fixed-size text. Preserve
            // that contract while sharing the resolved font-size boundary.
            style.letter_spacing = self.style.letter_spacing;
            style
        } else {
            self.style.clone()
        }
    }

    fn observe_renderer_text_scale(&mut self, renderer: &TextRenderer) {
        let scale = renderer.text_scale();
        if self.observed_renderer_text_scale.to_bits() != scale.to_bits() {
            self.observed_renderer_text_scale = scale;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    fn observe_theme_role(&mut self, theme: &Theme) {
        let text_scale = Self::sanitized_theme_text_scale(theme);
        if self.observed_theme_text_scale.to_bits() != text_scale.to_bits() {
            self.observed_theme_text_scale = text_scale;
            self.dirty = true;
            self.layout_dirty = true;
        }
        if !self.follows_role_style {
            if self.scales_with_theme {
                let size = self.explicit_base_size * text_scale;
                if (self.style.size - size).abs() > f32::EPSILON {
                    self.style.size = size;
                    self.dirty = true;
                    self.layout_dirty = true;
                }
            }
            return;
        }
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

    /// Override the screen-reader heading level while retaining the selected
    /// visual role. Levels are clamped to the platform-supported 1–6 range.
    /// This is useful for nested sections; `Title`/`Display` default to level
    /// 1 and `Heading` defaults to level 2.
    #[must_use]
    pub fn with_heading_level(mut self, level: u8) -> Self {
        self.heading_level = Some(level.clamp(1, 6));
        self
    }

    /// Mark this text as a live accessibility region.
    ///
    /// Use [`crate::a11y::Politeness::Assertive`] for feedback that must be
    /// heard before the person can proceed (for example, a rejected login),
    /// and `Polite` for non-blocking progress. This does not move focus or
    /// change visual styling.
    #[must_use]
    pub fn with_live(mut self, politeness: crate::a11y::Politeness) -> Self {
        self.live = Some(politeness);
        self
    }

    fn accessible_heading_level(&self) -> Option<u8> {
        self.heading_level.or(match self.semantic_role {
            Some(TextRole::Title | TextRole::Display) => Some(1),
            Some(TextRole::Heading) => Some(2),
            _ => None,
        })
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
        self.dimmed = false;
        self
    }

    /// Use the active theme's secondary text colour. This avoids freezing a
    /// dark-palette colour into a widget that may render in light or
    /// high-contrast mode.
    #[must_use]
    pub fn dimmed(mut self) -> Self {
        self.color = None;
        self.dimmed = true;
        self.dirty = true;
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
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::undo::UndoPoints
    );

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = crate::doc::document(state, path);
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
        self.observe_renderer_text_scale(text_renderer);
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
        let renderer_style = self.renderer_input_style(text_renderer);
        let (w, h) = if let Some(engine) = self.layout_engine.as_deref_mut() {
            engine.measure(text_renderer, &label, constraints, &renderer_style)
        } else {
            text_renderer.measure_style(&label, max_w, &renderer_style)
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
        self.observe_renderer_text_scale(text_renderer);
        let color = self.color.unwrap_or({
            if self.dimmed {
                theme.text_dim
            } else {
                theme.tokens.colors.text_primary
            }
        });
        if self.drawn_color != Some(color) {
            self.drawn_color = Some(color);
            self.dirty = true;
        }
        if self.dirty || self.cache_rect == vello::kurbo::Rect::ZERO {
            self.cached.reset();
            let label = self.resolved_label();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "label width in logical pixels is far below f32::MAX for any realistic UI element"
            )]
            let max_width = self.rect.width() as f32;
            let origin = Affine::translate((self.rect.x0, self.rect.y0));
            let constraints = TextLayoutConstraints {
                max_width: Some(max_width + 1.0),
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "text widget height is an ordinary logical-pixel value"
                )]
                max_height: Some(self.rect.height() as f32),
            };
            let renderer_style = self.renderer_input_style(text_renderer);
            if let Some(engine) = self.layout_engine.as_deref_mut() {
                engine.draw(
                    text_renderer,
                    &mut self.cached,
                    &label,
                    constraints,
                    &renderer_style,
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
                    &renderer_style,
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

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The FTL-resolved text *is* the accessible name. Empty text carries no
        // meaning, so it stays presentational (collapses out of the tree).
        let label = self.resolved_label();
        if label.is_empty() {
            return None;
        }
        let role = self
            .accessible_heading_level()
            .map_or(Role::StaticText, |level| Role::Heading { level });
        let mut node = AccessNode::new(role).with_name(label);
        if let Some(politeness) = self.live {
            node = node.with_live(politeness);
        }
        Some(node)
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

        if let Some(scope) = crate::permissions::capability::permission_scope(self)
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

        if let Some(key) = crate::presence::capability::presence_key(self)
            && let Some(peers) = presence.here.get(&key)
            && let Some(user) = peers.first()
        {
            presence.draw_peer_focus(scene, theme, user, self.rect);
        }
    }
}

impl crate::undo::UndoPoints for Text {
    fn needs_commit(&self) -> bool {
        false
    }
}

impl crate::presence::capability::PublishesPresence for Text {
    fn presence_key(&self) -> Option<PresenceKey> {
        if self.presence_path.is_empty() {
            None
        } else {
            Some(PresenceKey::Widget {
                path: self.presence_path.clone(),
            })
        }
    }
}

impl crate::permissions::capability::PermissionGated for Text {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_text_top_splits_measured_slack() {
        // Case `centered_text_top_splits_the_slack_evenly`.
        {
            // 18 px band, 15 px measured line: 1.5 px above and below.
            assert!((centered_text_top(100.0, 18.0, 15.0) - 101.5).abs() < f64::EPSILON);
            // Zero slack sits flush with the band.
            assert!((centered_text_top(7.0, 15.0, 15.0) - 7.0).abs() < f64::EPSILON);
            // A line taller than its band overhangs symmetrically instead of pinning to the top.
            assert!((centered_text_top(0.0, 10.0, 14.0) - (-2.0)).abs() < f64::EPSILON);
        }

        // Case `centered_text_top_uses_the_measured_line_not_the_font_size`.
        {
            // The defect this helper retires: centring on the 12 px font size
            // instead of the 15 px line the renderer actually lays out puts the
            // glyphs 1.5 px lower than the band's centre.
            let mut renderer = TextRenderer::new();
            let (_, measured) = renderer.measure("Home", 12.0, None);
            let measured = f64::from(measured);
            assert!(
                measured > 12.0,
                "cosmic-text line box exceeds the font size: {measured}"
            );
            let band = (0.0, 18.0);
            let correct = centered_text_top(band.0, band.1, measured);
            let drifted = band.0 + (band.1 - 12.0) / 2.0;
            let line_centre = correct + measured / 2.0;
            assert!(
                (line_centre - 9.0).abs() < 1e-9,
                "line centre {line_centre} != band centre 9"
            );
            assert!(
                drifted > correct,
                "font-size centring drifts downward ({drifted} vs {correct})"
            );
        }
    }

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

    fn assert_close(actual: f32, expected: f32, context: &str) {
        assert!(
            (actual - expected).abs() < 0.001,
            "{context}: expected {expected}, got {actual}",
        );
    }

    fn build_measure_draw(
        text: &mut Text,
        theme: &Theme,
        renderer: &mut TextRenderer,
    ) -> (taffy::Size<f32>, Scene) {
        let mut taffy = TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let _node = Widget::build_layout(text, &mut taffy, theme, &mut nodes);
        let measured = Widget::measure(
            text,
            taffy::Size {
                width: None,
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            renderer,
        );
        text.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 800.0, 200.0));
        let mut scene = Scene::new();
        Widget::draw(
            text,
            &mut scene,
            Affine::IDENTITY,
            renderer,
            theme,
            &crate::presence::PresenceCtx::empty(),
        );
        (measured, scene)
    }

    fn assert_scene_font_size(scene: &Scene, expected: f32, context: &str) {
        let runs = &scene.encoding().resources.glyph_runs;
        assert!(
            !runs.is_empty(),
            "{context}: real draw emitted no glyph runs"
        );
        for run in runs {
            assert_close(run.font_size, expected, context);
            assert!(!run.glyphs.is_empty(), "{context}: emitted an empty run");
        }
    }

    fn second_glyph_x(scene: &Scene) -> f32 {
        let resources = &scene.encoding().resources;
        let run = resources
            .glyph_runs
            .first()
            .expect("one controlled Latin run");
        assert!(run.glyphs.len() >= 2, "AA must emit two glyphs");
        resources.glyphs[run.glyphs.start.saturating_add(1)].x
    }

    struct DelegatingEngine {
        measured: std::rc::Rc<std::cell::RefCell<Vec<TextStyle>>>,
        drawn: std::rc::Rc<std::cell::RefCell<Vec<TextStyle>>>,
    }

    impl TextLayoutEngine for DelegatingEngine {
        fn measure(
            &mut self,
            renderer: &mut TextRenderer,
            text: &str,
            constraints: TextLayoutConstraints,
            style: &TextStyle,
        ) -> (f32, f32) {
            self.measured.borrow_mut().push(style.clone());
            renderer.measure_style(text, constraints.max_width, style)
        }

        fn draw(
            &mut self,
            renderer: &mut TextRenderer,
            scene: &mut Scene,
            text: &str,
            constraints: TextLayoutConstraints,
            style: &TextStyle,
            color: vello::peniko::Color,
            transform: Affine,
        ) {
            self.drawn.borrow_mut().push(style.clone());
            renderer.draw_style(scene, text, color, transform, constraints.max_width, style);
        }
    }

    #[test]
    fn pluggable_layout_engines_keep_lifecycle_and_size_ownership() {
        // Case `custom_layout_engine_keeps_text_widget_lifecycle`.
        {
            let measures = std::rc::Rc::new(std::cell::Cell::new(0));
            let draws = std::rc::Rc::new(std::cell::Cell::new(0));
            let revision = std::rc::Rc::new(std::cell::Cell::new(0));
            let mut text = Text::new("engine text").with_layout_engine(RecordingEngine {
                measures: std::rc::Rc::clone(&measures),
                draws: std::rc::Rc::clone(&draws),
                revision: std::rc::Rc::clone(&revision),
            });
            let mut renderer = crate::font_catalog::fixture_renderer();
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

        // Case `pluggable_engine_receives_base_font_size_and_delegates_real_runs`.
        {
            for (theme_scale, renderer_scale) in [(1.0, 2.0), (1.25, 1.5), (1.5, 1.25), (2.0, 1.0)]
            {
                let measured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let drawn = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let mut text = Text::new("Scale probe")
                    .with_role(TextRole::Body)
                    .with_letter_spacing(2.0)
                    .with_layout_engine(DelegatingEngine {
                        measured: std::rc::Rc::clone(&measured),
                        drawn: std::rc::Rc::clone(&drawn),
                    });
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(renderer_scale);
                let (size, scene) = build_measure_draw(&mut text, &theme, &mut renderer);
                assert_close(size.height, 14.0 * 1.4 * theme_scale, "engine measure");
                assert_scene_font_size(&scene, 14.0 * theme_scale, "engine draw");
                let measured_styles = measured.borrow();
                let drawn_styles = drawn.borrow();
                for style in measured_styles.iter().chain(drawn_styles.iter()) {
                    assert_close(
                        style.size,
                        14.0 * theme_scale / renderer_scale,
                        "engine renderer-input size",
                    );
                    assert_close(style.letter_spacing, 2.0, "engine letter spacing");
                }
                assert_eq!(measured_styles.len(), 1);
                assert_eq!(drawn_styles.len(), 1);
            }
        }

        // Case `pluggable_engine_preserves_base_and_fixed_size_ownership`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            for (mut text, expected_input, expected_draw) in [
                (
                    Text::new("Base engine")
                        .with_style(TextStyle::new(18.0))
                        .with_letter_spacing(2.0),
                    18.0,
                    27.0,
                ),
                (
                    Text::new("Fixed engine")
                        .with_fixed_size(19.0)
                        .with_letter_spacing(2.0),
                    19.0 / 1.5,
                    19.0,
                ),
            ] {
                let measured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let drawn = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                text = text.with_layout_engine(DelegatingEngine {
                    measured: std::rc::Rc::clone(&measured),
                    drawn: std::rc::Rc::clone(&drawn),
                });
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(1.5);
                let (_, scene) = build_measure_draw(&mut text, &theme, &mut renderer);
                assert_scene_font_size(&scene, expected_draw, "engine owned draw");
                let measured_styles = measured.borrow();
                let drawn_styles = drawn.borrow();
                assert_eq!(measured_styles.len(), 1);
                assert_eq!(drawn_styles.len(), 1);
                for style in measured_styles.iter().chain(drawn_styles.iter()) {
                    assert_close(style.size, expected_input, "engine owned input");
                    assert_close(style.letter_spacing, 2.0, "engine owned spacing");
                }
            }
        }
    }

    #[test]
    fn font_size_ownership_is_resolved_once_by_its_owner() {
        // Case `theme_baked_sizes_reach_default_widget_draw_exactly_once`.
        {
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut role_renderer = crate::font_catalog::fixture_renderer();
                role_renderer.set_text_scale(scale);
                let mut role = Text::new("Scale probe").with_role(TextRole::Body);
                let (role_measure, role_scene) =
                    build_measure_draw(&mut role, &theme, &mut role_renderer);
                assert_close(role.text_style().size, 14.0 * scale, "effective role style");
                assert_close(role_measure.height, 14.0 * 1.4 * scale, "role measure");
                assert_scene_font_size(&role_scene, 14.0 * scale, "role draw");

                let mut scaled_renderer = crate::font_catalog::fixture_renderer();
                scaled_renderer.set_text_scale(scale);
                let mut scaled = Text::new("Scale probe").with_scaled_size(16.0);
                let (scaled_measure, scaled_scene) =
                    build_measure_draw(&mut scaled, &theme, &mut scaled_renderer);
                assert_close(
                    scaled.text_style().size,
                    16.0 * scale,
                    "effective explicit style",
                );
                assert_close(scaled_measure.height, 16.0 * 1.25 * scale, "scaled measure");
                assert_scene_font_size(&scaled_scene, 16.0 * scale, "scaled draw");
            }
        }

        // Case `theme_resolved_sizes_are_independent_of_renderer_scale`.
        {
            for (theme_scale, renderer_scale) in [(1.0, 2.0), (1.25, 1.5), (1.5, 1.25), (2.0, 1.0)]
            {
                let theme = Theme::dark().with_text_scale(theme_scale);

                let mut role_renderer = crate::font_catalog::fixture_renderer();
                role_renderer.set_text_scale(renderer_scale);
                let mut role = Text::new("Role mismatch").with_role(TextRole::Body);
                let (role_measure, role_scene) =
                    build_measure_draw(&mut role, &theme, &mut role_renderer);
                assert_close(
                    role_measure.height,
                    14.0 * 1.4 * theme_scale,
                    "role mismatch measure",
                );
                assert_scene_font_size(&role_scene, 14.0 * theme_scale, "role mismatch draw");

                let mut scaled_renderer = crate::font_catalog::fixture_renderer();
                scaled_renderer.set_text_scale(renderer_scale);
                let mut scaled = Text::new("Explicit mismatch").with_scaled_size(14.0);
                let (scaled_measure, scaled_scene) =
                    build_measure_draw(&mut scaled, &theme, &mut scaled_renderer);
                assert_close(
                    scaled_measure.height,
                    14.0 * 1.25 * theme_scale,
                    "explicit mismatch measure",
                );
                assert_scene_font_size(&scaled_scene, 14.0 * theme_scale, "explicit mismatch draw");
            }
        }

        // Case `fixed_size_is_independent_of_theme_and_renderer_scales`.
        {
            for (theme_scale, renderer_scale) in [(1.0, 2.0), (1.25, 1.5), (1.5, 1.25), (2.0, 1.0)]
            {
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(renderer_scale);
                let mut fixed = Text::new("Fixed logical size").with_fixed_size(19.0);
                let (measured, scene) = build_measure_draw(&mut fixed, &theme, &mut renderer);
                assert_close(measured.height, 19.0 * 1.25, "fixed measure");
                assert_scene_font_size(&scene, 19.0, "fixed draw");
            }
        }

        // Case `later_size_modifier_takes_ownership`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let cases = [
                (
                    Text::new("role then fixed")
                        .with_role(TextRole::Body)
                        .with_fixed_size(19.0),
                    19.0,
                ),
                (
                    Text::new("fixed then role")
                        .with_fixed_size(19.0)
                        .with_role(TextRole::Body),
                    28.0,
                ),
                (
                    Text::new("fixed then scaled")
                        .with_fixed_size(19.0)
                        .with_scaled_size(10.0),
                    20.0,
                ),
                (
                    Text::new("scaled then fixed")
                        .with_scaled_size(10.0)
                        .with_fixed_size(19.0),
                    19.0,
                ),
                (
                    Text::new("fixed then base")
                        .with_fixed_size(19.0)
                        .with_size(10.0),
                    12.5,
                ),
                (
                    Text::new("base then fixed")
                        .with_size(10.0)
                        .with_fixed_size(19.0),
                    19.0,
                ),
                (
                    Text::new("fixed then style")
                        .with_fixed_size(19.0)
                        .with_style(TextStyle::new(11.0)),
                    13.75,
                ),
                (
                    Text::new("style then fixed")
                        .with_style(TextStyle::new(11.0))
                        .with_fixed_size(19.0),
                    19.0,
                ),
            ];

            for (mut text, expected) in cases {
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(1.25);
                let (_, scene) = build_measure_draw(&mut text, &theme, &mut renderer);
                assert_scene_font_size(&scene, expected, "later size modifier");
            }
        }

        // Case `fixed_font_size_keeps_letter_spacing_renderer_relative`.
        {
            let theme = Theme::dark().with_text_scale(1.5);
            let mut plain_renderer = crate::font_catalog::fixture_renderer();
            plain_renderer.set_text_scale(2.0);
            let mut plain = Text::new("AA").with_fixed_size(19.0);
            let (_, plain_scene) = build_measure_draw(&mut plain, &theme, &mut plain_renderer);

            let mut spaced_renderer = crate::font_catalog::fixture_renderer();
            spaced_renderer.set_text_scale(2.0);
            let mut spaced = Text::new("AA")
                .with_fixed_size(19.0)
                .with_letter_spacing(2.0);
            let (_, spaced_scene) = build_measure_draw(&mut spaced, &theme, &mut spaced_renderer);

            assert_scene_font_size(&plain_scene, 19.0, "fixed plain font size");
            assert_scene_font_size(&spaced_scene, 19.0, "fixed spaced font size");
            assert_close(
                second_glyph_x(&spaced_scene) - second_glyph_x(&plain_scene),
                4.0,
                "fixed renderer-owned letter spacing",
            );
        }

        // Case `base_sizes_and_letter_spacing_remain_renderer_owned`.
        {
            for (theme_scale, renderer_scale) in [(1.0, 2.0), (1.25, 1.5), (1.5, 1.25), (2.0, 1.0)]
            {
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut plain_renderer = crate::font_catalog::fixture_renderer();
                plain_renderer.set_text_scale(renderer_scale);
                let mut plain = Text::new("AA").with_size(18.0);
                let (plain_measure, plain_scene) =
                    build_measure_draw(&mut plain, &theme, &mut plain_renderer);

                let mut spaced_renderer = crate::font_catalog::fixture_renderer();
                spaced_renderer.set_text_scale(renderer_scale);
                let mut spaced = Text::new("AA").with_size(18.0).with_letter_spacing(2.0);
                let (spaced_measure, spaced_scene) =
                    build_measure_draw(&mut spaced, &theme, &mut spaced_renderer);
                assert_scene_font_size(&plain_scene, 18.0 * renderer_scale, "base draw");
                assert_scene_font_size(&spaced_scene, 18.0 * renderer_scale, "spaced base draw");
                assert_close(
                    plain_measure.height,
                    18.0 * 1.25 * renderer_scale,
                    "base measure",
                );
                assert_close(
                    spaced_measure.height,
                    plain_measure.height,
                    "spacing height control",
                );
                assert_close(
                    second_glyph_x(&spaced_scene) - second_glyph_x(&plain_scene),
                    2.0 * renderer_scale,
                    "letter spacing is scaled once",
                );
            }
        }

        // Case `explicit_scaled_size_follows_the_accessibility_multiplier`.
        {
            let mut text = Text::new("Readable").with_scaled_size(14.0);
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            Widget::build_layout(&mut text, &mut taffy, &Theme::dark(), &mut nodes);
            assert_eq!(text.text_style().size, 14.0);

            let scaled_theme = Theme::dark().with_text_scale(1.5);
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            Widget::build_layout(&mut text, &mut taffy, &scaled_theme, &mut nodes);
            assert_eq!(text.text_style().size, 21.0);
        }
    }

    #[test]
    fn text_caches_invalidate_only_when_inputs_change() {
        // Case `observed_theme_scale_invalidates_role_and_base_caches`.
        {
            let initial = Theme::dark();
            let scaled = Theme::dark().with_text_scale(2.0);
            let mut role = Text::new("Scale probe")
                .with_role(TextRole::Body)
                .with_letter_spacing(2.0);
            let mut role_renderer = crate::font_catalog::fixture_renderer();
            let _ = build_measure_draw(&mut role, &initial, &mut role_renderer);
            Widget::mark_drawn(&mut role);
            role_renderer.set_text_scale(2.0);
            let (role_measure, role_scene) =
                build_measure_draw(&mut role, &scaled, &mut role_renderer);
            assert!(Widget::needs_redraw(&role));
            assert!(Widget::is_layout_dirty(&role));
            assert_close(role.text_style().size, 28.0, "public effective style");
            assert_close(role.text_style().letter_spacing, 2.0, "modifier preserved");
            assert_close(role_measure.height, 28.0 * 1.4, "role remeasure");
            assert_scene_font_size(&role_scene, 28.0, "role cache redraw");

            let mut base = Text::new("Base cache").with_size(18.0);
            let mut base_renderer = crate::font_catalog::fixture_renderer();
            let _ = build_measure_draw(&mut base, &initial, &mut base_renderer);
            Widget::mark_drawn(&mut base);
            base_renderer.set_text_scale(2.0);
            let (base_measure, base_scene) =
                build_measure_draw(&mut base, &scaled, &mut base_renderer);
            assert!(Widget::needs_redraw(&base));
            assert!(Widget::is_layout_dirty(&base));
            assert_close(base.text_style().size, 18.0, "public base style");
            assert_close(base_measure.height, 45.0, "base remeasure");
            assert_scene_font_size(&base_scene, 36.0, "base cache redraw");
        }

        // Case `renderer_scale_change_invalidates_a_retained_base_size_cache`.
        {
            let theme = Theme::dark();
            let mut renderer = crate::font_catalog::fixture_renderer();
            let mut text = Text::new("Retained renderer scale").with_size(18.0);
            let (_, initial_scene) = build_measure_draw(&mut text, &theme, &mut renderer);
            assert_scene_font_size(&initial_scene, 18.0, "initial base draw");
            Widget::mark_drawn(&mut text);

            renderer.set_text_scale(2.0);
            let mut scaled_scene = Scene::new();
            Widget::draw(
                &mut text,
                &mut scaled_scene,
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &crate::presence::PresenceCtx::empty(),
            );
            assert!(Widget::needs_redraw(&text));
            assert!(Widget::is_layout_dirty(&text));
            assert_scene_font_size(&scaled_scene, 36.0, "retained base redraw");

            let scaled_measure = Widget::measure(
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
            assert_close(scaled_measure.height, 45.0, "retained base remeasure");
        }

        // Case `local_state_binding_invalidates_text_only_after_a_change`.
        {
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

        // Case `dynamic_text_compares_output_before_invalidating`.
        {
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

        // Case `document_text_keeps_multiple_views_in_sync_through_undo`.
        {
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
            let mut detail =
                Text::from_document(|doc, root| format!("Editing: {}", title(doc, root)));
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
    }

    #[test]
    fn typography_follows_semantic_roles_unless_explicitly_styled() {
        // Case `complete_style_is_retained_by_the_widget`.
        {
            let style = TextStyle::new(18.0)
                .with_line_height(1.6)
                .with_weight(TextWeight::Semibold)
                .with_italic(true)
                .with_family(FontChoice::Serif);
            let text = Text::new("Reading").with_style(style.clone());
            assert_eq!(text.text_style(), &style);
        }

        // Case `semantic_role_uses_active_theme_typography`.
        {
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

        // Case `semantic_role_follows_theme_changes_without_rebuilding_widget`.
        {
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

        // Case `explicit_typography_modifier_opts_out_of_semantic_role`.
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = Text::new("Fixed")
                .with_role(TextRole::Title)
                .with_size(19.0);
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            Widget::build_layout(&mut text, &mut taffy, &theme, &mut nodes);

            assert_eq!(text.text_style().size, 19.0);
        }

        // Case `font_family_override_label_resolves_to_named_family`.
        // A label with a font-family override resolves to that family: after
        // it draws once, re-shaping the same string in the SAME named family
        // is a shape-cache hit, while the sans default is a distinct miss —
        // proving the override reaches the family-aware path.
        {
            use crate::presence::PresenceCtx;
            let mut tr = crate::font_catalog::fixture_renderer();
            let fam = String::from("Adwaita Sans");
            assert!(
                tr.system_families().iter().any(|family| family == &fam),
                "the controlled fixture ships Adwaita Sans",
            );

            let theme = Theme::dark();
            let presence = PresenceCtx::empty();

            let mut label = Text::new("Reading").with_font_family(FontChoice::Named(fam.clone()));
            // The label draws with a max_width of `rect.width() + 1.0`.
            label.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 400.0, 40.0));
            let mut scene = Scene::new();
            label.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

            // Same string, size (16 default), wrap width (401.0) and family → a hit.
            let _ = tr.take_shape_stats();
            tr.draw_style(
                &mut scene,
                "Reading",
                theme.tokens.colors.text_primary,
                Affine::IDENTITY,
                Some(401.0),
                &TextStyle::new(16.0).with_family(FontChoice::Named(fam)),
            );
            let (hits, _m, _ns) = tr.take_shape_stats();
            assert_eq!(hits, 1, "the label shaped its text in the named family");

            // Sans of the same string is a separate shape entry.
            tr.draw_style(
                &mut scene,
                "Reading",
                theme.tokens.colors.text_primary,
                Affine::IDENTITY,
                Some(401.0),
                &TextStyle::new(16.0).with_family(FontChoice::SansSerif),
            );
            let (_h, misses, _ns) = tr.take_shape_stats();
            assert_eq!(misses, 1, "sans is a separate shape from the named family");
        }
    }

    use crate::a11y::Role;

    #[test]
    fn accessibility_exposes_names_headings_live_text_and_hides_empty_text() {
        // Case `accessibility_reports_static_text_with_label_as_name`.
        {
            let t = Text::new("Hello world");
            let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
            assert_eq!(node.role, Role::StaticText);
            assert_eq!(node.name.as_deref(), Some("Hello world"));
            assert_eq!(node.value, None);
            assert!(!node.state.focusable, "static text is not focusable");
        }

        // Case `semantic_title_and_heading_roles_create_navigable_headings`.
        {
            let title = Widget::accessibility(&Text::new("Projects").with_role(TextRole::Title))
                .expect("title is accessible");
            let heading = Widget::accessibility(&Text::new("Recent").with_role(TextRole::Heading))
                .expect("heading is accessible");
            assert_eq!(title.role, Role::Heading { level: 1 });
            assert_eq!(heading.role, Role::Heading { level: 2 });
        }

        // Case `custom_heading_style_keeps_semantics_and_level_can_be_overridden`.
        {
            let text = Text::new("Details")
                .with_role(TextRole::Heading)
                .with_size(19.0)
                .with_heading_level(3);
            let node = Widget::accessibility(&text).expect("heading is accessible");
            assert_eq!(node.role, Role::Heading { level: 3 });
            assert_eq!(text.text_style().size, 19.0);
        }

        // Case `accessibility_empty_text_is_presentational`.
        {
            let t = Text::new("");
            assert!(
                Widget::accessibility(&t).is_none(),
                "empty text collapses out of the a11y tree",
            );
        }

        // Case `accessibility_exposes_opt_in_live_text_without_changing_its_role`.
        {
            use crate::a11y::Politeness;

            let t = Text::new("Incorrect password").with_live(Politeness::Assertive);
            let node = Widget::accessibility(&t).expect("live status remains accessible text");
            assert_eq!(node.role, Role::StaticText);
            assert_eq!(node.name.as_deref(), Some("Incorrect password"));
            assert_eq!(node.live, Some(Politeness::Assertive));
        }
    }

    use crate::i18n::I18nManager;

    // The i18n manager is a thread-local, so these tests are isolated per test
    // thread; each re-inits a fresh bundle and asserts only on its own keys.

    #[test]
    fn fluent_text_interpolates_degrades_and_follows_locale() {
        // Case `with_arg_interpolates_fluent_variable`.
        {
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

            let t = Text::new("counter").with_arg("count", 5);
            let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
            // The placeholder must be replaced, not echoed literally.
            assert_eq!(node.name.as_deref(), Some("Count is 5"));
            assert!(!node.name.unwrap().contains("$count"));
        }

        // Case `with_args_accepts_strings_and_numbers`.
        {
            I18nManager::init("en-US");
            I18nManager::add_resource(
                "en-US",
                "greeting = Hi { $name }, you have { $count } messages".to_string(),
            );

            let t = Text::with_args("greeting", &[("name", "Ada".into()), ("count", 3.into())]);
            let node = Widget::accessibility(&t).expect("resolved");
            assert_eq!(node.name.as_deref(), Some("Hi Ada, you have 3 messages"));
        }

        // Case `missing_arg_degrades_without_panic`.
        {
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

            // No `count` supplied — must not panic; the formatter renders a
            // best-effort placeholder rather than crashing.
            let t = Text::new("counter");
            let node = Widget::accessibility(&t).expect("resolved");
            let name = node.name.unwrap();
            assert!(name.contains("Count is"), "got {name:?}");
        }

        // Case `unknown_key_falls_back_to_key`.
        {
            I18nManager::init("en-US");
            // No resource for this key at all.
            let t = Text::new("totally-unknown-key").with_arg("x", 1);
            let node = Widget::accessibility(&t).expect("resolved");
            assert_eq!(node.name.as_deref(), Some("totally-unknown-key"));
        }

        // Case `locale_switch_remeasures_and_repaints_retained_text_and_button`.
        {
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
                let mut driver = crate::headless::Driver::new_with_controlled_inputs(
                    crate::State::new(),
                    Box::new(root),
                    (700, 300),
                    Theme::dark(),
                    crate::font_catalog::fixture_renderer(),
                    crate::permissions::UserId("text-widget-locale-fixture".into()),
                );

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
            locale_switch_remeasures_and_repaints_retained_text_and_button();
        }
    }
}
