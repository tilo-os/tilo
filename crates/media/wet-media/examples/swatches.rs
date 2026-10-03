//! Paint a small landscape in each built-in medium and write PNGs, to look
//! at what the engines do: `cargo run -p tilo-wet-media --example swatches -- target/wet-media`.
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
    Canvas, ColourField, Engines, FieldKind, Fill, Heightfield, LayerPainter, Mark, Painted,
    Preset, Rgb, Settings, Stroke, StrokePoint, composite, parse_paint, presets,
};

const WIDTH: usize = 640;
const HEIGHT: usize = 420;

fn stroke(preset: &Preset, colour: &str, size: f32, points: &[(f32, f32, f32)], wet: bool) -> Mark {
    Mark::Stroke(Stroke {
        points: points
            .iter()
            .map(|(x, y, pressure)| StrokePoint::new(*x, *y, *pressure))
            .collect(),
        size,
        paint: parse_paint(colour, &preset.palette).expect("palette colour"),
        opacity: 1.0,
        wet,
        params: BTreeMap::new(),
        form: None,
        clip: Vec::new(),
    })
}

fn region(
    preset: &Preset,
    colour: &str,
    outline: &[(f32, f32)],
    brush: f32,
    angle: f32,
    wet: bool,
) -> Mark {
    Mark::Fill(Fill {
        contours: vec![outline.to_vec()],
        paint: parse_paint(colour, &preset.palette).expect("palette colour"),
        opacity: 1.0,
        wet,
        brush,
        angle,
        params: BTreeMap::new(),
        field: None,
    })
}

fn with(mut mark: Mark, values: &[(&str, f32)]) -> Mark {
    let params = match &mut mark {
        Mark::Stroke(stroke) => &mut stroke.params,
        Mark::Fill(fill) => &mut fill.params,
    };
    params.extend(values.iter().map(|(id, value)| ((*id).to_owned(), *value)));
    mark
}

fn hex(text: &str) -> Rgb {
    parse_paint(text, &[]).expect("hex colour").colour
}

/// The oil techniques a landscape is built from: a graded sky laid in and
/// softened with a blender, a feathered band of mist, a glaze over a dry
/// passage and a scumble of light over it.
fn techniques(preset: &Preset) -> Vec<Mark> {
    let w = 640.0;
    let mut sky = region(
        preset,
        "pigment:lead-white",
        &[(0.0, 0.0), (w, 0.0), (w, 260.0), (0.0, 260.0)],
        40.0,
        0.0,
        false,
    );
    if let Mark::Fill(fill) = &mut sky {
        fill.field = ColourField::new(
            FieldKind::Linear,
            (0.0, 0.0),
            (0.0, 250.0),
            vec![
                (0.0, hex("#4f5e7c"), 1.0),
                (0.55, hex("#a9a3a6"), 1.0),
                (0.85, hex("#e2c79d"), 1.0),
                (1.0, hex("#efd7a8"), 1.0),
            ],
        );
    }
    let mut glow = region(
        preset,
        "#f3dcae",
        &[
            (330.0, 120.0),
            (630.0, 120.0),
            (630.0, 260.0),
            (330.0, 260.0),
        ],
        30.0,
        0.0,
        true,
    );
    if let Mark::Fill(fill) = &mut glow {
        fill.field = ColourField::new(
            FieldKind::Radial,
            (480.0, 250.0),
            (480.0, 130.0),
            vec![(0.0, hex("#f6dfb0"), 1.0), (1.0, hex("#c9bba7"), 1.0)],
        );
    }
    let blender = |angle: f32, strength: f32| {
        with(
            region(
                preset,
                "pigment:lead-white",
                &[(0.0, 0.0), (w, 0.0), (w, 262.0), (0.0, 262.0)],
                60.0,
                angle,
                true,
            ),
            &[("blender", strength)],
        )
    };
    let land = region(
        preset,
        "#6d6a78",
        &[(0.0, 260.0), (w, 250.0), (w, 420.0), (0.0, 420.0)],
        30.0,
        2.0,
        false,
    );
    let mist = with(
        region(
            preset,
            "#c9c3c4",
            &[(0.0, 248.0), (w, 244.0), (w, 280.0), (0.0, 284.0)],
            20.0,
            0.0,
            true,
        ),
        &[("feather", 1.5), ("load", 0.5)],
    );
    let field = region(
        preset,
        "#e8e4dc",
        &[(0.0, 330.0), (w, 320.0), (w, 420.0), (0.0, 420.0)],
        26.0,
        1.0,
        false,
    );
    let glaze = with(
        region(
            preset,
            "#8a5a3a",
            &[(40.0, 340.0), (300.0, 335.0), (300.0, 410.0), (40.0, 410.0)],
            30.0,
            0.0,
            false,
        ),
        &[("glaze", 0.9), ("feather", 0.8)],
    );
    let scumble = with(
        stroke(
            preset,
            "#f2ede1",
            18.0,
            &[(340.0, 380.0, 0.8), (600.0, 372.0, 0.6)],
            false,
        ),
        &[("load", 0.18)],
    );
    let soften_edge = with(
        stroke(
            preset,
            "#000000",
            16.0,
            &[(20.0, 258.0, 1.0), (620.0, 250.0, 1.0)],
            true,
        ),
        &[("blender", 0.8)],
    );
    vec![
        sky,
        glow,
        blender(0.0, 0.9),
        blender(2.0, 0.6),
        land,
        mist,
        soften_edge,
        field,
        glaze,
        scumble,
    ]
}

fn landscape(preset: &Preset, oil: bool) -> Vec<Mark> {
    let (sky, glow, far, near, dark, light) = if oil {
        (
            "pigment:cobalt-blue",
            "pigment:naples-yellow",
            "pigment:green-earth",
            "pigment:raw-umber",
            "pigment:burnt-umber",
            "pigment:lead-white",
        )
    } else {
        (
            "pigment:cerulean",
            "pigment:new-gamboge",
            "pigment:sap-green",
            "pigment:burnt-sienna",
            "pigment:sepia",
            "pigment:ultramarine",
        )
    };
    let w = 640.0;
    let mut marks = vec![
        region(
            preset,
            sky,
            &[(0.0, 0.0), (w, 0.0), (w, 200.0), (0.0, 230.0)],
            40.0,
            4.0,
            false,
        ),
        region(
            preset,
            glow,
            &[(0.0, 170.0), (w, 150.0), (w, 240.0), (0.0, 250.0)],
            30.0,
            -3.0,
            true,
        ),
        region(
            preset,
            far,
            &[
                (0.0, 250.0),
                (120.0, 215.0),
                (260.0, 235.0),
                (420.0, 205.0),
                (w, 240.0),
                (w, 300.0),
                (0.0, 300.0),
            ],
            24.0,
            0.0,
            false,
        ),
        region(
            preset,
            near,
            &[(0.0, 290.0), (w, 280.0), (w, 420.0), (0.0, 420.0)],
            34.0,
            2.0,
            false,
        ),
    ];
    for (index, x) in [140.0_f32, 168.0, 470.0].into_iter().enumerate() {
        let top = 150.0 + 20.0 * index as f32;
        marks.push(stroke(
            preset,
            dark,
            9.0,
            &[(x, 330.0, 1.0), (x + 4.0, 240.0, 0.8), (x - 3.0, top, 0.4)],
            false,
        ));
        for branch in 0..5 {
            let y = top + 18.0 * branch as f32;
            let reach = 40.0 - 6.0 * branch as f32;
            marks.push(stroke(
                preset,
                dark,
                4.0,
                &[
                    (x, y + 20.0, 0.9),
                    (x + reach * 0.6, y, 0.6),
                    (x + reach, y - 12.0, 0.2),
                ],
                true,
            ));
            marks.push(stroke(
                preset,
                dark,
                4.0,
                &[
                    (x, y + 24.0, 0.9),
                    (x - reach * 0.5, y + 4.0, 0.6),
                    (x - reach, y - 6.0, 0.2),
                ],
                true,
            ));
        }
    }
    marks.push(stroke(
        preset,
        light,
        6.0,
        &[(60.0, 330.0, 0.6), (300.0, 322.0, 1.0), (560.0, 335.0, 0.5)],
        false,
    ));
    marks
}

fn render(preset: &Preset, engines: &Engines, marks: Vec<Mark>) -> Vec<u8> {
    let engine = engines.get(&preset.engine).expect("built-in engine");
    let texture = preset.ground.unwrap_or_default();
    let ground = Arc::new(Heightfield::new(WIDTH, HEIGHT, texture, 1.0, 1));
    let canvas = Canvas {
        width: WIDTH,
        height: HEIGHT,
        scale: 1.0,
        ground: Arc::clone(&ground),
    };
    let settings = Settings::new(engine.parameters(), &preset.params);
    let mut painter = LayerPainter::new(Arc::clone(&engine), canvas, settings);
    let marks: Vec<Painted> = marks
        .into_iter()
        .enumerate()
        .map(|(index, mark)| Painted::new(&format!("mark-{index}"), mark))
        .collect();
    let started = std::time::Instant::now();
    let layer = painter.paint(&marks).to_vec();
    eprintln!(
        "{}: {} marks in {:?}",
        preset.id,
        marks.len(),
        started.elapsed()
    );
    let mut image = ground.shade(Rgb::from_srgb8(0xf1, 0xea, 0xda));
    composite::onto(&mut image, &layer, engine.blend(), 1.0);
    image
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/wet-media".into());
    std::fs::create_dir_all(&out).expect("output directory");
    let engines = Engines::builtin();
    let mut sheets: Vec<(String, Preset, Vec<Mark>)> = presets::builtin()
        .into_iter()
        .map(|preset| {
            let marks = landscape(&preset, preset.engine == "oil");
            (preset.id.clone(), preset, marks)
        })
        .collect();
    if let Some(oil) = presets::builtin()
        .into_iter()
        .find(|preset| preset.id == "oil")
    {
        let marks = techniques(&oil);
        sheets.push(("oil-techniques".into(), oil, marks));
    }
    for (name, preset, marks) in sheets {
        let image = render(&preset, &engines, marks);
        let path = format!("{out}/{name}.png");
        let file = std::fs::File::create(&path).expect("png file");
        let mut encoder = png::Encoder::new(file, WIDTH as u32, HEIGHT as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&image))
            .expect("png data");
        println!("{path}");
    }
}
