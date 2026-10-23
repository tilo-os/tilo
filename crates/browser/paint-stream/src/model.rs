use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Stable identity of a layer: the engine's id for the element that roots it.
pub type LayerId = u64;

/// The root layer: the document, scrolled by the page's scroll offset.
pub const ROOT: LayerId = 0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.x + self.w && p.y < self.y + self.h
    }

    #[must_use]
    pub fn translate(&self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }
}

/// Straight-alpha sRGB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color(pub [u8; 4]);

/// The font a text run asks for. The family is the page's CSS list; the
/// client resolves it against the fonts it has.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FontSpec {
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
}

/// Content an item references by value rather than inline: an image's bytes.
/// The host resolves the page URL to a content hash before a frame leaves it,
/// so a client never learns, and never fetches, the authenticated URL.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BlobRef {
    /// Not yet resolved by the host. Never sent to a client.
    Url(String),
    /// blake3 of the encoded image.
    Hash([u8; 32]),
}

/// One filled and/or stroked SVG shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    /// SVG path data.
    pub path: String,
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub stroke_width: f32,
    /// `fill-rule: evenodd`.
    pub even_odd: bool,
}

/// Identity of an item across frames: the element and which of its fragments
/// (text line, box decoration) it is. Patches key on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub node: u64,
    pub part: u32,
}

/// One paint operation, in its layer's content coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Fill {
        key: ItemKey,
        rect: Rect,
        color: Color,
        radius: f32,
    },
    /// Top, right, bottom, left.
    Border {
        key: ItemKey,
        rect: Rect,
        widths: [f32; 4],
        colors: [Color; 4],
        radius: f32,
    },
    /// One line box of text. `rect` is the box the engine laid it out in; the
    /// client fits its own shaping to that width so line breaks and link
    /// positions match the host's layout.
    Text {
        key: ItemKey,
        rect: Rect,
        text: String,
        font: FontSpec,
        color: Color,
    },
    Image {
        key: ItemKey,
        rect: Rect,
        blob: BlobRef,
    },
    /// An inline SVG: its shapes as path data in `view_box` units, mapped
    /// onto `rect`. Icons stay vectors, crisp at any client scale.
    Vector {
        key: ItemKey,
        rect: Rect,
        view_box: Rect,
        shapes: Vec<Shape>,
    },
    /// Paint the child layer here, at this point in the paint order.
    Layer(LayerId),
}

impl Item {
    #[must_use]
    pub const fn key(&self) -> ItemKey {
        match self {
            Self::Fill { key, .. }
            | Self::Border { key, .. }
            | Self::Text { key, .. }
            | Self::Image { key, .. }
            | Self::Vector { key, .. } => *key,
            Self::Layer(id) => ItemKey {
                node: *id,
                part: u32::MAX,
            },
        }
    }

    #[must_use]
    pub const fn rect(&self) -> Option<Rect> {
        match self {
            Self::Fill { rect, .. }
            | Self::Border { rect, .. }
            | Self::Text { rect, .. }
            | Self::Image { rect, .. }
            | Self::Vector { rect, .. } => Some(*rect),
            Self::Layer(_) => None,
        }
    }
}

/// What a layer's origin is relative to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    /// The parent layer's content: moves when the parent scrolls.
    #[default]
    Content,
    /// The viewport (`position: fixed`): never moves when anything scrolls.
    Viewport,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub parent: LayerId,
    pub anchor: Anchor,
    /// Top-left of the layer box, in the parent's content coordinates (or the
    /// viewport's, for [`Anchor::Viewport`]).
    pub origin: Point,
    /// Size of the layer box. Content is clipped to it when `clips` is set.
    pub size: Point,
    pub clips: bool,
    /// How far the layer's content is scrolled.
    pub scroll: Point,
    pub opacity: f32,
    pub items: Vec<Item>,
}

impl Layer {
    /// Everything about the layer except its items.
    #[must_use]
    pub fn same_header(&self, other: &Self) -> bool {
        self.parent == other.parent
            && self.anchor == other.anchor
            && self.origin == other.origin
            && self.size == other.size
            && self.clips == other.clips
            && self.scroll == other.scroll
            && self.opacity.to_bits() == other.opacity.to_bits()
    }
}

/// A whole page at one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// Monotonic per tab; a patch names the frame it applies to.
    pub seq: u64,
    pub viewport: Point,
    /// Canvas colour behind everything (the root background propagates to it).
    pub background: Color,
    pub layers: BTreeMap<LayerId, Layer>,
}

impl Frame {
    #[must_use]
    pub fn root(&self) -> Option<&Layer> {
        self.layers.get(&ROOT)
    }

    pub fn blobs(&self) -> impl Iterator<Item = &BlobRef> {
        self.layers.values().flat_map(|l| {
            l.items.iter().filter_map(|i| match i {
                Item::Image { blob, .. } => Some(blob),
                _ => None,
            })
        })
    }

    /// Absolute offset that maps `layer`'s content coordinates to the
    /// viewport, and the layer's clip in viewport coordinates when it (or an
    /// ancestor) clips. `None` for an unknown layer or a parent cycle.
    #[must_use]
    pub fn layer_to_viewport(&self, layer: LayerId) -> Option<(Point, Option<Rect>)> {
        let mut chain = Vec::new();
        let mut at = layer;
        loop {
            let l = self.layers.get(&at)?;
            chain.push(l);
            if at == ROOT || l.anchor == Anchor::Viewport {
                break;
            }
            if chain.len() > self.layers.len() {
                return None;
            }
            at = l.parent;
        }
        let mut off = Point::default();
        let mut clip: Option<Rect> = None;
        for l in chain.iter().rev() {
            let (bx, by) = (off.x + l.origin.x, off.y + l.origin.y);
            if l.clips {
                let r = Rect::new(bx, by, l.size.x, l.size.y);
                clip = Some(clip.map_or(r, |c| intersect(c, r)));
            }
            off = Point {
                x: bx - l.scroll.x,
                y: by - l.scroll.y,
            };
        }
        Some((off, clip))
    }

    /// The topmost item under a viewport point: what a pointer or an agent
    /// would hit. Layers paint in item order, so the last hit wins.
    #[must_use]
    pub fn hit_test(&self, p: Point) -> Option<ItemKey> {
        self.hit_in(ROOT, p, 0)
    }

    fn hit_in(&self, layer: LayerId, p: Point, depth: usize) -> Option<ItemKey> {
        if depth > self.layers.len() {
            return None;
        }
        let l = self.layers.get(&layer)?;
        let (off, clip) = self.layer_to_viewport(layer)?;
        if clip.is_some_and(|c| !c.contains(p)) {
            return None;
        }
        let local = Point {
            x: p.x - off.x,
            y: p.y - off.y,
        };
        l.items.iter().rev().find_map(|item| match item {
            Item::Layer(child) => self.hit_in(*child, p, depth.saturating_add(1)),
            other => other
                .rect()
                .filter(|r| r.contains(local))
                .map(|_| other.key()),
        })
    }
}

#[must_use]
pub fn intersect(first: Rect, second: Rect) -> Rect {
    let left = first.x.max(second.x);
    let top = first.y.max(second.y);
    let right = (first.x + first.w).min(second.x + second.w);
    let bottom = (first.y + first.h).min(second.y + second.h);
    Rect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
}
