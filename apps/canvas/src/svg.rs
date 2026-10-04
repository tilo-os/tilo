//! The artwork as SVG, and SVG as marks.
//!
//! Export writes plain layers as the vector art they are: every shape,
//! colour, gradient, outline style, blend, shadow, clip and transform. A
//! painted layer is a simulation, not geometry, so it goes in as the picture
//! it paints, its lettering still as text. The reference picture is not part
//! of the artwork and is never written.
//!
//! Import reads a reasonable subset through `usvg`: shapes and paths (as
//! paths), solid and gradient paint, outlines with their dashes, caps and
//! joins, group opacity and blend, clip paths, and text. Everything lands as
//! one `draw`, so an import is one undo step.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo's geometry operators (Affine, Point, Vec2) are float geometry, not overflowing integer arithmetic"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};
use ui::vello::kurbo::{Affine, BezPath, Point, Rect};
use ui::vello::peniko::Color;
use usvg::roxmltree;

use crate::actions::{Draw, NewMark};
use crate::media;
use crate::model::{
    Artwork, Gradient, Layer, MAX_MARKS_PER_ACTION, Mark, Shape, Style, path_data,
    short_number as num,
};
use crate::render::{colour, silhouette};

/// Import an SVG drawing as marks, in one step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportSvg {
    /// SVG markup, an absolute path to a `.svg`, or a content address.
    pub svg: String,
    /// The layer to draw on; the top layer when absent.
    pub layer: Option<String>,
    /// The group the marks join (one is made when absent).
    pub group: Option<String>,
    /// Fit into `[x, y, width, height]`, aspect kept; own size otherwise.
    pub fit: Option<[f64; 4]>,
}

/// The id export gives the ground, which import leaves to the artwork.
const GROUND_ID: &str = "tilo-canvas-ground";

/// The largest SVG an import reads, in bytes.
pub const MAX_SVG_BYTES: usize = 8 << 20;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `#rrggbb` and the alpha of a colour.
fn hex(colour: Color) -> (String, f64) {
    let rgba = colour.to_rgba8();
    (
        format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b),
        f64::from(rgba.a) / 255.0,
    )
}

/// ` name="#rrggbb"` and, when translucent, ` name-opacity="a"`.
fn paint_attr(name: &str, colour: Color) -> String {
    let (hex, alpha) = hex(colour);
    if alpha < 1.0 {
        format!(r#" {name}="{hex}" {name}-opacity="{}""#, num(alpha))
    } else {
        format!(r#" {name}="{hex}""#)
    }
}

fn matrix(affine: Affine) -> String {
    format!("matrix({})", affine.as_coeffs().map(num).join(" "))
}

/// The artwork as an SVG document, on its ground when `ground`. `painted`
/// renders a painted layer alone over transparency as a PNG, which the
/// document embeds. A text mark with an entry in `outlines` is written as
/// that geometry instead of as text; an image mark with one in `pictures`
/// embeds that picture (its crop, turn and grade as shown) in its frame.
pub fn export(
    art: &Artwork,
    ground: bool,
    mut painted: impl FnMut(&Layer) -> Option<Vec<u8>>,
    outlines: BTreeMap<String, BezPath>,
    pictures: BTreeMap<String, Embedded>,
    dabbed: BTreeMap<String, (Vec<u8>, Rect)>,
) -> String {
    let mut out = Writer {
        outlines,
        pictures,
        dabbed,
        ..Writer::default()
    };
    if ground {
        let ground = colour(&art.ground.colour, &[]).unwrap_or(crate::render::BLANK_GROUND);
        let _ = write!(
            out.body,
            r#"<rect id="{GROUND_ID}" width="{}" height="{}"{}/>"#,
            num(art.width),
            num(art.height),
            paint_attr("fill", ground)
        );
    }
    for layer in art.layers.iter().filter(|layer| layer.visible) {
        let palette = media::palette_for(art, &layer.id);
        let _ = write!(out.body, r#"<g data-layer="{}""#, escape(&layer.id));
        if layer.opacity < 1.0 {
            let _ = write!(out.body, r#" opacity="{}""#, num(layer.opacity));
        }
        out.body.push('>');
        let simulated = !layer.medium.is_empty() && media::resolve(art, &layer.medium).is_some();
        if simulated {
            if let Some(png) = painted(layer) {
                let _ = write!(
                    out.body,
                    r#"<image width="{}" height="{}" href="data:image/png;base64,{}"/>"#,
                    num(art.width),
                    num(art.height),
                    base64::engine::general_purpose::STANDARD.encode(png)
                );
            }
            for mark in art.marks_of(&layer.id) {
                if matches!(mark.shape, Shape::Text { .. }) {
                    out.enter_group(&mark.group);
                    out.mark(art, mark, &palette);
                }
            }
        } else {
            let start = out.body.len();
            for mark in art.marks_of(&layer.id) {
                if mark.erases() {
                    out.enter_group("");
                    out.erase(art, mark, start);
                    continue;
                }
                out.enter_group(&mark.group);
                // Shapes' colours as their adjustments grade them; a
                // picture comes graded.
                let steps =
                    crate::render::grades_of(&[mark.style.adjust.as_ref(), layer.adjust.as_ref()]);
                let graded = crate::render::graded_mark(mark, &palette, &steps);
                out.mark(art, &graded, &palette);
            }
        }
        out.enter_group("");
        out.body.push_str("</g>");
    }
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#,
        w = num(art.width),
        h = num(art.height)
    );
    if !art.title.is_empty() {
        let _ = write!(svg, "<title>{}</title>", escape(&art.title));
    }
    if !out.defs.is_empty() {
        let _ = write!(svg, "<defs>{}</defs>", out.defs);
    }
    svg.push_str(&out.body);
    svg.push_str("</svg>\n");
    svg
}

/// A picture embedded in the document: its bytes and their type.
pub struct Embedded {
    pub bytes: Vec<u8>,
    /// `image/png` or `image/jpeg`.
    pub mime: &'static str,
}

#[derive(Default)]
struct Writer {
    defs: String,
    body: String,
    clips: BTreeSet<String>,
    /// Text marks written as their letters' outlines, by id.
    outlines: BTreeMap<String, BezPath>,
    /// Image marks' pictures as shown, by id.
    pictures: BTreeMap<String, Embedded>,
    /// How many paths text has been set along.
    text_paths: usize,
    /// Strokes laid in dabs (pencil, charcoal, airbrush, a soft eraser) as
    /// PNG pictures and the area they cover in the mark's units, by id.
    dabbed: BTreeMap<String, (Vec<u8>, Rect)>,
    /// The groups the next element is written in, outermost first.
    open: Vec<String>,
}

/// An `<image>` of `png` laid over `area`, with `attrs`.
fn image_element(attrs: &str, png: &[u8], area: Rect) -> String {
    format!(
        r#"<image{attrs} x="{}" y="{}" width="{}" height="{}" href="data:image/png;base64,{}"/>"#,
        num(area.x0),
        num(area.y0),
        num(area.width()),
        num(area.height()),
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

impl Writer {
    /// Close and open `<g data-group>` elements so what follows is written
    /// inside `group` (`a/b`: b inside a); empty is outside every group.
    fn enter_group(&mut self, group: &str) {
        let path: Vec<&str> = group.split('/').filter(|part| !part.is_empty()).collect();
        let kept = self
            .open
            .iter()
            .zip(&path)
            .take_while(|(open, part)| open.as_str() == **part)
            .count();
        for _ in kept..self.open.len() {
            self.body.push_str("</g>");
        }
        self.open.truncate(kept);
        for depth in kept..path.len() {
            let whole = path.get(..=depth).unwrap_or_default().join("/");
            let _ = write!(self.body, r#"<g data-group="{}">"#, escape(&whole));
            self.open.push((*path.get(depth).unwrap_or(&"")).to_owned());
        }
    }

    /// An eraser: everything written on its layer before it, from byte
    /// `start` of the body, is masked by what it covers.
    fn erase(&mut self, art: &Artwork, mark: &Mark, start: usize) {
        let inner = self.body.split_off(start.min(self.body.len()));
        let mut attrs = String::new();
        if let Some(transform) = mark.transform {
            let _ = write!(attrs, r#" transform="{}""#, matrix(Affine::new(transform)));
        }
        if mark.style.opacity() < 1.0 {
            let _ = write!(attrs, r#" opacity="{}""#, num(mark.style.opacity()));
        }
        let cover = match self.dabbed.get(&mark.id) {
            Some((png, area)) => image_element(&attrs, png, *area),
            None => format!(
                r#"<path{attrs} d="{}" fill="black"/>"#,
                path_data(&silhouette(mark))
            ),
        };
        let cover = match self.clip_def(art, mark) {
            Some(name) => format!(r#"<g clip-path="url(#c-{name})">{cover}</g>"#),
            None => cover,
        };
        let cover = match self.area_def(mark) {
            Some(area) => format!(r#"<g clip-path="url(#{area})">{cover}</g>"#),
            None => cover,
        };
        let (w, h) = (num(art.width), num(art.height));
        let _ = write!(
            self.defs,
            r#"<mask id="e-{id}" maskUnits="userSpaceOnUse" x="0" y="0" width="{w}" height="{h}"><rect width="{w}" height="{h}" fill="white"/>{cover}</mask>"#,
            id = mark.id
        );
        let _ = write!(self.body, r#"<g mask="url(#e-{})">{inner}</g>"#, mark.id);
    }

    /// The clip path of what `mark` is cut to (a mark, or a group's
    /// members), written into the defs once; its name.
    fn clip_def(&mut self, art: &Artwork, mark: &Mark) -> Option<String> {
        let clips = art.clips_of(mark);
        if clips.is_empty() {
            return None;
        }
        // A group's id may hold `/`, which an SVG id may not.
        let name = mark.clip.replace('/', "--");
        if self.clips.insert(name.clone()) {
            let mut paths = String::new();
            for clip in &clips {
                let transform = if clip.transform.is_some() {
                    format!(r#" transform="{}""#, matrix(clip.affine()))
                } else {
                    String::new()
                };
                let _ = write!(
                    paths,
                    r#"<path d="{}"{transform}/>"#,
                    path_data(&silhouette(clip))
                );
            }
            let _ = write!(self.defs, r#"<clipPath id="c-{name}">{paths}</clipPath>"#);
        }
        Some(name)
    }

    /// The clip path of the area `mark` is kept inside, written into the
    /// defs: its id, if the mark has an area.
    fn area_def(&mut self, mark: &Mark) -> Option<String> {
        let area = mark.area()?;
        let id = format!("a-{}", mark.id);
        let _ = write!(
            self.defs,
            r#"<clipPath id="{id}"><path d="{}"/></clipPath>"#,
            path_data(&area),
        );
        Some(id)
    }

    fn mark(&mut self, art: &Artwork, mark: &Mark, palette: &[tilo_wet_media::Pigment]) {
        let style = &mark.style;
        let area = self.area_def(mark);
        if let Some(area) = &area {
            let _ = write!(self.body, r#"<g clip-path="url(#{area})">"#);
        }
        let mut wrap = String::new();
        if let Some(name) = self.clip_def(art, mark) {
            let _ = write!(wrap, r#" clip-path="url(#c-{name})""#);
        }
        if let Some(blend) = style
            .blend
            .as_deref()
            .or_else(|| crate::brushes::default_blend(style))
        {
            let _ = write!(wrap, r#" style="mix-blend-mode:{blend}""#);
        }
        if let Some(shadow) = &style.shadow {
            let ink = shadow
                .colour
                .as_deref()
                .and_then(|value| colour(value, palette))
                .unwrap_or(crate::render::SHADOW_INK);
            let (ink, alpha) = hex(ink);
            let _ = write!(
                self.defs,
                r#"<filter id="s-{}" x="-50%" y="-50%" width="200%" height="200%"><feDropShadow dx="{}" dy="{}" stdDeviation="{}" flood-color="{ink}" flood-opacity="{}"/></filter>"#,
                mark.id,
                num(shadow.offset[0]),
                num(shadow.offset[1]),
                num(shadow.blur / 2.0),
                num(alpha)
            );
            let _ = write!(wrap, r#" filter="url(#s-{})""#, mark.id);
        }
        if !wrap.is_empty() {
            let _ = write!(self.body, "<g{wrap}>");
        }
        let finishes = self.finishes(mark, palette);
        for filter in &finishes {
            let _ = write!(self.body, r#"<g filter="url(#{filter})">"#);
        }
        self.element(mark, style, palette);
        for _ in &finishes {
            self.body.push_str("</g>");
        }
        if !wrap.is_empty() {
            self.body.push_str("</g>");
        }
        if area.is_some() {
            self.body.push_str("</g>");
        }
    }

    /// The filters that give `mark` its grain, inner shadow and feathered
    /// edge, outermost first, written into the defs.
    fn finishes(&mut self, mark: &Mark, palette: &[tilo_wet_media::Pigment]) -> Vec<String> {
        let style = &mark.style;
        let mut filters = Vec::new();
        let region = r#"x="-50%" y="-50%" width="200%" height="200%""#;
        if let Some(grain) = style.grain {
            let id = format!("n-{}", mark.id);
            let _ = write!(
                self.defs,
                r#"<filter id="{id}" {region}><feTurbulence type="fractalNoise" baseFrequency="0.8" numOctaves="1" seed="2"/><feColorMatrix type="saturate" values="0"/><feComposite in2="SourceAlpha" operator="in" result="grain"/><feBlend in="grain" in2="SourceGraphic" mode="overlay" result="grained"/><feComposite in="grained" in2="SourceGraphic" operator="arithmetic" k2="{}" k3="{}"/><feComposite in2="SourceAlpha" operator="in"/></filter>"#,
                num(grain),
                num(1.0 - grain)
            );
            filters.push(id);
        }
        if let Some(shadow) = &style.inner_shadow {
            let id = format!("i-{}", mark.id);
            let ink = shadow
                .colour
                .as_deref()
                .and_then(|value| colour(value, palette))
                .unwrap_or(crate::render::SHADOW_INK);
            let (ink, alpha) = hex(ink);
            let _ = write!(
                self.defs,
                r#"<filter id="{id}" {region}><feComponentTransfer in="SourceAlpha"><feFuncA type="table" tableValues="1 0"/></feComponentTransfer><feGaussianBlur stdDeviation="{}"/><feOffset dx="{}" dy="{}" result="outside"/><feFlood flood-color="{ink}" flood-opacity="{}"/><feComposite in2="outside" operator="in"/><feComposite in2="SourceAlpha" operator="in" result="inner"/><feMerge><feMergeNode in="SourceGraphic"/><feMergeNode in="inner"/></feMerge></filter>"#,
                num(shadow.blur / 2.0),
                num(shadow.offset[0]),
                num(shadow.offset[1]),
                num(alpha)
            );
            filters.push(id);
        }
        if let Some(feather) = style.feather {
            let id = format!("f-{}", mark.id);
            let _ = write!(
                self.defs,
                r#"<filter id="{id}" {region}><feGaussianBlur stdDeviation="{}"/></filter>"#,
                num(feather / 2.0)
            );
            filters.push(id);
        }
        filters
    }

    /// An image mark: its picture as shown, in its frame, and its border.
    fn picture(
        &mut self,
        mark: &Mark,
        style: &Style,
        attrs: &str,
        palette: &[tilo_wet_media::Pigment],
    ) {
        let Shape::Image {
            x,
            y,
            width,
            height,
            ..
        } = &mark.shape
        else {
            return;
        };
        let named = |text: &Option<String>| text.as_deref().and_then(|text| colour(text, palette));
        let place = format!(
            r#" x="{}" y="{}" width="{}" height="{}""#,
            num(*x),
            num(*y),
            num(*width),
            num(*height)
        );
        let _ = write!(self.body, "<g{attrs}>");
        if let Some(picture) = self.pictures.get(&mark.id) {
            let _ = write!(
                self.body,
                r#"<image{place} preserveAspectRatio="none" href="data:{};base64,{}"/>"#,
                picture.mime,
                base64::engine::general_purpose::STANDARD.encode(&picture.bytes)
            );
        }
        if style.stroke_width > 0.0
            && let Some(line) = named(&style.stroke)
        {
            let _ = write!(
                self.body,
                r#"<rect{place} fill="none"{} stroke-width="{}"/>"#,
                paint_attr("stroke", line),
                num(style.stroke_width)
            );
        }
        self.body.push_str("</g>");
    }

    fn element(&mut self, mark: &Mark, style: &Style, palette: &[tilo_wet_media::Pigment]) {
        let mut attrs = format!(r#" id="{}""#, mark.id);
        if let Some(transform) = mark.transform {
            let _ = write!(attrs, r#" transform="{}""#, matrix(Affine::new(transform)));
        }
        if style.opacity() < 1.0 {
            let _ = write!(attrs, r#" opacity="{}""#, num(style.opacity()));
        }
        let named = |text: &Option<String>| text.as_deref().and_then(|text| colour(text, palette));
        if matches!(mark.shape, Shape::Image { .. }) {
            self.picture(mark, style, &attrs, palette);
            return;
        }
        if let Shape::Stroke { .. } = mark.shape {
            if let Some((png, area)) = self.dabbed.get(&mark.id) {
                let image = image_element(&attrs, png, *area);
                self.body.push_str(&image);
            } else if let Some(ink) = named(&style.stroke).or_else(|| named(&style.fill)) {
                #[allow(clippy::cast_possible_truncation, reason = "an alpha within 0..=1")]
                let ink = ink.multiply_alpha(crate::brushes::ink_alpha(style) as f32);
                let _ = write!(
                    self.body,
                    r#"<path{attrs} d="{}"{}/>"#,
                    path_data(&silhouette(mark)),
                    paint_attr("fill", ink)
                );
            }
            return;
        }
        let closed = mark.shape.closed() || matches!(mark.shape, Shape::Text { .. });
        if closed {
            if let Some(gradient) = &style.gradient {
                self.gradient(&mark.id, gradient, palette);
                let _ = write!(attrs, r#" fill="url(#g-{})""#, mark.id);
            } else if let Some(fill) = named(&style.fill) {
                attrs.push_str(&paint_attr("fill", fill));
            } else {
                attrs.push_str(r#" fill="none""#);
            }
        } else {
            attrs.push_str(r#" fill="none""#);
        }
        let width = if mark.shape.closed() {
            style.stroke_width
        } else {
            style.stroke_width.max(1.0)
        };
        if width > 0.0
            && !matches!(mark.shape, Shape::Text { .. })
            && let Some(line) = named(&style.stroke)
        {
            attrs.push_str(&paint_attr("stroke", line));
            let _ = write!(
                attrs,
                r#" stroke-width="{}" stroke-linecap="{}" stroke-linejoin="{}""#,
                num(width),
                style.cap.as_deref().unwrap_or("round"),
                style.join.as_deref().unwrap_or("round")
            );
            if !style.dash.is_empty() {
                let dashes: Vec<String> = style.dash.iter().copied().map(num).collect();
                let _ = write!(attrs, r#" stroke-dasharray="{}""#, dashes.join(" "));
            }
        }
        if let Some(outline) = self.outlines.get(&mark.id) {
            let _ = write!(self.body, r#"<path{attrs} d="{}"/>"#, path_data(outline));
            return;
        }
        self.shape(&mark.shape, &attrs);
    }

    fn shape(&mut self, shape: &Shape, attrs: &str) {
        let points = |points: &[[f64; 2]]| {
            points
                .iter()
                .map(|[x, y]| format!("{},{}", num(*x), num(*y)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = match shape {
            Shape::Rect {
                x,
                y,
                width,
                height,
                radius,
            } => {
                let round = if *radius > 0.0 {
                    format!(r#" rx="{}""#, num(*radius))
                } else {
                    String::new()
                };
                write!(
                    self.body,
                    r#"<rect{attrs} x="{}" y="{}" width="{}" height="{}"{round}/>"#,
                    num(*x),
                    num(*y),
                    num(*width),
                    num(*height)
                )
            }
            Shape::Ellipse { cx, cy, rx, ry } => write!(
                self.body,
                r#"<ellipse{attrs} cx="{}" cy="{}" rx="{}" ry="{}"/>"#,
                num(*cx),
                num(*cy),
                num(*rx),
                num(*ry)
            ),
            Shape::Polygon { points: at } => {
                write!(self.body, r#"<polygon{attrs} points="{}"/>"#, points(at))
            }
            Shape::Line { points: at } => {
                write!(self.body, r#"<polyline{attrs} points="{}"/>"#, points(at))
            }
            Shape::Path { d } => write!(self.body, r#"<path{attrs} d="{}"/>"#, escape(d)),
            Shape::Text { .. } => {
                self.text(shape, attrs);
                Ok(())
            }
            Shape::Stroke { .. } | Shape::Image { .. } => Ok(()),
        };
    }

    fn text(&mut self, shape: &Shape, attrs: &str) {
        let Shape::Text {
            x,
            y,
            text,
            size,
            font,
            weight,
            italic,
            align,
            line_height,
            width,
            tracking,
            path,
        } = shape
        else {
            return;
        };
        let generic = match font.as_deref().map(crate::fonts::nearest).as_deref() {
            Some("serif") => "serif",
            Some("mono") => "monospace",
            _ => "sans-serif",
        };
        let family = font
            .as_deref()
            .and_then(crate::fonts::family)
            .unwrap_or(ui::UI_SANS_FAMILY);
        let family = format!("'{}', {generic}", escape(family));
        let (anchor, at) = match (align.as_deref(), width) {
            (Some("center"), Some(width)) => ("middle", x + width / 2.0),
            (Some("end"), Some(width)) => ("end", x + width),
            (Some("center"), None) => ("middle", *x),
            (Some("end"), None) => ("end", *x),
            _ => ("start", *x),
        };
        let spacing = tracking
            .filter(|tracking| *tracking != 0.0)
            .map(|tracking| format!(r#" letter-spacing="{}em""#, num(tracking / 1000.0)))
            .unwrap_or_default();
        let _ = write!(
            self.body,
            r#"<text{attrs} x="{}" y="{}" font-family="{family}" font-size="{}" font-weight="{}"{}{spacing} text-anchor="{anchor}">"#,
            num(at),
            num(*y),
            num(*size),
            weight.unwrap_or(400),
            if *italic {
                r#" font-style="italic""#
            } else {
                ""
            },
        );
        if let Some(d) = path {
            self.text_paths = self.text_paths.saturating_add(1);
            let id = format!("t-{}", self.text_paths);
            let _ = write!(self.defs, r#"<path id="{id}" d="{}"/>"#, escape(d));
            let offset = match anchor {
                "middle" => "50%",
                "end" => "100%",
                _ => "0",
            };
            let words = escape(&text.replace('\n', " "));
            let href = format!("#{id}");
            let _ = write!(
                self.body,
                r#"<textPath href="{href}" startOffset="{offset}">{words}</textPath></text>"#
            );
            return;
        }
        let leading = size * line_height.unwrap_or(1.2);
        for (index, line) in text.split('\n').enumerate() {
            let _ = write!(
                self.body,
                r#"<tspan x="{}" dy="{}">{}</tspan>"#,
                num(at),
                num(if index == 0 { 0.0 } else { leading }),
                escape(line)
            );
        }
        self.body.push_str("</text>");
    }

    fn gradient(&mut self, id: &str, gradient: &Gradient, palette: &[tilo_wet_media::Pigment]) {
        let [fx, fy] = gradient.from;
        let [tx, ty] = gradient.to;
        let _ = if gradient.kind == "radial" {
            let focal = gradient
                .focal
                .map(|[x, y]| format!(r#" fx="{}" fy="{}""#, num(x), num(y)))
                .unwrap_or_default();
            write!(
                self.defs,
                r#"<radialGradient id="g-{id}" gradientUnits="userSpaceOnUse" cx="{}" cy="{}" r="{}"{focal}>"#,
                num(fx),
                num(fy),
                num(Point::new(fx, fy).distance(Point::new(tx, ty)))
            )
        } else {
            write!(
                self.defs,
                r#"<linearGradient id="g-{id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}">"#,
                num(fx),
                num(fy),
                num(tx),
                num(ty)
            )
        };
        for (offset, text) in &gradient.stops {
            if let Some(stop) = colour(text, palette) {
                let (hex, alpha) = hex(stop);
                let _ = write!(
                    self.defs,
                    r#"<stop offset="{}" stop-color="{hex}" stop-opacity="{}"/>"#,
                    num(offset.clamp(0.0, 1.0)),
                    num(alpha)
                );
            }
        }
        self.defs.push_str(if gradient.kind == "radial" {
            "</radialGradient>"
        } else {
            "</linearGradient>"
        });
    }
}

/// The SVG an import names: markup itself, a file, or stored content.
///
/// # Errors
/// Returns why the SVG cannot be had.
pub fn source(svg: &str) -> Result<String, String> {
    let trimmed = svg.trim_start();
    if trimmed.starts_with('<') {
        return Ok(svg.to_owned());
    }
    let bytes = if let Some(hash) = ui::tilo_actions::content_hash(trimmed) {
        tilo_content::ContentStore::for_app()
            .read(hash)
            .ok_or_else(|| format!("no stored content {trimmed}"))?
    } else if trimmed.starts_with('/') {
        crate::resource::read_file(trimmed, MAX_SVG_BYTES)?
    } else {
        return Err("svg is SVG markup, an absolute path to a .svg, or a content address".into());
    };
    if bytes.len() > MAX_SVG_BYTES {
        return Err(format!("the SVG is over {MAX_SVG_BYTES} bytes"));
    }
    String::from_utf8(bytes).map_err(|_| "the SVG is not UTF-8 text".to_owned())
}

/// `import` as the `draw` it makes.
///
/// # Errors
/// Returns why the SVG cannot be read or holds nothing to draw.
pub fn import(request: &ImportSvg) -> Result<Draw, String> {
    let svg = source(&request.svg)?;
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default())
        .map_err(|error| format!("not an SVG this can read: {error}"))?;
    let digest = blake3::hash(svg.as_bytes()).to_hex();
    let group = request
        .group
        .clone()
        .unwrap_or_else(|| format!("svg-{}", digest.get(..6).unwrap_or_default()));
    let document = roxmltree::Document::parse(&svg).ok();
    let mut importer = Importer {
        marks: Vec::new(),
        prefix: group.clone(),
        groups: document.as_ref().map(group_paths).unwrap_or_default(),
    };
    importer.group(tree.root(), 1.0, Context::default());
    if let Some(document) = &document {
        importer.texts(document, tree.size());
    }
    let mut marks = importer.marks;
    if marks.is_empty() {
        return Err("the SVG has no shapes or text this can draw".into());
    }
    if marks.len() > MAX_MARKS_PER_ACTION {
        return Err(format!(
            "the SVG has {} shapes; at most {MAX_MARKS_PER_ACTION} per import",
            marks.len()
        ));
    }
    if let Some([x, y, width, height]) = request.fit {
        let size = tree.size();
        let (w, h) = (f64::from(size.width()), f64::from(size.height()));
        if !(width > 0.0 && height > 0.0 && x.is_finite() && y.is_finite()) {
            return Err("fit is [x, y, width, height] with a positive width and height".into());
        }
        let scale = (width / w).min(height / h);
        let fit = Affine::translate((
            x + (width - w * scale) / 2.0,
            y + (height - h * scale) / 2.0,
        )) * Affine::scale(scale);
        for mark in &mut marks {
            refit(mark, fit, scale);
        }
    }
    // The SVG's own groups nest inside the import's: `logo/mail`.
    for mark in &mut marks {
        mark.group = Some(match mark.group.take() {
            Some(inner) => format!("{group}/{inner}"),
            None => group.clone(),
        });
    }
    Ok(Draw {
        layer: request.layer.clone(),
        marks,
    })
}

/// Move an imported mark by `fit`, a uniform `scale` and a shift.
fn refit(mark: &mut NewMark, fit: Affine, scale: f64) {
    match &mut mark.shape {
        Shape::Path { d } => {
            let mut path = BezPath::from_svg(d).unwrap_or_default();
            path.apply_affine(fit);
            *d = path_data(&path);
        }
        Shape::Text { x, y, size, .. } => {
            let at = fit * Point::new(*x, *y);
            (*x, *y, *size) = (at.x, at.y, *size * scale);
        }
        _ => {}
    }
    if let Some(width) = &mut mark.stroke_width {
        *width *= scale;
    }
    if let Some(gradient) = &mut mark.gradient {
        let from = fit * Point::new(gradient.from[0], gradient.from[1]);
        let to = fit * Point::new(gradient.to[0], gradient.to[1]);
        gradient.from = [from.x, from.y];
        gradient.to = [to.x, to.y];
    }
}

/// What a group passes down to its members.
#[derive(Clone, Default)]
struct Context {
    clip: Option<String>,
    blend: Option<String>,
    shadow: Option<crate::model::Shadow>,
}

struct Importer {
    marks: Vec<NewMark>,
    prefix: String,
    /// The group path of each element with an id: see [`group_paths`].
    groups: std::collections::BTreeMap<String, String>,
}

/// A name from another program as a group id segment: `Mail Icon` is
/// `mail-icon`.
fn segment(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(40).collect();
    (!out.is_empty()).then_some(out)
}

/// The group each element of `document` with an id sits in: the
/// `data-group` this canvas writes, else the ids of the `<g>` elements
/// around it, outermost first (`icons/mail`). Layers are not groups.
fn group_paths(document: &roxmltree::Document<'_>) -> std::collections::BTreeMap<String, String> {
    let mut paths = std::collections::BTreeMap::new();
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        if let Some(path) = group_path(node)
            && let Some(id) = node.attribute("id")
        {
            paths.insert(id.to_owned(), path);
        }
    }
    paths
}

fn group_path(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let groups: Vec<roxmltree::Node<'_, '_>> = node
        .ancestors()
        .skip(1)
        .filter(|step| step.has_tag_name("g") && step.attribute("data-layer").is_none())
        .collect();
    if let Some(written) = groups.iter().find_map(|step| step.attribute("data-group")) {
        let parts: Vec<String> = written.split('/').filter_map(segment).collect();
        return (!parts.is_empty()).then(|| parts.join("/"));
    }
    let parts: Vec<String> = groups
        .iter()
        .rev()
        .filter_map(|step| step.attribute("id"))
        .filter_map(segment)
        .take(8)
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn affine(transform: usvg::Transform) -> Affine {
    Affine::new([
        f64::from(transform.sx),
        f64::from(transform.ky),
        f64::from(transform.kx),
        f64::from(transform.sy),
        f64::from(transform.tx),
        f64::from(transform.ty),
    ])
}

fn bez(path: &usvg::tiny_skia_path::Path, transform: Affine) -> BezPath {
    use usvg::tiny_skia_path::PathSegment;
    let point = |at: usvg::tiny_skia_path::Point| Point::new(f64::from(at.x), f64::from(at.y));
    let mut out = BezPath::new();
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(at) => out.move_to(point(at)),
            PathSegment::LineTo(at) => out.line_to(point(at)),
            PathSegment::QuadTo(a, at) => out.quad_to(point(a), point(at)),
            PathSegment::CubicTo(a, b, at) => out.curve_to(point(a), point(b), point(at)),
            PathSegment::Close => out.close_path(),
        }
    }
    out.apply_affine(transform);
    out
}

fn blend_name(mode: usvg::BlendMode) -> Option<&'static str> {
    use usvg::BlendMode as B;
    Some(match mode {
        B::Normal => return None,
        B::Multiply => "multiply",
        B::Screen => "screen",
        B::Overlay => "overlay",
        B::Darken => "darken",
        B::Lighten => "lighten",
        B::ColorDodge => "color-dodge",
        B::ColorBurn => "color-burn",
        B::HardLight => "hard-light",
        B::SoftLight => "soft-light",
        B::Difference => "difference",
        B::Exclusion => "exclusion",
        B::Hue => "hue",
        B::Saturation => "saturation",
        B::Color => "color",
        B::Luminosity => "luminosity",
    })
}

fn rgba(colour: usvg::Color, alpha: f64) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "alpha is clamped to 0..=1 before scaling to a byte"
    )]
    let alpha = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    if alpha == 255 {
        format!("#{:02x}{:02x}{:02x}", colour.red, colour.green, colour.blue)
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{alpha:02x}",
            colour.red, colour.green, colour.blue
        )
    }
}

/// A uniform scale's size: how much `transform` grows lengths.
fn growth(transform: Affine) -> f64 {
    transform.determinant().abs().sqrt()
}

impl Importer {
    fn next_id(&self) -> String {
        let prefix = self.prefix.get(..40).unwrap_or(&self.prefix);
        format!("{prefix}-{}", self.marks.len())
    }

    fn group(&mut self, group: &usvg::Group, opacity: f64, mut context: Context) {
        let opacity = opacity * f64::from(group.opacity().get());
        if let Some(blend) = blend_name(group.blend_mode()) {
            context.blend = Some(blend.to_owned());
        }
        let grows = growth(affine(group.abs_transform()));
        for filter in group.filters() {
            for primitive in filter.primitives() {
                if let usvg::filter::Kind::DropShadow(shadow) = primitive.kind() {
                    context.shadow = Some(crate::model::Shadow {
                        offset: [
                            f64::from(shadow.dx()) * grows,
                            f64::from(shadow.dy()) * grows,
                        ],
                        blur: f64::from(shadow.std_dev_x().get()) * 2.0 * grows,
                        colour: Some(rgba(shadow.color(), f64::from(shadow.opacity().get()))),
                    });
                }
            }
        }
        if let Some(clip) = group.clip_path() {
            let base = affine(group.abs_transform()) * affine(clip.transform());
            let mut outline = BezPath::new();
            collect_paths(clip.root(), base, &mut outline);
            if !outline.elements().is_empty() {
                let id = self.next_id();
                self.marks.push(NewMark {
                    id: Some(id.clone()),
                    name: Some("clip".into()),
                    fill: Some("#000000".into()),
                    opacity: Some(0.0),
                    clip: context.clip.clone(),
                    ..NewMark::new(Shape::Path {
                        d: path_data(&outline),
                    })
                });
                context.clip = Some(id);
            }
        }
        for child in group.children() {
            match child {
                usvg::Node::Group(inner) => self.group(inner, opacity, context.clone()),
                usvg::Node::Path(path) if path.is_visible() => {
                    self.path(path, opacity, &context);
                }
                _ => {}
            }
        }
    }

    fn path(&mut self, path: &usvg::Path, opacity: f64, context: &Context) {
        if path.id() == GROUND_ID {
            return;
        }
        let transform = affine(path.abs_transform());
        let outline = bez(path.data(), transform);
        if outline.elements().is_empty() {
            return;
        }
        let mut mark = NewMark {
            id: Some(self.next_id()),
            name: (!path.id().is_empty()).then(|| path.id().to_owned()),
            opacity: (opacity < 1.0).then_some(opacity),
            blend: context.blend.clone(),
            clip: context.clip.clone(),
            shadow: context.shadow.clone(),
            group: self.groups.get(path.id()).cloned(),
            ..NewMark::new(Shape::Path {
                d: path_data(&outline),
            })
        };
        if let Some(fill) = path.fill() {
            let alpha = f64::from(fill.opacity().get());
            match fill.paint() {
                usvg::Paint::Color(colour) => mark.fill = Some(rgba(*colour, alpha)),
                usvg::Paint::LinearGradient(gradient) => {
                    let to_art = transform * affine(gradient.transform());
                    let from = to_art * Point::new(gradient.x1().into(), gradient.y1().into());
                    let to = to_art * Point::new(gradient.x2().into(), gradient.y2().into());
                    mark.gradient = Some(Gradient {
                        kind: "linear".into(),
                        from: [from.x, from.y],
                        to: [to.x, to.y],
                        stops: stops(gradient.stops(), alpha),
                        focal: None,
                    });
                }
                usvg::Paint::RadialGradient(gradient) => {
                    let to_art = transform * affine(gradient.transform());
                    let centre = to_art * Point::new(gradient.cx().into(), gradient.cy().into());
                    let radius = f64::from(gradient.r().get()) * growth(to_art);
                    let focal = to_art * Point::new(gradient.fx().into(), gradient.fy().into());
                    mark.gradient = Some(Gradient {
                        kind: "radial".into(),
                        from: [centre.x, centre.y],
                        to: [centre.x + radius, centre.y],
                        stops: stops(gradient.stops(), alpha),
                        focal: (focal.distance(centre) > 1e-3).then_some([focal.x, focal.y]),
                    });
                }
                usvg::Paint::Pattern(_) => {
                    mark.fill = Some(rgba(usvg::Color::black(), alpha * 0.5));
                }
            }
        }
        if let Some(stroke) = path.stroke()
            && let usvg::Paint::Color(colour) = stroke.paint()
        {
            let scale = growth(transform);
            mark.stroke = Some(rgba(*colour, f64::from(stroke.opacity().get())));
            mark.stroke_width = Some(f64::from(stroke.width().get()) * scale);
            mark.cap = Some(
                match stroke.linecap() {
                    usvg::LineCap::Butt => "butt",
                    usvg::LineCap::Round => "round",
                    usvg::LineCap::Square => "square",
                }
                .into(),
            );
            mark.join = Some(
                match stroke.linejoin() {
                    usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => "miter",
                    usvg::LineJoin::Round => "round",
                    usvg::LineJoin::Bevel => "bevel",
                }
                .into(),
            );
            mark.dash = stroke
                .dasharray()
                .map(|dashes| dashes.iter().map(|dash| f64::from(*dash) * scale).collect());
        }
        if mark.fill.is_none() && mark.gradient.is_none() && mark.stroke.is_none() {
            return;
        }
        if mark.fill.is_none() && mark.gradient.is_none() {
            // An outline only: the path is open to the renderer.
            mark.stroke_width = mark.stroke_width.or(Some(1.0));
        }
        self.marks.push(mark);
    }

    /// `<text>` elements, which `usvg` leaves out without its font stack:
    /// position, size, family, weight, style, anchor and fill.
    fn texts(&mut self, document: &roxmltree::Document<'_>, size: usvg::Size) {
        let root = document.root_element();
        let view = root
            .attribute("viewBox")
            .map(|view| {
                view.split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|part| !part.is_empty())
                    .filter_map(|part| part.parse::<f64>().ok())
                    .collect::<Vec<_>>()
            })
            .filter(|view| view.len() == 4);
        let base = match view.as_deref() {
            Some([x, y, w, h]) if *w > 0.0 && *h > 0.0 => {
                Affine::scale_non_uniform(f64::from(size.width()) / w, f64::from(size.height()) / h)
                    * Affine::translate((-x, -y))
            }
            _ => Affine::IDENTITY,
        };
        for node in document
            .descendants()
            .filter(|node| node.has_tag_name("text"))
        {
            let mut transform = base;
            let mut chain: Vec<roxmltree::Node<'_, '_>> = node.ancestors().collect();
            chain.reverse();
            for step in chain {
                if let Some(value) = step.attribute("transform") {
                    transform *= parse_transform(value);
                }
            }
            let words = text_words(node);
            if words.trim().is_empty() || self.marks.len() >= MAX_MARKS_PER_ACTION {
                continue;
            }
            let number =
                |name: &str| inherited(node, name).and_then(|value| leading_number(&value));
            let x = node.attribute("x").and_then(leading_number).unwrap_or(0.0);
            let y = node.attribute("y").and_then(leading_number).unwrap_or(0.0);
            let at = transform * Point::new(x, y);
            let font_size = number("font-size").unwrap_or(16.0) * growth(transform);
            let family = font_family(node);
            let weight = inherited(node, "font-weight").and_then(|weight| match weight.as_str() {
                "bold" | "bolder" => Some(700),
                "normal" => Some(400),
                other => other.parse::<u16>().ok(),
            });
            let tracking = tracking_of(node, number("font-size").unwrap_or(16.0));
            let path = text_path(node, document, transform);
            self.marks.push(NewMark {
                id: Some(self.next_id()),
                fill: Some(text_fill(node)),
                group: group_path(node),
                ..NewMark::new(Shape::Text {
                    x: at.x,
                    y: at.y,
                    text: words,
                    size: font_size.max(0.5),
                    font: family
                        .map(|family| crate::fonts::nearest(&family))
                        .filter(|family| family != "sans"),
                    weight: weight.filter(|weight| (100..=900).contains(weight)),
                    italic: inherited(node, "font-style").is_some_and(|style| style == "italic"),
                    align: inherited(node, "text-anchor").and_then(|anchor| {
                        match anchor.as_str() {
                            "middle" => Some("center".to_owned()),
                            "end" => Some("end".to_owned()),
                            _ => None,
                        }
                    }),
                    line_height: None,
                    width: None,
                    tracking,
                    path,
                })
            });
        }
    }
}

/// A `<text>`'s words, one line per `<tspan>`, spaces collapsed.
fn text_words(node: roxmltree::Node<'_, '_>) -> String {
    let spans: Vec<String> = node
        .children()
        .filter(|child| child.has_tag_name("tspan"))
        .map(|span| {
            span.descendants()
                .filter(roxmltree::Node::is_text)
                .filter_map(|n| n.text())
                .collect()
        })
        .collect();
    let lines = if spans.is_empty() {
        vec![
            node.descendants()
                .filter(roxmltree::Node::is_text)
                .filter_map(|n| n.text())
                .collect::<String>(),
        ]
    } else {
        spans
    };
    lines
        .iter()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A text's `letter-spacing` as tracking, thousandths of its size.
fn tracking_of(node: roxmltree::Node<'_, '_>, size: f64) -> Option<f64> {
    let spacing = inherited(node, "letter-spacing")?;
    let amount = leading_number(&spacing)?;
    let em = if spacing.trim_end().ends_with("em") {
        amount
    } else {
        amount / size.max(0.1)
    };
    Some((em * 1000.0).clamp(-300.0, 1000.0)).filter(|tracking| *tracking != 0.0)
}

/// The path a text's `<textPath>` sets it along, in artwork units.
fn text_path(
    node: roxmltree::Node<'_, '_>,
    document: &roxmltree::Document<'_>,
    transform: Affine,
) -> Option<String> {
    let along = node
        .descendants()
        .find(|child| child.has_tag_name("textPath"))?;
    // `href`, or the older `xlink:href`.
    let href = along
        .attributes()
        .find(|attribute| attribute.name() == "href")?
        .value();
    let id = href.strip_prefix('#')?;
    let target = document
        .descendants()
        .find(|n| n.attribute("id") == Some(id))?;
    let mut path = BezPath::from_svg(target.attribute("d")?).ok()?;
    path.apply_affine(transform);
    Some(path_data(&path))
}

/// The first family a text names, as the canvas names it.
fn font_family(node: roxmltree::Node<'_, '_>) -> Option<String> {
    inherited(node, "font-family").map(|family| {
        let first = family
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches(['\'', '"'])
            .to_owned();
        match first.as_str() {
            "serif" => "serif".to_owned(),
            "monospace" => "mono".to_owned(),
            "sans-serif" | "" => "sans".to_owned(),
            _ => first,
        }
    })
}

/// A text's fill as `#rrggbb[aa]`; black when it is not a hex colour.
fn text_fill(node: roxmltree::Node<'_, '_>) -> String {
    let fill = inherited(node, "fill").unwrap_or_default();
    if fill.starts_with('#') && (fill.len() == 7 || fill.len() == 9) {
        fill
    } else if fill.starts_with('#') && fill.len() == 4 {
        fill.chars().skip(1).fold("#".to_owned(), |mut out, c| {
            out.push(c);
            out.push(c);
            out
        })
    } else {
        "#000000".into()
    }
}

fn stops(stops: &[usvg::Stop], alpha: f64) -> Vec<(f64, String)> {
    stops
        .iter()
        .map(|stop| {
            (
                f64::from(stop.offset().get()),
                rgba(stop.color(), alpha * f64::from(stop.opacity().get())),
            )
        })
        .collect()
}

fn collect_paths(group: &usvg::Group, base: Affine, into: &mut BezPath) {
    for child in group.children() {
        match child {
            usvg::Node::Group(inner) => collect_paths(inner, base, into),
            usvg::Node::Path(path) => {
                for element in bez(path.data(), base * affine(path.abs_transform())).elements() {
                    into.push(*element);
                }
            }
            _ => {}
        }
    }
}

/// A presentation attribute or `style` property of `node` or the nearest
/// ancestor that sets it.
fn inherited(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.ancestors()
        .filter(roxmltree::Node::is_element)
        .find_map(|step| {
            step.attribute("style")
                .and_then(|style| {
                    style.split(';').find_map(|rule| {
                        let (key, value) = rule.split_once(':')?;
                        (key.trim() == name).then(|| value.trim().to_owned())
                    })
                })
                .or_else(|| step.attribute(name).map(str::to_owned))
        })
}

/// The number a length starts with (`24px` is 24).
fn leading_number(value: &str) -> Option<f64> {
    let value = value.trim();
    let end = value
        .char_indices()
        .find(|(index, c)| {
            !(c.is_ascii_digit() || *c == '.' || (*index == 0 && (*c == '-' || *c == '+')))
        })
        .map_or(value.len(), |(index, _)| index);
    value.get(..end)?.parse().ok()
}

/// An SVG `transform` list: matrix, translate, scale and rotate.
fn parse_transform(value: &str) -> Affine {
    let mut result = Affine::IDENTITY;
    for part in value.split(')') {
        let Some((name, args)) = part.split_once('(') else {
            continue;
        };
        let args: Vec<f64> = args
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|arg| !arg.is_empty())
            .filter_map(|arg| arg.parse().ok())
            .collect();
        let step = match (name.trim(), args.as_slice()) {
            ("matrix", coeffs) if coeffs.len() == 6 => {
                <[f64; 6]>::try_from(coeffs).map_or(Affine::IDENTITY, Affine::new)
            }
            ("translate", [x]) => Affine::translate((*x, 0.0)),
            ("translate", [x, y]) => Affine::translate((*x, *y)),
            ("scale", [s]) => Affine::scale(*s),
            ("scale", [x, y]) => Affine::scale_non_uniform(*x, *y),
            ("rotate", [degrees]) => Affine::rotate(degrees.to_radians()),
            ("rotate", [degrees, x, y]) => {
                Affine::rotate_about(degrees.to_radians(), Point::new(*x, *y))
            }
            _ => Affine::IDENTITY,
        };
        result *= step;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_and_lengths_parse() {
        assert_eq!(leading_number("24px"), Some(24.0));
        assert_eq!(leading_number("-1.5em"), Some(-1.5));
        let moved = parse_transform("translate(10, 20) scale(2)") * Point::new(1.0, 1.0);
        assert_eq!((moved.x, moved.y), (12.0, 22.0));
    }
}
