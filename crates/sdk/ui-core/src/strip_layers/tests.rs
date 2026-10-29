use super::*;
use vello::peniko::Color;

const SIZE: (u32, u32) = (1000, 600);

fn scene(color: Color) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color,
        None,
        &Rect::new(10.0, 10.0, 50.0, 50.0),
    );
    scene
}

fn strip(scroll_x: u32) -> StripBlit {
    StripBlit {
        dest_x: 100,
        dest_y: 50,
        dest_w: 800,
        dest_h: 400,
        scroll_x,
    }
}

fn cache(drawn: Color, strips: Vec<StripBlit>) -> FrameCache {
    let strips_drawn: Vec<Scene> = strips.iter().map(|_| scene(drawn)).collect();
    let strip_parts = strips_drawn.iter().map(|_| None).collect();
    FrameCache {
        width: 3000,
        strips,
        generation: 1,
        chrome_damage: None,
        layers: FrameLayers {
            backdrop: scene(Color::BLACK),
            workspace: Scene::new(),
            strips: strips_drawn,
            strip_parts,
            chrome: scene(Color::WHITE),
        },
    }
}

#[test]
fn nothing_shown_renders_every_layer_and_the_whole_output() {
    let work = plan(None, &cache(Color::BLACK, vec![strip(40)]), SIZE, true);
    assert_eq!(work, LayerWork::whole(SIZE));
    assert_eq!(work.raster_pixels(SIZE), 1_800_000);
}

#[test]
fn a_pan_copies_what_stays_and_rasterises_what_it_uncovered() {
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 7);
    let work = plan(
        Some(&shown),
        &cache(Color::BLACK, vec![strip(52)]),
        SIZE,
        true,
    );
    assert!(!work.backdrop && !work.chrome);
    assert_eq!(
        work.workspace,
        WorkspaceWork::Pan {
            copies: vec![BandCopy {
                src_x: 112,
                dst_x: 100,
                y: 50,
                width: 788,
                height: 400,
            }],
            uncovered: vec![FineDamageRect {
                x: 888,
                y: 50,
                width: 12,
                height: 400,
            }],
        }
    );
    assert_eq!(
        work.composite,
        Some(FineDamageRect {
            x: 100,
            y: 50,
            width: 800,
            height: 400,
        })
    );
    assert_eq!(work.raster_pixels(SIZE), 4_800);

    let back = plan(
        Some(&shown),
        &cache(Color::BLACK, vec![strip(30)]),
        SIZE,
        true,
    );
    let WorkspaceWork::Pan { copies, uncovered } = back.workspace else {
        panic!("a pan back is a pan: {back:?}");
    };
    assert_eq!(
        copies,
        vec![BandCopy {
            src_x: 100,
            dst_x: 110,
            y: 50,
            width: 790,
            height: 400,
        }]
    );
    assert_eq!(
        uncovered,
        vec![FineDamageRect {
            x: 100,
            y: 50,
            width: 10,
            height: 400,
        }]
    );
}

#[test]
fn a_pan_wider_than_the_strip_rasterises_the_strip() {
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(0)]), 1);
    let work = plan(
        Some(&shown),
        &cache(Color::BLACK, vec![strip(900)]),
        SIZE,
        true,
    );
    assert_eq!(
        work.workspace,
        WorkspaceWork::Pan {
            copies: Vec::new(),
            uncovered: vec![FineDamageRect {
                x: 100,
                y: 50,
                width: 800,
                height: 400,
            }],
        }
    );
}

#[test]
fn a_changed_strip_rasterises_its_destination_and_geometry_the_layer() {
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
    let content = plan(
        Some(&shown),
        &cache(Color::WHITE, vec![strip(52)]),
        SIZE,
        true,
    );
    assert_eq!(
        content.workspace,
        WorkspaceWork::Pan {
            copies: Vec::new(),
            uncovered: vec![FineDamageRect {
                x: 100,
                y: 50,
                width: 800,
                height: 400,
            }],
        }
    );
    let mut taller = strip(52);
    taller.dest_h = 420;
    let geometry = plan(Some(&shown), &cache(Color::BLACK, vec![taller]), SIZE, true);
    assert_eq!(geometry.workspace, WorkspaceWork::Whole);
    assert_eq!(geometry.composite, Some(full_rect(SIZE)));
}

/// A strip drawn as two window groups, each a damage part: one near the
/// canvas's left edge, one far right of the screen.
fn grouped(near: Color, far: Color) -> (Scene, Option<crate::scene_parts::SectionParts>) {
    let mut drawn = Scene::new();
    let ((), parts) = crate::scene_parts::record(&mut drawn, |drawn| {
        for (index, (x, color)) in [(130.0, near), (2400.0, far)].into_iter().enumerate() {
            let bounds = Rect::new(x, 60.0, x + 300.0, 300.0);
            let key = crate::scene_parts::PartKey::new("test.group", index);
            crate::scene_parts::part(drawn, key, vec![bounds], |drawn| {
                drawn.fill(Fill::NonZero, Affine::IDENTITY, color, None, &bounds);
            });
        }
    });
    (drawn, parts)
}

fn grouped_cache(near: Color, far: Color, scroll_x: u32) -> FrameCache {
    let (drawn, parts) = grouped(near, far);
    let mut frame = cache(Color::BLACK, vec![strip(scroll_x)]);
    frame.layers.strips = vec![drawn];
    frame.layers.strip_parts = vec![parts];
    frame
}

/// A window group off screen that changed while the strip rested (an app
/// presenting its first content unseen) rasterises nothing; one on
/// screen rasterises its own bounds, in whole tiles inside the strip's
/// band, and nothing else of the strip. Panning while it changes copies
/// the strip as any pan does and rasterises the uncovered columns and
/// the group. A strip drawn without parts still rasterises its band.
#[test]
fn a_changed_window_group_rasterises_only_where_it_is_on_screen() {
    let shown = Shown::of(&grouped_cache(Color::WHITE, Color::WHITE, 40), 1);
    let far = plan(
        Some(&shown),
        &grouped_cache(Color::WHITE, Color::BLACK, 40),
        SIZE,
        true,
    );
    assert_eq!(far.workspace, WorkspaceWork::Unchanged);
    assert_eq!(far.composite, None);

    let near = plan(
        Some(&shown),
        &grouped_cache(Color::BLACK, Color::WHITE, 40),
        SIZE,
        true,
    );
    // The group changed without the strip moving: every row the band
    // holds is copied where it stands, and the group is drawn again on
    // the rows and columns it was drawn on.
    assert_eq!(
        near.workspace,
        WorkspaceWork::Pan {
            copies: vec![BandCopy {
                src_x: 100,
                dst_x: 100,
                y: 50,
                width: 800,
                height: 400,
            }],
            uncovered: vec![FineDamageRect {
                x: 100,
                y: 50,
                width: 300,
                height: 254,
            }],
        }
    );

    let panned = plan(
        Some(&shown),
        &grouped_cache(Color::BLACK, Color::WHITE, 52),
        SIZE,
        true,
    );
    let WorkspaceWork::Pan { copies, uncovered } = &panned.workspace else {
        panic!("a pan with a changed group copies: {:?}", panned.workspace);
    };
    // The run past the changed group's own columns copies every row the
    // pan keeps: that change is drawn where it stands, and the rows a run
    // copies are the run's.
    assert_eq!(
        copies,
        &vec![BandCopy {
            src_x: 112,
            dst_x: 100,
            y: 50,
            width: 788,
            height: 400,
        }],
        "{uncovered:?}"
    );
    // Canvas 130..430 shown 52 px left is 78..378, in tiles 64..384.
    assert!(
        uncovered
            .iter()
            .any(|rect| rect.x == 100 && rect.width == 284),
        "{uncovered:?}"
    );

    let mut unparted = grouped_cache(Color::BLACK, Color::WHITE, 40);
    unparted.layers.strip_parts = vec![None];
    let whole = plan(Some(&shown), &unparted, SIZE, true);
    assert_eq!(
        whole.workspace,
        WorkspaceWork::Pan {
            copies: Vec::new(),
            uncovered: vec![FineDamageRect {
                x: 100,
                y: 50,
                width: 800,
                height: 400,
            }],
        }
    );
}

/// A group at the strip's left edge changed while the strip panned: the
/// pan copies every row it keeps and draws the group again where it
/// stands. A change nowhere on screen is not drawn at all: no row moved
/// because of it, and nothing shows it.
#[test]
fn a_pan_copies_its_rows_and_draws_a_changed_group_where_it_stands() {
    // A group drawn at the canvas's left edge (110..410, shown at 10..310
    // with the strip 100 px right of the canvas), so the columns a pan
    // brings it to are the run a move of at most a strip's width leaves.
    let edge = |near: Color, far: Color| {
        let mut drawn = Scene::new();
        let ((), parts) = crate::scene_parts::record(&mut drawn, |drawn| {
            for (index, (x, color)) in [(10.0, near), (2400.0, far)].into_iter().enumerate() {
                let bounds = Rect::new(x, 60.0, x + 300.0, 300.0);
                let key = crate::scene_parts::PartKey::new("test.group", index);
                crate::scene_parts::part(drawn, key, vec![bounds], |drawn| {
                    drawn.fill(Fill::NonZero, Affine::IDENTITY, color, None, &bounds);
                });
            }
        });
        let mut frame = cache(Color::BLACK, vec![strip(0)]);
        frame.layers.strips = vec![drawn];
        frame.layers.strip_parts = vec![parts];
        frame
    };
    let shown = Shown::of(&edge(Color::WHITE, Color::WHITE), 1);
    // The strip moved 4 px left with the group at its edge changed: the
    // pan copies the rows it keeps and draws the group where it stands.
    let work = plan(Some(&shown), &edge(Color::BLACK, Color::WHITE), SIZE, true);
    let WorkspaceWork::Pan { copies, uncovered } = &work.workspace else {
        panic!("a pan: {work:?}");
    };
    assert_eq!(
        copies,
        &vec![BandCopy {
            src_x: 100,
            dst_x: 100,
            y: 50,
            width: 800,
            height: 400,
        }],
        "copies, with {uncovered:?}"
    );
    // The rows the change stands on are drawn again where it stands.
    assert_eq!(
        uncovered,
        &vec![FineDamageRect {
            x: 100,
            y: 50,
            width: 220,
            height: 254,
        }]
    );
    assert!(work.raster_pixels(SIZE) < 900 * 400);

    // A group the move does not reach changed with the strips beside it,
    // and nothing on screen shows that change: no row is copied and none
    // is drawn.
    let near = plan(Some(&shown), &edge(Color::WHITE, Color::BLACK), SIZE, true).workspace;
    assert_eq!(near, WorkspaceWork::Unchanged);
}

/// Two strips whose destinations meet (each draws its shadows past its
/// row): the upper one pans. Its own rows are copied; the band both
/// cover, where the lower strip's drawing stays put, is rasterised; the
/// lower strip's own rows are left alone.
#[test]
fn where_a_panning_strip_meets_a_still_one_the_shared_band_is_rasterised() {
    let mut lower = strip(0);
    lower.dest_y = 430;
    lower.dest_h = 150;
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(40), lower]), 1);
    let work = plan(
        Some(&shown),
        &cache(Color::BLACK, vec![strip(52), lower]),
        SIZE,
        true,
    );
    let WorkspaceWork::Pan { copies, uncovered } = &work.workspace else {
        panic!("a pan: {work:?}");
    };
    assert_eq!(
        copies,
        &vec![BandCopy {
            src_x: 112,
            dst_x: 100,
            y: 50,
            width: 788,
            height: 380,
        }]
    );
    assert_eq!(
        uncovered,
        &vec![
            FineDamageRect {
                x: 888,
                y: 50,
                width: 12,
                height: 380,
            },
            FineDamageRect {
                x: 100,
                y: 430,
                width: 800,
                height: 20,
            },
        ]
    );
    assert_eq!(work.raster_pixels(SIZE), 12 * 380 + 800 * 20);
    let both = plan(
        Some(&shown),
        &cache(
            Color::BLACK,
            vec![strip(52), {
                let mut moved = lower;
                moved.scroll_x = 12;
                moved
            }],
        ),
        SIZE,
        true,
    );
    let WorkspaceWork::Pan { copies, .. } = &both.workspace else {
        panic!("a pan: {both:?}");
    };
    assert_eq!(
        copies.iter().map(|copy| copy.height).sum::<u32>(),
        530,
        "strips moving together are copied through the band they share: {copies:?}"
    );
}

#[test]
fn what_no_strip_carries_drawn_in_place_keeps_strip_pixels_from_moving() {
    let shown_cache = {
        let mut cache = cache(Color::BLACK, vec![strip(40)]);
        cache.layers.workspace = scene(Color::WHITE);
        cache
    };
    let shown = Shown::of(&shown_cache, 1);
    let mut next = cache(Color::BLACK, vec![strip(52)]);
    next.layers.workspace = scene(Color::WHITE);
    let work = plan(Some(&shown), &next, SIZE, true);
    let WorkspaceWork::Pan { copies, uncovered } = work.workspace else {
        panic!("a pan: {work:?}");
    };
    assert!(copies.is_empty());
    assert_eq!(uncovered.len(), 1);
}

#[test]
fn an_unchanged_frame_composites_nothing_unless_the_output_lost_it() {
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
    let same = cache(Color::BLACK, vec![strip(40)]);
    let work = plan(Some(&shown), &same, SIZE, true);
    assert_eq!(work.workspace, WorkspaceWork::Unchanged);
    assert_eq!(work.composite, None);
    assert_eq!(work.raster_pixels(SIZE), 0);
    let lost = plan(Some(&shown), &same, SIZE, false);
    assert_eq!(lost.composite, Some(full_rect(SIZE)));
    assert_eq!(lost.raster_pixels(SIZE), 0);
}

#[test]
fn changed_chrome_rasterises_the_chrome_and_composites_the_output() {
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
    let mut next = cache(Color::BLACK, vec![strip(52)]);
    next.layers.chrome = scene(Color::BLACK);
    let work = plan(Some(&shown), &next, SIZE, true);
    assert!(work.chrome && !work.backdrop);
    assert_eq!(work.composite, Some(full_rect(SIZE)));
    assert!(matches!(work.workspace, WorkspaceWork::Pan { .. }));
}

/// Chrome that changed only in the tiles its damage names, from the
/// drawing the chrome texture holds, rasterises and composites those
/// tiles; measured from another drawing, it rasterises the whole layer.
#[test]
fn changed_chrome_from_the_drawing_held_rasterises_only_its_tiles() {
    let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
    let tiles = vec![
        FineDamageRect {
            x: 16,
            y: 0,
            width: 48,
            height: 32,
        },
        FineDamageRect {
            x: 900,
            y: 16,
            width: 64,
            height: 16,
        },
    ];
    let mut next = cache(Color::BLACK, vec![strip(40)]);
    next.generation = 2;
    next.layers.chrome = scene(Color::BLACK);
    next.chrome_damage = Some(ChromeDamage {
        since: 1,
        rects: tiles.clone(),
    });
    let work = plan(Some(&shown), &next, SIZE, true);
    assert!(work.chrome && !work.backdrop);
    assert_eq!(work.chrome_tiles, Some(tiles));
    assert_eq!(work.workspace, WorkspaceWork::Unchanged);
    assert_eq!(
        work.composite,
        Some(FineDamageRect {
            x: 16,
            y: 0,
            width: 948,
            height: 32,
        })
    );
    assert_eq!(work.raster_pixels(SIZE), 48 * 32 + 64 * 16);

    next.chrome_damage = Some(ChromeDamage {
        since: 7,
        rects: vec![],
    });
    let other = plan(Some(&shown), &next, SIZE, true);
    assert_eq!(other.chrome_tiles, None);
    assert_eq!(other.composite, Some(full_rect(SIZE)));
}

/// A warm that stopped while drawing a layer leaves that texture holding
/// nothing known: the next frame rasterises it and composites the whole
/// output, and what the warm finished stays as it is.
#[test]
fn a_layer_a_warm_left_unknown_is_rasterised_and_the_output_composited_whole() {
    let frame = cache(Color::BLACK, vec![strip(40)]);
    let mut shown = Shown::of(&frame, 3);
    shown.forget(Layer::Chrome);
    let work = plan(Some(&shown), &frame, SIZE, true);
    assert!(work.chrome && !work.backdrop);
    assert_eq!(work.workspace, WorkspaceWork::Unchanged);
    assert_eq!(work.composite, Some(full_rect(SIZE)));
    shown.record(Layer::Chrome, &frame);
    let warm = plan(Some(&shown), &frame, SIZE, true);
    assert!(warm.rasterises_nothing());
    assert_eq!(warm.composite, None);

    let mut nothing = Shown::unknown();
    let work = plan(Some(&nothing), &frame, SIZE, true);
    assert_eq!(work, LayerWork::whole(SIZE));
    for layer in [Layer::Backdrop, Layer::Chrome, Layer::Workspace] {
        nothing.record(layer, &frame);
    }
    let warmed = plan(Some(&nothing), &frame, SIZE, false);
    assert!(warmed.rasterises_nothing());
    assert_eq!(
        warmed.composite,
        Some(full_rect(SIZE)),
        "layers warmed at rest were never composited into the output"
    );
}

#[test]
fn warm_bands_cover_the_output_once_in_tile_rows() {
    assert_eq!(warm_bands((1024, 1024)), None);
    for size in [(1920, 1080), (2944, 1840), (3840, 2160), (7680, 4320)] {
        let bands = warm_bands(size).expect("a large output is warmed in bands");
        let mut y = 0_u32;
        for band in &bands {
            let end = band.y.saturating_add(band.height);
            assert_eq!((band.x, band.y, band.width), (0, y, size.0));
            assert!(band.height.is_multiple_of(WARM_BAND_ROWS) || end == size.1);
            assert!(
                area(*band)
                    <= WARM_BAND_PIXELS.saturating_add(
                        u64::from(size.0).saturating_mul(u64::from(WARM_BAND_ROWS))
                    )
            );
            y = end;
        }
        assert_eq!(y, size.1, "{size:?}: the bands cover the output");
    }
}

#[test]
fn a_composed_frame_draws_the_layers_in_order() {
    let frame = cache(Color::BLACK, vec![strip(40)]);
    let mut expected = frame.layers.backdrop.clone();
    append_panned(&mut expected, &frame.layers, &frame.strips);
    expected.append(&frame.layers.chrome, None);
    assert!(compose(&frame).encoding_eq(&expected));
}
