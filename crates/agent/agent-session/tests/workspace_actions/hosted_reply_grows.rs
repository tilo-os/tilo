//! A reply grows while its model writes it.
//!
//! The model a person linked writes its answer over seconds, and the server puts the words
//! into the reply as they arrive, so its person reads along on every device. The test tells
//! the provider what to write and where to wait: it reads the reply while the model is in the
//! middle of its answer.
use super::writing::{Next::*, begin, reads, reads_by, writing, written};
use super::*;
use chat::agent_stream::AgentFinish;
use std::sync::atomic::Ordering;

/// Her assistant's model writes its first sentences and is still writing: they stand in the
/// reply already, on the server's own copy of the conversation, and the reply says its work
/// goes on. When the model has written the rest, the reply reads the whole answer and says
/// it ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_grows_while_its_model_writes_it() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Writes("The launch is planned "),
            Writes("for May. "),
            Waits,
            Writes("The cake is ordered."),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(agents, workspace);

    let grown = reads(
        &workspaces,
        &workspace,
        &first,
        "The launch is planned for May.",
    )
    .await;
    assert_eq!(
        (theirs.waited.load(Ordering::SeqCst), going.is_finished()),
        (1, false),
        "the model has not written its last words"
    );
    assert!(
        chat::summons::unanswered(Some(&grown)),
        "a reply that grows says its work goes on: {:?}",
        grown.work
    );
    assert_eq!(grown.author, MemberId::agent(SCOUT), "the agent writes it");

    theirs.go_on();
    going.await.unwrap().unwrap();
    let whole = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        whole.text,
        "The launch is planned for May. The cake is ordered."
    );
    assert_eq!(end_of(&whole), Some(AgentFinish::Complete));
    assert_eq!(written(&whole).1, Some(AgentFinish::Complete));
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );
    let _ = person;
    page.peer.close();
}

/// A model that writes three hundred pieces in one go does not make three hundred changes of
/// the conversation: the first words go into the reply at once and the rest together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_written_fast_is_few_changes() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (_theirs, _lets_go) = writing(&workspaces, &workspace, [vec![Writes("word "); 300]]).await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "count to three hundred",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    begin!(agents, workspace).await.unwrap().unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(reply.text, "word ".repeat(300).trim_end());
    let (changes, end) = written(&reply);
    assert_eq!(end, Some(AgentFinish::Complete));
    assert!(
        changes <= 8,
        "three hundred pieces written at once came in {changes} changes"
    );
    let _ = person;
    page.peer.close();
}

/// What a model writes before it uses a tool stays where its person read it, and what it
/// writes after the tool stands a paragraph below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn words_before_a_tool_stay_and_the_answer_stands_below_them() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("I look through the workspace first. "), Looks],
            vec![Waits, Writes("The plan is "), Writes("ready.")],
        ],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "what is planned?",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(agents, workspace);

    // The tool was used and the model is asked again: what it wrote before stands there.
    until("the model is asked again after its tool", || {
        theirs.waited.load(Ordering::SeqCst) == 1
    })
    .await;
    let before = reads(
        &workspaces,
        &workspace,
        &first,
        "I look through the workspace first.",
    )
    .await;
    assert!(
        chat::summons::unanswered(Some(&before)),
        "{:?}",
        before.work
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let whole = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        whole.text,
        "I look through the workspace first.\n\nThe plan is ready."
    );
    assert_eq!(end_of(&whole), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// Her stop ends the call to the provider itself, while the provider is in the middle of its
/// answer. The reply keeps the words that came and says it was stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_ends_the_models_call_and_the_reply_keeps_what_came() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![Writes("The launch is "), Waits, Writes("never read")]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    agents.look(&workspace).await.unwrap();
    reads(&workspaces, &workspace, &first, "The launch is").await;
    assert_eq!(theirs.let_go.load(Ordering::SeqCst), 0);

    let me = themselves(&root, &browser, &workspace);
    stop_agent(&actions, &workspace, &me, &first)
        .await
        .expect("the person stops their agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.work
    );
    assert_eq!(reply.text, "The launch is", "what came stays");
    until("the provider's call is let go of", || {
        theirs.let_go.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(
        theirs.waited.load(Ordering::SeqCst),
        1,
        "the call ended where it stood: the provider wrote nothing more"
    );
    let _ = person;
    page.peer.close();
}

/// A provider whose answer breaks off in the middle, in a way asking again does not cure: the
/// reply keeps the words that came, and says below them that the model did not answer and why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_that_breaks_off_keeps_what_came_and_says_why() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (_theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![Writes("The launch is "), Breaks]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    begin!(agents, workspace).await.unwrap().unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text,
        "The launch is\n\nMy model did not answer: its answer could not be read to the end"
    );
    assert_eq!(end_of(&reply), Some(AgentFinish::Failed));
    assert_eq!(written(&reply).1, Some(AgentFinish::Failed));
    let _ = person;
    page.peer.close();
}

/// A call that breaks off and is asked again within the turn: what came of the first asking
/// stays, and the answer stands a paragraph below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_answer_of_a_call_asked_again_stands_below_what_came() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (_theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Writes("The launch is "),
            AskedAgain,
            Writes("The launch is planned "),
            Writes("for May."),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    begin!(agents, workspace).await.unwrap().unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text,
        "The launch is\n\nThe launch is planned for May."
    );
    assert_eq!(end_of(&reply), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// The server stops while a reply grows. Started again it asks the model anew, since the turn
/// changed nothing, and the answer stands below the words the reply held: nothing a person
/// read is taken away, and the answer is not lost behind them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_that_grew_on_a_server_that_stopped_gets_its_answer_below() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("The launch is "), Waits, Writes("never read")],
            vec![Writes("The launch is planned for May.")],
        ],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let dying = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(dying, workspace);
    reads(&workspaces, &workspace, &first, "The launch is").await;
    // The server's process ends: its turn is dropped where it stands, and the call nobody
    // waits for any more ends into nothing.
    going.abort();
    let _ = going.await;
    drop(dying);
    theirs.go_on();

    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    begin!(agents, workspace).await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text,
        "The launch is\n\nThe launch is planned for May."
    );
    assert_eq!(end_of(&reply), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// A sub-agent's model writes its answer over seconds as well. The words stand in its reply,
/// in the conversation of its own, while the model is still writing, and none of them stands
/// in the thread of the agent that started it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agents_reply_grows_in_its_own_conversation() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let own = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent("writer")]);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            // Its job, then what its starter says to the news of it.
            vec![Writes("The notes are tidy.")],
            vec![Writes("Noted.")],
            // What its person asks it in its own conversation.
            vec![
                Writes("Two drafts "),
                Writes("are left. "),
                Waits,
                Writes("I file them next."),
            ],
        ],
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Writer", "Tidy the notes."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let job = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await[0]
        .0
        .clone();
    agents.look(&workspace).await.unwrap();
    let done = reply_of(&workspaces, &workspace, &job, "writer").await;
    assert_eq!(done.text, "The notes are tidy.");
    agents.look(&workspace).await.unwrap();
    let news = caused(&workspaces, &workspace, (thread.as_str(), "news"), 1).await[0]
        .0
        .clone();
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &news, SCOUT).await;

    let asked = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "Where do the notes stand?"),
    )
    .await;
    let going = begin!(agents, workspace);
    let grown = reads_by(
        &workspaces,
        &workspace,
        (&asked, "writer"),
        "Two drafts are left.",
    )
    .await;
    assert_eq!(
        (theirs.waited.load(Ordering::SeqCst), going.is_finished()),
        (1, false),
        "its model has not written its last words"
    );
    assert_eq!(
        grown.author,
        MemberId::agent("writer"),
        "the sub-agent writes it"
    );
    assert_eq!(
        *conversation_id(&grown),
        own,
        "in the conversation of its own"
    );
    assert!(
        chat::summons::unanswered(Some(&grown)),
        "a reply that grows says its work goes on: {:?}",
        grown.work
    );
    let above = said_there(&workspaces, &workspace, thread.as_str()).await;
    assert!(
        above
            .iter()
            .all(|(_, message)| !message.text.contains("Two drafts")),
        "none of its words stands in its starter's thread: {above:?}"
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let whole = reply_of(&workspaces, &workspace, &asked, "writer").await;
    assert_eq!(whole.text, "Two drafts are left. I file them next.");
    assert_eq!(end_of(&whole), Some(AgentFinish::Complete));
    assert_eq!(written(&whole).1, Some(AgentFinish::Complete));
    page.peer.close();
}

/// The guest `guest` holds `role` in `workspace` from now on, as its administrator puts them
/// there: what the server may write for them is decided by it.
async fn guest_holds(
    workspaces: &Workspaces,
    workspace: &str,
    server: &Device,
    guest: &SigningKey,
    role: tilo_peer_state::join::Role,
) {
    workspaces
        .add_member(
            workspace,
            &server.cert,
            tilo_server::workspaces::AddMemberRequest {
                request_id: *blake3::hash(
                    format!(
                        "{} {role:?} {}",
                        hex::encode(guest.verifying_key().to_bytes()),
                        tilo_clock::unix_now_ns()
                    )
                    .as_bytes(),
                )
                .as_bytes(),
                user_public_key: guest.verifying_key().to_bytes(),
                place: tilo_server::workspaces::MemberPlace::Role(role),
                name: Some("Eve".into()),
            },
        )
        .await
        .unwrap();
}

/// Chat refuses the end of the answer: the turn ends with the error that nobody kept it, and its
/// reply is not taken for one its person read as answered. What the model wrote stays where they
/// read it, and once the workspace lets them write again, the answer stands below those words
/// and the reply ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_whose_end_chat_refused_is_not_taken_for_answered() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served, device
    );
    // A guest of the workspace asks the agent, and the workspace decides what the server may
    // write for them: an editor, until the answer is on its way.
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Eve", 2);
    let guest_device = enrol(&guest, "eve-browser", DeviceCapabilities::member());
    let editor = tilo_peer_state::join::Role::Editor;
    guest_holds(&workspaces, &workspace, &device, &guest, editor).await;
    // The guest's own device is the one the server acts as for them.
    let _guest =
        resource_support::open_admitted_at(&served, &guest_device, &device, &workspace).await;
    let asked = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&guest, &guest_device),
        "plan the launch",
    )
    .await;
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("The plan is ready."), Waits],
            vec![Writes("The plan is ready. The cake is ordered.")],
        ],
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(agents, workspace);
    let grown = reads(&workspaces, &workspace, &asked, "The plan is ready.").await;
    assert!(
        chat::summons::unanswered(Some(&grown)),
        "its end is not recorded yet: {:?}",
        grown.work
    );

    // The guest may read the workspace from now on and change nothing in it: Chat refuses the
    // end of the answer the server writes for them.
    guest_holds(
        &workspaces,
        &workspace,
        &device,
        &guest,
        tilo_peer_state::join::Role::Viewer,
    )
    .await;
    theirs.go_on();
    let refused = going.await.unwrap();
    assert!(
        refused.is_err(),
        "the end Chat did not take is not taken for one: {refused:?}"
    );
    let part = conversation_part(&workspaces, &workspace).await;
    let reply = part
        .messages
        .get(&chat::chat::reply_id(&asked, &MemberId::agent(SCOUT)))
        .expect("the reply stands where its person read it")
        .clone();
    assert_eq!(reply.text, "The plan is ready.");
    assert!(
        chat::summons::unanswered(Some(&reply)),
        "no end was kept, and the reply says nothing it did not: {:?}",
        reply.work
    );

    // The workspace lets the guest write for themselves again. The next look ends the turn, and
    // the answer the model wrote now stands below the words its person already read.
    guest_holds(&workspaces, &workspace, &device, &guest, editor).await;
    agents.look(&workspace).await.unwrap();
    let finished = ended(&workspaces, &workspace, &asked).await;
    assert_eq!(end_of(&finished), Some(AgentFinish::Complete));
    assert_eq!(
        finished.text,
        "The plan is ready.\n\nThe plan is ready. The cake is ordered."
    );
    assert_eq!(theirs.asked(), 2, "the answer is continued, not lost");
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "and it is not owed a turn again"
    );
    let _ = person;
    page.peer.close();
}
