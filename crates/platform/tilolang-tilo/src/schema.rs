//! A host's registry of record schemas.
//!
//! A tilolang resource stores its fields by **field id** and names its type by id in `$type`.
//! Nothing in the document says how a field merges, so every reader, and every peer replaying
//! an edit, resolves the type id through a [`Schemas`] table.
//!
//! One table belongs to each host and store, not to the process. Two packages are loaded at
//! once all the time: the Chat's conversation types and its channel types both declare a type
//! `Message`, and two versions of one app share every id while their types differ. A
//! process-wide table keyed by type id lets one loaded package overwrite the other's schema, and
//! then a record of the first reads its fields by the second's shape — a trap, or a field read
//! as missing. So a store keeps its own table, and hands it out as `&'static` schemas: one
//! package version is registered once and read for as long as anything may hold its records.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

use tilo_action_policy::EarlierIds;
use tilolang::types::Ty;

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
    /// A register naming a resource, of a record type the field declares: what links a resource
    /// to another (`linked(r)`, [`tilolang_runtime::store::LinkHow::Field`]).
    pub refers: bool,
    /// The type the field declares (`Option<Text>`), for a reader that holds a value the app
    /// wrote in a layout of its own (`crate::native`): what turns it into a value of the type a
    /// program reads. `None` for a field whose declared type does not read.
    pub ty: Option<Ty>,
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
            refers: false,
            ty: None,
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

    /// The type the field declares.
    #[must_use]
    pub fn of_ty(mut self, ty: Ty) -> Self {
        self.ty = Some(ty);
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
    /// A person or agent of the workspace, by the id the host names them with.
    Principal,
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
    /// The outside kind it holds (`type Message (origin: imap.message)`), when it holds one.
    pub origin: Option<String>,
    /// What one of its records is called while its title is empty, as the app wrote it
    /// (`(untitled: "Untitled")`); its type's name when it declares none.
    pub untitled: Option<String>,
}

/// A type's name as a sentence says it: `TodoList` as "Todo list".
#[must_use]
pub fn spoken(name: &str) -> String {
    let mut out = String::new();
    for (at, ch) in name.replace('_', " ").chars().enumerate() {
        if at > 0 && ch.is_uppercase() {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

impl TypeSchema {
    /// The type's name as a sentence says it: `TodoList` as "Todo list". What a resource of the
    /// type with no title of its own is called when its type declares no words for that.
    #[must_use]
    pub fn spoken_name(&self) -> String {
        spoken(&self.name)
    }

    /// What one of its records is called while its title is empty, in the words the app wrote.
    #[must_use]
    pub fn untitled(&self) -> String {
        self.untitled.clone().unwrap_or_else(|| self.spoken_name())
    }

    #[must_use]
    pub fn new(id: impl Into<String>, app: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            app: app.into(),
            name: name.into(),
            fields: Vec::new(),
            columns: Vec::new(),
            origin: None,
            untitled: None,
        }
    }

    /// Whether its origin derives its resources, and so they are never saved to a collaborative
    /// record: the store keeps them in its unsaved layer ([`crate::store`]).
    #[must_use]
    pub fn derived(&self) -> bool {
        self.origin
            .as_deref()
            .is_some_and(tilolang::origin::is_derived)
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

    /// The column `name`.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnSchema> {
        self.columns.iter().find(|column| column.name == name)
    }

    /// The order [`crate::reads::first_text`] looks at the fields: every text of the resource's
    /// own first, then the records and collections it holds, in that order. A resource holding
    /// a text of its own is named by it before any item it holds. The order is the type's
    /// declaration order, never the fields' ids: ids are drawn per compile, so an order over
    /// them would name a type with two texts differently on every build.
    #[must_use]
    pub fn naming_scan(&self) -> Vec<&FieldSchema> {
        let mut scan: Vec<&FieldSchema> =
            self.fields.iter().filter(|f| f.kind.holds_text()).collect();
        scan.extend(
            self.fields
                .iter()
                .filter(|field| matches!(field.kind, MergeKind::Record | MergeKind::Collection)),
        );
        scan
    }

    /// Where the field `id` stands in [`Self::naming_scan`]: a change to a field at or before
    /// this one may move the resource's name, a change to one after it cannot.
    #[must_use]
    pub fn naming_position(&self, id: &str) -> Option<usize> {
        self.naming_scan().iter().position(|field| field.id == id)
    }
}

thread_local! {
    /// The tables the code running on this thread resolves through, innermost last. A store
    /// installs its own for as long as it reads, writes or derives, and an action its package's
    /// while it applies: a record's type id means the schema of the package version that
    /// registered it, and two packages on one thread hold different ones.
    static CURRENT: std::cell::RefCell<Vec<Arc<Schemas>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Every schema any package registered here, by type id, whichever table did: what a caller
/// that holds no table of its own reads — a compiler tool, a layout check, a peer reading a
/// workspace's record without its app's host. Every `register` fills it as well as its own
/// table, so a record read through no host resolves the last version registered for its type;
/// a store, which holds one version, reads through its own and never through this.
///
/// One for the process, not one per thread: a caller with no table of its own (a view opened
/// from a thread the store was not installed on) reads what any package loaded here registered.
fn fallback() -> Arc<Schemas> {
    static FALLBACK: std::sync::OnceLock<Arc<Schemas>> = std::sync::OnceLock::new();
    Arc::clone(FALLBACK.get_or_init(|| Arc::new(Schemas::new())))
}

thread_local! {
    /// The schemas of each package this thread loaded, by the app it is of and the hash of its
    /// module: a record's type id names one package version, never another's. An action may be
    /// applied (`Action::apply`) or described outside the store that holds its records — a
    /// receiver replaying it, a task's door — and resolves through its own package's table.
    static PACKAGES: std::cell::RefCell<BTreeMap<(String, String), Arc<PackageSchemas>>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// One package's schemas: what its types are, by id.
pub struct PackageSchemas {
    schemas: Arc<Schemas>,
}

impl PackageSchemas {
    /// Make `schema` the one `schema.id` resolves to in this package, and return the copy.
    pub fn register(&self, schema: TypeSchema) -> &'static TypeSchema {
        self.schemas.register(schema)
    }

    /// The table of this package, to install for as long as its code runs.
    #[must_use]
    pub fn enter(&self) -> Current {
        self.schemas.enter()
    }
}

/// The schemas of the packages one host or store holds.
///
/// One per store, not one per process: two packages can be loaded at once whose types share an
/// id — two versions of an app share every id — so a table keyed by id alone would let one
/// overwrite the other's schema.
pub struct Schemas {
    types: RwLock<BTreeMap<String, &'static TypeSchema>>,
    /// The table consulted for the types this one does not hold: one package's table holds its
    /// own types by the version it loaded, and every other type by whatever registered it.
    parent: Option<Arc<Schemas>>,
}

impl std::fmt::Debug for Schemas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schemas")
            .field("types", &self.types.read().map_or(0, |held| held.len()))
            .field("parent", &self.parent.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for Schemas {
    fn default() -> Self {
        Self::new()
    }
}

impl Schemas {
    /// A table of its own: only what is registered in it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            types: RwLock::new(BTreeMap::new()),
            parent: None,
        }
    }

    /// A table of a store's own: the packages it loads, and, for a type none of them declares
    /// (a record naming another app's type), whatever package registered it on this thread.
    #[must_use]
    pub fn store() -> Self {
        Self::over(fallback())
    }

    /// A table of its own over `parent`: the types registered here, the rest from `parent`.
    #[must_use]
    pub fn over(parent: Arc<Schemas>) -> Self {
        Self {
            types: RwLock::new(BTreeMap::new()),
            parent: Some(parent),
        }
    }

    /// Make `schema` the one `schema.id` resolves to here, and return the registered copy.
    ///
    /// Registering what is already registered returns the held copy and leaks nothing; a changed
    /// schema (a new package version) replaces it.
    pub fn register(&self, schema: TypeSchema) -> &'static TypeSchema {
        // What this table holds itself, never what its parent does: a package's table keeps its
        // own copy of every type it declares, even when its parent holds the same schema.
        let held = self
            .types
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&schema.id)
            .copied();
        if let Some(held) = held
            && *held == schema
        {
            return held;
        }
        let leaked: &'static TypeSchema = Box::leak(Box::new(schema));
        self.types
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(leaked.id.clone(), leaked);
        // A caller that holds no table of its own reads whichever version was registered last
        // (the process's fallback); a store still reads this one.
        let fallback = fallback();
        if !std::ptr::eq(fallback.as_ref(), self) {
            fallback.register(leaked.clone());
        }
        leaked
    }

    /// The schema of the type `id`, when a package of it was registered here.
    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&'static TypeSchema> {
        self.types
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .copied()
            .or_else(|| self.parent.as_ref().and_then(|parent| parent.lookup(id)))
    }

    /// The registered type `name` of the app `app`.
    #[must_use]
    pub fn lookup_in(&self, app: &str, name: &str) -> Option<&'static TypeSchema> {
        let found = self
            .types
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .copied()
            .find(|schema| schema.app == app && schema.name == name);
        found.or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.lookup_in(app, name))
        })
    }

    /// Make this table the one [`lookup`] resolves through on this thread until the returned
    /// scope drops.
    ///
    /// The store, its snapshots and its index install their own while they read, write and
    /// derive, so what a record's type id means is the schema of the package version the store
    /// holds.
    #[must_use]
    pub fn enter(self: &Arc<Self>) -> Current {
        CURRENT.with(|current| current.borrow_mut().push(Arc::clone(self)));
        Current(())
    }

    /// The table this thread resolves through now: the innermost installed, else the fallback.
    fn grabbed() -> Arc<Self> {
        CURRENT.with(|current| current.borrow().last().cloned().unwrap_or_else(fallback))
    }
}

/// Register `meta`'s package's schemas for the process's shared table of packages, by its app
/// and `module` (the runtime's id of the loaded module, which an action records as its
/// `authorship.module`): what an action applied or described outside the store that holds its
/// records resolves its own package's types through ([`for_module`]).
pub fn register_package(
    meta: &tilolang_runtime::meta::Meta,
    module: &tilolang_runtime::ids::ModuleHash,
) -> Arc<PackageSchemas> {
    // The types it declares it holds by its own version; a type it does not (the host's
    // `Invocation`, a record of another app) it resolves by whatever registered it, as a store
    // does. An action that writes one of those would otherwise refuse under its package's table.
    let schemas = Arc::new(PackageSchemas {
        schemas: Arc::new(Schemas::over(fallback())),
    });
    {
        let _current = schemas.enter();
        for schema in crate::meta::MetaExt::schemas(meta) {
            schemas.register(schema);
        }
    }
    let package = Arc::clone(&schemas);
    PACKAGES.with(|packages| {
        packages
            .borrow_mut()
            .insert((meta.app.clone(), module.0.clone()), Arc::clone(&package));
    });
    package
}

/// The schemas of the package of `app` whose module is `module` (`register_package`).
#[must_use]
pub fn for_module(app: &str, module: &str) -> Option<Arc<PackageSchemas>> {
    PACKAGES.with(|packages| {
        packages
            .borrow()
            .get(&(app.to_owned(), module.to_owned()))
            .cloned()
    })
}

/// Makes the table [`Schemas::enter`] installed apply again when dropped.
pub struct Current(());

impl Drop for Current {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.borrow_mut().pop();
        });
    }
}

/// Make `schema` the one `schema.id` resolves to in the table this thread reads through, and
/// return the registered copy; see [`Schemas::register`].
pub fn register(schema: TypeSchema) -> &'static TypeSchema {
    Schemas::grabbed().register(schema)
}

/// The schema of the type `id` in the table this thread reads through; see [`Schemas::lookup`].
#[must_use]
pub fn lookup(id: &str) -> Option<&'static TypeSchema> {
    Schemas::grabbed().lookup(id)
}

/// The registered type `name` of the app `app`; see [`Schemas::lookup_in`].
#[must_use]
pub fn lookup_in(app: &str, name: &str) -> Option<&'static TypeSchema> {
    Schemas::grabbed().lookup_in(app, name)
}

/// Record that `app` was published under the ids `before` earlier: the records the workspace holds
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
/// before (`previously:`), else `stored` itself.
///
/// Read where an app's id is the process's ([`tilo_action_policy::earlier`]): its `previously:`
/// is fixed by the version of the app whose records these are, so a later version of another app
/// renaming to it, another package loaded here for another host, never takes its records over.
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
