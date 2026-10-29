//! The process-wide registry of record schemas.
//!
//! A tilolang resource stores its fields by **field id** and names its type by id in `$type`.
//! Nothing in the document says how a field merges, so every reader, and every peer replaying
//! an edit, resolves the type id here. Registration happens once per package version, so the
//! schemas are leaked and handed out as `&'static`.

use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

use tilo_action_policy::EarlierIds;

/// The key holding a resource's type id.
pub const TYPE_KEY: &str = "$type";
/// The key holding a resource's app id.
pub const APP_KEY: &str = "$app";

/// How a field merges and therefore which concrete model stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeKind {
    /// A scalar or structured value, last writer wins.
    Register,
    /// Increments add up.
    Counter,
    /// Splices merge.
    Text,
    /// Rich text (`RichText`): tilo-doc's rich text under the field, edited through
    /// `RichTextAction`s, so splices and marks merge.
    RichText,
    /// Per element.
    Set,
    /// Per key.
    Map,
    /// Stable item ids and order keys; the items are records.
    Collection,
    /// A nested record, stored as a map.
    Record,
}

impl MergeKind {
    /// Whether the field holds text search finds a resource by: a text or a rich text.
    #[must_use]
    pub const fn holds_text(self) -> bool {
        matches!(self, Self::Text | Self::RichText)
    }

    /// The kind a compiler metadata `merge` word and field type name stand for.
    ///
    /// `merge` is the declared merge (`register`, `counter`, `text`) or `None`; `ty` is the type
    /// expression. Containers merge structurally, so the type decides for them.
    #[must_use]
    pub fn of(merge: Option<&str>, ty: &str) -> Self {
        if matches!(ty.trim(), "RichText" | "std.RichText") {
            return Self::RichText;
        }
        match merge {
            Some("counter") => return Self::Counter,
            Some("text") => return Self::Text,
            Some("register") => return Self::Register,
            _ => {}
        }
        let ty = ty.trim();
        if ty.starts_with("Collection<") {
            Self::Collection
        } else if ty.starts_with("Set<") {
            Self::Set
        } else if ty.starts_with("Map<") {
            Self::Map
        } else if ty == "Text" {
            Self::Text
        } else {
            Self::Register
        }
    }
}

/// One field of a record type.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldSchema {
    /// The field id: the key it is stored under.
    pub id: String,
    pub kind: MergeKind,
    /// The type id of the items of a [`MergeKind::Collection`], or of a [`MergeKind::Record`].
    pub record: Option<String>,
    /// What an unwritten register reads as; `null` when none.
    pub default: serde_json::Value,
    /// A register holding a structured runtime value (an option, a list, a reference) rather
    /// than an integer, float, flag or text: the runtime reads such a value as JSON.
    pub composite: bool,
}

impl FieldSchema {
    #[must_use]
    pub fn new(id: impl Into<String>, kind: MergeKind) -> Self {
        Self {
            id: id.into(),
            kind,
            record: None,
            default: serde_json::Value::Null,
            composite: false,
        }
    }

    /// The field holds records of the type `type_id`.
    #[must_use]
    pub fn of_record(mut self, type_id: impl Into<String>) -> Self {
        self.record = Some(type_id.into());
        self
    }

    /// The register holds a structured value.
    #[must_use]
    pub const fn composite(mut self) -> Self {
        self.composite = true;
        self
    }

    #[must_use]
    pub fn with_default(mut self, default: serde_json::Value) -> Self {
        self.default = default;
        self
    }
}

/// What a filter column holds (search.md, "Who indexes"): how its values are written so that
/// their order is the values' order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    Bool,
    Int,
    Float,
    /// Text, by its letters without regard to case, then as written.
    Text,
    Date,
    Time,
    /// A case of an enum, by its id.
    Enum,
    /// A resource, by its id.
    Ref,
    /// An identifier, `scheme:value`, without regard to case.
    Ident,
}

/// A filter column of a type (search.md, "Who indexes"): the index keeps, per collection of the
/// type's resources and (for the type's filters) across the workspace, a list of them ordered
/// by the column's value, each value written in [`ColumnKind`]'s order.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSchema {
    /// What names it in the index: the field's id, or `=key` for a named filter.
    pub name: String,
    /// The field it holds, by id; none for a named filter.
    pub field: Option<String>,
    pub kind: ColumnKind,
    /// Whether the field holds several values (a set): one entry per member.
    pub many: bool,
    /// A named filter's predicate over the resource's own fields: the column holds whether it
    /// holds.
    pub pred: Option<tilolang::pred::Pred<String>>,
    /// Whether it is one of the type's filters, kept across the workspace as well as per
    /// collection.
    pub filter: bool,
    /// The values it can hold, when they are known: an enum's cases by id.
    pub cases: Vec<String>,
}

/// A record type: its id, the app that declared it and its fields.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeSchema {
    pub id: String,
    pub app: String,
    pub name: String,
    pub fields: Vec<FieldSchema>,
    /// Its filter columns: its filters' and those of every other field a `filter` compares.
    pub columns: Vec<ColumnSchema>,
}

impl TypeSchema {
    /// The type's name as a sentence says it: `TodoList` as "Todo list". What a resource of the
    /// type with no title of its own is called.
    #[must_use]
    pub fn spoken_name(&self) -> String {
        let mut out = String::new();
        for (at, ch) in self.name.replace('_', " ").chars().enumerate() {
            if at > 0 && ch.is_uppercase() {
                out.push(' ');
                out.extend(ch.to_lowercase());
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[must_use]
    pub fn new(id: impl Into<String>, app: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            app: app.into(),
            name: name.into(),
            fields: Vec::new(),
            columns: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_field(mut self, field: FieldSchema) -> Self {
        self.fields.push(field);
        self
    }

    /// The field `id`.
    #[must_use]
    pub fn field(&self, id: &str) -> Option<&FieldSchema> {
        self.fields.iter().find(|field| field.id == id)
    }

    /// The column of the field `id`.
    #[must_use]
    pub fn column_of(&self, id: &str) -> Option<&ColumnSchema> {
        self.columns
            .iter()
            .find(|column| column.field.as_deref() == Some(id))
    }

    /// The column `name`.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnSchema> {
        self.columns.iter().find(|column| column.name == name)
    }
}

fn registry() -> &'static RwLock<BTreeMap<String, &'static TypeSchema>> {
    static REGISTRY: RwLock<BTreeMap<String, &'static TypeSchema>> = RwLock::new(BTreeMap::new());
    &REGISTRY
}

/// Make `schema` the one `schema.id` resolves to, and return the registered copy.
///
/// Registering what is already registered returns the held copy and leaks nothing; a changed
/// schema (a new package version) replaces it.
pub fn register(schema: TypeSchema) -> &'static TypeSchema {
    if let Some(held) = lookup(&schema.id)
        && *held == schema
    {
        return held;
    }
    let leaked: &'static TypeSchema = Box::leak(Box::new(schema));
    registry()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(leaked.id.clone(), leaked);
    leaked
}

/// The schema of the type `id`, when its package registered it.
#[must_use]
pub fn lookup(id: &str) -> Option<&'static TypeSchema> {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(id)
        .copied()
}

/// The registered type `name` of the app `app`.
#[must_use]
pub fn lookup_in(app: &str, name: &str) -> Option<&'static TypeSchema> {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .copied()
        .find(|schema| schema.app == app && schema.name == name)
}

/// Record that `app` was published under the ids `before` earlier: the records the room holds
/// under them are `app`'s, where they are and under the ids they have, and grants written under
/// them are its (the process's one table, `tilo_action_policy::earlier`). That the same publisher
/// signed both is checked where the package is installed; a package loaded here is trusted.
pub fn register_previously(app: &str, before: &[String]) {
    tilo_action_policy::earlier::register(app, before.iter().cloned());
}

/// The ids `app` was published under before.
#[must_use]
pub fn previously(app: &str) -> Vec<String> {
    tilo_action_policy::earlier::held_of(app)
}

/// The app the records stored under `stored` belong to: the app that was published as `stored`
/// before, else `stored` itself.
#[must_use]
pub fn successor(stored: &str) -> String {
    tilo_action_policy::earlier::held_current(stored)
}

/// The ids every app loaded or installed here was published under before, as grants are decided
/// with them (`Context::with_earlier_ids`).
#[must_use]
pub fn earlier_ids() -> EarlierIds {
    tilo_action_policy::earlier::held()
}

/// The keys the records of `app` may be stored under: its own id, then each it had before.
#[must_use]
pub fn stored_under(app: &str) -> Vec<String> {
    let mut out = vec![app.to_owned()];
    out.extend(previously(app));
    out
}
