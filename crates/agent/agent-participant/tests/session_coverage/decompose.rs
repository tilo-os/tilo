//! Coordinated tasks over the real session, planned by canned model turns:
//! the coordinator lists the items, each item is planned by a fresh agent
//! told only the request, its item, the checklist and what the items before
//! it did, offered only its item's apps' operations; the coordinator holds
//! a wait with no model call, takes news for the items, and checks the
//! whole once.

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
/// (an id ending `#<action>`).
fn offers(body: &str, action: &str) -> bool {
    let text = request_text(body);
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
        news.contains("While an item's agent worked, new messages arrived (chat)")
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
