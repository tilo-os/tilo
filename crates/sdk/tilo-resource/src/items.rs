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

use std::sync::Arc;

use tilo_actions::{ActionArgs, ActionParam, ActionSpec, ExecutionLocus};

use crate::collection::Id;
use crate::resource::{ActionDecl, Resource};

/// The items of one collection field, as `#[derive(Model)]` lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCollection {
    /// The field's Rust name.
    pub field: &'static str,
    /// Each item's id and a human label (its first text, else its id).
    pub items: Vec<(Id, String)>,
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

fn targets(action: &ItemAction, collections: &[ItemCollection], field: &str) -> bool {
    if let Some(named) = action.collection {
        return named == field;
    }
    if collections
        .iter()
        .any(|collection| collection.field == action.param)
    {
        action.param == field
    } else {
        collections.len() == 1
    }
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
pub fn item_children<R: Resource>(
    collections: &[ItemCollection],
    actions: &[ItemAction],
) -> Vec<tilo_actions::Resource> {
    let declarations = R::declarations();
    let mut children = Vec::new();
    for collection in collections {
        let scoped: Vec<(&ItemAction, &ActionDecl<R>)> = actions
            .iter()
            .filter(|action| targets(action, collections, collection.field))
            .filter_map(|action| {
                declarations
                    .iter()
                    .find(|decl| decl.id == action.action)
                    .map(|decl| (action, decl))
            })
            .collect();
        if scoped.is_empty() {
            continue;
        }
        let kind = format!("{}.{}", R::KIND, collection.field);
        for (id, label) in &collection.items {
            let mut child = tilo_actions::Resource::new(
                item_resource_id::<R>(collection.field, id),
                &kind,
                label,
            );
            for (action, decl) in &scoped {
                child = child.with_action(item_spec(decl, action.param, id));
            }
            children.push(child);
        }
    }
    children
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
                arguments.insert(name.clone(), value.clone());
            }
            let action = ActionArgs::bind_json(&all, &arguments)
                .map_err(|error| error.to_string())
                .and_then(|args| build(&args, crate::resource::origin_now_ms()));
            crate::resource::encode_built::<R>(action)
        },
    )
    .requiring(decl.permission);
    if decl.kind == crate::ActionKind::Undoable {
        spec = spec.reversible();
    }
    if let Some(description) = &decl.description {
        spec = spec.with_description(description.clone());
    }
    if decl.destructive {
        spec = spec.destructive();
    }
    spec
}

/// Whether some resource in `children`, at any depth, offers `decl` with
/// arguments bound ([`bound`]).
pub(crate) fn offered_bound<R: Resource>(
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
