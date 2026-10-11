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
//! Which collection an action acts on is inferred from its parameter name
//! (`item` → `items`, `card` → `cards`, or the record's only collection) and
//! can be named explicitly with `#[action(item = "field")]`.

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
    item_resource_id_in(R::KIND, field, item)
}

/// [`item_resource_id`] of an item of a resource of kind `kind`: compiled
/// once in the shared runtime rather than once per resource type in every
/// app.
pub fn item_resource_id_in(kind: &str, field: &str, item: &Id) -> String {
    format!("{kind}/{field}/{item}")
}

/// The item an item resource id names, if it is one of `R`'s.
pub fn item_of_resource<R: Resource>(resource_id: &str) -> Option<(&str, Id)> {
    let rest = resource_id.strip_prefix(R::KIND)?.strip_prefix('/')?;
    let (field, item) = rest.split_once('/')?;
    Some((field, Id::from_raw(item)))
}

/// Whether `action` acts on the items of the collection `field`, among the
/// record's collection fields `fields`.
fn targets(action: &ItemAction, fields: &[&str], field: &str) -> bool {
    if let Some(named) = action.collection {
        return named == field;
    }
    let param = action.param;
    let named_after = |candidate: &str| {
        candidate == param
            || candidate.strip_suffix('s') == Some(param)
            || candidate.strip_suffix("es") == Some(param)
    };
    if fields.iter().any(|candidate| named_after(candidate)) {
        named_after(field)
    } else {
        fields.len() == 1
    }
}

/// The item resources of a record: one per item of each collection, carrying
/// the item actions that act on that collection with the item bound.
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

/// What [`for_each_item_child`] calls per item child: its collection field,
/// id and label, and how to build it.
pub type ItemChildVisitor<'a> =
    dyn FnMut(&'static str, &Id, &str, &dyn Fn() -> tilo_actions::Resource) + 'a;

/// [`item_children`] one item at a time, for a publisher that keeps the
/// items it built before: `each(field, id, label, build)` is called for
/// every item child in order, and `build()` makes that child exactly as
/// [`item_children`] does. A child is a function of its collection field,
/// its id and its label (and the locale its actions' labels are in).
pub fn for_each_item_child<R: Resource>(
    collections: &[ItemCollection],
    actions: &[ItemAction],
    each: &mut ItemChildVisitor<'_>,
) {
    let declarations = R::declarations();
    let fields: Vec<&str> = collections
        .iter()
        .map(|collection| collection.field)
        .collect();
    for collection in collections {
        let scoped: Vec<(&ItemAction, &ActionDecl<R>)> = actions
            .iter()
            .filter(|action| targets(action, &fields, collection.field))
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
            each(collection.field, id, label, &|| {
                let mut child = tilo_actions::Resource::new(
                    item_resource_id::<R>(collection.field, id),
                    &kind,
                    label,
                );
                for (action, decl) in &scoped {
                    child = child.with_action(item_spec(decl, action.param, id));
                }
                child
            });
        }
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

/// [`Resource::children_offer_bound`] of a record whose children are
/// `others` followed by its [`item_children`]: the declarations offered
/// bound on `others` or on the items of a collection, known from how many
/// items each collection holds (`sizes`, by field) without building a child
/// per item.
pub fn offered_on_items<R: Resource>(
    others: &[tilo_actions::Resource],
    sizes: &[(&'static str, usize)],
    actions: &[ItemAction],
) -> Vec<&'static str> {
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
        .collect()
}

/// The spec of `decl` with its item parameter bound to `item`.
fn item_spec<R: Resource>(decl: &ActionDecl<R>, param: &'static str, item: &Id) -> ActionSpec {
    bound(
        decl,
        &[(param, serde_json::Value::String(item.as_str().to_owned()))],
    )
}

/// `decl` offered on the item it acts on, with `values` bound to their
/// parameters: what a published child carries ("remove *this* photograph",
/// "rename *this* album"). The other parameters stay typed; the action is
/// built, checked and undone exactly as the declaration is. Published on a
/// child, the declaration stays on the resource for scripts that name the
/// item, out of the palette's way.
pub fn bound<R: Resource>(
    decl: &ActionDecl<R>,
    values: &[(&'static str, serde_json::Value)],
) -> ActionSpec {
    let params: Vec<ActionParam> = decl
        .params
        .iter()
        .filter(|candidate| !values.iter().any(|(name, _)| candidate.name == *name))
        .cloned()
        .collect();
    let all = decl.params.clone();
    let build = Arc::clone(&decl.build);
    let values: Vec<(&'static str, serde_json::Value)> = values.to_vec();
    let mut spec = ActionSpec::typed(
        decl.id,
        decl.label.clone(),
        decl.label.clone(),
        ExecutionLocus::ClientLocal,
        params,
        move |args| {
            let mut arguments = args.to_json();
            for (name, value) in &values {
                arguments.insert((*name).to_owned(), value.clone());
            }
            let action = ActionArgs::bind_json(&all, &arguments)
                .map_err(|error| error.to_string())
                .and_then(|args| build(&args, crate::resource::origin_now_ms()));
            crate::resource::encode_built::<R>(action)
        },
    )
    .requiring(decl.permission)
    .reversible();
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
