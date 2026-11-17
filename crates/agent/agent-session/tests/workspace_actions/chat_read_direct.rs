//! What the server's own action service gives a member's agent of Chat: what Chat's own window
//! gives the person that agent acts for, and never more. What is said in a direct conversation
//! is read by the people it is between and by an agent acting for one of them; nobody else's
//! agent reads it, lists it or finds it, whatever role its person has in the workspace. Every
//! call goes through the service and the tools a turn has, on a turn and outside one.
use super::*;
use tilo_peer_state::join::Role;

/// The agent Bo and Cy each keep elsewhere. This workspace's list never holds it.
const KEPT_ELSEWHERE: &str = "assistant-kept-elsewhere";
/// What Cy tells the workspace's agent, in his own conversation with it.
const CYS: &str = "My bicycle's lock opens with plover.";
/// What Ada says in the workspace's open chat, which every member reads.
const OPEN: &str = "Lunch is at noon in the atrium.";
/// One word of each of the three sentences: Ada's to her agent, Cy's to it, the open chat's.
const WORDS: [&str; 3] = ["heron", "plover", "atrium"];

/// Admit the person of `device` to `workspace` with `role`: the server issues them an
/// invitation and their device redeems it.
async fn admit_as(served: &Served, workspace: &str, device: &crate::support::Device, role: Role) {
    let invitation = served
        .workspaces
        .issue(
            workspace,
            &served.devices.0.cert,
            IssueInvitationRequest {
                request_id: device.cert.device_public_key,
                role,
                recipient_user_public_key: Some(device.cert.user_public_key),
                invitee_name: Some("A colleague".into()),
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    let verified = tilo_peer_state::join::decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&device.key.to_bytes())).await;
    workspace_join::WorkspaceJoinClient::new(endpoint)
        .redeem_hint(
            verified.grant().transport.first().unwrap(),
            invitation.signed_invitation.clone(),
            device.cert.clone(),
            &device.key,
            device.cert.device_public_key,
        )
        .await
        .unwrap();
}

/// The agent `agent` acting for `person`, twice: on a turn `cause` gave it in the person's own
/// conversation with it, and outside any turn.
fn acting_for(person: &Caller, agent: &str, turn: (&Id, &Id)) -> [Caller; 2] {
    let on_turn = on_a_turn(person, agent, turn, Vec::new());
    let outside = person
        .clone()
        .delegated_to(agent.to_owned())
        .named(agent)
        .in_agent_session(format!("agent-session:test-outside-{agent}"));
    [on_turn, outside]
}

/// The workspace's open chat: the first conversation of all its members that takes messages.
async fn open_chat(served: &Served, workspace: &str) -> Id {
    let held = served
        .workspaces
        .resource_document(workspace, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let chat = chat::Chat::read(&automerge::AutoCommit::load(&held).unwrap()).unwrap();
    let ada = MemberId::person(person_of(&served.ada));
    let nothing_said = chat::messages::Messages::default();
    chat.conversation_rows(&ada, &nothing_said, &chat::projection::PeopleNames::new())
        .into_iter()
        .filter(|row| !row.direct)
        .map(|row| Id::from_raw(row.id))
        .find(|id| chat.takes_messages(id))
        .expect("the chat begins with a conversation of the whole workspace")
}

/// What a read of Chat's messages answers `caller`, asked until it holds `wanted`: the open
/// chat's sentence, which was written last and which every member's agent reads, so an answer
/// that holds it was read from everything said before it.
async fn messages_read(served: &Served, workspace: &str, caller: &Caller, wanted: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reading = serde_json::json!({"path": "chat/Messages"});
        let (read, failed) = served.tool(workspace, caller, "tilo_read", reading).await;
        if !failed && read.contains(wanted) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "a read of Chat's messages never holds {wanted:?}: failed {failed}, {read}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// A workspace of Ada's with her agent in it and three things said: Ada to her agent and Cy to
/// the same agent, each in their own conversation with it, and Ada in the open chat. Bo reads
/// and nothing else, Cy edits. The workspace, Ada's page, for a viewer, an editor and the owner
/// the agent acting for them on a turn and outside one, and where each of the three was said.
async fn three_things_said(
    served: &mut Served,
) -> (String, Browser, Vec<(&'static str, Caller)>, [Id; 3]) {
    let (workspace, page, hers) = served.workspace(|_| Vec::new()).await;
    let person = |name: &str| {
        let (root, _) = tilo_user_identity::UserIdentity::generate(name, 1);
        let device = enrol(&root, "browser", DeviceCapabilities::member());
        (root, device)
    };
    let (bo, bo_device) = person("Bo");
    let (cy, cy_device) = person("Cy");
    admit_as(served, &workspace, &bo_device, Role::Viewer).await;
    admit_as(served, &workspace, &cy_device, Role::Editor).await;
    let now = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000_000).unwrap();
    let with_agent = conversation_with(&cy, SCOUT);
    served
        .said_at(&workspace, &cy, &with_agent, (CYS, now - 120))
        .await;
    let open = open_chat(served, &workspace).await;
    served
        .said_at(&workspace, &served.ada, &open, (OPEN, now - 60))
        .await;

    let asked = Id::from_raw("m_asked_elsewhere".to_owned());
    let elsewhere = |root: &SigningKey, device: &crate::support::Device| {
        acting_for(
            &Served::person(root, device, &workspace),
            KEPT_ELSEWHERE,
            (&conversation_with(root, KEPT_ELSEWHERE), &asked),
        )
    };
    let [bo_turn, bo_outside] = elsewhere(&bo, &bo_device);
    let [cy_turn, cy_outside] = elsewhere(&cy, &cy_device);
    let [ada_turn, ada_outside] = acting_for(
        &Served::person(&served.ada, &served.devices.1, &workspace),
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &hers),
    );
    let readers = vec![
        ("a viewer's agent on a turn", bo_turn),
        ("a viewer's agent outside a turn", bo_outside),
        ("an editor's agent on a turn", cy_turn),
        ("an editor's agent outside a turn", cy_outside),
        ("the owner's agent on a turn", ada_turn),
        ("the owner's agent outside a turn", ada_outside),
    ];
    let said_in = [conversation_with(&served.ada, SCOUT), with_agent, open];
    (workspace, page, readers, said_in)
}

/// A read of Chat's messages gives an agent what is said in the open chat and in a direct
/// conversation its own person takes part in, and nothing of a direct conversation between
/// others: not to a viewer's agent, not to an editor's, and not to the owner's, whose agent is
/// itself in the other conversation for somebody else. On a turn and outside one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_of_chat_gives_an_agent_no_direct_conversation_its_person_takes_no_part_in() {
    let mut served = Served::start(1).await;
    let (workspace, page, readers, _) = three_things_said(&mut served).await;
    let mut seen: Vec<(&str, [bool; 3])> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    for (who, caller) in &readers {
        let read = messages_read(&served, &workspace, caller, OPEN).await;
        seen.push((*who, WORDS.map(|word| read.contains(word))));
        said.push(format!("{who} reads of Chat: {read}"));
    }
    // Each: Ada's sentence to her agent, Cy's to the same agent, the open chat's.
    let expected = vec![
        ("a viewer's agent on a turn", [false, false, true]),
        ("a viewer's agent outside a turn", [false, false, true]),
        ("an editor's agent on a turn", [false, true, true]),
        ("an editor's agent outside a turn", [false, true, true]),
        ("the owner's agent on a turn", [true, false, true]),
        ("the owner's agent outside a turn", [true, false, true]),
    ];
    assert_eq!(
        seen,
        expected,
        "what each was answered:\n{}",
        said.join("\n")
    );
    page.peer.close();
}

/// One rule for Chat's window, the service's search and the service's read: a conversation the
/// window does not list for a person is one whose words that person's agent neither finds nor
/// reads, and one it lists is found and read. Asked of each of the three sentences for a
/// viewer, an editor and the owner, on a turn and outside one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_chats_window_does_not_list_is_neither_found_nor_read() {
    let mut served = Served::start(1).await;
    let (workspace, page, readers, said_in) = three_things_said(&mut served).await;
    let part = conversation_part(&served.workspaces, &workspace).await;
    let held = served
        .workspaces
        .resource_document(&workspace, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let chat = chat::Chat::read(&automerge::AutoCommit::load(&held).unwrap()).unwrap();
    // Each sentence by its word: the conversation it was said in and the message that says it.
    let sentences: Vec<(&str, Id, Id)> = WORDS
        .iter()
        .zip(said_in)
        .map(|(word, conversation)| {
            let (id, _) = part
                .messages
                .iter()
                .find(|(_, message)| message.text.contains(*word))
                .expect("the workspace keeps all three sentences");
            (*word, conversation, id.clone())
        })
        .collect();
    let messages = chat::messages::Messages::single(part);
    let person_of_agent = |caller: &Caller| {
        let person = caller
            .acting_for_person()
            .expect("an agent acts for a person");
        MemberId::person(person.to_owned())
    };
    let people: chat::projection::PeopleNames = readers
        .iter()
        .map(|(_, caller)| (person_of_agent(caller), "A member".to_owned()))
        .collect();
    // The search has read all three once someone it is for finds the one written last.
    let (_, owner) = readers.last().expect("the owner's agent reads");
    served
        .finds_soon(&workspace, owner, ("atrium", sentences[2].2.as_str()))
        .await;

    // For each reader and sentence: whether the person's window lists the conversation, whether
    // their agent's search finds the sentence, whether their agent's read holds it.
    let mut seen: Vec<(&str, &str, [bool; 3])> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    for (who, caller) in &readers {
        let listed = chat.conversation_rows(&person_of_agent(caller), &messages, &people);
        let read = messages_read(&served, &workspace, caller, OPEN).await;
        said.push(format!("{who} reads of Chat: {read}"));
        for (word, conversation, message) in &sentences {
            let found = served.finds(&workspace, caller, word).await;
            let in_window = listed.iter().any(|row| row.id == conversation.as_str());
            let in_search = found.contains(word) || found.contains(message.as_str());
            seen.push((*who, *word, [in_window, in_search, read.contains(word)]));
            said.push(format!("{who} finds for {word}: {found}"));
        }
    }
    let all = [true; 3];
    let none = [false; 3];
    let expected: Vec<(&str, &str, [bool; 3])> = [
        ("a viewer's agent on a turn", [none, none, all]),
        ("a viewer's agent outside a turn", [none, none, all]),
        ("an editor's agent on a turn", [none, all, all]),
        ("an editor's agent outside a turn", [none, all, all]),
        ("the owner's agent on a turn", [all, none, all]),
        ("the owner's agent outside a turn", [all, none, all]),
    ]
    .into_iter()
    .flat_map(|(who, shown)| {
        WORDS
            .into_iter()
            .zip(shown)
            .map(move |(word, shown)| (who, word, shown))
    })
    .collect();
    assert_eq!(
        seen,
        expected,
        "window, search, read; what each was answered:\n{}",
        said.join("\n")
    );
    page.peer.close();
}
