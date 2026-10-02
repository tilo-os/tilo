//! Looking at an artwork: what an agent sees after it paints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::float_cmp,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::actions::{AddLayer, Draw, NewMark, NewStroke, Paint};
use canvas::model::{Gradient, Shape};
use canvas::render::Renderer;
use canvas::resource::Look;
use canvas::{ArtworkResource, CanvasAction, view_ops};
use ui::prelude::{Invoke, Replica};

fn act(art: &mut Replica<ArtworkResource>, action: CanvasAction) {
    art.apply(Invoke::apply(&action).unwrap()).unwrap();
}

fn mark(id: &str, shape: Shape, fill: &str) -> NewMark {
    NewMark {
        id: Some(id.into()),
        name: None,
        shape,
        fill: Some(fill.into()),
        gradient: None,
        stroke: None,
        stroke_width: None,
        opacity: None,
        brush: None,
        angle: None,
        wet: None,
        params: None,
    }
}

fn stroke(points: &[[f64; 3]], size: f64, colour: &str, wet: bool) -> NewStroke {
    NewStroke {
        id: None,
        name: None,
        points: points.iter().map(|point| point.to_vec()).collect(),
        size,
        colour: colour.into(),
        opacity: None,
        wet: Some(wet),
        params: None,
    }
}

/// A small landscape in three media, painted through the vocabulary.
pub fn landscape() -> Replica<ArtworkResource> {
    let mut art = Replica::<ArtworkResource>::new();
    let layer = |id: &str, medium: &str| {
        CanvasAction::AddLayer(AddLayer {
            id: Some(id.into()),
            name: id.into(),
            medium: Some(medium.into()),
            ..AddLayer::default()
        })
    };
    act(&mut art, layer("sky", "watercolour-wet"));
    act(&mut art, layer("land", "oil"));
    act(&mut art, layer("ink", ""));
    act(
        &mut art,
        CanvasAction::Draw(Draw {
            layer: Some("sky".into()),
            marks: vec![
                mark(
                    "wash",
                    Shape::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1200.0,
                        height: 520.0,
                        radius: 0.0,
                    },
                    "pigment:cerulean",
                ),
                NewMark {
                    wet: Some(true),
                    ..mark(
                        "glow",
                        Shape::Ellipse {
                            cx: 820.0,
                            cy: 380.0,
                            rx: 380.0,
                            ry: 160.0,
                        },
                        "pigment:new-gamboge",
                    )
                },
            ],
        }),
    );
    act(
        &mut art,
        CanvasAction::Draw(Draw {
            layer: Some("land".into()),
            marks: vec![
                NewMark {
                    angle: Some(-4.0),
                    ..mark(
                        "far-hills",
                        Shape::Path {
                            d: "M 0 520 C 200 430 380 470 560 450 C 760 420 960 470 1200 440 L 1200 640 L 0 640 Z".into(),
                        },
                        "pigment:green-earth",
                    )
                },
                NewMark {
                    angle: Some(3.0),
                    ..mark(
                        "field",
                        Shape::Path {
                            d: "M 0 600 C 300 560 700 590 1200 570 L 1200 900 L 0 900 Z".into(),
                        },
                        "pigment:raw-umber",
                    )
                },
            ],
        }),
    );
    act(
        &mut art,
        CanvasAction::Paint(Paint {
            layer: Some("land".into()),
            strokes: vec![
                stroke(
                    &[
                        [300.0, 720.0, 1.0],
                        [600.0, 700.0, 0.9],
                        [900.0, 730.0, 0.7],
                    ],
                    26.0,
                    "pigment:yellow-ochre",
                    true,
                ),
                stroke(
                    &[
                        [140.0, 800.0, 0.8],
                        [520.0, 780.0, 1.0],
                        [1000.0, 810.0, 0.6],
                    ],
                    30.0,
                    "pigment:burnt-sienna",
                    true,
                ),
                stroke(
                    &[[420.0, 470.0, 1.0], [700.0, 455.0, 0.5]],
                    14.0,
                    "pigment:lead-white",
                    false,
                ),
            ],
        }),
    );
    act(
        &mut art,
        CanvasAction::Draw(Draw {
            layer: Some("ink".into()),
            marks: vec![
                NewMark {
                    gradient: Some(Gradient {
                        kind: "radial".into(),
                        from: [860.0, 210.0],
                        to: [860.0, 270.0],
                        stops: vec![(0.0, "#fff4c2".into()), (1.0, "#f2a541".into())],
                    }),
                    ..mark(
                        "sun",
                        Shape::Ellipse {
                            cx: 860.0,
                            cy: 210.0,
                            rx: 60.0,
                            ry: 60.0,
                        },
                        "#f2a541",
                    )
                },
                mark(
                    "title",
                    Shape::Text {
                        x: 60.0,
                        y: 860.0,
                        text: "Evening, three media".into(),
                        size: 36.0,
                    },
                    "#2a1d14",
                ),
            ],
        }),
    );
    act(
        &mut art,
        CanvasAction::Paint(Paint {
            layer: Some("ink".into()),
            strokes: vec![
                stroke(
                    &[
                        [180.0, 700.0, 1.0],
                        [186.0, 560.0, 0.8],
                        [176.0, 430.0, 0.3],
                    ],
                    14.0,
                    "#2a1d14",
                    false,
                ),
                stroke(
                    &[
                        [182.0, 520.0, 0.9],
                        [230.0, 470.0, 0.5],
                        [270.0, 440.0, 0.2],
                    ],
                    6.0,
                    "#2a1d14",
                    false,
                ),
                stroke(
                    &[
                        [180.0, 560.0, 0.9],
                        [130.0, 500.0, 0.5],
                        [100.0, 480.0, 0.2],
                    ],
                    6.0,
                    "#2a1d14",
                    false,
                ),
            ],
        }),
    );
    art
}

fn out(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/canvas-look");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A look renders the artwork the agent painted, at the size asked, and a
/// close look at a region magnifies it; both are real PNGs.
#[test]
fn a_look_shows_the_painting_whole_and_up_close() {
    let art = landscape().state().unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let (whole, info) = view_ops::look(&mut renderer, &mut text, &art, &Look::default()).unwrap();
    assert_eq!((whole.width, whole.height), (1024, 768), "{info}");
    assert!(whole.png.starts_with(b"\x89PNG"));
    std::fs::write(out("whole.png"), &whole.png).unwrap();

    let close = Look {
        region: Some([700.0, 100.0, 320.0, 240.0]),
        size: Some(640),
        grid: Some(true),
        layers: None,
    };
    let (detail, info) = view_ops::look(&mut renderer, &mut text, &art, &close).unwrap();
    assert_eq!((detail.width, detail.height), (640, 480));
    assert_eq!(info["pixels_per_unit"], 2.0);
    std::fs::write(out("detail.png"), &detail.png).unwrap();

    let gridded = Look {
        grid: Some(true),
        ..Look::default()
    };
    let (grid, _) = view_ops::look(&mut renderer, &mut text, &art, &gridded).unwrap();
    std::fs::write(out("grid.png"), &grid.png).unwrap();

    let outside = Look {
        region: Some([5000.0, 5000.0, 10.0, 10.0]),
        ..Look::default()
    };
    let error = view_ops::look(&mut renderer, &mut text, &art, &outside)
        .err()
        .unwrap();
    assert!(error.contains("outside the artwork"), "{error}");
}
