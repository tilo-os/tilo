//! The type language: scalars, records, containers, and how they map to wasm values.

use serde::{Deserialize, Serialize};
use std::fmt;

/// How a field merges concurrent edits.
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

/// The effect class of an action, derived by the compiler (`effects`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectClass {
    Read,
    Undoable,
    Outside,
}

/// A unit of a length literal such as `11rem` (layout.md, "Units and sizes").
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
    /// A calendar day, held as ISO text `YYYY-MM-DD`; not a `Text`.
    Date,
    /// An instant, held as ISO-8601 UTC text `YYYY-MM-DDTHH:MM:SSZ`; not a `Text`. ISO text
    /// orders chronologically, so both compare as text.
    Time,
    /// A length of time, held as its number of seconds (an `i64`, like an `Int`, and not one):
    /// a literal writes it as `5m`, `2h`, `3d` or `1w`, and a moment or a day moves by it
    /// (`now + 2h`, `today + 3d`), while two days or two moments apart are one
    /// (`end - start`). A day moves by whole days only. Not a field's type: a stored span of
    /// time is a number the app reads in its own unit.
    Duration,
    /// `std.Identifier`: an account of some service, held as its URI `scheme:value`
    /// (`mailto:jane@x.com`, `tel:+4930123456`); not a `Text` (see [`crate::identifier`]).
    Identifier,
    /// `RichText`: formatted text in which resources are referenced (a person, a channel, a
    /// task), held as tilo-doc's rich text, the one rich-text model of the platform. A field of
    /// it merges by splices and marks.
    RichText,
    /// `Bytes`: the answer of a service, as it came; `.text()` reads it as text. A value code
    /// holds while an invocation runs, not a stored field's type.
    Bytes,
    /// `Response`: what `web.get` and `web.post` answer: `ok`, `status`, `status_text`,
    /// `headers`, `body: Bytes`, and `.text()`. A value code holds while an invocation runs,
    /// not a stored field's type.
    Response,
    /// `json.Value`: a JSON document as it was read, before code says what its shape is
    /// (`json.parse`). Its cases are the six kinds JSON has (`.object`, `.array`, `.string`,
    /// `.number`, `.bool`, `.null`). A value code holds while an invocation runs, not a
    /// stored field's type.
    Json,
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
    /// `Filter<T>`: one filter a type declares, as a value: what `T.today` names, what
    /// `filters(T)` lists, and what a query takes. Held as its catalogue entry, the
    /// `host.Filter` the list's places and search's chips read.
    Filter(TypeKey),
    /// `Sort<T>`: the order by one field of `T`, as a value: what `T.title` names where a
    /// window's sorts are asked (`sorts: [Note.title]`). Held as the `host.Sort` its menu
    /// entry reads: what it is called, and the order it asks as search writes it.
    Sort(TypeKey),
    /// A declared `enum`: a host handle holding the variant and its payload.
    Enum(TypeKey),
    /// `A | B`: a reference to a resource of any of these types, in order, at least two.
    Union(Vec<TypeKey>),
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
    /// Whether an action's form can ask the person for a value of this type when nobody gave
    /// it: text, a number, a flag, a day, a moment, a length of time, an account of a service,
    /// a person, a resource, a case of an enum, or none of one of those. Each has its control.
    #[must_use]
    pub fn asked_by_form(&self) -> bool {
        match self {
            Self::Int
            | Self::Float
            | Self::Bool
            | Self::Text
            | Self::RichText
            | Self::Date
            | Self::Time
            | Self::Duration
            | Self::Identifier
            | Self::Principal
            | Self::Record(_)
            | Self::Enum(_) => true,
            Self::Option(inner) => !matches!(**inner, Self::Option(_)) && inner.asked_by_form(),
            _ => false,
        }
    }

    /// The wasm representation: `i64`, `f64`, or an `i32` (a bool or a handle); `None` for no
    /// value.
    pub const fn repr(&self) -> Option<Vt> {
        match self {
            Self::Float | Self::Length(_) => Some(Vt::F64),
            Self::Int | Self::Duration | Self::Choice(_) => Some(Vt::I64),
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
        matches!(
            self,
            Self::Int | Self::Duration | Self::Float | Self::Bool | Self::Choice(_)
        )
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
            Self::Filter(k) => format!("Filter<{}>", Self::Record(k.clone()).meta_text(own_app)),
            Self::Sort(k) => format!("Sort<{}>", Self::Record(k.clone()).meta_text(own_app)),
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

    /// Whether a value of this type is JSON's to make: a scalar, Text (and the types that hold
    /// text), a list, an option, a map, an enum, or a record.
    #[must_use]
    pub fn reads_json(&self) -> bool {
        match self {
            Self::Int
            | Self::Float
            | Self::Bool
            | Self::Text
            | Self::Date
            | Self::Time
            | Self::Identifier
            | Self::Unit
            | Self::Enum(_)
            | Self::Record(_) => true,
            Self::Option(t) | Self::List(t) | Self::Map(t) => t.reads_json(),
            _ => false,
        }
    }

    /// Whether `json.encode` writes a value of this type: everything JSON can make, and a
    /// `json.Value`, which is a document already.
    pub fn writes_json(&self) -> bool {
        self.reads_json() || matches!(self, Self::Json)
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
            Self::Date => "Date".into(),
            Self::Time => "Time".into(),
            Self::Duration => "Duration".into(),
            Self::Identifier => "std.Identifier".into(),
            Self::RichText => "RichText".into(),
            Self::Bytes => "Bytes".into(),
            Self::Response => "Response".into(),
            Self::Json => "json.Value".into(),
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
            Self::Filter(k) => format!("Filter<{}>", Self::Record(k.clone()).display(own_app)),
            Self::Sort(k) => format!("Sort<{}>", Self::Record(k.clone()).display(own_app)),
            Self::Result(t, e) => {
                format!("Result<{}, {}>", t.display(own_app), e.display(own_app))
            }
            Self::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(|t| t.display(own_app)).collect();
                format!("({})", inner.join(", "))
            }
            Self::Union(keys) => keys
                .iter()
                .map(|k| Self::Record(k.clone()).display(own_app))
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }

    /// Whether a value of this type may stand where `want` is expected: itself, a case of a
    /// union, or — where `host.Resource` is expected — any resource at all, since that type is
    /// what the language has for "a resource of whichever app" (a place a shell holds, a hit
    /// `find` answers), and every resource is one. A list whose items fit `want`'s items fits
    /// it too (`List<TextChannel>` where `List<TextChannel | Direct>` is expected): a list's
    /// items are read, and an item written back goes through the collection that holds it,
    /// never through a list value.
    #[must_use]
    pub fn fits(&self, want: &Self) -> bool {
        match (self, want) {
            (Self::Record(k), Self::Union(keys)) => keys.contains(k),
            (Self::Union(a), Self::Union(b)) => a.iter().all(|k| b.contains(k)),
            (Self::Record(_) | Self::Union(_), Self::Record(k)) if is_any_resource(k) => true,
            (Self::List(found), Self::List(want)) => found.fits(want),
            _ => self.same(want),
        }
    }

    /// The resource types a value of this type may be: one for a record, each case of a union.
    #[must_use]
    pub fn records(&self) -> Vec<TypeKey> {
        match self {
            Self::Record(k) => vec![k.clone()],
            Self::Union(keys) => keys.clone(),
            _ => Vec::new(),
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

/// The type a path names that the language itself declares, with no package behind it: `std`'s
/// text-held types, `std.RichText`, and a `json.Value` as `json.parse` reads one.
#[must_use]
pub fn named(dotted: &[&str]) -> Option<Ty> {
    match dotted {
        ["std", "Identifier"] => Some(Ty::Identifier),
        ["std", "RichText"] => Some(Ty::RichText),
        ["json", "Value"] => Some(Ty::Json),
        _ => None,
    }
}

/// Whether a value of this type is one only a run holds, or a part of a package: never stored
/// state, so never what a field holds.
#[must_use]
pub fn is_transient(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Action | Ty::View | Ty::Bytes | Ty::Response | Ty::Json
    )
}

/// Whether `key` is the host's `Resource`: the type that stands for any resource, of whichever
/// app (`host.Resource`; `find` and `grep` answer these across apps).
#[must_use]
pub fn is_any_resource(key: &TypeKey) -> bool {
    key.app == crate::host::HOST_APP && key.name == crate::host::RESOURCE_TYPE_NAME
}

/// Why `text` is not an ISO calendar date `YYYY-MM-DD`, or nothing when it is one.
pub fn check_date(text: &str) -> Result<(), String> {
    let b = text.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = text.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let shape = b.len() == 10 && b.get(4) == Some(&b'-') && b.get(7) == Some(&b'-');
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return Err(format!(
            "`{text}` is not a date: write it `YYYY-MM-DD`, such as `2026-10-24`"
        ));
    };
    if !shape {
        return Err(format!(
            "`{text}` is not a date: write it `YYYY-MM-DD`, such as `2026-10-24`"
        ));
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => {
            return Err(format!(
                "`{text}` has no month {month}; months are 01 to 12"
            ));
        }
    };
    if day == 0 || day > days {
        return Err(format!(
            "`{text}` has no day {day}; that month has {days} days"
        ));
    }
    Ok(())
}

/// Why `text` is not an ISO-8601 UTC instant `YYYY-MM-DDTHH:MM:SSZ`, or nothing when it is one.
pub fn check_time(text: &str) -> Result<(), String> {
    clock(text, Zone::Utc)
}

/// Why `text` is not a wall-clock time `YYYY-MM-DDTHH:MM[:SS]`, or nothing when it is one: a time
/// of day of no zone, read in the person's own where a value is built from it.
pub fn check_wall_clock(text: &str) -> Result<(), String> {
    clock(text, Zone::Person)
}

/// Whether a time text carries its zone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    /// The instant itself, in UTC (`2026-10-21T09:30:00Z`).
    Utc,
    /// The person's own time of day (`2026-10-21T09:30`).
    Person,
}

fn clock(text: &str, zone: Zone) -> Result<(), String> {
    let shape = || match zone {
        Zone::Utc => format!(
            "`{text}` is not a time: write it in UTC as `YYYY-MM-DDTHH:MM:SSZ`, such as `2026-10-21T09:30:00Z`"
        ),
        Zone::Person => format!(
            "`{text}` is not a wall-clock time: write it as `YYYY-MM-DDTHH:MM[:SS]`, such as `2026-10-21T09:30`"
        ),
    };
    let (Some(date), Some(clock)) = (text.get(..10), text.get(10..)) else {
        return Err(shape());
    };
    let b = clock.as_bytes();
    let ok = b.first() == Some(&b'T')
        && b.get(3) == Some(&b':')
        && b.get(6) == Some(&b':')
        && match zone {
            Zone::Utc => b.len() == 10 && b.get(9) == Some(&b'Z'),
            Zone::Person => matches!(b.len(), 6 | 9),
        };
    let part = |r: std::ops::Range<usize>| -> Option<u32> {
        let p = clock.get(r)?;
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let sec = if b.len() == 9 { part(7..9) } else { Some(0) };
    let (Some(h), Some(m), Some(sec)) = (part(1..3), part(4..6), sec) else {
        return Err(shape());
    };
    if !ok {
        return Err(shape());
    }
    check_date(date)?;
    if h > 23 || m > 59 || sec > 59 {
        return Err(format!(
            "`{text}` is not a time of day: hours run 00 to 23, minutes and seconds 00 to 59"
        ));
    }
    Ok(())
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

    /// A type, or several joined by `|`: a union of resource types.
    fn ty(&mut self) -> Option<Ty> {
        let first = self.one()?;
        if !self.eat('|') {
            return Some(first);
        }
        let mut keys = first.records();
        loop {
            let Ty::Record(k) = self.one()? else {
                return None;
            };
            keys.push(k);
            if !self.eat('|') {
                break;
            }
        }
        (keys.len() > 1).then_some(Ty::Union(keys))
    }

    fn one(&mut self) -> Option<Ty> {
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
            "Date" => Some(Ty::Date),
            "Time" => Some(Ty::Time),
            "Duration" => Some(Ty::Duration),
            "std.Identifier" => Some(Ty::Identifier),
            "RichText" | "std.RichText" => Some(Ty::RichText),
            "Bytes" => Some(Ty::Bytes),
            "Response" => Some(Ty::Response),
            "json.Value" => Some(Ty::Json),
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
            "List" | "Set" | "Map" | "Collection" | "Option" | "Filter" | "Sort" => {
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
                    "Filter" => match *inner {
                        Ty::Record(key) => Ty::Filter(key),
                        _ => return None,
                    },
                    "Sort" => match *inner {
                        Ty::Record(key) => Ty::Sort(key),
                        _ => return None,
                    },
                    _ => Ty::Option(inner),
                })
            }
            _ => Some(Ty::Record(key(&name, self.app))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ty;

    /// Every type a form has a control for is asked by it; the rest are reached only by an
    /// argument given where the action runs or by a default.
    #[test]
    fn every_type_is_asked_by_a_form_or_is_not() {
        let asked = [
            "Int",
            "Float",
            "Bool",
            "Text",
            "RichText",
            "Date",
            "Time",
            "Duration",
            "std.Identifier",
            "Principal",
            "Thing",
            "enum Colour",
            "Option<Duration>",
            "Option<enum Colour>",
            "Option<Thing>",
        ];
        for ty in asked {
            let parsed = Ty::parse_meta(ty, "t.forms").unwrap_or_else(|| panic!("{ty} parses"));
            assert!(parsed.asked_by_form(), "a form asks for {ty}");
        }
        let unasked = [
            "Bytes",
            "Response",
            "json.Value",
            "Action",
            "View",
            "List<Text>",
            "Set<Text>",
            "Map<Text>",
            "Collection<Thing>",
            "Filter<Thing>",
            "Option<List<Text>>",
            "Option<Option<Text>>",
        ];
        assert!(!Ty::Unit.asked_by_form(), "no form asks for no value");
        for ty in unasked {
            let parsed = Ty::parse_meta(ty, "t.forms").unwrap_or_else(|| panic!("{ty} parses"));
            assert!(!parsed.asked_by_form(), "no form asks for {ty}");
        }
    }
}
