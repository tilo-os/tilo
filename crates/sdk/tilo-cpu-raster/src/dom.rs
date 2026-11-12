//! A recorded vello scene as a list of DOM operations, for a page that shows
//! the interface as HTML instead of drawing it on a canvas.
//!
//! The scene is walked exactly as the CPU rasterizer walks it
//! ([`crate::decode`]), and each draw becomes the plainest element that shows
//! it: an axis-aligned rectangle or rounded rectangle becomes a box with a
//! background and border radius, any other path an SVG path, a glyph run that
//! carries its characters (`vello::DrawGlyphs::text`) real text set in the
//! very font file the toolkit shaped it with, a rectangular clip a box that
//! hides its overflow, a blurred rectangle a box shadow. Glyph runs without
//! characters (an icon font's, a canvas's) become SVG paths of their outlines.
//!
//! The output is JSON, one array per operation, read by the page's DOM
//! presenter (`web/dom-present.mjs`):
//!
//! | op | fields |
//! |---|---|
//! | `"r"` | x, y, w, h, radius, fill, stroke, stroke width |
//! | `"p"` | x, y, w, h (bounds), SVG path data relative to x/y, fill, stroke, stroke width, even-odd |
//! | `"t"` | text, x, baseline y, font size, font family, variation settings, colour, advance to last glyph |
//! | `"c"` | x, y, w, h, radius, opacity (a clip; `"e"` ends it) |
//! | `"cp"` | x, y, w, h, SVG clip path relative to x/y, opacity |
//! | `"g"` | opacity (a group without a clip) |
//! | `"s"` | x, y, w, h, radius, blur, colour (a shadow) |
//! | `"i"` | x, y, w, h (the bounds it is clipped to), image id (or null), the image-to-device matrix a b c d e f, alpha |
//! | `"e"` | end of the innermost clip or group |
//! | `"k"` | key (16 hex digits), x, y, scale: what follows belongs to the widget the key names, which stands at x, y |
//! | `"ke"` | end of the innermost key |
//!
//! Keys come from the encoding's marks (`vello_encoding::Mark`), which the
//! toolkit writes around each widget it draws while a host asks for them, so
//! the page can keep one element per widget from frame to frame.

use std::collections::HashMap;
use std::fmt::Write as _;

use skrifa::{FontRef, MetadataProvider, Tag};
use vello_cpu::kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};
use vello_cpu::peniko::{Extend, Fill, FontData, Gradient, GradientKind};
use vello_encoding::{Encoding, MarkKind};

use crate::Stats;
use crate::decode::{Brush, DisplayList, LayerMode, Op, Style};
use crate::glyph::GlyphCache;
use crate::image::ImageCache;

/// A font file the page has not been given yet: register `bytes` as
/// `family` before showing the operations that name it.
pub struct NewFont {
    pub family: String,
    pub bytes: vello_cpu::peniko::Blob<u8>,
    pub index: u32,
    /// Ascent and descent as fractions of the em, as the browser lays a
    /// line of this font out: where the baseline sits in a span's box.
    pub ascent: f32,
    pub descent: f32,
}

/// An image the page has not been given yet: its pixels, straight (not
/// premultiplied) RGBA, row by row, under the id its `"i"` operations name.
pub struct NewImage {
    pub id: u32,
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
    /// An image the page holds that this one replaces: a picture drawn again
    /// ([`Islands`]). The page forgets it.
    pub replaces: Option<u32>,
}

/// What a frame's widgets said about how the page should show them, for
/// [`DomEncoder::encode_with`].
#[derive(Default)]
pub struct Islands<'a> {
    /// Widgets that drew themselves as pictures: each is shown as one image
    /// of everything it drew, with its text as text over it.
    pub pictures: Option<&'a std::collections::HashSet<u64>>,
    /// Widgets that must stay elements (a native scroll container, a field
    /// the page shows): no subtree holding one becomes an image unasked.
    pub keep: Option<&'a std::collections::HashSet<u64>>,
    /// Device pixels per CSS point the page shows this frame at: an island is
    /// rasterized at that density and placed back at its size in points, so a
    /// picture is as crisp as the screen showing it (`0.0`, the default, is
    /// one pixel per point).
    pub device_scale: f64,
    /// The part of the scene the page shows, which an image is cut to.
    pub viewport: Rect,
}

/// How many draws a widget makes itself (not through the widgets inside
/// it) before the page is shown it as one image rather than an element per
/// draw: a chart, a drawing or a map without a picture's say-so.
pub const ISLAND_DRAWS: usize = 400;

/// A widget the page shows as an image, as last rasterized.
struct Island {
    /// What it drew, hashed: drawn the same again, the page's image stands.
    hash: u64,
    id: u32,
    last: u64,
}

/// An island of this frame: the index of its widget's end mark, its image,
/// where the image stands and the density it was drawn at.
struct PlannedIsland {
    end: usize,
    id: u32,
    rect: Rect,
    scale: f64,
}

/// The most device pixels one island's image may hold. A picture is drawn at
/// the screen's own density so it is crisp on a phone, but a window-sized one
/// at three device pixels per point would hold tens of megabytes for
/// something the page already paints from an image; above this its density
/// drops instead, never below one pixel per point.
pub const ISLAND_PIXELS: u64 = 4_000_000;

/// The CPU rasterizer that draws islands, made the first time one is shown.
struct IslandRaster {
    ctx: vello_cpu::RenderContext,
    resources: vello_cpu::Resources,
    scratch: crate::record::Scratch,
}

/// An image the page holds ([`DomEncoder::image_id`]).
struct Shown {
    pixmap: std::sync::Weak<vello_cpu::Pixmap>,
    id: u32,
    last: u64,
}

/// How many frames an image no frame showed is taken to be still on the
/// page. The page's presenter keeps one for 600 (`web/dom-present.mjs`), so
/// an image is handed over again before the page could have dropped it.
const IMAGE_FRAMES: u64 = 500;

/// What one [`DomEncoder::encode`] produced.
pub struct DomFrame {
    pub ops: String,
    pub fonts: Vec<NewFont>,
    /// Images the operations name for the first time.
    pub images: Vec<NewImage>,
    pub texts: usize,
    pub draws: usize,
}

struct FontInfo {
    family: String,
    ascent: f32,
    descent: f32,
    /// Each variation axis: tag, minimum, default, maximum.
    axes: Vec<(String, f32, f32, f32)>,
}

/// Turns scenes into DOM operations, keeping glyph outlines and font names
/// across frames.
#[derive(Default)]
pub struct DomEncoder {
    list: DisplayList,
    glyphs: GlyphCache,
    images: ImageCache,
    fonts: HashMap<(u64, u32), FontInfo>,
    /// The page's id of each image it was given, by the pixmap's address,
    /// with the pixmap held weakly so an address reused by a later image is
    /// not taken for it, and the frame that last showed it.
    shown: HashMap<usize, Shown>,
    next_image: u32,
    frame: u64,
    /// Fonts [`Self::text_face`] named that the page was not given yet.
    pending_fonts: Vec<NewFont>,
    /// The widgets shown as images, by key.
    islands: HashMap<u64, Island>,
    raster: Option<Box<IslandRaster>>,
}

impl DomEncoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The page's id for `pixmap`, handing its pixels over the first time.
    fn image_id(
        shown: &mut HashMap<usize, Shown>,
        next: &mut u32,
        frame: u64,
        pixmap: &std::sync::Arc<vello_cpu::Pixmap>,
        images: &mut Vec<NewImage>,
    ) -> u32 {
        let address = std::sync::Arc::as_ptr(pixmap) as usize;
        if let Some(held) = shown.get_mut(&address)
            && held
                .pixmap
                .upgrade()
                .is_some_and(|held| std::sync::Arc::ptr_eq(&held, pixmap))
        {
            held.last = frame;
            return held.id;
        }
        let id = *next;
        *next = next.wrapping_add(1);
        shown.insert(
            address,
            Shown {
                pixmap: std::sync::Arc::downgrade(pixmap),
                id,
                last: frame,
            },
        );
        let mut rgba = pixmap.data_as_u8_slice().to_vec();
        unpremultiply(&mut rgba);
        images.push(NewImage {
            id,
            width: pixmap.width(),
            height: pixmap.height(),
            rgba,
            replaces: None,
        });
        id
    }

    /// The page's family name for `font` set at the variation coordinates
    /// `coords`, and its `font-variation-settings`; a font the page was not
    /// given yet goes into `fonts`.
    fn face(
        known: &mut HashMap<(u64, u32), FontInfo>,
        fonts: &mut Vec<NewFont>,
        font: &FontData,
        coords: &[i16],
    ) -> (String, String) {
        let info = known
            .entry((font.data.id(), font.index))
            .or_insert_with(|| {
                let info = font_info(font);
                fonts.push(NewFont {
                    family: info.family.clone(),
                    bytes: font.data.clone(),
                    index: font.index,
                    ascent: info.ascent,
                    descent: info.descent,
                });
                info
            });
        let mut variations = String::new();
        for (i, (tag, min, default, max)) in info.axes.iter().enumerate() {
            let c = coords.get(i).map_or(0.0, |&c| f32::from(c) / 16384.0);
            let value = if c >= 0.0 {
                default + c * (max - default)
            } else {
                default + c * (default - min)
            };
            if !variations.is_empty() {
                variations.push(',');
            }
            let _ = write!(variations, "'{tag}' {value:.0}");
        }
        (info.family.clone(), variations)
    }

    /// The page's family name and `font-variation-settings` for `font` at
    /// `coords`, for text the page sets itself (a field it shows): a font it
    /// was not given yet is handed over with the next frame
    /// ([`DomFrame::fonts`]).
    pub fn text_face(&mut self, font: &FontData, coords: &[i16]) -> (String, String) {
        Self::face(&mut self.fonts, &mut self.pending_fonts, font, coords)
    }

    /// Write mark `at` of the frame (`kind`), or, for an island's begin, the
    /// widget's mark and its image, until its end.
    fn island_mark(
        out: &mut String,
        sep: &mut impl FnMut(&mut String),
        plan: &HashMap<usize, PlannedIsland>,
        at: usize,
        kind: &MarkKind,
        in_island: &mut Option<usize>,
    ) {
        if let Some(end) = *in_island {
            if at == end {
                *in_island = None;
                sep(out);
                mark(out, *kind);
            }
            return;
        }
        sep(out);
        mark(out, *kind);
        if let Some(island) = plan.get(&at) {
            *in_island = Some(island.end);
            sep(out);
            let rect = island.rect;
            // The image is drawn at the screen's density: the matrix puts it
            // back at the widget's size in points.
            image_op(
                out,
                rect,
                Some(island.id),
                Affine::translate((rect.x0, rect.y0)) * Affine::scale(1.0 / island.scale),
                1.0,
            );
        }
    }

    /// Which widgets of the decoded frame the page shows as images, by the
    /// index of their begin mark, rasterizing those that drew something new
    /// into `images`.
    fn plan_islands(
        &mut self,
        islands: &Islands<'_>,
        images: &mut Vec<NewImage>,
    ) -> HashMap<usize, PlannedIsland> {
        let marks = &self.list.marks;
        let mut planned = HashMap::new();
        if marks.is_empty() {
            return planned;
        }
        // Each begin's end, and the draws each key makes itself.
        let mut ends = vec![usize::MAX; marks.len()];
        let mut own = vec![0_usize; marks.len()];
        let mut open: Vec<usize> = Vec::new();
        let mut next = 0;
        let mut pair = |next: usize, kind: &MarkKind, open: &mut Vec<usize>| match kind {
            MarkKind::Begin { .. } => open.push(next),
            MarkKind::End => {
                if let Some(begin) = open.pop() {
                    ends[begin] = next;
                }
            }
        };
        for (index, op) in self.list.ops.iter().enumerate() {
            while let Some((at, kind)) = marks.get(next)
                && *at <= index
            {
                pair(next, kind, &mut open);
                next += 1;
            }
            if matches!(op, Op::Draw { .. } | Op::Glyphs { .. } | Op::Blur { .. })
                && let Some(&key) = open.last()
            {
                own[key] += 1;
            }
        }
        // Marks after the last draw.
        while let Some((_, kind)) = marks.get(next) {
            pair(next, kind, &mut open);
            next += 1;
        }
        let mut chosen = Vec::new();
        let mut at = 0;
        while at < marks.len() {
            let (MarkKind::Begin { key, .. }, end) = (marks[at].1, ends[at]) else {
                at += 1;
                continue;
            };
            if end == usize::MAX {
                at += 1;
                continue;
            }
            let picture = islands
                .pictures
                .is_some_and(|pictures| pictures.contains(&key));
            let heavy = own[at] >= ISLAND_DRAWS
                && !marks[at..end].iter().any(|(_, kind)| {
                    matches!(kind, MarkKind::Begin { key, .. }
                        if islands.keep.is_some_and(|keep| keep.contains(key)))
                });
            if picture || heavy {
                chosen.push((key, end, at, marks[at].0..marks[end].0));
                // What it holds is part of its image.
                at = end + 1;
            } else {
                at += 1;
            }
        }
        for (key, end, begin, ops) in chosen {
            if let Some(island) =
                self.island(key, ops, islands.viewport, islands.device_scale, images)
            {
                planned.insert(begin, PlannedIsland { end, ..island });
            }
        }
        let frame = self.frame;
        self.islands
            .retain(|_, island| frame.saturating_sub(island.last) < IMAGE_FRAMES);
        planned
    }

    /// The image of the draws `ops` of the widget keyed `key`, cut to
    /// `viewport`: the page's image when they drew the same as last time,
    /// otherwise rasterized again into `images`. `None` when nothing shows.
    fn island(
        &mut self,
        key: u64,
        ops: std::ops::Range<usize>,
        viewport: Rect,
        device_scale: f64,
        images: &mut Vec<NewImage>,
    ) -> Option<PlannedIsland> {
        use std::hash::{Hash, Hasher};
        let mut bounds: Option<Rect> = None;
        let mut drawn = Vec::new();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // What each draw paints, hashed: geometry, transform and paint (an
        // image by identity, never its pixels).
        let brush =
            |brush: &Brush, hasher: &mut std::collections::hash_map::DefaultHasher| match brush {
                Brush::Image(image, placed) => {
                    match &image.image {
                        vello_cpu::ImageSource::Pixmap(pixmap) => {
                            (std::sync::Arc::as_ptr(pixmap) as usize).hash(hasher);
                        }
                        other => format!("{other:?}").hash(hasher),
                    }
                    image.sampler.alpha.to_bits().hash(hasher);
                    placed.as_coeffs().map(f64::to_bits).hash(hasher);
                }
                other => format!("{other:?}").hash(hasher),
            };
        for index in ops {
            let op = &self.list.ops[index];
            match op {
                Op::Text { .. } => continue,
                Op::Draw {
                    bbox,
                    path,
                    transform,
                    style,
                    brush: paint,
                } => {
                    bounds = Some(bounds.map_or(*bbox, |b| b.union(*bbox)));
                    self.list.path(*path).to_svg().hash(&mut hasher);
                    transform.as_coeffs().map(f64::to_bits).hash(&mut hasher);
                    format!("{style:?}").hash(&mut hasher);
                    brush(paint, &mut hasher);
                }
                Op::Blur { bbox, path, .. } => {
                    bounds = Some(bounds.map_or(*bbox, |b| b.union(*bbox)));
                    self.list.path(*path).to_svg().hash(&mut hasher);
                    format!("{op:?}").hash(&mut hasher);
                }
                Op::Glyphs {
                    bbox,
                    glyphs,
                    style,
                    brush: paint,
                } => {
                    bounds = Some(bounds.map_or(*bbox, |b| b.union(*bbox)));
                    for glyph in &self.list.glyphs[glyphs.clone()] {
                        (std::sync::Arc::as_ptr(&glyph.outline) as usize).hash(&mut hasher);
                        glyph
                            .transform
                            .as_coeffs()
                            .map(f64::to_bits)
                            .hash(&mut hasher);
                    }
                    format!("{style:?}").hash(&mut hasher);
                    brush(paint, &mut hasher);
                }
                Op::Begin { path, .. } => {
                    if let Some(path) = path {
                        self.list.path(*path).to_svg().hash(&mut hasher);
                    }
                    format!("{op:?}").hash(&mut hasher);
                }
                Op::End => 1_u8.hash(&mut hasher),
            }
            drawn.push(u32::try_from(index).ok()?);
        }
        let shown = if viewport.area() > 0.0 {
            bounds?.intersect(viewport)
        } else {
            bounds?
        };
        let rect = Rect::new(
            shown.x0.floor(),
            shown.y0.floor(),
            shown.x1.ceil(),
            shown.y1.ceil(),
        );
        // The density it is drawn at: the screen's, unless that would hold
        // more pixels than an island may.
        let points = rect.width() * rect.height();
        let scale = if points > 0.0 {
            device_scale
                .max(1.0)
                .min((ISLAND_PIXELS as f64 / points).sqrt())
                .max(1.0)
        } else {
            1.0
        };
        let (width, height) = (
            u16::try_from((rect.width() * scale) as i64).ok()?,
            u16::try_from((rect.height() * scale) as i64).ok()?,
        );
        if width == 0 || height == 0 {
            return None;
        }
        [rect.x0, rect.y0, rect.x1, rect.y1]
            .map(f64::to_bits)
            .hash(&mut hasher);
        scale.to_bits().hash(&mut hasher);
        let hash = hasher.finish();
        let frame = self.frame;
        if let Some(held) = self.islands.get_mut(&key)
            && held.hash == hash
        {
            held.last = frame;
            return Some(PlannedIsland {
                end: 0,
                id: held.id,
                rect,
                scale,
            });
        }
        let raster = self.raster.get_or_insert_with(|| {
            Box::new(IslandRaster {
                ctx: vello_cpu::RenderContext::new(width, height),
                resources: vello_cpu::Resources::new(),
                scratch: crate::record::Scratch::new(true),
            })
        });
        if raster.ctx.width() == width && raster.ctx.height() == height {
            raster.ctx.reset();
        } else {
            raster.ctx.reset_and_resize(width, height);
        }
        let mut stats = Stats::default();
        raster.scratch.blurs.begin_frame();
        crate::record::record(
            &mut raster.ctx,
            &self.list,
            &drawn,
            rect,
            vello_cpu::color::AlphaColor::TRANSPARENT,
            scale,
            &mut raster.scratch,
            &mut stats,
        );
        raster.scratch.blurs.end_frame();
        raster.ctx.flush();
        let mut rgba = vec![0_u8; usize::from(width) * usize::from(height) * 4];
        if let Some(pixmap) = vello_cpu::PixmapMut::new(width, height, &mut rgba) {
            raster.ctx.render_with(
                pixmap,
                &mut raster.resources,
                vello_cpu::RasterizerSettings {
                    render_mode: vello_cpu::RenderMode::OptimizeSpeed,
                    ..vello_cpu::RasterizerSettings::default()
                },
            );
        }
        unpremultiply(&mut rgba);
        let id = self.next_image;
        self.next_image = self.next_image.wrapping_add(1);
        let replaces = self
            .islands
            .insert(
                key,
                Island {
                    hash,
                    id,
                    last: frame,
                },
            )
            .map(|old| old.id);
        images.push(NewImage {
            id,
            width,
            height,
            rgba,
            replaces,
        });
        Some(PlannedIsland {
            end: 0,
            id,
            rect,
            scale,
        })
    }

    pub fn encode(&mut self, encoding: &Encoding) -> DomFrame {
        self.encode_with(encoding, &Islands::default())
    }

    /// [`Self::encode`], showing as one image each widget `islands` names a
    /// picture and each that makes [`ISLAND_DRAWS`] draws itself.
    pub fn encode_with(&mut self, encoding: &Encoding, islands: &Islands<'_>) -> DomFrame {
        let mut stats = Stats::default();
        self.list.dom = true;
        self.list
            .decode(encoding, &mut self.glyphs, &mut self.images, &mut stats);
        let mut out = String::with_capacity(self.list.ops.len() * 64);
        let mut fonts = std::mem::take(&mut self.pending_fonts);
        let mut images = Vec::new();
        self.frame = self.frame.wrapping_add(1);
        let frame = self.frame;
        self.shown.retain(|_, held| {
            held.pixmap.strong_count() > 0 && frame.saturating_sub(held.last) < IMAGE_FRAMES
        });
        let plan = self.plan_islands(islands, &mut images);
        let marks = &self.list.marks;
        let mut next_mark = 0;
        // The end mark of the island being shown, while its draws are
        // skipped.
        let mut in_island: Option<usize> = None;
        let mut texts = 0;
        out.push('[');
        let mut first = true;
        let mut sep = |out: &mut String| {
            if !std::mem::replace(&mut first, false) {
                out.push(',');
            }
        };
        for (index, op) in self.list.ops.iter().enumerate() {
            while let Some((at, kind)) = marks.get(next_mark)
                && *at <= index
            {
                Self::island_mark(&mut out, &mut sep, &plan, next_mark, kind, &mut in_island);
                next_mark += 1;
            }
            // An island's draws are its image; its text stays text.
            if in_island.is_some() && !matches!(op, Op::Text { .. }) {
                continue;
            }
            match op {
                Op::Draw {
                    path,
                    transform,
                    style,
                    brush,
                    ..
                } => {
                    let path = self.list.path(*path);
                    sep(&mut out);
                    if let Brush::Image(image, placed) = brush {
                        let rect = transform.transform_rect_bbox(path.bounding_box());
                        let id = match &image.image {
                            vello_cpu::ImageSource::Pixmap(pixmap) => Some(Self::image_id(
                                &mut self.shown,
                                &mut self.next_image,
                                self.frame,
                                pixmap,
                                &mut images,
                            )),
                            _ => None,
                        };
                        image_op(&mut out, rect, id, *placed, image.sampler.alpha);
                    } else {
                        draw(&mut out, path, *transform, style, brush);
                    }
                }
                Op::Glyphs {
                    glyphs,
                    style,
                    brush,
                    bbox,
                } => {
                    let mut outline = BezPath::new();
                    for glyph in &self.list.glyphs[glyphs.clone()] {
                        for el in glyph.outline.path.elements() {
                            outline.push(glyph.transform * *el);
                        }
                    }
                    sep(&mut out);
                    path_op(&mut out, &outline, *bbox, style, brush);
                }
                Op::Text {
                    text,
                    origin,
                    advances_to_last,
                    font_size,
                    font,
                    coords,
                    brush,
                    ..
                } => {
                    texts += 1;
                    let (family, variations) =
                        Self::face(&mut self.fonts, &mut fonts, font, coords);
                    sep(&mut out);
                    out.push_str("[\"t\",");
                    json_str(&mut out, text);
                    let _ = write!(
                        out,
                        ",{:.2},{:.2},{:.2},\"{}\",",
                        origin.x, origin.y, font_size, family
                    );
                    json_str(&mut out, &variations);
                    let _ = write!(out, ",\"{}\",{:.2}]", css_brush(brush), advances_to_last);
                }
                Op::Blur {
                    path,
                    transform,
                    params,
                    color,
                    ..
                } => {
                    let rect = transform.transform_rect_bbox(self.list.path(*path).bounding_box());
                    sep(&mut out);
                    let _ = write!(
                        out,
                        "[\"s\",{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},\"{}\"]",
                        rect.x0,
                        rect.y0,
                        rect.width(),
                        rect.height(),
                        params.radius,
                        params.std_dev,
                        css(*color)
                    );
                }
                Op::Begin {
                    path,
                    transform,
                    mode,
                    ..
                } => {
                    let opacity = if let LayerMode::Blend(_, alpha) = mode {
                        *alpha
                    } else {
                        1.0
                    };
                    sep(&mut out);
                    match path {
                        // A layer without a clip: a group.
                        None => {
                            let _ = write!(out, "[\"g\",{opacity}]");
                        }
                        Some(index) => {
                            let path = self.list.path(*index);
                            if let Some((rect, radius)) = box_shape(path, *transform) {
                                let _ = write!(
                                    out,
                                    "[\"c\",{:.2},{:.2},{:.2},{:.2},{radius:.2},{opacity}]",
                                    rect.x0,
                                    rect.y0,
                                    rect.width(),
                                    rect.height()
                                );
                            } else {
                                let device = *transform * path.clone();
                                let rect = device.bounding_box();
                                let _ = write!(
                                    out,
                                    "[\"cp\",{:.2},{:.2},{:.2},{:.2},\"",
                                    rect.x0,
                                    rect.y0,
                                    rect.width(),
                                    rect.height()
                                );
                                svg_path(
                                    &mut out,
                                    &(Affine::translate((-rect.x0, -rect.y0)) * device),
                                );
                                let _ = write!(out, "\",{opacity}]");
                            }
                        }
                    }
                }
                Op::End => {
                    sep(&mut out);
                    out.push_str("[\"e\"]");
                }
            }
        }
        while let Some((_, kind)) = marks.get(next_mark) {
            Self::island_mark(&mut out, &mut sep, &plan, next_mark, kind, &mut in_island);
            next_mark += 1;
        }
        out.push(']');
        DomFrame {
            ops: out,
            fonts,
            images,
            texts,
            draws: self.list.ops.len(),
        }
    }
}

/// Premultiplied RGBA to straight, as a page's image data is.
fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        if let [r, g, b, a] = pixel
            && *a != 0
            && *a != 255
        {
            let alpha = u16::from(*a);
            for channel in [r, g, b] {
                *channel = u8::try_from((u16::from(*channel) * 255 + alpha / 2) / alpha)
                    .unwrap_or(u8::MAX);
            }
        }
    }
}

fn mark(out: &mut String, kind: MarkKind) {
    match kind {
        MarkKind::Begin { key, origin } => {
            let [a, b, _, _] = origin.matrix;
            let scale = f64::from(a).hypot(f64::from(b));
            let _ = write!(
                out,
                "[\"k\",\"{key:016x}\",{:.2},{:.2},{scale:.4}]",
                origin.translation[0], origin.translation[1]
            );
        }
        MarkKind::End => out.push_str("[\"ke\"]"),
    }
}

/// An image drawn into `rect` (device pixels), the path's bounds: the page
/// shows image `id` placed by `placed` (image pixels to device pixels) and
/// clipped to the rect, at `alpha`. An image the page cannot be given (one
/// the scene names only by a handle) has no id and shows as a placeholder.
fn image_op(out: &mut String, rect: Rect, id: Option<u32>, placed: Affine, alpha: f32) {
    let [a, b, c, d, e, f] = placed.as_coeffs();
    let id = id.map_or_else(|| "null".to_owned(), |id| id.to_string());
    let _ = write!(
        out,
        "[\"i\",{:.2},{:.2},{:.2},{:.2},{id},{a:.6},{b:.6},{c:.6},{d:.6},{e:.3},{f:.3},{alpha:.3}]",
        rect.x0,
        rect.y0,
        rect.width(),
        rect.height()
    );
}

fn draw(out: &mut String, path: &BezPath, transform: Affine, style: &Style, brush: &Brush) {
    if let Some((rect, radius)) = box_shape(path, transform) {
        let (fill, stroke, width) = match style {
            Style::Fill(_) => (css_fill(brush, rect), String::new(), 0.0),
            Style::Stroke(stroke) => {
                let scale = transform.determinant().abs().sqrt();
                (String::new(), css_brush(brush), stroke.width * scale)
            }
        };
        // A stroke straddles the path; a CSS border lies inside the box.
        let rect = rect.inflate(width / 2.0, width / 2.0);
        let radius = if width > 0.0 {
            radius + width / 2.0
        } else {
            radius
        };
        let _ = write!(
            out,
            "[\"r\",{:.2},{:.2},{:.2},{:.2},{radius:.2},\"{fill}\",\"{stroke}\",{width:.2}]",
            rect.x0,
            rect.y0,
            rect.width(),
            rect.height()
        );
        return;
    }
    let device = transform * path.clone();
    let style = match style {
        Style::Stroke(stroke) => {
            let mut stroke = stroke.clone();
            stroke.width *= transform.determinant().abs().sqrt();
            Style::Stroke(stroke)
        }
        Style::Fill(fill) => Style::Fill(*fill),
    };
    let bbox = device.bounding_box();
    path_op(out, &device, bbox, &style, brush);
}

fn path_op(out: &mut String, device: &BezPath, bbox: Rect, style: &Style, brush: &Brush) {
    let pad = match style {
        Style::Stroke(stroke) => stroke.width,
        Style::Fill(_) => 0.0,
    };
    let rect = bbox.inflate(pad, pad);
    let _ = write!(
        out,
        "[\"p\",{:.2},{:.2},{:.2},{:.2},\"",
        rect.x0,
        rect.y0,
        rect.width(),
        rect.height()
    );
    svg_path(
        out,
        &(Affine::translate((-rect.x0, -rect.y0)) * device.clone()),
    );
    let (fill, stroke, width, even_odd) = match style {
        Style::Fill(fill) => (css_brush(brush), String::new(), 0.0, *fill == Fill::EvenOdd),
        Style::Stroke(stroke) => (String::new(), css_brush(brush), stroke.width, false),
    };
    let _ = write!(out, "\",\"{fill}\",\"{stroke}\",{width:.2},{even_odd}]");
}

/// The device rectangle and corner radius of `path` under `transform`, when
/// it is a rectangle or a rounded rectangle with equal corners and the
/// transform keeps it axis-aligned.
fn box_shape(path: &BezPath, transform: Affine) -> Option<(Rect, f64)> {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    if b.abs() > 1e-9 || c.abs() > 1e-9 || a <= 0.0 || d <= 0.0 {
        return None;
    }
    let elements = path.elements();
    if elements.len() > 12 {
        return None;
    }
    let bbox = path.bounding_box();
    let eps = 0.01;
    let on_edge = |p: Point| {
        (p.x - bbox.x0).abs() < eps
            || (p.x - bbox.x1).abs() < eps
            || (p.y - bbox.y0).abs() < eps
            || (p.y - bbox.y1).abs() < eps
    };
    let mut curves = 0;
    let mut radius = None;
    let mut moves = 0;
    for el in elements {
        match el {
            PathEl::MoveTo(p) => {
                moves += 1;
                if !on_edge(*p) {
                    return None;
                }
                // The distance from the start to the nearest corner along
                // its edge is the corner radius.
                let along_x = (p.x - bbox.x0).abs().min((p.x - bbox.x1).abs());
                let along_y = (p.y - bbox.y0).abs().min((p.y - bbox.y1).abs());
                radius = Some(
                    along_x
                        .max(along_y)
                        .min(bbox.width().min(bbox.height()) / 2.0),
                );
            }
            PathEl::LineTo(p) => {
                if !on_edge(*p) {
                    return None;
                }
            }
            PathEl::CurveTo(_, _, p) | PathEl::QuadTo(_, p) => {
                curves += 1;
                if !on_edge(*p) {
                    return None;
                }
            }
            PathEl::ClosePath => {}
        }
    }
    if moves != 1 || (curves != 0 && curves != 4) {
        return None;
    }
    let radius = if curves == 0 {
        0.0
    } else {
        radius.unwrap_or(0.0)
    };
    if curves == 0 && elements.len() < 4 {
        return None;
    }
    Some((transform.transform_rect_bbox(bbox), radius * a.min(d)))
}

fn svg_path(out: &mut String, path: &BezPath) {
    for el in path.elements() {
        let _ = match el {
            PathEl::MoveTo(p) => write!(out, "M{:.2} {:.2}", p.x, p.y),
            PathEl::LineTo(p) => write!(out, "L{:.2} {:.2}", p.x, p.y),
            PathEl::QuadTo(a, p) => write!(out, "Q{:.2} {:.2} {:.2} {:.2}", a.x, a.y, p.x, p.y),
            PathEl::CurveTo(a, b, p) => write!(
                out,
                "C{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                a.x, a.y, b.x, b.y, p.x, p.y
            ),
            PathEl::ClosePath => write!(out, "Z"),
        };
    }
}

fn css(color: vello_cpu::color::AlphaColor<vello_cpu::color::Srgb>) -> String {
    let [r, g, b, a] = color.components;
    format!(
        "rgba({},{},{},{:.3})",
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        a
    )
}

/// The background of a box filling `rect` (device pixels) with `brush`: a
/// gradient as the CSS gradient that paints the same pixels, laid out
/// against the box; anything else as [`css_brush`].
fn css_fill(brush: &Brush, rect: Rect) -> String {
    if let Brush::Gradient(gradient, transform) = brush
        && let Some(css) = css_gradient(gradient, *transform, rect)
    {
        return css;
    }
    css_brush(brush)
}

/// A gradient as CSS for a box at `rect`, or `None` for one CSS cannot show
/// (a two-point radial gradient whose circles do not share a centre).
fn css_gradient(gradient: &Gradient, transform: Affine, rect: Rect) -> Option<String> {
    let repeat = !matches!(gradient.extend, Extend::Pad);
    let stops = |position: &dyn Fn(f32) -> String| {
        let mut out = String::new();
        for stop in gradient.stops.iter() {
            let color = css(stop.color.to_alpha_color::<vello_cpu::color::Srgb>());
            let _ = write!(out, ",{color} {}", position(stop.offset));
        }
        out
    };
    let prefix = if repeat { "repeating-" } else { "" };
    match &gradient.kind {
        GradientKind::Linear(line) => {
            // The ends are in device pixels already (`decode`).
            let (start, end) = (transform * line.start, transform * line.end);
            let d = end - start;
            let length = d.hypot();
            if length < 1e-6 {
                return None;
            }
            let u = d / length;
            // CSS runs the gradient through the box's centre, as long as the
            // box is along its angle; map each stop onto that line.
            let span = (rect.width() * u.x).abs() + (rect.height() * u.y).abs();
            if span < 1e-6 {
                return None;
            }
            let from = rect.center() - u * (span / 2.0);
            let t0 = (from - start).dot(d) / (length * length);
            let angle = u.x.atan2(-u.y).to_degrees();
            let at =
                |offset: f32| format!("{:.3}%", (f64::from(offset) - t0) * length / span * 100.0);
            Some(format!(
                "{prefix}linear-gradient({angle:.3}deg{})",
                stops(&at)
            ))
        }
        GradientKind::Radial(radial) => {
            if (radial.start_center - radial.end_center).hypot() > 1e-3 {
                return None;
            }
            let centre = transform * radial.end_center;
            let scale = transform.determinant().abs().sqrt();
            let (r0, r1) = (f64::from(radial.start_radius), f64::from(radial.end_radius));
            let at = |offset: f32| format!("{:.3}px", (r0 + f64::from(offset) * (r1 - r0)) * scale);
            Some(format!(
                "{prefix}radial-gradient(circle {:.3}px at {:.3}px {:.3}px{})",
                (r1 * scale).max(0.0),
                centre.x - rect.x0,
                centre.y - rect.y0,
                stops(&at)
            ))
        }
        GradientKind::Sweep(sweep) => {
            let centre = transform * sweep.center;
            let [a, b, ..] = transform.as_coeffs();
            let turn = b.atan2(a).to_degrees();
            let (start, end) = (
                f64::from(sweep.start_angle).to_degrees(),
                f64::from(sweep.end_angle).to_degrees(),
            );
            // CSS measures from the top, clockwise; peniko from +x, clockwise
            // with y down.
            let at = |offset: f32| format!("{:.3}deg", f64::from(offset) * (end - start));
            Some(format!(
                "{prefix}conic-gradient(from {:.3}deg at {:.3}px {:.3}px{})",
                start + turn + 90.0,
                centre.x - rect.x0,
                centre.y - rect.y0,
                stops(&at)
            ))
        }
    }
}

/// A solid colour as CSS; a gradient as its first stop, where it colours a
/// stroke, a path or text.
fn css_brush(brush: &Brush) -> String {
    match brush {
        Brush::Solid(color) => css(*color),
        Brush::Gradient(gradient, _) => gradient.stops.first().map_or_else(
            || "transparent".to_owned(),
            |stop| css(stop.color.to_alpha_color::<vello_cpu::color::Srgb>()),
        ),
        Brush::Image(_, _) => "transparent".to_owned(),
    }
}

fn json_str(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn font_info(font: &FontData) -> FontInfo {
    let family = format!("tilo-face-{}-{}", font.data.id(), font.index);
    let (ascent, descent) =
        FontRef::from_index(font.data.data(), font.index).map_or((0.8, 0.2), |font| {
            let metrics = font.metrics(
                skrifa::instance::Size::unscaled(),
                skrifa::instance::LocationRef::default(),
            );
            let em = f32::from(metrics.units_per_em.max(1));
            (metrics.ascent / em, -metrics.descent / em)
        });
    let axes = FontRef::from_index(font.data.data(), font.index)
        .map(|font| {
            font.axes()
                .iter()
                .map(|axis| {
                    let tag: Tag = axis.tag();
                    (
                        tag.to_string(),
                        axis.min_value(),
                        axis.default_value(),
                        axis.max_value(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    FontInfo {
        family,
        ascent,
        descent,
        axes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_cpu::color::palette::css::{BLACK, WHITE};

    #[test]
    fn a_linear_gradient_across_a_box_is_the_same_css_gradient() {
        let rect = Rect::new(10.0, 20.0, 110.0, 70.0);
        // Left edge to right edge: CSS's own line for 90deg.
        let gradient = Gradient::new_linear((10.0, 20.0), (110.0, 20.0)).with_stops([BLACK, WHITE]);
        assert_eq!(
            css_gradient(&gradient, Affine::IDENTITY, rect).unwrap(),
            "linear-gradient(90.000deg,rgba(0,0,0,1.000) 0.000%,rgba(255,255,255,1.000) 100.000%)"
        );
        // From the box's centre to its bottom edge: the stops land half way
        // down CSS's line, which runs top to bottom.
        let gradient = Gradient::new_linear((60.0, 45.0), (60.0, 70.0)).with_stops([BLACK, WHITE]);
        assert_eq!(
            css_gradient(&gradient, Affine::IDENTITY, rect).unwrap(),
            "linear-gradient(180.000deg,rgba(0,0,0,1.000) 50.000%,rgba(255,255,255,1.000) 100.000%)"
        );
    }

    #[test]
    fn a_radial_gradient_is_a_circle_at_its_centre_in_the_box() {
        let rect = Rect::new(0.0, 0.0, 40.0, 40.0);
        let gradient = Gradient::new_radial((20.0, 20.0), 10.0).with_stops([BLACK, WHITE]);
        assert_eq!(
            css_gradient(&gradient, Affine::translate((5.0, 5.0)), rect).unwrap(),
            "radial-gradient(circle 10.000px at 25.000px 25.000px,rgba(0,0,0,1.000) 0.000px,rgba(255,255,255,1.000) 10.000px)"
        );
    }
}
