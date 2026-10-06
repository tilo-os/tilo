//! Resources nested inside another: a note's rich-text body, a draft's
//! message, a saved query's SQL.
//!
//! A child is a resource type of its own — it has its own actions, views and
//! search — stored at a place inside its parent's document. Its edits are
//! the parent's: the parent has an action that carries one child action and
//! names where it applies (`EditBody { page, edit }`), so history, undo,
//! permissions and replay stay the parent's, and a collaborator replays the
//! edit exactly as it was made.
//!
//! [`publish_child`] publishes such a child beneath its parent with every
//! document action re-encoded as the parent's, so the palette, agents and
//! scripts address the child like any resource — its own named actions,
//! [`APPLY`](crate::APPLY) and [`GROUP`](crate::GROUP) — and every invocation
//! applies as the parent's action. The framework's child dispatcher submits
//! to the same address.

use std::sync::Arc;

use tilo_actions::EncodedAction;

use crate::group::{ActionGroup, GROUP, group_type};
use crate::resource::{APPLY, Resource, apply_label, group_label, publish_parts};

/// The id a child resource named `name` of `R` is published under:
/// `<kind>/<name>`, beside the item resources of `R`'s collections.
pub fn child_resource_id<R: Resource>(name: &str) -> String {
    format!("{}/{name}", R::KIND)
}

/// The child resource `C` at `id`, inside a resource `R`, for
/// [`Resource::children`]: `C`'s whole published tree (its declared actions,
/// [`APPLY`], [`GROUP`](crate::GROUP), and given its `state` its views and
/// its own children), each document action building `R`'s action
/// `wrap(edit)` instead of `C`'s. A group of `C`'s edits becomes one group
/// of `R`'s, so it is still one step. A parent that does not keep the
/// child's state passes `None`: the child's edits are still all there.
pub fn publish_child<C, R>(
    id: &str,
    label: &str,
    state: Option<&C::State>,
    wrap: impl Fn(C::Action) -> R::Action + Send + Sync + 'static,
) -> tilo_actions::Resource
where
    C: Resource,
    R: Resource,
{
    let wrap: Arc<dyn Fn(C::Action) -> R::Action + Send + Sync> = Arc::new(wrap);
    rewrap::<C, R>(publish_parts::<C>(id, label, state), &wrap)
}

fn rewrap<C: Resource, R: Resource>(
    mut resource: tilo_actions::Resource,
    wrap: &Arc<dyn Fn(C::Action) -> R::Action + Send + Sync>,
) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .map(|spec| {
            let wrap = Arc::clone(wrap);
            let named = spec.id.clone();
            let spec = spec.rewrapped(R::ACTION_TYPE, move |encoded| {
                wrap_encoded::<C, R>(&encoded, wrap.as_ref())
            });
            // A typed edit is named in history after what it did as the
            // parent's action; a named one keeps its own label.
            match named.as_str() {
                APPLY => spec.with_history_label(apply_label::<R>),
                GROUP => spec.with_history_label(group_label::<R>),
                _ => spec,
            }
        })
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| rewrap::<C, R>(child, wrap))
        .collect();
    resource
}

/// `C`'s encoded action (or group) as `R`'s; anything that does not decode
/// as one is refused with the reason.
fn wrap_encoded<C: Resource, R: Resource>(
    encoded: &EncodedAction,
    wrap: &(dyn Fn(C::Action) -> R::Action + Send + Sync),
) -> EncodedAction {
    let wrapped = if encoded.type_id == C::ACTION_TYPE {
        serde_json::from_slice::<C::Action>(&encoded.payload)
            .map_err(|error| error.to_string())
            .and_then(|action| {
                crate::resource::encode::<R>(&wrap(action)).map_err(|error| error.to_string())
            })
    } else if encoded.type_id == group_type::<C>() {
        serde_json::from_slice::<ActionGroup<C::Action>>(&encoded.payload)
            .map_err(|error| error.to_string())
            .and_then(|group| {
                let group = ActionGroup {
                    actions: group.actions.into_iter().map(wrap).collect::<Vec<_>>(),
                };
                EncodedAction::encode(group_type::<R>(), &group).map_err(|error| error.to_string())
            })
    } else {
        Err(format!(
            "`{}` is not an action of {}",
            encoded.type_id,
            C::LABEL
        ))
    };
    wrapped.unwrap_or_else(|reason| EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: reason.into_bytes(),
    })
}
