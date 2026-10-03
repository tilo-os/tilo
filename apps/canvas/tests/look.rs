//! Looking at an artwork: what an agent sees after it paints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::{ArtworkResource, view_ops};
use ui::prelude::{Invoke, Replica};

fn landscape() -> Replica<ArtworkResource> {
    let mut art = Replica::<ArtworkResource>::new();
    for action in canvas::sample::landscape() {
        art.apply(Invoke::apply(&action).unwrap()).unwrap();
    }
    art
}

fn out(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/canvas-look");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A look renders the artwork the agent painted, at the size asked, and a
/// close look at a region magnifies it. Looks are JPEGs unless a caller
/// asks for exact pixels.
#[test]
fn a_look_shows_the_painting_whole_and_up_close() {
    let art = landscape().state().unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let (whole, info) = view_ops::look(&mut renderer, &mut text, &art, &Look::default()).unwrap();
    assert_eq!((whole.width, whole.height), (768, 576), "{info}");
    assert_eq!(info["format"], "jpeg");
    assert!(whole.bytes.starts_with(&[0xff, 0xd8, 0xff]));
    std::fs::write(out("whole.jpg"), &whole.bytes).unwrap();

    let exact = Look {
        format: Some("png".into()),
        ..Look::default()
    };
    let (png, _) = view_ops::look(&mut renderer, &mut text, &art, &exact).unwrap();
    assert!(png.bytes.starts_with(b"\x89PNG"));
    assert!(
        whole.bytes.len() * 3 < png.bytes.len(),
        "a JPEG look is a fraction of the PNG: {} against {} bytes",
        whole.bytes.len(),
        png.bytes.len()
    );
    let unknown = Look {
        format: Some("gif".into()),
        ..Look::default()
    };
    let error = view_ops::look(&mut renderer, &mut text, &art, &unknown)
        .err()
        .unwrap();
    assert!(error.contains("jpeg or png"), "{error}");

    let close = Look {
        region: Some([700.0, 100.0, 320.0, 240.0]),
        size: Some(640),
        grid: Some(true),
        ..Look::default()
    };
    let (detail, info) = view_ops::look(&mut renderer, &mut text, &art, &close).unwrap();
    assert_eq!((detail.width, detail.height), (640, 480));
    assert_eq!(info["pixels_per_unit"], 2.0);
    std::fs::write(out("detail.jpg"), &detail.bytes).unwrap();

    let gridded = Look {
        grid: Some(true),
        ..Look::default()
    };
    let (grid, _) = view_ops::look(&mut renderer, &mut text, &art, &gridded).unwrap();
    std::fs::write(out("grid.jpg"), &grid.bytes).unwrap();

    let outside = Look {
        region: Some([5000.0, 5000.0, 10.0, 10.0]),
        ..Look::default()
    };
    let error = view_ops::look(&mut renderer, &mut text, &art, &outside)
        .err()
        .unwrap();
    assert!(error.contains("outside the artwork"), "{error}");
}

/// A painted fill whose gradient starts clear fades its paint in across
/// the mark: the stops' alpha thins the paint, it is not the whole mark's.
#[test]
fn a_painted_gradient_that_starts_clear_fades_in() {
    use canvas::CanvasAction;
    use canvas::actions::{AddLayer, Draw, NewMark};
    use canvas::model::{Gradient, Shape};
    use ui::vello::kurbo::Rect;

    let mut replica = Replica::<ArtworkResource>::new();
    let layer = CanvasAction::AddLayer(AddLayer {
        id: Some("mist".into()),
        name: "mist".into(),
        medium: Some("oil".into()),
        ..AddLayer::default()
    });
    let veil = NewMark {
        gradient: Some(Gradient {
            kind: "linear".into(),
            from: [0.0, 0.0],
            to: [400.0, 0.0],
            stops: vec![(0.0, "#20304000".into()), (1.0, "#203040".into())],
            focal: None,
        }),
        brush_size: Some(20.0),
        ..NewMark::new(Shape::Rect {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 300.0,
            radius: 0.0,
        })
    };
    let draw = CanvasAction::Draw(Draw {
        layer: Some("mist".into()),
        marks: vec![veil],
    });
    for action in [layer, draw] {
        replica.apply(Invoke::apply(&action).unwrap()).unwrap();
    }
    let art = replica.state().unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let options = canvas::render::Options {
        bare: true,
        ..canvas::render::Options::default()
    };
    let (rgba, width, _) =
        view_ops::pixels(Rect::new(0.0, 0.0, 400.0, 300.0), 1.0, true, |scene, to| {
            renderer.draw(&art, scene, to, &mut text, &options);
        })
        .unwrap();
    let width = usize::try_from(width).unwrap();
    let cover = |x: usize| {
        (100..200)
            .map(|y| u32::from(rgba[(y * width + x) * 4 + 3]))
            .sum::<u32>()
            / 100
    };
    assert!(cover(20) < 60, "clear where it starts: {}", cover(20));
    assert!(cover(380) > 200, "opaque where it ends: {}", cover(380));
}

/// Text stands on the baseline its mark names, whatever its size and
/// leading: letters without descenders end on `y`.
#[test]
fn text_stands_on_its_baseline() {
    use canvas::CanvasAction;
    use canvas::actions::{Draw, NewMark};
    use canvas::model::Shape;
    use ui::vello::kurbo::Rect;

    for (size, line_height) in [(60.0, None), (24.0, Some(2.0)), (120.0, Some(0.9))] {
        let mut replica = Replica::<ArtworkResource>::new();
        let mark = NewMark {
            fill: Some("#000000".into()),
            ..NewMark::new(Shape::Text {
                x: 20.0,
                y: 200.0,
                text: "HIEH".into(),
                size,
                font: None,
                weight: None,
                italic: false,
                align: None,
                line_height,
                width: None,
                tracking: None,
                path: None,
            })
        };
        let draw = CanvasAction::Draw(Draw {
            layer: None,
            marks: vec![mark],
        });
        replica.apply(Invoke::apply(&draw).unwrap()).unwrap();
        let art = replica.state().unwrap();
        let mut renderer = Renderer::default();
        let mut text = ui::TextRenderer::new();
        let options = canvas::render::Options {
            bare: true,
            ..canvas::render::Options::default()
        };
        let (rgba, width, _) =
            view_ops::pixels(Rect::new(0.0, 0.0, 400.0, 300.0), 1.0, true, |scene, to| {
                renderer.draw(&art, scene, to, &mut text, &options);
            })
            .unwrap();
        let inked: Vec<usize> = rgba
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, pixel)| pixel[3] > 128)
            .map(|(index, _)| index / usize::try_from(width).unwrap())
            .collect();
        let bottom = *inked.iter().max().expect("the text is drawn");
        let top = *inked.iter().min().unwrap();
        assert!(
            (199..=201).contains(&bottom),
            "size {size}: letters end at row {bottom}, not on the baseline at 200"
        );
        let cap = 200.0 - f64::from(u32::try_from(top).unwrap());
        assert!(
            cap > size * 0.6 && cap < size * 0.8,
            "size {size}: capitals {cap} tall"
        );
    }
}
