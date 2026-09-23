//! Pure data model for a Slides deck.
//!
//! The on-wire source of truth lives in the Automerge document (see
//! [`crate::doc`]); this module is the in-memory mirror plus the pure
//! logic the editor needs (geometry, palette resolution, the seed deck).
//! Nothing here touches the CRDT, the renderer, or winit — it is unit
//! testable in isolation.

// This module's several `from_str(s: &str) -> Self` methods are deliberately
// infallible tag parsers with a default fallback (an unknown/legacy document
// tag maps to a sensible variant rather than erroring) -- a different
// contract from `std::str::FromStr::from_str`, which returns `Result`.
// Implementing the trait would force a `Result` return and ripple
// `.parse()`/`?` handling through every call site for no behavioural
// benefit, so the name collision is accepted here rather than exempted
// call-by-call.
#![allow(clippy::should_implement_trait)]

use ui::ColorCompat;
use ui::paint::with_alpha;
use uuid::Uuid;
use vello::peniko::Color;

/// The slide's intrinsic coordinate space. Every element's `x/y/w/h` and
/// `font_size` is expressed in these units; the renderer scales the whole
/// space to whatever destination rect it is painting into (canvas,
/// thumbnail, or full-screen present). 16:9.
pub const SLIDE_W: f64 = 1280.0;
pub const SLIDE_H: f64 = 720.0;

pub type Id = String;

pub fn new_id() -> Id {
    Uuid::new_v4().to_string()
}

// ===========================================================================
// Elements
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementKind {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
    Justify,
}

impl TextAlign {
    /// Every alignment, in the order the inspector's segmented control shows
    /// them (left · center · right · justify).
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

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub id: Id,
    pub kind: ElementKind,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Text body (text elements). May contain `\n` for explicit line
    /// breaks; each line is aligned independently.
    pub text: String,
    /// Source path of an image element's picture on disk. Empty for every
    /// non-image element. Only the path travels in the CRDT; the decoded
    /// pixels are a per-peer runtime cache keyed by this path.
    pub src: String,
    /// Colour *role* (see [`Palette::resolve`]) for the fill. `"none"`
    /// means no fill (shapes become outline-only).
    pub fill: String,
    /// Colour role for the stroke / outline. `"none"` disables it.
    pub stroke: String,
    pub stroke_width: f64,
    pub font_size: f64,
    pub align: TextAlign,
    pub bold: bool,
    pub italic: bool,
    /// Corner radius (rect elements), in slide units.
    pub radius: f64,
    pub z: i64,
}

impl Element {
    pub fn text(text: &str, x: f64, y: f64, w: f64, h: f64, font_size: f64) -> Element {
        Element {
            id: new_id(),
            kind: ElementKind::Text,
            x,
            y,
            w,
            h,
            text: text.to_string(),
            src: String::new(),
            fill: "text".into(),
            stroke: "none".into(),
            stroke_width: 0.0,
            font_size,
            align: TextAlign::Left,
            bold: false,
            italic: false,
            radius: 0.0,
            z: 0,
        }
    }

    pub fn shape(kind: ElementKind, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element {
            id: new_id(),
            kind,
            x,
            y,
            w,
            h,
            text: String::new(),
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

    /// An image element backed by the picture at `src` (a filesystem path).
    /// The pixels are decoded lazily off the UI thread and cached by path.
    pub fn image(src: &str, x: f64, y: f64, w: f64, h: f64) -> Element {
        Element {
            id: new_id(),
            kind: ElementKind::Image,
            x,
            y,
            w,
            h,
            text: String::new(),
            src: src.to_string(),
            fill: "surface".into(),
            stroke: "none".into(),
            stroke_width: 0.0,
            font_size: 32.0,
            align: TextAlign::Left,
            bold: false,
            italic: false,
            radius: 12.0,
            z: 0,
        }
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

// ===========================================================================
// Slides + layouts
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Title,
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
            Layout::TitleContent => "Title + Body",
            Layout::TwoColumn => "Two Column",
            Layout::Section => "Section",
            Layout::Blank => "Blank",
        }
    }
    /// Whether the slide paints a full-bleed accent background (section &
    /// title cover slides) versus a neutral content surface.
    pub fn is_cover(self) -> bool {
        matches!(self, Layout::Section | Layout::Title)
    }
}

/// Per-slide background treatment, resolved against the palette at draw
/// time. Keeps slides theme-driven while still allowing variety.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Background {
    /// Neutral content surface (raised panel).
    Surface,
    /// Full accent wash — used for section / cover slides.
    Accent,
    /// Deep, near-black accent tint.
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

    /// Next z value for a newly added element (one above the current top).
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

    /// Elements ordered back-to-front for painting.
    pub fn z_sorted(&self) -> Vec<&Element> {
        let mut v: Vec<&Element> = self.elements.iter().collect();
        v.sort_by(|a, b| a.z.cmp(&b.z).then_with(|| a.id.cmp(&b.id)));
        v
    }
}

// ===========================================================================
// Deck + palette
// ===========================================================================

/// Named accent palettes a deck can adopt. Every colour is still pulled
/// from the OS [`Theme`](ui::theme::Theme) so the deck respects the
/// system look — the palette only chooses *which* token drives the accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
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

#[derive(Clone, Debug, PartialEq)]
pub struct Deck {
    pub title: String,
    pub palette: PaletteKind,
    pub slides: Vec<Slide>,
}

impl Deck {
    pub fn slide(&self, idx: usize) -> Option<&Slide> {
        self.slides.get(idx)
    }
    pub fn slide_mut(&mut self, idx: usize) -> Option<&mut Slide> {
        self.slides.get_mut(idx)
    }
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.slides.iter().position(|s| s.id == id)
    }
}

/// A palette resolved against the live [`Theme`](ui::theme::Theme). Holds
/// concrete colours so the renderer never re-derives them per element.
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
}

impl Palette {
    /// Build the resolved palette for `kind` against `theme`.
    pub fn resolve(kind: PaletteKind, theme: &ui::theme::Theme) -> Palette {
        let c = &theme.tokens.colors;
        let accent = match kind {
            PaletteKind::Indigo => theme.accent_primary,
            PaletteKind::Emerald => c.semantic.success,
            PaletteKind::Amber => c.semantic.warning,
            PaletteKind::Rose => c.semantic.danger,
            PaletteKind::Sky => c.semantic.info,
        };
        Palette {
            accent,
            accent_soft: with_alpha(accent, 56),
            accent_deep: mix(accent, c.surfaces.base, 0.62),
            surface: c.surfaces.raised,
            surface_dim: mix(c.surfaces.raised, c.surfaces.base, 0.5),
            // A "Deep" slide is an authored dark treatment whose ink is fixed
            // white (`on_bg`), so it must stay dark in every OS theme — a
            // theme-tracking neutral flips near-white in the light palette and
            // washes the white text out. A fixed deep tone keeps it dramatic.
            deep: Color::from_rgb8(15, 23, 42),
            text: c.text_primary,
            text_dim: c.text_dim,
            on_accent: Color::WHITE,
            success: c.semantic.success,
            warning: c.semantic.warning,
            danger: c.semantic.danger,
            info: c.semantic.info,
        }
    }

    /// Resolve a colour *role* string to a concrete colour. Returns `None`
    /// for `"none"` so callers can skip the fill/stroke entirely.
    ///
    /// Two kinds of value are accepted: a named theme *role* (`"accent"`,
    /// `"surface"`, …), resolved against the live palette so the deck tracks
    /// the OS theme; or a literal `#RRGGBB` / `#RRGGBBAA` hex chosen with the
    /// inspector colour picker, which resolves to that exact colour regardless
    /// of theme. Hex is what lets a user pick an arbitrary fill and what a
    /// `LibreOffice` `.odp` round-trips faithfully.
    pub fn resolve_role(&self, role: &str) -> Option<Color> {
        if role == "none" {
            return None;
        }
        if role.starts_with('#') {
            return ui::color::parse_hex(role).or(Some(self.accent));
        }
        Some(match role {
            "none" => return None,
            // Falls through to the wildcard below, which also resolves to
            // `self.accent` — an unrecognised role and the literal "accent"
            // role are meant to land on the same colour.
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

    /// Background colour for a slide given its layout + treatment.
    pub fn slide_bg(&self, bg: Background) -> Color {
        match bg {
            Background::Surface => self.surface,
            Background::Accent => self.accent,
            Background::Deep => self.deep,
        }
    }

    /// Default body / heading text colour for a slide background — white
    /// reads on accent & deep; the theme text colour on surface.
    pub fn on_bg(&self, bg: Background) -> Color {
        match bg {
            Background::Surface => self.text,
            Background::Accent | Background::Deep => Color::WHITE,
        }
    }
}

/// The fill/stroke roles offered in the inspector swatch row.
pub const FILL_ROLES: [&str; 8] = [
    "accent",
    "accent-soft",
    "surface",
    "deep",
    "success",
    "warning",
    "danger",
    "info",
];
pub const STROKE_ROLES: [&str; 6] = ["none", "accent", "text", "white", "deep", "danger"];

/// Linear blend `a*(1-t) + b*t` in straight RGB.
fn mix(a: Color, b: Color, t: f64) -> Color {
    let l = |x: u8, y: u8| {
        (f64::from(x) * (1.0 - t) + f64::from(y) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgba8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), 255)
}

// ===========================================================================
// Starter deck — the minimal blank document a *live* launch opens with.
// ===========================================================================

/// A single blank title slide. A live launch opens with this so the app never
/// auto-populates fabricated content; the deck's real state comes from the
/// shared CRDT document once the user starts editing (or a peer joins).
pub fn starter_deck() -> Deck {
    let mut s = Slide::new(Layout::Title);
    s.background = Background::Surface;
    s.add(Element::text("Title", 96.0, 250.0, 1088.0, 130.0, 88.0).with(|e| e.bold = true));
    s.add(
        Element::text("Subtitle", 96.0, 386.0, 1088.0, 70.0, 34.0).with(|e| {
            e.fill = "text-dim".into();
        }),
    );
    Deck {
        title: "Untitled deck".into(),
        palette: PaletteKind::Indigo,
        slides: vec![s],
    }
}

// ===========================================================================
// Sample deck — a designed, multi-slide deck used by `--shots` and tests.
// ===========================================================================

/// A hermetic, realistic sample deck. Used only for headless screenshots and
/// unit tests — never auto-loaded on a live launch (see [`starter_deck`]).
pub fn seed_deck() -> Deck {
    Deck {
        title: "Riverside Park — concept review".into(),
        palette: PaletteKind::Indigo,
        slides: vec![
            seed_slide_cover(),
            seed_slide_goals(),
            seed_slide_two_column(),
            seed_slide_diagram(),
            seed_slide_closing(),
        ],
    }
}

/// Slide 1: cover.
fn seed_slide_cover() -> Slide {
    let mut s = Slide::new(Layout::Section);
    s.background = Background::Accent;
    s.notes = "Set the scene. One line on why we're renewing the park and who \
               the workshop is for."
        .into();
    s.add(
        Element::text("CONCEPT REVIEW", 110.0, 250.0, 700.0, 70.0, 28.0).with(|e| {
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
            "Design workshop  ·  spring review",
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
        Element::shape(ElementKind::Rect, 110.0, 232.0, 92.0, 8.0).with(|e| {
            e.fill = "white".into();
            e.radius = 4.0;
        }),
    );
    s
}

/// Slide 2: title + body (selected on launch).
fn seed_slide_goals() -> Slide {
    let mut s = Slide::new(Layout::TitleContent);
    s.background = Background::Surface;
    s.notes = "Walk the four goals. Spend the most time on protecting the \
               wetland — that's the constraint everything else works around."
        .into();
    s.add(
        Element::text("Goals for this phase", 96.0, 70.0, 1088.0, 90.0, 56.0).with(|e| {
            e.bold = true;
        }),
    );
    s.add(
        Element::shape(ElementKind::Rect, 96.0, 168.0, 150.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }),
    );
    s.add(Element::text(
        "• Reconnect the waterfront path\n\n• Add shade, seating and lighting\n\n• Protect the existing wetland\n\n• Keep the budget under review",
        96.0,
        210.0,
        720.0,
        440.0,
        34.0,
    ));
    // A small decorative figure block on the right.
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

/// Slide 3: two column.
fn seed_slide_two_column() -> Slide {
    let mut s = Slide::new(Layout::TwoColumn);
    s.background = Background::Surface;
    s.notes = "Two path options. Ask the room which trade-off they prefer \
               before we cost them out."
        .into();
    s.add(
        Element::text("Two options to compare", 96.0, 70.0, 1088.0, 90.0, 56.0)
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
        Element::text("OPTION A", 128.0, 250.0, 460.0, 50.0, 28.0).with(|e| {
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
        Element::text("OPTION B", 696.0, 250.0, 460.0, 50.0, 28.0).with(|e| {
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

/// Slide 4: a built diagram.
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
    // node A
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
    // connector
    s.add(
        Element::shape(ElementKind::Line, 380.0, 435.0, 280.0, 0.0).with(|e| {
            e.stroke = "accent-soft".into();
            e.stroke_width = 8.0;
        }),
    );
    // node B
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

/// Slide 5: closing section.
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
            "Share feedback by Friday  ·  prototype in April",
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
    fn seed_deck_has_designed_slides() {
        let d = seed_deck();
        assert_eq!(d.slides.len(), 5);
        // Every slide has at least one element.
        for s in &d.slides {
            assert!(!s.elements.is_empty(), "slide {:?} empty", s.layout);
        }
    }

    #[test]
    fn add_element_increments_z() {
        let mut s = Slide::new(Layout::Blank);
        let a = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 10.0, 10.0));
        let b = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 10.0, 10.0));
        assert!(s.element(&a).unwrap().z < s.element(&b).unwrap().z);
    }

    #[test]
    fn kind_and_align_round_trip() {
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

    #[test]
    fn palette_resolves_none_role() {
        let theme = ui::theme::Theme::dark();
        let p = Palette::resolve(PaletteKind::Indigo, &theme);
        assert!(p.resolve_role("none").is_none());
        assert!(p.resolve_role("accent").is_some());
    }
}
