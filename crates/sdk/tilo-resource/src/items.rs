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
    let param = action.param;
    let named_after = |candidate: &str| {
        candidate == param
            || candidate.strip_suffix('s') == Some(param)
            || candidate.strip_suffix("es") == Some(param)
    };
    if collections
        .iter()
        .any(|collection| named_after(collection.field))
    {
        named_after(field)
    } else {
        collections.len() == 1
    }
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
    let params: Vec<ActionParam> = decl
        .params
        .iter()
        .filter(|candidate| candidate.name != param)
        .cloned()
        .collect();
    let all = decl.params.clone();
    let build = Arc::clone(&decl.build);
    let item = item.as_str().to_owned();
    let mut spec = ActionSpec::typed(
        decl.id,
        decl.label.clone(),
        decl.label.clone(),
        ExecutionLocus::ClientLocal,
        params,
        move |args| {
            let mut arguments = args.to_json();
            arguments.insert(param.to_owned(), serde_json::Value::String(item.clone()));
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
