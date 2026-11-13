//! A governed workspace's administration as `tilo.workspace` actions: offered as
//! far as the workspace's governance lets the caller, run through the same workspace
//! methods as the wire doors, and destructive governance held back until the
//! person's own confirmation can be asked for.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use serde_json::{Map, Value, json};
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, roles, roles::governance};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CatalogueQuery, Confirmation, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER,
};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::workspaces::JoinReply;
use tilo_user_identity::DeviceCert;

use super::{CANCEL_INVITATION, GovernedPlane, HostWorkspaceActions, INVITATIONS, MEMBERS};
use crate::app_serving::AppServingPlane as _;
use crate::workspaces::tests::{governor, guest_device, signed_pre_admission, test_host};
use crate::workspaces::{PreAdmitRequest, SetMemberRoleRequest, Workspaces};

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
        .put_room_grants(workspace, owner, request_id, vec![grant])
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
        target: RemoteActionTarget::workspace_governance(workspace, verb),
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
    }
}

/// A new workspace of `workspaces`, the workspace carrying it, and its governance
/// as actions.
pub(super) async fn governed(workspaces: &Workspaces) -> (String, u64, HostWorkspaceActions) {
    let summary = workspaces.create("Team").await.unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    (summary.workspace, summary.workspace_id, host)
}

/// The verbs `caller` is offered on the workspace `workspace_id` carries.
async fn offered(host: &HostWorkspaceActions, workspace_id: u64, caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    let page = settled(host.in_workspace(workspace_id).query(caller, query))
        .await
        .unwrap();
    page.rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// `request` by `caller`: what it produced, or why not.
async fn invoke(
    host: &HostWorkspaceActions,
    workspace_id: u64,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(host.in_workspace(workspace_id).invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

const INVITE_FAMILY: [&str; 4] = [governance::INVITE, INVITATIONS, CANCEL_INVITATION, MEMBERS];

#[tokio::test]
async fn the_catalogue_offers_what_the_rooms_governance_lets_each_caller_do() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;

    // The relay's person runs every action the provider serves, on the
    // workspace's own governance resource, removing and banning included: their
    // own request confirms it.
    let owners = offered(&host, workspace_id, &caller(&owner)).await;
    for verb in INVITE_FAMILY.iter().chain(&[
        governance::UNBAN,
        governance::CALL,
        governance::REMOVE_MEMBER,
        governance::BAN,
    ]) {
        assert!(
            owners.iter().any(|offered| offered == verb),
            "{verb}: {owners:?}"
        );
    }

    // An Editor by role alone is offered nothing.
    assert!(offered(&host, workspace_id, &caller(&ada)).await.is_empty());

    // Granted invite, Ada is offered inviting and nothing beyond it.
    grant(workspaces, &workspace, &owner, &ada, governance::INVITE).await;
    let granted = offered(&host, workspace_id, &caller(&ada)).await;
    assert_eq!(granted, INVITE_FAMILY.map(str::to_owned).to_vec());

    // A Moderator is offered unbanning members below them, but no
    // invitations. Roles, restrictions and the workspace's settings are written
    // by a member's own device, not asked of the host.
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [30; 32],
                user_public_key: bo.user_public_key,
                role: MemberRole::Moderator,
            },
        )
        .await
        .unwrap();
    let moderator = offered(&host, workspace_id, &caller(&bo)).await;
    assert_eq!(moderator, [governance::UNBAN].map(str::to_owned));

    // An agent governs nothing, even on the relay person's own key, and a
    // workspace that carries no governed workspace offers nothing.
    let agent = Caller::delegated_agent("writer", owner.user_public_key, owner.device_public_key);
    assert!(offered(&host, workspace_id, &agent).await.is_empty());
    assert!(
        offered(&host, workspace_id ^ 1, &caller(&owner))
            .await
            .is_empty()
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn an_invitation_lasts_the_seconds_the_desktop_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;

    // The desktop's invitation: seconds take precedence over the hours an
    // older relay would read, for exactly the person and name it asked.
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
    assert_eq!(timed.role, MemberRole::Viewer);
    assert_eq!(timed.recipient_user_public_key, Some(guest));
    assert_eq!(timed.invitee_name.as_deref(), Some("Ada"));
    let lifetime: u64 = 600_000_000_000;
    assert!(
        (before.saturating_add(lifetime)..=after.saturating_add(lifetime))
            .contains(&timed.expires_at_ns),
        "{timed:?}"
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn invitations_run_through_the_rooms_own_methods() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

    // The owner invites: the workspace records a pending invitation.
    let invite = request(&workspace, governance::INVITE, json!({ "role": "editor" }));
    let issued = invoke(&host, workspace_id, &caller(&owner), invite.clone())
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
    let again = invoke(&host, workspace_id, &caller(&owner), invite)
        .await
        .unwrap();
    assert_eq!(again, issued);
    assert_eq!(
        workspaces.pending(&workspace, &owner).await.unwrap().len(),
        pending.len()
    );
    let listed = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, INVITATIONS, json!({})),
    )
    .await
    .unwrap();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|invitation| invitation["invite"] == invite_id.as_str())
    );

    // An Editor may not invite: the action does not exist for them.
    let refused = invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(&workspace, governance::INVITE, json!({ "role": "viewer" })),
    )
    .await;
    assert_eq!(refused, Err(ActionHostError::UnknownAction));

    // Cancelling takes it off the pending list.
    let mut cancel = Map::new();
    cancel.insert("invite".into(), invite_id.clone().into());
    invoke(
        &host,
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
        &host,
        workspace_id,
        &caller(&owner),
        request(
            "another-room",
            governance::INVITE,
            json!({ "role": "viewer" }),
        ),
    )
    .await;
    assert_eq!(elsewhere, Err(ActionHostError::UnknownAction));
    relay.shutdown().await;
}

#[tokio::test]
async fn an_editor_invites_up_to_her_own_role_and_no_further() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;

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
        matches!(&above, Err(ActionHostError::Refused { reason }) if reason.contains("Moderator")),
        "{above:?}"
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn destructive_governance_waits_for_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);

    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [31; 32],
                user_public_key: bo.user_public_key,
                role: MemberRole::Moderator,
            },
        )
        .await
        .unwrap();

    // Banning would remove Ada: it waits for Bo's own confirmation, which
    // a caller's word is not, and nothing changes.
    let mut ban = request(
        &workspace,
        governance::BAN,
        json!({ "person": person(&ada) }),
    );
    ban.confirmation = Some(Confirmation::Confirmed);
    assert_eq!(
        invoke(&host, workspace_id, &caller(&bo), ban).await,
        Err(ActionHostError::confirmation_required())
    );
    let remove = request(
        &workspace,
        governance::REMOVE_MEMBER,
        json!({ "person": person(&ada) }),
    );
    assert_eq!(
        invoke(&host, workspace_id, &caller(&bo), remove).await,
        Err(ActionHostError::confirmation_required())
    );
    let end = request(
        &workspace,
        governance::CALL,
        json!({ "operation": "end", "call": hex::encode([9; 32]), "epoch": 1 }),
    );
    assert_eq!(
        invoke(&host, workspace_id, &caller(&owner), end).await,
        Err(ActionHostError::confirmation_required())
    );
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.user_public_key == ada.user_public_key && !member.removed),
        "{members:?}"
    );

    // An Editor is not told a ban exists: unknown and forbidden answer alike.
    let ban = request(
        &workspace,
        governance::BAN,
        json!({ "person": person(&bo) }),
    );
    assert_eq!(
        invoke(&host, workspace_id, &caller(&ada), ban).await,
        Err(ActionHostError::UnknownAction)
    );

    relay.shutdown().await;
}

#[tokio::test]
async fn the_owners_own_request_confirms_removing_a_member() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let person = |cert: &DeviceCert| hex::encode(cert.user_public_key);

    let removed = invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(
            &workspace,
            governance::REMOVE_MEMBER,
            json!({ "person": person(&ada) }),
        ),
    )
    .await
    .unwrap();
    assert!(removed["generation"].as_u64().is_some(), "{removed}");
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .any(|member| member.user_public_key == ada.user_public_key && member.removed),
        "{members:?}"
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn a_host_without_apps_answers_only_its_rooms_governance() {
    let plane = GovernedPlane {
        governance: HostWorkspaceActions::new(),
        apps: None,
    };
    let someone = Caller::authenticated([1; 32], [2; 32]);
    let governance = RemoteActionMessage::Invoke(request("room", governance::INVITE, json!({})));
    let mut app = request("room", "archive", json!({}));
    app.target.provider = "app".into();
    assert!(plane.answers(&someone, &governance));
    assert!(!plane.answers(&someone, &RemoteActionMessage::Invoke(app)));
    // Nothing is attached: the workspace's actions are unknown, never run.
    let Some(answer) = plane.serve(7, &someone, governance) else {
        panic!("an invocation is answered");
    };
    drop(plane);
    let RemoteActionMessage::Receipt(receipt) = settled(answer).await else {
        panic!("a receipt");
    };
    assert_eq!(
        receipt.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );
}

#[tokio::test]
async fn a_member_who_may_install_bots_asks_the_rooms_host_for_one_and_takes_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let (workspace, workspace_id, host) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let standup = json!({ "entry": "so.tilo.bots.standup", "version": "1.2.0" });

    // An Editor by role alone is neither offered a bot nor runs the request.
    assert!(
        !offered(&host, workspace_id, &caller(&ada))
            .await
            .iter()
            .any(|verb| verb == super::REQUEST_BOT)
    );
    assert!(matches!(
        invoke(
            &host,
            workspace_id,
            &caller(&ada),
            request(&workspace, super::REQUEST_BOT, standup.clone()),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));
    assert!(
        workspaces
            .bot_requests(&workspace)
            .await
            .unwrap()
            .is_empty()
    );

    // Let install bots, Ada asks the workspace's host for one: every member reads
    // the request in the workspace's governance, and the host follows it.
    grant(
        workspaces,
        &workspace,
        &owner,
        &ada,
        governance::INSTALL_BOT,
    )
    .await;
    assert!(
        offered(&host, workspace_id, &caller(&ada))
            .await
            .iter()
            .any(|verb| verb == super::REQUEST_BOT)
    );
    invoke(
        &host,
        workspace_id,
        &caller(&ada),
        request(&workspace, super::REQUEST_BOT, standup),
    )
    .await
    .unwrap();
    let requests = workspaces.bot_requests(&workspace).await.unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].request.entry, "so.tilo.bots.standup");
    assert_eq!(requests[0].request.version, "1.2.0");
    assert_eq!(requests[0].requested_by, hex::encode(ada.user_public_key));

    // Taking it back needs uninstalling bots, which the owner has.
    let withdraw = json!({ "entry": "so.tilo.bots.standup" });
    assert!(matches!(
        invoke(
            &host,
            workspace_id,
            &caller(&ada),
            request(&workspace, super::WITHDRAW_BOT_REQUEST, withdraw.clone()),
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));
    invoke(
        &host,
        workspace_id,
        &caller(&owner),
        request(&workspace, super::WITHDRAW_BOT_REQUEST, withdraw),
    )
    .await
    .unwrap();
    assert!(
        workspaces
            .bot_requests(&workspace)
            .await
            .unwrap()
            .is_empty()
    );

    // A request names a directory entry and a version, nothing else.
    assert!(matches!(
        invoke(
            &host,
            workspace_id,
            &caller(&owner),
            request(
                &workspace,
                super::REQUEST_BOT,
                json!({ "entry": "../standup", "version": "1.2.0" })
            ),
        )
        .await,
        Err(ActionHostError::InvalidRequest { .. })
    ));
    relay.shutdown().await;
}

/// A member gives the host only a secret a connector is known to take, and
/// a request's inputs are text settings by name.
#[test]
fn connector_secrets_are_the_known_ones_and_request_inputs_are_text() {
    let args = |value: Value| {
        super::Args::of(&InvokeArgument::Json(value.as_object().unwrap().clone())).unwrap()
    };
    for wrong in [
        json!({ "app": "so.tilo.mail", "purpose": "bot-token", "secret": "x" }),
        json!({ "app": "so.tilo.discord", "purpose": "password", "secret": "x" }),
        json!({ "app": "so.tilo.discord", "purpose": "bot-token", "secret": "  " }),
    ] {
        assert!(
            super::connector_secret(&args(wrong.clone())).is_err(),
            "{wrong}"
        );
    }
    let inputs = args(json!({ "inputs": { "guild": "123", "server": "Garden club" } }))
        .text_map("inputs")
        .unwrap();
    assert_eq!(inputs.get("guild").map(String::as_str), Some("123"));
    assert!(
        args(json!({ "inputs": { "guild": 123 } }))
            .text_map("inputs")
            .is_err()
    );
    assert!(args(json!({})).text_map("inputs").unwrap().is_empty());
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
    host: &HostWorkspaceActions,
    workspace_id: u64,
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
        Err(ActionHostError::UnknownAction)
    ));
    assert!(workspaces.app_versions(workspace).await.unwrap().is_empty());
}

/// Once the owner lets `developer` install apps, its own build runs, recorded
/// with who signed and installed it; the next push replaces it, so one version
/// runs at a time; and a release that does not name its hashes in full is
/// refused.
async fn a_developer_installs_its_own_build(
    workspaces: &Workspaces,
    host: &HostWorkspaceActions,
    workspace_id: u64,
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
    assert_eq!(running[0].release.surface, hex::encode([2; 32]));

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
        Err(ActionHostError::InvalidRequest { .. })
    ));
}

/// The workspace chooses the version of an app every member runs. An Editor by
/// role alone may not; a member the owner lets install apps (a developer)
/// installs their own build, which the workspace records with who signed and
/// installed it, and taking the choice back lets each member run their own.
#[tokio::test]
async fn a_developer_chooses_the_version_of_an_app_the_whole_room_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
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
    relay.shutdown().await;
}
