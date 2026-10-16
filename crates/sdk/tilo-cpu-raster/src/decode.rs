//! Walks a vello [`Encoding`] into a display list of device-space draws.
//!
//! The streams are read the way vello's `flatten` and `draw_leaf` shaders
//! read them: a path segment's first point is the previous segment's last
//! point, a segment carrying `SUBPATH_END` skips the next point (the move-to
//! of the following subpath), every `PATH` tag closes one draw object paired
//! with the next draw tag, a path's segments use the transform current at
//! each segment while its brush uses the transform current at its `PATH`
//! tag, and a stroked subpath ends in a cap marker segment (a quad for an
//! open subpath, a line for a closed one) that is not geometry. Glyph runs
//! live in `resources` and are spliced in at their recorded path-tag
//! offset, as `vello_encoding::Resolver` does.

use std::sync::Arc;

use smallvec::SmallVec;
use vello_cpu::color::{AlphaColor, Srgb};
use vello_cpu::kurbo::{Affine, BezPath, Cap, Join, Point, Rect, Shape, Stroke, StrokeOpts};
use vello_cpu::peniko::{
    self, BlendMode, ColorStop, ColorStops, Compose, Extend, Fill, Gradient, GradientKind,
    ImageQuality, ImageSampler, InterpolationAlphaSpace, LinearGradientPosition, Mix,
    RadialGradientPosition, SweepGradientPosition,
};
use vello_cpu::{Image, ImageSource};
use vello_encoding::{
    DrawBeginClip, DrawTag, Encoding, GlyphRun, Patch, PathTag, Style as EncStyle, Transform,
};

use crate::Stats;
use crate::glyph::{GlyphCache, GlyphKey, Outline};
use crate::image::ImageCache;

/// How a path is painted.
#[derive(Clone, Debug)]
pub(crate) enum Style {
    Fill(Fill),
    Stroke(Stroke),
}

/// What a draw is painted with. Gradient and image brushes carry their
/// device-space brush transform.
#[derive(Clone, Debug)]
pub(crate) enum Brush {
    Solid(AlphaColor<Srgb>),
    Gradient(Gradient, Affine),
    Image(Image, Affine),
}

/// How a layer composites when it is popped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LayerMode {
    /// A plain clip: no blending, full opacity.
    Clip,
    Blend(BlendMode, f32),
    /// The layer's luminance masks what is below it. Not supported.
    LuminanceMask,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlurParams {
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    pub std_dev: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct GlyphInstance {
    pub outline: Arc<Outline>,
    pub transform: Affine,
    pub bbox: Rect,
}

#[derive(Clone, Debug)]
pub(crate) enum Op {
    Draw {
        path: usize,
        transform: Affine,
        style: Style,
        brush: Brush,
        bbox: Rect,
    },
    Glyphs {
        glyphs: std::ops::Range<usize>,
        style: Style,
        brush: Brush,
        bbox: Rect,
    },
    Blur {
        path: usize,
        transform: Affine,
        brush_transform: Affine,
        params: BlurParams,
        color: AlphaColor<Srgb>,
        bbox: Rect,
    },
    Begin {
        /// `None` when the clip has no geometry: nothing inside is visible.
        path: Option<usize>,
        transform: Affine,
        fill: Fill,
        mode: LayerMode,
        bbox: Rect,
        /// Index of the matching [`Op::End`].
        end: usize,
        /// Whether a non-normal blend layer is nested inside, which makes the
        /// isolation of this layer observable.
        nested_blend: bool,
    },
    End,
}

/// The decoded scene. Paths live in a pool reused across frames.
#[derive(Default)]
pub(crate) struct DisplayList {
    pub ops: Vec<Op>,
    paths: Vec<BezPath>,
    paths_used: usize,
    pub glyphs: Vec<GlyphInstance>,
    open: Vec<usize>,
}

impl DisplayList {
    pub fn path(&self, index: usize) -> &BezPath {
        &self.paths[index]
    }

    pub fn draw_count(&self) -> usize {
        self.ops.iter().filter(|op| !matches!(op, Op::End)).count()
    }

    fn alloc_path(&mut self) -> usize {
        let index = self.paths_used;
        self.paths_used += 1;
        if let Some(path) = self.paths.get_mut(index) {
            path.truncate(0);
        } else {
            self.paths.push(BezPath::new());
        }
        index
    }

    pub fn decode(
        &mut self,
        encoding: &Encoding,
        glyphs: &mut GlyphCache,
        images: &mut ImageCache,
        stats: &mut Stats,
    ) {
        self.ops.clear();
        self.glyphs.clear();
        self.open.clear();
        self.paths_used = 0;
        let mut walker = Walker {
            encoding,
            list: self,
            glyph_cache: glyphs,
            images,
            stats,
            ramps: Vec::new(),
            image_patches: Vec::new(),
            draw_ix: 0,
            draw_data_ix: 0,
            transform_ix: None,
            style_ix: None,
            path: PathState::default(),
            bounds: None,
            complete: true,
        };
        walker.run();
    }

    /// Every pixel `encoding` can paint, as device-space rectangles: each
    /// draw's path (control points, stroke reach included) or glyph
    /// outlines under its transform, and the clip of every layer that
    /// composites other than by source over. `None` when some draw cannot be
    /// bounded (truncated data, a blending layer without a clip, bounds that
    /// are not finite). Layers' clips are not applied: the bounds hold
    /// whatever clips them. Nothing is rasterised and no image is decoded.
    pub fn bounds(
        &mut self,
        encoding: &Encoding,
        glyphs: &mut GlyphCache,
        images: &mut ImageCache,
        stats: &mut Stats,
    ) -> Option<Vec<Rect>> {
        self.ops.clear();
        self.glyphs.clear();
        self.open.clear();
        self.paths_used = 0;
        let mut walker = Walker {
            encoding,
            list: self,
            glyph_cache: glyphs,
            images,
            stats,
            ramps: Vec::new(),
            image_patches: Vec::new(),
            draw_ix: 0,
            draw_data_ix: 0,
            transform_ix: None,
            style_ix: None,
            path: PathState::default(),
            bounds: Some(Vec::new()),
            complete: true,
        };
        walker.run();
        let complete = walker.complete;
        let bounds = walker.bounds.take().unwrap_or_default();
        (complete && bounds.iter().all(Rect::is_finite)).then_some(bounds)
    }
}

#[derive(Default)]
struct PathState {
    /// Pool index of the path being built.
    index: Option<usize>,
    /// Transform current at the path's first segment; its geometry space.
    transform: Option<Transform>,
    /// Maps later segments into that space when the transform changed
    /// within the path.
    remap: Option<Affine>,
    in_subpath: bool,
    min: Point,
    max: Point,
}

struct Walker<'a> {
    encoding: &'a Encoding,
    list: &'a mut DisplayList,
    glyph_cache: &'a mut GlyphCache,
    images: &'a mut ImageCache,
    stats: &'a mut Stats,
    /// `(draw data offset, stops, extend)` for each gradient, by offset.
    ramps: Vec<(usize, std::ops::Range<usize>, Extend)>,
    /// `(draw data offset, patch index)` for each image, by offset.
    image_patches: Vec<(usize, usize)>,
    draw_ix: usize,
    draw_data_ix: usize,
    transform_ix: Option<usize>,
    style_ix: Option<usize>,
    path: PathState,
    /// Only bounds are collected ([`DisplayList::bounds`]): no draw is
    /// listed and no brush decoded.
    bounds: Option<Vec<Rect>>,
    /// Every draw so far was walked in full.
    complete: bool,
}

const IDENTITY: Transform = Transform::IDENTITY;

pub(crate) fn to_affine(t: &Transform) -> Affine {
    t.to_kurbo()
}

fn f32_point(data: &[u32], word: usize) -> Option<Point> {
    let x = f32::from_bits(*data.get(word)?);
    let y = f32::from_bits(*data.get(word + 1)?);
    Some(Point::new(f64::from(x), f64::from(y)))
}

fn i16_point(data: &[u32], word: usize) -> Option<Point> {
    let raw = *data.get(word)?;
    let x = ((raw << 16) as i32) >> 16;
    let y = (raw as i32) >> 16;
    Some(Point::new(f64::from(x), f64::from(y)))
}

/// IEEE binary16 bits to f32, for the stroke miter limit.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f32::from(bits & 0x3ff);
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

fn cap(bits: u32) -> Cap {
    match bits {
        1 => Cap::Square,
        2 => Cap::Round,
        _ => Cap::Butt,
    }
}

pub(crate) fn decode_style(style: EncStyle) -> Style {
    let flags = style.flags_and_miter_limit;
    if flags & EncStyle::FLAGS_STYLE_BIT == 0 {
        return Style::Fill(if flags & EncStyle::FLAGS_FILL_BIT == 0 {
            Fill::NonZero
        } else {
            Fill::EvenOdd
        });
    }
    let join = match flags & EncStyle::FLAGS_JOIN_MASK {
        EncStyle::FLAGS_JOIN_BITS_MITER => Join::Miter,
        EncStyle::FLAGS_JOIN_BITS_ROUND => Join::Round,
        _ => Join::Bevel,
    };
    let start_cap = cap((flags & EncStyle::FLAGS_START_CAP_MASK) >> 26);
    let end_cap = cap((flags & EncStyle::FLAGS_END_CAP_MASK) >> 24);
    let miter_limit = f16_to_f32((flags & EncStyle::MITER_LIMIT_MASK) as u16);
    Style::Stroke(
        Stroke::new(f64::from(style.line_width))
            .with_join(join)
            .with_start_cap(start_cap)
            .with_end_cap(end_cap)
            .with_miter_limit(f64::from(miter_limit)),
    )
}

/// Unpremultiply a packed premultiplied RGBA8 draw color.
fn premul_color(rgba: u32) -> AlphaColor<Srgb> {
    let [r, g, b, a] = rgba.to_le_bytes();
    if a == 0 {
        return AlphaColor::new([0.0; 4]);
    }
    let alpha = f32::from(a) / 255.0;
    let un = |c: u8| (f32::from(c) / 255.0 / alpha).min(1.0);
    AlphaColor::new([un(r), un(g), un(b), alpha])
}

/// Bounds of `local` (a control-point box) under `transform`, grown by a
/// stroke's reach when stroked.
fn device_bbox(local: Rect, transform: Affine, style: &Style) -> Rect {
    let local = match style {
        Style::Fill(_) => local,
        Style::Stroke(stroke) => {
            let reach = match stroke.join {
                Join::Miter => stroke.miter_limit.max(1.5),
                _ => 1.5,
            };
            local.inflate(stroke.width * 0.5 * reach, stroke.width * 0.5 * reach)
        }
    };
    transform.transform_rect_bbox(local)
}

/// Whether `encoding` opens a layer that blends with what is under it other
/// than by source over, found by walking the draw tags alone: no paths or
/// glyphs are decoded.
pub(crate) fn encoding_reads_backdrop(encoding: &Encoding) -> bool {
    let mut offset = 0usize;
    for tag in &encoding.draw_tags {
        if *tag == DrawTag::BEGIN_CLIP
            && let Some(&bits) = encoding.draw_data.get(offset)
            && let LayerMode::Blend(mode, _) = blend_mode(bits)
            && (mode.mix != Mix::Normal || mode.compose != Compose::SrcOver)
        {
            return true;
        }
        offset += ((tag.0 >> 2) & 7) as usize;
    }
    false
}

/// Whether every layer still open after the first `draws` draw tags of
/// `encoding` is a plain clip or a source-over group at full opacity, so a
/// draw there lands on the target as if no layer were open (clipped). Walks
/// the draw tags alone. `false` when `encoding` has fewer draw tags.
pub(crate) fn plain_layers_open_at(encoding: &Encoding, draws: usize) -> bool {
    let Some(tags) = encoding.draw_tags.get(..draws) else {
        return false;
    };
    let mut offset = 0usize;
    // Whether each open layer is plain, innermost last.
    let mut open: Vec<bool> = Vec::new();
    for tag in tags {
        match *tag {
            DrawTag::BEGIN_CLIP => {
                let plain = match (
                    encoding.draw_data.get(offset),
                    encoding.draw_data.get(offset + 1),
                ) {
                    (Some(&bits), Some(&alpha)) => match blend_mode(bits) {
                        LayerMode::Clip => true,
                        LayerMode::Blend(mode, _) => {
                            mode.mix == Mix::Normal
                                && mode.compose == Compose::SrcOver
                                && f32::from_bits(alpha) >= 1.0
                        }
                        LayerMode::LuminanceMask => false,
                    },
                    _ => false,
                };
                open.push(plain);
            }
            DrawTag::END_CLIP => {
                open.pop();
            }
            _ => {}
        }
        offset += ((tag.0 >> 2) & 7) as usize;
    }
    open.iter().all(|plain| *plain)
}

fn blend_mode(bits: u32) -> LayerMode {
    if bits == DrawBeginClip::LUMINANCE_MASK_BLEND_MODE {
        return LayerMode::LuminanceMask;
    }
    let mix = match (bits >> 8) & 0xff {
        0 => Mix::Normal,
        1 => Mix::Multiply,
        2 => Mix::Screen,
        3 => Mix::Overlay,
        4 => Mix::Darken,
        5 => Mix::Lighten,
        6 => Mix::ColorDodge,
        7 => Mix::ColorBurn,
        8 => Mix::HardLight,
        9 => Mix::SoftLight,
        10 => Mix::Difference,
        11 => Mix::Exclusion,
        12 => Mix::Hue,
        13 => Mix::Saturation,
        14 => Mix::Color,
        15 => Mix::Luminosity,
        // 0x80 is the legacy clip mix: a plain clip layer.
        _ => return LayerMode::Clip,
    };
    let compose = match bits & 0xff {
        0 => Compose::Clear,
        1 => Compose::Copy,
        2 => Compose::Dest,
        4 => Compose::DestOver,
        5 => Compose::SrcIn,
        6 => Compose::DestIn,
        7 => Compose::SrcOut,
        8 => Compose::DestOut,
        9 => Compose::SrcAtop,
        10 => Compose::DestAtop,
        11 => Compose::Xor,
        12 => Compose::Plus,
        13 => Compose::PlusLighter,
        _ => Compose::SrcOver,
    };
    LayerMode::Blend(BlendMode::new(mix, compose), 1.0)
}

fn extend_mode(bits: u32) -> Extend {
    match bits & 3 {
        1 => Extend::Repeat,
        2 => Extend::Reflect,
        _ => Extend::Pad,
    }
}

impl Walker<'_> {
    fn run(&mut self) {
        let encoding = self.encoding;
        let resources = &encoding.resources;
        for (index, patch) in resources.patches.iter().enumerate() {
            match patch {
                Patch::Ramp {
                    draw_data_offset,
                    stops,
                    extend,
                } => self.ramps.push((*draw_data_offset, stops.clone(), *extend)),
                Patch::Image {
                    draw_data_offset, ..
                } => self.image_patches.push((*draw_data_offset, index)),
                Patch::GlyphRun { .. } => {}
            }
        }
        // Patches are pushed in stream order, so these are already sorted.
        let runs: SmallVec<[usize; 64]> = resources
            .patches
            .iter()
            .filter_map(|patch| match patch {
                Patch::GlyphRun { index } => Some(*index),
                _ => None,
            })
            .collect();
        let mut next_run = 0;

        let tags = &encoding.path_tags;
        let data = &encoding.path_data;
        let mut cursor = 0usize;
        for (tag_ix, tag) in tags.iter().enumerate() {
            while let Some(&run) = runs.get(next_run) {
                match resources.glyph_runs.get(run) {
                    Some(r) if r.stream_offsets.path_tags == tag_ix => {
                        self.glyph_run(r);
                        next_run += 1;
                    }
                    Some(_) => break,
                    None => next_run += 1,
                }
            }
            match *tag {
                PathTag::TRANSFORM => {
                    self.transform_ix = Some(self.transform_ix.map_or(0, |i| i + 1));
                }
                PathTag::STYLE => self.style_ix = Some(self.style_ix.map_or(0, |i| i + 1)),
                PathTag::PATH => self.end_path(),
                tag if tag.is_path_segment() && !self.segment(tag, data, &mut cursor) => {
                    self.complete = false;
                    return;
                }
                _ => {}
            }
        }
        while let Some(&run) = runs.get(next_run) {
            if let Some(r) = resources.glyph_runs.get(run) {
                self.glyph_run(r);
            }
            next_run += 1;
        }
        // Layers left open are closed at the end of the scene.
        while !self.list.open.is_empty() {
            self.end_clip();
        }
    }

    fn transform(&self) -> Transform {
        self.transform_ix
            .and_then(|i| self.encoding.transforms.get(i))
            .copied()
            .unwrap_or(IDENTITY)
    }

    fn style(&self) -> EncStyle {
        self.style_ix
            .and_then(|i| self.encoding.styles.get(i))
            .copied()
            .unwrap_or_default()
    }

    /// Append one segment to the current path. Returns false when the path
    /// data is truncated.
    fn segment(&mut self, tag: PathTag, data: &[u32], cursor: &mut usize) -> bool {
        let n = usize::from(tag.path_segment_type().0);
        let is_f32 = tag.is_f32();
        let size = if is_f32 { 2 } else { 1 };
        let mut pts = [Point::ZERO; 4];
        for (k, pt) in pts.iter_mut().enumerate().take(n + 1) {
            let p = if is_f32 {
                f32_point(data, *cursor + 2 * k)
            } else {
                i16_point(data, *cursor + k)
            };
            match p {
                Some(p) => *pt = p,
                None => return false,
            }
        }
        *cursor += n * size;
        let subpath_end = tag.is_subpath_end();
        if subpath_end {
            *cursor += size;
        }

        let is_stroke = self.style().flags_and_miter_limit & EncStyle::FLAGS_STYLE_BIT != 0;
        let current = self.transform();
        if self.path.index.is_none() {
            self.path.index = Some(self.list.alloc_path());
            self.path.transform = Some(current);
            self.path.remap = None;
            self.path.in_subpath = false;
            self.path.min = Point::new(f64::INFINITY, f64::INFINITY);
            self.path.max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        } else if self.path.transform != Some(current) {
            let first = to_affine(&self.path.transform.unwrap_or(IDENTITY));
            self.path.remap = Some(first.inverse() * to_affine(&current));
        }
        if let Some(remap) = self.path.remap {
            for p in &mut pts[..=n] {
                *p = remap * *p;
            }
        }
        let Some(index) = self.path.index else {
            return true;
        };
        let path = &mut self.list.paths[index];

        if is_stroke && subpath_end {
            // Cap marker: a line closes the subpath, a quad leaves it open.
            if self.path.in_subpath && n == 1 {
                path.close_path();
            }
            self.path.in_subpath = false;
            return true;
        }
        if !self.path.in_subpath {
            path.move_to(pts[0]);
            self.path.in_subpath = true;
        }
        match n {
            1 => path.line_to(pts[1]),
            2 => path.quad_to(pts[1], pts[2]),
            _ => path.curve_to(pts[1], pts[2], pts[3]),
        }
        for p in &pts[..=n] {
            self.path.min.x = self.path.min.x.min(p.x);
            self.path.min.y = self.path.min.y.min(p.y);
            self.path.max.x = self.path.max.x.max(p.x);
            self.path.max.y = self.path.max.y.max(p.y);
        }
        if subpath_end {
            path.close_path();
            self.path.in_subpath = false;
        }
        true
    }

    /// The next draw tag and the offset of its data.
    fn next_draw(&mut self) -> Option<(DrawTag, usize)> {
        let tag = *self.encoding.draw_tags.get(self.draw_ix)?;
        self.draw_ix += 1;
        let offset = self.draw_data_ix;
        self.draw_data_ix += ((tag.0 >> 2) & 7) as usize;
        Some((tag, offset))
    }

    fn draw_data(&self, offset: usize, len: usize) -> Option<&[u32]> {
        self.encoding.draw_data.get(offset..offset + len)
    }

    fn end_path(&mut self) {
        let path = std::mem::take(&mut self.path);
        let Some((tag, offset)) = self.next_draw() else {
            return;
        };
        let brush_transform = to_affine(&self.transform());
        let style = decode_style(self.style());
        let geometry = path.index.map(|index| {
            let transform = to_affine(&path.transform.unwrap_or(IDENTITY));
            let local = Rect::from_points(path.min, path.max);
            (index, transform, local)
        });
        if self.bounds.is_some() {
            self.bound_draw(
                tag,
                offset,
                geometry.map(|(_, transform, local)| (transform, local)),
            );
            return;
        }
        match tag {
            DrawTag::END_CLIP => self.end_clip(),
            DrawTag::BEGIN_CLIP => {
                let Some(params) = self.draw_data(offset, 2) else {
                    return;
                };
                let mode = match blend_mode(params[0]) {
                    LayerMode::Blend(mode, _) => LayerMode::Blend(mode, f32::from_bits(params[1])),
                    other => other,
                };
                let (path, transform, fill, bbox) = match geometry {
                    Some((index, transform, local)) => match &style {
                        Style::Fill(fill) => (
                            Some(index),
                            transform,
                            *fill,
                            device_bbox(local, transform, &style),
                        ),
                        Style::Stroke(stroke) => {
                            // A stroked clip clips to its outline.
                            let outline = vello_cpu::kurbo::stroke(
                                self.list.paths[index].iter(),
                                stroke,
                                &StrokeOpts::default(),
                                0.1,
                            );
                            self.list.paths[index] = outline;
                            let bbox = transform
                                .transform_rect_bbox(self.list.paths[index].bounding_box());
                            (Some(index), transform, Fill::NonZero, bbox)
                        }
                    },
                    None => (None, Affine::IDENTITY, Fill::NonZero, Rect::ZERO),
                };
                if matches!(mode, LayerMode::LuminanceMask) {
                    self.stats.unsupported += 1;
                }
                if let LayerMode::Blend(mode, _) = mode
                    && (mode.mix != Mix::Normal || mode.compose != Compose::SrcOver)
                {
                    self.mark_nested_blend();
                }
                self.list.open.push(self.list.ops.len());
                self.list.ops.push(Op::Begin {
                    path,
                    transform,
                    fill,
                    mode,
                    bbox,
                    end: usize::MAX,
                    nested_blend: false,
                });
            }
            DrawTag::BLUR_RECT => {
                let Some(d) = self.draw_data(offset, 5) else {
                    return;
                };
                let Some((index, transform, local)) = geometry else {
                    return;
                };
                let params = BlurParams {
                    width: f32::from_bits(d[1]),
                    height: f32::from_bits(d[2]),
                    radius: f32::from_bits(d[3]),
                    std_dev: f32::from_bits(d[4]),
                };
                self.list.ops.push(Op::Blur {
                    path: index,
                    transform,
                    brush_transform,
                    params,
                    color: premul_color(d[0]),
                    bbox: device_bbox(local, transform, &Style::Fill(Fill::NonZero)),
                });
            }
            _ => {
                let Some(brush) = self.brush(tag, offset, brush_transform) else {
                    return;
                };
                let Some((index, transform, local)) = geometry else {
                    return;
                };
                let bbox = device_bbox(local, transform, &style);
                self.list.ops.push(Op::Draw {
                    path: index,
                    transform,
                    style,
                    brush,
                    bbox,
                });
            }
        }
    }

    /// [`DisplayList::bounds`]' share of one draw: what it can paint.
    fn bound_draw(&mut self, tag: DrawTag, offset: usize, geometry: Option<(Affine, Rect)>) {
        let style = decode_style(self.style());
        let reach = match tag {
            DrawTag::END_CLIP => return,
            DrawTag::BEGIN_CLIP => {
                let Some(&bits) = self.encoding.draw_data.get(offset) else {
                    self.complete = false;
                    return;
                };
                match blend_mode(bits) {
                    // A clip, or a group composited by source over at any
                    // opacity, paints only where what it holds does.
                    LayerMode::Clip | LayerMode::LuminanceMask => return,
                    LayerMode::Blend(mode, _)
                        if mode.mix == Mix::Normal && mode.compose == Compose::SrcOver =>
                    {
                        return;
                    }
                    // Any other blend reaches every pixel of its clip.
                    LayerMode::Blend(..) => {
                        geometry.map(|(transform, local)| device_bbox(local, transform, &style))
                    }
                }
            }
            DrawTag::BLUR_RECT => geometry.map(|(transform, local)| {
                device_bbox(local, transform, &Style::Fill(Fill::NonZero))
            }),
            // A draw without geometry paints nothing.
            _ => match geometry {
                Some((transform, local)) => Some(device_bbox(local, transform, &style)),
                None => return,
            },
        };
        match (reach, self.bounds.as_mut()) {
            (Some(rect), Some(bounds)) => bounds.push(rect),
            _ => self.complete = false,
        }
    }

    /// Every open layer now contains a non-normal blend layer.
    fn mark_nested_blend(&mut self) {
        for &open in &self.list.open {
            if let Some(Op::Begin { nested_blend, .. }) = self.list.ops.get_mut(open) {
                *nested_blend = true;
            }
        }
    }

    fn end_clip(&mut self) {
        let Some(begin) = self.list.open.pop() else {
            return;
        };
        let end_ix = self.list.ops.len();
        if let Some(Op::Begin { end, .. }) = self.list.ops.get_mut(begin) {
            *end = end_ix;
        }
        self.list.ops.push(Op::End);
    }

    fn ramp(&self, offset: usize) -> Option<(ColorStops, Extend)> {
        let ix = self
            .ramps
            .binary_search_by_key(&offset, |(o, _, _)| *o)
            .ok()?;
        let (_, stops, extend) = self.ramps.get(ix)?;
        let stops: &[ColorStop] = self.encoding.resources.color_stops.get(stops.clone())?;
        Some((ColorStops(stops.iter().copied().collect()), *extend))
    }

    fn gradient(&self, kind: GradientKind, offset: usize, transform: Affine) -> Option<Brush> {
        let (stops, extend) = self.ramp(offset)?;
        let gradient = Gradient {
            kind,
            extend,
            interpolation_cs: peniko::color::ColorSpaceTag::Srgb,
            hue_direction: peniko::color::HueDirection::default(),
            // vello's ramps interpolate premultiplied sRGB.
            interpolation_alpha_space: InterpolationAlphaSpace::Premultiplied,
            stops,
        };
        // vello's fine shader evaluates gradients at pixel corners, vello_cpu
        // at pixel centres; shifting the brush half a pixel matches vello.
        Some(Brush::Gradient(
            gradient,
            Affine::translate((0.5, 0.5)) * transform,
        ))
    }

    fn brush(&mut self, tag: DrawTag, offset: usize, transform: Affine) -> Option<Brush> {
        let pt = |d: &[u32], i: usize| {
            Point::new(
                f64::from(f32::from_bits(d[i])),
                f64::from(f32::from_bits(d[i + 1])),
            )
        };
        match tag {
            DrawTag::COLOR => {
                let d = self.draw_data(offset, 1)?;
                Some(Brush::Solid(premul_color(d[0])))
            }
            DrawTag::LINEAR_GRADIENT => {
                let d = self.draw_data(offset, 5)?;
                // vello maps a linear gradient's end points to the device and
                // runs it perpendicular to the line between them there, which
                // differs from an affine map of the gradient under a
                // non-uniform scale or skew. Do the same.
                let kind = GradientKind::Linear(LinearGradientPosition {
                    start: transform * pt(d, 1),
                    end: transform * pt(d, 3),
                });
                self.gradient(kind, offset, Affine::IDENTITY)
            }
            DrawTag::RADIAL_GRADIENT => {
                let d = self.draw_data(offset, 7)?;
                let kind = GradientKind::Radial(RadialGradientPosition {
                    start_center: pt(d, 1),
                    end_center: pt(d, 3),
                    start_radius: f32::from_bits(d[5]),
                    end_radius: f32::from_bits(d[6]),
                });
                self.gradient(kind, offset, transform)
            }
            DrawTag::SWEEP_GRADIENT => {
                let d = self.draw_data(offset, 5)?;
                let tau = std::f32::consts::TAU;
                let kind = GradientKind::Sweep(SweepGradientPosition {
                    center: pt(d, 1),
                    start_angle: f32::from_bits(d[3]) * tau,
                    end_angle: f32::from_bits(d[4]) * tau,
                });
                self.gradient(kind, offset, transform)
            }
            DrawTag::IMAGE => {
                let d = self.draw_data(offset, 3)?;
                let sample_alpha = d[2];
                let ix = self
                    .image_patches
                    .binary_search_by_key(&offset, |(o, _)| *o)
                    .ok()?;
                let (_, patch) = *self.image_patches.get(ix)?;
                let encoding = self.encoding;
                let Some(Patch::Image { image, .. }) = encoding.resources.patches.get(patch) else {
                    return None;
                };
                let pixmap = self.images.get(image)?;
                // vello's GPU path samples High like Medium (bilinear).
                let quality = match (sample_alpha >> 12) & 3 {
                    0 => ImageQuality::Low,
                    _ => ImageQuality::Medium,
                };
                let sampler = ImageSampler {
                    x_extend: extend_mode(sample_alpha >> 10),
                    y_extend: extend_mode(sample_alpha >> 8),
                    quality,
                    alpha: (sample_alpha & 0xff) as f32 / 255.0,
                };
                Some(Brush::Image(
                    Image {
                        image: ImageSource::Pixmap(pixmap),
                        sampler,
                    },
                    transform,
                ))
            }
            _ => None,
        }
    }

    /// Emit one glyph run as a single draw, as the resolver splices it in.
    fn glyph_run(&mut self, run: &GlyphRun) {
        let encoding = self.encoding;
        let resources = &encoding.resources;
        let Some(coords) = resources
            .normalized_coords
            .get(run.normalized_coords.clone())
        else {
            return;
        };
        let Some(run_glyphs) = resources.glyphs.get(run.glyphs.clone()) else {
            return;
        };
        let paint_transform = run.transform * run.brush_transform.unwrap_or(IDENTITY);
        let mut hint = run.hint;
        let mut font_size = run.font_size;
        let mut transform = run.transform;
        let mut scale = 1.0;
        if hint {
            if transform.matrix[0] == transform.matrix[3]
                && transform.matrix[1] == 0.0
                && transform.matrix[2] == 0.0
            {
                scale = transform.matrix[0];
                font_size *= scale;
                transform.matrix = [1.0, 0.0, 0.0, 1.0];
            } else {
                hint = false;
            }
        }
        let style = match &run.style {
            peniko::Style::Fill(fill) => Style::Fill(*fill),
            peniko::Style::Stroke(stroke) => {
                if stroke.width == 0.0 {
                    return;
                }
                Style::Stroke(stroke.clone())
            }
        };
        let Some(mut session) = self.glyph_cache.session(GlyphKey::run(
            &run.font,
            coords,
            font_size,
            &run.font_embolden,
            hint,
            &run.style,
        )) else {
            // vello's resolver drops a run whose font does not load without
            // consuming its draw tag; mirror that.
            return;
        };
        let start = self.list.glyphs.len();
        let mut bbox = Rect::ZERO;
        let mut first = true;
        for glyph in run_glyphs {
            let (outline, missed) = session.get(glyph.id);
            if missed {
                self.stats.glyph_misses += 1;
            }
            let Some(outline) = outline else {
                continue;
            };
            let mut xform = transform
                * Transform {
                    matrix: [1.0, 0.0, 0.0, -1.0],
                    translation: [glyph.x * scale, glyph.y * scale],
                };
            if let Some(glyph_transform) = run.glyph_transform {
                xform = xform * glyph_transform;
            }
            if hint {
                xform.translation[1] = xform.translation[1].round();
            }
            let xform = to_affine(&xform);
            let glyph_bbox = device_bbox(outline.bbox, xform, &style);
            bbox = if first {
                glyph_bbox
            } else {
                bbox.union(glyph_bbox)
            };
            first = false;
            self.list.glyphs.push(GlyphInstance {
                outline,
                transform: xform,
                bbox: glyph_bbox,
            });
        }
        let end = self.list.glyphs.len();
        let Some((tag, offset)) = self.next_draw() else {
            return;
        };
        if let Some(bounds) = self.bounds.as_mut() {
            if start != end {
                bounds.push(bbox);
            }
            return;
        }
        let Some(brush) = self.brush(tag, offset, to_affine(&paint_transform)) else {
            return;
        };
        if start == end {
            return;
        }
        self.list.ops.push(Op::Glyphs {
            glyphs: start..end,
            style,
            brush,
            bbox,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_round_trips_common_miter_limits() {
        for v in [0.0f32, 1.0, 4.0, 10.0, 0.5] {
            let bits = vello_encoding::Style::from_stroke(
                &Stroke::new(1.0).with_miter_limit(f64::from(v)),
            )
            .map(|s| (s.flags_and_miter_limit & 0xffff) as u16)
            .unwrap_or_default();
            assert!((f16_to_f32(bits) - v).abs() < 1e-3, "{v}");
        }
    }

    #[test]
    fn stroke_style_round_trips() {
        let stroke = Stroke::new(3.0)
            .with_join(Join::Round)
            .with_start_cap(Cap::Square)
            .with_end_cap(Cap::Round);
        let Some(enc) = vello_encoding::Style::from_stroke(&stroke) else {
            panic!("stroke encodes");
        };
        let Style::Stroke(back) = decode_style(enc) else {
            panic!("decodes as a stroke");
        };
        assert_eq!(back.width, 3.0);
        assert_eq!(back.join, Join::Round);
        assert_eq!(back.start_cap, Cap::Square);
        assert_eq!(back.end_cap, Cap::Round);
    }

    #[test]
    fn premul_color_unpremultiplies() {
        let c = premul_color(u32::from_le_bytes([64, 32, 0, 128]));
        let [r, g, b, a] = c.components;
        assert!((a - 128.0 / 255.0).abs() < 1e-6);
        assert!((r - 0.5).abs() < 0.01 && (g - 0.25).abs() < 0.01 && b == 0.0);
    }
}
