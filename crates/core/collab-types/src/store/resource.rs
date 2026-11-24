//! A resource as the store is given it and as its index holds it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Cell, Sort, Value};

/// The field a resource's parent is indexed under: a filter `Is(PARENT, parent)` lists the
/// resources nested in one resource.
pub const PARENT: &str = "$parent";

/// A resource as a write gives it: who may read it, where it is nested, its place, its indexed
/// fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// Its reader set: whatever names who may read it, which also picks the key of the
    /// document it is placed in.
    pub readers: String,
    pub parent: Option<String>,
    pub order: Option<String>,
    pub fields: BTreeMap<String, Cell>,
}

/// What the index holds of one resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Indexed {
    /// The document the resource is stored in: where a read of what the index does not hold goes.
    /// The store's own: it never leaves the device.
    #[serde(skip)]
    pub doc: String,
    /// The resource it is nested in; `None` for a resource at the top of the workspace.
    pub parent: Option<String>,
    /// Its place among its parent's resources of its kind; `None` for a kind without one.
    pub order: Option<String>,
    /// Each indexed field it holds, by name.
    pub fields: BTreeMap<String, Cell>,
}

impl Indexed {
    /// The scalar field `field` holds; [`Value::Null`] when it holds none.
    #[must_use]
    pub fn scalar(&self, field: &str) -> Value {
        if field == PARENT {
            return self.parent.clone().map_or(Value::Null, Value::Text);
        }
        self.fields
            .get(field)
            .and_then(Cell::one)
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// Its place among its parent's resources, as the index orders it; [`Value::Null`] for a kind
    /// without one.
    #[must_use]
    pub fn place(&self) -> Value {
        self.order.clone().map_or(Value::Null, Value::Text)
    }

    /// Where the resource stands in `sort`: what it sorts by, then its place. A field it does
    /// not hold sorts as [`Value::Null`], as a filter reads it; a set is no order.
    pub fn key(&self, sort: &Sort) -> Option<(Value, Value)> {
        match sort {
            Sort::Position => Some((self.place(), Value::Null)),
            Sort::Field(field) if field == PARENT => Some((self.scalar(PARENT), self.place())),
            Sort::Field(field) => {
                let value = match self.fields.get(field) {
                    Some(Cell::Set(_)) => return None,
                    Some(cell) => cell.one()?.clone(),
                    None => Value::Null,
                };
                Some((value, self.place()))
            }
        }
    }

    /// Whether the resource holds nothing in the scalar field `field`: it is listed under
    /// [`Value::Null`] there.
    pub fn lacks(&self, field: &str) -> bool {
        if field == PARENT {
            return self.parent.is_none();
        }
        !self.fields.contains_key(field)
    }

    /// Each value the resource is listed under in the index of `field`.
    pub fn indexed(&self) -> impl Iterator<Item = (&str, Value)> {
        let parent = self
            .parent
            .as_ref()
            .map(|parent| (PARENT, Value::Text(parent.clone())));
        parent
            .into_iter()
            .chain(self.fields.iter().flat_map(|(field, cell)| {
                let values: Vec<Value> = match cell {
                    Cell::One(value) => vec![value.clone()],
                    Cell::Set(members) => members.iter().cloned().collect(),
                };
                values.into_iter().map(move |value| (field.as_str(), value))
            }))
    }
}

/// One resource nested in a parent, as it stood at a version ([`super::wire::Command::ChildrenAt`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PastChild {
    pub kind: String,
    pub id: String,
    /// Its place, its parent and its indexed fields then.
    pub indexed: Indexed,
    /// Its content then; `None` where it held none.
    pub content: Option<serde_json::Value>,
}

/// What restoring some resources to a version would change, as a person reviews it before they
/// restore: the heads of every document it reads, and the resources that differ.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Review {
    /// The heads, each a full change hash, of every document the restore writes, sorted: a
    /// restore of this review is refused once any of them moved.
    pub live: Vec<String>,
    /// The ids of the resources that differ from the version, sorted.
    pub differing: Vec<String>,
}
