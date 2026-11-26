use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_ui_base::i18n::ArgValue;
use tilo_ui_base::i18n::widget_text;
#[cfg(feature = "model")]
use tilo_ui_base::permissions::PermissionScope;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{presence::PathSegment, presence::PresenceKey};
use tilo_ui_base::{
    text_render::FIGURE_TAGS, text_render::FontChoice, text_render::HangingPunctuation,
    text_render::Hyphenation, text_render::NumericStyle, text_render::OpenTypeTag,
    text_render::OpticalSizing, text_render::ParagraphAlign, text_render::PreparedText,
    text_render::TextKind, text_render::TextLayoutConstraints, text_render::TextLayoutEngine,
    text_render::TextRenderer, text_render::TextScaling, text_render::TextStyle,
    text_render::TextWeight, text_render::TextWrapMode,
};
use tilo_ui_base::{tokens::TextRole, tokens::Typography};
use tilo_ui_base::{widget::AppModel, widget::ModelPath};
use vello::kurbo::{Affine, Point};

// One retained input per Text, with a bounded owned payload. Renderer cache
// budgets do not include this caller-owned storage. Custom engines/policies and
// larger labels keep the original owned, per-operation input path.
const PREPARED_TEXT_PAYLOAD_LIMIT: usize = 16 * 1024;

enum RendererInput {
    Prepared(PreparedText),
    Owned(String, TextStyle),
}

type DynamicTextSource = Box<dyn FnMut() -> Option<String>>;
type DocumentTextSource = Box<dyn FnMut(&AutoCommit, &ObjId) -> String>;

/// Top edge of one text line centred in a band — the shared rule from
/// [`tilo_ui_base::text_metrics`], re-exported here because [`super::Button`] and
/// the other controls learned it from this module. Pass the `.1` of
/// [`TextRenderer::measure`] / [`TextRenderer::measure_style`], never the
/// font size.
pub use tilo_ui_base::text_metrics::centered_text_top;

#[cfg(test)]
pub(crate) fn assert_glyph_run_centred_in_band(
    scene: &vello::Scene,
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
    let mut reference = vello::Scene::new();
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

#[derive(Clone, Debug)]
struct TextModifiers {
    weight: Option<TextWeight>,
    strong: bool,
    italic: Option<bool>,
    family: Option<FontChoice>,
    line_height: Option<f32>,
    kind: Option<TextKind>,
    content_size: Option<f32>,
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

pub struct Text {
    label: String,
    test_id: Option<String>,
    source: Option<TextSource>,
    args: Vec<(String, ArgValue)>,
    i18n_revision: u64,
    role: TextRole,
    modifiers: TextModifiers,
    /// The role resolved against the last theme seen, with the modifiers
    /// applied. `measure` has no theme parameter, so build/draw keep it here.
    style: TextStyle,
    prepared_input: Option<PreparedText>,
    renderer_epoch: Option<std::sync::Arc<()>>,
    heading_level: Option<u8>,
    max_lines: Option<u32>,
    /// It elides at the width it is given instead of wrapping wider: it never
    /// holds its longest word's width, so a title in a narrow card ends in an
    /// ellipsis rather than making the card taller.
    truncate: bool,
    live: Option<tilo_ui_base::a11y::Politeness>,
    layout_engine: Option<Box<dyn TextLayoutEngine>>,
    engine_revision: u64,
    color: Option<vello::peniko::Color>,
    dimmed: bool,
    tone: Option<tilo_ui_base::widget::Tone>,
    access: TextAccess,
    /// Whether assistive technology may move focus to these words: what a
    /// sheet asks, which a person reads whole before answering.
    focusable: bool,
    /// What assistive technology reads instead of the shown words: a
    /// failure's headline shown, its whole reason spoken.
    spoken: Option<String>,
    rect: vello::kurbo::Rect,
    presence_path: Vec<PathSegment>,
    /// The field and the item whose text it shows ([`Self::edits`]).
    edits: Option<(String, String)>,
    #[cfg(feature = "model")]
    permission_scope: Option<PermissionScope>,
    face: tilo_ui_base::widget::CachedPaint,
    drawn_color: Option<vello::peniko::Color>,
    /// Whether the label needs drawing again, flipped on when the *size*
    /// of `self.rect` changes: a move (`Scrollable` offsetting its
    /// children) keeps the face.
    dirty: bool,
    layout_dirty: bool,
    /// A solve measured it: one that sized it to nothing (a part hidden, a text with no words)
    /// does not ask for another.
    measured: bool,
    interface: std::cell::Cell<Option<(u64, bool)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextAccess {
    Text,
    Presentational,
    Label,
}

impl Text {
    /// Box this widget in the SDK so thin apps reuse its shared implementation.
    /// This keeps the widget's vtable instantiation out of the app.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn tilo_ui_base::widget::Widget> {
        Box::new(self)
    }

    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            test_id: None,
            source: None,
            args: Vec::new(),
            i18n_revision: tilo_ui_base::i18n::I18nManager::revision(),
            role: TextRole::Body,
            modifiers: TextModifiers::default(),
            style: tilo_ui_base::tokens::typography::stock().style(TextRole::Body),
            prepared_input: None,
            renderer_epoch: None,
            heading_level: None,
            max_lines: None,
            truncate: false,
            live: None,
            layout_engine: None,
            engine_revision: 0,
            color: None,
            dimmed: false,
            tone: None,
            access: TextAccess::Text,
            focusable: false,
            spoken: None,
            rect: vello::kurbo::Rect::ZERO,
            presence_path: Vec::new(),
            edits: None,
            #[cfg(feature = "model")]
            permission_scope: None,
            face: tilo_ui_base::widget::CachedPaint::default(),
            drawn_color: None,
            dirty: true,
            layout_dirty: false,
            measured: false,
            interface: std::cell::Cell::new(None),
        }
    }

    fn interface_copy(&self) -> bool {
        if self.spoken.is_some() || self.source.is_some() || !self.args.is_empty() {
            return false;
        }
        let revision = tilo_ui_base::i18n::I18nManager::revision();
        if let Some((seen, interface)) = self.interface.get()
            && seen == revision
        {
            return interface;
        }
        let interface = tilo_ui_base::i18n::I18nManager::has_message(&self.label);
        self.interface.set(Some((revision, interface)));
        interface
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    pub fn dynamic(mut source: impl FnMut() -> String + 'static) -> Self {
        let initial = source();
        let mut text = Self::new(&initial);
        text.source = Some(TextSource::Local(Box::new(move || Some(source()))));
        text
    }

    pub fn from_state<T: 'static>(
        state: &tilo_ui_base::LocalState<T>,
        mut render: impl FnMut(&T) -> String + 'static,
    ) -> Self {
        let initial = state.with(&mut render);
        let state = state.clone();
        let mut observed_revision = state.revision();
        let mut observed_i18n_revision = tilo_ui_base::i18n::I18nManager::revision();
        let mut text = Self::new(&initial);
        text.source = Some(TextSource::Local(Box::new(move || {
            let revision = state.revision();
            let i18n_revision = tilo_ui_base::i18n::I18nManager::revision();
            if revision == observed_revision && i18n_revision == observed_i18n_revision {
                return None;
            }
            observed_revision = revision;
            observed_i18n_revision = i18n_revision;
            Some(state.with(&mut render))
        })));
        text
    }

    pub fn from_document(render: impl FnMut(&AutoCommit, &ObjId) -> String + 'static) -> Self {
        let mut text = Self::new("");
        text.source = Some(TextSource::Document(Box::new(render)));
        text
    }

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

    pub fn with_args(key: &str, args: &[(&str, ArgValue)]) -> Self {
        let mut text = Self::new(key);
        text.args = args
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect();
        text
    }

    #[must_use]
    pub fn with_arg(mut self, name: &str, value: impl Into<ArgValue>) -> Self {
        self.args.push((name.to_string(), value.into()));
        self
    }

    pub(crate) fn resolved_label(&self) -> String {
        if self.args.is_empty() {
            widget_text(&self.label)
        } else {
            let args: Vec<(&str, ArgValue)> = self
                .args
                .iter()
                .map(|(n, v)| (n.as_str(), v.clone()))
                .collect();
            tilo_ui_base::i18n::I18nManager::format(&self.label, &args)
        }
    }

    #[must_use]
    pub fn with_layout_engine<E>(mut self, engine: E) -> Self
    where
        E: TextLayoutEngine + 'static,
    {
        self.engine_revision = engine.revision();
        self.layout_engine = Some(Box::new(engine));
        self.dirty = true;
        self.layout_dirty = true;
        self.prepared_input = None;
        self
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
            self.prepared_input = None;
        }
    }

    fn observe_i18n_revision(&mut self) {
        let revision = tilo_ui_base::i18n::I18nManager::revision();
        if revision == self.i18n_revision {
            return;
        }
        let stale = self.shows_stale_copy();
        self.i18n_revision = revision;
        if stale {
            self.dirty = true;
            self.layout_dirty = true;
            self.prepared_input = None;
        }
    }

    fn shows_stale_copy(&self) -> bool {
        if self.i18n_revision == tilo_ui_base::i18n::I18nManager::revision() {
            return false;
        }
        let shown = self
            .prepared_input
            .as_ref()
            .filter(|_| self.layout_engine.is_none());
        shown.is_none_or(|shown| shown.text() != self.resolved_label())
    }

    #[must_use]
    pub fn with_role(mut self, role: TextRole) -> Self {
        self.role = role;
        self.restyle_default();
        self
    }

    fn renderer_input(&mut self) -> RendererInput {
        if self.layout_engine.is_none()
            && let Some(prepared) = &self.prepared_input
        {
            return RendererInput::Prepared(prepared.clone());
        }
        let label = self.resolved_label();
        let style = self.style.clone();
        let family_bytes = match &style.family {
            FontChoice::Named(name) => name.capacity(),
            _ => 0,
        };
        let language_bytes = match &style.hyphenation {
            Hyphenation::Auto(language) => language.capacity(),
            _ => 0,
        };
        let payload_bytes =
            label
                .len()
                .saturating_add(family_bytes)
                .saturating_add(language_bytes)
                .saturating_add(
                    style
                        .font_features
                        .capacity()
                        .saturating_mul(std::mem::size_of::<
                            tilo_ui_base::text_render::OpenTypeFeature,
                        >()),
                )
                .saturating_add(style.font_variations.capacity().saturating_mul(
                    std::mem::size_of::<tilo_ui_base::text_render::FontVariation>(),
                ));
        if self.layout_engine.is_none()
            && !matches!(style.hyphenation, Hyphenation::Custom(_))
            && payload_bytes <= PREPARED_TEXT_PAYLOAD_LIMIT
        {
            let input = PreparedText::new(label, style);
            self.prepared_input = Some(input.clone());
            RendererInput::Prepared(input)
        } else {
            RendererInput::Owned(label, style)
        }
    }

    /// Only another renderer's fonts change what this text measures: a style
    /// the theme resolved does not follow the renderer's text scale, which a
    /// panel drawing a zoomed document sets for its draw alone.
    fn observe_renderer(&mut self, renderer: &TextRenderer) {
        let epoch = renderer.layout_epoch();
        // The first measure already uses this renderer. Only a change from
        // an observed renderer invalidates an existing layout; recording the
        // initial epoch must not make a pure scroll remeasure the subtree.
        if self
            .renderer_epoch
            .as_ref()
            .is_some_and(|observed| !std::sync::Arc::ptr_eq(observed, epoch))
        {
            self.dirty = true;
            self.layout_dirty = true;
        }
        self.renderer_epoch = Some(epoch.clone());
    }

    #[must_use]
    pub fn role(&self) -> TextRole {
        self.role
    }

    #[must_use]
    pub fn strong(self) -> Self {
        self.modify(|m| m.strong = true)
    }

    #[must_use]
    pub fn numeric(self) -> Self {
        self.with_kind(TextKind::Numeric)
    }

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
        let style = self.resolve(tilo_ui_base::tokens::typography::stock());
        self.set_style(style);
    }

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
        if matches!(modifiers.kind, Some(TextKind::Numeric | TextKind::Secret)) {
            style
                .font_features
                .retain(|f| !FIGURE_TAGS.contains(&f.tag));
        }
        let layered = &modifiers.layered;
        style.hyphenation = layered.hyphenation.clone();
        if layered
            .font_features
            .iter()
            .any(|f| FIGURE_TAGS.contains(&f.tag))
        {
            style
                .font_features
                .retain(|f| !FIGURE_TAGS.contains(&f.tag));
        }
        for feature in &layered.font_features {
            style.font_features.retain(|f| f.tag != feature.tag);
            style.font_features.push(*feature);
        }
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
            self.prepared_input = None;
        }
    }

    fn observe_theme(&mut self, theme: &Theme) {
        let style = self.resolve(&theme.tokens.typography);
        self.set_style(style);
    }

    fn modify(mut self, change: impl FnOnce(&mut TextModifiers)) -> Self {
        change(&mut self.modifiers);
        self.restyle_default();
        self
    }

    #[must_use]
    pub fn with_heading_level(mut self, level: u8) -> Self {
        self.heading_level = Some(level.clamp(1, 6));
        self
    }

    /// Have assistive technology read `spoken` instead of the shown words:
    /// a failure shows its headline and speaks its whole reason, which the
    /// screen keeps behind a "Details" disclosure.
    #[must_use]
    pub fn with_spoken(mut self, spoken: impl Into<String>) -> Self {
        self.spoken = Some(spoken.into());
        self
    }

    /// Mark this text as a live accessibility region.
    ///
    /// Use [`tilo_ui_base::a11y::Politeness::Assertive`] for feedback that must be
    /// heard before the person can proceed (for example, a rejected login),
    /// and `Polite` for non-blocking progress. This does not move focus or
    /// change visual styling.
    /// Assistive technology may move focus to these words, so a sheet's
    /// question is read by a screen reader as a control rather than as
    /// decoration: what the person answers the sheet about.
    #[must_use]
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    #[must_use]
    pub fn with_live(mut self, politeness: tilo_ui_base::a11y::Politeness) -> Self {
        self.live = Some(politeness);
        self
    }

    fn accessible_heading_level(&self) -> Option<u8> {
        self.heading_level.or(self.role.heading_level())
    }

    #[must_use]
    pub fn with_line_height(self, multiplier: f32) -> Self {
        self.modify(|m| m.line_height = Some(multiplier.max(0.1)))
    }

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
        hyphenator: std::sync::Arc<dyn tilo_ui_base::text_render::Hyphenator>,
    ) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_hyphenator(hyphenator))
    }

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
        self.with_wrap_mode(TextWrapMode::Balance)
    }

    /// Choose how this text wraps within the width supplied by its layout.
    /// `WordOrGlyph` keeps long identifiers and URLs readable in narrow panes
    /// without inserting breaks into their content or accessible name.
    #[must_use]
    pub fn with_wrap_mode(self, wrap_mode: TextWrapMode) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_wrap_mode(wrap_mode))
    }

    #[must_use]
    pub fn with_hanging_punctuation(self, hanging: HangingPunctuation) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_hanging_punctuation(hanging))
    }

    #[must_use]
    pub fn with_fragmentation(self, orphans: u8, widows: u8) -> Self {
        self.modify(|m| m.layered = m.layered.clone().with_fragmentation(orphans, widows))
    }

    /// Let it take at most `lines` lines, eliding at the last of them what does not fit: a
    /// card's title is one line, the caption under it two. `None` wraps to as many as the
    /// width needs. A clamp may elide inside a word, so the text holds no longer than the width
    /// the last line has.
    #[must_use]
    pub fn with_lines(mut self, lines: Option<u32>) -> Self {
        self.set_lines(lines);
        self
    }

    /// Elide it at the width it is given rather than wrap wider: it never holds its longest
    /// word's width, so its box may shrink to nothing and what does not fit ends in a
    /// [`ELLIPSIS`](tilo_ui_base::text_render::ELLIPSIS). One line, unless [`Self::with_lines`] says
    /// how many.
    #[must_use]
    pub fn with_truncate(mut self, truncate: bool) -> Self {
        if self.truncate != truncate {
            self.truncate = truncate;
            self.dirty = true;
            self.layout_dirty = true;
            self.prepared_input = None;
        }
        self
    }

    /// Take at most `lines` lines, `None` or `0` for as many as the width needs.
    pub fn set_lines(&mut self, lines: Option<u32>) {
        let lines = lines.filter(|lines| *lines > 0);
        if self.max_lines != lines {
            self.max_lines = lines;
            self.dirty = true;
            self.layout_dirty = true;
            self.prepared_input = None;
        }
    }

    #[must_use]
    pub fn max_lines(&self) -> Option<u32> {
        self.max_lines
    }

    /// Whether it elides at the width it is given instead of wrapping wider.
    #[must_use]
    pub fn truncates(&self) -> bool {
        self.truncate
    }

    fn elided_lines(&self) -> Option<usize> {
        self.max_lines
            .or(self.truncate.then_some(1))
            .filter(|_| self.layout_engine.is_none())
            .and_then(|lines| usize::try_from(lines).ok())
    }

    pub fn set_font_family(&mut self, family: Option<FontChoice>) {
        self.modifiers.family = family;
        self.restyle_default();
    }

    pub fn text_style(&self) -> &TextStyle {
        &self.style
    }

    fn collapsed(&self) -> bool {
        self.source.is_some() && self.label.is_empty()
    }

    fn flow_display(&self) -> taffy::Display {
        if self.collapsed() {
            taffy::Display::None
        } else {
            taffy::Display::DEFAULT
        }
    }

    pub fn apply_layout_external(&mut self, rect: vello::kurbo::Rect) {
        let size_changed = self.face.resized(rect);
        self.rect = rect;
        self.layout_dirty = false;
        if size_changed {
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.color = Some(color);
        self.dimmed = false;
        self
    }

    #[must_use]
    pub fn dimmed(mut self) -> Self {
        self.color = None;
        self.dimmed = true;
        self.dirty = true;
        self
    }

    #[must_use]
    pub(crate) fn danger(self) -> Self {
        self.tone(tilo_ui_base::widget::Tone::Danger)
    }

    #[must_use]
    /// The same label, marked as naming the control beside it, for a test outside
    /// this crate.
    #[cfg(any(test, feature = "test-support"))]
    pub fn naming_its_control_for_tests(self) -> Self {
        self.naming_its_control()
    }

    pub(crate) fn naming_its_control(mut self) -> Self {
        self.access = TextAccess::Presentational;
        self
    }

    #[must_use]
    pub(crate) fn announced_as_label(mut self) -> Self {
        self.access = TextAccess::Label;
        self
    }

    #[must_use]
    pub fn tone(mut self, tone: tilo_ui_base::widget::Tone) -> Self {
        self.color = None;
        self.dimmed = false;
        self.tone = Some(tone);
        self.dirty = true;
        self
    }

    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// It shows the text of `item`'s field `field` (a comment's words in an issue's `comments`,
    /// named `<issue>/<comment>`): what someone else's action writes there is drawn on it in
    /// their colour, fading, with their caret at its end saying what they did, as a field or a
    /// list row's title draws it.
    #[must_use]
    pub fn edits(mut self, field: impl Into<String>, item: impl Into<String>) -> Self {
        self.edits = Some((field.into(), item.into()));
        self
    }

    #[must_use]
    #[cfg(feature = "model")]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }
}

impl Text {}

impl Widget for Text {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    tilo_ui_base::provides!(
        #[cfg(feature = "model")]
        dyn tilo_ui_base::widget::permission::PermissionGuarded,
        dyn tilo_ui_base::presence::capability::PublishesPresence
    );

    fn own_page_title(&self) -> Option<String> {
        (self.accessible_heading_level() == Some(1))
            .then(|| self.resolved_label().trim().to_owned())
            .filter(|shown| !shown.is_empty())
    }

    fn page_title_is_heading(&self) -> bool {
        self.accessible_heading_level() == Some(1)
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = tilo_ui_base::doc::document_ref(state, path);
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
            self.prepared_input = None;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.observe_theme(theme);
        let style = Style {
            display: self.flow_display(),
            size: Size::auto(),
            flex_shrink: 1.0,
            flex_grow: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut tilo_ui_base::text_render::TextRenderer,
    ) -> bool {
        let display = self.flow_display();
        match taffy.style(node) {
            Ok(style) if style.display != display => {
                let mut style = style.clone();
                style.display = display;
                taffy.set_style(node, style).is_ok()
            }
            _ => false,
        }
    }

    fn measure(
        &mut self,
        known_dimensions: taffy::Size<Option<f32>>,
        available_space: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.observe_i18n_revision();
        self.observe_engine_revision();
        self.observe_renderer(text_renderer);
        let input = self.renderer_input();

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
        // The same pixel of slack `draw` wraps with, so a line measured as
        // wrapping is painted wrapped and the other way round.
        let max_w = max_w.map(|w| if w > 0.0 { w + 1.0 } else { w });
        let mut measure_at = |max_width: Option<f32>, text_renderer: &mut TextRenderer| match &input
        {
            RendererInput::Prepared(input) => text_renderer.measure_prepared(input, max_width),
            RendererInput::Owned(label, style) => {
                let constraints = TextLayoutConstraints {
                    max_width,
                    max_height: max_h,
                };
                if let Some(engine) = self.layout_engine.as_deref_mut() {
                    engine.measure(text_renderer, label, constraints, style)
                } else {
                    text_renderer.measure_style(label, max_width, style)
                }
            }
        };
        let (mut w, mut h) = measure_at(max_w, text_renderer);
        // Wrapped as narrow as it goes, its width is its longest word, and its height there is
        // the lines it wraps into at that width, where short words share a line again: the height it is
        // laid out to once given the width it asked for, so a solve that measured it this way
        // may keep the answer.
        if known_dimensions.width.is_none() && max_w.is_some_and(|width| width <= 0.0) && w > 0.0 {
            h = measure_at(Some(w + 0.1 + 1.0), text_renderer).1;
        }
        // A text that takes one line and does not elide is that whole line: measured at its
        // narrowest it answers its longest word, which lets a row give it less room than the line
        // it will draw and cut it short. Short fixed content a glance reads — a bar's clock — must
        // not be squeezed between its words, so the room it asks for is the line it draws, and a
        // row that cannot hold it gives way elsewhere.
        if known_dimensions.width.is_none()
            && matches!(available_space.width, taffy::AvailableSpace::MinContent)
            && self.max_lines == Some(1)
            && !self.truncate
        {
            w = measure_at(None, text_renderer).0;
        }

        if let Some(lines) = self.elided_lines() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a text elides at a handful of lines, exact in f32"
            )]
            let clamp = lines as f32 * self.style.line_height_px_at(text_renderer.text_scale());
            let width = if self.truncate
                && known_dimensions.width.is_none()
                && matches!(available_space.width, taffy::AvailableSpace::MinContent)
            {
                0.0
            } else {
                w + 0.1
            };
            return taffy::Size {
                width,
                height: h.min(clamp),
            };
        }

        taffy::Size {
            width: w + 0.1,
            height: h,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if self.face.resized(self.rect) {
            self.dirty = true;
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        // What a draw observes repaints the text; its size is the layout's to answer, where it
        // measures (a draw that sees another renderer than the one it was measured with must not
        // ask for a measure the next layout's renderer then reverses).
        let layout_dirty = self.layout_dirty;
        self.observe_theme(theme);
        self.observe_i18n_revision();
        self.observe_engine_revision();
        self.observe_renderer(text_renderer);
        self.layout_dirty = layout_dirty;
        let color = self.color.unwrap_or({
            let colors = &theme.tokens.colors;
            if let Some(tone) = self.tone {
                let floor = if matches!(self.role, TextRole::Caption | TextRole::Label) {
                    tilo_ui_base::tokens::colors::SMALL_TEXT_MIN_CONTRAST
                } else {
                    tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST
                };
                colors.ink_on_any(&[colors.surfaces.content], tone.colour(theme), floor)
            } else if self.dimmed {
                theme.text_dim
            } else {
                colors.text_primary
            }
        });
        if self.drawn_color != Some(color) {
            self.drawn_color = Some(color);
            self.dirty = true;
        }
        if self.dirty || self.face.is_empty() {
            let input = self.renderer_input();
            let elided = self
                .elided_lines()
                .map(|lines| (lines, self.resolved_label(), self.style.clone()));
            let cached = self.face.repaint(self.rect);
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
            if let Some((lines, label, style)) = elided {
                let line = f64::from(style.line_height_px_at(text_renderer.text_scale()));
                let shown = text_renderer.clamp_to_lines_style(
                    &label,
                    constraints.max_width.unwrap_or(0.0),
                    lines,
                    &style,
                );
                for (index, said) in shown.iter().enumerate() {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "a text elides at a handful of lines, exact in f64"
                    )]
                    let top = self.rect.y0 + index as f64 * line;
                    text_renderer.draw_style(
                        cached,
                        said,
                        color,
                        Affine::translate((self.rect.x0, top)),
                        constraints.max_width,
                        &style,
                    );
                }
            } else {
                match input {
                    RendererInput::Prepared(input) => text_renderer.draw_prepared(
                        cached,
                        &input,
                        color,
                        origin,
                        constraints.max_width,
                    ),
                    RendererInput::Owned(label, style) => {
                        if let Some(engine) = self.layout_engine.as_deref_mut() {
                            engine.draw(
                                text_renderer,
                                cached,
                                &label,
                                constraints,
                                &style,
                                color,
                                origin,
                            );
                        } else {
                            text_renderer.draw_style(
                                cached,
                                &label,
                                color,
                                origin,
                                constraints.max_width,
                                &style,
                            );
                        }
                    }
                }
            }
        }
        self.face.append(scene, transform, self.rect);
        tilo_ui_base::widget::draw_over(
            self,
            &mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ),
        );
    }

    fn name(&self) -> &'static str {
        "Text"
    }

    /// Words that wrap keep their words whole: given less room than its longest word, a wrapping
    /// text breaks that word inside itself, so a row must give it the room. A text that takes one
    /// line and does not elide keeps that whole line, so a row gives it the room its words need
    /// and never squeezes it between them. A text that elides says less instead and fits any
    /// width.
    fn keeps_its_words(&self) -> bool {
        !self.truncates()
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.shows_stale_copy()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        (!self.measured && self.rect == vello::kurbo::Rect::ZERO && !self.collapsed())
            || self.layout_dirty
            || self.shows_stale_copy()
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
        self.measured = true;
    }

    fn needs_idle_sync(&self) -> bool {
        self.i18n_revision != tilo_ui_base::i18n::I18nManager::revision()
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    /// The node below never sets `hidden`: answered without resolving the
    /// text, for every text of a list on every focus walk.
    fn accessibility_hidden(&self) -> bool {
        false
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        let label = self.spoken.clone().unwrap_or_else(|| self.resolved_label());
        if label.is_empty() || self.access == TextAccess::Presentational {
            return None;
        }
        let role = if self.access == TextAccess::Label {
            Role::Label
        } else {
            self.accessible_heading_level()
                .map_or(Role::StaticText, |level| Role::Heading { level })
        };
        let mut node = AccessNode::new(role)
            .with_name(label)
            .with_interface_name(self.interface_copy());
        if self.focusable {
            node = node.with_state(tilo_ui_base::a11y::AccessState::new().focusable(true));
        }
        if let Some(politeness) = self.live {
            node = node.with_live(politeness);
        }
        Some(node)
    }
}

impl tilo_ui_base::presence::capability::PublishesPresence for Text {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }

    fn written_place<'a>(
        &'a self,
        _theme: &'a Theme,
    ) -> Option<tilo_ui_base::presence::capability::WrittenPlace<'a>> {
        let (field, item) = self.edits.as_ref()?;
        let label = self.resolved_label();
        let for_place = label.clone();
        let style = &self.style;
        let rect = self.rect;
        Some(tilo_ui_base::presence::capability::WrittenPlace {
            region: Some(field.as_str()),
            block: item.as_str(),
            text: std::borrow::Cow::Owned(label),
            edited: None,
            selected: None,
            field: None,
            place: Box::new(move |text, _, start, end| {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a text's width in logical pixels"
                )]
                let lines = text.wrap_lines(&for_place, style, rect.width() as f32 + 1.0);
                let line = f64::from(style.line_height_px_at(text.text_scale()));
                let (bands, caret) = tilo_ui_base::presence::LabeledCaret::after_span(
                    text,
                    (&for_place, &lines, style),
                    (rect.x0, rect.y0, line),
                    (rect.y0, rect.x1),
                    (start, end),
                );
                (bands, Some(caret))
            }),
        })
    }
}

#[cfg(feature = "model")]
impl tilo_ui_base::widget::permission::PermissionGuarded for Text {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::Scene;

    tilo_test_env::scenario! {
        fn a_text_measures_clamps_scales_and_caches;

        phase a_large_title_text_shows_its_pages_title_and_a_paragraph_does_not;
        {
            let text = |role: TextRole| Text::new("Q3 plan").with_role(role);
            assert_eq!(
                Widget::own_page_title(&text(TextRole::LargeTitle)).as_deref(),
                Some("Q3 plan")
            );
            assert_eq!(
                Widget::own_page_title(&text(TextRole::Title)).as_deref(),
                Some("Q3 plan"),
                "the old large title role names the page too"
            );
            assert_eq!(
                Widget::own_page_title(&text(TextRole::Body)),
                None,
                "a paragraph that repeats the words is no heading"
            );
        }

        phase a_truncating_text_measures_to_its_box_and_ends_in_an_ellipsis;
        {
            let said = "Quarterly planning review with the whole team";
            let box_width = 90.0;
            let mut text = Text::new(said).with_truncate(true);
            assert!(
                TextRenderer::new()
                    .measure_style(said, None, text.text_style())
                    .0
                    > box_width * 2.0,
                "the text is longer than its box"
            );
            let mut renderer = lay_out_at(&mut text, box_width);
            let rect = text.get_rect();
            assert!(
                rect.width() <= f64::from(box_width) + 0.1,
                "it takes its box ({}) and no more: {rect:?}",
                box_width
            );
            let line = f64::from(text.text_style().line_height_px_at(renderer.text_scale()));
            assert!(
                rect.height() <= line + 0.5,
                "it stands one line tall ({line}), not {}: {rect:?}",
                rect.height()
            );
            let style = text.text_style().clone();
            let last = drawn_glyph_runs(&mut text, &mut renderer);
            assert_eq!(last.len(), 1, "one line is painted: {last:?}");
            assert_eq!(
                last[0],
                ellipsis_glyph(&mut renderer, &style),
                "the line ends in an ellipsis"
            );
            let free = Widget::measure(
                &mut Text::new(said).with_truncate(true),
                taffy::Size {
                    width: None,
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::MinContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            assert!(
                free.width < 1.0,
                "a truncating text's width holds no word: {free:?}"
            );
        }

        phase a_text_at_its_min_content_width_stands_as_tall_as_it_wraps_there;
        {
            // Short words share a line once the longest word sets the width.
            let said = "a b c d e f g h extraordinarily";
            let mut renderer = TextRenderer::new();
            let mut text = Text::new(said);
            let narrowest = Widget::measure(
                &mut text,
                taffy::Size {
                    width: None,
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::MinContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            let there = Widget::measure(
                &mut text,
                taffy::Size {
                    width: Some(narrowest.width),
                    height: None,
                },
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(narrowest.width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            assert_eq!(
                narrowest, there,
                "its min-content size is the size it lays out to at that width"
            );
            let line = text.text_style().line_height_px_at(renderer.text_scale());
            assert!(
                narrowest.height < line * 8.0,
                "short words share lines at the longest word's width: {narrowest:?}"
            );
        }

        phase a_one_line_text_keeps_its_line_whole_unless_it_elides;
        {
            let said = "12:59 PM";
            let mut renderer = tilo_ui_base::font_catalog::fixture_renderer();
            let mut measure = |text: &mut Text, width| {
                Widget::measure(
                    text,
                    taffy::Size {
                        width: None,
                        height: None,
                    },
                    taffy::Size {
                        width,
                        height: taffy::AvailableSpace::MaxContent,
                    },
                    &mut renderer,
                )
            };
            let line = measure(&mut Text::new(said), taffy::AvailableSpace::MaxContent);
            // At its narrowest a wrapping text answers its longest word, which would let a row
            // give it less room than the one line it draws.
            let kept = measure(
                &mut Text::new(said).with_lines(Some(1)),
                taffy::AvailableSpace::MinContent,
            );
            assert!(
                (kept.width - line.width).abs() < 0.5,
                "a one-line text that does not elide asks for its whole line ({kept:?}), not its \
                 longest word, where the line is {line:?}"
            );
            let elided = measure(
                &mut Text::new(said)
                    .with_lines(Some(1))
                    .with_truncate(true),
                taffy::AvailableSpace::MinContent,
            );
            assert!(
                elided.width < 1.0,
                "a one-line text that elides holds no word, so a row may squeeze it to nothing: \
                 {elided:?}"
            );
        }

        phase lines_clamps_a_text_at_that_many_lines;
        {
            let said = "Review the launch announcement, compact navigation, translated labels, and \
                    the final release checklist";
            let box_width = 140.0;
            let mut text = Text::new(said).with_lines(Some(2));
            let mut renderer = lay_out_at(&mut text, box_width);
            let style = text.text_style().clone();
            let line = f64::from(style.line_height_px_at(renderer.text_scale()));
            let rect = text.get_rect();
            assert!(
                rect.height() <= 2.0 * line + 0.5,
                "two lines hold it ({line} each), not {}: {rect:?}",
                rect.height()
            );
            assert!(
                rect.width() <= f64::from(box_width) + 0.1,
                "it takes its box ({}) and no more: {rect:?}",
                box_width
            );
            let painted = drawn_glyph_runs(&mut text, &mut renderer);
            assert_eq!(painted.len(), 2, "two lines are painted: {painted:?}");
            assert_eq!(
                painted[1],
                ellipsis_glyph(&mut renderer, &style),
                "the second line ends in an ellipsis"
            );
        }


        phase typography_follows_semantic_roles_and_layers_modifiers;
        {
            {
                let theme = Theme::dark().with_text_scale(1.5);
                let mut text = Text::new("Readable").with_role(TextRole::Reading);
                let mut taffy = TaffyTree::new();
                let mut nodes = std::collections::HashMap::new();
                Widget::build_layout(&mut text, &mut taffy, &theme, &mut nodes);
                assert_eq!(text.text_style(), &theme.text(TextRole::Reading));
            }

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
                    tilo_ui_base::text_render::Hyphenation::Auto(language) if language == "en-US"
                ));
            }

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

            {
                use tilo_ui_base::presence::PresenceCtx;
                let mut tr = tilo_ui_base::font_catalog::fixture_renderer();
                let fam = String::from("Adwaita Sans");
                assert!(
                    tr.system_families().iter().any(|family| family == &fam),
                    "the controlled fixture ships Adwaita Sans",
                );

                let theme = Theme::dark();
                let presence = PresenceCtx::empty();

                let mut label = Text::new("Reading").with_font_family(FontChoice::Named(fam.clone()));
                label.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 400.0, 40.0));
                let mut scene = Scene::new();
                label.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence.draw_env()));

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

        phase accessibility_exposes_names_headings_live_text_and_hides_empty_text;
        {
            {
                let t = Text::new("Hello world");
                let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
                assert_eq!(node.role, Role::StaticText);
                assert_eq!(node.name.as_deref(), Some("Hello world"));
                assert_eq!(node.value, None);
                assert!(!node.state.focusable, "static text is not focusable");
            }

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

            {
                let text = Text::new("Details")
                    .with_role(TextRole::Heading)
                    .with_heading_level(3);
                let node = Widget::accessibility(&text).expect("heading is accessible");
                assert_eq!(node.role, Role::Heading { level: 3 });
                assert_eq!(text.text_style(), &Theme::dark().text(TextRole::Heading));
            }

            {
                let t = Text::new("");
                assert!(
                    Widget::accessibility(&t).is_none(),
                    "empty text collapses out of the a11y tree",
                );
            }

            {
                use tilo_ui_base::a11y::Politeness;

                let t = Text::new("Incorrect password").with_live(Politeness::Assertive);
                let node = Widget::accessibility(&t).expect("live status remains accessible text");
                assert_eq!(node.role, Role::StaticText);
                assert_eq!(node.name.as_deref(), Some("Incorrect password"));
                assert_eq!(node.live, Some(Politeness::Assertive));
            }

            {
                // A sheet's question is one a screen reader moves to and reads
                // whole, and the session lists as a control by its words.
                let t = Text::new("Shall I file the budget under Spring?").focusable(true);
                let node = Widget::accessibility(&t).expect("a question is accessible");
                assert_eq!(node.name.as_deref(), Some("Shall I file the budget under Spring?"));
                assert!(node.state.focusable, "the person's attention reaches it");
            }
        }
    }

    fn lay_out_at(w: &mut dyn Widget, width: f32) -> TextRenderer {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let mut renderer = TextRenderer::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                |known, available, id, _, _| {
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(known, available, &mut renderer)
                    })
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
        renderer
    }

    fn drawn_glyph_runs(w: &mut dyn Widget, renderer: &mut TextRenderer) -> Vec<u32> {
        let mut scene = Scene::new();
        w.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            renderer,
            &Theme::dark(),
            tilo_ui_base::presence::PresenceCtx::default().draw_env(),
        ));
        let encoding = scene.encoding();
        encoding
            .resources
            .glyph_runs
            .iter()
            .filter_map(|run| {
                run.glyphs
                    .end
                    .checked_sub(1)
                    .and_then(|last| encoding.resources.glyphs.get(last))
                    .map(|glyph| glyph.id)
            })
            .collect()
    }

    fn ellipsis_glyph(renderer: &mut TextRenderer, style: &TextStyle) -> u32 {
        let mut scene = Scene::new();
        renderer.draw_style(
            &mut scene,
            &tilo_ui_base::text_render::ELLIPSIS.to_string(),
            vello::peniko::Color::WHITE,
            Affine::IDENTITY,
            None,
            style,
        );
        scene.encoding().resources.glyphs[0].id
    }

    use tilo_ui_base::a11y::Role;

    use tilo_ui_base::i18n::I18nManager;

    #[test]
    fn fluent_text_interpolates_degrades_and_follows_locale() {
        {
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", "counter = Count is { $count }".to_string());

            let t = Text::new("counter").with_arg("count", 5);
            let node = Widget::accessibility(&t).expect("non-empty text is not presentational");
            // The placeholder must be replaced, not echoed literally.
            assert_eq!(node.name.as_deref(), Some("Count is 5"));
            assert!(!node.name.unwrap().contains("$count"));
        }

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

        {
            I18nManager::init("en-US");
            let t = Text::new("totally-unknown-key").with_arg("x", 1);
            let node = Widget::accessibility(&t).expect("resolved");
            assert_eq!(node.name.as_deref(), Some("totally-unknown-key"));
        }

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
                    .push(Box::new(Button::new("localized-action", "action")));
                let mut driver = crate::app::driver::Driver::new_with_controlled_inputs(
                    crate::State::new(),
                    Box::new(root),
                    (700, 300),
                    Theme::dark(),
                    tilo_ui_base::font_catalog::fixture_renderer(),
                    tilo_action_policy::MemberId::person("text-widget-locale-fixture"),
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

mod component;
pub use component::TextProps;
