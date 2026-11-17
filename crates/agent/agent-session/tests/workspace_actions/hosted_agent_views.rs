//! A turn of an agent on a workspace's server answers with a view it wrote: through Chat's own
//! `answer-with-view`, on its reply to the message that asked it, as the agent acting for its
//! person. Which message it answers and whose view it is are never the model's to say.
use super::*;

/// A view over what Tasks keeps for the workspace, as a model writes one.
const PROJECTS: &str =
    "view projects() {\n    for a in tasks.tracker.areas {\n        text(a.name)\n    }\n}";

/// A workspace on a server with one person in its chat and their agent kept there. The person's
/// page joins: its device is the one their agent's turns are admitted on.
macro_rules! team {
    ($root:ident, $browser:ident, $actions:ident, $workspaces:ident, $workspace:ident, $page:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let $browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let (serving, $actions) = crate::harness::hosted::serving_records_with_actions(
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
        let catalogue = crate::common::catalogue();
        let governor = $workspaces.governor().clone();
        $workspaces
            .prepare_application(&$workspace, &governor, &catalogue, CHAT, "")
            .await
            .unwrap();
        $workspaces
            .author_application(&$workspace, &governor, &catalogue, CHAT, "", |doc| {
                chat::demo::introduce(doc, &person, "Ada", "A").map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    };
}

/// Answer the one turn the workspace owes, with the model `$model`.
macro_rules! answer_the_turn {
    ($workspaces:ident, $actions:ident, $workspace:ident, $model:expr) => {
        let agents = HostAgents::new($workspaces.clone(), $actions.clone(), Some($model));
        let turns = agents.turns(&$workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "the message is the agent's turn: {turns:?}");
        tokio::time::timeout(
            Duration::from_secs(60),
            agents.answer(&$workspace, &turns[0]),
        )
        .await
        .expect("the turn ends")
        .unwrap();
    };
}

/// A model that calls `answer_with_view` once with each of `inputs`, one call a turn, then
/// answers `words`.
fn views_then_answers(
    inputs: Vec<serde_json::Value>,
    words: &'static str,
    asked: Arc<Mutex<Vec<ModelRequest>>>,
) -> Model {
    Arc::new(move || {
        let mut answers: Vec<ModelResponse> = inputs
            .iter()
            .enumerate()
            .map(|(n, input)| {
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: format!("call-{n}"),
                        name: "answer_with_view".into(),
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

/// The id of the agent's reply to the message `to`.
fn reply_to(part: &chat::parts::ChatPart, to: &Id) -> Id {
    let agent = MemberId::agent(SCOUT);
    part.messages
        .iter()
        .find(|(_, held)| held.author == agent && held.parent.as_ref() == Some(to))
        .map(|(id, _)| id.clone())
        .expect("the agent's reply is in the conversation")
}

/// What `request` tells the model its tools answered, each with whether it failed.
fn tool_answers(request: &ModelRequest) -> Vec<(String, bool)> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_a_server_answers_with_a_view_it_wrote() {
    team!(root, browser, actions, workspaces, workspace, page);
    let question = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "which projects are there?",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        workspaces,
        actions,
        workspace,
        views_then_answers(
            vec![serde_json::json!({"source": PROJECTS, "reads": ["tasks"]})],
            "Your projects, as Tasks keeps them.",
            Arc::clone(&asked),
        )
    );

    // The view is state of the agent's own reply to the message that asked it: the reply every
    // turn writes, so the turn's work, its words and its view are one message.
    let part = conversation_part(&workspaces, &workspace).await;
    assert_eq!(part.messages.len(), 2, "the question and one reply");
    let reply = part
        .messages
        .get(&reply_to(&part, &question))
        .expect("the reply");
    let view = reply
        .view
        .as_ref()
        .expect("the reply carries the view the model wrote");
    assert_eq!(view.source, PROJECTS);
    assert_eq!(view.reads, ["tasks"]);
    assert_eq!(view.version, 1);
    // Whose code it is: the agent's, written for its person, as Chat pinned both from the
    // turn's caller. A reader's window shows it as an assistant's view, which only suggests.
    assert_eq!(reply.author, MemberId::agent(SCOUT));
    assert!(
        reply
            .acting
            .as_ref()
            .is_some_and(|acting| !acting.is_person()),
        "the reply says what wrote it for whom: {:?}",
        reply.acting
    );
    // Its words are the model's last answer: what every reader has.
    assert_eq!(reply.text, "Your projects, as Tasks keeps them.");
    assert!(
        !chat::summons::unanswered(Some(reply)),
        "its work ended: {:?}",
        reply.work
    );

    // The model was offered the tool with how a view is written and when one is the answer,
    // and was told its view is shown before it said what it shows.
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 2, "one turn to write the view, one to answer");
    let tool = asked[0]
        .tools
        .iter()
        .find(|tool| tool.name == "answer_with_view")
        .expect("a turn in a conversation is offered the tool");
    assert!(
        tool.description.contains("tasks.tracker") && tool.description.contains("keeps changing"),
        "{}",
        tool.description
    );
    assert!(
        said_in(&asked[0]).contains("answer_with_view"),
        "its instructions say when a view is the answer"
    );
    let told = tool_answers(&asked[1]);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        !told[0].1 && told[0].0.contains("shown live"),
        "the tool said the view is on the reply: {told:?}"
    );
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_view_a_model_may_not_post_is_refused_and_told_to_it() {
    team!(root, browser, actions, workspaces, workspace, page);
    let question = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "which projects are there?",
    )
    .await;
    let person = hex::encode(root.verifying_key().to_bytes());
    // Each is one call of the tool, and each is refused before or by Chat's own verb: a view
    // that says whose it is, one that names the message it answers, one that lists an app it
    // does not name, one that shows nothing, and one with no definition at all.
    let refused: Vec<(serde_json::Value, &str)> = vec![
        (
            serde_json::json!({"source": PROJECTS, "reads": ["tasks"], "author": format!("person:{person}")}),
            "never arguments",
        ),
        (
            serde_json::json!({"source": PROJECTS, "reads": ["tasks"], "message": question.to_string()}),
            "never arguments",
        ),
        (
            serde_json::json!({"source": PROJECTS, "reads": ["mail"]}),
            "does not name mail",
        ),
        (serde_json::json!({"source": " ", "reads": []}), "empty"),
        (serde_json::json!({"reads": ["tasks"]}), "takes `source`"),
    ];
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(
        workspaces,
        actions,
        workspace,
        views_then_answers(
            refused.iter().map(|(input, _)| input.clone()).collect(),
            "I could not show that as a view.",
            Arc::clone(&asked),
        )
    );

    // Nothing of it reached the conversation: the reply has the model's words and no view, and
    // it is the agent's own.
    let part = conversation_part(&workspaces, &workspace).await;
    assert_eq!(part.messages.len(), 2, "the question and one reply");
    let reply = part
        .messages
        .get(&reply_to(&part, &question))
        .expect("the reply");
    assert!(
        reply.view.is_none(),
        "no refused view was posted: {:?}",
        reply.view
    );
    assert_eq!(reply.author, MemberId::agent(SCOUT));
    assert_eq!(reply.text, "I could not show that as a view.");

    // The model was told why, each time, as a failure of that call.
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), refused.len() + 1);
    let told = tool_answers(asked.last().expect("the last request"));
    assert_eq!(told.len(), refused.len(), "{told:?}");
    for ((answer, failed), (_, why)) in told.iter().zip(&refused) {
        assert!(*failed, "refused as a failure: {answer}");
        assert!(answer.contains(why), "refused for {why:?}: {answer}");
    }
    page.peer.close();
}
