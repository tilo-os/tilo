//! A governed workspace's administration as `tilo.workspace` actions: offered as
//! far as the workspace's grants let the caller, run through the same workspace
//! methods as the wire API does, and ending a call held back until everyone's
//! own confirmation can be asked for.

use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, roles, roles::governance};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, CatalogueRow, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_MEMBER_ADDED, WORKSPACE_MEMBER_ADMIT,
    WORKSPACE_MEMBER_PLACED, WORKSPACE_MEMBER_REMOVED, WORKSPACE_MEMBERS,
    workspace_governance_child, workspace_governance_operation, workspace_member,
};
use tilo_peer_state::join::Role;
use tilo_server_protocol::workspaces::{JoinReply, WorkspacesReply, WorkspacesRequest};
use tilo_user_identity::DeviceCert;

use super::{
    ADD_ATTESTOR, ADD_REGISTRY, CANCEL_INVITATION, DISABLE_REGISTRY, ENABLE_REGISTRY,
    GovernedServer, INVITATIONS, MAIL_INVITATION, MEMBERS, NAME, REGISTRIES, REGISTRY_CHANNEL,
    REMOVE_ATTESTOR, REMOVE_REGISTRY, REVOKE_PUBLISHER, REVOKED_PUBLISHERS,
    ServiceWorkspaceActions, TRUSTED_ATTESTORS, UNLIST, VERBS, WAIT_LIST,
};
use crate::app_serving::AppActionServer as _;
use crate::workspaces::tests::{
    governor, guest_device, is_banned, signed_pre_admission, test_host,
};
use crate::workspaces::{PreAdmitRequest, SetMemberRoleRequest, Workspaces};
use tilo_server_protocol::WorkspaceId;

/// What a pending answer comes to, on this runtime.
pub(super) async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the governance answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// A new person admitted to `workspace` as an Editor, through the owner's
/// pre-admission: their device's certificate.
pub(super) async fn admit(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    seed: u8,
) -> DeviceCert {
    admit_device(workspaces, workspace, owner, seed).await.1
}

/// [`admit`], with the admitted device's key.
pub(super) async fn admit_device(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    seed: u8,
) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let (key, cert) = guest_device();
    let reply = workspaces
        .admit_proven(
            issued.signed_invitation,
            cert.clone(),
            rand::random(),
            &format!("Person {seed}"),
            Some(token.as_slice()),
        )
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    (key, cert)
}

async fn grant(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    person: &DeviceCert,
    action: &str,
) {
    let who = hex::encode(person.user_public_key);
    let grant = Grant::new(
        format!("test:{action}:{who}"),
        Grantee::person(who),
        roles::governance_action(workspace, action),
        KindSet::ALL,
        Answer::Allow,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(grant.id.as_str().as_bytes()).as_bytes();
    workspaces
        .put_workspace_grants(workspace, owner, request_id, vec![grant])
        .await
        .unwrap();
}

pub(super) fn caller(device: &DeviceCert) -> Caller {
    Caller::authenticated(device.user_public_key, device.device_public_key)
}

pub(super) fn request(workspace: &str, verb: &str, argument: Value) -> InvokeRequest {
    let Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: match argument.get("person").and_then(Value::as_str) {
            // Removing is published on the person it removes.
            Some(person) if verb == governance::REMOVE_MEMBER => RemoteActionTarget {
                provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    tilo_actions::remote::workspace_member(workspace, person),
                    "remove",
                ),
            },
            // Pinning, unpinning and uninstalling are published on the app they change.
            _ => match (argument.get("app").and_then(Value::as_str), verb) {
                (Some(app), super::UNINSTALL_APP | super::PIN_APP | super::UNPIN_APP) => {
                    RemoteActionTarget {
                        provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                        address: tilo_actions::ActionAddress::new(
                            tilo_actions::remote::workspace_app(workspace, app),
                            match verb {
                                super::UNINSTALL_APP => "uninstall",
                                super::PIN_APP => "pin",
                                _ => "unpin",
                            },
                        ),
                    }
                }
                _ => RemoteActionTarget::workspace_governance(workspace, verb),
            },
        },
        argument: if argument.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(argument)
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A new workspace of `workspaces`, the workspace carrying it, and its governance
/// as actions.
pub(crate) async fn governed(
    workspaces: &Workspaces,
) -> (String, WorkspaceId, ServiceWorkspaceActions) {
    let summary = workspaces.create("Team").await.unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    (summary.workspace, summary.workspace_id, host)
}

/// The verbs `caller` is offered on the workspace `workspace_id` carries.
pub(super) async fn offered(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    caller: &Caller,
) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    let provider = host.in_workspace(workspace_id);
    let page = settled(provider.query(caller, query.clone()))
        .await
        .unwrap();
    // And what the resources under the workspace's root hold, and each of its
    // people, as browsing its tree finds them.
    let mut rows = Vec::new();
    let mut unread = page.rows;
    while let Some(row) = unread.pop() {
        if row.has_children
            && !rows
                .iter()
                .any(|known: &CatalogueRow| known.resource.id == row.resource.id)
        {
            let under = CatalogueQuery {
                parent_resource: Some(row.resource.id.clone()),
                ..query.clone()
            };
            unread.extend(settled(provider.query(caller, under)).await.unwrap().rows);
        }
        rows.push(row);
    }
    // Each by the operation a grant names, whichever child publishes it.
    let mut offered = rows
        .into_iter()
        .filter_map(|row| row.action)
        .map(|action| {
            workspace_governance_operation(&action.address).map_or_else(
                || action.address.action.clone(),
                |(_, granted)| granted.to_owned(),
            )
        })
        .collect::<Vec<_>>();
    offered.sort_by_key(|verb| VERBS.iter().position(|row| row.id == verb));
    offered.dedup();
    offered
}

#[tokio::test]
async fn the_catalogue_root_carries_the_surface_the_workspace_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let (_workspace, workspace_id, host) = governed(server.workspaces()).await;
    let page = settled(host.in_workspace(workspace_id).query(
        &caller(&owner),
        CatalogueQuery {
            provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
            max_results: 100,
            ..CatalogueQuery::default()
        },
    ))
    .await
    .unwrap();
    let root = page
        .rows
        .iter()
        .find(|row| row.parent_resource.is_empty())
        .expect("the workspace's root is catalogued");
    assert_eq!(
        root.surface.as_deref().map(str::as_bytes),
        Some(super::WORKSPACE_SURFACE),
        "the root row serves the surface the workspace's actions publish"
    );
    server.shutdown().await;
}

/// `request` by `caller`: what it produced, or why not.
async fn invoke(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<Value, ActionCallError> {
    let receipt = settled(host.in_workspace(workspace_id).invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

/// Inviting and what is part of it: the invitations made, who is in the workspace, and the
/// addresses left at its entry while it takes people by invitation.
const INVITE_FAMILY: [&str; 7] = [
    governance::INVITE,
    INVITATIONS,
    CANCEL_INVITATION,
    MEMBERS,
    WAIT_LIST,
    UNLIST,
    MAIL_INVITATION,
];

/// Leaving, naming themselves, and reading what their workspace trusts and
/// where it fetches from: what every person is offered in a workspace they are
/// in and no grant gives them.
const OWN: [&str; 5] = [
    governance::REMOVE_MEMBER,
    NAME,
    TRUSTED_ATTESTORS,
    REVOKED_PUBLISHERS,
    REGISTRIES,
];

/// `verbs` in the order the catalogue offers them.
fn offered_in_catalogue(verbs: impl IntoIterator<Item = &'static str>) -> Vec<String> {
    let mut offered = verbs.into_iter().map(str::to_owned).collect::<Vec<_>>();
    offered.sort_by_key(|verb| VERBS.iter().position(|row| row.id == verb));
    offered
}

#[tokio::test]
async fn the_catalogue_offers_what_the_workspaces_governance_lets_each_caller_do() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;

    // The server's person, the workspace's owner, runs every action the
    // provider serves on the workspace's own governance resource, removing and
    // banning included.
    let owners = offered(&host, workspace_id, &caller(&owner)).await;
    for verb in INVITE_FAMILY
        .iter()
        .chain(&[NAME])
        .chain(&[
            governance::UNBAN,
            governance::REMOVE_MEMBER,
            governance::BAN,
        ])
        .copied()
    {
        assert!(
            owners.iter().any(|offered| offered == verb),
            "{verb}: {owners:?}"
        );
    }

    // An Editor by role alone is offered leaving and naming themselves, and
    // nothing else.
    assert_eq!(
        offered(&host, workspace_id, &caller(&ada)).await,
        offered_in_catalogue(OWN)
    );

    // Granted invite, Ada is offered inviting, leaving and naming themselves, and
    // nothing beyond them.
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
    let granted = offered(&host, workspace_id, &caller(&ada)).await;
    assert_eq!(
        granted,
        offered_in_catalogue(INVITE_FAMILY.iter().chain(&OWN).copied())
    );

    // A Moderator is offered removing, banning and unbanning members, as its
    // role's grants say, but no invitations. Roles, restrictions and the workspace's settings are written
    // by a member's own device, not asked of the host.
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [30; 32],
                user_public_key: bo.user_public_key,
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();
    let moderator = offered(&host, workspace_id, &caller(&bo)).await;
    assert_eq!(
        moderator,
        offered_in_catalogue([
            governance::REMOVE_MEMBER,
            governance::BAN,
            NAME,
            governance::UNBAN,
            TRUSTED_ATTESTORS,
            REVOKED_PUBLISHERS,
            REGISTRIES,
        ])
    );

    // An agent governs nothing, even on the server person's own key, and a
    // workspace that carries no governed workspace offers nothing.
    let agent = Caller::delegated_agent("writer", owner.user_public_key, owner.device_public_key);
    assert!(offered(&host, workspace_id, &agent).await.is_empty());
    assert!(
        offered(&host, WorkspaceId::from_u128(41), &caller(&owner))
            .await
            .is_empty()
    );
    server.shutdown().await;
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn the_persons_vault_models_and_mail_are_paths_of_the_workspace() {
    use tilo_actions::remote::{
        WORKSPACE_MAIL_ACCOUNTS, WORKSPACE_MODELS, WORKSPACE_VAULT, person_accounts, person_models,
        person_vault,
    };

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let provider = host.in_workspace_as(workspace_id, &owner);
    let owner_caller = caller(&owner);
    let query = CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        parent_resource: Some(tilo_actions::remote::workspace_governance_resource(
            &workspace,
        )),
        max_results: 200,
        ..CatalogueQuery::default()
    };
    let page = settled(provider.query(&owner_caller, query)).await.unwrap();
    // Each of the person's own resources is published under the workspace's root, and the
    // action's address is the one the host dispatches to the provider it belongs to.
    let published = |child: &str, action: &str, params: usize| {
        let resource = workspace_governance_child(&workspace, Some(child));
        let row = page
            .rows
            .iter()
            .find(|row| {
                row.resource.id == resource
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|found| found.address.action == action)
            })
            .unwrap_or_else(|| panic!("{child}#{action} is published"));
        assert_eq!(row.provider, WORKSPACE_GOVERNANCE_PROVIDER);
        let found = row.action.as_ref().expect("a row of an action");
        assert_eq!(found.params.len(), params, "{child}#{action}");
        found.address.clone()
    };
    let entries = published(WORKSPACE_VAULT, person_vault::ENTRIES, 0);
    assert_eq!(
        super::vault::vault_action(&entries),
        Some((workspace.as_str(), person_vault::ENTRIES))
    );
    let add_provider = published(WORKSPACE_MODELS, person_models::ADD_PROVIDER, 4);
    assert_eq!(
        super::models::models_action(&add_provider),
        Some((workspace.as_str(), person_models::ADD_PROVIDER))
    );
    let fetch = published(WORKSPACE_MAIL_ACCOUNTS, person_accounts::FETCH, 5);
    assert_eq!(
        super::mail::account_action(&fetch),
        Some((workspace.as_str(), person_accounts::FETCH))
    );
    // Asking one is handed to its provider, never refused as unknown here.
    for address in [entries, add_provider, fetch] {
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
                address: address.clone(),
            },
            argument: InvokeArgument::Json(Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        if provider.invoke(&owner_caller, request).await
            == Some(Err(ActionCallError::UnknownAction))
        {
            panic!("{} is not dispatched to its provider", address.resource);
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn an_invitation_lasts_the_seconds_the_desktop_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;

    // The desktop's invitation: seconds take precedence over the hours an
    // older server would read, for exactly the person and name it asked.
    let guest = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
        .verifying_key()
        .to_bytes();
    let before = tilo_clock::unix_now_ns();
    let timed = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            governance::INVITE,
            json!({
                "role": "viewer",
                "recipient": hex::encode(guest),
                "invitee_name": "Ada",
                "expires_in_seconds": 600,
                "expires_in_hours": 1,
            }),
        ),
    )
    .await
    .unwrap();
    let after = tilo_clock::unix_now_ns();
    let timed_id = timed["invite"].as_str().unwrap();
    assert!(!timed["invitation"].as_str().unwrap().is_empty());
    let timed = workspaces
        .pending(&workspace, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|pending| hex::encode(pending.invite_id) == timed_id)
        .expect("the timed invitation is pending");
    assert_eq!(timed.role, Role::Viewer);
    assert_eq!(timed.recipient_user_public_key, Some(guest));
    assert_eq!(timed.invitee_name.as_deref(), Some("Ada"));
    let lifetime: u64 = 600_000_000_000;
    assert!(
        (before.saturating_add(lifetime)..=after.saturating_add(lifetime))
            .contains(&timed.expires_at_ns),
        "{timed:?}"
    );
    server.shutdown().await;
}

/// The workspace's invitations, as `owner` lists them.
async fn invitations(
    governing: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    owner: &DeviceCert,
    workspace: &str,
) -> Value {
    invoke(
        governing,
        workspace_id,
        &caller(owner),
        request(workspace, INVITATIONS, json!({})),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn invitations_run_through_the_workspaces_own_methods() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, governing) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // The owner invites: the workspace records a pending invitation.
    let invite = request(&workspace, governance::INVITE, json!({ "role": "editor" }));
    let issued = invoke(&governing, workspace_id, &caller(&owner), invite.clone())
        .await
        .unwrap();
    let invite_id = issued["invite"].as_str().unwrap().to_owned();
    let pending = workspaces.pending(&workspace, &owner).await.unwrap();
    assert!(
        pending
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id),
        "{pending:?}"
    );
    // A retry of the same invocation is the same invitation.
    let again = invoke(&governing, workspace_id, &caller(&owner), invite)
        .await
        .unwrap();
    assert_eq!(again, issued);
    assert_eq!(
        workspaces.pending(&workspace, &owner).await.unwrap().len(),
        pending.len()
    );
    let listed = invitations(&governing, workspace_id, &owner, &workspace).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|invitation| invitation["invite"] == invite_id.as_str()
                && invitation["place"] == "editor")
    );
    // An owner's invitation lists as what it makes whoever redeems it: an owner, not the
    // administrator and editor its signed grant names beneath.
    let owners = invoke(
        &governing,
        workspace_id,
        &caller(&owner),
        request(&workspace, governance::INVITE, json!({ "role": "owner" })),
    )
    .await
    .unwrap();
    let listed = invitations(&governing, workspace_id, &owner, &workspace).await;
    let listed_owners = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|invitation| invitation["invite"] == owners["invite"])
        .unwrap_or_else(|| panic!("the owner invitation is listed: {listed:?}"));
    assert_eq!(listed_owners["place"], "owner", "{listed_owners:?}");
    assert_eq!(listed_owners["owner"], true, "{listed_owners:?}");

    // An Editor may not invite: the action does not exist for them.
    let refused = invoke(
        &governing,
        workspace_id,
        &caller(&ada),
        request(&workspace, governance::INVITE, json!({ "role": "viewer" })),
    )
    .await;
    assert_eq!(refused, Err(ActionCallError::UnknownAction));

    // Cancelling takes it off the pending list.
    let mut cancel = Map::new();
    cancel.insert("invite".into(), invite_id.clone().into());
    invoke(
        &governing,
        workspace_id,
        &caller(&owner),
        request(&workspace, CANCEL_INVITATION, Value::Object(cancel)),
    )
    .await
    .unwrap();
    assert!(
        !workspaces
            .pending(&workspace, &owner)
            .await
            .unwrap()
            .iter()
            .any(|pending| hex::encode(pending.invite_id) == invite_id)
    );

    // A resource naming another workspace is not this workspace's action.
    let elsewhere = invoke(
        &governing,
        workspace_id,
        &caller(&owner),
        request(
            "another-workspace",
            governance::INVITE,
            json!({ "role": "viewer" }),
        ),
    )
    .await;
    assert_eq!(elsewhere, Err(ActionCallError::UnknownAction));
    server.shutdown().await;
}

#[tokio::test]
async fn an_editor_invites_up_to_her_own_role_and_no_further() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // An Editor by role alone governs nothing: she reads neither the pending
    // invitations nor the wait list.
    assert!(workspaces.pending(&workspace, &ada).await.is_err());
    assert!(matches!(
        workspaces
            .page_workspaces(
                &ada,
                WorkspacesRequest::WaitList {
                    workspace: workspace.clone()
                }
            )
            .await,
        WorkspacesReply::Refused(_)
    ));

    // Granted invite, Ada invites up to her own role and no further: the
    // workspace's own decision, refused with its reason.
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
    invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(&workspace, governance::INVITE, json!({ "role": "editor" })),
    )
    .await
    .unwrap();
    let above = invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(
            &workspace,
            governance::INVITE,
            json!({ "role": "moderator" }),
        ),
    )
    .await;
    assert!(
        matches!(&above, Err(ActionCallError::Refused { reason }) if reason.contains("Moderator")),
        "{above:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one invitation, its pending listing and the administrator's own grants are one story"
)]
async fn an_administrator_is_an_editor_the_invitation_also_governs() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // The owner may make administrators, so an invitation they make can as
    // well: it joins them as an Editor and says it also makes them one.
    let issued = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            governance::INVITE,
            json!({ "role": "administrator" }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(issued["administrator"], json!(true));
    let signed = hex::decode(issued["invitation"].as_str().unwrap()).unwrap();
    assert!(
        tilo_peer_state::join::makes_administrator(&signed),
        "the invitation is laid out as an administrator's"
    );
    let verified = tilo_peer_state::join::decode_and_verify_invitation(
        &signed,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert!(verified.administrator());
    assert_eq!(verified.grant().role, Role::Editor);

    // The pending list says what redeeming it makes them, beside the role.
    let invite_id = issued["invite"].as_str().unwrap();
    let pending = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, INVITATIONS, json!({})),
    )
    .await
    .unwrap();
    let row = pending
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["invite"] == invite_id)
        .expect("the administrator's invitation is pending");
    assert_eq!(row["role"], json!("editor"));
    assert_eq!(row["administrator"], json!(true));

    // An Editor granted inviting may not make administrators: the
    // workspace's own governance decides it, with its reason.
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
    let refused = invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(
            &workspace,
            governance::INVITE,
            json!({ "role": "administrator" }),
        ),
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("may not make administrators")),
        "{refused:?}"
    );

    // Made an administrator, Ada invites as an administrator and hands out
    // every part below her own, a Moderator's included.
    let who = hex::encode(ada.user_public_key);
    let administrator = roles::administrator_grant(
        &workspace,
        &who,
        Grantee::person(hex::encode(owner.user_public_key)),
    );
    let request_id = *blake3::hash(administrator.id.as_str().as_bytes()).as_bytes();
    workspaces
        .put_workspace_grants(&workspace, &owner, request_id, vec![administrator])
        .await
        .unwrap();
    invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(
            &workspace,
            governance::INVITE,
            json!({ "role": "moderator" }),
        ),
    )
    .await
    .unwrap();
    let made = invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(
            &workspace,
            governance::INVITE,
            json!({ "role": "administrator" }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(made["administrator"], json!(true));
    server.shutdown().await;
}

/// The invite action at the owner's place: the link it answers makes its
/// opener the workspace's owner as they come in, and the workspace has its
/// owner from then on.
#[tokio::test]
async fn an_invitation_at_the_owners_place_makes_its_opener_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    assert!(!workspaces.has_owner(&workspace).await);

    let issued = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, governance::INVITE, json!({ "role": "owner" })),
    )
    .await
    .unwrap();
    assert_eq!(issued["owner"], json!(true));
    assert_eq!(issued["administrator"], json!(true));
    let signed = hex::decode(issued["invitation"].as_str().unwrap()).unwrap();

    let (_, newcomer) = guest_device();
    let admitted = workspaces
        .admit_proven(signed, newcomer.clone(), rand::random(), "Ada", None)
        .await;
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
    assert!(
        workspaces.has_owner(&workspace).await,
        "the link's opener owns the workspace"
    );
    assert!(
        workspaces
            .is_owner(&workspace, newcomer.user_public_key)
            .await
    );
    server.shutdown().await;
}

#[tokio::test]
async fn banning_is_decided_by_grants_and_ending_a_call_waits_for_everyone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);
    let ban = |cert: &DeviceCert| {
        request(
            &workspace,
            governance::BAN,
            json!({ "person": person(cert) }),
        )
    };

    // An Editor holds no grant on banning, and is not told a ban exists:
    // unknown and forbidden answer alike. Someone outside the workspace is
    // answered the same.
    assert_eq!(
        invoke(&host, workspace_id, &caller(&ada), ban(&bo)).await,
        Err(ActionCallError::UnknownAction)
    );
    let (_stranger_key, stranger) = guest_device();
    assert_eq!(
        invoke(&host, workspace_id, &caller(&stranger), ban(&ada)).await,
        Err(ActionCallError::UnknownAction)
    );

    // The Moderator role's grant on banning lets Bo ban Ada, with no
    // confirmation beside the grant, and a retry of that invocation changes
    // nothing more.
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [31; 32],
                user_public_key: bo.user_public_key,
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();
    let banned = ban(&ada);
    invoke(&host, workspace_id, &caller(&bo), banned.clone())
        .await
        .unwrap();
    invoke(&host, workspace_id, &caller(&bo), banned)
        .await
        .unwrap();
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.root() == Some(ada.user_public_key) && member.removed),
        "{members:?}"
    );
    assert!(
        is_banned(workspaces, &workspace, &owner, ada.user_public_key).await,
        "Ada is kept out"
    );

    // The owner unbans her, and she may enter again; a retry of that
    // invocation is the same change, not another.
    let unban = request(
        &workspace,
        governance::UNBAN,
        json!({ "person": person(&ada) }),
    );
    invoke(&host, workspace_id, &caller(&owner), unban.clone())
        .await
        .unwrap();
    invoke(&host, workspace_id, &caller(&owner), unban)
        .await
        .unwrap();
    assert!(
        !is_banned(workspaces, &workspace, &owner, ada.user_public_key).await,
        "the ban is lifted"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn a_member_granted_removing_removes_anyone_but_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);
    let remove = |cert: &DeviceCert| {
        request(
            &workspace,
            governance::REMOVE_MEMBER,
            json!({ "person": person(cert) }),
        )
    };

    // An Editor removes nobody: unknown and forbidden answer alike.
    assert_eq!(
        invoke(&host, workspace_id, &caller(&ada), remove(&bo)).await,
        Err(ActionCallError::UnknownAction)
    );
    // Nobody removes the owner, not even themselves: the workspace keeps one.
    assert!(matches!(
        invoke(&host, workspace_id, &caller(&owner), remove(&owner)).await,
        Err(ActionCallError::Refused { reason }) if reason.contains("owner cannot be removed")
    ));
    // A Moderator removes another Moderator, answered with the people as they are then: the
    // role's grant on removing reaches every member but the owner.
    for (request_id, cert) in [([32; 32], &bo), ([33; 32], &ada)] {
        workspaces
            .set_member_role(
                &workspace,
                &owner,
                SetMemberRoleRequest {
                    request_id,
                    user_public_key: cert.user_public_key,
                    role: Role::Moderator,
                },
            )
            .await
            .unwrap();
    }
    let removed = invoke(&host, workspace_id, &caller(&bo), remove(&ada))
        .await
        .unwrap();
    assert!(
        invoke(&host, workspace_id, &caller(&bo), remove(&owner))
            .await
            .is_err(),
        "a moderator removes no owner"
    );
    assert!(
        removed.as_array().is_some_and(|people| people
            .iter()
            .all(|row| row["member"]["person"] != person(&ada))),
        "{removed}"
    );
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.root() == Some(ada.user_public_key) && member.removed),
        "{members:?}"
    );

    // In a workspace the server makes for Ada, the server's person is offered leaving, as any
    // member is, and never removing her.
    let hers = workspaces
        .create_for("Ada's", None, ada.user_public_key)
        .await
        .unwrap();
    let query = CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        parent_resource: Some(workspace_governance_child(
            &hers.workspace,
            Some(WORKSPACE_MEMBERS),
        )),
        ..CatalogueQuery::default()
    };
    let page = settled(
        host.in_workspace(hers.workspace_id)
            .query(&caller(&owner), query),
    )
    .await
    .unwrap();
    let removable: Vec<_> = page
        .rows
        .into_iter()
        .filter_map(|row| row.action)
        .filter(|action| action.address.action == WORKSPACE_MEMBER_REMOVED)
        .map(|action| action.address.resource)
        .collect();
    assert_eq!(
        removable,
        [workspace_member(&hers.workspace, &person(&owner))]
    );
    server.shutdown().await;
}

/// The per-person `role` request that changes `cert`'s place in `workspace` to `place`: the
/// person whose place it changes is the address's.
fn place_request(workspace: &str, cert: &DeviceCert, place: &str) -> InvokeRequest {
    let mut request = request(workspace, WORKSPACE_MEMBER_PLACED, json!({ "role": place }));
    request.target = RemoteActionTarget {
        provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(
            tilo_actions::remote::workspace_member(workspace, &hex::encode(cert.user_public_key)),
            WORKSPACE_MEMBER_PLACED,
        ),
    };
    request
}

/// The `member.admit` request a device of a person's asks before it signs a change to who is in
/// `workspace`: `change` is the action the tree publishes it at, and `role` the place it would
/// give `cert`.
fn admission_request(
    workspace: &str,
    change: &str,
    cert: &DeviceCert,
    role: &str,
) -> InvokeRequest {
    request(
        workspace,
        WORKSPACE_MEMBER_ADMIT,
        json!({ "change": change, "person": hex::encode(cert.user_public_key), "role": role }),
    )
}

/// The people `member.list` answers, by person: their role, for each person still in the
/// workspace.
fn roles(listed: &Value) -> Vec<(String, String)> {
    listed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| {
            Some((
                row["member"]["person"].as_str()?.to_owned(),
                row["role"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one workspace's role changes, who makes them and who may not are one story"
)]
async fn a_role_is_changed_on_a_member_never_on_someone_the_workspace_does_not_hold() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (_, stranger) = guest_device();
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);
    // Ada administers the workspace, and may move Bo, an Editor.
    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            super::ADD_MEMBER,
            json!({ "person": person(&ada), "role": "administrator" }),
        ),
    )
    .await
    .unwrap();
    invoke(
        &host,
        workspace_id,
        &caller(&ada),
        place_request(&workspace, &bo, "moderator"),
    )
    .await
    .unwrap();
    let listed = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, MEMBERS, json!({})),
    )
    .await
    .unwrap();
    assert!(
        roles(&listed).contains(&(person(&bo), "moderator".to_owned())),
        "{listed}"
    );

    // Someone the workspace does not hold has no place to change: refused before anything is
    // written.
    assert!(matches!(
        invoke(
            &host,
            workspace_id,
            &caller(&ada),
            place_request(&workspace, &stranger, "viewer")
        )
        .await,
        Err(ActionCallError::Refused { reason }) if reason.contains("not a member")
    ));
    let listed = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, MEMBERS, json!({})),
    )
    .await
    .unwrap();
    assert!(
        roles(&listed)
            .iter()
            .all(|(who, _)| *who != person(&stranger)),
        "{listed}"
    );

    // A Moderator holds no grant on changing roles: the action does not
    // exist for Bo either.
    let cy = admit(workspaces, &workspace, &owner, 3).await;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [34; 32],
                user_public_key: bo.user_public_key,
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        invoke(
            &host,
            workspace_id,
            &caller(&bo),
            place_request(&workspace, &cy, "viewer")
        )
        .await,
        Err(ActionCallError::UnknownAction)
    );

    // An Editor granted set-role hands out roles up to her own, and never
    // changes an administrator's: only the owner and whom the workspace is
    // kept for unmake those.
    let dee = admit(workspaces, &workspace, &owner, 4).await;
    grant(workspaces, &workspace, &owner, &dee, governance::ADD_MEMBER).await;
    grant(workspaces, &workspace, &owner, &dee, governance::SET_ROLE).await;
    invoke(
        &host,
        workspace_id,
        &caller(&dee),
        place_request(&workspace, &cy, "viewer"),
    )
    .await
    .unwrap();
    assert!(matches!(
        invoke(
            &host,
            workspace_id,
            &caller(&dee),
            place_request(&workspace, &cy, "moderator")
        )
        .await,
        Err(ActionCallError::Refused { .. })
    ));
    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            super::ADD_MEMBER,
            json!({ "person": person(&cy), "role": "administrator" }),
        ),
    )
    .await
    .unwrap();
    let administrator = invoke(
        &host,
        workspace_id,
        &caller(&dee),
        place_request(&workspace, &cy, "viewer"),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&administrator, ActionCallError::Refused { reason } if reason.contains("may not unmake or remove an administrator")),
        "{administrator:?}"
    );
    // The owner's devices change anyone's role.
    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        place_request(&workspace, &cy, "editor"),
    )
    .await
    .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn a_devices_role_change_is_admitted_for_a_member_and_refused_for_someone_else() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let (_, stranger) = guest_device();
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);
    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            super::ADD_MEMBER,
            json!({ "person": person(&ada), "role": "administrator" }),
        ),
    )
    .await
    .unwrap();

    // A device's role change on a person the workspace does not hold is refused at once, and
    // nothing is written.
    assert!(matches!(
        invoke(
            &host,
            workspace_id,
            &caller(&ada),
            admission_request(&workspace, WORKSPACE_MEMBER_PLACED, &stranger, "viewer")
        )
        .await,
        Err(ActionCallError::Refused { reason }) if reason.contains("not a member")
    ));
    let listed = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, MEMBERS, json!({})),
    )
    .await
    .unwrap();
    assert!(
        roles(&listed)
            .iter()
            .all(|(who, _)| *who != person(&stranger)),
        "{listed}"
    );

    // Adding a person is still the device's to ask for, and once the workspace holds them a
    // role is admitted.
    invoke(
        &host,
        workspace_id,
        &caller(&ada),
        admission_request(&workspace, WORKSPACE_MEMBER_ADDED, &stranger, "viewer"),
    )
    .await
    .unwrap();
    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            super::ADD_MEMBER,
            json!({ "person": person(&stranger), "role": "viewer" }),
        ),
    )
    .await
    .unwrap();
    invoke(
        &host,
        workspace_id,
        &caller(&ada),
        admission_request(&workspace, WORKSPACE_MEMBER_PLACED, &stranger, "editor"),
    )
    .await
    .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn a_host_without_apps_answers_only_its_workspaces_governance() {
    let server = GovernedServer {
        governance: ServiceWorkspaceActions::new(),
        apps: None,
    };
    let someone = Caller::authenticated([1; 32], [2; 32]);
    let governance =
        RemoteActionMessage::Invoke(request("workspace", governance::INVITE, json!({})));
    let mut app = request("workspace", "archive", json!({}));
    app.target.provider = "app".into();
    assert!(server.answers(&someone, &governance));
    assert!(!server.answers(&someone, &RemoteActionMessage::Invoke(app)));
    // Nothing is attached: the workspace's actions are unknown, never run.
    let Some(answer) = server.serve(WorkspaceId::from_u128(7), &someone, governance) else {
        panic!("an invocation is answered");
    };
    drop(server);
    let RemoteActionMessage::Receipt(receipt) = settled(answer).await else {
        panic!("a receipt");
    };
    assert_eq!(
        receipt.status,
        ReceiptStatus::Rejected {
            error: ActionCallError::UnknownAction
        }
    );
}

/// A dev release of `so.tilo.notes` that `publisher` (its public key in hex)
/// built and signed.
fn dev_release(publisher: &str, version: &str, surface: u8) -> Value {
    json!({
        "app": "so.tilo.notes",
        "version": version,
        "surface": hex::encode([surface; 32]),
        "package": hex::encode([surface ^ 0xff; 32]),
        "publisher": publisher,
        "dev": true,
    })
}

/// An Editor by role alone is offered no install verb, and asks for an action
/// the workspace does not carry: no version is recorded.
async fn an_editor_installs_nothing(
    workspaces: &Workspaces,
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    workspace: &str,
    editor: &DeviceCert,
    publisher: &str,
) {
    assert!(
        !offered(host, workspace_id, &caller(editor))
            .await
            .iter()
            .any(|verb| verb == governance::INSTALL_APP)
    );
    assert!(matches!(
        invoke(
            host,
            workspace_id,
            &caller(editor),
            request(
                workspace,
                governance::INSTALL_APP,
                dev_release(publisher, "1.5.0-dev.1", 1)
            ),
        )
        .await,
        Err(ActionCallError::UnknownAction)
    ));
    assert!(workspaces.app_versions(workspace).await.unwrap().is_empty());
}

/// Once the owner lets `developer` install apps, its own build runs, recorded
/// with who signed and installed it; the next push replaces it, so one version
/// runs at a time; and a release that does not name its hashes in full is
/// refused.
async fn a_developer_installs_its_own_build(
    workspaces: &Workspaces,
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    workspace: &str,
    owner: &DeviceCert,
    developer: &DeviceCert,
    publisher: &str,
) {
    grant(
        workspaces,
        workspace,
        owner,
        developer,
        governance::INSTALL_APP,
    )
    .await;
    let answer = invoke(
        host,
        workspace_id,
        &caller(developer),
        request(
            workspace,
            governance::INSTALL_APP,
            dev_release(publisher, "1.5.0-dev.1", 1),
        ),
    )
    .await
    .unwrap();
    assert_eq!(answer["running"]["release"]["version"], "1.5.0-dev.1");
    assert_eq!(answer["running"]["release"]["dev"], true);
    assert_eq!(answer["running"]["installed_by"], publisher);

    // The next push replaces it: one version runs at a time.
    invoke(
        host,
        workspace_id,
        &caller(developer),
        request(
            workspace,
            governance::INSTALL_APP,
            dev_release(publisher, "1.5.0-dev.2", 2),
        ),
    )
    .await
    .unwrap();
    let running = workspaces.app_versions(workspace).await.unwrap();
    assert_eq!(running.len(), 1, "{running:?}");
    assert_eq!(running[0].release.version, "1.5.0-dev.2");
    assert_eq!(
        running[0].release.surface.as_deref(),
        Some(hex::encode([2; 32]).as_str())
    );

    // The app is a resource of the workspace's apps, which offers updating, pinning and
    // uninstalling it; pinned, the version stays until it is unpinned.
    let verbs = offered(host, workspace_id, &caller(developer)).await;
    for verb in [
        governance::INSTALL_APP,
        super::PIN_APP,
        super::UNINSTALL_APP,
    ] {
        assert!(
            verbs.iter().any(|offered| offered == verb),
            "{verb}: {verbs:?}"
        );
    }
    invoke(
        host,
        workspace_id,
        &caller(developer),
        request(workspace, super::PIN_APP, json!({ "app": "so.tilo.notes" })),
    )
    .await
    .unwrap();
    let refused = invoke(
        host,
        workspace_id,
        &caller(developer),
        request(
            workspace,
            governance::INSTALL_APP,
            dev_release(publisher, "1.5.0-dev.3", 3),
        ),
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason, .. }) if reason.contains("unpin it first")),
        "{refused:?}"
    );
    let verbs = offered(host, workspace_id, &caller(developer)).await;
    assert!(
        verbs.iter().any(|offered| offered == super::UNPIN_APP),
        "{verbs:?}"
    );
    invoke(
        host,
        workspace_id,
        &caller(developer),
        request(
            workspace,
            super::UNPIN_APP,
            json!({ "app": "so.tilo.notes" }),
        ),
    )
    .await
    .unwrap();
    let running = workspaces.app_versions(workspace).await.unwrap();
    assert_eq!(running[0].release.version, "1.5.0-dev.2");
    assert!(!running[0].release.pinned);

    // A release names its hashes in full.
    assert!(matches!(
        invoke(
            host,
            workspace_id,
            &caller(developer),
            request(
                workspace,
                governance::INSTALL_APP,
                json!({ "app": "so.tilo.notes", "version": "1", "surface": "ab", "package": "cd", "publisher": "ef" }),
            ),
        )
        .await,
        Err(ActionCallError::InvalidRequest { .. })
    ));
}

/// The workspace chooses the version of an app every member runs. An Editor by
/// role alone may not; a member the owner lets install apps (a developer)
/// installs their own build, which the workspace records with who signed and
/// installed it, and taking the choice back lets each member run their own.
#[tokio::test]
async fn a_developer_chooses_the_version_of_an_app_the_whole_workspace_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let publisher = hex::encode(ada.user_public_key);

    an_editor_installs_nothing(
        workspaces,
        &host,
        workspace_id,
        &workspace,
        &ada,
        &publisher,
    )
    .await;
    a_developer_installs_its_own_build(
        workspaces,
        &host,
        workspace_id,
        &workspace,
        &owner,
        &ada,
        &publisher,
    )
    .await;

    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            super::UNINSTALL_APP,
            json!({ "app": "so.tilo.notes" }),
        ),
    )
    .await
    .unwrap();
    assert!(
        workspaces
            .app_versions(&workspace)
            .await
            .unwrap()
            .is_empty()
    );
    server.shutdown().await;
}

/// `request` of the person's own settings, asked by the device `device`
/// certifies in the workspace `workspace_id` carries.
pub(super) async fn ask_own(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    device: &DeviceCert,
    verb: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let mut request = request("", "", argument);
    request.target = tilo_actions::remote::person_models::target(verb);
    let provider = host.in_workspace_as(workspace_id, device);
    let asking = caller(device);
    let invocation = request.invocation;
    let mut receipt = settled(provider.invoke(&asking, request)).await?;
    // An ask of the person's model is accepted, then followed until it ends.
    if matches!(receipt.status, ReceiptStatus::Accepted) {
        let now = tilo_clock::now();
        let until = now.checked_add(tilo_test_env::hang_bound()).unwrap_or(now);
        receipt = settled(tilo_actions::remote::settle_with(
            &provider, &asking, invocation, None, until,
        ))
        .await
        .expect("the ask is the caller's to follow")
        .receipt();
    }
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a person's own settings answer: {other:?}"),
    }
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                waiting.push(path);
            } else if metadata.is_file() {
                let bytes = std::fs::read(&path).unwrap();
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    holding.push(path);
                }
            }
        }
    }
    holding
}

/// Every request of a person's model providers, the ones that name a
/// provider naming `provider`.
fn model_requests(provider: &str) -> Vec<(&'static str, Value)> {
    use tilo_actions::remote::person_models;

    vec![
        (person_models::LIST, json!({})),
        (
            person_models::ADD_PROVIDER,
            json!({"name": "Another", "protocol": "open-ai-compatible", "endpoint": "https://other.example/v1"}),
        ),
        (person_models::ADD_PRESET, json!({"preset": "anthropic"})),
        (
            person_models::SET_KEY,
            json!({"provider": provider, "key": "sk-replaced"}),
        ),
        (person_models::REMOVE_KEY, json!({"provider": provider})),
        (
            person_models::REMOVE_PROVIDER,
            json!({"provider": provider}),
        ),
        (person_models::REFRESH_MODELS, json!({"provider": provider})),
        (
            person_models::ADD_MODEL,
            json!({"provider": provider, "model": "typed"}),
        ),
        (
            person_models::SET_DEFAULT,
            json!({"provider": provider, "model": "typed"}),
        ),
        (
            person_models::SET_ASSIGNMENT,
            json!({"kind": "task", "provider": provider, "model": "typed"}),
        ),
        (
            person_models::SET_ASSIGNMENTS,
            json!({"kinds": ["request", "task"], "provider": provider, "model": "typed"}),
        ),
        (
            person_models::SET_REASONING,
            json!({"provider": provider, "effort": "low"}),
        ),
    ]
}

/// What another workspace sees of the host's providers, and that naming
/// `provider` there reaches nothing of this workspace's.
async fn reaches_none_of_it(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    other: &DeviceCert,
    provider: &str,
) {
    use tilo_actions::remote::person_models;

    let listed = ask_own(host, workspace_id, other, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(listed["providers"], json!([]), "{listed}");
    // Every request that names a provider: the three that name none are the
    // other workspace's own to make, of its own store.
    let naming = model_requests(provider).into_iter().filter(|(verb, _)| {
        ![
            person_models::LIST,
            person_models::ADD_PROVIDER,
            person_models::ADD_PRESET,
        ]
        .contains(verb)
    });
    for (verb, argument) in naming {
        let refused = ask_own(host, workspace_id, other, verb, argument).await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{verb}: {refused:?}"
        );
    }
}

/// A workspace's provider key is the workspace's, not a person's: the host
/// seals it in the workspace's own vault, every member reads back only that
/// one is kept, and the key is nowhere on the host's disk in the clear.
/// Another workspace on the same host neither lists the provider nor reaches
/// it through any request, nor finds its key in a store of its own.
#[tokio::test]
async fn a_workspaces_key_is_sealed_and_no_other_workspace_reaches_it() {
    use tilo_actions::remote::person_models;
    const SECRET: &str = "sk-ada-own-secret";

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [31; 32],
                user_public_key: bo.user_public_key,
                role: Role::Moderator,
            },
        )
        .await
        .unwrap();

    let added = ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::ADD_PROVIDER,
        json!({"name": "Ada's provider", "protocol": "open-ai-compatible", "endpoint": "https://models.example/v1"}),
    )
    .await
    .expect("Ada adds a provider");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    let keeping = json!({"provider": provider, "key": SECRET});
    ask_own(&host, workspace_id, &ada, person_models::SET_KEY, keeping)
        .await
        .expect("Ada keeps her key");
    let hers = ask_own(&host, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(hers["providers"][0]["configured"], json!(true), "{hers}");

    // Bo, another member of the workspace, reads the provider the workspace
    // keeps, and what he reads back says only that a key is kept.
    let his = ask_own(&host, workspace_id, &bo, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(his["providers"][0]["configured"], json!(true), "{his}");
    assert!(!his.to_string().contains(SECRET), "{his}");

    // Another workspace on the same host reaches none of it.
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    reaches_none_of_it(&host, elsewhere_id, &cy, &provider).await;

    // The key is an entry of the workspace's own vault, the one its
    // provider names; the host keeps no vault of its own beside it, and the
    // key is nowhere on the host's disk in the clear.
    let own = workspaces.workspace_root(&workspace_id.to_string());
    let named = crate::workspaces::person_settings::provider_config(&own)
        .profiles
        .iter()
        .find(|profile| profile.id == provider)
        .and_then(crate::workspaces::person_settings::key_reference)
        .expect("the provider names its key");
    let ours = workspaces
        .open_vault(&workspace_id.to_string())
        .await
        .unwrap();
    assert_eq!(
        ours.entry(named.id()).map(|entry| entry.password.as_str()),
        Some(SECRET)
    );
    let theirs = workspaces
        .open_vault(&elsewhere_id.to_string())
        .await
        .unwrap();
    assert!(
        theirs.entry(named.id()).is_none(),
        "another workspace's vault holds this workspace's key"
    );
    assert!(!accounts::vault_dir_in(&own).exists());
    let clear = files_holding(dir.path(), SECRET.as_bytes());
    assert!(clear.is_empty(), "the key in the clear: {clear:?}");
}

/// An agent no device of a member's stands behind reaches nothing of the
/// workspace's model providers, whatever it asks: not one
/// the server admitted itself, not a bot and not an app. Unknown and
/// forbidden are the same answer, and nothing of the workspace's changed.
#[tokio::test]
async fn no_agent_with_no_device_behind_it_reaches_the_model_providers() {
    use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};
    use tilo_actions::remote::person_models;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let added = ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::ADD_PRESET,
        json!({"preset": "openrouter"}),
    )
    .await
    .expect("Ada adds a provider");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::SET_KEY,
        json!({"provider": provider, "key": "sk-ada-own-secret"}),
    )
    .await
    .expect("Ada keeps her key");
    let before = ask_own(&host, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();

    let person = hex::encode(ada.user_public_key);
    let agents = [
        Caller::local_agent("assistant"),
        Caller::local_bot("bot"),
        Caller::local_app("so.tilo.chat").acting_for(person),
    ];
    for agent in &agents {
        let mut asks = model_requests(&provider);
        asks.push((
            person_models::ASK,
            serde_json::to_value(ModelRequest {
                messages: vec![Message::text(Role::User, "Spend the workspace's key")],
                tools: Vec::new(),
                tool_choice: ToolChoice::Auto,
                max_output_tokens: 16,
            })
            .unwrap(),
        ));
        for (verb, argument) in asks {
            let mut asking = request("", "", argument);
            asking.target = person_models::target(verb);
            let refused = settled(
                host.in_workspace_as(workspace_id, &ada)
                    .invoke(agent, asking),
            )
            .await
            .map(|_| ());
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{verb}: {refused:?}"
            );
        }
    }
    let after = ask_own(&host, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(after["providers"][0]["configured"], json!(true), "{after}");
    server.shutdown().await;
}

/// The model of `workspace` is asked once, and it answers.
async fn asks_their_model(workspaces: &Workspaces, workspace: WorkspaceId) {
    use agent_provider_config::Interaction;
    use agent_provider_config::protocol::{Message, ModelRequest, Role, ToolChoice};

    let request = ModelRequest {
        messages: vec![
            Message::text(Role::System, "You work for Ada."),
            Message::text(Role::User, "Is it time?"),
        ],
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 256,
    };
    let deadline = tilo_clock::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    workspaces
        .workspace_model(workspace, Interaction::Request)
        .answer(&request, deadline, &mut |_| {})
        .await
        .expect("the workspace's model answers");
}

/// What `at` was sent by the ask before.
fn sent(at: &test_http::Responder) -> Value {
    serde_json::from_slice(&at.next_request().body).unwrap()
}

/// One answer, as an OpenAI-compatible provider streams it.
const A_STREAMED_ANSWER: &str = concat!(
    "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"large\",",
    "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Yes.\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"large\",",
    "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"reply\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"large\",",
    "\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n",
    "data: [DONE]\n\n",
);

/// `person` asks for `effort` as the thinking level of their `provider`, or
/// with none takes their choice back.
async fn asks_for_level(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    provider: &str,
    effort: Option<&str>,
) -> Result<Value, ActionCallError> {
    let mut setting = json!({"provider": provider});
    if let Some(effort) = effort {
        setting["effort"] = json!(effort);
    }
    ask_own(
        actions,
        workspace_id,
        person,
        tilo_actions::remote::person_models::SET_REASONING,
        setting,
    )
    .await
}

/// The level the workspace's settings keep for `provider`, with the field
/// the provider is told it in.
fn kept_level(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    provider: &str,
) -> Option<agent_provider_config::ReasoningSetting> {
    let root = workspaces.workspace_root(&workspace_id.to_string());
    crate::workspaces::person_settings::provider_config(&root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .and_then(|profile| profile.reasoning)
}

/// The ready-made provider `name`, added for `person`: its id.
async fn ready_made(
    actions: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    name: &str,
) -> String {
    ask_own(
        actions,
        workspace_id,
        person,
        tilo_actions::remote::person_models::ADD_PRESET,
        json!({"preset": name}),
    )
    .await
    .expect("a ready-made provider is added")["id"]
        .as_str()
        .expect("the provider's id")
        .to_owned()
}

/// How hard the workspace's provider thinks is its members' to choose, in one
/// request: the next ask of its model carries the level to the provider, a
/// word that is no level changes nothing, and with the level taken back the
/// provider is told nothing again, as before anybody chose. Each provider is
/// told in the field it takes a level in, and one that is asked for none
/// says so and keeps nothing.
#[tokio::test]
async fn a_thinking_level_reaches_the_workspaces_provider_with_the_next_ask() {
    use agent_provider_config::{ReasoningParam, ReasoningSetting};
    use test_http::{Reply, Responder};
    use tilo_actions::remote::person_models;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    // The provider is on this machine, which a server calls only with its
    // operator's leave.
    workspaces.call_private_model_addresses(true);
    let provider_at = Responder::handle(|request| {
        Some(if request.method == "GET" {
            Reply::status(200).body(r#"{"data":[{"id":"large"}]}"#)
        } else {
            Reply::status(200)
                .header("Content-Type", "text/event-stream")
                .body(A_STREAMED_ANSWER)
        })
    });
    let adding = json!({
        "name": "A provider",
        "protocol": "open-ai-compatible",
        "endpoint": format!("{}/v1", provider_at.url()),
    });
    let provider = links_a_provider(&actions, workspace_id, &ada, &adding, "sk-ada").await;
    let choosing = json!({"provider": provider, "model": "large"});
    ask_own(
        &actions,
        workspace_id,
        &ada,
        person_models::SET_DEFAULT,
        choosing,
    )
    .await
    .expect("Ada chooses her model");
    provider_at.drain_requests();

    asks_their_model(workspaces, workspace_id).await;
    let before = sent(&provider_at);
    assert!(
        before.get("reasoning_effort").is_none() && before.get("reasoning").is_none(),
        "nothing is sent before anybody chose: {before}"
    );

    let chosen = asks_for_level(&actions, workspace_id, &ada, &provider, Some("low")).await;
    assert!(chosen.is_ok(), "a member's own choice is taken: {chosen:?}");
    assert_eq!(chosen.unwrap()["reasoning"], json!("low"));
    let listed = ask_own(&actions, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(
        listed["providers"][0]["reasoning"],
        json!("low"),
        "{listed}"
    );
    asks_their_model(workspaces, workspace_id).await;
    let low = sent(&provider_at);
    assert_eq!(low["reasoning_effort"], json!("low"), "{low}");

    // A word that is no level is refused and changes nothing.
    let refused = asks_for_level(&actions, workspace_id, &ada, &provider, Some("lowest")).await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "{refused:?}"
    );
    asks_their_model(workspaces, workspace_id).await;
    let still = sent(&provider_at);
    assert_eq!(still["reasoning_effort"], json!("low"), "{still}");

    // Taken back: the provider is told nothing again.
    let cleared = asks_for_level(&actions, workspace_id, &ada, &provider, None).await;
    assert!(cleared.is_ok(), "the choice is taken back: {cleared:?}");
    assert!(cleared.unwrap().get("reasoning").is_none());
    asks_their_model(workspaces, workspace_id).await;
    let after = sent(&provider_at);
    assert!(
        after.get("reasoning_effort").is_none() && after.get("reasoning").is_none(),
        "{after}"
    );
    assert_eq!(kept_level(workspaces, workspace_id, &provider), None);

    // Each provider is told in the field it takes a level in.
    let router = ready_made(&actions, workspace_id, &ada, "openrouter").await;
    let taken = asks_for_level(&actions, workspace_id, &ada, &router, Some("high")).await;
    assert!(taken.is_ok(), "{taken:?}");
    assert_eq!(
        kept_level(workspaces, workspace_id, &router),
        Some(ReasoningSetting {
            param: ReasoningParam::ReasoningObject,
            effort: "high".into(),
        })
    );
    let anthropic = ready_made(&actions, workspace_id, &ada, "anthropic").await;
    let turned_away = asks_for_level(&actions, workspace_id, &ada, &anthropic, Some("high")).await;
    assert!(
        matches!(turned_away, Err(ActionCallError::Refused { .. })),
        "{turned_away:?}"
    );
    assert_eq!(kept_level(workspaces, workspace_id, &anthropic), None);
    server.shutdown().await;
}

/// A provider `person` links as `adding` says, its `key` kept and its models
/// asked for: the provider's id.
async fn links_a_provider(
    host: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    person: &DeviceCert,
    adding: &Value,
    key: &str,
) -> String {
    use tilo_actions::remote::person_models;

    let added = ask_own(
        host,
        workspace_id,
        person,
        person_models::ADD_PROVIDER,
        adding.clone(),
    )
    .await
    .expect("a provider is added");
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    ask_own(
        host,
        workspace_id,
        person,
        person_models::SET_KEY,
        json!({"provider": provider, "key": key}),
    )
    .await
    .expect("the key is kept");
    let listed = ask_own(
        host,
        workspace_id,
        person,
        person_models::REFRESH_MODELS,
        json!({"provider": provider}),
    )
    .await
    .expect("the provider is asked");
    assert!(listed.get("problem").is_none(), "{listed}");
    assert!(!listed.to_string().contains(key), "{listed}");
    provider
}

/// A provider on this machine that lists `sk-ada`'s models to that key and
/// `sk-bo`'s to that one, and where a page adds it.
fn provider_of_two_keys() -> (test_http::Responder, Value) {
    use test_http::{Reply, Responder};

    let fake = Responder::handle(|request| {
        let models = match request.header("authorization") {
            Some("Bearer sk-ada") => r#"{"data":[{"id":"ada-large"},{"id":"ada-small"}]}"#,
            Some("Bearer sk-bo") => r#"{"data":[{"id":"bo-only"}]}"#,
            _ => return Some(Reply::status(401)),
        };
        Some(Reply::status(200).body(models))
    });
    let adding = json!({
        "name": "Fake provider",
        "protocol": "open-ai-compatible",
        "endpoint": format!("{}/v1", fake.url()),
    });
    (fake, adding)
}

/// The model `kind` of work runs on in the listing `reads`.
fn model_for(reads: &Value, kind: &str) -> Option<Value> {
    reads["assignments"]
        .as_array()
        .and_then(|chosen| chosen.iter().find(|chosen| chosen["kind"] == kind))
        .map(|chosen| chosen["model"].clone())
}

/// Two workspaces on one host each link a provider with a key of their own.
/// The host asks the provider for its models with each workspace's key, each
/// chooses the models its kinds of work run on, and neither reads the
/// other's or chooses from them.
#[tokio::test]
async fn each_workspace_links_a_provider_and_chooses_its_own_models() {
    use tilo_actions::remote::person_models;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &elsewhere, &owner, 2).await;
    // The provider is on this machine, which a server calls only with its
    // operator's leave.
    workspaces.call_private_model_addresses(true);
    let (_fake, adding) = provider_of_two_keys();
    let hers = links_a_provider(&host, workspace_id, &ada, &adding, "sk-ada").await;
    let his = links_a_provider(&host, elsewhere_id, &bo, &adding, "sk-bo").await;

    for (here, person, verb, choosing) in [
        (
            workspace_id,
            &ada,
            person_models::SET_DEFAULT,
            json!({"provider": hers, "model": "ada-small"}),
        ),
        (
            workspace_id,
            &ada,
            person_models::SET_ASSIGNMENTS,
            json!({"kinds": ["request", "task"], "provider": hers, "model": "ada-large"}),
        ),
        (
            workspace_id,
            &ada,
            person_models::SET_ASSIGNMENT,
            json!({"kind": "subtask", "provider": hers, "model": "ada-small"}),
        ),
        (
            elsewhere_id,
            &bo,
            person_models::SET_DEFAULT,
            json!({"provider": his, "model": "bo-only"}),
        ),
    ] {
        ask_own(&host, here, person, verb, choosing)
            .await
            .expect("a model is chosen");
    }

    let ada_reads = ask_own(&host, workspace_id, &ada, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(ada_reads["defaultProvider"], json!(hers), "{ada_reads}");
    assert_eq!(
        ada_reads["providers"][0]["defaultModel"],
        json!("ada-small")
    );
    let offered: Vec<&str> = ada_reads["providers"][0]["models"]
        .as_array()
        .expect("the models the provider listed")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert_eq!(offered, ["ada-large", "ada-small"]);
    assert_eq!(model_for(&ada_reads, "request"), Some(json!("ada-large")));
    assert_eq!(model_for(&ada_reads, "task"), Some(json!("ada-large")));
    assert_eq!(model_for(&ada_reads, "subtask"), Some(json!("ada-small")));
    assert_eq!(model_for(&ada_reads, "classifier"), None);

    let bo_reads = ask_own(&host, elsewhere_id, &bo, person_models::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(bo_reads["defaultProvider"], json!(his), "{bo_reads}");
    assert_eq!(bo_reads["providers"].as_array().map(Vec::len), Some(1));
    assert_eq!(bo_reads["providers"][0]["defaultModel"], json!("bo-only"));
    assert!(bo_reads.get("assignments").is_none(), "{bo_reads}");
    // His own provider offers no model of hers, and hers is no provider of
    // his.
    for provider in [&his, &hers] {
        let refused = ask_own(
            &host,
            elsewhere_id,
            &bo,
            person_models::SET_DEFAULT,
            json!({"provider": provider, "model": "ada-large"}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{refused:?}"
        );
    }
    server.shutdown().await;
}

/// A host calls a provider on its own network only with its operator's
/// leave: without it no provider is kept there, and one kept while it had
/// the leave is no longer asked.
#[tokio::test]
async fn a_host_reaches_its_own_network_only_with_its_operators_leave() {
    use tilo_actions::remote::person_models;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let (fake, adding) = provider_of_two_keys();

    let unkept = ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::ADD_PROVIDER,
        adding.clone(),
    )
    .await;
    assert!(
        matches!(unkept, Err(ActionCallError::Refused { .. })),
        "{unkept:?}"
    );
    assert!(fake.drain_requests().is_empty());

    workspaces.call_private_model_addresses(true);
    let hers = links_a_provider(&host, workspace_id, &ada, &adding, "sk-ada").await;
    // Once to ask whether it takes the key before it is kept, once to list
    // its models.
    assert_eq!(fake.drain_requests().len(), 2);

    workspaces.call_private_model_addresses(false);
    let uncalled = ask_own(
        &host,
        workspace_id,
        &ada,
        person_models::REFRESH_MODELS,
        json!({"provider": hers}),
    )
    .await
    .expect("an answer that says why not");
    assert_eq!(uncalled["problem"], json!("not-allowed"), "{uncalled}");
    assert!(fake.drain_requests().is_empty());
    server.shutdown().await;
}

/// What a service on this device's own network says to whoever asks it.
const SAID_INSIDE: &str = "what a service inside says";

/// Which holders keep a workspace, and how many copies it keeps, is its
/// owners' decision: to another person's device listing, adding or removing
/// them or choosing the copies is no action it has, and nothing is asked of
/// the site it names.
#[tokio::test]
async fn only_an_owner_chooses_a_workspaces_holders() {
    use test_http::{Reply, Responder};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let (workspace, workspace_id, governance) = governed(server.workspaces()).await;
    let (_, stranger) = governor();
    let stranger = caller(&stranger);
    let site = Responder::handle(|_| Some(Reply::status(404)));
    for (verb, argument) in [
        (super::SERVERS, json!({})),
        (super::ADD_SERVER, json!({ "origin": site.url() })),
        (super::REMOVE_SERVER, json!({ "server": site.url() })),
        (super::SET_COPIES, json!({ "key_holders": 1 })),
    ] {
        let asked = request(&workspace, verb, argument);
        assert_eq!(
            invoke(&governance, workspace_id, &stranger, asked).await,
            Err(ActionCallError::UnknownAction),
            "a stranger is offered no say in the workspace's holders"
        );
    }
    assert!(
        site.drain_requests().is_empty(),
        "a stranger's holder was asked"
    );
}

/// Why what was asked about a workspace's holders was refused.
fn holder_refused(answered: Result<Value, ActionCallError>) -> String {
    match answered {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("naming a holder was not refused: {other:?}"),
    }
}

/// An owner names a server by its own web-host.json line, which its operator hands over: this
/// device asks no site where a server is. A site's address, on the internet or on this device's
/// own network, is no server's line, and is refused before anything is asked, for adding a server
/// and for removing one, even where its operator lets this device call its own network.
#[tokio::test]
async fn a_server_is_named_by_its_own_line_and_no_site_is_asked() {
    use test_http::{Reply, Responder};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let (workspace, workspace_id, governance) = governed(server.workspaces()).await;
    server.workspaces().call_private_model_addresses(true);
    let owner = caller(&owner);
    let inside = Responder::handle(|_| Some(Reply::status(403).body(SAID_INSIDE)));
    for site in [
        inside.url(),
        format!("https://127.0.0.1:{}", inside.port()),
        "https://10.0.0.5:8500".to_owned(),
        "http://holder.example".to_owned(),
        "https://holder.example".to_owned(),
    ] {
        let adding = request(&workspace, super::ADD_SERVER, json!({ "host": site }));
        let reason = holder_refused(invoke(&governance, workspace_id, &owner, adding).await);
        assert!(
            reason.contains("not the host's own web-host.json line"),
            "{site}: {reason}"
        );
        assert!(!reason.contains(SAID_INSIDE), "{reason}");
    }
    let removing = request(
        &workspace,
        super::REMOVE_SERVER,
        json!({ "server": inside.url() }),
    );
    let reason = holder_refused(invoke(&governance, workspace_id, &owner, removing).await);
    assert!(
        reason.contains("no server keeping the workspace goes by"),
        "{reason}"
    );
    assert!(
        inside.drain_requests().is_empty(),
        "a site was asked where a server is"
    );
}

/// The workspace's package trust and registries are resources its install
/// grant changes and every member reads: an Editor by role alone has
/// neither, and a member the owner lets install apps adds an attestor,
/// refuses a publisher, and adds, disables, enables and re-channels a
/// registry, each answer being the list as it stands after the change.
#[tokio::test]
async fn the_workspace_keeps_the_package_trust_and_registries_its_members_fetch_by() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let ask = |device: &DeviceCert, verb: &str, argument: Value| {
        invoke(
            &host,
            workspace_id,
            &caller(device),
            request(&workspace, verb, argument),
        )
    };

    // Every member reads the lists, and an Editor by role alone changes none.
    let read = ask(&ada_caller, TRUSTED_ATTESTORS, json!({}))
        .await
        .unwrap();
    assert_eq!(read["attestors"], json!([]));
    assert_eq!(read["revoked"], json!([]));
    let read = ask(&ada_caller, REGISTRIES, json!({})).await.unwrap();
    assert_eq!(read["registries"], json!([]));
    for verb in [ADD_ATTESTOR, REVOKE_PUBLISHER, ADD_REGISTRY] {
        assert!(
            matches!(
                ask(
                    &ada,
                    verb,
                    json!({
                        "id": "acme/1",
                        "pubkey": "ab".repeat(32),
                        "name": "Tilo main",
                        "url": "https://apps.tilo.so",
                        "owner_key": "cd".repeat(32),
                    }),
                )
                .await,
                Err(ActionCallError::UnknownAction)
            ),
            "{verb} is no action an Editor may take"
        );
    }

    grant(
        workspaces,
        &workspace,
        &owner,
        &ada,
        governance::INSTALL_APP,
    )
    .await;

    let attestor_key = "ab".repeat(32);
    let added = ask(
        &ada,
        ADD_ATTESTOR,
        json!({ "id": "acme/1", "pubkey": attestor_key, "scope": "prefix:so.tilo." }),
    )
    .await
    .unwrap();
    assert_eq!(added["attestors"][0]["id"], "acme/1");
    assert_eq!(added["attestors"][0]["pubkey_hex"], attestor_key);
    assert_eq!(added["attestors"][0]["scope"], "prefix:so.tilo.");
    let listed = ask(&ada_caller, TRUSTED_ATTESTORS, json!({}))
        .await
        .unwrap();
    assert_eq!(listed["attestors"], added["attestors"]);

    let removed = ask(&ada_caller, REMOVE_ATTESTOR, json!({ "id": "acme/1" }))
        .await
        .unwrap();
    assert_eq!(removed["attestors"], json!([]));

    let refused_key = "ef".repeat(32);
    let revoked = ask(
        &ada,
        REVOKE_PUBLISHER,
        json!({ "pubkey": refused_key, "reason": "lost key" }),
    )
    .await
    .unwrap();
    assert_eq!(revoked["revoked"][0]["pubkey_hex"], refused_key);
    assert_eq!(revoked["revoked"][0]["reason"], "lost key");
    assert_eq!(
        ask(&ada_caller, REVOKED_PUBLISHERS, json!({}))
            .await
            .unwrap()["revoked"],
        revoked["revoked"]
    );

    let owner_key = "cd".repeat(32);
    let added = ask(
        &ada,
        ADD_REGISTRY,
        json!({ "name": "Tilo main", "url": "https://apps.tilo.so/", "owner_key": owner_key }),
    )
    .await
    .unwrap();
    assert_eq!(added["registries"][0]["name"], "Tilo main");
    assert_eq!(added["registries"][0]["base_url"], "https://apps.tilo.so");
    assert_eq!(added["registries"][0]["owner_pubkey_hex"], owner_key);
    assert_eq!(added["registries"][0]["enabled"], true);
    assert_eq!(added["registries"][0]["channel"], "stable");

    let disabled = ask(
        &ada_caller,
        DISABLE_REGISTRY,
        json!({ "name": "Tilo main" }),
    )
    .await
    .unwrap();
    assert_eq!(disabled["registries"][0]["enabled"], false);
    let enabled = ask(&ada_caller, ENABLE_REGISTRY, json!({ "name": "Tilo main" }))
        .await
        .unwrap();
    assert_eq!(enabled["registries"][0]["enabled"], true);
    let development = ask(
        &ada,
        REGISTRY_CHANNEL,
        json!({ "name": "Tilo main", "channel": "development" }),
    )
    .await
    .unwrap();
    assert_eq!(development["registries"][0]["channel"], "development");

    // A plain-HTTP address that is not loopback, and a placeholder owner key,
    // are refused: discovery never creates trust.
    assert!(matches!(
        ask(
            &ada,
            ADD_REGISTRY,
            json!({
                "name": "plain",
                "url": "http://example.test",
                "owner_key": "cd".repeat(32),
            }),
        )
        .await,
        Err(ActionCallError::InvalidRequest { .. })
    ));
    assert!(matches!(
        ask(
            &ada,
            ADD_REGISTRY,
            json!({
                "name": "unpinned",
                "url": "https://example.test",
                "owner_key": "00".repeat(32),
            }),
        )
        .await,
        Err(ActionCallError::InvalidRequest { .. })
    ));

    let removed = ask(&ada_caller, REMOVE_REGISTRY, json!({ "name": "Tilo main" }))
        .await
        .unwrap();
    assert_eq!(removed["registries"], json!([]));

    server.shutdown().await;
}
