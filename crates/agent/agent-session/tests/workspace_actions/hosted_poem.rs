//! A person's case from its first word to its last, through the server's own interfaces. They
//! tell their agent to write a poem into a document. In a later message they ask to see the
//! poem it wrote, and the reply carries the document itself, live in their conversation. The
//! agent changes the document afterwards, and their page, never opened again, holds the
//! change. At the end they end the share themselves, and the document stays theirs.
use super::*;

/// The poem, a line a paragraph.
const POEM: [&str; 3] = [
    "The page is shared, the cursor waits.",
    "Two hands write where one began.",
    "What either changes, both can read.",
];
/// The line the agent adds once the document stands in the conversation.
const ADDED: &str = "The share may end; the poem stays.";

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

/// The person invokes Chat's `stop-sharing` on `message`, through the server's action service,
/// as their page does.
async fn ends_the_share(
    actions: &tilo_agent_session::workspace_actions::WorkspaceActions,
    workspace: &str,
    (root, browser): (&SigningKey, &crate::support::Device),
    message: &Id,
) -> Result<serde_json::Value, String> {
    let id = WorkspaceId::parse(workspace).unwrap();
    let service = tilo_server::workspace_agents::WorkspaceActionProvider::new(actions.clone(), id);
    let caller = tilo_actions::remote::Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser.cert.device_public_key,
    )
    .in_workspace(workspace);
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: workspace.into(),
        document: chat::parts::part(0).as_str().into(),
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert("message".into(), serde_json::json!(message.as_str()));
    tilo_server::workspace_agents::invoke_named(
        &service,
        &caller,
        &route,
        &format!("{}/messages/{message}", chat::parts::ChatMessages::KIND),
        "stop-sharing",
        arguments,
    )
    .await
}

/// The message that shares a window, with how its conversation keeps it.
fn the_share(part: &chat::parts::ChatMessages) -> (Id, Shared) {
    let shared = shares(part);
    assert_eq!(shared.len(), 1, "one share: {shared:?}");
    let id = part
        .messages
        .iter()
        .find(|(_, held)| held.share.is_some())
        .map(|(id, _)| id.clone())
        .expect("the share's message");
    (id, shared.into_iter().next().expect("the one share"))
}

/// Told to write a poem into a document, the agent writes it and shows nothing. Asked a
/// message later to show the poem it wrote, it answers with that document, live in the
/// conversation the person asked in: what the share names reads as the poem to them. A line
/// the agent adds after that goes into the document the conversation shows, and the person's
/// page, opened before anything was written and never again, holds it. The person ends the
/// share themselves; the message stays and says what was shared, and the document stays
/// theirs to read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_poem_is_written_then_shown_live_then_changed_and_its_share_ended_by_its_person() {
    team!(root, browser, actions, workspaces, workspace, page);
    let mut page = page;
    let person = hex::encode(root.verifying_key().to_bytes());
    let conversation = chat::Chat::direct_id([MemberId::person(person), MemberId::agent(SCOUT)]);

    // 1. The poem is written into a document.
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
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
    let part = conversation_part(&workspaces, &workspace).await;
    assert!(
        shares(&part).is_empty(),
        "writing a document shows nothing: {:?}",
        shares(&part)
    );

    // 2. A message later: show me the poem you wrote.
    let question = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
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
    let part = conversation_part(&workspaces, &workspace).await;
    let (message, shown) = the_share(&part);
    assert_eq!(
        shown.author,
        MemberId::agent(SCOUT),
        "the agent's own share"
    );
    assert!(shown.for_someone, "it says what shared it for whom");
    assert_eq!(
        shown.conversation,
        conversation.as_str(),
        "in the conversation the person asked in"
    );
    assert!(shown.share.open && !shown.share.ended, "live: {shown:?}");
    assert_eq!(shown.at["app"], WRITER);
    assert_eq!(shown.at["label"], "The poem");
    let reply = part
        .messages
        .iter()
        .find(|(_, held)| {
            held.author == MemberId::agent(SCOUT) && held.parent.as_ref() == Some(&question)
        })
        .map(|(_, held)| held.clone())
        .expect("the reply");
    assert_eq!(reply.text, "The poem is above.");
    let document = shown.at[DOCUMENT].as_str().unwrap_or_default().to_owned();
    let reads = reads_to_the_person(&actions, &workspace, (&root, &browser), &document).await;
    for line in POEM {
        assert!(
            reads.contains(line),
            "the document {document:?} the share names is the poem of the turn before: {reads}"
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
    write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
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
    let part = conversation_part(&workspaces, &workspace).await;
    let (still, live) = the_share(&part);
    assert_eq!(still, message, "the same message shows it");
    assert!(live.share.open && !live.share.ended, "still live: {live:?}");
    assert_eq!(
        live.at[DOCUMENT].as_str(),
        Some(document.as_str()),
        "and names the same document"
    );
    page.until_heard(&route, heard).await;
    let on_the_page = once_on_the_page(&mut page, &route, ADDED).await;
    for line in POEM {
        assert!(
            on_the_page.contains(line),
            "the line was added to the poem the conversation shows: {on_the_page}"
        );
    }

    // 4. The person ends the share.
    ends_the_share(&actions, &workspace, (&root, &browser), &message)
        .await
        .expect("the person ends the share their agent made for them");
    let deadline = tilo_test_env::hang_deadline();
    let ended = loop {
        let part = conversation_part(&workspaces, &workspace).await;
        let (_, ended) = the_share(&part);
        if ended.share.ended {
            break ended;
        }
        assert!(
            Instant::now() < deadline,
            "the share never ended: {ended:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(!ended.share.open, "ended, and no longer open: {ended:?}");
    assert_eq!(ended.at, shown.at, "the message still says what was shared");
    let reads = reads_to_the_person(&actions, &workspace, (&root, &browser), &document).await;
    assert!(
        reads.contains(ADDED),
        "the document stays the person's to read: {reads}"
    );
    page.peer.close();
}
