//! A delegated agent's own session in a workspace the host itself governs.
//!
//! A node (a host certified to own workspaces) governs the workspace and takes
//! part in its channels. Ada asks for an agent from her page and approves it on her
//! laptop with a delegation naming the workspace; the agent runs on the node
//! under its own device key. When it opens a sealed record it asks the
//! channel's members for it like any member: the governor admits
//! it to the workspace's roster through the one admission decision, bound to its
//! delegation, and a member admits it to the channel. Its change
//! arrives at Bob, another member, sealed, signed by the agent's own key and
//! named on the roster as Ada's agent. Withdrawing the grant takes it off.

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{InvokeArgument, ReceiptStatus};
use tilo_peer_state::join::Role;
use tilo_server::certify::{ApproverDecision, ApproverReply};
use tilo_server_client::Approver;
use tilo_server_protocol::agents::{
    AGENTS_PROVIDER, AgentsStatus, GrantState, REQUEST_GRANT_ACTION, REVOKE_ACTION, SPAWN_ACTION,
    STATUS_ACTION, SUPERVISOR_RESOURCE, agent_resource,
};
use tilo_user_identity::{CertificationPolicy, RevocationList, issue_device_cert};

use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    Browser, Device, DeviceCapabilities, DocumentRoute, SHEETS, enrol, request, sheet_names,
};
use crate::harness::script::{doing, task};
use tilo_server_protocol::WorkspaceId;

/// The laptop: a root holder denying the named proposal and approving the others
/// with the agent's certificate and the delegation of exactly what was proposed.
fn approve_agents(
    server: &tilo_server::daemon::Server,
    root: SigningKey,
    laptop: &Device,
    denied: &'static str,
) -> tokio::task::JoinHandle<()> {
    let addr = server.node_addr();
    let cert = laptop.cert.clone();
    let key = laptop.key.clone();
    tokio::spawn(async move {
        let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .bind()
            .await
            .unwrap();
        let mut approver = Approver::connect(&endpoint, addr, cert, &key, &RevocationList::new())
            .await
            .unwrap();
        loop {
            let proposals = approver.list_agent_proposals().await.unwrap();
            for pending in approver.list_pending().await.unwrap() {
                let Some(agent) = proposals
                    .iter()
                    .find(|agent| agent.request_digest == pending.request_digest)
                else {
                    continue;
                };
                let decision = if agent.proposal.label == denied {
                    ApproverDecision::Deny
                } else {
                    let now = tilo_clock::unix_now_ns();
                    let cert = issue_device_cert(
                        &pending.request,
                        &root,
                        &CertificationPolicy::agent(),
                        &RevocationList::new(),
                        now,
                    )
                    .unwrap();
                    let delegation = agent
                        .proposal
                        .sign(&root, pending.request.device_public_key, now)
                        .unwrap();
                    ApproverDecision::IssueDelegate { cert, delegation }
                };
                assert_eq!(
                    approver
                        .decide(pending.request_digest, decision)
                        .await
                        .unwrap(),
                    ApproverReply::Decided
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

/// One request to the host's agent supervisor, answered.
async fn supervise(
    page: &mut Browser,
    resource: &str,
    action: &str,
    argument: InvokeArgument,
) -> serde_json::Value {
    let receipt = page
        .invoke(request(
            AGENTS_PROVIDER,
            ActionAddress::new(resource, action),
            argument,
        ))
        .await;
    match receipt.status {
        ReceiptStatus::Completed { output } => output.value.unwrap_or_default(),
        other => panic!("{resource}#{action}: {other:?}"),
    }
}

async fn status(page: &mut Browser) -> AgentsStatus {
    serde_json::from_value(
        supervise(
            page,
            SUPERVISOR_RESOURCE,
            STATUS_ACTION,
            InvokeArgument::None,
        )
        .await,
    )
    .unwrap()
}

fn json(value: &serde_json::Value) -> InvokeArgument {
    InvokeArgument::Json(value.as_object().unwrap().clone())
}

/// The sheets of the record at `route` as `member` received it, once what
/// arrived opens: until the change that sets the record up is in, the app
/// refuses what came before it.
fn opened_sheets(member: &Browser, route: &DocumentRoute) -> Option<Vec<String>> {
    let changes = member.documents.get(route)?;
    let record = ui::resource::DocumentId::parse(&route.document)?;
    let replica = tilo_agent_session::Replica::open(
        &crate::common::catalogue(),
        &route.app_id,
        &route.cluster_id,
        &tilo_actions::MemberId::person("bob"),
        tilo_agent_session::ReplicaActor::Owner,
        tilo_agent_session::Seed::Existing {
            record,
            changes: changes.iter().map(|(_, bytes)| bytes.clone()).collect(),
        },
    )
    .ok()?;
    Some(sheet_names(&replica))
}

/// Wait until `member` holds a Sheets record `author` changed whose sheets
/// include `name`, and return its route; `None` after a minute.
async fn until_sheet_by(
    member: &mut Browser,
    author: [u8; 32],
    name: &str,
) -> Option<DocumentRoute> {
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline {
        member.pump();
        let routes: Vec<DocumentRoute> = member
            .documents
            .iter()
            .filter(|(route, changes)| {
                route.app_id == SHEETS && changes.iter().any(|(by, _)| *by == author)
            })
            .map(|(route, _)| route.clone())
            .collect();
        for route in routes {
            if opened_sheets(member, &route).is_some_and(|sheets| sheets.iter().any(|s| s == name))
            {
                return Some(route);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

/// Every body `author` wrote that the server retains for `workspace`: its
/// message kind, app and whether it is sealed.
async fn retained_by(
    hosted: &super::Hosted,
    workspace: WorkspaceId,
    author: [u8; 32],
) -> Vec<String> {
    hosted
        .server
        .workspace_documents()
        .retained(workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|body| body.author == author)
        .map(
            |body| match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body.body) {
                Ok(neighbour_wire::NeighbourMsg::AppDocument {
                    app_id, envelope, ..
                }) => format!("{app_id} sealed={}", envelope.encryption.is_some()),
                Ok(other) => format!("{other:?}").chars().take(48).collect(),
                Err(_) => "undecodable".into(),
            },
        )
        .collect()
}

/// Whether every body `author` wrote to `route` that the server retains is
/// sealed on the route's channel, and how many there are.
async fn retained_sealed(
    hosted: &super::Hosted,
    workspace: WorkspaceId,
    route: &DocumentRoute,
    author: [u8; 32],
) -> (usize, bool) {
    let bodies: Vec<bool> = hosted
        .server
        .workspace_documents()
        .retained(workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|body| body.author == author)
        .filter_map(
            |body| match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body.body) {
                Ok(neighbour_wire::NeighbourMsg::AppDocument {
                    app_id,
                    document_id,
                    envelope,
                    ..
                }) if app_id == route.app_id && document_id == route.document => {
                    Some(envelope.encryption.as_ref().is_some())
                }
                _ => None,
            },
        )
        .collect();
    (bodies.len(), bodies.iter().all(|sealed| *sealed))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegated_agent_acts_on_sealed_records_of_a_governed_workspace_as_itself() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (other, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let host = enrol(
        &owner,
        "node",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let laptop = enrol(&owner, "laptop", DeviceCapabilities::primary());
    let page_device = enrol(&owner, "page", DeviceCapabilities::member());
    let member_device = enrol(&other, "member", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let options = crate::harness::hosted::Options {
        roster: true,
        ..crate::harness::hosted::Options::offering(
            crate::common::catalogue,
            tilo_agent_session::ModelChoice::Scripted,
        )
    };
    let hosted = crate::harness::hosted::start(&owner, &host, state.path(), options).await;
    let sessions = hosted.agents.agent_sessions();
    let server = hosted.server.agent_link();
    let keeper = owner.verifying_key().to_bytes();
    let workspaces = hosted.server.workspaces().unwrap().clone();
    let workspace = workspaces.create("An agent's own session").await.unwrap();
    let elsewhere = workspaces.create("Not the agent's").await.unwrap();
    let unjoined = workspaces.create("Approved but not joined").await.unwrap();
    admit_person(
        &hosted,
        &host,
        &workspace.workspace,
        &member_device,
        Role::Editor,
    )
    .await;
    let mut page =
        Browser::open_workspace(&hosted, &page_device, &host, &workspace.workspace).await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &workspace.workspace).await;
    // The workbook the agent adds a sheet to: its workspace holds it, and the
    // agent is admitted to the keys of its sealed document when it opens it.
    let _ = page.create(SHEETS);
    let approver = approve_agents(&hosted.server, owner.clone(), &laptop, "Declined");
    let declined = supervise(
        &mut page,
        SUPERVISOR_RESOURCE,
        REQUEST_GRANT_ACTION,
        json(&serde_json::json!({
            "label": "Declined",
            "workspaces": [workspace.workspace],
            "apps": [SHEETS],
            "expiresInSecs": 3600,
        })),
    )
    .await;
    let declined = declined["grant"].as_str().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while !status(&mut page)
        .await
        .grants
        .iter()
        .any(|g| g.id == declined && g.state == GrantState::Denied)
    {
        assert!(Instant::now() < deadline, "the grant was never denied");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, declined, &keeper)
            .await,
        None
    );

    // One consent step: Ada's page asks, her laptop approves the agent with
    // the delegation of this workspace and Sheets.
    let requested = supervise(
        &mut page,
        SUPERVISOR_RESOURCE,
        REQUEST_GRANT_ACTION,
        json(&serde_json::json!({
            "label": "Scribe",
            "workspaces": [workspace.workspace, unjoined.workspace],
            "apps": [SHEETS],
            "expiresInSecs": 3600,
        })),
    )
    .await;
    let grant = requested["grant"].as_str().unwrap().to_owned();
    let deadline = tilo_test_env::hang_deadline();
    while !status(&mut page)
        .await
        .grants
        .iter()
        .any(|g| g.id == grant && g.state == GrantState::Approved)
    {
        assert!(Instant::now() < deadline, "the grant was never approved");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    approver.abort();
    // The approved grant puts the agent on the roster of the workspace it names,
    // and of no other workspace this host governs.
    let deadline = tilo_test_env::hang_deadline();
    while workspaces
        .agents(&workspace.workspace)
        .await
        .unwrap()
        .is_empty()
    {
        assert!(
            Instant::now() < deadline,
            "the approved agent never joined the workspace's roster"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        workspaces
            .agents(&elsewhere.workspace)
            .await
            .unwrap()
            .is_empty(),
        "an agent is never on the roster of a workspace its grant does not name"
    );

    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
            .await,
        None,
        "approval alone does not open a session"
    );

    // The agent works in the governed workspace under its own key. Its task is
    // written out as the tool call its turn makes, adding a sheet named
    // "InvocationLog" to the workspace's workbook.
    let spawned = supervise(
        &mut page,
        SUPERVISOR_RESOURCE,
        SPAWN_ACTION,
        json(&serde_json::json!({
            "grant": grant,
            "task": task(&[doing("sheets#new-sheet", serde_json::json!({"name": "InvocationLog"}))]),
            "workspace": workspace.workspace,
        })),
    )
    .await;
    assert_eq!(spawned["agentId"], grant.as_str());
    let started = Instant::now();
    let device = loop {
        let status = status(&mut page).await;
        if let Some(agent) = status.agents.iter().find(|agent| agent.id == grant) {
            break agent.device.clone();
        }
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "the agent never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let agent_key: [u8; 32] = hex::decode(&device).unwrap().try_into().unwrap();
    assert_ne!(agent_key, host.cert.device_public_key);

    // Bob, another member, receives the agent's change and reads it.
    let Some(route) = until_sheet_by(&mut member, agent_key, "InvocationLog").await else {
        let agents = status(&mut page).await.agents;
        panic!(
            "the member never read the agent's sealed change.\nagents: {agents:#?}\n\
             roster: {:?}\nretained by the agent: {:?}\nmember: {}\npage: {}",
            workspaces.agents(&workspace.workspace).await,
            retained_by(&hosted, workspace.workspace_id, agent_key).await,
            member.peer.status().key_state,
            page.peer.status().key_state,
        );
    };
    eprintln!(
        "a delegated agent's sealed change reached another member in {:?}",
        started.elapsed()
    );
    let (count, sealed) = retained_sealed(&hosted, workspace.workspace_id, &route, agent_key).await;
    assert!(
        count > 0 && sealed,
        "the agent writes only sealed: {count}, {sealed}"
    );
    assert_eq!(
        retained_sealed(
            &hosted,
            workspace.workspace_id,
            &route,
            host.cert.device_public_key
        )
        .await
        .0,
        0,
        "the host device wrote none of it: the change is the agent's"
    );

    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
            .await,
        Some(agent_key)
    );
    assert_eq!(
        workspaces
            .approved_agent_device(unjoined.workspace_id, &grant, &keeper)
            .await,
        Some(agent_key)
    );
    assert_eq!(
        server
            .roster_session_device(&sessions, unjoined.workspace_id, &grant, &keeper)
            .await,
        None,
        "the grant covers this workspace but the session joined only the other one"
    );
    for (workspace, id, keeper) in [
        (elsewhere.workspace_id, grant.as_str(), keeper),
        (workspace.workspace_id, "another-agent", keeper),
        (
            workspace.workspace_id,
            grant.as_str(),
            other.verifying_key().to_bytes(),
        ),
    ] {
        assert_eq!(
            server
                .roster_session_device(&sessions, workspace, id, &keeper)
                .await,
            None
        );
    }
    let actor = tilo_actions::MemberId::agent(&grant);
    assert!(
        workspaces
            .work_device(workspace.workspace_id, keeper, &actor)
            .await
            .is_some_and(|device| device != agent_key),
        "the keeper's person-work caller uses a device of theirs"
    );
    assert_eq!(
        workspaces
            .work_device(
                workspace.workspace_id,
                other.verifying_key().to_bytes(),
                &actor
            )
            .await,
        Some(member_device.cert.device_public_key),
        "the other person's work still uses their own device"
    );

    // The roster names the agent as Ada's, acting.
    let roster = workspaces.agents(&workspace.workspace).await.unwrap();
    assert_eq!(roster.len(), 1, "{roster:?}");
    let agent = &roster[0];
    assert_eq!(agent.device_public_key, agent_key);
    assert_eq!(agent.user_public_key, owner.verifying_key().to_bytes());
    assert_eq!(agent.agent_id, grant);
    assert_eq!(agent.label, "Scribe");
    assert!(agent.acting && agent.withdrawn_at_ns.is_none());

    // Withdrawing the grant takes it off the workspace at once.
    supervise(
        &mut page,
        &agent_resource(&grant),
        REVOKE_ACTION,
        InvokeArgument::None,
    )
    .await;
    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
            .await,
        None,
        "withdrawal invalidates the match as soon as the action answers"
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let roster = workspaces.agents(&workspace.workspace).await.unwrap();
        if roster
            .iter()
            .any(|agent| agent.device_public_key == agent_key && !agent.acting)
        {
            assert!(roster[0].withdrawn_at_ns.is_some());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the withdrawn agent stayed on the roster acting: {roster:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    page.peer.close();
    member.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.server.shutdown().await;
}
