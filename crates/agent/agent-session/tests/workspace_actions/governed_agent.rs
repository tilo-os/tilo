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

use std::sync::{Arc, Mutex};
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

fn issue_delegate(
    root: &SigningKey,
    pending: &tilo_server_protocol::certify::PendingCertification,
    proposal: &tilo_server_protocol::delegation::DelegateProposal,
) -> ApproverDecision {
    let now = tilo_clock::unix_now_ns();
    let cert = issue_device_cert(
        &pending.request,
        root,
        &CertificationPolicy::agent(),
        &RevocationList::new(),
        now,
    )
    .unwrap();
    let delegation = proposal
        .sign(root, pending.request.device_public_key, now)
        .unwrap();
    ApproverDecision::IssueDelegate { cert, delegation }
}

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
                    issue_delegate(&root, &pending, &agent.proposal)
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
            .approved_session_device(&sessions, workspace.workspace_id, declined, &keeper)
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
            .approved_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
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
            .approved_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
            .await,
        Some(agent_key)
    );
    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
            .await,
        None,
        "a supervisor task agent is not a signing session for a hosted roster agent"
    );
    assert_eq!(
        workspaces
            .approved_agent_device(unjoined.workspace_id, &grant, &keeper)
            .await,
        Some(agent_key)
    );
    assert_eq!(
        server
            .approved_session_device(&sessions, unjoined.workspace_id, &grant, &keeper)
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
                .approved_session_device(&sessions, workspace, id, &keeper)
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
            .approved_session_device(&sessions, workspace.workspace_id, &grant, &keeper)
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

#[test]
fn an_existing_roster_agent_joins_after_one_approval_without_a_task_runner() {
    // Surface resolution is process-wide, as on the server.
    tilo_test_env::in_own_process(
        module_path!(),
        "an_existing_roster_agent_joins_after_one_approval_without_a_task_runner",
        || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(a_hosted_roster_turn());
        },
    );
}

async fn a_hosted_roster_turn() {
    use super::hosted_agent_turns::{
        TASKS, answered_to, does_then_answers, file_issue, make_agent,
    };
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    use tilo_server::workspace_agents::HostAgents;
    use tilo_session_client::Outbox as _;
    use ui::tilo_workspace_agent::{KIND, OWNER, WorkspaceAgents, workspace_agents::RetireAgent};

    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (other, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let node = enrol(&owner, "node", DeviceCapabilities::member());
    let laptop = enrol(&owner, "laptop", DeviceCapabilities::primary());
    let page_device = enrol(&owner, "page", DeviceCapabilities::member());
    let other_device = enrol(&other, "guest", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    tilo_server::surfaces::decide_receipt(&state.path().join("server/surfaces"));
    let options = || crate::harness::hosted::Options {
        roster: true,
        ..crate::harness::hosted::Options::offering(
            crate::common::catalogue,
            tilo_agent_session::ModelChoice::Scripted,
        )
    };
    let hosted = crate::harness::hosted::start(&owner, &node, state.path(), options()).await;
    let sessions = hosted.agents.agent_sessions();
    let server = hosted.server.agent_link();
    let keeper = owner.verifying_key().to_bytes();
    let workspaces = hosted.server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Existing hosted agent").await.unwrap();
    let elsewhere = workspaces.create("Another workspace").await.unwrap();
    let mut page =
        Browser::open_workspace(&hosted, &page_device, &node, &workspace.workspace).await;
    let mut second = Browser::open_workspace(&hosted, &laptop, &node, &workspace.workspace).await;
    let mut wrong =
        Browser::open_workspace(&hosted, &page_device, &node, &elsewhere.workspace).await;
    admit_person(
        &hosted,
        &node,
        &workspace.workspace,
        &other_device,
        Role::Editor,
    )
    .await;
    let mut guest = open_admitted_peer(&hosted, &other_device, &node, &workspace.workspace).await;
    make_agent(
        &page,
        &workspaces,
        &workspace.workspace,
        &hex::encode(keeper),
        None,
    )
    .await;
    let agent = super::hosted_agent_turns::SCOUT;
    let ask = || {
        request(
            AGENTS_PROVIDER,
            ActionAddress::new(SUPERVISOR_RESOURCE, REQUEST_GRANT_ACTION),
            json(&serde_json::json!({
                "agentId": agent,
                "workspaces": [workspace.workspace],
                "apps": [SHEETS, TASKS, STORE_FIXTURE],
                "expiresInSecs": 3600,
            })),
        )
    };
    for receipt in [
        guest.invoke(ask()).await,
        wrong.invoke(ask()).await,
        wrong
            .invoke(request(
                AGENTS_PROVIDER,
                ActionAddress::new(SUPERVISOR_RESOURCE, REQUEST_GRANT_ACTION),
                json(&serde_json::json!({
                    "agentId": agent,
                    "workspaces": [elsewhere.workspace],
                    "expiresInSecs": 3600,
                })),
            ))
            .await,
    ] {
        assert!(
            matches!(receipt.status, ReceiptStatus::Rejected { .. }),
            "{receipt:?}"
        );
    }
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let mut approver = Approver::connect(
        &endpoint,
        hosted.server.node_addr(),
        laptop.cert.clone(),
        &laptop.key,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    assert!(approver.list_agent_proposals().await.unwrap().is_empty());

    let declined = ask();
    let (first, repeated) = tokio::join!(page.invoke(declined.clone()), second.invoke(ask()));
    let output = |receipt: tilo_actions::remote::InvocationReceipt| match receipt.status {
        ReceiptStatus::Completed { output } => output.value.unwrap(),
        other => panic!("the roster grant request failed: {other:?}"),
    };
    let first = output(first);
    assert_eq!(first["grant"], agent);
    assert_eq!(first, output(repeated));
    let proposals = approver.list_agent_proposals().await.unwrap();
    assert_eq!(proposals.len(), 1);
    let proposal = &proposals[0];
    assert_eq!(proposal.proposal.delegate.agent_id(), Some(agent));
    assert_eq!(proposal.proposal.scope.workspaces, [workspace.workspace_id]);
    let first_device = approver.list_pending().await.unwrap()[0]
        .request
        .device_public_key;
    assert_eq!(
        approver
            .decide(proposal.request_digest, ApproverDecision::Deny)
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if status(&mut page)
            .await
            .grants
            .iter()
            .any(|g| g.id == agent && g.state == GrantState::Denied)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the keeper's denial was not recorded"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(sessions.link(&first_device).is_none());
    assert_eq!(output(page.invoke(declined).await), first);
    assert!(approver.list_agent_proposals().await.unwrap().is_empty());

    assert_eq!(output(page.invoke(ask()).await)["grant"], agent);
    let proposals = approver.list_agent_proposals().await.unwrap();
    assert_eq!(proposals.len(), 1);
    let proposal = &proposals[0];
    let pending = approver
        .list_pending()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.request_digest == proposal.request_digest)
        .unwrap();
    let device = pending.request.device_public_key;
    assert_ne!(first_device, device);
    assert_ne!(page_device.cert.device_public_key, device);
    assert_eq!(
        approver
            .decide(
                proposal.request_digest,
                issue_delegate(&owner, &pending, &proposal.proposal)
            )
            .await
            .unwrap(),
        ApproverReply::Decided
    );

    let deadline = tilo_test_env::hang_deadline();
    while server
        .roster_session_device(&sessions, workspace.workspace_id, agent, &keeper)
        .await
        != Some(device)
    {
        assert!(
            Instant::now() < deadline,
            "the approved device did not join: {:?}",
            status(&mut page).await
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        status(&mut page).await.agents.is_empty(),
        "approval must leave task execution to the roster runner"
    );
    let spawn = page.invoke(request(
        AGENTS_PROVIDER,
        ActionAddress::new(SUPERVISOR_RESOURCE, SPAWN_ACTION),
        json(&serde_json::json!({"grant": agent, "task": "Do something", "workspace": workspace.workspace})),
    )).await;
    assert!(
        matches!(spawn.status, ReceiptStatus::Rejected { .. }),
        "{spawn:?}"
    );
    for (workspace, id, keeper) in [
        (elsewhere.workspace_id, agent, keeper),
        (workspace.workspace_id, "another-agent", keeper),
        (
            workspace.workspace_id,
            agent,
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

    // The actual signing peer cannot use its connection to invoke an unbound write.
    let signer = sessions.link(&device).unwrap();
    let events = signer.events();
    let write = request(
        tilo_agent_session::member::actions::RESOURCE_PROVIDER,
        ActionAddress::new("sheets", "new-sheet"),
        json(&serde_json::json!({"name": "Unbound"})),
    );
    let invocation = write.invocation;
    signer
        .send(tilo_session_client::Cmd::RemoteAction(
            workspace.workspace_id,
            node.cert.device_public_key,
            RemoteActionFrame::new(RemoteActionMessage::Invoke(write))
                .encode()
                .unwrap(),
        ))
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let denied = events.drain().into_iter().any(|event| {
            if let tilo_session_client::Event::Remote(tilo_session_actions::Heard::Action { frame, .. }) = event
                && let Ok(RemoteActionFrame { message: RemoteActionMessage::Receipt(receipt), .. }) = RemoteActionFrame::decode(&frame)
                && receipt.invocation == invocation
            {
                assert!(matches!(receipt.status, ReceiptStatus::Rejected { error: tilo_actions::remote::ActionCallError::Refused { ref reason } } if reason.contains("signing session")), "{receipt:?}");
                return true;
            }
            false
        });
        if denied {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the signing peer did not receive a refusal"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let roster = hosted.roster.as_ref().unwrap();
    let workbook = page.create(SHEETS);
    let notebook = page.create("so.tilo.notes");
    file_issue(
        &roster.actions,
        &workspaces,
        &workspace.workspace,
        &owner,
        (&node, &page_device),
    )
    .await;
    let read = |route: &DocumentRoute| {
        let workspaces = workspaces.clone();
        let route = route.clone();
        async move {
            workspaces
                .resource_document(&route.cluster_id, &route.app_id, &route.document)
                .await
                .unwrap()
        }
    };
    let deadline = tilo_test_env::hang_deadline();
    let (workbook_before, notebook_before) = loop {
        if let (Some(sheets), Some(notes)) = (read(&workbook).await, read(&notebook).await) {
            break (sheets, notes);
        }
        assert!(
            Instant::now() < deadline,
            "the turn's documents were not kept"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let store_route = Box::pin(seed_store_resource(
        &workspaces,
        &workspace.workspace,
        &node,
        state.path(),
    ))
    .await;
    let mut store_writes = workspaces.document_writes();
    let edit = |invocation: &str, field: &str, value: &str| {
        serde_json::json!({
            "path": "resource-fixture#field-edit",
            "invocation_id": invocation,
            "args": {"edit": {"path": [{"item": "card"}, {"field":field}], "op": "set", "value": value}},
        })
    };
    let accepted = tilo_actions::InvocationId::new().0.to_string();
    let denied = tilo_actions::InvocationId::new().0.to_string();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = does_then_answers(
        vec![
            serde_json::json!({"path": "sheets#new-sheet", "args": {"name": "Roster signed"}}),
            serde_json::json!({"path": "notes#create-page", "args": {"title": "Outside the grant"}}),
            edit(&denied, "protected", "Denied"),
            edit(&accepted, "title", "Updated"),
        ],
        "I worked on the sheet.",
        Arc::clone(&requests),
    );
    let runner = HostAgents::new(workspaces.clone(), roster.actions.clone(), Some(model))
        .with_roster_sessions(&hosted.server, roster.sessions.clone());
    let turns = runner.turns(&workspace.workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let ended = runner.answer(&workspace.workspace, &turns[0]).await;
    {
        let requests = requests.lock().unwrap();
        let final_request = requests
            .last()
            .unwrap_or_else(|| panic!("no model call: {ended:?}"));
        let success = answered_to(final_request, "call-1");
        let refused = answered_to(final_request, "call-2");
        assert!(!success.1, "{success:?}");
        assert!(
            refused.1 && refused.0.contains("not_permitted"),
            "{refused:?}"
        );
        let denied = answered_to(final_request, "call-3");
        assert!(denied.1 && denied.0.contains("not permitted"), "{denied:?}");
        let updated = answered_to(final_request, "call-4");
        assert!(!updated.1, "{updated:?}");
    }
    ended.unwrap();
    assert_ne!(read(&workbook).await.unwrap(), workbook_before);
    assert_eq!(read(&notebook).await.unwrap(), notebook_before);
    let signed = roster_write(&hosted, workspace.workspace_id, &workbook, device).await;
    assert_eq!(signed.len(), 1, "{signed:?}");
    let (count, _) = retained_sealed(&hosted, workspace.workspace_id, &notebook, device).await;
    assert_eq!(count, 0);
    let mut store_events = 0;
    while let Ok(write) = store_writes.try_recv() {
        if write.app == STORE_FIXTURE {
            store_events += 1;
        }
    }
    assert_eq!(
        store_events, 1,
        "only the admitted edit published to the watch"
    );
    assert_store_edit(
        &hosted,
        state.path(),
        &store_route,
        device,
        agent,
        &accepted,
    )
    .await;
    drop(runner);

    // The same roster id can exist in another workspace. An answer for that
    // occurrence cannot borrow the first device or outlive its own roster row.
    make_agent(
        &wrong,
        &workspaces,
        &elsewhere.workspace,
        &hex::encode(keeper),
        None,
    )
    .await;
    let elsewhere_ask = request(
        AGENTS_PROVIDER,
        ActionAddress::new(SUPERVISOR_RESOURCE, REQUEST_GRANT_ACTION),
        json(&serde_json::json!({
            "agentId": agent,
            "workspaces": [elsewhere.workspace],
            "apps": [SHEETS],
            "expiresInSecs": 3600,
        })),
    );
    assert_eq!(output(wrong.invoke(elsewhere_ask).await)["grant"], agent);
    let proposals = approver.list_agent_proposals().await.unwrap();
    assert_eq!(proposals.len(), 1);
    let stale = &proposals[0];
    assert_eq!(stale.proposal.scope.workspaces, [elsewhere.workspace_id]);
    let pending = approver
        .list_pending()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.request_digest == stale.request_digest)
        .unwrap();
    let stale_device = pending.request.device_public_key;
    assert_ne!(stale_device, device);

    let document = collab_types::workspace_document_id(KIND);
    let saved = workspaces
        .resource_document(&elsewhere.workspace, OWNER, &document)
        .await
        .unwrap()
        .unwrap();
    let mut roster = ui::tilo_resource::Replica::<WorkspaceAgents>::from_doc(
        automerge::AutoCommit::load(&saved).unwrap(),
    )
    .as_author(&hex::encode(keeper));
    roster
        .apply(WorkspaceAgents::RETIRE_AGENT.with(RetireAgent {
            agent: ui::tilo_resource::Id::from_raw(agent.to_owned()),
        }))
        .unwrap();
    wrong
        .peer
        .write(
            DocumentRoute {
                app_id: OWNER.into(),
                cluster_id: elsewhere.workspace.clone(),
                document: document.clone(),
            },
            &collab_types::DocumentMutation::Snapshot(std::mem::take(roster.doc_mut()).save()),
        )
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let current = workspaces
            .read_resource::<WorkspaceAgents>(&elsewhere.workspace, (OWNER, &document))
            .await
            .unwrap()
            .unwrap();
        if !current
            .get(&ui::tilo_resource::Id::from_raw(agent.to_owned()))
            .unwrap()
            .is_active()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the roster retirement did not land"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        approver
            .decide(
                stale.request_digest,
                issue_delegate(&owner, &pending, &stale.proposal)
            )
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if status(&mut wrong).await.grants.iter().any(|g| {
            g.id == agent
                && g.workspaces == [elsewhere.workspace.as_str()]
                && g.state == GrantState::Denied
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the stale approval should be denied"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(sessions.link(&stale_device).is_none());
    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, agent, &keeper)
            .await,
        Some(device)
    );

    supervise(
        &mut page,
        &agent_resource(agent),
        REVOKE_ACTION,
        InvokeArgument::None,
    )
    .await;
    assert_eq!(
        server
            .roster_session_device(&sessions, workspace.workspace_id, agent, &keeper)
            .await,
        None
    );
    assert!(matches!(
        page.invoke(ask()).await.status,
        ReceiptStatus::Rejected { .. }
    ));
    for page in [&page, &second, &wrong, &guest] {
        page.peer.close();
    }
    drop(approver);
    drop(server);
    drop(workspaces);
    hosted.agents.shutdown();
    drop(hosted.roster);
    drop(hosted.web);
    hosted.server.shutdown().await;
    let reopened = crate::harness::hosted::start(&owner, &node, state.path(), options()).await;
    assert_store_edit(
        &reopened,
        state.path(),
        &store_route,
        device,
        agent,
        &accepted,
    )
    .await;
    let restored = roster_write(&reopened, workspace.workspace_id, &workbook, device).await;
    assert_eq!(
        restored, signed,
        "the same signed action is kept after reopening"
    );
    assert_eq!(
        reopened
            .server
            .workspaces()
            .unwrap()
            .resource_document(&workspace.workspace, &notebook.app_id, &notebook.document)
            .await
            .unwrap()
            .unwrap(),
        notebook_before,
        "the denied action left no persisted document change",
    );
    let book = reopened
        .server
        .workspaces()
        .unwrap()
        .resource_document(&workspace.workspace, SHEETS, &workbook.document)
        .await
        .unwrap()
        .unwrap();
    let mut book = automerge::AutoCommit::load(&book).unwrap();
    let contexts: Vec<_> = book
        .get_changes(&[])
        .iter()
        .filter_map(|change| tilo_actions::ActionContext::from_change_message(change.message()?))
        .filter(|context| {
            context.invoker.as_ref().is_some_and(|invoker| {
                invoker.acting.agent_id() == Some(agent)
                    && invoker
                        .device
                        .as_ref()
                        .is_some_and(|from| from.id == hex::encode(device))
            })
        })
        .collect();
    assert_eq!(contexts.len(), 1, "{contexts:?}");
    assert!(contexts[0].source_invocation.is_some());
    reopened.agents.shutdown();
    drop(reopened.web);
    reopened.server.shutdown().await;
    endpoint.close().await;
}

const STORE_FIXTURE: &str = "so.tilo.resource-fixture";
const STORE_KIND: &str = "fixture.card";

fn store_path(state: &std::path::Path, route: &DocumentRoute) -> std::path::PathBuf {
    state
        .join("server/workspaces")
        .join(&route.cluster_id)
        .join("store/docs")
        .join(hex::encode(route.document.as_bytes()))
}

async fn seed_store_resource(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    governor: &Device,
    state: &std::path::Path,
) -> DocumentRoute {
    use automerge::transaction::Transactable as _;
    use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    use tilo_peer_state::workspace_membership::{AppRelease, WorkspaceMembershipCommand};

    let surface = br#"
[[resources]]
kind = "fixture.card"
label = "Cards"
version = 1

[[resources.fields]]
path = "title"
type = "text"

[[resources.fields]]
path = "protected"
type = "text"
unit = true

[[resources.types]]
name = "fixture.card"
fields = [{ name = "title", type = "text" }, { name = "protected", type = "text" }]

[[resources.verbs]]
id = "field-edit"
kind = "undoable"
field = "title"
params = [{ name = "edit", required = true, schema = { type = "object" } }]
"#;
    let package = super::packaged_model::package(STORE_FIXTURE, b"\0asm\x01\0\0\0", surface);
    let file = blake3::hash(&package).to_hex().to_string();
    let kept = state.join("server/workspaces/app-files").join(workspace);
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join(format!("{file}.tapp")), &package).unwrap();
    let (_, canonical) = collab_types::surface::SurfaceFile::kept(surface).unwrap();
    workspaces
        .govern_app(
            workspace,
            &governor.cert,
            [7; 32],
            WorkspaceMembershipCommand::InstallApp(AppRelease {
                app: STORE_FIXTURE.into(),
                version: "1.0.0".into(),
                surface: blake3::hash(canonical.as_bytes()).to_hex().to_string(),
                package: file.clone(),
                publisher: hex::encode(governor.cert.user_public_key),
                file: Some(file),
                dev: true,
            }),
        )
        .await
        .unwrap();
    assert!(
        workspaces
            .installed_package(workspace, STORE_FIXTURE)
            .await
            .is_some(),
        "the action service can load the installed fixture package"
    );
    collab_types::app_surface::register_package(STORE_FIXTURE, surface);
    let route = DocumentRoute {
        app_id: STORE_FIXTURE.into(),
        cluster_id: workspace.into(),
        document: format!("*/{STORE_KIND}#0"),
    };
    let mut initial = automerge::AutoCommit::new();
    initial.put(automerge::ROOT, "w", "*").unwrap();
    let row = initial
        .put_object(automerge::ROOT, "r:card", automerge::ObjType::Map)
        .unwrap();
    initial.put(&row, "k", STORE_KIND).unwrap();
    let fields = initial
        .put_object(&row, "f", automerge::ObjType::Map)
        .unwrap();
    initial.put(&fields, "title", "Original").unwrap();
    initial.put(&fields, "protected", "Kept").unwrap();
    initial.commit_with(
        automerge::transaction::CommitOptions::default().with_message(
            collab_types::Stamp {
                verb: "field-edit".into(),
                invocation: "seed".into(),
                ..Default::default()
            }
            .message(),
        ),
    );
    let keys = workspaces
        .own_route_keys(&route, true)
        .await
        .unwrap()
        .unwrap();
    let artifact = collab_types::document_artifact_id(STORE_FIXTURE, workspace, &route.document);
    let frame = collab_types::DocumentMutation::Snapshot(initial.save())
        .encode()
        .unwrap();
    let (_, peer) = workspaces
        .governed_copy(WorkspaceId::parse(workspace).unwrap())
        .await
        .unwrap();
    let mut envelope = peer
        .draft_write(
            (STORE_FIXTURE, &route.document),
            &frame,
            Some(&mut |plaintext| {
                keys.seal_content(
                    artifact,
                    collab_types::ContentRetention::Checkpoint,
                    plaintext,
                )
                .map_err(|error| error.to_string())
            }),
            governor.cert.device_public_key,
            0,
            tilo_clock::wall(),
        )
        .unwrap();
    collab_types::sign_op(&mut envelope, &governor.key).unwrap();
    workspaces
        .commit_local_document(
            workspace,
            OwnerCommitRequest {
                workspace: workspace.into(),
                operation: CommitOperation::Document {
                    app: STORE_FIXTURE.into(),
                    document: route.document.clone(),
                    envelope: Box::new(envelope),
                    own: None,
                    readers: None,
                },
            },
        )
        .await
        .unwrap();

    assert!(
        workspaces
            .resource_document(workspace, STORE_FIXTURE, &route.document)
            .await
            .unwrap()
            .is_some(),
        "the server can read the seeded store document"
    );
    assert!(
        store_path(state, &route).exists(),
        "the fixture was taken into the workspace store"
    );

    route
}

async fn assert_store_edit(
    hosted: &super::Hosted,
    state: &std::path::Path,
    route: &DocumentRoute,
    device: [u8; 32],
    agent: &str,
    invocation: &str,
) {
    use automerge::ReadDoc as _;

    let workspaces = hosted.server.workspaces().unwrap();
    let bytes = workspaces
        .resource_document(&route.cluster_id, &route.app_id, &route.document)
        .await
        .unwrap()
        .unwrap();
    for bytes in [bytes, std::fs::read(store_path(state, route)).unwrap()] {
        let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
        let (_, row) = doc.get(automerge::ROOT, "r:card").unwrap().unwrap();
        let (_, fields) = doc.get(row, "f").unwrap().unwrap();
        for (field, expected) in [("title", "Updated"), ("protected", "Kept")] {
            let (value, _) = doc.get(&fields, field).unwrap().unwrap();
            assert_eq!(
                value.as_scalar().and_then(automerge::ScalarValue::as_str),
                Some(expected)
            );
        }
        let changes = doc.get_changes(&[]);
        assert_eq!(changes.len(), 2, "seed and one accepted edit");
        let change = &changes[1];
        assert_eq!(change.actor_id().to_bytes(), &device);
        let stamp = collab_types::Stamp::of_message(change.message().unwrap()).unwrap();
        assert_eq!(stamp.invocation, invocation);
        assert_eq!(stamp.verb, "field-edit");
        assert_eq!(stamp.invoker, agent);
        assert!(!stamp.membership.is_empty());
    }
    let envelopes: Vec<_> = hosted
        .server
        .workspace_documents()
        .retained(WorkspaceId::parse(&route.cluster_id).unwrap())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|body| match postcard::from_bytes(&body.body).unwrap() {
            neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                envelope,
                ..
            } if app_id == route.app_id && document_id == route.document => Some(envelope),
            _ => None,
        })
        .collect();
    assert_eq!(envelopes.len(), 2, "seed and one retained edit");
    let edit = envelopes
        .iter()
        .find(|envelope| envelope.author.as_bytes() == &device)
        .unwrap();
    assert!(edit.encryption.is_some());
    assert_eq!(edit.claims.len(), 1);
    collab_types::verify_op(edit).unwrap();
}

async fn roster_write(
    hosted: &super::Hosted,
    workspace: WorkspaceId,
    route: &DocumentRoute,
    device: [u8; 32],
) -> Vec<collab_types::SignedOp> {
    hosted
        .server
        .workspace_documents()
        .retained(workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|body| body.author == device)
        .filter_map(|body| {
            let neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                document_id,
                envelope,
                ..
            } = postcard::from_bytes(&body.body).ok()?
            else {
                return None;
            };
            (app_id == route.app_id && document_id == route.document).then_some(*envelope)
        })
        .inspect(|op| {
            assert_eq!(op.author.as_bytes(), &device);
            assert!(op.encryption.is_some());
            collab_types::verify_op(op).unwrap();
            assert!(op.claims.iter().any(|claim| {
                claim
                    .invoker
                    .as_ref()
                    .and_then(tilo_actions::Acting::agent_id)
                    == Some(super::hosted_agent_turns::SCOUT)
            }));
        })
        .collect()
}
