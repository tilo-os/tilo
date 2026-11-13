use vello::kurbo::{Point, Rect};

use super::{Drawing, Ink, Shape, Weight};
use crate::component::{PropType, Value};

fn ring(to: f64) -> Vec<Shape> {
    vec![
        Shape::Arc {
            cx: 0.5,
            cy: 0.5,
            radius: 0.4,
            from: 0.0,
            to: 1.0,
            stroke: Ink::Track,
            weight: Weight::Regular,
        },
        Shape::Arc {
            cx: 0.5,
            cy: 0.5,
            radius: 0.4,
            from: 0.0,
            to,
            stroke: Ink::Accent,
            weight: Weight::Regular,
        },
    ]
}

#[test]
fn a_shape_reads_from_its_case() {
    let value = Value::Case {
        case: "arc".to_owned(),
        fields: vec![
            ("cx".to_owned(), Value::Float(0.5)),
            ("cy".to_owned(), Value::Float(0.5)),
            ("radius".to_owned(), Value::Float(0.4)),
            ("from".to_owned(), Value::Int(0)),
            ("to".to_owned(), Value::Float(0.6)),
            ("stroke".to_owned(), Value::Choice("accent".to_owned())),
        ],
    };
    assert_eq!(Shape::from_value(&value).unwrap(), ring(0.6)[1]);
    let wrong = Value::Case {
        case: "arc".to_owned(),
        fields: vec![],
    };
    assert!(
        Shape::from_value(&wrong)
            .unwrap_err()
            .contains("needs `cx`")
    );
}

#[test]
fn a_press_finds_the_topmost_shape() {
    let drawing = Drawing {
        rect: Rect::new(0.0, 0.0, 100.0, 100.0),
        shapes: vec![
            Shape::Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                round: 0.0,
                fill: Some(Ink::Track),
                stroke: None,
                weight: Weight::Regular,
            },
            Shape::Ellipse {
                cx: 0.5,
                cy: 0.5,
                rx: 0.1,
                ry: 0.1,
                fill: Some(Ink::Accent),
                stroke: None,
                weight: Weight::Regular,
            },
        ],
        ..Drawing::default()
    };
    assert_eq!(drawing.shape_at(Point::new(50.0, 50.0), 1.0), Some(1));
    assert_eq!(drawing.shape_at(Point::new(5.0, 5.0), 1.0), Some(0));
    assert_eq!(drawing.shape_at(Point::new(150.0, 5.0), 1.0), None);
}

#[test]
fn the_shape_description_names_every_case() {
    let described = super::drawing_component::shape_description();
    let cases: Vec<&str> = described.cases.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cases, ["rect", "ellipse", "arc", "line", "path"]);
}
