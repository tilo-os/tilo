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
    /// Who did something: a person or an agent task. Values come only from the host, never
    /// from `Text`.
    Principal,
    /// No value.
    #[default]
    Unit,
    Record(TypeKey),
    List(Box<Ty>),
    Set(Box<Ty>),
    Map(Box<Ty>),
    Collection(Box<Ty>),
    /// The built-in enum `.none | .some(value)`.
    Option(Box<Ty>),
    /// The built-in enum `.ok(value) | .err(error)`.
    Result(Box<Ty>, Box<Ty>),
    /// A declared `enum`: a host handle holding the variant and its payload.
    Enum(TypeKey),
    Tuple(Vec<Ty>),
    Action,
    View,
    /// A length literal; the unit is part of the type.
    Length(LengthUnit),
    /// A built-in enum of the platform, held as its variant's index: the `space` enums and
    /// the options of UI primitives (`gap: .section`). See `prims::choice`.
    Choice(&'static str),
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
            Self::Int | Self::Choice(_) => Some(Vt::I64),
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
            (Self::Result(a, x), Self::Result(b, y)) => a.same(b) && x.same(y),
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
        matches!(self, Self::Int | Self::Float | Self::Bool | Self::Choice(_))
    }

    /// The element type of something a `for` loop can walk.
    pub fn element(&self) -> Option<&Self> {
        match self {
            Self::List(t) | Self::Set(t) | Self::Collection(t) => Some(t),
            _ => None,
        }
    }

    /// The type as metadata records it: like [`Ty::display`], except that a declared enum is
    /// written `enum Name`, so a reader of the metadata tells it from a record.
    pub fn meta_text(&self, own_app: &str) -> String {
        match self {
            Self::Enum(k) if k.app == own_app => format!("enum {}", k.name),
            Self::Enum(k) => format!("enum {}.{}", k.app, k.name),
            Self::List(t) => format!("List<{}>", t.meta_text(own_app)),
            Self::Set(t) => format!("Set<{}>", t.meta_text(own_app)),
            Self::Map(t) => format!("Map<{}>", t.meta_text(own_app)),
            Self::Collection(t) => format!("Collection<{}>", t.meta_text(own_app)),
            Self::Option(t) => format!("Option<{}>", t.meta_text(own_app)),
            Self::Result(t, e) => {
                format!("Result<{}, {}>", t.meta_text(own_app), e.meta_text(own_app))
            }
            Self::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(|t| t.meta_text(own_app)).collect();
                format!("({})", inner.join(", "))
            }
            other => other.display(own_app),
        }
    }

    /// The type as written in source: local records and enums by name, foreign ones qualified
    /// by their app id.
    pub fn display(&self, own_app: &str) -> String {
        match self {
            Self::Int => "Int".into(),
            Self::Float => "Float".into(),
            Self::Bool => "Bool".into(),
            Self::Text => "Text".into(),
            Self::Principal => "Principal".into(),
            Self::Unit => "()".into(),
            Self::Action => "Action".into(),
            Self::View => "View".into(),
            Self::Length(u) => format!("Length({})", u.text()),
            Self::Choice(k) => crate::prims::choice_name(k).to_string(),
            Self::Error => "?".into(),
            Self::Record(k) | Self::Enum(k) if k.app == own_app => k.name.clone(),
            Self::Record(k) | Self::Enum(k) => format!("{}.{}", k.app, k.name),
            Self::List(t) => format!("List<{}>", t.display(own_app)),
            Self::Set(t) => format!("Set<{}>", t.display(own_app)),
            Self::Map(t) => format!("Map<{}>", t.display(own_app)),
            Self::Collection(t) => format!("Collection<{}>", t.display(own_app)),
            Self::Option(t) => format!("Option<{}>", t.display(own_app)),
            Self::Result(t, e) => {
                format!("Result<{}, {}>", t.display(own_app), e.display(own_app))
            }
            Self::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(|t| t.display(own_app)).collect();
                format!("({})", inner.join(", "))
            }
        }
    }

    /// Parses the metadata form produced by [`Ty::meta_text`]; `app` is the app the metadata
    /// belongs to, which unqualified record and enum names refer to.
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
        let mut name = self.path();
        let is_enum = name == "enum";
        if is_enum {
            name = self.path();
        }
        let key = |name: &str, app: &str| match name.rsplit_once('.') {
            Some((a, ty)) => TypeKey {
                app: a.to_string(),
                name: ty.to_string(),
            },
            None => TypeKey {
                app: app.to_string(),
                name: name.to_string(),
            },
        };
        if is_enum {
            return (!name.is_empty()).then(|| Ty::Enum(key(&name, self.app)));
        }
        match name.as_str() {
            "" => None,
            "Int" => Some(Ty::Int),
            "Float" => Some(Ty::Float),
            "Bool" => Some(Ty::Bool),
            "Text" => Some(Ty::Text),
            "Principal" => Some(Ty::Principal),
            "Action" => Some(Ty::Action),
            "View" => Some(Ty::View),
            "Result" => {
                if !self.eat('<') {
                    return None;
                }
                let ok = Box::new(self.ty()?);
                if !self.eat(',') {
                    return None;
                }
                let err = Box::new(self.ty()?);
                if !self.eat('>') {
                    return None;
                }
                Some(Ty::Result(ok, err))
            }
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
            _ => Some(Ty::Record(key(&name, self.app))),
        }
    }
}
