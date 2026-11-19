//! The values the index holds: a resource field's scalar, or the members of a set.

use std::cmp::Ordering;
use std::collections::BTreeSet;

/// A scalar a resource field holds, as the index compares it.
///
/// Values of different kinds sort by kind: nothing first, then flags, numbers and texts. An
/// integer and a float compare by the number they stand for, so a field one app writes as `3`
/// and another as `3.0` sorts and filters as one.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Value {
    /// An absent field, or one holding nothing.
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// A text, an id, a date or an instant as ISO text, a reference.
    Text(String),
}

impl Value {
    const fn rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::Int(_) | Self::Float(_) => 2,
            Self::Text(_) => 3,
        }
    }

    /// The text, for a text value.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<i64> for Value {
    fn from(number: i64) -> Self {
        Self::Int(number)
    }
}

impl From<bool> for Value {
    fn from(flag: bool) -> Self {
        Self::Bool(flag)
    }
}

/// An integer and a float compared by the number they stand for: the integer as the float
/// nearest it, and on a tie the integer's own digits against the float's integral document, so two
/// integers a float cannot tell apart still compare as integers do.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "the float only decides when it differs; an exact tie is settled in integers"
)]
fn int_float(int: i64, float: f64) -> Ordering {
    match (int as f64).total_cmp(&float) {
        Ordering::Equal if float.fract() == 0.0 && float.abs() < 9.3e18 => int.cmp(&(float as i64)),
        other => other,
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::Float(a), Self::Float(b)) => a.total_cmp(b),
            (Self::Int(a), Self::Float(b)) => int_float(*a, *b),
            (Self::Float(a), Self::Int(b)) => int_float(*b, *a).reverse(),
            (Self::Text(a), Self::Text(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Value {}

impl std::hash::Hash for Value {
    /// Equal values hash alike: an integer and a float that compare equal stand for the same
    /// number, so both hash that number's float bits.
    #[allow(
        clippy::cast_precision_loss,
        reason = "an integer equal to a float is the float's exact value"
    )]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.rank().hash(state);
        match self {
            Self::Null => {}
            Self::Bool(flag) => flag.hash(state),
            Self::Int(number) => (*number as f64).to_bits().hash(state),
            Self::Float(number) => number.to_bits().hash(state),
            Self::Text(text) => text.hash(state),
        }
    }
}

/// What a resource holds in one indexed field.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Cell {
    /// A scalar field's value.
    One(Value),
    /// A set's members.
    Set(BTreeSet<Value>),
}

impl Cell {
    /// The scalar, for a scalar field.
    #[must_use]
    pub const fn one(&self) -> Option<&Value> {
        match self {
            Self::One(value) => Some(value),
            Self::Set(_) => None,
        }
    }
}

impl From<Value> for Cell {
    fn from(value: Value) -> Self {
        Self::One(value)
    }
}
