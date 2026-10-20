//! The process-wide registry of record schemas.
//!
//! A tilolang resource stores its fields by **field id** and names its type by id in `$type`.
//! Nothing in the document says how a field merges, so every reader, and every peer replaying
//! an edit, resolves the type id here. Registration happens once per package version, so the
//! schemas are leaked and handed out as `&'static`.

use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

/// The key holding a resource's type id.
pub const TYPE_KEY: &str = "$type";
/// The key holding a resource's app id.
pub const APP_KEY: &str = "$app";

/// How a field merges (spec 2.2) and therefore which concrete model stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeKind {
    /// A scalar or structured value, last writer wins.
    Register,
    /// Increments add up.
    Counter,
    /// Splices merge.
    Text,
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
    /// The kind a compiler metadata `merge` word and field type name stand for.
    ///
    /// `merge` is the declared merge (`register`, `counter`, `text`) or `None`; `ty` is the type
    /// expression. Containers merge structurally, so the type decides for them.
    #[must_use]
    pub fn of(merge: Option<&str>, ty: &str) -> Self {
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

/// A record type: its id, the app that declared it and its fields.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeSchema {
    pub id: String,
    pub app: String,
    pub name: String,
    pub fields: Vec<FieldSchema>,
}

impl TypeSchema {
    #[must_use]
    pub fn new(id: impl Into<String>, app: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            app: app.into(),
            name: name.into(),
            fields: Vec::new(),
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

/// The first registered type named `name` (the last segment of a qualified name), when the type
/// belongs to another package than the one being registered.
#[must_use]
pub fn lookup_named(name: &str) -> Option<&'static TypeSchema> {
    let last = name.rsplit('.').next().unwrap_or(name);
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .copied()
        .find(|schema| schema.name == last)
}
