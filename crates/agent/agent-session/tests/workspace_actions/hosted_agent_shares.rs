//! A turn of an agent on a workspace's server answers with the thing it worked on, shown live:
//! Chat's own `share-window` of that document, posted for the model's `answer_with_document` in
//! the turn's own conversation, as the agent acting for its person. Where it is shown and whose
//! share it is are never the model's to say, and a thing the turn cannot read is not shown.
use super::*;

/// The word processor, by its id.
const WRITER: &str = "so.tilo.word-processor";
/// What Chat's share verbs call the conversation they post in, and an address its document.
const IN: &str = "channel";
const DOCUMENT: &str = "record";

/// A workspace on a server with one person in its chat and their agent kept there. The person's
/// page joins: its device is the one their agent's turns are admitted on.
macro_rules! team {
    ($root:ident, $browser:ident, $actions:ident, $workspaces:ident, $workspace:ident, $page:ident, $served:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let $browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let $served = crate::harness::hosted::start(
            &$root,
            &server,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        let $actions = $served.roster.actions.clone();
        let $workspaces = $served.workspaces().unwrap().clone();
        let $workspace = $workspaces.create("Team").await.unwrap().workspace;
        let $page = Browser::open_at(&$served, &$browser, &server, &$workspace).await;
        let person = hex::encode($root.verifying_key().to_bytes());
        make_agent(&$page, &$workspaces, &$workspace, &person, None).await;
        let catalogue = crate::common::catalogue();
        let governor = $workspaces.governor().clone();
        $workspaces
            .prepare_application(&$workspace, &governor, &catalogue, CHAT, "")
            .await
            .unwrap();
    };
}

/// Answer the one turn the workspace owes, with the model `$model`.
macro_rules! answer_the_turn {
    ($served:ident, $workspace:ident, $model:expr) => {
        let agents = $served
            .approving(&$workspace, &[SCOUT])
            .await
            .scripted($model);
        let turns = agents.turns(&$workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "the message is the agent's turn: {turns:?}");
        tokio::time::timeout(
            Duration::from_secs(90),
            agents.answer(&$workspace, &turns[0]),
        )
        .await
        .expect("the turn ends")
        .unwrap();
    };
}

/// A model that makes each of `calls` (a tool and what it is given), one a turn, then answers
/// `words`.
fn calls_then_answers(
    calls: Vec<(&'static str, serde_json::Value)>,
    words: &'static str,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
) -> Model {
    Arc::new(move || {
        let mut answers: Vec<ModelResponse> = calls
            .iter()
            .enumerate()
            .map(|(n, (tool, input))| {
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: format!("call-{n}"),
                        name: (*tool).into(),
                        input: input.clone(),
                        extra_content: None,
                    })],
                    StopReason::ToolCalls,
                    1_000,
                )
            })
            .collect();
        answers.push(answer(
            vec![Content::Text { text: words.into() }],
            StopReason::EndTurn,
            1_000,
        ));
        Some(Box::new(Scripted {
            answers,
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// What the last request tells the model its tools answered, in order, each with whether it
/// failed.
fn told(asked: &[ModelRequest]) -> Vec<(String, bool)> {
    asked
        .last()
        .expect("the model was asked")
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}

/// A message that shares a window, as its conversation keeps it.
#[derive(Debug)]
struct Shared {
    author: MemberId,
    conversation: String,
    share: chat::chat::WindowShare,
    /// Whether it says an agent wrote it for someone.
    for_someone: bool,
    /// What it shares: the app, its document and the resource's kind, id and label.
    at: serde_json::Value,
}

/// The messages of the conversation that share a window.
fn shares(part: &chat::parts::ChatMessages) -> Vec<Shared> {
    part.messages
        .iter()
        .filter_map(|(_, held)| {
            let share = held.share.clone()?;
            let at = held
                .attachments
                .iter()
                .find_map(tilo_reference::Attached::at)
                .expect("a share names what it shares");
            Some(Shared {
                author: held.author.clone(),
                conversation: held.channel.as_str().to_owned(),
                share,
                for_someone: held
                    .acting
                    .as_ref()
                    .is_some_and(|acting| !acting.is_person()),
                at: serde_json::to_value(at).expect("an address is JSON"),
            })
        })
        .collect()
}

/// What the word processor's document `document` reads as to the person, through the server's
/// action service as their page reads it.
async fn reads_to_the_person(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    (root, browser): (&SigningKey, &crate::support::Device),
    document: &str,
) -> String {
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = tilo_workspace_service::agents::WorkspaceActionProvider::new(actions.clone(), id);
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace);
    let route = DocumentRoute {
        app_id: WRITER.into(),
        cluster_id: workspace.into(),
        document: document.into(),
    };
    tilo_workspace_service::agents::invoke_named(
        &service,
        &caller,
        &route,
        "word-processor.document",
        "read-text",
        serde_json::Map::new(),
    )
    .await
    .expect("the person reads the shared document")
    .to_string()
}

/// The owner's turn as a model takes it: it opens the word processor, writes a line, and
/// answers with that document.
fn writes_then_shows(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    calls_then_answers(
        vec![
            (
                "tilo_do",
                serde_json::json!({"path": "word-processor#open", "args": {}}),
            ),
            (
                "tilo_do",
                serde_json::json!({"path": "word-processor#append-paragraph",
                    "args": {"text": "The page is shared, the cursor waits."}}),
            ),
            (
                "answer_with_document",
                serde_json::json!({"path": "word-processor", "title": "A poem\nfor two"}),
            ),
        ],
        "The poem is in the document above.",
        Arc::clone(asked),
    )
}

/// Where a document is shown and whose share it is are never arguments, a path names a thing
/// and not an action, and no path is no thing: each call is refused and told to the model as a
/// failure, and nothing is shared anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn where_a_document_is_shown_and_whose_share_it_is_are_never_the_models_to_say() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Write a poem in the word processor and show me the document.",
    )
    .await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let refused: Vec<(serde_json::Value, &str)> = vec![
        (
            serde_json::json!({"path": "word-processor", IN: "c_chat"}),
            "never arguments",
        ),
        (
            serde_json::json!({"path": "word-processor", "conversation": "c_chat"}),
            "never arguments",
        ),
        (
            serde_json::json!({"path": "word-processor", "author": format!("person:{person}")}),
            "never arguments",
        ),
        (
            serde_json::json!({"path": "word-processor", "access": "edit"}),
            "never arguments",
        ),
        (serde_json::json!({"title": "A poem"}), "takes `path`"),
        (serde_json::json!({"path": "  "}), "takes `path`"),
        (
            serde_json::json!({"path": "word-processor#append-paragraph"}),
            "not of an action",
        ),
    ];
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        served,
        workspace,
        calls_then_answers(
            refused
                .iter()
                .map(|(input, _)| ("answer_with_document", input.clone()))
                .collect(),
            "I could not show it.",
            Arc::clone(&asked),
        )
    );

    let part = conversation_part(&workspaces, &workspace).await;
    assert!(
        shares(&part).is_empty(),
        "nothing refused was shared: {:?}",
        shares(&part)
    );
    assert_eq!(part.messages.len(), 2, "the question and one reply");
    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let told = told(&asked);
    assert_eq!(told.len(), refused.len(), "{told:?}");
    for ((answer, failed), (input, why)) in told.iter().zip(&refused) {
        assert!(*failed, "{input} is refused as a failure: {answer}");
        assert!(
            answer.contains(why),
            "{input} is refused for {why:?}: {answer}"
        );
    }
    page.peer.close();
}

/// What a document says changes nothing about a turn's shares.
#[path = "hosted_planted_words.rs"]
mod planted_words;

/// A path that names nothing the turn can read is not shown: the reading the turn itself has
/// decides, and what it answered is told to the model as a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thing_the_turn_cannot_read_is_not_shown() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Show me the budget.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        served,
        workspace,
        calls_then_answers(
            vec![
                (
                    "answer_with_document",
                    serde_json::json!({"path": "no-such-app-here"}),
                ),
                (
                    "answer_with_document",
                    serde_json::json!({"path": "word-processor/No such document"}),
                ),
            ],
            "There is no budget here.",
            Arc::clone(&asked),
        )
    );

    let part = conversation_part(&workspaces, &workspace).await;
    assert!(
        shares(&part).is_empty(),
        "nothing unread was shared: {:?}",
        shares(&part)
    );
    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let told = told(&asked);
    assert_eq!(told.len(), 2, "{told:?}");
    for (answer, failed) in &told {
        assert!(
            *failed && answer.starts_with("The document was not shown"),
            "refused with why, as a failure: {answer}"
        );
    }
    page.peer.close();
}

/// A share gives a reader nothing the workspace denies them.
#[path = "hosted_share_denied.rs"]
mod denied;

/// The owner's case: asked to write a document and show it, the agent writes in the word
/// processor and answers with that document shown live. The share is one message of the agent's
/// in the conversation the person asked in, of the document the turn wrote in, and the reply
/// carries the model's words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_a_server_answers_with_the_document_it_wrote_shown_live() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    let question = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Write a poem in the word processor and show me the word processor we are working in.",
    )
    .await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let conversation = chat::Chat::direct_id([MemberId::person(person), MemberId::agent(SCOUT)]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(served, workspace, writes_then_shows(&asked));

    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let told = told(&asked);
    assert_eq!(told.len(), 3, "{told:?}");
    assert!(
        told.iter().all(|(_, failed)| !failed),
        "every step was taken: {told:?}"
    );
    assert!(
        told[2].0.contains("shown live"),
        "the tool said the document is shown: {told:?}"
    );
    // The tool is offered with when it is the answer, beside the view's, and the turn's
    // instructions name it.
    let tool = asked[0]
        .tools
        .iter()
        .find(|tool| tool.name == "answer_with_document")
        .expect("a turn in a conversation is offered the tool");
    assert!(
        tool.description.contains("answer_with_view")
            && tool.description.contains("never an argument"),
        "{}",
        tool.description
    );
    assert!(
        said_in(&asked[0]).contains("answer_with_document"),
        "its instructions say when the thing itself is the answer"
    );

    let part = conversation_part(&workspaces, &workspace).await;
    let shared = shares(&part);
    assert_eq!(shared.len(), 1, "one share: {shared:?}");
    let shown = &shared[0];
    assert_eq!(
        shown.author,
        MemberId::agent(SCOUT),
        "the agent's own share"
    );
    assert!(shown.for_someone, "it says what shared it for whom");
    assert_eq!(
        shown.conversation,
        conversation.as_str(),
        "in the conversation the person asked in"
    );
    assert!(shown.share.open && !shown.share.ended, "live: {shown:?}");
    assert_eq!(
        format!("{:?}", shown.share.access),
        "Edit",
        "its card offers to work in it"
    );
    assert_eq!(shown.at["app"], WRITER);
    assert_eq!(shown.at["label"], "A poem", "the first line of its title");
    // The document is the one the turn wrote in, and its person reads it: what they read
    // there holds the line the agent wrote.
    let document = shown.at[DOCUMENT].as_str().unwrap_or_default();
    let reads = reads_to_the_person(&actions, &workspace, (&root, &browser), document).await;
    assert!(
        reads.contains("The page is shared, the cursor waits."),
        "the shared document {document:?} is the one the turn wrote: {reads}"
    );
    let reply = part
        .messages
        .iter()
        .find(|(_, held)| {
            held.author == MemberId::agent(SCOUT) && held.parent.as_ref() == Some(&question)
        })
        .map(|(_, held)| held.clone())
        .expect("the reply");
    assert_eq!(reply.text, "The poem is in the document above.");
    assert!(
        reply.share.is_none(),
        "the reply is the words; the share is its own message"
    );
    page.peer.close();
}

/// A poem written, shown live a message later, changed, and its share ended.
#[path = "hosted_poem.rs"]
mod poem;

/// Through its other tools the conversation of a share is an argument the model gives. A turn's
/// call of Chat's own sharing verbs for any conversation but the one it answers in is refused
/// in words and shares nothing: not where everyone reads, and not in its person's
/// conversation with another agent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_shares_only_into_the_conversation_it_answers_in() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Post the document for everyone.",
    )
    .await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let elsewhere = chat::Chat::direct_id([MemberId::person(person), MemberId::agent("other")]);
    let attached = serde_json::json!({"app": WRITER, DOCUMENT: "letter", "kind": "tilo.window",
        "id": "letter", "label": "A letter"});
    let sharing = |verb: &str, to: &str| {
        serde_json::json!({"path": format!("chat/Messages#{verb}"),
            "args": {IN: to, "attached": attached.clone()}})
    };
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        served,
        workspace,
        calls_then_answers(
            vec![
                ("tilo_do", sharing("share-window", "c_general")),
                ("tilo_do", sharing("share-resource", "c_general")),
                ("tilo_do", sharing("share-window", elsewhere.as_str())),
            ],
            "I did not post it.",
            Arc::clone(&asked),
        )
    );
    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let told = told(&asked);
    assert_eq!(told.len(), 3, "{told:?}");
    for (answer, failed) in &told {
        assert!(
            *failed && answer.contains("only into the conversation it is answering in"),
            "refused in words, as a failure: {answer}"
        );
    }
    let part = conversation_part(&workspaces, &workspace).await;
    assert!(
        part.messages
            .iter()
            .all(|(_, held)| held.attachments.is_empty()),
        "nothing was shared anywhere"
    );
    page.peer.close();
}

/// A turn's own call of Chat's `share-window` for the conversation it answers in is taken, as
/// before: the rule above holds only the conversation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_shares_a_window_into_its_conversation_through_the_action_tool() {
    team!(root, browser, actions, workspaces, workspace, page, served);
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "Show me the document.",
    )
    .await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let conversation = chat::Chat::direct_id([MemberId::person(person), MemberId::agent(SCOUT)]);
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        served,
        workspace,
        calls_then_answers(
            vec![(
                "tilo_do",
                serde_json::json!({"path": "chat/Messages#share-window", "args": {
                    IN: conversation.as_str(),
                    "attached": {"app": WRITER, DOCUMENT: "letter", "kind": "tilo.window",
                        "id": "letter", "label": "A letter"},
                    "access": "edit",
                }}),
            )],
            "It is above.",
            Arc::clone(&asked),
        )
    );
    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let told = told(&asked);
    assert_eq!(told.len(), 1, "{told:?}");
    let part = conversation_part(&workspaces, &workspace).await;
    let shared = shares(&part);
    assert!(
        !told[0].1 && shared.len() == 1,
        "what the action tool answered a share into the turn's own conversation: {told:?}; \
         shares: {shared:?}"
    );
    assert_eq!(shared[0].author, MemberId::agent(SCOUT));
    page.peer.close();
}
