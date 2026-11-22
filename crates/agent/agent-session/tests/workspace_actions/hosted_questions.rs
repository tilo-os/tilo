//! What an agent on the server asks its person before it does it, with no device of theirs on
//! while it works.
//!
//! An action the person's grants leave to them becomes a question in the agent's reply, and the
//! turn ends. The question waits there, over a restart of the server. The person answers the way
//! they write to the agent anywhere, in the channel they asked in: their "allow" does what the
//! server kept, once, and the agent is asked once what came of it. "Always" lets exactly that
//! action stand for them, so the next one goes ahead unasked. "No" does nothing. Anything else
//! they write is no answer: it asks the agent as ever, and the question still waits.
use super::hosted_secret_use::{Hosted, does};
use tilo_server::workspace_agents::ServerAgents;
use tilo_server::workspaces::person_model::scripted::Reply;
use tilo_workspace_service::asks::AskState;

/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked";
/// Taking the one book on the shelf off it: Undo cannot give the reader back the book's file,
/// so the action asks first.
const REMOVE: &str = "ebook/1 book#remove-from-library";

/// The assistant shelves the book `book` and asks to take it off the shelf again: the turn that
/// makes ends, its removal asked.
async fn shelves_then_removes(hosted: &Hosted, words: &str, book: &str) {
    let shelved = super::api_questions::shelved(book, book, "");
    hosted
        .turn(
            words,
            vec![
                does(
                    &format!("{book}-open"),
                    "ebook#open",
                    &serde_json::json!({}),
                ),
                does(&format!("{book}-shelve"), "ebook#shelve", &shelved),
                does(
                    &format!("{book}-remove"),
                    REMOVE,
                    &serde_json::json!({"book": format!("book:{book}")}),
                ),
                Reply::text("I need your answer before I take it off the shelf."),
            ],
        )
        .await;
    let told = hosted.tool_results();
    assert!(
        told.contains(ASKED),
        "the agent is told its person is asked, and its turn ended: {told}"
    );
}

/// The server stops and starts again: whatever ran its agents is gone, and what works them now
/// knows only what the records and the server's own store hold.
fn restart(hosted: &mut Hosted) {
    hosted.agents = ServerAgents::for_test(hosted.workspaces.clone(), hosted.actions.clone(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_action_that_asks_first_waits_in_the_reply_and_is_done_once_on_its_persons_answer() {
    let mut hosted = Hosted::start().await;
    shelves_then_removes(
        &hosted,
        "Put Dusk on the shelf, then take it off again.",
        "dusk",
    )
    .await;
    let asked = hosted.questions().await;
    assert_eq!(asked.len(), 1, "one question, in the reply: {asked:?}");
    assert!(
        asked[0].starts_with("I need your answer before I do this:")
            && asked[0].ends_with("Answer allow, always or no."),
        "the server's words for the action, and how it is answered: {}",
        asked[0]
    );
    assert!(
        hosted.said().await.contains(&asked[0]),
        "the reply says it where its person reads the answer"
    );
    assert_eq!(
        hosted.workspaces.open_asks(&hosted.workspace).len(),
        1,
        "the server keeps what was asked"
    );

    // Nobody answered: a look does nothing, however often the server looks, also after a
    // restart.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    restart(&mut hosted);
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert_eq!(hosted.workspaces.open_asks(&hosted.workspace).len(), 1);
    assert!(
        hosted
            .agents
            .turns(&hosted.workspace)
            .await
            .unwrap()
            .is_empty()
    );

    // She answers in the channel she asked in. Her answer is closed with the agent's word for it
    // and starts no turn of its own; what the server kept runs, and the agent is asked once what
    // came of it.
    hosted.answers("allow").await;
    let told = hosted.woken_with("Dusk is off the shelf.").await;
    assert!(
        told.contains("They answered: allowed, this once") && told.contains("It ran."),
        "the agent is told how she answered and that it ran: {told}"
    );
    let closed = hosted
        .asked()
        .await
        .into_iter()
        .find(|asked| {
            asked
                .said
                .as_deref()
                .is_some_and(|said| said.ends_with(" allow"))
        })
        .expect("her answer is an ask of the agent's");
    assert_eq!(closed.state, AskState::Done);
    assert_eq!(
        closed.latest().map(|reply| reply.body.trim().to_owned()),
        Some("Allowed this once. I'm doing it now.".to_owned())
    );

    // It ran once: nothing is kept any more, and a later look, after a restart too, finds
    // nothing to do.
    assert!(hosted.workspaces.open_asks(&hosted.workspace).is_empty());
    restart(&mut hosted);
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(
        hosted
            .agents
            .turns(&hosted.workspace)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(hosted.questions().await.len(), 1, "nothing was asked again");
    // Allowed once is allowed once: nothing stands.
    assert!(
        hosted
            .workspaces
            .permissions_of(&hosted.root.verifying_key().to_bytes())
            .permissions
            .is_empty()
    );
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_is_no_answer_asks_the_agent_and_a_no_does_nothing() {
    let hosted = Hosted::start().await;
    shelves_then_removes(
        &hosted,
        "Put Dusk on the shelf, then take it off again.",
        "dusk",
    )
    .await;

    // She writes something else first: it asks the agent as ever, and the question waits.
    hosted
        .turn(
            "why do you ask?",
            vec![Reply::text("Undo cannot bring the book's file back.")],
        )
        .await;
    assert_eq!(hosted.workspaces.open_asks(&hosted.workspace).len(), 1);

    // Then she says no: nothing runs, nothing is kept, and the agent is told so once.
    hosted.answers("no").await;
    let told = hosted.woken_with("I left Dusk on the shelf.").await;
    assert!(
        told.contains("They answered: not allowed") && told.contains("Nothing ran."),
        "{told}"
    );
    assert!(hosted.workspaces.open_asks(&hosted.workspace).is_empty());
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_lets_that_action_stand_so_the_next_one_asks_nobody() {
    let hosted = Hosted::start().await;
    shelves_then_removes(
        &hosted,
        "Put Dusk on the shelf, then take it off again.",
        "dusk",
    )
    .await;
    hosted.answers("always").await;
    let told = hosted.woken_with("Dusk is off the shelf.").await;
    assert!(
        told.contains("allowed, and from now on without asking") && told.contains("It ran."),
        "{told}"
    );
    assert_eq!(
        hosted
            .workspaces
            .permissions_of(&hosted.root.verifying_key().to_bytes())
            .permissions
            .len(),
        1,
        "what was asked stands for her"
    );

    // The next removal goes ahead with nobody asked.
    let shelved = super::api_questions::shelved("dawn", "dawn", "");
    hosted
        .turn(
            "Put Dawn on the shelf, then take it off again.",
            vec![
                does("dawn-open", "ebook#open", &serde_json::json!({})),
                does("dawn-shelve", "ebook#shelve", &shelved),
                does(
                    "dawn-remove",
                    REMOVE,
                    &serde_json::json!({"book": "book:dawn"}),
                ),
                Reply::text("Done."),
            ],
        )
        .await;
    assert_eq!(hosted.questions().await.len(), 1, "no second question");
    assert!(hosted.workspaces.open_asks(&hosted.workspace).is_empty());
    hosted.page.peer.close();
}
