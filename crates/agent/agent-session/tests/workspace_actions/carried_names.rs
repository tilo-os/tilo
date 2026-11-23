//! What a caller's listing says an action that carries others takes, beside what the same
//! caller's call of it is then answered.
//!
//! A typed edit (`apply`) and a group of edits are offered to whoever may make the least of
//! a resource's edits, and each call is decided by the edit it carries. Their parameter says
//! which edits they carry, by name. So a caller reads there what they may write: a name the
//! same caller's call is refused for as not theirs costs an agent a call it was told to make,
//! and a person's surface a control that does nothing. Every app publishes its edits this way,
//! so this holds for every app, every role, a person and their agent.

use super::super::*;
use super::{Membership, ended, invoked, json, listed};
use tilo_actions::remote::Caller;
use ui::permissions::Role;

const CHAT: &str = "so.tilo.chat";
/// Chat's verb for a new channel, which stands below its field `moderation`.
const NEW_CHANNEL: &str = "create-channel";

/// A document of `app` begun on the page and written to the workspace; `None` for an app that
/// begins no document of its own here.
fn begun_document(browser: &Browser, app: &str) -> Option<DocumentRoute> {
    let mut page = crate::common::open_in(
        &browser.cluster_id,
        app,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .ok()?;
    let route = DocumentRoute {
        app_id: app.into(),
        cluster_id: browser.cluster_id.clone(),
        document: page.wire_document().to_owned(),
    };
    let mutation = page.take_mutation().ok()??;
    browser.peer.write(route.clone(), &mutation).unwrap();
    Some(route)
}

/// Every id `schema` names as an `action`: the members of an enum of them, and a constant.
fn named_in(schema: &serde_json::Value, ids: &mut std::collections::BTreeSet<String>) {
    match schema {
        serde_json::Value::Object(object) => {
            if let Some(action) = object
                .get("properties")
                .and_then(|properties| properties.get("action"))
            {
                let listed = action.get("enum").and_then(serde_json::Value::as_array);
                ids.extend(
                    listed
                        .into_iter()
                        .flatten()
                        .chain(action.get("const"))
                        .filter_map(|id| id.as_str().map(str::to_owned)),
                );
            }
            for value in object.values() {
                named_in(value, ids);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                named_in(value, ids);
            }
        }
        _ => {}
    }
}

/// The ids of the actions `carrier`'s parameters say it carries; none for an action that
/// carries nothing.
fn names(carrier: &tilo_actions::ActionDescriptor) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    for schema in carrier
        .params
        .iter()
        .filter_map(|param| param.schema.as_deref())
    {
        named_in(schema, &mut ids);
    }
    ids
}

/// The value that names the action `id` with `args`, written as `schema` takes it; `None`
/// where `schema` names no such action in one object.
fn naming(
    schema: &serde_json::Value,
    id: &str,
    args: &serde_json::Value,
) -> Option<serde_json::Value> {
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    if let Some(properties) = properties {
        if let Some(action) = properties.get("action") {
            let listed = action
                .get("enum")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|listed| listed == id));
            if listed || action.get("const").and_then(serde_json::Value::as_str) == Some(id) {
                if properties.contains_key("args") {
                    return Some(serde_json::json!({"action": id, "args": args}));
                }
                let mut flat = args.as_object().cloned().unwrap_or_default();
                flat.insert("action".to_owned(), id.into());
                return Some(serde_json::Value::Object(flat));
            }
        }
        for (key, inner) in properties {
            if let Some(value) = naming(inner, id, args) {
                return Some(serde_json::json!({ key: value }));
            }
        }
    }
    ["oneOf", "anyOf"]
        .into_iter()
        .filter_map(|key| schema.get(key).and_then(serde_json::Value::as_array))
        .flatten()
        .find_map(|alternative| naming(alternative, id, args))
}

/// The action of `listing` on `resource` that `id` names.
fn own<'a>(
    listing: &'a [tilo_actions::ActionDescriptor],
    resource: &tilo_actions::ActionDescriptor,
    id: &str,
) -> Option<&'a tilo_actions::ActionDescriptor> {
    listing.iter().find(|action| {
        action.address.resource == resource.address.resource && action.address.action == id
    })
}

/// Whether `action` is listed as one its caller may not take, rather than as one that cannot
/// be done now.
fn refused_by_grant(action: &tilo_actions::ActionDescriptor) -> bool {
    matches!(
        &action.contract.availability,
        tilo_actions::Availability::Unavailable { code, .. }
            if code == tilo_actions::Availability::NOT_ALLOWED
    )
}

/// A caller is named, in what a carrier takes, no edit their own call of it through that
/// carrier is then refused as not theirs.
///
/// By calls first, in Chat, where a new channel is a moderator's: an editor's new channel
/// through the typed edit, and their agent's, is refused as not theirs (the grant's rule,
/// which this test holds as it is). Then for every app the workspace can hold, each role, the
/// person and their agent: every action a listed carrier names that the app publishes by
/// its own name is listed to that same caller by its own name on the same resource, and not
/// as not allowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_carrier_names_no_edit_its_caller_is_then_refused() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let server = start(&root, &device, state.path()).await;
    let browser = Browser::open(&server, &browser_device, &device).await;
    let mut providers = Vec::new();
    let mut begun: Vec<(String, DocumentRoute)> = Vec::new();
    let mut not_begun = Vec::new();
    for definition in crate::common::catalogue().iter() {
        let app = definition.app_id();
        match begun_document(&browser, app) {
            Some(route) => {
                providers.push((
                    app.to_owned(),
                    format!(
                        "{}:{app}:{}",
                        tilo_actions::remote::RESOURCE_PROVIDER,
                        route.document
                    ),
                ));
                begun.push((app.to_owned(), route));
            }
            None => not_begun.push(app.to_owned()),
        }
    }
    // Each app's document is answered for by the workspace: asked one at a
    // time, so one that is never answered names its app rather than hanging.
    for (app, route) in &begun {
        let saved = browser
            .peer
            .barrier_for(std::slice::from_ref(route))
            .unwrap();
        tilo_test_env::within(format!("{app}'s document to be saved"), saved)
            .await
            .unwrap_or_else(|_| panic!("{app}'s document was never answered"))
            .unwrap();
    }
    let membership = Membership::of(
        &server,
        &device,
        &guest_device,
        state.path().join("member-content"),
    );
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(membership.clone()).unwrap();
    let workspace = browser.workspace;
    let member = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let agent = super::their_agent(&member, "Claude Code");

    // Chat, by calls. The typed edit is found in a moderator's listing, with the value that
    // names a new channel written as its parameter's schema takes it; an editor's listing names
    // no channel to make, as a new channel is a moderator's.
    let Some((_, chat)) = providers.iter().find(|(app, _)| app == CHAT) else {
        panic!("Chat is begun; not begun: {not_begun:?}")
    };
    let chat = chat.clone();
    let new_channel = |listing: &[tilo_actions::ActionDescriptor], name: &str| {
        listing.iter().find_map(|carrier| {
            let args = serde_json::json!({ "name": name });
            carrier.params.iter().find_map(|param| {
                let value = naming(param.schema.as_deref()?, NEW_CHANNEL, &args)?;
                Some(request(
                    &chat,
                    carrier.address.clone(),
                    json(serde_json::json!({ param.name.as_ref(): value })),
                ))
            })
        })
    };
    membership.set_role(Some(Role::Moderator));
    let by_moderator = listed(&actions, workspace, &member, &chat);
    let asked =
        new_channel(&by_moderator, "launch").expect("a moderator's typed edit names a new channel");
    membership.set_role(Some(Role::Editor));
    for (caller, who) in [(&member, "an editor"), (&agent, "an editor's agent")] {
        // A call naming one anyway is refused before anything is applied.
        assert!(
            matches!(
                invoked(&actions, workspace, caller, &asked),
                ReceiptStatus::Rejected { .. }
            ),
            "{who} makes no channel through the typed edit: a new channel is a moderator's"
        );
        let offered = listed(&actions, workspace, caller, &chat);
        assert!(
            new_channel(&offered, "launch").is_none(),
            "{who} names no channel through the typed edit: a new channel is a moderator's"
        );
    }

    // Every app, every role, the person and their agent, by their own listings.
    let mut refused_names = Vec::new();
    let mut carriers = 0_usize;
    for (app, provider) in &providers {
        // What the app publishes by name, read as someone who may all of it.
        membership.set_role(Some(Role::Owner));
        let published = listed(&actions, workspace, &member, provider);
        for role in [Role::Viewer, Role::Commenter, Role::Editor, Role::Moderator] {
            membership.set_role(Some(role));
            for (caller, who) in [(&member, "the person"), (&agent, "their agent")] {
                let offered = listed(&actions, workspace, caller, provider);
                for carrier in &offered {
                    let ids = names(carrier);
                    if ids.is_empty() || !carrier.contract.availability.is_available() {
                        continue;
                    }
                    carriers += 1;
                    for id in ids {
                        // A typed value no named action restates is nobody's by name.
                        let Some(declared) = own(&published, carrier, &id) else {
                            continue;
                        };
                        // What only people are offered is not an agent's for another reason
                        // than the grant.
                        if caller.acting().is_agent() && !declared.contract.reaches_agents() {
                            continue;
                        }
                        let lacks = match own(&offered, carrier, &id) {
                            // Listed, and theirs: what the app says cannot be done now (no tag to
                            // delete yet) is still theirs to call.
                            Some(action) if !refused_by_grant(action) => continue,
                            Some(_) => "listed as not allowed",
                            None => "not listed",
                        };
                        refused_names.push(format!(
                            "{app}, {role:?}, {who}: {} on {} names {id}, which is {lacks} to them",
                            carrier.address.action, carrier.resource_kind
                        ));
                    }
                }
            }
        }
    }
    assert!(
        carriers > 0,
        "no carrier was listed at all; not begun: {not_begun:?}"
    );
    assert!(
        refused_names.is_empty(),
        "{} names of edits a carrier's caller is then refused, over {carriers} listed carriers \
         of {} apps (not begun: {not_begun:?}):\n{}",
        refused_names.len(),
        providers.len(),
        refused_names.join("\n")
    );

    ended(browser, server).await;
}
