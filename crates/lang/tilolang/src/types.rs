//! The type language: scalars, records, containers, and how they map to wasm values.

use serde::{Deserialize, Serialize};
use std::fmt;

/// How a field merges concurrent edits (spec section 2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeKind {
    Register,
    Counter,
    Text,
    /// Containers merge structurally; the source never names this.
    Structural,
}

impl MergeKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Counter => "counter",
            Self::Text => "text",
            Self::Structural => "structural",
        }
    }
}

/// The effect class of an action, derived by the compiler (spec section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectClass {
    Read,
    Undoable,
    Outside,
}

/// A unit of a length literal such as `11rem` (layout.md section 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LengthUnit {
    Rem,
    Dp,
    Fr,
}

impl LengthUnit {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Rem => "rem",
            Self::Dp => "dp",
            Self::Fr => "fr",
        }
    }
}

/// Identity of a record type: the app that declares it and its name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeKey {
    pub app: String,
    pub name: String,
}

/// A value type of the language.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Text,
    /// No value.
    #[default]
    Unit,
    Record(TypeKey),
    List(Box<Ty>),
    Set(Box<Ty>),
    Map(Box<Ty>),
    Collection(Box<Ty>),
    Option(Box<Ty>),
    Tuple(Vec<Ty>),
    Action,
    View,
    /// A length literal; the unit is part of the type.
    Length(LengthUnit),
    /// A value of a built-in enum of `space` (`"width_class"` or `"orientation"`).
    Enum(&'static str),
    /// A type that already produced an error; compatible with everything.
    Error,
}

/// A wasm value type of the host ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Vt {
    I32,
    I64,
    F64,
}

impl Vt {
    /// A handle into the host's value table.
    pub const H: Self = Self::I32;
}

impl Ty {
    /// The wasm representation (spec section 4.1); `None` for no value.
    pub const fn repr(&self) -> Option<Vt> {
        match self {
            Self::Float | Self::Length(_) => Some(Vt::F64),
            Self::Int | Self::Enum(_) => Some(Vt::I64),
            Self::Unit => None,
            _ => Some(Vt::I32),
        }
    }

    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Error)
    }

    /// Structural equality in which an error type matches anything.
    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Error, _) | (_, Self::Error) => true,
            (Self::List(a), Self::List(b))
            | (Self::Set(a), Self::Set(b))
            | (Self::Map(a), Self::Map(b))
            | (Self::Collection(a), Self::Collection(b))
            | (Self::Option(a), Self::Option(b)) => a.same(b),
            (Self::Tuple(a), Self::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            _ => self == other,
        }
    }

    /// Whether the type may only be the type of a field.
    pub const fn is_container(&self) -> bool {
        matches!(self, Self::Set(_) | Self::Map(_) | Self::Collection(_))
    }

    /// Whether values of this type are plain scalars in wasm (not handles).
    pub const fn is_scalar(&self) -> bool {
        matches!(self, Self::Int | Self::Float | Self::Bool)
    }

    /// The element type of something a `for` loop can walk.
    pub fn element(&self) -> Option<&Self> {
        match self {
            Self::List(t) | Self::Set(t) | Self::Collection(t) => Some(t),
            _ => None,
        }
    }

    /// The type as written in source and in metadata: local records by name, foreign records
    /// qualified by their app id.
    pub fn display(&self, own_app: &str) -> String {
        match self {
            Self::Int => "Int".into(),
            Self::Float => "Float".into(),
            Self::Bool => "Bool".into(),
            Self::Text => "Text".into(),
            Self::Unit => "()".into(),
            Self::Action => "Action".into(),
            Self::View => "View".into(),
            Self::Length(u) => format!("Length({})", u.text()),
            Self::Enum(k) => (*k).to_string(),
            Self::Error => "?".into(),
            Self::Record(k) if k.app == own_app => k.name.clone(),
            Self::Record(k) => format!("{}.{}", k.app, k.name),
            Self::List(t) => format!("List<{}>", t.display(own_app)),
            Self::Set(t) => format!("Set<{}>", t.display(own_app)),
            Self::Map(t) => format!("Map<{}>", t.display(own_app)),
            Self::Collection(t) => format!("Collection<{}>", t.display(own_app)),
            Self::Option(t) => format!("Option<{}>", t.display(own_app)),
            Self::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(|t| t.display(own_app)).collect();
                format!("({})", inner.join(", "))
            }
        }
    }

    /// Parses the metadata form produced by [`Ty::display`]; `app` is the app the metadata
    /// belongs to, which unqualified record names refer to.
    pub fn parse_meta(text: &str, app: &str) -> Option<Self> {
        let mut p = MetaTy {
            chars: text.chars().collect(),
            pos: 0,
            app,
        };
        let ty = p.ty()?;
        p.skip_ws();
        (p.pos >= p.chars.len()).then_some(ty)
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display(""))
    }
}

struct MetaTy<'a> {
    chars: Vec<char>,
    pos: usize,
    app: &'a str,
}

impl MetaTy<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, c: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn path(&mut self) -> String {
        self.skip_ws();
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                out.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        out
    }

    fn ty(&mut self) -> Option<Ty> {
        if self.eat('(') {
            let mut items = Vec::new();
            loop {
                items.push(self.ty()?);
                if self.eat(')') {
                    break;
                }
                if !self.eat(',') {
                    return None;
                }
            }
            return Some(Ty::Tuple(items));
        }
        let name = self.path();
        match name.as_str() {
            "" => None,
            "Int" => Some(Ty::Int),
            "Float" => Some(Ty::Float),
            "Bool" => Some(Ty::Bool),
            "Text" => Some(Ty::Text),
            "Action" => Some(Ty::Action),
            "View" => Some(Ty::View),
            "List" | "Set" | "Map" | "Collection" | "Option" => {
                if !self.eat('<') {
                    return None;
                }
                let inner = Box::new(self.ty()?);
                if !self.eat('>') {
                    return None;
                }
                Some(match name.as_str() {
                    "List" => Ty::List(inner),
                    "Set" => Ty::Set(inner),
                    "Map" => Ty::Map(inner),
                    "Collection" => Ty::Collection(inner),
                    _ => Ty::Option(inner),
                })
            }
            _ => Some(match name.rsplit_once('.') {
                Some((app, ty)) => Ty::Record(TypeKey {
                    app: app.to_string(),
                    name: ty.to_string(),
                }),
                None => Ty::Record(TypeKey {
                    app: self.app.to_string(),
                    name,
                }),
            }),
        }
    }
}
