//! A person's case from its first word to its last, through the server's own interfaces. They
//! tell their agent to write a poem into a document. In a later message they ask to see the
//! poem it wrote, and the reply carries the document itself, live, as a card. The agent changes
//! the document afterwards, and their page, never opened again, holds the change.
use super::{asked as asked_all, *};

/// The poem, a line a paragraph.
const POEM: [&str; 3] = [
    "The page is shared, the cursor waits.",
    "Two hands write where one began.",
    "What either changes, both can read.",
];
/// The line the agent adds once the document stands in its reply.
const ADDED: &str = "The card stays; the poem grows.";

/// A model that opens the word processor and writes the poem, as it is told to.
fn writes_the_poem(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let mut calls = vec![(
        "tilo_do",
        serde_json::json!({"path": "word-processor#open", "args": {}}),
    )];
    calls.extend(POEM.iter().map(|line| {
        (
            "tilo_do",
            serde_json::json!({"path": "word-processor#append-paragraph",
                "args": {"text": line}}),
        )
    }));
    calls_then_answers(
        calls,
        "I wrote the poem into a document.",
        Arc::clone(asked),
    )
}

/// A model that, asked a message later to show the poem, answers with the document: by the
/// path it wrote with, and nothing else.
fn shows_the_poem(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    calls_then_answers(
        vec![(
            "answer_with_document",
            serde_json::json!({"path": "word-processor", "title": "The poem"}),
        )],
        "The poem is above.",
        Arc::clone(asked),
    )
}

/// A model that adds a line to the poem, a message later again, by the same path.
fn adds_a_line(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    calls_then_answers(
        vec![(
            "tilo_do",
            serde_json::json!({"path": "word-processor#append-paragraph",
                "args": {"text": ADDED}}),
        )],
        "I added a last line.",
        Arc::clone(asked),
    )
}

/// What the word processor's document at `route` reads as on the person's page, from what
/// that page received over the one connection it opened before anything was written.
fn reads_on_the_page(page: &mut Browser, route: &DocumentRoute) -> String {
    page.pump();
    if page.heard(route) == 0 {
        return String::new();
    }
    let document = page.mount(route);
    document
        .read(|doc| {
            let containers = word_processor::model::open(doc).expect("a document");
            tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
                .iter()
                .map(|block| block.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap()
}

/// What the document reads as on the person's page once it holds `line` there.
async fn once_on_the_page(page: &mut Browser, route: &DocumentRoute, line: &str) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reads = reads_on_the_page(page, route);
        if reads.contains(line) {
            return reads;
        }
        assert!(
            Instant::now() < deadline,
            "the page never received {line:?}; it reads {reads:?}, and could not open {:?}",
            page.unopened()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Told to write a poem into a document, the agent writes it and shows nothing. Asked a
/// message later to show the poem it wrote, it answers with that document, live in its reply:
/// what the card names reads as the poem to them. A line the agent adds after that goes into
/// the document the card shows, and the person's page, opened before anything was written and
/// never again, holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_poem_is_written_then_shown_live_then_changed() {
    team!(root, browser, actions, workspaces, workspace, page);
    let mut page = page;
    let person = caller_of(&root, &browser, &workspace);

    // 1. The poem is written into a document.
    asks_scout(
        &workspaces,
        &workspace,
        &root,
        "Write a poem into a document.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(workspaces, actions, workspace, writes_the_poem(&asked));
    // Taken out whole: no lock is held while the test asks the server anything more.
    let asked = asked.lock().unwrap().clone();
    let wrote = told(&asked);
    assert!(
        wrote.len() == 4 && wrote.iter().all(|(_, failed)| !failed),
        "it opened the word processor and wrote each line: {wrote:?}"
    );
    let first = reply(&workspaces, &workspace, &root).await;
    assert!(
        first.shown.is_empty(),
        "writing a document shows nothing: {first:?}"
    );

    // 2. A message later: show me the poem you wrote.
    asks_scout(
        &workspaces,
        &workspace,
        &root,
        "Show me the poem you wrote.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(workspaces, actions, workspace, shows_the_poem(&asked));
    let asked = asked.lock().unwrap().clone();
    let showed = told(&asked);
    assert!(
        showed
            .last()
            .is_some_and(|(answer, failed)| !failed && answer.contains("shown live")),
        "a later turn shows the document an earlier one wrote: {showed:?}"
    );
    let shown = reply(&workspaces, &workspace, &root).await;
    let cards = cards(&shown);
    assert_eq!(cards.len(), 1, "one card: {shown:?}");
    assert_eq!(cards[0].0, WRITER);
    assert_eq!(shown.shown[0].0, "The poem");
    assert_eq!(shown.body, "The poem is above.");
    let document = cards[0].1.clone().unwrap_or_default();
    let reads = read_by(&actions, &workspace, &person, &document)
        .await
        .expect("the person reads the document shown");
    for line in POEM {
        assert!(
            reads.contains(line),
            "the document {document:?} the card names is the poem of the turn before: {reads}"
        );
    }
    let here: &str = &workspace;
    let route = DocumentRoute {
        app_id: WRITER.into(),
        cluster_id: here.into(),
        document: document.clone(),
    };
    let on_the_page = once_on_the_page(&mut page, &route, POEM[2]).await;
    assert!(
        !on_the_page.contains(ADDED),
        "nothing is added yet: {on_the_page}"
    );

    // 3. The agent changes the document afterwards.
    let heard = page.heard(&route);
    asks_scout(
        &workspaces,
        &workspace,
        &root,
        "Add a last line to the poem.",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    answer_the_turn!(workspaces, actions, workspace, adds_a_line(&asked));
    let asked = asked.lock().unwrap().clone();
    let added = told(&asked);
    assert!(
        added.last().is_some_and(|(_, failed)| !failed),
        "the line was added: {added:?}"
    );
    let asks = asked_all(&workspaces, &workspace, &root).await;
    let still = asks
        .iter()
        .filter_map(asks::Asked::latest)
        .find(|reply| !reply.shown.is_empty())
        .expect("the reply that shows the poem");
    assert_eq!(
        still.shown, shown.shown,
        "the reply that showed it still names the same document"
    );
    page.until_heard(&route, heard).await;
    let on_the_page = once_on_the_page(&mut page, &route, ADDED).await;
    for line in POEM {
        assert!(
            on_the_page.contains(line),
            "the line was added to the poem the card shows: {on_the_page}"
        );
    }
    page.peer.close();
}
