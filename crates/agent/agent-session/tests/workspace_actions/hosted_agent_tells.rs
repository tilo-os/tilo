//! What a person is told when a turn that a cause started for them ends, with nothing of theirs
//! on. A reminder that is due starts a turn of their assistant on the server, and how that turn
//! ended joins the person's own list of what they were told, as a reminder: what the assistant
//! answered, or the sentence its reply carries where the turn could not work. Whatever else asks
//! for them is told as news, whatever it names itself, and what they asked themselves tells
//! them nothing. Read as their own device reads it from the server.
use super::*;
use tilo_actions::remote::person_telling::{self, Kind, ToldList};

/// What the person finds they were told, as their own device reads it from the server once it
/// is on again.
async fn told_to(up: &Up, (browser, device): (&Device, &Device), workspace: &str) -> ToldList {
    let mut page = Browser::open_at(&up.hosted, browser, device, workspace).await;
    let listed = page
        .invoke_on(
            person_telling::PROVIDER,
            tilo_actions::api::ActionAddress::new(person_telling::TOLD, person_telling::LIST),
            tilo_actions::remote::InvokeArgument::None,
        )
        .await
        .expect("the server answers the person's own device")
        .value
        .unwrap_or_default();
    page.peer.close();
    serde_json::from_value(listed).expect("a list of what a person was told")
}

/// The message the reminder its person calls `label` asked the assistant with, once the
/// conversation holds it.
async fn asked_as(up: &Up, workspace: &str, label: &str) -> Id {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(&up.workspaces, workspace).await;
        let asked = part.messages.iter().find(|(_, held)| {
            held.cause
                .as_ref()
                .is_some_and(|cause| cause.label == label)
        });
        if let Some((id, _)) = asked {
            return id.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the reminder {label:?} never asked the assistant"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_a_reminder_started_tells_its_person_how_it_ended_and_their_own_asking_nothing() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    let me = themselves(&root, &browser, &workspace);

    // The person asks for a reminder and closes their page: nothing of theirs is on from here
    // until they come back. Their assistant answers their own words and sets the reminder up.
    reminded(&up, &workspace, (&root, &browser), &page, 2_000).await;

    // The moment comes. The server asks their assistant, and the assistant answers.
    let (first, _) = up.asked_by_a_cause(&workspace).await;
    let agents = up.agents(&workspace, Some(says_what_is_due())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let answer = ended(&up.workspaces, &workspace, &first).await;
    assert!(
        answer.text.contains("It is time to call home"),
        "{}",
        answer.text
    );
    drop(agents);

    // A second reminder is due while no model is linked for the assistant's work: its turn
    // cannot work, and its reply says why.
    let (when, _) = in_a_moment(1_500);
    let theirs = super::their_agent(&me, SCOUT).named("Scout");
    let set_up = serde_json::json!({
        "name": "Stand up",
        "when": when,
        "instruction": "Say that it is time to stand up.",
    });
    up.take(&workspace, &theirs, workspace_events::SET_UP, &set_up);
    let second = asked_as(&up, &workspace, "Stand up").await;
    let agents = up.agents(&workspace, None).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let could_not = ended(&up.workspaces, &workspace, &second).await;
    assert!(
        could_not.text.contains("No model is linked"),
        "{}",
        could_not.text
    );
    drop(agents);

    // Something else that acts for the person, and is no event of theirs, asks their assistant
    // and names itself a reminder.
    let person = hex::encode(root.verifying_key().to_bytes());
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let planner =
        super::their_agent(&themselves(&root, &browser, &workspace), "planner").named("Planner");
    ask_agent(&up.actions, &workspace, &planner, &thread, "plan-1")
        .await
        .expect("what acts for the person asks their agent");
    let third = chat::cause::message_id(&MemberId::person(person), "plan-1");
    let agents = up.agents(&workspace, Some(says_what_is_due())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(&workspace, &turns[0]).await.unwrap();
    let named = ended(&up.workspaces, &workspace, &third).await;
    drop(agents);

    // The person comes back. They were told how each turn a cause started ended, in the words
    // that are in their conversation and no others. What they asked themselves, and its answer,
    // told them nothing.
    let told = told_to(&up, (&browser, &device), &workspace).await.told;
    assert_eq!(
        told.len(),
        3,
        "one thing for each turn a cause started, and nothing else: {told:?}"
    );
    // A reminder of theirs is told as one, and reaches them.
    assert_eq!(
        (told[0].kind, told[0].title.as_str(), told[0].words.as_str()),
        (Kind::Reminder, "Call home", answer.text.as_str())
    );
    assert_eq!(
        (told[1].kind, told[1].title.as_str(), told[1].words.as_str()),
        (Kind::Reminder, "Stand up", could_not.text.as_str())
    );
    for (thing, ask) in told.iter().zip([&first, &second]) {
        assert!(thing.reaches, "a reminder reaches its person: {thing:?}");
        assert_eq!(
            thing.from.as_ref().map(|from| from.agent.as_str()),
            Some(SCOUT),
            "their assistant told it"
        );
        assert_eq!(
            thing.cause.as_ref().map(|cause| cause.kind.as_str()),
            Some("reminder")
        );
        assert!(
            thing
                .about
                .as_ref()
                .is_some_and(|about| about.resource.contains(ask.as_str())),
            "it leads to the message that asked: {thing:?}"
        );
    }
    // What names itself a reminder and is no event of theirs is news: their own rule marks it,
    // and with no rule it does not reach them.
    assert_eq!(
        (told[2].kind, told[2].title.as_str(), told[2].words.as_str()),
        (Kind::News, "Call mum", named.text.as_str())
    );
    assert!(
        !told[2].reaches,
        "nothing reaches a person by naming itself a reminder: {:?}",
        told[2]
    );
    up.down().await;
}
