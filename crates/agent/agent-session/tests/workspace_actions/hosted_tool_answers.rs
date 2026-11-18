//! What a tool answers reaches the turn's model whole: nothing of the server's cuts it.
//!
//! The agent's tools page their own answers, and a page ends with the line that says how to
//! read on. A cut of the server's own at 24 KiB, laid over that, took a long page's end away
//! and with it the way to the next page.
use super::*;

/// Write `paragraphs` as the workspace's letter, through the tools a turn of her agent has:
/// the caller those tools act as.
async fn written(served: &Served, workspace: &str, said: &Id, paragraphs: &[String]) -> Caller {
    let hers = on_a_turn(
        &Served::person(&served.ada, &served.devices.1, workspace),
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), said),
        Vec::new(),
    );
    let call = serde_json::json!({
        "path": "word-processor#replace-body",
        "args": {"markdown": paragraphs.join("\n\n")},
    });
    let (wrote, failed) = served.tool(workspace, &hers, "tilo_do", call).await;
    assert!(!failed, "{wrote}");
    hers
}

/// A model that reads the letter with `reading` and then answers in words. Every request it
/// is sent is kept in `asked`.
fn reads(asked: &Arc<Mutex<Vec<ModelRequest>>>, reading: serde_json::Value) -> Model {
    let asked = Arc::clone(asked);
    Arc::new(move || {
        Some(Box::new(Scripted {
            answers: vec![
                answer(
                    vec![Content::ToolCall(ToolCall {
                        id: "read".into(),
                        name: "tilo_read".into(),
                        input: reading.clone(),
                        extra_content: None,
                    })],
                    StopReason::ToolCalls,
                    1,
                ),
                answer(
                    vec![Content::Text {
                        text: "I read the letter.".into(),
                    }],
                    StopReason::EndTurn,
                    1,
                ),
            ],
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// The last hundred characters of `text`.
fn last_of(text: &str) -> String {
    let from = text.chars().count().saturating_sub(100);
    text.chars().skip(from).collect()
}

/// What the one tool of the turn answered, as its model was shown it.
fn was_shown(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> String {
    let mut answers = tools_answered(asked);
    assert_eq!(answers.len(), 1, "one tool answered");
    answers.pop().unwrap_or_default()
}

/// The cursor a page of a read ends with, when the page says how to read on.
fn way_on(page: &str) -> Option<String> {
    page.rsplit_once("cursor \"")
        .map(|(_, rest)| rest.trim_end_matches('"').to_owned())
}

/// Her agent reads a letter of 120 paragraphs. The word processor hands it a page at a time,
/// and a page of this letter is longer than 24 KiB. Its model is shown the first page to its
/// last line, which says how to read on, and that line leads through every paragraph of the
/// letter, in order, to its end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_page_reaches_the_model_whole_with_its_way_on() {
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let sentence = "The harbour master counted the crates twice and wrote the number down.";
    let paragraphs: Vec<String> = (1..=120)
        .map(|n| {
            format!(
                "Paragraph {n:03} of the letter. {}",
                [sentence; 5].join(" ")
            )
        })
        .collect();
    let hers = written(&served, &workspace, &said, &paragraphs).await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    let reading = serde_json::json!({"path": "word-processor/Document", "max_chars": 200_000});
    answered(&served, &workspace, &said, reads(&asked, reading)).await;
    let first = was_shown(&asked);
    assert!(
        !first.contains("(cut: ask for less)"),
        "the server cut what the tool answered to {} bytes; it ends {:?}",
        first.len(),
        last_of(&first)
    );
    let mut cursor = way_on(&first);
    assert!(
        cursor.is_some(),
        "the first page of a longer letter ends with the line that says how to read on: {:?}",
        last_of(&first)
    );
    // What its model was shown leads on: each next page is read as the page before says.
    let mut pages = vec![first];
    for _ in 0..24 {
        let Some(from) = cursor.take() else {
            break;
        };
        let reading = serde_json::json!({
            "path": "word-processor/Document",
            "max_chars": 200_000,
            "cursor": from,
        });
        let (text, failed) = served.tool(&workspace, &hers, "tilo_read", reading).await;
        assert!(!failed, "{text}");
        cursor = way_on(&text);
        pages.push(text);
    }
    assert!(
        cursor.is_none(),
        "the letter reads to its end: {} pages were read and the last names a next",
        pages.len()
    );
    let whole = pages.join("\n");
    let mut rest = whole.as_str();
    for paragraph in &paragraphs {
        let Some((_, after)) = rest.split_once(paragraph.as_str()) else {
            panic!(
                "{:?} does not follow the paragraphs before it in the {} pages read, {} bytes",
                paragraph.get(..17),
                pages.len(),
                whole.len()
            );
        };
        rest = after;
    }
    page.peer.close();
}
