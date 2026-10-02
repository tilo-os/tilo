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
