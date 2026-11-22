//! What a person is told when a turn that an event of theirs started ends, with nothing of
//! theirs on. A reminder that is due asks their assistant on the server, the ask made from the
//! reminder, and how that turn ended joins the person's own list of what they were told, as a
//! reminder: what the assistant answered, or the sentence its reply carries where the turn could
//! not work. An ask made from an event this server keeps nothing of for them is told as news,
//! and what they asked themselves tells them nothing. Read as their own device reads it from the
//! server.
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

/// The one turn owed in `workspace`, worked with `model`.
async fn works_the_turn(up: &Up, workspace: &str, model: Option<Model>) {
    let agents = up.agents(workspace, model).await;
    let turns = agents.turns(workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    agents.answer(workspace, &turns[0]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_a_reminder_started_tells_its_person_how_it_ended_and_their_own_asking_nothing() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let device = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let (up, workspace, page) = team_on(&root, (&device, &browser), state.path()).await;
    let me = themselves(&root, &browser, &workspace);
    let person = hex::encode(root.verifying_key().to_bytes());

    // The person writes to their assistant themselves and closes their page: nothing of theirs
    // is on from here until they come back. Their assistant answers their own words.
    let own = writes_to_assistant(&up.workspaces, &workspace, &person, "What is due today?").await;
    page.peer.close();
    works_the_turn(&up, &workspace, Some(says_what_is_due())).await;
    answer_of(&up, &workspace, &person, &own).await;

    // A reminder of theirs comes due. The server asks their assistant, and the assistant answers.
    let (call_home, _) = set_up(&up, &workspace, &me, "Call home", 1_500);
    let first = asked_from(&up, &workspace, &person, &call_home.id).await;
    works_the_turn(&up, &workspace, Some(says_what_is_due())).await;
    let answer = answer_of(&up, &workspace, &person, &first.at).await;
    assert!(answer.contains("It is time to call home"), "{answer}");

    // A second reminder is due while no model is linked for the assistant's work: its turn
    // cannot work, and its reply says why.
    let (stand_up, _) = set_up(&up, &workspace, &me, "Stand up", 1_500);
    let second = asked_from(&up, &workspace, &person, &stand_up.id).await;
    works_the_turn(&up, &workspace, None).await;
    let could_not = answer_of(&up, &workspace, &person, &second.at).await;
    assert!(could_not.contains("No model is linked"), "{could_not}");

    // An ask made from an event this server keeps nothing of for the person.
    let third = tilo_workspace_service::agents::ask_for_event(
        &up.workspaces,
        &workspace,
        (&person, SCOUT),
        ("planner", "plan-1"),
        "Plan the week.".to_owned(),
    )
    .await
    .expect("the ask is made");
    works_the_turn(&up, &workspace, Some(says_what_is_due())).await;
    let named = answer_of(&up, &workspace, &person, &third).await;

    // The person comes back. They were told how each turn an event asked for ended, in the words
    // of its reply and no others. What they asked themselves, and its answer, told them nothing.
    let told = told_to(&up, (&browser, &device), &workspace).await.told;
    assert_eq!(
        told.len(),
        3,
        "one thing for each turn an event asked for, and nothing else: {told:?}"
    );
    // A reminder of theirs is told as one, and reaches them.
    assert_eq!(
        (told[0].kind, told[0].title.as_str(), told[0].words.as_str()),
        (Kind::Reminder, "Call home", answer.as_str())
    );
    assert_eq!(
        (told[1].kind, told[1].title.as_str(), told[1].words.as_str()),
        (Kind::Reminder, "Stand up", could_not.as_str())
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
        assert_eq!(
            thing.about.as_ref().map(|about| about.resource.clone()),
            Some(ask.at.id_string()),
            "it leads to the ask: {thing:?}"
        );
    }
    // What an event this server keeps nothing of for them asked is news: their own rule marks
    // it, and with no rule it does not reach them.
    assert_eq!(
        (told[2].kind, told[2].title.as_str(), told[2].words.as_str()),
        (Kind::News, "", named.as_str())
    );
    assert!(
        !told[2].reaches,
        "nothing reaches a person by asking for them: {:?}",
        told[2]
    );
    up.down().await;
}
