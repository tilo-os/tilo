//! Derived keys (`derive finished: Bool = done and children.all(finished)`): a value a type's
//! resources have by what the collection holds, which the store works out from the record's own
//! facts instead of any record keeping it.
//!
//! A resource stores what people wrote: a task's title, the day it is due, whether it is done,
//! the task it stands under. Everything else — that a branch is done through, how many of a
//! task's subtasks are done — follows from those facts and the other items, and a copy of it in
//! the record would be a second truth that two people's changes could leave disagreeing. The
//! store derives it from the collection and keeps the answer where it reads fields from (the
//! index, `tilolang_tilo::index`), so a view reads a derived key as it reads a field, at the
//! same cost, and a change updates the few items whose answer it changes: the item and the ones
//! it stands under, and the ones under a key that reads them.
//!
//! A derived key reads the item's own fields and, through the type's parent field
//! (`parent: Option<Task> (parent)`), the items standing directly under it (`children`) and the
//! items it stands under (`parent`). A key may read itself through those items
//! (`children.all(finished)`, `parent.all(folded)`), which is how "it and everything under it is
//! done" and "it and everything above it is done" are written; it reads the keys the type
//! declares before it for anything else. It never stores anything, so a view may sort, filter
//! and group by it, and nothing may write it.

use serde::{Deserialize, Serialize};

/// How two counts compare, in a derived key that is a flag of them (`children.count > 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cmp {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `==`
    Eq,
    /// `!=`
    Ne,
}

impl Cmp {
    /// Whether `left` compares to `right` as the operator says.
    #[must_use]
    pub fn holds(self, left: i64, right: i64) -> bool {
        match self {
            Self::Lt => left < right,
            Self::Le => left <= right,
            Self::Gt => left > right,
            Self::Ge => left >= right,
            Self::Eq => left == right,
            Self::Ne => left != right,
        }
    }
}

/// What a derived key computes, over the item's own fields and the items standing under and over
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Derived<F> {
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Int(i64),
    /// One of the item's own fields.
    Field(F),
    /// `not x`.
    Not(Box<Self>),
    /// `x and y`: every one holds.
    And(Vec<Self>),
    /// `x or y`: one of them holds.
    Or(Vec<Self>),
    /// `x > y`: two counts compare.
    Compare {
        op: Cmp,
        left: Box<Self>,
        right: Box<Self>,
    },
    /// `children.count`: how many items stand under the item.
    Children,
    /// `children.count(x)`: how many of the items under the item hold `x`.
    Count { holds: Box<Self> },
    /// `children.all(x)`: whether every item under the item holds `x`, which an item with none
    /// under it does.
    All { holds: Box<Self> },
    /// `children.any(x)`: whether one of the items under the item holds `x`.
    Any { holds: Box<Self> },
    /// `parent.all(x)`: whether every item above the item holds `x`, which an item with none
    /// above it does.
    Above { holds: Box<Self> },
}

impl<F> Derived<F> {
    /// Every field it reads, on the item and (inside a relation) on the items under and above it.
    #[must_use]
    pub fn fields(&self) -> Vec<&F> {
        let mut out = Vec::new();
        self.each(&mut |f| out.push(f));
        out
    }

    fn each<'a>(&'a self, f: &mut impl FnMut(&'a F)) {
        match self {
            Self::Bool(_) | Self::Int(_) | Self::Children => {}
            Self::Field(field) => f(field),
            Self::Not(inner) | Self::Count { holds: inner } | Self::All { holds: inner } => {
                inner.each(f);
            }
            Self::Any { holds: inner } | Self::Above { holds: inner } => inner.each(f),
            Self::And(parts) | Self::Or(parts) => parts.iter().for_each(|p| p.each(f)),
            Self::Compare { left, right, .. } => {
                left.each(f);
                right.each(f);
            }
        }
    }

    /// Whether it reads the items standing under the item.
    #[must_use]
    pub fn reads_children(&self) -> bool {
        match self {
            Self::Bool(_) | Self::Int(_) | Self::Field(_) | Self::Above { .. } => false,
            Self::Not(inner) => inner.reads_children(),
            Self::And(parts) | Self::Or(parts) => parts.iter().any(Self::reads_children),
            Self::Compare { left, right, .. } => left.reads_children() || right.reads_children(),
            Self::Children | Self::Count { .. } | Self::All { .. } | Self::Any { .. } => true,
        }
    }

    /// Whether it reads the items standing above the item.
    #[must_use]
    pub fn reads_above(&self) -> bool {
        match self {
            Self::Bool(_) | Self::Int(_) | Self::Field(_) | Self::Children => false,
            Self::Not(inner) => inner.reads_above(),
            Self::And(parts) | Self::Or(parts) => parts.iter().any(Self::reads_above),
            Self::Compare { left, right, .. } => left.reads_above() || right.reads_above(),
            Self::Count { holds } | Self::All { holds } | Self::Any { holds } => {
                holds.reads_above()
            }
            Self::Above { .. } => true,
        }
    }
}
