//! A search whose word many messages of one conversation hold names its hits from one reading
//! of each level of the conversation's listing. It used to ask the listing again for every hit,
//! and a listing page costs what the conversation holds, whatever it lists. What a search
//! answers is the same, and what its caller may not read is still not found.
use super::*;
use crate::workspace_actions::hosted_secret_use::{Lines, logged};

/// The messages that hold the word.
const HOLDING: usize = 40;
/// The word they hold.
const WORD: &str = "reconciled";

/// Ada tells her agent `count` things in `workspace`, each holding the word.
async fn told(served: &Served, workspace: &str, count: usize) {
    let person = Served::person(&served.ada, &served.devices.1, workspace);
    let provider = turns::WorkspaceActionProvider::new(
        served.actions.clone(),
        WorkspaceId::parse(workspace).unwrap(),
    );
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let chain = ui::resource::chain_resource_id(&chat::parts::chain());
    let conversation = conversation_with(&served.ada, SCOUT);
    for at in 0..count {
        let text = format!("Count {at}: the figures of vendor {at} are {WORD}.");
        turns::invoke_named(
            &provider,
            &person,
            &route,
            &chain,
            "post",
            saying(&conversation, &text).as_object().unwrap().clone(),
        )
        .await
        .expect("she tells her agent");
    }
}

/// What the word finds for `caller` in `workspace`, and the listing pages each search of the
/// wait read: asked until the server has read at least `least` of what was just written.
async fn found_once_read(
    served: &Served,
    workspace: &str,
    caller: &Caller,
    least: usize,
    lines: &Lines,
) -> (Vec<Found>, Vec<usize>) {
    let deadline = tilo_test_env::hang_deadline();
    let mut searched = Vec::new();
    loop {
        let mark = lines.0.lock().unwrap().len();
        let found = served
            .found(workspace, caller, (WORD, &Within::default()))
            .await;
        searched.push(
            String::from_utf8_lossy(&lines.0.lock().unwrap()[mark..])
                .lines()
                .filter(|line| line.contains("a catalogue page was assembled"))
                .count(),
        );
        if found.len() >= least {
            return (found, searched);
        }
        assert!(
            Instant::now() < deadline,
            "the word never finds {least} messages: {found:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_search_with_many_hits_in_one_conversation_reads_each_level_of_its_listing_once() {
    let lines = logged();
    let mut served = Served::start(2).await;

    // An agent nothing narrows: its search answers the twenty best, as before.
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    told(&served, &workspace, HOLDING).await;
    let conversation = conversation_with(&served.ada, SCOUT);
    let person = Served::person(&served.ada, &served.devices.1, &workspace);
    let caller = on_a_turn(&person, SCOUT, (&conversation, &said), Vec::new());
    let (found, searched) = found_once_read(&served, &workspace, &caller, 20, &lines).await;
    assert_eq!(found.len(), 20, "the twenty best: {found:?}");
    assert!(
        found
            .iter()
            .all(|hit| hit.app.as_deref() == Some(CHAT) && !hit.label.is_empty()),
        "each is a message of the conversation, named as its listing names it: {found:?}"
    );
    // The listing is read as the word first names its hits, and kept from then on: the search
    // that names them reads fewer pages than it names hits, not a page for every one.
    assert!(
        searched.iter().any(|pages| *pages > 0)
            && searched.iter().all(|pages| *pages < found.len()),
        "the levels its hits are named in are read once, not once for every hit: {searched:?} \
         listing pages for {} hits",
        found.len()
    );
    page.peer.close();

    // An agent refused what Chat keeps finds none of it, however many messages hold the word.
    let (refused, refused_page, asked_of) = served
        .workspace(|workspace| vec![reading(chat_in(workspace), "deny")])
        .await;
    told(&served, &refused, 12).await;
    // The server has read them: a search of her own agent on a turn nothing narrows answers
    // with them.
    let unbound = on_a_turn(
        &Served::person(&served.ada, &served.devices.1, &refused),
        SCOUT,
        (&conversation, &asked_of),
        Vec::new(),
    );
    found_once_read(&served, &refused, &unbound, 12, &lines).await;
    // Her newest message is what her agent's turn answers.
    let newest = write_to_agent(
        &served.actions,
        &served.workspaces,
        &refused,
        (&served.ada, &served.devices.1),
        "What do my messages say of the vendors?",
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));
    Box::pin(answered(
        &served,
        &refused,
        &newest,
        searching(&[WORD], &asked),
    ))
    .await;
    let answer = tools_answered(&asked);
    assert_eq!(answer.len(), 1, "it searched once: {answer:?}");
    assert!(
        !names_in(&answer[0], "chat") && !answer[0].contains("vendor"),
        "an agent refused what Chat keeps is shown none of it: {answer:?}"
    );
    refused_page.peer.close();
}
