//! Does a `DestIn` mask layer cost more than a plain clip at viewport size?
//!
//! The wheel disc wants one — masking the whole disc with a vertical fade
//! makes the body a single smooth gradient instead of concentric strokes —
//! but whether it can have one depends on vello, which sizes a per-tile
//! blend buffer for everything under a blend layer. Times both shapes at
//! the compositor's actual run size.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Instant;

use crate::support::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{BlendMode, Color, ColorStop, Compose, Extend, Fill, Gradient, Mix};

const W: u32 = 1600;
const H: u32 = 900;

fn fade() -> Gradient {
    Gradient::new_linear(Point::new(0.0, 0.0), Point::new(0.0, f64::from(H)))
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: Color::from_rgba8(255, 255, 255, 0).into(),
                },
                ColorStop {
                    offset: 0.25,
                    color: Color::WHITE.into(),
                },
                ColorStop {
                    offset: 0.75,
                    color: Color::WHITE.into(),
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::from_rgba8(255, 255, 255, 0).into(),
                },
            ][..],
        )
}

/// The disc's footprint: a narrow column on the left, full height.
fn footprint() -> Rect {
    Rect::new(0.0, 0.0, 260.0, f64::from(H))
}

fn body(scene: &mut Scene) {
    let grad = Gradient::new_radial((-470.0, 450.0), 712.0)
        .with_extend(Extend::Pad)
        .with_stops(
            &[
                ColorStop {
                    offset: 0.0,
                    color: Color::from_rgba8(13, 14, 17, 0).into(),
                },
                ColorStop {
                    offset: 0.7,
                    color: Color::from_rgba8(13, 14, 17, 0).into(),
                },
                ColorStop {
                    offset: 1.0,
                    color: Color::from_rgba8(13, 14, 17, 168).into(),
                },
            ][..],
        );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &grad,
        None,
        &Circle::new((-470.0, 450.0), 712.0),
    );
}

#[test]
fn dest_in_mask_layer_is_affordable_at_viewport_size() {
    let mut renderer = OffscreenRenderer::new(W, H);

    // Control: plain clip layer, no blending.
    let mut plain = Scene::new();
    plain.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &footprint(),
    );
    body(&mut plain);
    plain.pop_layer();

    // Subject: same content, plus a DestIn mask over the same area.
    let mut masked = Scene::new();
    masked.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &footprint(),
    );
    body(&mut masked);
    masked.push_layer(
        Fill::NonZero,
        BlendMode::new(Mix::Normal, Compose::DestIn),
        1.0,
        Affine::IDENTITY,
        &footprint(),
    );
    masked.fill(Fill::NonZero, Affine::IDENTITY, &fade(), None, &footprint());
    masked.pop_layer();
    masked.pop_layer();

    // Each shape's first render builds its own pipelines and blend buffers;
    // warm both so neither's one-time setup is attributed to it, then take
    // each one's best frame so a loaded machine's stalls are not either.
    let _ = renderer.render(&plain, Color::BLACK);
    let _ = renderer.render(&masked, Color::BLACK);
    let mut best = |scene: &Scene| {
        (0..3)
            .map(|_| {
                let started = Instant::now();
                let _ = renderer.render(scene, Color::BLACK);
                started.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::INFINITY, f64::min)
    };
    let plain_ms = best(&plain);
    let masked_ms = best(&masked);

    eprintln!("plain clip: {plain_ms:.1} ms    DestIn mask: {masked_ms:.1} ms");
    assert!(
        masked_ms < plain_ms.max(50.0) * 8.0,
        "a DestIn mask over the disc's footprint costs {masked_ms:.1} ms against \
         {plain_ms:.1} ms for the same content under a plain clip — the wheel cannot \
         afford one, and its body has to stay a stack of concentric strokes"
    );
}
