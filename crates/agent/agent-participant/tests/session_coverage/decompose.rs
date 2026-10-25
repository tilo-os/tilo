//! Coordinated tasks over the real session, planned by canned model turns:
//! the coordinator lists the items, each item is planned by a fresh agent
//! told the request, its item, the checklist and what the items before it
//! did and read, offered its item's apps' operations; an agent that says
//! "cannot" before trying is asked again; an item that could not be done
//! goes to the coordinator; the coordinator holds waits with no model call
//! while the items that do not need them run, takes news for the items
//! without listing them again, and checks the whole once.

use std::time::Duration;

use tilo_relay_protocol::agents::TaskState;

use super::{
    CHAT, RelayRoom, SHEETS, ana_said, answered_model_endpoint, chat_parts_text, last_turn,
    message_text, offered, plan_call, post_in_chat, posts, wait_for,
};

/// Every message of the planning request `body`, as one text.
fn request_text(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(message_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the planning request `body` offers an operation named `action`
/// (an id ending `#<action>`). What an earlier item's steps answered names
/// the operations it ran, and offers none of them.
fn offers(body: &str, action: &str) -> bool {
    let text: String = request_text(body)
        .lines()
        .filter(|line| {
            !line
                .trim_start()
                .starts_with("What it read and its steps answered:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let marker = format!("#{action}");
    text.match_indices(&marker).any(|(at, _)| {
        !text
            .get(at + marker.len()..)
            .and_then(|after| after.chars().next())
            .is_some_and(|c| c.is_alphanumeric() || c == '-')
    })
}

/// How many messages the planning request `body` carries: a fresh
/// conversation's first turn carries the instructions and the request.
fn messages(body: &str) -> usize {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    request["messages"].as_array().unwrap().len()
}

/// The coordinator lists three items; each is planned by a fresh agent
/// whose one turn holds the request, its item, the checklist with what is
/// done and what the items before it did, and nothing of the plans before
/// it; the coordinator checks once and the task finishes with every item
/// done. Only a coordinated task's `plan` takes items and notes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coordinated_task_gives_each_item_a_fresh_agent() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("In Chat, post Alpha ready, Bravo ready and Charlie ready")
        .unwrap();
    let first = room.next_request(&sent, "the coordinator's list").await;
    let request: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert!(request["tools"].to_string().contains("\"adjust\""));
    let told = last_turn(&first);
    assert!(told.contains("List the request's items"), "{told}");
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]},
                {"text": "post Bravo ready", "apps": ["Chat"], "after": [1]},
                {"text": "post Charlie ready", "apps": ["so.tilo.chat"]}]}),
        ))
        .unwrap();

    for (number, word) in [(1, "Alpha"), (2, "Bravo"), (3, "Charlie")] {
        let body = room.next_request(&sent, "an item's agent").await;
        assert_eq!(messages(&body), 2, "a fresh conversation");
        let told = last_turn(&body);
        assert!(
            told.contains(&format!(
                "you are the agent of item {number} only: post {word} ready"
            )),
            "{told}"
        );
        assert!(
            told.contains("The original request, for reference: In Chat, post Alpha ready"),
            "{told}"
        );
        assert!(!told.contains("List the request's items"), "{told}");
        if number == 2 {
            assert!(
                told.contains(
                    "1. post Alpha ready (done); 2. post Bravo ready (yours); 3. post Charlie \
                     ready (to do)"
                ),
                "{told}"
            );
            assert!(
                told.contains("- 1. post Alpha ready: done.") && told.contains("Alpha ready"),
                "what item 1 did: {told}"
            );
        }
        let post = offered(&body, "post");
        answer
            .send(plan_call(
                &format!("i{number}"),
                &serde_json::json!({"steps": posts(&post, &[(&format!("{word} ready"), None)])}),
            ))
            .unwrap();
    }

    let check = last_turn(&room.next_request(&sent, "the coordinator's check").await);
    assert!(check.contains("Every item has ended"), "{check}");
    assert!(
        check.contains(
            "1. post Alpha ready (done); 2. post Bravo ready (done); 3. post Charlie ready (done)"
        ),
        "{check}"
    );
    answer
        .send(plan_call(
            "c2",
            &serde_json::json!({"steps": [], "reply": "All posted."}),
        ))
        .unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    let said = ana_said(&room);
    assert!(
        said.starts_with("Done: 3 actions.")
            && said.ends_with(
                "Items done: 1. post Alpha ready; 2. post Bravo ready; 3. post Charlie ready."
            ),
        "{said}"
    );
    let held = chat_parts_text(&room.ana.0, &ours);
    for word in ["Alpha", "Bravo", "Charlie"] {
        assert!(held.contains(&format!("{word} ready")), "{held}");
    }
    room.pump_for(Duration::from_millis(300)).await;
    assert!(sent.try_recv().is_err(), "no plan after the check");
    Box::pin(room.close()).await;
}

/// An item's agent is offered only its item's apps' operations: the
/// sheet's edits to the item that writes the sheet, the chat's post to the
/// item that posts, and of the other app's open record only its reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_s_agent_is_offered_only_its_item_s_apps() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::granting(&url, &[CHAT, SHEETS])).await;
    room.ana.0.decompose(true);
    room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("Write Alpha in the sheet, then post Alpha ready in Chat")
        .unwrap();
    let first = room.next_request(&sent, "the coordinator's list").await;
    assert!(offers(&first, "set-cells") && offers(&first, "post"));
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "write Alpha in A1", "apps": ["sheets"]},
                {"text": "post Alpha ready", "apps": ["chat"], "after": [1]}]}),
        ))
        .unwrap();

    let sheet = room.next_request(&sent, "the sheet item's agent").await;
    assert!(offers(&sheet, "set-cells"), "{}", request_text(&sheet));
    assert!(!offers(&sheet, "post"), "{}", request_text(&sheet));
    answer
        .send(plan_call(
            "i1",
            &serde_json::json!({"steps": [{"operation": offered(&sheet, "set-cells"),
                "arguments": {"sheet": "00000000-0000-4000-8000-000000000001", "start": "A1",
                    "rows": [["Alpha"]]}}]}),
        ))
        .unwrap();

    let chat = room.next_request(&sent, "the chat item's agent").await;
    assert!(offers(&chat, "post"), "{}", request_text(&chat));
    assert!(!offers(&chat, "set-cells"), "{}", request_text(&chat));
    assert!(
        last_turn(&chat).contains("write Alpha in A1: done. Ran: set-cells"),
        "{}",
        last_turn(&chat)
    );
    answer
        .send(plan_call(
            "i2",
            &serde_json::json!({"steps": posts(&offered(&chat, "post"), &[("Alpha ready", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer
        .send(plan_call(
            "c2",
            &serde_json::json!({"steps": [], "reply": "Done."}),
        ))
        .unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    Box::pin(room.close()).await;
}

/// An item that waits is the coordinator's: no model is asked while it
/// waits, and once what it waits for arrives the next item's agent starts
/// with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coordinator_s_wait_asks_no_model_and_hands_on_what_arrived() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("Ask Rae in Chat for the LR-2041 code, then post that you signed in with it")
        .unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "ask Rae for the code", "apps": ["chat"]},
                {"text": "wait for Rae's code", "wait_for": {"app": "chat", "contains": "code",
                    "seconds": 60}},
                {"text": "post signed in with the code", "apps": ["chat"], "after": [2]}]}),
        ))
        .unwrap();
    let ask = room.next_request(&sent, "item 1's agent").await;
    answer
        .send(plan_call(
            "i1",
            &serde_json::json!({"steps": posts(&offered(&ask, "post"),
                &[("Ana asks Rae for the code", None)])}),
        ))
        .unwrap();

    room.ana_waits(&task).await;
    room.pump_for(Duration::from_millis(500)).await;
    assert!(sent.try_recv().is_err(), "no model call while it waits");
    post_in_chat(&mut room.rae.0, &room.rae.1, "LR-2041 code 4321");

    let after = room.next_request(&sent, "item 3's agent").await;
    assert_eq!(messages(&after), 2, "a fresh conversation");
    let told = last_turn(&after);
    assert!(
        told.contains("2. wait for Rae's code (done)")
            && told.contains("- 2. wait for Rae's code: arrived: in chat")
            && told.contains("LR-2041 code 4321"),
        "{told}"
    );
    answer
        .send(plan_call(
            "i3",
            &serde_json::json!({"steps": posts(&offered(&after, "post"),
                &[("Signed in with 4321", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer
        .send(plan_call(
            "c2",
            &serde_json::json!({"steps": [], "reply": "Done."}),
        ))
        .unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    assert!(chat_parts_text(&room.ana.0, &ours).contains("Signed in with 4321"));
    Box::pin(room.close()).await;
}

/// News while an item's agent works goes to the coordinator, told the
/// items and the steps about to run. Its note on the running item reaches
/// that item's next turn with the news; its note on a done item has a fresh
/// agent do that item again, told the note and what it did before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn news_reaches_the_running_item_as_a_hint_and_redoes_a_done_one() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("In Chat, post Alpha ready and Bravo ready")
        .unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]},
                {"text": "post Bravo ready", "apps": ["chat"]}]}),
        ))
        .unwrap();
    let first = room.next_request(&sent, "item 1's agent").await;
    let post = offered(&first, "post");
    answer
        .send(plan_call(
            "i1",
            &serde_json::json!({"steps": posts(&post, &[("Alpha ready", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "item 2's agent").await;
    // While item 2 is planned, Ben corrects the request.
    room.ben_joins().await;
    post_in_chat(
        &mut room.ben.0,
        &room.ben.1,
        "Correction from Ben: it is Alfa, and Bravo per Ben",
    );
    wait_for("Ana to hold Ben's post", || {
        room.ana.0.pump();
        room.ben.0.pump();
        let held = chat_parts_text(&room.ana.0, &ours).contains("Correction from Ben");
        async move { held.then_some(()) }
    })
    .await;
    answer
        .send(plan_call(
            "i2",
            &serde_json::json!({"steps": posts(&post, &[("Bravo ready", None)])}),
        ))
        .unwrap();

    let news = last_turn(
        &room
            .next_request(&sent, "the coordinator's news turn")
            .await,
    );
    assert!(
        news.contains("While the items were worked on, new messages arrived (chat)")
            && news.contains("Correction from Ben"),
        "{news}"
    );
    assert!(
        news.contains("1. post Alpha ready (done); 2. post Bravo ready (running)")
            && news.contains("Item 2's agent is about to:")
            && news.contains("Bravo ready"),
        "{news}"
    );
    answer
        .send(plan_call(
            "c2",
            &serde_json::json!({"steps": [], "adjust": [
                {"item": 1, "note": "post Alfa ready instead"},
                {"item": 2, "note": "post Bravo ready per Ben"}]}),
        ))
        .unwrap();

    let hinted = last_turn(&room.next_request(&sent, "item 2's next turn").await);
    assert!(
        hinted.contains("Correction from Ben")
            && hinted
                .contains("The coordinator's note on it for your item: post Bravo ready per Ben")
            && hinted.contains("you are the agent of item 2 only"),
        "{hinted}"
    );
    answer
        .send(plan_call(
            "i3",
            &serde_json::json!({"steps": posts(&post, &[("Bravo ready per Ben", None)])}),
        ))
        .unwrap();

    let redo = room
        .next_request(&sent, "item 1 again, by a fresh agent")
        .await;
    assert_eq!(messages(&redo), 2, "a fresh conversation");
    let told = last_turn(&redo);
    assert!(
        told.contains("you are the agent of item 1 only: post Alpha ready")
            && told.contains("Do this item again: post Alfa ready instead.")
            && told.contains("What it did before: done."),
        "{told}"
    );
    answer
        .send(plan_call(
            "i4",
            &serde_json::json!({"steps": posts(&post, &[("Alfa ready", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer
        .send(plan_call(
            "c3",
            &serde_json::json!({"steps": [], "reply": "Done."}),
        ))
        .unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    let held = chat_parts_text(&room.ana.0, &ours);
    assert!(
        held.contains("Alfa ready") && held.contains("Bravo ready per Ben"),
        "{held}"
    );
    Box::pin(room.close()).await;
}

/// What a plan of no steps that says `reply` is, as a canned model turn.
fn reply_only(id: &str, reply: &str) -> serde_json::Value {
    plan_call(id, &serde_json::json!({"steps": [], "reply": reply}))
}

/// An item's agent that answers "cannot" before trying a step or a read is
/// asked once more, told to try first; it then does its item.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_s_agent_that_cannot_before_trying_is_asked_again_and_acts() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room.ana.1.message("In Chat, post Alpha ready").unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]}]}),
        ))
        .unwrap();
    room.next_request(&sent, "item 1's agent").await;
    answer
        .send(reply_only("i1", "cannot: the chat is not shown here"))
        .unwrap();

    let again = room
        .next_request(&sent, "item 1's agent, asked again")
        .await;
    assert_eq!(messages(&again), 2, "a fresh conversation");
    let told = last_turn(&again);
    assert!(
        told.contains("You answered \"cannot: the chat is not shown here\".")
            && told.contains("Try first: read or open what your item needs"),
        "{told}"
    );
    answer
        .send(plan_call(
            "i2",
            &serde_json::json!({"steps": posts(&offered(&again, "post"), &[("Alpha ready", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer.send(reply_only("c2", "Done.")).unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    assert!(chat_parts_text(&room.ana.0, &ours).contains("Alpha ready"));
    Box::pin(room.close()).await;
}

/// An item's agent plans with what the records the request is about hold
/// now, as one agent's first plan would, and with what the items before it
/// read: the messages item 2 read, not a line about them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_s_agent_plans_with_the_records_and_what_earlier_items_read() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::granting(&url, &[CHAT, SHEETS])).await;
    room.ana.0.decompose(true);
    room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message(
            "Write Alpha in the sheet, read what Rae requested in Chat, then post which \
             request it was",
        )
        .unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "write Alpha in A1", "apps": ["sheets"]},
                {"text": "read Rae's request", "apps": ["chat"]},
                {"text": "post which request it was", "apps": ["chat"], "after": [2]}]}),
        ))
        .unwrap();
    let sheet = room.next_request(&sent, "item 1's agent").await;
    answer
        .send(plan_call(
            "i1",
            &serde_json::json!({"steps": [{"operation": offered(&sheet, "set-cells"),
                "arguments": {"sheet": "00000000-0000-4000-8000-000000000001", "start": "A1",
                    "rows": [["Alpha"]]}}]}),
        ))
        .unwrap();
    let reads = room.next_request(&sent, "item 2's agent").await;
    answer
        .send(plan_call(
            "i2",
            &serde_json::json!({"steps": [{"operation": offered(&reads, "read-messages"),
                "arguments": {"channel": chat::record::CHAT_CHANNEL, "pageSize": 32}}]}),
        ))
        .unwrap();
    room.next_request(&sent, "item 2's turn after its read")
        .await;
    answer
        .send(reply_only("i3", "Read: Rae requested the LR-2041 code."))
        .unwrap();

    let body = room.next_request(&sent, "item 3's agent").await;
    let whole = request_text(&body);
    let Some((_, held)) = whole.split_once("What the documents this request is about hold now")
    else {
        panic!("an item's agent sees what the records hold: {whole}");
    };
    assert!(held.contains("Alpha"), "the sheet as it is now: {held}");
    let told = last_turn(&body);
    let Some((_, found)) = told.split_once("- 2. read Rae's request: done.") else {
        panic!("what item 2 did: {told}");
    };
    assert!(
        found.contains("What it read and its steps answered:")
            && found.contains("LR-2041 code requested"),
        "what item 2 read is handed on: {told}"
    );
    answer
        .send(plan_call(
            "i4",
            &serde_json::json!({"steps": posts(&offered(&body, "post"),
                &[("It was LR-2041", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer.send(reply_only("c2", "Done.")).unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    Box::pin(room.close()).await;
}

/// Lists two items, the second needing the first, and has item 1's agent
/// say "cannot" twice (the second time after being asked to try): what the
/// coordinator's failure turn is told.
async fn item_1_fails(
    room: &mut RelayRoom,
    sent: &std::sync::mpsc::Receiver<String>,
    answer: &std::sync::mpsc::Sender<serde_json::Value>,
) -> String {
    room.next_request(sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]},
                {"text": "post Bravo ready", "apps": ["chat"], "after": [1]}]}),
        ))
        .unwrap();
    room.next_request(sent, "item 1's agent").await;
    answer.send(reply_only("i1", "cannot: no channel")).unwrap();
    room.next_request(sent, "item 1's agent, asked again").await;
    answer
        .send(reply_only("i2", "cannot: the channel is closed"))
        .unwrap();
    last_turn(
        &room
            .next_request(sent, "the coordinator's failure turn")
            .await,
    )
}

/// An item that could not be done goes to the coordinator instead of
/// failing the items that need it: its note has a fresh agent do the item
/// again, and the item that needs it runs after the redo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_item_goes_to_the_coordinator_which_has_it_done_again() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("In Chat, post Alpha ready, then Bravo ready")
        .unwrap();
    let failed = item_1_fails(&mut room, &sent, &answer).await;
    assert!(
        failed.contains("Item 1 (post Alpha ready) could not be done: the channel is closed.")
            && failed.contains("To have an item done again by a fresh agent")
            && failed.contains(
                "1. post Alpha ready (not done: the channel is closed); 2. post Bravo ready (to do)"
            ),
        "{failed}"
    );
    answer
        .send(plan_call(
            "c2",
            &serde_json::json!({"steps": [], "adjust": [
                {"item": 1, "note": "post it in the chat channel"}]}),
        ))
        .unwrap();
    let redo = room.next_request(&sent, "item 1 again").await;
    let told = last_turn(&redo);
    assert!(
        told.contains("you are the agent of item 1 only: post Alpha ready")
            && told.contains("Do this item again: post it in the chat channel."),
        "{told}"
    );
    let post = offered(&redo, "post");
    answer
        .send(plan_call(
            "i3",
            &serde_json::json!({"steps": posts(&post, &[("Alpha ready", None)])}),
        ))
        .unwrap();
    let bravo = room.next_request(&sent, "item 2's agent").await;
    assert!(
        last_turn(&bravo).contains("you are the agent of item 2 only: post Bravo ready"),
        "{}",
        last_turn(&bravo)
    );
    answer
        .send(plan_call(
            "i4",
            &serde_json::json!({"steps": posts(&post, &[("Bravo ready", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer.send(reply_only("c3", "Done.")).unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    let held = chat_parts_text(&room.ana.0, &ours);
    assert!(
        held.contains("Alpha ready") && held.contains("Bravo ready"),
        "{held}"
    );
    Box::pin(room.close()).await;
}

/// A coordinator that goes on without an item that could not be done has
/// the items that need it run anyway; the task's result says the item was
/// not done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_items_after_a_failed_one_run_when_the_coordinator_goes_on() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("In Chat, post Alpha ready, then Bravo ready")
        .unwrap();
    item_1_fails(&mut room, &sent, &answer).await;
    answer.send(reply_only("c2", "Go on without it.")).unwrap();
    let bravo = room.next_request(&sent, "item 2's agent").await;
    let told = last_turn(&bravo);
    assert!(
        told.contains("you are the agent of item 2 only: post Bravo ready")
            && told.contains(
                "1. post Alpha ready (not done, the items after it go on: the channel is closed)"
            ),
        "{told}"
    );
    answer
        .send(plan_call(
            "i3",
            &serde_json::json!({"steps": posts(&offered(&bravo, "post"),
                &[("Bravo ready", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer
        .send(reply_only("c3", "Alpha was not posted."))
        .unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Failed);
    assert!(ana_said(&room).contains("Item 1 (post Alpha ready) could not be done"));
    assert!(chat_parts_text(&room.ana.0, &ours).contains("Bravo ready"));
    Box::pin(room.close()).await;
}

/// A wait the coordinator holds blocks only the items that need it: an item
/// that does not runs while it waits, then the task sleeps with no model
/// call until the message arrives, which starts the item that needs it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_wait_lets_independent_items_run_and_wakes_on_what_arrives() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message(
            "Wait for Rae's LR-2041 code in Chat and post that you signed in; post Bravo ready",
        )
        .unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "wait for Rae's code", "wait_for": {"app": "chat",
                    "contains": "LR-2041 code", "seconds": 60}},
                {"text": "post signed in with the code", "apps": ["chat"], "after": [1]},
                {"text": "post Bravo ready", "apps": ["chat"]}]}),
        ))
        .unwrap();
    let meanwhile = room
        .next_request(&sent, "item 3's agent, while the wait is held")
        .await;
    let told = last_turn(&meanwhile);
    assert!(
        told.contains("you are the agent of item 3 only: post Bravo ready")
            && told.contains(
                "1. wait for Rae's code (waiting); 2. post signed in with the code (to do)"
            ),
        "{told}"
    );
    let post = offered(&meanwhile, "post");
    answer
        .send(plan_call(
            "i3",
            &serde_json::json!({"steps": posts(&post, &[("Bravo ready", None)])}),
        ))
        .unwrap();

    room.ana_waits(&task).await;
    room.pump_for(Duration::from_millis(500)).await;
    assert!(sent.try_recv().is_err(), "no model call while it waits");
    post_in_chat(&mut room.rae.0, &room.rae.1, "LR-2041 code 4321");

    let after = room.next_request(&sent, "item 2's agent").await;
    let told = last_turn(&after);
    assert!(
        told.contains("you are the agent of item 2 only")
            && told.contains("- 1. wait for Rae's code: arrived: in chat")
            && told.contains("LR-2041 code 4321"),
        "{told}"
    );
    answer
        .send(plan_call(
            "i2",
            &serde_json::json!({"steps": posts(&post, &[("Signed in with 4321", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer.send(reply_only("c2", "Done.")).unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    let held = chat_parts_text(&room.ana.0, &ours);
    assert!(
        held.contains("Bravo ready") && held.contains("Signed in with 4321"),
        "{held}"
    );
    Box::pin(room.close()).await;
}

/// A message a held wait waits for that arrived during the task before the
/// wait began ends it as it begins: no model call, and the item that needs
/// it starts with what arrived.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_wait_ends_on_what_arrived_before_it_began() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("Post Alpha ready in Chat, wait for Rae's LR-2041 code, then post it")
        .unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]},
                {"text": "wait for Rae's code", "after": [1], "wait_for": {"app": "chat",
                    "contains": "LR-2041 code", "seconds": 60}},
                {"text": "post the code", "apps": ["chat"], "after": [2]}]}),
        ))
        .unwrap();
    let first = room.next_request(&sent, "item 1's agent").await;
    // While item 1 is planned, before the wait begins, Rae sends the code.
    post_in_chat(&mut room.rae.0, &room.rae.1, "LR-2041 code 4321");
    wait_for("Ana to hold Rae's code", || {
        room.ana.0.pump();
        room.rae.0.pump();
        let held = chat_parts_text(&room.ana.0, &ours).contains("LR-2041 code 4321");
        async move { held.then_some(()) }
    })
    .await;
    let post = offered(&first, "post");
    answer
        .send(plan_call(
            "i1",
            &serde_json::json!({"steps": posts(&post, &[("Alpha ready", None)])}),
        ))
        .unwrap();

    let after = room.next_request(&sent, "item 3's agent").await;
    let told = last_turn(&after);
    assert!(
        told.contains("you are the agent of item 3 only: post the code")
            && told.contains("2. wait for Rae's code (done)")
            && told.contains("LR-2041 code 4321"),
        "{told}"
    );
    answer
        .send(plan_call(
            "i3",
            &serde_json::json!({"steps": posts(&post, &[("The code is 4321", None)])}),
        ))
        .unwrap();
    room.next_request(&sent, "the coordinator's check").await;
    answer.send(reply_only("c2", "Done.")).unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    assert!(chat_parts_text(&room.ana.0, &ours).contains("The code is 4321"));
    Box::pin(room.close()).await;
}

/// A news turn that lists the items already listed again, as they were or
/// in other words, adds none of them; only the item the news asks for is
/// new.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_news_turn_that_lists_the_items_again_adds_no_duplicates() {
    let (url, sent, answer) = answered_model_endpoint();
    let mut room = Box::pin(RelayRoom::start(&url)).await;
    room.ana.0.decompose(true);
    let ours = room.ana.0.open(CHAT, None).unwrap();
    let task = room
        .ana
        .1
        .message("In Chat, post Alpha ready and Bravo ready")
        .unwrap();
    room.next_request(&sent, "the coordinator's list").await;
    answer
        .send(plan_call(
            "c1",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]},
                {"text": "post Bravo ready", "apps": ["chat"]}]}),
        ))
        .unwrap();
    let first = room.next_request(&sent, "item 1's agent").await;
    room.ben_joins().await;
    post_in_chat(
        &mut room.ben.0,
        &room.ben.1,
        "From Ben: please also post Delta ready",
    );
    wait_for("Ana to hold Ben's post", || {
        room.ana.0.pump();
        room.ben.0.pump();
        let held = chat_parts_text(&room.ana.0, &ours).contains("please also post Delta");
        async move { held.then_some(()) }
    })
    .await;
    let post = offered(&first, "post");
    answer
        .send(plan_call(
            "i1",
            &serde_json::json!({"steps": posts(&post, &[("Alpha ready", None)])}),
        ))
        .unwrap();
    let news = last_turn(
        &room
            .next_request(&sent, "the coordinator's news turn")
            .await,
    );
    assert!(
        news.contains("please also post Delta")
            && news.contains("never list the items below again"),
        "{news}"
    );
    answer
        .send(plan_call(
            "c2",
            &serde_json::json!({"steps": [], "obligations": [
                {"text": "post Alpha ready", "apps": ["chat"]},
                {"text": "Post Bravo ready.", "apps": ["chat"]},
                {"text": "post Delta ready", "apps": ["chat"]}]}),
        ))
        .unwrap();
    for (number, word) in [(2, "Bravo"), (3, "Delta")] {
        let body = room.next_request(&sent, "the next item's agent").await;
        assert!(
            last_turn(&body).contains(&format!(
                "you are the agent of item {number} only: post {word} ready"
            )),
            "{}",
            last_turn(&body)
        );
        answer
            .send(plan_call(
                &format!("i{number}"),
                &serde_json::json!({"steps": posts(&post, &[(&format!("{word} ready"), None)])}),
            ))
            .unwrap();
    }
    let check = last_turn(&room.next_request(&sent, "the coordinator's check").await);
    assert!(
        check.contains(
            "1. post Alpha ready (done); 2. post Bravo ready (done); 3. post Delta ready (done)."
        ),
        "no item listed twice: {check}"
    );
    answer.send(reply_only("c3", "Done.")).unwrap();
    assert_eq!(room.ana_ends(&task).await, TaskState::Completed);
    Box::pin(room.close()).await;
}
