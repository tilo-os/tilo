//! A roster agent's own signing session in a workspace the host itself governs.
//!
//! A node (a host certified to own workspaces) governs the workspace and takes part in its
//! channels. Ada keeps an agent on the workspace's roster; she asks for its grant from her
//! page and approves it on her laptop with a delegation naming the workspace. The node then
//! opens the agent's signing session under its own device key, and the agent's turns sign
//! their changes through it. Withdrawing the grant takes it off.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{InvokeArgument, ReceiptStatus};
use tilo_peer_state::join::Role;
use tilo_server::certify::{ApproverDecision, ApproverReply};
use tilo_server_client::Approver;
use tilo_server_protocol::agents::{
    AGENT_RESOURCE_PREFIX, AGENTS_PROVIDER, GrantState, REQUEST_GRANT_ACTION, REVOKE_ACTION,
    SUPERVISOR_RESOURCE,
};
use tilo_user_identity::RevocationList;

use crate::harness::hosted::issue_delegate;

use super::resource_support::{admit_person, open_admitted_peer};
use super::{Browser, Device, DeviceCapabilities, DocumentRoute, SHEETS, enrol, request};
use tilo_server_protocol::WorkspaceId;

/// The laptop: a root holder denying the named proposal and approving the others
/// with the agent's certificate and the delegation of exactly what was proposed.
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

/// Where the host's grant of `agent` in `workspace` stands, as it keeps it.
fn grant_state(hosted: &super::Hosted, agent: &str, workspace: WorkspaceId) -> Option<GrantState> {
    hosted
        .agents
        .supervisor()
        .snapshot()
        .grants
        .iter()
        .find(|grant| {
            grant.proposal.delegate.agent_id() == Some(agent)
                && grant.proposal.scope.workspaces == [workspace]
        })
        .map(|grant| grant.state)
}

/// An approved agent's invocations sent straight to the host's workspace actions, as its turn's
/// tools send them, signed with its own key or not.
struct Direct<'a> {
    actions: &'a tilo_agent_session::WorkspaceActions,
    sessions: &'a tilo_agent_session::AgentSessions,
    workspace: WorkspaceId,
    caller: &'a tilo_actions::remote::Caller,
    device: [u8; 32],
}

impl Direct<'_> {
    async fn run(
        &self,
        request: tilo_actions::remote::InvokeRequest,
        signed: bool,
    ) -> ReceiptStatus {
        let mut request = request;
        if signed {
            self.sessions
                .sign_as(&self.device, self.workspace, &mut request)
                .unwrap();
        }
        self.send(tilo_actions::remote::RemoteActionMessage::Invoke(request))
            .await
    }

    async fn send(&self, message: tilo_actions::remote::RemoteActionMessage) -> ReceiptStatus {
        let answer = self
            .actions
            .serve(self.workspace, self.caller, message)
            .unwrap()
            .await
            .unwrap();
        match answer {
            tilo_actions::remote::RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("an invocation is answered with its receipt: {other:?}"),
        }
    }
}

/// A new sheet `name` in `workbook`.
fn sheet_request(workbook: &DocumentRoute, name: &str) -> tilo_actions::remote::InvokeRequest {
    request(
        &tilo_agent_session::resource_provider(workbook),
        ActionAddress::new("", "new-sheet"),
        json(&serde_json::json!({"name": name})),
    )
}

fn json(value: &serde_json::Value) -> InvokeArgument {
    InvokeArgument::Json(value.as_object().unwrap().clone())
}

/// The sheets of the record at `route` as `member` received it, once what
/// arrived opens: until the change that sets the record up is in, the app
/// refuses what came before it.
fn retained_sealed(
    hosted: &super::Hosted,
    workspace: WorkspaceId,
    route: &DocumentRoute,
    author: [u8; 32],
) -> (usize, bool) {
    let bodies: Vec<bool> = hosted
        .server
        .workspace_documents()
        .retained(workspace)
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
    use tilo_session_client::Outbox as _;
    use tilo_workspace_service::agents::ServerAgents;
    use ui::tilo_workspace_agent::{KIND, OWNER, WorkspaceAgents, workspace_agents::RetireAgent};

    let (owner, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (other, _) = tilo_user_identity::UserIdentity::generate("Bob", 1);
    let node = enrol(&owner, "node", DeviceCapabilities::member());
    let laptop = enrol(&owner, "laptop", DeviceCapabilities::primary());
    let page_device = enrol(&owner, "page", DeviceCapabilities::member());
    let other_device = enrol(&other, "guest", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    tilo_server::surfaces::decide_receipt(&state.path().join("server/surfaces"));
    let options = || crate::harness::hosted::Options::offering(crate::common::catalogue);
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
    let resource = format!("{AGENT_RESOURCE_PREFIX}{agent}");
    let status = || {
        request(
            AGENTS_PROVIDER,
            ActionAddress::new(&resource, "grant-status"),
            InvokeArgument::None,
        )
    };
    let initial = supervise(&mut page, &resource, "grant-status", InvokeArgument::None).await;
    assert_eq!(
        initial,
        serde_json::json!({"state": "missing", "ready": false, "apps": [], "expiresAtMs": null})
    );
    assert!(matches!(
        guest.invoke(status()).await.status,
        ReceiptStatus::Rejected { .. }
    ));
    assert!(matches!(
        wrong.invoke(status()).await.status,
        ReceiptStatus::Rejected { .. }
    ));
    // A certified keeper can discover precisely the grant actions their assistant offers.
    let catalogue = |provider: Option<&str>| tilo_actions::remote::CatalogueQuery {
        provider: provider.map(str::to_owned),
        ..Default::default()
    };
    let discovery = page.catalogue(catalogue(None)).await.unwrap();
    assert!(discovery.providers.iter().any(|entry| {
        entry.provider == AGENTS_PROVIDER && entry.workspace == workspace.workspace
    }));
    let offered = page
        .catalogue(catalogue(Some(AGENTS_PROVIDER)))
        .await
        .unwrap();
    let offered: Vec<_> = offered
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .collect();
    assert_eq!(
        offered.len(),
        4,
        "an assistant with no grant has nothing to revoke"
    );
    assert!(
        offered
            .iter()
            .all(|action| action.address.resource == resource)
    );
    assert!(
        offered
            .iter()
            .any(|action| action.address.action == "grant-status"
                && action.contract.kind == tilo_actions::ActionKind::Read)
    );
    assert!(
        offered
            .iter()
            .any(|action| action.address.action == "request-chat-grant")
    );
    assert!(
        offered
            .iter()
            .any(|action| action.address.action == "request-assistant-work-grant")
    );
    let selected = offered
        .iter()
        .find(|action| action.address.action == "request-app-grant")
        .unwrap();
    let schema = selected
        .params
        .iter()
        .find(|param| param.name == "apps")
        .unwrap()
        .schema
        .as_deref()
        .unwrap();
    tilo_actions::validate_json_schema(
        &serde_json::json!(["so.tilo.chat", "so.tilo.mail"]),
        schema,
    )
    .unwrap();
    let choices = tilo_server_protocol::agents::AgentGrantApp::choices(selected);
    assert!(
        choices
            .iter()
            .any(|app| app.id == "so.tilo.chat" && !app.title.is_empty())
    );
    assert!(
        choices
            .iter()
            .any(|app| app.id == "tilo.models" && !app.title.is_empty())
    );
    assert!(choices.iter().any(|app| app.id == SHEETS));
    let choose = |apps: serde_json::Value| {
        request(
            AGENTS_PROVIDER,
            ActionAddress::new(&resource, "request-app-grant"),
            json(&apps),
        )
    };
    for apps in [
        serde_json::json!({"apps":[]}),
        serde_json::json!({"apps":["so.tilo.mail"]}),
        serde_json::json!({"apps":["so.tilo.chat", "*"]}),
        serde_json::json!({"apps":["so.tilo.chat", "so.tilo.unknown-to-this-host"]}),
        serde_json::json!({"apps":["so.tilo.chat", "so.tilo.chat"]}),
        serde_json::json!({"apps":["so.tilo.chat"], "workspaces":[elsewhere.workspace]}),
    ] {
        assert!(matches!(
            page.invoke(choose(apps)).await.status,
            ReceiptStatus::Rejected { .. }
        ));
    }
    let chosen = serde_json::json!({"apps":["so.tilo.mail", "so.tilo.chat"]});
    assert!(matches!(
        guest.invoke(choose(chosen.clone())).await.status,
        ReceiptStatus::Rejected { .. }
    ));
    for other in [&mut guest, &mut wrong] {
        assert!(
            other
                .catalogue(catalogue(Some(AGENTS_PROVIDER)))
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(
            !other
                .catalogue(catalogue(None))
                .await
                .unwrap()
                .providers
                .iter()
                .any(|entry| entry.provider == AGENTS_PROVIDER)
        );
    }
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
    assert!(matches!(
        guest.invoke(ask()).await.status,
        ReceiptStatus::Rejected {
            error: tilo_actions::remote::ActionCallError::Refused { reason },
        } if reason == "only the active keeper of this hosted roster agent can manage its grant here"
    ));
    let standalone = page
        .invoke(request(
            AGENTS_PROVIDER,
            ActionAddress::new(SUPERVISOR_RESOURCE, REQUEST_GRANT_ACTION),
            json(&serde_json::json!({"label": "Standalone", "workspaces": [workspace.workspace]})),
        ))
        .await;
    assert!(
        matches!(standalone.status, ReceiptStatus::Rejected { .. }),
        "a grant names its roster entry: {standalone:?}"
    );
    for receipt in [
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
    let offered = page
        .catalogue(catalogue(Some(AGENTS_PROVIDER)))
        .await
        .unwrap();
    assert!(offered.rows.iter().any(|row| {
        row.action
            .as_ref()
            .is_some_and(|action| action.address == ActionAddress::new(&resource, REVOKE_ACTION))
    }));
    assert_eq!(first, output(repeated));
    let waiting = output(second.invoke(status()).await);
    assert_eq!(waiting["state"], "pending");
    assert_eq!(waiting["ready"], false);
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
        if grant_state(&hosted, agent, workspace.workspace_id) == Some(GrantState::Denied) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the keeper's denial was not recorded"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(sessions.link(&first_device).is_none());
    assert_eq!(output(second.invoke(status()).await)["state"], "denied");
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
    assert!(sessions.link(&device).is_none());
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
            grant_state(&hosted, agent, workspace.workspace_id)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
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
        tilo_agent_session::record_actions::RESOURCE_PROVIDER,
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

    let roster = &hosted.roster;
    let workbook = page.create(SHEETS);
    let tracker = page.create("so.tilo.tasks");
    // A letter in Writer, which the agent's grant does not name.
    let letter = page.create("so.tilo.word-processor");
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
    let (workbook_before, tracker_before, letter_before) = loop {
        if let (Some(sheets), Some(tasks), Some(letter)) = (
            read(&workbook).await,
            read(&tracker).await,
            read(&letter).await,
        ) {
            break (sheets, tasks, letter);
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
            // Writer is no app its grant names.
            serde_json::json!({"path": "word-processor#append-paragraph", "args": {"text": "Outside the grant"}}),
            edit(&denied, "protected", "Denied"),
            edit(&accepted, "title", "Updated"),
        ],
        "I worked on the sheet.",
        Arc::clone(&requests),
    );
    let runner = ServerAgents::with_sessions(
        workspaces.clone(),
        roster.actions.clone(),
        Arc::new(hosted.server.agent_link()),
        roster.sessions.clone(),
    )
    .scripted(model);
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
            refused.1 && refused.0.contains("may not invoke this action"),
            "{refused:?}"
        );
        let denied = answered_to(final_request, "call-3");
        assert!(denied.1 && denied.0.contains("not permitted"), "{denied:?}");
        let updated = answered_to(final_request, "call-4");
        assert!(!updated.1, "{updated:?}");
    }
    ended.unwrap();
    assert_ne!(read(&workbook).await.unwrap(), workbook_before);
    assert_eq!(read(&tracker).await.unwrap(), tracker_before);
    assert_eq!(
        read(&letter).await.unwrap(),
        letter_before,
        "the refused write outside the grant changed nothing"
    );
    let signed = roster_write(&hosted, workspace.workspace_id, &workbook, device);
    assert_eq!(signed.len(), 1, "{signed:?}");
    // The agent's turn asks the workspace only under its own key, which the host checks
    // before anything runs: unsigned, nothing runs (its signed sheet above ran), and signed by
    // it, what its grant does not reach is refused.
    let created_at = workspaces
        .hosted_roster_agent(workspace.workspace_id, agent, &keeper, None)
        .await
        .unwrap()
        .created_at;
    let as_agent = tilo_actions::remote::Caller::delegated_agent(agent, keeper, device)
        .in_workspace(&workspace.workspace)
        .on_roster(created_at, keeper)
        .on_roster_turn(tilo_action_policy::turn::Turn::new(
            String::new(),
            String::new(),
        ));
    let direct = Direct {
        actions: &roster.actions,
        sessions: &sessions,
        workspace: workspace.workspace_id,
        caller: &as_agent,
        device,
    };
    let turn = tilo_workspace_service::workspaces::hosted_use::HostedTurn {
        workspace: workspace.workspace_id,
        caller: &as_agent,
        bound: None,
    };
    assert!(matches!(
        workspaces.secret_ask_for_turn(&turn).await,
        Err(tilo_actions::remote::ActionCallError::NotPermitted)
    ));
    assert!(workspaces.uses_offered(&turn).await.is_empty());
    assert!(workspaces.connection_rows_for_turn(&turn).await.is_empty());
    assert!(workspaces.mail_connect_offered(&turn).await.is_empty());
    assert!(workspaces.calendar_connect_offered(&turn).await.is_empty());
    // Services beside the records must honor the signed app list on a turn too.
    {
        use tilo_actions::remote::{
            ActionCallError, ActionProvider, workspace_events, workspace_initiative,
        };
        let tools = tilo_workspace_service::agents::WorkspaceActionProvider::new(
            roster.actions.clone(),
            workspace.workspace_id,
        )
        .offering(roster.events.clone())
        .offering_initiative(roster.initiative.clone());
        let dm = chat::Chat::direct_id([
            tilo_actions::MemberId::person(hex::encode(keeper)),
            tilo_actions::MemberId::agent(agent),
        ]);
        let turn = as_agent
            .clone()
            .on_roster_turn(tilo_action_policy::turn::Turn::new(
                chat::summons::conversation_address(&dm),
                "ask",
            ));
        for (provider, resource, list) in [
            (
                workspace_events::PROVIDER,
                workspace_events::RESOURCE,
                workspace_events::LIST,
            ),
            (
                workspace_initiative::PROVIDER,
                workspace_initiative::PROPOSALS,
                workspace_initiative::LIST,
            ),
        ] {
            let offers = tools
                .query(&turn, catalogue(Some(provider)))
                .await
                .unwrap()
                .unwrap();
            assert!(
                offers.rows.is_empty(),
                "{provider} was offered outside the grant"
            );
            let answer = tools
                .invoke(
                    &turn,
                    request(
                        provider,
                        ActionAddress::new(resource, list),
                        InvokeArgument::None,
                    ),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(
                    answer.status,
                    ReceiptStatus::Rejected {
                        error: ActionCallError::NotPermitted
                    }
                ),
                "{answer:?}"
            );
        }
    }
    let unsigned = direct
        .run(sheet_request(&workbook, "Unsigned"), false)
        .await;
    assert!(
        matches!(&unsigned, ReceiptStatus::Rejected {
            error: tilo_actions::remote::ActionCallError::Refused { reason },
        } if reason.starts_with("invocation_unsigned")),
        "{unsigned:?}"
    );
    let outside = direct
        .run(
            request(
                &tilo_agent_session::resource_provider(&letter),
                ActionAddress::new("", "append-paragraph"),
                json(&serde_json::json!({"text": "Outside the grant"})),
            ),
            true,
        )
        .await;
    assert_eq!(
        outside,
        ReceiptStatus::Rejected {
            error: tilo_actions::remote::ActionCallError::NotPermitted
        }
    );
    // Signed now, sent once its grant is withdrawn ([`REVOKE_ACTION`] below).
    let mut late = sheet_request(&workbook, "After the grant is withdrawn");
    sessions
        .sign_as(&device, workspace.workspace_id, &mut late)
        .unwrap();
    let (count, _) = retained_sealed(&hosted, workspace.workspace_id, &tracker, device);
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
        if grant_state(&hosted, agent, elsewhere.workspace_id) == Some(GrantState::Denied) {
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
        &format!("{AGENT_RESOURCE_PREFIX}{agent}"),
        REVOKE_ACTION,
        InvokeArgument::None,
    )
    .await;
    let after = direct
        .send(tilo_actions::remote::RemoteActionMessage::Invoke(late))
        .await;
    assert!(
        matches!(after, ReceiptStatus::Rejected { .. }),
        "a withdrawn grant runs nothing its key signed: {after:?}"
    );
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
    assert_eq!(output(second.invoke(status()).await)["state"], "revoked");
    let fresh = supervise(
        &mut page,
        &resource,
        "request-chat-grant",
        InvokeArgument::None,
    )
    .await;
    assert_eq!(fresh["grant"], agent);
    let proposal = approver
        .list_agent_proposals()
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(proposal.proposal.scope.workspaces, [workspace.workspace_id]);
    assert_eq!(proposal.proposal.scope.apps, ["so.tilo.chat"]);
    assert_eq!(output(second.invoke(status()).await)["state"], "pending");
    supervise(&mut page, &resource, REVOKE_ACTION, InvokeArgument::None).await;
    assert!(approver.list_agent_proposals().await.unwrap().is_empty());
    let selected = output(page.invoke(choose(chosen)).await);
    assert_eq!(
        selected,
        output(
            second
                .invoke(choose(
                    serde_json::json!({"apps":["so.tilo.chat","so.tilo.mail"]})
                ))
                .await
        )
    );
    let selected = approver.list_agent_proposals().await.unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(
        selected[0].proposal.scope.workspaces,
        [workspace.workspace_id]
    );
    assert_eq!(
        selected[0].proposal.scope.apps,
        ["so.tilo.chat", "so.tilo.mail"]
    );
    assert_eq!(
        output(second.invoke(status()).await)["apps"],
        serde_json::json!(["so.tilo.chat", "so.tilo.mail"])
    );
    assert!(matches!(
        page.invoke(choose(
            serde_json::json!({"apps":["so.tilo.chat", "tilo.models"]})
        ))
        .await
        .status,
        ReceiptStatus::Rejected { .. }
    ));
    supervise(&mut page, &resource, REVOKE_ACTION, InvokeArgument::None).await;
    supervise(
        &mut page,
        &resource,
        "request-assistant-work-grant",
        InvokeArgument::None,
    )
    .await;
    let proposals = approver.list_agent_proposals().await.unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(
        proposals[0].proposal.scope.apps,
        [
            "so.tilo.chat",
            "so.tilo.tasks",
            "so.tilo.assistant",
            "so.tilo.shortcuts"
        ]
    );
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
    let restored = roster_write(&reopened, workspace.workspace_id, &workbook, device);
    assert_eq!(
        restored, signed,
        "the same signed action is kept after reopening"
    );
    assert_eq!(
        reopened
            .server
            .workspaces()
            .unwrap()
            .resource_document(&workspace.workspace, &tracker.app_id, &tracker.document)
            .await
            .unwrap()
            .unwrap(),
        tracker_before,
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
            None,
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
        assert_eq!(
            stamp.invoked_by(),
            Some(tilo_actions::Actor::Member(tilo_actions::MemberId::agent(
                agent
            )))
        );
        assert!(!stamp.membership.is_empty());
    }
    let envelopes: Vec<_> = hosted
        .server
        .workspace_documents()
        .retained(WorkspaceId::parse(&route.cluster_id).unwrap())
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

fn roster_write(
    hosted: &super::Hosted,
    workspace: WorkspaceId,
    route: &DocumentRoute,
    device: [u8; 32],
) -> Vec<collab_types::SignedOp> {
    hosted
        .server
        .workspace_documents()
        .retained(workspace)
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
