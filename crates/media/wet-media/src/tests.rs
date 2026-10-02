use std::collections::BTreeMap;
use std::sync::Arc;

use crate::*;

fn canvas(texture: Texture) -> Canvas {
    Canvas {
        width: 160,
        height: 120,
        scale: 1.0,
        ground: Arc::new(Heightfield::new(160, 120, texture, 1.0, 7)),
    }
}

fn paint(hex: &str) -> Paint {
    parse_paint(hex, &[]).unwrap()
}

fn stroke(points: &[(f32, f32)], size: f32, colour: &str, wet: bool) -> Mark {
    Mark::Stroke(Stroke {
        points: points
            .iter()
            .map(|(x, y)| StrokePoint::new(*x, *y, 1.0))
            .collect(),
        size,
        paint: paint(colour),
        opacity: 1.0,
        wet,
        params: BTreeMap::new(),
    })
}

fn region(colour: &str) -> Mark {
    Mark::Fill(Fill {
        contours: vec![vec![
            (20.0, 20.0),
            (140.0, 20.0),
            (140.0, 70.0),
            (20.0, 70.0),
        ]],
        paint: paint(colour),
        opacity: 1.0,
        wet: false,
        brush: 14.0,
        angle: 10.0,
        params: BTreeMap::new(),
        field: None,
    })
}

fn painter(engine: &str, texture: Texture) -> LayerPainter {
    let engines = Engines::builtin();
    let engine = engines.get(engine).unwrap();
    let settings = Settings::new(engine.parameters(), &BTreeMap::new());
    LayerPainter::new(engine, canvas(texture), settings)
}

fn alpha(pixels: &[u8], x: usize, y: usize) -> u8 {
    pixels[(y * 160 + x) * 4 + 3]
}

fn marks() -> Vec<Painted> {
    vec![
        Painted::new("sky", region("#3a6ea5")),
        Painted::new(
            "hill",
            stroke(
                &[(10.0, 90.0), (80.0, 75.0), (150.0, 95.0)],
                18.0,
                "#6f7a52",
                false,
            ),
        ),
        Painted::new(
            "light",
            stroke(&[(60.0, 40.0), (100.0, 45.0)], 8.0, "#f2c12e", true),
        ),
    ]
}

/// Every collaborator simulates the same marks into the same pixels, and
/// painting a layer incrementally equals painting it from scratch.
#[test]
fn engines_are_deterministic_and_incremental_painting_equals_replay() {
    for (engine, texture) in [("oil", Texture::Linen), ("watercolour", Texture::ColdPress)] {
        let marks = marks();
        let whole = painter(engine, texture).paint(&marks).to_vec();
        assert_eq!(
            whole,
            painter(engine, texture).paint(&marks).to_vec(),
            "{engine}"
        );

        let mut incremental = painter(engine, texture);
        for count in 1..=marks.len() {
            incremental.paint(&marks[..count]);
        }
        assert_eq!(incremental.paint(&marks), whole.as_slice(), "{engine}");

        // Editing a mark re-simulates from before it and matches a fresh run.
        let mut edited = marks.clone();
        edited[1] = Painted::new(
            "hill",
            stroke(
                &[(10.0, 90.0), (80.0, 70.0), (150.0, 95.0)],
                18.0,
                "#8a3c1e",
                false,
            ),
        );
        let fresh = painter(engine, texture).paint(&edited).to_vec();
        assert_eq!(incremental.paint(&edited), fresh.as_slice(), "{engine}");
        assert_ne!(fresh, whole, "{engine}: the edit changed the painting");
        assert_eq!(
            edited[1].seed, marks[1].seed,
            "an edit keeps the mark's character"
        );
    }
}

/// Oil covers what it touches and leaves the rest bare; a thin dry brush
/// over a coarse weave leaves broken coverage rather than a solid band.
#[test]
fn oil_covers_its_strokes_and_a_dry_brush_breaks_over_the_weave() {
    let mut oil = painter("oil", Texture::Linen);
    let pixels = oil
        .paint(&[Painted::new(
            "a",
            stroke(&[(20.0, 60.0), (140.0, 60.0)], 20.0, "#c8902f", false),
        )])
        .to_vec();
    assert!(alpha(&pixels, 80, 60) > 200, "{}", alpha(&pixels, 80, 60));
    assert_eq!(alpha(&pixels, 80, 10), 0);

    let mut dry = painter("oil", Texture::Linen);
    let mut thin = stroke(&[(20.0, 60.0), (140.0, 60.0)], 20.0, "#c8902f", false);
    if let Mark::Stroke(stroke) = &mut thin {
        stroke.params.insert("load".into(), 0.12);
    }
    let pixels = dry.paint(&[Painted::new("a", thin)]).to_vec();
    let row: Vec<u8> = (40..120).map(|x| alpha(&pixels, x, 60)).collect();
    let bare = row.iter().filter(|alpha| **alpha < 80).count();
    assert!(bare > 8, "a dry brush skips the low threads: {row:?}");
}

/// Watercolour is transparent glaze: painted cells darken (multiply alpha
/// above zero) without hiding the paper, the paper elsewhere is untouched,
/// and a dried wash is darker at its rim than in its middle.
#[test]
fn watercolour_glazes_and_dries_with_a_dark_edge() {
    let mut water = painter("watercolour", Texture::Paper);
    let pixels = water
        .paint(&[Painted::new("wash", region("#2e45a6"))])
        .to_vec();
    assert!(alpha(&pixels, 80, 45) > 20);
    assert!(
        alpha(&pixels, 80, 45) < 250,
        "a single wash stays transparent"
    );
    assert_eq!(alpha(&pixels, 80, 110), 0);
    let middle: u32 = (60..100).map(|x| u32::from(alpha(&pixels, x, 45))).sum();
    let rim: u32 = (60..100)
        .map(|x| {
            (16..26)
                .map(|y| u32::from(alpha(&pixels, x, y)))
                .max()
                .unwrap()
        })
        .sum();
    assert!(rim > middle, "rim {rim} vs middle {middle}");
}

fn rgb(pixels: &[u8], x: usize, y: usize) -> [u8; 3] {
    let at = (y * 160 + x) * 4;
    [pixels[at], pixels[at + 1], pixels[at + 2]]
}

fn with(mut mark: Mark, values: &[(&str, f32)]) -> Mark {
    let params = match &mut mark {
        Mark::Stroke(stroke) => &mut stroke.params,
        Mark::Fill(fill) => &mut fill.params,
    };
    params.extend(values.iter().map(|(id, value)| ((*id).to_owned(), *value)));
    mark
}

fn band_fill(colour: &str, x0: f32, x1: f32, wet: bool) -> Fill {
    Fill {
        contours: vec![vec![(x0, 10.0), (x1, 10.0), (x1, 110.0), (x0, 110.0)]],
        paint: paint(colour),
        opacity: 1.0,
        wet,
        brush: 14.0,
        angle: 0.0,
        params: BTreeMap::new(),
        field: None,
    }
}

fn band(colour: &str, x0: f32, x1: f32, wet: bool) -> Mark {
    Mark::Fill(band_fill(colour, x0, x1, wet))
}

/// The mean luminance-ish value of a column of the painted layer.
fn column(pixels: &[u8], x: usize) -> f32 {
    (30..90)
        .map(|y| rgb(pixels, x, y).iter().map(|v| f32::from(*v)).sum::<f32>())
        .sum::<f32>()
        / 60.0
}

/// A graded fill lays its paint in to the colour the gradient wants where
/// each stroke is, darker at one end and lighter at the other.
#[test]
fn an_oil_fill_follows_its_colour_field() {
    let mut fill = band_fill("#808080", 0.0, 160.0, false);
    fill.field = ColourField::new(
        FieldKind::Linear,
        (0.0, 0.0),
        (160.0, 0.0),
        vec![
            (0.0, paint("#202840").colour, 1.0),
            (1.0, paint("#f0d8a0").colour, 1.0),
        ],
    );
    let pixels = painter("oil", Texture::Linen)
        .paint(&[Painted::new("sky", Mark::Fill(fill))])
        .to_vec();
    let (left, right) = (column(&pixels, 20), column(&pixels, 140));
    assert!(left < 250.0 && right > 450.0, "left {left} right {right}");
}

/// A gradient that fades to clear thins its paint out evenly, however its
/// strokes overlap: full cover at one end, bare at the other, half between.
#[test]
fn an_oil_fill_fades_out_where_its_field_is_clear() {
    let mut fill = band_fill("#f0d8a0", 0.0, 160.0, false);
    fill.field = ColourField::new(
        FieldKind::Linear,
        (0.0, 0.0),
        (160.0, 0.0),
        vec![
            (0.0, paint("#f0d8a0").colour, 1.0),
            (1.0, paint("#f0d8a0").colour, 0.0),
        ],
    );
    let pixels = painter("oil", Texture::Linen)
        .paint(&[Painted::new("glow", Mark::Fill(fill))])
        .to_vec();
    let cover = |x: usize| {
        (30..90)
            .map(|y| f32::from(alpha(&pixels, x, y)))
            .sum::<f32>()
            / 60.0
    };
    assert!(cover(20) > 200.0, "{}", cover(20));
    assert!((cover(80) - 128.0).abs() < 30.0, "{}", cover(80));
    assert!(cover(155) < 20.0, "{}", cover(155));
}

/// A blender drags wet paint across the meeting of two colours, so the
/// step between them becomes a gradation; over dry paint it does nothing.
#[test]
fn a_blender_softens_wet_paint_and_leaves_dry_paint_alone() {
    let marks = vec![
        Painted::new("dark", band("#203050", 0.0, 80.0, false)),
        Painted::new("light", band("#e0d0b0", 80.0, 160.0, true)),
    ];
    let hard = painter("oil", Texture::Linen).paint(&marks).to_vec();
    let mut softened = marks.clone();
    softened.push(Painted::new(
        "soften",
        with(band("#000000", 0.0, 160.0, true), &[("blender", 1.0)]),
    ));
    let soft = painter("oil", Texture::Linen).paint(&softened).to_vec();
    // The steepest change across the meeting.
    let step = |pixels: &[u8]| {
        (50..106)
            .map(|x| (column(pixels, x + 4) - column(pixels, x)).abs())
            .fold(0.0_f32, f32::max)
    };
    assert!(
        step(&soft) < step(&hard) * 0.6,
        "soft step {} hard step {}",
        step(&soft),
        step(&hard)
    );
    assert!(
        column(&soft, 70) > column(&hard, 70),
        "light paint dragged into the dark"
    );

    let mut dried = marks;
    dried.push(Painted::new("dry", band("#203050", 150.0, 159.0, false)));
    let before = painter("oil", Texture::Linen).paint(&dried).to_vec();
    dried.push(Painted::new(
        "soften",
        with(band("#000000", 0.0, 140.0, true), &[("blender", 1.0)]),
    ));
    let after = painter("oil", Texture::Linen).paint(&dried).to_vec();
    assert_eq!(before, after, "dry paint does not move");
}

/// A glaze tints dry paint beneath it without hiding its modelling: dark
/// over light shows, one coat filters once however its strokes overlap,
/// and a second coat deepens it.
#[test]
fn an_oil_glaze_filters_the_paint_beneath_once_per_coat() {
    let light = Painted::new("light", band("#e8e0d0", 0.0, 160.0, false));
    let glaze = |id: &str| {
        Painted::new(
            id,
            with(band("#8a3c1e", 40.0, 120.0, false), &[("glaze", 1.0)]),
        )
    };
    let plain = painter("oil", Texture::Linen)
        .paint(std::slice::from_ref(&light))
        .to_vec();
    let one_coat = painter("oil", Texture::Linen)
        .paint(&[light.clone(), glaze("a")])
        .to_vec();
    let two_coats = painter("oil", Texture::Linen)
        .paint(&[light, glaze("a"), glaze("b")])
        .to_vec();
    let (bare, once, twice) = (
        column(&plain, 80),
        column(&one_coat, 80),
        column(&two_coats, 80),
    );
    assert!(once < bare * 0.85, "a glaze darkens: {bare} -> {once}");
    assert!(
        once > bare * 0.35,
        "one coat filters once: {bare} -> {once}"
    );
    assert!(
        twice < once * 0.9,
        "a second coat deepens: {once} -> {twice}"
    );
    let [red, _, blue] = rgb(&one_coat, 80, 60);
    assert!(
        red > blue + 30,
        "it takes the glaze's warmth: {red} vs {blue}"
    );
    assert!(
        (column(&one_coat, 10) - column(&plain, 10)).abs() < 1.0,
        "outside it is untouched"
    );
}

#[test]
fn builtin_presets_validate_and_bad_presets_say_why() {
    let engines = Engines::builtin();
    for preset in presets::builtin() {
        preset.validate(&engines).unwrap();
    }
    let mut bad = presets::builtin().remove(0);
    bad.params.insert("viscosity".into(), 0.5);
    let error = bad.validate(&engines).unwrap_err();
    assert!(
        error.contains("no parameter") && error.contains("load"),
        "{error}"
    );
    bad = presets::builtin().remove(0);
    bad.engine = "gouache".into();
    assert!(bad.validate(&engines).unwrap_err().contains("oil"));
}
