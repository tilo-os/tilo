//! Derived keys in the index (`derive folded: Bool = done and children.all(folded)`): a value a
//! record holds by what the collection holds, worked out from the record's own facts and kept
//! where a view reads fields from ([`tilolang::derived`]).
//!
//! The items standing under an item are the index's own: the list of the items naming it as
//! their parent field ([`children_list`]), kept as an item is placed, dropped and moved, so
//! counting them or checking every one costs the items under the item and nothing else. A
//! derived key is written as a field of the item, and read as one; only [`super::derive`]
//! writes it. What a key says is worked out by [`tilolang_runtime::derived`], the one evaluator
//! every store reads its keys through; this module is what that evaluator reads here.

use tilolang::derived::Derived;
use tilolang_runtime::derived::{Computed, Facts};
use tilolang_runtime::store::doc_of;

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

/// What the field `field` of the item at `path` holds, as a derived key reads it: its value, or
/// what an item without one holds.
fn held(sink: &mut impl Sink, path: &str, field: &FieldSchema) -> Result<Computed, IndexError> {
    let value = sink.field(path, &field.id)?;
    Ok(Computed::held(value.as_ref(), &field.default))
}

/// The sink as the evaluator reads it: the fields of an item from the index, the items under it
/// from its children list, and the item it stands under from its parent field.
struct FactsOf<'a, S: Sink>(&'a mut S);

impl<S: Sink> Facts for FactsOf<'_, S> {
    type Error = IndexError;

    fn field(&mut self, path: &str, field: &str) -> Result<Computed, IndexError> {
        let Some(schema) = type_of(self.0, path)? else {
            return Ok(Computed::Bool(false));
        };
        match schema.field(field) {
            Some(field) => held(self.0, path, field),
            None => Ok(Computed::Bool(false)),
        }
    }

    fn children(&mut self, path: &str) -> Result<Vec<String>, IndexError> {
        let Some(schema) = type_of(self.0, path)? else {
            return Ok(Vec::new());
        };
        Ok(children_in(self.0, path, schema)?
            .into_iter()
            .map(|(_, id)| id)
            .collect())
    }

    fn above(&mut self, path: &str) -> Result<Option<String>, IndexError> {
        let Some(schema) = type_of(self.0, path)? else {
            return Ok(None);
        };
        let Some(parent) = schema.parent() else {
            return Ok(None);
        };
        parent_of(self.0, path, parent)
    }
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
    Ok(sink
        .field(path, parent)?
        .as_ref()
        .and_then(tilolang_runtime::derived::parent_in))
}

/// Evaluate `expr` for the item at `path` over the index.
///
/// # Errors
/// The index cannot be read.
pub fn eval(
    sink: &mut impl Sink,
    path: &str,
    expr: &Derived<String>,
) -> Result<Computed, IndexError> {
    tilolang_runtime::derived::eval(&mut FactsOf(sink), path, expr)
}

/// The items standing under the item at `path` (of the type `schema`), as their list's entries.
///
/// # Errors
/// The index cannot be read.
fn children_in(
    sink: &mut impl Sink,
    path: &str,
    schema: &TypeSchema,
) -> Result<Vec<Entry>, IndexError> {
    let Some(parent) = schema.parent() else {
        return Ok(Vec::new());
    };
    sink.entries(&children_list(path, parent))
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
    Ok(children_in(sink, path, schema)?
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
    tilolang_runtime::derived::chain(&mut FactsOf(sink), path)
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
