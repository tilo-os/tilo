//! A look reads what a workspace owes its agents and acts on what it read a moment later, when
//! the turn it read about may have ended. What a look read before an answer begins nothing a
//! second time: a message is asked of its model once, the message its person writes next gets
//! a turn of its own, and a turn is told its conversation as it stands when it begins, with
//! the answer before it. The first tests are that look themselves: they read the turns the
//! workspace owes and enter a turn's place with what they read only later. Where the look's
//! own entering is what matters, a test takes the look's two steps one at a time.
use super::*;

/// Whether the turn of `request` began with what its person wrote as `words`.
fn begins_with(request: &ModelRequest, words: &str) -> bool {
    asked_in(request).starts_with(words)
}

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
/// stops it by naming that reply. Their stop ends the turn whatever began it: their own
/// message, or a cause that asked for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_stop_that_names_the_agents_reply_ends_the_turn_whatever_began_it() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let me = themselves(&root, &browser, &workspace);
    let scout = MemberId::agent(SCOUT);

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
    stop_agent(
        &actions,
        &workspace,
        &me,
        &chat::chat::reply_id(&first, &scout),
    )
    .await
    .expect("the person stops their agent's work by the reply that shows it");
    agents.look(&workspace).await.unwrap();
    hold.open();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.work
    );
    assert_eq!(asked.lock().unwrap().len(), 2, "no step follows a stop");

    // A turn a cause began for them: a reminder, asked by what acts for them.
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), scout.clone()]);
    let planner =
        super::their_agent(&themselves(&root, &browser, &workspace), "planner").named("Planner");
    ask_agent(&actions, &workspace, &planner, &thread, "call-1")
        .await
        .expect("what acts for the person asks their agent");
    let second = chat::cause::message_id(&MemberId::person(person.clone()), "call-1");
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens_again = Opens(Arc::clone(&hold));
    let agents = worked(
        &served,
        &workspace,
        waits(vec![looks(), says("Mum is called.")], 1, &asked, &hold),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the reminder's turn reaches its second step", || {
        asked.lock().unwrap().len() == 2
    })
    .await;
    stop_agent(
        &actions,
        &workspace,
        &me,
        &chat::chat::reply_id(&second, &scout),
    )
    .await
    .expect("the person stops the work a reminder began for them");
    agents.look(&workspace).await.unwrap();
    hold.open();
    let reply = ended(&workspaces, &workspace, &second).await;
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "the reminder's reply says it was stopped: {:?}",
        reply.work
    );
    assert_eq!(asked.lock().unwrap().len(), 2, "no step follows a stop");
    page.peer.close();
}

/// She writes her next message while a look that read before her first answer enters its
/// place. The message gets a turn of its own: one model call that begins with her words and is
/// told the answer before them. Nothing of it goes into a second turn on her first message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_message_after_an_answer_gets_a_turn_of_its_own() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
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
        &served,
        &workspace,
        waits(
            vec![says("The launch is planned."), says("The cake is ordered.")],
            1,
            &asked,
            &hold,
        ),
    )
    .await;
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

/// A long conversation: once her answer has its words the server asks a model for what the
/// oldest messages add to the conversation's summary, and that question is held here. Her
/// answer is finished all the same, and what she writes meanwhile gets a turn of its own while
/// the summary is still being written. The turn that ends meanwhile asks for no second summary
/// of the same messages, and the one summary is kept once its question is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn her_next_message_does_not_wait_while_the_summary_is_written() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let written = (&root, &browser);
    let ada = themselves(&root, &browser, &workspace);
    // Much more than a turn is told as written. She stopped each of these, so none of them
    // asks for an answer of its own: only her question does.
    for index in 0..10 {
        let note = format!(
            "Note {index}: {}",
            "the crossing went as planned and nothing needs doing. ".repeat(22)
        );
        let noted = write_to_agent(&actions, &workspaces, &workspace, written, &note).await;
        stop_agent(&actions, &workspace, &ada, &noted)
            .await
            .expect("she stops what the note would ask");
    }
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "anything to do today?",
    )
    .await;
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    let held = Arc::clone(&hold);
    // A question asked without tools is the summary's: the model waits before it answers it.
    let agents = worked(
        &served,
        &workspace,
        plays(&asked, move |request| {
            if request.tools.is_empty() {
                held.pass();
                says("The crossings went as planned.")
            } else if begins_with(request, "what is the boat called?") {
                says("Aurelia.")
            } else {
                says("Nothing needs doing today.")
            }
        }),
    )
    .await;
    let summaries = |asked: &Arc<Mutex<Vec<ModelRequest>>>| {
        asked
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.tools.is_empty())
            .count()
    };
    let read = agents.turns(&workspace).await.unwrap();
    assert_eq!(read.len(), 1, "{read:?}");
    assert_eq!(read[0].cause, first.to_string(), "her question is the turn");
    assert!(
        read[0].fold.is_some(),
        "more stands outside the summary than a turn carries as written"
    );
    agents.look(&workspace).await.unwrap();
    until("the summary's question is asked", || summaries(&asked) == 1).await;

    // Her answer is finished while that question is still open.
    let part = conversation_part(&workspaces, &workspace).await;
    let reply = part
        .messages
        .get(&chat::chat::reply_id(&first, &MemberId::agent(SCOUT)))
        .expect("the agent's reply is in the conversation");
    assert_eq!(reply.text, "Nothing needs doing today.");
    assert!(
        !chat::summons::unanswered(Some(reply)),
        "her answer is finished before its summary is written: {:?}",
        reply.work
    );

    // She writes again, and the look her write starts begins a turn of its own.
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "what is the boat called?",
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let named = ended(&workspaces, &workspace, &second).await;
    assert_eq!(named.text, "Aurelia.");
    {
        let requests = asked.lock().unwrap().clone();
        let words: Vec<String> = requests.iter().map(said_in).collect();
        assert_eq!(
            requests.len(),
            3,
            "her two answers and the one summary still open: {words:?}"
        );
        assert!(
            begins_with(&requests[2], "what is the boat called?"),
            "her second message is a turn of its own: {words:?}"
        );
    }

    // The summary's question is answered: the one summary is kept, and the next turn is told it.
    hold.open();
    let third = write_to_agent(&actions, &workspaces, &workspace, written, "thanks").await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let owed = agents.turns(&workspace).await.unwrap();
        let told = owed
            .iter()
            .find(|turn| turn.cause == third.to_string())
            .map(|turn| chat::recall::instruction_with_history(&turn.request, &turn.history))
            .unwrap_or_default();
        if told.contains("What came before, as you summarised it")
            && told.contains("The crossings went as planned.")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the summary was never kept: the next turn is told {told}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        summaries(&asked),
        1,
        "one summary of the same messages, not one by each turn"
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
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let planner =
        super::their_agent(&themselves(&root, &browser, &workspace), "planner").named("Planner");
    ask_agent(&actions, &workspace, &planner, &thread, "due-1")
        .await
        .expect("the reminder asks");
    let reminder = chat::cause::message_id(&MemberId::person(person.clone()), "due-1");
    let (asked, hold) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Hold::default()));
    let _opens = Opens(Arc::clone(&hold));
    // The model waits before it answers its first call.
    let agents = worked(
        &served,
        &workspace,
        waits(
            vec![
                says("It is time to call mum."),
                says("The cake is ordered."),
            ],
            0,
            &asked,
            &hold,
        ),
    )
    .await;
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

/// A look reads while her first message is owed, and enters only once that message is answered
/// and a turn on her next one is at work: the look's two steps, taken one at a time. What it
/// read is older than what the turn that goes there answers now, and that turn takes nothing
/// of it. Her second message keeps its answer, her first answer stands as it was written, and
/// no model is asked a third time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_look_overtaken_by_the_next_turn_hands_that_turn_nothing() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
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
    // The model waits before it answers its second call: the one her second message's turn asks.
    let agents = worked(
        &served,
        &workspace,
        waits(
            vec![says("The launch is planned."), says("The cake is ordered.")],
            1,
            &asked,
            &hold,
        ),
    )
    .await;

    // A look reads: her first message is the turn. It does not enter yet.
    let overtaken = agents.read_look(&workspace).await.unwrap();

    // Another look answers her first message.
    agents.look(&workspace).await.unwrap();
    let planned = ended(&workspaces, &workspace, &first).await;
    assert_eq!(planned.text, "The launch is planned.");

    // She writes again, and the turn on that message is at work.
    let second = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        written,
        "also order the cake",
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the turn on her second message asks its model", || {
        asked.lock().unwrap().len() == 2
    })
    .await;

    // The first look enters only now, with what it read before both.
    let current = agents.enter_look(&workspace, overtaken).await;
    hold.open();

    let ordered = ended(&workspaces, &workspace, &second).await;
    assert_eq!(
        ordered.text, "The cake is ordered.",
        "her second message keeps its answer"
    );
    let after = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        (after.text.as_str(), end_of(&after)),
        (planned.text.as_str(), end_of(&planned)),
        "her first answer stands as it was written"
    );
    let requests = asked.lock().unwrap().clone();
    let words: Vec<String> = requests.iter().map(said_in).collect();
    assert_eq!(
        requests.len(),
        2,
        "one model call for each of her messages: {words:?}"
    );
    assert!(
        !current,
        "what the look read was overtaken, so a look reads again"
    );
    let _ = person;
    page.peer.close();
}

/// A look reads a note its starter gave a sub-agent while the sub-agent works on its job. That
/// look carries what it read only once the job's turn took the note, answered under it and
/// ended, and a turn on what its person wrote next is at work: the look's two steps, taken one
/// at a time. The turn that goes there now takes nothing of the note. Its person's message
/// keeps its answer, the note's answer stands as it was written, and no model is asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_read_before_its_answer_is_not_handed_to_the_next_turn() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("usage-drop"),
    ]);

    // The sub-agent's first step on its job waits, and so does its answer to its person.
    let (job_hold, person_hold) = (Arc::new(Hold::default()), Arc::new(Hold::default()));
    let _opens = (
        Opens(Arc::clone(&job_hold)),
        Opens(Arc::clone(&person_hold)),
    );
    let (at_job, at_person) = (Arc::clone(&job_hold), Arc::clone(&person_hold));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(
        &served,
        &workspace,
        plays(&asked, move |request| {
            if !of_sub_agent(request) {
                return says("Told.");
            }
            let told = said_in(request);
            if told.contains("only Safari") {
                at_person.pass();
                says("Yes, only Safari.")
            } else if !answered_a_tool(request) {
                at_job.pass();
                looks()
            } else if told.contains("Tuesday") {
                says("Done, sign-in included.")
            } else {
                says("Done.")
            }
        }),
    )
    .await;
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Usage drop", "Find out why daily use fell."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    agents.look(&workspace).await.unwrap();
    until("the sub-agent's first step is under way", || {
        asked.lock().unwrap().iter().any(of_sub_agent)
    })
    .await;

    // Its starter gives it a note, which the server writes into its conversation.
    on_roster(
        &workspaces,
        &workspace,
        (Some(SCOUT), person.as_str()),
        WorkspaceAgents::NOTE_SUB_AGENT.with(workspace_agents::NoteSubAgent {
            agent: Id::from_raw("usage-drop".to_owned()),
            text: NonBlank::parse("Also check Tuesday's sign-in change.").unwrap(),
        }),
    )
    .await
    .expect("its starter gives it a note");
    agents.look(&workspace).await.unwrap();
    let notes = caused(&workspaces, &workspace, (own.as_str(), "note"), 1).await;
    let note = notes[0].0.clone();

    // A look reads: the note is there and unanswered. It carries nothing yet.
    let overtaken = agents.read_look(&workspace).await.unwrap();

    // Another look hands the note to the turn that works, which answers under it and ends.
    agents.look(&workspace).await.unwrap();
    job_hold.open();
    let noted = reply_of(&workspaces, &workspace, &note, "usage-drop").await;
    assert_eq!(noted.text, "Done, sign-in included.");
    stands_as(&workspaces, &workspace, "usage-drop", Stands::Done).await;

    // Its person writes into its conversation, and the turn on that message is at work.
    let typed = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "is it only Safari?"),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    until("the turn on its person's message asks its model", || {
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| of_sub_agent(request) && said_in(request).contains("only Safari"))
    })
    .await;

    // The first look carries the note only now.
    let current = agents.enter_look(&workspace, overtaken).await;
    person_hold.open();

    let answered = reply_of(&workspaces, &workspace, &typed, "usage-drop").await;
    assert_eq!(
        answered.text, "Yes, only Safari.",
        "its person's message keeps its answer"
    );
    let after = reply_of(&workspaces, &workspace, &note, "usage-drop").await;
    assert_eq!(
        (after.text.as_str(), end_of(&after)),
        (noted.text.as_str(), end_of(&noted)),
        "the note's answer stands as it was written"
    );
    let steps = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|request| of_sub_agent(request))
        .count();
    assert_eq!(
        steps, 3,
        "two steps on its job and the note, one for its person"
    );
    assert!(
        !current,
        "the note the look read was overtaken, so a look reads again"
    );
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
    conversation_part(workspaces, workspace)
        .await
        .messages
        .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)))
        .is_some_and(|reply| !chat::summons::unanswered(Some(reply)))
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
        reply.work
    );
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Complete),
        "and its reply says it is whole: {:?}",
        reply.work
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
    let me = themselves(&root, &browser, &workspace);
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

    stop_agent(&actions, &workspace, &me, &first)
        .await
        .expect("she stops her agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.work
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
