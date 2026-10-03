use crate::i18n::ArgValue;
use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::{
    FontChoice, HangingPunctuation, NumericStyle, OpenTypeTag, OpticalSizing, ParagraphAlign,
    TextKind, TextLayoutConstraints, TextLayoutEngine, TextRenderer, TextScaling, TextStyle,
    TextWeight, TextWrapMode,
};
use crate::theme::Theme;
use crate::tokens::{TextRole, Typography};
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
/// where `text` in `style` lands when its *measured* line is centred in the
/// band `band_y0 ..= band_y0 + band_height`. The expectation is a reference
/// `draw_style` at the computed top rather than an assumed baseline offset,
/// so the check follows the renderer's own line metrics.
#[cfg(test)]
pub(crate) fn assert_glyph_run_centred_in_band(
    scene: &Scene,
    run_index: usize,
    text: &str,
    style: &TextStyle,
    band_y0: f64,
    band_height: f64,
    renderer: &mut TextRenderer,
    color: vello::peniko::Color,
) {
    let (_, measured_h) = renderer.measure_style(text, None, style);
    let measured_h = f64::from(measured_h);
    assert!(
        measured_h > f64::from(style.size),
        "{text:?}: the measured line ({measured_h}) must exceed the font size ({}); \
         otherwise font-size centring could not drift",
        style.size
    );
    let expected_top = centered_text_top(band_y0, band_height, measured_h);
    let band_centre = band_y0 + band_height / 2.0;
    assert!(
        (expected_top + measured_h / 2.0 - band_centre).abs() < 1e-9,
        "{text:?}: line centre must be the band centre {band_centre}"
    );
    let mut reference = Scene::new();
    renderer.draw_style(
        &mut reference,
        text,
        color,
        Affine::translate((0.0, expected_top)),
        None,
        style,
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

/// What a [`Text`] layers on its role. Every field is an override; an unset
/// one leaves the role's own value.
#[derive(Clone, Debug)]
struct TextModifiers {
    weight: Option<TextWeight>,
    strong: bool,
    italic: Option<bool>,
    family: Option<FontChoice>,
    line_height: Option<f32>,
    kind: Option<TextKind>,
    content_size: Option<f32>,
    /// Carries the OpenType features, variations, paragraph and
    /// hyphenation choices; its size, weight and family are unused.
    layered: TextStyle,
}

impl Default for TextModifiers {
    fn default() -> Self {
        Self {
            weight: None,
            strong: false,
            italic: None,
            family: None,
            line_height: None,
            kind: None,
            content_size: None,
            layered: TextStyle::new(0.0),
        }
    }
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
    /// What this text is. It drives the typography and the accessible
    /// heading level.
    role: TextRole,
    /// Emphasis and typographic choices layered on the role.
    modifiers: TextModifiers,
    /// The role resolved against the last theme seen, with the modifiers
    /// applied. `measure` has no theme parameter, so build/draw keep it here.
    style: TextStyle,
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
            role: TextRole::Body,
            modifiers: TextModifiers::default(),
            style: crate::tokens::typography::stock().style(TextRole::Body),
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

    /// Replace measurement and painting while retaining Text's localization,
    /// accessibility, Taffy lifecycle, caching, and inspection behavior. The
    /// engine receives the resolved role style.
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

    /// Say what this text is. The role picks face, size, weight and leading
    /// from the active theme and follows its text and UI scale when the
    /// retained widget is laid out again. Text without a role is
    /// [`TextRole::Body`]. Modifiers such as [`Self::strong`] layer on top of
    /// the role in any order.
    #[must_use]
    pub fn with_role(mut self, role: TextRole) -> Self {
        self.role = role;
        self.restyle_default();
        self
    }

    /// The role this text was given.
    #[must_use]
    pub fn role(&self) -> TextRole {
        self.role
    }

    /// Emphasis inside the role: the next emphasis weight (regular and
    /// medium become semibold, semibold becomes bold).
    #[must_use]
    pub fn strong(self) -> Self {
        self.modify(|m| m.strong = true)
    }

    /// Tabular lining figures, for numbers in a column or a changing readout.
    #[must_use]
    pub fn numeric(self) -> Self {
        self.with_kind(TextKind::Numeric)
    }

    /// Say what the text is ([`TextKind`]): the toolkit picks its OpenType
    /// features from that against the face it is set in. `Code` also sets it
    /// in the theme's mono face.
    #[must_use]
    pub fn with_kind(self, kind: TextKind) -> Self {
        self.modify(|m| m.kind = Some(kind))
    }

    /// Document-space text at exactly `px` logical pixels: a zoomed canvas,
    /// a page preview, a terminal grid. It keeps the role's face, weight and
    /// leading but never grows with the text scale; the content's own zoom
    /// scales it. Chrome never uses this.
    #[must_use]
    pub fn with_content_size(self, px: f32) -> Self {
        self.modify(|m| m.content_size = Some(px.max(0.1)))
    }

    fn restyle_default(&mut self) {
        let style = self.resolve(crate::tokens::typography::stock());
        self.set_style(style);
    }

    /// The role resolved against `typography` with every modifier applied.
    fn resolve(&self, typography: &Typography) -> TextStyle {
        let modifiers = &self.modifiers;
        let mut style = typography.style(self.role);
        if let Some(px) = modifiers.content_size {
            style.size = px;
            style.scaling = TextScaling::Content;
        }
        if let Some(weight) = modifiers.weight {
            style.weight = weight;
        }
        if modifiers.strong {
            style = style.strong();
        }
        if let Some(italic) = modifiers.italic {
            style.italic = italic;
        }
        if let Some(family) = &modifiers.family {
            style.family.clone_from(family);
        }
        if let Some(line_height) = modifiers.line_height {
            style.line_height = line_height;
        }
        let layered = &modifiers.layered;
        style.hyphenation = layered.hyphenation.clone();
        style.font_features.clone_from(&layered.font_features);
        style.font_variations.clone_from(&layered.font_variations);
        style.optical_sizing = layered.optical_sizing;
        style.align = layered.align;
        style.wrap_mode = layered.wrap_mode;
        style.hanging_punctuation = layered.hanging_punctuation;
        style.fragmentation = layered.fragmentation;
        if let Some(kind) = modifiers.kind {
            style.kind = kind;
            if kind == TextKind::Code && modifiers.family.is_none() {
                style.family = FontChoice::Named(typography.mono_family.clone());
            }
        }
        style
    }

    fn set_style(&mut self, style: TextStyle) {
        if self.style != style {
            self.style = style;
            self.dirty = true;
            self.layout_dirty = true;
        }
    }

    fn observe_theme(&mut self, theme: &Theme) {
        let style = self.resolve(&theme.tokens.typography);
        self.set_style(style);
    }

    /// Change one modifier and re-resolve against the stock theme until a
    /// real theme is seen.
    fn modify(mut self, change: impl FnOnce(&mut TextModifiers)) -> Self {
        change(&mut self.modifiers);
        self.restyle_default();
        self
    }

    /// Override the screen-reader heading level while retaining the selected
    /// visual role. Levels are clamped to the platform-supported 1–6 range.
    /// Without an override the role decides ([`TextRole::heading_level`]).
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
        self.heading_level.or(self.role.heading_level())
    }

    /// Override the role's leading (a line-height multiplier).
    #[must_use]
    pub fn with_line_height(self, multiplier: f32) -> Self {
        self.modify(|m| m.line_height = Some(multiplier.max(0.1)))
    }

    /// Enable TeX/Liang automatic hyphenation for this text block using a
    /// BCP-47 language tag such as `"en-US"` or `"de"`.
    #[must_use]
    pub fn with_auto_hyphenation(self, language: impl Into<String>) -> Self {
        let language = language.into();
        self.modify(|m| m.layered = m.layered.clone().with_auto_hyphenation(language))
    }

    /// Use an application-supplied discretionary-break algorithm while
    /// retaining the standard Text widget lifecycle and accessibility.
    #[must_use]
    pub fn with_hyphenator(
        self,
        hyphenator: std::sync::Arc<dyn crate::text_render::Hyphenator>,
    ) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_hyphenator(hyphenator))
    }

    /// Override the role's weight. Prefer [`Self::strong`] for emphasis.
    #[must_use]
    pub fn with_weight(self, weight: TextWeight) -> Self {
        self.modify(|m| m.weight = Some(weight))
    }

    #[must_use]
    pub fn with_italic(self, italic: bool) -> Self {
        self.modify(|m| m.italic = Some(italic))
    }

    /// Render this label in an explicit [`FontChoice`] instead of the role's
    /// family — a font picker previewing a face in itself. Measurement and
    /// draw shape use the same family.
    #[must_use]
    pub fn with_font_family(self, family: FontChoice) -> Self {
        self.modify(|m| m.family = Some(family))
    }

    #[must_use]
    pub fn with_font_feature(self, tag: OpenTypeTag, value: u32) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_font_feature(tag, value))
    }

    #[must_use]
    pub fn with_kerning(self, enabled: bool) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_kerning(enabled))
    }

    #[must_use]
    pub fn with_discretionary_ligatures(self, enabled: bool) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_discretionary_ligatures(enabled))
    }

    #[must_use]
    pub fn with_numeric_style(self, numeric_style: NumericStyle) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_numeric_style(numeric_style))
    }

    #[must_use]
    pub fn with_font_variation(self, tag: OpenTypeTag, value: f32) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_font_variation(tag, value))
    }

    #[must_use]
    pub fn with_optical_sizing(self, optical_sizing: OpticalSizing) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_optical_sizing(optical_sizing))
    }

    #[must_use]
    pub fn with_align(self, align: ParagraphAlign) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_align(align))
    }

    #[must_use]
    pub fn with_balanced_wrap(self) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_wrap_mode(TextWrapMode::Balance))
    }

    #[must_use]
    pub fn with_hanging_punctuation(self, hanging: HangingPunctuation) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_hanging_punctuation(hanging))
    }

    #[must_use]
    pub fn with_fragmentation(self, orphans: u8, widows: u8) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_fragmentation(orphans, widows))
    }

    /// Mutate the font-family override after construction. `None` restores
    /// the role's family.
    pub fn set_font_family(&mut self, family: Option<FontChoice>) {
        self.modifiers.family = family;
        self.restyle_default();
    }

    /// The resolved style measurement and painting use: the role in the last
    /// theme seen, with every modifier applied.
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
        self.observe_theme(theme);
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
        self.observe_theme(theme);
        self.observe_i18n_revision();
        self.observe_engine_revision();
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

        // Case `pluggable_engine_receives_the_resolved_role_and_delegates_real_runs`.
        {
            for (theme_scale, renderer_scale) in [(1.0, 2.0), (1.25, 1.5), (1.5, 1.25), (2.0, 1.0)]
            {
                let measured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let drawn = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let mut text = Text::new("Scale probe")
                    .with_role(TextRole::Body)
                    .with_layout_engine(DelegatingEngine {
                        measured: std::rc::Rc::clone(&measured),
                        drawn: std::rc::Rc::clone(&drawn),
                    });
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(renderer_scale);
                let (size, scene) = build_measure_draw(&mut text, &theme, &mut renderer);
                assert_close(size.height, 14.0 * 1.45 * theme_scale, "engine measure");
                assert_scene_font_size(&scene, 14.0 * theme_scale, "engine draw");
                let measured_styles = measured.borrow();
                let drawn_styles = drawn.borrow();
                for style in measured_styles.iter().chain(drawn_styles.iter()) {
                    assert_close(style.size, 14.0 * theme_scale, "engine resolved size");
                    assert_eq!(style.scaling, TextScaling::Resolved);
                }
                assert_eq!(measured_styles.len(), 1);
                assert_eq!(drawn_styles.len(), 1);
            }
        }
    }

    #[test]
    fn roles_are_scaled_exactly_once_whatever_the_renderer_scale() {
        // Case `theme_roles_reach_draw_exactly_once`.
        {
            for (theme_scale, renderer_scale) in [
                (1.0, 1.0),
                (1.5, 1.5),
                (2.0, 2.0),
                (1.0, 2.0),
                (1.25, 1.5),
                (2.0, 1.0),
            ] {
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(renderer_scale);
                let mut role = Text::new("Scale probe").with_role(TextRole::Body);
                let (measure, scene) = build_measure_draw(&mut role, &theme, &mut renderer);
                assert_close(role.text_style().size, 14.0 * theme_scale, "role style");
                assert_close(measure.height, 14.0 * 1.45 * theme_scale, "role measure");
                assert_scene_font_size(&scene, 14.0 * theme_scale, "role draw");
            }
        }

        // Case `text_without_a_role_is_body`.
        {
            let theme = Theme::dark().with_ui_scale(1.25).with_text_scale(1.5);
            let mut renderer = crate::font_catalog::fixture_renderer();
            renderer.set_text_scale(1.5);
            let mut plain = Text::new("Plain");
            let _ = build_measure_draw(&mut plain, &theme, &mut renderer);
            assert_eq!(plain.role(), TextRole::Body);
            assert_eq!(plain.text_style(), &theme.text(TextRole::Body));
        }

        // Case `text_role_matches_theme_text_at_every_scale`.
        {
            let theme = Theme::dark().with_ui_scale(1.25).with_text_scale(1.5);
            for role in TextRole::ALL {
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(1.5);
                let mut text = Text::new("Role").with_role(role);
                let (_, scene) = build_measure_draw(&mut text, &theme, &mut renderer);
                assert_eq!(text.text_style(), &theme.text(role), "{role:?}");
                assert_scene_font_size(&scene, theme.text(role).size, "role drawn once");
            }
        }

        // Case `modifiers_keep_the_role_size`.
        {
            let theme = Theme::dark().with_ui_scale(1.25).with_text_scale(1.5);
            let mut renderer = crate::font_catalog::fixture_renderer();
            for mut text in [
                Text::new("Strong").with_role(TextRole::Label).strong(),
                Text::new("Weight")
                    .with_weight(TextWeight::Bold)
                    .with_role(TextRole::Label),
                Text::new("Italic")
                    .with_role(TextRole::Label)
                    .with_italic(true),
                Text::new("12.50").with_role(TextRole::Label).numeric(),
            ] {
                let _ = build_measure_draw(&mut text, &theme, &mut renderer);
                assert_eq!(
                    text.text_style().size,
                    theme.text(TextRole::Label).size,
                    "a modifier never drops the role's resolved size"
                );
            }
            let strong = Text::new("Strong").with_role(TextRole::Label).strong();
            assert_eq!(strong.text_style().weight, TextWeight::Semibold);
            let numeric = Text::new("12.50").numeric();
            assert_eq!(numeric.text_style().kind, TextKind::Numeric);
            assert!(numeric.text_style().font_features.is_empty());
            let code = Text::new("a1b2").with_kind(TextKind::Code);
            assert_eq!(
                code.text_style().family,
                FontChoice::Named(crate::text_render::UI_MONOSPACE_FAMILY.into())
            );
        }

        // Case `content_size_ignores_every_accessibility_scale`.
        {
            for (theme_scale, renderer_scale) in [(1.0, 2.0), (1.25, 1.5), (2.0, 1.0)] {
                let theme = Theme::dark().with_text_scale(theme_scale);
                let mut renderer = crate::font_catalog::fixture_renderer();
                renderer.set_text_scale(renderer_scale);
                let mut content = Text::new("Page text")
                    .with_role(TextRole::Reading)
                    .with_content_size(19.0);
                let (measured, scene) = build_measure_draw(&mut content, &theme, &mut renderer);
                assert_close(measured.height, 19.0 * 1.6, "content measure");
                assert_scene_font_size(&scene, 19.0, "content draw");
                assert_eq!(content.text_style().scaling, TextScaling::Content);
            }
        }
    }

    #[test]
    fn text_caches_invalidate_only_when_inputs_change() {
        // Case `observed_theme_scale_invalidates_the_role_cache`.
        {
            let initial = Theme::dark();
            let scaled = Theme::dark().with_text_scale(2.0);
            let mut role = Text::new("Scale probe").with_role(TextRole::Body);
            let mut role_renderer = crate::font_catalog::fixture_renderer();
            let _ = build_measure_draw(&mut role, &initial, &mut role_renderer);
            Widget::mark_drawn(&mut role);
            role_renderer.set_text_scale(2.0);
            let (role_measure, role_scene) =
                build_measure_draw(&mut role, &scaled, &mut role_renderer);
            assert!(Widget::needs_redraw(&role));
            assert!(Widget::is_layout_dirty(&role));
            assert_close(role.text_style().size, 28.0, "public effective style");
            assert_close(role_measure.height, 28.0 * 1.45, "role remeasure");
            assert_scene_font_size(&role_scene, 28.0, "role cache redraw");
        }

        // Case `an_unchanged_theme_keeps_the_cache`.
        {
            let theme = Theme::dark();
            let mut renderer = crate::font_catalog::fixture_renderer();
            let mut text = Text::new("Retained").with_role(TextRole::Caption);
            let _ = build_measure_draw(&mut text, &theme, &mut renderer);
            Widget::mark_drawn(&mut text);
            let _ = build_measure_draw(&mut text, &theme, &mut renderer);
            assert!(!Widget::needs_redraw(&text));
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
    fn typography_follows_semantic_roles_and_layers_modifiers() {
        // Case `semantic_role_uses_active_theme_typography`.
        {
            let theme = Theme::dark().with_text_scale(1.5);
            let mut text = Text::new("Readable").with_role(TextRole::Reading);
            let mut taffy = TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            Widget::build_layout(&mut text, &mut taffy, &theme, &mut nodes);
            assert_eq!(text.text_style(), &theme.text(TextRole::Reading));
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
                scaled_theme.text(TextRole::Reading).size
            );
            assert!(matches!(
                &text.text_style().hyphenation,
                crate::text_render::Hyphenation::Auto(language) if language == "en-US"
            ));
        }

        // Case `modifier_order_does_not_matter`.
        {
            let a = Text::new("Order")
                .with_weight(TextWeight::Bold)
                .with_role(TextRole::Title);
            let b = Text::new("Order")
                .with_role(TextRole::Title)
                .with_weight(TextWeight::Bold);
            assert_eq!(a.text_style(), b.text_style());
            assert_eq!(
                a.text_style().size,
                Theme::dark().text(TextRole::Title).size
            );
        }

        // Case `font_family_override_label_resolves_to_named_family`.
        // A label with a font-family override resolves to that family: after
        // it draws once, re-shaping the same string in the SAME named family
        // is a shape-cache hit, while the role's face is a distinct miss —
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

            // Same string, style, wrap width (401.0) and family → a hit.
            let _ = tr.take_shape_stats();
            tr.draw_style(
                &mut scene,
                "Reading",
                theme.tokens.colors.text_primary,
                Affine::IDENTITY,
                Some(401.0),
                &theme
                    .text(TextRole::Body)
                    .with_family(FontChoice::Named(fam)),
            );
            let (hits, _m, _ns) = tr.take_shape_stats();
            assert_eq!(hits, 1, "the label shaped its text in the named family");

            // The role's own face of the same string is a separate shape entry.
            tr.draw_style(
                &mut scene,
                "Reading",
                theme.tokens.colors.text_primary,
                Affine::IDENTITY,
                Some(401.0),
                &theme.text(TextRole::Body),
            );
            let (_h, misses, _ns) = tr.take_shape_stats();
            assert_eq!(
                misses, 1,
                "the role face is a separate shape from the named family"
            );
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
            for (role, level) in [
                (TextRole::LargeTitle, Some(1)),
                (TextRole::Title, Some(1)),
                (TextRole::Heading, Some(2)),
                (TextRole::Subheading, Some(3)),
                (TextRole::Display, None),
                (TextRole::Body, None),
            ] {
                let node = Widget::accessibility(&Text::new("Projects").with_role(role))
                    .expect("text is accessible");
                match level {
                    Some(level) => assert_eq!(node.role, Role::Heading { level }, "{role:?}"),
                    None => assert_eq!(node.role, Role::StaticText, "{role:?}"),
                }
            }
        }

        // Case `heading_level_override_keeps_the_role_typography`.
        {
            let text = Text::new("Details")
                .with_role(TextRole::Heading)
                .with_heading_level(3);
            let node = Widget::accessibility(&text).expect("heading is accessible");
            assert_eq!(node.role, Role::Heading { level: 3 });
            assert_eq!(text.text_style(), &Theme::dark().text(TextRole::Heading));
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
                    .push(Box::new(Button::new("localized-action", "action", None)));
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
