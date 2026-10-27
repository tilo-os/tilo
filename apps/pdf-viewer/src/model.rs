//! Pure data model for the PDF viewer.
//!
//! Nothing here touches the CRDT, taffy, vello or winit — it's the
//! plain-Rust core: the page model, the annotation / comment / signature
//! value types, the tool enum, and the pure search routine. Everything is
//! unit-testable in isolation.
//!
//! A [`Page`] is a list of [`Block`]s in PDF points (1pt = 1px at 100% zoom).
//! For a loaded PDF the blocks are empty placeholders whose pixels come from
//! the PDF engine; [`sample_document`] is the bundled demonstration used by the
//! screenshot harness and tests, laid out from its blocks.

use ui::tilo_resource::{self, JsonSchema, Model};
use uuid::Uuid;

/// US-Letter page geometry, in PDF points.
pub const PAGE_W: f64 = 612.0;
pub const PAGE_H: f64 = 792.0;
/// Inner text margin on every page edge.
pub const MARGIN: f64 = 64.0;

/// A single laid-out content block on a page. The renderer walks these
/// top-to-bottom, measuring each so the next starts below it.
#[derive(Clone, Debug)]
pub enum Block {
    Title(String),
    H1(String),
    H2(String),
    Body(String),
    Caption(String),
    Quote(String),
    /// A reserved figure box of `height` points with a caption beneath.
    Figure {
        height: f64,
        label: String,
    },
    Divider,
    Spacer(f64),
}

impl Block {
    /// The text a search query runs against, if any.
    pub fn search_text(&self) -> Option<&str> {
        match self {
            Block::Title(t)
            | Block::H1(t)
            | Block::H2(t)
            | Block::Body(t)
            | Block::Caption(t)
            | Block::Quote(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    /// 1-based page number.
    pub number: usize,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub page: usize,
    /// 0 = section, 1 = sub-section (controls indent in the drawer).
    pub level: u8,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub pages: Vec<Page>,
    pub outline: Vec<OutlineItem>,
    /// Uniform page geometry, in PDF points. For a loaded PDF this is the
    /// first page's media box; for the bundled reader model it is US-Letter.
    /// Pages stack uniformly against this card size (the common case where a
    /// document has one page size throughout).
    pub page_w: f64,
    pub page_h: f64,
}

impl Document {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Borrow a page by 1-based number.
    pub fn page(&self, number: usize) -> Option<&Page> {
        self.pages.iter().find(|p| p.number == number)
    }

    /// An empty document with `count` blank pages of the given size, used for
    /// the raster backend where page pixels come from the PDF engine rather than the
    /// text model.
    pub fn blank_pages(title: &str, author: &str, count: usize, page_w: f64, page_h: f64) -> Self {
        let count = count.max(1);
        let pages = (1..=count)
            .map(|n| Page {
                number: n,
                blocks: Vec::new(),
            })
            .collect();
        Document {
            title: title.to_string(),
            author: author.to_string(),
            subject: String::new(),
            pages,
            outline: Vec::new(),
            page_w,
            page_h,
        }
    }
}

// ----------------------------------------------------------------------
// Tools
// ----------------------------------------------------------------------

/// What a press on the page does: select text, or author one kind of
/// markup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Select,
    Highlight,
    Ink,
    Note,
    /// Free text: a text box placed directly on the page.
    Text,
    Sign,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Select,
        Tool::Highlight,
        Tool::Ink,
        Tool::Note,
        Tool::Text,
        Tool::Sign,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Highlight => "Highlight",
            Tool::Ink => "Draw",
            Tool::Note => "Note",
            Tool::Text => "Text",
            Tool::Sign => "Sign",
        }
    }

    /// One-key shortcut hint shown in the tooltip / status bar.
    pub fn hotkey(self) -> char {
        match self {
            Tool::Select => '1',
            Tool::Highlight => '2',
            Tool::Ink => '3',
            Tool::Note => '4',
            Tool::Text => '6',
            Tool::Sign => '5',
        }
    }

    /// Tools that author markup on the page (and so show the colour
    /// swatches).
    pub fn annotates(self) -> bool {
        matches!(self, Tool::Highlight | Tool::Ink | Tool::Note | Tool::Text)
    }
}

// ----------------------------------------------------------------------
// Annotation colour swatches (resolved against the OS theme at draw time)
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Swatch {
    #[default]
    Yellow,
    Green,
    Pink,
    Blue,
    Purple,
    /// An exact colour, `0xRRGGBB` — markup that came from the PDF itself
    /// keeps its own colour rather than being snapped to a swatch.
    Custom(u32),
}

impl Swatch {
    pub const ALL: [Swatch; 5] = [
        Swatch::Yellow,
        Swatch::Green,
        Swatch::Pink,
        Swatch::Blue,
        Swatch::Purple,
    ];

    pub fn key(self) -> String {
        match self {
            Swatch::Yellow => "yellow".into(),
            Swatch::Green => "green".into(),
            Swatch::Pink => "pink".into(),
            Swatch::Blue => "blue".into(),
            Swatch::Purple => "purple".into(),
            Swatch::Custom(rgb) => format!("#{:06x}", rgb & 0x00ff_ffff),
        }
    }

    pub fn parse(s: &str) -> Swatch {
        match s {
            "green" => Swatch::Green,
            "pink" => Swatch::Pink,
            "blue" => Swatch::Blue,
            "purple" => Swatch::Purple,
            _ => s
                .strip_prefix('#')
                .filter(|hex| hex.len() == 6)
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .map_or(Swatch::Yellow, Swatch::Custom),
        }
    }

    /// The swatch for an exact colour: the named swatch with that colour, or
    /// a custom one.
    pub fn from_rgb(r: u8, g: u8, b: u8) -> Swatch {
        Swatch::ALL
            .into_iter()
            .find(|s| s.rgb() == (r, g, b))
            .unwrap_or_else(|| {
                Swatch::Custom((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))
            })
    }

    /// The swatch's sRGB colour. Markup is the document's own content, so
    /// the same colour is painted on screen (`render::swatch_color`) and
    /// written into a saved PDF, where it looks the same in every viewer.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Swatch::Yellow => (250, 204, 21),
            Swatch::Green => (34, 197, 94),
            Swatch::Pink => (236, 72, 153),
            Swatch::Blue => (59, 130, 246),
            Swatch::Purple => (139, 92, 246),
            Swatch::Custom(rgb) => ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        }
    }
}

/// A swatch is stored and carried as its key: `yellow`, … or `#rrggbb`.
impl serde::Serialize for Swatch {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.key())
    }
}

impl<'de> serde::Deserialize<'de> for Swatch {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|key| Swatch::parse(&key))
    }
}

impl JsonSchema for Swatch {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "string",
            "description": "yellow, green, pink, blue, purple, or an exact colour #rrggbb",
        })
    }
}

// ----------------------------------------------------------------------
// Geometry
// ----------------------------------------------------------------------

/// A rectangle in page-point space (origin top-left of the page).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct PtRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl PtRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Build a normalised rect from two corner points (any order).
    pub fn from_corners(ax: f64, ay: f64, bx: f64, by: f64) -> Self {
        let x = ax.min(bx);
        let y = ay.min(by);
        Self {
            x,
            y,
            w: (ax - bx).abs(),
            h: (ay - by).abs(),
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

// ----------------------------------------------------------------------
// Annotations
// ----------------------------------------------------------------------

/// How text markup is drawn and exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MarkupStyle {
    Highlight,
    Underline,
    StrikeOut,
    /// A wavy underline (from PDFs that use it; drawn as an underline).
    Squiggly,
}

impl MarkupStyle {
    pub fn key(self) -> &'static str {
        match self {
            MarkupStyle::Highlight => "highlight",
            MarkupStyle::Underline => "underline",
            MarkupStyle::StrikeOut => "strikeout",
            MarkupStyle::Squiggly => "squiggly",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "highlight" => Some(MarkupStyle::Highlight),
            "underline" => Some(MarkupStyle::Underline),
            "strikeout" => Some(MarkupStyle::StrikeOut),
            "squiggly" => Some(MarkupStyle::Squiggly),
            _ => None,
        }
    }
}

/// Default font size of new free text, in points.
pub const FREE_TEXT_SIZE: f64 = 14.0; // a PDF font size, not UI spacing

/// A generous estimate of the box height `text` needs at `size` points when
/// wrapped to `width`: explicit lines, each wrapped by an average advance
/// (half the size for alphabetic text, the full size for ideographs), plus
/// the box's inner padding.
pub fn free_text_height(text: &str, size: f64, width: f64) -> f64 {
    let inner = (width - 4.0).max(size);
    let lines: f64 = text
        .split('\n')
        .map(|line| {
            let advance: f64 = line
                .chars()
                .map(|c| {
                    if matches!(u32::from(c), 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xFF00..=0xFFEF) {
                        size
                    } else {
                        size * 0.55
                    }
                })
                .sum();
            (advance / inner).ceil().max(1.0)
        })
        .sum();
    lines * size * 1.2 + 4.0
}

/// What an annotation marks: its kind and its geometry.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AnnoKind {
    /// Text markup (highlight, underline, strike-through) over one band per
    /// line it covers.
    Markup {
        style: MarkupStyle,
        quads: Vec<PtRect>,
    },
    /// Freehand ink: one or more polylines.
    Ink { strokes: Vec<Vec<(f64, f64)>> },
    /// A sticky note anchored at a point. The body text is mirrored into
    /// the linked top-level comment so the comments rail can thread it.
    Note { x: f64, y: f64 },
    /// Text written on the page inside `rect`, wrapped to its width, at
    /// `size` points, in the annotation's colour.
    FreeText {
        rect: PtRect,
        text: String,
        size: f64,
    },
}

impl AnnoKind {
    /// A highlight over a single band.
    pub fn highlight(rect: PtRect) -> Self {
        AnnoKind::Markup {
            style: MarkupStyle::Highlight,
            quads: vec![rect],
        }
    }

    pub fn type_key(&self) -> &'static str {
        match self {
            AnnoKind::Markup { style, .. } => style.key(),
            AnnoKind::Ink { .. } => "ink",
            AnnoKind::Note { .. } => "note",
            AnnoKind::FreeText { .. } => "freetext",
        }
    }

    /// One freehand stroke.
    pub fn ink(points: Vec<(f64, f64)>) -> Self {
        AnnoKind::Ink {
            strokes: vec![points],
        }
    }

    /// Whether `(x, y)` lies on one of the markup's bands.
    pub fn markup_contains(&self, x: f64, y: f64) -> bool {
        matches!(self, AnnoKind::Markup { quads, .. } if quads.iter().any(|q| q.contains(x, y)))
    }

    /// Whether `(x, y)` lies inside a free text box.
    pub fn free_text_contains(&self, x: f64, y: f64) -> bool {
        matches!(self, AnnoKind::FreeText { rect, .. } if rect.contains(x, y))
    }
}

/// Which space an annotation's geometry is in. Records store PDF user space
/// (exact however the page is displayed); records written before that stored
/// the reader's page-point space. Everything the reader holds for drawing is
/// in page-point space.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Model,
    serde::Serialize,
    serde::Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum GeomSpace {
    #[default]
    Page,
    User,
}

#[derive(Clone, Debug)]
pub struct Annotation {
    pub id: String,
    pub page: usize,
    pub kind: AnnoKind,
    pub color: Swatch,
    pub author: String,
    /// The engine character range `[start, end)` the markup covers, for
    /// text-anchored markup.
    pub chars: Option<(u32, u32)>,
    /// When it was authored (unix milliseconds; 0 when unknown).
    pub created_ms: i64,
    pub space: GeomSpace,
}

impl Annotation {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// An annotation in page-point space with no text anchor.
    pub fn new(page: usize, kind: AnnoKind, color: Swatch, author: impl Into<String>) -> Self {
        Self {
            id: Self::new_id(),
            page,
            kind,
            color,
            author: author.into(),
            chars: None,
            created_ms: 0,
            space: GeomSpace::Page,
        }
    }

    /// The anchor point (page-point space) used to position the comment
    /// connector and selection chrome.
    pub fn anchor(&self) -> (f64, f64) {
        match &self.kind {
            AnnoKind::Markup { quads, .. } => quads.first().map_or((0.0, 0.0), |r| (r.x, r.y)),
            AnnoKind::Ink { strokes } => strokes
                .iter()
                .find_map(|s| s.first().copied())
                .unwrap_or((0.0, 0.0)),
            AnnoKind::Note { x, y } => (*x, *y),
            AnnoKind::FreeText { rect, .. } => (rect.x, rect.y),
        }
    }
}

// ----------------------------------------------------------------------
// Comments (threaded)
// ----------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Reply {
    /// Stable identity minted when the reply action is authored.  Replies are
    /// first-class collaborative resources, not just an unaddressable string
    /// in their parent comment's list.
    pub id: String,
    pub author: String,
    /// The reply's plain text: what export, search and labels read.
    pub body: String,
    /// The reply as written, as the editor shows it.
    pub blocks: Vec<ui::widget::BlockContent>,
}

impl Reply {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    /// The annotation this comment is pinned to, if any.
    pub anno_id: Option<String>,
    pub page: usize,
    pub author: String,
    /// The comment's plain text: what export, search and labels read.
    pub body: String,
    /// Monotonic ordinal for stable sort (seed time / creation order).
    pub created: i64,
    pub resolved: bool,
    pub replies: Vec<Reply>,
    /// The comment as written, as the editor shows it.
    pub blocks: Vec<ui::widget::BlockContent>,
}

impl Comment {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }
}

// ----------------------------------------------------------------------
// Signatures
// ----------------------------------------------------------------------

/// The visible content of a placed signature: either the signer's typed name
/// (rendered in a signature script) or freehand strokes, normalised into the
/// field's `0..1` box so it scales to any placement size.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(tag = "ink", rename_all = "lowercase")]
pub enum SignatureInk {
    #[default]
    /// Render the signer's name as a script-style typed signature.
    Typed,
    /// The signer drew this by hand: one or more polylines in normalised
    /// `[0,1]²` field space (origin top-left).
    Drawn { strokes: Vec<Vec<(f64, f64)>> },
}

impl SignatureInk {
    /// A drawn signature with no ink is empty.
    pub fn is_empty(&self) -> bool {
        match self {
            SignatureInk::Typed => false,
            SignatureInk::Drawn { strokes } => strokes.iter().all(|s| s.len() < 2),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub id: String,
    pub page: usize,
    pub rect: PtRect,
    pub signer: String,
    pub date: String,
    /// The real signature content (typed name or drawn strokes).
    pub ink: SignatureInk,
    /// Which space `rect` is in (see [`GeomSpace`]).
    pub space: GeomSpace,
}

impl Signature {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }
}

/// The scale that fits a `w` × `h` page into a `card_w` × `card_h` slot.
/// Pages fill the slot's width unless that would overflow its height by
/// more than 1% (a rotated or differently shaped page), in which case they
/// fit the height; the renderer and the page maps share this rule.
pub fn fit_scale(card_w: f64, card_h: f64, w: f64, h: f64) -> f64 {
    if !(w > 0.0 && h > 0.0 && card_w > 0.0 && card_h > 0.0) {
        return 1.0;
    }
    let by_width = card_w / w;
    if h * by_width <= card_h * 1.01 {
        by_width
    } else {
        card_h / h
    }
}

/// How one displayed page maps between the spaces the reader works in.
///
/// - *layer* space: the engine's display space for the source page (its own
///   `/Rotate` applied) — where text-layer and form-widget boxes arrive;
/// - PDF *user* space — where records store geometry;
/// - *card* space: the page-point space of the page's slot in the reader's
///   uniform page stack, after the page occurrence's recorded rotation and a
///   centred fit into the slot. Everything drawn and hit-tested is in card
///   space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMap {
    /// The source page's own geometry.
    pub own: tilo_pdf::PageGeometry,
    /// `own` with the occurrence's recorded rotation added.
    pub effective: tilo_pdf::PageGeometry,
    /// Fit of the effective page into the card: `card = display * scale + offset`.
    pub scale: f64,
    pub offset: (f64, f64),
}

impl PageMap {
    /// The map for a page of geometry `own`, rotated by `rotation` degrees,
    /// shown in a `card_w` × `card_h` slot.
    pub fn new(own: tilo_pdf::PageGeometry, rotation: i32, card_w: f64, card_h: f64) -> Self {
        let extra = tilo_pdf::Rotation::from_degrees(rotation).unwrap_or_default();
        let effective = tilo_pdf::PageGeometry {
            rotation: own.rotation.then(extra),
            ..own
        };
        let size = effective.size();
        let (w, h) = (f64::from(size.width), f64::from(size.height));
        let scale = fit_scale(card_w, card_h, w, h);
        // A page the size of its slot maps exactly; tiny float drift in page
        // boxes must not blur every overlay.
        let scale = if (scale - 1.0).abs() < 1e-6 {
            1.0
        } else {
            scale
        };
        let offset = (
            ((card_w - w * scale) / 2.0).max(0.0),
            ((card_h - h * scale) / 2.0).max(0.0),
        );
        Self {
            own,
            effective,
            scale,
            offset,
        }
    }

    /// The map of a plain page exactly filling a `w` × `h` slot (the reader's
    /// bundled sample document, or a page whose geometry is not known yet).
    pub fn identity(w: f64, h: f64) -> Self {
        let own = tilo_pdf::PageGeometry {
            user_box: tilo_pdf::Rect::from_xywh(0.0, 0.0, w as f32, h as f32),
            rotation: tilo_pdf::Rotation::None,
        };
        Self::new(own, 0, w, h)
    }

    pub fn is_identity(&self) -> bool {
        self.own == self.effective
            && (self.scale - 1.0).abs() < f64::EPSILON
            && self.offset.0.abs() < f64::EPSILON
            && self.offset.1.abs() < f64::EPSILON
    }

    fn fit(&self, p: tilo_pdf::Point) -> (f64, f64) {
        (
            f64::from(p.x) * self.scale + self.offset.0,
            f64::from(p.y) * self.scale + self.offset.1,
        )
    }

    fn unfit(&self, (x, y): (f64, f64)) -> tilo_pdf::Point {
        tilo_pdf::Point::new(
            ((x - self.offset.0) / self.scale) as f32,
            ((y - self.offset.1) / self.scale) as f32,
        )
    }

    pub fn user_to_card(&self, (x, y): (f64, f64)) -> (f64, f64) {
        self.fit(
            self.effective
                .to_display(tilo_pdf::Point::new(x as f32, y as f32)),
        )
    }

    pub fn card_to_user(&self, p: (f64, f64)) -> (f64, f64) {
        let u = self.effective.to_user(self.unfit(p));
        (f64::from(u.x), f64::from(u.y))
    }

    pub fn layer_to_card(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let u = self.own.to_user(tilo_pdf::Point::new(x as f32, y as f32));
        self.fit(self.effective.to_display(u))
    }

    pub fn card_to_layer(&self, p: (f64, f64)) -> (f64, f64) {
        let d = self.own.to_display(self.effective.to_user(self.unfit(p)));
        (f64::from(d.x), f64::from(d.y))
    }

    fn map_rect(r: &PtRect, f: impl Fn((f64, f64)) -> (f64, f64)) -> PtRect {
        let (ax, ay) = f((r.x, r.y));
        let (bx, by) = f((r.x + r.w, r.y + r.h));
        PtRect::from_corners(ax, ay, bx, by)
    }

    pub fn layer_rect_to_card(&self, r: &PtRect) -> PtRect {
        Self::map_rect(r, |p| self.layer_to_card(p))
    }

    /// An annotation held in page (card) space, whichever space its record
    /// uses.
    pub fn annotation_to_card(&self, a: &Annotation) -> Annotation {
        match a.space {
            GeomSpace::Page => a.clone(),
            GeomSpace::User => {
                Self::map_annotation(a, GeomSpace::Page, self.scale, |p| self.user_to_card(p))
            }
        }
    }

    /// An annotation in user space, whichever space it is held in.
    pub fn annotation_to_user(&self, a: &Annotation) -> Annotation {
        match a.space {
            GeomSpace::User => a.clone(),
            GeomSpace::Page => Self::map_annotation(a, GeomSpace::User, 1.0 / self.scale, |p| {
                self.card_to_user(p)
            }),
        }
    }

    fn map_annotation(
        a: &Annotation,
        to: GeomSpace,
        size_factor: f64,
        f: impl Fn((f64, f64)) -> (f64, f64),
    ) -> Annotation {
        let kind = match &a.kind {
            AnnoKind::Markup { style, quads } => AnnoKind::Markup {
                style: *style,
                quads: quads.iter().map(|q| Self::map_rect(q, &f)).collect(),
            },
            AnnoKind::Ink { strokes } => AnnoKind::Ink {
                strokes: strokes
                    .iter()
                    .map(|s| s.iter().copied().map(&f).collect())
                    .collect(),
            },
            AnnoKind::Note { x, y } => {
                let (x, y) = f((*x, *y));
                AnnoKind::Note { x, y }
            }
            // The box keeps its text upright: its corners move, and the text
            // size follows the fit.
            AnnoKind::FreeText { rect, text, size } => AnnoKind::FreeText {
                rect: Self::map_rect(rect, &f),
                text: text.clone(),
                size: size * size_factor,
            },
        };
        Annotation {
            kind,
            space: to,
            ..a.clone()
        }
    }

    pub fn signature_to_card(&self, s: &Signature) -> Signature {
        match s.space {
            GeomSpace::Page => s.clone(),
            GeomSpace::User => Signature {
                rect: Self::map_rect(&s.rect, |p| self.user_to_card(p)),
                space: GeomSpace::Page,
                ..s.clone()
            },
        }
    }

    pub fn signature_to_user(&self, s: &Signature) -> Signature {
        match s.space {
            GeomSpace::User => s.clone(),
            GeomSpace::Page => Signature {
                rect: Self::map_rect(&s.rect, |p| self.card_to_user(p)),
                space: GeomSpace::User,
                ..s.clone()
            },
        }
    }
}

/// A form field value held in the document's record, keyed by the field's
/// fully qualified name.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum FormEntry {
    Text(String),
    Checked { on: bool, export: Option<String> },
    Choice(String),
}

/// An empty text value: what a field holds before anyone fills it.
impl Default for FormEntry {
    fn default() -> Self {
        FormEntry::Text(String::new())
    }
}

// ----------------------------------------------------------------------
// Search
// ----------------------------------------------------------------------

/// A single search hit: which page, which block, and the byte range of
/// the match within that block's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub page: usize,
    pub block: usize,
    pub start: usize,
    pub len: usize,
}

/// Standard find-bar toggles. Both default off, matching the historical
/// case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Match only when the query is bounded by non-word characters on both
    /// sides (so "ink" doesn't match inside "thinking").
    pub whole_word: bool,
    /// Match the query's letter case exactly.
    pub case_sensitive: bool,
}

/// Hard result budget shared by both PDF backends. The find UI and resource
/// projection remain responsive even for adversarial documents containing a
/// match at every character; users can refine the query to narrow it.
pub const MAX_SEARCH_RESULTS: usize = 500;

/// Find bounded, non-overlapping spans in the original UTF-8 text. Both PDF
/// backends share the system search normalization and Unicode word boundaries.
/// Folding expansion or contraction (İ, K, a dropped accent) must never turn
/// an index into normalized text into an index into the source document.
pub(crate) fn search_text(
    text: &str,
    query: &str,
    opts: SearchOptions,
    limit: usize,
) -> Vec<std::ops::Range<usize>> {
    use std::borrow::Cow;

    let limit = limit.min(MAX_SEARCH_RESULTS);
    let trimmed = query.trim();
    if trimmed.is_empty() || limit == 0 {
        return Vec::new();
    }
    let needle = if opts.case_sensitive {
        Cow::Borrowed(trimmed)
    } else {
        Cow::Owned(ui::search::normalize_search_text(trimmed))
    };
    let hay = if opts.case_sensitive {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(ui::search::normalize_search_text(text))
    };
    // Each pair marks one original scalar boundary in normalized/source byte
    // space. ASCII and case-sensitive searches need no offset allocation.
    let boundaries = if !opts.case_sensitive && !text.is_ascii() {
        let mut pairs = vec![(0, 0)];
        let mut normalized_end = 0;
        for (source_start, scalar) in text.char_indices() {
            ui::search::fold_char(scalar, &mut |folded| {
                normalized_end += folded.len_utf8();
            });
            pairs.push((normalized_end, source_start + scalar.len_utf8()));
        }
        pairs
    } else {
        Vec::new()
    };
    let words: Vec<_> = if opts.whole_word {
        ui::text_break::unicode_word_ranges(text).collect()
    } else {
        Vec::new()
    };
    let inside_word = |byte| {
        words
            .get(words.partition_point(|word| word.end <= byte))
            .is_some_and(|word| word.start < byte && byte < word.end)
    };
    let mut out = Vec::new();
    let mut source_end = 0;
    for (start, matched) in hay.match_indices(needle.as_ref()) {
        let end = start + matched.len();
        let (range, complete_scalars) = if boundaries.is_empty() {
            (start..end, true)
        } else {
            let first = boundaries
                .partition_point(|pair| pair.0 <= start)
                .saturating_sub(1);
            let last = boundaries.partition_point(|pair| pair.0 < end);
            let (Some(&(fold_start, original_start)), Some(&(fold_end, original_end))) =
                (boundaries.get(first), boundaries.get(last))
            else {
                continue;
            };
            (
                original_start..original_end,
                fold_start == start && fold_end == end,
            )
        };
        // A substring can cover one expanded scalar, but a whole-word match
        // cannot end halfway through that scalar's normalized spelling.
        if range.start < source_end
            || (opts.whole_word
                && (!complete_scalars || inside_word(range.start) || inside_word(range.end)))
        {
            continue;
        }
        source_end = range.end;
        out.push(range);
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// Substring search across renderable text, with source UTF-8 byte spans.
pub fn search(doc: &Document, query: &str, opts: SearchOptions) -> Vec<Match> {
    let mut out = Vec::new();
    if query.trim().is_empty() {
        return out;
    }
    for page in &doc.pages {
        for (bi, block) in page.blocks.iter().enumerate() {
            let Some(text) = block.search_text() else {
                continue;
            };
            for range in search_text(text, query, opts, MAX_SEARCH_RESULTS - out.len()) {
                out.push(Match {
                    page: page.number,
                    block: bi,
                    start: range.start,
                    len: range.len(),
                });
            }
            if out.len() >= MAX_SEARCH_RESULTS {
                return out;
            }
        }
    }
    out
}

// ----------------------------------------------------------------------
// The bundled sample document
// ----------------------------------------------------------------------

fn sample_body(s: &str) -> Block {
    Block::Body(s.to_string())
}

fn sample_page_abstract() -> Page {
    Page {
        number: 1,
        blocks: vec![
            Block::Spacer(40.0),
            Block::Title("Tilo: A Collaborative Spatial Operating System".to_string()),
            Block::Spacer(6.0),
            Block::Caption(
                "Technical overview, revision 7. Example Systems, June 2026.".to_string(),
            ),
            Block::Spacer(20.0),
            Block::H1("Abstract".to_string()),
            sample_body(
                "Tilo is an immutable, container-based Linux platform that treats \
collaboration and spatial organisation as first-class primitives rather than \
features bolted onto a single-user desktop. This document describes the layered \
architecture: an immutable bootc image, a tiling Wayland compositor with a \
two-dimensional cluster model, a peer-to-peer filesystem that projects remote \
files and hydrates them on demand, and a CRDT-native user-interface toolkit \
shared by every system application.",
            ),
            Block::Spacer(8.0),
            sample_body(
                "The design goal is a daily-driver operating system where two people \
can sit inside the same workspace, edit the same document, and watch each other's \
cursors move, with the security boundary enforced by the compositor's per-cluster \
sandbox instead of by convention.",
            ),
            Block::Spacer(16.0),
            Block::H2("Document scope".to_string()),
            sample_body(
                "Sections two through five cover the compositor, the filesystem, the \
toolkit and the security model in turn. Appendix A records the on-disk image \
layout. Readers implementing a competing client should treat section four as \
normative.",
            ),
            Block::Spacer(18.0),
            Block::Quote(
                "\"Make the collaborative case the default case, and the \
single-user case a degenerate instance of it.\""
                    .to_string(),
            ),
        ],
    }
}

fn sample_page_compositor() -> Page {
    Page {
        number: 2,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("1  The Compositor".to_string()),
            sample_body(
                "The compositor arranges windows into clusters. A cluster is a group \
of tiled windows that never overlap; clusters themselves tile across an infinite \
two-dimensional canvas. Panning the canvas is the primary navigation gesture, \
replacing the alt-tab stack of conventional desktops.",
            ),
            Block::Spacer(10.0),
            Block::H2("1.1  Per-cluster sandboxing".to_string()),
            sample_body(
                "Each cluster runs inside its own Bubblewrap sandbox with an explicit \
file-access policy. When an application requests a path outside the cluster's \
allow-list the compositor pauses the request and prompts the user, who can grant \
access temporarily or permanently. Because the sandbox is the boundary, sharing a \
cluster with a remote peer never widens the host's exposure beyond that cluster.",
            ),
            Block::Spacer(10.0),
            Block::Figure {
                height: 150.0,
                label:
                    "Figure 1. Two clusters tiled on the spatial canvas, each a separate sandbox."
                        .to_string(),
            },
            Block::Spacer(10.0),
            Block::H2("1.2  Remote input injection".to_string()),
            sample_body(
                "Collaboration streams Wayland buffers to invited peers and injects \
their pointer and keyboard events back into the originating cluster. Coordination \
runs over an Iroh gossip mesh reached through authenticated invite addresses \
segment and through relays elsewhere.",
            ),
        ],
    }
}

fn sample_page_filesystem() -> Page {
    Page {
        number: 3,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("2  The Filesystem".to_string()),
            sample_body(
                "The peer-to-peer filesystem mounts at a well-known path and projects \
metadata for remote files immediately while deferring content until first read. \
Metadata propagates through the same gossip layer the compositor uses; content is \
addressed by a BLAKE3 hash and fetched on demand from whichever peer holds it.",
            ),
            Block::Spacer(10.0),
            Block::H2("2.1  Projection versus hydration".to_string()),
            sample_body(
                "A directory listing is cheap: it is a projection of gossiped \
metadata. Opening a file is where hydration happens. The first read blocks while \
the content is fetched and verified against its hash, then subsequent reads are \
served from a local cache with a short metadata time-to-live.",
            ),
            Block::Spacer(10.0),
            Block::H2("2.2  Conflict handling".to_string()),
            sample_body(
                "Concurrent writes to the same path produce conflict copies rather \
than silent overwrites. The losing writer's bytes are preserved under a suffixed \
name so no edit is ever destroyed, and the user resolves the divergence \
explicitly. This mirrors the toolkit's CRDT philosophy at the file granularity.",
            ),
            Block::Spacer(12.0),
            Block::Quote("Never assume that open() means the bytes are already local.".to_string()),
        ],
    }
}

fn sample_page_toolkit() -> Page {
    Page {
        number: 4,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("3  The UI Toolkit".to_string()),
            sample_body(
                "Every system application is built on a retained-mode toolkit whose \
widgets read their state from a shared CRDT document rather than from local \
fields. A frame is five passes: synchronise from the document, build the layout \
tree, apply the computed layout, draw into a vector scene, and handle input \
events that may mutate the document directly.",
            ),
            Block::Spacer(10.0),
            Block::H2("3.1  State lives in the document".to_string()),
            sample_body(
                "Because all durable state is in the CRDT, undo and redo are simply \
snapshots of the document, and live collaboration falls out for free: two \
instances editing the same object converge without a message-passing layer. \
Widgets keep only ephemeral caches such as caret position and scroll offset.",
            ),
            Block::Spacer(10.0),
            Block::H2("3.2  Theming is OS-controlled".to_string()),
            sample_body(
                "Widgets never hard-code colour. Every fill and stroke is read from a \
theme handed in at draw time, which lets the operating system enforce a coherent \
look across applications and switch palettes without each app participating. This \
PDF reader obeys the same rule: its highlights, ink and chrome all resolve through \
the active theme.",
            ),
        ],
    }
}

fn sample_page_security() -> Page {
    Page {
        number: 5,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("4  Security Model".to_string()),
            sample_body(
                "Trust in Tilo flows outward from the compositor. The sandbox is the \
unit of isolation; a cluster cannot read files, capture input, or stream buffers \
beyond the grants the user has approved for it. Collaboration never relaxes this: \
an invited peer is granted presence inside one cluster, not access to the host.",
            ),
            Block::Spacer(10.0),
            Block::H2("4.1  Capability grants".to_string()),
            sample_body(
                "Grants are explicit, revocable, and scoped to a single cluster. A \
temporary grant expires when the cluster closes; a permanent grant is recorded \
against the cluster's policy and surfaced for review. There is no ambient \
authority that an application can escalate into.",
            ),
            Block::Spacer(10.0),
            Block::H2("4.2  Signing and attestation".to_string()),
            sample_body(
                "Documents carry detached signatures. A signature records the signer, \
the time, and the page region it attests to. Cryptographic verification of the \
signing key against a workspace identity is layered on top and is described in the \
companion identity specification.",
            ),
            Block::Spacer(14.0),
            Block::Divider,
            Block::Spacer(10.0),
            Block::Caption("Signed below to acknowledge review of this revision.".to_string()),
        ],
    }
}

fn sample_page_appendix() -> Page {
    Page {
        number: 6,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("Appendix A  Image Layout".to_string()),
            sample_body(
                "The production image is derived from a Fedora bootc base. Persistent \
state lives under a writable subtree while the root filesystem stays read-only, \
which is what makes atomic updates and rollback possible. Binaries for the \
compositor, the filesystem driver and the system applications are baked into the \
image at build time.",
            ),
            Block::Spacer(10.0),
            Block::H2("A.1  Boot chain".to_string()),
            sample_body(
                "Firmware hands off to the boot loader, which starts the login \
greeter. The greeter launches the session, which mounts the peer-to-peer \
filesystem before starting the compositor. First-boot setup runs once to seed \
per-user state and then disables itself.",
            ),
            Block::Spacer(10.0),
            Block::H2("A.2  Updates".to_string()),
            sample_body(
                "A timer pulls new image layers in the background and stages them for \
the next boot. Because the root is immutable, a failed update never leaves the \
system in a half-written state: the previous deployment remains bootable and is \
selected automatically if the new one fails its health check.",
            ),
        ],
    }
}

fn sample_pages() -> Vec<Page> {
    vec![
        sample_page_abstract(),
        sample_page_compositor(),
        sample_page_filesystem(),
        sample_page_toolkit(),
        sample_page_security(),
        sample_page_appendix(),
    ]
}

fn sample_outline() -> Vec<OutlineItem> {
    vec![
        OutlineItem {
            title: "Abstract".into(),
            page: 1,
            level: 0,
        },
        OutlineItem {
            title: "1  The Compositor".into(),
            page: 2,
            level: 0,
        },
        OutlineItem {
            title: "1.1  Per-cluster sandboxing".into(),
            page: 2,
            level: 1,
        },
        OutlineItem {
            title: "1.2  Remote input injection".into(),
            page: 2,
            level: 1,
        },
        OutlineItem {
            title: "2  The Filesystem".into(),
            page: 3,
            level: 0,
        },
        OutlineItem {
            title: "2.1  Projection vs. hydration".into(),
            page: 3,
            level: 1,
        },
        OutlineItem {
            title: "2.2  Conflict handling".into(),
            page: 3,
            level: 1,
        },
        OutlineItem {
            title: "3  The UI Toolkit".into(),
            page: 4,
            level: 0,
        },
        OutlineItem {
            title: "3.1  State in the document".into(),
            page: 4,
            level: 1,
        },
        OutlineItem {
            title: "3.2  Theming".into(),
            page: 4,
            level: 1,
        },
        OutlineItem {
            title: "4  Security Model".into(),
            page: 5,
            level: 0,
        },
        OutlineItem {
            title: "4.1  Capability grants".into(),
            page: 5,
            level: 1,
        },
        OutlineItem {
            title: "4.2  Signing".into(),
            page: 5,
            level: 1,
        },
        OutlineItem {
            title: "Appendix A  Image Layout".into(),
            page: 6,
            level: 0,
        },
    ]
}

/// Build the bundled sample PDF: a believable multi-page technical
/// document. Its text supports search, highlight ranges and selection.
pub fn sample_document() -> Document {
    Document {
        title: "Tilo Technical Overview".into(),
        author: "Example Systems".into(),
        subject: "Architecture specification".into(),
        pages: sample_pages(),
        outline: sample_outline(),
        page_w: PAGE_W,
        page_h: PAGE_H,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn one_page(text: &str) -> Document {
        let mut doc = Document::blank_pages("", "", 1, PAGE_W, PAGE_H);
        doc.pages[0].blocks = vec![Block::Body(text.into())];
        doc
    }

    #[test]
    fn page_map_fits_rotated_pages_and_round_trips_every_space() {
        // A 600x800 page shown rotated by 90 degrees.
        let own = tilo_pdf::PageGeometry {
            user_box: tilo_pdf::Rect::from_xywh(0.0, 0.0, 600.0, 800.0),
            rotation: tilo_pdf::Rotation::None,
        };
        let map = PageMap::new(own, 90, 600.0, 800.0);
        assert!(!map.is_identity());
        // 800x600 once rotated, fitted by width into the 600x800 card.
        assert!((map.scale - 0.75).abs() < 1e-9);
        assert!(close(map.offset, (0.0, 175.0)));
        assert!(close(map.user_to_card((0.0, 800.0)), (600.0, 175.0)));
        let p = (123.0, 456.0);
        assert!(close(map.card_to_user(map.user_to_card(p)), p));
        assert!(close(map.card_to_layer(map.layer_to_card(p)), p));
        // Layer (display) space and user space agree through the map.
        let layer = map.own.to_display(tilo_pdf::Point::new(123.0, 456.0));
        let via_layer = map.layer_to_card((f64::from(layer.x), f64::from(layer.y)));
        assert!(close(via_layer, map.user_to_card(p)));
        assert!(PageMap::new(own, 0, 600.0, 800.0).is_identity());

        // Annotations round-trip between card and user space on a page whose
        // own rotation and the view rotation differ.
        let own = tilo_pdf::PageGeometry {
            user_box: tilo_pdf::Rect::from_xywh(0.0, 0.0, 612.0, 792.0),
            rotation: tilo_pdf::Rotation::Cw90,
        };
        let map = PageMap::new(own, 270, 612.0, 792.0);
        let a = Annotation::new(
            1,
            AnnoKind::Markup {
                style: MarkupStyle::Underline,
                quads: vec![PtRect::new(100.0, 200.0, 50.0, 12.0)],
            },
            Swatch::Blue,
            "Ada",
        );
        let user = map.annotation_to_user(&a);
        assert_eq!(user.space, GeomSpace::User);
        let back = map.annotation_to_card(&user);
        let (AnnoKind::Markup { quads: q0, .. }, AnnoKind::Markup { quads: q1, .. }) =
            (&a.kind, &back.kind)
        else {
            panic!("markup");
        };
        assert!(close((q0[0].x, q0[0].y), (q1[0].x, q1[0].y)));
        assert!(close((q0[0].w, q0[0].h), (q1[0].w, q1[0].h)));
    }

    #[test]
    fn rects_normalise_and_swatches_round_trip() {
        let r = PtRect::from_corners(100.0, 80.0, 40.0, 200.0);
        assert!((r.x - 40.0).abs() < f64::EPSILON);
        assert!((r.y - 80.0).abs() < f64::EPSILON);
        assert!((r.w - 60.0).abs() < f64::EPSILON);
        assert!((r.h - 120.0).abs() < f64::EPSILON);
        assert!(r.contains(50.0, 100.0));
        assert!(!r.contains(10.0, 10.0));

        for s in Swatch::ALL {
            assert_eq!(Swatch::parse(&s.key()), s);
        }
        assert_eq!(Swatch::parse("nonsense"), Swatch::Yellow);
        let custom = Swatch::from_rgb(12, 200, 99);
        assert_eq!(custom, Swatch::Custom(0x0c_c8_63));
        assert_eq!(Swatch::parse(&custom.key()), custom);
        assert_eq!(custom.rgb(), (12, 200, 99));
        assert_eq!(Swatch::from_rgb(250, 204, 21), Swatch::Yellow);
    }

    #[test]
    fn sample_document_has_real_outline_and_searchable_text() {
        let d = sample_document();
        assert_eq!(d.page_count(), 6);
        assert!(d.page(1).is_some());
        assert!(d.page(7).is_none());
        assert!(!d.outline.is_empty());
        for item in &d.outline {
            assert!(
                d.page(item.page).is_some(),
                "outline -> missing page {}",
                item.page
            );
        }

        let hits = search(&d, "sandbox", SearchOptions::default());
        assert!(!hits.is_empty(), "expected to find 'sandbox'");
        // Page 2 discusses sandboxing.
        assert!(hits.iter().any(|m| m.page == 2));
        let m = &hits[0];
        let page = d.page(m.page).unwrap();
        let text = page.blocks[m.block].search_text().unwrap();
        assert_eq!(text[m.start..m.start + m.len].to_lowercase(), "sandbox");

        // A blank or whitespace-only query finds nothing.
        assert!(search(&d, "   ", SearchOptions::default()).is_empty());
        assert!(search(&d, "", SearchOptions::default()).is_empty());
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn search_options_match_exact_source_byte_spans() {
        const ANY: SearchOptions = SearchOptions {
            case_sensitive: false,
            whole_word: false,
        };
        const CASE: SearchOptions = SearchOptions {
            case_sensitive: true,
            whole_word: false,
        };
        const WORD: SearchOptions = SearchOptions {
            case_sensitive: false,
            whole_word: true,
        };
        let unicode = "İ test \u{212a} TEST";
        let words = "étest testé e\u{301}test test\u{301} test test_2 (test) İ";
        // (case, text, query, options, expected hits as (byte start, matched source text))
        #[allow(
            clippy::type_complexity,
            reason = "one row per search case: name, text, query, options, expected hits"
        )]
        let cases: &[(&str, &str, &str, SearchOptions, Vec<(usize, &str)>)] = &[
            // Guard against an infinite loop when a match could re-find at the
            // same spot: "aaaa" matches at 0 and 2 (non-overlapping stepping).
            (
                "overlapping progresses",
                "aaaa",
                "aa",
                ANY,
                vec![(0, "aa"), (2, "aa")],
            ),
            (
                "case-insensitive matches both 'The' and 'the'",
                "The Tilo shell. the OS.",
                "the",
                ANY,
                vec![(0, "The"), (16, "the")],
            ),
            (
                "case-sensitive matches only lowercase 'the'",
                "The Tilo shell. the OS.",
                "the",
                CASE,
                vec![(16, "the")],
            ),
            (
                "substring search finds 'ink' three times",
                "ink is thinking and ink.",
                "ink",
                ANY,
                vec![(0, "ink"), (9, "ink"), (20, "ink")],
            ),
            (
                "whole-word skips the 'ink' inside 'thinking'",
                "ink is thinking and ink.",
                "ink",
                WORD,
                vec![(0, "ink"), (20, "ink")],
            ),
            (
                "unicode case folding keeps source spans",
                unicode,
                "test",
                ANY,
                vec![
                    (unicode.find("test").unwrap(), "test"),
                    (unicode.find("TEST").unwrap(), "TEST"),
                ],
            ),
            (
                "kelvin sign folds to k",
                unicode,
                "k",
                ANY,
                vec![(unicode.find('\u{212a}').unwrap(), "\u{212a}")],
            ),
            (
                "dotted capital I expands",
                unicode,
                "i\u{307}",
                ANY,
                vec![(0, "İ")],
            ),
            ("case-sensitive kelvin misses", unicode, "k", CASE, vec![]),
            (
                "whole words use complete source words",
                words,
                "test",
                WORD,
                vec![
                    (words.find(" test test_2").unwrap() + 1, "test"),
                    (words.find("(test)").unwrap() + 1, "test"),
                ],
            ),
            (
                "accented whole word matches both spellings",
                words,
                "étest",
                WORD,
                vec![
                    (0, "étest"),
                    (words.find("e\u{301}test").unwrap(), "e\u{301}test"),
                ],
            ),
            // İ folds to a dotless base like every other accented letter, so a
            // plain "i" names the whole word.
            (
                "plain i names the dotted capital as a whole word",
                words,
                "i",
                WORD,
                vec![(words.find('İ').unwrap(), "İ")],
            ),
            (
                "full dotted expansion is a whole word",
                words,
                "i\u{307}",
                WORD,
                vec![(words.find('İ').unwrap(), "İ")],
            ),
        ];
        for (case, text, query, options, expected) in cases {
            let hits = search(&one_page(text), query, *options);
            let got: Vec<(usize, &str)> = hits
                .iter()
                .map(|m| (m.start, &text[m.start..m.start + m.len]))
                .collect();
            assert_eq!(&got, expected, "case {case}");
        }

        let d = one_page(&"a ".repeat(MAX_SEARCH_RESULTS + 100));
        assert_eq!(
            search(&d, "a", SearchOptions::default()).len(),
            MAX_SEARCH_RESULTS
        );
    }
}
