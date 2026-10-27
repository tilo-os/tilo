//! Brush presets, erasers and strokes edited after they are laid, as an
//! agent uses them and as they look.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "test assertions over exact, hand-written fixtures"
)]

use std::collections::BTreeMap;
use ui::tilo_resource::Invoke;

use canvas::actions::{AddLayer, EditMarks, NewStroke, Paint, RemoveMarks};
use canvas::model::Shape;
use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::{Artwork, view_ops};
use ui::prelude::Replica;

#[test]
fn choosing_pixel_starts_at_one_artwork_cell_on_every_canvas_size() {
    let tools = canvas::tools::Tools::default();
    for extent in [32.0, 64.0, 1200.0] {
        tools.extent.set(extent);
        tools.size.set(0.8);
        tools.choose_preset(canvas::brushes::Preset::Pixel);
        assert_eq!(tools.brush_size(), 1.0);
        tools.size.set(0.8);
        tools.choose_preset(canvas::brushes::Preset::PixelEraser);
        assert_eq!(tools.brush_size(), 1.0);
    }
}

fn apply(art: &mut Replica<Artwork>, action: Invoke<Artwork>) -> Result<(), String> {
    art.apply(action)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn line(id: &str, from: [f64; 2], to: [f64; 2], pressure: f64) -> NewStroke {
    let points = (0..=10)
        .map(|step| {
            let t = f64::from(step) / 10.0;
            vec![
                from[0] + (to[0] - from[0]) * t,
                from[1] + (to[1] - from[1]) * t,
                pressure,
            ]
        })
        .collect();
    NewStroke {
        id: Some(id.into()),
        points,
        size: 16.0,
        colour: "#1d1b19".into(),
        ..NewStroke::default()
    }
}

fn brushed(stroke: NewStroke, brush: &str) -> NewStroke {
    NewStroke {
        brush: Some(brush.into()),
        ..stroke
    }
}

fn paint(art: &mut Replica<Artwork>, layer: Option<&str>, strokes: Vec<NewStroke>) {
    apply(
        art,
        Artwork::PAINT.with(Paint {
            layer: layer.map(Into::into),
            strokes,
            group: None,
        }),
    )
    .unwrap();
}

/// The artwork as exact pixels, a unit to a pixel.
fn pixels(art: &Replica<Artwork>, name: &str) -> image::RgbaImage {
    let state = art.state().unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let longest = state.width.max(state.height) as u32;
    let look = Look {
        format: Some("png".into()),
        size: Some(longest),
        ..Look::default()
    };
    let (picture, _) = view_ops::look(&mut renderer, &mut text, &state, &look).unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/canvas-brushes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), &picture.bytes).unwrap();
    image::load_from_memory(&picture.bytes).unwrap().to_rgba8()
}

/// How dark the picture is at `[x, y]`, `0` the paper's lightness to `1`.
fn darkness(picture: &image::RgbaImage, [x, y]: [u32; 2]) -> f64 {
    let pixel = picture.get_pixel(x, y);
    let light = (f64::from(pixel[0]) + f64::from(pixel[1]) + f64::from(pixel[2])) / (3.0 * 255.0);
    (0.93 - light).max(0.0) / 0.93
}

fn sheet(width: f64, height: f64) -> Replica<Artwork> {
    let mut art = Replica::<Artwork>::new();
    apply(
        &mut art,
        Artwork::SET_ARTWORK.with(canvas::actions::SetArtwork {
            width: Some(width),
            height: Some(height),
            ..Default::default()
        }),
    )
    .unwrap();
    art
}

/// Every preset lays its stroke from the same mark, and pressure means
/// what it should: a pen's line swells, a pencil's darkens.
#[test]
fn presets_draw_from_the_stroke_mark() {
    let mut art = sheet(400.0, 360.0);
    let presets = [
        "round",
        "pen",
        "pencil",
        "marker",
        "airbrush",
        "charcoal",
        "calligraphy",
    ];
    let strokes = presets
        .iter()
        .enumerate()
        .flat_map(|(row, brush)| {
            let y = 30.0 + 45.0 * row as f64;
            [
                brushed(
                    line(&format!("{brush}-light"), [30.0, y], [180.0, y], 0.2),
                    brush,
                ),
                brushed(
                    line(&format!("{brush}-firm"), [220.0, y], [370.0, y], 1.0),
                    brush,
                ),
            ]
        })
        .collect();
    paint(&mut art, None, strokes);
    let state = art.state().unwrap();
    assert_eq!(
        state.mark("pencil-firm").unwrap().style.brush.as_deref(),
        Some("pencil")
    );
    assert_eq!(
        state.mark("round-firm").unwrap().style.brush,
        None,
        "the round brush is no preset at all"
    );
    let picture = pixels(&art, "presets.png");
    for (row, brush) in presets.iter().enumerate() {
        let y = (30.0 + 45.0 * row as f64) as u32;
        let light = darkness(&picture, [105, y]);
        let firm = darkness(&picture, [295, y]);
        assert!(firm > 0.15, "{brush} lays paint: {firm}");
        if *brush == "pencil" {
            assert!(
                firm > light * 1.8,
                "pencil pressure darkens: {light} vs {firm}"
            );
        }
    }
    // A pen's light line is thin: a few units off its centre is paper.
    let pen = (30.0 + 45.0) as u32;
    assert!(darkness(&picture, [105, pen + 5]) < 0.1);
    assert!(darkness(&picture, [295, pen + 5]) > 0.3);
}

/// A pixel eraser is a mark that clears the paint beneath it on its layer,
/// whatever the medium; paint laid after it shows.
#[test]
fn an_eraser_clears_what_is_under_it_on_its_layer() {
    let mut art = sheet(300.0, 200.0);
    apply(
        &mut art,
        Artwork::ADD_LAYER.with(AddLayer {
            id: Some("oil".into()),
            name: "Oil".into(),
            medium: Some("oil".into()),
            ..AddLayer::default()
        }),
    )
    .unwrap();
    paint(
        &mut art,
        Some("layer-1"),
        vec![brushed(
            line("ink", [20.0, 60.0], [280.0, 60.0], 1.0),
            "pen",
        )],
    );
    paint(
        &mut art,
        Some("oil"),
        vec![NewStroke {
            colour: "pigment:ultramarine".into(),
            size: 24.0,
            ..line("paint", [20.0, 140.0], [280.0, 140.0], 1.0)
        }],
    );
    let eraser = |id: &str, x: f64| NewStroke {
        id: Some(id.into()),
        points: vec![vec![x, 20.0], vec![x, 180.0]],
        size: 30.0,
        brush: Some("eraser".into()),
        ..NewStroke::default()
    };
    paint(&mut art, Some("layer-1"), vec![eraser("rub-ink", 150.0)]);
    paint(&mut art, Some("oil"), vec![eraser("rub-paint", 150.0)]);
    // Ink laid after the eraser is not erased.
    paint(
        &mut art,
        Some("layer-1"),
        vec![brushed(
            line("after", [150.0, 20.0], [150.0, 40.0], 1.0),
            "pen",
        )],
    );
    let picture = pixels(&art, "erased.png");
    assert!(
        darkness(&picture, [60, 60]) > 0.5,
        "ink left of the eraser stays"
    );
    assert!(
        darkness(&picture, [150, 60]) < 0.05,
        "ink under the eraser is gone"
    );
    assert!(
        darkness(&picture, [60, 140]) > 0.3,
        "oil left of the eraser stays"
    );
    assert!(
        darkness(&picture, [150, 140]) < 0.05,
        "oil under the eraser is gone"
    );
    assert!(darkness(&picture, [150, 30]) > 0.5, "ink laid after shows");

    let refused = apply(
        &mut art,
        Artwork::PAINT.with(Paint {
            layer: None,
            strokes: vec![NewStroke {
                colour: String::new(),
                ..line("blank", [0.0, 0.0], [10.0, 0.0], 1.0)
            }],
            group: None,
        }),
    )
    .unwrap_err();
    assert!(refused.contains("only an eraser"), "{refused}");
    let unknown = apply(
        &mut art,
        Artwork::PAINT.with(Paint {
            layer: Some("layer-1".into()),
            strokes: vec![brushed(
                line("crayon", [0.0, 0.0], [10.0, 0.0], 1.0),
                "crayon",
            )],
            group: None,
        }),
    )
    .unwrap_err();
    assert!(
        unknown.contains("pencil") && unknown.contains("eraser"),
        "{unknown}"
    );
    let medium = apply(
        &mut art,
        Artwork::PAINT.with(Paint {
            layer: Some("layer-1".into()),
            strokes: vec![NewStroke {
                params: Some(BTreeMap::from([("load".to_owned(), 0.3)])),
                ..line("loaded", [0.0, 0.0], [10.0, 0.0], 1.0)
            }],
            group: None,
        }),
    )
    .unwrap_err();
    assert!(medium.contains("flow, hardness"), "{medium}");
}

/// `inspect` lists an eraser with what it takes paint off, and it is
/// removed by id like any mark, the paint under it back.
#[test]
fn an_eraser_is_listed_with_what_it_erases_and_removed_by_id() {
    let mut art = sheet(300.0, 200.0);
    paint(
        &mut art,
        None,
        vec![
            line("crossed", [20.0, 60.0], [280.0, 60.0], 1.0),
            line("apart", [20.0, 160.0], [100.0, 160.0], 1.0),
        ],
    );
    paint(
        &mut art,
        None,
        vec![NewStroke {
            id: Some("rub".into()),
            points: vec![vec![150.0, 20.0], vec![150.0, 100.0]],
            size: 30.0,
            brush: Some("eraser".into()),
            ..NewStroke::default()
        }],
    );
    let state = art.state().unwrap();
    let listed = view_ops::inspect(&state, &canvas::resource::Inspect::default()).unwrap();
    let rub = listed["marks_bottom_first"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mark| mark["id"] == "rub")
        .unwrap()
        .clone();
    assert_eq!(rub["brush"], "eraser");
    assert_eq!(rub["erases"], serde_json::json!(["crossed"]), "{rub}");
    apply(
        &mut art,
        Artwork::REMOVE_MARKS.with(RemoveMarks {
            ids: vec!["rub".into()],
            ..RemoveMarks::default()
        }),
    )
    .unwrap();
    assert!(art.state().unwrap().mark("rub").is_none());
}

/// A stroke eraser takes the strokes a path crosses, whole, in one step,
/// and only on the layer named.
#[test]
fn a_stroke_eraser_takes_whole_strokes_it_crosses() {
    let mut art = sheet(300.0, 200.0);
    paint(
        &mut art,
        None,
        vec![
            line("top", [20.0, 40.0], [280.0, 40.0], 1.0),
            line("middle", [20.0, 100.0], [280.0, 100.0], 1.0),
            line("bottom", [20.0, 160.0], [280.0, 160.0], 1.0),
        ],
    );
    let steps = art.log().len();
    apply(
        &mut art,
        Artwork::REMOVE_MARKS.with(RemoveMarks {
            along: Some(vec![[150.0, 20.0], [150.0, 120.0]]),
            ..RemoveMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let left: Vec<&str> = state.marks.iter().map(|mark| mark.id.as_str()).collect();
    assert_eq!(left, ["bottom"]);
    assert_eq!(art.log().len(), steps + 1, "one undo step");
    let missed = apply(
        &mut art,
        Artwork::REMOVE_MARKS.with(RemoveMarks {
            along: Some(vec![[10.0, 190.0], [12.0, 190.0]]),
            ..RemoveMarks::default()
        }),
    )
    .unwrap_err();
    assert!(missed.contains("crosses no brush stroke"), "{missed}");
}

fn points(art: &Replica<Artwork>, id: &str) -> Vec<Vec<f64>> {
    match &art.state().unwrap().mark(id).unwrap().shape {
        Shape::Stroke { points, .. } => points.clone(),
        _ => Vec::new(),
    }
}

/// A laid stroke is re-brushed, resized and reshaped by id: smoothed,
/// simplified, carried on, cut in two and joined again.
#[test]
fn strokes_are_rebrushed_and_reshaped_after_the_fact() {
    let mut art = sheet(300.0, 200.0);
    let wobbly = NewStroke {
        id: Some("wobbly".into()),
        points: (0..=40)
            .map(|step| {
                let wobble = if step % 2 == 0 { 3.0 } else { -3.0 };
                vec![20.0 + f64::from(step) * 6.0, 100.0 + wobble, 0.8]
            })
            .collect(),
        size: 10.0,
        colour: "#1d1b19".into(),
        ..NewStroke::default()
    };
    paint(&mut art, None, vec![wobbly]);
    let edit = |edit: EditMarks| Artwork::EDIT_MARKS.with(edit);
    apply(
        &mut art,
        edit(EditMarks {
            ids: vec!["wobbly".into()],
            brush: Some("charcoal".into()),
            size: Some(18.0),
            params: Some(BTreeMap::from([("flow".to_owned(), 0.4)])),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let mark = state.mark("wobbly").unwrap();
    assert_eq!(mark.style.brush.as_deref(), Some("charcoal"));
    assert!(matches!(mark.shape, Shape::Stroke { size, .. } if size == 18.0));

    apply(
        &mut art,
        edit(EditMarks {
            ids: vec!["wobbly".into()],
            smooth: Some(1.0),
            simplify: Some(1.0),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let smooth = points(&art, "wobbly");
    assert!(smooth.len() < 10, "{} points left", smooth.len());
    assert!(smooth.iter().all(|point| (point[1] - 100.0).abs() <= 3.0));

    apply(
        &mut art,
        edit(EditMarks {
            ids: vec!["wobbly".into()],
            extend: Some(vec![vec![270.0, 110.0], vec![290.0, 140.0]]),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    assert_eq!(points(&art, "wobbly").last().unwrap()[..2], [290.0, 140.0]);

    apply(
        &mut art,
        edit(EditMarks {
            ids: vec!["wobbly".into()],
            split: Some([150.0, 100.0]),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert_eq!(state.marks.len(), 2);
    let far = state
        .marks
        .iter()
        .find(|mark| mark.id != "wobbly")
        .unwrap()
        .clone();
    assert_eq!(
        far.style.brush.as_deref(),
        Some("charcoal"),
        "the far part keeps the brush"
    );
    assert!(points(&art, "wobbly").last().unwrap()[0] <= 151.0);
    assert!(points(&art, &far.id).first().unwrap()[0] >= 149.0);

    apply(
        &mut art,
        edit(EditMarks {
            ids: vec!["wobbly".into(), far.id],
            merge: Some(true),
            ..EditMarks::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert_eq!(state.marks.len(), 1);
    assert_eq!(points(&art, "wobbly").last().unwrap()[..2], [290.0, 140.0]);

    let start = points(&art, "wobbly")[0].clone();
    let refused = apply(
        &mut art,
        edit(EditMarks {
            ids: vec!["wobbly".into()],
            split: Some([start[0] - 1.0, start[1]]),
            ..EditMarks::default()
        }),
    )
    .unwrap_err();
    assert!(refused.contains("end of the stroke"), "{refused}");
}

/// A live view never waits for a painted layer: it shows what is finished
/// while the stroke just laid is simulated in the background. A `look`
/// through the same renderer waits, and sees exactly what a renderer
/// that always waits sees.
#[test]
fn a_live_view_does_not_wait_for_paint_and_a_look_does() {
    let mut art = sheet(300.0, 200.0);
    apply(
        &mut art,
        Artwork::ADD_LAYER.with(AddLayer {
            id: Some("oil".into()),
            name: "Oil".into(),
            medium: Some("oil".into()),
            ..AddLayer::default()
        }),
    )
    .unwrap();
    paint(
        &mut art,
        Some("oil"),
        vec![NewStroke {
            colour: "pigment:ultramarine".into(),
            size: 24.0,
            ..line("paint", [20.0, 100.0], [280.0, 100.0], 1.0)
        }],
    );
    let state = art.state().unwrap();
    let mut live = Renderer::impatient();
    let mut text = ui::TextRenderer::new();
    let options = canvas::render::Options {
        live: true,
        ..canvas::render::Options::default()
    };
    let mut scene = ui::vello::Scene::new();
    live.draw(
        &state,
        &mut scene,
        ui::vello::kurbo::Affine::IDENTITY,
        &mut text,
        &options,
    );
    assert!(live.settling(), "the stroke is simulated in the background");

    let look = Look {
        format: Some("png".into()),
        size: Some(300),
        ..Look::default()
    };
    let (seen, _) = view_ops::look(&mut live, &mut text, &state, &look).unwrap();
    assert!(!live.settling(), "the look waited for the layer");
    let (waited, _) = view_ops::look(&mut Renderer::default(), &mut text, &state, &look).unwrap();
    assert_eq!(seen.bytes, waited.bytes);
}

/// A brush's single dab, a click without a drag, is where it was laid:
/// what finds, lists and selects marks by where they are finds it there,
/// not at the artwork's corner.
#[test]
fn a_single_dab_is_where_it_was_laid() {
    let mut art = sheet(1200.0, 900.0);
    apply(
        &mut art,
        Artwork::PAINT.with(Paint {
            layer: None,
            strokes: vec![NewStroke {
                points: vec![vec![393.5, 358.2, 0.35]],
                ..line("dab", [0.0, 0.0], [0.0, 0.0], 1.0)
            }],
            group: None,
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let bounds = state.mark("dab").unwrap().bounds();
    assert!(
        (bounds.center().x - 393.5).abs() < 1e-6 && (bounds.center().y - 358.2).abs() < 1e-6,
        "{bounds:?}"
    );
    assert!((bounds.width() - 16.0).abs() < 1e-6, "{bounds:?}");
}
