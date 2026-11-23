use vello::kurbo::{Point, Rect};

use super::{Anchor, Drawing, Ink, Shape, Weight};
use crate::component::{PropType, Value};
use crate::time::Duration;
use crate::tokens::{EffectiveMotion, Motion, TextRole};

/// The `to` of the drawing's second arc as drawn at `at`.
fn drawn_to(drawing: &Drawing, at: crate::time::Instant) -> f64 {
    let _frame = crate::motion::pin_frame_at(at);
    let shapes = drawing
        .shapes_at(at)
        .unwrap_or_else(|| drawing.shapes.clone());
    match &shapes[1] {
        Shape::Arc { to, .. } => *to,
        other => panic!("not an arc: {other:?}"),
    }
}

/// A drawing at rest on `ring(0.2)` that has been drawn with `motion`.
fn drawn_ring(motion: EffectiveMotion) -> Drawing {
    let mut drawing = Drawing::default();
    drawing.set_shapes(ring(0.2));
    drawing.motion = Some(motion);
    drawing
}

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

tilo_test_env::scenario! {
    /// The shapes of a drawing.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_drawing_reads_and_hit_tests_its_shapes;

    phase a_shape_reads_from_its_case;
    {
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

    phase a_press_finds_the_topmost_shape;
    {
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

    phase the_shape_description_names_every_case;
    {
        let described = super::drawing_component::shape_description();
        let cases: Vec<&str> = described.cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cases, ["rect", "ellipse", "arc", "line", "path", "text"]);
    }

    phase a_text_reads_with_the_themes_roles_and_its_defaults;
    {
        let value = Value::Case {
            case: "text".to_owned(),
            fields: vec![
                ("x".to_owned(), Value::Float(0.5)),
                ("y".to_owned(), Value::Float(0.5)),
                ("text".to_owned(), Value::Text("62%".to_owned())),
                ("role".to_owned(), Value::Choice("title".to_owned())),
            ],
        };
        assert_eq!(
            Shape::from_value(&value).unwrap(),
            Shape::Text {
                x: 0.5,
                y: 0.5,
                text: "62%".to_owned(),
                role: TextRole::Title,
                ink: Ink::Ink,
                anchor: Anchor::Middle,
                strong: false,
            }
        );
    }

    phase a_text_is_found_where_it_was_set;
    {
        let mut drawing = Drawing {
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            shapes: ring(0.5),
            ..Drawing::default()
        };
        drawing.shapes.push(Shape::Text {
            x: 0.5,
            y: 0.5,
            text: "50%".to_owned(),
            role: TextRole::Title,
            ink: Ink::Ink,
            anchor: Anchor::Middle,
            strong: true,
        });
        drawing.text_boxes = vec![None, None, Some(Rect::new(40.0, 44.0, 60.0, 56.0))];
        assert_eq!(drawing.shape_at(Point::new(50.0, 50.0), 1.0), Some(2));
    }

    phase hover_reports_each_change_of_shape_once;
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = seen.clone();
        let mut drawing = Drawing {
            on_hover: Some(Box::new(move |at| log.borrow_mut().push(at))),
            ..Drawing::default()
        };
        drawing.hover(Some(1));
        drawing.hover(Some(1));
        drawing.hover(Some(0));
        drawing.hover(None);
        drawing.hover(None);
        assert_eq!(*seen.borrow(), [Some(1), Some(0), None]);
    }

    phase a_new_reading_moves_from_where_it_is_drawn_and_comes_to_rest;
    {
        let mut drawing = drawn_ring(EffectiveMotion::new(Motion::default(), false));
        let start = crate::time::now();
        {
            let _frame = crate::motion::pin_frame_at(start);
            drawing.set_shapes(ring(0.8));
        }
        assert!((drawn_to(&drawing, start) - 0.2).abs() < 1e-6, "it starts where it was");
        let mid = drawn_to(&drawing, start + Duration::from_millis(60));
        assert!(mid > 0.2 && mid < 0.8, "on its way: {mid}");
        let later = start + Duration::from_secs(3);
        {
            let _frame = crate::motion::pin_frame_at(later);
            assert!(!drawing.is_moving(later), "at rest it asks for no frames");
        }
        assert!((drawn_to(&drawing, later) - 0.8).abs() < 1e-6);
    }

    phase a_reading_taken_mid_flight_goes_on_from_where_it_is_drawn;
    {
        let mut drawing = drawn_ring(EffectiveMotion::new(Motion::default(), false));
        let start = crate::time::now();
        {
            let _frame = crate::motion::pin_frame_at(start);
            drawing.set_shapes(ring(0.8));
        }
        let turn = start + Duration::from_millis(60);
        let there = drawn_to(&drawing, turn);
        {
            let _frame = crate::motion::pin_frame_at(turn);
            drawing.set_shapes(ring(0.1));
        }
        let after = drawn_to(&drawing, turn);
        assert!((after - there).abs() < 1e-6, "no jump: {there} then {after}");
        let soon = drawn_to(&drawing, turn + Duration::from_millis(4));
        assert!((soon - there).abs() < 0.02, "it turns, never jumps: {there} then {soon}");
        let rest = drawn_to(&drawing, turn + Duration::from_secs(3));
        assert!((rest - 0.1).abs() < 1e-6, "it comes to rest at the new reading: {rest}");
    }

    phase reduced_motion_and_a_still_feel_land_at_once;
    {
        let start = crate::time::now();
        let _frame = crate::motion::pin_frame_at(start);
        let mut reduced = drawn_ring(EffectiveMotion::new(Motion::default(), true));
        reduced.set_shapes(ring(0.8));
        assert!(!reduced.is_moving(start));

        let mut still = drawn_ring(EffectiveMotion::new(Motion::default(), false));
        still.feel = crate::motion::Feel::Still;
        still.set_shapes(ring(0.8));
        assert!(!still.is_moving(start));
    }

    phase a_pointer_drags_wheels_pinches_and_says_where_it_is;
    {
        use std::cell::RefCell;
        use std::rc::Rc;
        use winit::event::{DeviceId, ElementState, MouseButton, MouseScrollDelta};

        let heard: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mut drawing = Drawing {
            rect: Rect::new(0.0, 0.0, 100.0, 50.0),
            ..Drawing::default()
        };
        let note = |label: &'static str, two: bool| {
            let heard = Rc::clone(&heard);
            let boxed: Box<dyn FnMut(f64, f64)> = if two {
                Box::new(move |x: f64, y: f64| {
                    heard.borrow_mut().push(format!("{label} {x:.2} {y:.2}"));
                })
            } else {
                Box::new(move |x: f64, _y: f64| {
                    heard.borrow_mut().push(format!("{label} {x:.2}"));
                })
            };
            boxed
        };
        drawing.on_pointer = Some(note("pointer", true));
        drawing.on_drag_start = Some(note("start", true));
        drawing.on_drag = Some(note("drag", true));
        drawing.on_drag_end = Some(note("end", true));
        drawing.on_wheel = Some(note("wheel", true));
        drawing.on_pinch = Some(note("pinch", false));

        let moved = |x: f64, y: f64| WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        };
        let click = |state: ElementState| WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        };

        // A pointer move inside reports its place as fractions of the box.
        assert!(!drawing.gesture(&moved(50.0, 25.0), 1.0));
        // A press begins a drag; the moves under it are the drawing's, so a page does not scroll.
        assert!(!drawing.gesture(&click(ElementState::Pressed), 1.0));
        assert!(drawing.gesture(&moved(75.0, 25.0), 1.0));
        assert!(drawing.gesture(&click(ElementState::Released), 1.0));
        // A wheel and a pinch over it are its own too.
        let wheel = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -20.0)),
            phase: winit::event::TouchPhase::Moved,
        };
        assert!(drawing.gesture(&wheel, 1.0));
        let pinch = WindowEvent::PinchGesture {
            device_id: DeviceId::dummy(),
            delta: 0.5,
            phase: winit::event::TouchPhase::Moved,
        };
        assert!(drawing.gesture(&pinch, 1.0));
        // Away from the drawing, they are no one's here.
        assert!(!drawing.gesture(&moved(500.0, 500.0), 1.0));
        assert!(!drawing.gesture(&click(ElementState::Pressed), 1.0));

        assert_eq!(
            *heard.borrow(),
            [
                "pointer 0.50 0.50",
                "start 0.50 0.50",
                "pointer 0.75 0.50",
                "drag 0.75 0.50",
                "end 0.75 0.50",
                "wheel 0.00 -20.00",
                "pinch 1.50",
            ]
        );
    }

    phase the_first_reading_and_a_new_form_land_at_once;
    {
        let start = crate::time::now();
        let _frame = crate::motion::pin_frame_at(start);
        let mut first = Drawing::default();
        first.set_shapes(ring(0.2));
        first.set_shapes(ring(0.8));
        assert!(!first.is_moving(start), "never drawn: nothing to move from");

        let mut drawing = drawn_ring(EffectiveMotion::new(Motion::default(), false));
        let mut grown = ring(0.8);
        grown.pop();
        drawing.set_shapes(grown);
        assert!(!drawing.is_moving(start), "a shape gone: no number to follow");
    }
}
