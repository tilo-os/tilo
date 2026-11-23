//! Tenant isolation on one hosted server.
//!
//! A hosted server governs workspaces for people who share nothing but the host:
//! two tenants, each its own governed workspace with its own members. These
//! tests stand the real server up with two such workspaces and drive every entry a
//! member, an agent or a compute client has, asserting that one tenant
//! cannot read, write, enumerate, administer or invoke anything in the
//! other's workspace, that an agent holds only what its delegation names and
//! never governs, and that a removed member loses access at once. Each
//! refusal is checked against the other workspace's state afterwards, so an entry
//! that answered "no" but acted anyway still fails.
//!
//! The suite is the `tenant_isolation::` module of the server's `it` target.

#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose server and sessions live until it ends"
)]

use std::path::Path;
use std::time::Duration;

use tilo_action_policy::{Answer, Grant, Grantee, KindSet, roles, roles::governance};
use tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_INVITATIONS,
    workspace_governance_child, workspace_governance_resource,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::join::Role;
use tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_server::workspaces::{
    CREATE_WORKSPACE, ENTER_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    IssueInvitationRequest, IssuedInvitation, LIST_WORKSPACES, RemoveMemberRequest,
    WorkspaceSummary, Workspaces,
};
use tilo_server::{Envelope, Server, ServerClient, ServerConfig};
use tilo_server_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, device_endpoint, invitation, redeem};
use tilo_server::WorkspaceId;

/// How long a refused write is given to surface anywhere before its absence
/// counts as evidence.
const QUIET: Duration = Duration::from_millis(500);

async fn connect(server: &Server, device: &TestDevice) -> ServerClient {
    try_connect(server, device).await.unwrap()
}

/// A connection the server may refuse at its handshake.
async fn try_connect(
    server: &Server,
    device: &TestDevice,
) -> Result<ServerClient, tilo_server::ClientError> {
    ServerClient::connect_to_pinned_host(
        device_endpoint(device).await,
        server.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
}

/// The server's first answer to `client` subscribing to `workspace_id`.
async fn subscribe(client: &ServerClient, workspace_id: WorkspaceId) -> Option<Envelope> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the server answers the subscription", async {
        loop {
            match client.recv().await {
                Some(answer @ (Envelope::Ready { .. } | Envelope::NotAuthorised { .. })) => {
                    break Some(answer);
                }
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await
}

/// A session of `device` that the server admitted to `workspace_id`.
async fn subscribed(
    server: &Server,
    device: &TestDevice,
    workspace_id: WorkspaceId,
) -> ServerClient {
    let client = connect(server, device).await;
    assert!(
        matches!(
            subscribe(&client, workspace_id).await,
            Some(Envelope::Ready { .. })
        ),
        "the workspace admits its own member"
    );
    client
}

/// Admit `member`'s person to `workspace` as `role`, through an invitation the
/// server's own device issues and `member`'s device redeems.
async fn admit(
    server: &Server,
    host: &TestDevice,
    member: &TestDevice,
    workspace: &str,
    role: Role,
) {
    let issued = invitation(server.workspaces().unwrap(), host, workspace, role, member).await;
    redeem(member, &issued).await.unwrap();
}

/// What a workspace's governance holds, as far as any entry could change it.
async fn governance(workspaces: &Workspaces, host: &TestDevice, workspace: &str) -> String {
    let (generation, members) = workspaces.membership(workspace).await.unwrap();
    let pending = workspaces.pending(workspace, &host.cert).await.unwrap();
    let summary = workspaces.summary(workspace).await.unwrap();
    format!("{generation} {members:?} {pending:?} {:?}", summary.members)
}

/// `device` asks the server to remove `victim` from `workspace`: it must be
/// refused.
async fn removal_refused(
    who: &str,
    device: &TestDevice,
    server: &Server,
    workspace: &WorkspaceSummary,
    victim: [u8; 32],
) {
    let answer = server
        .workspaces()
        .unwrap()
        .remove_member(
            &workspace.workspace,
            &device.cert,
            RemoveMemberRequest {
                request_id: [0xA4; 32],
                user_public_key: victim,
            },
        )
        .await;
    assert!(
        answer.is_err(),
        "{who} must be refused removing a member of {}",
        workspace.name
    );
}

/// An invocation of `action` on `resource` of `provider` with `argument`.
fn invocation(
    provider: &str,
    resource: String,
    action: &str,
    argument: serde_json::Value,
) -> RemoteActionMessage {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address: ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// Whether `answer` is a receipt of an action that ran.
fn completed(answer: &RemoteActionMessage) -> bool {
    matches!(
        answer,
        RemoteActionMessage::Receipt(receipt)
            if matches!(receipt.status, ReceiptStatus::Completed { .. })
    )
}

/// Whether `answer` is a receipt of an action the server refused.
fn rejected(answer: &RemoteActionMessage) -> bool {
    matches!(
        answer,
        RemoteActionMessage::Receipt(receipt)
            if matches!(receipt.status, ReceiptStatus::Rejected { .. })
    )
}

/// A retained delta carrying `marker`, as a device publishes one.
fn retained_delta(marker: &[u8]) -> Vec<u8> {
    tilo_server::coedit::CoeditPayload::RetainedDelta {
        channel: [9; 32],
        body: marker.to_vec(),
    }
    .encode()
    .unwrap()
}

fn persisted(root: &Path, marker: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            persisted(&path, marker)
        } else {
            std::fs::read(&path).is_ok_and(|bytes| carries(&bytes, marker))
        }
    })
}

/// Everything `client` receives until it has been quiet for [`QUIET`], or
/// its session ends.
async fn received_until_quiet(client: &ServerClient) -> Vec<Envelope> {
    let mut received = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(QUIET, client.recv()).await {
        received.push(envelope);
        assert!(received.len() < 10_000, "an unbounded stream of envelopes");
    }
    received
}

fn carries(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.windows(marker.len()).any(|window| window == marker)
}

/// Whatever `client` receives within [`QUIET`], or `None`.
async fn next_within_quiet(client: &ServerClient) -> Option<Envelope> {
    tokio::time::timeout(QUIET, client.recv())
        .await
        .ok()
        .flatten()
}

/// A remote action from `client` to the server's own device `host`, in
/// `workspace_id`.
fn to_host(
    client: &ServerClient,
    host: &TestDevice,
    workspace_id: WorkspaceId,
    message: RemoteActionMessage,
) {
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .unwrap();
}

/// The server's answer to the remote action `client` sent last.
async fn action_answer(client: &ServerClient) -> RemoteActionMessage {
    tilo_test_env::within("the server answers the action", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::RemoteAction { frame, .. } => {
                    break RemoteActionFrame::decode(&frame).unwrap().message;
                }
                Envelope::RemoteActionUndeliverable { refusal, .. } => {
                    panic!("the server did not answer: {refusal}")
                }
                _ => {}
            }
        }
    })
    .await
}

fn invite_action(workspace: &str) -> RemoteActionMessage {
    let mut argument = serde_json::Map::new();
    argument.insert("role".into(), "viewer".into());
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, "invite"),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// The governance resources `client`'s catalogue lists in `workspace_id`.
async fn governance_resources(
    client: &ServerClient,
    host: &TestDevice,
    workspace_id: WorkspaceId,
) -> Vec<String> {
    to_host(
        client,
        host,
        workspace_id,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
                ..CatalogueQuery::default()
            },
        },
    );
    let RemoteActionMessage::CatalogueResponse { result, .. } = action_answer(client).await else {
        panic!("a catalogue request is answered with a page");
    };
    result
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.resource.id)
        .collect()
}

/// A hosted server with two tenants' workspaces: Acme, where Ann invites, and
/// Globex, where Bob edits and an invitation for Carol waits.
struct TwoTenants {
    state: tempfile::TempDir,
    provider: TestUser,
    host: TestDevice,
    server: Server,
    acme: WorkspaceSummary,
    globex: WorkspaceSummary,
    ann: TestUser,
    ann_laptop: TestDevice,
    bob: TestUser,
    bob_phone: TestDevice,
    carol_phone: TestDevice,
    carol_invitation: IssuedInvitation,
}

impl TwoTenants {
    async fn start() -> Self {
        let provider = TestUser::fresh("Provider");
        let host = provider.enrol("hosted-server", DeviceCapabilities::member());
        let ann = TestUser::fresh("Ann of Acme");
        let ann_laptop = ann.enrol("ann-laptop", DeviceCapabilities::primary());
        let bob = TestUser::fresh("Bob of Globex");
        let bob_phone = bob.enrol("bob-phone", DeviceCapabilities::member());
        let carol = TestUser::fresh("Carol, invited to Globex");
        let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let server = crate::common::start_server(
            ServerConfig::loopback(state.path().to_path_buf()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: provider.user_pk,
                device_signing_key: host.sk.clone(),
                device_cert: host.cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let workspaces = server.workspaces().unwrap();
        let acme = workspaces.create("Acme").await.unwrap();
        let globex = workspaces.create("Globex").await.unwrap();
        assert_ne!(acme.workspace_id, globex.workspace_id);

        // Ann invites in Acme: the owner grants her the workspace's `invite`
        // governance action, as an owner hands a member a part of running
        // their workspace.
        admit(&server, &host, &ann_laptop, &acme.workspace, Role::Editor).await;
        let who = hex::encode(ann.user_pk);
        let grant = Grant::new(
            format!("tenant-isolation:invite:{who}"),
            Grantee::person(who),
            roles::governance_action(&acme.workspace, governance::INVITE),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person(hex::encode(provider.user_pk)),
        );
        workspaces
            .put_workspace_grants(&acme.workspace, &host.cert, [0x11; 32], vec![grant])
            .await
            .unwrap();
        admit(&server, &host, &bob_phone, &globex.workspace, Role::Editor).await;
        let carol_invitation = invitation(
            workspaces,
            &host,
            &globex.workspace,
            Role::Editor,
            &carol_phone,
        )
        .await;
        Self {
            state,
            provider,
            host,
            server,
            acme,
            globex,
            ann,
            ann_laptop,
            bob,
            bob_phone,
            carol_phone,
            carol_invitation,
        }
    }

    fn workspaces(&self) -> Workspaces {
        self.server.workspaces().unwrap().clone()
    }

    async fn globex_governance(&self) -> String {
        governance(&self.workspaces(), &self.host, &self.globex.workspace).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_tenants_inviter_reaches_nothing_in_another_tenants_workspace() {
    // The fixture's setup future is larger than the lint's ceiling on the stack: it builds two
    // tenants' people, their devices and two governed workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let workspaces = t.workspaces();
    let before = t.globex_governance().await;
    let ann = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;

    // Positive control: Ann governs her own workspace through the same actions.
    let own = invocation(
        WORKSPACE_GOVERNANCE_PROVIDER,
        workspace_governance_child(&t.acme.workspace, Some(WORKSPACE_INVITATIONS)),
        "list",
        serde_json::json!({}),
    );
    to_host(&ann, &t.host, t.acme.workspace_id, own);
    let own = action_answer(&ann).await;
    assert!(
        completed(&own),
        "Ann lists her own workspace's invitations: {own:?}"
    );

    // Globex's governance, the host's own operations on Globex and a
    // member's own business there: every one rejected, from Ann's workspace.
    let globex = t.globex.workspace.as_str();
    let probes = [
        (
            WORKSPACE_GOVERNANCE_PROVIDER,
            workspace_governance_child(globex, Some(WORKSPACE_INVITATIONS)),
            "invite",
            serde_json::json!({ "role": "viewer" }),
        ),
        (
            WORKSPACE_GOVERNANCE_PROVIDER,
            workspace_governance_child(globex, Some(WORKSPACE_INVITATIONS)),
            "list",
            serde_json::json!({}),
        ),
        (
            WORKSPACE_GOVERNANCE_PROVIDER,
            workspace_governance_child(globex, Some(WORKSPACE_INVITATIONS)),
            "cancel",
            serde_json::json!({ "invite": hex::encode(t.carol_invitation.invite_id) }),
        ),
        (
            WORKSPACE_GOVERNANCE_PROVIDER,
            workspace_governance_resource(globex),
            governance::UNBAN,
            serde_json::json!({ "person": hex::encode(t.bob.user_pk) }),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_WORKSPACES_RESOURCE.to_owned(),
            LIST_WORKSPACES,
            serde_json::json!({}),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_WORKSPACES_RESOURCE.to_owned(),
            CREATE_WORKSPACE,
            serde_json::json!({ "name": "Squatter" }),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_WORKSPACES_RESOURCE.to_owned(),
            ENTER_WORKSPACE,
            serde_json::json!({ "workspace": globex }),
        ),
    ];
    for (provider, resource, action, argument) in probes {
        to_host(
            &ann,
            &t.host,
            t.acme.workspace_id,
            invocation(provider, resource, action, argument),
        );
        let answer = action_answer(&ann).await;
        assert!(
            rejected(&answer),
            "Ann ran {provider} {action} on Globex: {answer:?}"
        );
    }

    // Removing a member is refused.
    removal_refused(
        "Acme's inviter",
        &t.ann_laptop,
        &t.server,
        &t.globex,
        t.bob.user_pk,
    )
    .await;

    // Nothing of that changed Globex.
    assert_eq!(t.globex_governance().await, before);
    assert_eq!(workspaces.list().await.len(), 2, "no workspace was created");

    // An invitation Globex issued to someone else admits nobody else.
    assert!(
        redeem(&t.ann_laptop, &t.carol_invitation).await.is_err(),
        "an invitation bound to Carol's person admits Ann"
    );
    assert_eq!(t.globex_governance().await, before);
    // Carol herself still redeems it: the refusal was about Ann, not the
    // invitation.
    redeem(&t.carol_phone, &t.carol_invitation).await.unwrap();
    t.server.shutdown().await;
    drop(t.state);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_tenants_member_cannot_read_write_or_invoke_in_another_tenants_workspace() {
    // The fixture's setup future is larger than the lint's ceiling on the stack: it builds two
    // tenants' people, their devices and two governed workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let before = t.globex_governance().await;
    let bob_in_globex = subscribed(&t.server, &t.bob_phone, t.globex.workspace_id).await;
    received_until_quiet(&bob_in_globex).await;

    // Reading: Ann's subscription to Globex is refused outright.
    let ann_probe = connect(&t.server, &t.ann_laptop).await;
    assert!(
        matches!(
            subscribe(&ann_probe, t.globex.workspace_id).await,
            Some(Envelope::NotAuthorised { .. }) | None
        ),
        "Ann reads Globex"
    );
    drop(ann_probe);

    // Writing: a delta Ann addresses to Globex from her own workspace's session
    // reaches no Globex member and is persisted nowhere on the server. (The
    // server may end the session that sent it; that is a refusal too.)
    let marker = b"acme-writes-into-globex-7f3c";
    let ann_writer = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    ann_writer
        .send(Envelope::Delta {
            workspace_id: t.globex.workspace_id,
            author_device_public_key: t.ann_laptop.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: retained_delta(marker),
            signature: [0; 64],
        })
        .unwrap();
    let leaked = received_until_quiet(&bob_in_globex).await;
    assert!(
        !leaked.iter().any(|envelope| matches!(
            envelope,
            Envelope::Delta { delta, .. } if carries(delta, marker)
        )),
        "Bob received Ann's write: {leaked:?}"
    );
    assert!(
        !persisted(t.state.path(), marker),
        "Ann's write was persisted"
    );
    drop(ann_writer);

    // Invoking: Ann's catalogue in her own workspace lists Acme's governance and
    // nothing of Globex's, and invoking Globex's invite from her workspace does
    // nothing.
    let ann_in_acme = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    let resources = governance_resources(&ann_in_acme, &t.host, t.acme.workspace_id).await;
    assert!(
        resources.contains(&workspace_governance_resource(&t.acme.workspace)),
        "Ann's own workspace offers her its governance: {resources:?}"
    );
    assert!(
        !resources.contains(&workspace_governance_resource(&t.globex.workspace)),
        "Ann's catalogue enumerates Globex: {resources:?}"
    );
    to_host(
        &ann_in_acme,
        &t.host,
        t.acme.workspace_id,
        invite_action(&t.globex.workspace),
    );
    let RemoteActionMessage::Receipt(receipt) = action_answer(&ann_in_acme).await else {
        panic!("an invocation is answered with a receipt");
    };
    assert!(
        matches!(receipt.status, ReceiptStatus::Rejected { .. }),
        "Ann invited into Globex: {:?}",
        receipt.status
    );
    drop(ann_in_acme);

    // Addressed to Globex's workspace, the same action is not run.
    let ann_foreign = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    to_host(
        &ann_foreign,
        &t.host,
        t.globex.workspace_id,
        invite_action(&t.globex.workspace),
    );
    let answer = tilo_test_env::within("the server answers Ann's foreign action", async {
        loop {
            match ann_foreign.recv().await {
                Some(
                    answer @ (Envelope::RemoteAction { .. }
                    | Envelope::RemoteActionUndeliverable { .. }
                    | Envelope::NotAuthorised { .. }),
                ) => break Some(answer),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await;
    if let Some(Envelope::RemoteAction { frame, .. }) = answer {
        let message = RemoteActionFrame::decode(&frame).unwrap().message;
        assert!(
            !matches!(
                &message,
                RemoteActionMessage::Receipt(receipt)
                    if matches!(receipt.status, ReceiptStatus::Completed { .. })
            ),
            "Ann invoked an action in Globex's workspace: {message:?}"
        );
    }
    drop(ann_foreign);

    // Nor may Ann address a Globex member's device directly.
    let ann_direct = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    ann_direct
        .send(Envelope::RemoteAction {
            workspace_id: t.globex.workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: t.bob_phone.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(invite_action(&t.globex.workspace))
                .encode()
                .unwrap(),
        })
        .unwrap();
    let leaked = received_until_quiet(&bob_in_globex).await;
    assert!(
        !leaked
            .iter()
            .any(|envelope| matches!(envelope, Envelope::RemoteAction { .. })),
        "Bob received Ann's action: {leaked:?}"
    );
    assert_eq!(t.globex_governance().await, before);
    t.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_member_loses_access_at_once_and_other_tenants_keep_theirs() {
    // The fixture's setup future is larger than the lint's ceiling on the stack: it builds two
    // tenants' people, their devices and two governed workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let ann_in_acme = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    let bob_in_globex = subscribed(&t.server, &t.bob_phone, t.globex.workspace_id).await;
    crate::common::wait_until_subscribed(
        &t.server,
        t.globex.workspace_id,
        &t.bob_phone.device_pubkey(),
    )
    .await;

    t.workspaces()
        .remove_member(
            &t.globex.workspace,
            &t.host.cert,
            RemoveMemberRequest {
                request_id: [0x21; 32],
                user_public_key: t.bob.user_pk,
            },
        )
        .await
        .unwrap();

    // `remove_member` returns once the server stops admitting Bob: his very
    // next write is refused, never fanned out or kept.
    let marker = b"removed-member-writes-9d21";
    bob_in_globex
        .send(Envelope::Delta {
            workspace_id: t.globex.workspace_id,
            author_device_public_key: t.bob_phone.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: retained_delta(marker),
            signature: [0; 64],
        })
        .ok();
    let answer = tilo_test_env::within("the server answers the removed member", async {
        loop {
            match bob_in_globex.recv().await {
                Some(answer @ Envelope::NotAuthorised { .. }) => break Some(answer),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await;
    assert!(
        matches!(answer, Some(Envelope::NotAuthorised { .. }) | None),
        "{answer:?}"
    );
    assert!(
        !persisted(t.state.path(), marker),
        "a removed member's write was kept"
    );
    tilo_test_env::within("the removed member's subscription ends", async {
        while t
            .server
            .is_subscribed(t.globex.workspace_id, &t.bob_phone.device_pubkey())
            .await
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // A fresh connection is not admitted either: the handshake or the
    // subscription refuses it.
    if let Ok(again) = try_connect(&t.server, &t.bob_phone).await {
        let answer = subscribe(&again, t.globex.workspace_id).await;
        assert!(
            matches!(answer, Some(Envelope::NotAuthorised { .. }) | None),
            "a removed member subscribed again: {answer:?}"
        );
    }

    // Acme is untouched by Globex's decision.
    assert!(
        t.server
            .is_subscribed(t.acme.workspace_id, &t.ann_laptop.device_pubkey())
            .await
    );
    assert!(
        !matches!(
            next_within_quiet(&ann_in_acme).await,
            Some(Envelope::NotAuthorised { .. })
        ),
        "Globex's removal cut Acme's member"
    );
    t.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_never_governs_and_reaches_no_workspace_outside_its_delegation() {
    // The fixture's setup future is larger than the lint's ceiling on the stack: it builds two
    // tenants' people, their devices and two governed workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let before = t.globex_governance().await;
    // The host's own agent: a device of the server's person with no
    // capabilities, delegated Acme alone.
    let agent = t
        .provider
        .enrol("acme-agent", DeviceCapabilities::nothing());
    let issued = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &t.provider.user_sk,
        agent.device_pubkey(),
        Delegate::Agent {
            agent_id: "acme-helper".into(),
        },
        "Acme helper",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
            workspaces: vec![t.acme.workspace_id],
            apps: Vec::new(),
        },
        issued,
        issued.checked_add(600_000_000_000).unwrap(),
    )
    .unwrap();
    t.server
        .register_agent(&agent.cert, delegation)
        .await
        .unwrap();
    // Registering the agent puts it on the roster of the workspace its delegation
    // names, which is the roster's business and no one else's: the workspace the
    // delegation does not name never hears of it.
    let acme_joined = governance(&t.workspaces(), &t.host, &t.acme.workspace).await;
    if cfg!(feature = "agents") {
        let roster = t.workspaces().agents(&t.acme.workspace).await.unwrap();
        assert_eq!(roster.len(), 1, "the agent's place on Acme's roster");
        assert_eq!(roster[0].device_public_key, agent.device_pubkey());
        assert!(
            t.workspaces()
                .agents(&t.globex.workspace)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // An agent governs nothing, not even the workspace it is delegated, and it
    // is no host person: the one decision every entry asks refuses it
    // removing anyone, the host's operations and either workspace's governance.
    removal_refused("the agent", &agent, &t.server, &t.acme, t.ann.user_pk).await;
    removal_refused("the agent", &agent, &t.server, &t.globex, t.bob.user_pk).await;
    let workspaces = t.workspaces();
    for workspace in [&t.acme.workspace, &t.globex.workspace] {
        let expires_at_ns = tilo_clock::unix_now_ns()
            .checked_add(600_000_000_000)
            .unwrap();
        let issued = workspaces
            .issue(
                workspace,
                &agent.cert,
                IssueInvitationRequest {
                    request_id: [0xB1; 32],
                    role: Role::Viewer,
                    recipient_user_public_key: None,
                    invitee_name: None,
                    inviter_name: None,
                    expires_at_ns,
                },
            )
            .await;
        assert!(issued.is_err(), "the agent invited into {workspace}");
        let administrator = workspaces
            .set_member_administrator(workspace, &agent.cert, [0xB2; 32], t.ann.user_pk, None)
            .await;
        assert!(
            administrator.is_err(),
            "the agent made an administrator in {workspace}"
        );
        let removed = workspaces
            .remove_member(
                workspace,
                &agent.cert,
                RemoveMemberRequest {
                    request_id: [0xB3; 32],
                    user_public_key: t.bob.user_pk,
                },
            )
            .await;
        assert!(
            removed.is_err(),
            "the agent removed a member of {workspace}"
        );
    }
    assert_eq!(t.globex_governance().await, before);
    assert_eq!(
        governance(&t.workspaces(), &t.host, &t.acme.workspace).await,
        acme_joined
    );
    assert_eq!(
        t.workspaces().list().await.len(),
        2,
        "the agent created a workspace"
    );

    // Outside its delegation it is not admitted to a workspace at all.
    let probe = connect(&t.server, &agent).await;
    assert!(
        matches!(
            subscribe(&probe, t.globex.workspace_id).await,
            Some(Envelope::NotAuthorised { .. }) | None
        ),
        "the agent reads a workspace its delegation does not name"
    );
    t.server.shutdown().await;
}
