//! A query of one kind: which resources, in what order, from where on, how many.

use std::ops::Bound;

use super::Indexed;
use super::{Cell, Value};

/// A page of a kind's resources, as the store's index reads it ([`super`]).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Query {
    /// The kind, as its type names it.
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The scalar text field matches the pattern, where `*` stands for any letters.
    Like(String, String),
    /// The resource holds the field: a scalar other than [`Value::Null`], or a set.
    Present(String),
    /// The filter does not hold.
    Not(Box<Filter>),
    /// At least one of the filters holds; none for an empty list.
    Any(Vec<Filter>),
}

impl Filter {
    /// The fields the filter reads.
    #[must_use]
    pub fn fields(&self) -> Vec<&str> {
        match self {
            Self::Is(field, _)
            | Self::Has(field, _)
            | Self::Within { field, .. }
            | Self::Contains(field, _)
            | Self::Like(field, _)
            | Self::Present(field) => vec![field.as_str()],
            Self::Not(filter) => filter.fields(),
            Self::Any(filters) => filters.iter().flat_map(Self::fields).collect(),
        }
    }

    /// Whether the resource as the index holds it passes the filter: what a reader that already
    /// has a resource checks against a query without asking the store again.
    #[must_use]
    pub fn holds(&self, stored: &Indexed) -> bool {
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
            Self::Like(field, pattern) => stored
                .scalar(field)
                .as_text()
                .is_some_and(|held| like(held, pattern)),
            Self::Present(field) => match stored.fields.get(field) {
                Some(Cell::One(value)) => *value != Value::Null,
                Some(Cell::Set(_)) => true,
                None => field == super::PARENT && stored.parent.is_some(),
            },
            Self::Not(filter) => !filter.holds(stored),
            Self::Any(filters) => filters.iter().any(|filter| filter.holds(stored)),
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
pub fn past(value: &Value, to: &Bound<Value>) -> bool {
    match to {
        Bound::Included(to) => value > to,
        Bound::Excluded(to) => value >= to,
        Bound::Unbounded => false,
    }
}

/// Whether `held` matches `pattern`, where each `*` stands for any letters, none too.
fn like(held: &str, pattern: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return true;
    };
    let Some(mut rest) = held.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => {
                rest = rest
                    .get(at.saturating_add(part.len())..)
                    .unwrap_or_default();
            }
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Order {
    pub by: Sort,
    pub descending: bool,
    /// Where resources that hold [`Value::Null`] in a field sorted by go: `None` sorts it as the
    /// smallest value, so a descending order lists them last.
    pub absent: Option<Absent>,
    /// The fields resources sorting alike in `by` are sorted by next, in turn, each smallest
    /// first unless it says descending.
    pub then: Vec<(String, bool)>,
}

/// Where resources without a value in a field sorted by go, whichever way the order goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Absent {
    First,
    Last,
}

impl Order {
    /// By the scalar field `field`, smallest first. A resource without the field is not listed.
    #[must_use]
    pub fn by(field: impl Into<String>) -> Self {
        Self {
            by: Sort::Field(field.into()),
            ..Self::default()
        }
    }

    /// This order, then by `field` among resources it sorts alike.
    #[must_use]
    pub fn then(mut self, field: impl Into<String>, descending: bool) -> Self {
        self.then.push((field.into(), descending));
        self
    }

    /// This order, with resources holding nothing in a field it sorts by placed `absent`.
    #[must_use]
    pub const fn absent(mut self, absent: Absent) -> Self {
        self.absent = Some(absent);
        self
    }

    /// Whether only a sort of each stretch of resources alike in `by` answers this order, not one
    /// of the index's orders as it stands.
    pub fn is_composite(&self) -> bool {
        self.absent.is_some() || !self.then.is_empty()
    }

    /// How `a` and `b`, two values of a field this order sorts by `descending` or not, compare.
    pub fn compare(&self, a: &Value, b: &Value, descending: bool) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let placed = |first: bool| match (self.absent, first) {
            (Some(Absent::First), true) | (Some(Absent::Last), false) => Ordering::Less,
            _ => Ordering::Greater,
        };
        match (a, b, self.absent) {
            (Value::Null, Value::Null, Some(_)) => Ordering::Equal,
            (Value::Null, _, Some(_)) => placed(true),
            (_, Value::Null, Some(_)) => placed(false),
            _ if descending => b.cmp(a),
            _ => a.cmp(b),
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Sort {
    /// The kind's own order: each resource's place among its parent's, as people arranged them.
    #[default]
    Position,
    Field(String),
}

/// Where a page ended: the next page starts after it. It names a place in the order, not a
/// resource, so a page still starts in the right place after the resource it ended at changed or
/// left.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    pub key: Value,
    pub place: Value,
    pub id: String,
    /// What the resource holds of each field of the order's `then`.
    pub rest: Vec<Value>,
}

/// One page of a [`Query`].
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
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
