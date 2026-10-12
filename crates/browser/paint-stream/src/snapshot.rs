//! Frame from a `DevTools` `DOMSnapshot.captureSnapshot` result.
//!
//! The snapshot is the engine's own layout: every box and text line with the
//! bounds the engine computed, in document coordinates, plus paint order. This
//! module groups those into layers and makes every item relative to its layer,
//! which is what keeps scroll and sticky motion out of the item lists.
//!
//! Call the engine with [`capture_params`]; pass the result to [`convert`].

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

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
    "border-top-style",
    "border-top-left-radius",
    "background-image",
    "position",
    "mask-image",
    "padding-left",
    "fill",
    "stroke",
    "stroke-width",
    "fill-rule",
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
    BorderStyle,
    Radius,
    BgImage,
    Position,
    Mask,
    PaddingLeft,
    SvgFill,
    SvgStroke,
    SvgStrokeWidth,
    FillRule,
}

/// Parameters for `DOMSnapshot.captureSnapshot`.
#[must_use]
pub fn capture_params() -> Value {
    json!({
        "computedStyles": COMPUTED_STYLES,
        "includePaintOrder": true,
        "includeDOMRects": true,
    })
}

const PLACEHOLDER: Color = Color([117, 117, 117, 255]);
const PART_FILL: u32 = 0;
const PART_BORDER: u32 = 1;
const PART_IMAGE: u32 = 2;
const PART_FIELD: u32 = 3;
const PART_VECTOR: u32 = 4;
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
    paint_order: Vec<i64>,
    layout_text: Vec<i64>,
    layout_of: HashMap<i64, usize>,
    sources: HashMap<i64, &'a str>,
    input_values: HashMap<i64, &'a str>,
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

    /// Does the element at layout index `li` root a layer?
    fn roots_layer(&self, node: i64, li: usize) -> bool {
        const ELEMENT: i64 = 1;
        let element = usize::try_from(node)
            .ok()
            .and_then(|n| self.node_type.get(n))
            .is_some_and(|&t| t == ELEMENT);
        // Text boxes report their parent's style; replaced elements compute
        // `overflow: clip` for their own content, which is not a scroller.
        if !element || self.is_root_box(node) || self.is_replaced(node) {
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
        paint_order: ints(&layout["paintOrders"]),
        layout_text: ints(&layout["text"]),
        sources: resolve(rare(&nodes["currentSourceURL"])),
        input_values: resolve(rare(&nodes["inputValue"])),
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
}

impl Converter<'_> {
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
        // Create layers for any rooting elements on the way down.
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
            .map_or_else(Point::default, |r| Point { x: r.x, y: r.y });
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
            // Root boxes paint the canvas instead (see `background`).
            if self.doc.is_root_box(node) {
                continue;
            }
            let order = self.doc.paint_order.get(li).copied().unwrap_or(0);
            let node_key = self.doc.node_key(node);
            let key = |part| ItemKey {
                node: node_key,
                part,
            };
            let radius = px(self.doc.style(li, S::Radius));
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
            let widths = [
                S::BorderTopW,
                S::BorderRightW,
                S::BorderBottomW,
                S::BorderLeftW,
            ]
            .map(|s| px(self.doc.style(li, s)));
            let colors = [
                S::BorderTopC,
                S::BorderRightC,
                S::BorderBottomC,
                S::BorderLeftC,
            ]
            .map(|s| css_color(self.doc.style(li, s)).unwrap_or_default());
            if widths.iter().any(|w| *w > 0.0)
                && colors.iter().any(|c| c.0[3] > 0)
                && !matches!(self.doc.style(li, S::BorderStyle), "none" | "hidden")
            {
                self.push_decoration(
                    node,
                    order,
                    1,
                    Item::Border {
                        key: key(PART_BORDER),
                        rect,
                        widths,
                        colors,
                        radius,
                    },
                );
            }
            let name = self.doc.name(node);
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
            if matches!(name, "INPUT" | "TEXTAREA") {
                self.field(node, li, order, rect);
            }
            if name == "svg" {
                self.vector(node, li, order, rect);
            }
        }
    }

    /// Form fields draw their value through the engine's shadow tree, which
    /// the snapshot omits: draw value or placeholder from the node instead.
    fn field(&mut self, node: i64, li: usize, order: i64, rect: Rect) {
        let raw = self.doc.input_values.get(&node).copied().unwrap_or("");
        // A password never leaves the host: viewers see its length as dots,
        // as the page itself shows it.
        let masked;
        let value = if self
            .doc
            .attr(node, "type")
            .is_some_and(|t| t.eq_ignore_ascii_case("password"))
        {
            masked = "\u{2022}".repeat(raw.chars().count());
            masked.as_str()
        } else {
            raw
        };
        let (text, color) = if value.is_empty() {
            let placeholder = self.doc.attr(node, "placeholder").unwrap_or("");
            (placeholder, PLACEHOLDER)
        } else {
            (
                value,
                css_color(self.doc.style(li, S::Fg)).unwrap_or_default(),
            )
        };
        if text.is_empty() {
            return;
        }
        let font = self.font(li);
        let pad = px(self.doc.style(li, S::PaddingLeft)) + px(self.doc.style(li, S::BorderLeftW));
        let line = (font.size * 1.25).min(rect.h);
        let r = Rect::new(
            rect.x + pad,
            rect.y + (rect.h - line) / 2.0,
            (rect.w - pad).max(0.0),
            line,
        );
        let key = ItemKey {
            node: self.doc.node_key(node),
            part: PART_FIELD,
        };
        self.push(
            node,
            order,
            3,
            Item::Text {
                key,
                rect: r,
                text: text.to_string(),
                font,
                color,
            },
        );
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
            .unwrap_or_else(|| Rect::new(0.0, 0.0, rect.w, rect.h));
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
            family: self.doc.style(li, S::FontFamily).to_string(),
            size: px(self.doc.style(li, S::FontSize)).max(1.0),
            weight: self.doc.style(li, S::FontWeight).parse().unwrap_or(400),
            italic: self.doc.style(li, S::FontStyle) == "italic",
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
            let part = fragment.entry(li).or_insert(PART_TEXT);
            let this_part = *part;
            *part = part.saturating_add(1);
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

/// Convert a snapshot of a `viewport`-sized page into frame `seq`.
///
/// # Errors
///
/// [`SnapshotError`] when the snapshot lacks its string table or documents.
pub fn convert(snapshot: &Value, viewport: Point, seq: u64) -> Result<Frame, SnapshotError> {
    let doc0 = snapshot["documents"]
        .get(0)
        .ok_or(SnapshotError("documents"))?;
    let doc_scroll = Point {
        x: css_px(doc0["scrollOffsetX"].as_f64().unwrap_or(0.0)),
        y: css_px(doc0["scrollOffsetY"].as_f64().unwrap_or(0.0)),
    };
    let mut c = Converter {
        doc: parse(snapshot)?,
        doc_scroll,
        layer_of_node: HashMap::new(),
        layers: BTreeMap::new(),
    };
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
    let background = c.background();
    let layers = c
        .layers
        .into_iter()
        .map(|(id, mut b)| {
            b.items.sort_by_key(|(order, tie, _)| (*order, *tie));
            b.layer.items = b.items.into_iter().map(|(_, _, item)| item).collect();
            (id, b.layer)
        })
        .collect();
    Ok(Frame {
        seq,
        viewport,
        background,
        layers,
    })
}
