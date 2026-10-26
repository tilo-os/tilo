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

use crate::group::{ActionGroup, group_type};
use crate::resource::{Resource, publish_parts};

/// The id a child resource named `name` of `R` is published under:
/// `<kind>/<name>`, beside the item resources of `R`'s collections.
pub fn child_resource_id<R: Resource>(name: &str) -> String {
    format!("{}/{name}", R::KIND)
}

/// The child resource `C` at `id`, inside a resource `R`, for
/// [`Resource::children`]: `C`'s whole published tree (its declared actions,
/// [`APPLY`](crate::resource::APPLY), [`GROUP`](crate::GROUP), and given its `state` its views and
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
    let resource = {
        let _phase = crate::profiling::Phase::start("child.publish_parts");
        publish_parts::<C>(id, label, state)
    };
    let _phase = crate::profiling::Phase::start("child.rewrap");
    rewrap::<C, R>(resource, &wrap)
}

fn rewrap<C: Resource, R: Resource>(
    mut resource: tilo_actions::Resource,
    wrap: &Arc<dyn Fn(C::Action) -> R::Action + Send + Sync>,
) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .map(|spec| {
            let wrap = Arc::clone(wrap);
            spec.rewrapped(R::ACTION_TYPE, move |encoded| {
                wrap_encoded::<C, R>(&encoded, wrap.as_ref())
            })
            .applied_as(R::KIND, R::LABEL)
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
        tilo_actions::decode_json::<C::Action>(&encoded.payload)
            .map_err(|error| error.to_string())
            .and_then(|action| {
                crate::resource::encode::<R>(&wrap(action)).map_err(|error| error.to_string())
            })
    } else if encoded.type_id == group_type::<C>() {
        tilo_actions::decode_payload::<ActionGroup<C::Action>>(&encoded.type_id, &encoded.payload)
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

/// A field that holds a resource of its own type (`#[model(resource)]`): a
/// note's rich-text body. `#[resource]` publishes it beneath the resource
/// of the record that holds it — the item resource of an item of a
/// collection (`<kind>/pages/<page>/body`), or the record itself
/// (`<kind>/body`) — with every document action of the field's type applied
/// as the record's field edit at the field's path. So the field is
/// addressed like any resource, with its own named actions, and nothing
/// about it is repeated on the record: no copy of its verbs taking an item
/// id, no second write path. The record's history, undo, permissions and
/// replay are the ones that apply.
#[derive(Clone, Copy)]
pub struct NestedResource {
    /// The field type's [`Resource::KIND`].
    pub kind: &'static str,
    publish: fn(&str, &str) -> tilo_actions::Resource,
    ops: fn(&EncodedAction) -> Result<Vec<serde_json::Value>, String>,
}

impl std::fmt::Debug for NestedResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NestedResource")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl NestedResource {
    /// The nested resource of a field of type `C`, whose own operations are
    /// its actions: what `#[model(resource)]` records in the field's
    /// [`FieldShape`](crate::FieldShape).
    #[must_use]
    pub const fn of<C>() -> Self
    where
        C: Resource + crate::Operations<Op = <C as Resource>::Action>,
    {
        Self {
            kind: C::KIND,
            publish: publish_nested::<C>,
            ops: nested_ops::<C>,
        }
    }
}

/// `C`'s published tree at `id`, labelled `label`, its actions still `C`'s.
fn publish_nested<C: Resource>(id: &str, label: &str) -> tilo_actions::Resource {
    publish_parts::<C>(id, label, None)
}

/// The operations an encoded action (or group) of `C` carries, as the JSON a
/// field edit of a `C` field takes ([`crate::EditOp::Rich`]).
fn nested_ops<C>(encoded: &EncodedAction) -> Result<Vec<serde_json::Value>, String>
where
    C: Resource + crate::Operations<Op = <C as Resource>::Action>,
{
    let actions: Vec<C::Action> = if encoded.type_id == C::ACTION_TYPE {
        vec![
            tilo_actions::decode_json::<C::Action>(&encoded.payload)
                .map_err(|error| error.to_string())?,
        ]
    } else if encoded.type_id == group_type::<C>() {
        tilo_actions::decode_payload::<ActionGroup<C::Action>>(&encoded.type_id, &encoded.payload)
            .map_err(|error| error.to_string())?
            .actions
    } else {
        return Err(format!(
            "`{}` is not an action of {}",
            encoded.type_id,
            C::LABEL
        ));
    };
    let mut ops = Vec::with_capacity(actions.len());
    for action in &actions {
        ops.push(crate::__private::json_via_bytes(action).map_err(|error| error.to_string())?);
    }
    Ok(ops)
}

/// The id of the resource the field `key` of the record at `parent` (a
/// published resource id) is published as: `<parent>/<key>`.
#[must_use]
pub fn nested_resource_id(parent: &str, key: &str) -> String {
    format!("{parent}/{key}")
}

/// The fields of the record shaped `shape` that hold resources of their own,
/// each published beneath `parent` (the record's published id) with its
/// actions applied as field edits of the resource `vocabulary` at `prefix`
/// (the record's path) followed by the field's key.
///
/// A field of an item is labelled with its item's (`item`): "Body of
/// Review v1". Every note's body would otherwise read "Body", and an agent
/// choosing where to write, shown only the labels, could not tell one
/// note's body from another's.
pub(crate) fn nested_children(
    vocabulary: &'static crate::Vocabulary,
    shape: crate::Shape,
    parent: &str,
    prefix: &[crate::Segment],
    item: Option<&str>,
) -> Vec<tilo_actions::Resource> {
    let mut children = Vec::new();
    for field in shape.fields() {
        let Some(nested) = field.resource else {
            continue;
        };
        let mut path = prefix.to_vec();
        path.push(crate::Segment::Field(field.key.to_owned()));
        let id = nested_resource_id(parent, field.key);
        let label = item.filter(|label| !label.trim().is_empty()).map_or_else(
            || field.title.to_owned(),
            |item| format!("{} of {item}", field.title),
        );
        let resource = (nested.publish)(&id, &label);
        children.push(as_field_edits(
            resource,
            vocabulary,
            nested.ops,
            &Arc::new(path),
        ));
    }
    children
}

/// The children `nested_children` publishes for the record itself
/// (`#[model(resource)]` fields of `R`, not of its items).
#[must_use]
pub fn record_nested_children<R: Resource>() -> Vec<tilo_actions::Resource> {
    nested_children_of(crate::vocabulary::<R>())
}

/// [`record_nested_children`] of the record whose vocabulary is `vocabulary`.
pub(crate) fn nested_children_of(
    vocabulary: &'static crate::Vocabulary,
) -> Vec<tilo_actions::Resource> {
    match (vocabulary.shape)() {
        Some(shape) => nested_children(vocabulary, shape, vocabulary.kind, &[], None),
        None => Vec::new(),
    }
}

type NestedOps = fn(&EncodedAction) -> Result<Vec<serde_json::Value>, String>;

/// `resource` with every document action re-encoded as the field edit of
/// the resource `vocabulary` at `path` carrying the operations it carried.
fn as_field_edits(
    mut resource: tilo_actions::Resource,
    vocabulary: &'static crate::Vocabulary,
    ops: NestedOps,
    path: &Arc<Vec<crate::Segment>>,
) -> tilo_actions::Resource {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .map(|spec| {
            let path = Arc::clone(path);
            spec.rewrapped(vocabulary.action_type, move |encoded| {
                field_edits(vocabulary, ops, &path, &encoded)
            })
            .applied_as(vocabulary.kind, vocabulary.label)
        })
        .collect();
    resource.children = std::mem::take(&mut resource.children)
        .into_iter()
        .map(|child| as_field_edits(child, vocabulary, ops, path))
        .collect();
    resource
}

/// The field edits at `path` carrying the operations of `encoded`: one edit,
/// or a group of them that is still one step. Refused with the reason when
/// `encoded` is no action of the field's type.
fn field_edits(
    vocabulary: &'static crate::Vocabulary,
    ops: NestedOps,
    path: &[crate::Segment],
    encoded: &EncodedAction,
) -> EncodedAction {
    let built = ops(encoded).and_then(|ops| {
        let now_ms = crate::resource::origin_now_ms();
        let mut members = Vec::with_capacity(ops.len());
        for rich in ops {
            let edit = crate::Edit::<()>::new(path.to_vec(), crate::EditOp::Rich { rich });
            members.push(crate::resource::stamped_edit(vocabulary, edit, now_ms)?);
        }
        Ok(members)
    });
    match built {
        Ok(members) => crate::resource::encode_group(vocabulary, &members),
        Err(reason) => crate::resource::encode_built(Err(reason)),
    }
}
