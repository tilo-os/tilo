//! What a document says reaches an agent's model behind the label of content from outside.
//!
//! A document's words are whoever wrote them's: its person, another member of the workspace, a
//! file somebody passed on. `tilo_read` hands them to a model, so it says first that they are
//! data to reason about and never instructions to follow: for a document read by its path, for
//! one read by its app and id, and for whatever a provider itself says carries such content.

use super::*;

/// Whether `text` says the label of content from outside before anything else, and before
/// `words`, which it holds.
fn labelled_before(text: &str, words: &str) -> bool {
    let label = format!("[{}: {}]", crate::UNTRUSTED, crate::UNTRUSTED_NOTE);
    text.starts_with(&label) && text.find(words).is_some_and(|at| at > label.len())
}

#[tokio::test]
async fn what_a_document_says_is_read_behind_the_label_of_content_from_outside() {
    let read = json!({"path": "notes/Todo"});
    // A note whose reader is no document read, from a provider that marks nothing, reads as
    // it did: what is labelled is said below, and nothing else is.
    let plain = Notes::new(true).live();
    let (text, failed) = tool(&agent(&plain), "tilo_read", read.clone()).await;
    assert!(!failed && text.contains("milk"), "{text}");
    assert!(!text.contains(crate::UNTRUSTED_NOTE), "{text}");

    // A document read by its path: the label first, then what the document says.
    let documents = Notes::new(true).live().reading_documents();
    let (text, failed) = tool(&agent(&documents), "tilo_read", read.clone()).await;
    assert!(!failed, "{text}");
    assert!(
        labelled_before(&text, "milk"),
        "a model reads the label before the document: {text}"
    );
    // A program that asks for the typed read is told the same, first.
    let (typed, failed) = answer(&agent(&documents), "tilo_read", read.clone()).await;
    assert!(!failed, "{typed}");
    assert_eq!(typed[crate::UNTRUSTED], crate::UNTRUSTED_NOTE, "{typed}");

    // What a provider itself says carries content from outside keeps that label when it is
    // read by a path, as it has it when an action answers it.
    let outside = Notes::new(true).live().answering_from_outside();
    let (text, failed) = tool(&agent(&outside), "tilo_read", read.clone()).await;
    assert!(!failed, "{text}");
    assert!(
        labelled_before(&text, "milk"),
        "the provider's label is kept by a read: {text}"
    );
}

#[tokio::test]
async fn a_document_read_by_its_app_and_id_says_first_that_it_is_content_from_outside() {
    let mut held = tilo_actions::remote::HeldCopies::default();
    let mut letter = tilo_resource::model::Doc::new();
    held.0.insert("letter".to_owned(), letter.save());
    let kept = Notes::new(true).live().keeping(held);
    let path = format!(
        "{}:so.tilo.test:letter",
        tilo_actions::remote::RESOURCE_PROVIDER
    );
    let reply = agent(&kept)
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"tilo_read","arguments":{"path": path}}}))
        .await
        .unwrap();
    let result = &reply["result"];
    assert!(result["isError"] != true, "{reply}");
    // The text a model is handed is the answer itself, still JSON, the label its first word.
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.starts_with(&format!("{{\"{}\":", crate::UNTRUSTED))
            && text.contains(crate::UNTRUSTED_NOTE),
        "{reply}"
    );
    assert!(serde_json::from_str::<Value>(text).is_ok(), "{text}");
    // And a program that takes the typed answer is told the same.
    assert_eq!(
        result["structuredContent"][crate::UNTRUSTED],
        crate::UNTRUSTED_NOTE,
        "{reply}"
    );
}
