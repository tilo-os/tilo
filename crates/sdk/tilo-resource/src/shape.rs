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
    /// Items of one shape stored under the ids they carry
    /// ([`Keyed`](crate::Keyed)): each item's value directly under its id.
    Keyed(&'static Shape),
    /// A map (`BTreeMap<String, T>`) of values of one shape, each under its
    /// key: an edit inside an entry is about that entry, named by its key or
    /// its value's label.
    Map(&'static Shape),
}

/// Who may change a field without being asked, by what a change of it can
/// do to the person: every door decides it from the published field edit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trust {
    /// Anyone with write access changes it.
    #[default]
    Ordinary,
    /// A change can expose the person (what others see of them, where their
    /// data goes): it asks for confirmation first, from whoever makes it.
    Sensitive,
    /// A change one way can weaken the person's security (a longer lock
    /// delay, notifications shown on the lock screen): a change that way is
    /// [`Self::Weakening`], any other is ordinary. Which way the value is
    /// now decides it, so without it every change counts as weakening.
    Weakens(tilo_action_policy::edits::Weaker<'static>),
    /// Any change can weaken the person's security (who may control this
    /// machine): it asks for confirmation and crosses a security boundary,
    /// so a caller acting for the person (an agent, a script) needs their
    /// own grant for it.
    Weakening,
}

impl Trust {
    /// The risk of an edit of a field of this trust.
    #[must_use]
    pub const fn risk(self) -> tilo_actions::RiskLevel {
        match self {
            Self::Ordinary => tilo_actions::RiskLevel::Low,
            Self::Sensitive => tilo_actions::RiskLevel::Destructive,
            Self::Weakens(_) | Self::Weakening => tilo_actions::RiskLevel::Privileged,
        }
    }
}

/// One named field of a [`Shape::Record`].
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent field attribute"
)]
#[derive(Clone, Copy, Debug)]
pub struct FieldShape {
    /// The key the field is stored under (after `#[model(rename)]`).
    pub key: &'static str,
    /// The record type the field belongs to, in kebab case (`snippet`): with
    /// the key it names the field in history messages
    /// (`<app>-<owner>-<key>`).
    pub owner: &'static str,
    /// The field's Rust name.
    pub name: &'static str,
    /// The field's Rust type, as written.
    pub ty: &'static str,
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
    /// Where the field stands among those whose texts, joined by spaces,
    /// name the record before its `label` field does
    /// (`#[model(label = "first last")]` on the type): 1 for the first, 0
    /// when it is not one of them.
    pub label_part: u8,
    /// `#[model(remove = "quiet")]` on a collection: removing an item by
    /// field edit asks no confirmation. Otherwise it is destructive, as a
    /// delete verb is, so the generic edit is never a quieter way around it.
    pub quiet_remove: bool,
    /// `#[model(trust = "..")]`: who may change the field without being
    /// asked first.
    pub trust: Trust,
    /// The JSON Schema of the field's value ([`Model::value_schema`](crate::Model::value_schema)).
    pub schema: fn() -> serde_json::Value,
    /// The first line of the field's text as stored ([`stored_text`]): what
    /// a label made of it reads, rich text included.
    pub text: StoredText,
    /// What the field reads as while its key is absent, for a scalar field
    /// ([`absent_scalar`]): history names it as the value an edit replaced.
    pub absent: fn() -> Option<serde_json::Value>,
    /// `#[model(resource)]`: the field holds a resource of its own type,
    /// published beneath the resource of the record holding it
    /// ([`NestedResource`](crate::child::NestedResource)).
    pub resource: Option<crate::child::NestedResource>,
}

/// `value`, the JSON form of a value of `shape`, without the record fields
/// that hold what their absent key reads as: what an edit that writes a new
/// value carries of it, which reads back as the whole value.
///
/// Only a record has such fields, and `T::SHAPE` is known where this is
/// instantiated, so for every other model this compiles to `value`.
#[must_use]
#[inline]
pub(crate) fn without_absent<T: crate::Model>(value: serde_json::Value) -> serde_json::Value {
    if matches!(T::SHAPE, Shape::Record(_)) {
        (T::TABLE.ops.without_absent)(T::TABLE, value)
    } else {
        value
    }
}

/// [`without_absent`] of a record, reached through its table's functions
/// rather than `T::SHAPE`, which would copy its field list into every caller.
pub(crate) fn record_without_absent(
    table: &crate::table::ModelTable,
    value: serde_json::Value,
) -> serde_json::Value {
    match table.record().map(|record| record.shape) {
        Some(Shape::Record(fields)) => without_absent_fields(fields, value),
        _ => value,
    }
}

/// [`without_absent`] of a value that is no record: all of it.
pub(crate) fn kept_whole(
    _: &crate::table::ModelTable,
    value: serde_json::Value,
) -> serde_json::Value {
    value
}

#[inline(never)]
fn without_absent_fields(
    fields: &'static [FieldShape],
    mut value: serde_json::Value,
) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut value {
        for field in fields {
            if let Some(absent) = (field.absent)()
                && map.get(field.key) == Some(&absent)
            {
                map.remove(field.key);
            }
        }
    }
    value
}

/// The value an absent key of a scalar `T` reads as, in JSON; `None` for a
/// type stored as an object or without an empty value.
#[must_use]
pub fn absent_scalar<T: crate::Model>() -> Option<serde_json::Value> {
    if T::SCALAR {
        T::missing().ok().map(|value| value.to_json())
    } else {
        None
    }
}

/// Reads the first line of a field's text from where it is stored.
pub type StoredText =
    fn(&crate::model::Doc, &automerge::Value<'_>, &automerge::ObjId) -> Option<String>;

/// The first non-empty line of the text a `T` stored as `value` at `id`
/// carries, as [`Model::label`](crate::Model::label) reads it from the value.
pub fn stored_text<T: crate::Model>(
    doc: &crate::model::Doc,
    value: &automerge::Value<'_>,
    id: &automerge::ObjId,
) -> Option<String> {
    // A value that loads on demand is read whole first.
    let mut stored = T::load(doc, value, id).ok()?;
    stored.read_whole(doc).ok()?;
    crate::__private::first_line(&stored)
}

/// Two fields are the same field when everything but their value schema's
/// function (which compares by address) is.
impl PartialEq for FieldShape {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.owner == other.owner
            && self.name == other.name
            && self.ty == other.ty
            && self.title == other.title
            && self.shape == other.shape
            && self.unit == other.unit
            && self.readonly == other.readonly
            && self.label == other.label
            && self.quiet_remove == other.quiet_remove
            && self.trust == other.trust
            && self.resource.map(|nested| nested.kind) == other.resource.map(|nested| nested.kind)
    }
}

impl Eq for FieldShape {}

impl Shape {
    /// The fields of a record shape; none for any other.
    #[must_use]
    pub const fn fields(&self) -> &'static [FieldShape] {
        match self {
            Self::Record(fields) => fields,
            Self::Value | Self::Collection(_) | Self::Keyed(_) | Self::Map(_) => &[],
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
                    items_are_units =
                        field.unit && matches!(field.shape, Self::Collection(_) | Self::Keyed(_));
                    if field.unit && !items_are_units {
                        unit.clone_from(&walked);
                    }
                }
                (Self::Collection(item) | Self::Keyed(item), Segment::Item(_)) => {
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

    /// The collections below this shape whose removals ask nothing
    /// (`#[model(remove = "quiet")]`), by their dotted paths (`*` for an
    /// item), as [`tilo_action_policy::edits::dotted`] spells them.
    #[must_use]
    pub fn quiet_removals(&self) -> Vec<String> {
        fn walk(shape: Shape, prefix: &str, out: &mut Vec<String>, depth: usize) {
            if depth > 16 {
                return;
            }
            match shape {
                Shape::Record(fields) => {
                    for field in fields {
                        let path = if prefix.is_empty() {
                            field.key.to_owned()
                        } else {
                            format!("{prefix}.{}", field.key)
                        };
                        if field.quiet_remove {
                            out.push(path.clone());
                        }
                        walk(field.shape, &path, out, depth.saturating_add(1));
                    }
                }
                Shape::Collection(item) | Shape::Keyed(item) | Shape::Map(item) => {
                    walk(*item, &format!("{prefix}.*"), out, depth.saturating_add(1));
                }
                Shape::Value => {}
            }
        }
        let mut out = Vec::new();
        walk(*self, "", &mut out, 0);
        out
    }

    /// The fields below this shape that are not [`Trust::Ordinary`], by
    /// their dotted paths (`*` for an item), as
    /// [`tilo_action_policy::edits::dotted`] spells them.
    #[must_use]
    pub fn guarded_fields(&self) -> Vec<(String, Trust)> {
        fn walk(shape: Shape, prefix: &str, out: &mut Vec<(String, Trust)>, depth: usize) {
            if depth > 16 {
                return;
            }
            match shape {
                Shape::Record(fields) => {
                    for field in fields {
                        let path = if prefix.is_empty() {
                            field.key.to_owned()
                        } else {
                            format!("{prefix}.{}", field.key)
                        };
                        if field.trust != Trust::Ordinary {
                            out.push((path.clone(), field.trust));
                        }
                        walk(field.shape, &path, out, depth.saturating_add(1));
                    }
                }
                Shape::Collection(item) | Shape::Keyed(item) | Shape::Map(item) => {
                    walk(*item, &format!("{prefix}.*"), out, depth.saturating_add(1));
                }
                Shape::Value => {}
            }
        }
        let mut out = Vec::new();
        walk(*self, "", &mut out, 0);
        out
    }

    /// The trust of the field at `path`: the strictest of the fields it is
    /// at or below.
    #[must_use]
    pub fn trust_at(&self, path: &[crate::path::Segment]) -> Trust {
        use crate::path::Segment;
        let mut shape = *self;
        let mut trust = Trust::Ordinary;
        for segment in path {
            match (shape, segment) {
                (Self::Record(fields), Segment::Field(key)) => {
                    let Some(field) = fields.iter().find(|field| field.key == key) else {
                        break;
                    };
                    trust = trust.max(field.trust);
                    shape = field.shape;
                }
                (
                    Self::Collection(item) | Self::Keyed(item) | Self::Map(item),
                    Segment::Item(_) | Segment::Key(_),
                ) => shape = *item,
                _ => break,
            }
        }
        trust
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
                (Self::Collection(item) | Self::Keyed(item), Segment::Item(_)) => shape = *item,
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
