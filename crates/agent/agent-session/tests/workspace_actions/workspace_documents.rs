//! What an app keeps for its whole workspace beside its own document, reached through the
//! workspace's server with nobody's page open.
//!
//! Chat keeps the workspace's agents in a document of their own, which every app of the
//! workspace reads. Whoever is outside a window (a native app, an agent, an automation, a
//! script with a key) reaches a workspace through its server, so the server offers that
//! document's read and its actions under Chat, as it offers Chat's conversations: to the
//! person whose server it is, and to every member as far as the workspace's grants let them.
//! Someone the workspace does not admit is offered nothing and changes nothing.

use super::super::*;
use super::{Membership, ended, invoked, json, listed};
use tilo_actions::remote::Caller;
use ui::permissions::Role;

const CHAT: &str = "so.tilo.chat";
/// The app that keeps the workspace's agents.
const AGENTS_OWNER: &str = ui::tilo_workspace_agent::OWNER;
/// The kind of the workspace's agents, which is also what a caller addresses them by.
const AGENTS: &str = ui::tilo_workspace_agent::KIND;
/// The kind of the workspace's emoji, which Chat keeps beside them.
const EMOJI: &str = "tilo.workspace.emoji";

/// A request to make the agent `name`.
fn make(provider: &str, name: &str) -> InvokeRequest {
    request(
        provider,
        ActionAddress::new(AGENTS, "create-agent"),
        json(serde_json::json!({ "name": name })),
    )
}

/// A request to read the workspace's agents.
fn read(provider: &str) -> InvokeRequest {
    request(
        provider,
        ActionAddress::new(AGENTS, "read"),
        InvokeArgument::None,
    )
}

/// The answer to a caller the workspace does not let do what it asked.
fn refused() -> ReceiptStatus {
    ReceiptStatus::Rejected {
        error: ActionCallError::NotPermitted,
    }
}

/// What a completed read holds, as text; a read that did not complete fails the test.
fn held(status: ReceiptStatus) -> String {
    match status {
        ReceiptStatus::Completed { output } => output.value.unwrap_or_default().to_string(),
        other => panic!("the agents are read: {other:?}"),
    }
}

/// What the workspace's own server answers a device of its person reading the agents, once
/// the answer names `name`.
async fn found_by_its_person(browser: &mut Browser, provider: &str, name: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let status = browser.invoke_settled(read(provider)).await.status;
        if let ReceiptStatus::Completed { output } = &status {
            let agents = output.value.clone().unwrap_or_default().to_string();
            if agents.contains(name) {
                return agents;
            }
        }
        assert!(
            Instant::now() < deadline,
            "a device of the server's own person never reads {name}: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Chat's own document and Assistant's own document held where no window is open offer what
/// each keeps for the workspace once told that the workspace holds none of it: emoji under
/// Chat, agents under Assistant, each with its read and its actions.
#[test]
fn chats_document_offers_what_chat_keeps_for_the_workspace_once_told() {
    let mut chat = crate::common::open_in(
        WORKSPACE,
        CHAT,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut assistant = crate::common::open_in(
        WORKSPACE,
        AGENTS_OWNER,
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let surveyed = |replica: &Replica, prefix: &str| -> Vec<String> {
        replica
            .survey()
            .iter()
            .map(|offered| {
                format!(
                    "{}#{}",
                    offered.address.resource.as_str(),
                    offered.address.action
                )
            })
            .filter(|offered| offered.starts_with(prefix))
            .collect()
    };
    assert_eq!(
        surveyed(&chat, EMOJI),
        Vec::<String>::new(),
        "nothing of emoji opens before the document is told"
    );
    assert_eq!(
        surveyed(&assistant, AGENTS),
        Vec::<String>::new(),
        "nothing of agents opens before the document is told"
    );
    chat.workspace_answered(
        ui::resource::DocumentId::parse(&collab_types::workspace_document_id(EMOJI)).unwrap(),
        ui::resource::WorkspaceAnswer::HoldsNone,
    );
    assistant.workspace_answered(
        ui::resource::DocumentId::parse(&collab_types::workspace_document_id(AGENTS)).unwrap(),
        ui::resource::WorkspaceAnswer::HoldsNone,
    );
    let chat_offered = surveyed(&chat, EMOJI);
    for expected in [format!("{EMOJI}#add-emoji"), format!("{EMOJI}#read")] {
        assert!(
            chat_offered.contains(&expected),
            "{expected} in {chat_offered:?}"
        );
    }
    let agents_offered = surveyed(&assistant, AGENTS);
    for expected in [format!("{AGENTS}#create-agent"), format!("{AGENTS}#read")] {
        assert!(
            agents_offered.contains(&expected),
            "{expected} in {agents_offered:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspaces_agents_are_found_and_made_through_its_server_with_no_page_open() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let (outsider, _) = tilo_user_identity::UserIdentity::generate("Mallory", 1);
    let server = enrol(&root, "home-server", DeviceCapabilities::member());
    let own_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let outsider_device = enrol(&outsider, "outsider-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &server, state.path()).await;
    let mut browser = Browser::open(&hosted, &own_device, &server).await;
    // Assistant's own document of the workspace, made and closed again: from here on no window
    // of Assistant is open on any device, and nobody ever made an agent.
    let assistant_doc = browser.create(AGENTS_OWNER);
    crate::workspace_actions::saved(&browser.peer.barrier().unwrap()).unwrap();
    let provider = resource_provider(&assistant_doc);

    let membership = Membership::of(
        &hosted,
        &server,
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
    // What a caller is listed under Chat, as "resource#action".
    let listing = |caller: &Caller| -> Vec<String> {
        listed(&actions, workspace, caller, &provider)
            .iter()
            .map(|offered| {
                format!(
                    "{}#{}",
                    offered.address.resource.as_str(),
                    offered.address.action
                )
            })
            .collect()
    };
    let offers =
        |caller: &Caller, action: &str| listing(caller).contains(&format!("{AGENTS}#{action}"));

    // An editor is offered the workspace's agents under Chat, makes one and reads it back.
    membership.set_role(Some(Role::Editor));
    let offered = listing(&member);
    for action in ["read", "create-agent"] {
        assert!(
            offered.contains(&format!("{AGENTS}#{action}")),
            "an editor is offered {action} on the workspace's agents: {offered:?}"
        );
    }
    let made = invoked(&actions, workspace, &member, &make(&provider, "Scout"));
    assert!(
        matches!(made, ReceiptStatus::Completed { .. }),
        "an editor makes an agent: {made:?}"
    );
    let agents = held(invoked(&actions, workspace, &member, &read(&provider)));
    assert!(
        agents.contains("Scout"),
        "the agent just made is read: {agents}"
    );

    // The person whose server it is finds that agent from a device of theirs, and makes one
    // of their own there.
    found_by_its_person(&mut browser, &provider, "Scout").await;
    let made = browser.invoke_settled(make(&provider, "Reed")).await.status;
    assert!(
        matches!(made, ReceiptStatus::Completed { .. }),
        "the server's own person makes an agent: {made:?}"
    );
    let agents = found_by_its_person(&mut browser, &provider, "Reed").await;
    assert!(agents.contains("Scout"), "both are kept: {agents}");

    // A viewer and a commenter read the agents; neither is offered to make one, and one who
    // asks all the same is refused.
    for (role, who) in [(Role::Viewer, "a viewer"), (Role::Commenter, "a commenter")] {
        membership.set_role(Some(role));
        assert!(offers(&member, "read"), "{who} is offered the read");
        assert!(
            !offers(&member, "create-agent"),
            "{who} is not offered to make an agent"
        );
        let agents = held(invoked(&actions, workspace, &member, &read(&provider)));
        assert!(agents.contains("Scout"), "{who} reads the agents: {agents}");
        assert_eq!(
            invoked(&actions, workspace, &member, &make(&provider, "Not theirs")),
            refused(),
            "{who} makes no agent"
        );
    }

    // A person of another workspace is offered nothing and answered nothing, while the
    // member is an editor; so is the member once the workspace removed them.
    membership.set_role(Some(Role::Editor));
    let stranger = Caller::authenticated(
        outsider_device.cert.user_public_key,
        outsider_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let shut_out = |caller: &Caller, who: &str| {
        assert!(
            listed(&actions, workspace, caller, &provider).is_empty(),
            "{who} is offered nothing"
        );
        for asked in [make(&provider, "Not theirs"), read(&provider)] {
            assert_eq!(
                invoked(&actions, workspace, caller, &asked),
                refused(),
                "{who} is answered nothing"
            );
        }
    };
    shut_out(&stranger, "a person of another workspace");
    membership.set_role(None);
    shut_out(&member, "a member the workspace removed");

    // Nothing a refused caller asked for was made.
    membership.set_role(Some(Role::Viewer));
    let agents = held(invoked(&actions, workspace, &member, &read(&provider)));
    assert!(
        !agents.contains("Not theirs"),
        "a refused caller's agent is kept: {agents}"
    );
    actions.shutdown();
    ended(browser, hosted).await;
}
