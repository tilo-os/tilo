//! A reply shows its model thinking.
//!
//! Where a person's provider shows what its model thinks before it writes, the server puts
//! that into the reply's thinking as it arrives, so a wait is not silent, and never among the
//! answer's words. The test's provider thinks in pieces and then waits until the test lets it
//! go on: the test reads the reply while its model has thought and not yet answered.
use super::writing::{Next::*, writing};
use super::*;
use chat::agent_stream::AgentFinish;

/// What the provider's model thinks, in the pieces its provider shows it in.
const THOUGHTS: [&str; 3] = ["The plan names ", "one month for the launch: ", "May."];

/// What it answers.
const ANSWER: &str = "The launch is planned for May.";

/// What `reply` shows of its model's thinking.
fn thinking_of(reply: &chat::chat::Message) -> String {
    reply
        .work
        .as_ref()
        .and_then(|work| work.stream.as_ref())
        .map_or_else(String::new, |stream| stream.reasoning.clone())
}

/// How the writing of `reply` ended, if it has.
fn finish_of(reply: &chat::chat::Message) -> Option<AgentFinish> {
    reply
        .work
        .as_ref()
        .and_then(|work| work.stream.as_ref())
        .and_then(|stream| stream.finish)
}

/// The agent's reply to `message` once it shows its model thinking `thoughts`.
async fn thought(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    message: &Id,
    thoughts: &str,
) -> chat::chat::Message {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(workspaces, workspace).await;
        let reply = part
            .messages
            .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)));
        if let Some(reply) = reply
            && thinking_of(reply) == thoughts
        {
            return reply.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the agent's reply to {message} never showed its model thinking {thoughts:?}: it shows {:?}",
            reply.map(thinking_of)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Her assistant's model thinks before it answers, and her provider shows it. While the model
/// has not answered, the reply shows what it thought and holds no word. The answer then
/// arrives whole: the reply reads it, with the thinking beside it and never in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_shows_its_model_thinking_and_keeps_an_answer_that_came_whole() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Thinks("The plan names "),
            Thinks("one month for the launch: "),
            Thinks("May."),
            Waits,
            Says(ANSWER),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "When is the launch?",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let (answering, at, turn) = (agents.clone(), workspace.clone(), turns[0].clone());
    let going = tokio::spawn(async move { answering.answer(&at, &turn).await });

    let thinks = thought(&workspaces, &workspace, &first, &THOUGHTS.concat()).await;
    assert_eq!(
        thinks.text, "",
        "what a model thinks is never among its reply's words"
    );
    assert_eq!(
        finish_of(&thinks),
        None,
        "the reply says its model is still at work"
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text, ANSWER,
        "the reply reads the answer that came whole"
    );
    assert_eq!(
        thinking_of(&reply),
        THOUGHTS.concat(),
        "what the model thought stays beside its answer"
    );
    assert_eq!(finish_of(&reply), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// The same model writes its answer piece by piece after it thought: the reply reads the
/// answer once, and what the model thought stands beside it and nowhere in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_model_thought_stands_beside_the_words_it_then_writes() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Thinks("The plan names "),
            Thinks("one month for the launch: "),
            Thinks("May."),
            Waits,
            Writes("The launch "),
            Writes("is planned "),
            Writes("for May."),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "When is the launch?",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let (answering, at, turn) = (agents.clone(), workspace.clone(), turns[0].clone());
    let going = tokio::spawn(async move { answering.answer(&at, &turn).await });

    let thinks = thought(&workspaces, &workspace, &first, &THOUGHTS.concat()).await;
    assert_eq!(
        thinks.text, "",
        "what a model thinks is never among its reply's words"
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(reply.text, ANSWER, "the reply reads the answer once");
    assert_eq!(
        thinking_of(&reply),
        THOUGHTS.concat(),
        "what the model thought stays beside its answer"
    );
    assert_eq!(finish_of(&reply), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}
