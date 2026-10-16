use std::collections::BTreeMap;
use std::sync::Arc;

use crate::grid::float;
use crate::*;

fn canvas(texture: Texture) -> Canvas {
    Canvas::whole(1.0, Arc::new(Heightfield::new(160, 120, texture, 1.0, 7)))
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
        form: None,
        clip: Vec::new(),
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

/// A window of the grid paints what the whole grid paints there: the
/// ground, the mottling and the brushwork follow whole-grid cells, so
/// windows of a painting meet without seams and a zoomed view can
/// simulate only what it shows. Marks that end outside the window, or dry
/// the wash elsewhere, still count.
#[test]
fn a_window_paints_what_the_whole_grid_paints_there() {
    for (engine, texture) in [("oil", Texture::Linen), ("watercolour", Texture::ColdPress)] {
        let marks = marks();
        let whole = painter(engine, texture).paint(&marks).to_vec();
        let engines = Engines::builtin();
        let medium = engines.get(engine).unwrap();
        let settings = Settings::new(medium.parameters(), &BTreeMap::new());
        let window = |origin: (usize, usize), (width, height): (usize, usize)| {
            let ground = Arc::new(Heightfield::window(origin, width, height, texture, 1.0, 7));
            let canvas = Canvas::window(1.0, ground, origin);
            let mut painter = LayerPainter::new(Arc::clone(&medium), canvas, settings.clone());
            painter
                .paint(&windowed(&marks, origin, width, height))
                .to_vec()
        };
        // A window a little inside the grid paints its middle alike; only
        // its rim, where marks run out of it, may differ.
        let (origin, size) = ((4, 6), (152, 110));
        let inner = window(origin, size);
        assert_eq!(inner, window(origin, size), "{engine}: deterministic");
        let mut differ = 0_usize;
        let mut worst = 0_u8;
        for y in 12..size.1 - 12 {
            for x in 12..size.0 - 12 {
                let a = &inner[(y * size.0 + x) * 4..][..4];
                let b = &whole[((y + origin.1) * 160 + x + origin.0) * 4..][..4];
                let gap = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                worst = worst.max(gap);
                differ += usize::from(gap > 8);
            }
        }
        assert!(
            differ * 1000 < size.0 * size.1,
            "{engine}: {differ} cells differ, worst by {worst}"
        );
        // A small window inside the painting, cut through its marks, keeps
        // its middle as the whole grid paints it there.
        let (origin, size) = ((50, 30), (60, 50));
        let part = window(origin, size);
        let margin = 16;
        let mut differ = 0_usize;
        let mut cells = 0_usize;
        for y in margin..size.1 - margin {
            for x in margin..size.0 - margin {
                let a = &part[(y * size.0 + x) * 4..][..4];
                let b = &whole[((y + origin.1) * 160 + x + origin.0) * 4..][..4];
                let gap = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                differ += usize::from(gap > 24);
                cells += 1;
            }
        }
        assert!(
            differ * 50 < cells,
            "{engine}: {differ} of {cells} cells differ"
        );
    }
}

/// Oil covers what it touches and leaves the rest bare; a thin dry brush
/// over a coarse weave leaves broken coverage rather than a solid band.
/// A stroke still being painted shows as it will once painted, and leaves
/// the layer as it was.
#[test]
fn a_preview_shows_the_wet_stroke_without_keeping_it() {
    for (engine, texture) in [("oil", Texture::Linen), ("watercolour", Texture::ColdPress)] {
        let marks = marks();
        let (done, wet) = marks.split_at(2);
        let mut layer = painter(engine, texture);
        let preview = layer.preview(done, wet);
        assert_eq!(
            preview,
            painter(engine, texture).paint(&marks).to_vec(),
            "{engine}"
        );
        assert_eq!(layer.applied(), 2, "{engine}: the wet stroke is not kept");
        assert_eq!(
            layer.paint(done).to_vec(),
            painter(engine, texture).paint(done).to_vec(),
            "{engine}"
        );
    }
}

/// A stroke laid exactly as it was last previewed takes the preview's
/// simulation, and paints what painting it afresh paints; a stroke laid
/// differently from its preview is painted afresh. A formed stroke is
/// previewed piece by piece, as it is laid.
#[test]
fn laying_the_previewed_stroke_equals_painting_it_afresh() {
    let mut grass = stroke(&[(30.0, 100.0), (120.0, 96.0)], 6.0, "#4f7a2a", true);
    if let Mark::Stroke(stroke) = &mut grass {
        stroke.form = Some(Form::Dabs {
            count: 9,
            scatter: 6.0,
            length: 14.0,
            angle: None,
        });
    }
    for (engine, texture) in [("oil", Texture::Linen), ("watercolour", Texture::ColdPress)] {
        let mut marks = marks();
        marks.push(Painted::new("grass", grass.clone()));
        let fresh = painter(engine, texture).paint(&marks).to_vec();
        let (done, wet) = marks.split_at(2);

        let mut layer = painter(engine, texture);
        let preview = layer.preview(done, wet);
        assert_eq!(preview, fresh, "{engine}: the preview is the laid stroke");
        assert_eq!(layer.paint(&marks), fresh.as_slice(), "{engine}");
        assert_eq!(layer.applied(), marks.len(), "{engine}");

        // Previewed shorter than it was laid: painted afresh all the same.
        let mut layer = painter(engine, texture);
        layer.preview(done, &wet[..1]);
        let mut other = marks.clone();
        other[2] = Painted::new(
            "light",
            stroke(&[(60.0, 40.0), (110.0, 50.0)], 8.0, "#f2c12e", true),
        );
        assert_eq!(
            layer.paint(&other),
            painter(engine, texture).paint(&other),
            "{engine}"
        );
    }
}

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
        Mark::Erase(_) => return mark,
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
/// step between them becomes a gradation. A passage laid dry elsewhere on
/// the layer sets only the paint under it, so the blender still works the
/// rest.
#[test]
fn a_blender_softens_wet_paint_however_much_is_laid_dry_elsewhere() {
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

    let mut elsewhere = marks;
    elsewhere.push(Painted::new(
        "elsewhere",
        stroke(&[(10.0, 112.0), (150.0, 112.0)], 6.0, "#203050", false),
    ));
    let before = painter("oil", Texture::Linen).paint(&elsewhere).to_vec();
    elsewhere.push(Painted::new(
        "soften",
        with(band("#000000", 0.0, 160.0, true), &[("blender", 1.0)]),
    ));
    let after = painter("oil", Texture::Linen).paint(&elsewhere).to_vec();
    assert!(
        step(&after) < step(&before) * 0.6,
        "still workable: soft step {} hard step {}",
        step(&after),
        step(&before)
    );
}

/// Paint laid over without `wet` covers what is beneath as over dry paint:
/// a light stroke across a wet dark band stays light, where the same stroke
/// laid wet picks the dark up.
#[test]
fn a_stroke_laid_dry_covers_and_one_laid_wet_mixes() {
    let over = |wet: bool| {
        let mut painter = painter("oil", Texture::Smooth);
        painter
            .paint(&[
                Painted::new("dark", band("#202838", 10.0, 150.0, true)),
                Painted::new(
                    "light",
                    with(
                        stroke(&[(20.0, 60.0), (140.0, 60.0)], 20.0, "#e8e0d0", wet),
                        &[("blend", 1.0)],
                    ),
                ),
            ])
            .to_vec()
    };
    let light = |pixels: &[u8]| {
        (60..120)
            .map(|x| {
                rgb(pixels, x, 60)
                    .iter()
                    .map(|v| f32::from(*v))
                    .sum::<f32>()
            })
            .sum::<f32>()
    };
    assert!(
        light(&over(false)) > light(&over(true)),
        "wet picks up the dark"
    );
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

/// How unevenly the light falls across a stroke's middle, relative to its
/// colour: the ridges the light picks out, whatever the paint's value.
fn ridging(pixels: &[u8]) -> f32 {
    let values: Vec<f32> = (50..70)
        .flat_map(|y| (40..120).map(move |x| (x, y)))
        .map(|(x, y)| rgb(pixels, x, y).iter().map(|v| f32::from(*v)).sum::<f32>())
        .collect();
    let mean = values.iter().sum::<f32>() / float(values.len());
    let spread = values.iter().map(|v| (v - mean).abs()).sum::<f32>() / float(values.len());
    spread / mean
}

/// Lights are mixed with body (lead white) and stand up in ridges; darks
/// go on lean and lie flat, so the light finds less in them.
#[test]
fn oil_lights_stand_up_in_ridges_and_darks_lie_lean() {
    let ridges = |colour: &str| {
        let pixels = painter("oil", Texture::Smooth)
            .paint(&[Painted::new(
                "a",
                stroke(&[(20.0, 60.0), (140.0, 60.0)], 30.0, colour, false),
            )])
            .to_vec();
        ridging(&pixels)
    };
    let (light, dark) = (ridges("#f0e8d8"), ridges("#3a4050"));
    assert!(light > dark * 1.3, "light {light} vs dark {dark}");
}

/// The row where a fill's top edge first covers, per column.
fn top_edge(pixels: &[u8]) -> Vec<usize> {
    (30..130)
        .map(|x| (5..40).find(|y| alpha(pixels, x, *y) > 128).unwrap_or(40))
        .collect()
}

/// A broken edge wanders in and out of the drawn contour, as a loaded
/// brush leaves it, instead of following it like a ruler.
#[test]
fn an_oil_fill_breaks_its_edge_when_asked() {
    let edge = |amount: f32| {
        let pixels = painter("oil", Texture::Panel)
            .paint(&[Painted::new(
                "a",
                with(region("#30343a"), &[("edge", amount)]),
            )])
            .to_vec();
        let rows = top_edge(&pixels);
        let mean = float(rows.iter().sum::<usize>()) / float(rows.len());
        rows.iter()
            .map(|row| (float(*row) - mean).abs())
            .sum::<f32>()
            / float(rows.len())
    };
    let (clean, broken) = (edge(0.0), edge(1.0));
    assert!(broken > clean + 0.5, "clean {clean} vs broken {broken}");
}

/// A gessoed panel has tooth but no weave: shifted along a row, it never
/// lines up with itself the way woven linen does at its thread pitch.
#[test]
fn a_panel_ground_has_tooth_but_no_repeating_weave() {
    let difference = |field: &Heightfield, shift: usize| {
        let (mut sum, mut count) = (0.0, 0_usize);
        for y in 0..field.height {
            for x in 0..field.width - shift {
                sum += (field.at(x, y) - field.at(x + shift, y)).abs();
                count += 1;
            }
        }
        sum / float(count)
    };
    let closest = |texture: Texture| {
        let field = Heightfield::new(160, 120, texture, 1.0, 7);
        let far = difference(&field, 64);
        assert!(far > 0.02, "{texture:?} has tooth: {far}");
        (3..40)
            .map(|shift| difference(&field, shift) / far)
            .fold(f32::MAX, f32::min)
    };
    let (panel, linen) = (closest(Texture::Panel), closest(Texture::Linen));
    assert!(panel > 0.8, "panel repeats: {panel}");
    assert!(linen < panel, "linen {linen} vs panel {panel}");
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

/// A stroke cut to an outline paints only inside it, however far its brush
/// or its dabs reach; its twigs and dabs paint the same on every replay.
#[test]
fn a_clipped_stroke_stays_inside_its_outline() {
    let mut mark = stroke(&[(10.0, 60.0), (150.0, 60.0)], 30.0, "#203040", false);
    if let Mark::Stroke(stroke) = &mut mark {
        stroke.clip = vec![vec![
            (40.0, 30.0),
            (120.0, 30.0),
            (120.0, 90.0),
            (40.0, 90.0),
        ]];
        stroke.form = Some(Form::Dabs {
            count: 120,
            scatter: 30.0,
            length: 0.0,
            angle: None,
        });
    }
    for engine in ["oil", "watercolour"] {
        let mut first = painter(engine, Texture::Linen);
        let pixels = first.paint(&[Painted::new("moss", mark.clone())]).to_vec();
        let mut inside = 0;
        for y in 0..120 {
            for x in 0..160 {
                let a = alpha(&pixels, x, y);
                if (38..=122).contains(&x) && (28..=92).contains(&y) {
                    inside += usize::from(a > 0);
                } else {
                    assert_eq!(a, 0, "{engine}: paint at {x},{y} outside the clip");
                }
            }
        }
        assert!(inside > 1000, "{engine}: {inside} cells painted inside");
        let mut again = painter(engine, Texture::Linen);
        assert_eq!(
            again.paint(&[Painted::new("moss", mark.clone())]),
            pixels.as_slice()
        );
    }
}

/// How much neighbouring cells of a patch differ, on average, in 8-bit
/// steps of the summed channels: the fine grain a weave prints, not the
/// broad variation of a stroke's bristles.
fn grain_over(pixels: &[u8], xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f32 {
    let value = |x: usize, y: usize| rgb(pixels, x, y).iter().map(|v| f32::from(*v)).sum::<f32>();
    let mut total = 0.0;
    let mut count = 0.0;
    for y in ys {
        for x in xs.clone() {
            total += (value(x + 1, y) - value(x, y)).abs() + (value(x, y + 1) - value(x, y)).abs();
            count += 2.0;
        }
    }
    total / count
}

/// A loaded brush floods the weave, so a light stroke over a dark passage is
/// even paint, not the canvas grid printed in two colours; the same brush
/// run dry catches the tooth and breaks.
#[test]
fn a_loaded_oil_brush_floods_the_weave_and_a_dry_one_breaks_on_it() {
    let paint_over = |load: f32| {
        let mut painter = painter("oil", Texture::Linen);
        painter
            .paint(&[
                Painted::new("dark", band("#202838", 10.0, 150.0, false)),
                Painted::new(
                    "light",
                    with(
                        stroke(&[(10.0, 60.0), (150.0, 60.0)], 40.0, "#e8e0d0", false),
                        &[("load", load), ("streak", 0.0), ("relief", 0.0)],
                    ),
                ),
            ])
            .to_vec()
    };
    let loaded = grain_over(&paint_over(0.6), 40..120, 52..68);
    let dry = grain_over(&paint_over(0.12), 40..120, 52..68);
    assert!(loaded < 10.0, "a loaded stroke lies even: {loaded}");
    assert!(dry > loaded * 3.0, "a dry brush breaks: {dry} vs {loaded}");
}

/// A dry brush dragged over dark paint leaves broken light: some of the
/// dark shows through all along the stroke rather than the bristles'
/// overlapping touches building up to a solid patch.
#[test]
fn a_dry_brush_scumbles_broken_colour_instead_of_a_solid_patch() {
    let pixels = painter("oil", Texture::Linen)
        .paint(&[
            Painted::new("dark", band("#202838", 10.0, 150.0, false)),
            Painted::new(
                "scumble",
                with(
                    stroke(&[(10.0, 60.0), (150.0, 60.0)], 30.0, "#e8e0d0", false),
                    &[("load", 0.2)],
                ),
            ),
        ])
        .to_vec();
    let lights: Vec<f32> = (54..66)
        .flat_map(|y| (30..130).map(move |x| (x, y)))
        .map(|(x, y)| {
            rgb(&pixels, x, y)
                .iter()
                .map(|v| f32::from(*v))
                .sum::<f32>()
                / 765.0
        })
        .collect();
    let lit = lights.iter().filter(|value| **value > 0.5).count();
    let share = float(lit) / float(lights.len());
    assert!(
        (0.1..0.8).contains(&share),
        "a scumble covers some of the dark, not none or all: {share}"
    );
}

/// Fresh oil stays workable however thinly it went on: a blender dragged
/// across a thin wet stroke spreads it as far as it spreads a loaded one.
#[test]
fn thin_fresh_oil_is_as_workable_as_loaded_oil() {
    let softened = |load: f32| {
        let mut painter = painter("oil", Texture::Smooth);
        let pixels = painter
            .paint(&[
                Painted::new("ground", band("#e0d8c8", 10.0, 150.0, false)),
                Painted::new(
                    "line",
                    with(
                        stroke(&[(20.0, 60.0), (140.0, 60.0)], 10.0, "#202838", true),
                        &[("load", load)],
                    ),
                ),
                Painted::new(
                    "blend",
                    with(
                        stroke(&[(80.0, 30.0), (80.0, 90.0)], 30.0, "#000000", true),
                        &[("blender", 1.0)],
                    ),
                ),
            ])
            .to_vec();
        // How far the dark is dragged off the line, above and below it.
        (40..54)
            .chain(67..80)
            .map(|y| {
                765.0
                    - rgb(&pixels, 80, y)
                        .iter()
                        .map(|v| f32::from(*v))
                        .sum::<f32>()
            })
            .sum::<f32>()
    };
    let (thin, loaded) = (softened(0.08), softened(1.0));
    assert!(thin > loaded * 0.5, "thin {thin} vs loaded {loaded}");
}

fn eraser(points: &[(f32, f32)], size: f32, strength: f32) -> Erase {
    Erase {
        points: points
            .iter()
            .map(|(x, y)| StrokePoint::new(*x, *y, 1.0))
            .collect(),
        size,
        strength,
        hardness: 1.0,
        clip: Vec::new(),
    }
}

/// An eraser across the middle of a stroke clears it there and leaves its
/// ends; half strength takes off about half; paint laid after the eraser
/// shows where it cleared.
#[test]
fn an_eraser_lifts_the_paint_beneath_it_and_not_what_comes_after() {
    let line = stroke(&[(10.0, 60.0), (150.0, 60.0)], 20.0, "#7a4a8c", false);
    let across = eraser(&[(80.0, 20.0), (80.0, 100.0)], 24.0, 1.0);
    for engine in ["oil", "watercolour"] {
        let painted = painter(engine, Texture::Linen)
            .paint(&[Painted::new("line", line.clone())])
            .to_vec();
        let full = f32::from(alpha(&painted, 80, 60));
        assert!(full > 100.0, "{engine}: painted {full}");

        let erased = painter(engine, Texture::Linen)
            .paint(&[
                Painted::new("line", line.clone()),
                Painted::new("rub", Mark::Erase(across.clone())),
            ])
            .to_vec();
        assert!(alpha(&erased, 80, 60) <= 2, "{engine}: middle left");
        for x in [30, 130] {
            assert_eq!(
                alpha(&erased, x, 60),
                alpha(&painted, x, 60),
                "{engine}: end at {x}"
            );
        }

        let half = painter(engine, Texture::Linen)
            .paint(&[
                Painted::new("line", line.clone()),
                Painted::new(
                    "rub",
                    Mark::Erase(eraser(&[(80.0, 20.0), (80.0, 100.0)], 24.0, 0.5)),
                ),
            ])
            .to_vec();
        let ratio = f32::from(alpha(&half, 80, 60)) / full;
        assert!(
            (0.3..=0.75).contains(&ratio),
            "{engine}: half strength left {ratio}"
        );

        let over = painter(engine, Texture::Linen)
            .paint(&[
                Painted::new("line", line.clone()),
                Painted::new("rub", Mark::Erase(across.clone())),
                Painted::new(
                    "mend",
                    stroke(&[(80.0, 30.0), (80.0, 90.0)], 10.0, "#2a6a3a", false),
                ),
            ])
            .to_vec();
        assert!(alpha(&over, 80, 60) > 100, "{engine}: later paint hidden");
    }
}

/// An eraser cut to an outline lifts paint only inside it.
#[test]
fn a_clipped_eraser_leaves_paint_outside_its_outline() {
    let line = stroke(&[(10.0, 60.0), (150.0, 60.0)], 20.0, "#7a4a8c", false);
    let mut along = eraser(&[(0.0, 60.0), (160.0, 60.0)], 40.0, 1.0);
    along.clip = vec![vec![(0.0, 0.0), (80.0, 0.0), (80.0, 120.0), (0.0, 120.0)]];
    for engine in ["oil", "watercolour"] {
        let painted = painter(engine, Texture::Linen)
            .paint(&[Painted::new("line", line.clone())])
            .to_vec();
        let erased = painter(engine, Texture::Linen)
            .paint(&[
                Painted::new("line", line.clone()),
                Painted::new("rub", Mark::Erase(along.clone())),
            ])
            .to_vec();
        assert!(
            alpha(&erased, 40, 60) <= 2,
            "{engine}: inside the clip left"
        );
        assert!(
            alpha(&erased, 120, 60) > 100,
            "{engine}: outside the clip lifted"
        );
        assert_eq!(
            alpha(&erased, 120, 60),
            alpha(&painted, 120, 60),
            "{engine}"
        );
    }
}
