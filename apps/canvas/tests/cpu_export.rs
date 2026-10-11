//! An export where no GPU renderer starts (a confined agent worker): the
//! artwork is drawn on the CPU and comes out as the GPU would draw it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use canvas::render::{Options, Renderer};
use canvas::resource::Export;
use canvas::view_ops;
use serde_json::{Value, json};
use ui::prelude::TestApp;
use ui::vello::Scene;
use ui::vello::kurbo::Affine;
use ui::vello::peniko::Color;

const WIDTH: u32 = 1520;
const HEIGHT: u32 = 855;
/// The rectangle, `[x, y, width, height]` in artwork units.
const RECT: [u32; 4] = [300, 200, 640, 360];
const TEAL: [u8; 4] = [0x0b, 0x55, 0x63, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

fn call(app: &mut TestApp<App>, action: &str, args: Value) {
    app.call(action, args)
        .unwrap_or_else(|error| panic!("{action}: {error}"));
}

fn near(a: [u8; 4], b: [u8; 4], within: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= within)
}

/// A white 1520x855 artwork with a deep-teal rectangle on part of it.
fn artwork() -> TestApp<App> {
    let mut app = TestApp::<App>::new().unwrap();
    call(
        &mut app,
        "set-artwork",
        json!({"width": WIDTH, "height": HEIGHT, "texture": "smooth", "ground_colour": "#ffffff"}),
    );
    let [x, y, width, height] = RECT;
    call(
        &mut app,
        "draw",
        json!({"marks": [{"id": "block", "fill": "#0b5563",
            "shape": {"kind": "rect", "x": x, "y": y, "width": width, "height": height}}]}),
    );
    app
}

/// What the GPU draws of the export, straight RGBA, when there is a GPU.
fn gpu_reference(app: &TestApp<App>) -> Option<Vec<u8>> {
    let mut offscreen = ui::offscreen::OffscreenRenderer::try_new(WIDTH, HEIGHT).ok()?;
    let art = app.state();
    let mut renderer = Renderer::default();
    renderer.pictures.read_from(app.content());
    let options = Options {
        detail: Some(canvas::detail::Detail {
            visible: art.bounds(),
            pixels_per_unit: 1.0,
            wait: true,
        }),
        ..Options::default()
    };
    let mut scene = Scene::new();
    renderer.draw(
        &art,
        &mut scene,
        Affine::IDENTITY,
        &mut ui::TextRenderer::new(),
        &options,
    );
    Some(
        offscreen
            .try_render_resilient(&scene, Color::from_rgba8(120, 120, 120, 255))
            .unwrap(),
    )
}

/// One test, so the GPU reference is drawn before the GPU is disabled for
/// the rest of this process.
#[test]
fn an_export_without_a_gpu_is_drawn_on_the_cpu() {
    let app = artwork();
    let reference = gpu_reference(&app);

    ui::offscreen::disable_gpu();
    assert!(
        ui::offscreen::OffscreenRenderer::try_new(WIDTH, HEIGHT).is_err(),
        "a disabled GPU builds no renderer"
    );
    let art = app.state();
    let mut renderer = Renderer::default();
    renderer.pictures.read_from(app.content());
    let (picture, info) = view_ops::export(
        &mut renderer,
        &mut ui::TextRenderer::new(),
        &art,
        &Export::default(),
    )
    .unwrap();
    assert_eq!(picture.format, "png");
    assert!(picture.bytes.starts_with(b"\x89PNG"));
    assert_eq!(
        (info["width"].clone(), info["height"].clone()),
        (json!(WIDTH), json!(HEIGHT))
    );
    let decoded = image::load_from_memory(&picture.bytes).unwrap().to_rgba8();
    assert_eq!(decoded.dimensions(), (WIDTH, HEIGHT));

    let [x, y, width, height] = RECT;
    let pixel = |px: u32, py: u32| decoded.get_pixel(px, py).0;
    for (px, py) in [
        (x + 2, y + 2),
        (x + width / 2, y + height / 2),
        (x + width - 3, y + height - 3),
    ] {
        assert!(
            near(pixel(px, py), TEAL, 3),
            "inside at ({px}, {py}): {:?}",
            pixel(px, py)
        );
    }
    for (px, py) in [
        (0, 0),
        (x - 3, y + height / 2),
        (x + width + 2, y + height / 2),
        (x + width / 2, y + height + 2),
        (WIDTH - 1, HEIGHT - 1),
    ] {
        assert!(
            near(pixel(px, py), WHITE, 3),
            "outside at ({px}, {py}): {:?}",
            pixel(px, py)
        );
    }

    // Where there is a GPU, the CPU draws what it draws, but for
    // antialiasing along edges.
    if let Some(reference) = reference {
        let cpu = decoded.as_raw();
        assert_eq!(cpu.len(), reference.len());
        let differing = cpu
            .chunks_exact(4)
            .zip(reference.chunks_exact(4))
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(p, q)| p.abs_diff(*q) > 8))
            .count();
        let ratio = differing as f64 / f64::from(WIDTH * HEIGHT);
        assert!(ratio < 0.001, "{differing} pixels differ from the GPU's");
    }
}
