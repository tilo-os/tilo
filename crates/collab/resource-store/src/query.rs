//! A query of one kind: which resources, in what order, from where on, how many.

use std::ops::Bound;

use crate::index::Indexed;
use crate::value::{Cell, Value};

/// A page of a kind's resources. See the crate docs.
#[derive(Clone, Debug)]
pub struct Query {
    /// The kind ([`crate::Type::name`]).
    pub kind: String,
    /// What every resource shown holds; all of them.
    pub filters: Vec<Filter>,
    pub order: Order,
    /// Where the previous page ended; the first page when `None`.
    pub after: Option<Cursor>,
    /// The most resources the page shows.
    pub limit: usize,
}

impl Query {
    /// The first `limit` resources of `kind` in its own order.
    #[must_use]
    pub fn of(kind: impl Into<String>, limit: usize) -> Self {
        Self {
            kind: kind.into(),
            filters: Vec::new(),
            order: Order::default(),
            after: None,
            limit,
        }
    }

    /// This query, also holding to `filter`.
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// This query in `order`.
    #[must_use]
    pub fn order(mut self, order: Order) -> Self {
        self.order = order;
        self
    }

    /// This query from where `page` ended.
    #[must_use]
    pub fn after(mut self, cursor: Option<Cursor>) -> Self {
        self.after = cursor;
        self
    }
}

/// What a resource must hold to be shown.
#[derive(Clone, Debug)]
pub enum Filter {
    /// The scalar field holds the value (an absent field holds [`Value::Null`]).
    Is(String, Value),
    /// The set field has the value among its members, or the scalar field holds it.
    Has(String, Value),
    /// The scalar field's value lies between the bounds.
    Within {
        field: String,
        from: Bound<Value>,
        to: Bound<Value>,
    },
    /// The text field holds the text, whatever its letters' case.
    Contains(String, String),
}

impl Filter {
    pub(crate) fn holds(&self, stored: &Indexed) -> bool {
        match self {
            Self::Is(field, value) => stored.scalar(field) == *value,
            Self::Has(field, value) => match stored.fields.get(field) {
                Some(Cell::Set(members)) => members.contains(value),
                Some(Cell::One(held)) => held == value,
                None => *value == Value::Null,
            },
            Self::Within { field, from, to } => within(&stored.scalar(field), from, to),
            Self::Contains(field, text) => stored
                .scalar(field)
                .as_text()
                .is_some_and(|held| contains_folded(held, text)),
        }
    }
}

pub(crate) fn within(value: &Value, from: &Bound<Value>, to: &Bound<Value>) -> bool {
    let above = match from {
        Bound::Included(from) => value >= from,
        Bound::Excluded(from) => value > from,
        Bound::Unbounded => true,
    };
    above && !past(value, to)
}

/// Whether `value` lies beyond the upper bound `to`.
pub(crate) fn past(value: &Value, to: &Bound<Value>) -> bool {
    match to {
        Bound::Included(to) => value > to,
        Bound::Excluded(to) => value >= to,
        Bound::Unbounded => false,
    }
}

fn contains_folded(held: &str, text: &str) -> bool {
    if text.is_ascii() && held.is_ascii() {
        let text = text.as_bytes();
        text.is_empty()
            || held
                .as_bytes()
                .windows(text.len())
                .any(|window| window.eq_ignore_ascii_case(text))
    } else {
        held.to_lowercase().contains(&text.to_lowercase())
    }
}

/// The order a page lists resources in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Order {
    pub by: Sort,
    pub descending: bool,
}

impl Order {
    /// By the scalar field `field`, smallest first. A resource without the field is not listed.
    #[must_use]
    pub fn by(field: impl Into<String>) -> Self {
        Self {
            by: Sort::Field(field.into()),
            descending: false,
        }
    }

    /// This order turned around.
    #[must_use]
    pub const fn descending(mut self) -> Self {
        self.descending = !self.descending;
        self
    }
}

/// What resources are sorted by; resources sorting alike are in the order of their ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// The kind's own order: each resource's place among its parent's, as people arranged them.
    #[default]
    Position,
    Field(String),
}

/// Where a page ended: the next page starts after it. It names a place in the order, not a
/// resource, so a page still starts in the right place after the resource it ended at changed or
/// left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub(crate) key: Value,
    pub(crate) place: Value,
    pub(crate) id: String,
}

/// One page of a [`Query`].
#[derive(Clone, Debug, Default)]
pub struct Page {
    /// Each resource shown, by its id.
    pub resources: Vec<(String, Indexed)>,
    /// Where the next page starts; `None` when this one is the last.
    pub next: Option<Cursor>,
}

impl Page {
    /// The ids shown, in order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.resources.iter().map(|(id, _)| id.as_str())
    }
}
