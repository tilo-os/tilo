//! The stored layout of a model, as data.
//!
//! [`Model::SHAPE`](crate::Model::SHAPE) describes how a value lives in the
//! document without reading it: which keys a record has, which of them are
//! keyed collections, what is written whole. Generic services use it to act on
//! any record correctly — History restores a past version field by field and
//! item by item, the way an action would have written it, instead of replacing
//! the document wholesale.
//!
//! `#[derive(Model)]` produces the shape of a struct from its fields; plain
//! types are [`Shape::Value`]. A type with its own layout may declare a shape
//! by hand, or keep the default and opt out of layout-aware services.

/// How one value is laid out in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Written and restored whole: a scalar, a text, a list, a JSON value.
    Value,
    /// A map of named fields, each with its own shape.
    Record(&'static [FieldShape]),
    /// A keyed, ordered [`Collection`](crate::Collection) of items of one
    /// shape. Each item is stored under its id as an entry map holding its
    /// order key ([`ENTRY_ORDER_KEY`]) and its value ([`ENTRY_VALUE_KEY`]).
    Collection(&'static Shape),
}

/// One named field of a [`Shape::Record`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldShape {
    /// The key the field is stored under (after `#[model(rename)]`).
    pub key: &'static str,
    pub shape: Shape,
}

/// The key of a collection entry's order key.
pub const ENTRY_ORDER_KEY: &str = crate::collection::ORDER;
/// The key of a collection entry's value.
pub const ENTRY_VALUE_KEY: &str = crate::collection::VALUE;
