//! A look reads what a workspace owes its agents and acts on what it read a moment later, when
//! the turn it read about may have ended. What a look read before an answer begins nothing a
//! second time: a message is asked of its model once, the message its person writes next gets
//! a turn of its own, and a turn is told its conversation as it stands when it begins, with
//! the answer before it. A look cannot be stopped between its reading and its entering, so
//! these tests are that look themselves: they read the turns the workspace owes and enter a
//! turn's place with what they read only later.
use super::*;

/// Whether the turn of `request` began with what its person wrote as `words`.
fn begins_with(request: &ModelRequest, words: &str) -> bool {
    request
        .messages
        .iter()
        .find(|message| matches!(message.role, Role::User))
        .is_some_and(|message| {
            message
                .content
                .iter()
                .any(|content| matches!(content, Content::Text { text } if text.starts_with(words)))
        })
}

/// Her message is answered, and a look that read the workspace before that answer enters its
/// place only afterwards. The message is not asked of the model again, and its reply stands as
/// it was written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_look_that_read_before_an_answer_does_not_ask_the_message_again() {
    team!(root, browser, actions, workspaces, workspace, page, person);
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
        &workspaces,
        &actions,
        looks_then_answers(1_000, Arc::clone(&asked)),
    );
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

/// She writes her next message while a look that read before her first answer enters its
/// place. The message gets a turn of its own: one model call that begins with her words and is
/// told the answer before them. Nothing of it goes into a second turn on her first message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_message_after_an_answer_gets_a_turn_of_its_own() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let written = (&root, &browser);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "plan the launch",
    )
    .await;
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    // The model waits before it answers its second call, whichever turn asks it.
    let agents = worked(
        &workspaces,
        &actions,
        waits(
            vec![says("The launch is planned."), says("The cake is ordered.")],
            1,
            &asked,
            &hold,
        ),
    );
    let read = agents.turns(&workspace).await.unwrap();
    assert_eq!(read.len(), 1, "{read:?}");
    agents.answer(&workspace, &read[0]).await.unwrap();
    let answer = ended(&workspaces, &workspace, &first).await;
    assert_eq!(answer.text, "The launch is planned.");

    // The look that read before that answer enters the place now. Where it begins her first
    // message again its model is asked and waits; where it begins nothing it is done.
    let (again, at, stale) = (agents.clone(), workspace.clone(), read[0].clone());
    let entering = tokio::spawn(async move { again.answer(&at, &stale).await });
    until("the look that read before the answer entered", || {
        entering.is_finished() || asked.lock().unwrap().len() == 2
    })
    .await;

    // She writes her next message, and the look its write starts finds it.
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "also order the cake",
    )
    .await;
    agents.look(&workspace).await.unwrap();
    hold.open();
    let ordered = ended(&workspaces, &workspace, &second).await;
    entering.await.unwrap().unwrap();

    let requests = asked.lock().unwrap().clone();
    let words: Vec<String> = requests.iter().map(said_in).collect();
    assert_eq!(
        requests.len(),
        2,
        "one model call for each of her messages: {words:?}"
    );
    assert!(
        begins_with(&requests[1], "also order the cake"),
        "the call that answers her second message begins with it: {words:?}"
    );
    assert!(
        !words[1].contains("your person wrote again"),
        "it is a turn of its own, not her first one going on: {}",
        words[1]
    );
    assert!(
        words[1].contains("The launch is planned."),
        "and it is told the answer before it: {}",
        words[1]
    );
    assert_eq!(ordered.text, "The cake is ordered.");
    let planned = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        planned.text, "The launch is planned.",
        "her first answer stands"
    );
    let _ = person;
    page.peer.close();
}

/// A reminder's turn is worked on when she writes, and a look reads her message then. That
/// look enters the place only once the reminder's answer stands. The turn that begins is told
/// the conversation as it stands now, that answer included, not as the look read it. What a
/// cause asks never stands before her own words, so which of the two a look reads as the turn
/// does not hang on the second each was written in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_is_told_its_conversation_as_it_stands_when_it_begins() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let planner = themselves(&root, &browser, &workspace)
        .delegated_to("planner")
        .named("Planner");
    ask_agent(&actions, &workspace, &planner, &thread, "due-1")
        .await
        .expect("the reminder asks");
    let reminder = chat::cause::message_id(&MemberId::person(person.clone()), "due-1");
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    // The model waits before it answers its first call.
    let agents = worked(
        &workspaces,
        &actions,
        waits(
            vec![
                says("It is time to call mum."),
                says("The cake is ordered."),
            ],
            0,
            &asked,
            &hold,
        ),
    );
    let read = agents.turns(&workspace).await.unwrap();
    assert_eq!(read.len(), 1, "{read:?}");
    assert_eq!(read[0].cause, reminder.to_string(), "the reminder's turn");
    let (working, at, turn) = (agents.clone(), workspace.clone(), read[0].clone());
    let answering = tokio::spawn(async move { working.answer(&at, &turn).await });
    until("the reminder's turn asks its model", || {
        asked.lock().unwrap().len() == 1
    })
    .await;

    // She writes. What a look reads now is her message, with no answer above it yet.
    let hers = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "also order the cake",
    )
    .await;
    let read = agents.turns(&workspace).await.unwrap();
    assert_eq!(read.len(), 1, "{read:?}");
    assert_eq!(read[0].cause, hers.to_string(), "her own words come first");
    assert!(
        !read[0].request.contains("It is time to call mum."),
        "read before the reminder's answer was written: {}",
        read[0].request
    );

    // The reminder's turn ends before that look enters the place.
    hold.open();
    answering.await.unwrap().unwrap();
    let reminded = ended(&workspaces, &workspace, &reminder).await;
    assert_eq!(reminded.text, "It is time to call mum.");
    agents.answer(&workspace, &read[0]).await.unwrap();
    let ordered = ended(&workspaces, &workspace, &hers).await;
    assert_eq!(ordered.text, "The cake is ordered.");

    let requests = asked.lock().unwrap().clone();
    let words: Vec<String> = requests.iter().map(said_in).collect();
    assert_eq!(requests.len(), 2, "one model call for each: {words:?}");
    assert!(
        begins_with(&requests[1], "also order the cake"),
        "her message is a turn of its own: {words:?}"
    );
    assert!(
        words[1].contains("It is time to call mum."),
        "told the answer that was written before it began: {}",
        words[1]
    );
    page.peer.close();
}
