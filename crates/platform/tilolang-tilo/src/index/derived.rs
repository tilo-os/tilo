//! Derived keys in the index (`derive folded: Bool = done and children.all(folded)`): a value a
//! record holds by what the collection holds, worked out from the record's own facts and kept
//! where a view reads fields from ([`tilolang::derived`]).
//!
//! The items standing under an item are the index's own: the list of the items naming it as
//! their parent field ([`children_list`]), kept as an item is placed, dropped and moved, so
//! counting them or checking every one costs the items under the item and nothing else. A
//! derived key is written as a field of the item, and read as one; only [`super::derive`]
//! writes it.

use tilolang::derived::Derived;
use tilolang::pred::Lit;
use tilolang_runtime::store::{FieldValue, Scalar, doc_of};

use super::IndexError;
use super::derive::{Sink, item_path, member_list};
use super::lists::Entry;
use crate::schema::{FieldSchema, TypeSchema, lookup};

/// The list of the items standing under the item at `path` of the type with the parent field
/// `parent`: the entries are the items' ids, as the items under one are a set, not an order.
#[must_use]
pub fn children_list(path: &str, parent: &str) -> String {
    member_list(path, parent)
}

/// The entry of the item `id` in the list of the items under its parent.
#[must_use]
pub fn child_entry(id: &str) -> Entry {
    (id.to_owned(), id.to_owned())
}

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

    fn of(value: &FieldValue) -> Option<Self> {
        match value {
            FieldValue::Scalar(Scalar::Bool(b)) => Some(Self::Bool(*b)),
            FieldValue::Scalar(Scalar::Int(i)) | FieldValue::Counter(i) => Some(Self::Int(*i)),
            _ => None,
        }
    }
}

/// What the field `field` of the item at `path` holds, as a derived key reads it: its value, or
/// what an item without one holds.
fn held(sink: &mut impl Sink, path: &str, field: &FieldSchema) -> Result<Computed, IndexError> {
    let value = sink.field(path, &field.id)?;
    if let Some(now) = value.as_ref().and_then(Computed::of) {
        return Ok(now);
    }
    Ok(match field.default {
        serde_json::Value::Bool(b) => Computed::Bool(b),
        serde_json::Value::Number(ref n) if n.is_i64() => Computed::Int(n.as_i64().unwrap_or(0)),
        _ => Computed::Bool(false),
    })
}

/// The path the item at `path` stands under, through the parent field `parent`: `None` when it
/// stands under none.
///
/// # Errors
/// The index cannot be read.
pub fn parent_of(
    sink: &mut impl Sink,
    path: &str,
    parent: &str,
) -> Result<Option<String>, IndexError> {
    Ok(sink.field(path, parent)?.as_ref().and_then(parent_in))
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

/// Evaluate `expr` for the item at `path`, of the type `schema`, reading its fields and the
/// items standing under it from `sink`.
///
/// # Errors
/// The index cannot be read.
pub fn eval(
    sink: &mut impl Sink,
    path: &str,
    schema: &TypeSchema,
    expr: &Derived<String>,
) -> Result<Computed, IndexError> {
    Ok(match expr {
        Derived::Bool(b) => Computed::Bool(*b),
        Derived::Int(i) => Computed::Int(*i),
        Derived::Field(id) => match schema.field(id) {
            Some(field) => held(sink, path, field)?,
            None => Computed::Bool(false),
        },
        Derived::Not(inner) => match eval(sink, path, schema, inner)? {
            Computed::Bool(b) => Computed::Bool(!b),
            count @ Computed::Int(_) => count,
        },
        Derived::And(parts) => {
            for part in parts {
                if eval(sink, path, schema, part)? != Computed::Bool(true) {
                    return Ok(Computed::Bool(false));
                }
            }
            Computed::Bool(true)
        }
        Derived::Or(parts) => {
            for part in parts {
                if eval(sink, path, schema, part)? == Computed::Bool(true) {
                    return Ok(Computed::Bool(true));
                }
            }
            Computed::Bool(false)
        }
        Derived::Compare { op, left, right } => {
            let (Computed::Int(left), Computed::Int(right)) = (
                eval(sink, path, schema, left)?,
                eval(sink, path, schema, right)?,
            ) else {
                return Ok(Computed::Bool(false));
            };
            Computed::Bool(op.holds(left, right))
        }
        Derived::Children => Computed::Int(count(sink, path, schema, None)?),
        Derived::Count { holds } => Computed::Int(count(sink, path, schema, Some(holds))?),
        Derived::All { holds } => Computed::Bool(every(sink, path, schema, holds, true)?),
        Derived::Any { holds } => Computed::Bool(every(sink, path, schema, holds, false)?),
        Derived::Above { holds } => {
            // The items it stands under, through the item each of them stands under: one with
            // none above it holds.
            let mut all = true;
            for at in chain(sink, path)?.into_iter().skip(1) {
                if eval(sink, &at, schema, holds)? != Computed::Bool(true) {
                    all = false;
                    break;
                }
            }
            Computed::Bool(all)
        }
    })
}

/// The items standing under the item at `path`, as their paths.
///
/// # Errors
/// The index cannot be read.
pub fn children(
    sink: &mut impl Sink,
    path: &str,
    schema: &TypeSchema,
) -> Result<Vec<String>, IndexError> {
    let Some(parent) = schema.parent() else {
        return Ok(Vec::new());
    };
    Ok(sink
        .entries(&children_list(path, parent))?
        .into_iter()
        .map(|(_, id)| item_path(doc_of(path), &id))
        .collect())
}

/// The way a key's value is kept in step with a change: the items under the item are read, or
/// the items above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// It reads the items under the item: worked out from the deepest item up.
    Under,
    /// It reads the items above the item: worked out from the top item down.
    Above,
}

/// The way the key `field` of `schema` is kept in step, reading the keys it names: a key that
/// reads another reads it the way the other is kept, and the type declares the keys a key reads
/// before it.
#[must_use]
pub fn reach(schema: &TypeSchema, field: &FieldSchema) -> Reach {
    let mut above: Vec<&str> = Vec::new();
    for key in &schema.fields {
        let Some(expr) = &key.derived else {
            continue;
        };
        let names_the_above = expr
            .fields()
            .iter()
            .any(|read| above.contains(&read.as_str()));
        let reads_above = expr.reads_above() || names_the_above;
        if key.id == field.id {
            return if reads_above {
                Reach::Above
            } else {
                Reach::Under
            };
        }
        if reads_above {
            above.push(key.id.as_str());
        }
    }
    Reach::Under
}

/// How many items stand under the item at `path`, those holding `holds` when it is given.
fn count(
    sink: &mut impl Sink,
    path: &str,
    schema: &TypeSchema,
    holds: Option<&Derived<String>>,
) -> Result<i64, IndexError> {
    let Some(parent) = schema.parent() else {
        return Ok(0);
    };
    let entries = sink.entries(&children_list(path, parent))?;
    if holds.is_none() {
        return Ok(i64::try_from(entries.len()).unwrap_or(i64::MAX));
    }
    let mut n = 0_i64;
    for (_, id) in entries {
        let child = item_path(doc_of(path), &id);
        if eval(sink, &child, schema, holds.unwrap_or(&Derived::Bool(false)))?
            == Computed::Bool(true)
        {
            n = n.saturating_add(1);
        }
    }
    Ok(n)
}

/// Whether every item standing under the item at `path` holds `holds` (`all`), or one of them
/// does.
fn every(
    sink: &mut impl Sink,
    path: &str,
    schema: &TypeSchema,
    holds: &Derived<String>,
    all: bool,
) -> Result<bool, IndexError> {
    let Some(parent) = schema.parent() else {
        return Ok(all);
    };
    for (_, id) in sink.entries(&children_list(path, parent))? {
        let child = item_path(doc_of(path), &id);
        let holds = eval(sink, &child, schema, holds)? == Computed::Bool(true);
        if holds != all {
            return Ok(!all);
        }
    }
    Ok(all)
}

/// Whether the type `schema` derives any value from the field `field`.
#[must_use]
pub fn reads(schema: &TypeSchema, field: &str) -> bool {
    schema
        .fields
        .iter()
        .filter_map(|f| f.derived.as_ref())
        .any(|d| d.fields().iter().any(|read| read.as_str() == field))
}

/// The type of the item at `path`, when the index names one.
///
/// # Errors
/// The index cannot be read.
pub fn type_of(
    sink: &mut impl Sink,
    path: &str,
) -> Result<Option<&'static TypeSchema>, IndexError> {
    Ok(sink.text(path, super::TYPE)?.and_then(|ty| lookup(&ty)))
}

/// The item at `path` and the items it stands under, deepest first.
///
/// # Errors
/// The index cannot be read.
pub fn chain(sink: &mut impl Sink, path: &str) -> Result<Vec<String>, IndexError> {
    let mut out = vec![path.to_owned()];
    let mut at = path.to_owned();
    while let Some(schema) = type_of(sink, &at)? {
        let Some(parent) = schema.parent() else {
            break;
        };
        let Some(above) = parent_of(sink, &at, parent)? else {
            break;
        };
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
/// The index cannot be read.
pub fn depth(sink: &mut impl Sink, path: &str) -> Result<usize, IndexError> {
    Ok(chain(sink, path)?.len().saturating_sub(1))
}

/// The item `path` was placed, or stopped being: it stands under the item `parent` (its own
/// path) as `standing`, or under none.
///
/// # Errors
/// The index cannot be read or written.
pub fn standing(
    sink: &mut impl Sink,
    path: &str,
    parent_field: &str,
    parent: Option<&str>,
    standing: bool,
) -> Result<(), IndexError> {
    let Some(id) = super::derive::id_of(path) else {
        return Ok(());
    };
    let Some(parent) = parent else {
        return Ok(());
    };
    let list = children_list(parent, parent_field);
    if standing {
        sink.insert(&list, child_entry(id))?;
    } else {
        sink.remove_entry(&list, &child_entry(id))?;
    }
    Ok(())
}

/// The item at `path` stands under `now` and stood under `before`: each item under an item
/// keeps the item where it is now.
///
/// # Errors
/// The index cannot be written.
pub fn reparent(
    sink: &mut impl Sink,
    path: &str,
    parent_field: &str,
    before: Option<&str>,
    now: Option<&str>,
) -> Result<(), IndexError> {
    if before.is_some_and(|before| Some(before) != now) {
        standing(sink, path, parent_field, before, false)?;
    }
    if now.is_some_and(|now| Some(now) != before) {
        standing(sink, path, parent_field, now, true)?;
    }
    Ok(())
}
