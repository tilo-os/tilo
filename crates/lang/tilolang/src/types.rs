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

/// What a field is to the platform, as its declaration marks it: the text that names the
/// record, the record's substance, what the type held before, the flag saying an item is
/// finished, and — for an item that comes round again or reminds someone — the fields a repeat
/// counts from and a reminder reads. A
/// host holding the record finds them here, without the app's code: which field repeats, which
/// day it counts from, when it reminds, and whom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldRole {
    /// `(title)`: the text a person knows the record by.
    Title,
    /// `(body)`: the record's substance, shown where a title would stand.
    Body,
    /// `(previously)`: what the type held before, which only its migrations read.
    Previously,
    /// `(repeats: due)`: the field holds a `std.Repeat`, counting from the day named here.
    Repeats { date: String },
    /// `(reminds: due, at, who: assignee)`: the field holds a `std.Reminder` of the day named
    /// here, at the time of day named here where the type has one, for the person named here
    /// where it names one.
    Reminds {
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        who: Option<String>,
    },
    /// `(parent)`: the field holding the item this one stands under (`Option<Task>`), the one
    /// a type's items nest by and its derived keys read the items above and under it through
    /// ([`crate::derived`]).
    Parent,
    /// `(time of: due)`: the time of day the day named here is due at, so the two read as one
    /// fact (`Today 09:00`) wherever the day shows and one control picks both.
    TimeOf { date: String },
    /// `(done)`: the `Bool` that says the item is finished: what a place leaves out of its
    /// count and folds into its foot, and what a window's complete action sets.
    Done,
}

impl FieldRole {
    /// The word the marker is written with, without its fields.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Body => "body",
            Self::Previously => "previously",
            Self::Done => "done",
            Self::Repeats { .. } => "repeats",
            Self::Reminds { .. } => "reminds",
            Self::Parent => "parent",
            Self::TimeOf { .. } => "time",
        }
    }

    /// The marker as the declaration writes it, `repeats: due` or `reminds: due, at, who:
    /// assignee`.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Title | Self::Body | Self::Previously | Self::Done | Self::Parent => {
                self.word().to_owned()
            }
            Self::TimeOf { date } => format!("time of: {date}"),
            Self::Repeats { date } => format!("repeats: {date}"),
            Self::Reminds { date, time, who } => {
                let mut text = format!("reminds: {date}");
                if let Some(time) = time {
                    text.push_str(", ");
                    text.push_str(time);
                }
                if let Some(who) = who {
                    text.push_str(", who: ");
                    text.push_str(who);
                }
                text
            }
        }
    }

    /// Whether the field names the record (`(title)`).
    #[must_use]
    pub const fn is_title(&self) -> bool {
        matches!(self, Self::Title)
    }

    /// Whether the field is the record's substance (`(body)`).
    #[must_use]
    pub const fn is_body(&self) -> bool {
        matches!(self, Self::Body)
    }

    /// Whether the field is what the type held before (`(previously)`).
    #[must_use]
    pub const fn is_previously(&self) -> bool {
        matches!(self, Self::Previously)
    }

    /// Whether the field holds the item this one stands under (`(parent)`).
    #[must_use]
    pub const fn is_parent(&self) -> bool {
        matches!(self, Self::Parent)
    }

    /// Whether the field says the item is finished (`(done)`).
    #[must_use]
    pub const fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }

    /// The day the field is the time of (`(time of: due)`), when it is one.
    #[must_use]
    pub fn time_of(&self) -> Option<&str> {
        match self {
            Self::TimeOf { date } => Some(date),
            _ => None,
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
    /// `std.Identifier`: an account of some service, held as its URI `scheme:value`
    /// (`mailto:jane@x.com`, `tel:+4930123456`); not a `Text` (see [`crate::identifier`]).
    Identifier,
    /// `std.Repeat`: how a repeating item comes round again, held as its RFC 5545 rule text
    /// (`FREQ=WEEKLY;INTERVAL=2`), which carries `X-FROM=DONE` when the next occurrence counts
    /// from the day it was completed (see [`crate::repeat`]), and its field is marked
    /// `(repeats: due)`, naming the day it counts from; not a `Text`.
    Repeat,
    /// `std.Reminder`: how long before a due moment to remind, held as its RFC 5545 `TRIGGER`
    /// duration (`-PT15M`), read against the person's zone (see [`crate::repeat`]), and its
    /// field is marked `(reminds: due, at, who: assignee)`, naming the day, time of day and
    /// person it reads; not a `Text`.
    Reminder,
    /// `RichText`: formatted text in which resources are referenced (a person, a channel, a
    /// task), held as tilo-doc's rich text, the one rich-text model of the platform. A field of
    /// it merges by splices and marks.
    RichText,
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
    /// it (forms-and-diagrams.md §1): text, a number, a flag, a day or a moment, a person, a
    /// resource, a case of an enum, or none of one of those. Each has its control.
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
            Self::Date => "Date".into(),
            Self::Time => "Time".into(),
            Self::Identifier => "std.Identifier".into(),
            Self::Repeat => "std.Repeat".into(),
            Self::Reminder => "std.Reminder".into(),
            Self::RichText => "RichText".into(),
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
    /// `find` answers), and every resource is one.
    #[must_use]
    pub fn fits(&self, want: &Self) -> bool {
        match (self, want) {
            (Self::Record(k), Self::Union(keys)) => keys.contains(k),
            (Self::Union(a), Self::Union(b)) => a.iter().all(|k| b.contains(k)),
            (Self::Record(_) | Self::Union(_), Self::Record(k)) if is_any_resource(k) => true,
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
            "std.Identifier" => Some(Ty::Identifier),
            "std.Repeat" => Some(Ty::Repeat),
            "std.Reminder" => Some(Ty::Reminder),
            "RichText" | "std.RichText" => Some(Ty::RichText),
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
