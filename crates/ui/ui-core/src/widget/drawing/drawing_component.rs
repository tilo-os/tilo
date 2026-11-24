//! [`Drawing`] as a described component, and [`Shape`] as its closed set of shapes.

use tilo_ui_graph::component::{CaseDescription, EnumDescription, FieldDescription};

use crate::component::{Component, Described, Event, PropType, Type, Value};

use super::{Anchor, Drawing, Ink, Meaning, Shape, Weight};
use crate::tokens::TextRole;

/// Shapes drawn with the design system over a box of their own.
#[derive(Component)]
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
    /// The pointer moved over the drawing, at (x, y): fractions of its box, so the place is the
    /// one its shapes are placed at.
    #[component(event(value = f64, at = f64))]
    pub pointer: Event,
    /// A drag began over the drawing, at (x, y) in fractions of its box.
    #[component(event(value = f64, at = f64))]
    pub drag_start: Event,
    /// The pointer moved while dragging, to (x, y) in fractions of its box.
    #[component(event(value = f64, at = f64))]
    pub drag: Event,
    /// A drag ended at (x, y) in fractions of its box.
    #[component(event(value = f64, at = f64))]
    pub drag_end: Event,
    /// A pointer went down over the drawing, at (x, y) in fractions of its box: the point a person
    /// pressed, for a drawing they set things on.
    #[component(event(value = Vec<f64>))]
    pub press_at: Event,
    /// The pointer moved over the drawing while pressed, to (x, y) in fractions of its box.
    #[component(event(value = Vec<f64>))]
    pub drag_at: Event,
    /// The pointer came up over the drawing, at (x, y) in fractions of its box.
    #[component(event(value = Vec<f64>))]
    pub release_at: Event,
    /// The wheel turned over the drawing: across it and down it, in logical pixels, positive
    /// down and to the end.
    #[component(event(value = f64, at = f64))]
    pub wheel: Event,
    /// A pinch over the drawing: the scale factor the fingers ask for, 1 for none.
    #[component(event(value = f64))]
    pub pinch: Event,
    /// Whether a press on it runs now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the label does not say (an action whose requirement
    /// does not hold): spoken as its description beside its name.
    pub description: Option<String>,
}

/// A drawing takes a press until something says otherwise.
impl Default for DrawingProps {
    fn default() -> Self {
        Self {
            shapes: Vec::new(),
            meaning: Meaning::default(),
            label: None,
            value: None,
            width: None,
            height: None,
            motion: crate::motion::Feel::default(),
            press: Event::None,
            hover: Event::None,
            pointer: Event::None,
            drag_start: Event::None,
            drag: Event::None,
            drag_end: Event::None,
            press_at: Event::None,
            drag_at: Event::None,
            release_at: Event::None,
            wheel: Event::None,
            pinch: Event::None,
            enabled: true,
            description: None,
        }
    }
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
        self.description = props.description;
        let press = props.press;
        self.on_press = (props.enabled && !press.is_none())
            .then(|| -> Box<dyn FnMut(usize)> { Box::new(move |at| press.fire(Some(index(at)))) });
        let hover = props.hover;
        self.on_hover = (!hover.is_none()).then(|| -> Box<dyn FnMut(Option<usize>)> {
            Box::new(move |at| hover.fire(Some(at.map_or(Value::None, index))))
        });
        let pair = |event: &Event| -> Option<Box<dyn FnMut(f64, f64)>> {
            (!event.is_none()).then(|| {
                let event = event.clone();
                Box::new(move |x: f64, y: f64| {
                    event.fire(Some(Value::List(vec![Value::Float(x), Value::Float(y)])));
                }) as Box<dyn FnMut(f64, f64)>
            })
        };
        // Shadowing the props so the closure can take the event by value.
        let (pointer, drag_start, drag, drag_end, wheel, press_at, drag_at, release_at) = (
            &props.pointer,
            &props.drag_start,
            &props.drag,
            &props.drag_end,
            &props.wheel,
            &props.press_at,
            &props.drag_at,
            &props.release_at,
        );
        self.on_pointer = pair(pointer);
        self.on_drag_start = pair(drag_start);
        self.on_drag = pair(drag);
        self.on_drag_end = pair(drag_end);
        self.on_wheel = pair(wheel);
        self.on_press_at = pair(press_at);
        self.on_drag_at = pair(drag_at);
        self.on_release_at = pair(release_at);
        self.on_pinch = (!props.pinch.is_none()).then(|| {
            let pinch = props.pinch.clone();
            Box::new(move |scale: f64| pinch.fire(Some(Value::Float(scale)))) as Box<dyn FnMut(f64)>
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
            case(
                "area",
                "A box filled in a colour of its own (`#rrggbb` or `#rrggbbaa`), not the theme's.",
                vec![
                    float("x", ""),
                    float("y", ""),
                    float("w", ""),
                    float("h", ""),
                    FieldDescription {
                        default: Some(Value::Float(0.0)),
                        ..float("round", "")
                    },
                    FieldDescription {
                        name: "colour".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: "Its fill, `#rrggbb` or `#rrggbbaa`.".to_owned(),
                    },
                    FieldDescription {
                        name: "stroke_colour".to_owned(),
                        ty: Type::Text,
                        default: Some(Value::Text(String::new())),
                        doc: "Its outline, `#rrggbb`; none unless said.".to_owned(),
                    },
                    FieldDescription {
                        name: "stroke_width".to_owned(),
                        ty: Type::Float,
                        default: Some(Value::Float(0.0)),
                        doc: "Its outline's width, a fraction of the box's shorter side."
                            .to_owned(),
                    },
                ],
            ),
            case(
                "oval",
                "An ellipse filled and outlined in colours of its own, not the theme's.",
                vec![
                    float("cx", ""),
                    float("cy", ""),
                    float("rx", ""),
                    float("ry", ""),
                    FieldDescription {
                        name: "colour".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: "Its fill, `#rrggbb` or `#rrggbbaa`.".to_owned(),
                    },
                    FieldDescription {
                        name: "stroke_colour".to_owned(),
                        ty: Type::Text,
                        default: Some(Value::Text(String::new())),
                        doc: "Its outline, `#rrggbb`; none unless said.".to_owned(),
                    },
                    FieldDescription {
                        name: "stroke_width".to_owned(),
                        ty: Type::Float,
                        default: Some(Value::Float(0.0)),
                        doc: "Its outline's width, a fraction of the box's shorter side."
                            .to_owned(),
                    },
                ],
            ),
            case(
                "stroke",
                "A line through `points` in a colour of its own, not the theme's.",
                vec![
                    FieldDescription {
                        name: "points".to_owned(),
                        ty: Type::List(Box::new(Type::Float)),
                        default: None,
                        doc: String::new(),
                    },
                    FieldDescription {
                        name: "colour".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: "Its line, `#rrggbb` or `#rrggbbaa`.".to_owned(),
                    },
                    float(
                        "width",
                        "The line's width, a fraction of the box's shorter side.",
                    ),
                ],
            ),
            case(
                "picture",
                "A picture covering (x, y, w, h), its bytes at the content address `src` in the \
                 device's store.",
                vec![
                    float("x", "Its left edge, a fraction of the box."),
                    float("y", "Its top edge, a fraction of the box."),
                    float("w", "Its width, a fraction of the box."),
                    float("h", "Its height, a fraction of the box."),
                    FieldDescription {
                        name: "src".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: "Where its bytes are: a content address (`tilo-blob://…`).".to_owned(),
                    },
                ],
            ),
            case(
                "label",
                "A text at (x, y) in a colour and size of its own, not the theme's.",
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
                        name: "colour".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: "Its words, `#rrggbb` or `#rrggbbaa`.".to_owned(),
                    },
                    float("size", "Its height, a fraction of the box's height."),
                    FieldDescription {
                        name: "anchor".to_owned(),
                        ty: Anchor::ty(),
                        default: Some(Value::Choice("middle".to_owned())),
                        doc: "Which side of it stands at (x, y).".to_owned(),
                    },
                    flag("strong", "Set heavier than its size alone."),
                    flag("italic", "Set in italics."),
                    FieldDescription {
                        default: Some(Value::Float(0.0)),
                        ..float(
                            "wrap",
                            "A width to break its lines within, a fraction of the box's; one line \
                             unless said.",
                        )
                    },
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
        let text = |name: &str| -> Result<String, String> {
            match field(name) {
                Some(Value::Text(value)) => Ok(value.clone()),
                _ => Err(format!("`{case}` needs `{name}`, a text")),
            }
        };
        let hex = |name: &str| -> Result<String, String> {
            let value = text(name)?;
            if crate::color::parse_hex(&value).is_none() {
                return Err(format!(
                    "`{case}`'s `{name}` is a colour as `#rrggbb` or `#rrggbbaa`, not `{value}`"
                ));
            }
            Ok(value)
        };
        let optional_hex = |name: &str| -> Result<String, String> {
            match field(name) {
                None | Some(Value::None) => Ok(String::new()),
                Some(Value::Text(value)) if value.is_empty() => Ok(String::new()),
                Some(Value::Text(value)) => {
                    if crate::color::parse_hex(value).is_none() {
                        return Err(format!(
                            "`{case}`'s `{name}` is a colour as `#rrggbb`, not `{value}`"
                        ));
                    }
                    Ok(value.clone())
                }
                Some(_) => Err(format!("`{case}`'s `{name}` is a colour as `#rrggbb`")),
            }
        };
        let optional_float = |name: &str, default: f64| -> Result<f64, String> {
            match field(name) {
                None | Some(Value::None) => Ok(default),
                Some(Value::Float(v)) => Ok(*v),
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a fraction written as a whole number"
                )]
                Some(Value::Int(v)) => Ok(*v as f64),
                Some(_) => Err(format!("`{case}`'s `{name}` is a number")),
            }
        };
        let points = |name: &str| -> Result<Vec<f64>, String> {
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
                        _ => Err("a shape's points are numbers".to_owned()),
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
                points: points("points")?,
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
                colour: hex("colour")?,
                stroke_colour: optional_hex("stroke_colour")?,
                stroke_width: optional_float("stroke_width", 0.0)?,
            },
            "oval" => Shape::Oval {
                cx: float("cx")?,
                cy: float("cy")?,
                rx: float("rx")?,
                ry: float("ry")?,
                colour: hex("colour")?,
                stroke_colour: optional_hex("stroke_colour")?,
                stroke_width: optional_float("stroke_width", 0.0)?,
            },
            "stroke" => Shape::Stroke {
                points: points("points")?,
                colour: hex("colour")?,
                width: float("width")?,
            },
            "label" => Shape::Label {
                x: float("x")?,
                y: float("y")?,
                text: text("text")?,
                colour: hex("colour")?,
                size: float("size")?,
                strong: flag("strong"),
                italic: flag("italic"),
                wrap: optional_float("wrap", 0.0)?,
                anchor: match field("anchor") {
                    None | Some(Value::None) => Anchor::Middle,
                    Some(v) => Anchor::from_value(v)?,
                },
            },
            "picture" => Shape::Picture {
                x: float("x")?,
                y: float("y")?,
                w: float("w")?,
                h: float("h")?,
                src: match field("src") {
                    Some(Value::Text(src)) => src.clone(),
                    _ => {
                        return Err(
                            "`picture` needs `src`, the content address its bytes are at"
                                .to_owned(),
                        );
                    }
                },
            },
            other => {
                return Err(format!(
                    "a shape is rect, ellipse, arc, line, path, text, area, stroke, label or picture, not `{other}`"
                ));
            }
        })
    }
}
