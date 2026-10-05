//! Oil handling studies, written as PNGs to look at closely: dry-brush
//! strokes at every load over dry and over wet paint, and a blender worked
//! across a horizon after unrelated passages were laid dry:
//! `cargo run -p tilo-wet-media --release --example oil_study -- target/oil-study`.
#![allow(
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "a developer tool over small fixed sizes that should abort loudly on failure"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use tilo_wet_media::{
    Canvas, ColourField, Engines, FieldKind, Fill, Heightfield, LayerPainter, Mark, Painted, Rgb,
    Settings, Stroke, StrokePoint, Texture, composite, parse_paint,
};

const LOADS: [f32; 7] = [0.1, 0.2, 0.3, 0.45, 0.6, 0.8, 1.2];

fn hex(text: &str) -> Rgb {
    parse_paint(text, &[]).expect("hex colour").colour
}

fn stroke(colour: &str, size: f32, points: &[(f32, f32, f32)], wet: bool) -> Mark {
    Mark::Stroke(Stroke {
        points: points
            .iter()
            .map(|(x, y, pressure)| StrokePoint::new(*x, *y, *pressure))
            .collect(),
        size,
        paint: parse_paint(colour, &[]).expect("colour"),
        opacity: 1.0,
        wet,
        params: BTreeMap::new(),
        form: None,
        clip: Vec::new(),
    })
}

fn region(colour: &str, outline: &[(f32, f32)], brush: f32, angle: f32, wet: bool) -> Mark {
    Mark::Fill(Fill {
        contours: vec![outline.to_vec()],
        paint: parse_paint(colour, &[]).expect("colour"),
        opacity: 1.0,
        wet,
        brush,
        angle,
        params: BTreeMap::new(),
        field: None,
    })
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn with(mut mark: Mark, values: &[(&str, f32)]) -> Mark {
    let params = match &mut mark {
        Mark::Stroke(stroke) => &mut stroke.params,
        Mark::Fill(fill) => &mut fill.params,
        Mark::Erase(_) => return mark,
    };
    params.extend(values.iter().map(|(id, value)| ((*id).to_owned(), *value)));
    mark
}

fn graded(mut mark: Mark, from: (f32, f32), to: (f32, f32), stops: &[(f32, &str)]) -> Mark {
    if let Mark::Fill(fill) = &mut mark {
        fill.field = ColourField::new(
            FieldKind::Linear,
            from,
            to,
            stops
                .iter()
                .map(|(at, colour)| (*at, hex(colour), 1.0))
                .collect(),
        );
    }
    mark
}

/// Light strokes at each load over a dry dark band (a scumble), then over a
/// wet mid-tone band (wet into wet).
fn loads() -> Vec<Mark> {
    let mut marks = vec![region(
        "#2b3142",
        &rect(0.0, 20.0, 900.0, 150.0),
        30.0,
        0.0,
        false,
    )];
    for (index, load) in LOADS.into_iter().enumerate() {
        let x = 20.0 + index as f32 * 125.0;
        marks.push(with(
            stroke(
                "#e9dcc0",
                26.0,
                &[
                    (x, 60.0, 0.8),
                    (x + 50.0, 52.0, 1.0),
                    (x + 105.0, 62.0, 0.7),
                ],
                false,
            ),
            &[("load", load)],
        ));
        marks.push(with(
            stroke(
                "#e9dcc0",
                26.0,
                &[(x, 110.0, 0.5), (x + 105.0, 104.0, 0.5)],
                false,
            ),
            &[("load", load), ("thickness", 0.15)],
        ));
    }
    marks.push(region(
        "#6d7f9c",
        &rect(0.0, 190.0, 900.0, 320.0),
        30.0,
        0.0,
        false,
    ));
    for (index, load) in LOADS.into_iter().enumerate() {
        let x = 20.0 + index as f32 * 125.0;
        marks.push(with(
            stroke(
                "#e9dcc0",
                26.0,
                &[
                    (x, 230.0, 0.8),
                    (x + 50.0, 222.0, 1.0),
                    (x + 105.0, 232.0, 0.7),
                ],
                true,
            ),
            &[("load", load)],
        ));
        marks.push(with(
            stroke(
                "#f2d89b",
                22.0,
                &[(x, 280.0, 1.0), (x + 105.0, 276.0, 1.0)],
                true,
            ),
            &[("load", load), ("thickness", 0.15)],
        ));
    }
    marks
}

/// A dark trunk, limbs and a crown of dabs over a wet meadow and sky, as a
/// tree is painted into a landscape.
fn tree() -> Vec<Mark> {
    let sky = graded(
        region("#9fa6b1", &rect(0.0, 0.0, 400.0, 200.0), 50.0, 0.0, false),
        (0.0, 0.0),
        (0.0, 200.0),
        &[(0.0, "#8d9bb2"), (1.0, "#e6d4a8")],
    );
    let meadow = region("#8e9450", &rect(0.0, 200.0, 400.0, 400.0), 36.0, 1.0, true);
    let mut marks = vec![sky, meadow];
    let mut trunk = stroke(
        "#2e241b",
        18.0,
        &[
            (200.0, 380.0, 1.0),
            (204.0, 300.0, 0.9),
            (196.0, 220.0, 0.7),
            (202.0, 160.0, 0.5),
        ],
        false,
    );
    if let Mark::Stroke(stroke) = &mut trunk {
        stroke.params.insert("load".into(), 1.1);
    }
    marks.push(trunk);
    for (colour, size, count, scatter, wet, points) in [
        (
            "#2f3a22",
            20.0,
            260,
            45.0,
            false,
            [(100.0, 150.0), (200.0, 80.0), (300.0, 150.0)],
        ),
        (
            "#6b7944",
            12.0,
            90,
            28.0,
            true,
            [(130.0, 110.0), (190.0, 80.0), (250.0, 90.0)],
        ),
    ] {
        marks.push(Mark::Stroke(Stroke {
            points: points
                .iter()
                .map(|(x, y)| StrokePoint::new(*x, *y, 1.0))
                .collect(),
            size,
            paint: parse_paint(colour, &[]).expect("colour"),
            opacity: 1.0,
            wet,
            params: BTreeMap::from([("load".to_owned(), 0.9)]),
            form: Some(tilo_wet_media::Form::Dabs {
                count,
                scatter,
                length: 0.0,
                angle: None,
            }),
            clip: Vec::new(),
        }));
    }
    marks
}

/// The passages of a fresh agent's evening field, in its order: sky, hills
/// and field laid dry, then wet passages and a small dry tree line, then a
/// blender along the horizon when `blend`.
fn horizon(blend: bool) -> Vec<Mark> {
    let sky = with(
        graded(
            region("#6f86a8", &rect(0.0, 0.0, 900.0, 440.0), 60.0, 0.0, false),
            (0.0, 0.0),
            (0.0, 440.0),
            &[(0.0, "#6f86a8"), (0.55, "#b9b7b0"), (1.0, "#efc98c")],
        ),
        &[("load", 0.6), ("thickness", 0.2), ("blend", 0.6)],
    );
    let hills = with(
        region(
            "#9c9caf",
            &[
                (0.0, 420.0),
                (220.0, 393.0),
                (320.0, 400.0),
                (520.0, 365.0),
                (640.0, 372.0),
                (900.0, 395.0),
                (900.0, 450.0),
                (0.0, 450.0),
            ],
            28.0,
            0.0,
            false,
        ),
        &[("thickness", 0.2)],
    );
    let field = graded(
        region("#b4a45c", &rect(0.0, 440.0, 900.0, 700.0), 40.0, 0.0, false),
        (0.0, 440.0),
        (0.0, 700.0),
        &[(0.0, "#b4a45c"), (1.0, "#5e6a32")],
    );
    let mut marks = vec![
        sky,
        hills,
        field,
        region(
            "#c9b468",
            &[(0.0, 448.0), (900.0, 446.0), (900.0, 520.0), (0.0, 515.0)],
            22.0,
            0.0,
            true,
        ),
        region(
            "#4c5a2c",
            &[(0.0, 600.0), (900.0, 590.0), (900.0, 700.0), (0.0, 700.0)],
            30.0,
            -10.0,
            true,
        ),
        region(
            "#d3b981",
            &[
                (470.0, 448.0),
                (490.0, 448.0),
                (600.0, 700.0),
                (380.0, 700.0),
            ],
            16.0,
            90.0,
            true,
        ),
        region(
            "#6a7058",
            &[
                (560.0, 446.0),
                (620.0, 428.0),
                (680.0, 420.0),
                (760.0, 428.0),
                (820.0, 438.0),
                (820.0, 448.0),
                (560.0, 448.0),
            ],
            10.0,
            0.0,
            false,
        ),
    ];
    if blend {
        marks.push(with(
            stroke(
                "#b0aeb6",
                40.0,
                &[
                    (0.0, 410.0, 1.0),
                    (300.0, 405.0, 1.0),
                    (600.0, 395.0, 1.0),
                    (900.0, 405.0, 1.0),
                ],
                true,
            ),
            &[("blender", 0.8)],
        ));
    }
    marks
}

fn render(name: &str, out: &str, (width, height): (usize, usize), marks: Vec<Mark>) -> Vec<u8> {
    let engines = Engines::builtin();
    let engine = engines.get("oil").expect("oil engine");
    let ground = Arc::new(Heightfield::new(width, height, Texture::Linen, 1.0, 1));
    let canvas = Canvas::whole(1.0, Arc::clone(&ground));
    let preset = tilo_wet_media::presets::builtin()
        .into_iter()
        .find(|preset| preset.id == "oil")
        .expect("oil preset");
    let settings = Settings::new(engine.parameters(), &preset.params);
    let mut painter = LayerPainter::new(Arc::clone(&engine), canvas, settings);
    let marks: Vec<Painted> = marks
        .into_iter()
        .enumerate()
        .map(|(index, mark)| Painted::new(&format!("mark-{index}"), mark))
        .collect();
    let started = std::time::Instant::now();
    let layer = painter.paint(&marks).to_vec();
    eprintln!("{name}: {} marks in {:?}", marks.len(), started.elapsed());
    let mut image = ground.shade(Rgb::from_srgb8(0xe9, 0xe2, 0xd2));
    composite::onto(&mut image, &layer, engine.blend(), 1.0);
    let path = format!("{out}/{name}.png");
    let file = std::fs::File::create(&path).expect("png file");
    let mut encoder = png::Encoder::new(file, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&image))
        .expect("png data");
    println!("{path}");
    image
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/oil-study".into());
    std::fs::create_dir_all(&out).expect("output directory");
    render("loads", &out, (900, 340), loads());
    render("tree", &out, (400, 400), tree());
    let before = render("horizon-dry", &out, (900, 700), horizon(false));
    let after = render("horizon-blended", &out, (900, 700), horizon(true));
    // How much the blender changed the band it was worked across.
    let changed: u64 = before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .skip(370 * 900)
        .take(60 * 900)
        .map(|(a, b)| {
            a.iter()
                .zip(b)
                .take(3)
                .map(|(x, y)| u64::from(x.abs_diff(*y)))
                .sum::<u64>()
        })
        .sum();
    println!("blender changed the horizon band by {changed}");
}
