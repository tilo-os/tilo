//! `share-link` and `share-invite-link`: what a person, a bot or an agent
//! asks for to get "a link to this".
//!
//! Every resource that declares a view publishes both. A resource without a
//! view has nothing a link could open, so it has no link. The specs are bound
//! here, where the views are attached, so every way a resource is published
//! (the resource itself, an item, a live row) offers them the same way; the
//! framework's root answers them. See `docs/architecture/web-routing.md` §4.3.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionKind, ActionParam, ActionSpec, ActionType, EncodedAction, ExecutionMode, OperationKind,
    ParamType, ResultSpec, Target,
};

/// The link to the resource as a URL.
pub const SHARE_LINK: &str = "share-link";
/// The link with an invitation for someone not yet in the room.
pub const SHARE_INVITE_LINK: &str = "share-invite-link";
/// `share-link` on `resource`, which `provider` publishes: how one app asks
/// another app's resource for its link without knowing its views.
#[must_use]
pub fn share_link_target(
    provider: impl Into<String>,
    resource: impl Into<String>,
) -> tilo_actions::remote::RemoteActionTarget {
    tilo_actions::remote::RemoteActionTarget {
        provider: provider.into(),
        address: tilo_actions::ActionAddress::new(resource, SHARE_LINK),
    }
}

/// `share-link` as the published resource `resource` offers it: what a
/// caller holding the resource's id invokes for its link.
#[must_use]
pub fn share_link_address(resource: impl Into<String>) -> tilo_actions::ActionAddress {
    tilo_actions::ActionAddress::new(resource, SHARE_LINK)
}

/// The operation type the framework answers `share-link` with.
pub const SHARE_TYPE: &str = "tilo.resource.share-link";
/// The operation type the framework answers `share-invite-link` with.
pub const SHARE_INVITE_TYPE: &str = "tilo.resource.share-invite-link";

/// One declared view, as the link names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SharedView {
    pub id: String,
    pub primary: bool,
    /// Whether the view opens at a range of the resource's text.
    pub range: bool,
}

/// What a `share-link` invocation asks for: the resource it was published
/// on, its views as they were published, and the caller's arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShareRequest {
    pub resource: String,
    pub kind: String,
    pub views: Vec<SharedView>,
    pub invite: bool,
    pub args: serde_json::Map<String, serde_json::Value>,
}

/// Offer `share-link` and `share-invite-link` on `resource` when it
/// declares a view and does not offer them already.
pub fn offer(resource: &mut tilo_actions::Resource) {
    if resource.views.is_empty()
        || resource
            .actions
            .iter()
            .any(|spec| spec.id == SHARE_LINK || spec.id == SHARE_INVITE_LINK)
    {
        return;
    }
    let views: Vec<SharedView> = resource
        .views
        .iter()
        .map(|view| SharedView {
            id: view.id.clone(),
            primary: view.primary,
            range: view.range.is_some(),
        })
        .collect();
    let target = Target::new(resource.id.as_str().to_owned());
    for invite in [false, true] {
        let ty = share_type(invite, &resource.kind, &views);
        resource.actions.push(ty.bind(target.clone()));
    }
}

/// Which link type: whether it invites, the kind of resource, its views.
type ShareKey = (bool, String, Vec<SharedView>);

/// The type of the link to resources of kind `kind` with `views`, kept once
/// for every resource that offers it; its target is the resource's id.
fn share_type(invite: bool, kind: &str, views: &[SharedView]) -> Arc<ActionType> {
    thread_local! {
        static KEPT: std::cell::RefCell<crate::kept::KeptTypes<ShareKey>> =
            std::cell::RefCell::default();
    }
    let key = (invite, kind.to_owned(), views.to_vec());
    crate::kept::kept_type(&KEPT, key, || {
        Arc::clone(spec(invite, kind.to_owned(), views.to_vec()).action_type())
    })
}

/// [`offer`] on `resource` and everything nested in it.
pub fn offer_tree(resource: &mut tilo_actions::Resource) {
    offer(resource);
    for child in &mut resource.children {
        offer_tree(child);
    }
}

/// Offer the link actions again after `resource` gained views.
pub fn reoffer(resource: &mut tilo_actions::Resource) {
    resource
        .actions
        .retain(|spec| spec.id != SHARE_LINK && spec.id != SHARE_INVITE_LINK);
    offer(resource);
}

/// The parameters of the link to a resource with `views`; an invitation's
/// also say what it admits.
fn params(invite: bool, views: &[SharedView]) -> Vec<ActionParam> {
    let ranged = views.iter().any(|view| view.range);
    let mut params = vec![
        ActionParam::optional(
            "view",
            ParamType::Text,
            "Which declared view the link opens (default: the primary view)",
        )
        .with_choices(views.iter().map(|view| view.id.clone()))
        .nullable(),
    ];
    if ranged {
        params.extend([
            ActionParam::optional(
                "start",
                ParamType::Integer,
                "Where the linked range of the text starts",
            )
            .nullable(),
            ActionParam::optional(
                "end",
                ParamType::Integer,
                "Where the linked range of the text ends",
            )
            .nullable(),
            ActionParam::optional(
                "unit",
                ParamType::Text,
                "What start and end count (default: chars)",
            )
            .with_choices(["chars", "bytes"])
            .nullable(),
        ]);
    }
    params.push(
        ActionParam::optional(
            "mode",
            ParamType::Text,
            "app opens it alone in a full browser tab, desktop on the workspace's desktop (default: app)",
        )
        .with_choices(["app", "desktop"])
        .nullable(),
    );
    if invite {
        params.extend([
            ActionParam::optional(
                "role",
                ParamType::Text,
                "The role the invitation carries (default: viewer)",
            )
            .with_choices(["viewer", "commenter", "editor"])
            .nullable(),
            ActionParam::optional(
                "expires_in",
                ParamType::Integer,
                "Seconds until the invitation can no longer be redeemed",
            )
            .nullable(),
        ]);
    }
    params
}

/// The link spec of resources of kind `kind` with `views`, whose target is
/// the resource's id.
fn spec(invite: bool, kind: String, views: Vec<SharedView>) -> ActionSpec {
    let params = params(invite, &views);
    let (id, label, type_id, action_kind, description) = if invite {
        (
            SHARE_INVITE_LINK,
            "Share invite link",
            SHARE_INVITE_TYPE,
            ActionKind::Outside,
            "A link to this that also invites someone who is not yet in the workspace. The invitation is a capability: send it only to the person it is for.",
        )
    } else {
        (
            SHARE_LINK,
            "Copy link",
            SHARE_TYPE,
            ActionKind::Read,
            "A link to this, as a URL that opens it in a browser or an installed Tilo. Anyone opening it still needs access to the workspace.",
        )
    };
    let mut spec = ActionSpec::typed_operation_on(
        id,
        label,
        label,
        OperationKind::Effect,
        type_id,
        params,
        move |target, args| {
            let request = ShareRequest {
                resource: target.get::<String>().cloned().unwrap_or_default(),
                kind: kind.clone(),
                views: views.clone(),
                invite,
                args: args.to_json(),
            };
            EncodedAction::encode_operation(type_id, &request).unwrap_or_else(|_| EncodedAction {
                type_id: type_id.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(action_kind)
    .with_description(description);
    // Minting waits on the room's host for a fresh locator, which only
    // answers in-process at once; over the session it arrives later.
    let contract = &mut spec.type_mut().contract;
    contract.execution = ExecutionMode::Background { cancellable: true };
    contract.result = ResultSpec::Json {
        schema: serde_json::json!({
            "type": "object",
            "properties": {"url": {"type": "string"}},
            "required": ["url"],
        }),
    };
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(spec: &ActionSpec) -> Vec<String> {
        spec.params()
            .iter()
            .map(|param| param.name.to_string())
            .collect()
    }

    #[test]
    fn a_resource_with_a_view_offers_both_and_range_params_only_with_a_range() {
        let mut plain =
            tilo_actions::Resource::new("notes.page:1", "notes.page", "Page").with_view(
                tilo_actions::ResourceViewSpec::new("page", "Page", "open-page"),
            );
        offer(&mut plain);
        let ids: Vec<_> = plain.actions.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids, [SHARE_LINK, SHARE_INVITE_LINK]);
        assert_eq!(params(&plain.actions[0]), ["view", "mode"]);
        assert_eq!(
            params(&plain.actions[1]),
            ["view", "mode", "role", "expires_in"]
        );

        let mut ranged = tilo_actions::Resource::new("notes.page:2", "notes.page", "Page")
            .with_view(
                tilo_actions::ResourceViewSpec::new("page", "Page", "open-page")
                    .with_range(tilo_actions::ResourceViewRangeSpec::new("open-page-range")),
            );
        offer(&mut ranged);
        assert_eq!(
            params(&ranged.actions[0]),
            ["view", "start", "end", "unit", "mode"]
        );
        offer(&mut ranged);
        assert_eq!(ranged.actions.len(), 2, "offered once");

        let mut viewless = tilo_actions::Resource::new("notes.tag:1", "notes.tag", "Tag");
        offer(&mut viewless);
        assert!(viewless.actions.is_empty());
    }
}
