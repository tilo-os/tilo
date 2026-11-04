//! Working out a derived key (`derive folded: Bool = done and children.all(folded)`): a value a
//! type's resources have by what the collection holds, from the record's own facts
//! ([`tilolang::derived`]).
//!
//! The one evaluator for every store. What it reads comes from [`Facts`]: the fields of an item,
//! the items standing directly under it and the item it stands under. A store that keeps an index
//! works the values out as changes come and keeps them where it reads fields from (the index,
//! `tilolang_tilo::index`), passing its own index as the facts; a store that keeps none
//! ([`crate::amstore`]) works a key out as it is read, over its own documents.

use serde_json::Value as Json;
use tilolang::derived::Derived;
use tilolang::pred::Lit;

use crate::store::{FieldValue, Scalar};

/// A value a derived key's expression computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Computed {
    Bool(bool),
    Int(i64),
}

impl Computed {
    /// What a field of the item holds of it.
    #[must_use]
    pub fn value(self) -> FieldValue {
        FieldValue::Scalar(match self {
            Self::Bool(b) => Scalar::Bool(b),
            Self::Int(i) => Scalar::Int(i),
        })
    }

    /// Whether a field's value is it.
    #[must_use]
    pub fn is(self, value: &FieldValue) -> bool {
        match (self, value) {
            (Self::Bool(b), FieldValue::Scalar(Scalar::Bool(held))) => b == *held,
            (Self::Int(i), FieldValue::Scalar(Scalar::Int(held)) | FieldValue::Counter(held)) => {
                i == *held
            }
            _ => false,
        }
    }

    /// What the field holds as a derived key reads it, `value` being what a store holds of it
    /// (`None` for a register nothing wrote) and `default` its declared default, `null` for one
    /// of none: every key reads a flag or a count.
    #[must_use]
    pub fn held(value: Option<&FieldValue>, default: &Json) -> Self {
        if let Some(now) = value.and_then(Self::of) {
            return now;
        }
        match default {
            Json::Bool(b) => Self::Bool(*b),
            Json::Number(n) if n.is_i64() => Self::Int(n.as_i64().unwrap_or(0)),
            _ => Self::Bool(false),
        }
    }

    fn of(value: &FieldValue) -> Option<Self> {
        match value {
            FieldValue::Scalar(Scalar::Bool(b)) => Some(Self::Bool(*b)),
            FieldValue::Scalar(Scalar::Int(i)) | FieldValue::Counter(i) => Some(Self::Int(*i)),
            _ => None,
        }
    }
}

/// What a derived key is worked out from: the resources it reads, named as
/// [`crate::value::ResourceRef::id_string`] writes them (the document, then `/item` for an item).
pub trait Facts {
    /// Why a read failed.
    type Error;

    /// What the field `field` of the resource at `path` holds, its declared default when
    /// nothing wrote it; a field a key of its own holds is the value that key derives.
    ///
    /// # Errors
    /// What it holds cannot be read.
    fn field(&mut self, path: &str, field: &str) -> Result<Computed, Self::Error>;

    /// The items standing directly under the resource at `path`, through its type's parent
    /// field, as their item ids.
    ///
    /// # Errors
    /// What it holds cannot be read.
    fn children(&mut self, path: &str) -> Result<Vec<String>, Self::Error>;

    /// The item the resource at `path` stands under, through its type's parent field: `None`
    /// when it stands under none.
    ///
    /// # Errors
    /// What it holds cannot be read.
    fn above(&mut self, path: &str) -> Result<Option<String>, Self::Error>;
}

/// Evaluate `expr` for the item at `path`, reading its fields and the items under and above it
/// through `facts`.
///
/// # Errors
/// What it reads cannot be read.
pub fn eval<F: Facts>(
    facts: &mut F,
    path: &str,
    expr: &Derived<String>,
) -> Result<Computed, F::Error> {
    Ok(match expr {
        Derived::Bool(b) => Computed::Bool(*b),
        Derived::Int(i) => Computed::Int(*i),
        Derived::Field(id) => facts.field(path, id)?,
        Derived::Not(inner) => match eval(facts, path, inner)? {
            Computed::Bool(b) => Computed::Bool(!b),
            count @ Computed::Int(_) => count,
        },
        Derived::And(parts) => {
            for part in parts {
                if eval(facts, path, part)? != Computed::Bool(true) {
                    return Ok(Computed::Bool(false));
                }
            }
            Computed::Bool(true)
        }
        Derived::Or(parts) => {
            for part in parts {
                if eval(facts, path, part)? == Computed::Bool(true) {
                    return Ok(Computed::Bool(true));
                }
            }
            Computed::Bool(false)
        }
        Derived::Compare { op, left, right } => {
            let (Computed::Int(left), Computed::Int(right)) =
                (eval(facts, path, left)?, eval(facts, path, right)?)
            else {
                return Ok(Computed::Bool(false));
            };
            Computed::Bool(op.holds(left, right))
        }
        Derived::Children => Computed::Int(count(facts, path, None)?),
        Derived::Count { holds } => Computed::Int(count(facts, path, Some(holds))?),
        Derived::All { holds } => Computed::Bool(every(facts, path, holds, true)?),
        Derived::Any { holds } => Computed::Bool(every(facts, path, holds, false)?),
        Derived::Above { holds } => {
            // The items it stands under, through the item each of them stands under: one with
            // none above it holds.
            let mut all = true;
            for at in chain(facts, path)?.into_iter().skip(1) {
                if eval(facts, &at, holds)? != Computed::Bool(true) {
                    all = false;
                    break;
                }
            }
            Computed::Bool(all)
        }
    })
}

/// How many items stand under the item at `path`, those holding `holds` when it is given.
fn count<F: Facts>(
    facts: &mut F,
    path: &str,
    holds: Option<&Derived<String>>,
) -> Result<i64, F::Error> {
    let children = facts.children(path)?;
    let Some(holds) = holds else {
        return Ok(i64::try_from(children.len()).unwrap_or(i64::MAX));
    };
    let mut n = 0_i64;
    for child in children {
        if eval(facts, &item_path(path, &child), holds)? == Computed::Bool(true) {
            n = n.saturating_add(1);
        }
    }
    Ok(n)
}

/// Whether every item standing under the item at `path` holds `holds` (`all`), or one of them
/// does.
fn every<F: Facts>(
    facts: &mut F,
    path: &str,
    holds: &Derived<String>,
    all: bool,
) -> Result<bool, F::Error> {
    for child in facts.children(path)? {
        let holds = eval(facts, &item_path(path, &child), holds)? == Computed::Bool(true);
        if holds != all {
            return Ok(!all);
        }
    }
    Ok(all)
}

/// The resource at `path` and the items it stands under, deepest first.
///
/// # Errors
/// What it reads cannot be read.
pub fn chain<F: Facts>(facts: &mut F, path: &str) -> Result<Vec<String>, F::Error> {
    let mut out = vec![path.to_owned()];
    let mut at = path.to_owned();
    while let Some(above) = facts.above(&at)? {
        if out.contains(&above) {
            break;
        }
        out.push(above.clone());
        at = above;
    }
    Ok(out)
}

/// How deep the item at `path` stands among the items it stands under.
///
/// # Errors
/// What it reads cannot be read.
pub fn depth<F: Facts>(facts: &mut F, path: &str) -> Result<usize, F::Error> {
    Ok(chain(facts, path)?.len().saturating_sub(1))
}

/// The path of the item `id` of the document of the resource at `path`.
#[must_use]
pub fn item_path(path: &str, id: &str) -> String {
    let doc = path.split_once('/').map_or(path, |(doc, _)| doc);
    format!("{doc}/{id}")
}

/// The path a parent field's value names: the item the record stands under, `None` for one
/// standing under none.
#[must_use]
pub fn parent_in(value: &FieldValue) -> Option<String> {
    match value {
        FieldValue::Scalar(Scalar::Json(json)) => match Lit::of_json(json) {
            Some(Lit::Ref(path)) => Some(path),
            _ => None,
        },
        _ => None,
    }
}
