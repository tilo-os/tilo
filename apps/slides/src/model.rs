//! The deck as the window draws it, with the pure logic the editor needs:
//! geometry, palette resolution and the seed deck. Nothing here touches the
//! record, the renderer or winit.

// The `from_str(s: &str) -> Self` methods here are infallible tag parsers with a
// default fallback, not `std::str::FromStr`: the trait would force a `Result`
// and ripple `?` through every call site for no behavioural gain.
#![allow(
    clippy::should_implement_trait,
    reason = "the infallible from_str parsers fall back to a default; FromStr would force a Result through every call site"
)]

use serde::{Deserialize, Serialize};
use ui::paint::with_alpha;
use ui::tilo_resource::{self, JsonSchema, Model};
use ui::tokens::colors::{PresenceHue, mix8};
use ui::widget::BlockContent;
use ui::widget::marks::{Mark, MarkSpan};
use uuid::Uuid;
use vello::peniko::Color;

/// The slide's intrinsic coordinate space: every element's `x/y/w/h` and
/// `font_size` is in these units, scaled to whatever rect paints it. 16:9.
pub const SLIDE_W: f64 = 1280.0;
pub const SLIDE_H: f64 = 720.0;

pub type Id = String;

pub fn new_id() -> Id {
    Uuid::new_v4().to_string()
}

// ===========================================================================
// Elements
// ===========================================================================

#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ElementKind {
    #[default]
    Text,
    Rect,
    Ellipse,
    Line,
    Image,
}

impl ElementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ElementKind::Text => "text",
            ElementKind::Rect => "rect",
            ElementKind::Ellipse => "ellipse",
            ElementKind::Line => "line",
            ElementKind::Image => "image",
        }
    }
    pub fn from_str(s: &str) -> ElementKind {
        match s {
            "rect" => ElementKind::Rect,
            "ellipse" => ElementKind::Ellipse,
            "line" => ElementKind::Line,
            "image" => ElementKind::Image,
            _ => ElementKind::Text,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ElementKind::Text => "Text",
            ElementKind::Rect => "Rectangle",
            ElementKind::Ellipse => "Ellipse",
            ElementKind::Line => "Line",
            ElementKind::Image => "Image",
        }
    }
}

#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

impl TextAlign {
    /// In the order the inspector shows them.
    pub const ALL: [TextAlign; 4] = [
        TextAlign::Left,
        TextAlign::Center,
        TextAlign::Right,
        TextAlign::Justify,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
            TextAlign::Justify => "justify",
        }
    }
    pub fn from_str(s: &str) -> TextAlign {
        match s {
            "center" => TextAlign::Center,
            "right" => TextAlign::Right,
            "justify" => TextAlign::Justify,
            _ => TextAlign::Left,
        }
    }
    #[must_use]
    pub fn next(self) -> TextAlign {
        match self {
            TextAlign::Left => TextAlign::Center,
            TextAlign::Center => TextAlign::Right,
            TextAlign::Right => TextAlign::Justify,
            TextAlign::Justify => TextAlign::Left,
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            TextAlign::Left => "Left",
            TextAlign::Center => "Center",
            TextAlign::Right => "Right",
            TextAlign::Justify => "Justify",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Element {
    pub id: Id,
    pub kind: ElementKind,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// A line per paragraph: what search, the outline and export read.
    pub text: String,
    /// Empty for an element built from plain `text`, which
    /// [`Self::rich_blocks`] derives.
    pub blocks: Vec<BlockContent>,
    /// Only the path travels in the record; the decoded pixels are a per-peer
    /// cache keyed by it.
    pub src: String,
    /// A colour role (see [`Palette::resolve`]); `"none"` makes a shape
    /// outline-only.
    pub fill: String,
    pub stroke: String,
    pub stroke_width: f64,
    pub font_size: f64,
    pub align: TextAlign,
    pub bold: bool,
    pub italic: bool,
    /// In slide units (rect elements).
    pub radius: f64,
    pub z: i64,
}

impl Element {
    pub fn text(text: &str, x: f64, y: f64, w: f64, h: f64, font_size: f64) -> Element {
        let mut element = Element::of_kind(new_id(), ElementKind::Text, x, y, w, h);
        element.text = text.to_string();
        element.font_size = font_size;
        element
    }

    pub fn shape(kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element::shape_with_id(new_id(), kind, x, y, w, h)
    }

    pub fn image(src: &str, x: f64, y: f64, w: f64, h: f64) -> Element {
        let mut element = Element::of_kind(new_id(), ElementKind::Image, x, y, w, h);
        element.src = src.to_string();
        element
    }

    /// Taking the id lets an action create an element whose id comes from its
    /// own context.
    pub fn of_kind(id: Id, kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> Element {
        let mut element = Element::shape_with_id(id, kind, x, y, w, h);
        match kind {
            ElementKind::Text => element.fill = "text".into(),
            ElementKind::Image => {
                element.fill = "surface".into();
                element.radius = 12.0;
            }
            ElementKind::Rect | ElementKind::Ellipse | ElementKind::Line => {}
        }
        element
    }

    fn shape_with_id(id: Id, kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element {
            id,
            kind,
            x,
            y,
            w,
            h,
            text: String::new(),
            blocks: Vec::new(),
            src: String::new(),
            fill: if kind == ElementKind::Line {
                "none".into()
            } else {
                "accent".into()
            },
            stroke: if kind == ElementKind::Line {
                "accent".into()
            } else {
                "none".into()
            },
            stroke_width: if kind == ElementKind::Line { 6.0 } else { 0.0 },
            font_size: 32.0,
            align: TextAlign::Left,
            bold: false,
            italic: false,
            radius: if kind == ElementKind::Rect { 16.0 } else { 0.0 },
            z: 0,
        }
    }

    /// Whether a text box has no words yet.
    pub fn is_blank_text(&self) -> bool {
        self.kind == ElementKind::Text
            && self.text.trim().is_empty()
            && self.blocks.iter().all(|block| block.text.trim().is_empty())
    }

    /// The text as written: its blocks, or for an element built from plain
    /// text, a paragraph per line carrying the element's bold and italic.
    pub fn rich_blocks(&self) -> Vec<BlockContent> {
        if !self.blocks.is_empty() {
            return self.blocks.clone();
        }
        paragraphs_of(&self.text, self.bold, self.italic)
    }

    /// The plain text is kept in step. The blocks' document ids are dropped: the
    /// model shows words, the document names its paragraphs.
    pub fn set_blocks(&mut self, mut blocks: Vec<BlockContent>) {
        for block in &mut blocks {
            block.id.clear();
        }
        self.text = plain_text_of(&blocks);
        self.bold = !blocks.is_empty() && covered_by(&blocks, &Mark::Bold);
        self.italic = !blocks.is_empty() && covered_by(&blocks, &Mark::Italic);
        self.blocks = blocks;
    }

    #[must_use]
    pub fn with(mut self, f: impl FnOnce(&mut Element)) -> Element {
        f(&mut self);
        self
    }

    #[must_use]
    pub fn center_x(mut self) -> Element {
        self.align = TextAlign::Center;
        self
    }
}

pub fn paragraphs_of(text: &str, bold: bool, italic: bool) -> Vec<BlockContent> {
    text.split('\n')
        .map(|line| {
            let mut block = BlockContent::paragraph("", line);
            if !line.is_empty() {
                for (on, mark) in [(bold, Mark::Bold), (italic, Mark::Italic)] {
                    if on {
                        block.marks.push(MarkSpan::new(0..line.len(), mark));
                    }
                }
            }
            block
        })
        .collect()
}

/// `(text, bold, italic)` in order, the way presentation formats store text.
pub fn runs_of(block: &BlockContent) -> Vec<(&str, bool, bool)> {
    let styled = |mark: &Mark, at: usize| {
        block
            .marks
            .iter()
            .any(|span| &span.mark == mark && span.range.start <= at && at < span.range.end)
    };
    let mut cuts: Vec<usize> = vec![0, block.text.len()];
    for span in &block.marks {
        if matches!(span.mark, Mark::Bold | Mark::Italic) {
            cuts.push(span.range.start.min(block.text.len()));
            cuts.push(span.range.end.min(block.text.len()));
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter_map(|pair| {
            let [start, end] = pair else { return None };
            let text = block.text.get(*start..*end)?;
            (!text.is_empty()).then(|| {
                (
                    text,
                    styled(&Mark::Bold, *start),
                    styled(&Mark::Italic, *start),
                )
            })
        })
        .collect()
}

pub fn push_run(block: &mut BlockContent, text: &str, bold: bool, italic: bool) {
    let start = block.text.len();
    block.text.push_str(text);
    let end = block.text.len();
    if start == end {
        return;
    }
    for (on, mark) in [(bold, Mark::Bold), (italic, Mark::Italic)] {
        if !on {
            continue;
        }
        // A run continuing the one before extends its mark.
        match block
            .marks
            .iter_mut()
            .find(|span| span.mark == mark && span.range.end == start)
        {
            Some(span) => span.range.end = end,
            None => block.marks.push(MarkSpan::new(start..end, mark)),
        }
    }
}

pub fn plain_text_of(blocks: &[BlockContent]) -> String {
    blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn covered_by(blocks: &[BlockContent], mark: &Mark) -> bool {
    blocks.iter().all(|block| {
        let mut covered = 0;
        let mut spans: Vec<_> = block
            .marks
            .iter()
            .filter(|span| &span.mark == mark)
            .map(|span| span.range.clone())
            .collect();
        spans.sort_by_key(|range| range.start);
        for range in spans {
            if range.start > covered {
                return false;
            }
            covered = covered.max(range.end);
        }
        covered >= block.text.len()
    })
}

/// Equal when they show the same words with the same marks, however those were
/// written down.
impl PartialEq for Element {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.kind == other.kind
            && self.x == other.x
            && self.y == other.y
            && self.w == other.w
            && self.h == other.h
            && self.text == other.text
            && self.src == other.src
            && self.fill == other.fill
            && self.stroke == other.stroke
            && self.stroke_width == other.stroke_width
            && self.font_size == other.font_size
            && self.align == other.align
            && self.radius == other.radius
            && self.z == other.z
            && if self.blocks.is_empty() && other.blocks.is_empty() {
                self.bold == other.bold && self.italic == other.italic
            } else {
                self.rich_blocks() == other.rich_blocks()
            }
    }
}

// ===========================================================================
// Slides + layouts
// ===========================================================================

#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Title,
    #[default]
    TitleContent,
    TwoColumn,
    Section,
    Blank,
}

impl Layout {
    pub const ALL: [Layout; 5] = [
        Layout::Title,
        Layout::TitleContent,
        Layout::TwoColumn,
        Layout::Section,
        Layout::Blank,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Layout::Title => "title",
            Layout::TitleContent => "title-content",
            Layout::TwoColumn => "two-column",
            Layout::Section => "section",
            Layout::Blank => "blank",
        }
    }
    pub fn from_str(s: &str) -> Layout {
        match s {
            "title" => Layout::Title,
            "two-column" => Layout::TwoColumn,
            "section" => Layout::Section,
            "blank" => Layout::Blank,
            _ => Layout::TitleContent,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Layout::Title => "Title",
            Layout::TitleContent => "Title and body",
            Layout::TwoColumn => "Two columns",
            Layout::Section => "Section",
            Layout::Blank => "Blank",
        }
    }
    /// A full-bleed accent background, as section and title covers have.
    pub fn is_cover(self) -> bool {
        matches!(self, Layout::Section | Layout::Title)
    }
}

/// Resolved against the palette at draw time.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    #[default]
    Surface,
    Accent,
    Deep,
}

impl Background {
    pub const ALL: [Background; 3] = [Background::Surface, Background::Accent, Background::Deep];
    pub fn as_str(self) -> &'static str {
        match self {
            Background::Surface => "surface",
            Background::Accent => "accent",
            Background::Deep => "deep",
        }
    }
    pub fn from_str(s: &str) -> Background {
        match s {
            "accent" => Background::Accent,
            "deep" => Background::Deep,
            _ => Background::Surface,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Background::Surface => "Surface",
            Background::Accent => "Accent",
            Background::Deep => "Deep",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Slide {
    pub id: Id,
    pub layout: Layout,
    pub background: Background,
    pub notes: String,
    pub elements: Vec<Element>,
}

impl Slide {
    pub fn new(layout: Layout) -> Slide {
        Slide {
            id: new_id(),
            layout,
            background: if layout.is_cover() {
                Background::Accent
            } else {
                Background::Surface
            },
            notes: String::new(),
            elements: Vec::new(),
        }
    }

    pub fn next_z(&self) -> i64 {
        self.elements.iter().map(|e| e.z).max().unwrap_or(0) + 1
    }

    pub fn add(&mut self, mut el: Element) -> Id {
        el.z = self.next_z();
        let id = el.id.clone();
        self.elements.push(el);
        id
    }

    pub fn element(&self, id: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }
    pub fn element_mut(&mut self, id: &str) -> Option<&mut Element> {
        self.elements.iter_mut().find(|e| e.id == id)
    }

    /// Back to front.
    pub fn z_sorted(&self) -> Vec<&Element> {
        let mut v: Vec<&Element> = self.elements.iter().collect();
        v.sort_by(|a, b| a.z.cmp(&b.z).then_with(|| a.id.cmp(&b.id)));
        v
    }
}

// ===========================================================================
// Deck + palette
// ===========================================================================

/// Every colour still comes from the OS [`Theme`](ui::theme::Theme); the palette
/// only chooses which token drives the accent.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaletteKind {
    #[default]
    Indigo,
    Emerald,
    Amber,
    Rose,
    Sky,
}

impl PaletteKind {
    pub const ALL: [PaletteKind; 5] = [
        PaletteKind::Indigo,
        PaletteKind::Emerald,
        PaletteKind::Amber,
        PaletteKind::Rose,
        PaletteKind::Sky,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            PaletteKind::Indigo => "indigo",
            PaletteKind::Emerald => "emerald",
            PaletteKind::Amber => "amber",
            PaletteKind::Rose => "rose",
            PaletteKind::Sky => "sky",
        }
    }
    pub fn from_str(s: &str) -> PaletteKind {
        match s {
            "emerald" => PaletteKind::Emerald,
            "amber" => PaletteKind::Amber,
            "rose" => PaletteKind::Rose,
            "sky" => PaletteKind::Sky,
            _ => PaletteKind::Indigo,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::Indigo => "Indigo",
            PaletteKind::Emerald => "Emerald",
            PaletteKind::Amber => "Amber",
            PaletteKind::Rose => "Rose",
            PaletteKind::Sky => "Sky",
        }
    }
    #[must_use]
    pub fn next(self) -> PaletteKind {
        let all = PaletteKind::ALL;
        let i = all.iter().position(|p| *p == self).unwrap_or(0);
        all.get((i + 1) % all.len()).copied().unwrap_or(self)
    }
}

/// The deck's surfaces and inks, stored with the deck, so it looks the same on
/// every screen, in Present and in a saved file, whatever the reader's theme.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeckLook {
    #[default]
    Light,
    Dark,
}

impl DeckLook {
    pub const ALL: [DeckLook; 2] = [DeckLook::Light, DeckLook::Dark];
    pub fn as_str(self) -> &'static str {
        match self {
            DeckLook::Light => "light",
            DeckLook::Dark => "dark",
        }
    }
    pub fn from_str(s: &str) -> DeckLook {
        match s {
            "dark" => DeckLook::Dark,
            _ => DeckLook::Light,
        }
    }
    #[must_use]
    pub fn label(self) -> String {
        crate::strings::tr(match self {
            DeckLook::Light => "slides-look-light",
            DeckLook::Dark => "slides-look-dark",
        })
    }
    #[must_use]
    pub fn colors(self) -> ui::tokens::Colors {
        match self {
            DeckLook::Light => ui::tokens::Colors::light(),
            DeckLook::Dark => ui::tokens::Colors::dark(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deck {
    pub title: String,
    pub palette: PaletteKind,
    pub look: DeckLook,
    pub slides: Vec<Slide>,
}

impl Deck {
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.slides.iter().position(|s| s.id == id)
    }
}

/// Resolved for the deck's own [`DeckLook`], never the reader's theme; concrete
/// colours, so the renderer never re-derives them per element.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub accent: Color,
    pub accent_soft: Color,
    pub accent_deep: Color,
    pub surface: Color,
    pub surface_dim: Color,
    pub deep: Color,
    pub text: Color,
    pub text_dim: Color,
    pub on_accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    /// An image that could not be loaded.
    pub failed: Color,
    /// The deck's words lay out and paint against it
    /// ([`crate::render::words_theme`]).
    pub look: DeckLook,
    /// For each slide background, in [`Background::ALL`] order. See
    /// [`Palette::on`].
    inks: [(Color, Color); 3],
}

/// An authored dark tone in every OS theme: a theme-tracking neutral would flip
/// near-white in the light palette.
const DEEP: Color = Color::from_rgb8(15, 23, 42);

/// A deck's colours are content: the presence palette's evenly spaced hues,
/// never the status colours, and taken from the light palette (deep enough for
/// white type) so a deck looks the same in either theme.
static DECK_HUES: std::sync::LazyLock<ui::tokens::colors::PresencePalette> =
    std::sync::LazyLock::new(|| ui::tokens::Colors::light().presence);

impl Palette {
    pub fn resolve(kind: PaletteKind, look: DeckLook) -> Palette {
        let colors = look.colors();
        let c = &colors;
        let hue = |h: PresenceHue| DECK_HUES.hue(h);
        let accent = match kind {
            PaletteKind::Indigo => hue(PresenceHue::Blue),
            PaletteKind::Emerald => hue(PresenceHue::Green),
            PaletteKind::Amber => hue(PresenceHue::Amber),
            PaletteKind::Rose => hue(PresenceHue::Pink),
            PaletteKind::Sky => hue(PresenceHue::Sky),
        };
        let mut palette = Palette {
            accent,
            accent_soft: with_alpha(accent, 56),
            accent_deep: mix8(accent, DEEP, 0.62),
            surface: c.surfaces.raised,
            surface_dim: mix8(c.surfaces.raised, c.surfaces.content, 0.5),
            // A fixed deep tone: a theme-tracking neutral flips near-white in
            // the light palette. Its text inks are chosen against it below.
            deep: DEEP,
            text: c.text_primary,
            text_dim: c.text_dim,
            // White type on the accent, as slides set it large.
            on_accent: c.ink_on_any(
                &[accent],
                Color::WHITE,
                ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
            ),
            success: hue(PresenceHue::Green),
            warning: hue(PresenceHue::Amber),
            danger: hue(PresenceHue::Red),
            info: hue(PresenceHue::Violet),
            failed: c.semantic.danger,
            look,
            inks: [(c.text_primary, c.text_dim); 3],
        };
        // A slide background can be the opposite of the look's own surfaces (the
        // deep tone under the light look's near-black text measured 1.05:1), so
        // resolve `text` and `text-dim` per background: the theme's ink where it
        // clears WCAG AA, else the legible ink, and a softened dim ink.
        palette.inks = Background::ALL.map(|background| {
            let ground = palette.slide_bg(background);
            let text = c.readable_on(ground, c.text_primary);
            let preferred_dim = if text == c.text_primary {
                c.text_dim
            } else {
                mix8(text, ground, 0.3)
            };
            (text, c.readable_on(ground, preferred_dim))
        });
        palette
    }

    /// `text` and `text-dim` become the inks that contrast with `bg`, so text in
    /// a theme text role stays legible when its slide switches treatment. Every
    /// other role is unchanged.
    #[must_use]
    pub fn on(&self, bg: Background) -> Palette {
        let index = Background::ALL
            .iter()
            .position(|candidate| *candidate == bg)
            .unwrap_or(0);
        let (text, text_dim) = self
            .inks
            .get(index)
            .copied()
            .unwrap_or((self.text, self.text_dim));
        Palette {
            text,
            text_dim,
            ..*self
        }
    }

    /// `None` for `"none"`, so callers skip the fill or stroke. A named role
    /// resolves against the deck's palette and look; a `#RRGGBB` hex is that
    /// exact colour, which is what an `.odp` round-trips faithfully.
    pub fn resolve_role(&self, role: &str) -> Option<Color> {
        if role == "none" {
            return None;
        }
        if role.starts_with('#') {
            return ui::color::parse_hex(role).or(Some(self.accent));
        }
        Some(match role {
            "none" => return None,
            // Falls through to the wildcard: an unrecognised role and "accent"
            // land on the same colour.
            "accent-soft" => self.accent_soft,
            "accent-deep" => self.accent_deep,
            "surface" => self.surface,
            "surface-dim" => self.surface_dim,
            "deep" => self.deep,
            "text" => self.text,
            "text-dim" => self.text_dim,
            "on-accent" => self.on_accent,
            "white" => Color::WHITE,
            "success" => self.success,
            "warning" => self.warning,
            "danger" => self.danger,
            "info" => self.info,
            _ => self.accent,
        })
    }

    pub fn slide_bg(&self, bg: Background) -> Color {
        match bg {
            Background::Surface => self.surface,
            Background::Accent => self.accent,
            Background::Deep => self.deep,
        }
    }

    /// The slide's own ink where it clears AA on `ground`, else whichever of the
    /// palette's inks reads best.
    pub fn ink_on(&self, ground: Color, dim: bool) -> Color {
        use ui::a11y::contrast_ratio;
        use ui::tokens::colors::ON_FILL_MIN_CONTRAST as AA;
        let text = if contrast_ratio(self.text, ground) >= AA {
            self.text
        } else {
            std::iter::once(self.text)
                .chain(self.inks.iter().map(|(ink, _)| *ink))
                .max_by(|a, b| contrast_ratio(*a, ground).total_cmp(&contrast_ratio(*b, ground)))
                .unwrap_or(self.text)
        };
        if !dim {
            return text;
        }
        if text == self.text && contrast_ratio(self.text_dim, ground) >= AA {
            return self.text_dim;
        }
        let soft = mix8(text, ground, 0.3);
        if contrast_ratio(soft, ground) >= AA {
            soft
        } else {
            text
        }
    }
}

/// `surface` and `text` sit at opposite ends whichever theme is showing, and the
/// rest are the deck's hues.
pub const FILL_ROLES: [&str; 8] = [
    "accent",
    "accent-soft",
    "surface",
    "text",
    "success",
    "warning",
    "danger",
    "info",
];
pub const STROKE_ROLES: [&str; 7] = [
    "none", "text", "accent", "success", "warning", "danger", "info",
];

// ===========================================================================
// Starter deck — the minimal blank document a *live* launch opens with.
// ===========================================================================

/// A single blank title slide, so a live launch never opens on fabricated
/// content; the real deck comes from the record.
pub fn starter_deck() -> Deck {
    let mut s = Slide::new(Layout::Title);
    s.background = Background::Surface;
    // Empty boxes: the editor prompts, a presentation shows nothing.
    s.add(Element::text("", 96.0, 250.0, 1088.0, 130.0, 88.0).with(|e| e.bold = true));
    s.add(
        Element::text("", 96.0, 386.0, 1088.0, 70.0, 34.0).with(|e| {
            e.fill = "text-dim".into();
        }),
    );
    Deck {
        title: "Untitled deck".into(),
        palette: PaletteKind::Indigo,
        look: DeckLook::default(),
        slides: vec![s],
    }
}

// ===========================================================================
// Sample deck — a designed, multi-slide deck used by `--shots` and tests.
// ===========================================================================

/// Only for headless screenshots and tests; never loaded on a live launch (see
/// [`starter_deck`]).
pub fn seed_deck() -> Deck {
    Deck {
        title: "Riverside Park — concept review".into(),
        palette: PaletteKind::Indigo,
        look: DeckLook::default(),
        slides: vec![
            seed_slide_cover(),
            seed_slide_goals(),
            seed_slide_two_column(),
            seed_slide_diagram(),
            seed_slide_closing(),
        ],
    }
}

fn seed_slide_cover() -> Slide {
    let mut s = Slide::new(Layout::Section);
    s.background = Background::Accent;
    s.notes = "Set the scene. One line on why we're renewing the park and who \
               the workshop is for."
        .into();
    s.add(
        Element::text("Concept review", 110.0, 250.0, 700.0, 70.0, 28.0).with(|e| {
            e.fill = "on-accent".into();
            e.bold = true;
        }),
    );
    s.add(
        Element::text(
            "Riverside Park\nrenewal plan",
            105.0,
            300.0,
            900.0,
            220.0,
            72.0,
        )
        .with(|e| {
            e.fill = "white".into();
            e.bold = true;
        }),
    );
    s.add(
        Element::text(
            "Design workshop, spring review",
            110.0,
            500.0,
            900.0,
            60.0,
            30.0,
        )
        .with(|e| {
            e.fill = "on-accent".into();
        }),
    );
    s.add(
        Element::shape(ElementKind::Rect, 110.0, 222.0, 92.0, 8.0).with(|e| {
            e.fill = "white".into();
            e.radius = 4.0;
        }),
    );
    s
}

fn seed_slide_goals() -> Slide {
    let mut s = Slide::new(Layout::TitleContent);
    s.background = Background::Surface;
    s.notes = "Walk the four goals. Spend the most time on protecting the \
               wetland — that's the constraint everything else works around."
        .into();
    s.add(
        Element::text("Goals for this phase", 96.0, 70.0, 1088.0, 76.0, 56.0).with(|e| {
            e.bold = true;
        }),
    );
    s.add(
        Element::shape(ElementKind::Rect, 96.0, 168.0, 150.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }),
    );
    // A bulleted list, as a person makes one.
    s.add(
        Element::text("", 96.0, 210.0, 720.0, 440.0, 34.0).with(|e| {
            e.set_blocks(
                [
                    "Reconnect the waterfront path",
                    "Add shade, seating and lighting",
                    "Protect the existing wetland",
                    "Keep the budget under review",
                ]
                .into_iter()
                .map(|goal| {
                    let mut item = BlockContent::paragraph("", goal);
                    item.kind = ui::widget::BlockKind::Bullet { indent: 0 };
                    item
                })
                .collect(),
            );
        }),
    );
    s.add(
        Element::shape(ElementKind::Rect, 880.0, 230.0, 300.0, 300.0).with(|e| {
            e.fill = "accent-soft".into();
            e.radius = 28.0;
        }),
    );
    s.add(
        Element::shape(ElementKind::Ellipse, 940.0, 290.0, 180.0, 180.0).with(|e| {
            e.fill = "accent".into();
        }),
    );
    s.add(
        Element::text("Q3", 940.0, 350.0, 180.0, 80.0, 52.0).with(|e| {
            e.fill = "white".into();
            e.bold = true;
            e.align = TextAlign::Center;
        }),
    );
    s
}

fn seed_slide_two_column() -> Slide {
    let mut s = Slide::new(Layout::TwoColumn);
    s.background = Background::Surface;
    s.notes = "Two path options. Ask the room which trade-off they prefer \
               before we cost them out."
        .into();
    s.add(
        Element::text("Two options to compare", 96.0, 70.0, 1088.0, 76.0, 56.0)
            .with(|e| e.bold = true),
    );
    s.add(
        Element::shape(ElementKind::Rect, 96.0, 168.0, 150.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }),
    );
    s.add(
        Element::shape(ElementKind::Rect, 96.0, 220.0, 520.0, 400.0).with(|e| {
            e.fill = "surface-dim".into();
            e.stroke = "accent".into();
            e.stroke_width = 3.0;
            e.radius = 24.0;
        }),
    );
    s.add(
        Element::text("Option A", 128.0, 250.0, 460.0, 50.0, 28.0).with(|e| {
            e.fill = "accent".into();
            e.bold = true;
        }),
    );
    s.add(Element::text(
        "A boardwalk along the east\nbank with a single wide\npromenade and open views.",
        128.0,
        312.0,
        460.0,
        240.0,
        30.0,
    ));
    s.add(
        Element::shape(ElementKind::Rect, 664.0, 220.0, 520.0, 400.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 24.0;
        }),
    );
    s.add(
        Element::text("Option B", 696.0, 250.0, 460.0, 50.0, 28.0).with(|e| {
            e.fill = "white".into();
            e.bold = true;
        }),
    );
    s.add(
        Element::text(
            "A split path with a quiet\ngarden loop inland and a\nterrace at the water's edge.",
            696.0,
            312.0,
            460.0,
            240.0,
            30.0,
        )
        .with(|e| e.fill = "on-accent".into()),
    );
    s
}

fn seed_slide_diagram() -> Slide {
    let mut s = Slide::new(Layout::Blank);
    s.background = Background::Deep;
    s.notes = "The route at a glance: north gate, through the plaza, out to \
               the pier. Trace it as you talk."
        .into();
    s.add(
        Element::text("How the paths connect", 96.0, 70.0, 1088.0, 90.0, 52.0).with(|e| {
            e.fill = "white".into();
            e.bold = true;
        }),
    );
    s.add(
        Element::shape(ElementKind::Ellipse, 150.0, 320.0, 230.0, 230.0).with(|e| {
            e.fill = "accent".into();
        }),
    );
    s.add(
        Element::text("North\ngate", 150.0, 385.0, 230.0, 110.0, 34.0).with(|e| {
            e.fill = "white".into();
            e.bold = true;
            e.align = TextAlign::Center;
        }),
    );
    s.add(
        Element::shape(ElementKind::Line, 380.0, 435.0, 280.0, 0.0).with(|e| {
            e.stroke = "accent-soft".into();
            e.stroke_width = 8.0;
        }),
    );
    s.add(
        Element::shape(ElementKind::Ellipse, 660.0, 320.0, 230.0, 230.0).with(|e| {
            e.fill = "surface".into();
            e.stroke = "accent".into();
            e.stroke_width = 4.0;
        }),
    );
    s.add(
        Element::text("Plaza", 660.0, 405.0, 230.0, 60.0, 36.0).with(|e| {
            e.bold = true;
            e.align = TextAlign::Center;
        }),
    );
    s.add(
        Element::shape(ElementKind::Line, 890.0, 435.0, 160.0, 0.0).with(|e| {
            e.stroke = "accent-soft".into();
            e.stroke_width = 8.0;
        }),
    );
    s.add(
        Element::shape(ElementKind::Rect, 1050.0, 360.0, 150.0, 150.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 20.0;
        }),
    );
    s.add(
        Element::text("Pier", 1050.0, 405.0, 150.0, 60.0, 34.0).with(|e| {
            e.fill = "white".into();
            e.bold = true;
            e.align = TextAlign::Center;
        }),
    );
    s
}

fn seed_slide_closing() -> Slide {
    let mut s = Slide::new(Layout::Section);
    s.background = Background::Deep;
    s.notes = "Close on the ask: feedback by Friday, prototype in April. \
               Leave the last slide up for questions."
        .into();
    s.add(
        Element::shape(ElementKind::Rect, 110.0, 300.0, 92.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }),
    );
    s.add(
        Element::text("Questions & next steps", 105.0, 330.0, 1000.0, 110.0, 64.0).with(|e| {
            e.fill = "white".into();
            e.bold = true;
        }),
    );
    s.add(
        Element::text(
            "Share feedback by Friday, prototype in April",
            110.0,
            450.0,
            1000.0,
            60.0,
            30.0,
        )
        .with(|e| {
            e.fill = "text-dim".into();
        }),
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_model_seeds_orders_and_round_trips_kinds() {
        {
            let d = seed_deck();
            assert_eq!(d.slides.len(), 5);
            for s in &d.slides {
                assert!(!s.elements.is_empty(), "slide {:?} empty", s.layout);
            }
        }

        {
            let mut s = Slide::new(Layout::Blank);
            let a = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 10.0, 10.0));
            let b = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 10.0, 10.0));
            assert!(s.element(&a).unwrap().z < s.element(&b).unwrap().z);
        }

        {
            for k in [
                ElementKind::Text,
                ElementKind::Rect,
                ElementKind::Ellipse,
                ElementKind::Line,
                ElementKind::Image,
            ] {
                assert_eq!(ElementKind::from_str(k.as_str()), k);
            }
            for a in [TextAlign::Left, TextAlign::Center, TextAlign::Right] {
                assert_eq!(TextAlign::from_str(a.as_str()), a);
            }
        }

        {
            let p = Palette::resolve(PaletteKind::Indigo, DeckLook::Dark);
            assert!(p.resolve_role("none").is_none());
            assert!(p.resolve_role("accent").is_some());
        }
    }

    /// Every text element a template creates clears WCAG AA on every background,
    /// under every accent palette, in both looks: 4.5:1 for body, 3:1 for
    /// titles.
    #[test]
    fn template_text_contrasts_with_every_background_in_both_themes() {
        let mut templates: Vec<(String, Element)> = Vec::new();
        for (name, deck) in [("starter deck", starter_deck())] {
            for slide in deck.slides {
                for element in slide.elements {
                    templates.push((format!("{name}: {:?}", element.text), element));
                }
            }
        }
        for element in crate::actions::new_slide_template().elements {
            templates.push((format!("new slide: {:?}", element.text), element));
        }
        templates.push((
            "inserted text box".into(),
            crate::actions::inserted_element_template(ElementKind::Text),
        ));
        let texts: Vec<&(String, Element)> = templates
            .iter()
            .filter(|(_, element)| element.kind == ElementKind::Text)
            .collect();
        assert!(
            texts.len() >= 5,
            "the templates carry their text: {texts:?}"
        );

        let mut failures = Vec::new();
        for look in DeckLook::ALL {
            let theme_name = look.as_str();
            for kind in PaletteKind::ALL {
                let palette = Palette::resolve(kind, look);
                for background in Background::ALL {
                    let on = palette.on(background);
                    let ground = on.slide_bg(background);
                    for (name, element) in &texts {
                        let ink = on.resolve_role(&element.fill).unwrap_or(on.text);
                        let ratio = ui::color::contrast_ratio(ink, ground);
                        let title = element.bold && element.font_size >= 24.0;
                        let minimum = if title { 3.0 } else { 4.5 };
                        if ratio < minimum {
                            failures.push(format!(
                                "{name} on {} ({}, {theme_name}): {ratio:.2}:1 < {minimum}:1",
                                background.as_str(),
                                kind.as_str(),
                            ));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The theme's text role on a slide switched to the deep background, in the
    /// light look.
    #[test]
    fn theme_text_on_a_deep_slide_is_light_in_the_light_look() {
        let palette = Palette::resolve(PaletteKind::Indigo, DeckLook::Light);
        let deep = palette.slide_bg(Background::Deep);
        assert!(ui::color::contrast_ratio(palette.text, deep) < 1.5);
        let on = palette.on(Background::Deep);
        for role in ["text", "text-dim"] {
            let ink = on.resolve_role(role).unwrap();
            assert!(ui::color::contrast_ratio(ink, deep) >= 4.5, "{role}");
        }
        let surface = palette.on(Background::Surface);
        assert_eq!(surface.text, palette.text);
        assert_eq!(surface.text_dim, palette.text_dim);
    }
}
