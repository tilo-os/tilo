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
//! | `"i"` | x, y, w, h (an image, shown as a placeholder) |
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
use vello_cpu::peniko::{Fill, FontData};
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

/// What one [`DomEncoder::encode`] produced.
pub struct DomFrame {
    pub ops: String,
    pub fonts: Vec<NewFont>,
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
}

impl DomEncoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn encode(&mut self, encoding: &Encoding) -> DomFrame {
        let mut stats = Stats::default();
        self.list.dom = true;
        self.list
            .decode(encoding, &mut self.glyphs, &mut self.images, &mut stats);
        let mut marks = self.list.marks.iter().peekable();
        let mut out = String::with_capacity(self.list.ops.len() * 64);
        let mut fonts = Vec::new();
        let mut texts = 0;
        out.push('[');
        let mut first = true;
        let mut sep = |out: &mut String| {
            if !std::mem::replace(&mut first, false) {
                out.push(',');
            }
        };
        for (index, op) in self.list.ops.iter().enumerate() {
            while let Some((_, kind)) = marks.next_if(|(at, _)| *at <= index) {
                sep(&mut out);
                mark(&mut out, *kind);
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
                    draw(&mut out, path, *transform, style, brush);
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
                    let info = self
                        .fonts
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
                    sep(&mut out);
                    out.push_str("[\"t\",");
                    json_str(&mut out, text);
                    let _ = write!(
                        out,
                        ",{:.2},{:.2},{:.2},\"{}\",",
                        origin.x, origin.y, font_size, info.family
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
        for (_, kind) in marks {
            sep(&mut out);
            mark(&mut out, *kind);
        }
        out.push(']');
        DomFrame {
            ops: out,
            fonts,
            texts,
            draws: self.list.ops.len(),
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

fn draw(out: &mut String, path: &BezPath, transform: Affine, style: &Style, brush: &Brush) {
    if let Brush::Image(_, _) = brush {
        let rect = transform.transform_rect_bbox(path.bounding_box());
        let _ = write!(
            out,
            "[\"i\",{:.2},{:.2},{:.2},{:.2}]",
            rect.x0,
            rect.y0,
            rect.width(),
            rect.height()
        );
        return;
    }
    if let Some((rect, radius)) = box_shape(path, transform) {
        let (fill, stroke, width) = match style {
            Style::Fill(_) => (css_brush(brush), String::new(), 0.0),
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

/// A solid colour as CSS; a gradient as its first stop, which the prototype
/// does not translate yet.
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
