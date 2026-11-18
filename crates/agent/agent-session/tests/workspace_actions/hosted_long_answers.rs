//! An answer arrives whole, however long it is.
//!
//! The server names no allowance of output tokens for a turn's answer, so the model's own
//! maximum is the one that holds. The provider here does what a provider does with an
//! allowance it is sent: it stops its answer there, a word a token and its thought counted
//! with its words, and says that the answer was cut.
use super::*;
use tilo_server::workspaces::person_model::{
    ProviderProtocol,
    scripted::{Linking, Reply, ScriptedProvider},
};

/// `count` words, no two alike.
fn words(count: usize) -> String {
    (0..count)
        .map(|nth| format!("w{nth}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Link `root` a provider of their own that answers `replies`, and have the server ask it
/// for their agent's turns.
fn answering(
    workspaces: &Workspaces,
    root: &SigningKey,
    replies: impl IntoIterator<Item = Reply>,
) -> Arc<ScriptedProvider> {
    let theirs = ScriptedProvider::answering(replies);
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            &root.verifying_key().to_bytes(),
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-made-up-for-this-test"),
                models: &["gemini-main"],
            },
            true,
        )
        .unwrap();
    theirs
}

/// Her assistant's model writes an answer of five thousand words. Every word of it stands in
/// the reply, in order, and the reply ends as an answer that is complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_of_five_thousand_words_arrives_whole() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let long = words(5_000);
    let theirs = answering(&workspaces, &root, [Reply::text(&long)]);
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "write the whole report",
    )
    .await;
    // The server's own model is never asked: the one she linked answers.
    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, never_asked(Arc::clone(&unasked)));
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(theirs.asked().len(), 1, "her own model is asked once");
    let stood = reply.text.split_whitespace().count();
    assert_eq!(
        stood, 5_000,
        "the reply holds {stood} of the answer's 5,000 words: it was cut where an allowance \
         of the server's ended"
    );
    assert_eq!(reply.text, long, "every word, in order");
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Complete),
        "{:?}",
        reply.work
    );
    assert!(unasked.lock().unwrap().is_empty());
    let _ = person;
    page.peer.close();
}

/// Her assistant's model thinks for four and a half thousand tokens before it says a word.
/// A provider counts that thought against the allowance it was sent, so an allowance of the
/// server's could end the step before its first word. The answer stands in the reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_step_that_thinks_long_still_says_its_answer() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let theirs = answering(
        &workspaces,
        &root,
        [Reply::saying(vec![
            Content::Reasoning {
                text: words(4_500),
                extra_content: None,
            },
            Content::Text {
                text: "The report is ready.".into(),
            },
        ])],
    );
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "think it through, then say whether the report is ready",
    )
    .await;
    let unasked = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, never_asked(Arc::clone(&unasked)));
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text, "The report is ready.",
        "its thought used up an allowance of the server's before its first word: {:?}",
        reply.work
    );
    assert_eq!(theirs.asked().len(), 1, "the step is asked once");
    assert_eq!(
        end_of(&reply),
        Some(chat::agent_stream::AgentFinish::Complete),
        "{:?}",
        reply.work
    );
    assert!(unasked.lock().unwrap().is_empty());
    let _ = person;
    page.peer.close();
}
