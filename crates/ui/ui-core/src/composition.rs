//! A window's composition: what its widgets are and how they are put together, independent of how it is
//! painted. Every widget lays itself out as a node of the window's layout tree (a flex or grid
//! container, its gap, padding and sizes) and says what it is through its semantics.

use serde::Serialize;

pub mod page;

/// An icon as a page shows it: its outlines as SVG path data in `size` pixels on a side, in
/// `color`; `None` for a brand mark (a host paints those).
#[must_use]
pub fn glyph(
    icon: crate::icons::Icon,
    size: f64,
    color: vello::peniko::Color,
    filled: bool,
) -> Option<Glyph> {
    icon.glyph(size, color, filled)
}

/// A bar control's face as a page shows it, on `ground`.
#[must_use]
pub fn bar_control_face(
    face: &crate::widget::BarControlFace<'_>,
    theme: &crate::theme::Theme,
    ground: vello::peniko::Color,
) -> Option<Face> {
    face.composed(theme, ground)
}

/// A bar chip's face as a page shows it, on `ground`; `None` for a chip that shows as its
/// paint.
#[must_use]
pub fn bar_chip_face(
    face: &crate::widget::BarChipFace<'_>,
    theme: &crate::theme::Theme,
    ground: vello::peniko::Color,
) -> Option<Face> {
    face.composed(theme, ground)
}

/// What a widget shows of it, besides its children.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Own {
    /// Words, set the way the widget sets them.
    Text(TextLook),
    /// An editable single-line or multi-line field holding `value`, set in `look`.
    Field {
        value: String,
        multiline: bool,
        placeholder: Option<String>,
        look: TextLook,
        /// From its edges to its text, logical pixels: `[inline, block]`.
        inset: [f64; 2],
    },
    /// A document of blocks, each edited through the semantic targets its widget publishes for it
    /// (`block-text:` + its id).
    Blocks {
        blocks: Vec<DocBlock>,
        /// Where its blocks' text sets out from, as the widget lays it.
        edges: DocEdges,
    },
    /// A control's face: its icon and words on the ground it sits on.
    Face(Face),
    /// A control a page gives its own element: the box, the switch, the slider,
    /// the entry, the chooser, the strip of choices or the link, with what it
    /// holds (a widget paints several of its parts, a colour picker its swatches
    /// beside its hex field). What the person does on that element comes back
    /// through the semantic action the widget already performs. A struct
    /// variant: a page reads `{kind: "control", controls: [...]}`, and a
    /// tagged newtype holding a list cannot be written at all.
    Control { controls: Vec<Control> },
    /// The widget paint is it: a host shows it as one picture of the widget's box, drawn again
    /// when `version` changes.
    Painted {
        version: u64,
        /// The picture a page shows, as a `data:` address of a PNG
        /// ([`crate::headless::FrameHarness::paint_pictures`]); none until a host draws it.
        #[serde(skip)]
        picture: Option<std::sync::Arc<str>>,
    },
}

/// The families of control a page gives its own element ([`Own::Control`]): one per kind of thing
/// the browser and an assistive technology already know, carrying what it shows.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Control {
    /// A box, a switch or one of a set: what the person checks, and its label.
    Check {
        style: CheckStyle,
        checked: bool,
        /// Some of what it stands for is chosen.
        mixed: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<TextLook>,
    },
    /// A range: a slider from `min` to `max`, at `value`.
    Range {
        value: f64,
        min: f64,
        max: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        /// How the value is said ("50 percent"), where the app has its own words for it.
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// The value is shown in those words beside the track.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        shown: bool,
    },
    /// A typed entry: a number with its steppers, a date, a time, a colour or a search field.
    Entry {
        input: InputKind,
        /// What the field shows.
        text: String,
        placeholder: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        /// The page puts a stepper button on each side of the field.
        steppers: bool,
        /// The steppers' names, in the app's words (`increase`, `decrease`).
        #[serde(skip_serializing_if = "Option::is_none")]
        increase: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        decrease: Option<String>,
        /// What it holds is a secret: the page shows its mask and never holds its
        /// characters, which reach the widget as keys do.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        secret: bool,
    },
    /// A chooser: a button showing the current value, its options while `open`.
    Select {
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<TextLook>,
        placeholder: String,
        options: Vec<ControlOption>,
        open: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<TextLook>,
        #[serde(skip_serializing_if = "Option::is_none")]
        icon: Option<Glyph>,
        /// The mark at its end that says it opens.
        #[serde(skip_serializing_if = "Option::is_none")]
        chevron: Option<Glyph>,
    },
    /// Choices shown together, each its own button: a tab bar, a segmented strip, a menu.
    Choices {
        /// The ARIA role of the set: `tablist`, `radiogroup`, `menu` or `group`.
        list: &'static str,
        /// The ARIA role of each choice: `tab`, `radio`, `menuitem` or `button`.
        item: &'static str,
        /// What the set itself says, above its choices (a dialog's question).
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<TextLook>,
        options: Vec<ControlOption>,
    },
    /// A link: an anchor to `href`, or to the action it runs, showing its words.
    Link {
        #[serde(skip_serializing_if = "Option::is_none")]
        href: Option<String>,
        label: TextLook,
    },
}

/// What a [`Control::Entry`] holds, as the page's `<input>` says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputKind {
    /// A number, stepped by the two buttons beside it.
    Number,
    /// A calendar date.
    Date,
    /// A time of day.
    Time,
    /// A colour.
    Colour,
    /// A search query.
    Search,
}

/// What a [`Control::Check`] is, as a page's element says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStyle {
    /// A box that is marked.
    Checkbox,
    /// A switch that is on.
    Switch,
    /// One of a set, only one of which is chosen.
    Radio,
}

/// One choice of a control: a chooser's option, a tab, a menu row.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ControlOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<Glyph>,
    /// What choosing it sets, where the widget takes a value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The semantic target choosing it acts on, where the widget names its rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Straight sRGB, 0–255: the colour it shows (a swatch), where it is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<[u8; 4]>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub selected: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Where it starts from its set's leading edge, logical pixels, where the widget places its
    /// choices itself (a toolbar's tools around its spacers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// Where a separator before it stands, from the set's leading edge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<f64>,
    /// It carries a dot saying something new is behind it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dot: bool,
}

impl ControlOption {
    /// A choice showing `label` and nothing else.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            icon: None,
            value: None,
            target: None,
            colour: None,
            selected: false,
            disabled: false,
            at: None,
            rule: None,
            dot: false,
        }
    }

    /// It stands `at` from its set's leading edge, with a separator at `rule` before it.
    #[must_use]
    pub fn placed(mut self, at: f64, rule: Option<f64>) -> Self {
        self.at = Some(at);
        self.rule = rule;
        self
    }

    #[must_use]
    pub fn with_dot(mut self, dot: bool) -> Self {
        self.dot = dot;
        self
    }

    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Glyph) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_icon_opt(mut self, icon: Option<Glyph>) -> Self {
        self.icon = icon;
        self
    }

    #[must_use]
    pub fn with_colour(mut self, colour: vello::peniko::Color) -> Self {
        self.colour = Some(rgba8(colour));
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A control's face ([`Own::Face`]): a leading icon, its words, a fact or shortcut after them and a
/// trailing icon, on its ground.
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
    /// Straight sRGB, 0–255; none where the face sits on its container's
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

impl Face {
    /// A face that is `icon` alone, on its container's ground.
    pub fn glyph(icon: Glyph) -> Self {
        Self {
            icon: Some(icon),
            label: None,
            detail: None,
            trailing: None,
            fill: None,
            edge: None,
            radius: 0.0,
            pad: 0.0,
            gap: 0.0,
            spread: false,
        }
    }
}

/// The ground a widget's element is filled with, under its words and its children: a card's, a
/// banner's, a badge's. Colours are straight sRGB, 0–255.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ground {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<[u8; 4]>,
    /// Its hairline edge, drawn inside the fill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<[u8; 4]>,
    /// Its corners, logical pixels.
    pub radius: f64,
    /// How far the fill stands in from the element's sides and from its top and bottom, logical
    /// pixels: a row's lift, inset from the list's edges, while the row still reaches them.
    #[serde(skip_serializing_if = "edge_to_edge")]
    pub inset: [f64; 2],
}

fn edge_to_edge(inset: &[f64; 2]) -> bool {
    inset.iter().all(|side| side.abs() < f64::EPSILON)
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

/// `src` as an `<img>`'s: the address itself when a page can load it (a URL, a path, a `data:`
/// URI), and `None` for a name only the host resolves, whose picture shows as its paint.
#[must_use]
pub fn web_picture(src: &str) -> Option<&str> {
    const SCHEMES: [&str; 5] = ["data:", "blob:", "http://", "https://", "//"];
    let src = src.trim();
    (!src.is_empty()
        && (SCHEMES.iter().any(|scheme| src.starts_with(scheme)) || src.starts_with('/')))
    .then_some(src)
}

/// Where a document's text is in its box, logical pixels: the edge a paragraph's text starts at
/// (which a title above it lines up with), and how far a list item's and a nested item's text move in
/// from it.
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
    /// What it is: `paragraph`, `heading`, `quote`, `bullet`, `numbered`, `check`, `code`, `table`,
    /// `divider` or `callout`.
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
    /// Inline marks over byte ranges of `text`: `[start, end, mark]`, with what a link opens.
    pub marks: Vec<InlineMark>,
    /// The caret or selection, byte offsets, while the person edits it.
    pub selection: Option<[usize; 2]>,
    /// It can be edited.
    pub editable: bool,
    /// The gap above it, logical pixels, as the editor stacks it.
    pub gap: f32,
    /// Its type size, logical pixels.
    pub size: f32,
    /// Its leading, a multiple of `size`, as CSS's unitless `line-height`.
    pub line_height: f32,
    /// CSS weight.
    pub weight: u16,
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
    /// Struck through, as a done or removed item's words are painted.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub struck: bool,
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
            struck: false,
        }
    }

    /// Struck through when `struck`.
    #[must_use]
    pub fn struck(mut self, struck: bool) -> Self {
        self.struck = struck;
        self
    }

    /// Set to `align`: `start`, `center` or `end`.
    #[must_use]
    pub fn aligned(mut self, align: &'static str) -> Self {
        self.align = align;
        self
    }

    #[must_use]
    pub fn with_max_lines(mut self, lines: Option<u32>) -> Self {
        self.max_lines = lines;
        self
    }
}

/// How a widget's node is laid out, as CSS says it: the declarations of its layout node's style that a
/// host layout engine needs to lay its children out the same way.
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

/// The composed node at `path` among `nodes` and the regions they were grouped into.
fn find_mut<'a>(nodes: &'a mut [Composed], path: &str) -> Option<&'a mut Composed> {
    for node in nodes {
        if node.path == path {
            return Some(node);
        }
        if node.widget == "Region"
            && let Some(found) = find_mut(&mut node.children, path)
        {
            return Some(found);
        }
    }
    None
}

/// `layout` without the sizes the frame gave it: what a page keeps of a node's style when its
/// parent's CSS sizes it instead.
#[must_use]
pub fn without_sizes(layout: &str) -> String {
    layout
        .split(';')
        .filter(|declaration| {
            let name = declaration.split(':').next().unwrap_or_default();
            !declaration.is_empty()
                && !matches!(
                    name,
                    "width"
                        | "height"
                        | "min-width"
                        | "min-height"
                        | "max-width"
                        | "max-height"
                        | "flex-basis"
                        | "flex-grow"
                        | "flex-shrink"
                        | "margin"
                )
        })
        .map(|declaration| format!("{declaration};"))
        .collect()
}

/// Whether `node` or a widget under it already composes the control `target`.
fn composed_target(node: &Composed, target: &str) -> bool {
    node.semantics
        .as_ref()
        .and_then(|node| node.action_target.as_deref())
        == Some(target)
        || node
            .children
            .iter()
            .any(|child| composed_target(child, target))
}

/// The semantic target that reaches the synthetic child of a widget at `keys`, whose action routes
/// are keyed `namespace`: what a page sends for a row the widget paints itself (a prompt's button, a
/// swatch, a table cell).
#[must_use]
pub fn keyed_target(namespace: &str, keys: &[u64]) -> String {
    let keys = keys
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("/");
    format!("tilo.key-action:{namespace}:{keys}")
}

/// One widget of a composition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Composed {
    /// The element the widget renders as, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<&'static str>,
    /// The widget renders through the toolkit's default from its layout and role
    /// ([`HtmlCx::from_layout`]), not an HTML rendering of it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// The widget's path, as semantic actions name it: also its identity from one composition to the
    /// next.
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
    /// Its own role, words and state: a page reads a node's own fields, and a
    /// widget's synthetic children are composed as parts of their own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantics: Option<crate::a11y::AccessNode>,
    /// The ground its element is filled with, under its words and children.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground: Option<Ground>,
    /// It floats over what is under it, as a window or a panel does: its ground casts a shadow.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub floats: bool,
    /// The one side a floating body set on a screen edge casts its shadow past (`top`,
    /// `bottom`, `left`, `right`): a top bar's falls below it only. Unset, it falls all round.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_side: Option<&'static str>,
    /// Attributes its element carries beyond what its role gives it: an `<img>`'s `src` and
    /// `alt`, a `<time>`'s `datetime`, a `<progress>`'s value.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<(&'static str, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own: Option<Own>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focusable: bool,
    /// Its children stand where its layout node's style puts them, so a
    /// host layout engine lays them out from `layout`; otherwise the
    /// widget places them itself and a host places them at their `rect`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flows: bool,
    /// It places its children at their `rect`s as a canvas does, and that is what it shows:
    /// a page places them there too, in a box that scrolls, where it would otherwise flow
    /// what a widget places ([`HtmlCx::canvas_children`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub places: bool,
    /// The place whose window shows here (a `WindowWell`'s): the compositor shows the app's
    /// window over it natively, and a page's host fills it with what stands for that window
    /// on the page ([`page::Page::wells`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub well: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Composed>,
    /// Surfaces the widget lays under its children: `[x, y, width, height]` in the window and the
    /// theme's surface (`sidebar`, `inspector`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<([f64; 4], &'static str)>,
}

impl Composed {
    /// A widget at `path` at `rect` in the window, showing nothing yet: what a host that
    /// composes its parts fills in.
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
            ground: None,
            floats: false,
            shadow_side: None,
            attrs: Vec::new(),
            own: None,
            focusable: false,
            flows: false,
            places: false,
            well: None,
            children: Vec::new(),
            surfaces: Vec::new(),
        }
    }
}

impl Composed {
    /// Keep only each node's own fields: the page reads a widget's role, words
    /// and state, and a widget that names a synthetic child composed it as a
    /// part of its own. A page that never reads a subtree would otherwise take
    /// a copy of every descendant, on every frame.
    pub(crate) fn forget_semantics_children(&mut self) {
        if let Some(node) = self.semantics.as_mut() {
            node.children.clear();
        }
        for child in &mut self.children {
            child.forget_semantics_children();
        }
    }

    /// A part of it keyed `key` under its path, at `rect` in the window: a widget that
    /// lays out and paints its own content composes it from parts of its own.
    pub fn part(&mut self, key: &str, widget: &str, rect: vello::kurbo::Rect) -> &mut Composed {
        let path = format!("{}#{key}", self.path);
        let index = self.children.len();
        self.children.push(Composed::new(
            path,
            widget,
            [rect.x0, rect.y0, rect.width(), rect.height()],
        ));
        &mut self.children[index]
    }

    /// A part of it showing `icon` alone in `ink`, centred in `rect` at the size of the
    /// smaller side; a picture of it where the page has no code for that icon.
    pub fn icon(
        &mut self,
        key: &str,
        rect: vello::kurbo::Rect,
        icon: crate::icons::Icon,
        ink: vello::peniko::Color,
    ) -> &mut Self {
        let glyph = glyph(icon, rect.width().min(rect.height()), ink, false);
        let part = self.part(key, "Icon", rect);
        match glyph {
            Some(glyph) => part.own = Some(Own::Face(Face::glyph(glyph))),
            None => part.painted(0),
        }
        part
    }

    /// The part shows as a picture of the widget's paint within its rect.
    pub fn painted(&mut self, version: u64) {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        version.hash(&mut hash);
        self.rect[2].to_bits().hash(&mut hash);
        self.rect[3].to_bits().hash(&mut hash);
        self.own = Some(Own::Painted {
            version: hash.finish(),
            picture: None,
        });
    }

    /// [`Self::painted`], at the version `what` it shows names: drawn again when that changes.
    pub fn painted_of(&mut self, what: &impl std::hash::Hash) {
        use std::hash::Hasher;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        what.hash(&mut hash);
        self.painted(hash.finish());
    }
}

/// `widget`, laid out and drawn, composed alone at `path` as a host would compose it in its window,
/// without children of it: for a widget that composes its parts itself, and for its tests.
#[must_use]
pub fn compose_alone(
    widget: &dyn crate::widget::Widget,
    theme: &crate::Theme,
    path: &str,
) -> Composed {
    let rect = widget.get_rect();
    let mut composed = Composed::new(
        path.to_owned(),
        widget.name(),
        [rect.x0, rect.y0, rect.width(), rect.height()],
    );
    composed.semantics = widget
        .accessibility()
        .map(|access| crate::a11y::with_synthetic_bounds(access, widget));
    composed.focusable = widget.is_focusable();
    let mut none = |_: Option<[u8; 4]>| (Vec::new(), Vec::new(), false);
    let mut cx = HtmlCx {
        composed,
        theme,
        compose_children: &mut none,
        child_widgets: Vec::new(),
    };
    widget.html(&mut cx);
    cx.composed.forget_semantics_children();
    cx.composed
}

/// A window's whole composition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Composition {
    pub viewport: [u32; 2],
    /// The path of the widget keyboard focus is in.
    pub focus: Option<String>,
    pub root: Composed,
    /// What sits over the window (an open menu, the palette), in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<Composed>,
}

impl Composition {
    /// The widget or part at `path`, over the window or in it.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<&Composed> {
        fn find<'a>(node: &'a Composed, path: &str) -> Option<&'a Composed> {
            if node.path == path {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, path))
        }
        std::iter::once(&self.root)
            .chain(&self.layers)
            .find_map(|root| find(root, path))
    }

    /// Keep only each node's own fields
    /// ([`Composed::forget_semantics_children`]).
    pub(crate) fn forget_semantics_children(&mut self) {
        self.root.forget_semantics_children();
        for layer in &mut self.layers {
            layer.forget_semantics_children();
        }
    }
}

impl Composed {
    /// The widget moved as a whole by `[dx, dy]` logical pixels: every rect it holds, its parts'
    /// and its children's, stands that much further on.
    pub(crate) fn translate(&mut self, [dx, dy]: [f64; 2]) {
        let shift = |rect: &mut [f64; 4]| {
            rect[0] += dx;
            rect[1] += dy;
        };
        shift(&mut self.rect);
        if let Some(clip) = self.clip.as_mut() {
            shift(clip);
        }
        for (rect, _) in &mut self.surfaces {
            shift(rect);
        }
        if let Some(bounds) = self
            .semantics
            .as_mut()
            .and_then(|node| node.bounds.as_mut())
        {
            bounds.x0 += dx;
            bounds.x1 += dx;
            bounds.y0 += dy;
            bounds.y1 += dy;
        }
        for child in &mut self.children {
            child.translate([dx, dy]);
        }
    }
}

/// Composes a widget's laid-out children on the ground it fills: the composed
/// children, the address of each widget (0 for a layout node that is no widget),
/// and whether they stood where the layout node put them.
pub(crate) type ComposeChildren<'a> =
    &'a mut dyn FnMut(Option<[u8; 4]>) -> (Vec<Composed>, Vec<usize>, bool);

/// What a widget's [`html`](crate::widget::Widget::html) renders into: the element it is, what it shows
/// of it, and its children.
pub struct HtmlCx<'a> {
    pub(crate) composed: Composed,
    pub(crate) theme: &'a crate::Theme,
    /// Composes the widget's laid-out children on the ground it fills (straight sRGB), in the
    /// theme its paint hands them there; whether they stand where its layout node put them.
    /// Composes its children; with them, the address of the widget each is (0 for a layout
    /// node that is no widget).
    pub(crate) compose_children: ComposeChildren<'a>,
    /// The path each child widget composed at, by the widget's address: what [`Self::region`]
    /// and [`Self::restyle`] find a widget's element by, wherever the widget moved it since.
    pub(crate) child_widgets: Vec<(usize, String)>,
}

impl<'a> HtmlCx<'a> {
    /// The element itself, for a look composed by a shared face ([`crate::widget::AvatarFace`]).
    pub fn node(&mut self) -> &mut Composed {
        &mut self.composed
    }

    /// The widget is a `tag` element (`button`, `li`, `section`, `dialog`).
    pub fn element(&mut self, tag: &'static str) {
        self.composed.tag = Some(tag);
    }

    /// The widget's laid-out children, in its element, as its layout node lays them out.
    pub fn children(&mut self) {
        let ground = self.composed.ground.as_ref().and_then(|ground| ground.fill);
        let (children, widgets, flows) = (self.compose_children)(ground);
        self.child_widgets = widgets
            .into_iter()
            .zip(&children)
            .filter(|(address, _)| *address != 0)
            .map(|(address, child)| (address, child.path.clone()))
            .collect();
        self.composed.children = children;
        self.composed.flows = flows;
    }

    /// The widget's laid-out children, each where the widget placed it: a canvas, whose
    /// children stand at points of their own rather than in a flow, and whose page places them
    /// at those points too ([`Composed::places`]).
    pub fn canvas_children(&mut self) {
        self.children();
        self.composed.places = true;
    }

    /// The widget is where the window of the place `place` shows: a host fills it
    /// ([`Composed::well`]).
    pub fn well(&mut self, place: &str) {
        self.composed.well = Some(place.to_owned());
    }

    /// The composed children ([`Self::children`]) that are `widgets`, taken out of where they
    /// stood and put, in the order given, in a `tag` element of their own keyed `key`, which lays
    /// them out as `css` says: a column of a shell, a header row. A widget that places its
    /// children at rects itself groups them this way, so a page lays them out with CSS instead of
    /// at where the frame put them. The region stands where the first of them stood; a widget
    /// that composed nothing (one not laid out now) is left out. Returns whether it holds any.
    pub fn region(
        &mut self,
        key: &str,
        tag: &'static str,
        css: &str,
        widgets: &[&dyn crate::widget::Widget],
    ) -> bool {
        let paths: Vec<String> = widgets
            .iter()
            .filter_map(|widget| self.child_path(*widget).map(str::to_owned))
            .collect();
        self.group(key, tag, css, |child| {
            paths.iter().position(|path| *path == child.path)
        })
    }

    /// The path the child that is `widget` composed at.
    pub(crate) fn child_path(&self, widget: &dyn crate::widget::Widget) -> Option<&str> {
        let address = std::ptr::from_ref(widget).cast::<()>() as usize;
        self.child_widgets
            .iter()
            .find(|(at, _)| *at == address)
            .map(|(_, path)| path.as_str())
    }

    /// The composed children `member` ranks, taken out of where they stood and put, in its
    /// order, in a `tag` element of their own keyed `key`, laid out as `css` says
    /// ([`Self::region`]). Returns whether it holds any.
    pub fn group(
        &mut self,
        key: &str,
        tag: &'static str,
        css: &str,
        member: impl Fn(&Composed) -> Option<usize>,
    ) -> bool {
        let mut at = None;
        let mut members = Vec::new();
        let mut index = 0;
        self.composed.children.retain(|child| {
            let rank = member(child);
            if let Some(rank) = rank {
                at.get_or_insert(index);
                members.push((rank, child.clone()));
            } else {
                index += 1;
            }
            rank.is_none()
        });
        let Some(at) = at else {
            return false;
        };
        members.sort_by_key(|(rank, _)| *rank);
        let members: Vec<Composed> = members.into_iter().map(|(_, child)| child).collect();
        let rect = members.iter().fold(None::<[f64; 4]>, |bounds, member| {
            let [x, y, w, h] = member.rect;
            Some(bounds.map_or(member.rect, |[bx, by, bw, bh]| {
                let (x0, y0) = (bx.min(x), by.min(y));
                let (x1, y1) = ((bx + bw).max(x + w), (by + bh).max(y + h));
                [x0, y0, x1 - x0, y1 - y0]
            }))
        });
        let mut region = Composed::new(
            format!("{}#{key}", self.composed.path),
            "Region",
            rect.unwrap_or_default(),
        );
        region.tag = Some(tag);
        region.layout = Some(css.to_owned());
        region.flows = true;
        region.children = members;
        self.composed.children.insert(at, region);
        self.composed.flows = true;
        true
    }

    /// The widget's children flow as `css` lays them out (`display:flex;flex-direction:column`),
    /// in place of where it puts them itself: what a widget that places its children by hand
    /// says it does, so a page lays them out the same way at any width.
    pub fn flow(&mut self, css: &str) {
        self.composed.layout = Some(css.to_owned());
        self.composed.flows = true;
    }

    /// The composed child that is `widget` is laid out by `css` instead of its layout node's
    /// style: what its parent's CSS layout asks of it (to fill a column) rather than the size the
    /// frame gave it.
    pub fn restyle(
        &mut self,
        widget: &dyn crate::widget::Widget,
        css: impl FnOnce(&str) -> String,
    ) {
        let Some(path) = self.child_path(widget).map(str::to_owned) else {
            return;
        };
        if let Some(child) = find_mut(&mut self.composed.children, &path) {
            // A child kept from the frame before holds the style given it then: a declaration
            // given again replaces the one before rather than piling up behind it.
            let styled = css(child.layout.as_deref().unwrap_or_default());
            let declarations: Vec<&str> = styled.split(';').filter(|d| !d.is_empty()).collect();
            let mut layout = String::new();
            for (at, declaration) in declarations.iter().enumerate() {
                if !declarations[at + 1..].contains(declaration) {
                    layout.push_str(declaration);
                    layout.push(';');
                }
            }
            child.layout = Some(layout);
        }
    }

    /// The widget places its children ([`Self::children`]) at their rects itself, among parts
    /// it places there too ([`Self::part`]), rather than as its layout node flows them.
    pub fn place_children(&mut self) {
        self.composed.flows = false;
    }

    /// The widget lays the theme's `surface` (`sidebar`, `inspector`) under its children over `rect`,
    /// in window coordinates.
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

    /// One of the widget parts, which it lays out and paints itself rather than holding as a widget of
    /// it (a bar's controls): keyed `key` under the widget's path, at `rect` in the window.
    pub fn part(&mut self, key: &str, widget: &str, rect: vello::kurbo::Rect) -> &mut Composed {
        self.composed.part(key, widget, rect)
    }

    /// A part of it showing `icon` alone in `ink`, centred in `rect` ([`Composed::icon`]).
    pub fn icon(
        &mut self,
        key: &str,
        rect: vello::kurbo::Rect,
        icon: crate::icons::Icon,
        ink: vello::peniko::Color,
    ) -> &mut Composed {
        self.composed.icon(key, rect, icon, ink)
    }

    /// A part of it showing `look`'s words as a `tag` element (`h3`, `dt`, `td`) at `rect`.
    pub fn words(
        &mut self,
        key: &str,
        tag: &'static str,
        rect: vello::kurbo::Rect,
        look: TextLook,
    ) -> &mut Composed {
        let part = self.part(key, "Text", rect);
        part.tag = Some(tag);
        part.own = Some(Own::Text(look));
        part
    }

    /// `widget`, which the widget lays out and draws itself rather than through its layout
    /// node, composed as a part keyed `key`: a banner's buttons.
    pub fn part_widget(&mut self, key: &str, widget: &dyn crate::widget::Widget) {
        let rect = widget.get_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let path = format!("{}#{key}", self.composed.path);
        self.composed
            .children
            .push(compose_alone(widget, self.theme, &path));
    }

    /// The widget's element is filled with `ground`, under its words and its children.
    pub fn ground(&mut self, ground: Ground) {
        self.composed.ground = Some(ground);
    }

    /// Its element carries `name` `value`: an `<img>`'s `alt`, a `<time>`'s `datetime`, a
    /// `<progress>`'s `max`.
    pub fn attr(&mut self, name: &'static str, value: impl Into<String>) {
        self.composed.attrs.push((name, value.into()));
    }

    /// The part composed last ([`Self::part`]), to say what it shows.
    pub fn last_part(&mut self) -> Option<&mut Composed> {
        self.composed.children.last_mut()
    }

    /// Every control `access` names with where it stands, as a button keyed `key` under the
    /// widget's path: what a widget that paints its controls tells a reader of is what a page
    /// shows as buttons, while the paint keeps the look.
    pub fn controls(&mut self, key: &str, access: &crate::a11y::AccessNode) {
        fn walk(cx: &mut HtmlCx<'_>, node: &crate::a11y::AccessNode, key: &str, index: &mut usize) {
            for child in &node.children {
                if let Some(bounds) = child.bounds
                    && (child.state.focusable || child.action_target.is_some())
                {
                    let rect = bounds.to_rect();
                    if !rect.is_zero_area() {
                        *index = index.saturating_add(1);
                        let part = cx.part(&format!("{key}:{index}"), "ComposedControl", rect);
                        part.tag = Some("button");
                        part.focusable = true;
                        part.semantics = Some(child.clone());
                    }
                }
                walk(cx, child, key, index);
            }
        }
        let mut index = 0;
        walk(self, access, key, &mut index);
    }

    /// Every node of the widget's own semantics ([`Self::semantics`]) as a page element of its
    /// own, and the nodes under it: what a widget that shows a structure it publishes for a
    /// reader gives a page to lay out and search.
    ///
    /// Each node is its role's element — a group a `<section>`, a button a `<button>`, a list's
    /// row an `<li>`, a leaf a `<span>` — and a node whose `value` is a date, a time or a span of
    /// them says it in a `<time datetime="…">`: the element itself where the node is a leaf, and
    /// a `<time>` beside the node's words where it has them (a day's date above its events, an
    /// event's hours under its title). Its time shows the node's description, or its name where
    /// it has none. A node's `action_target` comes back the same way a press on it does, so a
    /// page's element acts as the widget's own control.
    ///
    /// Each node stands where its bounds say; one with none stands where its parent does.
    pub fn semantic_children(
        &mut self,
        key: &str,
        look: &dyn Fn(&crate::a11y::AccessNode) -> SemanticText,
    ) {
        let Some(root) = self.composed.semantics.clone() else {
            return;
        };
        let fallback = self.composed.rect;
        let fallback = vello::kurbo::Rect::new(
            fallback[0],
            fallback[1],
            fallback[0] + fallback[2],
            fallback[1] + fallback[3],
        );
        for (index, child) in root.children.iter().enumerate() {
            semantic_parts(
                &mut self.composed,
                &format!("{key}.{index}"),
                child,
                look,
                fallback,
            );
        }
    }

    /// The widget is a control showing `face`.
    pub fn face(&mut self, face: Face) {
        self.composed.own = Some(Own::Face(face));
    }

    /// The widget is a control a page gives its own element, holding what `control` says; a
    /// widget that shows several parts of one control (a colour's swatches beside its hex field)
    /// adds them.
    pub fn control(&mut self, control: Control) {
        match &mut self.composed.own {
            Some(Own::Control { controls }) => controls.push(control),
            _ => {
                self.composed.own = Some(Own::Control {
                    controls: vec![control],
                });
            }
        }
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
            inset: [0.0, 0.0],
        });
    }

    /// The field ([`Self::field`]) keeps its text `x` from its sides and `y` from its top and
    /// bottom, as its chrome does.
    pub fn field_inset(&mut self, x: f64, y: f64) {
        if let Some(Own::Field { inset, .. }) = &mut self.composed.own {
            *inset = [x, y];
        }
    }

    /// The widget is a document of `blocks`, its text at `edges`.
    pub fn blocks(&mut self, blocks: Vec<DocBlock>, edges: DocEdges) {
        self.element("article");
        self.composed.own = Some(Own::Blocks { blocks, edges });
    }

    /// Each control the widget's accessibility tree names — a target with bounds — as a part
    /// where it stands: a real button a page, a reader and the keyboard reach, over what
    /// the widget draws itself. One the widget already composed ([`Self::part`]) is not composed
    /// twice.
    pub fn targets(&mut self) {
        let Some(root) = self.composed.semantics.clone() else {
            return;
        };
        let mut found = Vec::new();
        let mut pending = std::collections::VecDeque::from([root]);
        while let Some(node) = pending.pop_front() {
            pending.extend(node.children.iter().cloned());
            let Some(target) = node.action_target.clone() else {
                continue;
            };
            let Some(bounds) = node.bounds else {
                continue;
            };
            let rect = bounds.to_rect();
            if rect.width() > 0.0 && rect.height() > 0.0 {
                found.push((target, rect, node));
            }
        }
        for (target, rect, node) in found {
            if composed_target(&self.composed, &target) {
                continue;
            }
            let part = self.part(&format!("target:{target}"), "Target", rect);
            part.tag = Some("button");
            part.focusable = true;
            part.semantics = Some(node);
        }
    }

    /// The widget's `version` of what it draws, with the controls it names ([`Self::targets`]) over
    /// it: for a widget that draws its own tabs, buttons and toggles rather than holding them as
    /// widgets of it.
    pub fn painted_targets(&mut self, version: u64) {
        self.painted(version);
        self.targets();
    }

    /// The widget is drawn at `version` — a count it bumps each time it paints anew: it shows as a
    /// picture of its paint, fetched again when the version changes.
    pub fn painted(&mut self, version: u64) {
        self.composed.painted(version);
    }

    /// The widget is drawn: it shows as a picture of it paint, redrawn whenever what it shows or its
    /// size may have changed.
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
            picture: None,
        });
    }

    /// The toolkit's default for a widget with no HTML rendering of it
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

    /// The theme the window is drawn in: the typography and colours a widget sets its words in.
    /// It outlives the composition it is read in, so a widget can hold it while it composes.
    #[must_use]
    pub fn theme(&self) -> &'a crate::Theme {
        self.theme
    }

    /// The widget is the chosen one of its list, as it shows it.
    pub fn selected(&mut self, selected: bool) {
        if let Some(semantics) = &mut self.composed.semantics {
            semantics.state.selected |= selected;
        }
    }

    /// Of the children it composed ([`Self::children`]), the ones at `chosen` are the chosen
    /// ones of its list: a list that shows its choice under its rows rather than in them says which
    /// rows hold it.
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

/// How a page sets the words of one semantic node and of the time it holds
/// ([`HtmlCx::semantic_children`]): the look carries type and colour, and the node's own words
/// replace the ones it was built with.
#[derive(Clone, Debug)]
pub struct SemanticText {
    /// The look of the node's own words: an event's title, a row's name.
    pub words: TextLook,
    /// The look of the time it carries: a day's date, an event's hours.
    pub time: TextLook,
    /// The ground its element is filled with, where it has one of its own (a chip's tint).
    pub ground: Option<Ground>,
}

impl SemanticText {
    /// The words of a node and of its time set as given, on its container's ground.
    #[must_use]
    pub fn new(words: TextLook, time: TextLook) -> Self {
        Self {
            words,
            time,
            ground: None,
        }
    }

    /// Its element is filled with `ground` under its words.
    #[must_use]
    pub fn on(mut self, ground: Ground) -> Self {
        self.ground = Some(ground);
        self
    }
}

/// Whether `value` is a date, a time, a moment or a span of them: what a `<time>`'s `datetime`
/// holds, and what makes a semantic node one a page shows as time.
fn is_time_value(value: &str) -> bool {
    // A clock or a date says so in its punctuation; a count, a percentage or a word does not.
    let plain = |part: &str| {
        (part.contains(':') || part.contains('-'))
            && (part.parse::<jiff::civil::DateTime>().is_ok()
                || part.parse::<jiff::civil::Date>().is_ok()
                || part.parse::<jiff::civil::Time>().is_ok())
    };
    let mut parts = value.split('/');
    let (Some(start), end) = (parts.next(), parts.next()) else {
        return false;
    };
    plain(start) && end.is_none_or(plain) && parts.next().is_none()
}

/// The element a semantic node is: its role's, or a `<time>` where it is a leaf that carries one.
fn semantic_tag(node: &crate::a11y::AccessNode) -> &'static str {
    use crate::a11y::Role;
    if node.role == Role::Group {
        return "section";
    }
    if node.value.as_deref().is_some_and(is_time_value)
        && matches!(
            node.role,
            Role::StaticText | Role::Paragraph | Role::Label | Role::Heading { .. }
        )
    {
        return "time";
    }
    match node.role {
        Role::Button | Role::ToggleButton => "button",
        Role::ListItem => "li",
        Role::Link => "a",
        Role::Paragraph => "p",
        _ => "span",
    }
}

/// How a semantic node's element is laid out by the page.
fn semantic_layout(tag: &str) -> &'static str {
    match tag {
        "section" => {
            "display:flex;flex-direction:column;gap:6px;min-width:0;\
             padding:8px;box-sizing:border-box;"
        }
        "button" => {
            "display:flex;flex-direction:column;align-items:flex-start;gap:2px;\
             text-align:start;border:0;padding:6px 8px;min-width:0;overflow-wrap:anywhere;"
        }
        "time" => "display:block;font-variant-numeric:tabular-nums;",
        "li" => "display:block;min-width:0;overflow-wrap:anywhere;",
        _ => "display:block;min-width:0;",
    }
}

/// `node` and the nodes under it as parts of `composed`: [`HtmlCx::semantic_children`].
fn semantic_parts(
    composed: &mut Composed,
    key: &str,
    node: &crate::a11y::AccessNode,
    look: &dyn Fn(&crate::a11y::AccessNode) -> SemanticText,
    fallback: vello::kurbo::Rect,
) {
    let area = node
        .bounds
        .map_or(fallback, crate::a11y::AccessBounds::to_rect);
    let style = look(node);
    let time = node.value.as_deref().filter(|value| is_time_value(value));
    let tag = semantic_tag(node);
    let words = |look: &TextLook, text: &str| {
        let mut look = look.clone();
        text.clone_into(&mut look.text);
        look
    };
    let name_look = node.name.as_ref().map(|name| words(&style.words, name));
    let time_words = node
        .description
        .clone()
        .filter(|description| !description.trim().is_empty())
        .or_else(|| node.name.clone());
    let part = composed.part(key, "Semantics", area);
    part.tag = Some(tag);
    part.layout = Some(semantic_layout(tag).to_owned());
    part.focusable = node.state.focusable || node.action_target.is_some();
    part.semantics = Some(node.clone());
    part.ground = style.ground;
    if tag == "time" {
        if let Some(value) = time {
            part.attrs.push(("datetime", value.to_owned()));
        }
        part.own = name_look.map(Own::Text);
    } else if node.role != crate::a11y::Role::Group
        && let Some(look) = name_look
    {
        part.own = Some(Own::Text(look));
    }
    if tag != "time"
        && let Some(value) = time
        && let Some(text) = time_words
    {
        let time_part = part.part("time", "Semantics", area);
        time_part.tag = Some("time");
        time_part.layout = Some(semantic_layout("time").to_owned());
        time_part.attrs.push(("datetime", value.to_owned()));
        time_part.own = Some(Own::Text(words(&style.time, &text)));
    }
    for (index, child) in node.children.iter().enumerate() {
        semantic_parts(part, &index.to_string(), child, look, area);
    }
}

#[cfg(test)]
mod tests {
    use super::{Composed, Control, ControlOption, InputKind, Own};
    use crate::a11y::Action;
    use crate::headless::FrameHarness;

    /// The composed widget named `widget`, depth first.
    fn find<'a>(node: &'a Composed, widget: &str) -> Option<&'a Composed> {
        if node.widget == widget {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, widget))
    }

    fn controls(node: &Composed) -> &[Control] {
        match node.own.as_ref() {
            Some(Own::Control { controls }) => controls,
            other => panic!("{} composes {other:?}, not controls", node.widget),
        }
    }

    #[test]
    fn a_frame_shown_as_its_composition_paints_no_scene() {
        let mut frame = FrameHarness::new(
            crate::State::new(),
            Box::new(crate::widget::TextInput::local()),
            (400, 80),
        );
        frame.set_scene_drawn(false);
        frame.relayout();
        assert!(
            frame.scene().encoding().is_empty(),
            "a frame shown as its composition paints nothing"
        );
        // The composition it shows instead is the frame's own.
        let composition = frame.composition();
        assert!(find(&composition.root, "TextInput").is_some());
        assert!(composition.root.rect[2] > 0.0);
    }

    #[test]
    fn a_checkbox_is_a_label_around_its_box() {
        let state = crate::LocalState::new(false);
        let mut host = FrameHarness::new(
            crate::State::new(),
            Box::new(crate::widget::Checkbox::from_state(&state).with_label("Show hidden files")),
            (420, 60),
        );
        let composition = host.composition();
        let checkbox = find(&composition.root, "Checkbox").expect("the box is composed");
        assert_eq!(checkbox.tag, Some("label"));
        let [control] = controls(checkbox) else {
            panic!("one box");
        };
        let Control::Check {
            style,
            checked,
            mixed,
            label,
        } = control
        else {
            panic!("a check, not {control:?}");
        };
        assert_eq!(*style, super::CheckStyle::Checkbox);
        assert!(!checked);
        assert!(!mixed);
        assert_eq!(label.as_ref().unwrap().text, "Show hidden files");
        // A page reads it as its kind and its list of controls.
        let json = serde_json::to_value(&composition).expect("a composition with a box is written");
        let own = &serde_json::to_value(checkbox).unwrap()["own"];
        assert_eq!(own["kind"], "control");
        assert_eq!(own["controls"][0]["kind"], "check");
        assert!(json["root"].is_object());

        // What the page does on the box is the widget's own action: the next
        // frame shows it marked.
        host.perform_named(&checkbox.path, &Action::ToggleCheck)
            .expect("the box takes the toggle");
        let composition = host.composition();
        let checkbox = find(&composition.root, "Checkbox").unwrap();
        assert!(matches!(
            controls(checkbox)[0],
            Control::Check { checked: true, .. }
        ));
    }

    #[test]
    fn a_switch_is_a_switch_and_a_radio_is_one_of_a_set() {
        let toggle = crate::widget::Toggle::from_state(&crate::LocalState::new(true));
        let composed = super::compose_alone(&toggle, &crate::Theme::dark(), "switch");
        assert_eq!(composed.tag, Some("label"));
        assert!(matches!(
            controls(&composed)[0],
            Control::Check {
                style: super::CheckStyle::Switch,
                checked: true,
                ..
            }
        ));

        let radio = crate::widget::Checkbox::bound("any", "path").radio();
        let composed = super::compose_alone(&radio, &crate::Theme::dark(), "radio");
        assert!(matches!(
            controls(&composed)[0],
            Control::Check {
                style: super::CheckStyle::Radio,
                ..
            }
        ));
    }

    #[test]
    fn a_slider_is_a_range_and_the_page_sets_its_value() {
        let state = crate::LocalState::new(0.25);
        let mut host = FrameHarness::new(
            crate::State::new(),
            Box::new(crate::widget::Slider::from_state(&state).range(0.0, 2.0)),
            (300, 80),
        );
        let composition = host.composition();
        let slider = find(&composition.root, "Slider").expect("the slider is composed");
        let [
            Control::Range {
                value, min, max, ..
            },
        ] = controls(slider)
        else {
            panic!("one range");
        };
        assert_eq!((*value, *min, *max), (0.25, 0.0, 2.0));
        host.perform_named(&slider.path, &Action::SetValue("1.5".into()))
            .expect("the range takes a value");
        assert!((state.get() - 1.5).abs() < 1e-9, "the value reached it");
        let composition = host.composition();
        let slider = find(&composition.root, "Slider").unwrap();
        assert!(
            matches!(controls(slider)[0], Control::Range { value, .. } if (value - 1.5).abs() < 1e-9)
        );
    }

    #[test]
    fn a_number_is_an_entry_with_its_steppers() {
        let input = crate::widget::NumberInput::new(12.0)
            .with_range(0.0, 100.0)
            .with_suffix("px");
        let composed = super::compose_alone(&input, &crate::Theme::dark(), "number");
        let [
            Control::Entry {
                input,
                text,
                steppers,
                min,
                max,
                ..
            },
        ] = controls(&composed)
        else {
            panic!("one entry");
        };
        assert_eq!(*input, InputKind::Number);
        assert_eq!(text, "12 px");
        assert!(steppers);
        assert_eq!((*min, *max), (Some(0.0), Some(100.0)));
    }

    #[test]
    fn a_link_is_an_anchor() {
        let link = crate::widget::Link::new("https://tilo.coffee").with_label("Tilo");
        let composed = super::compose_alone(&link, &crate::Theme::dark(), "link");
        assert_eq!(composed.tag, Some("a"));
        let [Control::Link { href, label }] = controls(&composed) else {
            panic!("one link");
        };
        assert_eq!(href.as_deref(), Some("https://tilo.coffee"));
        assert_eq!(label.text, "Tilo");
    }

    #[test]
    fn a_list_row_carries_its_icon_its_title_and_the_line_under_it() {
        let host = FrameHarness::new(
            crate::State::new(),
            Box::new(
                crate::widget::ListRow::new("basics", "Basics")
                    .secondary("Type, buttons, chips, cards and empty states")
                    .value("12")
                    .icon(crate::icons::Icon::LayoutGrid),
            ),
            (640, 80),
        );
        let composition = host.composition();
        let row = find(&composition.root, "ListRow").expect("the row is composed");
        assert_eq!(row.tag, Some("li"));
        assert!(row.flows, "a page lays its parts out, at any width");
        fn under<'a>(node: &'a Composed, key: &str) -> Option<&'a Composed> {
            node.children.iter().find_map(|part| {
                if part.path.ends_with(&format!("#{key}")) {
                    Some(part)
                } else {
                    under(part, key)
                }
            })
        }
        let part = |key: &str| {
            under(row, key).unwrap_or_else(|| panic!("no {key} part in {:?}", row.children))
        };
        let words = |key: &str| match &part(key).own {
            Some(Own::Text(look)) => look.clone(),
            other => panic!("{key} shows {other:?}"),
        };
        assert_eq!(words("title").text, "Basics");
        assert_eq!(
            words("secondary").text,
            "Type, buttons, chips, cards and empty states"
        );
        assert_eq!(words("value").text, "12");
        assert!(matches!(&part("leading").own, Some(Own::Face(face)) if face.icon.is_some()));
        // The icon leads, then the words (the title's line over the line under it), then the
        // value; none of them where the frame put it.
        let order: Vec<&str> = row
            .children
            .iter()
            .filter_map(|part| part.path.rsplit('#').next())
            .collect();
        assert_eq!(order, ["leading", "words", "value"]);
        let words_region = part("words");
        let lines: Vec<&str> = words_region
            .children
            .iter()
            .filter_map(|part| part.path.rsplit('#').next())
            .collect();
        assert_eq!(lines, ["title-line", "secondary"]);
        assert!(
            words_region
                .layout
                .as_deref()
                .is_some_and(|css| css.contains("flex-direction:column"))
        );
    }

    #[test]
    fn a_list_rows_box_is_a_checkbox_the_page_toggles() {
        let checked = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = checked.clone();
        let mut host = FrameHarness::new(
            crate::State::new(),
            Box::new(
                crate::widget::ListRow::new("milk", "Milk")
                    .checkbox(false, move |on| seen.set(on)),
            ),
            (400, 60),
        );
        let composition = host.composition();
        let row = find(&composition.root, "ListRow").expect("the row is composed");
        let check = find(row, "ListRowCheck").expect("its box is composed");
        assert!(matches!(
            controls(check),
            [Control::Check { checked: false, .. }]
        ));
        let target = check
            .semantics
            .as_ref()
            .and_then(|node| node.action_target.clone())
            .expect("the box names its target");
        host.perform_semantic_named(&row.path, Some(&target), &Action::ToggleCheck)
            .expect("the box takes the toggle");
        assert!(checked.get(), "the press reached the row's box");
    }

    #[test]
    fn a_time_value_is_a_date_a_moment_a_clock_or_a_span_of_them() {
        assert!(super::is_time_value("2026-08-31"));
        assert!(super::is_time_value("09:00"));
        assert!(super::is_time_value("2026-08-31T09:00:00"));
        assert!(super::is_time_value(
            "2026-08-31T09:00:00/2026-08-31T10:30:00"
        ));
        assert!(super::is_time_value("2026-11-02/2026-11-05"));
        assert!(!super::is_time_value(""));
        assert!(!super::is_time_value("12"));
        assert!(!super::is_time_value("50 percent"));
        assert!(!super::is_time_value("2026-08-31/2026-08-31/2026-08-31"));
    }

    #[test]
    fn a_choice_shows_its_target_and_value_serialises_for_the_page() {
        let option = ControlOption::new("Bold")
            .with_target("toolbar.item.0")
            .selected(true)
            .with_detail("Ctrl+B");
        let control = Control::Choices {
            list: "menu",
            item: "menuitem",
            label: None,
            options: vec![option],
        };
        let json = serde_json::to_value(&control).unwrap();
        assert_eq!(json["kind"], "choices");
        assert_eq!(json["list"], "menu");
        assert_eq!(json["options"][0]["target"], "toolbar.item.0");
        assert_eq!(json["options"][0]["selected"], true);
        assert_eq!(json["options"][0]["detail"], "Ctrl+B");
    }
}
