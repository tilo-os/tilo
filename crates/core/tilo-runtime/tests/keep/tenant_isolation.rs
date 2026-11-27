//! Tenant isolation on one hosted server.
//!
//! A hosted server serves workspaces for people who share nothing but the host:
//! two tenants, each its own hosted workspace with its own members. These
//! tests stand the real server up with two such workspaces and drive every entry a
//! member, an agent or a compute client has, asserting that one tenant
//! cannot read, write, enumerate, administer or invoke anything in the
//! other's workspace, that an agent holds only what its grant names and
//! never administers, and that a removed member loses access at once. Each
//! refusal is checked against the other workspace's state afterwards, so an entry
//! that answered "no" but acted anyway still fails.
//!
//! The suite is the `tenant_isolation::` module of the server's `it` target.

#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose server and sessions live until it ends"
)]

use std::time::Duration;

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use tilo_action_policy::{Answer, Grant, Grantee, roles, roles::governance};
use tilo_action_types::KindSet;
use tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_INVITATIONS,
    workspace_governance_child, workspace_governance_resource,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::join::Role;
use tilo_runtime::device::keep::server::{KeptServer, Server};
use tilo_server_protocol::config::ServerConfig;
use tilo_user_identity::{AgentScope, DeviceCapabilities, RevocationList};
use tilo_workspace_service::workspaces::{
    CREATE_WORKSPACE, ENTER_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    IssueInvitationRequest, IssuedInvitation, LIST_WORKSPACES, RemoveMemberRequest,
    WorkspaceSummary, Workspaces,
};

use crate::keep::common::{KEYS, Linked, TestDevice, TestUser, invitation, opened, redeem};
use tilo_server_protocol::WorkspaceId;

/// The app and record a tenant writes through its link, and the field it writes.
const APP: &str = "test.app";
const RECORD: &str = "note:1";

/// How long a refused write is given to surface anywhere before its absence
/// counts as evidence.
const QUIET: Duration = Duration::from_millis(500);

/// The workspace through which `device` is a member, its link to it, or the
/// typed refusal the host gave.
async fn subscribed(server: &Server, device: &TestDevice, workspace_id: WorkspaceId) -> Linked {
    Linked::open(server, device, workspace_id)
        .await
        .expect("the workspace admits its own member")
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
        asks: false,
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

/// The record a device wrote through its link, as `workspace`'s state holds it.
async fn written(workspaces: &Workspaces, workspace: &str) -> Option<Vec<u8>> {
    let state = workspaces.state(workspace).await?;
    opened(&state, &KEYS, (APP, RECORD))
}

/// Whether the record carries `marker`.
fn marked(bytes: &[u8], marker: &str) -> bool {
    AutoCommit::load(bytes).is_ok_and(|doc| {
        doc.get(ROOT, "marker")
            .ok()
            .flatten()
            .is_some_and(|(value, _)| value.as_str() == Some(marker))
    })
}

/// Whatever `linked` receives within [`QUIET`], or `None`.
async fn next_within_quiet(linked: &mut Linked) -> Option<tilo_peer_state::direct::Heard> {
    tokio::time::timeout(QUIET, linked.next())
        .await
        .ok()
        .flatten()
}

/// The host's answer to `message`, asked as `linked`'s device in `linked`'s workspace.
async fn action_answer(linked: &Linked, message: RemoteActionMessage) -> RemoteActionMessage {
    tilo_test_env::within(
        "the server answers the action",
        linked.actions.ask_message(message),
    )
    .await
    .expect("the server answers")
    .expect("the server answers the action")
}

fn invite_action(workspace: &str) -> RemoteActionMessage {
    let mut argument = serde_json::Map::new();
    argument.insert("role".into(), "viewer".into());
    RemoteActionMessage::Invoke(InvokeRequest {
        asks: false,
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

/// The governance resources `linked`'s catalogue lists.
async fn governance_resources(linked: &Linked, user: [u8; 32]) -> Vec<String> {
    use tilo_actions::remote::ActionProvider;
    let page = tilo_test_env::within(
        "the host answers the catalogue",
        ActionProvider::query(
            &linked.actions,
            &linked.caller(user),
            CatalogueQuery {
                provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
                ..CatalogueQuery::default()
            },
        ),
    )
    .await
    .expect("the host answers")
    .expect("a catalogue request is answered with a page");
    page.rows.into_iter().map(|row| row.resource.id).collect()
}

/// A hosted server with two tenants' workspaces: Acme, where Ann invites, and
/// Globex, where Bob edits and an invitation for Carol waits.
struct TwoTenants {
    state: tempfile::TempDir,
    provider: TestUser,
    host: TestDevice,
    server: KeptServer,
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
        let server = crate::keep::common::start_server(
            ServerConfig::loopback(),
            state.path().to_path_buf(),
            tilo_runtime::device::keep::server::ServerIdentity {
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

        // Ann invites in Acme: the owner grants her the workspace's `add-member`
        // governance action, as an owner hands a member a part of running
        // their workspace.
        admit(&server, &host, &ann_laptop, &acme.workspace, Role::Editor).await;
        let who = hex::encode(ann.user_pk);
        let grant = Grant::new(
            format!("tenant-isolation:invite:{who}"),
            Grantee::person(who),
            roles::governance_action(&acme.workspace, governance::ADD_MEMBER),
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
    // tenants' people, their devices and two hosted workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let workspaces = t.workspaces();
    let before = t.globex_governance().await;
    let ann = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;

    // Positive control: Ann administers her own workspace through the same actions.
    let own = invocation(
        WORKSPACE_GOVERNANCE_PROVIDER,
        workspace_governance_child(&t.acme.workspace, Some(WORKSPACE_INVITATIONS)),
        "list",
        serde_json::json!({}),
    );
    let own = action_answer(&ann, own).await;
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
        let answer = action_answer(&ann, invocation(provider, resource, action, argument)).await;
        assert!(
            rejected(&answer),
            "Ann ran {provider} {action} on Globex: {answer:?}"
        );
    }

    // The host's list of workspaces answers Ann with her own alone: Acme, and nothing of
    // Globex, neither its name, its id nor a row that counts it.
    let listed = action_answer(
        &ann,
        invocation(
            HOST_ACTIONS_PROVIDER,
            HOST_WORKSPACES_RESOURCE.to_owned(),
            LIST_WORKSPACES,
            serde_json::json!({}),
        ),
    )
    .await;
    let RemoteActionMessage::Receipt(receipt) = &listed else {
        panic!("the host answers Ann's list with a receipt: {listed:?}");
    };
    let ReceiptStatus::Completed { output } = &receipt.status else {
        panic!("Ann lists the workspaces she is in: {listed:?}");
    };
    let rows = output
        .value
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .expect("the list is a list of workspaces");
    let ids: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["workspace"].as_str())
        .collect();
    assert_eq!(
        ids,
        [t.acme.workspace.as_str()],
        "Ann's own workspace alone: {rows:?}"
    );
    let text = serde_json::to_string(rows).unwrap();
    assert!(
        !text.contains(globex) && !text.contains("Globex"),
        "nothing of Globex reaches Ann: {text}"
    );

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
    // tenants' people, their devices and two hosted workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let before = t.globex_governance().await;
    let bob_in_globex = subscribed(&t.server, &t.bob_phone, t.globex.workspace_id).await;

    // Reading: Ann's link to Globex is refused outright.
    let ann_probe = Linked::open(&t.server, &t.ann_laptop, t.globex.workspace_id).await;
    assert!(ann_probe.is_err(), "Ann reads Globex");

    // Writing: a record Ann writes in her own workspace reaches no Globex
    // member and is kept nowhere in Globex's state. (The link she would write
    // through does not open; this is the write she would have made.)
    let ann_writer = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    ann_writer
        .write(&t.ann_laptop.sk, (APP, RECORD), |doc| {
            doc.put(ROOT, "marker", "acme-writes-into-globex-7f3c")
                .unwrap();
        })
        .unwrap();
    let workspaces_for_write = t.workspaces();
    let leak = tokio::time::timeout(QUIET, async {
        loop {
            let leaked = written(&workspaces_for_write, &t.globex.workspace).await;
            assert!(
                !leaked.is_some_and(|bytes| marked(&bytes, "acme-writes-into-globex-7f3c")),
                "Ann's write was kept in Globex"
            );
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(leak.is_err(), "Ann's write was kept in Globex");
    let leaked = bob_in_globex.opened((APP, RECORD));
    assert!(
        !leaked.is_some_and(|bytes| marked(&bytes, "acme-writes-into-globex-7f3c")),
        "Bob received Ann's write"
    );
    drop(ann_probe);

    // Invoking: Ann's catalogue in her own workspace lists Acme's governance and
    // nothing of Globex's, and invoking Globex's invite from her workspace does
    // nothing.
    let ann_in_acme = ann_writer
        .reopen(&t.server, t.acme.workspace_id)
        .await
        .expect("the workspace admits Ann's device again");
    let resources = governance_resources(&ann_in_acme, t.ann.user_pk).await;
    assert!(
        resources.contains(&workspace_governance_resource(&t.acme.workspace)),
        "Ann's own workspace offers her its governance: {resources:?}"
    );
    assert!(
        !resources.contains(&workspace_governance_resource(&t.globex.workspace)),
        "Ann's catalogue enumerates Globex: {resources:?}"
    );
    let RemoteActionMessage::Receipt(receipt) =
        action_answer(&ann_in_acme, invite_action(&t.globex.workspace)).await
    else {
        panic!("an invocation is answered with a receipt");
    };
    assert!(
        matches!(receipt.status, ReceiptStatus::Rejected { .. }),
        "Ann invited into Globex: {:?}",
        receipt.status
    );
    drop(ann_in_acme);

    // Addressed to Globex's workspace, the same action is not run.
    let ann_foreign = ann_writer
        .reopen(&t.server, t.acme.workspace_id)
        .await
        .expect("the workspace admits Ann's device again");
    let globex_actions = ann_foreign
        .asking_in(&t.globex.workspace)
        .await
        .expect("the host answers its actions link");
    let answer = tilo_test_env::within(
        "the server answers Ann's foreign action",
        globex_actions.ask_message(invite_action(&t.globex.workspace)),
    )
    .await
    .and_then(Result::ok);
    assert!(
        !matches!(
            answer,
            Some(RemoteActionMessage::Receipt(ref receipt))
                if matches!(receipt.status, ReceiptStatus::Completed { .. })
        ),
        "Ann invoked an action in Globex's workspace: {answer:?}"
    );
    drop(globex_actions);
    drop(ann_foreign);

    // Nor may Ann address a Globex member's device directly: she asks in
    // Globex's workspace, addressed to Bob's device, and the host refuses her
    // before it passes anything on.
    let ann_direct = ann_writer
        .reopen(&t.server, t.acme.workspace_id)
        .await
        .expect("the workspace admits Ann's device again");
    let to_bob = ann_direct
        .asking_device_in(&t.globex.workspace, t.bob_phone.device_pubkey())
        .await
        .expect("the host answers its actions link");
    let direct = tilo_test_env::within(
        "the server answers Ann's direct action",
        to_bob.ask_message(invite_action(&t.globex.workspace)),
    )
    .await
    .and_then(Result::ok);
    assert!(
        !matches!(
            direct,
            Some(RemoteActionMessage::Receipt(ref receipt))
                if matches!(receipt.status, ReceiptStatus::Completed { .. })
        ),
        "Ann reached Bob's device in Globex: {direct:?}"
    );
    drop(to_bob);
    drop(ann_direct);
    drop(ann_writer);
    assert_eq!(t.globex_governance().await, before);
    t.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_member_loses_access_at_once_and_other_tenants_keep_theirs() {
    // The fixture's setup future is larger than the lint's ceiling on the stack: it builds two
    // tenants' people, their devices and two hosted workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let mut ann_in_acme = subscribed(&t.server, &t.ann_laptop, t.acme.workspace_id).await;
    let bob_in_globex = subscribed(&t.server, &t.bob_phone, t.globex.workspace_id).await;
    crate::keep::common::wait_until_subscribed(
        &t.server,
        t.globex.workspace_id,
        &t.bob_phone.device_pubkey(),
    )
    .await;
    // Ann's link opens with the workspace's changes and state its host keeps, which her copy
    // already holds: they are heard before Globex's removal, so the quiet below is the removal's.
    tilo_test_env::within("Ann's link has brought what it opens with", async {
        while next_within_quiet(&mut ann_in_acme).await.is_some() {}
    })
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
    let _ = bob_in_globex.write(&t.bob_phone.sk, (APP, RECORD), |doc| {
        doc.put(ROOT, "marker", "removed-member-writes-9d21")
            .unwrap();
    });
    let workspaces_for_write = t.workspaces();
    let kept = tokio::time::timeout(QUIET, async {
        loop {
            let leaked = written(&workspaces_for_write, &t.globex.workspace).await;
            assert!(
                !leaked.is_some_and(|bytes| marked(&bytes, "removed-member-writes-9d21")),
                "a removed member's write was kept"
            );
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(kept.is_err(), "a removed member's write was kept");
    tilo_test_env::within("the removed member's link ends", async {
        while t
            .server
            .is_subscribed(t.globex.workspace_id, &t.bob_phone.device_pubkey())
            .await
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // A fresh link is not admitted either.
    let again = bob_in_globex.reopen(&t.server, t.globex.workspace_id).await;
    assert!(again.is_err(), "a removed member linked Globex again");

    // Acme is untouched by Globex's decision.
    assert!(
        t.server
            .is_subscribed(t.acme.workspace_id, &t.ann_laptop.device_pubkey())
            .await
    );
    assert!(
        next_within_quiet(&mut ann_in_acme).await.is_none(),
        "Globex's removal cut Acme's member"
    );
    t.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_never_administers_and_reaches_no_workspace_outside_its_grant() {
    // The fixture's setup future is larger than the lint's ceiling on the stack: it builds two
    // tenants' people, their devices and two hosted workspaces.
    let t = Box::pin(TwoTenants::start()).await;
    let before = t.globex_governance().await;
    // The host's person's agent, under a key their root certified for Acme alone.
    let agent = t.provider.agent_key(
        "acme-agent",
        DeviceCapabilities::agent_grant(AgentScope::agent_in(
            *t.acme.workspace_id.as_uuid().as_bytes(),
            "acme-helper",
        )),
    );
    let acme_joined = governance(&t.workspaces(), &t.host, &t.acme.workspace).await;

    // An agent administers nothing, not even the workspace it is delegated, and
    // it is no host person: the one decision every entry asks refuses it
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

    // Outside its grant it is not admitted to a workspace at all.
    let probe = Linked::open(&t.server, &agent, t.globex.workspace_id).await;
    assert!(
        probe.is_err(),
        "the agent reads a workspace its grant does not name"
    );
    t.server.shutdown().await;
}
