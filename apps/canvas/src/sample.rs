//! A sample artwork for screenshots and tests, painted through the same
//! actions a person or an agent uses. Never shown in the app a user opens.

use crate::CanvasAction;
use crate::actions::{AddLayer, Draw, NewMark, NewStroke, Paint};
use crate::model::{Gradient, Shape};

fn mark(id: &str, shape: Shape, fill: &str) -> NewMark {
    NewMark {
        id: Some(id.into()),
        fill: Some(fill.into()),
        ..NewMark::new(shape)
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
        ..NewStroke::default()
    }
}

/// A small landscape in three media, as the actions that paint it.
#[allow(
    clippy::too_many_lines,
    reason = "one composition's actions, read top to bottom"
)]
pub fn landscape() -> Vec<CanvasAction> {
    let mut art = Vec::new();
    let layer = |id: &str, medium: &str| {
        CanvasAction::AddLayer(AddLayer {
            id: Some(id.into()),
            name: id.into(),
            medium: Some(medium.into()),
            ..AddLayer::default()
        })
    };
    art.push(layer("sky", "watercolour-wet"));
    art.push(layer("land", "oil"));
    art.push(layer("ink", ""));
    art.push(CanvasAction::Draw(Draw {
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
    }));
    art.push(CanvasAction::Draw(Draw {
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
    art.push(CanvasAction::Paint(Paint {
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
    }));
    art.push(CanvasAction::Draw(Draw {
        layer: Some("ink".into()),
        marks: vec![
            NewMark {
                gradient: Some(Gradient {
                    kind: "radial".into(),
                    from: [860.0, 210.0],
                    to: [860.0, 270.0],
                    stops: vec![(0.0, "#fff4c2".into()), (1.0, "#f2a541".into())],
                    focal: None,
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
                    font: Some("serif".into()),
                    weight: None,
                    italic: true,
                    align: None,
                    line_height: None,
                    width: None,
                    tracking: None,
                    path: None,
                },
                "#2a1d14",
            ),
        ],
    }));
    art.push(CanvasAction::Paint(Paint {
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
    }));
    art
}

/// A small vector composition on one plain layer: shapes, a turned card, a
/// grouped flower, a curved leaf and a label, for screenshots of the
/// selection and path tools.
#[allow(
    clippy::too_many_lines,
    reason = "one composition's actions, read top to bottom"
)]
pub fn shapes() -> Vec<CanvasAction> {
    use ui::vello::kurbo::{Affine, Point};
    let turned = |degrees: f64, about: (f64, f64)| {
        Some(Affine::rotate_about(degrees.to_radians(), Point::new(about.0, about.1)).as_coeffs())
    };
    let mut marks = vec![
        NewMark {
            stroke: Some("#2a1d14".into()),
            stroke_width: Some(3.0),
            ..mark(
                "card",
                Shape::Rect {
                    x: 120.0,
                    y: 140.0,
                    width: 300.0,
                    height: 200.0,
                    radius: 24.0,
                },
                "#e9d8b4",
            )
        },
        NewMark {
            transform: turned(-12.0, (300.0, 620.0)),
            ..mark(
                "tilted",
                Shape::Rect {
                    x: 180.0,
                    y: 540.0,
                    width: 240.0,
                    height: 160.0,
                    radius: 0.0,
                },
                "#3d6f8f",
            )
        },
        mark(
            "leaf",
            Shape::Path {
                d: "M 620 700 C 640 560 760 470 900 450 C 880 600 780 690 620 700 Z".into(),
            },
            "#4f7a3a",
        ),
        NewMark {
            fill: Some("#2a1d14".into()),
            ..mark(
                "label",
                Shape::Text {
                    x: 120.0,
                    y: 820.0,
                    text: "Shapes, paths and words".into(),
                    size: 40.0,
                    font: Some("serif".into()),
                    weight: None,
                    italic: false,
                    align: None,
                    line_height: None,
                    width: None,
                    tracking: None,
                    path: None,
                },
                "#2a1d14",
            )
        },
    ];
    for petal in 0..6 {
        marks.push(NewMark {
            group: Some("flower".into()),
            transform: turned(f64::from(petal) * 60.0, (860.0, 230.0)),
            ..mark(
                &format!("petal-{petal}"),
                Shape::Ellipse {
                    cx: 860.0,
                    cy: 165.0,
                    rx: 26.0,
                    ry: 58.0,
                },
                "#e07a5f",
            )
        });
    }
    marks.push(NewMark {
        group: Some("flower".into()),
        ..mark(
            "heart",
            Shape::Ellipse {
                cx: 860.0,
                cy: 230.0,
                rx: 34.0,
                ry: 34.0,
            },
            "#f2cc8f",
        )
    });
    vec![
        CanvasAction::AddLayer(AddLayer {
            id: Some("ink".into()),
            name: "Ink".into(),
            ..AddLayer::default()
        }),
        CanvasAction::Draw(Draw {
            layer: Some("ink".into()),
            marks,
        }),
    ]
}
