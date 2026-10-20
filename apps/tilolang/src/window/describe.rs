//! What the platform shows of a package's declarations: a resource's title, the actions offered
//! on it, an enum's case labels.

use std::rc::Rc;

use tilolang_tilo::MetaExt;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{package_resource_id, resource_id};
use tilolang_tilo::runtime::meta::{ActionMeta, Meta, TypeMeta};
use tilolang_tilo::runtime::{ResourceRef, Value};

use super::list::{RowAction, icon_named};
use crate::reconcile::{Case, display};

/// The package that declares the type `ty` (an id), with the type.
#[must_use]
pub fn type_of(host: &Host, ty: &str) -> Option<(Meta, TypeMeta)> {
    host.packages().into_iter().find_map(|package| {
        let declared = package
            .meta
            .types
            .iter()
            .find(|declared| declared.id == ty)?
            .clone();
        Some((package.meta, declared))
    })
}

/// A name in sentence case: `new_note` and `NewNote` read "New note".
#[must_use]
pub fn sentence(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let joined = words.join(" ");
    let mut chars = joined.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// What a type is called in the platform's sentences ("No note selected"): its name, in lower
/// case words.
#[must_use]
pub fn noun(ty: &TypeMeta) -> String {
    sentence(&ty.name).to_lowercase()
}

/// The title of a resource: its first `Text` field marked `title`; else, for a package's root,
/// the package's title; else its type's name.
#[must_use]
pub fn title_of(host: &Host, resource: &ResourceRef) -> String {
    let Some((meta, ty)) = type_of(host, &resource.ty) else {
        return String::new();
    };
    let titled = ty
        .fields
        .iter()
        .find(|field| field.role.as_deref() == Some("title"));
    if let Some(field) = titled
        && let Ok(value) = host.runtime().read(resource, &field.id)
    {
        let text = display(&value);
        if !text.trim().is_empty() {
            return text;
        }
    }
    if resource.path.is_empty() && meta.roots.contains(&ty.id) {
        return meta.title;
    }
    sentence(&ty.name)
}

/// The label of an action: as the compiler derived it, else its name in sentence case.
#[must_use]
pub fn label_of(action: &ActionMeta) -> String {
    if action.label.is_empty() {
        sentence(&action.name)
    } else {
        action.label.clone()
    }
}

/// The actions a person can run on a resource of the type `ty` from a menu: those whose subject
/// is the type and that take nothing else. The primary one first.
#[must_use]
pub fn actions_on(meta: &Meta, ty: &TypeMeta) -> Vec<ActionMeta> {
    let mut actions: Vec<ActionMeta> = meta
        .offered_actions()
        .filter(|action| action.params.len() == 1)
        .filter(|action| {
            meta.subject_of(&action.params)
                .is_some_and(|subject| subject.id == ty.id)
        })
        .cloned()
        .collect();
    actions.sort_by_key(|action| action.role.as_deref() != Some("primary"));
    actions
}

/// The actions offered on `resource`, as its row and its window offer them.
#[must_use]
pub fn offered(host: &Host, resource: &ResourceRef) -> Rc<[RowAction]> {
    let Some((meta, ty)) = type_of(host, &resource.ty) else {
        return Rc::from(Vec::new());
    };
    let published = resource_id(&meta.app, &resource.id_string());
    actions_on(&meta, &ty)
        .iter()
        .map(|action| {
            presented(
                host,
                action,
                resource,
                RowAction {
                    label: label_of(action),
                    icon: action.icon.as_deref().and_then(icon_named),
                    resource: published.clone(),
                    action: action.id.clone(),
                    primary: action.role.as_deref() == Some("primary"),
                    destructive: action.role.as_deref() == Some("destructive"),
                    args: Vec::new(),
                    on: None,
                },
            )
        })
        .collect::<Vec<_>>()
        .into()
}

/// `action` as it presents itself for `subject`: the label, icon and toggled state its
/// presentation view (`label: if n.pinned { "Unpin" } else { "Pin" }`) gives now, over `base`.
fn presented(
    host: &Host,
    action: &ActionMeta,
    subject: &ResourceRef,
    base: RowAction,
) -> RowAction {
    let Some(view) = &action.present else {
        return base;
    };
    let opened = host
        .runtime()
        .open_view(host.viewer(), view, vec![Value::resource(subject)]);
    let Ok(instance) = opened else {
        return base;
    };
    let node = instance.tree();
    let mut shown = base;
    if let Some(Value::Text(label)) = node.props.get("label") {
        shown.label.clone_from(label);
    }
    if let Some(Value::Text(icon)) = node.props.get("icon") {
        shown.icon = icon_named(icon).or(shown.icon);
    }
    shown.on = match node.props.get("on") {
        Some(Value::Bool(on)) => Some(*on),
        Some(Value::Int(on)) => Some(*on != 0),
        _ => None,
    };
    shown
}

/// The actions offered on the item `item` of a list `owner` shows: its own, and the owner's
/// that take nothing but the item (`delete(nb: Notebook, n: Note)`), invoked on the owner with
/// the item.
#[must_use]
pub fn offered_in(host: &Host, item: &ResourceRef, owner: &ResourceRef) -> Rc<[RowAction]> {
    let mut actions: Vec<RowAction> = offered(host, item).to_vec();
    if let Some((meta, owner_ty)) = type_of(host, &owner.ty) {
        let published = resource_id(&meta.app, &owner.id_string());
        for action in meta.offered_actions() {
            let [first, second] = action.params.as_slice() else {
                continue;
            };
            let on_owner = meta
                .type_named(&first.ty)
                .is_some_and(|ty| ty.id == owner_ty.id);
            let takes_item = meta
                .type_named(&second.ty)
                .is_some_and(|ty| ty.id == item.ty);
            if on_owner && takes_item {
                actions.push(RowAction {
                    label: label_of(action),
                    icon: action.icon.as_deref().and_then(icon_named),
                    resource: published.clone(),
                    action: action.id.clone(),
                    primary: false,
                    destructive: action.role.as_deref() == Some("destructive"),
                    args: vec![(second.name.clone(), item.id_string())],
                    on: None,
                });
            }
        }
    }
    actions.into()
}

/// The primary action of a resource, when its type declares one.
#[must_use]
pub fn primary(host: &Host, resource: &ResourceRef) -> Option<RowAction> {
    let (meta, ty) = type_of(host, &resource.ty)?;
    let action = actions_on(&meta, &ty)
        .into_iter()
        .find(|action| action.role.as_deref() == Some("primary"))?;
    let base = RowAction {
        label: label_of(&action),
        icon: action.icon.as_deref().and_then(icon_named),
        resource: resource_id(&meta.app, &resource.id_string()),
        action: action.id.clone(),
        primary: true,
        destructive: false,
        args: Vec::new(),
        on: None,
    };
    Some(presented(host, &action, resource, base))
}

/// The primary action of the package itself (one that takes no resource), when it declares one.
#[must_use]
pub fn package_primary(meta: &Meta) -> Option<RowAction> {
    let action = meta
        .offered_actions()
        .find(|action| action.params.is_empty() && action.role.as_deref() == Some("primary"))?;
    Some(RowAction {
        label: label_of(action),
        icon: action.icon.as_deref().and_then(icon_named),
        resource: package_resource_id(&meta.app),
        action: action.id.clone(),
        primary: true,
        destructive: false,
        args: Vec::new(),
        on: None,
    })
}

/// The cases of the enum `id` with what a control shows for each.
#[must_use]
pub fn cases(host: &Host, id: &str) -> Vec<Case> {
    host.packages()
        .into_iter()
        .find_map(|package| {
            package
                .meta
                .enums
                .iter()
                .find(|declared| declared.id == id || declared.name == id)
                .map(|declared| {
                    declared
                        .variants
                        .iter()
                        .map(|variant| Case {
                            name: variant.name.clone(),
                            label: if variant.label.is_empty() {
                                sentence(&variant.name)
                            } else {
                                variant.label.clone()
                            },
                            icon: variant.icon.as_deref().and_then(icon_named),
                        })
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// The case a value of an enum holds: its variant's name.
#[must_use]
pub fn case_of(value: &Value) -> Option<String> {
    match value {
        Value::Text(name) => Some(name.clone()),
        Value::Enum(value) => Some(value.variant.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_in_sentence_case() {
        assert_eq!(sentence("new_note"), "New note");
        assert_eq!(sentence("NewNote"), "New note");
        assert_eq!(sentence("toggle_pin"), "Toggle pin");
        assert_eq!(sentence("Note"), "Note");
        assert_eq!(sentence(""), "");
    }
}
