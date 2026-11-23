//! A listing asked of a workspace that is not serving it yet, or slow to answer: it waits
//! for the answer as long as the workspace is coming back, and says so when it gave up.
use super::*;

/// A workspace that is not serving this session yet is waited for, not answered: a listing
/// asked while it comes up reads once it serves, and never tells the caller it was not ready.
#[tokio::test]
async fn a_listing_waits_for_a_workspace_that_is_not_serving_it_yet() {
    let notes = Notes::new(true).not_serving();
    let server = std::sync::Arc::new(agent(&notes));
    let asking = tokio::spawn({
        let server = std::sync::Arc::clone(&server);
        async move { tool(&server, "tilo_ls", json!({"path": "notes"})).await }
    });
    // The ask is refused as not ready until the workspace says it serves the session.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!asking.is_finished(), "the listing is still waiting");
    notes.served();
    let (text, failed) = asking.await.unwrap();
    assert!(!failed, "{text}");
    assert!(text.contains("Todo"), "{text}");
}

/// A workspace that takes long to answer — one that just started opens the documents a
/// listing reads first — is waited for until it answers: the listing ends when its answer
/// comes, rather than giving up on it and waiting for an answer it was already sent.
#[tokio::test(start_paused = true)]
async fn a_listing_ends_when_a_slow_workspace_answers() {
    let notes = Notes::new(true);
    *notes.slow.lock().unwrap() = Some(std::time::Duration::from_secs(20));
    let server = agent(&notes);
    let started = tokio::time::Instant::now();
    let (text, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(!failed, "{text}");
    assert!(text.contains("Todo"), "{text}");
    let waited = started.elapsed();
    assert!(
        waited < std::time::Duration::from_secs(45),
        "the listing ended {waited:?} after it asked"
    );
}

/// A workspace that answers nothing at all is given up on once the call waited its
/// whole wait, and the caller is told it may ask again.
#[tokio::test(start_paused = true)]
async fn a_listing_a_workspace_never_answers_says_to_ask_again() {
    let notes = Notes::new(true);
    *notes.slow.lock().unwrap() = Some(std::time::Duration::from_secs(3600));
    let server = agent(&notes);
    let (text, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(failed, "{text}");
    assert!(text.contains("ask again"), "{text}");
}
