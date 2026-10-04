//! Working with areas: selecting, using the selection, masks, clipping and
//! combined shapes, applied as an agent or the palette applies them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "test assertions over exact, hand-written fixtures and the pixels they draw"
)]

use std::cell::RefCell;
use std::rc::Rc;

use canvas::actions::{AddLayer, Draw, EditLayer, NewMark, NewStroke, Paint, RemoveLayer};
use canvas::model::{Artwork, Shape};
use canvas::region::Region;
use canvas::render::{Options, Renderer};
use canvas::selection::{Combine, Select, UseSelection};
use canvas::{ArtworkResource, CanvasAction, view_ops};
use ui::prelude::{Invoke, Replica};
use ui::tilo_resource::ActionRef;
use ui::vello::kurbo::{BezPath, Point, Rect};

fn apply(art: &mut Replica<ArtworkResource>, action: CanvasAction) -> Result<(), String> {
    art.apply(Invoke::apply(&action).unwrap())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn select(art: &mut Replica<ArtworkResource>, select: Select) {
    apply(art, CanvasAction::Select(Box::new(select))).unwrap();
}

fn rect(id: &str, [x, y, width, height]: [f64; 4], fill: &str) -> NewMark {
    NewMark {
        id: Some(id.into()),
        fill: Some(fill.into()),
        ..NewMark::new(Shape::Rect {
            x,
            y,
            width,
            height,
            radius: 0.0,
        })
    }
}

fn draw(art: &mut Replica<ArtworkResource>, layer: Option<&str>, marks: Vec<NewMark>) {
    apply(
        art,
        CanvasAction::Draw(Draw {
            layer: layer.map(Into::into),
            marks,
        }),
    )
    .unwrap();
}

fn selected(art: &Artwork) -> Region {
    art.selection
        .as_ref()
        .expect("something is selected")
        .region()
}

/// The pixels of the artwork as straight RGBA, one per unit, with the
/// ground left out.
fn pixels(art: &Artwork) -> (Vec<u8>, usize) {
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let options = Options {
        bare: true,
        ..Options::default()
    };
    let (rgba, width, _) = view_ops::pixels(art.bounds(), 1.0, true, |scene, to| {
        renderer.draw(art, scene, to, &mut text, &options);
    })
    .unwrap();
    (rgba, usize::try_from(width).unwrap())
}

fn alpha((rgba, width): &(Vec<u8>, usize), x: usize, y: usize) -> u8 {
    rgba[(y * width + x) * 4 + 3]
}

fn red((rgba, width): &(Vec<u8>, usize), x: usize, y: usize) -> u8 {
    rgba[(y * width + x) * 4]
}

/// Shapes, lassos and marks select; add, subtract and intersect change the
/// selection; grow, feather and invert refine it; none clears it. Each is
/// one step, undone like any other.
#[test]
fn a_selection_is_made_of_shapes_and_refined() {
    let mut art = Replica::<ArtworkResource>::new();
    select(
        &mut art,
        Select {
            rect: Some([100.0, 100.0, 200.0, 100.0]),
            ..Select::default()
        },
    );
    let area = |art: &Replica<ArtworkResource>| selected(&art.state().unwrap()).area();
    assert!((area(&art) - 20_000.0).abs() < 1.0, "{}", area(&art));
    select(
        &mut art,
        Select {
            ellipse: Some([300.0, 100.0, 100.0, 100.0]),
            mode: Some("add".into()),
            ..Select::default()
        },
    );
    let disc = std::f64::consts::PI * 50.0 * 50.0;
    assert!(
        (area(&art) - 20_000.0 - disc).abs() < 10.0,
        "{}",
        area(&art)
    );
    select(
        &mut art,
        Select {
            points: Some(vec![[100.0, 100.0], [200.0, 100.0], [100.0, 200.0]]),
            mode: Some("subtract".into()),
            ..Select::default()
        },
    );
    assert!(
        (area(&art) - 15_000.0 - disc).abs() < 10.0,
        "{}",
        area(&art)
    );
    select(
        &mut art,
        Select {
            rect: Some([0.0, 0.0, 250.0, 900.0]),
            mode: Some("intersect".into()),
            ..Select::default()
        },
    );
    assert!((area(&art) - 10_000.0).abs() < 1.0, "{}", area(&art));

    select(
        &mut art,
        Select {
            grow: Some(10.0),
            feather: Some(6.0),
            ..Select::default()
        },
    );
    let state = art.state().unwrap();
    assert_eq!(state.selection.as_ref().unwrap().feather, 6.0);
    let bounds = selected(&state).bounds().unwrap();
    assert!((bounds.x0 - 90.0).abs() < 0.5 && (bounds.y1 - 210.0).abs() < 0.5);

    select(
        &mut art,
        Select {
            invert: Some(true),
            ..Select::default()
        },
    );
    let state = art.state().unwrap();
    let inverted = selected(&state);
    assert!(!inverted.contains(Point::new(150.0, 150.0)));
    assert!(inverted.contains(Point::new(600.0, 600.0)));
    assert_eq!(state.selection.as_ref().unwrap().feather, 6.0, "kept");

    // A selection outside the artwork is cut to it.
    select(
        &mut art,
        Select {
            rect: Some([-50.0, -50.0, 100.0, 100.0]),
            ..Select::default()
        },
    );
    assert!((area(&art) - 2_500.0).abs() < 1.0);

    select(
        &mut art,
        Select {
            mode: Some("none".into()),
            ..Select::default()
        },
    );
    assert!(art.state().unwrap().selection.is_none());
    let error = apply(&mut art, CanvasAction::Select(Box::default())).unwrap_err();
    assert!(error.contains("give rect"), "{error}");
}

/// Marks select by their shape: a filled shape its inside, a brush stroke
/// its ink; a group all of its members.
#[test]
fn marks_select_by_their_shape() {
    let mut art = Replica::<ArtworkResource>::new();
    draw(
        &mut art,
        None,
        vec![
            NewMark {
                group: Some("pair".into()),
                ..rect("a", [0.0, 0.0, 100.0, 100.0], "#ff0000")
            },
            NewMark {
                group: Some("pair".into()),
                ..rect("b", [200.0, 0.0, 100.0, 100.0], "#00ff00")
            },
        ],
    );
    select(
        &mut art,
        Select {
            marks: Some(vec!["pair".into()]),
            ..Select::default()
        },
    );
    assert!((selected(&art.state().unwrap()).area() - 20_000.0).abs() < 1.0);
    let error = apply(
        &mut art,
        CanvasAction::Select(Box::new(Select {
            marks: Some(vec!["nothing".into()]),
            ..Select::default()
        })),
    )
    .unwrap_err();
    assert!(error.contains("no mark or group"), "{error}");
}

/// The selection becomes ordinary marks: a fill, feathered as the
/// selection is, or an outline.
#[test]
fn a_selection_fills_and_outlines_as_marks() {
    let mut art = Replica::<ArtworkResource>::new();
    let error = apply(
        &mut art,
        CanvasAction::UseSelection(UseSelection {
            what: "fill".into(),
            ..UseSelection::default()
        }),
    )
    .unwrap_err();
    assert!(error.contains("nothing is selected"), "{error}");
    select(
        &mut art,
        Select {
            ellipse: Some([100.0, 100.0, 200.0, 200.0]),
            feather: Some(8.0),
            ..Select::default()
        },
    );
    for (what, id) in [("fill", "glow"), ("stroke", "ring")] {
        apply(
            &mut art,
            CanvasAction::UseSelection(UseSelection {
                what: what.into(),
                colour: Some("#ffcc00".into()),
                id: Some(id.into()),
                ..UseSelection::default()
            }),
        )
        .unwrap();
    }
    let state = art.state().unwrap();
    let glow = state.mark("glow").unwrap();
    assert_eq!(glow.style.fill.as_deref(), Some("#ffcc00"));
    assert_eq!(glow.style.feather, Some(8.0));
    assert!(glow.hit(Point::new(200.0, 200.0)));
    let ring = state.mark("ring").unwrap();
    assert_eq!(ring.style.stroke.as_deref(), Some("#ffcc00"));
    assert_eq!(ring.style.stroke_width, 2.0);
    let error = apply(
        &mut art,
        CanvasAction::UseSelection(UseSelection {
            what: "smudge".into(),
            ..UseSelection::default()
        }),
    )
    .unwrap_err();
    assert!(error.contains("fill, stroke, mask or clear"), "{error}");
}

/// A layer's mask is a layer of grey marks: masking to the selection shows
/// only it, clearing the selection hides it, inverting swaps them, and the
/// mask can be switched off or removed. Pixels of the layer are never
/// touched.
#[test]
fn a_mask_shows_and_hides_its_layer() {
    let mut art = Replica::<ArtworkResource>::new();
    draw(
        &mut art,
        None,
        vec![rect("sheet", [0.0, 0.0, 400.0, 300.0], "#ff0000")],
    );
    select(
        &mut art,
        Select {
            rect: Some([100.0, 100.0, 100.0, 100.0]),
            ..Select::default()
        },
    );
    apply(
        &mut art,
        CanvasAction::UseSelection(UseSelection {
            what: "mask".into(),
            ..UseSelection::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let mask = state.mask_of("layer-1").expect("the layer has a mask");
    assert_eq!(mask.id, "layer-1-mask");
    assert_eq!(mask.base, 0.0);
    assert_eq!(
        state.top_layer().unwrap().id,
        "layer-1",
        "a mask is not drawn on"
    );
    let shown = pixels(&state);
    assert!(
        alpha(&shown, 150, 150) > 250,
        "inside the selection it shows"
    );
    assert!(alpha(&shown, 50, 50) < 5, "outside it is hidden");

    apply(
        &mut art,
        CanvasAction::EditLayer(EditLayer {
            id: "layer-1".into(),
            invert: Some(true),
            ..EditLayer::default()
        }),
    )
    .unwrap();
    let shown = pixels(&art.state().unwrap());
    assert!(
        alpha(&shown, 150, 150) < 5,
        "inverted, the selection is hidden"
    );
    assert!(alpha(&shown, 50, 50) > 250);

    // Off, the whole layer shows.
    apply(
        &mut art,
        CanvasAction::EditLayer(EditLayer {
            id: "layer-1-mask".into(),
            visible: Some(false),
            ..EditLayer::default()
        }),
    )
    .unwrap();
    let shown = pixels(&art.state().unwrap());
    assert!(alpha(&shown, 150, 150) > 250 && alpha(&shown, 50, 50) > 250);

    // Removed, and made again by clearing: everything shows but the cleared
    // selection.
    apply(
        &mut art,
        CanvasAction::EditLayer(EditLayer {
            id: "layer-1".into(),
            mask: Some("remove".into()),
            ..EditLayer::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert!(state.mask_of("layer-1").is_none());
    assert!(state.marks.iter().all(|mark| mark.layer == "layer-1"));
    apply(
        &mut art,
        CanvasAction::UseSelection(UseSelection {
            what: "clear".into(),
            ..UseSelection::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert_eq!(state.mask_of("layer-1").unwrap().base, 1.0);
    let shown = pixels(&state);
    assert!(alpha(&shown, 150, 150) < 5, "the cleared part is hidden");
    assert!(alpha(&shown, 50, 50) > 250);
    assert_eq!(
        state.mark("sheet").unwrap().shape,
        Shape::Rect {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 300.0,
            radius: 0.0
        },
        "the layer's own marks are untouched"
    );

    // Painting on the mask in grey hides more of the layer.
    apply(
        &mut art,
        CanvasAction::Paint(Paint {
            layer: Some("layer-1-mask".into()),
            strokes: vec![NewStroke {
                points: vec![vec![300.0, 20.0], vec![300.0, 280.0]],
                size: 20.0,
                colour: "#000000".into(),
                ..NewStroke::default()
            }],
        }),
    )
    .unwrap();
    let shown = pixels(&art.state().unwrap());
    assert!(alpha(&shown, 300, 150) < 5);

    // A mask is not painted in a medium; naming the mask to mask or clear
    // works on the layer it is part of.
    let error = apply(
        &mut art,
        CanvasAction::EditLayer(EditLayer {
            id: "layer-1-mask".into(),
            medium: Some("oil".into()),
            ..EditLayer::default()
        }),
    )
    .unwrap_err();
    assert!(error.contains("greys"), "{error}");
    apply(
        &mut art,
        CanvasAction::UseSelection(UseSelection {
            what: "clear".into(),
            layer: Some("layer-1-mask".into()),
            ..UseSelection::default()
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert!(
        state.mask_of("layer-1-mask").is_none() && state.layers.len() == 2,
        "no mask of the mask is made"
    );

    // The layer goes with its mask.
    apply(
        &mut art,
        CanvasAction::RemoveLayer(RemoveLayer {
            id: "layer-1".into(),
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert!(state.layers.is_empty() && state.marks.is_empty());
}

/// Paint and shapes cut to the selection stay inside it; the cut is a mark
/// of the selection's shape that draws nothing and is never picked.
#[test]
fn paint_cut_to_the_selection_stays_inside_it() {
    let mut art = Replica::<ArtworkResource>::new();
    select(
        &mut art,
        Select {
            rect: Some([100.0, 0.0, 100.0, 300.0]),
            ..Select::default()
        },
    );
    apply(
        &mut art,
        CanvasAction::Paint(Paint {
            layer: None,
            strokes: vec![NewStroke {
                id: Some("swash".into()),
                points: vec![vec![20.0, 150.0], vec![380.0, 150.0]],
                size: 30.0,
                colour: "#0000ff".into(),
                clip: Some("selection".into()),
                ..NewStroke::default()
            }],
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let swash = state.mark("swash").unwrap();
    let cut = state.mark(&swash.clip).expect("cut to a mark");
    assert!(!cut.inked(), "the cut draws nothing");
    assert_eq!(
        state.mark_at(Point::new(150.0, 20.0)),
        None,
        "nor is it picked"
    );
    let shown = pixels(&state);
    assert!(alpha(&shown, 150, 150) > 200, "inside the selection");
    assert!(alpha(&shown, 50, 150) < 5, "not outside it");

    // A second passage in the same selection uses the same cut.
    apply(
        &mut art,
        CanvasAction::Paint(Paint {
            layer: None,
            strokes: vec![NewStroke {
                id: Some("again".into()),
                points: vec![vec![20.0, 50.0], vec![380.0, 50.0]],
                size: 10.0,
                colour: "#0000ff".into(),
                clip: Some("selection".into()),
                ..NewStroke::default()
            }],
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    assert_eq!(state.mark("again").unwrap().clip, swash.clip);
    assert_eq!(state.marks.len(), 3);
}

/// A layer clipped to the one below shows only where that one has paint.
#[test]
fn a_clipped_layer_shows_only_over_the_layer_below() {
    let mut art = Replica::<ArtworkResource>::new();
    draw(
        &mut art,
        None,
        vec![rect("disc", [100.0, 100.0, 100.0, 100.0], "#ff0000")],
    );
    apply(
        &mut art,
        CanvasAction::AddLayer(AddLayer {
            id: Some("shade".into()),
            name: "Shade".into(),
            ..AddLayer::default()
        }),
    )
    .unwrap();
    draw(
        &mut art,
        Some("shade"),
        vec![rect("band", [0.0, 140.0, 400.0, 20.0], "#0000ff")],
    );
    let shown = pixels(&art.state().unwrap());
    assert!(
        alpha(&shown, 20, 150) > 250,
        "unclipped, the band shows across"
    );
    apply(
        &mut art,
        CanvasAction::EditLayer(EditLayer {
            id: "shade".into(),
            clip: Some(true),
            ..EditLayer::default()
        }),
    )
    .unwrap();
    let shown = pixels(&art.state().unwrap());
    assert!(alpha(&shown, 20, 150) < 5, "clipped, not past the disc");
    assert!(red(&shown, 150, 150) < 20, "over the disc it shows");
    assert!(
        red(&shown, 150, 120) > 240,
        "the disc beside it is untouched"
    );
}

fn area_of(art: &Artwork, id: &str) -> f64 {
    let Shape::Path { d } = &art.mark(id).unwrap().shape else {
        panic!("{id} is a path");
    };
    Region::of_path(&BezPath::from_svg(d).unwrap()).area()
}

/// Shapes combine into one path mark in one step, in the bottom shape's
/// place and style.
#[test]
fn shapes_combine_into_one_path() {
    let cases = [
        ("union", 100.0 * 100.0 * 2.0 - 50.0 * 100.0),
        ("subtract", 50.0 * 100.0),
        ("intersect", 50.0 * 100.0),
        ("exclude", 100.0 * 100.0),
    ];
    for (op, area) in cases {
        let mut art = Replica::<ArtworkResource>::new();
        draw(
            &mut art,
            None,
            vec![
                rect("left", [0.0, 0.0, 100.0, 100.0], "#ff0000"),
                rect("right", [50.0, 0.0, 100.0, 100.0], "#00ff00"),
                rect("other", [300.0, 0.0, 10.0, 10.0], "#0000ff"),
            ],
        );
        let steps = art.log().len();
        apply(
            &mut art,
            CanvasAction::Combine(Combine {
                ids: vec!["right".into(), "left".into()],
                op: op.into(),
            }),
        )
        .unwrap();
        let state = art.state().unwrap();
        assert_eq!(art.log().len(), steps + 1, "one step");
        assert!(state.mark("right").is_none());
        let left = state.mark("left").unwrap();
        assert_eq!(
            left.style.fill.as_deref(),
            Some("#ff0000"),
            "the bottom's style"
        );
        assert!(
            (area_of(&state, "left") - area).abs() < 1.0,
            "{op}: {}",
            area_of(&state, "left")
        );
        assert_eq!(state.marks.first().unwrap().id, "left", "in its place");
    }
    // Curves are fitted again: a disc stays a few curves, not a polygon.
    let mut art = Replica::<ArtworkResource>::new();
    draw(
        &mut art,
        None,
        vec![
            NewMark {
                id: Some("o".into()),
                fill: Some("#000000".into()),
                ..NewMark::new(Shape::Ellipse {
                    cx: 100.0,
                    cy: 100.0,
                    rx: 80.0,
                    ry: 80.0,
                })
            },
            rect("cut", [0.0, 90.0, 200.0, 20.0], "#000000"),
        ],
    );
    apply(
        &mut art,
        CanvasAction::Combine(Combine {
            ids: vec!["o".into(), "cut".into()],
            op: "subtract".into(),
        }),
    )
    .unwrap();
    let state = art.state().unwrap();
    let Shape::Path { d } = &state.mark("o").unwrap().shape else {
        panic!("a path")
    };
    assert!(d.matches('C').count() >= 4 && d.len() < 1500, "{d}");
    let disc = std::f64::consts::PI * 80.0 * 80.0;
    let area = area_of(&state, "o");
    assert!(area < disc - 2_000.0 && area > disc - 4_000.0, "{area}");

    let error = apply(
        &mut art,
        CanvasAction::Combine(Combine {
            ids: vec!["o".into()],
            op: "union".into(),
        }),
    )
    .unwrap_err();
    assert!(error.contains("two or more"), "{error}");
}

/// The magic wand finds like colour touching a point, as the window shows
/// the artwork; selecting by colour finds it everywhere. Each becomes the
/// plain area it found, so every replica applies the same selection.
#[test]
fn the_wand_finds_colour_where_the_artwork_is_shown() {
    let mut art = Replica::<ArtworkResource>::new();
    draw(
        &mut art,
        None,
        vec![
            rect("a", [100.0, 100.0, 200.0, 100.0], "#d03020"),
            rect("b", [600.0, 100.0, 100.0, 100.0], "#d03020"),
            rect("c", [100.0, 400.0, 300.0, 100.0], "#2040c0"),
        ],
    );
    let wand: ActionRef<ArtworkResource, ([f64; 2], f64)> =
        ActionRef::new("select", &["at", "tolerance"]);
    let error = art
        .apply(wand.with(([150.0, 150.0], 0.1)))
        .unwrap_err()
        .to_string();
    assert!(error.contains("none is open"), "{error}");

    let renderer = Rc::new(RefCell::new(Renderer::default()));
    canvas::author::see(&renderer, &Rc::new(art.state().unwrap()));
    art.apply(wand.with(([150.0, 150.0], 0.1))).unwrap();
    let state = art.state().unwrap();
    let found = selected(&state);
    let area = found.area();
    assert!((area - 20_000.0).abs() < 600.0, "{area}");
    assert!(
        !found.contains(Point::new(650.0, 150.0)),
        "b does not touch"
    );
    let d = &state.selection.as_ref().unwrap().d;
    assert!(d.len() < 400, "a rectangle traces to a few corners: {d}");

    let by_colour: ActionRef<ArtworkResource, String> = ActionRef::new("select", &["colour"]);
    art.apply(by_colour.with("#d03020".into())).unwrap();
    let found = selected(&art.state().unwrap());
    assert!(found.contains(Point::new(650.0, 150.0)));
    assert!(!found.contains(Point::new(200.0, 450.0)));
    let bounds = found.bounds().unwrap();
    assert!(bounds.union(Rect::new(100.0, 100.0, 700.0, 200.0)).area() < 61_000.0);

    // Applied as a plain value, a colour cannot be found.
    let error = apply(
        &mut art,
        CanvasAction::Select(Box::new(Select {
            at: Some([10.0, 10.0]),
            ..Select::default()
        })),
    )
    .unwrap_err();
    assert!(error.contains("select action itself"), "{error}");
}
