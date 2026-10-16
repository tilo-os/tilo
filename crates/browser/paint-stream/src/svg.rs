//! SVG images as vector drawings (feature `svg`): a page's logos reach a
//! client as paths it draws at any scale, not as bytes it cannot decode.
//! Without the feature (the WASM client, which only draws) nothing converts.

/// An SVG image as shapes: its view box and its paths.
pub type Drawing = (crate::Rect, Vec<crate::Shape>);

/// Most shapes an SVG image may become; larger drawings stay unrendered.
#[cfg(feature = "svg")]
const MAX_SVG_SHAPES: usize = 4096;

/// Without the `svg` feature no image converts.
#[cfg(not(feature = "svg"))]
pub fn drawing(_bytes: &[u8]) -> Option<Drawing> {
    None
}

/// Parse an SVG image into shapes with their transforms applied. Gradients
/// fall back to their first stop; text without fonts is skipped.
#[cfg(feature = "svg")]
pub fn drawing(bytes: &[u8]) -> Option<Drawing> {
    let head = bytes.get(..bytes.len().min(1024))?;
    if !String::from_utf8_lossy(head).contains("<svg") {
        return None;
    }
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let mut shapes = Vec::new();
    svg_group(tree.root(), &mut shapes);
    (!shapes.is_empty()).then(|| {
        (
            crate::Rect::new(0.0, 0.0, size.width(), size.height()),
            shapes,
        )
    })
}

#[cfg(feature = "svg")]
fn svg_group(group: &usvg::Group, shapes: &mut Vec<crate::Shape>) {
    for node in group.children() {
        if shapes.len() >= MAX_SVG_SHAPES {
            return;
        }
        match node {
            usvg::Node::Group(g) => svg_group(g, shapes),
            usvg::Node::Path(p) if p.is_visible() => {
                let Some(data) = p.data().clone().transform(p.abs_transform()) else {
                    continue;
                };
                let fill = p
                    .fill()
                    .and_then(|f| svg_paint(f.paint(), f.opacity().get()));
                let stroke = p
                    .stroke()
                    .and_then(|s| svg_paint(s.paint(), s.opacity().get()));
                if fill.is_none() && stroke.is_none() {
                    continue;
                }
                let scale = p.abs_transform().get_scale();
                shapes.push(crate::Shape {
                    path: svg_path_data(&data),
                    fill,
                    stroke,
                    stroke_width: p
                        .stroke()
                        .map_or(0.0, |s| s.width().get() * scale.0.max(scale.1)),
                    even_odd: p
                        .fill()
                        .is_some_and(|f| f.rule() == usvg::FillRule::EvenOdd),
                });
            }
            _ => {}
        }
    }
}

#[cfg(feature = "svg")]
fn svg_paint(paint: &usvg::Paint, opacity: f32) -> Option<crate::Color> {
    let (c, stop_opacity) = match paint {
        usvg::Paint::Color(c) => (*c, 1.0),
        usvg::Paint::LinearGradient(g) => {
            let stop = g.stops().first()?;
            (stop.color(), stop.opacity().get())
        }
        usvg::Paint::RadialGradient(g) => {
            let stop = g.stops().first()?;
            (stop.color(), stop.opacity().get())
        }
        usvg::Paint::Pattern(_) => return None,
    };
    Some(crate::Color([
        c.red,
        c.green,
        c.blue,
        unit_alpha(opacity * stop_opacity),
    ]))
}

#[cfg(feature = "svg")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=1.0 and scaled to 0.0..=255.0 before rounding"
)]
fn unit_alpha(alpha: f32) -> u8 {
    (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(feature = "svg")]
fn svg_path_data(path: &usvg::tiny_skia_path::Path) -> String {
    use std::fmt::Write as _;
    let mut d = String::new();
    for segment in path.segments() {
        // Writing to a String cannot fail.
        let _ = match segment {
            usvg::tiny_skia_path::PathSegment::MoveTo(p) => write!(d, "M{} {}", p.x, p.y),
            usvg::tiny_skia_path::PathSegment::LineTo(p) => write!(d, "L{} {}", p.x, p.y),
            usvg::tiny_skia_path::PathSegment::QuadTo(a, b) => {
                write!(d, "Q{} {} {} {}", a.x, a.y, b.x, b.y)
            }
            usvg::tiny_skia_path::PathSegment::CubicTo(a, b, c) => {
                write!(d, "C{} {} {} {} {} {}", a.x, a.y, b.x, b.y, c.x, c.y)
            }
            usvg::tiny_skia_path::PathSegment::Close => write!(d, "Z"),
        };
    }
    d
}
