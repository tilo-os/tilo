//! [`Drawing`] as a described component, and [`Shape`] as its closed set of shapes.

use tilo_ui_graph::component::{CaseDescription, EnumDescription, FieldDescription};

use crate::component::{Component, Described, Event, PropType, Type, Value};

use super::{Anchor, Drawing, Ink, Meaning, Shape, Weight};
use crate::tokens::TextRole;

/// Shapes drawn with the design system over a box of their own.
#[derive(Component, Default)]
#[component(name = "Drawing", role = "Image", label = "label")]
pub struct DrawingProps {
    /// The shapes, drawn in order, each over the ones before it.
    #[component(required, positional)]
    pub shapes: Vec<Shape>,
    /// What it means, for assistive technology and agents.
    #[component(required)]
    pub meaning: Meaning,
    /// What it shows, as it is announced: "Disk 60% full".
    pub label: Option<String>,
    /// How far it has got, for a progress drawing: a fraction.
    pub value: Option<f64>,
    /// Its width in logical pixels; a square of its height unless said.
    pub width: Option<f64>,
    /// Its height in logical pixels.
    pub height: Option<f64>,
    /// How its shapes move to a new reading: at the theme's pace unless said, never for someone
    /// who asked for reduced motion.
    pub motion: crate::motion::Feel,
    /// The person pressed a shape: carries its index in `shapes`.
    #[component(event(value = i64))]
    pub press: Event,
    /// The pointer came over a shape, or left them all: carries the index of the shape under it in
    /// `shapes`, or none.
    #[component(event(value = Option<i64>))]
    pub hover: Event,
    /// The pointer pressed on the drawing, on a shape or between them: carries the point, its x
    /// then its y, as fractions of the box from its top-left corner. What a page's readers place
    /// there (a note, a signature, a selection's start) is placed by what answers it.
    #[component(event(value = Vec<f64>))]
    pub press_at: Event,
    /// The pointer moved with the button down: carries the point, its x then its y, as fractions
    /// of the box, past its edges as far as the pointer goes — a drag that draws ink or marks
    /// a selection.
    #[component(event(value = Vec<f64>))]
    pub drag_at: Event,
    /// The pointer let go: carries the point, its x then its y, as fractions of the box, so what
    /// a drag made is finished where the person let go.
    #[component(event(value = Vec<f64>))]
    pub release_at: Event,
}

impl Described for Drawing {
    type Props = DrawingProps;

    fn from_props(_key: &str, props: DrawingProps) -> Self {
        let mut drawing = Self::default();
        drawing.set_props(props);
        drawing
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a widget's size, well inside f32"
    )]
    fn set_props(&mut self, props: DrawingProps) -> bool {
        let height = props.height.unwrap_or(48.0);
        let size = (props.width.unwrap_or(height) as f32, height as f32);
        if size != (self.width, self.height) {
            (self.width, self.height) = size;
            self.layout_dirty = true;
        }
        self.feel = props.motion;
        self.set_shapes(props.shapes);
        self.meaning = props.meaning;
        self.label = props.label;
        self.value = props.value;
        let index = |at: usize| Value::Int(i64::try_from(at).unwrap_or(i64::MAX));
        let press = props.press;
        self.on_press = (!press.is_none())
            .then(|| -> Box<dyn FnMut(usize)> { Box::new(move |at| press.fire(Some(index(at)))) });
        let hover = props.hover;
        self.on_hover = (!hover.is_none()).then(|| -> Box<dyn FnMut(Option<usize>)> {
            Box::new(move |at| hover.fire(Some(at.map_or(Value::None, index))))
        });
        let point = |x: f64, y: f64| Value::List(vec![Value::Float(x), Value::Float(y)]);
        let press_at = props.press_at;
        self.on_press_at = (!press_at.is_none()).then(|| {
            let fire = press_at;
            Box::new(move |x: f64, y: f64| fire.fire(Some(point(x, y)))) as Box<dyn FnMut(f64, f64)>
        });
        let drag_at = props.drag_at;
        self.on_drag_at = (!drag_at.is_none()).then(|| {
            let fire = drag_at;
            Box::new(move |x: f64, y: f64| fire.fire(Some(point(x, y)))) as Box<dyn FnMut(f64, f64)>
        });
        let release_at = props.release_at;
        self.on_release_at = (!release_at.is_none()).then(|| {
            let fire = release_at;
            Box::new(move |x: f64, y: f64| fire.fire(Some(point(x, y)))) as Box<dyn FnMut(f64, f64)>
        });
        true
    }
}

/// The description of [`Shape`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn shape_description() -> EnumDescription {
    let float = |name: &str, doc: &str| FieldDescription {
        name: name.to_owned(),
        ty: Type::Float,
        default: None,
        doc: doc.to_owned(),
    };
    let ink = |name: &str, required: bool| FieldDescription {
        name: name.to_owned(),
        ty: if required {
            Ink::ty()
        } else {
            Type::Option(Box::new(Ink::ty()))
        },
        default: (!required).then_some(Value::None),
        doc: String::new(),
    };
    let weight = FieldDescription {
        name: "weight".to_owned(),
        ty: Weight::ty(),
        default: Some(Value::Choice("regular".to_owned())),
        doc: "How heavy its line is.".to_owned(),
    };
    let text_field = |name: &str, doc: &str| FieldDescription {
        name: name.to_owned(),
        ty: Type::Text,
        default: None,
        doc: doc.to_owned(),
    };
    let flag = |name: &str, doc: &str| FieldDescription {
        name: name.to_owned(),
        ty: Type::Bool,
        default: Some(Value::Bool(false)),
        doc: doc.to_owned(),
    };
    let case = |name: &str, doc: &str, fields: Vec<FieldDescription>| CaseDescription {
        name: name.to_owned(),
        doc: doc.to_owned(),
        fields,
    };
    EnumDescription {
        name: "Shape".to_owned(),
        doc: "One shape of a drawing, in fractions of its box.".to_owned(),
        cases: vec![
            case(
                "rect",
                "A rectangle, rounded by `round` (a fraction of its shorter side).",
                vec![
                    float("x", ""),
                    float("y", ""),
                    float("w", ""),
                    float("h", ""),
                    FieldDescription {
                        default: Some(Value::Float(0.0)),
                        ..float("round", "")
                    },
                    ink("fill", false),
                    ink("stroke", false),
                    weight.clone(),
                ],
            ),
            case(
                "ellipse",
                "An ellipse around (cx, cy).",
                vec![
                    float("cx", ""),
                    float("cy", ""),
                    float("rx", ""),
                    float("ry", ""),
                    ink("fill", false),
                    ink("stroke", false),
                    weight.clone(),
                ],
            ),
            case(
                "arc",
                "An arc around (cx, cy), from `from` to `to` in fractions of a turn from the top.",
                vec![
                    float("cx", ""),
                    float("cy", ""),
                    float("radius", ""),
                    float("from", ""),
                    float("to", ""),
                    ink("stroke", true),
                    weight.clone(),
                ],
            ),
            case(
                "line",
                "A line from (x1, y1) to (x2, y2).",
                vec![
                    float("x1", ""),
                    float("y1", ""),
                    float("x2", ""),
                    float("y2", ""),
                    ink("stroke", true),
                    weight.clone(),
                ],
            ),
            case(
                "path",
                "A path through `points`, x and y in turn.",
                vec![
                    FieldDescription {
                        name: "points".to_owned(),
                        ty: Type::List(Box::new(Type::Float)),
                        default: None,
                        doc: String::new(),
                    },
                    flag("smooth", "A smooth curve rather than straight segments."),
                    flag("closed", "Back to its first point."),
                    ink("fill", false),
                    ink("stroke", false),
                    weight,
                    flag(
                        "wash",
                        "Its fill fading out downward, down to the drawing's foot when it is open.",
                    ),
                ],
            ),
            case(
                "area",
                "A rectangle filled in a colour the content brings (`#rrggbb`, `#rrggbbaa`): a highlight, a selection or a find hit over a page.",
                vec![
                    float("x", ""),
                    float("y", ""),
                    float("w", ""),
                    float("h", ""),
                    FieldDescription {
                        default: Some(Value::Float(0.0)),
                        ..float("round", "")
                    },
                    text_field("colour", "Its colour, `#rrggbb` or `#rrggbbaa`; the content's own, never the theme's."),
                ],
            ),
            case(
                "stroke",
                "A line through `points`, `width` of the box's shorter side thick, in a colour the content brings.",
                vec![
                    FieldDescription {
                        name: "points".to_owned(),
                        ty: Type::List(Box::new(Type::Float)),
                        default: None,
                        doc: "x and y in turn.".to_owned(),
                    },
                    text_field("colour", "Its colour, `#rrggbb` or `#rrggbbaa`."),
                    float("width", "How thick its line is, of the box's shorter side."),
                ],
            ),
            case(
                "label",
                "Words at (x, y) in a colour the content brings, `size` of the box's height: a page's own text, a signature's name.",
                vec![
                    float("x", ""),
                    float("y", ""),
                    text_field("text", ""),
                    text_field("colour", "Its colour, `#rrggbb` or `#rrggbbaa`."),
                    float("size", "How tall its letters stand, of the box's height."),
                    FieldDescription {
                        name: "anchor".to_owned(),
                        ty: Anchor::ty(),
                        default: Some(Value::Choice("middle".to_owned())),
                        doc: "Which side of it stands at (x, y).".to_owned(),
                    },
                ],
            ),
            case(
                "text",
                "A text at (x, y), set in one of the theme's type roles.",
                vec![
                    float("x", ""),
                    float("y", ""),
                    FieldDescription {
                        name: "text".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: String::new(),
                    },
                    FieldDescription {
                        name: "role".to_owned(),
                        ty: TextRole::ty(),
                        default: Some(Value::Choice("body".to_owned())),
                        doc: "Its type role: the theme's size and face for it.".to_owned(),
                    },
                    FieldDescription {
                        name: "ink".to_owned(),
                        ty: Ink::ty(),
                        default: Some(Value::Choice("ink".to_owned())),
                        doc: String::new(),
                    },
                    FieldDescription {
                        name: "anchor".to_owned(),
                        ty: Anchor::ty(),
                        default: Some(Value::Choice("middle".to_owned())),
                        doc: "Which side of it stands at (x, y).".to_owned(),
                    },
                    flag("strong", "Set heavier than its role."),
                ],
            ),
        ],
    }
}

impl PropType for Shape {
    fn ty() -> Type {
        Type::Cases {
            name: "Shape".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        let field = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v);
        let float = |name: &str| match field(name) {
            Some(Value::Float(v)) => Ok(*v),
            #[expect(
                clippy::cast_precision_loss,
                reason = "a fraction written as a whole number"
            )]
            Some(Value::Int(v)) => Ok(*v as f64),
            Some(Value::None) | None if name == "round" => Ok(0.0),
            _ => Err(format!("`{case}` needs `{name}`, a number")),
        };
        let flag = |name: &str| matches!(field(name), Some(Value::Bool(true)));
        let ink = |name: &str| -> Result<Option<Ink>, String> {
            match field(name) {
                None | Some(Value::None) => Ok(None),
                Some(v) => Ink::from_value(v).map(Some),
            }
        };
        let must = |name: &str| -> Result<Ink, String> {
            ink(name)?.ok_or_else(|| format!("`{case}` needs `{name}`, an ink"))
        };
        let word = |name: &str| -> Result<String, String> {
            match field(name) {
                Some(Value::Text(text)) => Ok(text.clone()),
                _ => Err(format!("`{case}` needs `{name}`, a text")),
            }
        };
        let numbers = |name: &str| -> Result<Vec<f64>, String> {
            match field(name) {
                Some(Value::List(items)) => items
                    .iter()
                    .map(|item| match item {
                        Value::Float(v) => Ok(*v),
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "a fraction written as a whole number"
                        )]
                        Value::Int(v) => Ok(*v as f64),
                        _ => Err(format!("`{name}` is a list of numbers")),
                    })
                    .collect(),
                _ => Err(format!("`{case}` needs `{name}`, a list of numbers")),
            }
        };
        let weight = match field("weight") {
            None | Some(Value::None) => Weight::Regular,
            Some(v) => Weight::from_value(v)?,
        };
        Ok(match case.as_str() {
            "rect" => Shape::Rect {
                x: float("x")?,
                y: float("y")?,
                w: float("w")?,
                h: float("h")?,
                round: float("round")?,
                fill: ink("fill")?,
                stroke: ink("stroke")?,
                weight,
            },
            "ellipse" => Shape::Ellipse {
                cx: float("cx")?,
                cy: float("cy")?,
                rx: float("rx")?,
                ry: float("ry")?,
                fill: ink("fill")?,
                stroke: ink("stroke")?,
                weight,
            },
            "arc" => Shape::Arc {
                cx: float("cx")?,
                cy: float("cy")?,
                radius: float("radius")?,
                from: float("from")?,
                to: float("to")?,
                stroke: must("stroke")?,
                weight,
            },
            "line" => Shape::Line {
                x1: float("x1")?,
                y1: float("y1")?,
                x2: float("x2")?,
                y2: float("y2")?,
                stroke: must("stroke")?,
                weight,
            },
            "path" => Shape::Path {
                points: match field("points") {
                    Some(Value::List(items)) => items
                        .iter()
                        .map(|item| match item {
                            Value::Float(v) => Ok(*v),
                            #[expect(
                                clippy::cast_precision_loss,
                                reason = "a fraction written as a whole number"
                            )]
                            Value::Int(v) => Ok(*v as f64),
                            _ => Err("a path's points are numbers".to_owned()),
                        })
                        .collect::<Result<_, _>>()?,
                    _ => return Err("`path` needs `points`, a list of numbers".to_owned()),
                },
                smooth: flag("smooth"),
                closed: flag("closed"),
                fill: ink("fill")?,
                stroke: ink("stroke")?,
                weight,
                wash: flag("wash"),
            },
            "text" => Shape::Text {
                x: float("x")?,
                y: float("y")?,
                text: match field("text") {
                    Some(Value::Text(text)) => text.clone(),
                    _ => return Err("`text` needs `text`, a text".to_owned()),
                },
                role: match field("role") {
                    None | Some(Value::None) => TextRole::Body,
                    Some(v) => TextRole::from_value(v)?,
                },
                ink: ink("ink")?.unwrap_or(Ink::Ink),
                anchor: match field("anchor") {
                    None | Some(Value::None) => Anchor::Middle,
                    Some(v) => Anchor::from_value(v)?,
                },
                strong: flag("strong"),
            },
            "area" => Shape::Area {
                x: float("x")?,
                y: float("y")?,
                w: float("w")?,
                h: float("h")?,
                round: float("round")?,
                colour: word("colour")?,
            },
            "stroke" => Shape::Stroke {
                points: numbers("points")?,
                colour: word("colour")?,
                width: float("width")?,
            },
            "label" => Shape::Label {
                x: float("x")?,
                y: float("y")?,
                text: word("text")?,
                colour: word("colour")?,
                size: float("size")?,
                anchor: match field("anchor") {
                    None | Some(Value::None) => Anchor::Middle,
                    Some(v) => Anchor::from_value(v)?,
                },
            },
            other => {
                return Err(format!(
                    "a shape is rect, ellipse, arc, line, path, text, area, stroke or label, not \
                     `{other}`"
                ));
            }
        })
    }
}
