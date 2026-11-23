//! Who ends a share an agent on a workspace's server made for its person, through the server's
//! own interfaces: the person themselves, from any device of theirs, and nobody else.
use super::*;

/// The word processor, by its id.
const WRITER: &str = "so.tilo.word-processor";

/// What these tests reach a workspace's server through.
type Actions = tilo_agent_session::workspace_actions::WorkspaceActions;

/// Admit `device`'s person to `workspace` as an editor, by an invitation its server issues, as
/// another member joins.
async fn admit(workspaces: &Workspaces, server: &Device, workspace: &str, device: &Device) {
    crate::workspace_actions::resource_support::admit_with(
        workspaces,
        &server.cert,
        workspace,
        device,
        tilo_peer_state::join::Role::Editor,
        "Bo",
    )
    .await;
}

/// A model that shares the word processor's document into `conversation` through the action
/// tool, a turn's own call of Chat's `share-window`, and says so once the tool answered.
fn shares_a_letter(conversation: &Id, asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let mut sharing = saying(conversation, "The letter, to work on together.");
    sharing["attached"] = serde_json::json!({"app": WRITER, "kind": "tilo.window",
        "id": WRITER, "label": "A letter"});
    sharing["access"] = serde_json::json!("edit");
    let call = serde_json::json!({"path": "chat/Messages#share-window", "args": sharing});
    plays(asked, move |request| {
        let answered = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .any(|content| matches!(content, Content::ToolResult(_)));
        if answered {
            says("The letter is above.")
        } else {
            answer(
                vec![Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "tilo_do".into(),
                    input: call.clone(),
                    extra_content: None,
                })],
                StopReason::ToolCalls,
                1_000,
            )
        }
    })
}

/// The person writes to their agent, and the turn that gives shares the letter into their
/// conversation: the share's message as the server keeps it, the agent's own, made for its
/// person as the server pinned it.
async fn shared_by_a_turn(
    served: &crate::harness::hosted::Hosted,
    actions: &Actions,
    workspaces: &Workspaces,
    workspace: &str,
    (root, browser): (&SigningKey, &Device),
) -> (Id, Kept) {
    let person = hex::encode(root.verifying_key().to_bytes());
    let conversation =
        chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    write_to_agent(
        actions,
        workspaces,
        workspace,
        (root, browser),
        "Show me the letter we are working on.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(served, workspace, shares_a_letter(&conversation, &asked)).await;
    // Boxed: the server's own futures stay out of the future of the test that waits here.
    let turns = Box::pin(agents.turns(workspace)).await.unwrap();
    assert_eq!(turns.len(), 1, "her message is the agent's turn: {turns:?}");
    Box::pin(agents.answer(workspace, &turns[0])).await.unwrap();
    let part = conversation_part(workspaces, workspace).await;
    let (shared, made) = part
        .messages
        .iter()
        .find(|(_, held)| held.share.is_some())
        .map(|(id, held)| (id.clone(), held.clone()))
        .expect("the turn shared the document");
    assert_eq!(
        made.author,
        MemberId::agent(SCOUT),
        "the share is the agent's own message"
    );
    assert_eq!(
        made.acting
            .as_ref()
            .and_then(tilo_actions::Acting::acted_for_person),
        Some(person.as_str()),
        "made for its person, as the server pinned it: {:?}",
        made.acting
    );
    assert_eq!(
        conversation_id(&made),
        &conversation,
        "in the conversation the turn answers in"
    );
    (shared, made)
}

/// Another member of the workspace, whose own words reach Chat.
async fn another_member(
    actions: &Actions,
    workspaces: &Workspaces,
    workspace: &str,
    server: &Device,
) -> Caller {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let browser = enrol(&root, "bo-browser", DeviceCapabilities::member());
    admit(workspaces, server, workspace, &browser).await;
    write_to_agent(
        actions,
        workspaces,
        workspace,
        (&root, &browser),
        "Hello from another member.",
    )
    .await;
    themselves(&root, &browser, workspace)
}

/// `caller` invokes Chat's `action` on the message `message` with `more` arguments, through
/// the server's action service.
async fn on_message(
    actions: &Actions,
    workspace: &str,
    caller: &Caller,
    (message, action): (&Id, &str),
    more: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut arguments = more;
    arguments["message"] = serde_json::json!(message.as_str());
    chat_action(
        actions,
        workspace,
        caller,
        (
            chat::parts::part(0).as_str(),
            &format!("{}/messages/{message}", chat::parts::ChatMessages::KIND),
        ),
        action,
        arguments,
    )
    .await
}

/// The message `message` as the server keeps it now.
async fn kept(workspaces: &Workspaces, workspace: &str, message: &Id) -> Kept {
    conversation_part(workspaces, workspace)
        .await
        .messages
        .get(message)
        .cloned()
        .expect("the message stays")
}

/// The message `message` once the server keeps its share as ended.
async fn once_ended(workspaces: &Workspaces, workspace: &str, message: &Id) -> Kept {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let held = kept(workspaces, workspace, message).await;
        if held.share.as_ref().is_some_and(|share| share.ended) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "the share never ended: {:?}",
            held.share
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A turn of a person's agent shares a document into their conversation. Another member of
/// the workspace does not end that share or say whether its window is open, and neither does
/// what acts for the person. The person ends it themselves, from a second device of theirs,
/// and the message stays, saying what was shared. What the others may do there, and whether
/// its window is open, stay its sharer's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_ends_from_any_device_the_share_their_agent_made_and_nobody_else_does() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served, device
    );
    let (shared, made) = shared_by_a_turn(
        &served,
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
    )
    .await;
    let bo = another_member(&actions, &workspaces, &workspace, &device).await;
    // What acts for the person and did not make the share.
    let planner =
        super::their_agent(&themselves(&root, &browser, &workspace), "planner").named("Planner");
    for (who, caller) in [
        ("another member", &bo),
        ("what acts for the person", &planner),
    ] {
        for (action, more) in [
            ("stop-sharing", serde_json::json!({})),
            ("share-open", serde_json::json!({"open": false})),
        ] {
            let refused = on_message(&actions, &workspace, caller, (&shared, action), more)
                .await
                .expect_err(who);
            assert!(!refused.is_empty(), "{who} is told why {action} is refused");
        }
    }

    // The person, on a second device of theirs.
    let phone = enrol(&root, "phone", DeviceCapabilities::member());
    let phone_page = Browser::open_at(&served, &phone, &device, &workspace).await;
    let on_phone = themselves(&root, &phone, &workspace);
    for (action, more) in [
        ("set-share-access", serde_json::json!({"access": "view"})),
        ("share-open", serde_json::json!({"open": false})),
    ] {
        on_message(&actions, &workspace, &on_phone, (&shared, action), more)
            .await
            .expect_err("what the others may do, and whether it is open, stay its sharer's");
    }
    assert_eq!(
        kept(&workspaces, &workspace, &shared).await.share,
        made.share,
        "nothing that was refused changed the share"
    );

    on_message(
        &actions,
        &workspace,
        &on_phone,
        (&shared, "stop-sharing"),
        serde_json::json!({}),
    )
    .await
    .expect("the person ends, from another device of theirs, the share their agent made");
    let after = once_ended(&workspaces, &workspace, &shared).await;
    assert!(
        after.share.as_ref().is_some_and(|share| !share.open),
        "ended, and no longer open: {:?}",
        after.share
    );
    assert_eq!(
        after.attachments, made.attachments,
        "the message stays and says what was shared"
    );
    let _ = person;
    phone_page.peer.close();
    page.peer.close();
}
