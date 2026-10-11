//! Looking at an artwork: what an agent sees after it paints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::{Artwork, view_ops};
use ui::prelude::Replica;

fn landscape() -> Replica<Artwork> {
    let mut art = Replica::<Artwork>::new();
    for action in canvas::sample::landscape() {
        art.apply(action).unwrap();
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
    use canvas::actions::{AddLayer, Draw, NewMark};
    use canvas::model::{Gradient, Shape};
    use ui::vello::kurbo::Rect;

    let mut replica = Replica::<Artwork>::new();
    let layer = Artwork::ADD_LAYER.with(AddLayer {
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
    let draw = Artwork::DRAW.with(Draw {
        layer: Some("mist".into()),
        marks: vec![veil],
    });
    for action in [layer, draw] {
        replica.apply(action).unwrap();
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
    use canvas::actions::{Draw, NewMark};
    use canvas::model::Shape;
    use ui::vello::kurbo::Rect;

    for (size, line_height) in [(60.0, None), (24.0, Some(2.0)), (120.0, Some(0.9))] {
        let mut replica = Replica::<Artwork>::new();
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
        let draw = Artwork::DRAW.with(Draw {
            layer: None,
            marks: vec![mark],
        });
        replica.apply(draw).unwrap();
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

/// How much neighbouring pixels differ across a picture: how much fine
/// detail it holds.
fn detail_in(rgba: &[u8], width: usize) -> f64 {
    let grey = |index: usize| -> f64 {
        let pixel = &rgba[index * 4..index * 4 + 3];
        f64::from(pixel[0]) * 0.3 + f64::from(pixel[1]) * 0.59 + f64::from(pixel[2]) * 0.11
    };
    let count = rgba.len() / 4;
    let mut sum = 0.0;
    for index in 0..count {
        if index % width + 1 < width {
            sum += (grey(index + 1) - grey(index)).abs();
        }
        if index + width < count {
            sum += (grey(index + width) - grey(index)).abs();
        }
    }
    sum / count as f64
}

/// A close look at painted layers simulates the part shown finer than the
/// whole layer, from the marks, rather than enlarging the coarser picture:
/// the brushwork and the tooth of the ground hold detail at every pixel, the
/// same on every run.
#[test]
fn a_close_look_simulates_the_paint_finer_rather_than_enlarging_it() {
    let art = landscape().state().unwrap();
    let mut text = ui::TextRenderer::new();
    let region = ui::vello::kurbo::Rect::new(380.0, 560.0, 540.0, 680.0);
    let options = canvas::render::Options::default();
    let pixels = |renderer: &mut Renderer, text: &mut ui::TextRenderer, close: bool| {
        let options = canvas::render::Options {
            detail: Some(canvas::detail::Detail {
                visible: region,
                pixels_per_unit: if close { 4.0 } else { 1.0 },
                wait: true,
            }),
            ..options.clone()
        };
        view_ops::render(renderer, text, &art, (region, 4.0), &options, false).unwrap()
    };
    let sharp = pixels(&mut Renderer::default(), &mut text, true);
    let again = pixels(&mut Renderer::default(), &mut text, true);
    assert_eq!(
        sharp.bytes, again.bytes,
        "the finer simulation is deterministic"
    );
    let soft = pixels(&mut Renderer::default(), &mut text, false);
    std::fs::write(out("close-sharp.png"), &sharp.bytes).unwrap();
    std::fs::write(out("close-soft.png"), &soft.bytes).unwrap();
    let decode = |bytes: &[u8]| {
        image::load_from_memory(bytes)
            .unwrap()
            .to_rgba8()
            .into_raw()
    };
    let (sharp, soft) = (decode(&sharp.bytes), decode(&soft.bytes));
    let (sharp, soft) = (detail_in(&sharp, 640), detail_in(&soft, 640));
    assert!(
        sharp > soft * 1.5,
        "the finer simulation holds more detail: {sharp:.2} against {soft:.2} enlarged"
    );
}

/// The finer tiles of a painted layer meet without a seam: the step
/// between the columns either side of a tile's edge is no larger than
/// between any two neighbouring columns.
#[test]
fn finer_tiles_meet_without_a_seam() {
    let art = landscape().state().unwrap();
    let mut text = ui::TextRenderer::new();
    // Eight pixels a unit: tiles of 256 cells are 32 units wide, so their
    // edges fall at 256 and 512 pixels into this region.
    let region = ui::vello::kurbo::Rect::new(384.0, 560.0, 480.0, 632.0);
    let picture = view_ops::render(
        &mut Renderer::default(),
        &mut text,
        &art,
        (region, 8.0),
        &canvas::render::Options::default(),
        false,
    )
    .unwrap();
    std::fs::write(out("tiles.png"), &picture.bytes).unwrap();
    let rgba = image::load_from_memory(&picture.bytes).unwrap().to_rgba8();
    let (width, height) = rgba.dimensions();
    let step = |column: u32| -> f64 {
        (0..height)
            .map(|row| {
                let a = rgba.get_pixel(column - 1, row);
                let b = rgba.get_pixel(column, row);
                (0..3)
                    .map(|channel| f64::from(a[channel].abs_diff(b[channel])))
                    .sum::<f64>()
            })
            .sum::<f64>()
            / f64::from(height)
    };
    let typical = (1..width).map(step).sum::<f64>() / f64::from(width - 1);
    for edge in [256, 512] {
        assert!(
            step(edge) < typical * 2.5 + 1.0,
            "a seam at column {edge}: {:.2} against {typical:.2} between neighbours",
            step(edge)
        );
    }
}
