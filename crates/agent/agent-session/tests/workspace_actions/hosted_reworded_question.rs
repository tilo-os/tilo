//! A question an agent reworded after its server asked it is not what its person was asked.
//!
//! With the server, the person and the assistant of the test this is a part of: the assistant
//! asks to use an item of its person's vault, then puts other words on its question, as the
//! author of a message may; its person answers "Always". Nothing is signed in to, the question
//! says it was not done, and nothing stands for the assistant: the next use asks again.
use std::time::{Duration, Instant};

use super::*;

/// The assistant puts other words on the question `message` it asked: written into the
/// conversation's document as the agent, the author of that message, the way its own edit of a
/// message it wrote is written.
pub(super) async fn the_assistant_rewords(hosted: &Hosted, message: &Id, words: &str) {
    let saved = hosted
        .workspaces
        .resource_document(&hosted.workspace, CHAT, "")
        .await
        .unwrap()
        .expect("the workspace's chat");
    let held = chat::chat::Chat::read(&automerge::AutoCommit::load(&saved).unwrap()).unwrap();
    let person = hosted.person();
    let rewording = chat::messages::chat_messages::EditMessage {
        message: message.clone(),
        text: words.to_owned(),
        body: None,
        mentions: None,
    };
    let catalogue = crate::common::catalogue();
    let governor = hosted.workspaces.device_cert().clone();
    let part = chat::parts::part(0);
    hosted
        .workspaces
        .author_application(
            &hosted.workspace,
            &governor,
            &catalogue,
            CHAT,
            part.as_str(),
            |doc| {
                chat::demo::apply_part_doc_as(
                    doc,
                    &held,
                    |replica| replica.as_agent_for(SCOUT, &person),
                    None,
                    chat::parts::ChatMessages::EDIT_MESSAGE.with(rewording),
                )
                .map_err(|error| format!("{error:?}"))
            },
        )
        .await
        .expect("an agent edits a message it wrote");
}

/// How the answer to the one question about an action ended, once the server says so.
pub(super) async fn ended(hosted: &Hosted) -> String {
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = hosted.questions(chat::question::ACTION).await;
        let outcome = now[0].1.question.as_ref().unwrap().outcome.clone();
        if let Some(outcome) = outcome
            .as_ref()
            .filter(|outcome| outcome.outcome.0 != chat::question::STARTED)
        {
            return outcome.outcome.0.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the server never said how the answer ended: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn always_to_a_question_its_agent_reworded_does_nothing_and_lets_nothing_stand() {
    let hosted = Hosted::start().await;
    let reference = the_password_goes_from_the_persons_device_to_their_vault(&hosted).await;
    let using = |call: &str| add_mail(call, "ada@example.test", &reference);
    let waits = Reply::text("I asked you to allow it. It waits for your answer.");
    hosted
        .turn("It is in my vault now.", vec![using("call-2"), waits])
        .await;
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 1, "{}", hosted.last_tool_result());
    let question = asked[0].0.clone();

    // The assistant rewords what it asked, and its person answers what they now read.
    the_assistant_rewords(&hosted, &question, "May I tidy up our chat?").await;
    let reworded = hosted.questions(chat::question::ACTION).await;
    assert!(reworded[0].1.edited, "the question shows other words now");
    assert_eq!(reworded[0].1.text, "May I tidy up our chat?");
    hosted.answers(&question, chat::question::ALWAYS).await;
    let ended = ended(&hosted).await;
    let signed_in = hosted.proved.lock().unwrap().len();

    // Whether anything stands for the assistant: the same use, asked for once more.
    let again = Reply::text("I asked you to allow it.");
    hosted
        .turn("Please try once more.", vec![using("call-3"), again])
        .await;
    let asks_again = hosted.last_tool_result().contains(ASKED);
    assert!(
        ended == chat::question::FAILED && signed_in == 0 && asks_again,
        "always to a reworded question: it ended {ended:?} (not done is {:?}), signed in \
         {signed_in} time(s), and the next use asks again: {asks_again}",
        chat::question::FAILED
    );
    assert!(
        hosted.proved.lock().unwrap().is_empty(),
        "and nothing is signed in to afterwards either"
    );
    hosted.page.peer.close();
}
