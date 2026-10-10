//! Actions on the items of a collection, offered on the items themselves.
//!
//! An action whose first parameter is an item [`Id`] — "mark this todo
//! done", "delete this card" — acts on one element of a
//! [`Collection`](crate::Collection). Asking the user (or an agent) to type
//! that id is the wrong interface: the palette and context menus should offer
//! the action *on* the item, with the id already bound. So `#[resource]`
//! publishes every item of the record's collections as a child resource
//! whose actions are the item actions for it, their remaining parameters
//! still typed. The action stays published on the record too, with its id
//! parameter, for scripts that address items by id.
//!
//! The collection an action acts on is the field its item parameter is
//! named after, or the record's only collection; `#[action(item = field)]`
//! names it otherwise. `#[resource]` refuses to compile an item action whose
//! collection is neither.
//!
//! ```
//! use tilo_resource::{Collection, Id, Model, resource};
//!
//! #[derive(Model, Default)]
//! pub struct Shelf { pub books: Collection<String>, pub loans: Collection<String> }
//!
//! #[resource(kind = "doc.shelf")]
//! impl Shelf {
//!     /// Lend a book.
//!     #[action(item = books)]
//!     pub fn lend(&mut self, book: &Id) { self.books.remove(book); }
//!
//!     /// Return a loan: the parameter names its collection.
//!     #[action]
//!     pub fn loans(&mut self, loans: &Id) { self.loans.remove(loans); }
//! }
//! # fn main() {}
//! ```
//!
//! With two collections and neither named, it does not compile:
//!
//! ```compile_fail
//! use tilo_resource::{Collection, Id, Model, resource};
//!
//! #[derive(Model, Default)]
//! pub struct Shelf { pub books: Collection<String>, pub loans: Collection<String> }
//!
//! #[resource(kind = "doc.shelf")]
//! impl Shelf {
//!     /// Lend a book.
//!     #[action]
//!     pub fn lend(&mut self, book: &Id) { self.books.remove(book); }
//! }
//! # fn main() {}
//! ```
//!
//! nor does naming a collection the record does not have:
//!
//! ```compile_fail
//! use tilo_resource::{Collection, Id, Model, resource};
//!
//! #[derive(Model, Default)]
//! pub struct Shelf { pub books: Collection<String> }
//!
//! #[resource(kind = "doc.shelf")]
//! impl Shelf {
//!     /// Lend a book.
//!     #[action(item = loans)]
//!     pub fn lend(&mut self, book: &Id) { self.books.remove(book); }
//! }
//! # fn main() {}
//! ```
//!
//! or naming one for an action that takes no item:
//!
//! ```compile_fail
//! use tilo_resource::{Collection, Model, resource};
//!
//! #[derive(Model, Default)]
//! pub struct Shelf { pub books: Collection<String> }
//!
//! #[resource(kind = "doc.shelf")]
//! impl Shelf {
//!     /// Clear the shelf.
//!     #[action(item = books)]
//!     pub fn clear(&mut self, everything: bool) { if everything { self.books = Collection::new(); } }
//! }
//! # fn main() {}
//! ```

use std::any::TypeId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use tilo_actions::{ActionArgs, ActionParam, ActionSpec, ExecutionLocus};

use crate::collection::Id;
use crate::resource::{ActionDecl, Resource};

/// One labelled item of a collection, as search indexes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemText {
    /// The collection field's Rust name.
    pub field: &'static str,
    pub id: Id,
    /// Its `#[model(label)]`.
    pub label: String,
    /// All of its text.
    pub text: String,
}

/// The items of one collection field, as `#[derive(Model)]` lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCollection {
    /// The field's Rust name.
    pub field: &'static str,
    /// Its items carry a `#[model(label)]`: they are published as item
    /// resources (which search hits and agents name) even with no item
    /// verbs or effects.
    pub labelled: bool,
    /// Each item's id and a human label (its first text, else its id).
    pub items: Vec<(Id, String)>,
    /// The items this collection sits in, outermost first: a slide's
    /// elements are within `("slides", <slide>)`. Empty for the record's own
    /// collections.
    pub within: Vec<(&'static str, Id)>,
}

/// The id of the child resource an item of a nested collection is
/// published as: `<kind>/slides/<slide>/elements/<element>`.
pub fn nested_resource_id<R: Resource>(within: &[(&str, Id)], field: &str, item: &Id) -> String {
    let mut id = R::KIND.to_owned();
    for (outer, outer_id) in within {
        id.push('/');
        id.push_str(outer);
        id.push('/');
        id.push_str(outer_id.as_str());
    }
    format!("{id}/{field}/{item}")
}

/// One action whose first parameter is an item id, as `#[resource]`
/// records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemAction {
    /// The action's id.
    pub action: &'static str,
    /// The name of its item parameter.
    pub param: &'static str,
    /// The collection field named by `#[action(item = "...")]`, if any.
    pub collection: Option<&'static str>,
    /// For an item of a nested collection (`item = slides.elements`): the
    /// enclosing collections, outermost first…
    pub within: &'static [&'static str],
    /// …and the parameters naming their items.
    pub within_params: &'static [&'static str],
}

/// The id of the child resource an item is published as.
pub fn item_resource_id<R: Resource>(field: &str, item: &Id) -> String {
    format!("{}/{field}/{item}", R::KIND)
}

/// The item an item resource id names, if it is one of `R`'s.
pub fn item_of_resource<R: Resource>(resource_id: &str) -> Option<(&str, Id)> {
    let rest = resource_id.strip_prefix(R::KIND)?.strip_prefix('/')?;
    let (field, item) = rest.split_once('/')?;
    Some((field, Id::from_raw(item)))
}

fn targets(action: &ItemAction, fields: &[&str], field: &str) -> bool {
    if !action.within.is_empty() {
        return false;
    }
    if let Some(named) = action.collection {
        return named == field;
    }
    if fields.contains(&action.param) {
        action.param == field
    } else {
        fields.len() == 1
    }
}

/// How many items of one collection a published tree carries; the rest are
/// resolved one by one when addressed ([`item_child`]).
pub const ITEM_PAGE: usize = 256;

type Cached = HashMap<Id, (String, u64, tilo_actions::Resource)>;

thread_local! {
    /// Item resources built before, by resource type and collection, then
    /// item: rebuilt only when the item's label or the app's code changed.
    static BUILT: RefCell<HashMap<(TypeId, String), Cached>> = RefCell::new(HashMap::new());
}

/// The item resource for `id` in `field`, labelled `label`, offering the
/// item actions `scoped`: from the cache when it was built for the same
/// label and code, else built and kept.
fn item_resource<R: Resource>(
    scoped: &[(&ItemAction, &ActionDecl<R>)],
    field: &str,
    id: &Id,
    label: &str,
) -> tilo_actions::Resource {
    cached_item(TypeId::of::<R>(), field, id, label, &|| {
        build_item(scoped, field, id, label)
    })
}

/// The item resource `build` makes, from the cache of the resource type
/// `kind` when it was built for the same label and code.
fn cached_item(
    kind: TypeId,
    field: &str,
    id: &Id,
    label: &str,
    build: &dyn Fn() -> tilo_actions::Resource,
) -> tilo_actions::Resource {
    let epoch = crate::resource::type_epoch();
    let key = (kind, field.to_owned());
    let cached = BUILT.with(|built| {
        built
            .borrow()
            .get(&key)
            .and_then(|items| items.get(id))
            .filter(|(cached_label, cached_epoch, _)| {
                cached_label == label && *cached_epoch == epoch
            })
            .map(|(_, _, resource)| resource.clone())
    });
    if let Some(resource) = cached {
        debug_assert_eq!(
            fingerprint(&resource),
            fingerprint(&build()),
            "a cached item resource differs from a fresh one"
        );
        return resource;
    }
    let resource = build();
    BUILT.with(|built| {
        built
            .borrow_mut()
            .entry(key)
            .or_default()
            .insert(id.clone(), (label.to_owned(), epoch, resource.clone()));
    });
    resource
}

fn build_item<R: Resource>(
    scoped: &[(&ItemAction, &ActionDecl<R>)],
    field: &str,
    id: &Id,
    label: &str,
) -> tilo_actions::Resource {
    let mut child = tilo_actions::Resource::new(
        item_resource_id::<R>(field, id),
        format!("{}.{field}", R::KIND),
        label,
    );
    for (action, decl) in scoped {
        child = child.with_action(item_spec(decl, action.param, id));
    }
    for spec in R::item_effects(field, id) {
        child = child.with_action(spec);
    }
    for view in R::item_views(field, id) {
        child = child.with_view(view);
    }
    if let Some(rename) = rename_spec::<R>(field, id)
        && !child.actions.iter().any(|action| action.id == rename.id)
    {
        child = child.with_action(rename);
    }
    // Its fields, edited from the item.
    if let Some(spec) = crate::resource::item_field_edit_spec::<R>(field, id) {
        child = child.with_action(spec);
    }
    child
}

/// The id of the prompt that renames an item.
pub const RENAME: &str = "rename";

/// "Rename" on an item whose `#[model(label)]` is a plain text field: the
/// field edit that sets it, asking for the new name, so the palette and
/// agents offer what a person would otherwise type into the item's title.
fn rename_spec<R: Resource>(field: &str, id: &Id) -> Option<ActionSpec> {
    use crate::path::{EditOp, Segment};
    use crate::shape::Shape;
    if !R::EDITS {
        return None;
    }
    let collection = R::shape()?
        .fields()
        .iter()
        .find(|candidate| candidate.name == field)?;
    let (Shape::Collection(item) | Shape::Keyed(item)) = collection.shape else {
        return None;
    };
    let label = item.fields().iter().find(|candidate| candidate.label)?;
    let plain_text = label.ty.ends_with("String") || label.ty.ends_with("Atom");
    if collection.readonly || label.readonly || !plain_text {
        return None;
    }
    let path = vec![
        Segment::Field(collection.key.to_owned()),
        Segment::Item(id.clone()),
        Segment::Field(label.key.to_owned()),
    ];
    let param = label.name;
    let decl = ActionDecl::<R>::new(
        RENAME,
        "Rename",
        vec![ActionParam::required_owned(
            param,
            tilo_actions::ParamType::Text,
            label.title,
        )],
        move |args, now_ms| {
            let Some(tilo_actions::ArgValue::Text(name)) = args.get(param) else {
                return Err(format!("missing `{param}`"));
            };
            let edit = crate::Edit::<R>::new(
                path.clone(),
                EditOp::Set {
                    value: serde_json::Value::String(name.clone()),
                },
            );
            let mut action =
                R::edit_action(edit).ok_or_else(|| format!("{} has no field edits", R::LABEL))?;
            R::stamp(&mut action, now_ms);
            Ok(action)
        },
    );
    Some(crate::resource::declaration_spec(&decl))
}

/// What a published resource is, as text: its identity, its actions with
/// their parameters and contract, and its children, for telling two
/// publications of the same thing apart.
#[must_use]
pub fn fingerprint(resource: &tilo_actions::Resource) -> String {
    use std::fmt::Write as _;
    let mut out = format!(
        "{}|{}|{}",
        resource.id.as_str(),
        resource.kind,
        resource.label
    );
    for spec in &resource.actions {
        let _ = write!(
            out,
            "|{}:{}:{:?}:{}",
            spec.id, spec.label, spec.kind, spec.contract.reversible
        );
        for param in spec.params() {
            let _ = write!(out, ",{}:{:?}:{}", param.name, param.ty, param.required);
        }
    }
    for child in &resource.children {
        let _ = write!(out, "[{}]", fingerprint(child));
    }
    out
}

/// The item resources of the collections `fields` of `R` that `actions`
/// act on: what a publication would carry for item `resource_id`, built on
/// its own. `label_of(field, id)` names an item, or says it is not there.
pub fn item_child<R: Resource>(
    resource_id: &str,
    fields: &[&str],
    actions: &[ItemAction],
    label_of: impl FnOnce(&str, &Id) -> Option<String>,
) -> Option<tilo_actions::Resource> {
    let (field, id) = item_of_resource::<R>(resource_id)?;
    let field = *fields.iter().find(|candidate| **candidate == field)?;
    let declarations = R::declarations();
    let scoped = scoped::<R>(&declarations, actions, fields, field);
    let label = label_of(field, &id)?;
    Some(item_resource(&scoped, field, &id, &label))
}

fn scoped<'d, 'a, R: Resource>(
    declarations: &'d [ActionDecl<R>],
    actions: &'a [ItemAction],
    fields: &[&str],
    field: &str,
) -> Vec<(&'a ItemAction, &'d ActionDecl<R>)> {
    actions
        .iter()
        .filter(|action| targets(action, fields, field))
        .filter_map(|action| {
            declarations
                .iter()
                .find(|decl| decl.id == action.action)
                .map(|decl| (action, decl))
        })
        .collect()
}

/// The collection an item action with item parameter `param` (and the
/// collection `named` by `#[action(item = field)]`, or `""`) acts on.
#[doc(hidden)]
#[must_use]
pub fn item_field(
    collections: &'static [&'static str],
    param: &str,
    named: &str,
) -> Option<&'static str> {
    if !named.is_empty() {
        return collections.iter().copied().find(|field| *field == named);
    }
    collections
        .iter()
        .copied()
        .find(|field| *field == param)
        .or(match collections {
            [only] => Some(*only),
            _ => None,
        })
}

/// Whether an item action with item parameter `param` (and the collection
/// `named` by `#[action(item = field)]`, or `""`) acts on one of
/// `collections`: the check `#[resource]` runs when it compiles.
#[doc(hidden)]
#[must_use]
pub const fn item_target(collections: &[&str], param: &str, named: &str) -> bool {
    if !named.is_empty() {
        return contains(collections, named);
    }
    contains(collections, param) || collections.len() == 1
}

/// Whether `#[action(item = field)]` names one of `outboxes`: the verb acts
/// on an outbox entry, which is published on no item.
#[doc(hidden)]
#[must_use]
pub const fn entry_target(outboxes: &[&str], named: &str) -> bool {
    !named.is_empty() && contains(outboxes, named)
}

#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a const fn walks the slices by index, bounded by their lengths"
)]
const fn contains(names: &[&str], name: &str) -> bool {
    let mut index = 0;
    while index < names.len() {
        if same(names[index].as_bytes(), name.as_bytes()) {
            return true;
        }
        index += 1;
    }
    false
}

#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a const fn walks the slices by index, bounded by their lengths"
)]
const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The item resources of a record: one per item of each collection, carrying
/// the item actions that act on that collection with the item bound.
#[inline(never)]
pub fn item_children<R: Resource>(
    collections: &[ItemCollection],
    actions: &[ItemAction],
) -> Vec<tilo_actions::Resource> {
    let mut children = Vec::new();
    for_each_item_child::<R>(collections, actions, &mut |_, _, _, build| {
        children.push(build());
    });
    children
}

/// What [`for_each_item_child`] calls per item child: the collection it is
/// an item of, its id and label, and how to build it.
pub type ItemChildVisitor<'a> =
    dyn FnMut(&ItemCollection, &Id, &str, &dyn Fn() -> tilo_actions::Resource) + 'a;

/// [`item_children`] one item at a time, for a publisher that keeps the
/// children it built for an earlier version: `each(collection, id, label,
/// build)` is called for every item child in order, and `build()` makes
/// that child exactly as [`item_children`] does. A child is a function of
/// its collection (field and the items it sits in), its id and its label,
/// and the locale its actions' labels are in.
#[inline(never)]
pub fn for_each_item_child<R: Resource>(
    collections: &[ItemCollection],
    actions: &[ItemAction],
    each: &mut ItemChildVisitor<'_>,
) {
    let declarations = R::declarations();
    let fields: Vec<&str> = collections
        .iter()
        .filter(|collection| collection.within.is_empty())
        .map(|collection| collection.field)
        .collect();
    for collection in collections {
        if !collection.within.is_empty() {
            // An item of a collection inside an item: its label, its fields,
            // `open`, and the verbs declared on it (`item = slides.elements`).
            let outer: Vec<&str> = collection.within.iter().map(|(field, _)| *field).collect();
            let verbs: Vec<(&ItemAction, &ActionDecl<R>)> = actions
                .iter()
                .filter(|action| {
                    action.collection == Some(collection.field) && action.within == outer.as_slice()
                })
                .filter_map(|action| {
                    declarations
                        .iter()
                        .find(|decl| decl.id == action.action)
                        .map(|decl| (action, decl))
                })
                .collect();
            if collection.labelled || !verbs.is_empty() {
                for (id, label) in &collection.items {
                    each(collection, id, label, &|| {
                        let mut child =
                            nested_item::<R>(&collection.within, collection.field, id, label);
                        for (action, decl) in &verbs {
                            let mut values: Vec<(String, serde_json::Value)> = action
                                .within_params
                                .iter()
                                .zip(&collection.within)
                                .map(|(param, (_, outer_id))| {
                                    ((*param).to_owned(), outer_id.as_str().into())
                                })
                                .collect();
                            values.push((action.param.to_owned(), id.as_str().into()));
                            child = child.with_action(bound(decl, &values));
                        }
                        child
                    });
                }
            }
            continue;
        }
        let scoped = scoped::<R>(&declarations, actions, &fields, collection.field);
        let offers_effects = collection
            .items
            .first()
            .is_some_and(|(id, _)| !R::item_effects(collection.field, id).is_empty());
        if scoped.is_empty() && !offers_effects && !collection.labelled {
            continue;
        }
        for (id, label) in &collection.items {
            each(collection, id, label, &|| {
                item_resource(&scoped, collection.field, id, label)
            });
        }
        // Forget items that are gone.
        BUILT.with(|built| {
            if let Some(items) = built
                .borrow_mut()
                .get_mut(&(TypeId::of::<R>(), collection.field.to_owned()))
                && items.len() > collection.items.len().saturating_mul(2).max(ITEM_PAGE)
            {
                let present: std::collections::HashSet<&Id> =
                    collection.items.iter().map(|(id, _)| id).collect();
                items.retain(|id, _| present.contains(id));
            }
        });
    }
}

/// The id of the resource the item `id` of `collection` is published as.
pub fn item_child_id<R: Resource>(collection: &ItemCollection, id: &Id) -> String {
    if collection.within.is_empty() {
        item_resource_id::<R>(collection.field, id)
    } else {
        nested_resource_id::<R>(&collection.within, collection.field, id)
    }
}

/// [`Resource::children`] in the parts a publisher can keep between
/// versions ([`Resource::children_parts`]): the children that are not
/// items, then one per item of `items` carrying `item_actions`
/// ([`item_children`]).
pub struct ChildrenParts {
    /// The children before the items, built afresh.
    pub others: Vec<tilo_actions::Resource>,
    /// The record's collections, each item with its label.
    pub items: Vec<ItemCollection>,
    /// The actions offered on items.
    pub item_actions: Vec<ItemAction>,
}

/// The resource of an item of a nested collection.
fn nested_item<R: Resource>(
    within: &[(&'static str, Id)],
    field: &'static str,
    id: &Id,
    label: &str,
) -> tilo_actions::Resource {
    let mut child = tilo_actions::Resource::new(
        nested_resource_id::<R>(within, field, id),
        format!("{}.{field}", R::KIND),
        label,
    );
    let mut path: Vec<crate::Segment> = Vec::new();
    for (outer, outer_id) in within {
        path.push(crate::Segment::Field((*outer).to_owned()));
        path.push(crate::Segment::Item(outer_id.clone()));
    }
    path.push(crate::Segment::Field(field.to_owned()));
    path.push(crate::Segment::Item(id.clone()));
    if let Some(spec) = crate::resource::field_edit_spec_at::<R>(path) {
        child = child.with_action(spec);
    }
    child
}

/// [`Resource::children_offer_bound`] of a record whose children are
/// `others` followed by its [`item_children`]: the declarations offered
/// bound on `others` or on the items of a collection, known from how many
/// items each collection holds (`sizes`, by field) without building a child
/// per item.
pub fn offered_on_items<R: Resource>(
    others: &[tilo_actions::Resource],
    sizes: &[(&'static str, usize)],
    actions: &[ItemAction],
) -> Option<Vec<&'static str>> {
    // Verbs on items of a nested collection are known only from the items.
    if actions.iter().any(|action| !action.within.is_empty()) {
        return None;
    }
    let fields: Vec<&str> = sizes.iter().map(|(field, _)| *field).collect();
    let declarations = R::declarations();
    // `item_children` binds the first declaration of an action's id.
    let first = |id: &str| declarations.iter().find(|decl| decl.id == id);
    declarations
        .iter()
        .filter(|decl| {
            let on_items = actions.iter().any(|action| {
                action.action == decl.id
                    && first(action.action).is_some_and(|bound| {
                        // What `offered_bound` asks of the item's spec: it
                        // takes fewer parameters than the declaration.
                        let kept = bound
                            .params
                            .iter()
                            .filter(|param| param.name != action.param)
                            .count();
                        kept < decl.params.len()
                    })
                    && sizes
                        .iter()
                        .any(|&(field, size)| size > 0 && targets(action, &fields, field))
            });
            on_items || offered_bound(others, decl)
        })
        .map(|decl| decl.id)
        .collect::<Vec<_>>()
        .into()
}

/// The spec of `decl` with its item parameter bound to `item`.
fn item_spec<R: Resource>(decl: &ActionDecl<R>, param: &'static str, item: &Id) -> ActionSpec {
    bound(
        decl,
        &[(
            param.to_owned(),
            serde_json::Value::String(item.as_str().to_owned()),
        )],
    )
}

/// `decl` offered on the item it acts on, with `values` bound to their
/// parameters: what a published child carries ("remove *this* photograph",
/// "rename *this* album"). The other parameters stay typed; the action is
/// built, checked and undone exactly as the declaration is. Published on a
/// child, the declaration stays on the resource for scripts that name the
/// item, out of the palette's way.
#[inline(never)]
pub fn bound<R: Resource, N: AsRef<str>>(
    decl: &ActionDecl<R>,
    values: &[(N, serde_json::Value)],
) -> ActionSpec {
    let values: Vec<(String, serde_json::Value)> = values
        .iter()
        .map(|(name, value)| (name.as_ref().to_owned(), value.clone()))
        .collect();
    let params: Vec<ActionParam> = decl
        .params
        .iter()
        .filter(|candidate| !values.iter().any(|(name, _)| candidate.name == *name))
        .cloned()
        .collect();
    let all = decl.params.clone();
    let build = Arc::clone(&decl.build);
    let mut spec = ActionSpec::typed(
        decl.id,
        decl.label.clone(),
        decl.label.clone(),
        ExecutionLocus::ClientLocal,
        params,
        move |args| {
            let mut arguments = args.to_json();
            for (name, value) in &values {
                // An optional parameter bound to nothing is left out.
                if !value.is_null() {
                    arguments.insert(name.clone(), value.clone());
                }
            }
            let action = ActionArgs::bind_json(&all, &arguments)
                .map_err(|error| error.to_string())
                .and_then(|args| build(&args, crate::resource::origin_now_ms()));
            crate::resource::encode_built::<R>(action)
        },
    )
    .of_kind(decl.kind);
    if decl.kind == crate::ActionKind::Undoable {
        spec = spec.reversible();
    }
    if let Some(field) = decl.field {
        spec = spec.below_field(field);
    }
    if let Some(description) = &decl.description {
        spec = spec.with_description(description.clone());
    }
    if decl.destructive {
        spec = spec.destructive();
    }
    // An item verb that answers answers on its item too.
    if let Some(result) = &decl.result {
        spec.contract.result = result.clone();
    }
    spec
}

/// Whether some resource in `children`, at any depth, offers `decl` with
/// arguments bound ([`bound`]).
#[inline(never)]
pub fn offered_bound<R: Resource>(
    children: &[tilo_actions::Resource],
    decl: &ActionDecl<R>,
) -> bool {
    children.iter().any(|child| {
        child
            .actions
            .iter()
            .any(|spec| spec.id == decl.id && spec.params().len() < decl.params.len())
            || offered_bound(&child.children, decl)
    })
}
