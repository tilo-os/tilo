//! A window's composition: what its widgets are and how they are put
//! together, independent of how it is painted.
//!
//! Every widget lays itself out as a node of the window's layout tree (a
//! flex or grid container, its gap, padding and sizes) and says what it is
//! through its semantics. That is a description a host can render with its
//! own elements: a page renders it as HTML, flex and grid containers as CSS
//! layout, text as text, a button as a button, a scroll region as a native
//! scroller. The toolkit's own renderer paints the same tree.
//!
//! Every widget renders itself as HTML too, next to its paint:
//! [`Widget::html`](crate::widget::Widget::html) is handed an [`HtmlCx`] and
//! says which element it is, what it shows of its own (words and how they
//! are set, a field, a document's blocks) and where its children go. A
//! widget that is genuinely drawn (a chart, a canvas, a map) renders as a
//! picture of its paint ([`HtmlCx::picture`]). Nothing here is specific to
//! the web; a host that paints ignores it.

use serde::Serialize;

/// The code that composes what only a page shows: a control's face, its
/// icon as SVG outlines. A widget's `html` reaches it through
/// [`HTML_CODE`] rather than calling it, because every widget's `html`
/// stands in its vtable and so in a program's start module; a page that
/// never composes (the canvas) does not wait for this code.
#[derive(Clone, Copy)]
pub struct HtmlCode {
    pub(crate) button: fn(&crate::widget::Button, &crate::theme::Theme) -> Option<Face>,
}

/// The code this build links; only the HTML presenter names it.
#[must_use]
pub fn builtin_html() -> HtmlCode {
    HtmlCode {
        button: crate::widget::Button::composed_face,
    }
}

crate::lazy_code! {
    /// What only a page shows. A program built with `lazy-html-faces`
    /// has its HTML presenter install it (`HTML_CODE.install(builtin_html())`);
    /// until then a face shows as its paint.
    pub static HTML_CODE: HtmlCode = builtin_html, feature = "lazy-html-faces";
}

/// What a widget shows of its own, besides its children.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Own {
    /// Words, set the way the widget sets them.
    Text(TextLook),
    /// An editable single-line or multi-line field holding `value`, set in
    /// `look`.
    Field {
        value: String,
        multiline: bool,
        placeholder: Option<String>,
        look: TextLook,
    },
    /// A document of blocks, each edited through the semantic targets its
    /// widget publishes for it (`block-text:` + its id).
    Blocks {
        blocks: Vec<DocBlock>,
        /// Where its blocks' text sets out from, as the widget lays it.
        edges: DocEdges,
    },
    /// A control's face: its icon and words on the ground it stands on.
    Face(Face),
    /// The widget's own paint stands for it: a host shows it as one picture
    /// of the widget's box. `version` changes whenever the picture may have.
    Painted { version: u64 },
}

/// A control's face ([`Own::Face`]): a leading icon, its words, a fact or
/// shortcut after them and a trailing icon, on its ground.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Face {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<Glyph>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<TextLook>,
    /// Told after the label, at the face's trailing edge when `spread`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<TextLook>,
    /// After everything else (a menu's chevron).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trailing: Option<Glyph>,
    /// Straight sRGB, 0–255; none where the face stands on its container's
    /// ground.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<[u8; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<[u8; 4]>,
    /// Logical pixels.
    pub radius: f64,
    /// From each side to the content.
    pub pad: f64,
    /// Between the icon, the label and the rest.
    pub gap: f64,
    /// The label leads and the detail trails, rather than all of it centred.
    pub spread: bool,
}

/// An icon: Lucide-style outlines in a 24 × 24 box, as SVG path data.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Glyph {
    /// Its name, for a host that keys its icons.
    pub name: &'static str,
    pub paths: Vec<String>,
    /// Its edge, logical pixels.
    pub size: f64,
    /// Its outline's width, logical pixels at `size`.
    pub stroke: f64,
    /// Straight sRGB, 0–255.
    pub color: [u8; 4],
    /// Filled rather than outlined (a toggle that is on).
    pub filled: bool,
}

/// Straight sRGB of `color`, 0–255, as a composition writes colours.
#[must_use]
pub fn rgba8(color: vello::peniko::Color) -> [u8; 4] {
    let rgba = color.to_rgba8();
    [rgba.r, rgba.g, rgba.b, rgba.a]
}

/// Where a document's text stands in its box, logical pixels: the edge a
/// paragraph's text starts at (which a title above it lines up with), and how
/// far a list item's and a nested item's text move in from it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DocEdges {
    /// From the widget's left edge to a paragraph's first glyph.
    pub text: f64,
    /// From a paragraph's edge to a list item's text.
    pub list: f64,
    /// How much further each level of nesting moves a list item in.
    pub indent: f64,
    /// From a paragraph's edge to a quote's text.
    pub quote: f64,
}

/// One block of a document ([`Own::Blocks`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocBlock {
    /// Its durable id.
    pub id: String,
    /// What it is: `paragraph`, `heading`, `quote`, `bullet`, `numbered`,
    /// `check`, `code`, `table`, `divider` or `callout`.
    pub kind: &'static str,
    /// A heading's level, a list's indent.
    pub level: u8,
    /// A checklist item is ticked.
    pub done: bool,
    /// A numbered item's number.
    pub ordinal: Option<u32>,
    /// A code block's language, a callout's kind.
    pub tag: Option<String>,
    /// It continues the block before it as one paragraph.
    pub soft_break: bool,
    pub text: String,
    /// Inline marks over byte ranges of `text`: `[start, end, mark]`, with
    /// what a link opens.
    pub marks: Vec<InlineMark>,
    /// The caret or selection, byte offsets, while the person edits it.
    pub selection: Option<[usize; 2]>,
    /// It can be edited.
    pub editable: bool,
}

/// A mark over part of a block's text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InlineMark {
    pub start: usize,
    pub end: usize,
    /// `bold`, `italic`, `code`, `underline`, `strike`, `link` or `mention`.
    pub mark: &'static str,
    /// What a link opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

/// Words and how they are set.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextLook {
    pub text: String,
    /// Logical pixels.
    pub size: f32,
    /// A multiple of `size`, as CSS's unitless `line-height`.
    pub line_height: f32,
    /// CSS weight (400, 500, 600, 700).
    pub weight: u16,
    pub italic: bool,
    /// Straight sRGB, 0–255.
    pub color: Option<[u8; 4]>,
    /// `start`, `center` or `end`.
    pub align: &'static str,
    /// The most lines it takes; what does not fit is elided.
    pub max_lines: Option<u32>,
    /// Monospaced rather than the interface face.
    pub mono: bool,
}

impl TextLook {
    /// `text` set in `style`, in `color`.
    #[must_use]
    pub fn new(
        text: String,
        style: &crate::text_render::TextStyle,
        color: Option<vello::peniko::Color>,
    ) -> Self {
        use crate::text_render::{FontChoice, ParagraphAlign, TextWeight};
        Self {
            text,
            size: style.size,
            line_height: style.line_height,
            weight: match style.weight {
                TextWeight::Regular => 400,
                TextWeight::Medium => 500,
                TextWeight::Semibold => 600,
                TextWeight::Bold => 700,
            },
            italic: style.italic,
            color: color.map(rgba8),
            align: match style.align {
                ParagraphAlign::Center => "center",
                ParagraphAlign::End => "end",
                _ => "start",
            },
            max_lines: None,
            mono: matches!(style.family, FontChoice::Monospace),
        }
    }

    #[must_use]
    pub fn with_max_lines(mut self, lines: Option<u32>) -> Self {
        self.max_lines = lines;
        self
    }
}

/// How a widget's node is laid out, as CSS says it: the declarations of its
/// layout node's style that a host's own layout engine needs to lay its
/// children out the same way.
#[must_use]
pub fn css(style: &taffy::Style) -> String {
    use std::fmt::Write as _;
    use taffy::{Dimension, Display, FlexDirection, LengthPercentage, LengthPercentageAuto};
    fn dimension(value: Dimension) -> Option<String> {
        let raw = value.into_raw();
        match raw.tag() {
            taffy::CompactLength::LENGTH_TAG => Some(format!("{}px", raw.value())),
            taffy::CompactLength::PERCENT_TAG => Some(format!("{}%", raw.value() * 100.0)),
            _ => None,
        }
    }
    fn length(value: LengthPercentage) -> String {
        let raw = value.into_raw();
        if raw.tag() == taffy::CompactLength::PERCENT_TAG {
            format!("{}%", raw.value() * 100.0)
        } else {
            format!("{}px", raw.value())
        }
    }
    fn auto(value: LengthPercentageAuto) -> String {
        let raw = value.into_raw();
        match raw.tag() {
            taffy::CompactLength::PERCENT_TAG => format!("{}%", raw.value() * 100.0),
            taffy::CompactLength::AUTO_TAG => "auto".into(),
            _ => format!("{}px", raw.value()),
        }
    }
    let mut out = String::new();
    let display = match style.display {
        Display::Flex => "flex",
        Display::Grid => "grid",
        Display::Block => "block",
        Display::None => "none",
    };
    let _ = write!(out, "display:{display};");
    if style.display == Display::Flex {
        let direction = match style.flex_direction {
            FlexDirection::Row => "row",
            FlexDirection::Column => "column",
            FlexDirection::RowReverse => "row-reverse",
            FlexDirection::ColumnReverse => "column-reverse",
        };
        let _ = write!(out, "flex-direction:{direction};");
        if style.flex_wrap == taffy::FlexWrap::Wrap {
            out.push_str("flex-wrap:wrap;");
        }
    }
    let gap = (length(style.gap.height), length(style.gap.width));
    if gap != ("0px".into(), "0px".into()) {
        let _ = write!(out, "gap:{} {};", gap.0, gap.1);
    }
    let padding = [
        length(style.padding.top),
        length(style.padding.right),
        length(style.padding.bottom),
        length(style.padding.left),
    ];
    if padding.iter().any(|side| side != "0px") {
        let _ = write!(out, "padding:{};", padding.join(" "));
    }
    let margin = [
        auto(style.margin.top),
        auto(style.margin.right),
        auto(style.margin.bottom),
        auto(style.margin.left),
    ];
    if margin.iter().any(|side| side != "0px") {
        let _ = write!(out, "margin:{};", margin.join(" "));
    }
    if let Some(align) = style.align_items {
        let _ = write!(out, "align-items:{};", align_name(align));
    }
    if let Some(align) = style.align_self {
        let _ = write!(out, "align-self:{};", align_name(align));
    }
    if let Some(justify) = style.justify_content {
        let _ = write!(out, "justify-content:{};", content_name(justify));
    }
    if style.flex_grow != 0.0 {
        let _ = write!(out, "flex-grow:{};", style.flex_grow);
    }
    if (style.flex_shrink - 1.0).abs() > f32::EPSILON {
        let _ = write!(out, "flex-shrink:{};", style.flex_shrink);
    }
    if let Some(basis) = dimension(style.flex_basis) {
        let _ = write!(out, "flex-basis:{basis};");
    }
    for (name, value) in [
        ("width", style.size.width),
        ("height", style.size.height),
        ("min-width", style.min_size.width),
        ("min-height", style.min_size.height),
        ("max-width", style.max_size.width),
        ("max-height", style.max_size.height),
    ] {
        if let Some(value) = dimension(value) {
            let _ = write!(out, "{name}:{value};");
        }
    }
    out
}

fn align_name(align: taffy::AlignItems) -> &'static str {
    use taffy::AlignItems;
    match align {
        AlignItems::Start | AlignItems::FlexStart => "flex-start",
        AlignItems::End | AlignItems::FlexEnd => "flex-end",
        AlignItems::Center => "center",
        AlignItems::Baseline => "baseline",
        AlignItems::Stretch => "stretch",
    }
}

fn content_name(align: taffy::JustifyContent) -> &'static str {
    use taffy::JustifyContent;
    match align {
        JustifyContent::Start | JustifyContent::FlexStart => "flex-start",
        JustifyContent::End | JustifyContent::FlexEnd => "flex-end",
        JustifyContent::Center => "center",
        JustifyContent::Stretch => "stretch",
        JustifyContent::SpaceBetween => "space-between",
        JustifyContent::SpaceAround => "space-around",
        JustifyContent::SpaceEvenly => "space-evenly",
    }
}

/// One widget of a composition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Composed {
    /// The element the widget renders as, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<&'static str>,
    /// The widget renders through the toolkit's default from its layout and
    /// role ([`HtmlCx::from_layout`]), not an HTML rendering of its own.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// The widget's path, as semantic actions name it: also its identity
    /// from one composition to the next.
    pub path: String,
    pub widget: String,
    /// `[x, y, width, height]`, logical pixels in the window.
    pub rect: [f64; 4],
    /// Where its children are clipped to, in the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<[f64; 4]>,
    /// Its layout node's CSS, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// It scrolls vertically: `[offset, content, viewport]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantics: Option<crate::a11y::AccessNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own: Option<Own>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focusable: bool,
    /// Its children stand where its layout node's style puts them, so a
    /// host's own layout engine lays them out from `layout`; otherwise the
    /// widget places them itself and a host places them at their `rect`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flows: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Composed>,
    /// Surfaces the widget lays under its children: `[x, y, width, height]`
    /// in the window and the theme's surface (`sidebar`, `inspector`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<([f64; 4], &'static str)>,
}

impl Composed {
    /// A widget at `path` standing at `rect` in the window, showing nothing
    /// yet: what a host that composes its own parts fills in.
    #[must_use]
    pub fn new(path: String, widget: &str, rect: [f64; 4]) -> Self {
        Self {
            tag: None,
            fallback: false,
            path,
            widget: widget.to_owned(),
            rect,
            clip: None,
            layout: None,
            scroll: None,
            semantics: None,
            own: None,
            focusable: false,
            flows: false,
            children: Vec::new(),
            surfaces: Vec::new(),
        }
    }
}

/// A window's whole composition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Composition {
    pub viewport: [u32; 2],
    /// The path of the widget keyboard focus is in.
    pub focus: Option<String>,
    pub root: Composed,
    /// What stands over the window (an open menu, the palette), in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Composed>,
}

/// What a widget's [`html`](crate::widget::Widget::html) renders into: the
/// element it is, what it shows of its own, and its children.
pub struct HtmlCx<'a> {
    pub(crate) composed: Composed,
    pub(crate) theme: &'a crate::Theme,
    /// Composes the widget's laid-out children; whether they stand where
    /// its layout node put them.
    pub(crate) compose_children: &'a mut dyn FnMut() -> (Vec<Composed>, bool),
}

impl HtmlCx<'_> {
    /// The widget is a `tag` element (`button`, `li`, `section`, `dialog`).
    pub fn element(&mut self, tag: &'static str) {
        self.composed.tag = Some(tag);
    }

    /// The widget's laid-out children, in its element, as its layout node
    /// lays them out.
    pub fn children(&mut self) {
        let (children, flows) = (self.compose_children)();
        self.composed.children = children;
        self.composed.flows = flows;
    }

    /// The widget lays the theme's `surface` (`sidebar`, `inspector`) under
    /// its children over `rect`, in window coordinates.
    pub fn surface(&mut self, rect: vello::kurbo::Rect, surface: &'static str) {
        if rect.width() > 0.0 && rect.height() > 0.0 {
            self.composed
                .surfaces
                .push(([rect.x0, rect.y0, rect.width(), rect.height()], surface));
        }
    }

    /// The widget shows words, set as `look` sets them.
    pub fn text(&mut self, look: TextLook) {
        self.composed.own = Some(Own::Text(look));
    }

    /// The widget is a control showing `face`.
    pub fn face(&mut self, face: Face) {
        self.composed.own = Some(Own::Face(face));
    }

    /// The widget is an editable field holding `value`.
    pub fn field(
        &mut self,
        value: String,
        multiline: bool,
        placeholder: Option<String>,
        look: TextLook,
    ) {
        self.element(if multiline { "textarea" } else { "input" });
        self.composed.own = Some(Own::Field {
            value,
            multiline,
            placeholder,
            look,
        });
    }

    /// The widget is a document of `blocks`, its text standing at `edges`.
    pub fn blocks(&mut self, blocks: Vec<DocBlock>, edges: DocEdges) {
        self.element("article");
        self.composed.own = Some(Own::Blocks { blocks, edges });
    }

    /// The widget is drawn: it shows as a picture of its own paint, redrawn
    /// whenever what it shows or its size may have changed.
    pub fn picture(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.composed.widget.hash(&mut hash);
        self.composed.rect[2].to_bits().hash(&mut hash);
        self.composed.rect[3].to_bits().hash(&mut hash);
        serde_json::to_string(&self.composed.semantics)
            .unwrap_or_default()
            .hash(&mut hash);
        self.composed.own = Some(Own::Painted {
            version: hash.finish(),
        });
    }

    /// The toolkit's default for a widget with no HTML rendering of its own
    /// yet: the element its role names, its laid-out children, and, for a
    /// leaf, a picture of its paint. Every widget is to replace it with its
    /// own rendering (`tools/policy/check_widget_html.py`).
    pub fn from_layout(&mut self) {
        self.composed.fallback = true;
        self.children();
        if self.composed.children.is_empty()
            && self.composed.rect[2] > 0.0
            && self.composed.rect[3] > 0.0
        {
            self.picture();
        }
    }

    /// The theme the window is drawn in: the typography and colours a widget
    /// sets its words in.
    #[must_use]
    pub fn theme(&self) -> &crate::Theme {
        self.theme
    }

    /// The widget is the chosen one of its list, as it shows it.
    pub fn selected(&mut self, selected: bool) {
        if let Some(semantics) = &mut self.composed.semantics {
            semantics.state.selected |= selected;
        }
    }

    /// Of the children it composed ([`Self::children`]), the ones standing at
    /// `chosen` are the chosen ones of its list: a list that shows its
    /// choice under its rows rather than in them says which rows hold it.
    pub fn choose_children(&mut self, chosen: &[vello::kurbo::Rect]) {
        for child in &mut self.composed.children {
            let [x, y, width, height] = child.rect;
            let stands_at = |rect: &vello::kurbo::Rect| {
                (rect.x0 - x).abs() < 0.5
                    && (rect.y0 - y).abs() < 0.5
                    && (rect.width() - width).abs() < 0.5
                    && (rect.height() - height).abs() < 0.5
            };
            if chosen.iter().any(stands_at)
                && let Some(semantics) = &mut child.semantics
            {
                semantics.state.selected = true;
            }
        }
    }

    /// What the widget says it is, for one that words its element by it.
    #[must_use]
    pub fn semantics(&self) -> Option<&crate::a11y::AccessNode> {
        self.composed.semantics.as_ref()
    }
}
