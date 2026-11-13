//! [`Drawing`] as a described component, and [`Shape`] as its closed set of shapes.

use tilo_ui_graph::component::{CaseDescription, EnumDescription, FieldDescription};

use crate::component::{Component, Described, Event, PropType, Type, Value};

use super::{Drawing, Ink, Meaning, Shape, Weight};

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
    /// The person pressed a shape: carries its index in `shapes`.
    #[component(event(value = i64))]
    pub press: Event,
}

impl Described for Drawing {
    type Props = DrawingProps;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a widget's size, well inside f32"
    )]
    fn from_props(_key: &str, props: DrawingProps) -> Self {
        let mut drawing = Self::default();
        let height = props.height.unwrap_or(48.0);
        drawing.width = props.width.unwrap_or(height) as f32;
        drawing.height = height as f32;
        drawing.set_props(props);
        drawing
    }

    fn set_props(&mut self, props: DrawingProps) -> bool {
        self.shapes = props.shapes;
        self.meaning = props.meaning;
        self.label = props.label;
        self.value = props.value;
        let press = props.press;
        self.on_press = Some(Box::new(move |index| {
            press.fire(Some(Value::Int(i64::try_from(index).unwrap_or(i64::MAX))));
        }));
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
                    flag("wash", "Its fill fading out downward."),
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
            other => {
                return Err(format!(
                    "a shape is rect, ellipse, arc, line or path, not `{other}`"
                ));
            }
        })
    }
}
