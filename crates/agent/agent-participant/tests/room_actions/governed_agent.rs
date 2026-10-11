//! A delegated agent's own session in a room the host itself governs.
//!
//! A node (a host certified to own workspaces) governs the room and stewards
//! its channels. Ada asks for an agent from her page and approves it on her
//! laptop with a delegation naming the room; the agent runs on the node
//! under its own device key. When it opens a sealed record it asks the
//! steward for the record's channel like any collaborator: the governor
//! admits it to the room's roster through the one admission decision, bound
//! to its delegation, and the steward admits it to the channel. Its change
//! arrives at Bob, another member, sealed, signed by the agent's own key and
//! named on the roster as Ada's agent. Withdrawing the grant takes it off.

use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_actions::door::ActionAddress;
use tilo_actions::remote::{InvokeArgument, ReceiptStatus};
use tilo_peer_state::join::MemberRole;
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay_client::Approver;
use tilo_relay_protocol::agents::{
    AGENTS_PROVIDER, AgentsStatus, GrantState, REQUEST_GRANT_ACTION, REVOKE_ACTION, SPAWN_ACTION,
    STATUS_ACTION, SUPERVISOR_RESOURCE, agent_resource,
};
use tilo_user_identity::{CertificationPolicy, RevocationList, issue_device_cert};

use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    Browser, Device, DeviceCapabilities, DocumentRoute, SHEETS, enrol, request, sheet_names, start,
};

/// The laptop: a root holder answering every agent request with the
/// agent's certificate and the delegation of exactly what was proposed.
fn approve_agents(
    relay: &tilo_relay::daemon::Relay,
    root: SigningKey,
    laptop: &Device,
) -> tokio::task::JoinHandle<()> {
    let addr = relay.node_addr();
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
                assert_eq!(
                    approver
                        .decide(
                            pending.request_digest,
                            ApproverDecision::IssueAgent { cert, delegation }
                        )
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

fn json(value: serde_json::Value) -> InvokeArgument {
    InvokeArgument::Json(value.as_object().unwrap().clone())
}

/// The sheets of the record at `route` as `member` received it, once what
/// arrived opens: until the change that sets the record up is in, the app
/// refuses what came before it.
fn opened_sheets(member: &Browser, route: &DocumentRoute) -> Option<Vec<String>> {
    let changes = member.documents.get(route)?;
    let record = ui::record::RecordId::parse(&route.record)?;
    let replica = tilo_agent_participant::Replica::open(
        &super::common::catalogue(),
        &route.app_id,
        &route.cluster_id,
        "bob",
        false,
        tilo_agent_participant::Seed::Existing {
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
    let deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
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

/// Every body `author` wrote that the relay retains for `workspace`: its
/// message kind, app and whether it is sealed.
async fn retained_by(hosted: &super::Hosted, workspace: u64, author: [u8; 32]) -> Vec<String> {
    hosted
        .relay
        .room_records()
        .retained(workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|body| body.author == author)
        .map(
            |body| match postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(&body.body) {
                Ok(tilo_peer_state::NeighbourMsg::AppDocument {
                    app_id, envelope, ..
                }) => format!("{app_id} sealed={}", envelope.encryption.is_some()),
                Ok(other) => format!("{other:?}").chars().take(48).collect(),
                Err(_) => "undecodable".into(),
            },
        )
        .collect()
}

/// Whether every body `author` wrote to `route` that the relay retains is
/// sealed on the route's channel, and how many there are.
async fn retained_sealed(
    hosted: &super::Hosted,
    workspace: u64,
    route: &DocumentRoute,
    author: [u8; 32],
) -> (usize, bool) {
    let channel =
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    let bodies: Vec<bool> = hosted
        .relay
        .room_records()
        .retained(workspace)
        .await
        .unwrap()
        .into_iter()
        .filter(|body| body.author == author)
        .filter_map(|body| {
            match postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(&body.body) {
                Ok(tilo_peer_state::NeighbourMsg::AppDocument {
                    app_id,
                    record_id,
                    envelope,
                    ..
                }) if app_id == route.app_id && record_id == route.record => Some(
                    envelope
                        .encryption
                        .as_ref()
                        .is_some_and(|metadata| metadata.channel == channel),
                ),
                _ => None,
            }
        })
        .collect();
    (bodies.len(), bodies.iter().all(|sealed| *sealed))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegated_agent_acts_on_sealed_records_of_a_governed_room_as_itself() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (other, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let host = enrol(
        &owner,
        "node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let laptop = enrol(&owner, "laptop", DeviceCapabilities::primary());
    let page_device = enrol(&owner, "page", DeviceCapabilities::member());
    let member_device = enrol(&other, "member", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&owner, &host, state.path()).await;
    let rooms = hosted.relay.rooms().unwrap().clone();
    let room = rooms.create("An agent's own session").await.unwrap();
    let elsewhere = rooms.create("Not the agent's").await.unwrap();
    admit_person(
        &hosted,
        &host,
        &room.room,
        &member_device,
        MemberRole::Editor,
    )
    .await;
    let mut page = Browser::open_room(&hosted, &page_device, &host, &room.room).await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &room.room).await;
    let approver = approve_agents(&hosted.relay, owner.clone(), &laptop);

    // One consent step: Ada's page asks, her laptop approves the agent with
    // the delegation of this room and Sheets.
    let requested = supervise(
        &mut page,
        SUPERVISOR_RESOURCE,
        REQUEST_GRANT_ACTION,
        json(serde_json::json!({
            "label": "Scribe",
            "rooms": [room.room],
            "apps": [SHEETS],
            "expiresInSecs": 3600,
        })),
    )
    .await;
    let grant = requested["grant"].as_str().unwrap().to_owned();
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
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
    // The approved grant puts the agent on the roster of the room it names,
    // and of no other room this host governs.
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    while rooms.agents(&room.room).await.unwrap().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the approved agent never joined the room's roster"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        rooms.agents(&elsewhere.room).await.unwrap().is_empty(),
        "an agent is never on the roster of a room its grant does not name"
    );

    // The agent works in the governed room under its own key.
    let spawned = supervise(
        &mut page,
        SUPERVISOR_RESOURCE,
        SPAWN_ACTION,
        json(serde_json::json!({
            "grant": grant,
            "task": "Add a new sheet named \"Ledger\"",
            "room": room.room,
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
            started.elapsed() < Duration::from_secs(30),
            "the agent never started"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let agent_key: [u8; 32] = hex::decode(&device).unwrap().try_into().unwrap();
    assert_ne!(agent_key, host.cert.device_public_key);

    // Bob, another member, receives the agent's change and reads it.
    let Some(route) = until_sheet_by(&mut member, agent_key, "Ledger").await else {
        let agents = status(&mut page).await.agents;
        panic!(
            "the member never read the agent's sealed change.\nagents: {agents:#?}\n\
             roster: {:?}\nretained by the agent: {:?}\nmember: {}\npage: {}",
            rooms.agents(&room.room).await,
            retained_by(&hosted, room.workspace_id, agent_key).await,
            member.peer.status().channels,
            page.peer.status().channels,
        );
    };
    eprintln!(
        "a delegated agent's sealed change reached another member in {:?}",
        started.elapsed()
    );
    let (count, sealed) = retained_sealed(&hosted, room.workspace_id, &route, agent_key).await;
    assert!(
        count > 0 && sealed,
        "the agent writes only sealed: {count}, {sealed}"
    );
    assert_eq!(
        retained_sealed(
            &hosted,
            room.workspace_id,
            &route,
            host.cert.device_public_key
        )
        .await
        .0,
        0,
        "the host device wrote none of it: the change is the agent's"
    );

    // The roster names the agent as Ada's, acting.
    let roster = rooms.agents(&room.room).await.unwrap();
    assert_eq!(roster.len(), 1, "{roster:?}");
    let agent = &roster[0];
    assert_eq!(agent.device_public_key, agent_key);
    assert_eq!(agent.user_public_key, owner.verifying_key().to_bytes());
    assert_eq!(agent.agent_id, grant);
    assert_eq!(agent.label, "Scribe");
    assert!(agent.acting && agent.withdrawn_at_ns.is_none());

    // Withdrawing the grant takes it off the room at once.
    supervise(
        &mut page,
        &agent_resource(&grant),
        REVOKE_ACTION,
        InvokeArgument::None,
    )
    .await;
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        let roster = rooms.agents(&room.room).await.unwrap();
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
    hosted.relay.shutdown().await;
}
