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
    /// The field's Rust name.
    pub name: &'static str,
    /// What the field is, for people: its doc comment's first line, else its
    /// name in words. History falls back to it ("Changed *Folder*").
    pub title: &'static str,
    pub shape: Shape,
    /// `#[model(unit)]`: the field is its own storage unit (for a
    /// collection, each item is one), so a relay can tell who may write it
    /// without reading content.
    pub unit: bool,
    /// `#[model(readonly)]`: published for reading, changed only by verbs.
    pub readonly: bool,
    /// `#[model(label)]`: the field names the record.
    pub label: bool,
}

impl Shape {
    /// The fields of a record shape; none for any other.
    #[must_use]
    pub const fn fields(&self) -> &'static [FieldShape] {
        match self {
            Self::Record(fields) => fields,
            Self::Value | Self::Collection(_) => &[],
        }
    }

    /// The storage unit a change at `path` (from this shape's root) writes:
    /// the longest prefix of the path that ends at a `#[model(unit)]` field
    /// or an item of one. The record itself, the empty path, is a unit too.
    #[must_use]
    pub fn unit_of(&self, path: &[crate::path::Segment]) -> Vec<crate::path::Segment> {
        use crate::path::Segment;
        let mut unit = Vec::new();
        let mut shape = *self;
        let mut walked = Vec::new();
        let mut items_are_units = false;
        for segment in path {
            walked.push(segment.clone());
            match (shape, segment) {
                (Self::Record(fields), Segment::Field(key)) => {
                    let Some(field) = fields.iter().find(|field| field.key == key) else {
                        break;
                    };
                    shape = field.shape;
                    items_are_units = field.unit && matches!(field.shape, Self::Collection(_));
                    if field.unit && !items_are_units {
                        unit.clone_from(&walked);
                    }
                }
                (Self::Collection(item), Segment::Item(_)) => {
                    shape = *item;
                    if items_are_units {
                        unit.clone_from(&walked);
                    }
                    items_are_units = false;
                }
                _ => break,
            }
        }
        unit
    }

    /// Whether the field at `path` is `#[model(readonly)]`.
    #[must_use]
    pub fn readonly_at(&self, path: &[crate::path::Segment]) -> bool {
        use crate::path::Segment;
        let mut shape = *self;
        for segment in path {
            match (shape, segment) {
                (Self::Record(fields), Segment::Field(key)) => {
                    let Some(field) = fields.iter().find(|field| field.key == key) else {
                        return false;
                    };
                    if field.readonly {
                        return true;
                    }
                    shape = field.shape;
                }
                (Self::Collection(item), Segment::Item(_)) => shape = *item,
                _ => return false,
            }
        }
        false
    }
}

/// The key of a collection entry's order key.
pub const ENTRY_ORDER_KEY: &str = crate::collection::ORDER;
/// The key of a collection entry's value.
pub const ENTRY_VALUE_KEY: &str = crate::collection::VALUE;
