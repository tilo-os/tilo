//! Frame from a `DevTools` `DOMSnapshot.captureSnapshot` result.
//!
//! The snapshot is the engine's own layout: every box and text line with the
//! bounds the engine computed, in document coordinates, plus paint order. This
//! module groups those into layers and makes every item relative to its layer,
//! which is what keeps scroll and sticky motion out of the item lists.
//!
//! Capture a snapshot with [`COMPUTED_STYLES`]; pass the result to [`convert`].
//!
//! A snapshot is layout, not paint: what the engine's own theme draws of a
//! form control (a field's edge, a select's mark, a ticked box, the ring
//! around the field being edited) is not in it. [`controls`] draws those
//! from what the snapshot does say of each control, as items every client
//! already draws. What a field shows where that is not what it holds (a date
//! in the engine's own words) the engine is asked for: [`asks`] names those
//! fields, and [`convert_with`] takes what it said.

mod controls;

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::fonts::FontClass;
use crate::model::{
    Anchor, BlobRef, Color, FontSpec, Frame, Item, ItemKey, Layer, LayerId, Point, ROOT, Rect,
    Shape,
};

/// Computed styles requested from the engine, in the order the converter reads them.
pub const COMPUTED_STYLES: &[&str] = &[
    "background-color",
    "color",
    "font-size",
    "font-family",
    "font-weight",
    "font-style",
    "opacity",
    "visibility",
    "overflow-x",
    "overflow-y",
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
    "border-top-left-radius",
    "background-image",
    "position",
    "mask-image",
    "padding-left",
    "fill",
    "stroke",
    "stroke-width",
    "fill-rule",
    // From here on what the engine's theme paints is told from: a snapshot
    // recorded without them converts as it did.
    "appearance",
    "border-top-style",
    "outline-style",
    "outline-width",
    "outline-color",
    "outline-offset",
    "accent-color",
    "text-decoration-line",
    "text-decoration-color",
    "-webkit-text-security",
    "direction",
    "text-align",
    "padding-right",
    "white-space",
];

#[derive(Clone, Copy)]
enum S {
    Bg,
    Fg,
    FontSize,
    FontFamily,
    FontWeight,
    FontStyle,
    Opacity,
    Visibility,
    OverflowX,
    OverflowY,
    BorderTopW,
    BorderRightW,
    BorderBottomW,
    BorderLeftW,
    BorderTopC,
    BorderRightC,
    BorderBottomC,
    BorderLeftC,
    Radius,
    BgImage,
    Position,
    Mask,
    PaddingLeft,
    SvgFill,
    SvgStroke,
    SvgStrokeWidth,
    FillRule,
    Appearance,
    BorderTopStyle,
    OutlineStyle,
    OutlineWidth,
    OutlineColor,
    OutlineOffset,
    Accent,
    DecorationLine,
    DecorationColor,
    TextSecurity,
    Direction,
    TextAlign,
    PaddingRight,
    WhiteSpace,
}

const PLACEHOLDER: Color = Color([117, 117, 117, 255]);
const PART_FILL: u32 = 0;
const PART_BORDER: u32 = 1;
const PART_IMAGE: u32 = 2;
const SLOT: u32 = ItemKey::SLOT;
const SLOT_END: u32 = ItemKey::SLOT_END;
const SLOT_CENTRE: u32 = ItemKey::SLOT_CENTRE;
const PART_FIELD: u32 = 3 | SLOT;
const PART_VECTOR: u32 = 4;
/// The part of a checkbox or a radio button that says it is chosen: drawn
/// only while it is.
pub const CHOICE_MARK: u32 = 5;
/// The box of a checkbox or a radio button the page gave no box of its own.
pub const CHOICE_BOX: u32 = 6;
/// The ring an element is outlined with: the engine's own around the field
/// being edited, or one the page draws.
pub const OUTLINE: u32 = 7;
/// What marks a field as one that opens: a select's drop-down mark, the
/// picker of a date or a time.
pub const FIELD_MARK: u32 = 8;
/// The filling of a ticked box, of a chosen radio button's ring and of a
/// slider up to its thumb.
const PART_CHOICE_FILL: u32 = 9;
/// A slider's thumb, and the swatch of a colour.
pub const FIELD_VALUE: u32 = 10;
/// What stands behind a fieldset's legend, where its border is not drawn.
const PART_LEGEND_GAP: u32 = 11;
/// The inner half of a border the engine draws in two tones.
const PART_BORDER_INNER: u32 = 12;
/// A line drawn on text: under it, through it or over it.
pub const TEXT_LINE: u32 = 13;
/// The first line of a field that holds several; a field has few.
const PART_LINE: u32 = 32 | SLOT;
/// Lines of one field a frame carries.
const MAX_FIELD_LINES: usize = 64;
/// Shapes one inline SVG may carry; larger drawings are not icons.
const MAX_SHAPES: usize = 256;
/// Nesting below an `<svg>` searched for shapes.
const MAX_SVG_DEPTH: usize = 16;
const PART_TEXT: u32 = 16;

/// Why a snapshot could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotError(pub &'static str);

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unreadable layout snapshot: {}", self.0)
    }
}

impl std::error::Error for SnapshotError {}

fn ints(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_i64().unwrap_or(-1)).collect())
        .unwrap_or_default()
}

fn rect_of(v: Option<&Value>) -> Option<Rect> {
    let a = v?.as_array()?;
    let n = |i: usize| a.get(i).and_then(Value::as_f64).map(css_px);
    Some(Rect::new(n(0)?, n(1)?, n(2)?, n(3)?))
}

/// A CSS pixel length. Pages are far inside `f32`'s exact range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "CSS pixel coordinates; f32 keeps sub-pixel precision for any page size an engine lays out"
)]
fn css_px(v: f64) -> f32 {
    v as f32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=255.0 and rounded first, so the cast neither truncates nor wraps"
)]
fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

/// `rgb(...)`/`rgba(...)` to a colour; `None` for transparent or unparsable.
fn css_color(s: &str) -> Option<Color> {
    let inner = s.split_once('(')?.1.split_once(')')?.0;
    let parts: Vec<f32> = inner
        .split([',', ' ', '/'])
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse().ok())
        .collect();
    let channel = |i: usize| parts.get(i).map(|v| byte(*v));
    let alpha = parts.get(3).copied().unwrap_or(1.0);
    if alpha <= 0.0 {
        return None;
    }
    Some(Color([
        channel(0)?,
        channel(1)?,
        channel(2)?,
        byte(alpha.clamp(0.0, 1.0) * 255.0),
    ]))
}

fn px(s: &str) -> f32 {
    s.trim_end_matches("px").parse().unwrap_or(0.0)
}

/// An ellipse as SVG path data: two half arcs.
fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> String {
    format!(
        "M{} {cy}A{rx} {ry} 0 1 0 {} {cy}A{rx} {ry} 0 1 0 {} {cy}z",
        cx - rx,
        cx + rx,
        cx - rx
    )
}

/// The text of one line box as its line shows it. An engine keeps a text as
/// its page wrote it and lays out the spaces it collapses to: a line break in
/// the source is a space on the line, and a run of spaces is one, unless the
/// page keeps its spaces (`kept`). A client that drew a line break inside a
/// line box would set one line as two.
fn spaced(text: &str, kept: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_space = false;
    for letter in text.chars() {
        let space = matches!(letter, ' ' | '\n' | '\r' | '\t' | '\u{c}');
        if space && after_space && !kept {
            continue;
        }
        out.push(if space { ' ' } else { letter });
        after_space = space;
    }
    out
}

fn css_url(s: &str) -> Option<&str> {
    let start = s.find("url(")?;
    let rest = s.get(start.saturating_add(4)..)?;
    let end = rest.find(')')?;
    Some(rest.get(..end)?.trim_matches(['"', '\'']))
}

struct Doc<'a> {
    strings: Vec<&'a str>,
    parent: Vec<i64>,
    node_name: Vec<i64>,
    node_type: Vec<i64>,
    backend_id: Vec<i64>,
    layout_node: Vec<i64>,
    styles: Vec<Vec<i64>>,
    bounds: Vec<Option<Rect>>,
    scroll: Vec<Option<Rect>>,
    /// Each box as the page's own script measures it: in the page's pixels,
    /// which a zoomed page lays out larger.
    offset: Vec<Option<Rect>>,
    paint_order: Vec<i64>,
    layout_text: Vec<i64>,
    layout_of: HashMap<i64, usize>,
    sources: HashMap<i64, &'a str>,
    input_values: HashMap<i64, &'a str>,
    /// A textarea's text, which an input's value does not carry.
    text_values: HashMap<i64, &'a str>,
    /// The checkboxes and radio buttons that are chosen, and the options.
    checked: HashSet<i64>,
    selected: HashSet<i64>,
    node_value: Vec<i64>,
    /// Per node, attribute name/value string indices, alternating.
    attributes: Vec<Vec<i64>>,
    children: HashMap<i64, Vec<i64>>,
}

impl<'a> Doc<'a> {
    fn string(&self, idx: i64) -> &'a str {
        usize::try_from(idx)
            .ok()
            .and_then(|i| self.strings.get(i))
            .copied()
            .unwrap_or("")
    }

    fn style(&self, li: usize, s: S) -> &'a str {
        let idx = self
            .styles
            .get(li)
            .and_then(|st| st.get(s as usize))
            .copied()
            .unwrap_or(-1);
        self.string(idx)
    }

    fn attr(&self, node: i64, name: &str) -> Option<&'a str> {
        let list = usize::try_from(node)
            .ok()
            .and_then(|n| self.attributes.get(n))?;
        list.chunks_exact(2).find_map(|kv| match kv {
            [k, v] if self.string(*k) == name => Some(self.string(*v)),
            _ => None,
        })
    }

    fn name(&self, node: i64) -> &'a str {
        usize::try_from(node)
            .ok()
            .and_then(|n| self.node_name.get(n))
            .map_or("", |&i| self.string(i))
    }

    /// The words of the option a `<select>` shows as chosen.
    fn chosen_option(&self, select: i64) -> String {
        let below = |node: i64| self.children.get(&node).into_iter().flatten().copied();
        let option = below(select)
            .flat_map(|child| match self.name(child) {
                "OPTGROUP" => below(child).collect::<Vec<_>>(),
                _ => vec![child],
            })
            .find(|node| self.name(*node) == "OPTION" && self.selected.contains(node));
        let Some(option) = option else {
            return String::new();
        };
        let words = self
            .attr(option, "label")
            .filter(|label| !label.trim().is_empty())
            .map_or_else(
                || {
                    below(option)
                        .filter_map(|child| usize::try_from(child).ok())
                        .filter(|child| self.node_type.get(*child) == Some(&3))
                        .filter_map(|child| self.node_value.get(child))
                        .map(|value| self.string(*value))
                        .collect::<String>()
                },
                str::to_owned,
            );
        words.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// How much larger the engine lays the page out than the page's own
    /// pixels say: the zoom of the page. A snapshot's boxes are laid out
    /// zoomed and its computed lengths are not, so every length a style gives
    /// is scaled by this. The root element says it, as its laid out width
    /// over the width its script measures.
    fn zoom(&self) -> f32 {
        let root = self
            .layout_node
            .iter()
            .position(|node| self.name(*node) == "HTML");
        let widths = root.and_then(|li| {
            let laid = self.bounds.get(li).copied().flatten()?;
            let measured = self.offset.get(li).copied().flatten()?;
            (laid.w >= 1.0 && measured.w >= 1.0).then_some(laid.w / measured.w)
        });
        match widths {
            // A script measures in whole pixels: a width a fraction off is no zoom.
            Some(zoom) if (zoom - 1.0).abs() >= 0.02 => zoom.clamp(0.2, 8.0),
            _ => 1.0,
        }
    }

    fn parent_of(&self, node: i64) -> i64 {
        usize::try_from(node)
            .ok()
            .and_then(|n| self.parent.get(n))
            .copied()
            .unwrap_or(-1)
    }

    fn node_key(&self, node: i64) -> u64 {
        usize::try_from(node)
            .ok()
            .and_then(|n| self.backend_id.get(n))
            .and_then(|&id| u64::try_from(id).ok())
            .unwrap_or(0)
    }

    fn is_replaced(&self, node: i64) -> bool {
        matches!(
            self.name(node),
            "IMG"
                | "INPUT"
                | "TEXTAREA"
                | "SELECT"
                | "BUTTON"
                | "VIDEO"
                | "CANVAS"
                | "IFRAME"
                | "EMBED"
                | "OBJECT"
                | "svg"
        )
    }

    fn is_root_box(&self, node: i64) -> bool {
        matches!(self.name(node), "#document" | "HTML" | "BODY")
    }

    fn is_element(&self, node: i64) -> bool {
        const ELEMENT: i64 = 1;
        usize::try_from(node)
            .ok()
            .and_then(|n| self.node_type.get(n))
            .is_some_and(|&t| t == ELEMENT)
    }

    /// Does the element at layout index `li` root a layer?
    fn roots_layer(&self, node: i64, li: usize) -> bool {
        // Text boxes report their parent's style; replaced elements compute
        // `overflow: clip` for their own content, which is not a scroller.
        if !self.is_element(node) || self.is_root_box(node) || self.is_replaced(node) {
            return false;
        }
        let pos = self.style(li, S::Position);
        let clips =
            self.style(li, S::OverflowX) != "visible" || self.style(li, S::OverflowY) != "visible";
        let opacity: f32 = self.style(li, S::Opacity).parse().unwrap_or(1.0);
        pos == "fixed" || pos == "sticky" || clips || opacity < 1.0
    }
}

fn parse(snapshot: &Value) -> Result<Doc<'_>, SnapshotError> {
    let strings: Vec<&str> = snapshot["strings"]
        .as_array()
        .ok_or(SnapshotError("strings"))?
        .iter()
        .map(|s| s.as_str().unwrap_or(""))
        .collect();
    let doc = snapshot["documents"]
        .get(0)
        .ok_or(SnapshotError("documents"))?;
    let nodes = &doc["nodes"];
    let layout = &doc["layout"];
    let layout_node = ints(&layout["nodeIndex"]);
    let layout_of = layout_node
        .iter()
        .enumerate()
        .map(|(li, &n)| (n, li))
        .collect();
    let rare = |v: &Value| -> HashMap<i64, i64> {
        ints(&v["index"])
            .into_iter()
            .zip(ints(&v["value"]))
            .collect()
    };
    // The nodes a snapshot marks: it lists those a thing is true of.
    let flagged = |v: &Value| -> HashSet<i64> { ints(&v["index"]).into_iter().collect() };
    let resolve = |m: HashMap<i64, i64>| -> HashMap<i64, &str> {
        m.into_iter()
            .map(|(n, s)| {
                let text = usize::try_from(s)
                    .ok()
                    .and_then(|i| strings.get(i))
                    .copied()
                    .unwrap_or("");
                (n, text)
            })
            .collect()
    };
    let attributes = nodes["attributes"]
        .as_array()
        .map(|a| a.iter().map(ints).collect())
        .unwrap_or_default();
    let parent = ints(&nodes["parentIndex"]);
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    for (n, &p) in parent.iter().enumerate() {
        children
            .entry(p)
            .or_default()
            .push(i64::try_from(n).unwrap_or(-1));
    }
    let rects = |v: &Value| -> Vec<Option<Rect>> {
        v.as_array()
            .map(|a| a.iter().map(|r| rect_of(Some(r))).collect())
            .unwrap_or_default()
    };
    Ok(Doc {
        parent,
        node_name: ints(&nodes["nodeName"]),
        node_type: ints(&nodes["nodeType"]),
        backend_id: ints(&nodes["backendNodeId"]),
        styles: layout["styles"]
            .as_array()
            .map(|a| a.iter().map(ints).collect())
            .unwrap_or_default(),
        bounds: rects(&layout["bounds"]),
        scroll: rects(&layout["scrollRects"]),
        offset: rects(&layout["offsetRects"]),
        paint_order: ints(&layout["paintOrders"]),
        layout_text: ints(&layout["text"]),
        sources: resolve(rare(&nodes["currentSourceURL"])),
        input_values: resolve(rare(&nodes["inputValue"])),
        text_values: resolve(rare(&nodes["textValue"])),
        checked: flagged(&nodes["inputChecked"]),
        selected: flagged(&nodes["optionSelected"]),
        node_value: ints(&nodes["nodeValue"]),
        attributes,
        children,
        layout_node,
        layout_of,
        strings,
    })
}

/// Per-layer bookkeeping while converting.
struct Building {
    /// Document-space point that is (0,0) in the layer's content coordinates.
    content_origin: Point,
    /// Sticky boxes move with their content, so their decoration does too.
    sticky: bool,
    layer: Layer,
    /// (paint order, tiebreak, item)
    items: Vec<(i64, u32, Item)>,
}

struct Converter<'a> {
    doc: Doc<'a>,
    doc_scroll: Point,
    layer_of_node: HashMap<i64, LayerId>,
    layers: BTreeMap<LayerId, Building>,
    /// The page's zoom ([`Doc::zoom`]).
    zoom: f32,
    told: &'a Told,
    /// The colour behind everything, for what is drawn over a box to hide it.
    canvas: Color,
    asks: Vec<(u64, String)>,
    passwords: Vec<u64>,
    /// The family lists text is set in that the engine has not said the
    /// class of.
    families: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

/// What a caller knows of a page that its snapshot does not say.
#[derive(Clone, Debug, Default)]
pub struct Told {
    /// What the engine said a field shows, by the field's node: the value the
    /// field held when it was asked, and the text it showed then, or `None`
    /// where the engine could not say.
    pub shown: HashMap<u64, (String, Option<String>)>,
    /// Whether the engine is asked what the fields of [`Converted::asks`]
    /// show. Until it has said, such a field is drawn empty: what it holds is
    /// not what its page shows.
    pub asking: bool,
    /// Fields that were password fields: a page that shows a password on
    /// request turns its field into plain text, and what it holds is no less
    /// a secret for that.
    pub secret: HashSet<u64>,
    /// What class of face the engine said it sets a family list like, by the
    /// list as its style computes it; `None` where the engine could not tell
    /// (the face is like none of its generic families, or like several).
    pub classes: HashMap<String, Option<FontClass>>,
}

/// A frame, and what its page's engine is still to be asked.
#[derive(Clone, Debug)]
pub struct Converted {
    pub frame: Frame,
    /// The fields that show something other than what they hold, each with
    /// what it holds: a date, a time or a number in the engine's own words,
    /// the words on a button the page gave none. Only the engine can say what
    /// they show.
    pub asks: Vec<(u64, String)>,
    /// The password fields, for [`Told::secret`].
    pub passwords: Vec<u64>,
    /// The family lists the page's text is set in that [`Told::classes`]
    /// does not hold: only the engine can say what class of face it sets
    /// each in on its machine.
    pub families: Vec<String>,
}

impl Converter<'_> {
    /// A length a style gives, in the pixels the page is laid out in.
    fn len(&self, li: usize, s: S) -> f32 {
        px(self.doc.style(li, s)) * self.zoom
    }

    /// Whether the engine's own theme paints the control: its page left its
    /// appearance alone. A snapshot that does not say is taken as a page
    /// that drew the control itself.
    fn themed(&self, li: usize) -> bool {
        !matches!(self.doc.style(li, S::Appearance), "" | "none")
    }

    /// The colour `node` stands on: its own background, else the nearest one
    /// behind it, else the canvas.
    fn backdrop(&self, node: i64) -> Color {
        let mut at = node;
        for _ in 0..64 {
            if at < 0 {
                break;
            }
            let own = self
                .doc
                .layout_of
                .get(&at)
                .and_then(|li| css_color(self.doc.style(*li, S::Bg)))
                .filter(|color| color.0[3] == 255);
            if let Some(color) = own {
                return color;
            }
            at = self.doc.parent_of(at);
        }
        self.canvas
    }

    /// The layer whose content holds `node`'s boxes: the nearest
    /// layer-rooting ancestor-or-self.
    fn layer_for(&mut self, node: i64) -> LayerId {
        let mut chain = Vec::new();
        let mut at = node;
        let found = loop {
            if at < 0 {
                break ROOT;
            }
            if let Some(&l) = self.layer_of_node.get(&at) {
                break l;
            }
            chain.push(at);
            if chain.len() > self.doc.parent.len() {
                break ROOT;
            }
            at = self.doc.parent_of(at);
        };
        let mut current = found;
        for &n in chain.iter().rev() {
            if let Some(&li) = self.doc.layout_of.get(&n)
                && self.doc.roots_layer(n, li)
            {
                current = self.create_layer(n, li, current);
            }
            self.layer_of_node.insert(n, current);
        }
        current
    }

    fn create_layer(&mut self, node: i64, li: usize, parent: LayerId) -> LayerId {
        let id = self.doc.node_key(node).max(1);
        let b = self
            .doc
            .bounds
            .get(li)
            .copied()
            .flatten()
            .unwrap_or_default();
        let pos = self.doc.style(li, S::Position);
        let scroll = self
            .doc
            .scroll
            .get(li)
            .copied()
            .flatten()
            .map_or_else(Point::default, |r| Point {
                x: r.x * self.zoom,
                y: r.y * self.zoom,
            });
        let parent_origin = self
            .layers
            .get(&parent)
            .map_or_else(Point::default, |p| p.content_origin);
        let (anchor, origin) = if pos == "fixed" {
            (
                Anchor::Viewport,
                Point {
                    x: b.x - self.doc_scroll.x,
                    y: b.y - self.doc_scroll.y,
                },
            )
        } else {
            (
                Anchor::Content,
                Point {
                    x: b.x - parent_origin.x,
                    y: b.y - parent_origin.y,
                },
            )
        };
        let clips = self.doc.style(li, S::OverflowX) != "visible"
            || self.doc.style(li, S::OverflowY) != "visible";
        let opacity: f32 = self.doc.style(li, S::Opacity).parse().unwrap_or(1.0);
        let order = self.doc.paint_order.get(li).copied().unwrap_or(0);
        if let Some(p) = self.layers.get_mut(&parent) {
            // After the rooting box's own decoration at the same paint order.
            p.items.push((order, 5, Item::Layer(id)));
        }
        self.layers.insert(
            id,
            Building {
                content_origin: Point {
                    x: b.x - scroll.x,
                    y: b.y - scroll.y,
                },
                sticky: pos == "sticky",
                layer: Layer {
                    parent,
                    anchor,
                    origin,
                    size: Point { x: b.w, y: b.h },
                    clips,
                    scroll,
                    opacity: opacity.clamp(0.0, 1.0),
                    items: Vec::new(),
                },
                items: Vec::new(),
            },
        );
        id
    }

    fn push(&mut self, node: i64, order: i64, tiebreak: u32, item: Item) {
        let layer = self.layer_for(node);
        self.push_in(layer, order, tiebreak, item);
    }

    /// A box's own background and border. A plain scroll container's stay in
    /// the parent, so scrolling it moves only its content; a fixed, sticky or
    /// translucent box's travel and fade with it.
    fn push_decoration(&mut self, node: i64, order: i64, tiebreak: u32, item: Item) {
        let layer = self.layer_for(node);
        let target = self
            .layers
            .get(&layer)
            .filter(|b| {
                layer != ROOT
                    && layer == self.doc.node_key(node).max(1)
                    && b.layer.clips
                    && !b.sticky
                    && b.layer.anchor == Anchor::Content
                    && b.layer.opacity >= 1.0
            })
            .map_or(layer, |b| b.layer.parent);
        self.push_in(target, order, tiebreak, item);
    }

    fn push_in(&mut self, layer: LayerId, order: i64, tiebreak: u32, mut item: Item) {
        let Some(b) = self.layers.get_mut(&layer) else {
            return;
        };
        let o = b.content_origin;
        match &mut item {
            Item::Fill { rect, .. }
            | Item::Border { rect, .. }
            | Item::Text { rect, .. }
            | Item::Image { rect, .. }
            | Item::Vector { rect, .. } => *rect = rect.translate(-o.x, -o.y),
            Item::Layer(_) => {}
        }
        b.items.push((order, tiebreak, item));
    }

    fn boxes(&mut self) {
        for li in 0..self.doc.layout_node.len() {
            let node = self.doc.layout_node.get(li).copied().unwrap_or(-1);
            let Some(rect) = self.doc.bounds.get(li).copied().flatten() else {
                continue;
            };
            if rect.w <= 0.0 || rect.h <= 0.0 || self.doc.style(li, S::Visibility) != "visible" {
                continue;
            }
            // Root boxes paint the canvas instead (see `background`), and a
            // text box only reports its parent's style: its background and
            // border are the parent's, drawn once from the parent's own box.
            if !self.doc.is_element(node) || self.doc.is_root_box(node) {
                continue;
            }
            let order = self.doc.paint_order.get(li).copied().unwrap_or(0);
            let node_key = self.doc.node_key(node);
            let key = |part| ItemKey {
                node: node_key,
                part,
            };
            let name = self.doc.name(node);
            let masked = !matches!(self.doc.style(li, S::Mask), "" | "none");
            let rect = self.dress(node, li, order, rect);
            let src = if name == "IMG" {
                self.doc.sources.get(&node).copied()
            } else if masked {
                None
            } else {
                css_url(self.doc.style(li, S::BgImage))
            };
            if let Some(src) = src.filter(|s| !s.is_empty()) {
                self.push_decoration(
                    node,
                    order,
                    2,
                    Item::Image {
                        key: key(PART_IMAGE),
                        rect,
                        blob: BlobRef::Url(src.to_string()),
                    },
                );
            }
            if matches!(name, "INPUT" | "TEXTAREA" | "SELECT") {
                self.field(node, li, order, rect);
            }
            if name == "svg" {
                self.vector(node, li, order, rect);
            }
        }
    }

    /// Draw `node`'s own box: its background, its border and the ring it is
    /// outlined with. The box these are drawn in is returned, which is not
    /// always the box the engine laid out: a fieldset's starts halfway down
    /// its legend.
    fn dress(&mut self, node: i64, li: usize, order: i64, rect: Rect) -> Rect {
        let node_key = self.doc.node_key(node);
        let key = |part| ItemKey {
            node: node_key,
            part,
        };
        let edge = self.edge(node, li, rect);
        let (rect, radius) = (edge.rect, edge.radius);
        // A background under a CSS mask is an icon shape we cannot cut
        // yet; drawing it unmasked paints a solid square.
        let masked = !matches!(self.doc.style(li, S::Mask), "" | "none");
        if !masked && let Some(color) = css_color(self.doc.style(li, S::Bg)) {
            self.push_decoration(
                node,
                order,
                0,
                Item::Fill {
                    key: key(PART_FILL),
                    rect,
                    color,
                    radius,
                },
            );
        }
        // A computed border width is 0 unless its style is a visible one,
        // so the widths alone say which sides the page draws.
        if edge.widths.iter().any(|w| *w > 0.0) && edge.colors.iter().any(|c| c.0[3] > 0) {
            self.push_decoration(
                node,
                order,
                1,
                Item::Border {
                    key: key(PART_BORDER),
                    rect,
                    widths: edge.widths,
                    colors: edge.colors,
                    radius,
                },
            );
            if let Some((inner, colors)) = edge.inner {
                self.push_decoration(
                    node,
                    order,
                    1,
                    Item::Border {
                        key: key(PART_BORDER_INNER),
                        rect: inner,
                        widths: edge.widths,
                        colors,
                        radius: (radius - edge.widths[0]).max(0.0),
                    },
                );
            }
            if let Some(gap) = edge.gap {
                let color = self.backdrop(node);
                self.push_decoration(
                    node,
                    order,
                    2,
                    Item::Fill {
                        key: key(PART_LEGEND_GAP),
                        rect: gap,
                        color,
                        radius: 0.0,
                    },
                );
            }
        }
        self.outline(node, li, order, rect, radius);
        rect
    }

    /// An inline `<svg>`'s shapes, with the fill and stroke the engine
    /// computed for each (so `currentColor` and CSS rules are resolved).
    fn vector(&mut self, svg: i64, svg_li: usize, order: i64, rect: Rect) {
        let view_box = self
            .doc
            .attr(svg, "viewBox")
            .and_then(|v| {
                let numbers: Vec<f32> = v
                    .split([' ', ','])
                    .filter(|p| !p.is_empty())
                    .filter_map(|p| p.parse().ok())
                    .collect();
                match numbers.as_slice() {
                    [left, top, width, height] if *width > 0.0 && *height > 0.0 => {
                        Some(Rect::new(*left, *top, *width, *height))
                    }
                    _ => None,
                }
            })
            .unwrap_or_else(|| Rect::new(0.0, 0.0, rect.w / self.zoom, rect.h / self.zoom));
        let mut shapes = Vec::new();
        let mut stack: Vec<(i64, usize)> = vec![(svg, 0)];
        while let Some((node, depth)) = stack.pop() {
            if shapes.len() >= MAX_SHAPES {
                break;
            }
            if node != svg
                && let Some(shape) = self.shape(node, svg_li)
            {
                shapes.push(shape);
            }
            if depth < MAX_SVG_DEPTH
                && !matches!(self.doc.name(node), "defs" | "clipPath" | "mask" | "symbol")
                && let Some(kids) = self.doc.children.get(&node)
            {
                stack.extend(kids.iter().rev().map(|&k| (k, depth.saturating_add(1))));
            }
        }
        if shapes.is_empty() {
            return;
        }
        let key = ItemKey {
            node: self.doc.node_key(svg),
            part: PART_VECTOR,
        };
        self.push_decoration(
            svg,
            order,
            3,
            Item::Vector {
                key,
                rect,
                view_box,
                shapes,
            },
        );
    }

    fn shape(&self, node: i64, svg_li: usize) -> Option<Shape> {
        let num = |name: &str| self.doc.attr(node, name).map_or(0.0, px);
        let path = match self.doc.name(node) {
            "path" => self.doc.attr(node, "d")?.to_string(),
            "circle" => ellipse(num("cx"), num("cy"), num("r"), num("r")),
            "ellipse" => ellipse(num("cx"), num("cy"), num("rx"), num("ry")),
            "rect" => {
                let (x, y, w, h) = (num("x"), num("y"), num("width"), num("height"));
                format!("M{x} {y}h{w}v{h}h{}z", -w)
            }
            "line" => format!("M{} {}L{} {}", num("x1"), num("y1"), num("x2"), num("y2")),
            name @ ("polyline" | "polygon") => {
                let points = self.doc.attr(node, "points")?;
                let close = if name == "polygon" { "z" } else { "" };
                format!("M{points}{close}")
            }
            _ => return None,
        };
        // Shapes usually have their own layout object; otherwise they take
        // the drawing's style.
        let li = self.doc.layout_of.get(&node).copied().unwrap_or(svg_li);
        if self.doc.style(li, S::Visibility) == "hidden"
            || self.doc.style(li, S::Visibility) == "collapse"
        {
            return None;
        }
        let fill = css_color(self.doc.style(li, S::SvgFill));
        let stroke = css_color(self.doc.style(li, S::SvgStroke));
        if fill.is_none() && stroke.is_none() {
            return None;
        }
        Some(Shape {
            path,
            fill,
            stroke,
            stroke_width: px(self.doc.style(li, S::SvgStrokeWidth)).max(0.0),
            even_odd: self.doc.style(li, S::FillRule) == "evenodd",
        })
    }

    fn font(&self, li: usize) -> FontSpec {
        FontSpec {
            family: self.family(self.doc.style(li, S::FontFamily)),
            size: self.len(li, S::FontSize).max(1.0),
            weight: self.doc.style(li, S::FontWeight).parse().unwrap_or(400),
            italic: self.doc.style(li, S::FontStyle) == "italic",
        }
    }

    /// The family list a client is sent for the list a style computes to:
    /// the list itself, or the list as the engine said it sets it
    /// ([`crate::fonts::told`]). A list the engine has not said of is noted
    /// for it to be asked.
    fn family(&self, css: &str) -> String {
        match self.told.classes.get(css) {
            Some(Some(class)) => crate::fonts::told(css, *class),
            Some(None) => css.to_owned(),
            None => {
                self.families.borrow_mut().insert(css.to_owned());
                css.to_owned()
            }
        }
    }

    fn texts(&mut self, boxes: &Value) {
        let layout_index = ints(&boxes["layoutIndex"]);
        let starts = ints(&boxes["start"]);
        let lengths = ints(&boxes["length"]);
        let bounds = boxes["bounds"].as_array().cloned().unwrap_or_default();
        let mut fragment: HashMap<usize, u32> = HashMap::new();
        for (t, &li) in layout_index.iter().enumerate() {
            let Ok(li) = usize::try_from(li) else {
                continue;
            };
            // A node of very many lines numbers its last ones alike: the parts
            // above are kept for text that is no line of the engine's.
            let part = fragment.entry(li).or_insert(PART_TEXT);
            let this_part = *part;
            *part = part.saturating_add(1).min(ItemKey::LINES - 1);
            let node = self.doc.layout_node.get(li).copied().unwrap_or(-1);
            let full = self
                .doc
                .string(self.doc.layout_text.get(li).copied().unwrap_or(-1));
            let start = starts
                .get(t)
                .and_then(|&s| usize::try_from(s).ok())
                .unwrap_or(0);
            let len = lengths
                .get(t)
                .and_then(|&s| usize::try_from(s).ok())
                .unwrap_or(0);
            // Offsets count UTF-16 code units, as the engine stores text.
            let text: String = char::decode_utf16(full.encode_utf16().skip(start).take(len))
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            if text.trim().is_empty() || self.doc.style(li, S::Visibility) != "visible" {
                continue;
            }
            let kept = matches!(
                self.doc.style(li, S::WhiteSpace),
                "pre" | "pre-wrap" | "break-spaces"
            );
            let text = spaced(&text, kept);
            let Some(color) = css_color(self.doc.style(li, S::Fg)) else {
                continue;
            };
            let Some(rect) = rect_of(bounds.get(t)) else {
                continue;
            };
            let order = self.doc.paint_order.get(li).copied().unwrap_or(0);
            let key = ItemKey {
                node: self.doc.node_key(node),
                part: this_part,
            };
            let font = self.font(li);
            for (line, tint) in self.decorations(li, node, rect, font.size) {
                self.push(
                    node,
                    order,
                    4,
                    Item::Fill {
                        key: ItemKey {
                            node: key.node,
                            part: TEXT_LINE,
                        },
                        rect: line,
                        color: tint.unwrap_or(color),
                        radius: 0.0,
                    },
                );
            }
            self.push(
                node,
                order,
                4,
                Item::Text {
                    key,
                    rect,
                    text,
                    font,
                    color,
                },
            );
        }
    }

    /// The lines the page draws on a run of text: under it, through it, over
    /// it, each as the box it fills and the colour the page names for it. A
    /// line is the run's own or one an element around it draws on all its
    /// text, so a struck-out price inside a link is still struck out.
    fn decorations(
        &self,
        li: usize,
        node: i64,
        rect: Rect,
        size: f32,
    ) -> Vec<(Rect, Option<Color>)> {
        let mut found = Some(li);
        let mut at = node;
        let mut lines = "";
        for _ in 0..16 {
            if let Some(li) = found {
                lines = self.doc.style(li, S::DecorationLine);
                if !matches!(lines, "" | "none") {
                    break;
                }
            }
            at = self.doc.parent_of(at);
            if at < 0 {
                return Vec::new();
            }
            found = self.doc.layout_of.get(&at).copied();
        }
        let Some(li) = found else {
            return Vec::new();
        };
        let tint = css_color(self.doc.style(li, S::DecorationColor));
        let thick = (size / 16.0).max(1.0);
        [
            ("underline", 0.88),
            ("line-through", 0.56),
            ("overline", 0.0),
        ]
        .into_iter()
        .filter(|(name, _)| lines.split(' ').any(|line| line == *name))
        .map(|(_, down)| {
            let top = (rect.y + rect.h * down).min(rect.y + rect.h - thick);
            (Rect::new(rect.x, top.max(rect.y), rect.w, thick), tint)
        })
        .collect()
    }

    /// The canvas takes the root element's background, or else body's.
    fn background(&self) -> Color {
        let mut html = None;
        let mut body = None;
        for (li, &node) in self.doc.layout_node.iter().enumerate() {
            match self.doc.name(node) {
                "HTML" => html = html.or_else(|| css_color(self.doc.style(li, S::Bg))),
                "BODY" => body = body.or_else(|| css_color(self.doc.style(li, S::Bg))),
                _ => {}
            }
        }
        html.or(body).unwrap_or(Color([255, 255, 255, 255]))
    }
}

/// Convert a snapshot of a `viewport`-sized page into frame `seq`, as a caller
/// does that knows nothing of the page beside it.
pub fn convert(snapshot: &Value, viewport: Point, seq: u64) -> Result<Frame, SnapshotError> {
    convert_with(snapshot, viewport, seq, &Told::default()).map(|converted| converted.frame)
}

/// Convert a snapshot of a `viewport`-sized page into frame `seq`, with what
/// its engine was `told` to say of it so far.
pub fn convert_with(
    snapshot: &Value,
    viewport: Point,
    seq: u64,
    told: &Told,
) -> Result<Converted, SnapshotError> {
    let doc0 = snapshot["documents"]
        .get(0)
        .ok_or(SnapshotError("documents"))?;
    let doc_scroll = Point {
        x: css_px(doc0["scrollOffsetX"].as_f64().unwrap_or(0.0)),
        y: css_px(doc0["scrollOffsetY"].as_f64().unwrap_or(0.0)),
    };
    let doc = parse(snapshot)?;
    let zoom = doc.zoom();
    let mut c = Converter {
        doc,
        doc_scroll,
        layer_of_node: HashMap::new(),
        layers: BTreeMap::new(),
        zoom,
        told,
        canvas: Color([255, 255, 255, 255]),
        asks: Vec::new(),
        passwords: Vec::new(),
        families: std::cell::RefCell::default(),
    };
    c.canvas = c.background();
    c.layers.insert(
        ROOT,
        Building {
            content_origin: Point::default(),
            sticky: false,
            layer: Layer {
                parent: ROOT,
                anchor: Anchor::Content,
                origin: Point::default(),
                size: viewport,
                clips: true,
                scroll: doc_scroll,
                opacity: 1.0,
                items: Vec::new(),
            },
            items: Vec::new(),
        },
    );
    c.boxes();
    c.texts(&doc0["textBoxes"]);
    let background = c.canvas;
    let layers = c
        .layers
        .into_iter()
        .map(|(id, mut b)| {
            b.items.sort_by_key(|(order, tie, _)| (*order, *tie));
            b.layer.items = b.items.into_iter().map(|(_, _, item)| item).collect();
            (id, b.layer)
        })
        .collect();
    Ok(Converted {
        frame: Frame {
            seq,
            viewport,
            background,
            layers,
        },
        asks: c.asks,
        passwords: c.passwords,
        families: c.families.into_inner().into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::spaced;

    #[test]
    fn a_line_break_in_the_source_is_a_space_on_the_line() {
        // As an engine hands over the text of one line box of a paragraph its page wrote
        // on several lines.
        assert_eq!(
            spaced("had long\nsince stopped arguing", false),
            "had long since stopped arguing"
        );
        assert_eq!(spaced(" 9 crowns,\n", false), " 9 crowns, ");
        assert_eq!(spaced("to   read\n\t  twice", false), "to read twice");
        // A page that keeps its spaces has them drawn, and still no line break in a line.
        assert_eq!(spaced("a  b\tc", true), "a  b c");
    }
}
