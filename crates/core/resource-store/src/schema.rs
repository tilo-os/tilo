//! What an app declares: its resource types, the fields of each, and which types nest in which.
//! Nothing here says how a resource is stored; that is the store's alone.
//!
//! It also declares what each of the app's verbs writes, so a holder checks that a change writes
//! only what the verb it names may write.

use std::collections::{BTreeMap, BTreeSet};

use tilo_actions::CREATE_ACTION;
use tilo_actions::surface::{FieldType, NamedType, RESOURCE_UNIT, ReaderRule, Surface, TypeField};

/// The verb a change that sets a resource's field names: the field edit any editable field
/// takes, which writes only fields that are not read-only.
pub const FIELD_EDIT: &str = "edit";

/// The verbs of history every schema has: name a version (`checkpoint`), say what a step did
/// (`describe`), and tie a step to another resource (`link`). Each is a change with no
/// operations, judged by who may write its document and by its arguments alone
/// ([`MAX_MARK_ARGS`]).
pub const HISTORY_VERBS: [&str; 3] = ["checkpoint", "describe", "link"];

/// The verb that takes a resource from one workspace to another: the destination's founding of it
/// and the source's removal, each judged by whether its caller may write that kind there. Every
/// schema has it, as it has the verbs of history.
pub const MOVE_VERB: &str = "move";

/// The verb that founds an empty resource of a declared kind: an app's record its first write
/// names no verb of, which the app opens empty. It writes nothing in it. Every schema has it, as
/// it has the move, and no app declares it.
pub const CREATE_VERB: &str = "create";

/// The most bytes the arguments of a history verb's change take, as JSON.
pub const MAX_MARK_ARGS: usize = 4096;

static HISTORY: std::sync::LazyLock<Vec<Verb>> = std::sync::LazyLock::new(|| {
    HISTORY_VERBS
        .iter()
        .map(|name| Verb {
            name: (*name).to_owned(),
            marks: true,
            ..Verb::default()
        })
        .chain([
            Verb {
                name: MOVE_VERB.to_owned(),
                moves: true,
                ..Verb::default()
            },
            Verb {
                name: CREATE_VERB.to_owned(),
                creates: true,
                ..Verb::default()
            },
        ])
        .collect()
});

/// The resource types one app declares, and what each of its verbs writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub types: Vec<Type>,
    /// Each verb's write-set; a schema that declares none checks no change's verb.
    pub verbs: Vec<Verb>,
    /// Who reads a resource of each kind that says, by kind: the store places it by this alone.
    pub readers: BTreeMap<String, ReaderRule>,
}

/// What one verb writes: each kind it writes resources of, and the kinds it may write that
/// another surface declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verb {
    pub name: String,
    pub writes: BTreeMap<String, Writes>,
    /// The kinds nested in the verb's own record that this surface does not type — a tilolang
    /// package publishes a surface per type, so a list's items are another surface's kind. The
    /// verb may make, move and remove them, and their own surface declares their fields.
    pub nested: BTreeSet<String>,
    /// A verb of history ([`HISTORY_VERBS`]): its change has no operations.
    pub marks: bool,
    /// The move ([`MOVE_VERB`]): it founds, fills and removes a resource of any declared kind.
    pub moves: bool,
    /// The creation ([`CREATE_VERB`]): it founds an empty resource of any declared kind.
    pub creates: bool,
}

/// What a verb writes in a resource of one kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Writes {
    /// The fields it may set.
    pub fields: BTreeSet<String>,
    /// It only sets fields: it makes, removes and places none, and edits no content.
    pub sets_only: bool,
}

/// One resource type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Type {
    /// What the store and every query name it by.
    pub name: String,
    /// The fields the index holds: what a filter, a sort or a count reads.
    pub fields: Vec<Field>,
    /// The types of the resources nested in one of this type, each under the field holding them.
    pub nested: Vec<(String, String)>,
    /// When one is live (a track playing, a timer running): filters over its indexed fields,
    /// every one of which holds of a live resource ([`crate::Store::live`]). `None` for a type
    /// that never is.
    pub live: Option<Vec<collab_types::store::Filter>>,
    /// The containers at the root of a resource's content that its fields keep their items in,
    /// by field: what its derived founding makes ([`collab_types::document::founding`]).
    pub founding: Vec<(String, automerge::ObjType)>,
}

/// One indexed field of a resource type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub kind: FieldKind,
    /// The values it may hold.
    pub value: Expected,
    /// It may hold nothing.
    pub optional: bool,
}

/// The values a field may hold, as a holder checks each change it receives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expected {
    Any,
    Text,
    Int,
    Float,
    Bool,
    /// One of these cases of an enum.
    OneOf(Vec<String>),
}

impl Expected {
    /// The values a field of type `ty` holds, scalar or member of a set, and whether it is
    /// optional.
    fn of(ty: &FieldType, cases: &dyn Fn(&str) -> Option<Vec<String>>) -> (Self, bool) {
        match ty {
            FieldType::Option(inner) | FieldType::Set(inner) => (Self::of(inner, cases).0, true),
            FieldType::Text
            | FieldType::Date
            | FieldType::Time
            | FieldType::Identifier
            | FieldType::Actor
            | FieldType::Resource(_) => (Self::Text, false),
            FieldType::Int => (Self::Int, false),
            FieldType::Float => (Self::Float, false),
            FieldType::Bool => (Self::Bool, false),
            FieldType::Named(name) => (cases(name).map_or(Self::Any, Self::OneOf), false),
            _ => (Self::Any, false),
        }
    }
}

/// How the index holds a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// One value, compared, sorted and ranged.
    Scalar,
    /// Distinct values, filtered by membership.
    Set,
}

impl FieldKind {
    /// How the index holds a field of type `ty`; `None` for one it does not hold: rich text,
    /// lists, maps, nested resources and resources, the platform's reactions and outboxes. Those
    /// are the resource's own content, read from the resource.
    #[must_use]
    pub fn of(ty: &FieldType, enums: &dyn Fn(&str) -> bool) -> Option<Self> {
        match ty {
            FieldType::Option(inner) => Self::of(inner, enums),
            FieldType::Set(inner) => scalar(inner, enums).then_some(Self::Set),
            other => scalar(other, enums).then_some(Self::Scalar),
        }
    }
}

/// Whether a value of `ty` is one scalar: a text, number, flag, date, instant, account, actor,
/// reference to a resource, or a case of an enum without fields.
fn scalar(ty: &FieldType, enums: &dyn Fn(&str) -> bool) -> bool {
    match ty {
        FieldType::Text
        | FieldType::Int
        | FieldType::Float
        | FieldType::Bool
        | FieldType::Date
        | FieldType::Time
        | FieldType::Identifier
        | FieldType::Actor
        | FieldType::Resource(_)
        | FieldType::Duration => true,
        FieldType::Named(name) => enums(name),
        FieldType::Option(inner) => scalar(inner, enums),
        _ => false,
    }
}

impl Schema {
    /// The resource types `surface` declares: each resource type it names, with its fields, and
    /// the types nested in it as collections of them. A surface that names no type of its own
    /// kind — a tilolang package describes its record by the fields it lists — declares that
    /// record too, so its own kind and what its verbs write are known to the store.
    #[must_use]
    pub fn from_surface(surface: &Surface) -> Self {
        let enums = |name: &str| {
            surface.types.iter().any(|ty| {
                ty.name == name
                    && ty.is_enum()
                    && ty.cases.iter().all(|case| case.fields.is_empty())
            })
        };
        let cases = |name: &str| {
            surface
                .types
                .iter()
                .find(|ty| ty.name == name && ty.is_enum())
                .map(|ty| ty.cases.iter().map(|case| case.name.clone()).collect())
        };
        let mut declared: Vec<NamedType> = surface.types.clone();
        // The surface's own record: the type named by its kind, else by its label, else the
        // record it describes in its fields (a tilolang package names no type of its own).
        let own_name = if declared.iter().any(|ty| ty.name == surface.kind) {
            surface.kind.clone()
        } else if declared.iter().any(|ty| ty.name == surface.label) {
            surface.label.clone()
        } else {
            declared.push(own_type(surface));
            surface.kind.clone()
        };
        let own = declared
            .iter()
            .find(|ty| ty.name == own_name)
            .or_else(|| declared.first());
        // What the surface says of when its own record is live. A condition this store cannot
        // read (one a later version wrote) leaves the type never live here rather than wrong.
        let live: Option<Vec<collab_types::store::Filter>> = surface
            .live
            .as_ref()
            .and_then(|live| serde_json::from_value(live.when.clone()).ok());
        let types = declared
            .iter()
            .filter(|ty| !ty.is_enum())
            .map(|ty| {
                let mut ty_of = Type::of(ty, &enums, &cases);
                // A record one of the surface's own fields holds is nested in its record, as an
                // item of a collection is: its type names no `stored`, the surface's field does.
                if ty.name == own_name {
                    ty_of.nested.extend(held_records(surface));
                    ty_of.live.clone_from(&live);
                    ty_of.founding = collab_types::document::founding::containers(surface);
                }
                ty_of
            })
            .collect();
        let verbs = surface
            .verbs
            .iter()
            .map(|verb| {
                let mut units: BTreeSet<&str> = verb
                    .writes
                    .iter()
                    .filter(|unit| unit.as_str() != RESOURCE_UNIT)
                    .map(|unit| unit.strip_suffix(".*").unwrap_or(unit))
                    .collect();
                units.extend(verb.field.as_deref());
                let mut nested = nested_kinds(surface);
                // A verb that answers a record it makes (`answer(a: Ask) -> Reply`) makes one
                // of that kind, wherever the app nests it.
                if let Some(FieldType::Resource(kind)) =
                    verb.returns.as_ref().map(|returns| &returns.ty)
                {
                    nested.insert(kind.clone());
                }
                let mut writes = writes_of(surface, own, &|_, field| units.contains(field), false);
                // A tilolang action on no resource is declared on its package's own surface,
                // which holds no fields: its verb names that kind, so the package's code is
                // what bounds it within the package's records, as for a verb of one of its types.
                if writes.is_empty() && surface.kind.starts_with("tl.") {
                    writes.insert(surface.kind.clone(), Writes::default());
                }
                Verb {
                    name: verb.id.clone(),
                    writes,
                    nested,
                    ..Verb::default()
                }
            })
            .chain(std::iter::once(Verb {
                name: FIELD_EDIT.to_owned(),
                writes: writes_of(surface, own, &|_, _| true, true),
                ..Verb::default()
            }))
            .collect();
        Self {
            types,
            verbs,
            readers: surface
                .readers
                .iter()
                .map(|rule| (surface.kind.clone(), rule.clone()))
                .collect(),
        }
    }

    /// What the verb `name` writes, when it is declared.
    #[must_use]
    pub fn verb(&self, name: &str) -> Option<&Verb> {
        self.verbs.iter().find(|verb| verb.name == name)
    }

    /// Every declared verb that names `name`: the verb of that name, and each family of verbs
    /// declared by a prefix ending in `*` (`set-*`, a typed setter per field of a kind whose
    /// fields another table lists) that `name` starts with.
    /// The verbs of history ([`HISTORY_VERBS`]), the move ([`MOVE_VERB`]) and the creation
    /// ([`CREATE_VERB`]) are declared in every schema.
    pub fn verbs_naming<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Verb> + 'a {
        self.verbs.iter().chain(HISTORY.iter()).filter(move |verb| {
            verb.name == name
                || verb
                    .name
                    .strip_suffix('*')
                    .is_some_and(|prefix| name.len() > prefix.len() && name.starts_with(prefix))
        })
    }

    /// The type named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Type> {
        self.types.iter().find(|ty| ty.name == name)
    }
}

/// The kinds the surface's own record holds records of that no surface here types: each
/// collection of resources and each record a field holds, by its kind.
fn nested_kinds(surface: &Surface) -> BTreeSet<String> {
    let mut kinds = BTreeSet::new();
    for field in surface
        .fields
        .iter()
        .filter(|field| !field.path.contains('.'))
    {
        let mut ty = &field.ty;
        while let FieldType::Option(inner) | FieldType::Set(inner) | FieldType::List(inner) = ty {
            ty = inner;
        }
        if let FieldType::Collection(item) = ty {
            kinds.extend(collected(item).filter_map(|case| match case {
                FieldType::Resource(kind) => Some(kind.clone()),
                _ => None,
            }));
        }
    }
    kinds.extend(held_records(surface).into_iter().map(|(_, kind)| kind));
    kinds
}

/// What a collection of `item` holds: that one type, or each case of a union of them
/// (`Collection<Category | TextChannel>`).
fn collected(item: &FieldType) -> impl Iterator<Item = &FieldType> {
    match item {
        FieldType::Union(cases) => cases.as_slice(),
        item => std::slice::from_ref(item),
    }
    .iter()
}

/// The records the surface's own record holds one of in a field (`stored = "record"`), each by
/// the name its record keeps the field under ([`own_type`]) and the kind it is.
fn held_records(surface: &Surface) -> Vec<(String, String)> {
    let by_title = surface.verbs.iter().any(|verb| verb.id == CREATE_ACTION);
    surface
        .fields
        .iter()
        .filter(|field| !field.path.contains('.') && field.stored == RESOURCE_UNIT)
        .filter_map(|field| {
            let mut ty = &field.ty;
            while let FieldType::Option(inner) = ty {
                ty = inner;
            }
            let FieldType::Resource(kind) = ty else {
                return None;
            };
            let name = if by_title && !field.title.is_empty() {
                field.title.clone()
            } else {
                field.path.clone()
            };
            Some((name, kind.clone()))
        })
        .collect()
}

/// The surface's own record, as a type: the fields it lists at its root (a dotted path names a
/// field of a type nested in it, which declares its own). Its name is the kind its resources are.
///
/// A tilolang package — one whose surface declares `$create`, the host's own verb for a
/// package's root — names a field's path by the id the runtime keeps it under and stores it
/// under its title, so the record's fields are the titles there; any other surface stores the
/// field its path names.
fn own_type(surface: &Surface) -> NamedType {
    let by_title = surface.verbs.iter().any(|verb| verb.id == CREATE_ACTION);
    NamedType {
        name: surface.kind.clone(),
        doc: String::new(),
        fields: surface
            .fields
            .iter()
            .filter(|field| !field.path.contains('.'))
            .map(|field| TypeField {
                name: if by_title && !field.title.is_empty() {
                    field.title.clone()
                } else {
                    field.path.clone()
                },
                ty: field.ty.clone(),
                title: field.title.clone(),
            })
            .collect(),
        cases: Vec::new(),
    }
}

/// The kinds and fields a verb of `surface` writes: everything `own`, the surface's type, holds
/// outside a storage unit of its own, and the units `declared` names, wherever they sit. A field
/// edit (`editing`) writes any unit and no read-only field.
fn writes_of(
    surface: &Surface,
    own: Option<&NamedType>,
    declared: &dyn Fn(&str, &str) -> bool,
    editing: bool,
) -> BTreeMap<String, Writes> {
    // Each declared field's owner and name, as the surface lists them.
    let flags = |ty: &str, field: &str, flag: fn(&tilo_actions::surface::FieldSurface) -> bool| {
        surface.fields.iter().any(|stored| {
            flag(stored)
                && stored.path.rsplit('.').next() == Some(field)
                && (stored.owner.is_empty() || stored.owner == kebab(ty))
        })
    };
    let declared_types: Vec<&NamedType> = surface.types.iter().filter(|ty| !ty.is_enum()).collect();
    let named = |name: &str| {
        declared_types
            .iter()
            .copied()
            .find(|ty| ty.name == name)
            .or_else(|| own.filter(|own| own.name == name))
    };
    let mut writes: BTreeMap<String, Writes> = BTreeMap::new();
    let mut reached: Vec<&NamedType> = Vec::new();
    // Every type a declared unit nests, and the verb's own: the surface's.
    for ty in &declared_types {
        for field in &ty.fields {
            if declared(&ty.name, &field.name)
                && let FieldType::Collection(item) = &field.ty
                && let FieldType::Named(nested) = item.as_ref()
                && let Some(nested) = named(nested)
            {
                reached.push(nested);
            }
        }
    }
    reached.extend(own.or_else(|| declared_types.first().copied()));
    while let Some(ty) = reached.pop() {
        if writes.contains_key(&ty.name) {
            continue;
        }
        let writing = writes.entry(ty.name.clone()).or_default();
        writing.sets_only = editing;
        for field in &ty.fields {
            let unit = flags(&ty.name, &field.name, |stored| stored.unit);
            if (unit && !editing && !declared(&ty.name, &field.name))
                || (editing && flags(&ty.name, &field.name, |stored| stored.readonly))
            {
                continue;
            }
            match &field.ty {
                FieldType::Collection(item) => {
                    if let FieldType::Named(nested) = item.as_ref()
                        && let Some(nested) = named(nested)
                    {
                        reached.push(nested);
                    }
                }
                _ => {
                    writing.fields.insert(field.name.clone());
                }
            }
        }
    }
    writes
}

/// `name` in kebab case, as a surface names a field's owner.
fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len().saturating_add(4));
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

impl Type {
    fn of(
        declared: &NamedType,
        enums: &dyn Fn(&str) -> bool,
        cases: &dyn Fn(&str) -> Option<Vec<String>>,
    ) -> Self {
        let mut fields = Vec::new();
        let mut nested = Vec::new();
        for field in &declared.fields {
            match &field.ty {
                FieldType::Collection(item) => {
                    // A type names what it nests by name (a Rust resource) or by kind (a
                    // tilolang package, whose records are kinds of their own).
                    for case in collected(item) {
                        if let FieldType::Named(kind) | FieldType::Resource(kind) = case {
                            nested.push((field.name.clone(), kind.clone()));
                        }
                    }
                }
                ty => {
                    if let Some(kind) = FieldKind::of(ty, enums) {
                        let (value, optional) = Expected::of(ty, cases);
                        fields.push(Field {
                            name: field.name.clone(),
                            kind,
                            value,
                            optional,
                        });
                    }
                }
            }
        }
        Self {
            name: declared.name.clone(),
            fields,
            nested,
            live: None,
            founding: Vec::new(),
        }
    }

    /// The indexed field `name`.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }
}
