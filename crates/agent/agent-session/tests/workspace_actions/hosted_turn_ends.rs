//! A look reads what a workspace owes its agents and acts on what it read a moment later, when
//! the turn it read about may have ended. What a look read before an answer begins nothing a
//! second time: a message is asked of its model once, the message its person writes next gets
//! a turn of its own, and a turn is told its conversation as it stands when it begins, with
//! the answer before it. The first tests are that look themselves: they read the turns the
//! workspace owes and enter a turn's place with what they read only later. Where the look's
//! own entering is what matters, a test takes the look's two steps one at a time.
use super::*;

/// Her message is answered, and a look that read the workspace before that answer enters its
/// place only afterwards. The message is not asked of the model again, and its reply stands as
/// it was written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_look_that_read_before_an_answer_does_not_ask_the_message_again() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &served,
        &workspace,
        looks_then_answers(1_000, Arc::clone(&asked)),
    )
    .await;
    // What a look reads: the turn her message is owed.
    let read = agents.turns(&workspace).await.unwrap();
    assert_eq!(read.len(), 1, "{read:?}");
    // That turn is answered and ends: its place is free again.
    agents.answer(&workspace, &read[0]).await.unwrap();
    let answer = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        answer.text,
        "I looked through the workspace: the plan is ready for you."
    );
    assert_eq!(asked.lock().unwrap().len(), 2, "it looked, then answered");

    // The look that read before the answer enters the place only now.
    agents.answer(&workspace, &read[0]).await.unwrap();
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "her message is asked of the model once"
    );
    let after = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        (after.text.as_str(), end_of(&after)),
        (answer.text.as_str(), end_of(&answer)),
        "and its reply stands as it was written"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "nothing is owed"
    );
    let _ = person;
    page.peer.close();
}

/// A person's app shows their agent at work by the agent's reply, where its work is kept, and
/// stops it by naming that reply: their stop ends the turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_stop_that_names_the_agents_reply_ends_the_turn() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );

    // A turn their own message began.
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    let agents = worked(
        &served,
        &workspace,
        waits(
            vec![looks(), says("The launch is planned.")],
            1,
            &asked,
            &hold,
        ),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the turn reaches its second step", || {
        asked.lock().unwrap().len() == 2
    })
    .await;
    stop_agent(&workspaces, &workspace, &person, &first)
        .await
        .expect("the person stops their agent's work by the reply that shows it");
    agents.look(&workspace).await.unwrap();
    hold.open();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.state
    );
    assert_eq!(asked.lock().unwrap().len(), 2, "no step follows a stop");

    page.peer.close();
}

/// A model that looks through the workspace until `steps` of its looks were answered, then says
/// `words`. Every call is kept in `asked`.
fn looks_then_says(
    steps: usize,
    words: &'static str,
    asked: &Arc<Mutex<Vec<ModelRequest>>>,
) -> Model {
    plays(asked, move |request| {
        let answered = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, Role::Tool))
            .count();
        if answered < steps {
            looks()
        } else {
            says(words)
        }
    })
}

/// Whether the agent's reply to `message` has ended.
async fn has_ended(workspaces: &Workspaces, workspace: &str, message: &Id) -> bool {
    let asked_now = asked_on(workspaces, workspace).await;
    ask_on(&asked_now, message, SCOUT)
        .is_some_and(|ask| !matches!(ask.state, asks::AskState::Waiting | asks::AskState::Working))
}

/// Her agent needs thirty looks through the workspace before it can answer. Nothing of the
/// server's counts them: the turn asks its model for as long as the model calls tools, and ends
/// with the model's own answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_of_thirty_steps_ends_with_its_models_answer() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "tidy the workspace",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &served,
        &workspace,
        looks_then_says(30, "All thirty are tidied.", &asked),
    )
    .await;
    let read = agents.turns(&workspace).await.unwrap();
    assert_eq!(read.len(), 1, "{read:?}");
    agents.answer(&workspace, &read[0]).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text, "All thirty are tidied.",
        "the turn ends with its model's answer: {:?}",
        reply.state
    );
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Complete),
        "and its reply says it is whole: {:?}",
        reply.state
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        31,
        "its model was asked for each of the thirty steps and for the answer"
    );
    let _ = person;
    page.peer.close();
}

/// Her agent's model never answers: every call of it asks for one more look. Nothing of the
/// server's ends such a turn, so it is still at work past its thirtieth step, and it is hers to
/// stop: her stop ends it at its next step, and no step follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_that_never_answers_goes_on_until_its_person_stops_it() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "tidy the workspace",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&served, &workspace, plays(&asked, |_| looks())).await;
    agents.look(&workspace).await.unwrap();
    // A server that counted its asks would have ended the turn by itself before this.
    let deadline = tilo_test_env::hang_deadline();
    while asked.lock().unwrap().len() <= 30 {
        assert!(
            !has_ended(&workspaces, &workspace, &first).await,
            "the turn ended by itself after {} asks of its model, and nobody stopped it",
            asked.lock().unwrap().len()
        );
        assert!(
            Instant::now() < deadline,
            "the turn never came past its thirtieth step"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    stop_agent(&workspaces, &workspace, &person, &first)
        .await
        .expect("she stops her agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.state
    );
    let steps = asked.lock().unwrap().len();
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "a stopped message asks nobody"
    );
    assert_eq!(
        asked.lock().unwrap().len(),
        steps,
        "no step follows her stop"
    );
    let _ = person;
    page.peer.close();
}
