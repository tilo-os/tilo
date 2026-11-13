//! The reader's plain data: the page model, the annotation, comment and signature
//! values, the tool enum and the search routine. Page geometry is in PDF points.
//! Nothing here touches the document, the layout or the window.

use ui::tilo_resource::{self, JsonSchema, Model};
use uuid::Uuid;

pub const PAGE_W: f64 = 612.0;
pub const PAGE_H: f64 = 792.0;
pub const MARGIN: f64 = 64.0;

#[derive(Clone, Debug)]
pub struct Page {
    /// 1-based page number.
    pub number: usize,
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub page: usize,
    /// 0 = section, 1 = sub-section.
    pub level: u8,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub pages: Vec<Page>,
    pub outline: Vec<OutlineItem>,
    /// Uniform page geometry, in PDF points: a loaded PDF's first page's media box.
    pub page_w: f64,
    pub page_h: f64,
}

impl Document {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page(&self, number: usize) -> Option<&Page> {
        self.pages.iter().find(|p| p.number == number)
    }

    pub fn blank_pages(title: &str, author: &str, count: usize, page_w: f64, page_h: f64) -> Self {
        let count = count.max(1);
        let pages = (1..=count).map(|number| Page { number }).collect();
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Select,
    Highlight,
    Ink,
    Note,
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

    pub fn label(self) -> String {
        crate::t(match self {
            Tool::Select => "pdf-tool-select",
            Tool::Highlight => "pdf-tool-highlight",
            Tool::Ink => "pdf-tool-draw",
            Tool::Note => "pdf-tool-note",
            Tool::Text => "pdf-tool-text",
            Tool::Sign => "pdf-tool-sign",
        })
    }

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

    pub fn annotates(self) -> bool {
        matches!(self, Tool::Highlight | Tool::Ink | Tool::Note | Tool::Text)
    }
}

pub use tilo_document::annotation::HighlightColor;

pub fn annotation_label(kind: &AnnoKind) -> String {
    crate::t(match kind {
        AnnoKind::Markup { style, .. } => match style {
            MarkupStyle::Highlight => "pdf-annotation-highlight",
            MarkupStyle::Underline => "pdf-annotation-underline",
            MarkupStyle::StrikeOut => "pdf-annotation-strikeout",
            MarkupStyle::Squiggly => "pdf-annotation-squiggly",
        },
        AnnoKind::Ink { .. } => "pdf-annotation-drawing",
        AnnoKind::Note { .. } => "pdf-annotation-note",
        AnnoKind::FreeText { .. } => "pdf-annotation-text",
    })
}

pub fn colour_label(colour: HighlightColor) -> String {
    crate::t(match colour {
        HighlightColor::Yellow => "pdf-colour-yellow",
        HighlightColor::Green => "pdf-colour-green",
        HighlightColor::Pink => "pdf-colour-pink",
        HighlightColor::Blue => "pdf-colour-blue",
        HighlightColor::Purple => "pdf-colour-purple",
        // A colour a file brought along is named by its own value; no menu offers one.
        HighlightColor::Custom(rgb) => return format!("#{:06x}", rgb & 0x00ff_ffff),
    })
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MarkupStyle {
    Highlight,
    Underline,
    StrikeOut,
    /// A wavy underline, drawn as an underline.
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

pub const FREE_TEXT_SIZE: f64 = 14.0; // a PDF font size, not UI spacing

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

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AnnoKind {
    Markup {
        style: MarkupStyle,
        quads: Vec<PtRect>,
    },
    Ink {
        strokes: Vec<Vec<(f64, f64)>>,
    },
    Note {
        x: f64,
        y: f64,
    },
    FreeText {
        rect: PtRect,
        text: String,
        size: f64,
    },
}

impl AnnoKind {
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

    pub fn ink(points: Vec<(f64, f64)>) -> Self {
        AnnoKind::Ink {
            strokes: vec![points],
        }
    }

    pub fn markup_contains(&self, x: f64, y: f64) -> bool {
        matches!(self, AnnoKind::Markup { quads, .. } if quads.iter().any(|q| q.contains(x, y)))
    }

    pub fn free_text_contains(&self, x: f64, y: f64) -> bool {
        matches!(self, AnnoKind::FreeText { rect, .. } if rect.contains(x, y))
    }
}

/// Which space an annotation's geometry is in: records store PDF user space, drawing
/// is in page-point space, and older records stored page points.
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

#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Annotation::valid)]
pub struct Annotation {
    #[model(atom, readonly)]
    pub id: String,
    /// Page: the page occurrence it is on.
    #[model(atom)]
    pub page_id: String,
    /// The page it is shown as, 1-based, resolved from `page_id` when it is
    /// listed; 0 while its page is deleted. Not stored: page numbers change
    #[model(skip)]
    pub page: usize,
    /// Mark: what it marks, and where.
    #[model(rename = "mark")]
    pub kind: AnnoKind,
    /// Colour
    pub color: HighlightColor,
    /// Who made it.
    #[model(readonly)]
    pub author: String,
    /// The engine character range `[start, end)` text markup covers.
    #[model(readonly)]
    pub chars: Option<[u32; 2]>,
    /// When it was made (unix milliseconds; 0 when unknown).
    #[model(readonly)]
    pub created_ms: i64,
    /// Which space its geometry is in.
    pub space: GeomSpace,
}

impl Default for AnnoKind {
    fn default() -> Self {
        AnnoKind::Note { x: 0.0, y: 0.0 }
    }
}

impl tilo_resource::Identified for Annotation {
    fn id(&self) -> tilo_resource::Id {
        tilo_resource::Id::from_raw(self.id.clone())
    }

    fn order(&self, other: &Self) -> std::cmp::Ordering {
        self.created_ms.cmp(&other.created_ms)
    }
}

impl Annotation {
    pub fn new(
        page: usize,
        kind: AnnoKind,
        color: HighlightColor,
        author: impl Into<String>,
    ) -> Self {
        Self {
            id: new_id(),
            page,
            kind,
            color,
            author: author.into(),
            ..Self::default()
        }
    }

    fn valid(&self) -> Result<(), tilo_resource::Refusal> {
        let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
        let ok = match &self.kind {
            AnnoKind::Markup { quads, .. } => {
                !quads.is_empty() && quads.iter().all(|q| finite(&[q.x, q.y, q.w, q.h]))
            }
            AnnoKind::Ink { strokes } => strokes
                .iter()
                .all(|stroke| stroke.iter().all(|(x, y)| finite(&[*x, *y]))),
            AnnoKind::Note { x, y } => finite(&[*x, *y]),
            AnnoKind::FreeText { rect, size, .. } => {
                finite(&[rect.x, rect.y, rect.w, rect.h]) && size.is_finite() && *size > 0.0
            }
        };
        if !ok || self.id.trim().is_empty() || self.page_id.is_empty() {
            return Err(tilo_resource::Refusal::new(
                "an annotation is on a page and has finite geometry",
            ));
        }
        Ok(())
    }

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

pub type Comment = tilo_document::annotation::Comment<PdfAnchor>;
pub use tilo_document::annotation::Reply;

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct PdfAnchor {
    /// The page occurrence it is on.
    #[model(atom)]
    pub page_id: String,
    /// The annotation it is pinned to; empty for a comment free on a page.
    #[model(atom, readonly)]
    pub annotation: String,
}

impl PdfAnchor {
    pub fn pinned(&self) -> Option<&str> {
        (!self.annotation.is_empty()).then_some(self.annotation.as_str())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Thread {
    pub id: String,
    pub page: usize,
    pub comment: Comment,
}

impl Thread {
    pub fn pinned(&self) -> Option<&str> {
        self.comment.anchor.pinned()
    }

    pub fn replies(&self) -> impl Iterator<Item = (&str, &Reply)> {
        self.comment
            .replies
            .iter()
            .map(|(id, reply)| (id.as_str(), reply))
    }

    pub fn blocks(&self) -> Vec<ui::widget::BlockContent> {
        blocks_of(&self.comment.text, &self.comment.body)
    }
}

pub fn reply_blocks(reply: &Reply) -> Vec<ui::widget::BlockContent> {
    blocks_of(&reply.text, &reply.body)
}

fn blocks_of(text: &str, rich: &[tilo_document::ClipBlock]) -> Vec<ui::widget::BlockContent> {
    if rich.is_empty() {
        tilo_rich_text::plain_blocks(text)
    } else {
        rich.iter().map(tilo_rich_text::content_of_clip).collect()
    }
}

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// A placed signature's content: the signer's typed name, or their strokes normalised
/// into the field's `0..1` box so it scales to any placement size.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(tag = "ink", rename_all = "lowercase")]
pub enum SignatureInk {
    #[default]
    Typed,
    Drawn {
        strokes: Vec<Vec<(f64, f64)>>,
    },
}

impl SignatureInk {
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
    pub ink: SignatureInk,
    pub space: GeomSpace,
}

/// The scale that fits a `w` × `h` page into a `card_w` × `card_h` slot: by width unless
/// that would overflow the height by more than one percent.
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

/// How one displayed page maps between the spaces the reader works in: the engine's
/// layer space, the PDF user space records store, and the card space everything is
/// drawn and hit-tested in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMap {
    pub own: tilo_pdf::PageGeometry,
    pub effective: tilo_pdf::PageGeometry,
    pub scale: f64,
    pub offset: (f64, f64),
}

impl PageMap {
    pub fn new(own: tilo_pdf::PageGeometry, rotation: i32, card_w: f64, card_h: f64) -> Self {
        let extra = tilo_pdf::Rotation::from_degrees(rotation).unwrap_or_default();
        let effective = tilo_pdf::PageGeometry {
            rotation: own.rotation.then(extra),
            ..own
        };
        let size = effective.size();
        let (w, h) = (f64::from(size.width), f64::from(size.height));
        let scale = fit_scale(card_w, card_h, w, h);
        // A page the size of its slot maps exactly; float drift in page boxes must not
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

    pub fn annotation_to_card(&self, a: &Annotation) -> Annotation {
        match a.space {
            GeomSpace::Page => a.clone(),
            GeomSpace::User => {
                Self::map_annotation(a, GeomSpace::Page, self.scale, |p| self.user_to_card(p))
            }
        }
    }

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
            // The box keeps its text upright: its corners move and the text follows the fit.
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum FormEntry {
    Text(String),
    Checked { on: bool, export: Option<String> },
    Choice(String),
}

impl Default for FormEntry {
    fn default() -> Self {
        FormEntry::Text(String::new())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, JsonSchema)]
pub struct PdfText {
    pub file: String,
    pub page_count: usize,
    pub pages: Vec<PdfPageText>,
    pub next: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, JsonSchema)]
pub struct PdfPageText {
    pub page: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, JsonSchema)]
pub struct FormFields {
    pub file: String,
    pub fields: Vec<FormFieldRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, JsonSchema)]
pub struct FormFieldRow {
    pub name: String,
    pub kind: FormFieldKind,
    pub value: FormEntry,
    pub path: Option<serde_json::Value>,
    pub options: Vec<String>,
    pub export: Option<String>,
    pub page: Option<usize>,
    pub read_only: bool,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FormFieldKind {
    Text,
    Checkbox,
    Radio,
    Choice,
}

/// The find bar's toggles, both off by default as the substring search always was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Whole word: the query must be bounded by non-word characters, so "ink" misses
    /// "thinking".
    pub whole_word: bool,
    pub case_sensitive: bool,
}

pub const MAX_SEARCH_RESULTS: usize = 500;

/// Find non-overlapping spans in the original text, keeping source byte offsets: a
/// folding match must never turn an index into normalized text into one into the
/// document.
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
    // Each pair marks one original scalar boundary in normalized and source bytes.
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
        // A substring can cover one expanded scalar; a whole-word match cannot end
        // halfway through that scalar's spelling.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn page_map_fits_rotated_pages_and_round_trips_every_space() {
        let own = tilo_pdf::PageGeometry {
            user_box: tilo_pdf::Rect::from_xywh(0.0, 0.0, 600.0, 800.0),
            rotation: tilo_pdf::Rotation::None,
        };
        let map = PageMap::new(own, 90, 600.0, 800.0);
        assert!(!map.is_identity());
        assert!((map.scale - 0.75).abs() < 1e-9);
        assert!(close(map.offset, (0.0, 175.0)));
        assert!(close(map.user_to_card((0.0, 800.0)), (600.0, 175.0)));
        let p = (123.0, 456.0);
        assert!(close(map.card_to_user(map.user_to_card(p)), p));
        assert!(close(map.card_to_layer(map.layer_to_card(p)), p));
        let layer = map.own.to_display(tilo_pdf::Point::new(123.0, 456.0));
        let via_layer = map.layer_to_card((f64::from(layer.x), f64::from(layer.y)));
        assert!(close(via_layer, map.user_to_card(p)));
        assert!(PageMap::new(own, 0, 600.0, 800.0).is_identity());

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
            HighlightColor::Blue,
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

        for s in HighlightColor::ALL {
            assert_eq!(HighlightColor::parse(&s.key()), s);
        }
        assert_eq!(HighlightColor::parse("nonsense"), HighlightColor::Yellow);
        let custom = HighlightColor::from_rgb(12, 200, 99);
        assert_eq!(custom, HighlightColor::Custom(0x0c_c8_63));
        assert_eq!(HighlightColor::parse(&custom.key()), custom);
        assert_eq!(custom.rgb(), (12, 200, 99));
        assert_eq!(
            HighlightColor::from_rgb(250, 204, 21),
            HighlightColor::Yellow
        );
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
        #[allow(
            clippy::type_complexity,
            reason = "one row per search case: name, text, query, options, expected hits"
        )]
        let cases: &[(&str, &str, &str, SearchOptions, Vec<(usize, &str)>)] = &[
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
            let hits = search_text(text, query, *options, MAX_SEARCH_RESULTS);
            let got: Vec<(usize, &str)> = hits
                .iter()
                .map(|range| (range.start, &text[range.clone()]))
                .collect();
            assert_eq!(&got, expected, "case {case}");
        }

        let many = "a ".repeat(MAX_SEARCH_RESULTS + 100);
        assert_eq!(
            search_text(&many, "a", SearchOptions::default(), usize::MAX).len(),
            MAX_SEARCH_RESULTS
        );
    }
}
