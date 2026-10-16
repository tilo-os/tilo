//! Every draw feature vello encodes, rendered through vello's GPU pipeline
//! and through the CPU replay, compared pixel by pixel.
//!
//! The bound is what two independent anti-aliasing implementations produce:
//! edge pixels differ by a few levels, a share of them by more. A wrong
//! transform, missing draw or misread stream shows up as large differences
//! over whole areas and fails it. PNGs of both renders and their difference
//! land in `target/tmp/cpu-raster/`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "test scenes: failures are the assertion"
)]

mod support;

use std::sync::Arc;

use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};
use tilo_cpu_raster::Region;
use tilo_cpu_raster::compare::{Diff, compare};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Ellipse, Join, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{
    BlendMode, Blob, Color, ColorStop, Compose, Extend, Fill, FontData, Gradient, ImageAlphaType,
    ImageBrush, ImageData, ImageFormat, ImageQuality, Mix,
};
use vello::{Glyph, NormalizedCoord};

const W: u32 = 512;
const H: u32 = 384;

fn color(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

fn star(center: Point, outer: f64, inner: f64, points: usize) -> BezPath {
    let mut path = BezPath::new();
    for i in 0..points * 2 {
        let r = if i % 2 == 0 { outer } else { inner };
        let a = std::f64::consts::PI * i as f64 / points as f64;
        let p = center + (r * a.sin(), -r * a.cos());
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    path.close_path();
    path
}

fn fills() -> Scene {
    let mut s = Scene::new();
    let id = Affine::IDENTITY;
    s.fill(
        Fill::NonZero,
        id,
        color(200, 60, 40, 255),
        None,
        &Rect::new(10.0, 10.0, 120.0, 80.0),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(40, 120, 220, 255),
        None,
        &RoundedRect::new(140.5, 10.25, 260.7, 90.3, 14.0),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(30, 160, 90, 200),
        None,
        &Circle::new((330.0, 55.0), 45.3),
    );
    s.fill(
        Fill::EvenOdd,
        id,
        color(120, 40, 160, 255),
        None,
        &star(Point::new(450.0, 60.0), 50.0, 20.0, 7),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(220, 160, 30, 255),
        None,
        &star(Point::new(60.0, 170.0), 50.0, 20.0, 7),
    );
    let mut curvy = BezPath::new();
    curvy.move_to((120.0, 130.0));
    curvy.quad_to((180.0, 100.0), (240.0, 160.0));
    curvy.curve_to((260.0, 220.0), (150.0, 240.0), (130.0, 200.0));
    curvy.close_path();
    curvy.move_to((150.0, 150.0));
    curvy.line_to((200.0, 170.0));
    curvy.line_to((160.0, 190.0));
    curvy.close_path();
    s.fill(Fill::NonZero, id, color(20, 20, 20, 255), None, &curvy);
    s.fill(
        Fill::NonZero,
        Affine::translate((330.0, 180.0)) * Affine::rotate(0.4),
        color(0, 150, 150, 255),
        None,
        &Rect::new(-50.0, -30.0, 50.0, 30.0),
    );
    s.fill(
        Fill::NonZero,
        Affine::translate((450.0, 180.0))
            * Affine::skew(0.5, 0.1)
            * Affine::scale_non_uniform(1.5, 0.7),
        color(250, 90, 140, 180),
        None,
        &Ellipse::new((0.0, 0.0), (30.0, 40.0), 0.3),
    );
    for i in 0..8 {
        let x = 20.0 + f64::from(i) * 60.3;
        s.fill(
            Fill::NonZero,
            id,
            color(0, 0, 0, 255),
            None,
            &Rect::new(x, 270.0, x + 0.3 + f64::from(i) * 0.25, 370.0),
        );
        s.fill(
            Fill::NonZero,
            id,
            color(40, 90, 200, 120),
            None,
            &Rect::new(x - 10.0, 300.0 + f64::from(i), x + 30.0, 330.0),
        );
    }
    s
}

fn strokes() -> Scene {
    let mut s = Scene::new();
    let id = Affine::IDENTITY;
    let joins = [Join::Miter, Join::Round, Join::Bevel];
    let caps = [Cap::Butt, Cap::Round, Cap::Square];
    for (j, join) in joins.into_iter().enumerate() {
        for (c, cap) in caps.into_iter().enumerate() {
            let ox = 20.0 + 170.0 * j as f64;
            let oy = 20.0 + 70.0 * c as f64;
            let mut zig = BezPath::new();
            zig.move_to((ox, oy + 40.0));
            zig.line_to((ox + 40.0, oy));
            zig.line_to((ox + 80.0, oy + 40.0));
            zig.quad_to((ox + 110.0, oy - 10.0), (ox + 140.0, oy + 30.0));
            s.stroke(
                &Stroke::new(9.0).with_join(join).with_caps(cap),
                id,
                color(30, 60, 160, 255),
                None,
                &zig,
            );
        }
    }
    s.stroke(
        &Stroke::new(3.0),
        id,
        color(200, 30, 30, 255),
        None,
        &Rect::new(20.5, 230.5, 140.5, 300.5),
    );
    s.stroke(
        &Stroke::new(4.0).with_dashes(2.0, [12.0, 6.0]),
        id,
        color(0, 120, 0, 255),
        None,
        &Circle::new((220.0, 265.0), 40.0),
    );
    s.stroke(
        &Stroke::new(2.0),
        Affine::translate((340.0, 265.0)) * Affine::scale_non_uniform(3.0, 1.0),
        color(140, 0, 140, 255),
        None,
        &Circle::new((0.0, 0.0), 25.0),
    );
    for i in 0..6 {
        let y = 320.5 + f64::from(i) * 9.0;
        s.stroke(
            &Stroke::new(0.5 + f64::from(i) * 0.5),
            id,
            color(0, 0, 0, 255),
            None,
            &vello::kurbo::Line::new((20.0, y), (490.0, y + f64::from(i) * 3.0)),
        );
    }
    let mut cubic = BezPath::new();
    cubic.move_to((420.0, 220.0));
    cubic.curve_to((520.0, 230.0), (380.0, 330.0), (480.0, 310.0));
    s.stroke(
        &Stroke::new(6.0).with_caps(Cap::Round),
        id,
        color(220, 120, 0, 255),
        None,
        &cubic,
    );
    s
}

fn stops(colors: &[(f32, Color)]) -> Vec<ColorStop> {
    colors
        .iter()
        .map(|(offset, color)| ColorStop::from((*offset, *color)))
        .collect()
}

fn linear() -> Scene {
    let mut s = Scene::new();
    let id = Affine::IDENTITY;
    let three = stops(&[
        (0.0, color(255, 0, 0, 255)),
        (0.5, color(0, 255, 0, 255)),
        (1.0, color(0, 0, 255, 255)),
    ]);
    for (i, extend) in [Extend::Pad, Extend::Repeat, Extend::Reflect]
        .into_iter()
        .enumerate()
    {
        let x = 10.0 + 165.0 * i as f64;
        let g = Gradient::new_linear((x + 40.0, 20.0), (x + 100.0, 60.0))
            .with_stops(three.as_slice())
            .with_extend(extend);
        s.fill(
            Fill::NonZero,
            id,
            &g,
            None,
            &Rect::new(x, 10.0, x + 155.0, 130.0),
        );
    }
    let fade = stops(&[(0.0, color(20, 20, 20, 255)), (1.0, color(20, 20, 20, 0))]);
    let g = Gradient::new_linear((0.0, 0.0), (1.0, 0.0)).with_stops(fade.as_slice());
    s.fill(
        Fill::NonZero,
        id,
        &g,
        Some(Affine::translate((10.0, 0.0)) * Affine::scale(240.0)),
        &RoundedRect::new(10.0, 150.0, 250.0, 260.0, 20.0),
    );
    let g = Gradient::new_linear((-40.0, 0.0), (40.0, 0.0))
        .with_stops(three.as_slice())
        .with_extend(Extend::Reflect);
    s.fill(
        Fill::NonZero,
        Affine::translate((380.0, 210.0)) * Affine::rotate(0.6),
        &g,
        Some(Affine::rotate(-0.9)),
        &Rect::new(-110.0, -50.0, 110.0, 50.0),
    );
    let g = Gradient::new_linear((10.0, 280.0), (500.0, 370.0)).with_stops(
        stops(&[
            (0.0, color(255, 255, 255, 255)),
            (0.3, color(255, 200, 0, 128)),
            (0.31, color(0, 100, 255, 255)),
            (1.0, color(0, 0, 0, 255)),
        ])
        .as_slice(),
    );
    s.stroke(
        &Stroke::new(30.0),
        id,
        &g,
        None,
        &vello::kurbo::Line::new((20.0, 325.0), (490.0, 325.0)),
    );
    s
}

fn radial() -> Scene {
    let mut s = Scene::new();
    let id = Affine::IDENTITY;
    let three = stops(&[
        (0.0, color(255, 255, 0, 255)),
        (0.6, color(255, 0, 80, 255)),
        (1.0, color(40, 0, 120, 255)),
    ]);
    for (i, extend) in [Extend::Pad, Extend::Repeat, Extend::Reflect]
        .into_iter()
        .enumerate()
    {
        let x = 10.0 + 165.0 * i as f64;
        let c = Point::new(x + 77.0, 70.0);
        let g = Gradient::new_radial(c, 35.0)
            .with_stops(three.as_slice())
            .with_extend(extend);
        s.fill(
            Fill::NonZero,
            id,
            &g,
            None,
            &Rect::new(x, 10.0, x + 155.0, 130.0),
        );
    }
    let g = Gradient::new_two_point_radial((90.0, 210.0), 10.0, (120.0, 220.0), 70.0)
        .with_stops(three.as_slice());
    s.fill(
        Fill::NonZero,
        id,
        &g,
        None,
        &Rect::new(10.0, 140.0, 240.0, 290.0),
    );
    let g = Gradient::new_two_point_radial((330.0, 215.0), 0.0, (380.0, 215.0), 50.0)
        .with_stops(three.as_slice())
        .with_extend(Extend::Repeat);
    s.fill(
        Fill::NonZero,
        id,
        &g,
        None,
        &Rect::new(260.0, 140.0, 500.0, 290.0),
    );
    let g = Gradient::new_radial((0.0, 0.0), 1.0)
        .with_stops(stops(&[(0.0, color(0, 0, 0, 200)), (1.0, color(0, 0, 0, 0))]).as_slice());
    s.fill(
        Fill::NonZero,
        id,
        &g,
        Some(Affine::translate((250.0, 340.0)) * Affine::scale_non_uniform(200.0, 40.0)),
        &Rect::new(10.0, 300.0, 500.0, 380.0),
    );
    s
}

fn sweep() -> Scene {
    let mut s = Scene::new();
    let id = Affine::IDENTITY;
    let hues = stops(&[
        (0.0, color(255, 0, 0, 255)),
        (0.33, color(0, 255, 0, 255)),
        (0.66, color(0, 0, 255, 255)),
        (1.0, color(255, 0, 0, 255)),
    ]);
    let g =
        Gradient::new_sweep((120.0, 120.0), 0.0, std::f32::consts::TAU).with_stops(hues.as_slice());
    s.fill(
        Fill::NonZero,
        id,
        &g,
        None,
        &Circle::new((120.0, 120.0), 100.0),
    );
    for (i, extend) in [Extend::Pad, Extend::Repeat, Extend::Reflect]
        .into_iter()
        .enumerate()
    {
        let c = Point::new(
            300.0 + 100.0 * (i % 2) as f64,
            80.0 + 120.0 * (i / 2) as f64,
        );
        let g = Gradient::new_sweep(c, 0.5, 2.5)
            .with_stops(hues.as_slice())
            .with_extend(extend);
        s.fill(Fill::NonZero, id, &g, None, &Circle::new(c, 48.0));
    }
    let g = Gradient::new_sweep((0.0, 0.0), 1.0, 4.0).with_stops(hues.as_slice());
    s.fill(
        Fill::NonZero,
        Affine::translate((150.0, 310.0)),
        &g,
        Some(Affine::rotate(0.7) * Affine::scale_non_uniform(1.0, 0.4)),
        &Rect::new(-130.0, -60.0, 130.0, 60.0),
    );
    s
}

/// Brushes under non-uniform scale and skew, where an affine map of the
/// gradient and vello's map of its end points differ.
fn skewed() -> Scene {
    let mut s = Scene::new();
    let unit = Rect::new(0.0, 0.0, 1.0, 1.0);
    let hues = stops(&[
        (0.0, color(255, 40, 0, 255)),
        (0.5, color(250, 250, 250, 255)),
        (1.0, color(0, 60, 255, 255)),
    ]);
    let linear = Gradient::new_linear((-0.1, 1.0), (0.8, -0.1)).with_stops(hues.as_slice());
    s.fill(
        Fill::NonZero,
        Affine::translate((10.0, 10.0)) * Affine::scale_non_uniform(490.0, 170.0),
        &linear,
        None,
        &unit,
    );
    let radial = Gradient::new_radial((0.5, 0.5), 0.5)
        .with_stops(hues.as_slice())
        .with_extend(Extend::Reflect);
    s.fill(
        Fill::NonZero,
        Affine::translate((10.0, 190.0)) * Affine::scale_non_uniform(230.0, 90.0),
        &radial,
        None,
        &unit,
    );
    let sweep =
        Gradient::new_sweep((0.5, 0.5), 0.0, std::f32::consts::TAU).with_stops(hues.as_slice());
    s.fill(
        Fill::NonZero,
        Affine::translate((260.0, 190.0))
            * Affine::skew(0.3, 0.0)
            * Affine::scale_non_uniform(200.0, 90.0),
        &sweep,
        None,
        &unit,
    );
    let image = pattern(32, 32, ImageFormat::Rgba8, ImageAlphaType::Alpha);
    s.draw_image(
        &ImageBrush::new(image),
        Affine::translate((40.0, 290.0))
            * Affine::skew(0.4, 0.0)
            * Affine::scale_non_uniform(6.0, 2.5),
    );
    s
}

fn pattern(width: u32, height: u32, format: ImageFormat, alpha: ImageAlphaType) -> ImageData {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let checker = ((x / 8) + (y / 8)) % 2 == 0;
            let a = if x < width / 4 { 128u8 } else { 255 };
            let (r, g, b) = if checker {
                ((x * 255 / width) as u8, (y * 255 / height) as u8, 200)
            } else {
                (250, 240, (x * 4) as u8)
            };
            let (r, g, b) = if alpha == ImageAlphaType::AlphaPremultiplied {
                (
                    (u16::from(r) * u16::from(a) / 255) as u8,
                    (u16::from(g) * u16::from(a) / 255) as u8,
                    (u16::from(b) * u16::from(a) / 255) as u8,
                )
            } else {
                (r, g, b)
            };
            match format {
                ImageFormat::Bgra8 => data.extend_from_slice(&[b, g, r, a]),
                _ => data.extend_from_slice(&[r, g, b, a]),
            }
        }
    }
    ImageData {
        data: Blob::new(Arc::new(data)),
        format,
        alpha_type: alpha,
        width,
        height,
    }
}

fn images() -> Scene {
    let mut s = Scene::new();
    let rgba = pattern(48, 40, ImageFormat::Rgba8, ImageAlphaType::Alpha);
    let bgra = pattern(
        48,
        40,
        ImageFormat::Bgra8,
        ImageAlphaType::AlphaPremultiplied,
    );
    s.draw_image(
        &ImageBrush::new(rgba.clone()),
        Affine::translate((10.0, 10.0)),
    );
    s.draw_image(
        &ImageBrush::new(rgba.clone()).with_quality(ImageQuality::Low),
        Affine::translate((70.0, 10.0)) * Affine::scale(2.5),
    );
    s.draw_image(
        &ImageBrush::new(rgba.clone()).with_quality(ImageQuality::Medium),
        Affine::translate((200.3, 10.6)) * Affine::scale(2.5),
    );
    s.draw_image(
        &ImageBrush::new(bgra.clone()).with_alpha(0.6),
        Affine::translate((330.0, 10.0)) * Affine::scale(2.0),
    );
    s.draw_image(
        &ImageBrush::new(bgra),
        Affine::translate((90.0, 160.0)) * Affine::rotate(0.5) * Affine::scale(2.0),
    );
    let repeat = ImageBrush::new(rgba.clone())
        .with_extend(Extend::Repeat)
        .with_quality(ImageQuality::Low);
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &repeat,
        Some(Affine::translate((200.0, 150.0))),
        &RoundedRect::new(200.0, 150.0, 500.0, 260.0, 18.0),
    );
    let reflect = ImageBrush::new(rgba)
        .with_x_extend(Extend::Reflect)
        .with_y_extend(Extend::Pad)
        .with_quality(ImageQuality::Medium);
    s.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &reflect,
        Some(Affine::translate((20.0, 280.0)) * Affine::scale(1.5)),
        &Rect::new(10.0, 280.0, 500.0, 375.0),
    );
    s
}

fn blur() -> Scene {
    let mut s = Scene::new();
    let shadow = color(0, 0, 0, 150);
    let id = Affine::IDENTITY;
    s.draw_blurred_rounded_rect(id, Rect::new(30.0, 30.0, 150.0, 110.0), shadow, 8.0, 6.0);
    s.draw_blurred_rounded_rect(id, Rect::new(190.3, 30.6, 330.1, 120.2), shadow, 16.0, 12.0);
    s.draw_blurred_rounded_rect(
        id,
        Rect::new(370.0, 40.0, 480.0, 100.0),
        color(20, 80, 200, 255),
        0.0,
        2.0,
    );
    s.draw_blurred_rounded_rect(
        Affine::translate((90.0, 200.0)) * Affine::scale(2.0),
        Rect::new(-25.0, -20.0, 25.0, 20.0),
        shadow,
        6.0,
        4.0,
    );
    s.draw_blurred_rounded_rect(
        Affine::translate((260.0, 210.0)) * Affine::rotate(0.35),
        Rect::new(-60.0, -35.0, 60.0, 35.0),
        color(160, 0, 60, 200),
        10.0,
        7.0,
    );
    s.draw_blurred_rounded_rect_in(
        &Circle::new((410.0, 210.0), 60.0),
        id,
        Rect::new(360.0, 170.0, 460.0, 250.0),
        shadow,
        12.0,
        10.0,
    );
    // A window-sized shadow, as the shell draws under each window.
    s.fill(
        Fill::NonZero,
        id,
        color(240, 240, 240, 255),
        None,
        &Rect::new(0.0, 290.0, 512.0, 384.0),
    );
    s.draw_blurred_rounded_rect(
        id,
        Rect::new(40.0, 300.0, 470.0, 420.0),
        color(0, 0, 0, 90),
        12.0,
        16.0,
    );
    s.fill(
        Fill::NonZero,
        id,
        color(255, 255, 255, 255),
        None,
        &RoundedRect::new(40.0, 300.0, 470.0, 420.0, 12.0),
    );
    s
}

fn layers() -> Scene {
    let mut s = Scene::new();
    let id = Affine::IDENTITY;
    let bg = Gradient::new_linear((0.0, 0.0), (512.0, 0.0)).with_stops(
        stops(&[
            (0.0, color(250, 220, 120, 255)),
            (1.0, color(80, 160, 230, 255)),
        ])
        .as_slice(),
    );
    s.fill(
        Fill::NonZero,
        id,
        &bg,
        None,
        &Rect::new(0.0, 0.0, 512.0, 384.0),
    );

    s.push_clip_layer(Fill::NonZero, id, &Circle::new((90.0, 90.0), 70.0));
    s.push_clip_layer(Fill::NonZero, id, &Rect::new(40.0, 0.0, 200.0, 120.0));
    s.fill(
        Fill::NonZero,
        id,
        color(200, 0, 0, 255),
        None,
        &Rect::new(0.0, 0.0, 300.0, 300.0),
    );
    s.pop_layer();
    s.pop_layer();

    s.push_layer(
        Fill::NonZero,
        Mix::Normal,
        0.5,
        id,
        &Rect::new(180.0, 20.0, 330.0, 160.0),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(0, 0, 0, 255),
        None,
        &Circle::new((230.0, 70.0), 45.0),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(0, 0, 0, 255),
        None,
        &Circle::new((280.0, 110.0), 45.0),
    );
    s.pop_layer();

    let modes = [
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        BlendMode::new(Mix::Screen, Compose::SrcOver),
        BlendMode::new(Mix::Difference, Compose::SrcOver),
        BlendMode::new(Mix::Overlay, Compose::SrcOver),
        BlendMode::new(Mix::Color, Compose::SrcOver),
        BlendMode::new(Mix::Normal, Compose::SrcIn),
        BlendMode::new(Mix::Normal, Compose::DestOut),
        BlendMode::new(Mix::Normal, Compose::Xor),
    ];
    // vello's GPU pipeline applies destructive compose modes to whole
    // 16-pixel tiles of the layer, so these layers are tile-aligned.
    for (i, mode) in modes.into_iter().enumerate() {
        let x = 64.0 * i as f64;
        let area = Rect::new(x, 192.0, x + 48.0, 256.0);
        s.fill(
            Fill::NonZero,
            id,
            color(30, 120, 60, 255),
            None,
            &Rect::new(x, 200.0, x + 55.0, 225.0),
        );
        s.push_layer(Fill::NonZero, mode, 1.0, id, &area);
        s.fill(
            Fill::NonZero,
            id,
            color(220, 60, 200, 230),
            None,
            &Circle::new((x + 27.0, 230.0), 25.0),
        );
        s.pop_layer();
    }

    s.push_layer(
        &Stroke::new(14.0),
        Mix::Normal,
        1.0,
        id,
        &Circle::new((400.0, 90.0), 50.0),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(0, 0, 160, 255),
        None,
        &Rect::new(330.0, 20.0, 470.0, 160.0),
    );
    s.pop_layer();

    s.push_clip_layer(Fill::NonZero, id, &Rect::new(100.0, 300.0, 100.0, 300.0));
    s.fill(
        Fill::NonZero,
        id,
        color(255, 0, 0, 255),
        None,
        &Rect::new(0.0, 280.0, 512.0, 384.0),
    );
    s.pop_layer();

    s.push_clip_layer(
        Fill::EvenOdd,
        Affine::translate((256.0, 330.0)),
        &star(Point::ZERO, 50.0, 25.0, 5),
    );
    s.fill(
        Fill::NonZero,
        id,
        color(20, 20, 20, 255),
        None,
        &Rect::new(0.0, 270.0, 512.0, 384.0),
    );
    s.pop_layer();
    s
}

fn font(name: &str) -> FontData {
    let path = tilo_font_catalog::checkout_root().join(name);
    FontData::new(Blob::new(Arc::new(std::fs::read(path).unwrap())), 0)
}

/// Lay `text` out on one line with nominal advances.
fn line(
    font: &FontData,
    size: f32,
    coords: &[NormalizedCoord],
    text: &str,
    x: f32,
    y: f32,
) -> Vec<Glyph> {
    let font_ref = FontRef::from_index(font.data.as_ref(), font.index).unwrap();
    let charmap = font_ref.charmap();
    let skrifa_coords: Vec<_> = coords
        .iter()
        .map(|&c| skrifa::instance::NormalizedCoord::from_bits(c))
        .collect();
    let metrics = font_ref.glyph_metrics(Size::new(size), LocationRef::new(&skrifa_coords));
    let mut pen = x;
    text.chars()
        .filter_map(|ch| {
            let id = charmap.map(ch)?;
            let glyph = Glyph {
                id: id.to_u32(),
                x: pen,
                y,
            };
            pen += metrics.advance_width(id).unwrap_or(0.0);
            Some(glyph)
        })
        .collect()
}

fn text() -> Scene {
    let mut s = Scene::new();
    let sans = font("AdwaitaSans-Regular.ttf");
    let noto = font("NotoSans[wght].ttf");
    let sample = "The quick brown fox jumps over 0123456789";
    let mut y = 20.0;
    for size in [9.0f32, 11.0, 13.0, 16.0, 22.0, 32.0] {
        let glyphs = line(&sans, size, &[], sample, 8.0, y);
        s.draw_glyphs(&sans)
            .font_size(size)
            .brush(color(20, 20, 30, 255))
            .draw(Fill::NonZero, glyphs.into_iter());
        y += size * 1.4;
    }
    // A variable instance (bold) and a translucent colour.
    let bold: [NormalizedCoord; 1] = [(0.8 * 16384.0) as i16];
    let glyphs = line(&noto, 18.0, &bold, "Variable weight, alpha", 8.0, y + 6.0);
    s.draw_glyphs(&noto)
        .font_size(18.0)
        .normalized_coords(&bold)
        .brush(color(160, 20, 20, 180))
        .draw(Fill::NonZero, glyphs.into_iter());
    // Rotated run with a synthetic oblique.
    let glyphs = line(&sans, 20.0, &[], "Rotated oblique", 0.0, 0.0);
    s.draw_glyphs(&sans)
        .font_size(20.0)
        .transform(Affine::translate((330.0, 220.0)) * Affine::rotate(-0.3))
        .glyph_transform(Some(Affine::skew(-0.25, 0.0)))
        .brush(color(0, 90, 160, 255))
        .draw(Fill::NonZero, glyphs.into_iter());
    // Stroked outline glyphs.
    let glyphs = line(&sans, 40.0, &[], "Outline", 8.0, 270.0);
    s.draw_glyphs(&sans)
        .font_size(40.0)
        .brush(color(0, 0, 0, 255))
        .draw(&Stroke::new(1.2), glyphs.into_iter());
    // Gradient brush with a brush transform.
    let g = Gradient::new_linear((0.0, 0.0), (1.0, 0.0)).with_stops(
        stops(&[
            (0.0, color(255, 0, 100, 255)),
            (1.0, color(0, 80, 255, 255)),
        ])
        .as_slice(),
    );
    let glyphs = line(&noto, 36.0, &[], "Gradient", 200.0, 275.0);
    s.draw_glyphs(&noto)
        .font_size(36.0)
        .brush(&g)
        .brush_transform(Some(Affine::translate((200.0, 0.0)) * Affine::scale(160.0)))
        .draw(Fill::NonZero, glyphs.into_iter());
    // Hinted, at a scale the resolver folds into the size.
    let glyphs = line(&sans, 12.0, &[], "Hinted at scale 1.5", 0.0, 0.0);
    s.draw_glyphs(&sans)
        .font_size(12.0)
        .hint(true)
        .transform(Affine::translate((8.0, 320.0)) * Affine::scale(1.5))
        .brush(color(0, 0, 0, 255))
        .draw(Fill::NonZero, glyphs.into_iter());
    // Synthetic bold.
    let glyphs = line(&sans, 20.0, &[], "Emboldened", 8.0, 365.0);
    s.draw_glyphs(&sans)
        .font_size(20.0)
        .font_embolden(vello::FontEmbolden::new(vello::kurbo::Diagonal2::new(
            0.6, 0.6,
        )))
        .brush(color(0, 100, 0, 255))
        .draw(Fill::NonZero, glyphs.into_iter());
    s
}

fn emoji() -> Scene {
    let mut s = Scene::new();
    let colr = font("Noto-COLRv1.ttf");
    let text = "😀🎨🌈🔥🍕🚀💡🧪🐙🌻🎉❤️";
    let mut y = 70.0;
    for size in [48.0f32, 28.0, 64.0] {
        let glyphs = line(&colr, size, &[], text, 8.0, y);
        s.draw_glyphs(&colr)
            .font_size(size)
            .brush(color(0, 0, 0, 255))
            .draw(Fill::NonZero, glyphs.into_iter());
        y += size * 1.6;
    }
    s
}

/// What a scene may differ by. Two anti-aliasing implementations disagree on
/// edge pixels, mostly by a few levels; hard colour seams (a repeating
/// gradient's wrap, a stop pair at one offset) differ fully on the one pixel
/// each side samples differently. Neither covers more than a sliver of a
/// scene, while a misread stream or wrong transform changes whole areas.
const MIN_PSNR: f64 = 33.0;
const MAX_OVER_8: f64 = 0.02;
const MAX_OVER_32: f64 = 0.005;

fn check(name: &str, diff: Diff, failures: &mut Vec<String>) {
    println!("{name:12} {diff}");
    if diff.psnr < MIN_PSNR
        || diff.over_8_ratio() > MAX_OVER_8
        || diff.over_32_ratio() > MAX_OVER_32
    {
        failures.push(format!("{name}: {diff}"));
    }
}

/// A named scene and the base colour it renders over.
type Case = (&'static str, fn() -> Scene, Color);

#[test]
fn every_feature_matches_vello_gpu() {
    let Some(mut gpu) = support::Gpu::new() else {
        eprintln!("no wgpu adapter: skipping the GPU comparison");
        return;
    };
    println!("reference adapter: {}", gpu.adapter());
    let mut raster = support::new_raster();
    let white = color(255, 255, 255, 255);
    let scenes: [Case; 11] = [
        ("fills", fills, white),
        ("strokes", strokes, white),
        ("linear", linear, white),
        ("radial", radial, white),
        ("sweep", sweep, white),
        ("skewed", skewed, white),
        ("images", images, color(30, 30, 30, 255)),
        ("blur", blur, white),
        ("layers", layers, white),
        ("text", text, white),
        ("emoji", emoji, white),
    ];
    let mut failures = Vec::new();
    for (name, build, base) in scenes {
        let scene = build();
        let diff = support::parity(&mut gpu, &mut raster, name, &scene, W, H, base);
        check(name, diff, &mut failures);
    }
    // Transparent base: the alpha channel itself must match.
    let scene = layers();
    let diff = support::parity(
        &mut gpu,
        &mut raster,
        "transparent",
        &scene,
        W,
        H,
        Color::TRANSPARENT,
    );
    check("transparent", diff, &mut failures);
    assert!(
        failures.is_empty(),
        "outside the bound:\n{}",
        failures.join("\n")
    );
}

#[test]
fn cached_blur_matches_per_pixel_blur() {
    let scene = blur();
    let mut cached = support::new_raster();
    let mut direct = tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options {
        threads: 0,
        cached_blur: false,
    });
    let white = color(255, 255, 255, 255);
    let a = support::cpu_render(&mut cached, &scene, W, H, white, None);
    let b = support::cpu_render(&mut direct, &scene, W, H, white, None);
    support::write_diff_png("blur-cached-vs-direct.diff.png", W, H, &a, &b);
    let diff = compare(&a, &b);
    println!("cached vs direct blur: {diff}");
    assert!(diff.max <= 12 && diff.psnr > 45.0, "{diff}");
}

#[test]
fn damage_regions_match_the_full_frame() {
    let white = color(255, 255, 255, 255);
    let scenes = [layers(), text(), blur(), images()];
    for scene in &scenes {
        let mut raster = support::new_raster();
        let full = support::cpu_render(&mut raster, scene, W, H, white, None);
        // A wider stride than the image, prefilled with something else.
        let stride = (W as usize + 13) * 4;
        let mut buffer = vec![77u8; stride * H as usize];
        let partial = [
            Region::new(3, 5, 101, 77),
            Region::new(250, 190, 200, 120),
            Region::new(500, 370, 40, 40),
        ];
        {
            let mut target = tilo_cpu_raster::Target::new(&mut buffer, W, H, stride).unwrap();
            raster.render(scene.encoding(), &mut target, Some(&partial), white);
        }
        for y in 0..H as usize {
            for x in 0..W as usize {
                let inside = partial.iter().any(|r| {
                    x >= r.x as usize
                        && x < (r.x + r.width) as usize
                        && y >= r.y as usize
                        && y < (r.y + r.height) as usize
                });
                let got = &buffer[y * stride + x * 4..y * stride + x * 4 + 4];
                if inside {
                    let want = &full[(y * W as usize + x) * 4..(y * W as usize + x) * 4 + 4];
                    let d = got
                        .iter()
                        .zip(want)
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap_or(0);
                    assert!(d <= 2, "pixel {x},{y}: {got:?} vs {want:?}");
                } else {
                    assert_eq!(
                        got,
                        [77, 77, 77, 77],
                        "pixel {x},{y} outside the damage changed"
                    );
                }
            }
            // The padding past the row is never written.
            assert!(
                buffer[y * stride + W as usize * 4..(y + 1) * stride]
                    .iter()
                    .all(|&b| b == 77)
            );
        }
    }
}

#[test]
fn captured_scenes_render_identically() {
    let white = color(255, 255, 255, 255);
    for scene in [text(), images(), linear(), layers(), emoji()] {
        let mut bytes = Vec::new();
        tilo_cpu_raster::capture::write(scene.encoding(), &mut bytes).unwrap();
        let replayed = tilo_cpu_raster::capture::read(bytes.as_slice()).unwrap();
        let mut raster = support::new_raster();
        let a = support::cpu_render(&mut raster, &scene, W, H, white, None);
        let mut b = vec![0u8; a.len()];
        let mut target = tilo_cpu_raster::Target::new(&mut b, W, H, W as usize * 4).unwrap();
        support::new_raster().render(&replayed, &mut target, None, white);
        assert_eq!(compare(&a, &b).max, 0);
    }
}

/// Every pixel a scene paints lies inside [`tilo_cpu_raster::DrawBounds`]:
/// fills, strokes, gradients, images, blurs, blending layers and glyphs.
#[test]
fn draw_bounds_hold_every_painted_pixel() {
    type Built = (&'static str, fn() -> Scene);
    let mut bounds = tilo_cpu_raster::DrawBounds::default();
    let scenes: [Built; 11] = [
        ("fills", fills),
        ("strokes", strokes),
        ("linear", linear),
        ("radial", radial),
        ("sweep", sweep),
        ("skewed", skewed),
        ("images", images),
        ("blur", blur),
        ("layers", layers),
        ("text", text),
        ("emoji", emoji),
    ];
    // Two grounds, so a pixel a draw leaves at one ground's colour is still
    // seen on the other.
    for base in [color(255, 255, 255, 255), color(0, 0, 0, 255)] {
        for (name, build) in scenes {
            let scene = build();
            let rects = bounds
                .of(scene.encoding())
                .unwrap_or_else(|| panic!("{name}: every draw is bounded"));
            let mut raster = support::new_raster();
            let painted = support::cpu_render(&mut raster, &scene, W, H, base, None);
            let ground = support::cpu_render(&mut raster, &Scene::new(), W, H, base, None);
            let mut outside = 0;
            for y in 0..H as usize {
                for x in 0..W as usize {
                    let at = (y * W as usize + x) * 4;
                    if painted[at..at + 4] == ground[at..at + 4] {
                        continue;
                    }
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let held = rects.iter().any(|rect| {
                        let rect = rect.inflate(1.0, 1.0);
                        px >= rect.x0 && px <= rect.x1 && py >= rect.y0 && py <= rect.y1
                    });
                    if !held {
                        outside += 1;
                    }
                }
            }
            assert_eq!(outside, 0, "{name}: painted pixels outside the draw bounds");
        }
    }
}

#[test]
fn only_plain_clips_and_opaque_groups_leave_a_draw_unblended() {
    let open = |blend: Option<(BlendMode, f32)>| {
        let mut scene = Scene::new();
        let shape = Rect::new(8.0, 8.0, 56.0, 56.0);
        match blend {
            Some((blend, alpha)) => {
                scene.push_layer(Fill::NonZero, blend, alpha, Affine::IDENTITY, &shape);
            }
            None => scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &shape),
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color(200, 100, 50, 255),
            None,
            &Rect::new(0.0, 0.0, 64.0, 64.0),
        );
        scene
    };
    let normal = BlendMode::new(Mix::Normal, Compose::SrcOver);
    let multiply = BlendMode::new(Mix::Multiply, Compose::SrcOver);
    let plain = |scene: &Scene, draws: usize| {
        tilo_cpu_raster::plain_layers_open_at(scene.encoding(), draws)
    };
    // After the layer opened (its one draw tag), inside it.
    assert!(plain(&open(None), 1), "a clip");
    assert!(plain(&open(Some((normal, 1.0))), 1), "an opaque group");
    assert!(!plain(&open(Some((normal, 0.5))), 1), "a translucent group");
    assert!(!plain(&open(Some((multiply, 1.0))), 1), "a multiply layer");
    // Before it opened, nothing is open.
    assert!(plain(&open(Some((multiply, 1.0))), 0));
    // Closed again, it is no longer open.
    let mut closed = open(Some((multiply, 1.0)));
    closed.pop_layer();
    let draws = closed.encoding().draw_tags.len();
    assert!(plain(&closed, draws));
    // Past the end is refused.
    assert!(!plain(&closed, draws + 1));
}
