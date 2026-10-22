//! What an app's resource types are made of, as its views and its search
//! read them: each type's fields, the role a field plays in a presentation,
//! and whether the field is found by typed text or narrows by a chip.

use std::any::Any;
use std::sync::Arc;

/// The part a field plays where an item of its type is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldRole {
    /// The name a person looks for: a song's title, an album's name.
    Title,
    /// The line under it that tells two titles apart: the artist.
    Subtitle,
    /// The picture: a cover, a portrait.
    Artwork,
    /// When it is from: a release year, a date.
    Date,
    /// Anything else a presentation shows: an album on a song's row.
    Detail,
}

/// How a type's items are presented in a list of results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// One row each: songs, messages, settings.
    Row,
    /// A tile each, in a grid: albums, artists, books.
    Tile,
    /// The item's page of its own.
    Detail,
}

/// How a filterable field narrows results, which decides the chip it gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterKind {
    /// A chip naming one of the field's values: Artist: Ada.
    Value,
    /// A chip that is on or off: Liked, In library.
    Toggle,
}

/// One field of a resource type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDecl {
    /// The field's key in an [`Item`]'s fields, and the key a typed token
    /// names (`artist:ada`).
    pub key: &'static str,
    /// What the field is called, for its chip.
    pub label: String,
    pub role: Option<FieldRole>,
    /// Typed text finds items by this field.
    pub searchable: bool,
    /// The field narrows results by a chip.
    pub filter: Option<FilterKind>,
}

impl FieldDecl {
    /// A field called `label`, neither searchable nor a filter.
    #[must_use]
    pub fn new(key: &'static str, label: impl Into<String>) -> Self {
        Self {
            key,
            label: label.into(),
            role: None,
            searchable: false,
            filter: None,
        }
    }

    /// The type's title: searchable, in the title role.
    #[must_use]
    pub fn title(key: &'static str, label: impl Into<String>) -> Self {
        Self::new(key, label).role(FieldRole::Title).searchable()
    }

    #[must_use]
    pub fn role(mut self, role: FieldRole) -> Self {
        self.role = Some(role);
        self
    }

    #[must_use]
    pub fn searchable(mut self) -> Self {
        self.searchable = true;
        self
    }

    /// The field narrows results by a chip of `kind`.
    #[must_use]
    pub fn filter(mut self, kind: FilterKind) -> Self {
        self.filter = Some(kind);
        self
    }
}

/// One resource type of an app: what its items are called, how they are
/// presented in results, and their fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindDecl {
    /// The resource kind (`music.song`).
    pub kind: &'static str,
    /// One item ("Song").
    pub label: String,
    /// Several, which names its group of results ("Songs").
    pub plural: String,
    pub form: Form,
    pub fields: Vec<FieldDecl>,
}

impl KindDecl {
    #[must_use]
    pub fn new(
        kind: &'static str,
        label: impl Into<String>,
        plural: impl Into<String>,
        form: Form,
    ) -> Self {
        Self {
            kind,
            label: label.into(),
            plural: plural.into(),
            form,
            fields: Vec::new(),
        }
    }

    #[must_use]
    pub fn field(mut self, field: FieldDecl) -> Self {
        self.fields.push(field);
        self
    }

    /// The field `key`.
    #[must_use]
    pub fn field_of(&self, key: &str) -> Option<&FieldDecl> {
        self.fields.iter().find(|field| field.key == key)
    }
}

/// The filterable fields of `kinds`, each key once, in declaration order: the
/// chips a query view offers.
#[must_use]
pub fn filters(kinds: &[KindDecl]) -> Vec<&FieldDecl> {
    let mut found: Vec<&FieldDecl> = Vec::new();
    for field in kinds
        .iter()
        .flat_map(|kind| &kind.fields)
        .filter(|field| field.filter.is_some())
    {
        if !found.iter().any(|seen| seen.key == field.key) {
            found.push(field);
        }
    }
    found
}

/// One item of a resource type, as search and views read it: its type, its
/// id, and its fields by key. A field may hold several values (a song by two
/// artists). A remote source attaches what it fetched (`payload`), so the app
/// can make the item playable or openable before anything is stored.
#[derive(Clone)]
pub struct Item {
    pub kind: &'static str,
    pub id: String,
    pub fields: Vec<(&'static str, String)>,
    pub payload: Option<Arc<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Item")
            .field("kind", &self.kind)
            .field("id", &self.id)
            .field("fields", &self.fields)
            .field("payload", &self.payload.is_some())
            .finish()
    }
}

impl PartialEq for Item {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.id == other.id && self.fields == other.fields
    }
}

impl Item {
    #[must_use]
    pub fn new(kind: &'static str, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            fields: Vec::new(),
            payload: None,
        }
    }

    /// Add `value` to the field `key`; an empty value adds nothing.
    #[must_use]
    pub fn with(mut self, key: &'static str, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.is_empty() {
            self.fields.push((key, value));
        }
        self
    }

    /// Set the toggle field `key` on, when `on`.
    #[must_use]
    pub fn flag(self, key: &'static str, on: bool) -> Self {
        if on { self.with(key, TOGGLE_ON) } else { self }
    }

    /// Attach what a remote source fetched.
    #[must_use]
    pub fn payload(mut self, payload: Arc<dyn Any + Send + Sync>) -> Self {
        self.payload = Some(payload);
        self
    }

    /// The first value of the field `key`.
    #[must_use]
    pub fn value<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        self.values(key).next()
    }

    /// Every value of the field `key`.
    pub fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.fields
            .iter()
            .filter(move |(field, _)| *field == key)
            .map(|(_, value)| value.as_str())
    }

    /// Whether the toggle field `key` is on.
    #[must_use]
    pub fn is_on(&self, key: &str) -> bool {
        self.values(key).any(|value| value == TOGGLE_ON)
    }

    /// What identifies the item across sources: its type and id.
    #[must_use]
    pub fn key(&self) -> (&'static str, &str) {
        (self.kind, self.id.as_str())
    }

    /// The payload a remote source attached, as `T`.
    #[must_use]
    pub fn payload_as<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.payload.as_deref()?.downcast_ref()
    }
}

/// The value a toggle field holds while it is on.
pub(crate) const TOGGLE_ON: &str = "true";
