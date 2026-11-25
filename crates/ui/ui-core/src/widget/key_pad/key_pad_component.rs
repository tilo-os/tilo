//! [`KeyPad`] as a described component: the rows of keys a keyboard draws, with what a finger's
//! tap, hold, swipe and caret drag mean reported as data.
//!
//! The app owns the layout (which keys, in which rows, and what each is called), the words
//! around it (the toolbar, the suggestions) and what a press types; the pad draws the keys,
//! hit-tests them the way a thumb does, and reports what happened.

use crate::component::{Component, Described, Event, PropType, Type, Value};
use crate::widget::key_pad::{KeyPad, PadKey, PadSize, PadTone};

/// The keyboard's keys.
#[derive(Component)]
#[component(name = "KeyPad", role = "Group", label = "label")]
pub struct KeyPadProps {
    /// The keys, each with its row and column: the pad lays each row out by the keys' widths, and
    /// a key's column is where it stands in its row.
    #[component(required, positional)]
    pub keys: Vec<PadKey>,
    /// How tall a key's rows are, independent of the desktop's density.
    pub size: PadSize,
    /// Caps Lock is latched: the Shift key stands lit and letters show upper case.
    pub caps: bool,
    /// The field is secret: the pad shows no key preview, no alternates and no swipe trail.
    pub secret: bool,
    /// Whether a press runs now.
    pub enabled: bool,
    /// What the pad shows, as assistive technology announces it: "On-screen keyboard".
    pub label: Option<String>,
    /// Why it cannot be pressed now, where the label does not say.
    pub description: Option<String>,
    /// A key was pressed: carries its place in `keys` and where the finger was, as fractions of
    /// the pad's box, `[index, x, y]`.
    #[component(event(value = i64, at = Vec<f64>))]
    pub press: Event,
    /// A long press chose one of a key's forms: carries its place in `keys` and the form,
    /// `[index, form]`.
    #[component(event(value = Vec<String>))]
    pub hold: Event,
    /// A finger crossed the keys and lifted: carries the path in fractions of the pad's box
    /// (`x, y, x, y, …`) and the box's width in its heights, so the keys it crossed keep their
    /// shape wherever the pad stands.
    #[component(event(value = Vec<f64>, at = f64))]
    pub swipe: Event,
    /// The person dragged the space bar: carries the caret steps it asks for, left for a negative
    /// number.
    #[component(event(value = i64))]
    pub caret: Event,
}

impl Default for KeyPadProps {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            size: PadSize::Standard,
            caps: false,
            secret: false,
            enabled: true,
            label: None,
            description: None,
            press: Event::None,
            hold: Event::None,
            swipe: Event::None,
            caret: Event::None,
        }
    }
}

impl Described for KeyPad {
    type Props = KeyPadProps;

    fn from_props(_key: &str, props: KeyPadProps) -> Self {
        let mut pad = Self::default();
        pad.set_props(props);
        pad
    }

    fn set_props(&mut self, props: KeyPadProps) -> bool {
        let mut changed = false;
        if props.keys != self.keys()
            || props.size != self.size
            || props.caps != self.caps
            || props.secret != self.secret
            || props.enabled != self.enabled
            || props.label != self.label
            || props.description != self.description
        {
            changed = true;
        }
        self.set_keys(props.keys);
        self.size = props.size;
        self.caps = props.caps;
        self.secret = props.secret;
        self.enabled = props.enabled;
        self.label.clone_from(&props.label);
        self.description.clone_from(&props.description);
        let press = props.press;
        self.on_press = (!press.is_none()).then(|| {
            Box::new(move |index: i64, point: (f64, f64)| {
                press.fire(Some(Value::List(vec![
                    Value::Int(index),
                    Value::List(vec![Value::Float(point.0), Value::Float(point.1)]),
                ])));
            }) as Box<dyn FnMut(i64, (f64, f64))>
        });
        let hold = props.hold;
        self.on_hold = (!hold.is_none()).then(|| {
            Box::new(move |index: i64, form: String| {
                hold.fire(Some(Value::List(vec![
                    Value::Int(index),
                    Value::Text(form),
                ])));
            }) as Box<dyn FnMut(i64, String)>
        });
        let swipe = props.swipe;
        self.on_swipe = (!swipe.is_none()).then(|| {
            Box::new(move |path: Vec<f64>, aspect: f64| {
                swipe.fire(Some(Value::List(vec![
                    Value::List(path.into_iter().map(Value::Float).collect()),
                    Value::Float(aspect),
                ])));
            }) as Box<dyn FnMut(Vec<f64>, f64)>
        });
        let caret = props.caret;
        self.on_caret = (!caret.is_none()).then(|| {
            Box::new(move |steps: i64| {
                caret.fire(Some(Value::Int(steps)));
            }) as Box<dyn FnMut(i64)>
        });
        changed
    }
}

/// One key of the pad: a label or an icon, the tone it is drawn in, the row and column it stands
/// in, and what a long press or a hold on it offers.
impl PropType for PadKey {
    fn ty() -> Type {
        Type::Cases {
            name: "PadKey".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "key" {
            return Err(format!("has no case `{case}`"));
        }
        let field = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v);
        let text = |name: &str| -> Result<String, String> {
            match field(name) {
                Some(Value::Text(value)) => Ok(value.clone()),
                None => Ok(String::new()),
                _ => Err(format!("`key`'s `{name}` is a text")),
            }
        };
        let number = |name: &str, default: f64| -> Result<f64, String> {
            match field(name) {
                Some(Value::Float(value)) => Ok(*value),
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a width written as a whole number"
                )]
                Some(Value::Int(value)) => Ok(*value as f64),
                None => Ok(default),
                _ => Err(format!("`key`'s `{name}` is a number")),
            }
        };
        let whole = |name: &str| -> Result<i64, String> {
            match field(name) {
                Some(Value::Int(value)) => Ok(*value),
                None => Ok(0),
                _ => Err(format!("`key`'s `{name}` is a whole number")),
            }
        };
        let flag = |name: &str| matches!(field(name), Some(Value::Bool(true)));
        let holds = match field("holds") {
            Some(Value::List(items)) => items
                .iter()
                .map(|item| match item {
                    Value::Text(form) => Ok(form.clone()),
                    _ => Err("`key`'s `holds` are texts".to_owned()),
                })
                .collect::<Result<Vec<_>, _>>()?,
            None | Some(Value::None) => Vec::new(),
            _ => return Err("`key`'s `holds` are a list of texts".to_owned()),
        };
        Ok(PadKey {
            label: text("label")?,
            name: text("name")?,
            icon: text("icon")?,
            tone: <PadTone as PropType>::from_value(field("tone").unwrap_or(&Value::None))?,
            width: number("width", 1.0)?,
            row: whole("row")?,
            column: whole("column")?,
            holds,
            repeat: flag("repeat"),
            caret: flag("caret"),
        })
    }
}

/// The pad's keys, as the catalogue describes them: one case, its fields and what each carries.
#[must_use]
pub fn key_pad_description() -> crate::component::EnumDescription {
    use tilo_ui_graph::component::{CaseDescription, EnumDescription, FieldDescription};
    let text = |name: &str, doc: &str| FieldDescription {
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
    let number = |name: &str, default: f64, doc: &str| FieldDescription {
        name: name.to_owned(),
        ty: Type::Float,
        default: Some(Value::Float(default)),
        doc: doc.to_owned(),
    };
    let whole = |name: &str, doc: &str| FieldDescription {
        name: name.to_owned(),
        ty: Type::Int,
        default: None,
        doc: doc.to_owned(),
    };
    EnumDescription {
        name: "PadKey".to_owned(),
        doc: "One key of the pad: what it shows, the row and column it stands in, and what a long \
              press or a hold on it offers."
            .to_owned(),
        cases: vec![CaseDescription {
            name: "key".to_owned(),
            doc: "A key, a tap away.".to_owned(),
            fields: vec![
                text("label", "What it shows, a word or one character."),
                text(
                    "name",
                    "What assistive technology calls it, where that says more than the label.",
                ),
                text(
                    "icon",
                    "A glyph instead of the label, by its icon name (`delete`); empty for none.",
                ),
                FieldDescription {
                    name: "tone".to_owned(),
                    ty: PadTone::ty(),
                    default: Some(Value::Choice("plain".to_owned())),
                    doc: "How it stands on the pad: a letter, a modifier, or the key that commits."
                        .to_owned(),
                },
                number(
                    "width",
                    1.0,
                    "Its width in letter keys: the space bar is wider.",
                ),
                whole("row", "The row it stands in, counted from 0 at the top."),
                whole("column", "Its place in its row, counted from 0."),
                FieldDescription {
                    name: "holds".to_owned(),
                    ty: Type::List(Box::new(Type::Text)),
                    default: Some(Value::List(Vec::new())),
                    doc: "What a long press on it offers, the plain key first; empty for none."
                        .to_owned(),
                },
                flag(
                    "repeat",
                    "Holding it acts again and again: the Backspace key's repeat.",
                ),
                flag(
                    "caret",
                    "Dragging along it moves the caret: the space bar's steering.",
                ),
            ],
        }],
    }
}
