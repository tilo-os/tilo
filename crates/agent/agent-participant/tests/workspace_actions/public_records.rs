//! The host's action service in a public workspace — one listed in its host's
//! directory, which anyone may walk into.
//!
//! Such a workspace replays a device only its governance and the shared
//! desktop, and sends every other record once that device opens it, saying so
//! in the `Ready` that ends the replay. The service that answers a page's or an
//! agent's `tilo_read` and `tilo_do` joins the workspace as the host's own
//! device, so what a member wrote after the service's session joined reached it
//! not at all: a request naming that record was refused as one the workspace
//! has none of, and nothing could be read or done there.
//!
//! A member of the public workspace writes a note, and the host's service reads
//! it by id and acts on it: it lists the record's actions, the note the member
//! made named by the title they gave it, and makes another note there, whose
//! change the member's page then sees. A record the workspace holds none of is
//! still refused as absent, once the service heard the host say so.
//!
//! What the workspace holds is the host's to say, and a listing of it is what a
//! member's agent names a record by: the chat the operator seated there is one
//! of the workspace's records whether or not the service's own session was ever
//! sent it, and a post goes into the channel it opens with.

use super::own_agent::{listed_resources, page_paragraphs};
use super::resource_support::{admit_person, open_admitted_peer};
use super::*;
use serde_json::{Value, json};
use tilo_actions::remote::{CataloguePage, ReceiptStatus};
use tilo_peer_state::join::MemberRole;
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_core::remote::SessionActions;
use tilo_user_identity::DeviceCapabilities;

const NOTES: &str = "so.tilo.notes";

/// The app a workspace keeps one record of, whose channel its operator names.
const CHAT: &str = "so.tilo.chat";

/// The channel the operator gives the workspace's chat.
const CHANNEL: &str = "general";

/// The title the member's own note holds.
const TITLE: &str = "Kickoff";

/// The address `replica` publishes `action` under.
fn address_of(replica: &Replica, action: &str) -> ActionAddress {
    replica
        .survey()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
        .unwrap_or_else(|| panic!("the record publishes {action}"))
        .address
}

/// The host's catalogue of `provider`, asked again while the service is still
/// joining the workspace: a host that has not joined yet says so, and the
/// caller asks again, as a page does.
async fn catalogue(page: &mut Browser, provider: &str) -> Result<CataloguePage, ActionHostError> {
    catalogue_under(page, provider, None).await
}

/// [`catalogue`] of one resource's children: what the host lists where
/// `parent` stands.
async fn catalogue_under(
    page: &mut Browser,
    provider: &str,
    parent: Option<&str>,
) -> Result<CataloguePage, ActionHostError> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let query = CatalogueQuery {
            provider: Some(provider.to_owned()),
            parent_resource: parent.map(str::to_owned),
            max_results: 256,
            ..CatalogueQuery::default()
        };
        match page.catalogue(query).await {
            Err(ActionHostError::Unavailable { reason }) => {
                assert!(
                    Instant::now() < deadline,
                    "the host never joined the workspace: {reason}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_action_service_reads_and_acts_on_a_public_workspaces_records() {
    // The workspace, public: anyone may walk in, and its host sends a device
    // its records when it opens them.
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let host = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host.cert,
            Listing {
                description: "Everyone".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();

    // A member of the workspace wrote a note there.
    let (member_root, _) = tilo_user_identity::UserIdentity::generate("Member", 2);
    let member_device = enrol(&member_root, "member-browser", DeviceCapabilities::member());
    admit_person(
        &hosted,
        &host,
        &workspace,
        &member_device,
        MemberRole::Editor,
    )
    .await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &workspace).await;
    let mut note =
        crate::common::open_in(&workspace, NOTES, "member", ReplicaActor::Owner, Seed::New)
            .unwrap();
    let new_note = address_of(&note, "new-note");
    note.invoke(
        &new_note,
        serde_json::json!({ "title": TITLE }).as_object().unwrap(),
        false,
    )
    .unwrap();
    let route = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        record: note.wire_record().to_owned(),
    };
    crate::support::written(
        &member.peer,
        &route,
        &note.take_mutation().unwrap().unwrap(),
    )
    .await;
    let provider = record_provider(&route);

    // The host's own action service never had the note: its session was sent
    // the workspace's governance and desktop, and nothing else until asked.
    let page = catalogue(&mut member, &provider)
        .await
        .expect("the host serves the record the member wrote");
    // Read: the rows are what the note's own app publishes over the document
    // the member wrote, at the note they made, so the host holds their
    // document and not an empty one of its own.
    assert!(
        !page.rows.is_empty(),
        "the note publishes its actions to the member"
    );
    // The note is a page of the record: it is listed where it stands, under
    // the notebook the record publishes.
    let Some(notebook) = page
        .rows
        .iter()
        .find(|row| row.resource.id == "notes.notebook")
        .map(|row| row.resource.id.clone())
    else {
        panic!("the host lists the notebook the note is in: {page:?}");
    };
    let pages = catalogue_under(&mut member, &provider, Some(&notebook))
        .await
        .expect("the host serves the record the member wrote");
    let named = pages.rows.iter().any(|row| {
        [
            Some(&row.resource.kind),
            Some(&row.resource.label),
            row.action.as_ref().map(|action| &action.resource_label),
        ]
        .into_iter()
        .flatten()
        .any(|label| label.contains(TITLE))
    });
    assert!(
        named,
        "the host lists the note the member wrote among the notebook's pages: {pages:?}"
    );

    // Act: the host makes a note of its own in the member's record, and the
    // member's page is sent the change as an ordinary durable one.
    let receipt = member
        .invoke(request(
            &provider,
            new_note.clone(),
            InvokeArgument::Json(
                serde_json::json!({ "title": "Second" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
        ))
        .await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the host made a note in the member's record: {receipt:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        member.pump();
        let authored = member
            .documents
            .get(&route)
            .into_iter()
            .flatten()
            .any(|checkpoint| checkpoint.0 == host.cert.device_public_key);
        if authored || Instant::now() >= deadline {
            assert!(
                authored,
                "the host's change reached the member's page in their record"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A record the workspace holds none of is still absent: the host asks,
    // is answered it holds none of it, and refuses rather than making one.
    let missing = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        record: ui::record::RecordId::minted().as_str().to_owned(),
    };
    let refused = catalogue(&mut member, &record_provider(&missing)).await;
    assert!(
        matches!(
            refused,
            Err(ActionHostError::UnknownAction | ActionHostError::NotPermitted)
        ),
        "a record the workspace has none of is refused: {refused:?}"
    );
    member.pump();
    assert!(
        !member.documents.contains_key(&missing),
        "the host founded a record the workspace has none of"
    );

    member.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.relay.shutdown().await;
}

/// A public workspace — anyone may walk in, and its host sends a device its
/// records when it opens them — whose chat the operator seated: the app's own
/// record of the workspace, holding the channel it opens with. Nobody's
/// device has it open, and the workspace sends it to no one who does not.
async fn lounge_with_chat() -> (Hosted, Device, String, tempfile::TempDir) {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let host = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host.cert,
            Listing {
                description: "Everyone".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();

    // Its chat, as an operator seats it: the app's own record of the workspace,
    // holding the channel it opens with. Nobody's device has it open, and the
    // workspace sends it to no one who does not.
    workspaces
        .prepare_application(
            &workspace,
            &host.cert,
            &crate::common::catalogue(),
            CHAT,
            "",
        )
        .await
        .unwrap();
    workspaces
        .author_application(
            &workspace,
            &host.cert,
            &crate::common::catalogue(),
            CHAT,
            "",
            |doc| {
                // The channel the operator seats, under the id the workspace's
                // own settle gives it (`c_` and its name).
                chat::demo::apply_as(
                    doc,
                    "operator",
                    chat::Chat::CREATE_CHANNEL_WITH_ID.with(
                        chat::record::chat::CreateChannelWithId {
                            id: format!("c_{CHANNEL}"),
                            name: CHANNEL.parse().expect("a channel name"),
                            category: None,
                            kind: None,
                            topic: None,
                            participants: None,
                        },
                    ),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();

    (hosted, host, workspace, state)
}

/// What an agent through the door asks before it names a record: what runs in
/// the workspace. The chat its operator seated there is one of them, and the
/// channel it opens with is where a post goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_action_service_lists_a_public_workspaces_chat_it_was_never_sent() {
    let (hosted, host, workspace, _state) = lounge_with_chat().await;

    // A member of the workspace, through its door.
    let (member_root, _) = tilo_user_identity::UserIdentity::generate("Member", 2);
    let member_device = enrol(&member_root, "member-browser", DeviceCapabilities::member());
    admit_person(
        &hosted,
        &host,
        &workspace,
        &member_device,
        MemberRole::Editor,
    )
    .await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &workspace).await;

    // What runs there, as the host's service lists it: the chat is one, though
    // the member's device was sent none of its record. A page names the app it
    // acts on by this listing, and holds every app as installed rather than
    // running without it.
    let chat = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        record: String::new(),
    };
    let chat_provider = record_provider(&chat);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let page = member
            .catalogue(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            })
            .await
            .expect("the host lists the workspace");
        if page
            .providers
            .iter()
            .any(|listed| listed.provider == chat_provider)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace's chat is never listed as running: {:?}",
            page.providers
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A post to the channel the operator gave it reads the chat the workspace
    // holds: the service asks for the record and its newest part, and the
    // message lands in the channel that is really there.
    let post = ActionAddress::new(ui::record::chain_resource_id(&chat::parts::chain()), "post");
    let posted = |channel: &str| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("channel".into(), serde_json::json!(channel));
        arguments.insert("text".into(), serde_json::json!("first in the Lounge"));
        request(
            &chat_provider,
            post.clone(),
            InvokeArgument::Json(arguments),
        )
    };
    let receipt = member.invoke(posted("c_general")).await;
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the post reached the workspace's own chat: {:?}",
        receipt.status
    );
    // A channel the workspace's chat does not have is refused: the service read
    // the chat the workspace holds, not one of its own.
    let refused = member.invoke(posted("c_missing")).await;
    assert!(
        !matches!(refused.status, ReceiptStatus::Completed { .. }),
        "a channel the chat has not got took a post: {:?}",
        refused.status
    );

    member.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.relay.shutdown().await;
}

/// What the host keeps of the workspace's chat: its writes, which a chat
/// founded again would add to.
async fn chat_history(hosted: &Hosted, workspace: &str) -> (Vec<u8>, usize) {
    let history = hosted
        .relay
        .workspaces()
        .unwrap()
        .record_history(workspace, CHAT, "")
        .await
        .unwrap()
        .expect("the host keeps the workspace's chat");
    (history.base, history.writes.len())
}

/// A member's agent opens Chat in a public workspace whose chat the operator
/// seated. The service holds none of that record until asked, and Chat keeps
/// one record per workspace: opening it must open the chat the workspace
/// holds, never found it again as if it were new, which would lay an empty
/// chat's genesis over the one people talk in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_opening_chat_in_a_public_workspace_opens_the_chat_it_has() {
    let (hosted, host, workspace, _state) = lounge_with_chat().await;
    let before = chat_history(&hosted, &workspace).await;

    // A member of the workspace, whose device runs their agent.
    let (member_root, _) = tilo_user_identity::UserIdentity::generate("Member", 2);
    let member_device = enrol(&member_root, "member-agent", DeviceCapabilities::member());
    admit_person(
        &hosted,
        &host,
        &workspace,
        &member_device,
        MemberRole::Editor,
    )
    .await;
    let mut member = open_admitted_peer(&hosted, &member_device, &host, &workspace).await;
    tilo_session_client::device::run_as_agent(&member.peer, "Claude Code")
        .await
        .expect("the member's device runs their agent");

    // The agent opens Chat, as `app.so.tilo.chat#open` asks.
    let opened = member
        .invoke(request(
            &format!("workspace:{workspace}"),
            ActionAddress::new(format!("app.{CHAT}"), "open"),
            InvokeArgument::None,
        ))
        .await;
    let ReceiptStatus::Completed { output } = &opened.status else {
        panic!("the agent opened Chat: {opened:?}");
    };
    let chat = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.clone(),
        record: String::new(),
    };
    assert_eq!(
        output.value.as_ref().map(|value| value["provider"].clone()),
        Some(serde_json::json!(record_provider(&chat))),
        "the open names the workspace's own chat: {output:?}"
    );

    // Nothing was founded: the chat the host keeps is the one the operator
    // seated, write for write.
    assert!(
        chat_history(&hosted, &workspace).await == before,
        "opening Chat wrote to the workspace's chat as if it were new"
    );

    // And it is the chat people talk in: its channel takes a post.
    let mut arguments = serde_json::Map::new();
    arguments.insert("channel".into(), serde_json::json!(format!("c_{CHANNEL}")));
    arguments.insert("text".into(), serde_json::json!("hello from the agent"));
    let posted = member
        .invoke(request(
            &record_provider(&chat),
            ActionAddress::new(ui::record::chain_resource_id(&chat::parts::chain()), "post"),
            InvokeArgument::Json(arguments),
        ))
        .await;
    assert!(
        matches!(posted.status, ReceiptStatus::Completed { .. }),
        "the agent posts in the chat it opened: {:?}",
        posted.status
    );

    member.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.relay.shutdown().await;
}

/// A visitor's agent writing in the public workspace's Notes, which the
/// visitor already has open, as `tilo mcp` runs it: after every call the
/// records it changed are shown as windows on the person's desk while its
/// next call runs. Every paragraph the host answered `ok` for is in the note
/// the person's page holds, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_paragraph_a_visitors_agent_was_told_it_wrote_reaches_their_open_notes() {
    // A public workspace, as a hosted Tilo runs one: anyone may walk in, and
    // its host sends a device a record once it opens it.
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Owner", 1);
    let host = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host.cert,
            Listing {
                description: "Everyone".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let workspace = lounge.workspace.clone();
    // A visitor, their page and the computer their agent runs on.
    let (root, _) = tilo_user_identity::UserIdentity::generate("Visitor", 2);
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    admit_person(
        &hosted,
        &host,
        &workspace,
        &browser_device,
        MemberRole::Editor,
    )
    .await;
    let mut browser = open_admitted_peer(&hosted, &browser_device, &host, &workspace).await;
    // They opened Notes before their agent came: their page founded it.
    let mut founded =
        crate::common::open_in(&workspace, NOTES, "visitor", ReplicaActor::Owner, Seed::New)
            .unwrap();
    let notes_route = DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: workspace.clone(),
        record: founded.wire_record().to_owned(),
    };
    crate::support::written(
        &browser.peer,
        &notes_route,
        &founded.take_mutation().unwrap().unwrap(),
    )
    .await;
    admit_person(&hosted, &host, &workspace, &computer, MemberRole::Editor).await;
    let agent = open_admitted_peer(&hosted, &computer, &host, &workspace).await;
    let _ = tilo_session_client::device::run_as_agent(&agent.peer, "Claude Code").await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let actions = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let windows = std::sync::Arc::new(tilo_runtime::mcp_windows::AgentWindows::new(
        actions.clone(),
        person.clone(),
        workspace.clone(),
    ));
    let server = McpServer::new(actions, workspace.clone(), person)
        .with_surface(Surface::Agent)
        .with_agent_run("run-9", "Claude Code");
    // One call as `tilo mcp` serves it: answered, then its records shown on
    // the desk beside the calls that follow.
    let call = async |name: &str, arguments: Value| {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                             "params": {"name": name, "arguments": arguments}});
        let answer = server.handle(request.clone()).await.expect("answered");
        let (changed, run) = (server.take_changed(), server.run_of(&request));
        let windows = std::sync::Arc::clone(&windows);
        tokio::spawn(async move { windows.changed(changed, run).await });
        let text = answer["result"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let failed = answer["result"]["isError"] == true || answer.get("error").is_some();
        assert!(!failed, "{name} {arguments}: {text} {answer}");
        text
    };

    call("tilo_ls", json!({"path": "notes"})).await;
    call(
        "tilo_do",
        json!({"path": "notes#new-note", "args": {"title": "Launch notes"}}),
    )
    .await;
    let listing = call("tilo_ls", json!({"path": "notes"})).await;
    let notebook = listed_resources(&listing)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("notes lists a notebook: {listing}"));
    let pages = call("tilo_ls", json!({"path": format!("notes/{notebook}")})).await;
    let page = listed_resources(&pages)
        .into_iter()
        .find(|page| page.contains("Launch notes"))
        .unwrap_or_else(|| panic!("the notebook lists the note: {pages}"));
    let note = format!("notes/{notebook}/{page}");
    let parts = call("tilo_ls", json!({"path": note})).await;
    let body = listed_resources(&parts)
        .into_iter()
        .map(|part| format!("{note}/{part}"))
        .next()
        .unwrap_or_else(|| panic!("the note lists its body: {parts}"));
    let write = format!("{body}#append-paragraph");
    let batched = [
        "Launch notes, from Claude Code.",
        "Each line lands in the workspace as I write it.",
        "Following me puts this window in front of you.",
    ];
    call(
        "tilo_do",
        json!({"calls": batched
            .iter()
            .map(|text| json!({"path": write, "args": {"text": text}}))
            .collect::<Vec<_>>()}),
    )
    .await;
    let single = [
        "A second invocation, a moment later.",
        "Ship the hosted page with the agent invite.",
        "Check the phone layout once more.",
        "Write the release note last.",
    ];
    for text in single {
        call("tilo_do", json!({"path": write, "args": {"text": text}})).await;
    }
    let sent: Vec<&str> = batched.iter().chain(single.iter()).copied().collect();
    let read = call("tilo_read", json!({"path": note})).await;
    for line in &sent {
        assert!(read.contains(line), "the host reads {line:?}: {read}");
    }
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let seen = page_paragraphs(&mut browser, "Launch notes");
        if seen.iter().any(|texts| texts == &sent) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the page never read the note as written ({sent:?}): {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
