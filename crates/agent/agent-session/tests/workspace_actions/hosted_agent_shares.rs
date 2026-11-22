//! A turn of an agent on a workspace's server answers with the thing it worked on, shown live:
//! a card of that document in its reply to the ask, written for the model's
//! `answer_with_document` as the agent. Where it is shown and whose card it is are never the
//! model's to say, and a thing the turn cannot read is not shown.
use super::*;
use tilo_workspace_service::asks::{self, AskState, Reference};
use tilo_workspace_service::tl_records::runtime::Value;

/// The word processor, by its id.
const WRITER: &str = "so.tilo.word-processor";

/// A workspace on a server with one person in it and their agent kept there. The person's page
/// joins: its device is the one their agent's turns are admitted on.
macro_rules! team {
    ($root:ident, $browser:ident, $actions:ident, $workspaces:ident, $workspace:ident, $page:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let $browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let (serving, $actions) = crate::harness::hosted::serving_documents_with_actions(
            &$root,
            &server,
            state.path(),
            crate::common::catalogue,
        )
        .await;
        let $workspaces = serving.workspaces().unwrap().clone();
        let $workspace = $workspaces.create("Team").await.unwrap().workspace;
        let $page = Browser::open_at(&serving, &$browser, &server, &$workspace).await;
        let person = hex::encode($root.verifying_key().to_bytes());
        make_agent(&$page, &$workspaces, &$workspace, &person, None).await;
    };
}

/// Answer the one turn the workspace owes, with the model `$model`.
macro_rules! answer_the_turn {
    ($workspaces:ident, $actions:ident, $workspace:ident, $model:expr) => {
        let agents = ServerAgents::for_test($workspaces.clone(), $actions.clone(), Some($model));
        let turns = agents.turns(&$workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "the ask is the agent's turn: {turns:?}");
        tokio::time::timeout(
            Duration::from_secs(90),
            agents.answer(&$workspace, &turns[0]),
        )
        .await
        .expect("the turn ends")
        .unwrap();
    };
}

/// The person `root` mentions the agent in a channel of tilolang Chat, `@Scout` then `words`:
/// the mention asks it, as theirs.
pub(super) async fn asks_scout(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
    words: &str,
) {
    let packages = asks::chat_packages().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());
    let ada = asks::Actor::Member(MemberId::person(person));
    let chat = packages.app().to_owned();
    let then = format!(" {words}");
    workspaces
        .with_tl_record(workspace, &packages, ada.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let channel = opened.invoke(
                &ada,
                (&chat, "new_channel"),
                vec![Value::resource(&all), Value::Text("plans".into())],
                None,
            )?;
            opened
                .invoke(
                    &ada,
                    (&chat, "send"),
                    vec![
                        channel,
                        asks::mentioning(&MemberId::agent(SCOUT), "Scout", &then),
                        Value::Opt(None),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// The asks the record holds now, as the person `root` reads them.
pub(super) async fn asked(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
) -> Vec<asks::Asked> {
    let packages = asks::chat_packages().unwrap();
    let person = hex::encode(root.verifying_key().to_bytes());
    workspaces
        .with_tl_record(
            workspace,
            &packages,
            asks::Actor::Member(MemberId::person(person)),
            |opened| asks::asks(opened),
        )
        .await
        .unwrap()
}

/// The newest ask's newest reply, once it ended.
pub(super) async fn reply(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
) -> asks::Replied {
    let asked = asked(workspaces, workspace, root).await;
    let ask = asked.last().expect("the person asked");
    let reply = ask.latest().expect("the ask has a reply").clone();
    assert_ne!(reply.state, AskState::Working, "the turn ended: {reply:?}");
    reply
}

/// The documents a reply shows as cards: each app and record.
pub(super) fn cards(reply: &asks::Replied) -> Vec<(String, Option<String>)> {
    reply
        .shown
        .iter()
        .map(|shown| match shown {
            Reference::Resource(at) => (at.app.clone(), at.record.clone()),
            Reference::Member(member) => panic!("a reply shows things, not {member:?}"),
        })
        .collect()
}

/// A model that makes each of `calls` (a tool and what it is given), one a turn, then answers
/// `words`.
pub(super) fn calls_then_answers(
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
pub(super) fn told(asked: &[ModelRequest]) -> Vec<(String, bool)> {
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

/// What the word processor's document `document` answers `caller` who reads it, through the
/// server's action service as their page reads it: its text, or the words it is refused with.
pub(super) async fn read_by(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    caller: &tilo_actions::remote::Caller,
    document: &str,
) -> Result<String, String> {
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = tilo_server::workspace_agents::WorkspaceActionProvider::new(actions.clone(), id);
    let route = DocumentRoute {
        app_id: WRITER.into(),
        cluster_id: workspace.into(),
        document: document.into(),
    };
    tilo_server::workspace_agents::invoke_named(
        &service,
        caller,
        &route,
        "word-processor.document",
        "read-text",
        serde_json::Map::new(),
    )
    .await
    .map(|read| read.to_string())
}

/// The caller the person `root` is on their device `device`, in `workspace`.
pub(super) fn caller_of(
    root: &SigningKey,
    device: &crate::support::Device,
    workspace: &str,
) -> tilo_actions::remote::Caller {
    tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        device.cert.device_public_key,
    )
    .in_workspace(workspace)
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

/// Where a document is shown and whose card it is are never arguments, a path names a thing
/// and not an action, and no path is no thing: each call is refused and told to the model as a
/// failure, and the reply shows nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn where_a_document_is_shown_and_whose_card_it_is_are_never_the_models_to_say() {
    team!(root, browser, actions, workspaces, workspace, page);
    asks_scout(
        &workspaces,
        &workspace,
        &root,
        "Write a poem in the word processor and show me the document.",
    )
    .await;
    let person = hex::encode(root.verifying_key().to_bytes());
    let refused: Vec<(serde_json::Value, &str)> = vec![
        (
            serde_json::json!({"path": "word-processor", "channel": "plans"}),
            "never arguments",
        ),
        (
            serde_json::json!({"path": "word-processor", "conversation": "plans"}),
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
        workspaces,
        actions,
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

    let reply = reply(&workspaces, &workspace, &root).await;
    assert!(
        reply.shown.is_empty(),
        "nothing refused was shown: {reply:?}"
    );
    assert!(
        reply.body.contains("I could not show it."),
        "{}",
        reply.body
    );
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

/// A path that names nothing the turn can read is not shown: the reading the turn itself has
/// decides, and what it answered is told to the model as a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thing_the_turn_cannot_read_is_not_shown() {
    team!(root, browser, actions, workspaces, workspace, page);
    asks_scout(&workspaces, &workspace, &root, "Show me the budget.").await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        workspaces,
        actions,
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

    let reply = reply(&workspaces, &workspace, &root).await;
    assert!(
        reply.shown.is_empty(),
        "nothing unread was shown: {reply:?}"
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

/// A card gives a reader nothing the workspace denies them.
#[path = "hosted_share_denied.rs"]
mod denied;

/// The owner's case: asked to write a document and show it, the agent writes in the word
/// processor and answers with that document shown live. The card stands in the agent's reply
/// to the ask, before the model's words, and names the document the turn wrote in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_a_server_answers_with_the_document_it_wrote_shown_live() {
    team!(root, browser, actions, workspaces, workspace, page);
    asks_scout(
        &workspaces,
        &workspace,
        &root,
        "Write a poem in the word processor and show me the word processor we are working in.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(workspaces, actions, workspace, writes_then_shows(&asked));

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
        .expect("a turn that answers an ask is offered the tool");
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

    let reply = reply(&workspaces, &workspace, &root).await;
    assert_eq!(reply.state, AskState::Done, "{reply:?}");
    let cards = cards(&reply);
    assert_eq!(cards.len(), 1, "one card: {reply:?}");
    let (app, document) = &cards[0];
    assert_eq!(app, WRITER);
    assert!(
        reply.body.starts_with("A poem\n"),
        "the card is called by the first line of its title, before the words: {:?}",
        reply.body
    );
    assert!(
        reply.body.ends_with("The poem is in the document above."),
        "{:?}",
        reply.body
    );
    // The document is the one the turn wrote in, and its person reads it: what they read
    // there holds the line the agent wrote.
    let document = document.clone().unwrap_or_default();
    let reads = read_by(
        &actions,
        &workspace,
        &caller_of(&root, &browser, &workspace),
        &document,
    )
    .await
    .expect("the person reads the document shown");
    assert!(
        reads.contains("The page is shared, the cursor waits."),
        "the document {document:?} shown is the one the turn wrote: {reads}"
    );
    page.peer.close();
}

/// A poem written, then shown live a message later and changed.
#[path = "hosted_poem.rs"]
mod poem;
